// Pure predicates behind studioStore's two extra `/studio/state` refresh
// triggers (07d-B Phase 4 fix). Kept in a plain, framework-free module —
// deliberately not inlined into `studioStore.svelte.ts` — so they can be
// unit-tested under plain `node` the same way `busyReconcile.ts` is (svelte
// runes only run through the Svelte compiler).
//
// Why these triggers exist: `pending_gates`, `changed_added`, and
// `changed_removed` on `SocketInfo` are recomputed by the server on every
// `/studio/state` call, but the client only ever refetched that snapshot on
// the `TOPOLOGY_FAMILIES` set (project/socket/seat/lifetime facts) plus a WS
// reconnect. A task worker proposing a write (`agent.file.changed`), a human
// deciding on it (`agent.edit.accepted`/`agent.edit.rejected`), or a
// terminal/editor writing directly into the task worktree with no synthetic
// event at all, never moved those counters — they sat frozen at whatever the
// last topology event left behind. Two independent fixes close this:
//
//  1. `isGateRefreshFamily` — any edit-gate family schedules a (debounced)
//     refresh, so an accept/reject/new-proposal event moves the counters
//     promptly without waiting for an unrelated topology event.
//  2. `hasBusyTaskSocket` — while at least one task socket has a busy agent,
//     a bounded polling cadence (studioStore's own timer) also calls
//     `refresh()`, so changes with no event at all (a terminal command, an
//     editor save) still move the counters within a few seconds, per the
//     spec's "moves the Floor counters without a reload or synthetic event"
//     acceptance item.
//
// A conversation's `status` / `status_note` ("Needs you", "Working", …) is the
// same kind of fact: derived server-side on every call, from things that carry
// no topology event. Without these two a row kept saying "Needs you" after the
// human had answered, until something unrelated refetched.
//
//  3. `isStatusRefreshFamily` — a question or native permission asked and then
//     answered or decided, and a turn starting or ending.
//  4. `hasWorkingConversation` — a turn's terminal event is broadcast just
//     before the server drops its lease, so the first refetch can still read
//     the old "working"; the bounded poll settles it.
import type { SocketInfo } from "./node.ts";

/** True for every Heart family that can change a socket's gate/diff counters
 *  (`pending_gates`, `changed_added`, `changed_removed`): a new edit
 *  proposal, a human's accept/reject decision, or Stop closing an active
 *  approval. Matches
 *  `agent.edit.*` by prefix — the family vocabulary (`agent.edit.accepted`,
 *  `agent.edit.rejected`, plus the distinct stop-settlement family) is the gate
 *  surface's own concern, not this predicate's to enumerate one by one. */
export function isGateRefreshFamily(family: string): boolean {
  return family === "agent.file.changed" || family.startsWith("agent.edit.") || family === "agent.approval.cancelled";
}

/** The families a conversation's derived status is read from
 *  (`conversation_status_locked`): the server's `AwaitingApproval` phase
 *  appearing and clearing, and the last turn event. File-edit and Stop
 *  decisions move it too, but they are already `isGateRefreshFamily`. */
const STATUS_REFRESH_FAMILIES: ReadonlySet<string> = new Set([
  "agent.ask.pending",
  "agent.ask.answered",
  "agent.child_tool.pending",
  "agent.child_tool.decided",
  "agent.turn.started",
  "agent.turn.done",
  "agent.turn.failed",
  "agent.turn.exhausted",
  "agent.turn.cancelled",
  // A turn waiting out a passing failure says what it waits on.
  "agent.turn.retrying",
  // A sandbox asking to reach a host needs the operator until they answer.
  "network.host.ask",
  "network.host.decided",
  // So does a Mac sandbox asking to install a toolchain.
  "toolchain.install.ask",
  "toolchain.install.decided",
  // And a room refused a secret-named file.
  "file.read.ask",
  "file.read.decided",
]);

/** True for every Heart family that can change a conversation's `status` /
 *  `status_note`. */
export function isStatusRefreshFamily(family: string): boolean {
  return STATUS_REFRESH_FAMILIES.has(family);
}

/** True while any active (non-discarded) task socket has at least one busy
 *  agent — the condition under which studioStore's bounded poll actually
 *  issues a `/studio/state` refresh, so idle projects generate zero extra
 *  traffic. */
export function hasBusyTaskSocket(sockets: SocketInfo[]): boolean {
  return sockets.some((s) => !s.discarded && s.state === "task" && s.agents.some((a) => a.busy));
}

/** True while any active conversation still reads as working — the other
 *  condition under which the bounded poll refreshes. */
export function hasWorkingConversation(sockets: SocketInfo[]): boolean {
  return sockets.some((s) => !s.discarded && s.status === "working");
}
