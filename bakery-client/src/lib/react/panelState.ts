// Pure panel derivations — the rules Chat.svelte held inline, lifted out so
// `panelState.check.ts` exercises the exact functions the React panel calls
// rather than a second reimplementation (same reason `studioTarget.ts` and
// `reviewFocus.ts` exist).
//
// Nothing here touches the DOM, React, or Svelte. `Draftbox` takes its
// storage as an argument so drafts are testable without a browser.

// Relative, with the explicit extension: this module is imported by
// `panelState.check.ts`, which runs directly under `node`.
import { payloadOf, type AcpRuntimeSnapshot, type AgentActivity, type BakeryEvent, type OperatingMode } from "../node.ts";
import { type AskQuestion, isCommand, messageText, startsTurn, type TranscriptItem, type TurnUsage } from "../transcript.ts";

/** Whether the newest turn stopped short and Resume can continue it — the
 *  same rule as the node's `interrupted_turn`, read off the transcript:
 *  - "restart": the newest human instruction has no terminal item after it
 *    and nothing is live (Checkpoint 2 item 3). A parked turn (awaiting
 *    approval or an answer) still has an activity phase, so it never reads
 *    as interrupted — only a restart-killed one does;
 *  - "failed": that turn failed (a network error, a provider outage) or hit
 *    its step limit, which used to leave no way to continue it.
 *  A turn that finished, or that the operator stopped, is not resumable, and
 *  nor is a command's (`/compact`): it runs only as sent, so it is sent again. */
export function deriveInterrupted(
  items: readonly TranscriptItem[],
  hasActivityPhase: boolean,
): "restart" | "failed" | null {
  if (hasActivityPhase) return null;
  // Waiting out a passing failure is not cut off: the node keeps trying,
  // and after a restart carries on by itself.
  if (items.at(-1)?.kind === "retrying") return null;
  let failed = false;
  for (let i = items.length - 1; i >= 0; i--) {
    const it = items[i];
    if (it.kind === "done" || it.kind === "cancelled") return null;
    if (it.kind === "failed" || it.kind === "exhausted") failed = true;
    if (startsTurn(it)) return it.kind === "human" && isCommand(it.text) ? null : failed ? "failed" : "restart";
  }
  return null;
}

export type PendingAsk = {
  hash: string;
  questions: readonly AskQuestion[];
};

/** The newest `ask_user` still waiting on a human. The turn is PARKED on it
 *  (continue-after-decision), so at most one can be open — scanning from the
 *  end and stopping at the first unanswered one is the whole rule. */
export function derivePendingAsk(items: readonly TranscriptItem[]): PendingAsk | null {
  for (let i = items.length - 1; i >= 0; i--) {
    const it = items[i];
    if (it.kind !== "ask") continue;
    if (it.answer !== undefined || it.cancelled) return null;
    return { hash: it.hash, questions: it.questions };
  }
  return null;
}

export type ActivityView = {
  /** `complete`/`error`/`cancelled` are hidden: the matching transcript row
   *  right above already tells that story. */
  show: boolean;
  /** Stop is available for every live, non-terminal turn, including an
   *  approval wait. The node terminalizes that exact pending gate before it
   *  cancels the operation, so a late Accept/Answer cannot restart it. */
  cancellable: boolean;
};

export function activityView(activity: AgentActivity | null): ActivityView {
  const phase = activity?.phase ?? null;
  if (!phase) return { show: false, cancellable: false };
  const show = phase.phase !== "complete" && phase.phase !== "error" && phase.phase !== "cancelled";
  const cancellable =
    phase.phase === "contacting_model" ||
    phase.phase === "streaming" ||
    phase.phase === "running_tool" ||
    phase.phase === "awaiting_approval";
  return { show, cancellable };
}

/** Drops a delayed non-terminal activity snapshot once Heart already has a
 * terminal row from after that turn began. The transcript is durable and
 * authoritative; a reconnect fetch or late WebSocket activity frame must not
 * resurrect ephemeral streaming text after the completed reply landed. */
