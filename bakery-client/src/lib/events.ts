// One WebSocket for the whole app.
//
// The node broadcasts every stream on a single `/events` socket. Before routes
// existed, one page owned that socket; now the foyer, the workpane, and the
// chrome all want it at once. Opening three sockets would triple the node's
// broadcast fan-out and give each surface its own reconnect story. So: one
// socket, opened on demand, with per-stream subscriptions on top.

import { authHeaders, HTTP, WS, type AgentActivity, type BakeryEvent } from "./node.ts";

export type ConnStatus = "connecting" | "open" | "closed";

type Sub = { streamId: string | null; cb: (ev: BakeryEvent) => void };
type ActivitySub = { streamId: string | null; cb: (a: AgentActivity) => void };

const subs = new Set<Sub>();
const activitySubs = new Set<ActivitySub>();
const envelopeSubs = new Map<string, Set<(envelope: Record<string, unknown>) => void>>();
const statusSubs = new Set<(s: ConnStatus) => void>();

let socket: WebSocket | null = null;
let status: ConnStatus = "closed";
let attempt = 0;
let retryTimer: ReturnType<typeof setTimeout> | null = null;

function setStatus(next: ConnStatus) {
  if (status === next) return;
  status = next;
  for (const cb of statusSubs) cb(status);
}

/** Fast first reconnect (dev restarts); still caps so a long-dead node is quiet. */
function retryDelay(): number {
  // attempt is already incremented when scheduling reopen
  if (attempt <= 1) return 80;
  return Math.min(3000, 120 * 2 ** Math.min(attempt - 1, 4));
}

let opening = false;

/** `/events` requires a one-time ticket minted over the authenticated HTTP
 *  surface (a browser WS upgrade cannot carry an Authorization header). */
async function fetchEventsTicket(): Promise<string> {
  const res = await fetch(`${HTTP}/events/ticket`, {
    method: "POST",
    headers: { ...authHeaders(), "content-type": "application/json" },
    body: "{}",
  });
  if (!res.ok) throw new Error(`events ticket refused: ${res.status}`);
  const body = (await res.json()) as { ticket?: string };
  if (!body.ticket) throw new Error("events ticket response had no ticket");
  return body.ticket;
}

function scheduleRetry() {
  setStatus("closed");
  if (subs.size === 0 && activitySubs.size === 0 && statusSubs.size === 0 && envelopeSubs.size === 0) return;
  attempt++;
  retryTimer = setTimeout(open, retryDelay());
}

function open() {
  if (socket || opening || typeof WebSocket === "undefined") return;
  opening = true;
  setStatus("connecting");
  fetchEventsTicket()
    .then((ticket) => {
      opening = false;
      if (socket) return; // superseded while the ticket was in flight
      openWithTicket(ticket);
    })
    .catch(() => {
      opening = false;
      scheduleRetry();
    });
}

function openWithTicket(ticket: string) {
  const ws = new WebSocket(`${WS}?ticket=${encodeURIComponent(ticket)}`);
  socket = ws;

  ws.onopen = () => {
    attempt = 0;
    said = null;
    setStatus("open");
    sayVisible();
    sayFollowing();
  };

  ws.onmessage = (msg) => {
    let parsed: unknown;
    try {
      parsed = JSON.parse(String(msg.data));
    } catch {
      return; // a frame we cannot parse is not worth tearing the socket down
    }
    // 07d-A5: an ephemeral activity update rides the same socket as every
    // Heart event, wrapped in a `{"kind":"activity",...}` envelope a real
    // `BakeryEvent` can never structurally produce (no such top-level key)
    // — checked first so every existing `onEvent` consumer below is
    // completely unaffected.
    if (
      parsed &&
      typeof parsed === "object" &&
      (parsed as { kind?: unknown }).kind === "activity"
    ) {
      const activity = (parsed as { activity: AgentActivity }).activity;
      // A conversation this window does not follow comes slim: its phase
      // only, for every-conversation listeners (the glyphs).
      const slim = (parsed as { slim?: unknown }).slim === true;
      for (const s of activitySubs) {
        if (s.streamId === null || (!slim && s.streamId === activity.stream_id)) s.cb(activity);
      }
      return;
    }
    // Other envelopes ("ui-layout", "ui-appearance"): the app's own state,
    // saved on the node by anyone (the Baker included), never a Heart event.
    const kind = parsed && typeof parsed === "object" ? (parsed as { kind?: unknown }).kind : undefined;
    if (typeof kind === "string") {
      for (const cb of envelopeSubs.get(kind) ?? []) cb(parsed as Record<string, unknown>);
      return;
    }
    const ev = parsed as BakeryEvent;
    for (const s of subs) {
      if (s.streamId === null || s.streamId === ev.stream_id) s.cb(ev);
    }
  };

  const reopen = () => {
    if (socket !== ws) return; // superseded by a newer socket
    socket = null;
    scheduleRetry();
  };
  ws.onclose = reopen;
  ws.onerror = () => ws.close();
}

