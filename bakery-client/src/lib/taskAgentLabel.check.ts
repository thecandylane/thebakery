import type { AgentInfo } from "./node.ts";
import { taskDriverAgent, taskAgentChipText } from "./taskAgentLabel.ts";

let failures = 0;
function assert(label: string, cond: boolean) {
  if (!cond) {
    console.error(`FAIL: ${label}`);
    failures++;
  } else {
    console.log(`pass: ${label}`);
  }
}

function agent(opts: Partial<AgentInfo>): AgentInfo {
  return {
    agent_id: "wt:test:x",
    role_id: "orchestrator",
    role_label: "Orchestrator",
    assigned_model_id: null,
    socket_ready: false,
    busy: false,
    ...opts,
  };
}

// task_driver.rs always runs a task's turns as Seat::Orchestrator — that
// agent must be picked even when it is not first in the list.
{
  const s = {
    agents: [
      agent({ agent_id: "w", role_id: "worker", role_label: "Worker", assigned_model_id: "claude" }),
      agent({ agent_id: "o", role_id: "orchestrator", role_label: "Orchestrator", assigned_model_id: "grok" }),
    ],
  };
  const picked = taskDriverAgent(s);
  assert("picks the orchestrator seat even when listed second", picked?.agent_id === "o");
  assert("chip text names the orchestrator's model", taskAgentChipText(s) === "Orchestrator · grok");
}

// no orchestrator present (malformed/partial data) — falls back to the
// first agent rather than going blank.
{
  const s = { agents: [agent({ agent_id: "w", role_id: "worker", role_label: "Worker", assigned_model_id: "claude" })] };
  assert("falls back to the only agent when no orchestrator seat exists", taskDriverAgent(s)?.agent_id === "w");
  assert("chip text still renders", taskAgentChipText(s) === "Worker · claude");
}

// unassigned model still renders a visible chip, never blank
{
  const s = { agents: [agent({ role_id: "orchestrator", role_label: "Orchestrator", assigned_model_id: null })] };
  assert("unassigned model renders explicitly", taskAgentChipText(s) === "Orchestrator · unassigned");
}

// no agents configured at all
{
  const s = { agents: [] as AgentInfo[] };
  assert("no agents => null", taskDriverAgent(s) === null);
  assert("no agents => explicit chip text, not blank", taskAgentChipText(s) === "no agent configured");
}

if (failures > 0) throw new Error(`${failures} taskAgentLabel check failure(s)`);