export function activityAfterTranscript(
  activity: AgentActivity | null,
  items: readonly TranscriptItem[],
): AgentActivity | null {
  if (!activity) return null;
  if (
    activity.phase.phase === "complete" ||
    activity.phase.phase === "error" ||
    activity.phase.phase === "cancelled"
  ) {
    return activity;
  }
  const started = Date.parse(activity.turn_started_at);
  if (!Number.isFinite(started)) return activity;
  const superseded = items.some((item) => {
    if (item.kind !== "done" && item.kind !== "failed" && item.kind !== "cancelled" && item.kind !== "exhausted") {
      return false;
    }
    const ended = Date.parse(item.at);
    return Number.isFinite(ended) && ended >= started;
  });
  if (superseded) return null;
  // At each native tool call the node records the words written so far as a
  // segment row, then moves the phase on. Until that phase lands, the same
  // words would show twice — the row, and the live text under it — and then
  // collapse to one: the transcript jumped. Once recorded, the row wins.
  if (activity.phase.phase === "streaming") {
    const live = activity.phase.text.trim();
    for (let i = items.length - 1; i >= 0 && live !== ""; i--) {
      const item = items[i];
      if (!(Date.parse(item.at) >= started)) break;
      if (item.kind !== "segment") continue;
      if (item.text.trimEnd().endsWith(live)) return { ...activity, phase: { ...activity.phase, text: "" } };
      break;
    }
  }
  return activity;
}

/** How many transcript rows the panel renders at once. A 2,000-row socket
 *  must not put 2,000 Markdown renderers in the DOM; it keeps the newest
 *  window and offers the rest explicitly. Windowing rather than virtualising
 *  is deliberate — `use-stick-to-bottom` anchors on real scroll height, and
 *  a virtualiser's estimated heights break that anchor while streaming. */
export const DEFAULT_HISTORY_WINDOW = 200;

export type BoundedHistory<T> = { shown: readonly T[]; hidden: number };

/** The newest `limit` items, plus how many older ones were withheld. */
export function boundedHistory<T>(items: readonly T[], limit: number): BoundedHistory<T> {
  if (limit <= 0) return { shown: [], hidden: items.length };
  if (items.length <= limit) return { shown: items, hidden: 0 };
  return { shown: items.slice(items.length - limit), hidden: items.length - limit };
}

/** The most recent turn's reported usage, for the context affordance. Only
 *  a real `agent.turn.usage` payload produces one — absent means the
 *  provider reported nothing, and the affordance is then not rendered at
 *  all rather than showing a fabricated 0%. */
export function latestUsage(items: readonly TranscriptItem[]): TurnUsage | null {
  for (let i = items.length - 1; i >= 0; i--) {
    const it = items[i];
    if (it.kind === "done" && it.usage) return it.usage;
  }
  return null;
}

export type ContextGauge = { used: number; window: number };

/** Context percentage is only honest when the provider reported BOTH halves
 *  and the window is a positive number. */
export function contextGauge(usage: TurnUsage | null): ContextGauge | null {
  if (!usage) return null;
  const { contextUsed, contextWindow } = usage;
  if (typeof contextUsed !== "number" || typeof contextWindow !== "number") return null;
  if (!(contextWindow > 0)) return null;
  return { used: contextUsed, window: contextWindow };
}

/** The live turn's own usage, which the work-trace snapshot carries while a
 *  turn is running. Preferred over the last completed turn's figure when
 *  present — it describes what is happening now — but it stays `null` until
 *  a provider actually reports numbers, so callers fall back rather than
 *  showing a live 0%. */
/** The recorded change an edit row's file name opens: the first change to
 *  that file at or after the edit (a native runtime's land when its turn
 *  ends), or null while there is none yet. Paths match whole segments, as
 *  a tool may name the file absolute and the change relative. */
export function changeFor(items: readonly TranscriptItem[], path: string, at: string): string | null {
  const same = (p: string) => p !== "" && (p === path || path.endsWith(`/${p}`) || p.endsWith(`/${path}`));
  for (const it of items) if (it.kind === "proposal" && it.at >= at && same(it.filePath)) return it.hash;
  return null;
}

/** The turns with more than one landed change still waiting on a decision
 *  (turn snapshots): each can be kept or undone all at once. `undoable` when
 *  any of its changes can be restored. */
export function pendingTurns(items: readonly TranscriptItem[]): { turnId: string; count: number; undoable: boolean }[] {
  const by = new Map<string, { count: number; undoable: boolean }>();
  for (const it of items) {
    if (it.kind !== "proposal" || !it.landed || it.verdict !== "pending" || !it.landed.turnId) continue;
    const t = by.get(it.landed.turnId) ?? { count: 0, undoable: false };
    t.count++;
    t.undoable ||= it.landed.restorable;
    by.set(it.landed.turnId, t);
  }
  return [...by].filter(([, t]) => t.count > 1).map(([turnId, t]) => ({ turnId, ...t }));
}

/** A runtime's name as its pill shows it: what the node calls "Claude
 *  native agent (subscription)" reads "Claude", "Grok (xAI)" reads "Grok".
 *  The list it opens keeps the full names. */
