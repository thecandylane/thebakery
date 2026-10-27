// The phase each conversation's running turn is in, for its glyph (thinking,
// writing or working). The node already pushes it: every phase change of
// every stream's turn rides the one events socket as an activity frame. So
// this keeps a small table of those frames, not a source per thread, and a
// conversation shows its phase whether or not anything has it open. Pure and
// DOM-free: livePhase.check.ts runs it on node.

import { type Phase, phaseOf } from "./threadStates.ts";

/** The part of the node's activity frame (`AgentActivity`) this reads. */
export type ActivityFrame = { agent_id: string; stream_id: string; phase: { phase: string } };

/** What each agent with a turn running is doing, and when that began (a
 *  count of changes), so a conversation of several agents shows the newest. */
export type Phases = { agents: ReadonlyMap<string, { stream: string; phase: Phase; at: number }>; changes: number };

export const NONE: Phases = { agents: new Map(), changes: 0 };

/** The table after a frame: the agent's phase, or nothing when the frame
 *  shows no turn running (waiting on you, finished, failed, stopped). The
 *  same table when that is what it already said. */
export function applyFrame(p: Phases, f: ActivityFrame): Phases {
  const phase = phaseOf(f.phase.phase);
  if ((p.agents.get(f.agent_id)?.phase ?? null) === phase) return p;
  const agents = new Map(p.agents);
  if (phase) agents.set(f.agent_id, { stream: f.stream_id, phase, at: p.changes + 1 });
  else agents.delete(f.agent_id);
  return { agents, changes: p.changes + 1 };
}

/** The phase a conversation's glyph shows: its newest running agent's. */
export function livePhase(p: Phases, streamId: string): Phase | null {
  let newest: { phase: Phase; at: number } | null = null;
  for (const e of p.agents.values()) if (e.stream === streamId && (!newest || e.at > newest.at)) newest = e;
  return newest?.phase ?? null;
}

/** The table, and who watches it, one conversation at a time: a watcher is
 *  called only when what its conversation's glyph shows changes. */
export function createLivePhases() {
  let table = NONE;
  // Frames pushed so far, per stream: an answer to a question asked before
  // one landed is older than that frame.
  const pushed = new Map<string, number>();
  const watchers = new Map<string, Set<() => void>>();

  function take(f: ActivityFrame) {
    const before = livePhase(table, f.stream_id);
    table = applyFrame(table, f);
    if (livePhase(table, f.stream_id) === before) return;
    for (const watch of watchers.get(f.stream_id) ?? []) watch();
  }

  return {
    /** What a conversation's glyph shows now. */
    get: (streamId: string) => livePhase(table, streamId),
    /** A frame the node pushed. */
    push(f: ActivityFrame) {
      pushed.set(f.stream_id, (pushed.get(f.stream_id) ?? 0) + 1);
      take(f);
    },
    /** For a window that opened, or reconnected, mid-turn, which no push has
     *  told yet: asks the node where a conversation's turn stands (`ask`).
     *  A push that lands while it answers is newer, and wins. */
    seed(streamId: string, ask: () => Promise<ActivityFrame | null>): Promise<void> {
      const mark = pushed.get(streamId) ?? 0;
      return ask().then(
        (f) => {
          if (f && (pushed.get(streamId) ?? 0) === mark) take(f);
        },
        () => {},
      );
    },
    watch(streamId: string, cb: () => void): () => void {
      const set = watchers.get(streamId) ?? new Set();
      set.add(cb);
      watchers.set(streamId, set);
      return () => {
        set.delete(cb);
        if (set.size === 0 && watchers.get(streamId) === set) watchers.delete(streamId);
      };
    },
  };
}
