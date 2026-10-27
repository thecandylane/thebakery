//! The Baker's tools, for every runtime: the hand-written ops (`baker_ops`)
//! and every route in the catalog (`api_catalog`), behind one gate. Native
//! runtimes reach [`call`] through the Bakery MCP server, API models through
//! the node's tool loop.
//!
//! The menu, what a model is shown up front, is the ops, the routes marked
//! `menu`, and two tools that reach every other visible route without
//! growing the prompt: `api_find` and `api_call`. A menu is curation, never
//! permission: [`call`] checks reach, then grades and gates every call the
//! same way, whichever name it came in by.
//!
//! An ask is the existing permission card, kept apart from the per-kind
//! allow lists: it allows one exact call. It carries no turn, so the
//! operator can still decide after the turn that asked moved on, and the
//! decision stands for ten minutes: an MCP call gives up waiting after
//! [`MCP_APPROVAL_WAIT`], and its retry then runs or refuses at once.

use crate::agent_control::OperatingMode;
use crate::api_catalog::{self, Method, Risk};
use crate::node::Seat;
use crate::pipe::{ChildToolGateDecisionKind, CHILD_TOOL_DECIDED_FAMILY, CHILD_TOOL_PENDING_FAMILY};
use crate::state::AppState;
use axum::body::Body;
use axum::http::{header::CONTENT_TYPE, Request};
use serde_json::{json, Map, Value};
use std::future::Future;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tower::ServiceExt;

pub(crate) const FIND: &str = "api_find";
pub(crate) const CALL: &str = "api_call";
/// How long a Bakery MCP call waits on the operator before it answers "call
/// again": each runtime abandons a silent MCP call after its own timeout,
/// and we know none of them for certain.
pub(crate) const MCP_APPROVAL_WAIT: Duration = Duration::from_secs(55);
pub(crate) const WAITING: &str =
    "waiting for the operator's approval — call the same tool again after they decide";
/// A decision stands this long for the same tool and arguments.
const DECISION_MEMORY: chrono::TimeDelta = chrono::TimeDelta::minutes(10);
/// Marks an allowed call as run, so one approval runs it once.
pub(crate) const APPROVAL_USED_FAMILY: &str = "agent.child_tool.used";
/// The most of a route's response a model is handed.
const MAX_RESULT_BYTES: usize = 64 * 1024;

/// Who is calling, under which mode, and how long it may wait on the
/// operator.
pub(crate) struct Caller {
    pub(crate) stream_id: String,
    pub(crate) seat: Seat,
    pub(crate) turn_id: String,
    pub(crate) mode: OperatingMode,
    /// `None` waits until the operator decides or the turn is stopped.
    pub(crate) wait: Option<Duration>,
}

impl Caller {
    /// A native runtime's MCP call, during whichever turn is current.
    pub(crate) fn mcp(state: &AppState, stream_id: &str, seat: Seat) -> Self {
        let agent_id = crate::node::agent_id(stream_id, seat);
        let activity = crate::state::agent_activity_snapshot(state, &agent_id);
        Caller {
            stream_id: stream_id.to_string(),
            seat,
            turn_id: activity.as_ref().map(|a| a.turn_id.clone()).unwrap_or_default(),
            // The running turn's mode (a read-only Ask runs in Plan), else
            // the conversation's.
            mode: activity
                .and_then(|a| a.mode)
                .unwrap_or_else(|| crate::agent_control::operating_mode_for(state, stream_id, &agent_id)),
            wait: Some(MCP_APPROVAL_WAIT),
        }
    }

    fn agent_id(&self) -> String {
        crate::node::agent_id(&self.stream_id, self.seat)
    }