export function shortRuntime(label: string): string {
  return label.replace(/\s*\([^)]*\)\s*$/, "").replace(/\s+native agent$/i, "").trim() || label;
}

/** How long a stretch of thinking took, for "Thought for 12s": whole
 *  seconds, at least one, then minutes. */
export function thoughtFor(ms: number): string {
  const s = Math.max(1, Math.round(ms / 1000));
  if (s < 60) return `${s}s`;
  return s % 60 ? `${Math.floor(s / 60)}m ${s % 60}s` : `${s / 60}m`;
}

/** A running turn's age from its start: "8s", "54m 30s", "2h 5m". */
export function elapsedText(fromIso: string, now: number): string {
  const seconds = Math.floor((now - Date.parse(fromIso)) / 1000);
  if (!Number.isFinite(seconds) || seconds < 0) return "";
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  return minutes < 60 ? `${minutes}m ${seconds % 60}s` : `${Math.floor(minutes / 60)}h ${minutes % 60}m`;
}

/** The tokens a running turn has produced so far: "↓ 842 tokens", "↓ 1.2k tokens". */
export function liveTokensText(tokens: number | undefined): string {
  if (!tokens) return "";
  if (tokens < 1000) return `↓ ${tokens} ${tokens === 1 ? "token" : "tokens"}`;
  return `↓ ${(tokens / 1000).toFixed(1)}k tokens`;
}

export function activityGauge(activity: AgentActivity | null): ContextGauge | null {
  const u = activity?.usage;
  if (!u) return null;
  return contextGauge({
    contextUsed: u.context_used ?? undefined,
    contextWindow: u.context_window ?? undefined,
  });
}

/** Derives durable composer chrome from the same raw Heart events that are
 * already retained while a Dockview pane is closed.  Queue entries are
 * consumed only by a causal started/applied/failed event, so this reflects
 * the server's exactly-once state rather than optimistic browser state. */
export type ComposerControlState = {
  operatingMode: OperatingMode;
  queuedInstructions: number;
  queuedItems: readonly QueuedItem[];
  dispatchPaused: boolean;
};

/** One still-pending Heart queue event, in durable submission order.
 *
 * This is deliberately a display projection rather than a second queue.
 * Every field comes from the queued event itself, including the configuration
 * captured at submission time; later picker changes therefore cannot rewrite
 * what the read-only queue says will run. */
export type QueuedItem = Readonly<{
  id: string;
  kind: "follow_up" | "steer";
  queuedAt: string;
  instruction: string;
  /** Another conversation's message: its sender's name and its own words. */
  message: Readonly<{ from: string; text: string }> | null;
  filePath: string | null;
  attachmentCount: number;
  /** A read-only ask: it runs in Plan. Every other runs on what the selector
   *  and mode pill show when it starts. */
  readOnly: boolean;
}>;

function queuedItem(event: BakeryEvent, payload: Record<string, any>): QueuedItem {
  return Object.freeze({
    id: event.event_hash,
    kind: event.family === "agent.steer.queued" ? "steer" : "follow_up",
    queuedAt: event.ts,
    instruction: typeof payload.instruction === "string" ? payload.instruction : "",
    message:
      typeof payload.message?.name === "string" && payload.message.name
        ? Object.freeze({
            from: payload.message.name,
            text: typeof payload.message.text === "string" ? payload.message.text : messageText(typeof payload.instruction === "string" ? payload.instruction : ""),
          })
        : null,
    filePath: typeof payload.file_path === "string" && payload.file_path !== "" ? payload.file_path : null,
    attachmentCount: Array.isArray(payload.attachment_ids) ? payload.attachment_ids.length : 0,
    readOnly: payload.read_only === true,
  });
}

/** A message this tab is sending that has not been answered yet: its words,
 *  and the newest event the stream held when the send began. */
export type SendInFlight = Readonly<{ text: string; after: number }>;

