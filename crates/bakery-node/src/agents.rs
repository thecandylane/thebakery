//! Settings → Agents and Settings → Agent storage: every configured agent on
//! every socket — the Baker and its threads, project agents, task workers —
//! whatever backs it (Claude, Codex, Cursor, Grok, Antigravity, or an API
//! model). Abilities are what each runtime reported about itself when it
//! started, never a per-vendor table. Storage is every place an agent can
//! write outside its project, all of it inside `~/.bakery`: the runtime
//! sandbox allows no other writes (`bakery_exec::spawn_boxed_acp`), and never
//! into a project's own agent folders (`bakery_exec::AGENT_STATE_DIRS`).

use crate::node::Seat;
use crate::pipe::ChildToolKind;
use crate::state::AppState;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

type HttpError = (StatusCode, String);

fn internal(e: impl ToString) -> HttpError {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

/// One configured agent, as `/studio/state` lists it.
#[derive(Debug, Clone)]
struct Member {
    stream_id: String,
    seat: Seat,
    socket: String,
    scope: &'static str,
    model_id: Option<String>,
    busy: bool,
    workspace: String,
}

fn seat_of(role_id: &str) -> Option<Seat> {
    match role_id {
        "orchestrator" => Some(Seat::Orchestrator),
        "worker" => Some(Seat::Worker),
        "baker" => Some(Seat::Baker),
        _ => None,
    }
}

/// Every live (not discarded) socket's agents, from the one topology
/// projection the rest of the app reads.
async fn roster(st: &AppState) -> Result<Vec<Member>, HttpError> {
    let Json(studio) = crate::routes::studio::studio_state(State(st.clone())).await?;
    let sockets: Vec<_> = studio
        .factory_sockets
        .into_iter()
        .chain(studio.projects.into_iter().flat_map(|p| p.sockets))
        .chain(studio.ungrouped_sockets)
        .filter(|s| !s.discarded)
        .collect();
    let heart = st.heart.lock().unwrap();
    let mut out = Vec::new();
    for s in sockets {
        let task = crate::state::socket_task_id(&heart, &s.stream_id).is_some();
        let (scope, socket) = match (&s.baker_thread, task) {
            (Some(thread), _) => {
                let mut label = if thread.main {
                    "Baker".to_string()
                } else {
                    format!("Baker · {}", thread.title)
                };
                if thread.archived {
                    label.push_str(" (archived)");
                }
                ("baker", label)
            }
            (None, true) => ("task", format!("Task · {}", s.title.as_ref().unwrap_or(&s.name))),
            (None, false) if s.project.is_empty() || s.project == s.name => {
                ("project", s.title.clone().unwrap_or_else(|| s.name.clone()))
            }
            (None, false) => ("project", format!("{} · {}", s.project, s.title.as_ref().unwrap_or(&s.name))),
        };
        for agent in &s.agents {
            let Some(seat) = seat_of(&agent.role_id) else {
                continue;
            };
            out.push(Member {
                stream_id: s.stream_id.clone(),
                seat,
                socket: socket.clone(),
                scope,
                model_id: agent.assigned_model_id.clone(),
                busy: agent.busy,
                workspace: s.worktree_path.clone(),
            });
        }
    }
    Ok(out)
}

/// A native runtime (its own agent loop, in the sandbox) rather than an API
/// model driven by Bakery's tool loop.
fn is_native(state: &AppState, model_id: &str) -> bool {
    model_id.eq_ignore_ascii_case(crate::claude_native::ACCOUNT_ID)
        || model_id.eq_ignore_ascii_case(crate::pipe::ACP_MODEL_ID)
        || model_id.eq_ignore_ascii_case("grok-acp")
        || state
            .node
            .lock()
            .unwrap()
            .acp_plugs
            .iter()
            .any(|plug| plug.id.eq_ignore_ascii_case(model_id))
}

// ─── Abilities ──────────────────────────────────────────────────────────────

#[derive(schemars::JsonSchema, Debug, Serialize)]
struct Tool {
    name: String,
    summary: String,
    read_only: bool,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
struct Sandbox {
    home: String,
    /// Everywhere outside the kernel's own system paths this runtime can write.
    writes: Vec<String>,
    /// Extra folders it can read (beyond its workspace and home).
    reads: Vec<String>,
    /// Extra folders it can run programs from.
    runs: Vec<String>,
    /// Project folders it can read but never change.
    never_writes: Vec<String>,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
struct Agent {
    stream_id: String,
    seat: &'static str,
    agent_id: String,
    socket: String,
    scope: &'static str,
    model_id: Option<String>,
    /// `native` (its own agent loop, sandboxed), `api` (Bakery's tool
    /// loop), or `none` (no model assigned).
    transport: &'static str,
    busy: bool,
    /// What the runtime reported about itself when it started — name and
    /// version, sign-in, models, effort, modes, slash commands, context.
    /// `None` while no runtime is running for this agent.
    runtime: Option<crate::pipe::AcpRuntimeSnapshot>,
    bakery_tools: Vec<Tool>,
    sandbox: Option<Sandbox>,
    /// Its place on the scope dial (Settings → Agents).
    reach: crate::machine_access::Reach,
    /// Whether it may use containers, through Bakery's container tools.
    containers: bool,
    /// Its container seat's CPUs and memory, when the operator set them;
    /// `None` is the default (`AgentsView::seat_default`).
    seat_size: Option<crate::machine_access::SeatSize>,
    /// Its calls through the model proxy; `None` before its first.
    model_usage: Option<crate::model_proxy::AgentUsage>,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct KindPolicy {
    kind: ChildToolKind,
    /// `always`, `ask`, or `never`.
    policy: &'static str,
    /// Read-only kinds are never asked about: `always` unless set `never`.
    read_only: bool,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct AgentsView {
    agents: Vec<Agent>,
    tool_policy: Vec<KindPolicy>,
    write_mode: crate::node::WriteMode,
    /// What a container seat gets of this Mac unless set per agent.
    seat_default: crate::machine_access::SeatSize,
}

const KINDS: &[ChildToolKind] = &[
    ChildToolKind::Read,
    ChildToolKind::Search,
    ChildToolKind::Think,
    ChildToolKind::Fetch,
    ChildToolKind::Edit,
    ChildToolKind::Delete,
    ChildToolKind::Move,
    ChildToolKind::Execute,
    ChildToolKind::SwitchMode,
    ChildToolKind::Other,
];

fn tool_policy(state: &AppState) -> Vec<KindPolicy> {
    let node = state.node.lock().unwrap();
    KINDS
        .iter()
        .map(|&kind| {
            let read_only = kind.is_auto_safe();
            let policy = if node.denied_child_tools.contains(&kind) {
                "never"
            } else if read_only || node.always_kinds.contains(&kind) {
                "always"
            } else {
                "ask"
            };
            KindPolicy {
                kind,
                policy,
                read_only,
            }
        })
        .collect()
}

const READ_ONLY_TOOLS: &[&str] = &["read_file", "glob", "grep", "list_dir"];

fn bakery_tools(state: &AppState, member: &Member, native: bool) -> Vec<Tool> {
    let from_decls = |decls: Vec<bakery_agent::ToolDecl>| {
        decls
            .into_iter()
            .map(|d| Tool {
                read_only: READ_ONLY_TOOLS.contains(&d.name.as_str())
                    || crate::api_tools::risk_of(&d.name) == Some(crate::api_catalog::Risk::Read),
                name: d.name,
                summary: d.description,
            })
            .collect::<Vec<_>>()
    };
    if native {
        // A native runtime brings its own tools; Bakery adds only what its
        // MCP server hands this socket.
        return crate::mcp::mcp_scope_for_stream(state, &member.stream_id)
            .map(|scope| from_decls(crate::mcp::tool_decls_for(state, &member.stream_id, scope)))
            .unwrap_or_default();
    }
    let mut decls = crate::tools::http_tool_decls(true, &[]);
    let limit = crate::api_tools::tools_limit(state, &member.stream_id);
    if member.scope == "baker" {
        decls.extend(crate::api_tools::baker_decls(limit));
    } else {
        decls.extend(crate::api_tools::agent_decls(limit));
    }
    if member.scope == "task" && crate::machine_access::containers_on(state, &member.stream_id) {
        decls.extend(crate::containers::worker_decls());
    }
    from_decls(decls)
}

fn sandbox(state: &AppState, member: &Member) -> Sandbox {
    let home = bakery_exec::seat_acp_home(
        &crate::state::cache_root(state),
        &member.stream_id,
        member.seat.as_str(),
    );
    let mut writes = vec![home.display().to_string()];
    if crate::state::stream_host_allowed(state, &member.stream_id) {
        writes.push(member.workspace.clone());
    }
    if member
        .model_id
        .as_deref()
        .is_some_and(|m| m.eq_ignore_ascii_case(crate::claude_native::ACCOUNT_ID))
    {
        writes.push(
            crate::claude_native::credential_dir(state)
                .display()
                .to_string(),
        );
    }
    let grants = crate::machine_access::shown_for(state, &member.stream_id);
    writes.extend(grants.write.iter().map(|p| p.display().to_string()));
    Sandbox {
        home: home.display().to_string(),
        writes,
        reads: grants
            .read
            .iter()
            .map(|p| p.display().to_string())
            .collect(),
        runs: grants
            .exec
            .iter()
            .map(|p| p.display().to_string())
            .collect(),
        never_writes: bakery_exec::AGENT_STATE_DIRS
            .iter()
            .map(|d| d.to_string())
            .collect(),
    }
}

pub(crate) async fn agents(State(st): State<AppState>) -> Result<Json<AgentsView>, HttpError> {
    let members = roster(&st).await?;
    crate::actor::spawn_blocking(move || {
        let agents = members
            .into_iter()
            .map(|member| {
                let native = member
                    .model_id
                    .as_deref()
                    .is_some_and(|m| is_native(&st, m));
                let transport = match (&member.model_id, native) {
                    (None, _) => "none",
                    (Some(_), true) => "native",
                    (Some(_), false) => "api",
                };
                let agent_id = crate::node::agent_id(&member.stream_id, member.seat);
                Agent {
                    model_usage: crate::model_proxy::agent_usage(&st, &agent_id),
                    agent_id,
                    seat: member.seat.as_str(),
                    runtime: native
                        .then(|| {
                            crate::pipe::acp_runtime_snapshot(&st, &member.stream_id, member.seat)
                        })
                        .flatten(),
                    bakery_tools: bakery_tools(&st, &member, native),
                    sandbox: native.then(|| sandbox(&st, &member)),
                    reach: crate::machine_access::reach_for(&st, &member.stream_id),
                    containers: crate::machine_access::containers_on(&st, &member.stream_id),
                    seat_size: st
                        .node
                        .lock()
                        .unwrap()
                        .machine_access
                        .seat_size
                        .get(&member.stream_id)
                        .copied(),
                    transport,
                    stream_id: member.stream_id,
                    socket: member.socket,
                    scope: member.scope,
                    model_id: member.model_id,
                    busy: member.busy,
                }
            })
            .collect();
        let write_mode = st.node.lock().unwrap().write_mode;
        AgentsView {
            agents,
            tool_policy: tool_policy(&st),
            write_mode,
            seat_default: crate::seat_container::seat_default(),
        }
    })
    .await
    .map(Json)
    .map_err(internal)
}

pub(crate) async fn tool_policy_view(State(st): State<AppState>) -> Json<Vec<KindPolicy>> {
    Json(tool_policy(&st))
}

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct SetPolicy {
    kind: ChildToolKind,
    policy: String,
}

/// Node-wide, for every runtime: `always` (never asked again), `ask` (a gate
/// each time), or `never` (refused before any mode or allow list).
pub(crate) async fn set_tool_policy(
    State(st): State<AppState>,
    Json(req): Json<SetPolicy>,
) -> Result<Json<Vec<KindPolicy>>, HttpError> {
    crate::actor::spawn_blocking(move || {
        {
            let mut node = st.node.lock().unwrap();
            node.always_kinds.retain(|k| *k != req.kind);
            node.denied_child_tools.retain(|k| *k != req.kind);
            match req.policy.as_str() {
                "never" => node.denied_child_tools.push(req.kind),
                "always" if !req.kind.is_auto_safe() => node.always_kinds.push(req.kind),
                "always" => {}
                "ask" if req.kind.is_auto_safe() => {
                    return Err((
                        StatusCode::BAD_REQUEST,
                        "read-only kinds are never asked about — choose always or never"
                            .to_string(),
                    ))
                }
                "ask" => {}
                other => {
                    return Err((
                        StatusCode::BAD_REQUEST,
                        format!("unknown policy {other:?} — use always, ask, or never"),
                    ))
                }
            }
            node.save(&st.base).map_err(internal)?;
        }
        Ok(Json(tool_policy(&st)))
    })
    .await
    .map_err(internal)?
}

fn always_allowed_view(st: &AppState) -> Vec<crate::pipe::GrantView> {
    let grants = st.node.lock().unwrap().always_grants.clone();
    grants.into_iter().map(|g| g.view(st)).collect()
}

/// Every "Always allow" an approval card made, with its project's name.
pub(crate) async fn always_allowed(State(st): State<AppState>) -> Json<Vec<crate::pipe::GrantView>> {
    Json(crate::actor::spawn_blocking(move || always_allowed_view(&st)).await.unwrap_or_default())
}

/// Takes one back: that kind asks again in that scope.
pub(crate) async fn remove_always_allowed(
    State(st): State<AppState>,
    Json(grant): Json<crate::pipe::AlwaysGrant>,
) -> Result<Json<Vec<crate::pipe::GrantView>>, HttpError> {
    crate::actor::spawn_blocking(move || {
        {
            let mut node = st.node.lock().unwrap();
            node.always_grants.retain(|g| *g != grant);
            node.save(&st.base).map_err(internal)?;
        }
        Ok(Json(always_allowed_view(&st)))
    })
    .await
    .map_err(internal)?
}

#[derive(schemars::JsonSchema, Debug, Serialize, Deserialize)]
pub(crate) struct PeerLimit {
    /// The most open peers agents may open in one project, or one workspace.
    limit: usize,
}

pub(crate) async fn peer_limit_view(State(st): State<AppState>) -> Json<PeerLimit> {
    Json(PeerLimit { limit: crate::baker_threads::peer_limit(&st) })
}

pub(crate) async fn set_peer_limit(
    State(st): State<AppState>,
    Json(req): Json<PeerLimit>,
) -> Result<Json<PeerLimit>, HttpError> {
    crate::actor::spawn_blocking(move || {
        let mut node = st.node.lock().unwrap();
        node.peer_limit = Some(req.limit);
        node.save(&st.base).map_err(internal)?;
        Ok(Json(req))
    })
    .await
    .map_err(internal)?
}

// ─── Conversation status ────────────────────────────────────────────────────
// One answer, for every list that shows a conversation, to "what is it doing
// and does it need me": working, waiting on you, failed, done (close it when
// you are finished), or idle.

#[derive(schemars::JsonSchema, Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ConversationStatus {
    /// A turn is running.
    Working,
    /// Waiting on you: a change to review, a permission prompt, a question —
    /// or its last reply asked you something.
    NeedsInput,
    /// Its last turn failed or hit its step limit.
    Failed,
    /// Its last turn finished and nothing is waiting on you.
    Done,
    /// Nothing has run yet, or it was stopped.
    Idle,
}

const TURN_FAMILIES: &[&str] = &[
    "agent.turn.started",
    "agent.turn.done",
    "agent.turn.failed",
    "agent.turn.exhausted",
    "agent.turn.cancelled",
];

/// Which agents are parked on the human right now, and on what — read once,
/// before the Heart is locked (never nested under it).
pub(crate) fn awaiting_by_agent(
    state: &AppState,
) -> std::collections::HashMap<String, crate::state::ApprovalKind> {
    state
        .agent_activity
        .lock()
        .unwrap()
        .iter()
        .filter_map(|(agent_id, entry)| match &entry.snapshot.phase {
            crate::state::ActivityPhase::AwaitingApproval { kind, .. } => {
                Some((agent_id.clone(), *kind))
            }
            _ => None,
        })
        .collect()
}

fn clip(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        return text.to_string();
    }
    let cut: String = text.chars().take(max).collect();
    format!("{}…", cut.trim_end())
}

/// The question a reply ends on, if it ends on one.
fn question_in(reply: &str) -> Option<String> {
    let last = reply.lines().map(str::trim).rfind(|l| !l.is_empty())?;
    // Markdown dressing (`**…?**`, `> …?`, `- …?`) is not part of the question.
    let bare = last.trim_matches(['#', '*', '_', '`', '>', '-', ' ']);
    bare.ends_with('?').then(|| clip(bare, 140))
}

/// A conversation's status and a one-line reason.
pub(crate) fn conversation_status_locked(
    heart: &bakery_heart::Heart,
    stream_id: &str,
    busy: bool,
    pending_gates: usize,
    awaiting: Option<crate::state::ApprovalKind>,
) -> (ConversationStatus, Option<String>) {
    use crate::state::ApprovalKind;
    if let Some(kind) = awaiting {
        let note = match kind {
            ApprovalKind::Question => "asked you a question",
            ApprovalKind::ChildTool => "asks permission to act",
            ApprovalKind::FileEdit => "a change to review",
        };
        return (ConversationStatus::NeedsInput, Some(note.to_string()));
    }
    if pending_gates > 0 {
        let s = if pending_gates == 1 { "" } else { "s" };
        return (
            ConversationStatus::NeedsInput,
            Some(format!("{pending_gates} change{s} to review")),
        );
    }
    if let Some(host) = crate::host_list::asking_locked(heart, stream_id) {
        return (
            ConversationStatus::NeedsInput,
            Some(format!("wants to reach {host}")),
        );
    }
    if let Some(path) = crate::secret_reads::asking_locked(heart, stream_id) {
        return (
            ConversationStatus::NeedsInput,
            Some(format!("wants to read {path}")),
        );
    }
    if let Some(what) = crate::mac_sandbox::installing_locked(heart, stream_id) {
        return (
            ConversationStatus::NeedsInput,
            Some(format!("wants to install {what}")),
        );
    }
    if busy {
        // A turn waiting out a passing failure says what it waits on.
        let since = [
            crate::resume::RETRYING,
            "agent.turn.started",
            "agent.tool.called",
            "agent.acp.text",
            "agent.acp.tool_call",
        ];
        let waiting = heart
            .last_of_families(stream_id, &since)
            .ok()
            .flatten()
            .filter(|e| e.family == crate::resume::RETRYING)
            .and_then(|e| serde_json::from_str::<serde_json::Value>(&e.payload).ok())
            .and_then(|p| p["reason"].as_str().map(|r| format!("waiting out {}; it keeps trying", clip(r, 100))));
        return (ConversationStatus::Working, waiting);
    }
    let Some(last) = heart
        .last_of_families(stream_id, TURN_FAMILIES)
        .ok()
        .flatten()
    else {
        return (ConversationStatus::Idle, None);
    };
    let payload: serde_json::Value = serde_json::from_str(&last.payload).unwrap_or_default();
    let text = |key: &str| {
        payload
            .get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
    };
    match last.family.as_str() {
        "agent.turn.done" => match question_in(text("summary")) {
            Some(question) => (ConversationStatus::NeedsInput, Some(question)),
            None => {
                let first = text("summary")
                    .lines()
                    .map(str::trim)
                    .find(|l| !l.is_empty());
                (ConversationStatus::Done, first.map(|l| clip(l, 120)))
            }
        },
        "agent.turn.failed" => (ConversationStatus::Failed, Some(clip(text("reason"), 140))),
        "agent.turn.exhausted" => (
            ConversationStatus::Failed,
            Some("hit its step limit — send a message to continue".to_string()),
        ),
        "agent.turn.cancelled" => (ConversationStatus::Idle, Some("stopped".to_string())),
        // Started and never finished, and nothing is running it now — the
        // node restarted under it.
        _ => (
            ConversationStatus::Idle,
            Some("interrupted — send a message to continue".to_string()),
        ),
    }
}

// ─── Agent map ──────────────────────────────────────────────────────────────
// Every agent and who started whom: the Baker and its threads, the tasks and
// chats a Baker conversation created, project agents, and every subagent a
// native runtime launched in its latest turn (Claude's Agent and Task calls).

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct MapNode {
    id: String,
    /// "baker" | "thread" | "task" | "chat" | "project" | "subagent"
    kind: &'static str,
    label: String,
    /// The model it runs on, or a subagent's type.
    detail: Option<String>,
    /// "working" | "needs_input" | "failed" | "done" | "idle" | "stopped"
    status: &'static str,
    note: Option<String>,
    /// The node that started it; none for a top-level agent.
    parent: Option<String>,
    stream_id: String,
    seat: String,
    /// A subagent's launching tool call.
    tool_call_id: Option<String>,
    /// The tool calls a subagent made so far.
    steps: usize,
    last_ts: String,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct AgentMap {
    nodes: Vec<MapNode>,
}

/// Events read per agent for its subagents — enough for one long turn.
const MAP_TAIL_EVENTS: usize = 3000;

fn status_word(status: ConversationStatus) -> &'static str {
    match status {
        ConversationStatus::Working => "working",
        ConversationStatus::NeedsInput => "needs_input",
        ConversationStatus::Failed => "failed",
        ConversationStatus::Done => "done",
        ConversationStatus::Idle => "idle",
    }
}

/// The subagents `agent_id` launched in its latest native turn, newest last.
/// A call is a subagent's launch when the runtime marks it one (Claude's
/// `_meta.claudeCode.toolName` "Agent"/"Task") or when another call names it
/// as its parent. A launch nested inside a subagent hangs under that one.
fn subagents_locked(
    heart: &bakery_heart::Heart,
    stream_id: &str,
    agent_id: &str,
    seat: &str,
    busy: bool,
    out: &mut Vec<MapNode>,
) {
    use serde_json::Value;
    struct Call {
        id: String,
        title: String,
        input: Value,
        parent: Option<String>,
        launch: bool,
        status: Option<String>,
        last: String,
    }
    let Ok(events) = heart.read_stream_tail(stream_id, MAP_TAIL_EVENTS) else {
        return;
    };
    let mine: Vec<(&bakery_core::Event, Value)> = events
        .iter()
        .filter(|e| e.family == crate::pipe::ACP_TOOL_EVENT_FAMILY)
        .filter_map(|e| {
            let p: Value = serde_json::from_str(&e.payload).ok()?;
            (p["agent_id"] == agent_id).then_some((e, p))
        })
        .collect();
    let Some(turn) = mine.last().map(|(_, p)| p["turn_id"].clone()) else {
        return;
    };
    let mut calls: Vec<Call> = Vec::new();
    for (event, p) in mine.iter().filter(|(_, p)| p["turn_id"] == turn) {
        let detail = &p["detail"];
        let Some(id) = detail["toolCallId"].as_str() else {
            continue;
        };
        let meta = &detail["_meta"]["claudeCode"];
        let at = match calls.iter().position(|c| c.id == id) {
            Some(at) => at,
            None => {
                calls.push(Call {
                    id: id.to_string(),
                    title: String::new(),
                    input: Value::Null,
                    parent: None,
                    launch: false,
                    status: None,
                    last: String::new(),
                });
                calls.len() - 1
            }
        };
        let call = &mut calls[at];
        if let Some(title) = detail["title"].as_str().filter(|t| !t.trim().is_empty()) {
            call.title = title.trim().to_string();
        }
        if detail["rawInput"].is_object() {
            call.input = detail["rawInput"].clone();
        }
        if call.parent.is_none() {
            call.parent = meta["parentToolUseId"].as_str().map(str::to_string);
        }
        call.launch |= matches!(meta["toolName"].as_str(), Some("Agent" | "Task")) || meta["subagent"] == true;
        if let Some(status) = detail["status"].as_str() {
            call.status = Some(status.to_string());
        }
        call.last = event.ts.clone();
    }
    let named: std::collections::HashSet<&str> =
        calls.iter().filter_map(|c| c.parent.as_deref()).collect();
    let node_id = |call: &str| format!("{agent_id}/{call}");
    for call in calls.iter().filter(|c| c.launch || named.contains(c.id.as_str())) {
        let status = match call.status.as_deref() {
            Some("completed") => "done",
            Some("failed") => "failed",
            // The latest turn is over unless the agent is still busy.
            _ if !busy => "stopped",
            _ => "working",
        };
        let label = call.input["description"]
            .as_str()
            .filter(|d| !d.trim().is_empty())
            .map(|d| d.trim().to_string())
            .or_else(|| (!call.title.is_empty()).then(|| call.title.clone()))
            .unwrap_or_else(|| "Subagent".to_string());
        out.push(MapNode {
            id: node_id(&call.id),
            kind: "subagent",
            label: clip(&label, 80),
            detail: call.input["subagent_type"].as_str().map(str::to_string),
            status,
            note: None,
            parent: Some(match &call.parent {
                Some(parent) if calls.iter().any(|c| &c.id == parent) => node_id(parent),
                _ => agent_id.to_string(),
            }),
            stream_id: stream_id.to_string(),
            seat: seat.to_string(),
            tool_call_id: Some(call.id.clone()),
            steps: calls
                .iter()
                .filter(|c| c.parent.as_deref() == Some(call.id.as_str()))
                .count(),
            last_ts: call.last.clone(),
        });
    }
}

/// `GET /agents/map`
pub(crate) async fn agent_map(State(st): State<AppState>) -> Result<Json<AgentMap>, HttpError> {
    let Json(studio) = crate::routes::studio::studio_state(State(st.clone())).await?;
    // Read before the Heart is locked, never under it.
    let awaiting = awaiting_by_agent(&st);
    let baker = crate::baker_threads::baker_pointer(&st);
    crate::actor::spawn_blocking(move || {
        let sockets: Vec<_> = studio
            .factory_sockets
            .into_iter()
            .chain(studio.projects.into_iter().flat_map(|p| p.sockets))
            .chain(studio.ungrouped_sockets)
            .filter(|s| !s.discarded && !s.archived)
            .collect();
        let heart = st.heart.lock().unwrap();
        let mut nodes = Vec::new();
        // Each peer's node, and the conversation that opened it.
        let mut opened: Vec<(usize, String)> = Vec::new();
        for s in &sockets {
            let task = crate::state::socket_task_id(&heart, &s.stream_id).is_some();
            let spawned_by = heart
                .last_of_family(&s.stream_id, crate::baker_ops::SPAWNED_BY_FAMILY)
                .ok()
                .flatten()
                .and_then(|e| serde_json::from_str::<serde_json::Value>(&e.payload).ok())
                .and_then(|p| p["by"].as_str().map(str::to_string));
            let opener = crate::baker_threads::opened_by_locked(&heart, &s.stream_id);
            let (kind, label) = match (&s.baker_thread, task) {
                (Some(thread), _) if thread.main => ("baker", "Baker".to_string()),
                (Some(thread), _) => ("thread", format!("Baker · {}", thread.title)),
                (None, true) => ("task", format!("Task · {}", s.name)),
                (None, false) if s.project.is_empty() => ("chat", s.name.clone()),
                (None, false) if s.project == s.name => ("project", s.name.clone()),
                (None, false) => ("project", format!("{} · {}", s.project, s.name)),
            };
            // A thread hangs under the main Baker unless another Baker
            // conversation started it.
            let owner = match (&s.baker_thread, &baker) {
                (Some(thread), Some(main)) if !thread.main => {
                    Some(crate::node::agent_id(main, Seat::Baker))
                }
                _ => None,
            };
            for agent in s.agents.iter().filter(|a| a.assigned_model_id.is_some() || a.busy) {
                let (status, note) = conversation_status_locked(
                    &heart,
                    &s.stream_id,
                    agent.busy,
                    s.pending_gates,
                    awaiting.get(&agent.agent_id).copied(),
                );
                let default_seat = matches!(agent.role_id.as_str(), "orchestrator" | "baker");
                if let Some(opener) = opener.clone().filter(|_| spawned_by.is_none()) {
                    opened.push((nodes.len(), opener));
                }
                nodes.push(MapNode {
                    id: agent.agent_id.clone(),
                    kind,
                    label: if default_seat {
                        label.clone()
                    } else {
                        format!("{label} · {}", agent.role_label)
                    },
                    detail: agent.assigned_model_id.clone(),
                    status: status_word(status),
                    note,
                    parent: spawned_by
                        .clone()
                        .or_else(|| owner.clone())
                        .filter(|parent| *parent != agent.agent_id),
                    stream_id: s.stream_id.clone(),
                    seat: agent.role_id.clone(),
                    tool_call_id: None,
                    steps: 0,
                    last_ts: s.last_ts.clone(),
                });
                subagents_locked(
                    &heart,
                    &s.stream_id,
                    &agent.agent_id,
                    &agent.role_id,
                    agent.busy,
                    &mut nodes,
                );
            }
        }
        // A peer hangs under the conversation that opened it (a worker
        // under its orc), while that one is on the map.
        for (at, opener) in opened {
            let by = nodes.iter().find(|n| n.stream_id == opener && n.kind != "subagent").map(|n| n.id.clone());
            if let Some(by) = by.filter(|by| *by != nodes[at].id) {
                nodes[at].parent = Some(by);
            }
        }
        // An edge to a conversation no longer on the map would dangle.
        let ids: std::collections::HashSet<String> = nodes.iter().map(|n| n.id.clone()).collect();
        for node in &mut nodes {
            if node.parent.as_ref().is_some_and(|parent| !ids.contains(parent)) {
                node.parent = None;
            }
        }
        Ok(Json(AgentMap { nodes }))
    })
    .await
    .map_err(internal)?
}

// ─── Conversation actions ───────────────────────────────────────────────────
// The same actions for every conversation, whatever its kind or runtime:
// restart its runtime, archive or restore it. Delete is `/stream/discard`.

/// Archive facts for every socket that is not a Baker thread (those keep
/// `baker.thread.archived`, which also enforces their own limits).
pub(crate) const SOCKET_ARCHIVED_FAMILY: &str = "socket.archived";

#[derive(Debug, Serialize, Deserialize)]
struct Archived {
    archived: bool,
}

/// Hidden from conversation lists, everything else kept.
pub(crate) fn socket_archived_locked(heart: &bakery_heart::Heart, stream_id: &str) -> bool {
    heart
        .last_of_family(stream_id, SOCKET_ARCHIVED_FAMILY)
        .ok()
        .flatten()
        .and_then(|ev| serde_json::from_str::<Archived>(&ev.payload).ok())
        .is_some_and(|a| a.archived)
}

fn turn_running(state: &AppState, stream_id: &str) -> bool {
    state
        .lifecycle
        .lock()
        .unwrap()
        .running_turns
        .iter()
        .any(|((sid, _), n)| sid == stream_id && *n > 0)
}

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct ArchiveReq {
    stream_id: String,
    archived: bool,
}

pub(crate) async fn archive_conversation(
    State(st): State<AppState>,
    Json(req): Json<ArchiveReq>,
) -> Result<Json<serde_json::Value>, HttpError> {
    crate::actor::spawn_blocking(move || {
        set_archived(&st, &req.stream_id, req.archived, "human")
            .map(|()| Json(serde_json::json!({ "archived": req.archived })))
    })
    .await
    .map_err(internal)?
}

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct RenameConversationReq {
    stream_id: String,
    /// Its new name: the first line, clipped.
    title: String,
}

/// `POST /conversation/rename`.
pub(crate) async fn rename_conversation(
    State(st): State<AppState>,
    Json(req): Json<RenameConversationReq>,
) -> Result<Json<serde_json::Value>, HttpError> {
    crate::actor::spawn_blocking(move || {
        rename(&st, &req.stream_id, &req.title, "human").map(|title| Json(serde_json::json!({ "title": title })))
    })
    .await
    .map_err(internal)?
}

/// Name any conversation: a Baker thread's title, or the name a project's
/// thread or a chat shows (the one it was opened with stays, for reopening
/// it). The main Baker keeps its name.
pub(crate) fn rename(st: &AppState, stream_id: &str, title: &str, actor: &str) -> Result<String, HttpError> {
    if crate::baker_threads::is_baker_thread_stream(st, stream_id)
        || crate::baker_ops::is_permanent_baker_stream(st, stream_id)
    {
        return crate::baker_threads::rename_thread(st, stream_id, title, actor)
            .map(|thread| thread.title)
            .map_err(|e| (StatusCode::CONFLICT, e));
    }
    crate::state::context_for(st, stream_id).map_err(|e| (StatusCode::NOT_FOUND, e))?;
    let title = crate::baker_threads::clean_title(title)
        .ok_or_else(|| (StatusCode::BAD_REQUEST, "a name must not be empty".to_string()))?;
    let fact = serde_json::json!({ "title": title }).to_string();
    crate::state::append(st, stream_id, actor, crate::state::CONVERSATION_RENAMED_FAMILY, fact, vec![])
        .map_err(internal)?;
    Ok(title)
}

/// Archive or restore any conversation — the route's and the Baker's one path.
pub(crate) fn set_archived(
    st: &AppState,
    stream_id: &str,
    archived: bool,
    actor: &str,
) -> Result<(), HttpError> {
    if crate::baker_ops::is_permanent_baker_stream(st, stream_id) {
        return Err((
            StatusCode::CONFLICT,
            "the main Baker conversation is never archived — archive its threads".to_string(),
        ));
    }
    if crate::baker_threads::is_baker_thread_stream(st, stream_id) {
        crate::baker_threads::set_thread_archived(st, stream_id, archived, actor)
            .map_err(|e| (StatusCode::CONFLICT, e))?;
        if archived {
            crate::views::stop_all(stream_id);
        }
        return Ok(());
    }
    crate::state::context_for(st, stream_id).map_err(|e| (StatusCode::NOT_FOUND, e))?;
    if archived && turn_running(st, stream_id) {
        return Err((
            StatusCode::CONFLICT,
            "a turn is running — stop it before archiving".to_string(),
        ));
    }
    crate::state::append(
        st,
        stream_id,
        actor,
        SOCKET_ARCHIVED_FAMILY,
        serde_json::json!({ "archived": archived }).to_string(),
        vec![],
    )
    .map_err(internal)?;
    if archived {
        // Its editor and previews end with it.
        crate::views::stop_all(stream_id);
    }
    Ok(())
}

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct StreamReq {
    stream_id: String,
}

/// Stops a conversation's live runtime so its next turn starts a fresh one
/// (its history is replayed into it). Never while a turn is running.
pub(crate) fn restart_runtime_inner(state: &AppState, stream_id: &str) -> Result<bool, String> {
    if turn_running(state, stream_id) {
        return Err("a turn is running — stop it first".to_string());
    }
    let had = state
        .acp_sessions
        .lock()
        .unwrap()
        .keys()
        .any(|(sid, _)| sid == stream_id);
    if had {
        crate::state::restart_acp_runtimes(state, stream_id);
    }
    Ok(had)
}

pub(crate) async fn restart_runtime(
    State(st): State<AppState>,
    Json(req): Json<StreamReq>,
) -> Result<Json<serde_json::Value>, HttpError> {
    crate::actor::spawn_blocking(move || restart_runtime_inner(&st, &req.stream_id))
        .await
        .map_err(internal)?
        .map(|had| Json(serde_json::json!({ "restarted": had })))
        .map_err(|e| (StatusCode::CONFLICT, e))
}

// ─── Storage ────────────────────────────────────────────────────────────────

/// Walks stop here; a bigger folder reports `truncated`.
const MAX_ENTRIES: u64 = 50_000;

#[derive(schemars::JsonSchema, Debug, Default, Serialize)]
struct Measure {
    exists: bool,
    bytes: u64,
    files: u64,
    newest: Option<String>,
    truncated: bool,
}

fn measure(path: &Path) -> Measure {
    let mut m = Measure {
        exists: path.exists(),
        ..Default::default()
    };
    let mut newest: Option<SystemTime> = None;
    let mut stack = vec![path.to_path_buf()];
    let mut seen = 0u64;
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            seen += 1;
            if seen > MAX_ENTRIES {
                m.truncated = true;
                stack.clear();
                break;
            }
            let Ok(meta) = entry.path().symlink_metadata() else {
                continue;
            };
            if meta.is_dir() {
                stack.push(entry.path());
            } else {
                m.files += 1;
                m.bytes += meta.len();
                if let Ok(modified) = meta.modified() {
                    newest = newest.max(Some(modified));
                }
            }
        }
    }
    m.newest = newest.map(|t| chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339());
    m
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
struct Area {
    label: &'static str,
    path: String,
    /// Deleting inside it is allowed (never while its runtime runs).
    clearable: bool,
    #[serde(flatten)]
    measure: Measure,
}

fn area(label: &'static str, path: PathBuf, clearable: bool) -> Area {
    Area {
        label,
        measure: measure(&path),
        path: path.display().to_string(),
        clearable,
    }
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
struct AgentStorage {
    stream_id: String,
    seat: &'static str,
    socket: String,
    scope: &'static str,
    model_id: Option<String>,
    running: bool,
    areas: Vec<Area>,
    /// Agent folders and instruction files present at the project root —
    /// yours, readable by agents, never writable by a runtime.
    project_files: Vec<String>,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
struct HostNote {
    what: &'static str,
    detail: &'static str,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct StorageView {
    agents: Vec<AgentStorage>,
    /// Folders shared by every agent on one account.
    shared: Vec<Area>,
    /// The only agent-related writes that happen outside the sandbox.
    host_side: Vec<HostNote>,
    never_in_projects: Vec<&'static str>,
}

const INSTRUCTION_FILES: &[&str] = &["CLAUDE.md", "AGENTS.md", "GEMINI.md", ".cursorrules"];

fn live_keys(state: &AppState) -> Vec<(String, Seat)> {
    state
        .acp_sessions
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, slot)| matches!(slot, crate::pipe::AcpSlot::Live(_)))
        .map(|(key, _)| key.clone())
        .collect()
}

pub(crate) async fn storage(State(st): State<AppState>) -> Result<Json<StorageView>, HttpError> {
    let members = roster(&st).await?;
    crate::actor::spawn_blocking(move || {
        let cache = crate::state::cache_root(&st);
        let live = live_keys(&st);
        let agents = members
            .into_iter()
            .map(|m| {
                let home = bakery_exec::seat_acp_home(&cache, &m.stream_id, m.seat.as_str());
                let workspace = PathBuf::from(&m.workspace);
                let project_files = bakery_exec::AGENT_STATE_DIRS
                    .iter()
                    .chain(INSTRUCTION_FILES)
                    .filter(|name| !m.workspace.is_empty() && workspace.join(name).exists())
                    .map(|name| name.to_string())
                    .collect();
                AgentStorage {
                    running: live.contains(&(m.stream_id.clone(), m.seat)),
                    areas: vec![
                        area("Private home", home.clone(), true),
                        area("Temp", home.join("tmp"), true),
                        area(
                            "Socket room (shared by this socket's agents)",
                            bakery_exec::socket_runtime_dir(&cache, &m.stream_id),
                            false,
                        ),
                    ],
                    project_files,
                    stream_id: m.stream_id,
                    seat: m.seat.as_str(),
                    socket: m.socket,
                    scope: m.scope,
                    model_id: m.model_id,
                }
            })
            .collect();
        StorageView {
            agents,
            shared: vec![area(
                "Claude account (sign-in, transcripts, todos, memory — every Claude agent)",
                crate::claude_native::credential_dir(&st),
                true,
            )],
            host_side: vec![
                HostNote {
                    what: "Claude sign-in and status check",
                    detail: "run outside the sandbox, with your home folder as HOME; the account, transcripts and settings go to Bakery's Claude folder above.",
                },
                HostNote {
                    what: "Codex and Cursor sign-in",
                    detail: "run each vendor's own login, which saves to its own folder (~/.codex, ~/.cursor); Bakery copies that login into each agent's private home when it starts, and the agent never writes back.",
                },
            ],
            never_in_projects: bakery_exec::AGENT_STATE_DIRS.to_vec(),
        }
    })
    .await
    .map(Json)
    .map_err(internal)
}

/// The folders storage may be browsed and deleted in — Bakery-owned agent
/// state only, never the operator's projects or home.
fn storage_roots(state: &AppState) -> Vec<PathBuf> {
    let cache = crate::state::cache_root(state);
    [
        cache.join("seats"),
        cache.join("sockets"),
        crate::claude_native::credential_dir(state),
    ]
    .iter()
    .filter_map(|root| root.canonicalize().ok())
    .collect()
}

/// `raw` resolved (symlinks followed) and proven inside a storage root.
fn inside_storage(state: &AppState, raw: &str) -> Result<(PathBuf, PathBuf), HttpError> {
    let path = Path::new(raw)
        .canonicalize()
        .map_err(|e| (StatusCode::NOT_FOUND, format!("{raw}: {e}")))?;
    let root = storage_roots(state)
        .into_iter()
        .find(|root| path.starts_with(root))
        .ok_or_else(|| (StatusCode::FORBIDDEN, format!("{raw} is not agent storage")))?;
    Ok((path, root))
}

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct PathQuery {
    path: String,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct Entry {
    name: String,
    dir: bool,
    bytes: u64,
    modified: Option<String>,
    /// Named like a credential: listed, never readable here.
    credential: bool,
}

pub(crate) async fn storage_list(
    State(st): State<AppState>,
    Query(q): Query<PathQuery>,
) -> Result<Json<Vec<Entry>>, HttpError> {
    crate::actor::spawn_blocking(move || {
        let (path, _) = inside_storage(&st, &q.path)?;
        let mut out: Vec<Entry> = std::fs::read_dir(&path)
            .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?
            .flatten()
            .filter_map(|entry| {
                let meta = entry.path().symlink_metadata().ok()?;
                Some(Entry {
                    name: entry.file_name().to_string_lossy().into_owned(),
                    dir: meta.is_dir(),
                    bytes: if meta.is_dir() { 0 } else { meta.len() },
                    modified: meta
                        .modified()
                        .ok()
                        .map(|t| chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339()),
                    credential: bakery_exec::is_credential_filename(&entry.path()),
                })
            })
            .collect();
        out.sort_by(|a, b| b.dir.cmp(&a.dir).then(a.name.cmp(&b.name)));
        Ok(Json(out))
    })
    .await
    .map_err(internal)?
}

const MAX_READ: usize = 256 * 1024;

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct FileView {
    text: String,
    bytes: u64,
    truncated: bool,
    binary: bool,
}

pub(crate) async fn storage_file(
    State(st): State<AppState>,
    Query(q): Query<PathQuery>,
) -> Result<Json<FileView>, HttpError> {
    crate::actor::spawn_blocking(move || {
        let (path, _) = inside_storage(&st, &q.path)?;
        if bakery_exec::is_credential_filename(&path) {
            return Err((
                StatusCode::FORBIDDEN,
                "a credential file is never shown — sign out from Provider accounts instead"
                    .to_string(),
            ));
        }
        let bytes = std::fs::read(&path).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
        let binary = bytes.iter().take(8192).any(|&b| b == 0);
        let shown = &bytes[..bytes.len().min(MAX_READ)];
        Ok(Json(FileView {
            text: if binary {
                String::new()
            } else {
                String::from_utf8_lossy(shown).into_owned()
            },
            bytes: bytes.len() as u64,
            truncated: bytes.len() > MAX_READ,
            binary,
        }))
    })
    .await
    .map_err(internal)?
}

/// What a running runtime is using right now: its own home, and — while any
/// Claude agent runs — the shared Claude account folder.
fn in_use(state: &AppState, path: &Path) -> Option<String> {
    let cache = crate::state::cache_root(state);
    let claude_dir = crate::claude_native::credential_dir(state)
        .canonicalize()
        .ok();
    for (stream_id, seat) in live_keys(state) {
        let home = bakery_exec::seat_acp_home(&cache, &stream_id, seat.as_str());
        if home.canonicalize().is_ok_and(|home| path.starts_with(home)) {
            return Some(format!(
                "its runtime ({stream_id}) is running — stop it first"
            ));
        }
        let claude = crate::pipe::acp_runtime_snapshot(state, &stream_id, seat)
            .is_some_and(|s| s.account_id == crate::claude_native::ACCOUNT_ID);
        if claude
            && claude_dir
                .as_deref()
                .is_some_and(|dir| path.starts_with(dir))
        {
            return Some(
                "a Claude runtime is running and uses this folder — stop it first".to_string(),
            );
        }
    }
    None
}

pub(crate) async fn storage_delete(
    State(st): State<AppState>,
    Json(q): Json<PathQuery>,
) -> Result<Json<serde_json::Value>, HttpError> {
    crate::actor::spawn_blocking(move || {
        let (path, root) = inside_storage(&st, &q.path)?;
        if path == root {
            return Err((
                StatusCode::FORBIDDEN,
                "a storage root itself is never deleted".to_string(),
            ));
        }
        if bakery_exec::is_credential_filename(&path) {
            return Err((
                StatusCode::FORBIDDEN,
                "a credential file is removed by signing out, not here".to_string(),
            ));
        }
        if let Some(reason) = in_use(&st, &path) {
            return Err((StatusCode::CONFLICT, reason));
        }
        let meta = path.symlink_metadata().map_err(internal)?;
        if meta.is_dir() {
            std::fs::remove_dir_all(&path).map_err(internal)?;
        } else {
            std::fs::remove_file(&path).map_err(internal)?;
        }
        Ok(Json(
            serde_json::json!({ "deleted": path.display().to_string() }),
        ))
    })
    .await
    .map_err(internal)?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;
    use bakery_agent::StubModel;
    use std::sync::Arc;

    /// Storage can be browsed and cleaned only inside Bakery's own agent
    /// folders: never the operator's projects or home, never a root, never a
    /// credential file, and never under a runtime that is running.
    #[test]
    fn storage_is_reachable_only_inside_bakery_agent_folders() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let cache = crate::state::cache_root(&state);
        let home = bakery_exec::seat_acp_home(&cache, "wt:store", "worker");
        std::fs::create_dir_all(home.join("tmp")).unwrap();
        std::fs::write(home.join("tmp/scratch.txt"), "x").unwrap();
        std::fs::write(home.join("auth.json"), "{}").unwrap();
        let outside = tempfile::tempdir().unwrap();

        assert!(
            inside_storage(&state, &home.join("tmp/scratch.txt").display().to_string()).is_ok()
        );
        let err = inside_storage(&state, &outside.path().display().to_string()).unwrap_err();
        assert_eq!(err.0, StatusCode::FORBIDDEN);
        assert!(bakery_exec::is_credential_filename(&home.join("auth.json")));

        let seats_root = cache.join("seats").canonicalize().unwrap();
        let (root_path, root) = inside_storage(&state, &seats_root.display().to_string()).unwrap();
        assert_eq!(
            root_path, root,
            "a root resolves to itself and is refused by delete"
        );
        assert!(in_use(&state, &home.canonicalize().unwrap()).is_none());
    }

    /// The operator sees each "Always allow" with its project's name, and
    /// takes one back for good.
    #[tokio::test]
    async fn always_allowed_lists_each_grant_by_project_and_removes_one() {
        use crate::pipe::{AlwaysGrant, GrantScope};
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let (p, _) = crate::catalog::create_project(&state, "thebakery", None).unwrap();
        let grant = |scope, id: Option<&str>| AlwaysGrant { scope, id: id.map(str::to_string), kind: ChildToolKind::Execute };
        let project = grant(GrantScope::Project, Some(&p.project_id));
        state.node.lock().unwrap().always_grants = vec![project.clone(), grant(GrantScope::Baker, None)];

        let listed = serde_json::to_value(always_allowed(State(state.clone())).await.0).unwrap();
        assert_eq!(listed[0]["name"], "thebakery", "{listed}");
        assert_eq!(listed[1]["scope"], "baker", "{listed}");
        let left = remove_always_allowed(State(state.clone()), Json(project)).await.unwrap().0;
        assert_eq!(left.len(), 1);
        let reloaded = crate::node::NodeConfig::load_or_create(dir.path());
        assert_eq!(reloaded.always_grants, vec![grant(GrantScope::Baker, None)]);
    }

    /// Showing an agent starts nothing of its room: a native seat in a Mac
    /// sandbox project is listed with what it may write, its project's own
    /// copies not readied and its thread's door not opened.
    #[tokio::test]
    async fn listing_an_agent_readies_nothing_of_its_room() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let app = dir.path().join("app");
        std::fs::create_dir_all(&app).unwrap();
        let root = crate::catalog::DirectoryRootSpec::Attach(app.to_str().unwrap());
        let (project, thread) = crate::catalog::create_project(&state, "app", Some(root)).unwrap();
        let stacks = crate::node::ProjectStacks { sandbox: "mac".to_string(), ..Default::default() };
        crate::catalog::set_project_stacks(&state, &project.project_id, stacks).unwrap();
        let native = crate::claude_native::ACCOUNT_ID;
        crate::state::assign_seat_model(&state, &thread.stream_id, crate::node::Seat::Orchestrator, native).unwrap();

        let Json(view) = agents(State(state.clone())).await.unwrap();
        let shown = view.agents.iter().find(|a| a.stream_id == thread.stream_id && a.transport == "native").unwrap();
        let own = crate::mac_sandbox::own(&state, &project.project_id);
        let writes = &shown.sandbox.as_ref().unwrap().writes;
        assert!(writes.contains(&own.join("home").display().to_string()), "{writes:?}");
        assert!(!own.join("home").exists(), "its copies aren't readied");
        assert!(!dir.path().join("doors.json").exists(), "its door isn't opened");
    }

    /// Any conversation takes a name: a project's thread and a chat show it
    /// over the name they were opened with, which stays theirs to reopen by;
    /// a Baker thread's title changes; the main Baker keeps its name.
    #[tokio::test]
    async fn any_conversation_can_be_renamed() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let thread = crate::routes::project::open_stream(&state, &open_req(&repo, "work")).unwrap().context.stream_id;
        let chat = crate::catalog::create_chat(&state, "loose").unwrap().stream_id;
        let main = crate::routes::baker::ensure_permanent_baker(&state, None, None).unwrap().stream_id;
        let baker = crate::baker_threads::create_thread(&state, &Default::default(), "human", None, None).unwrap().thread.stream_id;

        assert_eq!(rename(&state, &thread, "  Fix the login\nand more", "human").unwrap(), "Fix the login");
        rename(&state, &chat, "Questions", "human").unwrap();
        assert_eq!(rename(&state, &baker, "Planning", "human").unwrap(), "Planning", "a Baker thread's own title");
        assert!(rename(&state, &main, "Not the Baker", "human").is_err(), "the main Baker keeps its name");
        assert_eq!(rename(&state, &thread, "   ", "human").unwrap_err().0, StatusCode::BAD_REQUEST);

        let shown = |sid: &str| crate::state::known_streams(&state).unwrap().into_iter().find(|s| s.stream_id == sid).unwrap();
        let work = shown(&thread);
        assert_eq!((work.name.as_str(), work.title.as_deref()), ("work", Some("Fix the login")), "opened as work, shown renamed");
        assert_eq!(shown(&chat).title.as_deref(), Some("Questions"));
        let Json(studio) = crate::routes::studio::studio_state(State(state.clone())).await.unwrap();
        let socket = studio.projects.iter().flat_map(|p| p.sockets.iter()).chain(&studio.ungrouped_sockets).find(|s| s.stream_id == thread).unwrap();
        assert_eq!(socket.title.as_deref(), Some("Fix the login"));
        let again = crate::routes::project::open_stream(&state, &open_req(&repo, "work")).unwrap().context.stream_id;
        assert_eq!(again, thread, "reopened by the name it was opened with");
    }

    /// Every agent on every socket appears in both views, whatever backs it,
    /// with the node-wide policy for every native tool kind.
    #[tokio::test]
    async fn every_socket_agent_is_listed_with_its_abilities_and_storage() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = crate::routes::project::open_stream(&state, &open_req(&repo, "roster"))
            .unwrap()
            .context
            .stream_id;

        let Json(view) = agents(State(state.clone())).await.unwrap();
        let mine: Vec<&Agent> = view
            .agents
            .iter()
            .filter(|a| a.stream_id == stream_id)
            .collect();
        assert!(!mine.is_empty(), "the socket's agents are listed");
        assert!(mine.iter().all(|a| a.scope == "project"));
        assert_eq!(view.tool_policy.len(), KINDS.len());

        let Json(stored) = storage(State(state.clone())).await.unwrap();
        let areas = stored
            .agents
            .iter()
            .find(|a| a.stream_id == stream_id)
            .map(|a| a.areas.len())
            .unwrap();
        assert_eq!(areas, 3, "home, temp, and the socket room");
        assert!(stored.never_in_projects.contains(&".claude"));
    }

    /// The map shows every agent and who started whom: a conversation another
    /// one created hangs under it, and the latest native turn's subagents hang
    /// under their agent — a nested one under its subagent — with what each
    /// is doing and how many steps it took.
    #[tokio::test]
    async fn the_agent_map_draws_every_agent_and_its_subagents() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let open = |name: &str| {
            crate::routes::project::open_stream(&state, &open_req(&repo, name))
                .unwrap()
                .context
                .stream_id
        };
        let (parent, child) = (open("map-parent"), open("map-child"));
        for stream in [&parent, &child] {
            crate::state::assign_seat_model(&state, stream, Seat::Orchestrator, "claude-native").unwrap();
        }
        let lead = crate::node::agent_id(&parent, Seat::Orchestrator);
        crate::state::append(
            &state,
            &child,
            "agent:baker",
            crate::baker_ops::SPAWNED_BY_FAMILY,
            serde_json::json!({ "by": lead, "op": "chat_create" }).to_string(),
            vec![],
        )
        .unwrap();
        let call = |turn: &str, detail: serde_json::Value| {
            crate::state::append(
                &state,
                &parent,
                "agent:orchestrator",
                crate::pipe::ACP_TOOL_EVENT_FAMILY,
                serde_json::json!({ "event": "tool_call", "agent_id": lead, "turn_id": turn, "detail": detail })
                    .to_string(),
                vec![],
            )
            .unwrap();
        };
        let meta = |name: &str, parent: Option<&str>| {
            serde_json::json!({ "claudeCode": { "toolName": name, "parentToolUseId": parent } })
        };
        call("t1", serde_json::json!({ "toolCallId": "old", "_meta": meta("Agent", None) }));
        call(
            "t2",
            serde_json::json!({
                "toolCallId": "a1",
                "title": "Agent",
                "status": "in_progress",
                "rawInput": { "description": "Read the refs", "subagent_type": "general-purpose" },
                "_meta": meta("Agent", None),
            }),
        );
        call("t2", serde_json::json!({ "toolCallId": "c1", "title": "Read README.md", "_meta": meta("Read", Some("a1")) }));
        call(
            "t2",
            serde_json::json!({
                "toolCallId": "a2",
                "rawInput": { "description": "Check licenses" },
                "_meta": meta("Agent", Some("a1")),
            }),
        );
        call("t2", serde_json::json!({ "toolCallId": "c2", "_meta": meta("Bash", Some("a2")) }));
        call("t2", serde_json::json!({ "toolCallId": "a2", "status": "completed" }));

        let Json(map) = agent_map(State(state.clone())).await.unwrap();
        let node = |id: &str| map.nodes.iter().find(|n| n.id == id);
        let child_agent = node(&crate::node::agent_id(&child, Seat::Orchestrator)).unwrap();
        assert_eq!(child_agent.parent.as_deref(), Some(lead.as_str()), "started by the lead");
        assert!(node(&lead).unwrap().parent.is_none());

        let outer = node(&format!("{lead}/a1")).unwrap();
        assert_eq!((outer.kind, outer.label.as_str()), ("subagent", "Read the refs"));
        assert_eq!(outer.detail.as_deref(), Some("general-purpose"));
        assert_eq!(outer.parent.as_deref(), Some(lead.as_str()));
        assert_eq!((outer.steps, outer.status), (2, "stopped"), "its turn is over, unfinished");
        let nested = node(&format!("{lead}/a2")).unwrap();
        let outer_id = format!("{lead}/a1");
        assert_eq!(nested.parent.as_deref(), Some(outer_id.as_str()));
        assert_eq!((nested.steps, nested.status), (1, "done"));
        assert!(node(&format!("{lead}/old")).is_none(), "only the latest turn's subagents");
    }

    /// A peer hangs under the conversation that opened it, a worker under
    /// its orc, whichever comes first on the map; until its opener is on the
    /// map it stands alone.
    #[tokio::test]
    async fn a_worker_hangs_under_the_orc_that_opened_it() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let open = |name: &str| crate::routes::project::open_stream(&state, &open_req(&repo, name)).unwrap().context.stream_id;
        let (worker, orc) = (open("map-worker"), open("map-orc"));
        let opened = serde_json::json!({ "opened_by": orc }).to_string();
        crate::state::append(&state, &worker, "human", crate::baker_threads::PEER_OPENED_FAMILY, opened, vec![]).unwrap();
        let parent = || async {
            let Json(map) = agent_map(State(state.clone())).await.unwrap();
            let id = crate::node::agent_id(&worker, Seat::Orchestrator);
            map.nodes.into_iter().find(|n| n.id == id).unwrap().parent
        };
        crate::state::assign_seat_model(&state, &worker, Seat::Orchestrator, "claude-native").unwrap();
        assert_eq!(parent().await, None, "its orc is not on the map yet");
        crate::state::assign_seat_model(&state, &orc, Seat::Orchestrator, "claude-native").unwrap();
        assert_eq!(parent().await, Some(crate::node::agent_id(&orc, Seat::Orchestrator)), "under its orc");
    }

    /// "Never" is stored and reported; a read-only kind can't be set to ask.
    #[tokio::test]
    async fn tool_policy_round_trips_and_refuses_asking_about_reads() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let set = |kind: ChildToolKind, policy: &str| {
            set_tool_policy(
                State(state.clone()),
                Json(SetPolicy {
                    kind,
                    policy: policy.to_string(),
                }),
            )
        };
        let Json(after) = set(ChildToolKind::Delete, "never").await.unwrap();
        let delete = after
            .iter()
            .find(|p| p.kind == ChildToolKind::Delete)
            .unwrap();
        assert_eq!(delete.policy, "never");
        assert!(crate::pipe::tool_policy_denies(
            &state,
            ChildToolKind::Delete
        ));

        let Json(after) = set(ChildToolKind::Delete, "always").await.unwrap();
        let delete = after
            .iter()
            .find(|p| p.kind == ChildToolKind::Delete)
            .unwrap();
        assert_eq!(delete.policy, "always");
        assert!(!crate::pipe::tool_policy_denies(
            &state,
            ChildToolKind::Delete
        ));

        let err = set(ChildToolKind::Read, "ask").await.unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn measuring_counts_files_bytes_and_the_newest_write() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("a/b")).unwrap();
        std::fs::write(dir.path().join("a/one.txt"), "12345").unwrap();
        std::fs::write(dir.path().join("a/b/two.txt"), "123").unwrap();
        let m = measure(dir.path());
        assert!(m.exists && !m.truncated);
        assert_eq!((m.files, m.bytes), (2, 8));
        assert!(m.newest.is_some());
        assert!(!measure(&dir.path().join("missing")).exists);
    }

    /// Browsing goes through the real handlers: a folder lists (folders
    /// first), a file reads, a credential file is listed but never read, and
    /// a delete removes exactly the file named.
    #[tokio::test]
    async fn storage_lists_reads_and_deletes_through_its_handlers() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let cache = crate::state::cache_root(&state);
        let home = bakery_exec::seat_acp_home(&cache, "wt:browse", "worker");
        std::fs::create_dir_all(home.join("tmp/nested")).unwrap();
        std::fs::write(home.join("tmp/notes.txt"), "hello").unwrap();
        std::fs::write(home.join("tmp/auth.json"), "{}").unwrap();
        let q = |path: std::path::PathBuf| {
            Query(PathQuery {
                path: path.display().to_string(),
            })
        };

        let Json(entries) = storage_list(State(state.clone()), q(home.join("tmp")))
            .await
            .unwrap();
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["nested", "auth.json", "notes.txt"]);
        assert!(entries[1].credential && !entries[2].credential);

        let Json(file) = storage_file(State(state.clone()), q(home.join("tmp/notes.txt")))
            .await
            .unwrap();
        assert_eq!((file.text.as_str(), file.bytes), ("hello", 5));
        let err = storage_file(State(state.clone()), q(home.join("tmp/auth.json")))
            .await
            .unwrap_err();
        assert_eq!(err.0, StatusCode::FORBIDDEN);

        let Json(deleted) = storage_delete(
            State(state.clone()),
            Json(PathQuery {
                path: home.join("tmp/notes.txt").display().to_string(),
            }),
        )
        .await
        .unwrap();
        assert!(deleted["deleted"].as_str().unwrap().ends_with("notes.txt"));
        assert!(!home.join("tmp/notes.txt").exists());
        assert!(home.join("tmp/auth.json").exists());
    }

    async fn archived_in_studio(state: &AppState, stream_id: &str) -> bool {
        let Json(view) = crate::routes::studio::studio_state(State(state.clone()))
            .await
            .unwrap();
        view.projects
            .iter()
            .flat_map(|p| p.sockets.iter())
            .chain(&view.ungrouped_sockets)
            .chain(&view.factory_sockets)
            .find(|s| s.stream_id == stream_id)
            .expect("listed")
            .archived
    }

    async fn archive(state: &AppState, stream_id: &str, archived: bool) -> Result<(), HttpError> {
        archive_conversation(
            State(state.clone()),
            Json(ArchiveReq {
                stream_id: stream_id.to_string(),
                archived,
            }),
        )
        .await
        .map(|_| ())
    }

    /// One archive action for every conversation: a project chat and a Baker
    /// thread both hide and come back; the main Baker never hides, and
    /// nothing is archived out from under a running turn.
    #[tokio::test]
    async fn every_conversation_archives_the_same_way_except_the_main_baker() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let project = crate::routes::project::open_stream(&state, &open_req(&repo, "archive"))
            .unwrap()
            .context
            .stream_id;
        let baker = crate::routes::baker::ensure_permanent_baker(&state, None, None)
            .unwrap()
            .stream_id;
        let thread = crate::baker_threads::create_thread(
            &state,
            &crate::baker_threads::CreateThreadReq {
                title: Some("side".into()),
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

        for sid in [&project, &thread] {
            assert!(!archived_in_studio(&state, sid).await);
            archive(&state, sid, true).await.unwrap();
            assert!(archived_in_studio(&state, sid).await, "{sid} archived");
            archive(&state, sid, false).await.unwrap();
            assert!(!archived_in_studio(&state, sid).await, "{sid} restored");
        }

        let err = archive(&state, &baker, true).await.unwrap_err();
        assert_eq!(err.0, StatusCode::CONFLICT);

        state
            .lifecycle
            .lock()
            .unwrap()
            .running_turns
            .insert((project.clone(), Seat::Orchestrator), 1);
        let err = archive(&state, &project, true).await.unwrap_err();
        assert_eq!(err.0, StatusCode::CONFLICT);
        assert!(!archived_in_studio(&state, &project).await);
    }

    /// Each status comes from what actually happened last: a running turn, a
    /// wait on the human, or the durable outcome of the last turn.
    #[test]
    fn a_conversations_status_says_what_it_is_doing_and_whether_it_needs_you() {
        use crate::state::ApprovalKind;
        use ConversationStatus::*;
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let turn = |sid: &str, family: &str, payload: serde_json::Value| {
            crate::state::append(&state, sid, "node", family, payload.to_string(), vec![]).unwrap();
        };
        let status = |sid: &str, busy: bool, gates: usize, awaiting: Option<ApprovalKind>| {
            let heart = state.heart.lock().unwrap();
            conversation_status_locked(&heart, sid, busy, gates, awaiting)
        };

        assert_eq!(status("wt:new", false, 0, None), (Idle, None));
        assert_eq!(status("wt:new", true, 0, None), (Working, None));
        assert_eq!(
            status("wt:new", true, 0, Some(ApprovalKind::Question)),
            (NeedsInput, Some("asked you a question".into()))
        );
        assert_eq!(
            status("wt:new", false, 2, None),
            (NeedsInput, Some("2 changes to review".into()))
        );

        turn(
            "wt:done",
            "agent.turn.started",
            serde_json::json!({"instruction": "fix it"}),
        );
        turn(
            "wt:done",
            "agent.turn.done",
            serde_json::json!({"summary": "Fixed the parser.\n\nAll tests pass."}),
        );
        assert_eq!(
            status("wt:done", false, 0, None),
            (Done, Some("Fixed the parser.".into()))
        );

        turn(
            "wt:asks",
            "agent.turn.done",
            serde_json::json!({"summary": "Two options.\n\n**Which one should I use?**"}),
        );
        assert_eq!(
            status("wt:asks", false, 0, None),
            (NeedsInput, Some("Which one should I use?".into()))
        );

        turn(
            "wt:fail",
            "agent.turn.failed",
            serde_json::json!({"reason": "model refused"}),
        );
        assert_eq!(
            status("wt:fail", false, 0, None),
            (Failed, Some("model refused".into()))
        );

        turn(
            "wt:done",
            "agent.turn.started",
            serde_json::json!({"instruction": "again"}),
        );
        let (s, note) = status("wt:done", false, 0, None);
        assert_eq!(s, Idle);
        assert!(
            note.unwrap().starts_with("interrupted"),
            "a turn the node lost is not done"
        );

        turn("wt:stop", "agent.turn.cancelled", serde_json::json!({}));
        assert_eq!(
            status("wt:stop", false, 0, None),
            (Idle, Some("stopped".into()))
        );
    }

    /// Restart stops only an idle runtime, and only that conversation's.
    #[test]
    fn restart_stops_an_idle_runtime_and_never_a_running_turn() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let slot = || crate::pipe::AcpSlot::Closing { generation: 1 };
        {
            let mut sessions = state.acp_sessions.lock().unwrap();
            sessions.insert(("wt:a".into(), Seat::Orchestrator), slot());
            sessions.insert(("wt:b".into(), Seat::Orchestrator), slot());
        }

        assert_eq!(restart_runtime_inner(&state, "wt:none"), Ok(false));
        assert_eq!(restart_runtime_inner(&state, "wt:a"), Ok(true));
        let keys: Vec<String> = state
            .acp_sessions
            .lock()
            .unwrap()
            .keys()
            .map(|(sid, _)| sid.clone())
            .collect();
        assert_eq!(keys, ["wt:b"]);

        state
            .lifecycle
            .lock()
            .unwrap()
            .running_turns
            .insert(("wt:b".into(), Seat::Orchestrator), 1);
        assert!(restart_runtime_inner(&state, "wt:b")
            .unwrap_err()
            .contains("stop it"));
        assert!(state
            .acp_sessions
            .lock()
            .unwrap()
            .contains_key(&("wt:b".to_string(), Seat::Orchestrator)));
    }
}
