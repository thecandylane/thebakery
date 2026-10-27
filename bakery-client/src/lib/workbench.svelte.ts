// Shared reactive state for one Studio socket — the single source Dockview's
// imperatively-mounted panels (DockviewHost.svelte's createComponent) read
// from via getters, since they live outside the normal Svelte component tree
// and can't receive props the usual top-down way. +page.svelte owns one
// instance per bound stream; every panel factory closure captures it.
//
// 07d-A-AGENT-IDENTITY: a socket now has one conversation + busy/run state
// PER CONFIGURED AGENT, not one shared thread for the whole socket. The raw
// Heart `events` array stays a single per-socket fetch/subscription (one
// history load, one WebSocket subscription) — `AgentConversation` derives
// its own filtered view from it via `eventsForAgent`, never a second fetch.
import { node, type AgentActivity, type BakeryEvent, type NodeInfo, type SocketInfo, type AgentInfo } from "./node";
import { onActivity, onEnvelope, onEvent, onStatus } from "./events";
import { untrack } from "svelte";
import { eventsForAgent, mergeEvents, summarize, toTranscript } from "./transcript";
import { studioStore } from "./studioStore.svelte";

/** A history read waits this long before it is asked for again: a slow or
 *  restarting node, or a filter holding the first connection, is waited out. */
const HIST_ATTEMPT_MS = 30_000;
/** Between reads that failed, longer apart each time; it never gives up
 *  while the stream is open. */
const HIST_RETRY_MS = [1000, 2000, 4000, 8000, 15_000, 30_000];

/** The result of one `AgentConversation.send` — see the method's own doc. */
export type SendOutcome = { ok: true } | { ok: false; error: string };

/** One configured agent's own conversation + busy/run state (07d-A "Law": one
 *  conversation + busy/run per agent_id, not per pane and not per socket).
 *  Outlives any Chat pane bound to it — closing the pane only unmounts the
 *  component; this object (and any in-flight `send()`) lives on inside the
 *  owning `WorkbenchState.conversations` map, so reopening the pane finds
 *  the same history and the same in-flight turn, not a fresh one. */
export class AgentConversation {
  readonly agentId: string;
  readonly roleId: string;
  private workbench: WorkbenchState;

  /** True while THIS tab is committing a durable queue record. This is only
   * a short network request now; server `busy` remains the turn truth. */
  private sendingLocally = $state(false);
  /** The message this tab is sending to the agent, until its send is
   *  answered: its words, and where the stream stood when it began. A copy of
   *  it the event socket brings in meanwhile is a turn about to start, not a
   *  message waiting (`deriveComposerControl`). */
  sending: { text: string; after: number } | null = $state.raw(null);
  notice = $state("");

  private serverAgentInfo: AgentInfo | null = $derived.by(
    () => this.workbench.socket?.agents.find((a) => a.agent_id === this.agentId) ?? null,
  );
  /** Local flag OR the server's own per-agent projection — the second half
   *  covers a turn that was already running before this tab loaded (a
   *  reload mid-turn, or another client/tab started it): 07d-A "Studio:
   *  /studio/state agents expose ... busy". */
  busy = $derived(this.sendingLocally || (this.serverAgentInfo?.busy ?? false));

  /** The last list `events` gave: an event for another agent on this socket
   *  leaves this agent's list the same, so its transcript is not rebuilt. */
  private ownEvents: BakeryEvent[] = [];
  events = $derived.by(() => {
    const next = eventsForAgent(this.workbench.events, this.agentId);
    const last = this.ownEvents;
    if (next.length === last.length && next.every((ev, i) => ev === last[i])) return last;
    this.ownEvents = next;
    return next;
  });
  items = $derived.by(() => toTranscript(this.events));
  progress = $derived.by(() => summarize(this.items));

