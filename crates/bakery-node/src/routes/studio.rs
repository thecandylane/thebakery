use crate::node::{ProjectStacks, Seat};
use crate::state::{
    known_streams, latest_seat_model_id, resolve_acp_binary_config, AppState, StreamRef,
};
use axum::{
    extract::{Query, State},
    http::StatusCode,
    Json,
};
use bakery_agent::{ClaudeModel, GeminiModel, GrokModel};
use serde::{Deserialize, Serialize};

#[derive(schemars::JsonSchema, Clone, Serialize)]
pub(crate) struct ModelOption {
    pub id: String,
    pub label: String,
    pub transport: String,
    pub node_available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(schemars::JsonSchema, Clone, Serialize)]
pub(crate) struct RoleOption {
    pub id: String,
    pub label: String,
}

#[derive(schemars::JsonSchema, Clone, Serialize)]
pub(crate) struct AgentInfo {
    pub agent_id: String,
    pub role_id: String,
    pub role_label: String,
    pub assigned_model_id: Option<String>,
    /// Claude native's own setting — "subscription" | "console" |
    /// "openrouter" — so the picker can show it beside the model, e.g.
    /// "Opus 5.5 · console". `None` for every other model.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assigned_provider: Option<String>,
    /// A switch asked for while this seat's turn ran, which lands when that
    /// turn ends (`seat.model.pending`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub switching: Option<SeatSwitching>,
    /// The seat's last switch, when it couldn't land and nothing has moved
    /// the seat since (`seat.model.switch_failed`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub switch_failed: Option<SeatSwitchFailed>,
    pub socket_ready: bool,
    /// A turn is currently in flight for this agent (07d-A: "one
    /// conversation + busy/run per agent_id, not per socket") — projected
    /// from `state::is_turn_running`, not derived from any client-local flag.
    pub busy: bool,
    /// What its picker shows on the Bakery's own loop, and every turn runs
    /// on (`selection`): the model and effort chosen, none for the runtime's
    /// own. A native seat's picker shows its session's instead.
    pub selection: crate::selection::Selection,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Where a seat's waiting switch goes.
#[derive(schemars::JsonSchema, Clone, Serialize)]
pub(crate) struct SeatSwitching {
    pub model_id: String,
    /// Claude native's provider, as `AgentInfo::assigned_provider`; `None`
    /// for every other model.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
}

/// A seat's switch that couldn't land: where it went, and why.
#[derive(schemars::JsonSchema, Clone, Serialize)]
pub(crate) struct SeatSwitchFailed {
    /// Its `seat.model.pending`, by hash: which failure this is.
    pub id: String,
    /// Where it went, as [`SeatSwitching`].
    pub to: SeatSwitching,
    pub error: String,
}

/// One model's usability on THIS socket specifically — distinct from
/// `ModelOption.node_available`, which is node-wide. An HTTP model's
/// readiness never varies by socket; ACP's does (workspace + mise.toml).
#[derive(schemars::JsonSchema, Clone, Serialize)]
pub(crate) struct SocketModelOption {
    pub id: String,
    pub ready: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// 07c-LIVE-STATE: `/studio/state` is now the client's *only* socket
/// inventory (`GET /streams` is no longer read for topology by any active
/// Studio surface) — so `SocketInfo` carries every field `StreamRef` does,
/// one-to-one, plus this route's own agent/model projection. Do not narrow
/// this back down; a second, incomplete wire shape is exactly what caused
/// the shadow `/streams` fetch this slice removed.
///
/// 07d-A3: `project` is no longer this socket's own recorded git-repo name
/// verbatim — for a socket grouped under a Project, it is that Project's
/// CURRENT (possibly renamed) display name, resolved fresh on every
/// request; only an Ungrouped or factory socket falls back to its own
/// recorded name. `project_id`/`root_id` are `None` for both of those.
#[derive(schemars::JsonSchema, Clone, Serialize)]
pub(crate) struct SocketInfo {
    pub stream_id: String,
    pub name: String,
    /// The name the operator gave it, shown in place of `name`.
    pub title: Option<String>,
    pub project: String,
    pub repo_root: String,
    pub branch: String,
    pub worktree_path: String,
    pub exists_on_disk: bool,
    pub in_place: bool,
    pub owner_node_id: Option<String>,
    pub last_ts: String,
    pub events: u64,
    pub lifetime: crate::node::Lifetime,
    /// Raw `stream.discarded` truth — `state` below is the same fact spelled
    /// as a friendlier string; keep both so callers can match on whichever
    /// is more convenient without re-deriving one from the other.
    pub discarded: bool,
    pub state: String,
    pub agents: Vec<AgentInfo>,
    /// Candidate models for this socket specifically, with per-socket
    /// readiness — the picker's source of truth, not `model_options`'
    /// node-wide `node_available`.
    pub available_models: Vec<SocketModelOption>,
    pub project_id: Option<String>,
    pub root_id: Option<String>,
    /// Undecided write proposals on this exact socket — the
    /// `routes::operator::gates_inner` projection filtered to `stream_id`
    /// (07d-B Phase 4 item 3: "pending approvals come from the proposal/gate
    /// projection"). Every socket kind gets this, not just tasks.
    pub pending_gates: usize,
    /// Lines added/removed in the COMPLETE baseline diff (worktree vs
    /// `task.workspace.materialized`, not the recorded-proposal log) for a
    /// materialized task socket — `0`/`0` for every non-task socket, and for
    /// a task with no materialized baseline yet (chat-only, not yet
    /// started). Bounded-staleness cached
    /// (`task_driver::changed_counts_cached`); the three review/sign-off/
    /// integration surfaces never read this field, they recompute fresh.
    pub changed_added: usize,
    pub changed_removed: usize,
    /// Set on the permanent Baker's own socket (`main: true`) and on each of
    /// its threads (docs/slices/BAKER-THREADS-03.md) — the Baker sidebar's
    /// only source for its conversation list, so no second client cache.
    /// `null` for every other socket, legacy `/baker/assign` jobs included.
    pub baker_thread: Option<crate::baker_threads::BakerThreadInfo>,
    /// Hidden from conversation lists — a Baker thread's own archive fact,
    /// or `socket.archived` for any other conversation. Nothing is removed.
    pub archived: bool,
    /// Where this conversation stands: working, needs_input, failed, done,
    /// or idle — the same answer in every list that shows it.
    pub status: crate::agents::ConversationStatus,
    /// One line on why: what it waits on, or how its last turn ended.
    pub status_note: Option<String>,
    /// A chat with a folder of its own: Delete moves its files to the
    /// Trash, and it can be made a project.
    pub chat: bool,
    /// The conversation that opened this one as its peer: a worker's orc.
    pub opened_by: Option<String>,
    /// A workspace's orc: a conversation beneath the Baker on a worktree of
    /// its own, whose workers name it in `opened_by`.
    pub workspace: bool,
}

#[derive(schemars::JsonSchema, Clone, Serialize)]
pub(crate) struct RootView {
    pub root_id: String,
    pub path: String,
    pub is_git: bool,
}

/// One Project with its live roots and current member sockets. 07d-A3 §1:
/// grouping is by a socket's own `socket.project.set` fact alone, never by
/// `repo_root` — two sockets can share a Project despite pointing at
/// entirely different roots (or none).
#[derive(schemars::JsonSchema, Clone, Serialize)]
pub(crate) struct ProjectGroup {
    pub project_id: String,
    pub name: String,
    pub pinned: bool,
    pub roots: Vec<RootView>,
    pub sockets: Vec<SocketInfo>,
    pub stacks: ProjectStacks,
}

#[derive(schemars::JsonSchema, Clone, Serialize)]
pub(crate) struct StudioState {
    pub model_options: Vec<ModelOption>,
    pub role_options: Vec<RoleOption>,
    pub projects: Vec<ProjectGroup>,
    /// Sockets with no (or no longer resolvable — a retired/unknown
    /// project_id) Project membership. Factory sockets are never here (see
    /// `factory_sockets`) — 07d-A2's "Never list Baker's implementation
    /// stream as a normal project socket" generalizes to: factory-kind
    /// sockets never appear in Projects OR Ungrouped.
    pub ungrouped_sockets: Vec<SocketInfo>,
    /// Every factory-kind socket, flat, ungrouped by design (07d-A2: Baker
    /// job sockets are Floor content, not a sidebar tree section) — kept in
    /// the wire payload so an already-open `/w/[sid]` workbench on one can
    /// still resolve its own agents/busy/available_models.
    pub factory_sockets: Vec<SocketInfo>,
    /// The picture's revision. A later answer with this same number is the
    /// same picture: the window keeps what it is already showing.
    pub revision: u64,
}

const ROLES: &[(Seat, &str, &str)] = &[
    (Seat::Orchestrator, "orchestrator", "Orchestrator"),
    (Seat::Worker, "worker", "Worker"),
    (Seat::Baker, "baker", "Baker"),
];

fn http_model_ready(model_id: &str) -> (bool, Option<String>) {
    match model_id.trim() {
        "grok" => {
            let avail = GrokModel::credentials_available();
            let reason = (!avail).then(|| "Setup needed: add XAI_API_KEY or run Grok OAuth".into());
            (avail, reason)
        }
        "claude" => {
            let avail = ClaudeModel::credentials_available();
            let reason = (!avail).then(|| "Setup needed: add ANTHROPIC_API_KEY".into());
            (avail, reason)
        }
        "claude-code" => {
            let built = bakery_agent::claude_code_slot::is_built();
            let avail = built && bakery_agent::claude_code_slot::credentials_available();
            let reason = if !built {
                Some("Setup needed: Claude Code login is not built on this node".into())
            } else if !avail {
                Some("Setup needed: sign in with Claude Code".into())
            } else {
                None
            };
            (avail, reason)
        }
        "gemini" => {
            let avail = GeminiModel::credentials_available();
            let reason =
                (!avail).then(|| "Setup needed: add GEMINI_API_KEY or run Gemini OAuth".into());
            (avail, reason)
        }
        "stub" => (true, None),
        other => (
            false,
            Some(format!("Unknown model id '{other}' — reassign the seat")),
        ),
    }
}

/// The closed built-in identities — always offered regardless of what a
/// node has configured beyond them (`all_model_ids` in `studio_state`
/// appends every named plug on top of this fixed base).
const MODEL_IDS: &[&str] = &[
    "grok",
    "claude",
    "claude-code",
    "claude-native",
    "gemini",
    "acp",
];

/// `(id, label, node_available, reason, mcp_capable)` for one configured
/// plug — the common shape both `model_options` (node-wide) and
/// `model_ready_for_socket` (per-socket) read, so a plug's readiness can
/// never be computed two different ways in the same response. `mcp_capable`
/// is [`crate::node::AcpPlugConfig::supports_mcp_stdio`] for an ACP plug and
/// always `true` for an openai-compat plug (07d-B Phase 2 Stage C item 4 —
/// openai-compat plugs never spawn an ACP session, so MCP transport is not a
/// concept that applies to them; the field is carried only so both plug
/// kinds share one tuple shape).
type PlugReadiness = (String, String, bool, Option<String>, bool);

/// Every named ACP plug (07d-A3B), each checked through
/// [`crate::state::resolve_acp_plug_config`] — the exact same seam
/// `resolve_model_id_for_seat` uses for a real spawn, so readiness here can
/// never drift from what assigning and turning on that plug would do.
fn acp_plug_readiness(state: &AppState) -> Vec<PlugReadiness> {
    let plugs = state.node.lock().unwrap().acp_plugs.clone();
    plugs
        .into_iter()
        .map(|p| {
            let label = p.label.clone().unwrap_or_else(|| p.id.clone());
            let mcp_capable = p.supports_mcp_stdio();
            match crate::state::resolve_acp_plug_config(state, &p.id) {
                Ok(_) => (p.id, label, true, None, mcp_capable),
                Err(reason) => (p.id, label, false, Some(reason), mcp_capable),
            }
        })
        .collect()
}

/// Every named openai-compat plug (07d-A3B) — syntax-only base_url check,
/// same seam `resolve_model_id_for_seat` uses before ever making a network
/// call.
fn openai_compat_plug_readiness(state: &AppState) -> Vec<PlugReadiness> {
    let plugs = state.node.lock().unwrap().openai_compat_plugs.clone();
    plugs
        .into_iter()
        .map(|p| {
            let label = p.label.clone().unwrap_or_else(|| p.id.clone());
            match crate::state::openai_compat_plug_ready(&p) {
                Ok(()) => (p.id, label, true, None, true),
                Err(reason) => (p.id, label, false, Some(reason), true),
            }
        })
        .collect()
}

/// One model's readiness on one specific socket — the single decision both
/// the per-role assigned-model check and the socket's candidate-model list
/// consult, so a socket's picker and its live assignment can never disagree
/// about whether a model is usable here. `requires_mcp` is true exactly when
/// `s` is the permanent Baker stream or a task socket
/// (`crate::mcp::mcp_scope_for_stream`) — 07d-B Phase 2 Stage C item 4: a
/// plug that cannot deliver MCP is honestly refused here rather than
/// silently degraded, for those sockets only.
fn model_ready_for_socket(
    model_id: &str,
    s: &StreamRef,
    acp_node_available: bool,
    acp_unavailable_reason: &Option<String>,
    claude_native_available: bool,
    claude_native_unavailable_reason: &Option<String>,
    acp_plugs: &[PlugReadiness],
    openai_compat_plugs: &[PlugReadiness],
    requires_mcp: bool,
) -> (bool, Option<String>) {
    if model_id
        .trim()
        .eq_ignore_ascii_case(crate::claude_native::ACCOUNT_ID)
    {
        if !claude_native_available {
            return (
                false,
                Some(
                    claude_native_unavailable_reason
                        .clone()
                        .unwrap_or_else(|| "Claude native adapter is not installed".into()),
                ),
            );
        }
        if !s.exists_on_disk {
            return (false, Some("Socket workspace not found on disk".into()));
        }
        return (true, None);
    }
    if model_id.trim().eq_ignore_ascii_case("acp")
        || model_id.trim().eq_ignore_ascii_case("grok-acp")
    {
        if !acp_node_available {
            return (
                false,
                Some(
                    acp_unavailable_reason
                        .clone()
                        .unwrap_or_else(|| "ACP CLI not configured on this node".into()),
                ),
            );
        }
        if !s.exists_on_disk {
            return (false, Some("Socket workspace not found on disk".into()));
        }
        // Zero-config contract: `bakery_exec::session::provision` already
        // defaults to a bare rust toolchain spec when a workspace has no
        // mise.toml, so a missing mise.toml is not a reason ACP can't boot
        // here — mise.toml is one optional adapter, not a gate. The legacy
        // singular row has no `mcp_transport` field to gate on ("default
        // stdio for the legacy acp_cli row") — never refused here.
        return (true, None);
    }
    if let Some((_, _, ready, reason, mcp_capable)) =
        acp_plugs.iter().find(|(id, ..)| id == model_id)
    {
        if !ready {
            return (false, reason.clone());
        }
        if !s.exists_on_disk {
            return (false, Some("Socket workspace not found on disk".into()));
        }
        if requires_mcp && !mcp_capable {
            return (
                false,
                Some(format!(
                    "Setup needed: plug '{model_id}' has mcp_transport=\"none\" — cannot host the permanent Baker or a task worker"
                )),
            );
        }
        return (true, None);
    }
    // openai-compat plugs are a plain HTTP-shaped endpoint — readiness never
    // varies by socket/workspace, same as grok/claude/gemini.
    if let Some((_, _, ready, reason, _)) =
        openai_compat_plugs.iter().find(|(id, ..)| id == model_id)
    {
        return (*ready, reason.clone());
    }
    http_model_ready(model_id)
}

/// Every model the node offers, each with whether it can run here now.
fn model_options_from(
    acp: (bool, &Option<String>),
    claude_native: (bool, &Option<String>),
    acp_plugs: &[PlugReadiness],
    openai_compat_plugs: &[PlugReadiness],
) -> Vec<ModelOption> {
    let mut model_options = vec![
        {
            let avail = GrokModel::credentials_available();
            ModelOption {
                id: "grok".into(),
                label: "Grok (xAI)".into(),
                transport: "http".into(),
                node_available: avail,
                reason: (!avail).then(|| "Setup needed: add XAI_API_KEY or run Grok OAuth".into()),
            }
        },
        {
            let avail = ClaudeModel::credentials_available();
            ModelOption {
                id: "claude".into(),
                label: "Claude (Anthropic)".into(),
                transport: "http".into(),
                node_available: avail,
                reason: (!avail).then(|| "Setup needed: add ANTHROPIC_API_KEY".into()),
            }
        },
        {
            let built = bakery_agent::claude_code_slot::is_built();
            let avail = built && bakery_agent::claude_code_slot::credentials_available();
            ModelOption {
                id: "claude-code".into(),
                label: "Claude Code".into(),
                transport: "http".into(),
                node_available: avail,
                reason: if !built {
                    Some("Setup needed: Claude Code login is not built on this node".into())
                } else if !avail {
                    Some("Setup needed: sign in with Claude Code".into())
                } else {
                    None
                },
            }
        },
        ModelOption {
            id: crate::claude_native::ACCOUNT_ID.into(),
            label: crate::claude_native::ACCOUNT_LABEL.into(),
            transport: "acp".into(),
            node_available: claude_native.0,
            reason: claude_native.1.clone(),
        },
        {
            let avail = GeminiModel::credentials_available();
            ModelOption {
                id: "gemini".into(),
                label: "Gemini (Google)".into(),
                transport: "http".into(),
                node_available: avail,
                reason: (!avail)
                    .then(|| "Setup needed: add GEMINI_API_KEY or run Gemini OAuth".into()),
            }
        },
        ModelOption {
            id: "acp".into(),
            label: "ACP (local CLI)".into(),
            transport: "acp".into(),
            node_available: acp.0,
            reason: acp.1.clone(),
        },
    ];
    for (id, label, ready, reason, _) in acp_plugs {
        model_options.push(ModelOption {
            id: id.clone(),
            label: label.clone(),
            transport: "acp".into(),
            node_available: *ready,
            reason: reason.clone(),
        });
    }
    for (id, label, ready, reason, _) in openai_compat_plugs {
        model_options.push(ModelOption {
            id: id.clone(),
            label: label.clone(),
            transport: "openai-compat".into(),
            node_available: *ready,
            reason: reason.clone(),
        });
    }
    model_options
}

/// [`model_options_from`] on this node now: the models the memory's
/// learning loop may ask.
pub(crate) fn model_options(st: &AppState) -> Vec<ModelOption> {
    let acp = resolve_acp_binary_config(st).err();
    let claude_native = crate::claude_native::resolve(st).err();
    model_options_from(
        (acp.is_none(), &acp),
        (claude_native.is_none(), &claude_native),
        &acp_plug_readiness(st),
        &openai_compat_plug_readiness(st),
    )
}

/// Heart families that change what `/studio/state` shows. Token and chat
/// traffic is absent on purpose: a working turn must not rebuild the floor.
fn family_moves_studio(family: &str) -> bool {
    matches!(
        family,
        "project.onboarded"
            | "conversation.renamed"
            | "stream.lifetime.set"
            | "stream.discarded"
            | "stream.ownership.claimed"
            | "socket.workspace.set"
            | "socket.kind.recorded"
            | "socket.project.set"
            | "socket.task.set"
            | "socket.archived"
            | "baker.thread.opened"
            | "baker.thread.renamed"
            | "baker.thread.archived"
            | "baker.assignment.accepted"
            | "agent.peer.opened"
            | "workspace.opened"
            | "task.created"
            | "task.workspace.materialized"
            | "task.retired"
            | "task.cancelled"
            | "project.created"
            | "project.renamed"
            | "project.pin.set"
            | "project.stacks.set"
            | "project.root.attached"
            | "project.root.detached"
            | "project.retired"
            | "seat.model.assigned"
            | "agent.file.changed"
            | "agent.approval.cancelled"
            | "agent.turn.retrying"
            | "agent.turn.started"
            | "agent.turn.done"
            | "agent.turn.failed"
            | "agent.turn.exhausted"
            | "agent.turn.cancelled"
            | "network.host.ask"
            | "network.host.decided"
            | "file.read.ask"
            | "file.read.decided"
            | "toolchain.install.ask"
            | "toolchain.install.decided"
    ) || family.starts_with("agent.edit.")
}

/// One materialized task's baseline diff, as the last picture recorded it.
#[derive(Clone, PartialEq, Eq)]
struct DiffSnap {
    stream: String,
    worktree: String,
    base: String,
    added: usize,
    removed: usize,
}

/// The last studio picture and the inputs it was built from.
pub(crate) struct StudioHeld {
    body: StudioState,
    heart_gen: u64,
    running: Vec<(String, &'static str)>,
    awaiting: Vec<(String, u8)>,
    baker: Option<String>,
    diffs: Vec<DiffSnap>,
}

/// Arm `heart` so a studio-relevant append bumps `studio_input_gen`.
pub(crate) fn studio_watch(
    heart: &mut bakery_heart::Heart,
) -> (
    std::sync::Arc<std::sync::atomic::AtomicU64>,
    std::sync::Arc<std::sync::Mutex<Option<StudioHeld>>>,
) {
    let gen = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let hooked = std::sync::Arc::clone(&gen);
    heart.set_on_append(move |family| {
        if family_moves_studio(family) {
            hooked.fetch_add(1, std::sync::atomic::Ordering::Release);
        }
    });
    (gen, std::sync::Arc::new(std::sync::Mutex::new(None)))
}

fn boot_revision() -> u64 {
    static BOOT: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *BOOT.get_or_init(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(1)
            .max(1)
    })
}

fn approval_tag(kind: crate::state::ApprovalKind) -> u8 {
    match kind {
        crate::state::ApprovalKind::FileEdit => 1,
        crate::state::ApprovalKind::ChildTool => 2,
        crate::state::ApprovalKind::Question => 3,
    }
}

fn running_key_from(
    turns: &std::collections::HashSet<(String, crate::node::Seat)>,
) -> Vec<(String, &'static str)> {
    let mut keys: Vec<_> = turns
        .iter()
        .map(|(stream, seat)| (stream.clone(), seat.as_str()))
        .collect();
    keys.sort();
    keys
}

fn running_key(st: &AppState) -> Vec<(String, &'static str)> {
    let turns: std::collections::HashSet<(String, crate::node::Seat)> = st
        .lifecycle
        .lock()
        .unwrap()
        .running_turns
        .iter()
        .filter(|(_, count)| **count > 0)
        .map(|(key, _)| key.clone())
        .collect();
    running_key_from(&turns)
}

fn awaiting_key(st: &AppState) -> Vec<(String, u8)> {
    let mut keys: Vec<_> = crate::agents::awaiting_by_agent(st)
        .into_iter()
        .map(|(id, kind)| (id, approval_tag(kind)))
        .collect();
    keys.sort();
    keys
}

/// The kept picture when every input still matches. Drops the cache lock
/// before asking for diff counts, so a stale 3s cache can spawn git without
/// holding it. Does not lock Heart.
fn studio_cached(st: &AppState) -> Option<StudioState> {
    let heart_gen = st
        .studio_input_gen
        .load(std::sync::atomic::Ordering::Acquire);
    let running = running_key(st);
    let awaiting = awaiting_key(st);
    let baker = crate::baker_threads::baker_pointer(st);
    let (diffs, body) = {
        let guard = st.studio_cache.lock().unwrap();
        let held = guard.as_ref()?;
        if held.heart_gen != heart_gen
            || held.running != running
            || held.awaiting != awaiting
            || held.baker != baker
        {
            return None;
        }
        (held.diffs.clone(), held.body.clone())
    };
    if !diffs_match(st, &diffs) {
        return None;
    }
    // An input that moved while git ran belongs to a new picture.
    let heart_gen_now = st
        .studio_input_gen
        .load(std::sync::atomic::Ordering::Acquire);
    if heart_gen_now != heart_gen
        || running_key(st) != running
        || awaiting_key(st) != awaiting
        || crate::baker_threads::baker_pointer(st) != baker
    {
        return None;
    }
    Some(body)
}

fn diffs_match(st: &AppState, diffs: &[DiffSnap]) -> bool {
    diffs.iter().all(|d| {
        let (added, removed) =
            crate::task_driver::changed_counts_cached(st, &d.stream, &d.worktree, &d.base);
        added == d.added && removed == d.removed
    })
}

fn remember_studio(
    st: &AppState,
    heart_gen: u64,
    running: &std::collections::HashSet<(String, crate::node::Seat)>,
    awaiting: &std::collections::HashMap<String, crate::state::ApprovalKind>,
    baker: Option<&str>,
    baselines: &std::collections::HashMap<String, (String, String)>,
    counts: &std::collections::HashMap<String, (usize, usize)>,
    body: &mut StudioState,
) {
    let mut diffs: Vec<DiffSnap> = baselines
        .iter()
        .map(|(stream, (worktree, base))| {
            let (added, removed) = counts.get(stream).copied().unwrap_or((0, 0));
            DiffSnap {
                stream: stream.clone(),
                worktree: worktree.clone(),
                base: base.clone(),
                added,
                removed,
            }
        })
        .collect();
    diffs.sort_by(|a, b| a.stream.cmp(&b.stream));
    let mut awaiting_key: Vec<(String, u8)> = awaiting
        .iter()
        .map(|(id, kind)| (id.clone(), approval_tag(*kind)))
        .collect();
    awaiting_key.sort();
    let mut slot = st.studio_cache.lock().unwrap();
    let prev = slot.as_ref().map(|held| held.body.revision);
    // The first picture takes the process start time, not 1. A restarted
    // node must not hand back a number the open window already shows.
    body.revision = match prev {
        Some(n) => n.saturating_add(1).max(1),
        None => boot_revision(),
    };
    *slot = Some(StudioHeld {
        body: body.clone(),
        heart_gen,
        running: running_key_from(running),
        awaiting: awaiting_key,
        baker: baker.map(str::to_string).filter(|id| !id.is_empty()),
        diffs,
    });
}

pub(crate) async fn studio_state(
    State(st): State<AppState>,
) -> Result<Json<StudioState>, (StatusCode, String)> {
    if let Some(hit) = studio_cached(&st) {
        return Ok(Json(hit));
    }
    // Sampled before the walk. An append during the build bumps the
    // generation past this value, so the next call rebuilds instead of
    // serving a picture that missed it.
    let heart_gen = st
        .studio_input_gen
        .load(std::sync::atomic::Ordering::Acquire);
    let streams = known_streams(&st).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let (acp_node_available, acp_unavailable_reason) = match resolve_acp_binary_config(&st) {
        Ok(_) => (true, None),
        Err(reason) => (false, Some(reason)),
    };
    let (claude_native_available, claude_native_unavailable_reason) =
        match crate::claude_native::resolve(&st) {
            Ok(_) => (true, None),
            Err(reason) => (false, Some(reason)),
        };
    let acp_plugs = acp_plug_readiness(&st);
    let openai_compat_plugs = openai_compat_plug_readiness(&st);

    let model_options = model_options_from(
        (acp_node_available, &acp_unavailable_reason),
        (claude_native_available, &claude_native_unavailable_reason),
        &acp_plugs,
        &openai_compat_plugs,
    );

    let role_options: Vec<RoleOption> = ROLES
        .iter()
        .map(|(_, id, label)| RoleOption {
            id: id.to_string(),
            label: label.to_string(),
        })
        .collect();

    // Snapshot which (stream, seat) pairs are busy BEFORE locking `heart`
    // below, never while it's held. `admit_activity` (state.rs) already
    // locks `lifecycle` then `heart`, in that order; doing the reverse here
    // — `heart` held across an `is_turn_running` call, which locks
    // `lifecycle` — is the other half of a classic AB-BA lock-order
    // inversion: this request and a concurrent `/agent/turn` call could
    // each hold one lock and block forever waiting on the other. Two
    // separate, non-overlapping critical sections instead.
    let running_turns: std::collections::HashSet<(String, crate::node::Seat)> = st
        .lifecycle
        .lock()
        .unwrap()
        .running_turns
        .iter()
        .filter(|(_, &count)| count > 0)
        .map(|(key, _)| key.clone())
        .collect();

    // Same separate-critical-section discipline as `running_turns` above:
    // both of these lock (and this one specifically locks `heart`) BEFORE
    // the main per-socket pass below takes `heart` again — nesting the two
    // would deadlock (`std::sync::Mutex` is not reentrant).
    let mut pending_gate_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for g in crate::routes::operator::gates_inner(&st).unwrap_or_default() {
        *pending_gate_counts.entry(g.stream_id).or_insert(0) += 1;
    }
    let task_baselines = {
        let h = st.heart.lock().unwrap();
        crate::task_driver::task_baselines_locked(&h)
    };
    // Computed here, in this same pre-pass, and NOT inside the per-socket
    // loop below: `changed_counts_cached` can spawn `git` subprocesses and
    // read files on a cache miss (`task_review::diff_counts`) — real,
    // possibly-slow I/O that must never run with `heart` (locked just below,
    // for the whole loop) held. Doing it here, before that lock is taken,
    // is the same separate-critical-section discipline `running_turns` and
    // `pending_gate_counts` already use just above.
    let changed_counts: std::collections::HashMap<String, (usize, usize)> = task_baselines
        .iter()
        .map(|(stream_id, (worktree_path, base_commit))| {
            (
                stream_id.clone(),
                crate::task_driver::changed_counts_cached(
                    &st,
                    stream_id,
                    worktree_path,
                    base_commit,
                ),
            )
        })
        .collect();

    // Read BEFORE `heart` is locked below: the per-socket Baker-scope check
    // then never has to take `node` underneath the Heart guard.
    let baker_pointer = crate::baker_threads::baker_pointer(&st);
    // Same discipline: which agents are parked on the human right now.
    let awaiting = crate::agents::awaiting_by_agent(&st);

    let heart = st.heart.lock().unwrap();

    // Every non-retired Project up front, indexed by id — filled with
    // sockets below as the single pass over `streams` finds members.
    let project_records = crate::catalog::known_projects_locked(&heart)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let mut projects: Vec<ProjectGroup> = project_records
        .into_iter()
        .map(|r| ProjectGroup {
            project_id: r.project_id,
            name: r.name,
            pinned: r.pinned,
            roots: r
                .roots
                .into_iter()
                .map(|root| RootView {
                    root_id: root.root_id,
                    path: root.path,
                    is_git: root.is_git,
                })
                .collect(),
            sockets: Vec::new(),
            stacks: r.stacks,
        })
        .collect();
    let project_index: std::collections::HashMap<String, usize> = projects
        .iter()
        .enumerate()
        .map(|(i, p)| (p.project_id.clone(), i))
        .collect();

    // Every candidate model id a socket's picker may offer — the 4 closed
    // HTTP/legacy-ACP ids plus every configured named plug (07d-A3B).
    let all_model_ids: Vec<String> = MODEL_IDS
        .iter()
        .map(|s| s.to_string())
        .chain(acp_plugs.iter().map(|(id, ..)| id.clone()))
        .chain(openai_compat_plugs.iter().map(|(id, ..)| id.clone()))
        .collect();

    let mut ungrouped_sockets: Vec<SocketInfo> = Vec::new();
    let mut factory_sockets: Vec<SocketInfo> = Vec::new();

    for s in &streams {
        let kind = crate::state::socket_kind(&heart, &s.stream_id);
        let is_factory = kind == crate::node::SocketKind::Factory;
        let baker_thread = crate::baker_threads::thread_info_locked(
            &heart,
            baker_pointer.as_deref(),
            &s.stream_id,
        );
        // 07d-B Phase 2 Stage C item 4: every Baker conversation (main or
        // thread) and every task socket require a plug that can actually
        // deliver MCP — `model_ready_for_socket` only enforces
        // `mcp_transport` for these.
        let requires_mcp =
            baker_thread.is_some() || crate::state::socket_task_id(&heart, &s.stream_id).is_some();

        let mut agents = Vec::new();
        // 07d-A1 §4: Baker is factory-only. A project socket exposes only
        // the ordinary project roles; the factory socket exposes only baker.
        for &(seat, role_id, role_label) in ROLES {
            if is_factory != (seat == Seat::Baker) {
                continue;
            }
            let fact = latest_seat_model_id(&heart, &s.stream_id, seat);
            let (assigned_model_id, socket_ready, reason) = match fact {
                Err(()) => (
                    None,
                    false,
                    Some("Assignment record unreadable — reassign this role".into()),
                ),
                Ok(None) => (
                    None,
                    false,
                    Some("No model assigned — configure a model for this role".into()),
                ),
                Ok(Some(mid)) => {
                    let (ready, reason) = model_ready_for_socket(
                        &mid,
                        s,
                        acp_node_available,
                        &acp_unavailable_reason,
                        claude_native_available,
                        &claude_native_unavailable_reason,
                        &acp_plugs,
                        &openai_compat_plugs,
                        requires_mcp,
                    );
                    (Some(mid), ready, reason)
                }
            };
            let assigned_provider = assigned_model_id
                .as_deref()
                .is_some_and(|m| m.eq_ignore_ascii_case(crate::claude_native::ACCOUNT_ID))
                // The Heart's lock is held for this whole roster (`heart`
                // above): a lookup that takes it again would wait on
                // itself for ever.
                .then(|| crate::claude_providers::for_seat_locked(&heart, &s.stream_id, seat).id().to_string());
            let switching_to = |to: crate::node::SeatSwitch| SeatSwitching {
                provider: to.model_id.eq_ignore_ascii_case(crate::claude_native::ACCOUNT_ID).then(|| {
                    to.provider.as_deref().and_then(crate::claude_providers::Provider::parse).unwrap_or_default().id().to_string()
                }),
                model_id: to.model_id,
            };
            let switching = crate::state::seat_switch_waiting_locked(&heart, &s.stream_id, seat).map(|(_, to)| switching_to(to));
            let selection = crate::selection::chosen_locked(&heart, &s.stream_id, seat, assigned_model_id.as_deref().unwrap_or_default());
            let switch_failed = crate::state::seat_switch_failed_locked(&heart, &s.stream_id, seat)
                .map(|(event, to, error)| SeatSwitchFailed { id: event.event_hash, to: switching_to(to), error });
            agents.push(AgentInfo {
                agent_id: crate::node::agent_id(&s.stream_id, seat),
                role_id: role_id.into(),
                role_label: role_label.into(),
                assigned_model_id,
                assigned_provider,
                switching,
                switch_failed,
                socket_ready,
                busy: running_turns.contains(&(s.stream_id.clone(), seat)),
                selection,
                reason,
            });
        }

        let available_models: Vec<SocketModelOption> = all_model_ids
            .iter()
            .map(|id| {
                let (ready, reason) = model_ready_for_socket(
                    id,
                    s,
                    acp_node_available,
                    &acp_unavailable_reason,
                    claude_native_available,
                    &claude_native_unavailable_reason,
                    &acp_plugs,
                    &openai_compat_plugs,
                    requires_mcp,
                );
                SocketModelOption {
                    id: id.clone(),
                    ready,
                    reason,
                }
            })
            .collect();

        // 07d-A2/A3: factory sockets are never grouped by Project membership
        // at all — `socket.project.set` is only consulted for project-kind
        // sockets. A socket whose linked project_id no longer resolves to
        // an active Project (retired, or a malformed/unknown id) falls back
        // to Ungrouped rather than being silently dropped.
        let (project_id, root_id) = if is_factory {
            (None, None)
        } else {
            crate::state::socket_project(&heart, &s.stream_id)
        };
        let resolved_project_index = project_id
            .as_deref()
            .and_then(|pid| project_index.get(pid).copied());
        let display_project_name = resolved_project_index
            .map(|i| projects[i].name.clone())
            .unwrap_or_else(|| s.project.clone());

        let (changed_added, changed_removed) =
            changed_counts.get(&s.stream_id).copied().unwrap_or((0, 0));
        let archived = baker_thread.as_ref().map_or_else(
            || crate::agents::socket_archived_locked(&heart, &s.stream_id),
            |thread| thread.archived,
        );
        let pending_gates = pending_gate_counts.get(&s.stream_id).copied().unwrap_or(0);
        let (status, status_note) = if s.discarded {
            (crate::agents::ConversationStatus::Idle, None)
        } else {
            crate::agents::conversation_status_locked(
                &heart,
                &s.stream_id,
                agents.iter().any(|a| a.busy),
                pending_gates,
                agents
                    .iter()
                    .find_map(|a| awaiting.get(&a.agent_id).copied()),
            )
        };

        let socket_info = SocketInfo {
            stream_id: s.stream_id.clone(),
            name: s.name.clone(),
            title: s.title.clone(),
            project: display_project_name,
            repo_root: s.repo_root.clone(),
            branch: s.branch.clone(),
            worktree_path: s.worktree_path.clone(),
            exists_on_disk: s.exists_on_disk,
            in_place: s.in_place,
            owner_node_id: s.owner_node_id.clone(),
            last_ts: s.last_ts.clone(),
            events: s.events,
            lifetime: s.lifetime,
            discarded: s.discarded,
            state: if s.discarded {
                "retired".into()
            } else if kind == crate::node::SocketKind::Task {
                "task".into()
            } else {
                "active".into()
            },
            agents,
            available_models,
            project_id: resolved_project_index.map(|_| project_id.clone().unwrap()),
            root_id: resolved_project_index.and(root_id),
            pending_gates,
            changed_added,
            changed_removed,
            baker_thread,
            archived,
            status,
            status_note,
            chat: crate::catalog::is_chat_folder(&st.base, &s.worktree_path),
            opened_by: crate::baker_threads::opened_by_locked(&heart, &s.stream_id),
            workspace: crate::workspaces::is_workspace_locked(&heart, &s.stream_id),
        };

        if is_factory {
            factory_sockets.push(socket_info);
        } else if let Some(i) = resolved_project_index {
            projects[i].sockets.push(socket_info);
        } else {
            ungrouped_sockets.push(socket_info);
        }
    }

    drop(heart);

    let mut body = StudioState {
        model_options,
        role_options,
        projects,
        ungrouped_sockets,
        factory_sockets,
        revision: 0,
    };
    remember_studio(
        &st,
        heart_gen,
        &running_turns,
        &awaiting,
        baker_pointer.as_deref(),
        &task_baselines,
        &changed_counts,
        &mut body,
    );
    Ok(Json(body))
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct ModelCatalogQuery {
    pub(crate) id: String,
}

/// One provider's live model/effort/context catalog (07d-A3D1 Phase B) — a
/// separate route from `/studio/state` on purpose: that endpoint is polled
/// continuously by every open Studio view, and a provider's list-models
/// call is a real network round trip. This route is only ever hit when an
/// operator actually opens a model picker, and is itself backed by
/// `state.catalog_cache` (see `crate::model_catalog`) so repeated opens
/// inside the cache TTL never re-hit the network.
pub(crate) async fn model_catalog(
    State(st): State<AppState>,
    Query(q): Query<ModelCatalogQuery>,
) -> Json<bakery_agent::ModelCatalog> {
    Json(
        crate::account_adapter::capabilities(&st, q.id.trim())
            .await
            .catalog,
    )
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct AccountCapabilitiesQuery {
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) stream_id: Option<String>,
    #[serde(default)]
    pub(crate) seat: Option<Seat>,
    /// Ask the adapter now instead of answering from memory — the picker's
    /// Retry after a model list could not be read.
    #[serde(default)]
    pub(crate) refresh: bool,
}

/// Full account-adapter response for controls that need to distinguish a live
/// catalog from a configured endpoint or an honestly unavailable ACP account.
pub(crate) async fn account_capabilities(
    State(st): State<AppState>,
    Query(q): Query<AccountCapabilitiesQuery>,
) -> Json<crate::account_adapter::AccountCapabilities> {
    match (q.stream_id.as_deref(), q.seat) {
        (Some(stream_id), Some(seat)) if !stream_id.trim().is_empty() => Json(
            crate::account_adapter::capabilities_for_agent(
                &st,
                q.id.trim(),
                stream_id.trim(),
                seat,
            )
            .await,
        ),
        _ => {
            let probe = if q.refresh {
                crate::account_adapter::Probe::Force
            } else {
                crate::account_adapter::Probe::IfNeeded
            };
            Json(crate::account_adapter::capabilities_with(&st, q.id.trim(), probe).await)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::{
        DiscardRecord, SubscriptionCapabilities, SubscriptionEffortCapability,
        SubscriptionModelCapability, DISCARD_FAMILY, SEAT_ASSIGNED_FAMILY,
    };
    use crate::routes::project::open_stream;
    use crate::routes::seats::{assign_seat, SeatAssignReq};
    use crate::testutil::*;
    use bakery_agent::{secrets, StubModel};
    use std::sync::Arc;

    fn req(stream_id: &str, seat: Seat, model_id: &str) -> SeatAssignReq {
        SeatAssignReq {
            stream_id: stream_id.to_string(),
            seat,
            model_id: model_id.to_string(),
            provider: None,
            reason: None,
            because: None,
        }
    }

    /// Every test in this module isolates credentials AND the ACP process-
    /// override env vars (`BAKERY_ACP_CLI`/`_ARGS`/`_CONFIG_SRC`/`_CONFIG_DEST`,
    /// now part of `PROVIDER_ENV_VARS`) — `studio_state` calls provider
    /// `credentials_available()` and `resolve_acp_binary_config` on every
    /// request, so an un-isolated test can read the live secrets file or an
    /// ambient ACP override left in the developer's/CI's own environment.
    /// `f` runs on a fresh single-threaded runtime inside the isolation
    /// seam so `.await` is available without a `#[tokio::test]`, which
    /// cannot itself be wrapped in `with_isolated_credentials`'s sync closure.
    fn with_isolated_studio<T>(f: impl FnOnce(&tokio::runtime::Runtime) -> T) -> T {
        let dir = tempfile::tempdir().unwrap();
        let secrets_path = dir.path().join("secrets.json");
        let rt = tokio::runtime::Runtime::new().unwrap();
        secrets::test_support::with_isolated_credentials(&secrets_path, || f(&rt))
    }

    /// 1. With isolated test credentials where only Grok HTTP is available, Grok is
    ///    the only socket-ready option and no secret material appears in serialized state.
    #[test]
    fn only_grok_available_is_socket_ready_and_no_secrets_leak() {
        with_isolated_studio(|rt| {
            let dir = tempfile::tempdir().unwrap();
            let repo = make_repo(&dir.path().join("repo"));
            let state = state_with(dir.path(), Arc::new(StubModel));

            let secrets_path = std::env::var("BAKERY_SECRETS_PATH_FOR_TEST").unwrap();
            let mut s = secrets::SecretsFile::default();
            s.xai_api_key = Some("test-only-not-real-xai-key".into());
            secrets::save_to(std::path::Path::new(&secrets_path), &s).unwrap();

            let (body, id_grokonly) = rt.block_on(async {
                let id_grokonly = open_stream(&state, &open_req(&repo, "grokonly"))
                    .unwrap()
                    .context
                    .stream_id;
                let _ = assign_seat(
                    axum::extract::State(state.clone()),
                    axum::Json(req(&id_grokonly, Seat::Orchestrator, "grok")),
                )
                .await
                .unwrap();
                let body = studio_state(axum::extract::State(state.clone()))
                    .await
                    .unwrap()
                    .0;
                (body, id_grokonly)
            });

            let grok = body.model_options.iter().find(|m| m.id == "grok").unwrap();
            assert!(
                grok.node_available,
                "grok must be available with XAI key set"
            );

            let claude = body
                .model_options
                .iter()
                .find(|m| m.id == "claude")
                .unwrap();
            assert!(!claude.node_available, "claude must be unavailable");

            let sock = body
                .ungrouped_sockets
                .iter()
                .find(|s| s.stream_id == id_grokonly)
                .unwrap();
            let orc = sock
                .agents
                .iter()
                .find(|a| a.role_id == "orchestrator")
                .unwrap();
            assert!(
                orc.socket_ready,
                "orchestrator assigned grok must be socket-ready"
            );

            let serialized = serde_json::to_string(&body).unwrap();
            assert!(
                !serialized.contains("test-only-not-real-xai-key"),
                "secret key must not appear in response"
            );
            assert!(
                !serialized.contains("xai_api_key"),
                "secrets field name must not appear"
            );
        });
    }

    /// 2. Zero-config contract: a configured ACP plug on a socket with no
    ///    mise.toml is socket-ready — mise.toml is an optional adapter, not
    ///    a gate (`bakery_exec::session::provision` already defaults to a
    ///    bare rust spec). No spawn, no Heart write from the GET itself.
    #[test]
    fn acp_without_mise_toml_is_still_socket_ready() {
        with_isolated_studio(|rt| {
            let dir = tempfile::tempdir().unwrap();
            let repo = make_repo(&dir.path().join("repo"));
            let state = state_with(dir.path(), Arc::new(StubModel));
            let fake_cli = dir.path().join("fake-acp-cli");
            std::fs::write(&fake_cli, "#!/bin/sh\n").unwrap();
            state.node.lock().unwrap().acp_cli = Some(fake_cli.to_string_lossy().into_owned());

            let (body, events_before, events_after, id_acptest) = rt.block_on(async {
                let id_acptest = open_stream(&state, &open_req(&repo, "acptest"))
                    .unwrap()
                    .context
                    .stream_id;
                let _ = assign_seat(
                    axum::extract::State(state.clone()),
                    axum::Json(req(&id_acptest, Seat::Orchestrator, "acp")),
                )
                .await
                .unwrap();

                let events_before = state
                    .heart
                    .lock()
                    .unwrap()
                    .list_streams()
                    .unwrap()
                    .iter()
                    .find(|s| s.stream_id == id_acptest)
                    .map(|s| s.events)
                    .unwrap_or(0);

                let body = studio_state(axum::extract::State(state.clone()))
                    .await
                    .unwrap()
                    .0;

                let events_after = state
                    .heart
                    .lock()
                    .unwrap()
                    .list_streams()
                    .unwrap()
                    .iter()
                    .find(|s| s.stream_id == id_acptest)
                    .map(|s| s.events)
                    .unwrap_or(0);

                (body, events_before, events_after, id_acptest)
            });

            assert_eq!(
                events_before, events_after,
                "GET must not append Heart events"
            );

            let sock = body
                .ungrouped_sockets
                .iter()
                .find(|s| s.stream_id == id_acptest)
                .unwrap();
            let orc = sock
                .agents
                .iter()
                .find(|a| a.role_id == "orchestrator")
                .unwrap();

            assert!(
                orc.socket_ready,
                "ACP without mise.toml must still be socket-ready (zero-config)"
            );
            assert!(
                orc.reason.is_none(),
                "a ready agent must carry no Setup reason, got: {:?}",
                orc.reason
            );

            // The picker's own candidate list must agree with the agent projection.
            let acp_candidate = sock
                .available_models
                .iter()
                .find(|m| m.id == "acp")
                .unwrap();
            assert!(
                acp_candidate.ready,
                "picker must offer ACP without mise.toml (zero-config)"
            );
        });
    }

    /// A configured `acp_cli` path that does not exist on disk is reported as
    /// node-unavailable with a sanitized reason (never the configured
    /// absolute path) — the shared `resolve_acp_binary_config` seam must
    /// fail the same way the real spawn path eventually would.
    #[test]
    fn acp_cli_pointing_at_a_nonexistent_binary_is_node_unavailable() {
        with_isolated_studio(|rt| {
            let dir = tempfile::tempdir().unwrap();
            let repo = make_repo(&dir.path().join("repo"));
            let state = state_with(dir.path(), Arc::new(StubModel));
            let missing_binary = dir.path().join("does-not-exist-acp-cli");
            state.node.lock().unwrap().acp_cli =
                Some(missing_binary.to_string_lossy().into_owned());

            let (body, id_acpmissing) = rt.block_on(async {
                let id_acpmissing = open_stream(&state, &open_req(&repo, "acpmissing"))
                    .unwrap()
                    .context
                    .stream_id;
                let _ = assign_seat(
                    axum::extract::State(state.clone()),
                    axum::Json(req(&id_acpmissing, Seat::Orchestrator, "acp")),
                )
                .await
                .unwrap();
                let body = studio_state(axum::extract::State(state.clone()))
                    .await
                    .unwrap()
                    .0;
                (body, id_acpmissing)
            });

            let acp_opt = body.model_options.iter().find(|m| m.id == "acp").unwrap();
            assert!(
                !acp_opt.node_available,
                "nonexistent acp_cli binary must be node-unavailable"
            );
            let opt_reason = acp_opt.reason.as_deref().unwrap_or("");
            assert!(
                opt_reason.to_ascii_lowercase().contains("setup needed"),
                "model option reason must be actionable, got: {opt_reason}"
            );
            assert!(
                !opt_reason.contains(&missing_binary.to_string_lossy().into_owned()),
                "reason must never echo the configured absolute path, got: {opt_reason}"
            );

            let sock = body
                .ungrouped_sockets
                .iter()
                .find(|s| s.stream_id == id_acpmissing)
                .unwrap();
            let orc = sock
                .agents
                .iter()
                .find(|a| a.role_id == "orchestrator")
                .unwrap();
            assert!(
                !orc.socket_ready,
                "ACP on a missing binary must not be socket-ready"
            );
            let reason = orc.reason.as_deref().unwrap_or("");
            assert!(
                !reason.contains(&missing_binary.to_string_lossy().into_owned()),
                "agent reason must never echo the configured absolute path, got: {reason}"
            );
        });
    }

    /// 07d-B Phase 2 Stage C item 4: a plug configured `mcp_transport:
    /// "none"` is otherwise perfectly usable (its binary resolves fine) but
    /// must be reported NOT socket-ready on the permanent Baker stream, with
    /// an honest reason naming the transport gap — never silently degraded
    /// to a Baker session with no tools. The same plug on an ordinary
    /// project socket (`requires_mcp == false`) is unaffected.
    #[test]
    fn a_plug_with_mcp_transport_none_is_refused_on_the_permanent_baker_stream_but_fine_on_a_project_socket(
    ) {
        with_isolated_studio(|rt| {
            let dir = tempfile::tempdir().unwrap();
            let repo = make_repo(&dir.path().join("repo"));
            // The permanent Baker needs a REAL, on-disk root — otherwise
            // `model_ready_for_socket`'s `exists_on_disk` check refuses
            // first, before this test's own `mcp_transport` gate is ever
            // reached (a rootless Baker is a separate, already-covered case
            // — `routes::agent`'s `rootless_permanent_baker_turn_...` test).
            let bakery_repo = crate::testutil::bakery_repo_for_test(dir.path());
            let state = crate::testutil::state_with_bakery_root(dir.path(), &bakery_repo);
            let fake_cli = dir.path().join("fake-no-mcp-cli");
            std::fs::write(&fake_cli, "#!/bin/sh\n").unwrap();
            state.node.lock().unwrap().acp_plugs = vec![crate::node::AcpPlugConfig {
                id: "acp:no-mcp".into(),
                label: Some("No-MCP CLI".into()),
                argv: vec![fake_cli.to_string_lossy().into_owned()],
                config: None,
                env: Default::default(),
                mcp_transport: "none".into(),
            }];

            let (body, baker_stream_id, id_project) = rt.block_on(async {
                let baker_home =
                    crate::routes::baker::ensure_permanent_baker(&state, None, None).unwrap();
                let _ = assign_seat(
                    axum::extract::State(state.clone()),
                    axum::Json(req(&baker_home.stream_id, Seat::Baker, "acp:no-mcp")),
                )
                .await
                .unwrap();

                let id_project = open_stream(&state, &open_req(&repo, "ordinary"))
                    .unwrap()
                    .context
                    .stream_id;
                let _ = assign_seat(
                    axum::extract::State(state.clone()),
                    axum::Json(req(&id_project, Seat::Orchestrator, "acp:no-mcp")),
                )
                .await
                .unwrap();

                let body = studio_state(axum::extract::State(state.clone()))
                    .await
                    .unwrap()
                    .0;
                (body, baker_home.stream_id, id_project)
            });

            let baker_sock = body
                .factory_sockets
                .iter()
                .find(|s| s.stream_id == baker_stream_id)
                .unwrap();
            let baker_agent = baker_sock
                .agents
                .iter()
                .find(|a| a.role_id == "baker")
                .unwrap();
            assert!(!baker_agent.socket_ready, "a plug with mcp_transport=\"none\" must not be socket-ready for the permanent Baker");
            let reason = baker_agent.reason.as_deref().unwrap_or("");
            assert!(reason.to_ascii_lowercase().contains("mcp"), "{reason}");
            let baker_candidate = baker_sock
                .available_models
                .iter()
                .find(|m| m.id == "acp:no-mcp")
                .unwrap();
            assert!(
                !baker_candidate.ready,
                "the picker's own candidate row must agree"
            );

            let project_sock = body
                .ungrouped_sockets
                .iter()
                .find(|s| s.stream_id == id_project)
                .unwrap();
            let project_agent = project_sock
                .agents
                .iter()
                .find(|a| a.role_id == "orchestrator")
                .unwrap();
            assert!(
                project_agent.socket_ready,
                "an ordinary project socket does not require MCP — same plug must be fine there"
            );
        });
    }

    /// 07d-A3B Proof: "Studio options: http/acp/openai-compat rows; no
    /// secret or filesystem path in reason." One named ACP plug (missing
    /// binary — Setup) and one named openai-compat plug (bad base_url —
    /// Setup) alongside the fixed http/legacy-acp rows, none leaking a
    /// configured path or URL into its reason.
    #[test]
    fn studio_state_lists_named_acp_and_openai_compat_plugs_with_sanitized_reasons() {
        with_isolated_studio(|rt| {
            let dir = tempfile::tempdir().unwrap();
            let state = state_with(dir.path(), Arc::new(StubModel));
            let missing_binary = dir.path().join("does-not-exist-codex-cli");
            {
                let mut node = state.node.lock().unwrap();
                node.acp_plugs = vec![crate::node::AcpPlugConfig {
                    id: "acp:codex".into(),
                    label: Some("Codex CLI".into()),
                    argv: vec![missing_binary.to_string_lossy().into_owned()],
                    config: None,
                    env: Default::default(),
                    mcp_transport: crate::node::default_mcp_transport(),
                }];
                node.openai_compat_plugs = vec![crate::node::OpenAiCompatPlugConfig {
                    id: "local:ollama".into(),
                    label: Some("Local Ollama".into()),
                    base_url: "not-a-url".into(),
                    model: "llama3".into(),
                }];
            }

            let body = rt
                .block_on(studio_state(axum::extract::State(state.clone())))
                .unwrap()
                .0;

            let http_rows: Vec<_> = body
                .model_options
                .iter()
                .filter(|m| m.transport == "http")
                .map(|m| m.id.as_str())
                .collect();
            assert!(
                http_rows.contains(&"grok")
                    && http_rows.contains(&"claude")
                    && http_rows.contains(&"claude-code")
                    && http_rows.contains(&"gemini")
            );

            let acp_row = body
                .model_options
                .iter()
                .find(|m| m.id == "acp:codex")
                .unwrap();
            assert_eq!(acp_row.transport, "acp");
            assert!(!acp_row.node_available);
            let acp_reason = acp_row.reason.as_deref().unwrap_or("");
            assert!(
                acp_reason.to_ascii_lowercase().contains("setup needed"),
                "{acp_reason}"
            );
            assert!(
                !acp_reason.contains(&missing_binary.to_string_lossy().into_owned()),
                "must never echo the configured path: {acp_reason}"
            );

            let oc_row = body
                .model_options
                .iter()
                .find(|m| m.id == "local:ollama")
                .unwrap();
            assert_eq!(oc_row.transport, "openai-compat");
            assert!(!oc_row.node_available);
            let oc_reason = oc_row.reason.as_deref().unwrap_or("");
            assert!(
                oc_reason.to_ascii_lowercase().contains("setup needed"),
                "{oc_reason}"
            );

            let serialized = serde_json::to_string(&body).unwrap();
            assert!(!serialized.contains(&missing_binary.to_string_lossy().into_owned()));
        });
    }

    /// 3. Two sockets with different seat assignments project their own agents;
    ///    there is no node-global assignment leak.
    #[test]
    fn two_sockets_with_different_assignments_project_independently() {
        with_isolated_studio(|rt| {
            let dir = tempfile::tempdir().unwrap();
            let repo = make_repo(&dir.path().join("repo"));
            let state = state_with(dir.path(), Arc::new(StubModel));

            let (body, id_sock_a, id_sock_b) = rt.block_on(async {
                let id_sock_a = open_stream(&state, &open_req(&repo, "sockA"))
                    .unwrap()
                    .context
                    .stream_id;
                let id_sock_b = open_stream(&state, &open_req(&repo, "sockB"))
                    .unwrap()
                    .context
                    .stream_id;

                let _ = assign_seat(
                    axum::extract::State(state.clone()),
                    axum::Json(req(&id_sock_a, Seat::Orchestrator, "grok")),
                )
                .await
                .unwrap();
                let _ = assign_seat(
                    axum::extract::State(state.clone()),
                    axum::Json(req(&id_sock_b, Seat::Orchestrator, "claude")),
                )
                .await
                .unwrap();

                let body = studio_state(axum::extract::State(state.clone()))
                    .await
                    .unwrap()
                    .0;
                (body, id_sock_a, id_sock_b)
            });

            let a = body
                .ungrouped_sockets
                .iter()
                .find(|s| s.stream_id == id_sock_a)
                .unwrap();
            let b = body
                .ungrouped_sockets
                .iter()
                .find(|s| s.stream_id == id_sock_b)
                .unwrap();

            let a_orc = a
                .agents
                .iter()
                .find(|ag| ag.role_id == "orchestrator")
                .unwrap();
            let b_orc = b
                .agents
                .iter()
                .find(|ag| ag.role_id == "orchestrator")
                .unwrap();

            assert_eq!(a_orc.assigned_model_id.as_deref(), Some("grok"));
            assert_eq!(b_orc.assigned_model_id.as_deref(), Some("claude"));
            assert_ne!(
                a_orc.assigned_model_id, b_orc.assigned_model_id,
                "each socket must project its own assignment"
            );
        });
    }

    /// 4. A malformed latest assignment fails that agent closed (socket_ready=false,
    ///    reason mentions "unreadable") rather than falling back to an older good one.
    #[test]
    fn malformed_latest_assignment_fails_closed_not_older_good_one() {
        with_isolated_studio(|rt| {
            let dir = tempfile::tempdir().unwrap();
            let repo = make_repo(&dir.path().join("repo"));
            let state = state_with(dir.path(), Arc::new(StubModel));

            let (body, id_badrec) = rt.block_on(async {
                let id_badrec = open_stream(&state, &open_req(&repo, "badrec"))
                    .unwrap()
                    .context
                    .stream_id;

                let _ = assign_seat(
                    axum::extract::State(state.clone()),
                    axum::Json(req(&id_badrec, Seat::Orchestrator, "grok")),
                )
                .await
                .unwrap();
                crate::state::append(
                    &state,
                    &id_badrec,
                    "human",
                    SEAT_ASSIGNED_FAMILY,
                    "not valid json at all".to_string(),
                    vec![],
                )
                .unwrap();

                let body = studio_state(axum::extract::State(state.clone()))
                    .await
                    .unwrap()
                    .0;
                (body, id_badrec)
            });

            let sock = body
                .ungrouped_sockets
                .iter()
                .find(|s| s.stream_id == id_badrec)
                .unwrap();
            let orc = sock
                .agents
                .iter()
                .find(|a| a.role_id == "orchestrator")
                .unwrap();

            assert!(
                !orc.socket_ready,
                "malformed latest assignment must not be socket-ready"
            );
            let reason = orc.reason.as_deref().unwrap_or("");
            assert!(
                reason.to_ascii_lowercase().contains("unreadable"),
                "reason must mention unreadable, got: {reason}"
            );
            assert!(
                !reason.to_ascii_lowercase().contains("grok"),
                "must not reveal the older assignment: {reason}"
            );
        });
    }

    /// BAKER-THREADS-03: the Baker's conversations — its main one and each
    /// thread — are marked on the one topology source the sidebar reads,
    /// while a legacy `/baker/assign` branch job (also factory-kind) is not a
    /// conversation and carries no marker.
    #[test]
    fn baker_conversations_are_marked_and_legacy_jobs_are_not() {
        with_isolated_studio(|rt| {
            let dir = tempfile::tempdir().unwrap();
            let repo = make_repo(&dir.path().join("repo"));
            let state = state_with(dir.path(), Arc::new(StubModel));

            let (body, baker, thread, job) = rt.block_on(async {
                let baker = crate::routes::baker::ensure_permanent_baker(&state, None, None)
                    .unwrap()
                    .stream_id;
                let thread = crate::baker_threads::create_thread(
                    &state,
                    &crate::baker_threads::CreateThreadReq {
                        title: Some("Parallel".into()),
                        instruction: None,
                        ..Default::default()
                    },
                    "human",
                    None,
                    None,
                )
                .unwrap()
                .thread
                .stream_id;
                let job = open_stream(&state, &open_req(&repo, "baker-job"))
                    .unwrap()
                    .context
                    .stream_id;
                mark_factory(&state, &job);
                let body = studio_state(axum::extract::State(state.clone()))
                    .await
                    .unwrap()
                    .0;
                (body, baker, thread, job)
            });

            let find = |sid: &str| {
                body.factory_sockets
                    .iter()
                    .find(|s| s.stream_id == sid)
                    .unwrap_or_else(|| panic!("{sid} must be a factory socket"))
            };
            let main = find(&baker)
                .baker_thread
                .as_ref()
                .expect("main conversation marked");
            assert!(main.main && main.title == "Baker" && !main.archived);
            let t = find(&thread).baker_thread.as_ref().expect("thread marked");
            assert!(!t.main && t.title == "Parallel" && !t.archived);
            assert!(
                find(&job).baker_thread.is_none(),
                "a legacy branch job is not one of the Baker's conversations"
            );
        });
    }

    /// 5. Factory and project sockets have correct `kind` and Active/Retired state.
    #[test]
    fn factory_and_project_sockets_have_correct_kind_and_lifecycle_state() {
        with_isolated_studio(|rt| {
            let dir = tempfile::tempdir().unwrap();
            let repo = make_repo(&dir.path().join("repo"));
            let state = state_with(dir.path(), Arc::new(StubModel));

            let (body, id_regular, id_baker_job_1) = rt.block_on(async {
                let id_regular = open_stream(&state, &open_req(&repo, "regular"))
                    .unwrap()
                    .context
                    .stream_id;
                let id_baker_job_1 = open_stream(&state, &open_req(&repo, "baker-job-1"))
                    .unwrap()
                    .context
                    .stream_id;
                mark_factory(&state, &id_baker_job_1);

                let record = DiscardRecord {
                    worktree_removed: true,
                    worktree_error: None,
                    room_destroyed: false,
                    room_error: None,
                    kept: None,
                    chat_trashed: false,
                    chat_error: None,
                };
                crate::state::append(
                    &state,
                    &id_baker_job_1,
                    "node",
                    DISCARD_FAMILY,
                    serde_json::to_string(&record).unwrap(),
                    vec![],
                )
                .unwrap();

                let body = studio_state(axum::extract::State(state.clone()))
                    .await
                    .unwrap()
                    .0;
                (body, id_regular, id_baker_job_1)
            });

            // 07d-A2/A3: a plain-opened project socket with no `socket.project.set`
            // fact is Ungrouped; a factory socket is never in Projects OR
            // Ungrouped — bucket membership itself is the "kind" proof now,
            // there is no separate `kind` field to assert on.
            let regular = body
                .ungrouped_sockets
                .iter()
                .find(|s| s.stream_id == id_regular)
                .unwrap();
            let baker = body
                .factory_sockets
                .iter()
                .find(|s| s.stream_id == id_baker_job_1)
                .unwrap();
            assert!(
                body.projects.is_empty(),
                "neither socket has a Project — nothing to group"
            );

            assert_eq!(regular.state, "active");
            assert_eq!(baker.state, "retired");

            // 07c-LIVE-STATE: SocketInfo is now the client's only socket
            // inventory — every StreamRef field it carries must round-trip.
            assert!(!regular.discarded, "raw discarded bool must match state");
            assert!(baker.discarded, "raw discarded bool must match state");
            assert!(!regular.project.is_empty());
            assert_eq!(
                regular.project, baker.project,
                "same source repo → same project name"
            );
            assert!(!regular.repo_root.is_empty());
            assert_eq!(regular.repo_root, baker.repo_root, "same source repo");
            assert!(!regular.worktree_path.is_empty());
            assert!(!baker.worktree_path.is_empty());
            assert_eq!(
                regular.lifetime,
                crate::node::Lifetime::Slice,
                "default lifetime"
            );
            assert!(!regular.last_ts.is_empty());
            assert!(
                regular.events >= 1,
                "onboard alone appends at least one event"
            );
        });
    }

    /// 6. 07d-A1 §4: a project socket's `agents` never includes baker; the
    ///    factory socket's `agents` is baker alone.
    #[test]
    fn project_socket_never_offers_baker_and_factory_offers_only_baker() {
        with_isolated_studio(|rt| {
            let dir = tempfile::tempdir().unwrap();
            let repo = make_repo(&dir.path().join("repo"));
            let state = state_with(dir.path(), Arc::new(StubModel));

            let (body, id_rolesproj, id_baker_roles) = rt.block_on(async {
                let id_rolesproj = open_stream(&state, &open_req(&repo, "rolesproj"))
                    .unwrap()
                    .context
                    .stream_id;
                let id_baker_roles = open_stream(&state, &open_req(&repo, "baker-roles"))
                    .unwrap()
                    .context
                    .stream_id;
                mark_factory(&state, &id_baker_roles);
                let body = studio_state(axum::extract::State(state.clone()))
                    .await
                    .unwrap()
                    .0;
                (body, id_rolesproj, id_baker_roles)
            });

            let proj = body
                .ungrouped_sockets
                .iter()
                .find(|s| s.stream_id == id_rolesproj)
                .unwrap();
            let fac = body
                .factory_sockets
                .iter()
                .find(|s| s.stream_id == id_baker_roles)
                .unwrap();

            let proj_roles: Vec<_> = proj.agents.iter().map(|a| a.role_id.as_str()).collect();
            assert_eq!(
                proj_roles,
                vec!["orchestrator", "worker"],
                "a project socket must never offer baker"
            );

            let fac_roles: Vec<_> = fac.agents.iter().map(|a| a.role_id.as_str()).collect();
            assert_eq!(
                fac_roles,
                vec!["baker"],
                "the factory socket must offer only baker"
            );
        });
    }

    /// 07d-A1 correction: a project named/branched to look like a Baker job
    /// (`baker-...` name — the OLD heuristic) but never recorded as
    /// `Factory` (never went through `/baker/assign`) must project ordinary
    /// project roles, not baker — factory authority is a durable fact, not
    /// a naming convention any project could match by accident.
    #[test]
    fn a_baker_named_socket_with_no_recorded_kind_projects_as_project_not_factory() {
        with_isolated_studio(|rt| {
            let dir = tempfile::tempdir().unwrap();
            let repo = make_repo(&dir.path().join("repo"));
            let state = state_with(dir.path(), Arc::new(StubModel));

            let (body, id_baker_spoof_roles) = rt.block_on(async {
                let id_baker_spoof_roles =
                    open_stream(&state, &open_req(&repo, "baker-spoof-roles"))
                        .unwrap()
                        .context
                        .stream_id;
                // Deliberately never `mark_factory`'d.
                let body = studio_state(axum::extract::State(state.clone()))
                    .await
                    .unwrap()
                    .0;
                (body, id_baker_spoof_roles)
            });

            let spoof = body
                .ungrouped_sockets
                .iter()
                .find(|s| s.stream_id == id_baker_spoof_roles)
                .unwrap();

            let roles: Vec<_> = spoof.agents.iter().map(|a| a.role_id.as_str()).collect();
            assert_eq!(
                roles,
                vec!["orchestrator", "worker"],
                "a baker-shaped name with no recorded kind must project as project, not factory"
            );

            // Proves the "project" side of "kind" directly: this socket is
            // reachable in the project-kind bucket (Ungrouped, since it has
            // no Project link) and absent from the factory-kind bucket.
            assert!(
                !body
                    .factory_sockets
                    .iter()
                    .any(|s| s.stream_id == id_baker_spoof_roles),
                "a baker-shaped name with no recorded kind must never land in factory_sockets"
            );
        });
    }

    // ─── 07d-A3: grouping by real Project membership ───────────────────────

    #[test]
    fn a_socket_linked_to_a_project_is_grouped_there_with_the_projects_live_name() {
        with_isolated_studio(|rt| {
            let dir = tempfile::tempdir().unwrap();
            let repo = make_repo(&dir.path().join("repo"));
            let state = state_with(dir.path(), Arc::new(StubModel));

            let (body, project_id, socket_id) = rt.block_on(async {
                let socket_ctx = open_stream(&state, &open_req(&repo, "grouped"))
                    .unwrap()
                    .context;
                let (record, _) =
                    crate::catalog::create_project(&state, "Grouped Home", None).unwrap();
                crate::catalog::link_socket_to_project(
                    &state,
                    &socket_ctx.stream_id,
                    &record.project_id,
                    &socket_ctx.worktree_path,
                    true,
                )
                .unwrap();
                let body = studio_state(axum::extract::State(state.clone()))
                    .await
                    .unwrap()
                    .0;
                (body, record.project_id, socket_ctx.stream_id)
            });

            assert!(
                body.ungrouped_sockets
                    .iter()
                    .all(|s| s.stream_id != socket_id),
                "must not also appear Ungrouped"
            );
            let group = body
                .projects
                .iter()
                .find(|p| p.project_id == project_id)
                .unwrap();
            assert_eq!(group.name, "Grouped Home");
            assert_eq!(group.roots.len(), 1);
            let sock = group
                .sockets
                .iter()
                .find(|s| s.stream_id == socket_id)
                .unwrap();
            assert_eq!(
                sock.project, "Grouped Home",
                "the socket's own `project` field mirrors the live group name"
            );
            assert_eq!(sock.project_id.as_deref(), Some(project_id.as_str()));
            assert!(sock.root_id.is_some());
        });
    }

    #[test]
    fn renaming_a_project_changes_the_next_studio_state_call_immediately() {
        with_isolated_studio(|rt| {
            let dir = tempfile::tempdir().unwrap();
            let state = state_with(dir.path(), Arc::new(StubModel));

            let (record, _) = crate::catalog::create_project(&state, "Before", None).unwrap();
            crate::catalog::rename_project(&state, &record.project_id, "After").unwrap();

            let body = rt
                .block_on(studio_state(axum::extract::State(state.clone())))
                .unwrap()
                .0;
            let group = body
                .projects
                .iter()
                .find(|p| p.project_id == record.project_id)
                .unwrap();
            assert_eq!(
                group.name, "After",
                "no separate stale copy of the name to go out of sync"
            );
        });
    }

    #[test]
    fn two_sockets_in_the_same_project_group_together() {
        with_isolated_studio(|rt| {
            let dir = tempfile::tempdir().unwrap();
            let repo = make_repo(&dir.path().join("repo"));
            let state = state_with(dir.path(), Arc::new(StubModel));

            let (body, project_id) = rt.block_on(async {
                let (record, first_ctx) =
                    crate::catalog::create_project(&state, "Duo", None).unwrap();
                let second_ctx = open_stream(&state, &open_req(&repo, "seconddesk"))
                    .unwrap()
                    .context;
                crate::catalog::link_socket_to_project(
                    &state,
                    &second_ctx.stream_id,
                    &record.project_id,
                    &second_ctx.worktree_path,
                    true,
                )
                .unwrap();
                let _ = first_ctx;
                let body = studio_state(axum::extract::State(state.clone()))
                    .await
                    .unwrap()
                    .0;
                (body, record.project_id)
            });

            let group = body
                .projects
                .iter()
                .find(|p| p.project_id == project_id)
                .unwrap();
            assert_eq!(
                group.sockets.len(),
                2,
                "both the starter socket and the attached one belong here"
            );
        });
    }

    #[test]
    fn a_socket_whose_project_is_now_retired_falls_back_to_ungrouped_not_dropped() {
        with_isolated_studio(|rt| {
            let dir = tempfile::tempdir().unwrap();
            let repo = make_repo(&dir.path().join("repo"));
            let state = state_with(dir.path(), Arc::new(StubModel));

            let (body, socket_id) = rt.block_on(async {
                let socket_ctx = open_stream(&state, &open_req(&repo, "orphanedbyretire"))
                    .unwrap()
                    .context;
                let (record, _) =
                    crate::catalog::create_project(&state, "Temporary", None).unwrap();
                crate::catalog::link_socket_to_project(
                    &state,
                    &socket_ctx.stream_id,
                    &record.project_id,
                    &socket_ctx.worktree_path,
                    true,
                )
                .unwrap();
                crate::catalog::retire_project(&state, &record.project_id).unwrap();
                let body = studio_state(axum::extract::State(state.clone()))
                    .await
                    .unwrap()
                    .0;
                (body, socket_ctx.stream_id)
            });

            assert!(
                body.projects.is_empty(),
                "the retired project itself does not appear"
            );
            assert!(
                body.ungrouped_sockets.iter().any(|s| s.stream_id == socket_id),
                "its still-active socket must still be listed, as Ungrouped, never silently dropped"
            );
        });
    }

    /// Claude Code participates in the subscription-adapter catalog seam. With
    /// credentials deliberately isolated, that adapter must preserve the
    /// ordinary `Unavailable` wire shape while explaining the actionable
    /// account requirement; the route must never turn that into a 400/500.
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn model_catalog_route_reports_claude_code_sign_in_requirement() {
        with_isolated_studio(|rt| {
            let dir = tempfile::tempdir().unwrap();
            let state = state_with(dir.path(), Arc::new(StubModel));
            let body = rt.block_on(async {
                model_catalog(
                    axum::extract::State(state.clone()),
                    axum::extract::Query(ModelCatalogQuery {
                        id: "claude-code".to_string(),
                    }),
                )
                .await
                .0
            });
            match body {
                bakery_agent::ModelCatalog::Unavailable { default, reason } => {
                    assert_eq!(default.id, "claude-code");
                    assert_eq!(
                        reason,
                        "Setup needed: sign in with Claude Code before listing models"
                    );
                }
                bakery_agent::ModelCatalog::Available { .. } => panic!("expected Unavailable"),
            }
        });
    }

    #[test]
    fn account_capabilities_route_returns_subscription_adapter_facts() {
        with_isolated_studio(|rt| {
            let dir = tempfile::tempdir().unwrap();
            let state = state_with(dir.path(), Arc::new(StubModel));
            state.node.lock().unwrap().subscription_capabilities.insert(
                "acp".to_string(),
                SubscriptionCapabilities {
                    models: vec![SubscriptionModelCapability {
                        id: "reported-opus".to_string(),
                        label: "Reported Opus".to_string(),
                        context_window: Some(1_000_000),
                        effort_levels: vec![SubscriptionEffortCapability {
                            id: "max".to_string(),
                            label: "Max".to_string(),
                        }],
                    }],
                },
            );

            let body = rt.block_on(async {
                account_capabilities(
                    axum::extract::State(state.clone()),
                    axum::extract::Query(AccountCapabilitiesQuery {
                        id: "acp".to_string(),
                        stream_id: None,
                        seat: None,
                        refresh: false,
                    }),
                )
                .await
                .0
            });
            assert_eq!(body.source, "subscription_adapter");
            assert!(!body.text_attachments);
            assert!(!body.plan_enforceable);
            match body.catalog {
                bakery_agent::ModelCatalog::Available { models } => {
                    assert_eq!(models[0].id, "reported-opus");
                    assert_eq!(models[0].context_window, Some(1_000_000));
                    assert_eq!(models[0].effort_levels[0].id, "max");
                }
                bakery_agent::ModelCatalog::Unavailable { .. } => {
                    panic!("expected reported capabilities")
                }
            }
        });
    }

    /// A real HTTP provider (grok) with no credentials configured must also
    /// degrade to `Unavailable` — through the exact same route a configured
    /// provider would use — without ever attempting a network call (the
    /// provider's own `catalog()` short-circuits on the missing credential
    /// check, same as `only_grok_available_is_socket_ready_and_no_secrets_leak`
    /// relies on for `credentials_available()`).
    #[test]
    fn model_catalog_route_reports_unavailable_when_grok_has_no_credentials() {
        with_isolated_studio(|rt| {
            let dir = tempfile::tempdir().unwrap();
            let state = state_with(dir.path(), Arc::new(StubModel));
            let body = rt.block_on(async {
                model_catalog(
                    axum::extract::State(state.clone()),
                    axum::extract::Query(ModelCatalogQuery {
                        id: "grok".to_string(),
                    }),
                )
                .await
                .0
            });
            match body {
                bakery_agent::ModelCatalog::Unavailable { default, reason } => {
                    // `default.id` is grok's own configured model id (e.g.
                    // "grok-4.5"), not the provider bucket name "grok" — the
                    // route just passes through whatever the provider's own
                    // `catalog()` reports as its verified default.
                    assert!(!default.id.is_empty());
                    assert!(reason.contains("no xAI credentials"), "{reason}");
                }
                bakery_agent::ModelCatalog::Available { .. } => panic!("expected Unavailable"),
            }
        });
    }

    /// 07d-B Phase 4 item 3: a task socket's Floor-row `changed_added`/
    /// `changed_removed` reflect the complete baseline diff, computed in the
    /// pre-pass that runs BEFORE `studio_state` takes its own `heart` lock
    /// (this test's real job — asserting the numbers come out right at all
    /// is the only way to notice if that pre-pass were ever wired to the
    /// wrong map, or dropped back inside the locked loop where a `git`
    /// subprocess would run with the global Heart mutex held).
    #[test]
    fn a_task_sockets_changed_counts_reflect_an_untracked_file_on_disk() {
        with_isolated_studio(|rt| {
            let dir = tempfile::tempdir().unwrap();
            let state = state_with(dir.path(), Arc::new(StubModel));
            let repo = make_repo(&dir.path().join("repo"));
            let project = crate::catalog::create_project(
                &state,
                "P",
                Some(crate::catalog::DirectoryRootSpec::Attach(
                    &repo.to_string_lossy(),
                )),
            )
            .unwrap()
            .0;
            let created = crate::tasks::task_create(
                &state,
                crate::tasks::TaskCreateReq {
                    parent_project_id: project.project_id.clone(),
                    title: "Do work".into(),
                    work_order: "wo-1".into(),
                    contract: Default::default(),
                },
            )
            .unwrap();
            crate::tasks::task_start(
                &state,
                crate::tasks::TaskStartReq {
                    project_id: project.project_id.clone(),
                    task_id: created.task.task_id.clone(),
                },
            )
            .unwrap();
            let ctx = crate::state::context_for(&state, &created.task.stream_id).unwrap();
            std::fs::write(
                std::path::Path::new(&ctx.worktree_path).join("new.txt"),
                "one\ntwo\n",
            )
            .unwrap();

            let body = rt
                .block_on(studio_state(axum::extract::State(state.clone())))
                .unwrap()
                .0;
            let proj = body
                .projects
                .iter()
                .find(|p| p.project_id == project.project_id)
                .unwrap();
            let sock = proj
                .sockets
                .iter()
                .find(|s| s.stream_id == created.task.stream_id)
                .expect("the task's own socket must be listed under its project");
            assert_eq!(
                sock.changed_added, 2,
                "two new lines in an untracked file must count as added"
            );
            assert_eq!(sock.changed_removed, 0);
        });
    }

    /// Two calls with nothing changed share a revision, and the second does
    /// not enter `diff_counts` (no git). The first call has a real baseline,
    /// so a missing snapshot would still spawn git on the repeat.
    #[test]
    fn studio_state_same_revision_skips_diff_counts() {
        with_isolated_studio(|rt| {
            let dir = tempfile::tempdir().unwrap();
            let state = state_with(dir.path(), Arc::new(StubModel));
            let repo = make_repo(&dir.path().join("repo"));
            let project = crate::catalog::create_project(
                &state,
                "P",
                Some(crate::catalog::DirectoryRootSpec::Attach(
                    &repo.to_string_lossy(),
                )),
            )
            .unwrap()
            .0;
            let created = crate::tasks::task_create(
                &state,
                crate::tasks::TaskCreateReq {
                    parent_project_id: project.project_id.clone(),
                    title: "Do work".into(),
                    work_order: "wo-snap".into(),
                    contract: Default::default(),
                },
            )
            .unwrap();
            crate::tasks::task_start(
                &state,
                crate::tasks::TaskStartReq {
                    project_id: project.project_id.clone(),
                    task_id: created.task.task_id.clone(),
                },
            )
            .unwrap();

            let before =
                crate::task_review::DIFF_COUNTS_CALLS.load(std::sync::atomic::Ordering::Relaxed);
            let first = rt
                .block_on(studio_state(axum::extract::State(state.clone())))
                .unwrap()
                .0;
            let mid =
                crate::task_review::DIFF_COUNTS_CALLS.load(std::sync::atomic::Ordering::Relaxed);
            let second = rt
                .block_on(studio_state(axum::extract::State(state.clone())))
                .unwrap()
                .0;
            let after =
                crate::task_review::DIFF_COUNTS_CALLS.load(std::sync::atomic::Ordering::Relaxed);

            assert!(first.revision >= 1);
            assert_eq!(first.revision, second.revision);
            assert!(mid > before, "the first call counts the baseline diff");
            assert_eq!(after, mid, "the second call does not call diff_counts");
        });
    }
}
