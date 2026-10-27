// Canonical client-side topology store (07c-LIVE-STATE). `GET /studio/state`
// is the ONLY socket inventory the client reads for identity, grouping,
// active/retired state, and configured agents — sidebar, Factory Floor,
// Baker's visible open-job rows, and workbench socket identity all read
// this one module instead of keeping their own `/streams` copy. History,
// gates, Heart metrics, operator metrics, and Baker's push/PR/merge detail
// stay on their own specialized routes; this store is topology only.
import { SvelteSet } from "svelte/reactivity";
import { node, payloadOf, type BakeryEvent, type SocketInfo, type StudioState, type DiscardResp } from "./node";
import { onEvent, onStatus } from "./events";
import { reconcileBusyOverrides } from "./busyReconcile";
import { keep, kept } from "./kept";
import { whileShown } from "./pageShown";
import {
  isGateRefreshFamily,
  isStatusRefreshFamily,
  hasBusyTaskSocket,
  hasWorkingConversation,
} from "./studioRefreshTriggers";

// Every Heart family that changes socket topology (who exists, what group
// it's in, its lifetime/discarded state, its agent/model assignment, its
// owning node). The store's one global subscription reacts to exactly
// these — chat/history events stay on their own stream-scoped
// subscriptions elsewhere, and gates/operator metrics keep their own.
// `socket.kind.recorded` (07d-A1 correction): the durable factory/project
// fact `baker_assign_inner` writes right after `baker.assignment.accepted`
// — without its own subscription entry, a socket could sit classified as
// "project" (its `kind` field, and its role list) until some UNRELATED
// topology event happened to trigger the next refresh.
//
// 07d-A3: the Project catalog families — a Project's own facts plus the
// per-socket `socket.project.set` membership fact — also change grouping
// and must trigger the same refresh (create/rename/pin/retire a Project,
// attach/detach a root, or re-parent a socket).
const TOPOLOGY_FAMILIES = new Set([
  "project.onboarded",
  "baker.assignment.accepted",
  "seat.model.assigned",
  // A seat's switch waiting for its turn to end, and one that couldn't land:
  // every window shows them, not only the one that asked.
  "seat.model.pending",
  "seat.model.switch_failed",
  // A selector's model or effort: every window's picker shows the change.
  "agent.native_config.acknowledged",
  "socket.kind.recorded",
  "stream.lifetime.set",
  "stream.discarded",
  "stream.ownership.claimed",
  "project.created",
  "project.renamed",
  "project.pin.set",
  "project.root.attached",
  "project.root.detached",
  "project.retired",
  "socket.project.set",
  // Baker threads (BAKER-THREADS-03) — including the ones the Baker itself
  // opens, renames, or archives through its ops, so they appear live.
  "baker.thread.opened",
  "baker.thread.renamed",
  "baker.thread.archived",
  // Any other conversation archived or restored (`POST /conversation/archive`).
  "socket.archived",
]);

// 07d-A1 §3: live `busy` transitions for one specific agent_id — patched
// locally off ordered Heart events, never by refetching `/studio/state`.
// `agent.tool.called` is deliberately absent: it does not clear busy.
const TURN_TERMINAL_FAMILIES = new Set([
  "agent.turn.done",
  "agent.turn.failed",
  "agent.turn.exhausted",
  "agent.file.changed",
  // 07d-A5: a human explicitly stopping a turn must clear busy too.
  "agent.turn.cancelled",
]);

class StudioStore {
  data: StudioState | null = $state(null);
  /** The node has answered since this window opened: `data` is its, not
   *  the copy kept from before. */
  fresh = $state(false);
  loading = $state(false);
  error = $state("");

  /** Sockets with a discard POST in flight — a purely local, UI-facing
   *  overlay (disable the button, show a spinner) distinct from server
   *  truth. Never used to filter `activeSockets`; a socket stays active
   *  until the server actually confirms the discard. */
  private retiringIds = new SvelteSet<string>();
  private generation = 0;
  private fetching = false;
  private started = false;
  /** agent_id -> the most recent locally-patched busy value, from an
   *  ordered Heart event. Always wins over whatever a `/studio/state`
   *  response says for that agent — not just a response already in flight
   *  when the patch landed, but ANY later one too: a terminal event
   *  (`agent.turn.done`/`failed`/`exhausted`) is appended to the Heart
   *  (and broadcast) before the HTTP handler that appended it returns and
   *  its `TurnLease` actually drops, so a `/studio/state` fetch issued
   *  after this client already saw and patched that event can still land
   *  on the server inside that gap and answer with the old `busy: true`
   *  (07d-A1 correction — see `reconcileBusyOverrides`, unit-tested in
   *  `busyReconcile.check.ts`). Cleared only by a newer patch for the same
   *  agent, or wholesale on WS reconnect (`ensureStarted`'s `onStatus`
   *  handler) — the one point where re-trusting a fresh snapshot over any
   *  held override is correct, since a dropped connection may have hidden
   *  live events entirely. */
  private busyOverrides = new Map<string, boolean>();
  private stopEvents: (() => void) | null = null;
  private stopBusyEvents: (() => void) | null = null;
  private stopGateEvents: (() => void) | null = null;
  private stopStatus: (() => void) | null = null;
  private gateRefreshTimer: ReturnType<typeof setTimeout> | null = null;
  private stopTaskPoll: (() => void) | null = null;