  /** Server-owned live work-trace snapshot (07d-A5) — bootstrapped once via
   *  `node.activity`, kept live via `onActivity`. `null` before the
   *  bootstrap fetch resolves, or once resolved, whenever no turn has ever
   *  run for this agent. Additive to `busy` above, not a replacement — the
   *  existing busy/`busyOverrides` mechanism is untouched. */
  activity: AgentActivity | null = $state(null);
  /** Bumped on every genuine event-socket reconnect. A reconnect can mean
   *  the node restarted and every native session died with it, so views of
   *  session state refetch on it rather than wait for a click. */
  reconnects = $state(0);
  /** The two genuinely new raw subscriptions on this class (everything else
   *  derives reactively off `workbench.events`) — released explicitly by
   *  `dispose()`, called from `WorkbenchState.bindTo`/`unbind` before the
   *  conversation map is cleared, or they leak across a stream rebind. */
  private stopActivity: (() => void) | null = null;
  private stopStatus: (() => void) | null = null;
  /** Bumped on every real `onActivity` push or local Stop transition for
   *  this agent — lets `refreshActivity` detect whether a fresher snapshot landed while
   *  its own `GET /agent/activity` fetch was still in flight, so a slow,
   *  now-stale HTTP response (bootstrap OR reconnect — a genuine reconnect
   *  has exactly the same race, not just construction-time duplication)
   *  can never clobber it (07d-A5 fix-verification review, round 2). */
  private activitySeq = 0;

  constructor(workbench: WorkbenchState, agentId: string, roleId: string) {
    this.workbench = workbench;
    this.agentId = agentId;
    this.roleId = roleId;

    // Listening by name makes this window follow the stream (whole frames,
    // not the slim ones every-stream listeners hear); unbound, there is no
    // turn of its own to hear.
    const streamId = this.workbench.streamId;
    this.stopActivity =
      streamId === null
        ? null
        : onActivity(streamId, (a) => {
            if (a.agent_id === this.agentId) {
              this.activitySeq++;
              this.activity = a;
            }
          });
    this.refreshActivity();
    // A dropped WS can hide an activity push entirely (a terminal
    // transition broadcast while disconnected) — without this, a stale
    // in-flight snapshot, and its armed Cancel button, would otherwise
    // persist until a full page reload (07d-A5 review #8). Mirrors
    // `studioStore`'s own reconnect-resync pattern (`ensureStarted`'s
    // `onStatus` handler) — with one addition `ensureStarted` never needed:
    // `onStatus`'s callback fires synchronously with the CURRENT status the
    // instant it is subscribed, not only on a future transition.
    // `ensureStarted` is a app-lifetime singleton, so that one extra replay
    // is harmless; `AgentConversation` is constructed on every Chat-panel
    // (re)open, typically while the WS is already "open" (bindTo connects
    // it well before any panel mounts) — without skipping that first
    // replay, every single construction would fire a spurious "reconnect"
    // refetch racing the real bootstrap fetch above. Only a genuine later
    // transition should ever trigger a resync.
    let firstStatusReplay = true;
    this.stopStatus = onStatus((status) => {
      if (firstStatusReplay) {
        firstStatusReplay = false;
        return;
      }
      if (status === "open") {
        this.reconnects++;
        this.refreshActivity();
      }
    });
  }

  /** Bootstraps (or re-syncs, on WS reconnect) `activity` from `GET /agent/
   *  activity`. Applies the response only if `activitySeq` has not advanced
   *  since the fetch started — i.e. no live `onActivity` push landed while
   *  it was in flight. This covers both callers uniformly: on first
   *  bootstrap, a push that arrives first has already set the correct
   *  value and must not be clobbered by a slower, now-stale fetch response;
   *  on a genuine reconnect, the fresh fetch is only trustworthy if nothing
   *  newer has since arrived over the (now reconnected) socket — an
   *  unconditional overwrite here was a real, confirmed race (07d-A5
   *  fix-verification review, round 2): a delayed GET resolving after a
   *  newer WS push would otherwise silently revert `activity` to a stale
   *  snapshot. */
  private refreshActivity() {
    const streamId = this.workbench.streamId;
    if (!streamId) return;
    const seqAtStart = this.activitySeq;
    void node
      .activity(streamId, this.roleId)
      .then((a) => {
        if (this.activitySeq !== seqAtStart) return;
        this.activity = a;
      })
      .catch(() => {});
  }

  /** Tears down the `onActivity`/`onStatus` subscriptions — see the field
   *  doc above. */
  dispose() {
    this.stopActivity?.();
    this.stopActivity = null;
    this.stopStatus?.();
    this.stopStatus = null;
  }