    /// Stop was pressed, or the turn that is waiting is gone.
    fn stopped(&self, state: &AppState) -> bool {
        !self.turn_id.is_empty()
            && crate::state::agent_activity_snapshot(state, &self.agent_id())
                .is_none_or(|a| a.turn_id != self.turn_id || a.cancel_requested)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    Allow,
    Ask,
    Deny,
}

/// The one gate for every Baker tool on every runtime: the grade decides
/// whether to ask first, the mode how much runs unattended. Plan changes
/// nothing unasked; destructive calls always ask, and so does a tool marked
/// `always_ask` in api_overrides.json, in every mode, Plan included.
pub(crate) fn gate(mode: OperatingMode, risk: Risk, always_ask: bool) -> Verdict {
    match (risk, mode) {
        _ if always_ask => Verdict::Ask,
        (Risk::Read, _) => Verdict::Allow,
        (_, OperatingMode::Plan) => Verdict::Deny,
        (Risk::Destructive, _) => Verdict::Ask,
        (_, OperatingMode::Auto) => Verdict::Allow,
        (_, OperatingMode::Manual | OperatingMode::AutoEdit) => Verdict::Ask,
    }
}

/// Old op names that now reach a route under its own name.
const ALIASES: &[(&str, &str)] = &[("project_root_attach", "socket_open")];

fn canonical(name: &str) -> &str {
    ALIASES.iter().find(|(old, _)| *old == name).map_or(name, |(_, new)| new)
}

/// The grade of a Baker tool by name; a hand-written op shadows a route of
/// the same name.
pub(crate) fn risk_of(name: &str) -> Option<Risk> {
    let name = canonical(name);
    crate::baker_ops::find_op(name)
        .map(|op| op.risk)
        .or_else(|| api_catalog::find(name).map(|t| t.risk))
}

/// Whether a Baker tool asks in every mode: a hand-written op marked so, or
/// one that shares its name with a route marked so.
fn always_asks(name: &str) -> bool {
    let name = canonical(name);
    crate::baker_ops::find_op(name).is_some_and(|op| op.always_ask)
        || api_catalog::find(name).is_some_and(|t| t.always_ask)
}

pub(crate) fn is_tool(name: &str) -> bool {
    name == FIND || name == CALL || risk_of(name).is_some()
}

fn decl(name: &str, description: String, parameters: Value) -> bakery_agent::ToolDecl {
    bakery_agent::ToolDecl {
        name: name.to_string(),
        description,
        parameters,
    }
}

fn menu_decl(t: &api_catalog::Tool) -> bakery_agent::ToolDecl {
    decl(&t.name, format!("{} [{}]", t.summary, t.risk.as_str()), t.input.clone())
}

/// A route's input as an agent other than the Baker sees it: the
/// conversation or project its reach fills in (`within_reach`) is the
/// caller's own when left out, so the agent never has to look it up.
fn agent_input(t: &api_catalog::Tool) -> Value {
    let mut input = t.input.clone();
    if !matches!(t.agents, Some(api_catalog::Reach::Own | api_catalog::Reach::Project)) {
        return input;
    }
    let props = &input["properties"];
    let (key, own) = match ["project_id", "parent_project_id"].into_iter().find(|k| props.get(*k).is_some()) {
        Some(key) => (key, "project"),
        None if t.path == "/stream/{id}" => ("id", "conversation"),
        None if props.get("stream_id").is_some() => ("stream_id", "conversation"),
        None => return input,
    };
    if let Some(required) = input.get_mut("required").and_then(Value::as_array_mut) {
        required.retain(|k| k != key);
    }
    if let Some(field) = input["properties"].get_mut(key).and_then(Value::as_object_mut) {
        let about = field.get("description").and_then(Value::as_str).map(|d| format!("{d} ")).unwrap_or_default();
        field.insert("description".into(), json!(format!("{about}Your own {own} when left out.")));
    }
    input
}

fn agent_menu_decl(t: &api_catalog::Tool) -> bakery_agent::ToolDecl {
    decl(&t.name, format!("{} [{}]", t.summary, t.risk.as_str()), agent_input(t))
}

fn finder_decls() -> [bakery_agent::ToolDecl; 2] {
    [
        decl(
            FIND,
            "Search every other Bakery tool (each node route) by words; returns names, risk grades and input schemas for api_call.".into(),
            json!({"type": "object", "properties": {"query": {"type": "string", "description": "words to match, e.g. \"task review\""}}, "required": ["query"]}),
        ),
        decl(
            CALL,
            "Run one Bakery tool found with api_find. Its risk grade decides whether the operator is asked first, unless it is marked always_ask: then they are always asked.".into(),
            json!({"type": "object", "properties": {
                "tool": {"type": "string", "description": "the tool's name from api_find"},
                "args": {"type": "object", "description": "the tool's input, as its schema says"},
            }, "required": ["tool"]}),
        ),
    ]
}

/// What the model is shown up front: the menu, then `api_find`/`api_call`.
pub(crate) fn decls() -> Vec<bakery_agent::ToolDecl> {
    let mut decls = crate::baker_ops::baker_op_tool_decls();
    decls.extend(
        api_catalog::tools()
            .iter()
            .filter(|t| t.menu && crate::baker_ops::find_op(&t.name).is_none())
            .map(menu_decl),
    );
    decls.extend(finder_decls());
    decls
}

/// What an agent other than the Baker is shown: the menu tools it may use
/// within its limit, then `api_find`/`api_call` over the rest it may use.
/// Never the Baker's ops: a route an op shares a name with is the route for
/// an agent. Nothing, for an agent given none of the Bakery's tools.
pub(crate) fn agent_decls(limit: ToolsLimit) -> Vec<bakery_agent::ToolDecl> {
    if limit == ToolsLimit::None {
        return Vec::new();
    }
    let mut decls: Vec<_> = api_catalog::tools()
        .iter()
        .filter(|t| t.menu && agent_may_use(t, limit))
        .map(agent_menu_decl)
        .collect();
    decls.extend(finder_decls());
    decls
}

/// `limit` is set for an agent other than the Baker: it finds only the
/// routes it may use, never an op.
fn find(query: &str, limit: Option<ToolsLimit>) -> String {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    let ops = crate::baker_ops::baker_op_tool_decls()
        .into_iter()
        .filter(|_| limit.is_none())
        .map(|d| {
            let risk = risk_of(&d.name).unwrap_or(Risk::Write);
            (d.name, d.description, risk, d.parameters)
        });
    let routes = api_catalog::tools()
        .iter()
        .filter(|t| match limit {
            None => crate::baker_ops::find_op(&t.name).is_none(),
            Some(l) => agent_may_use(t, l),
        })
        .map(|t| {
            let input = if limit.is_some() { agent_input(t) } else { t.input.clone() };
            (t.name.clone(), format!("{} ({} {})", t.summary, t.method.as_str(), t.path), t.risk, input)
        });
    let hits: Vec<Value> = ops
        .chain(routes)
        .filter(|(name, about, _, _)| {
            let text = format!("{name} {about}").to_lowercase().replace('_', " ");
            words.iter().all(|w| text.contains(&w.replace('_', " ")))
        })
        .take(20)
        .map(|(name, about, risk, input)| {
            let mut hit = json!({"name": name, "risk": risk, "about": about, "input": input});
            if always_asks(&name) {
                hit["always_ask"] = json!(true);
            }
            hit
        })
        .collect();
    json!({"tools": hits}).to_string()
}

/// Runs one Baker tool call: reach, then the gate, then the same handler
/// the operator's UI calls.
pub(crate) fn call(
    state: &AppState,
    caller: &Caller,
    name: &str,
    args: &Value,
) -> Result<String, String> {
    // Every agent but the Baker reaches its own project only, or its own
    // conversation without one (`within_reach`).
    let agent = !crate::baker_ops::is_baker_scope_stream(state, &caller.stream_id);
    if name == FIND {
        let limit = agent.then(|| tools_limit(state, &caller.stream_id));
        return Ok(find(args.get("query").and_then(Value::as_str).unwrap_or_default(), limit));
    }
    let (name, mut args) = if name == CALL {
        let target = args
            .get("tool")
            .and_then(Value::as_str)
            .ok_or("api_call needs \"tool\": a name from api_find")?;
        (target, args.get("args").cloned().unwrap_or_else(|| json!({})))
    } else {
        (name, args.clone())
    };
    if name == FIND || name == CALL {
        return Err(format!("{name} cannot be called through api_call"));
    }
    let name = canonical(name);
    // Who acted is the caller's scope, never an argument.
    if let Some(obj) = args.as_object_mut() {
        obj.remove("actor");
    }
    // An agent's tool is always a route, even one an op of the Baker's shares
    // its name with; the Baker's may be an op.
    let route = if agent { api_catalog::find(name).filter(|t| t.agents.is_some()) } else { None };
    let risk = match route {
        Some(tool) => tool.risk,
        None => risk_of(name).ok_or_else(|| format!("unknown Bakery tool {name:?}: api_find lists them"))?,
    };
    let always_ask = always_asks(name);
    // A learning scratch thread asks no one and reaches nothing outward.
    if risk != Risk::Read
        && crate::learning::scratch(state, &caller.stream_id)
        && (!crate::learning::SCRATCH_TOOLS.contains(&name) || matches!(gate(caller.mode, risk, always_ask), Verdict::Ask))
    {
        return Err(crate::learning::refuse(state, &caller.stream_id, name));
    }
    // A task worker's container tools keep their own guard (`agent_may`).
    let reach = if agent && !crate::containers::is_worker_tool(name) {
        within_reach(state, caller, name, &mut args)?
    } else {
        None
    };
    // A Baker thread opened with a tools limit keeps to it too.
    if !agent && !limit_allows(tools_limit(state, &caller.stream_id), risk) {
        return Err(format!("{name}: this conversation may not use it (its tools limit)"));
    }
    check_reach(state, caller, risk, &args)?;
    crate::containers::agent_may(state, &caller.stream_id, name, &args)?;
    crate::desktop::own_previews_only(&caller.stream_id, name, &mut args)?;
    // A push, or a Google change, the operator granted this conversation
    // goes ahead without a card (grants.rs); the node alone decides what a
    // grant covers.
    let mut granted = None;
    match gate(caller.mode, risk, always_ask) {
        Verdict::Allow => {}
        Verdict::Deny => {
            return Err(format!(
                "{name} ({}) is blocked in Plan mode. Explain what you would do; change nothing.",
                risk.as_str()
            ))
        }
        Verdict::Ask => {
            granted = crate::grants::covering(state, &caller.stream_id, name, &args);
            match granted {
                Some(_) => settle_card_under_grant(state, caller, name, &args),
                // An approved push sends the commit its card showed.
                None => {
                    if let Some(head) = ask(state, caller, name, risk, &args)? {
                        args["commit"] = json!(head);
                    }
                }
            }
        }
    }
    if !agent && crate::baker_ops::is_op(name) {
        let actor = crate::actor::agent(&caller.stream_id, caller.seat);
        return crate::actor::with(actor, || {
            crate::baker_ops::dispatch_baker_op_as(state, &caller.stream_id, name, &args)
        });
    }
    let tool = route
        .or_else(|| api_catalog::find(name))
        .ok_or_else(|| format!("unknown Bakery tool {name:?}"))?;
    let body = call_route(state, crate::actor::agent(&caller.stream_id, caller.seat), tool, args)?;
    if let Some(grant) = &granted {
        crate::grants::used(state, &caller.stream_id, grant);
    }
    // A screenshot's pixels reach the model beside the result, never in its
    // text, which Heart keeps.
    if name == "desktop_screenshot" {
        return crate::desktop::keep_screenshot(&caller.agent_id(), &caller.turn_id, &body);
    }
    if reach == Some(api_catalog::Reach::Listed) {
        return Ok(project_entries(state, caller, &body));
    }
    if agent && name == "task_create" {
        give_creators_mode(state, caller, &body)?;
    }
    Ok(body)
}

/// An agent's new task runs in its creator's mode, never a freer one (the
/// operator can change it later). Its tools need no cap: only an agent with
/// all of them may create a task.
fn give_creators_mode(state: &AppState, creator: &Caller, body: &str) -> Result<(), String> {
    let reply: Value = serde_json::from_str(body).unwrap_or_default();
    match reply["task"]["stream_id"].as_str() {
        Some(task) if reply["created"] == true => crate::actor::with(crate::actor::agent(&creator.stream_id, creator.seat), || {
            crate::agent_control::set_operating_mode(state, task, Seat::Orchestrator, creator.mode).map(drop)
        }),
        _ => Ok(()),
    }
}

// ─── Agents other than the Baker ────────────────────────────────────────────

/// What an agent may use of the Bakery's tools: chosen when it is created,
/// changed only by the operator.
#[derive(schemars::JsonSchema, Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ToolsLimit {
    /// Every tool it can reach.
    #[default]
    All,
    /// Only those graded read.
    Read,
    /// None of the Bakery's tools.
    None,
}

/// A conversation's tools limit, each time it is set.
pub(crate) const TOOLS_FAMILY: &str = "agent.tools.set";

pub(crate) fn tools_limit(state: &AppState, stream_id: &str) -> ToolsLimit {
    let last = state.heart.lock().unwrap().last_of_family(stream_id, TOOLS_FAMILY).ok().flatten();
    last.and_then(|e| serde_json::from_str::<Value>(&e.payload).ok())
        .and_then(|v| serde_json::from_value(v["tools"].clone()).ok())
        .unwrap_or_default()
}

pub(crate) fn set_tools_limit(state: &AppState, stream_id: &str, limit: ToolsLimit) -> Result<(), String> {
    let actor = crate::actor::attribute("human");
    crate::state::append(state, stream_id, &actor, TOOLS_FAMILY, json!({"tools": limit}).to_string(), vec![]).map(drop)
}

/// Records a limit chosen at creation; All, the default, needs no record.
pub(crate) fn set_tools_at_creation(state: &AppState, stream_id: &str, limit: Option<ToolsLimit>) -> Result<(), String> {
    match limit {
        Some(limit) if limit != ToolsLimit::All => set_tools_limit(state, stream_id, limit),
        _ => Ok(()),
    }
}

fn limit_allows(limit: ToolsLimit, risk: Risk) -> bool {
    match limit {
        ToolsLimit::All => true,
        ToolsLimit::Read => risk == Risk::Read,
        ToolsLimit::None => false,
    }
}

fn agent_may_use(tool: &api_catalog::Tool, limit: ToolsLimit) -> bool {
    tool.agents.is_some() && limit_allows(limit, tool.risk)
}

/// The Baker's tools ([`decls`]) for a Baker conversation under `limit`.
pub(crate) fn baker_decls(limit: ToolsLimit) -> Vec<bakery_agent::ToolDecl> {
    decls()
        .into_iter()
        .filter(|d| {
            limit != ToolsLimit::None
                && (d.name == FIND || d.name == CALL || risk_of(&d.name).is_some_and(|r| limit_allows(limit, r)))
        })
        .collect()
}