export function deriveComposerControl(
  events: readonly BakeryEvent[],
  agentId: string,
  sending: SendInFlight | null = null,
): ComposerControlState {
  const queued = new Map<string, QueuedItem>();
  let operatingMode: OperatingMode = "manual";
  let dispatchPaused = false;
  for (const event of events) {
    const payload = payloadOf(event);
    if (payload.agent_id !== agentId) continue;
    if (event.family === "agent.follow_up.queued" || event.family === "agent.steer.queued") {
      // The message this tab is sending right now, queued a moment ago: an
      // idle agent starts it before the send is answered, so until then it is
      // not a message waiting. If the send comes back without the turn
      // started, `sending` is gone and it shows as waiting after all.
      if (
        sending &&
        event.family === "agent.follow_up.queued" &&
        event.seq > sending.after &&
        payload.instruction === sending.text
      ) {
        continue;
      }
      queued.set(event.event_hash, queuedItem(event, payload));
      continue;
    }
    if (
      event.family === "agent.turn.started" ||
      event.family === "agent.steer.applied" ||
      event.family === "agent.queue.failed" ||
      // "Steer all now": the merged follow-ups became one queued steer.
      event.family === "agent.queue.merged" ||
      // ↑ took it back to edit: it will never send.
      event.family === "agent.queue.withdrawn" ||
      // A thread's finish notice the newer one from it replaced.
      event.family === "message.replaced"
    ) {
      for (const cause of event.causality) queued.delete(cause);
      continue;
    }
    if (event.family === "agent.operating_mode.set") {
      const mode = payload.mode;
      if (mode === "manual" || mode === "plan" || mode === "auto_edit" || mode === "auto") operatingMode = mode;
      continue;
    }
    if (event.family === "agent.dispatch.paused") dispatchPaused = true;
    if (event.family === "agent.dispatch.resumed") dispatchPaused = false;
  }
  const queuedItems = Object.freeze([...queued.values()]);
  return { operatingMode, queuedInstructions: queuedItems.length, queuedItems, dispatchPaused };
}

/** The runtime's snapshot with one setting's current value changed: how a
 *  pick moves its control before the runtime has answered. `null` is none
 *  chosen, which a refused pick goes back to when the runtime reports none. */
export function withConfigValue(
  runtime: AcpRuntimeSnapshot,
  optionId: string,
  value: string | boolean | null,
): AcpRuntimeSnapshot {
  return {
    ...runtime,
    config_options: runtime.config_options.map((o) => (o.id === optionId ? { ...o, current_value: value } : o)),
  };
}

// ---- drafts ---------------------------------------------------------------
//
// UI-only state, never a server fact, so it lives beside the panel rather
// than in Heart. Keyed by stream_id + agent_id per the slice: switching
// sockets or agents must not hand one conversation another's unsent text.
//
// Only the composer's UNSENT text is stored. Recorded conversation history
// is never copied here — Heart already has it, and a second copy in
// localStorage would be both a duplicate and a leak.

const DRAFT_PREFIX = "bakery.draft.v1";
const DRAFT_REVISION_PREFIX = "bakery.draft-revision.v1";
let draftRevision = 0;
type StoredChange = Readonly<{ previous: string | null; next: string | null }>;
const draftListeners = new Map<string, Set<(change: StoredChange) => void>>();

function notifyStoredChange(
  listeners: Map<string, Set<(change: StoredChange) => void>>,
  key: string,
  change: StoredChange,
): void {
  for (const listener of listeners.get(key) ?? []) listener(change);
}

function subscribeStoredChange(
  listeners: Map<string, Set<(change: StoredChange) => void>>,
  key: string,
  listener: (change: StoredChange) => void,
): () => void {
  const current = listeners.get(key) ?? new Set<(change: StoredChange) => void>();
  current.add(listener);
  listeners.set(key, current);
  return () => {
    current.delete(listener);
    if (current.size === 0) listeners.delete(key);
  };
}

/** What actually happened to a draft write.
 *
 *  This exists because the alternative is silent data loss. An earlier
 *  version capped drafts at 32k characters and truncated past it, so a long
 *  instruction came back shorter than it went in with nothing said — the
 *  worst possible failure for a text box. Drafts are now stored whole, and a
 *  store that refuses (quota, disabled, private mode) says so out loud, so
 *  the human can copy their words somewhere safe before closing the pane.
 *
 *  - `saved`      — the whole draft is persisted.
 *  - `cleared`    — empty text; the key was removed.
 *  - `failed`     — a real storage rejected the write. SHOW THIS.
 *  - `no-storage` — no storage at all (SSR, locked-down browser). Drafts are
 *                   in-memory for the session; not worth alarming over. */
export type DraftWrite = "saved" | "cleared" | "failed" | "no-storage";
export type DraftConsume = DraftWrite | "preserved";

export function draftKey(streamId: string | null, agentId: string): string {
  return `${DRAFT_PREFIX}:${streamId ?? "-"}:${agentId}`;
}

function draftRevisionKey(streamId: string | null, agentId: string): string {
  return `${DRAFT_REVISION_PREFIX}:${streamId ?? "-"}:${agentId}`;
}