  sockets = $derived.by(() => {
    const d = this.data;
    if (!d) return [];
    return [...d.projects.flatMap((p) => p.sockets), ...d.ungrouped_sockets, ...d.factory_sockets];
  });
  activeSockets = $derived.by(() => this.sockets.filter((s) => !s.discarded));
  retiredSockets = $derived.by(() => this.sockets.filter((s) => s.discarded));
  activeIds = $derived.by(() => new Set(this.activeSockets.map((s) => s.stream_id)));

  /** Idempotent — every consumer may call this from its own `onMount`; only
   *  the first call does anything. Owns the one global topology
   *  subscription ("do not create another global socket-directory
   *  subscriber" — 07c-LIVE-STATE). */
  ensureStarted(): void {
    if (this.started) return;
    this.started = true;
    // The last snapshot at once (kept across restarts), so the window opens
    // on it rather than on "Connecting…"; the node's replaces it.
    const last = kept("studio");
    if (last && !this.data) this.data = last.data as StudioState;
    this.refresh();
    this.stopEvents = onEvent(null, (ev) => {
      if (TOPOLOGY_FAMILIES.has(ev.family)) this.refresh();
    });
    // Separate subscription, deliberately not folded into the topology one
    // above: that one refetches `/studio/state`, and a refetch here could
    // race the server's `TurnLease` drop and overwrite a correct local idle
    // transition with a transient stale `busy: true` (07d-A1 §3). Live busy
    // state is a pure local patch off ordered events; `/studio/state` stays
    // the bootstrap/reconnect snapshot only.
    this.stopBusyEvents = onEvent(null, (ev) => this.patchBusyFromEvent(ev));
    // A dropped WS can hide topology changes — including a busy-relevant
    // turn event — that landed while disconnected. Clear any held busy
    // override before refreshing: this is the one point where a fresh
    // `/studio/state` snapshot is meant to win outright (07d-A1 §3:
    // "/studio/state for bootstrap/reconnect"), same as the per-surface
    // fetches this store replaces used to do individually on reconnect.
    this.stopStatus = onStatus((status) => {
      if (status === "open") {
        this.busyOverrides.clear();
        this.refresh();
      }
    });
    // Gate/diff counters (`pending_gates`, `changed_added`, `changed_removed`)
    // and a conversation's status are part of `/studio/state`, but they are
    // not a topology fact. An edit proposal, an accept/reject, or a status
    // change still calls refresh(); the node returns the kept picture when
    // those inputs did not move. Debounced (not an immediate `refresh()` per
    // event) because a burst of `agent.edit.*` decisions — "accept all" —
    // would otherwise fire one HTTP request per event; `refresh()`'s own
    // generation/pump coalescing further collapses anything that lands
    // while a fetch is already in flight. The busy patch above stays a
    // separate, purely local path.
    this.stopGateEvents = onEvent(null, (ev) => {
      if (isGateRefreshFamily(ev.family) || isStatusRefreshFamily(ev.family)) this.scheduleGateRefresh();
    });
    // Bounded reconciliation cadence (spec: a terminal-created file, or any
    // other change with no Heart event at all, must move the Floor counters
    // "without a reload or synthetic event"). Only polls while it can
    // possibly matter — at least one task socket has a busy agent, or a
    // conversation still reads as working — so an idle Studio generates zero
    // extra traffic. Nor a hidden window: it asks once as it shows again.
    this.stopTaskPoll = whileShown(() => {
      if (hasBusyTaskSocket(this.sockets) || hasWorkingConversation(this.sockets)) this.refresh();
    }, 4000);
  }

  private scheduleGateRefresh(): void {
    if (this.gateRefreshTimer) clearTimeout(this.gateRefreshTimer);
    this.gateRefreshTimer = setTimeout(() => {
      this.gateRefreshTimer = null;
      this.refresh();
    }, 400);
  }

