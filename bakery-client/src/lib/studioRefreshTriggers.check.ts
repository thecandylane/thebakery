import type { AgentInfo, SocketInfo } from "./node.ts";
import {
  isGateRefreshFamily,
  isStatusRefreshFamily,
  hasBusyTaskSocket,
  hasWorkingConversation,
} from "./studioRefreshTriggers.ts";

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
    agent_id: "wt:test:orchestrator",
    role_id: "orchestrator",
    role_label: "Orchestrator",
    assigned_model_id: "grok",
    socket_ready: true,
    busy: false,
    ...opts,
  };
}

function socket(opts: Partial<SocketInfo>): SocketInfo {
  return {
    stream_id: "wt:test",
    name: "test",
    project: "test",
    repo_root: "/repo",
    branch: "main",
    worktree_path: "/repo",
    exists_on_disk: true,
    in_place: false,
    owner_node_id: null,
    last_ts: "2026-01-01T00:00:00Z",
    events: 1,
    lifetime: "slice",
    discarded: false,
    state: "active",
    agents: [],
    available_models: [],
    project_id: null,
    root_id: null,
    pending_gates: 0,
    changed_added: 0,
    changed_removed: 0,
    ...opts,
  };
}

// --- isGateRefreshFamily ---

assert("agent.file.changed schedules a gate refresh", isGateRefreshFamily("agent.file.changed"));
assert("agent.edit.accepted schedules a gate refresh", isGateRefreshFamily("agent.edit.accepted"));
assert("agent.edit.rejected schedules a gate refresh", isGateRefreshFamily("agent.edit.rejected"));
assert("agent.approval.cancelled schedules a gate refresh", isGateRefreshFamily("agent.approval.cancelled"));
assert("an unrelated family does not", !isGateRefreshFamily("agent.tool.called"));
assert("a family merely prefixed similarly does not false-positive", !isGateRefreshFamily("agent.editorial.note"));

// --- isStatusRefreshFamily ---

for (const family of [
  "agent.ask.pending",
  "agent.ask.answered",
  "agent.child_tool.pending",
  "agent.child_tool.decided",
  "agent.turn.started",
  "agent.turn.done",
  "agent.turn.failed",
  "agent.turn.exhausted",
  "agent.turn.cancelled",
  "agent.turn.retrying",
  "network.host.ask",
  "network.host.decided",
  "toolchain.install.ask",
  "toolchain.install.decided",
  "file.read.ask",
  "file.read.decided",
]) {
  assert(`${family} schedules a status refresh`, isStatusRefreshFamily(family));
}
assert("an unrelated family does not schedule a status refresh", !isStatusRefreshFamily("agent.tool.called"));
assert("a family merely prefixed similarly does not false-positive", !isStatusRefreshFamily("agent.turn.started.extra"));

// --- hasBusyTaskSocket ---

{
  const sockets = [socket({ state: "task", agents: [agent({ busy: true })] })];
  assert("a task socket with a busy agent is detected", hasBusyTaskSocket(sockets));
}
{
  const sockets = [socket({ state: "task", agents: [agent({ busy: false })] })];
  assert("a task socket with no busy agent is not", !hasBusyTaskSocket(sockets));
}
{
  const sockets = [socket({ state: "active", agents: [agent({ busy: true })] })];
  assert("a busy agent on a non-task socket does not count", !hasBusyTaskSocket(sockets));
}
{
  const sockets = [socket({ state: "task", discarded: true, agents: [agent({ busy: true })] })];
  assert("a discarded task socket does not count even if busy", !hasBusyTaskSocket(sockets));
}
{
  assert("no sockets at all is false", !hasBusyTaskSocket([]));
}

// --- hasWorkingConversation ---

{
  const sockets = [socket({ status: "working" })];
  assert("an active conversation reading as working is detected", hasWorkingConversation(sockets));
}
{
  const sockets = [socket({ status: "needs_input" }), socket({ status: "done" })];
  assert("conversations that are not working do not count", !hasWorkingConversation(sockets));
}
{
  const sockets = [socket({ status: "working", discarded: true })];
  assert("a discarded conversation does not count even if it reads as working", !hasWorkingConversation(sockets));
}
{
  assert("a node that sends no status is not working", !hasWorkingConversation([socket({})]));
}
{
  assert("no sockets at all is false", !hasWorkingConversation([]));
}

if (failures > 0) throw new Error(`${failures} studioRefreshTriggers check failure(s)`);
