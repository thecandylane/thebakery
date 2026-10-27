import type { AgentInfo, SocketInfo, StudioState } from "./node.ts";
import { reconcileBusyOverrides } from "./busyReconcile.ts";

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

function socket(agents: AgentInfo[]): SocketInfo {
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
    agents,
    available_models: [],
    project_id: null,
    root_id: null,
    pending_gates: 0,
    changed_added: 0,
    changed_removed: 0,
  };
}

function studioState(sockets: SocketInfo[]): StudioState {
  return {
    model_options: [],
    role_options: [],
    projects: [],
    ungrouped_sockets: sockets,
    factory_sockets: [],
  };
}

let failures = 0;

function assert(label: string, cond: boolean) {
  if (!cond) {
    console.error(`FAIL: ${label}`);
    failures++;
  } else {
    console.log(`pass: ${label}`);
  }
}

// The original reported race: a fetch was already in flight (issued before
// the turn began) when agent.turn.started patched busy=true — the fetch's
// own, necessarily-stale response still says busy=false. Reconciling must
// not let that stale response win.
{
  const fresh = studioState([socket([agent({ busy: false })])]);
  const overrides = new Map<string, boolean>([["wt:test:orchestrator", true]]);
  reconcileBusyOverrides(fresh, overrides);
  assert("a fetch in flight before a busy-true patch does not overwrite it", fresh.ungrouped_sockets[0].agents[0].busy === true);
}

// 07d-A1 correction, the exact reported gap: agent.turn.failed/done/... is
// appended to the Heart (and broadcast) BEFORE the HTTP handler that
// appended it returns and its TurnLease actually drops. A /studio/state
// fetch issued by this client AFTER it already received and patched that
// terminal event can still reach the server inside that gap and answer
// with the stale, pre-drop busy: true. There is no client-observable
// signal for "the lease has now really dropped," so the override must win
// regardless of the fetch's own timing relative to the patch — trusting
// "issued after the patch" (the earlier, incorrect assumption this test
// used to encode) would leave the UI stuck busy.
{
  const fresh = studioState([socket([agent({ busy: true })])]); // the racing fetch's stale, pre-lease-drop answer
  const overrides = new Map<string, boolean>([["wt:test:orchestrator", false]]); // this client's own terminal-event patch
  reconcileBusyOverrides(fresh, overrides);
  assert(
    "a fetch issued after a terminal-event patch still does not overwrite it (TurnLease may not have dropped yet)",
    fresh.ungrouped_sockets[0].agents[0].busy === false,
  );
  assert("the override survives reconciliation — only a newer patch or an explicit reset retires it", overrides.get("wt:test:orchestrator") === false);
}

// No override recorded for this agent at all — fresh's value passes
// through completely unchanged.
{
  const fresh = studioState([socket([agent({ busy: true })])]);
  const overrides = new Map<string, boolean>();
  reconcileBusyOverrides(fresh, overrides);
  assert("no override leaves fresh's own value untouched", fresh.ungrouped_sockets[0].agents[0].busy === true);
}

// A recorded override only ever touches the agent it names — a second
// agent on the same (or another) socket is unaffected.
{
  const fresh = studioState([
    socket([
      agent({ agent_id: "wt:test:orchestrator", role_id: "orchestrator", busy: false }),
      agent({ agent_id: "wt:test:worker", role_id: "worker", busy: false }),
    ]),
  ]);
  const overrides = new Map<string, boolean>([["wt:test:orchestrator", true]]);
  reconcileBusyOverrides(fresh, overrides);
  const agents = fresh.ungrouped_sockets[0].agents;
  assert("the named agent's override applies", agents.find((a) => a.agent_id === "wt:test:orchestrator")!.busy === true);
  assert("a different agent on the same socket is unaffected", agents.find((a) => a.agent_id === "wt:test:worker")!.busy === false);
}

if (failures > 0) throw new Error(`${failures} busyReconcile check failure(s)`);