/** Each page tells the node whether its window shows, as its socket opens
 *  and as it shows or hides: the node asks GitHub again faster while one
 *  does. */
function sayVisible() {
  if (socket?.readyState === WebSocket.OPEN && typeof document !== "undefined") {
    socket.send(JSON.stringify({ visible: document.visibilityState === "visible" }));
  }
}
if (typeof document !== "undefined") document.addEventListener("visibilitychange", sayVisible);

/** The follow list the node last heard on this socket. */
let said: string | null = null;
let saying = false;

/** Tells the node which conversations this window follows (those with a
 *  listener of their own): it sends their whole activity frames, and of the
 *  rest only phase changes. Batched, so a page mounting many says it once. */
function sayFollowing() {
  if (saying) return;
  saying = true;
  queueMicrotask(() => {
    saying = false;
    if (socket?.readyState !== WebSocket.OPEN) return;
    const follow = [...new Set([...activitySubs].flatMap((s) => (s.streamId === null ? [] : [s.streamId])))].sort();
    const now = JSON.stringify({ follow });
    if (now === said) return;
    said = now;
    socket.send(now);
  });
}

/** Ensure the socket is up. Idempotent. */
export function connectEvents() {
  if (retryTimer) {
    clearTimeout(retryTimer);
    retryTimer = null;
  }
  open();
}

/**
 * Receive events. `streamId` of `null` means every stream (the foyer wants
 * that; a workpane does not). Returns an unsubscribe function.
 */
export function onEvent(streamId: string | null, cb: (ev: BakeryEvent) => void): () => void {
  const sub: Sub = { streamId, cb };
  subs.add(sub);
  connectEvents();
  return () => subs.delete(sub);
}

/** What every-conversation listeners are sure of in a frame: a conversation
 *  this window does not follow comes with its phase only. */
export type ActivityPhaseFrame = Pick<AgentActivity, "stream_id" | "agent_id"> & { phase: { phase: AgentActivity["phase"]["phase"] } };

/**
 * Receive live work-trace updates (07d-A5). `streamId` of `null` means
 * every stream, same convention as `onEvent`. Each callback fires with a
 * full current snapshot, never a delta — for a stream listened to by name.
 * Listening to one makes this window follow it (`sayFollowing`); of the
 * others, every-stream listeners hear phase changes only.
 */
export function onActivity(streamId: string, cb: (a: AgentActivity) => void): () => void;
export function onActivity(streamId: null, cb: (a: ActivityPhaseFrame) => void): () => void;
export function onActivity(streamId: string | null, cb: ((a: AgentActivity) => void) | ((a: ActivityPhaseFrame) => void)): () => void {
  const sub: ActivitySub = { streamId, cb: cb as (a: AgentActivity) => void };
  activitySubs.add(sub);
  connectEvents();
  if (streamId !== null) sayFollowing();
  return () => {
    activitySubs.delete(sub);
    if (streamId !== null) sayFollowing();
  };
}

/** Receive one kind of non-Heart envelope, e.g. "ui-layout": the whole
 *  frame, whose payload is untrusted until the caller validates it. */
export function onEnvelope(kind: string, cb: (envelope: Record<string, unknown>) => void): () => void {
  const set = envelopeSubs.get(kind) ?? new Set();
  set.add(cb);
  envelopeSubs.set(kind, set);
  connectEvents();
  return () => {
    set.delete(cb);
    if (set.size === 0) envelopeSubs.delete(kind);
  };
}

/** Watch the connection itself — the chrome's status dot. */
export function onStatus(cb: (s: ConnStatus) => void): () => void {
  statusSubs.add(cb);
  cb(status);
  connectEvents();
  return () => statusSubs.delete(cb);
}

export function connectionStatus(): ConnStatus {
  return status;
}