  findSocket(streamId: string): SocketInfo | null {
    return this.sockets.find((s) => s.stream_id === streamId) ?? null;
  }

  /** One agent's `busy` fact, patched in place from an ordered Heart event —
   *  `agent.turn.started` sets it, a turn-ending event (`agent.turn.done`,
   *  `agent.turn.failed`, `agent.turn.exhausted`, or the turn's completed
   *  edit proposal `agent.file.changed`) clears it, `agent.tool.called` (and
   *  everything else) leaves it untouched. Second Studio pages and reopened
   *  panes see the same transition because every open tab shares this one
   *  store's event subscription — no per-pane state to reconcile. A send that
   *  is answered with its turn's start before the socket has said so calls it
   *  with that event too (`AgentConversation.send`). */
  patchBusyFromEvent(ev: BakeryEvent): void {
    const busy = ev.family === "agent.turn.started" ? true : TURN_TERMINAL_FAMILIES.has(ev.family) ? false : null;
    if (busy === null) return;
    const agentId = payloadOf(ev).agent_id;
    if (typeof agentId !== "string" || !agentId) return;
    this.busyOverrides.set(agentId, busy);
    const agent = this.findSocket(ev.stream_id)?.agents.find((a) => a.agent_id === agentId);
    if (agent) agent.busy = busy;
  }

  isRetiring(streamId: string): boolean {
    return this.retiringIds.has(streamId);
  }

  /** Bumps the generation and ensures a fetch is running. Coalesces: a
   *  refresh requested while one is already in flight does not start a
   *  second HTTP request — the in-flight request's own completion handler
   *  notices the generation moved and refetches once, picking up
   *  everything that changed while it was in the air. The generation check
   *  is also what makes a delayed/stale response unable to resurrect a
   *  socket a newer discard already retired: `discard()` bumps the
   *  generation the instant it commits locally, so an older in-flight
   *  response is discarded on arrival instead of overwriting `data` — same
   *  class of guard as DockviewHost's `loadGeneration`. */
  refresh(): void {
    this.generation++;
    this.pump();
  }

  private pump(): void {
    if (this.fetching) return;
    const gen = this.generation;
    this.fetching = true;
    // The first fetch is the one the floor waits on. A later answer with
    // the same revision must not flip loading or replace `data`: that
    // redraws a picture the window already shows. `fresh` stays false
    // until that first answer lands, so a kept snapshot whose revision
    // happens to match a restarted node is still replaced once.
    if (!this.fresh) this.loading = true;
    node
      .studioState()
      .then((fresh) => {
        if (gen !== this.generation) return; // superseded — discard this stale response
        if (this.fresh && fresh.revision != null && fresh.revision === this.data?.revision) {
          if (this.error) this.error = "";
          return;
        }
        // 07d-A1 correction: a held busy override always wins over this
        // response's own value for that agent — see `busyOverrides`'s own
        // doc comment for why "issued after the patch" is not a safe
        // enough condition to trust the fetch instead.
        reconcileBusyOverrides(fresh, this.busyOverrides);
        this.data = fresh;
        this.fresh = true;
        this.error = "";
        keep("studio", fresh);
      })
      .catch((e) => {
        if (gen === this.generation) this.error = `${e}`;
      })
      .finally(() => {
        this.fetching = false;
        if (gen !== this.generation) this.pump(); // something changed mid-flight — refetch
        else if (this.loading) this.loading = false;
      });
  }

  /** Explicit, reversible-until-success discard lifecycle:
   *  1. Mark retiring locally — callers disable repeated clicks off `isRetiring`.
   *  2. On success, commit `discarded: true` on the local copy immediately
   *     (every active/retired derived view updates in this same tick, not
   *     waiting on a refetch), bump the generation so no in-flight fetch
   *     started before this can silently undo it, then kick a real refetch
   *     to reconcile with server truth — idempotent with the local commit;
   *     the Heart-driven `stream.discarded` event will also trigger one via
   *     `ensureStarted`'s subscription, this just does not wait for that
   *     round-trip.
   *  3. On failure, clear retiring and leave the socket exactly as it
   *     was — a failed discard must never look like it succeeded. */
  async discard(streamId: string, confirm = false): Promise<DiscardResp> {
    this.retiringIds.add(streamId);
    try {
      const resp = await node.discardStream(streamId, confirm);
      const s = this.findSocket(streamId);
      if (s) {
        s.discarded = true;
        s.state = "retired";
      }
      this.refresh();
      return resp;
    } finally {
      this.retiringIds.delete(streamId);
    }
  }
}

export const studioStore = new StudioStore();