/** The slice of `Storage` this needs. A caller in a context without
 *  localStorage (SSR, a locked-down browser) passes `null` and drafts
 *  degrade to in-memory only — never an exception. */
export type DraftStorage = Pick<Storage, "getItem" | "setItem" | "removeItem">;

export class Draftbox {
  private store: DraftStorage | null;

  constructor(store: DraftStorage | null) {
    this.store = store;
  }

  read(streamId: string | null, agentId: string): string {
    try {
      return this.store?.getItem(draftKey(streamId, agentId)) ?? "";
    } catch {
      // A storage that throws on read (disabled, over quota, private mode)
      // means no draft — not a broken panel.
      return "";
    }
  }

  /** Stores the draft WHOLE — never truncated. Returns what happened so a
   *  caller can surface a refusal instead of pretending the words are safe. */
  write(streamId: string | null, agentId: string, text: string): DraftWrite {
    const key = draftKey(streamId, agentId);
    if (!this.store) return "no-storage";
    try {
      const previous = this.store.getItem(key);
      if (text === "") {
        this.store.removeItem(key);
        this.store.removeItem(draftRevisionKey(streamId, agentId));
        if (previous !== null) notifyStoredChange(draftListeners, key, { previous, next: null });
        return "cleared";
      }
      // Effects and unmount flushes commonly persist the same revision more
      // than once. Do not mint a new ownership token unless the text changed.
      if (previous === text) return "saved";
      this.store.setItem(key, text);
      this.store.setItem(
        draftRevisionKey(streamId, agentId),
        `${Date.now().toString(36)}-${(++draftRevision).toString(36)}`,
      );
      notifyStoredChange(draftListeners, key, { previous, next: text });
      return "saved";
    } catch {
      // Over quota, or a storage that throws. The in-memory composer value is
      // untouched — only persistence across reopen is lost, and the caller is
      // told so it can say that out loud.
      return "failed";
    }
  }

  clear(streamId: string | null, agentId: string): DraftWrite {
    return this.write(streamId, agentId, "");
  }

  /** Removes only the submitted revision after the node acknowledges it.
   * A reopened pane may already have replaced the stored value or appended a
   * newer draft; an older projection must never clear that newer work. */
  capture(streamId: string | null, agentId: string): string {
    try {
      return JSON.stringify([
        this.store?.getItem(draftKey(streamId, agentId)) ?? null,
        this.store?.getItem(draftRevisionKey(streamId, agentId)) ?? null,
      ]);
    } catch {
      return "unavailable";
    }
  }

  consumeSubmitted(
    streamId: string | null,
    agentId: string,
    submitted: string,
    captured: string,
  ): DraftConsume {
    const current = this.read(streamId, agentId);
    if (this.capture(streamId, agentId) === captured) return this.clear(streamId, agentId);
    const stagedPrefix = `${submitted}\n\n`;
    if (current.startsWith(stagedPrefix)) {
      return this.write(streamId, agentId, current.slice(stagedPrefix.length));
    }
    return "preserved";
  }

  /** Re-stages a refused submission without overwriting work entered in a
   * newer projection while the request was in flight. */
  restoreSubmitted(streamId: string | null, agentId: string, submitted: string): DraftConsume {
    const current = this.read(streamId, agentId);
    if (current === submitted || current.startsWith(`${submitted}\n\n`)) return "preserved";
    return this.write(streamId, agentId, current === "" ? submitted : `${submitted}\n\n${current}`);
  }

  subscribe(
    streamId: string | null,
    agentId: string,
    listener: (change: StoredChange) => void,
  ): () => void {
    return subscribeStoredChange(draftListeners, draftKey(streamId, agentId), listener);
  }
}

/** The part of a text box `toEndOfDraft` moves. */
export type DraftBox = {
  value: string;
  scrollTop: number;
  readonly scrollHeight: number;
  setSelectionRange(start: number, end: number): void;
};

/** Where a draft is picked up again: the caret after its last character, and
 *  the box (which grows to a height, then scrolls inside) at its last line. */
export function toEndOfDraft(box: DraftBox): void {
  box.setSelectionRange(box.value.length, box.value.length);
  box.scrollTop = box.scrollHeight;
}

// ---- the Bakery's own loop's model / effort pick ---------------------------
//
// The thread's selection, which lives on the node (`selection`): every turn
// runs on it until changed. Native ACP configuration is the session's.

export type HttpTurnOverride = Readonly<{
  modelId: string;
  effort: string;
}>;
