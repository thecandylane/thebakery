// Pure reconciliation logic for studioStore's busy-patch/refetch race
// (07d-A1 correction): a `/studio/state` response can never be trusted over
// a live, event-driven busy patch — not even one issued after the patch.
//
// Why "issued after" is not good enough: `agent.turn.started`/`done`/
// `failed`/`exhausted` land on the Heart (and broadcast over the event
// socket) BEFORE the HTTP handler that appended them returns and its
// `TurnLease` actually drops — the terminal event can reach this client,
// and a fresh `/studio/state` fetch can be issued and answered by the
// server, all inside that gap, and that answer still reports the old
// `busy: true`. There is no client-observable signal for "the lease has
// now really dropped," so a patched value must simply always win over
// whatever a fetch says, for as long as it is the most recent thing this
// client knows. It is retired only by a newer patch for the same agent
// (a real subsequent transition) or an explicit reset (`studioStore`'s own
// WS-reconnect handling — the one point where re-trusting a fresh snapshot
// is intentional, since a dropped connection may have hidden live events).
//
// Extracted from studioStore.svelte.ts (a Svelte runes file — `$state` is
// compiler syntax, not valid plain JS/TS, so it cannot be exercised by a
// framework-free `node` script) so this algorithm has a deterministic,
// dependency-free test (`busyReconcile.check.ts`).
import type { StudioState } from "./node.ts";

/**
 * Mutates `fresh` in place: every agent with a recorded override has
 * `fresh`'s own busy value overwritten with the override's, unconditionally
 * — a fetch response is never authoritative over a live patch, regardless
 * of when the fetch was issued relative to it (see module doc for why).
 */
export function reconcileBusyOverrides(fresh: StudioState, overrides: Map<string, boolean>): void {
  const allSockets = [
    ...fresh.projects.flatMap((p) => p.sockets),
    ...fresh.ungrouped_sockets,
    ...fresh.factory_sockets,
  ];
  for (const s of allSockets) {
    for (const a of s.agents) {
      if (overrides.has(a.agent_id)) a.busy = overrides.get(a.agent_id)!;
    }
  }
}