  /** Whether the instruction was actually accepted by the node. Every side
   *  effect above is unchanged and existing callers may keep ignoring this;
   *  it exists so a composer can tell "refused — keep the human's text" from
   *  "accepted — clear the box" without re-reading `notice` and guessing
   *  (a turn that merely exhausted its steps also sets `notice`, and that is
   *  a completed send, not a refused one). */
  async send(
    instruction: string,
    filePath: string,
    attachmentIds: string[] = [],
    steer = false,
  ): Promise<SendOutcome> {
    const streamId = this.workbench.streamId;
    if (!streamId) return { ok: false, error: "No session selected" };
    this.notice = "";
    this.sendingLocally = true;
    if (!steer) this.sending = { text: instruction, after: this.workbench.events.at(-1)?.seq ?? -1 };
    try {
      // To an idle agent the answer waits for the turn to begin and holds its
      // start beside the queued event: both land at once, so the message goes
      // straight into the thread and never shows as queued.
      const { started, ...queued } = await node.queueTurn(streamId, instruction, filePath, this.roleId, attachmentIds, steer);
      if (this.workbench.streamId === streamId) {
        // If the event socket has not brought the start in yet, busy is set
        // from here, so it never blinks off between the answer and the socket.
        const heard = started != null && this.workbench.events.some((e) => e.event_hash === started.event_hash);
        this.workbench.add(queued, ...(started ? [started] : []));
        if (started && !heard) studioStore.patchBusyFromEvent(started);
      }
      this.notice = "";
      return { ok: true };
    } catch (e) {
      this.notice = `${e}`;
      return { ok: false, error: `${e}` };
    } finally {
      this.sending = null;
      this.sendingLocally = false;
    }
  }

  /** Requests that this agent's in-flight turn stop (07d-A5). Mark the
   *  current non-terminal activity before the HTTP round trip, so Stop
   *  immediately becomes “stopping…” even while the node is still settling
   *  an ACP cancellation or an approval wait. A failed/refused request puts
   *  back only this same turn's prior snapshot; it can never clobber a newer
   *  activity push. */
  async requestStop(streamId = this.workbench.streamId) {
    if (!streamId) throw new Error("No session selected");
    if (this.workbench.streamId !== streamId) throw new Error("Session changed before Stop could be sent");
    const before = this.activity;
    const live =
      before != null &&
      before.phase.phase !== "complete" &&
      before.phase.phase !== "error" &&
      before.phase.phase !== "cancelled";
    if (before && live && !before.cancel_requested) {
      this.activitySeq++;
      this.activity = { ...before, cancel_requested: true };
    }
    try {
      const result = await node.cancelTurn(streamId, this.roleId);
      if (!result.cancelled && live && this.activity?.turn_id === before?.turn_id && this.activity.cancel_requested) {
        this.activitySeq++;
        this.activity = before;
      }
      return result;
    } catch (error) {
      if (live && this.activity?.turn_id === before?.turn_id && this.activity.cancel_requested) {
        this.activitySeq++;
        this.activity = before;
      }
      throw error;
    }
  }

  /** Compatibility wrapper for non-React callers. The Composer uses
   *  `requestStop` directly so it can preserve a request error as an action
   *  outcome rather than hide it in this conversation-local notice. */
  async cancel() {
    try {
      await this.requestStop();
    } catch (e) {
      this.notice = `${e}`;
    }
  }
}

/** How many streams keep their history warm while no page shows them (you
 *  went to Settings, the Floor, another project). Coming back renders at
 *  once from what is already here — no refetch, no scroll from the top.
 *  ponytail: a fixed count; make it size-based if histories get huge. */
const WARM_STREAMS = 24;
const NO_EVENTS: BakeryEvent[] = [];

/** One stream's durable history, shared by every view that shows it and
 *  kept live (subscribed) while none does, up to `WARM_STREAMS`. */
class StreamHistory {
  /** Raw, not deeply reactive: events are immutable records, so only a new
   *  list needs to notify — no proxy per event to build or walk. */
  events: BakeryEvent[] = $state.raw([]);
  loading = $state(false);
  error = $state("");
  /** Views bound to this stream right now; only an unused one is evicted. */
  users = 0;
  private seen = new Set<string>();
  private stopEvents: () => void;
  private stopStatus: () => void;
  private closed = false;
  /** The read in flight: a gap or a reconnect while one runs joins it. */
  private reading: Promise<void> | null = null;