/// Whether an agent other than the Baker is offered `name` at all; each
/// call is still checked (`within_reach`).
pub(crate) fn is_agent_tool(name: &str) -> bool {
    name == FIND || name == CALL || api_catalog::find(name).is_some_and(|t| t.agents.is_some())
}

fn project_of(state: &AppState, stream_id: &str) -> Option<String> {
    let heart = state.heart.lock().unwrap();
    crate::state::socket_project(&heart, stream_id).0
}

/// One call by an agent other than the Baker: the tool must be one agents
/// may use, within its limit, and what it acts on must be its own
/// conversation (`Own`), or in its own project (`Project`) — its own
/// conversation alone when it has no project. A call that names none acts
/// on its own conversation or project. A peer's settings are the operator's.
fn within_reach(
    state: &AppState,
    caller: &Caller,
    name: &str,
    args: &mut Value,
) -> Result<Option<api_catalog::Reach>, String> {
    use api_catalog::Reach;
    let refused = |why: &str| Err(format!("{name}: {why}"));
    let limit = tools_limit(state, &caller.stream_id);
    if limit == ToolsLimit::None {
        return refused("this agent was given none of the Bakery's tools");
    }
    let Some(tool) = api_catalog::find(name) else {
        return refused("the Baker's only");
    };
    let Some(reach) = tool.agents else {
        return refused("the Baker's only: it acts beyond your project, or on the whole Bakery");
    };
    if !agent_may_use(tool, limit) {
        return refused("this agent may only read with the Bakery's tools");
    }
    if matches!(reach, Reach::Any | Reach::Listed) {
        return Ok(Some(reach));
    }
    if args.is_null() {
        *args = json!({});
    }
    let Some(fields) = args.as_object_mut() else {
        return refused("takes an object");
    };
    let own = caller.stream_id.as_str();
    let mine = project_of(state, own);
    let named = |fields: &Map<String, Value>, key: &str| {
        fields.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
    };
    let props = &tool.input["properties"];
    if let Some(key) = ["project_id", "parent_project_id"].into_iter().find(|k| props.get(*k).is_some()) {
        let Some(mine) = mine else {
            return refused("this conversation is in no project");
        };
        return match named(fields, key) {
            None => {
                fields.insert(key.into(), json!(mine));
                Ok(Some(reach))
            }
            Some(p) if p == mine => Ok(Some(reach)),
            Some(_) => refused("another project is the Baker's to reach"),
        };
    }
    // The conversation it acts on.
    let target = if tool.path == "/stream/{id}" || props.get("stream_id").is_some() {
        let key = if tool.path == "/stream/{id}" { "id" } else { "stream_id" };
        match named(fields, key) {
            Some(target) => target,
            None => {
                fields.insert(key.into(), json!(own));
                own.to_string()
            }
        }
    } else if tool.path.starts_with("/jobs/") && props.get("id").is_some() {
        let job = named(fields, "id").unwrap_or_default();
        match crate::jobs::list_jobs(None).into_iter().find(|j| j.id == job) {
            Some(j) => j.stream_id,
            None => return refused("no such job"),
        }
    } else {
        return refused("it names no conversation of yours");
    };
    if target == own {
        if tool.path == "/agent/turn/cancel" {
            return refused("it stops a peer's turn, never your own turn: end it instead");
        }
        return Ok(Some(reach));
    }
    if reach == Reach::Own {
        return refused("it acts on your own conversation only");
    }
    // A workspace's orc reads its own workers: its workspace, nothing wider.
    let its_worker = crate::workspaces::is_orc_of(state, own, &target);
    if its_worker && tool.risk != api_catalog::Risk::Read {
        return refused("an orc only reads its workers' conversations: send_message them to ask for more");
    }
    if !its_worker && (mine.is_none() || project_of(state, &target) != mine) {
        return refused(match (&mine, crate::workspaces::workspace_of(state, own)) {
            (Some(_), _) => "that conversation is outside your project",
            (None, Some(_)) => "that conversation is out of your reach: send_message it within your workspace",
            (None, None) => "a chat with no project reaches only its own conversation",
        });
    }
    if let Some(setting) = ["model_id", "effort", "mode"].iter().find(|k| fields.get(**k).is_some_and(|v| !v.is_null())) {
        return refused(&format!("a peer's {setting} is the operator's to change"));
    }
    Ok(Some(reach))
}

/// A Bakery-wide list as an agent sees it: its own project's entries, or
/// its own conversation alone when it has no project; a workspace's orc
/// sees its workers too.
fn project_entries(state: &AppState, caller: &Caller, body: &str) -> String {
    let Ok(Value::Array(items)) = serde_json::from_str::<Value>(body) else {
        return "[]".to_string();
    };
    let mine = project_of(state, &caller.stream_id);
    let keep = |item: &Value| {
        let stream = item["stream_id"].as_str().unwrap_or_default();
        stream == caller.stream_id
            || (mine.is_some() && project_of(state, stream) == mine)
            || crate::workspaces::is_orc_of(state, &caller.stream_id, stream)
    };
    Value::Array(items.into_iter().filter(keep).collect()).to_string()
}

#[derive(schemars::JsonSchema, serde::Deserialize)]
pub(crate) struct ToolsReq {
    pub(crate) stream_id: String,
    pub(crate) tools: ToolsLimit,
}

#[derive(schemars::JsonSchema, serde::Deserialize)]
pub(crate) struct ToolsQuery {
    pub(crate) stream_id: String,
}

#[derive(schemars::JsonSchema, serde::Serialize)]
pub(crate) struct Tools {
    pub(crate) stream_id: String,
    pub(crate) tools: ToolsLimit,
}

/// `GET /agent/tools`: what a conversation's agents may use of the
/// Bakery's tools.
pub(crate) async fn tools_route(
    axum::extract::State(st): axum::extract::State<AppState>,
    axum::extract::Query(q): axum::extract::Query<ToolsQuery>,
) -> axum::Json<Tools> {
    let tools = tools_limit(&st, &q.stream_id);
    axum::Json(Tools { stream_id: q.stream_id, tools })
}

/// `POST /agent/tools`: the operator changes it. Hidden from every agent.
pub(crate) async fn set_tools_route(
    axum::extract::State(st): axum::extract::State<AppState>,
    axum::Json(req): axum::Json<ToolsReq>,
) -> Result<axum::Json<Tools>, (axum::http::StatusCode, String)> {
    let (state, stream_id, tools) = (st.clone(), req.stream_id.clone(), req.tools);
    crate::actor::spawn_blocking(move || set_tools_limit(&state, &stream_id, tools))
        .await
        .map_err(|e| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map_err(|e| (axum::http::StatusCode::BAD_REQUEST, e))?;
    Ok(axum::Json(Tools { stream_id: req.stream_id, tools: req.tools }))
}

// ─── Reach ──────────────────────────────────────────────────────────────────

/// Refuses a call whose arguments name a path its caller cannot reach:
/// `machine_access::scope_allows` (the deny list at every level, then the
/// caller's place on the scope dial) for the access the call's grade needs.
/// A relative path is the named stream's (`stream_id`), and is checked as
/// that conversation's own workspace. Route handlers run in the unsandboxed
/// node, so this is the boundary for route tools; a command's own reach is
/// its sandbox's.
fn check_reach(state: &AppState, caller: &Caller, risk: Risk, args: &Value) -> Result<(), String> {
    use crate::machine_access::Access;
    let access = match risk {
        Risk::Read => Access::Read,
        Risk::Run => Access::Exec,
        Risk::Write | Risk::Destructive => Access::Write,
    };
    let target = args.get("stream_id").and_then(Value::as_str);
    let mut paths = Vec::new();
    collect_paths(args, None, &mut paths);
    for raw in paths {
        let path = match raw.strip_prefix('~') {
            Some(rest) => std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default()
                .join(rest.trim_start_matches('/')),
            None => PathBuf::from(&raw),
        };
        let stream = match target {
            Some(target) if path.is_relative() => target,
            _ => caller.stream_id.as_str(),
        };
        crate::machine_access::scope_allows(state, stream, &path, access)
            .map_err(|why| format!("{raw} is out of reach: {why}"))?;
        // CLAUDE.md's hard deny, for a Baker on any model.
        let text = path.to_string_lossy();
        if text.contains("crates/bakery-agent/src/private")
            && path.file_name().is_none_or(|n| n != "README.md")
        {
            return Err(format!("{raw} is out of reach: private provider code"));
        }
    }
    Ok(())
}

/// The strings in fields that name a path: `path`, `paths`, `root`, `cwd`, …
/// and any `*_path` or `*_root`.
fn collect_paths(value: &Value, key: Option<&str>, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => map.iter().for_each(|(k, v)| collect_paths(v, Some(k), out)),
        Value::Array(items) => items.iter().for_each(|v| collect_paths(v, key, out)),
        Value::String(s) => {
            let path_key = key.is_some_and(|k| {
                matches!(k, "path" | "paths" | "root" | "dir" | "parent" | "repo" | "file" | "cwd")
                    || k.ends_with("_path")
                    || k.ends_with("_root")
            });
            if path_key && !s.trim().is_empty() {
                out.push(s.clone());
            }
        }
        _ => {}
    }
}

// ─── Asking the operator ────────────────────────────────────────────────────

