// Pure "which agent is driving this task, and how do we label it" helper
// (07d-B Phase 4 item 3 fix), shared by TaskRow.svelte (sidebar) and
// FactoryFloor.svelte's Tasks section so the two live task-row surfaces
// render the exact same agent/model chip instead of each guessing
// independently. Kept plain/framework-free so it is unit-testable under
// `node` (see busyReconcile.ts's identical constraint).
//
// A task socket's `agents` list carries every project role configured on
// it (Orchestrator, Worker — never Baker, which is factory-only), but
// `task_driver.rs` always issues a task's turns "as `Seat::Orchestrator` —
// the seat every task runs under" (its own doc comment on
// `run_task_turn`), regardless of which role a human eventually renames or
// reassigns. The Orchestrator seat's `AgentInfo` is therefore the one that
// answers "which agent/model is driving this task" — falling back to
// whatever's first only for a malformed/empty list, so the row never goes
// silently blank.
import type { AgentInfo, SocketInfo } from "./node.ts";

export function taskDriverAgent(s: Pick<SocketInfo, "agents">): AgentInfo | null {
  return s.agents.find((a) => a.role_id === "orchestrator") ?? s.agents[0] ?? null;
}

/** "Agent + model" text per Phase 4 item 3's row spec — always renders
 *  something (never blank) so a human watching several parallel tasks can
 *  tell which agent/model drives each one without opening the socket. */
export function taskAgentChipText(s: Pick<SocketInfo, "agents">): string {
  const a = taskDriverAgent(s);
  if (!a) return "no agent configured";
  return `${a.role_label} · ${a.assigned_model_id ?? "unassigned"}`;
}