  constructor(readonly id: string) {
    this.stopEvents = onEvent(id, this.add);
    // A dropped socket can miss events, so after a genuine reconnect fetch
    // again (deduplicated) and a warm history never goes quietly stale.
    // `onStatus` replays the current status once on subscribe — skip that.
    let replay = true;
    this.stopStatus = onStatus((status) => {
      if (replay) {
        replay = false;
        return;
      }
      if (status === "open") void this.load();
    });
    void this.load();
  }

  add = (ev: BakeryEvent) => this.addAll([ev]);

  /** Several events as one new list, so nothing renders between them. */
  addAll(evs: BakeryEvent[]) {
    // A stream's events number on from 0 without a hole, so one past the
    // next we hold means the socket missed some: read the history again.
    let last = this.events.at(-1)?.seq;
    let missed = false;
    for (const ev of [...evs].sort((a, b) => a.seq - b.seq)) {
      if (last !== undefined && ev.seq > last + 1) missed = true;
      last = Math.max(last ?? ev.seq, ev.seq);
    }
    const merged = mergeEvents(this.events, evs, this.seen);
    if (merged) this.events = merged;
    if (missed) void this.load();
  }

  load(): Promise<void> {
    this.reading ??= this.read().finally(() => {
      this.reading = null;
    });
    return this.reading;
  }

  private async read() {
    // Only a first load shows as loading; refreshing a history that is
    // already on screen must not flash the transcript.
    const first = this.events.length === 0;
    if (first) this.loading = true;
    for (let attempt = 0; !this.closed; attempt++) {
      try {
        const history = await node.history(this.id, { signal: AbortSignal.timeout(HIST_ATTEMPT_MS) });
        const merged = mergeEvents(this.events, history, this.seen);
        if (merged) this.events = merged;
        this.error = "";
        break;
      } catch (e) {
        // Said while it keeps trying, so a slow node reads as one.
        if (first) this.error = `${e}`;
        await new Promise((r) => setTimeout(r, HIST_RETRY_MS[Math.min(attempt, HIST_RETRY_MS.length - 1)]));
      }
    }
    if (first) this.loading = false;
  }

  close() {
    this.closed = true;
    this.stopEvents();
    this.stopStatus();
  }
}

/** Least recently used first (Map keeps insertion order). */
const warm = new Map<string, StreamHistory>();

/** Heard once a history is first kept: the node says its socket fell behind
 *  and skipped events for this window, so every history kept here reads
 *  again. */
let hearingLag = false;

function acquireHistory(id: string): StreamHistory {
  if (!hearingLag) {
    hearingLag = true;
    onEnvelope("events-lagged", () => {
      for (const kept of warm.values()) void kept.load();
    });
  }
  let h = warm.get(id);
  if (h) warm.delete(id);
  else h = new StreamHistory(id);
  warm.set(id, h);
  h.users++;
  for (const [key, old] of warm) {
    if (warm.size <= WARM_STREAMS) break;
    if (old.users > 0) continue;
    warm.delete(key);
    old.close();
  }
  return h;
}

function releaseHistory(h: StreamHistory) {
  h.users = Math.max(0, h.users - 1);
}

/** Start loading a conversation's history before it is opened (a hovered
 *  sidebar row, the most recent conversations) and keep it warm, so opening
 *  it never shows "loading history". Cheap when it is already warm. */
export function warmHistory(id: string) {
  untrack(() => releaseHistory(acquireHistory(id)));
}

export class WorkbenchState {
  streamId: string | null = $state(null);
  info: NodeInfo | null = $state(null);
  /** This stream's history — shared, and kept warm across page changes. */
  private history: StreamHistory | null = $state(null);
  get events(): BakeryEvent[] {
    return this.history?.events ?? NO_EVENTS;
  }
  get histLoading(): boolean {
    return this.history?.loading ?? false;
  }
  get histError(): string {
    return this.history?.error ?? "";
  }
  /** Decide (accept/reject) in-flight — Review is one socket-wide queue, not
   *  per-agent, so this stays here rather than on `AgentConversation`. */
  busy = $state(false);
  notice = $state("");
  selectedFile = $state("");

