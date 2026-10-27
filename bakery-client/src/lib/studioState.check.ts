import type { AgentInfo, SocketInfo, ModelOption, SocketModelOption, ModelCatalog, ModelCatalogEntry } from "./node.ts";
import { deriveAvailableModels, deriveTargetSelection, effortLevelsForOverride } from "./studioTarget.ts";

function agent(opts: Partial<AgentInfo>): AgentInfo {
  return {
    agent_id: "wt:test:orchestrator",
    role_id: "orchestrator",
    role_label: "Orchestrator",
    assigned_model_id: null,
    socket_ready: false,
    busy: false,
    ...opts,
  };
}

function modelCandidate(opts: Partial<SocketModelOption>): SocketModelOption {
  return { id: "grok", ready: false, ...opts };
}

function socket(agents: AgentInfo[], available_models: SocketModelOption[] = []): SocketInfo {
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
    available_models,
    project_id: null,
    root_id: null,
    pending_gates: 0,
    changed_added: 0,
    changed_removed: 0,
  };
}

function modelOption(opts: Partial<ModelOption>): ModelOption {
  return { id: "grok", label: "Grok (xAI)", transport: "http", node_available: true, ...opts };
}

function catalogEntry(opts: Partial<ModelCatalogEntry>): ModelCatalogEntry {
  return { id: "claude", label: "Claude", effort_levels: [], ...opts };
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

// unassigned agents are not targets
{
  const result = deriveTargetSelection(
    socket([agent({ role_id: "orchestrator", assigned_model_id: null, socket_ready: false })]),
    null,
  );
  assert("unassigned agent is not a target", result.selectedRoleId === null);
  assert("unassigned agent disables send", !result.canSend);
}

// unavailable assignment is not a target
{
  const result = deriveTargetSelection(
    socket([agent({ role_id: "orchestrator", assigned_model_id: "claude", socket_ready: false, reason: "Setup needed: add ANTHROPIC_API_KEY" })]),
    null,
  );
  assert("unavailable assignment is not a target", result.selectedRoleId === null);
  assert("unavailable assignment disables send", !result.canSend);
}

// one ready model becomes the selected option
{
  const result = deriveTargetSelection(
    socket([
      agent({ role_id: "orchestrator", assigned_model_id: "grok", socket_ready: true }),
      agent({ role_id: "worker", assigned_model_id: null, socket_ready: false }),
    ]),
    null,
  );
  assert("one ready model is preselected", result.selectedRoleId === "orchestrator");
  assert("one ready model enables send", result.canSend);
}

// multiple ready models require explicit selection (no prior choice)
{
  const result = deriveTargetSelection(
    socket([
      agent({ role_id: "orchestrator", assigned_model_id: "grok", socket_ready: true }),
      agent({ role_id: "worker", assigned_model_id: "claude", socket_ready: true }),
    ]),
    null,
  );
  assert("multiple ready models require explicit selection", result.selectedRoleId === null);
  assert("multiple ready models do not auto-enable send", !result.canSend);
}

// multiple ready models: an explicit prior choice among them is preserved
{
  const result = deriveTargetSelection(
    socket([
      agent({ role_id: "orchestrator", assigned_model_id: "grok", socket_ready: true }),
      agent({ role_id: "worker", assigned_model_id: "claude", socket_ready: true }),
    ]),
    "worker",
  );
  assert("explicit prior choice among ready agents is preserved", result.selectedRoleId === "worker");
  assert("explicit prior choice enables send", result.canSend);
}

// a prior choice that is no longer ready is dropped, not silently kept
{
  const result = deriveTargetSelection(
    socket([
      agent({ role_id: "orchestrator", assigned_model_id: "grok", socket_ready: true }),
      agent({ role_id: "worker", assigned_model_id: null, socket_ready: false }),
    ]),
    "worker",
  );
  assert("stale prior choice falls back to the single ready agent", result.selectedRoleId === "orchestrator");
}

// no ready model disables assignment
{
  const result = deriveTargetSelection(
    socket([
      agent({ role_id: "orchestrator", assigned_model_id: "claude", socket_ready: false }),
      agent({ role_id: "worker", assigned_model_id: "acp", socket_ready: false }),
    ]),
    null,
  );
  assert("no ready model disables send", !result.canSend);
  assert("no ready model has no selection", result.selectedRoleId === null);
}

// null socket means no targets
{
  const result = deriveTargetSelection(null, null);
  assert("null socket produces no selection", result.selectedRoleId === null);
  assert("null socket disables send", !result.canSend);
}

// --- model picker: per-socket readiness, not node-wide availability ---

// ACP node-available but not ready on this socket (e.g. workspace missing
// from disk) is excluded — zero-config: a bare workspace with no mise.toml
// is still ready, so this covers a socket that fails for a different reason.
{
  const models = deriveAvailableModels(
    socket([], [
      modelCandidate({ id: "grok", ready: true }),
      modelCandidate({ id: "acp", ready: false, reason: "Socket workspace not found on disk" }),
    ]),
    [
      modelOption({ id: "grok", node_available: true }),
      modelOption({ id: "acp", label: "ACP (local CLI)", transport: "acp", node_available: true }),
    ],
  );
  assert("picker excludes ACP when this socket reports not-ready", !models.some((m) => m.id === "acp"));
  assert("picker keeps a socket-ready HTTP model", models.some((m) => m.id === "grok"));
}

// ACP ready on this socket (zero-config: no mise.toml required) is offered
{
  const models = deriveAvailableModels(
    socket([], [modelCandidate({ id: "acp", ready: true })]),
    [modelOption({ id: "acp", label: "ACP (local CLI)", transport: "acp", node_available: true })],
  );
  assert("picker offers ACP when this socket is ready", models.some((m) => m.id === "acp"));
}

// a node-available model absent from this socket's candidate list is excluded
{
  const models = deriveAvailableModels(
    socket([], [modelCandidate({ id: "grok", ready: true })]),
    [modelOption({ id: "grok", node_available: true }), modelOption({ id: "claude", node_available: true })],
  );
  assert("picker excludes models missing from this socket's candidates", !models.some((m) => m.id === "claude"));
}

// null socket offers nothing
{
  const models = deriveAvailableModels(null, [modelOption({ id: "grok", node_available: true })]);
  assert("null socket offers no picker options", models.length === 0);
}

// --- per-turn override effort options (07d-A3D1 Phase D) ---

// no catalog fetched yet => nothing to offer
{
  const levels = effortLevelsForOverride(null, "claude", "");
  assert("no catalog yet => empty", levels.length === 0);
}

// an Unavailable catalog => nothing to offer (matches every provider's own
// empty effort_levels in its Unavailable branch)
{
  const unavailable: ModelCatalog = {
    status: "unavailable",
    default: catalogEntry({}),
    reason: "no credentials configured",
  };
  assert("unavailable catalog => empty", effortLevelsForOverride(unavailable, "claude", "").length === 0);
}

// an Available catalog with real variants
{
  const catalog: ModelCatalog = {
    status: "available",
    models: [
      catalogEntry({
        id: "claude-sonnet-4-5",
        effort_levels: [
          { id: "low", label: "Low" },
          { id: "high", label: "High" },
        ],
      }),
      catalogEntry({ id: "claude-opus-4-1", effort_levels: [{ id: "max", label: "Max" }] }),
    ],
  };
  assert(
    "no model override picked => the ASSIGNED model's own levels",
    effortLevelsForOverride(catalog, "claude-sonnet-4-5", "")
      .map((l) => l.id)
      .join(",") === "low,high",
  );
  assert(
    "a model override picked => THAT model's levels, not the assigned one's",
    effortLevelsForOverride(catalog, "claude-sonnet-4-5", "claude-opus-4-1")
      .map((l) => l.id)
      .join(",") === "max",
  );
  assert(
    "assigned model missing from the catalog => empty, not a crash",
    effortLevelsForOverride(catalog, "claude-not-listed", "").length === 0,
  );
}

if (failures > 0) throw new Error(`${failures} studioState check failure(s)`);