struct Card {
    hash: String,
    decision: Option<ChildToolGateDecisionKind>,
    used: bool,
    /// A push card's commit (`push.head`).
    push_head: Option<String>,
}

fn payload(event: &bakery_core::Event) -> Value {
    serde_json::from_str(&event.payload).unwrap_or_default()
}

/// The newest card for this exact call that still counts: undecided and
/// under ten minutes old, or decided under ten minutes ago. A card closed by
/// Stop counts for nothing.
fn last_card(state: &AppState, stream_id: &str, name: &str, args: &Value) -> Option<Card> {
    let events = state.heart.lock().unwrap().read_stream(stream_id).ok()?;
    let fresh = |e: &bakery_core::Event| {
        chrono::DateTime::parse_from_rfc3339(&e.ts)
            .is_ok_and(|t| chrono::Utc::now() - t.with_timezone(&chrono::Utc) <= DECISION_MEMORY)
    };
    let pending = events.iter().rev().find(|e| {
        e.family == CHILD_TOOL_PENDING_FAMILY && {
            let p = payload(e);
            p["bakery_tool"]["name"] == name && p["bakery_tool"]["args"] == *args
        }
    })?;
    let cites = |e: &&bakery_core::Event| e.causality.iter().any(|h| *h == pending.event_hash);
    let closed = events
        .iter()
        .filter(cites)
        .find(|e| e.family == CHILD_TOOL_DECIDED_FAMILY || e.family == "agent.approval.cancelled");
    let card = |decision| Card {
        hash: pending.event_hash.clone(),
        decision,
        used: events.iter().filter(cites).any(|e| e.family == APPROVAL_USED_FAMILY),
        push_head: payload(pending)["push"]["head"].as_str().map(str::to_string),
    };
    match closed {
        Some(e) if e.family == "agent.approval.cancelled" || !fresh(e) => None,
        Some(e) => Some(card(serde_json::from_value(payload(e)["decision"].clone()).ok())),
        None => fresh(pending).then(|| card(None)),
    }
}

/// The commit a push would send now, when the call is a push.
fn push_head_now(state: &AppState, name: &str, args: &Value) -> Option<String> {
    let stream_id = args["stream_id"].as_str()?;
    (name == "git_push")
        .then(|| crate::routes::git::branch_head(state, stream_id, args["branch"].as_str()).ok())
        .flatten()
        .map(|(_, _, head)| head)
}

/// Asks the operator, and answers with the commit a push card was approved
/// for. A push card whose branch moved since asks again: an approval covers
/// exactly what its card showed.
fn ask(state: &AppState, caller: &Caller, name: &str, risk: Risk, args: &Value) -> Result<Option<String>, String> {
    let moved = |card: &Card| card.push_head.is_some() && card.push_head != push_head_now(state, name, args);
    let hash = match last_card(state, &caller.stream_id, name, args) {
        Some(card) if card.used => post_card(state, caller, name, risk, args)?,
        Some(card) if moved(&card) => {
            close_card(state, caller, card, "the branch moved since: asked again");
            post_card(state, caller, name, risk, args)?
        }
        Some(Card { hash, decision: Some(decision), push_head, .. }) => {
            return settle(state, caller, &hash, decision, name).map(|()| push_head)
        }
        Some(card) => card.hash,
        None => post_card(state, caller, name, risk, args)?,
    };
    let started = Instant::now();
    loop {
        if let Some(decision) =
            crate::pipe::child_tool_gate_decision(state, &caller.stream_id, &hash)
        {
            let push_head = last_card(state, &caller.stream_id, name, args).and_then(|card| card.push_head);
            return settle(state, caller, &hash, decision, name).map(|()| push_head);
        }
        if caller.stopped(state) {
            crate::pipe::withdraw_pending_approval(
                state,
                &caller.stream_id,
                caller.seat,
                &caller.turn_id,
                &hash,
                "child_tool",
            );
            return Err(format!("{name} was stopped before the operator decided"));
        }
        if caller.wait.is_some_and(|wait| started.elapsed() >= wait) {
            crate::state::resolve_awaiting_approval(
                state,
                &caller.agent_id(),
                &hash,
                crate::state::ActivityPhase::ContactingModel,
            );
            return Err(WAITING.to_string());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// A call that goes ahead under a grant closes its own card, if one is open:
/// an approval left unused would otherwise allow a later call once the grant
/// has ended.
fn settle_card_under_grant(state: &AppState, caller: &Caller, name: &str, args: &Value) {
    if let Some(card) = last_card(state, &caller.stream_id, name, args) {
        close_card(state, caller, card, "a grant let this call go ahead");
    }
}

/// Closes a card for good: a decided one is used up, an open one withdrawn.
fn close_card(state: &AppState, caller: &Caller, card: Card, why: &str) {
    match card {
        Card { used: true, .. } => {}
        Card { hash, decision: Some(_), .. } => {
            let used = json!({ "decided_event": hash }).to_string();
            let _ = crate::state::append(state, &caller.stream_id, "node", APPROVAL_USED_FAMILY, used, vec![hash]);
        }
        Card { hash, decision: None, .. } => {
            crate::pipe::withdraw_pending_approval_because(state, &caller.stream_id, caller.seat, &caller.turn_id, &hash, "child_tool", why);
        }
    }
}

fn settle(
    state: &AppState,
    caller: &Caller,
    hash: &str,
    decision: ChildToolGateDecisionKind,
    name: &str,
) -> Result<(), String> {
    if decision == ChildToolGateDecisionKind::Deny {
        return Err(format!("the operator denied {name}; do not retry it unless they ask"));
    }
    crate::state::append(
        state,
        &caller.stream_id,
        "node",
        APPROVAL_USED_FAMILY,
        json!({ "decided_event": hash }).to_string(),
        vec![hash.to_string()],
    )
    .map(drop)
}

fn post_card(state: &AppState, caller: &Caller, name: &str, risk: Risk, args: &Value) -> Result<String, String> {
    let shown: String = args.to_string().chars().take(400).collect();
    let mut title = format!("{name} {shown}");
    // The operator approves a UI build on what the node finds, not on what
    // the agent says it is.
    if name == "ui_bundle_install" {
        title = format!("{title}\n\n{}", crate::routes::ui_bundle::approval_note(state, args));
    }
    // A push of this conversation's own branch, or a change to a Google Cloud
    // project from its rooms: the card also offers to let it do that from
    // now on (grants.rs).
    let grant = crate::grants::offer(state, &caller.stream_id, name, args);
    // Exactly what a push sends: its commits and diff against the remote.
    let push = (name == "git_push")
        .then(|| crate::routes::git::push_preview(state, args["stream_id"].as_str()?, args["branch"].as_str()).ok())
        .flatten();
    let agent_id = caller.agent_id();
    let event = crate::state::append(
        state,
        &caller.stream_id,
        "node",
        CHILD_TOOL_PENDING_FAMILY,
        json!({
            "plug": "bakery",
            "kind": "other",
            "title": title,
            "seat": caller.seat,
            "agent_id": agent_id,
            // No turn: the decision outlives the turn that asked (module doc).
            "turn_id": "",
            "native_reviewable": false,
            "diffs": [],
            "bakery_tool": { "name": name, "risk": risk, "args": args },
            "grant": grant,
            "push": push,
        })
        .to_string(),
        vec![],
    )?;
    let live = crate::state::agent_activity_snapshot(state, &agent_id)
        .is_some_and(|a| a.turn_id == caller.turn_id);
    if live {
        crate::state::set_agent_phase(
            state,
            &agent_id,
            crate::state::ActivityPhase::AwaitingApproval {
                kind: crate::state::ApprovalKind::ChildTool,
                event_hash: event.event_hash.clone(),
            },
        );
    }
    Ok(event.event_hash)
}

// ─── Running a route ────────────────────────────────────────────────────────

fn router() -> axum::Router<AppState> {
    static ROUTER: OnceLock<axum::Router<AppState>> = OnceLock::new();
    ROUTER.get_or_init(crate::api_routes).clone()
}

/// Drives `fut` on a multi-thread runtime and waits for it on this thread,
/// which every caller here is free to block: an MCP call's blocking task or
/// a turn's own thread.
pub(crate) fn block_on<F>(fut: F) -> Result<F::Output, String>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    static SPARE: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    let handle = tokio::runtime::Handle::try_current()
        .ok()
        .filter(|h| h.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread)
        .unwrap_or_else(|| {
            SPARE
                .get_or_init(|| {
                    tokio::runtime::Builder::new_multi_thread()
                        .worker_threads(2)
                        .thread_name("bakery-api-tools")
                        .enable_all()
                        .build()
                        .expect("a runtime for Baker route calls")
                })
                .handle()
                .clone()
        });
    let (tx, rx) = std::sync::mpsc::channel();
    handle.spawn(async move {
        let _ = tx.send(fut.await);
    });
    rx.recv()
        .map_err(|_| "the route call ended without an answer".to_string())
}

fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn query(args: &Map<String, Value>) -> String {
    args.iter()
        .filter(|(_, v)| !v.is_null())
        .map(|(k, v)| {
            let v = match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            format!("{}={}", encode(k), encode(&v))
        })
        .collect::<Vec<_>>()
        .join("&")
}

/// The calling machine's identity (`{hostname}-{hex}`) never reaches a
/// model, whichever route returned it.
pub(crate) fn scrub(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for key in ["owner_node_id", "opened_by_node", "node_id"] {
                map.remove(key);
            }
            map.values_mut().for_each(scrub);
        }
        Value::Array(items) => items.iter_mut().for_each(scrub),
        _ => {}
    }
}