  /** Unfiltered, socket-wide transcript — Review (the pending-proposal
   *  queue) and Events are operational views across every configured
   *  agent, not one agent's own conversation, so they read this rather
   *  than an `AgentConversation`'s per-agent `items`. */
  items = $derived.by(() => toTranscript(this.events));
  progress = $derived.by(() => summarize(this.items));

  /** The one topology projection of this stream (07c-LIVE-STATE) — reads
   *  live off the canonical `studioStore`, so a discard or reassignment
   *  landing anywhere (another tab, this workbench's own "Discard" click)
   *  is reflected here in the same tick with no local refetch. */
  socket: SocketInfo | null = $derived.by(() =>
    this.streamId == null ? null : studioStore.findSocket(this.streamId),
  );
  modelOptions = $derived(studioStore.data?.model_options ?? []);
  /** Which sidebar group this socket belongs to — Dockview's only use of it
   *  is picking a default layout preset (STUDIO.md: "same Studio, different
   *  default layouts only"). `null` until studio state has loaded once.
   *  07d-A3: bucket membership itself is the "kind" now — a socket in
   *  `factory_sockets` is factory, everything else (Projects or Ungrouped)
   *  is project. */
  groupKind: "factory" | "project" | null = $derived.by(() => {
    if (this.streamId == null || studioStore.data == null) return null;
    if (studioStore.data.factory_sockets.some((s) => s.stream_id === this.streamId)) return "factory";
    const inProjects = studioStore.data.projects.some((p) => p.sockets.some((s) => s.stream_id === this.streamId));
    const inUngrouped = studioStore.data.ungrouped_sockets.some((s) => s.stream_id === this.streamId);
    return inProjects || inUngrouped ? "project" : null;
  });

  private boundId: string | null = null;
  /** Per-agent conversation state, keyed by `agent_id` — lazily created,
   *  cleared on `bindTo`/`unbind` (a previous stream's agents are gone). */
  private conversations = new Map<string, AgentConversation>();

  add = (...evs: BakeryEvent[]) => {
    this.history?.addAll(evs);
  };

  /** Get-or-create this agent's conversation state. `roleId` is only needed
   *  to seed a brand-new entry (turns are sent as this role); an existing
   *  entry's `roleId` never changes underneath a caller. */
  conversationFor(agentId: string, roleId: string): AgentConversation {
    let c = this.conversations.get(agentId);
    if (!c) {
      c = new AgentConversation(this, agentId, roleId);
      this.conversations.set(agentId, c);
    }
    return c;
  }

  async decide(hash: string, accept: boolean, newContent?: string) {
    if (!this.streamId) throw new Error("No session selected");
    const sid = this.streamId;
    this.notice = "";
    this.busy = true;
    try {
      const ev = await node.decide(sid, hash, accept, newContent);
      if (this.streamId === sid) this.add(ev);
    } catch (e) {
      if (this.streamId === sid) this.notice = `${e}`;
      throw e;
    } finally {
      if (this.streamId === sid) this.busy = false;
    }
  }

  /** Rebind to a (possibly new) stream id — mirrors the old +page.svelte
   *  `$effect`'s reset block exactly. No-op if already bound to `id`. */
  bindTo(id: string) {
    // Pages call this from an `$effect`. Nothing read in here may become that
    // effect's dependency, or the writes below re-run it — a chat page that
    // returns `unbind` as its cleanup then looped unbind → bind forever and
    // froze the app.
    untrack(() => {
      this.streamId = id;
      if (id === this.boundId) return;
      this.boundId = id;
      this.notice = "";
      this.selectedFile = "";
      this.busy = false;
      for (const c of this.conversations.values()) c.dispose();
      this.conversations = new Map();
      if (this.history) releaseHistory(this.history);
      this.history = acquireHistory(id);
      // Idempotent — owns the one global topology subscription; `socket` above
      // reads reactively off it, so no per-workbench refetch is needed here.
      studioStore.ensureStarted();
      void node
        .info()
        .then((nodeInfo) => {
          this.info = nodeInfo;
        })
        .catch(() => {});
    });
  }

  /** Leaves the stream's history warm for the next view that shows it. */
  unbind() {
    untrack(() => {
      if (this.history) releaseHistory(this.history);
      this.history = null;
      this.boundId = null;
      this.streamId = null;
      for (const c of this.conversations.values()) c.dispose();
      this.conversations = new Map();
    });
  }
}