pub(crate) fn call_route(state: &AppState, actor: String, tool: &api_catalog::Tool, args: Value) -> Result<String, String> {
    let mut args = match args {
        Value::Object(map) => map,
        Value::Null => Map::new(),
        other => return Err(format!("{} takes an object, not {other}", tool.name)),
    };
    let mut path = tool.path.to_string();
    for part in tool.path.split('/') {
        if let Some(param) = part.strip_prefix('{').and_then(|p| p.strip_suffix('}')) {
            let value = args
                .remove(param)
                .and_then(|v| v.as_str().map(str::to_string))
                .ok_or_else(|| format!("{} needs \"{param}\"", tool.name))?;
            path = path.replace(part, &encode(&value));
        }
    }
    let request = match tool.method {
        Method::Get if args.is_empty() => Request::get(path).body(Body::empty()),
        Method::Get => Request::get(format!("{path}?{}", query(&args))).body(Body::empty()),
        Method::Post => Request::post(path)
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from(Value::Object(args).to_string())),
    }
    .map_err(|e| e.to_string())?;
    let app = router().with_state(state.clone());
    let (status, body) = block_on(crate::actor::scope(actor, async move {
        let response = match app.oneshot(request).await {
            Ok(response) => response,
            Err(never) => match never {},
        };
        let status = response.status();
        (status, axum::body::to_bytes(response.into_body(), 64 * MAX_RESULT_BYTES).await)
    }))?;
    let body = body.map_err(|_| format!("{}: the response is too large to hand back", tool.name))?;
    let mut text = match serde_json::from_slice::<Value>(&body) {
        Ok(mut json) => {
            scrub(&mut json);
            json.to_string()
        }
        Err(_) => String::from_utf8_lossy(&body).into_owned(),
    };
    if text.len() > MAX_RESULT_BYTES {
        let cut = (0..=MAX_RESULT_BYTES).rev().find(|&i| text.is_char_boundary(i)).unwrap_or(0);
        text.truncate(cut);
        text.push_str("\n… (cut at 64 KB)");
    }
    if status.is_success() {
        Ok(text)
    } else {
        Err(format!("{status}: {text}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::project::open_stream;
    use crate::testutil::*;
    use bakery_agent::StubModel;
    use std::sync::Arc;

    /// This conversation is the Baker's: its calls are the Baker's, beyond
    /// any one project.
    fn as_baker(state: &AppState, stream_id: &str) {
        state.node.lock().unwrap().baker_stream_id = Some(stream_id.to_string());
    }

    fn caller(stream_id: &str, mode: OperatingMode, wait: Option<Duration>) -> Caller {
        Caller {
            stream_id: stream_id.to_string(),
            seat: Seat::Baker,
            turn_id: String::new(),
            mode,
            wait,
        }
    }

    /// Every agent but the Baker reaches its own project, its peers there
    /// included, or its own conversation alone with no project; never the
    /// Bakery as a whole, another project, or a peer's settings.
    #[test]
    fn an_agent_reaches_its_own_project_and_a_chat_only_itself() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let (p, p1) = crate::catalog::create_project(&state, "P", None).unwrap();
        let p2 = crate::routes::project_catalog::open_root(
            &state,
            &crate::routes::project_catalog::OpenRootReq {
                project_id: p.project_id.clone(),
                name: "second".into(),
                root: crate::routes::project_catalog::CreateProjectRoot::Create {
                    parent: dir.path().to_string_lossy().into_owned(),
                    name: "second".into(),
                },
                tools: None,
            },
        )
        .unwrap();
        let (q, q1) = crate::catalog::create_project(&state, "Q", None).unwrap();
        let chat = crate::catalog::create_chat(&state, "loose").unwrap();
        let (p1, p2, q1, chat) = (p1.stream_id, p2.stream_id, q1.stream_id, chat.stream_id);
        let agent = |stream: &str| Caller { seat: Seat::Orchestrator, ..caller(stream, OperatingMode::Auto, None) };
        let run = |stream: &str, tool: &str, args: Value| call(&state, &agent(stream), tool, &args);

        let peer = run(&p1, "agent_activity", json!({"stream_id": p2, "seat": "orchestrator"}));
        assert!(peer.is_ok(), "a peer in its project: {peer:?}");
        assert!(run(&p1, "agent_activity", json!({"seat": "orchestrator"})).is_ok(), "itself, by default");
        let err = run(&p1, "agent_activity", json!({"stream_id": q1, "seat": "orchestrator"})).unwrap_err();
        assert!(err.contains("outside your project"), "{err}");
        let err = run(&chat, "agent_activity", json!({"stream_id": p1, "seat": "orchestrator"})).unwrap_err();
        assert!(err.contains("a chat with no project reaches only its own conversation"), "{err}");
        assert!(run(&chat, "agent_activity", json!({"stream_id": chat, "seat": "orchestrator"})).is_ok());

        let err = run(&p1, "git_commit", json!({"stream_id": p2, "message": "x"})).unwrap_err();
        assert!(err.contains("your own conversation only"), "writes stay in its own checkout: {err}");
        let err = run(&p1, "project_create", json!({"name": "R"})).unwrap_err();
        assert!(err.contains("the Baker's only"), "{err}");
        let err = run(&p1, "agent_turn_queue", json!({"stream_id": p2, "instruction": "hi", "model_id": "grok"})).unwrap_err();
        assert!(err.contains("a peer's model_id is the operator's"), "{err}");
        // It may stop a peer's running turn: the route answers, not the reach.
        let stop = run(&p1, "turn_stop", json!({"stream_id": p2, "seat": "orchestrator"}));
        assert!(stop.as_ref().map_or_else(|e| !e.starts_with("turn_stop:"), |_| true), "{stop:?}");
        // It reads and waits on a peer in its project, never one outside it.
        let read: Value = serde_json::from_str(&run(&p1, "thread_read", json!({"stream_id": p2})).unwrap()).unwrap();
        assert_eq!((read["title"].as_str(), read["state"].as_str()), (Some("second"), Some("idle")), "{read}");
        let waited = run(&p1, "thread_wait", json!({"stream_id": p2, "timeout_secs": 1})).unwrap();
        assert!(waited.contains(r#""done":true"#), "{waited}");
        let err = run(&p1, "thread_read", json!({"stream_id": q1})).unwrap_err();
        assert!(err.contains("outside your project"), "{err}");
        // Its terminals and its memory are its own.
        let err = run(&p1, "terminal_list", json!({"stream_id": p2})).unwrap_err();
        assert!(err.contains("your own conversation only"), "{err}");
        assert!(run(&p1, "terminal_list", json!({})).is_ok());
        let err = run(&p1, "memory_write", json!({"stream_id": p2, "text": "x"})).unwrap_err();
        assert!(err.contains("your own conversation only"), "{err}");

        // Its project fills in, and another project is out of reach.
        assert!(run(&p1, "project_rollup", json!({})).is_ok());
        let err = run(&p1, "project_rollup", json!({"project_id": q.project_id})).unwrap_err();
        assert!(err.contains("another project"), "{err}");
        // A Bakery-wide list shows its own project only.
        let listed: Vec<Value> = serde_json::from_str(&run(&p1, "socket_list", json!({})).unwrap()).unwrap();
        let ids: Vec<&str> = listed.iter().filter_map(|s| s["stream_id"].as_str()).collect();
        assert!(ids.contains(&p1.as_str()) && ids.contains(&p2.as_str()), "{ids:?}");
        assert!(!ids.contains(&q1.as_str()) && !ids.contains(&chat.as_str()), "{ids:?}");
        // It finds only what it may use.
        let found = call(&state, &agent(&p1), FIND, &json!({"query": "project"})).unwrap();
        assert!(found.contains("project_rollup") && !found.contains("project_create"), "{found}");
        assert!(is_agent_tool("git_status") && !is_agent_tool("project_create"));

        // Its limit, chosen at creation or by the operator later.
        set_tools_limit(&state, &p1, ToolsLimit::Read).unwrap();
        let err = run(&p1, "git_commit", json!({"message": "x"})).unwrap_err();
        assert!(err.contains("may only read"), "{err}");
        assert!(run(&p1, "agent_activity", json!({"seat": "orchestrator"})).is_ok());
        assert!(agent_decls(ToolsLimit::Read).iter().all(|d| d.name == FIND || d.name == CALL || risk_of(&d.name) == Some(Risk::Read)));
        set_tools_limit(&state, &p1, ToolsLimit::None).unwrap();
        let err = run(&p1, "agent_activity", json!({"seat": "orchestrator"})).unwrap_err();
        assert!(err.contains("none of the Bakery's tools"), "{err}");
        assert!(agent_decls(ToolsLimit::None).is_empty());
    }

    /// An agent is never asked for the conversation or project its call
    /// fills in itself; what it must name stays required, and the app's own
    /// schema is untouched.
    #[test]
    fn an_agents_own_conversation_and_project_are_never_required() {
        let tool = |path: &str| api_catalog::tools().iter().find(|t| t.path == path && t.method == api_catalog::Method::Get).unwrap();
        let required = |input: &Value| input["required"].as_array().cloned().unwrap_or_default();
        let read = tool("/terminal/read");
        assert!(required(&read.input).contains(&json!("stream_id")), "the app's schema keeps it");
        let theirs = agent_input(read);
        assert!(!required(&theirs).contains(&json!("stream_id")) && required(&theirs).contains(&json!("terminal_id")), "{theirs}");
        assert!(theirs["properties"]["stream_id"]["description"].as_str().unwrap().ends_with("Your own conversation when left out."));
        let project = agent_input(tool("/project/rollup"));
        assert!(!required(&project).contains(&json!("project_id")), "{project}");
        let own_filled = |name: &str| {
            api_catalog::find(name).is_some_and(|t| matches!(t.agents, Some(api_catalog::Reach::Own | api_catalog::Reach::Project)))
        };
        let menu = agent_decls(ToolsLimit::All);
        assert!(menu.iter().filter(|d| own_filled(&d.name)).all(|d| !required(&d.parameters).contains(&json!("stream_id"))));
    }

    #[test]
    fn an_agent_opens_a_peer_in_its_project_from_its_own_head() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let repo = make_repo(&dir.path().join("repo"));
        let root = crate::catalog::DirectoryRootSpec::Attach(&repo.to_string_lossy());
        let (p, p1) = crate::catalog::create_project(&state, "P", Some(root)).unwrap();
        let p1 = p1.stream_id;
        // Its work in progress, committed on a branch of its own.
        run_git(&repo, &["checkout", "-qb", "feature"]);
        std::fs::write(repo.join("g.txt"), "two\n").unwrap();
        run_git(&repo, &["add", "."]);
        run_git(&repo, &["commit", "-qm", "wip"]);
        crate::agent_control::set_operating_mode(&state, &p1, Seat::Orchestrator, OperatingMode::Manual).unwrap();
        let agent = Caller { seat: Seat::Orchestrator, ..caller(&p1, OperatingMode::Auto, None) };
        // Opening a peer asks the operator even in Auto; they allow each.
        let open = |args: Value| allowing(&state, &p1, "thread_create", || call(&state, &agent, "thread_create", &args));

        let err = open(json!({"title": "Helper", "mode": "auto"})).unwrap_err();
        assert!(err.contains("at most yours (manual)"), "{err}");
        let err = open(json!({"title": "Helper", "model": "x"})).unwrap_err();
        assert!(err.contains("leave them out"), "{err}");

        let made: Value = serde_json::from_str(&open(json!({"title": "Helper", "instruction": "check it"})).unwrap()).unwrap();
        assert!(made["queued"].is_object(), "its first message is queued: {made}");
        let peer = made["thread"]["stream_id"].as_str().unwrap();
        let ctx = crate::state::context_for(&state, peer).unwrap();
        assert!(ctx.created_by_bakery && ctx.branch.starts_with("helper-"), "{ctx:?}");
        assert!(std::path::Path::new(&ctx.worktree_path).join("g.txt").is_file(), "it starts from its creator's HEAD");
        let project = crate::state::socket_project(&state.heart.lock().unwrap(), peer).0;
        assert_eq!(project, Some(p.project_id.clone()));
        let mode = crate::agent_control::configured_operating_mode(&state, peer, &crate::node::agent_id(peer, Seat::Orchestrator));
        assert_eq!(mode, Some(OperatingMode::Manual), "its creator's mode");
        let human: Vec<String> = state.heart.lock().unwrap().read_stream(peer).unwrap().into_iter()
            .filter(|e| e.actor == "human").map(|e| e.family).collect();
        assert!(human.is_empty(), "what the agent did is recorded as its own: {human:?}");
        let queued: Value = serde_json::from_str(made["queued"]["payload"].as_str().unwrap()).unwrap();
        assert_eq!(queued["actor"], crate::actor::agent(&p1, Seat::Orchestrator), "its first turn starts as the creator's");

        state.node.lock().unwrap().peer_limit = Some(1);
        let err = open(json!({"title": "Another"})).unwrap_err();
        assert!(err.contains("limit of 1 open peers"), "{err}");
    }

    #[test]
    fn an_agents_task_is_in_its_project_and_runs_in_its_mode() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let (p, p1) = crate::catalog::create_project(&state, "P", None).unwrap();
        let (q, _) = crate::catalog::create_project(&state, "Q", None).unwrap();
        let agent = Caller { seat: Seat::Orchestrator, ..caller(&p1.stream_id, OperatingMode::Auto, None) };
        let order = json!({"title": "Fix the build", "work_order": "wo-1"});
        // Creating a task asks the operator even in Auto; they allow it.
        let made = allowing(&state, &p1.stream_id, "task_create", || call(&state, &agent, "task_create", &order));
        let reply: Value = serde_json::from_str(&made.unwrap()).unwrap();
        assert_eq!(reply["task"]["parent_project_id"], json!(p.project_id), "its own project fills in");
        let task = reply["task"]["stream_id"].as_str().unwrap();
        let task_agent = crate::node::agent_id(task, Seat::Orchestrator);
        let mode = crate::agent_control::configured_operating_mode(&state, task, &task_agent);
        assert_eq!(mode, Some(OperatingMode::Auto), "its creator's mode");
        let human: Vec<String> = state.heart.lock().unwrap().read_stream(task).unwrap().into_iter()
            .filter(|e| e.actor == "human").map(|e| e.family).collect();
        assert!(human.is_empty(), "what the agent did is recorded as its own: {human:?}");

        let elsewhere = json!({"parent_project_id": q.project_id, "title": "x", "work_order": "wo-2"});
        let err = call(&state, &agent, "task_create", &elsewhere).unwrap_err();
        assert!(err.contains("another project"), "{err}");
        let retire = json!({"project_id": p.project_id, "task_id": reply["task"]["task_id"]});
        let err = call(&state, &agent, "task_retire", &retire).unwrap_err();
        assert!(err.contains("the Baker's only"), "retiring stays with the operator and the Baker: {err}");
    }

    #[test]
    fn the_gate_asks_by_grade_and_mode() {
        use OperatingMode::*;
        for mode in [Manual, Plan, AutoEdit, Auto] {
            assert_eq!(gate(mode, Risk::Read, false), Verdict::Allow, "{mode:?}");
            let destructive = if mode == Plan { Verdict::Deny } else { Verdict::Ask };
            assert_eq!(gate(mode, Risk::Destructive, false), destructive, "{mode:?}");
            for risk in [Risk::Read, Risk::Write, Risk::Run, Risk::Destructive] {
                assert_eq!(gate(mode, risk, true), Verdict::Ask, "always_ask, {risk:?} in {mode:?}");
            }
        }
        for risk in [Risk::Write, Risk::Run] {
            assert_eq!(gate(Manual, risk, false), Verdict::Ask);
            assert_eq!(gate(AutoEdit, risk, false), Verdict::Ask);
            assert_eq!(gate(Plan, risk, false), Verdict::Deny);
            assert_eq!(gate(Auto, risk, false), Verdict::Allow);
        }
    }

    /// Everything that opens a thread asks the operator first in every mode,
    /// Plan included, whoever calls: a Baker thread, a workspace and its
    /// worker, a chat, a project, a checkout, a task and its start. So does
    /// merging a thread into main. Its card still names its grade. A backlog
    /// add follows the mode.
    #[test]
    fn opening_a_thread_or_a_workspace_always_asks() {
        let dir = tempfile::tempdir().unwrap();
        let repo = bakery_repo_for_test(dir.path());
        run_git(&repo, &["add", "."]);
        run_git(&repo, &["commit", "-qm", "the bakery"]);
        let state = state_with_bakery_root(dir.path(), &repo);
        crate::routes::baker::ensure_permanent_baker(&state, None, None).unwrap();
        let baker = state.node.lock().unwrap().baker_stream_id.clone().unwrap();
        let orc = crate::workspaces::create_workspace(&state, "A", None, "human").unwrap().stream_id;
        let project = crate::catalog::create_project(&state, "P", None).unwrap().1.stream_id;
        let cards = |stream: &str, tool: &str| -> Vec<Value> {
            let events = state.heart.lock().unwrap().read_stream(stream).unwrap();
            events.iter().filter(|e| e.family == CHILD_TOOL_PENDING_FAMILY).map(payload)
                .filter(|p| p["bakery_tool"]["name"] == tool).collect()
        };
        // What opens a thread, and merging one into main: a Baker op with no
        // route of its own.
        let asks = [
            "thread_create", "workspace", "chat_create", "socket_open", "project_open", "project_create",
            "worktree", "github_clone", "baker_assign", "task_create", "task_start", "thread_merge",
        ];
        let mut callers: Vec<(&String, Seat, &str)> = asks.iter().map(|tool| (&baker, Seat::Baker, *tool)).collect();
        callers.push((&orc, Seat::Orchestrator, "thread_create"));
        callers.extend(["thread_create", "task_create", "task_start"].map(|tool| (&project, Seat::Orchestrator, tool)));

        use OperatingMode::*;
        for mode in [Auto, AutoEdit, Manual, Plan] {
            let title = format!("{mode:?}");
            for &(stream, seat, tool) in &callers {
                let asker = Caller { seat, ..caller(stream, mode, Some(Duration::ZERO)) };
                let asked = call(&state, &asker, tool, &json!({"title": title}));
                assert_eq!(asked.unwrap_err(), WAITING, "{tool} by {seat:?} in {mode:?}");
                let card = cards(stream, tool).pop().unwrap();
                assert_eq!(card["bakery_tool"]["args"]["title"], title.as_str(), "{card}");
                let grade = risk_of(tool).unwrap().as_str();
                assert_eq!(card["bakery_tool"]["risk"], grade, "the card names its grade: {card}");
            }
        }

        let item = json!({"title": "Later", "why": "it can wait"});
        call(&state, &caller(&baker, Auto, None), "backlog_add", &item).unwrap();
        assert!(cards(&baker, "backlog_add").is_empty(), "Auto adds to the backlog unasked");
        let plan = call(&state, &caller(&baker, Plan, None), "backlog_add", &item).unwrap_err();
        assert!(plan.contains("Plan mode"), "{plan}");

        // api_find tells the model which ones always ask.
        let marked = |tool: &str, limit: Option<ToolsLimit>| {
            // A route's hit names its path; the Baker's op names none.
            let path = api_catalog::find(tool).filter(|_| limit.is_some() || !crate::baker_ops::is_op(tool));
            let query = format!("{tool} {}", path.map(|t| format!("{})", t.path)).unwrap_or_default());
            let found: Value = serde_json::from_str(&find(&query, limit)).unwrap();
            let hit = found["tools"].as_array().unwrap().iter().find(|t| t["name"] == tool).cloned();
            hit.unwrap_or_else(|| panic!("{tool} not found: {found}"))["always_ask"] == true
        };
        for tool in asks {
            assert!(marked(tool, None), "{tool}");
        }
        for tool in ["thread_create", "task_create", "task_start"] {
            assert!(marked(tool, Some(ToolsLimit::All)), "an agent's find too: {tool}");
        }
        assert!(!marked("backlog_add", None));
    }

    #[test]
    fn the_menu_keeps_the_ops_and_reaches_every_route_through_two_tools() {
        let names: Vec<String> = decls().into_iter().map(|d| d.name).collect();
        for op in crate::baker_ops::BAKER_OPS {
            assert!(names.iter().any(|n| n == op.wire), "{} left the menu", op.wire);
        }
        assert!(names.iter().any(|n| n == FIND) && names.iter().any(|n| n == CALL));
        let found: Value = serde_json::from_str(&find("discard stream", None)).unwrap();
        assert_eq!(found["tools"][0]["name"], "stream_discard", "{found}");
        assert_eq!(found["tools"][0]["risk"], "destructive", "{found}");
        assert!(!find("api key", None).contains("auth_api_key"), "a hidden route is never found");
    }

    #[test]
    fn a_route_tool_runs_the_real_handler_as_the_baker() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let repo = make_repo(&dir.path().join("repo"));
        let stream_id = open_stream(&state, &open_req(&repo, "api-tools"))
            .unwrap()
            .context
            .stream_id;
        as_baker(&state, &stream_id);
        let auto = caller(&stream_id, OperatingMode::Auto, None);

        let args = json!({"tool": "agent_mode", "args": {"stream_id": stream_id, "seat": "orchestrator", "mode": "plan", "actor": "human"}});
        let out = call(&state, &auto, CALL, &args).unwrap();
        assert!(out.contains("plan"), "{out}");
        let events = state.heart.lock().unwrap().read_stream(&stream_id).unwrap();
        let set = events.iter().find(|e| e.family == crate::agent_control::OPERATING_MODE_SET).unwrap();
        assert_eq!(set.actor, crate::actor::agent(&stream_id, Seat::Baker), "the Heart says which Baker conversation acted");

        let listed = call(&state, &auto, "socket_list", &json!({})).unwrap();
        assert!(!listed.contains("owner_node_id"), "{listed}");
        let read = call(&state, &auto, CALL, &json!({"tool": "stream", "args": {"id": stream_id}})).unwrap();
        assert!(read.contains(&stream_id), "{read}");
        let missing = call(
            &state,
            &auto,
            CALL,
            &json!({"tool": "files_read", "args": {"stream_id": stream_id, "path": "nope.txt"}}),
        );
        let refused = missing.unwrap_err();
        assert!(refused.starts_with("500"), "a handler's refusal is the call's error: {refused}");
    }

    #[test]
    fn hidden_routes_and_secret_paths_are_out_of_reach() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let repo = make_repo(&dir.path().join("repo"));
        let stream_id = open_stream(&state, &open_req(&repo, "api-reach"))
            .unwrap()
            .context
            .stream_id;
        as_baker(&state, &stream_id);
        let auto = caller(&stream_id, OperatingMode::Auto, None);
        let refused = |tool: &str, args: Value| {
            call(&state, &auto, CALL, &json!({"tool": tool, "args": args})).unwrap_err()
        };
        assert!(refused("auth_api_key", json!({})).contains("unknown Bakery tool"));
        assert!(refused("project_inspect", json!({"path": "~/.ssh"})).contains("out of reach"));
        let sneaky = state.base.join("projects/../node.json");
        assert!(refused("project_inspect", json!({"path": sneaky})).contains("out of reach"));
        let secret = refused("files_read", json!({"stream_id": stream_id, "path": ".env"}));
        assert!(secret.contains("out of reach"), "{secret}");
        let private = json!({"stream_id": stream_id, "path": "crates/bakery-agent/src/private/x.rs"});
        assert!(refused("files_read", private).contains("private provider code"));
        // A path outside the caller's reach is refused; a plain text field
        // that happens to start with "/" is not a path.
        assert!(refused("project_inspect", json!({"path": "/opt/elsewhere"})).contains("out of reach"));
        let text = json!({"stream_id": stream_id, "seat": "orchestrator", "instruction": "/compact"});
        call(&state, &auto, CALL, &json!({"tool": "agent_turn_queue", "args": text})).unwrap();
    }

    /// A push card shows exactly what goes out, its commits and diff against
    /// the remote; if the branch moves before the push, it asks again, and
    /// an approval sends the commit its card showed and nothing newer.
    #[test]
    fn a_push_card_shows_what_goes_out_and_asks_again_when_it_changes() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let local = crate::git::tests::behind_its_origin(dir.path());
        let ctx = open_stream(&state, &open_req(&local, "showing")).unwrap().context;
        let sid = ctx.stream_id.clone();
        as_baker(&state, &sid);
        let wt = std::path::Path::new(&ctx.worktree_path);
        let commit = |name: &str| {
            std::fs::write(wt.join(name), "x\n").unwrap();
            run_git(wt, &["add", "."]);
            run_git(wt, &["commit", "-qm", name]);
            crate::git::run(wt, &["rev-parse", "HEAD"]).unwrap().trim().to_string()
        };
        let origin = dir.path().join("origin.git");
        let args = json!({"stream_id": sid});
        let manual = caller(&sid, OperatingMode::Manual, Some(Duration::from_millis(300)));
        let card_payload = |hash: &str| -> Value {
            let heart = state.heart.lock().unwrap();
            let events = heart.read_stream(&sid).unwrap();
            serde_json::from_str(&events.iter().find(|e| e.event_hash == hash).unwrap().payload).unwrap()
        };

        let one = commit("one.txt");
        assert_eq!(call(&state, &manual, "git_push", &args).unwrap_err(), WAITING);
        let first = last_card(&state, &sid, "git_push", &args).unwrap();
        let push = card_payload(&first.hash)["push"].clone();
        assert_eq!(push["head"], one);
        assert_eq!((push["remote_head"].is_null(), push["base"].as_str()), (true, Some("origin/main")), "new on the remote: against where it forked");
        assert_eq!(push["commits"][0]["subject"], "one.txt");
        assert!(push["diff"].as_str().unwrap().contains("+x"), "{push}");

        // The branch moves before the operator answers: a new card, the old
        // one withdrawn.
        let two = commit("two.txt");
        assert_eq!(call(&state, &manual, "git_push", &args).unwrap_err(), WAITING);
        let second = last_card(&state, &sid, "git_push", &args).unwrap();
        assert_ne!(second.hash, first.hash);
        assert_eq!(second.push_head.as_deref(), Some(two.as_str()));

        // Approved for two; three lands before the push: it asks again.
        crate::pipe::decide_child_tool_gate(&state, &sid, &second.hash, ChildToolGateDecisionKind::AllowOnce).unwrap();
        let three = commit("three.txt");
        assert_eq!(call(&state, &manual, "git_push", &args).unwrap_err(), WAITING);
        let third = last_card(&state, &sid, "git_push", &args).unwrap();
        assert_eq!(third.push_head.as_deref(), Some(three.as_str()));
        crate::pipe::decide_child_tool_gate(&state, &sid, &third.hash, ChildToolGateDecisionKind::AllowOnce).unwrap();
        call(&state, &manual, "git_push", &args).unwrap();
        assert_eq!(crate::git::run(&origin, &["rev-parse", &ctx.branch]).unwrap().trim(), three);

        // A push of a given commit sends exactly it, and only one of the
        // branch's own.
        let four = commit("four.txt");
        commit("five.txt");
        let req = |commit: &str| crate::routes::git::PushReq { stream_id: sid.clone(), branch: None, commit: Some(commit.to_string()) };
        crate::routes::git::push(&state, &req(&four)).unwrap();
        assert_eq!(crate::git::run(&origin, &["rev-parse", &ctx.branch]).unwrap().trim(), four);
        let elsewhere = crate::git::run(wt, &["rev-parse", "origin/main"]).unwrap().trim().to_string();
        run_git(wt, &["switch", "-q", "-c", "side", &one]);
        let side = commit("side.txt");
        run_git(wt, &["switch", "-q", &ctx.branch]);
        let err = crate::routes::git::push(&state, &req(&side)).unwrap_err();
        assert!(err.contains("is not a commit of"), "{err}");
        assert!(!elsewhere.is_empty());
    }

    /// "Push for me": a push card also offers this conversation its own
    /// branch from now on; once the operator gives that, the next push goes
    /// ahead with no card, and history says it went under the grant.
    #[test]
    fn a_granted_push_goes_ahead_without_a_card() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let local = crate::git::tests::behind_its_origin(dir.path());
        let ctx = open_stream(&state, &open_req(&local, "pushing")).unwrap().context;
        let sid = ctx.stream_id.clone();
        as_baker(&state, &sid);
        let wt = std::path::Path::new(&ctx.worktree_path);
        let commit = |name: &str| {
            std::fs::write(wt.join(name), "x\n").unwrap();
            run_git(wt, &["add", "."]);
            run_git(wt, &["commit", "-qm", name]);
        };
        let pushed = || crate::git::run(&dir.path().join("origin.git"), &["rev-parse", &ctx.branch]).ok();
        let args = json!({"stream_id": sid});
        let manual = caller(&sid, OperatingMode::Manual, Some(Duration::from_millis(300)));

        commit("one.txt");
        assert_eq!(call(&state, &manual, "git_push", &args).unwrap_err(), WAITING);
        let card = last_card(&state, &sid, "git_push", &args).unwrap();
        let payload: Value = {
            let heart = state.heart.lock().unwrap();
            let events = heart.read_stream(&sid).unwrap();
            serde_json::from_str(&events.iter().find(|e| e.event_hash == card.hash).unwrap().payload).unwrap()
        };
        assert_eq!((payload["grant"]["kind"].as_str(), payload["grant"]["branch"].as_str()), (Some("push"), Some(ctx.branch.as_str())));
        crate::pipe::decide_child_tool_gate(&state, &sid, &card.hash, ChildToolGateDecisionKind::AllowAlways).unwrap();
        call(&state, &manual, "git_push", &args).unwrap();
        assert_eq!(crate::grants::live(&state, &sid).len(), 1);
        let first = pushed().expect("pushed");

        commit("two.txt");
        call(&state, &manual, "git_push", &args).unwrap();
        assert_ne!(pushed().unwrap(), first, "the next push went ahead");
        let used = state.heart.lock().unwrap().events_of_family(&sid, crate::grants::USED).unwrap().len();
        assert_eq!(used, 2, "history says each went under the grant");
        assert!(last_card(&state, &sid, "git_push", &args).unwrap().used, "the card's approval is used up with its call");

        // Once the grant ends, the next push asks again.
        let grant = crate::grants::live(&state, &sid).remove(0);
        crate::grants::end(&state, &crate::grants::EndReq { stream_id: sid.clone(), grant_id: grant.grant_id }).unwrap();
        commit("three.txt");
        assert_eq!(call(&state, &manual, "git_push", &args).unwrap_err(), WAITING);
        let plan = caller(&sid, OperatingMode::Plan, None);
        assert!(call(&state, &plan, "git_push", &args).unwrap_err().contains("Plan mode"), "Plan still changes nothing");
    }

    #[test]
    fn an_ask_waits_for_the_operator_and_its_decision_holds_for_the_exact_call() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let repo = make_repo(&dir.path().join("repo"));
        let stream_id = open_stream(&state, &open_req(&repo, "api-ask"))
            .unwrap()
            .context
            .stream_id;
        as_baker(&state, &stream_id);
        let args = json!({"stream_id": stream_id, "seat": "orchestrator", "mode": "plan"});
        let manual = caller(&stream_id, OperatingMode::Manual, Some(Duration::from_millis(300)));
        let decide = |hash: &str, decision| {
            crate::pipe::decide_child_tool_gate(&state, &stream_id, hash, decision).unwrap()
        };
        let wrote = || {
            let heart = state.heart.lock().unwrap();
            let events = heart.read_stream(&stream_id).unwrap();
            let set = crate::agent_control::OPERATING_MODE_SET;
            events.iter().filter(|e| e.family == set).count()
        };

        // Nobody decides in time: the card stays and the call says so.
        assert_eq!(call(&state, &manual, "agent_mode", &args).unwrap_err(), WAITING);
        let card = last_card(&state, &stream_id, "agent_mode", &args).expect("the card stays");
        assert!(card.decision.is_none());

        // The operator allows it after the call gave up; the retry runs once.
        decide(&card.hash, ChildToolGateDecisionKind::AllowOnce);
        assert!(
            state.child_tool_allow_once.lock().unwrap().is_empty(),
            "the card allows that call, never a whole tool kind"
        );
        call(&state, &manual, "agent_mode", &args).unwrap();
        assert_eq!(wrote(), 1);
        // The same call again needs a new approval.
        assert_eq!(call(&state, &manual, "agent_mode", &args).unwrap_err(), WAITING);
        assert_eq!(wrote(), 1);

        // A denial refuses the retry at once.
        let card = last_card(&state, &stream_id, "agent_mode", &args).unwrap();
        decide(&card.hash, ChildToolGateDecisionKind::Deny);
        let err = call(&state, &manual, "agent_mode", &args).unwrap_err();
        assert!(err.contains("denied"), "{err}");

        // An unbounded caller (the API tool loop) blocks until the decision.
        let other = json!({"stream_id": stream_id, "seat": "orchestrator", "mode": "auto"});
        let decider = {
            let (state, stream_id, other) = (state.clone(), stream_id.clone(), other.clone());
            std::thread::spawn(move || loop {
                if let Some(card) = last_card(&state, &stream_id, "agent_mode", &other) {
                    crate::pipe::decide_child_tool_gate(
                        &state,
                        &stream_id,
                        &card.hash,
                        ChildToolGateDecisionKind::AllowOnce,
                    )
                    .unwrap();
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            })
        };
        let blocking = caller(&stream_id, OperatingMode::Manual, None);
        call(&state, &blocking, "agent_mode", &other).unwrap();
        decider.join().unwrap();
        assert_eq!(wrote(), 2);

        // Plan changes nothing; reads never ask.
        let plan = caller(&stream_id, OperatingMode::Plan, None);
        assert!(call(&state, &plan, "agent_mode", &other).unwrap_err().contains("Plan mode"));
        call(&state, &plan, "agent_activity", &json!({"stream_id": stream_id, "seat": "orchestrator"})).unwrap();
    }

    /// A workspace's orc reads its own workers through its tools, and lists
    /// them; it changes none of them and reaches no other workspace. A
    /// worker reads only itself.
    #[test]
    fn an_orc_reads_its_own_workers_and_nothing_wider() {
        let dir = tempfile::tempdir().unwrap();
        let repo = bakery_repo_for_test(dir.path());
        run_git(&repo, &["add", "."]);
        run_git(&repo, &["commit", "-qm", "the bakery"]);
        let state = state_with_bakery_root(dir.path(), &repo);
        crate::routes::baker::ensure_permanent_baker(&state, None, None).unwrap();
        let orc = |title: &str| crate::workspaces::create_workspace(&state, title, None, "human").unwrap().stream_id;
        let (a, b) = (orc("A"), orc("B"));
        let worker = |orc: &str| {
            let req = crate::baker_threads::CreateThreadReq { title: Some("worker".into()), ..Default::default() };
            crate::baker_threads::create_peer(&state, &req, orc, "human").unwrap().thread.stream_id
        };
        let (a1, b1) = (worker(&a), worker(&b));
        let agent = |stream: &str| Caller { seat: Seat::Orchestrator, ..caller(stream, OperatingMode::Auto, None) };
        let run = |stream: &str, tool: &str, args: Value| call(&state, &agent(stream), tool, &args);

        assert!(run(&a, "agent_activity", json!({"stream_id": a1, "seat": "orchestrator"})).is_ok(), "its worker");
        let read: Value = serde_json::from_str(&run(&a, "thread_read", json!({"stream_id": a1})).unwrap()).unwrap();
        assert_eq!(read["state"].as_str(), Some("idle"), "{read}");
        let err = run(&a, "turn_stop", json!({"stream_id": a1, "seat": "orchestrator"})).unwrap_err();
        assert!(err.contains("an orc only reads its workers"), "{err}");
        let err = run(&a, "agent_activity", json!({"stream_id": b1, "seat": "orchestrator"})).unwrap_err();
        assert!(err.contains("out of your reach"), "another workspace's worker: {err}");
        let err = run(&a1, "agent_activity", json!({"stream_id": a, "seat": "orchestrator"})).unwrap_err();
        assert!(err.contains("out of your reach"), "a worker reads only itself: {err}");
        let listed: Vec<Value> = serde_json::from_str(&run(&a, "socket_list", json!({})).unwrap()).unwrap();
        let ids: Vec<&str> = listed.iter().filter_map(|s| s["stream_id"].as_str()).collect();
        assert!(ids.contains(&a.as_str()) && ids.contains(&a1.as_str()), "{ids:?}");
        assert!(!ids.contains(&b.as_str()) && !ids.contains(&b1.as_str()), "{ids:?}");
    }
}
