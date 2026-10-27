//! Declared Bakery operator tools (07d-B Phase 2 / Stage B) — the compiled-in
//! ops table that gives [`crate::operator_tools::OperatorRunner`] its first
//! real callers plug into. This is deliberately NOT the `~/.bakery/tools`
//! registry (`crate::operator_tools::load_from_default_dir`, an
//! operator-supplied, out-of-tree JSON table): the Bakery ops here are
//! compiled into this binary, and every op's `execute` path below calls the
//! exact inner function a human HTTP route calls — `catalog::create_project_with`,
//! `crate::tasks::*`, `routes::seats::seat_assign_checked`,
//! `routes::memory::memory_view_inner`/`memory_write_inner_as` — never a
//! second implementation.
//!
//! **Authorization** (Phase 2 item 2): callable only by an agent turn in the
//! Baker's own scope — the recorded `NodeConfig.baker_stream_id` or one of
//! its threads ([`is_baker_scope_stream`]; a thread's write-once owner fact
//! must name that same pointer, `crate::baker_threads`) — never by
//! `is_baker_stream_ctx`'s kind bit alone, which is also true of every old
//! `/baker/assign` branch-job socket (also `SocketKind::Factory`) and would
//! silently grant these ops to all of them.
//!
//! **Gating**: every op carries a risk grade, and `api_tools::call` runs it
//! through the same gate as every route tool, on every runtime: Manual asks
//! before any write, Plan changes nothing, destructive calls always ask.
//! `task_retire` is no longer an op: the `POST /task/retire` route serves it,
//! graded destructive, so it asks the operator first.
//!
//! **Only what is not a route**: an op that only wrapped a route's own inner
//! function is gone; the route serves that name (`api_tools`). What stays
//! adds behaviour a route lacks (a caller's mode or spawn edge, the Baker's
//! one memory, a self-message guard) or has no route at all.
//!
//! **No prompt-injected hostnames** (Phase 2 item 6): route results reach a
//! model scrubbed of node identity (`api_tools::call_route`), so
//! `owner_node_id` — the operator machine's identity — never reaches it.
//!
//! **Actor attribution**: an op is recorded as the Baker conversation that
//! called it, `agent:<its thread>:baker` (`actor::agent`). The ops pass that
//! actor to their `_as` inner functions, and whatever they call that records
//! `"human"` runs inside the caller's actor scope (`actor::with`, set by
//! `api_tools::call`), so nothing an op does reads as the operator's.

use crate::api_catalog::Risk;
use crate::catalog;
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One compiled-in Bakery op: identity + whether it is a read-only "sense"
/// query or a mutating "act" op. `GET /baker` / `GET /baker/capabilities`
/// render this table's names (and, for capabilities, summaries) — see
/// `routes::baker::baker_capabilities`/`baker_home_for`.
///
/// `id` is the spec's own dotted identifier (07d-B Phase 2 item 1's "Initial
/// set" names it this way, and doc comments/prose below refer to ops by it).
/// `wire` is the SAME op under the name actually handed to a model/MCP client
/// as a callable tool: the Anthropic and OpenAI-shaped function-calling APIs
/// both require tool names to match `^[a-zA-Z0-9_-]{1,64}$`, which `.` does
/// not satisfy, so `id` itself can never be a `ToolDecl.name` — every
/// declaration/dispatch surface below keys off `wire`, never `id`, and
/// `id`→`wire` is a fixed 1:1 mapping (`.` → `_`) enforced by
/// `wire_names_are_valid_tool_names_and_map_uniquely_to_ids` below.
#[derive(Debug, Clone, Copy, Serialize)]
pub(crate) struct BakerOpSpec {
    pub(crate) id: &'static str,
    pub(crate) wire: &'static str,
    pub(crate) summary: &'static str,
    /// The gate's input (`api_tools::gate`): a read op runs in every mode.
    pub(crate) risk: Risk,
    /// Asks the operator before every call, in every mode, as a route marked
    /// `always_ask` in api_overrides.json does; an op also follows the route
    /// it shares its name with.
    pub(crate) always_ask: bool,
}

/// The complete registry (07d-B Phase 2 item 1's "Initial set"). Order here
/// is the order `baker_op_tool_decls`/prompt rendering use.
pub(crate) const BAKER_OPS: &[BakerOpSpec] = &[
    BakerOpSpec { id: "project.list", wire: "project_list", summary: "List every known Project (name, roots, stacks).", risk: Risk::Read, always_ask: false },
    BakerOpSpec {
        id: "node.versions",
        wire: "node_versions",
        summary: "What this node runs, read from what executes: Bakery build (and whether a restart is needed), Node.js, the Claude native adapter, SDK and Claude Code engine, and each live session's runtime. check_latest also asks npm for the newest releases.",
        risk: Risk::Read,
        always_ask: false,
    },
    BakerOpSpec {
        id: "task.create",
        wire: "task_create",
        summary: "Mint a task's durable identity + completion contract (scope/owned_files/depends_on/proof). Optional `mode` (manual|plan|auto_edit|auto) for the task's agent; unset, it inherits this conversation's mode.",
        risk: Risk::Write,
        always_ask: false,
    },
    BakerOpSpec {
        id: "memory.write",
        wire: "memory_write",
        summary: "Author a durable L1 note in the Baker's one shared memory (a thread's stream_id lands on the Baker's own).",
        risk: Risk::Write,
        always_ask: false,
    },
    BakerOpSpec {
        id: "thread.create",
        wire: "thread_create",
        summary: "Open a new parallel Baker thread, a worker: on its own runtime, model, effort and mode (unset, mode is yours), optionally in its own git worktree; an instruction starts it at once. Then thread_wait for its result. You are its spawner: when it says its work is ready to merge, the card and the review come to you.",
        risk: Risk::Write,
        always_ask: false,
    },
    BakerOpSpec {
        id: "thread.message",
        wire: "thread_message",
        summary: "Queue a message into another Baker conversation (it runs there, in parallel, attributed to the Baker).",
        risk: Risk::Write,
        always_ask: false,
    },
    BakerOpSpec {
        id: "thread.merge",
        wire: "thread_merge",
        summary: "Merge a chef's work into main: the Baker thread's own branch, merged (no fast-forward) into the main checkout's branch, once you have reviewed, tested and audited it and the operator has told you to merge it for this case. The conversation that opened the thread merges its work, and the permanent Baker; only a thread's own branch, only into a clean main checkout; a conflict merges nothing. Then the thread is archived and its worktree and branch are removed (anything not committed goes to the Trash first). Pushing and deploying stay the operator's.",
        risk: Risk::Write,
        always_ask: true,
    },
    BakerOpSpec {
        id: "merge.ready",
        wire: "merge_ready",
        summary: "Say your work is ready to merge (a Baker thread with its own branch, everything committed): the conversation that opened you (the permanent Baker when that one is archived or gone) gets a card and a message to review, test and audit it and bring the operator the merge review, and it joins the waiting list. A note says what it is and how you tested it. Asking again replaces it.",
        risk: Risk::Write,
        always_ask: false,
    },
    BakerOpSpec {
        id: "thread.archive",
        wire: "thread_archive",
        summary: "Archive (hide) a Baker thread — reversible; its history is kept. A busy thread must be stopped first.",
        risk: Risk::Write,
        always_ask: false,
    },
    BakerOpSpec { id: "thread.restore", wire: "thread_restore", summary: "Restore an archived Baker thread.", risk: Risk::Write, always_ask: false },
    BakerOpSpec {
        id: "chat.create",
        wire: "chat_create",
        summary: "Start a new chat for the operator (its own folder inside Bakery). With model_id and instruction it starts working on the instruction right away.",
        risk: Risk::Run,
        always_ask: false,
    },
    BakerOpSpec {
        id: "turn.stop",
        wire: "turn_stop",
        summary: "Stop the turn running in another conversation and pause its queue (queued messages are kept).",
        risk: Risk::Write,
        always_ask: false,
    },
    BakerOpSpec {
        id: "access.request",
        wire: "access_request",
        summary: "Ask the operator for a folder outside your workspace — a project to check out, a fresh clone or build. access: read, write, run, or full (read, write and run its programs). Call it before touching the folder, then end your turn: the operator approves it once in this chat and this conversation continues with the folder reachable.",
        // Only asks: nothing is reachable until the operator allows it in the
        // chat, so like a sense op it runs in every mode, with no second
        // permission card in front of the real one.
        risk: Risk::Read,
        always_ask: false,
    },
];

/// Look up a registered op by its WIRE name — the name a model/MCP client
/// actually calls (`ToolDecl.name`, `dispatch_baker_op`'s own `name`
/// argument), never the dotted spec `id` (see [`BakerOpSpec`]'s doc comment).
pub(crate) fn find_op(name: &str) -> Option<&'static BakerOpSpec> {
    BAKER_OPS.iter().find(|o| o.wire == name)
}

pub(crate) fn is_op(name: &str) -> bool {
    find_op(name).is_some()
}

/// Authorization primitive (Phase 2 item 2): the permanent Baker identity —
/// `NodeConfig.baker_stream_id` — never `routes::agent::is_baker_stream_ctx`'s
/// kind bit alone. Read fresh every call, never cached (same posture as
/// `state::socket_kind`). An empty id never matches, even against an empty
/// pointer (S0 "empty pointer" guard).
pub(crate) fn is_permanent_baker_stream(state: &AppState, stream_id: &str) -> bool {
    !stream_id.trim().is_empty()
        && state.node.lock().unwrap().baker_stream_id.as_deref() == Some(stream_id)
}

/// The Baker's whole conversation scope: the permanent Baker's own stream or
/// one of its threads (`crate::baker_threads` — a write-once owner fact
/// naming the CURRENT pointer). This is the predicate for Baker identity,
/// op authority, and every Baker protection; `is_permanent_baker_stream`
/// stays the literal "is this the main conversation" test.
pub(crate) fn is_baker_scope_stream(state: &AppState, stream_id: &str) -> bool {
    is_permanent_baker_stream(state, stream_id)
        || crate::baker_threads::is_baker_thread_stream(state, stream_id)
}

fn schema(required: &[&str], props: &[(&str, &str)]) -> Value {
    let mut properties = serde_json::Map::new();
    for (name, ty) in props {
        properties.insert((*name).to_string(), serde_json::json!({ "type": ty }));
    }
    serde_json::json!({
        "type": "object",
        "properties": properties,
        "required": required,
    })
}

/// Native tool declarations for the permanent Baker's op registry — joined
/// onto the HTTP core set only for a turn on `baker_stream_id`
/// (`routes::agent::agent_tool_turn`). `ToolDecl.name` is each op's WIRE name
/// (see [`BakerOpSpec`]'s doc comment) — the dotted `id` never reaches a
/// provider's tool-declaration wire format.
pub(crate) fn baker_op_tool_decls() -> Vec<bakery_agent::ToolDecl> {
    let schemas: &[(&str, Value)] = &[
        ("project_list", schema(&[], &[])),
        ("node_versions", schema(&[], &[("check_latest", "boolean")])),
        (
            "task_create",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "parent_project_id": { "type": "string" },
                    "title": { "type": "string" },
                    "work_order": { "type": "string" },
                    "contract": {
                        "type": "object",
                        "description": "{scope, owned_files: [glob], depends_on: [task_id], proof: [[argv]], turn_budget}",
                    },
                },
                "required": ["parent_project_id", "title", "work_order"],
            }),
        ),
        (
            "memory_write",
            schema(
                &["stream_id", "text"],
                &[
                    ("stream_id", "string"),
                    ("text", "string"),
                    ("id", "string"),
                    ("layer", "string"),
                    ("sensitivity", "string"),
                ],
            ),
        ),
        (
            "thread_create",
            // The same request the POST /baker/threads route takes.
            crate::api_catalog::ROUTES
                .iter()
                .find(|r| r.path == "/baker/threads" && r.method == crate::api_catalog::Method::Post)
                .map(crate::api_catalog::Route::input)
                .expect("POST /baker/threads is catalogued"),
        ),
        (
            "thread_message",
            schema(
                &["stream_id", "text"],
                &[("stream_id", "string"), ("text", "string")],
            ),
        ),
        (
            "thread_merge",
            schema(&["stream_id"], &[("stream_id", "string"), ("message", "string")]),
        ),
        (
            "merge_ready",
            schema(&[], &[("note", "string")]),
        ),
        (
            "thread_archive",
            schema(&["stream_id"], &[("stream_id", "string")]),
        ),
        (
            "thread_restore",
            schema(&["stream_id"], &[("stream_id", "string")]),
        ),
        (
            "chat_create",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "title": { "type": "string", "description": "optional; defaults to the instruction's first line" },
                    "model_id": { "type": "string", "description": "the model to run it on (a `model_options` id, e.g. claude-native)" },
                    "instruction": { "type": "string", "description": "optional first message; needs model_id" },
                },
                "required": [],
            }),
        ),
        (
            "turn_stop",
            schema(&["stream_id"], &[("stream_id", "string")]),
        ),
        (
            "access_request",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "the folder, absolute or ~/…" },
                    "access": { "type": "string", "enum": ["read", "write", "run", "full"] },
                    "reason": { "type": "string", "description": "one line the operator sees" },
                },
                "required": ["path", "access"],
            }),
        ),
    ];
    BAKER_OPS
        .iter()
        .map(|op| {
            let parameters = schemas
                .iter()
                .find(|(wire, _)| *wire == op.wire)
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| schema(&[], &[]));
            bakery_agent::ToolDecl {
                name: op.wire.to_string(),
                description: op.summary.to_string(),
                parameters,
            }
        })
        .collect()
}

/// Which conversation started another: written on the new conversation's
/// own stream when a Baker op creates it, so the agent map can draw the edge.
pub(crate) const SPAWNED_BY_FAMILY: &str = "agent.spawned_by";

fn record_spawn(state: &AppState, child: &str, caller: &str, op: &str) {
    if caller.is_empty() || child == caller {
        return;
    }
    let by = crate::node::agent_id(caller, crate::node::Seat::Baker);
    let _ = crate::state::append(
        state,
        child,
        &crate::actor::agent(caller, crate::node::Seat::Baker),
        SPAWNED_BY_FAMILY,
        serde_json::json!({ "by": by, "op": op }).to_string(),
        vec![],
    );
}

fn args_of<T: for<'de> Deserialize<'de>>(name: &str, args: &Value) -> Result<T, String> {
    serde_json::from_value(args.clone()).map_err(|e| format!("{name} args: {e}"))
}

fn to_json<T: Serialize>(v: &T) -> Result<String, String> {
    serde_json::to_string(v).map_err(|e| e.to_string())
}

/// Test convenience: [`dispatch_baker_op_as`] with no calling conversation.
#[cfg(test)]
pub(crate) fn dispatch_baker_op(
    state: &AppState,
    name: &str,
    args: &Value,
) -> Result<String, String> {
    dispatch_baker_op_as(state, "", name, args)
}

/// Dispatch one Bakery op by WIRE name (see [`BakerOpSpec`]'s doc comment —
/// never the dotted `id`). Callers MUST have already confirmed
/// [`is_baker_scope_stream`] for the calling turn's stream — this function
/// does not re-check authorization, matching
/// `operator_tools::dispatch_operator_tool`'s own "dispatch trusts the
/// caller checked" shape. `caller` is that stream: the thread ops use it to
/// refuse a conversation queueing messages onto itself (an unbounded
/// self-feeding loop), never to widen what the op may touch.
pub(crate) fn dispatch_baker_op_as(
    state: &AppState,
    caller: &str,
    name: &str,
    args: &Value,
) -> Result<String, String> {
    // The Baker conversation that asked; plain "agent:baker" for an op with
    // no caller to name.
    let actor = match caller {
        "" => "agent:baker".to_string(),
        caller => crate::actor::agent(caller, crate::node::Seat::Baker),
    };
    let actor = actor.as_str();
    match name {
        "project_list" => {
            let views: Vec<crate::routes::project_catalog::ProjectView> =
                catalog::known_projects(state)?
                    .into_iter()
                    .map(Into::into)
                    .collect();
            to_json(&views)
        }
        "node_versions" => {
            #[derive(Deserialize)]
            struct Args {
                #[serde(default)]
                check_latest: bool,
            }
            let a: Args = args_of(name, args)?;
            if a.check_latest {
                crate::versions::check_latest();
            }
            to_json(&crate::versions::report(state))
        }
        "task_create" => {
            let req: crate::tasks::TaskCreateReq = args_of(name, args)?;
            // Optional `mode` for the new task's own agent. Unset, the task
            // inherits the calling Baker conversation's mode: an Auto Baker's
            // long-running tasks run unattended too, a Manual one's stay
            // reviewed. There is no node-wide write gate to lean on instead.
            let explicit = args
                .get("mode")
                .and_then(Value::as_str)
                .map(|m| {
                    crate::agent_control::OperatingMode::from_id(m)
                        .ok_or_else(|| format!("unknown mode '{m}' (manual, plan, auto_edit, auto)"))
                })
                .transpose()?;
            let resp = crate::tasks::task_create_as(state, req, actor)?;
            if resp.created {
                record_spawn(state, &resp.task.stream_id, caller, name);
                let inherited = (!caller.is_empty()).then(|| {
                    crate::agent_control::operating_mode_for(
                        state,
                        caller,
                        &crate::node::agent_id(caller, crate::node::Seat::Baker),
                    )
                });
                if let Some(mode) = explicit.or(inherited) {
                    crate::agent_control::set_operating_mode(
                        state,
                        &resp.task.stream_id,
                        crate::node::Seat::Orchestrator,
                        mode,
                    )?;
                }
            }
            to_json(&resp)
        }
        "memory_write" => {
            let mut req: crate::routes::memory::MemoryWriteReq = args_of(name, args)?;
            // One Baker, one memory (BAKER-THREADS-03): a note a thread
            // authors about itself lands on the permanent Baker's stream,
            // where every conversation's prompt reads it back.
            if crate::baker_threads::is_baker_thread_stream(state, &req.stream_id) {
                if let Some(baker) = crate::baker_threads::baker_pointer(state) {
                    req.stream_id = baker;
                }
            }
            // Phase 1 item 5: agent-authored memory is scoped to the
            // permanent Baker stream only ("General agent-authored memory
            // stays 07d-C"). Dispatch itself is only reachable from a turn
            // already running in the Baker's scope, but this op's OWN
            // `stream_id` argument can still name any other socket the
            // model has seen (e.g. via `socket.list`) — check it explicitly
            // rather than trusting the caller's stream.
            if !is_permanent_baker_stream(state, &req.stream_id) {
                return Err(format!(
                    "memory.write is scoped to the permanent Baker stream only; \
                     general agent-authored memory on {} is 07d-C, not built here",
                    req.stream_id
                ));
            }
            to_json(&crate::routes::memory::memory_write_inner_as(
                state,
                &req,
                actor,
            )?)
        }
        "thread_create" => {
            let mut req: crate::baker_threads::CreateThreadReq = args_of(name, args)?;
            // Named for a conversation outside the Baker's: an agent of its
            // own, a workspace's worker or a project's peer.
            let opener = req.stream_id.clone().filter(|s| !s.trim().is_empty() && !is_baker_scope_stream(state, s));
            if let Some(opener) = opener {
                let created = crate::baker_threads::create_peer(state, &req, &opener, actor)?;
                record_spawn(state, &created.thread.stream_id, caller, name);
                return to_json(&created);
            }
            // Unset, the thread runs in the mode of the conversation that
            // opens it, as a task does (`task_create` above).
            if req.mode.is_none() && !caller.is_empty() {
                req.mode = Some(crate::agent_control::operating_mode_for(
                    state,
                    caller,
                    &crate::node::agent_id(caller, crate::node::Seat::Baker),
                ));
            }
            // The Baker already exists (the caller is in its scope), so no
            // source-root hints are needed to resolve it.
            let created = crate::baker_threads::create_thread(state, &req, actor, None, None)?;
            record_spawn(state, &created.thread.stream_id, caller, name);
            to_json(&created)
        }
        "thread_message" => {
            #[derive(Deserialize)]
            struct Args {
                stream_id: String,
                text: String,
            }
            let a: Args = args_of(name, args)?;
            if !caller.is_empty() && a.stream_id == caller {
                return Err(
                    "that is this conversation — keep working here instead of queueing a message to yourself"
                        .to_string(),
                );
            }
            let ev =
                crate::baker_threads::message_thread(state, &a.stream_id, &a.text, actor)?;
            to_json(&serde_json::json!({
                "queued": true,
                "stream_id": a.stream_id,
                "event_hash": ev.event_hash,
            }))
        }
        "thread_merge" => {
            #[derive(Deserialize)]
            struct Args {
                stream_id: String,
                #[serde(default)]
                message: Option<String>,
            }
            let a: Args = args_of(name, args)?;
            if !crate::baker_threads::may_merge(state, caller, &a.stream_id) {
                return Err("only the permanent Baker, or the conversation that opened the thread, merges its work: ask it from there".to_string());
            }
            to_json(&crate::baker_threads::merge_thread(state, &a.stream_id, a.message.as_deref(), actor)?)
        }
        "merge_ready" => {
            #[derive(Deserialize)]
            struct Args {
                #[serde(default)]
                note: Option<String>,
            }
            let a: Args = args_of(name, args)?;
            if caller.is_empty() {
                return Err("merge_ready is a Baker thread's own: call it from the thread".to_string());
            }
            to_json(&crate::baker_threads::ready_to_merge(state, caller, a.note.as_deref(), actor)?)
        }
        "thread_archive" | "thread_restore" => {
            #[derive(Deserialize)]
            struct Args {
                stream_id: String,
            }
            let a: Args = args_of(name, args)?;
            to_json(&crate::baker_threads::set_thread_archived(
                state,
                &a.stream_id,
                name == "thread_archive",
                actor,
            )?)
        }
        "chat_create" => {
            #[derive(Deserialize)]
            struct Args {
                #[serde(default)]
                title: String,
                #[serde(default)]
                model_id: Option<String>,
                #[serde(default)]
                instruction: Option<String>,
            }
            let a: Args = args_of(name, args)?;
            let instruction = a
                .instruction
                .as_deref()
                .map(str::trim)
                .filter(|t| !t.is_empty());
            if instruction.is_some() && a.model_id.is_none() {
                return Err(
                    "chat_create: give model_id to start a chat with a first message".into(),
                );
            }
            let title = match a.title.trim() {
                "" => instruction.and_then(|t| t.lines().next()).unwrap_or(""),
                t => t,
            };
            let chat = catalog::create_chat(state, title)?;
            record_spawn(state, &chat.stream_id, caller, name);
            if let Some(model_id) = &a.model_id {
                crate::routes::seats::seat_assign_checked(
                    state,
                    &chat.stream_id,
                    crate::node::Seat::Orchestrator,
                    model_id,
                )
                .map_err(|(_, msg)| msg)?;
            }
            let queued = match instruction {
                Some(text) => Some(crate::agent_control::enqueue_as(
                    state,
                    crate::routes::agent::AgentTurnReq {
                        stream_id: chat.stream_id.clone(),
                        file_path: None,
                        instruction: text.to_string(),
                        seat: Some(crate::node::Seat::Orchestrator),
                        model_id: None,
                        effort: None,
                        actor: None,
                    },
                    vec![],
                    false,
                    actor,
                )?),
                None => None,
            };
            to_json(&serde_json::json!({ "chat": chat, "queued": queued }))
        }
        "turn_stop" => {
            #[derive(Deserialize)]
            struct Args {
                stream_id: String,
            }
            let a: Args = args_of(name, args)?;
            if a.stream_id == caller {
                return Err(
                    "turn_stop: a conversation does not stop its own turn — finish it instead"
                        .into(),
                );
            }
            let ctx = crate::state::context_for(state, &a.stream_id)?;
            let seat = crate::routes::agent::default_seat_for(state, &ctx);
            to_json(&crate::routes::agent::stop_turn(state, &a.stream_id, seat)?)
        }
        "access_request" => crate::machine_access::request_access(state, caller, args_of(name, args)?),
        other => Err(format!("unknown Bakery op {other:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Baker tool call in Auto, the way a turn makes one: a route serves
    /// every name an op no longer does.
    fn tool(state: &AppState, name: &str, args: &serde_json::Value) -> Result<String, String> {
        // The Baker's call: beyond any one project.
        state.node.lock().unwrap().baker_stream_id = Some("wt:baker".to_string());
        let caller = crate::api_tools::Caller {
            stream_id: "wt:baker".to_string(),
            seat: crate::node::Seat::Baker,
            turn_id: String::new(),
            mode: crate::agent_control::OperatingMode::Auto,
            wait: None,
        };
        crate::api_tools::call(state, &caller, name, args)
    }
    use crate::testutil::*;
    use bakery_agent::StubModel;
    use std::sync::Arc;

    fn chat_only_project(state: &AppState) -> String {
        catalog::create_project(state, "Chat P", None)
            .expect("create project")
            .0
            .project_id
    }

    /// The Baker does what the operator's conversation menus do: start a
    /// chat, archive and restore it, restart its runtime, stop its turn —
    /// never stopping its own turn, never starting a chat with a message but
    /// no model to run it.
    #[test]
    fn the_baker_can_run_every_conversation_action_the_operator_can() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        state.node.lock().unwrap().baker_stream_id = Some("wt:baker".to_string());
        // Through the Baker's tools, as a turn calls them: an op, or the
        // route that serves a name an op no longer does.
        let call = |caller: &str, op: &str, args: Value| -> Result<Value, String> {
            let caller = crate::api_tools::Caller {
                stream_id: caller.to_string(),
                seat: crate::node::Seat::Baker,
                turn_id: String::new(),
                mode: crate::agent_control::OperatingMode::Auto,
                wait: None,
            };
            crate::api_tools::call(&state, &caller, op, &args)
                .map(|out| serde_json::from_str(&out).unwrap())
        };

        // Starting a chat asks the operator in every mode; they allow each.
        let chat_create = |args: Value| allowing(&state, "wt:baker", "chat_create", || call("wt:baker", "chat_create", args));
        let err = chat_create(serde_json::json!({ "instruction": "hi" })).unwrap_err();
        assert!(err.contains("model_id"), "{err}");
        let out = chat_create(serde_json::json!({ "title": "Notes" })).unwrap();
        let chat = out["chat"]["stream_id"].as_str().unwrap().to_string();
        assert_eq!(out["chat"]["name"], "Notes");
        assert!(out["queued"].is_null());
        // The new chat says which conversation started it (the agent map's edge).
        let started_by = state
            .heart
            .lock()
            .unwrap()
            .last_of_family(&chat, SPAWNED_BY_FAMILY)
            .unwrap()
            .unwrap();
        assert!(started_by.payload.contains("wt:baker:baker"), "{}", started_by.payload);

        let archived = |sid: &str| {
            let heart = state.heart.lock().unwrap();
            crate::agents::socket_archived_locked(&heart, sid)
        };
        call(
            "wt:baker",
            "conversation_archive",
            serde_json::json!({ "stream_id": chat, "archived": true }),
        )
        .unwrap();
        assert!(archived(&chat));
        call(
            "wt:baker",
            "conversation_archive",
            serde_json::json!({ "stream_id": chat, "archived": false }),
        )
        .unwrap();
        assert!(!archived(&chat));

        let out = call(
            "wt:baker",
            "runtime_restart",
            serde_json::json!({ "stream_id": chat }),
        )
        .unwrap();
        assert_eq!(out["restarted"], false, "nothing was running");
        let out = call(
            "wt:baker",
            "turn_stop",
            serde_json::json!({ "stream_id": chat }),
        )
        .unwrap();
        assert_eq!(out["cancelled"], false);
        let err = call(&chat, "turn_stop", serde_json::json!({ "stream_id": chat })).unwrap_err();
        assert!(err.contains("own turn"), "{err}");
    }

    /// "Are we up to date?" is answerable from chat with no permission
    /// prompt: a sense op returning the same report as Settings → Versions.
    #[test]
    fn node_versions_is_a_read_only_op_returning_the_versions_report() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        assert_eq!(find_op("node_versions").unwrap().risk, Risk::Read);
        let out: Value = serde_json::from_str(
            &dispatch_baker_op(&state, "node_versions", &serde_json::json!({})).unwrap(),
        )
        .unwrap();
        let ids: Vec<&str> = out["components"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["id"].as_str().unwrap())
            .collect();
        assert_eq!(
            ids,
            [
                "nodejs",
                "claude-native-adapter",
                "claude-agent-sdk",
                "claude-code"
            ]
        );
        assert!(out["build"].is_object() && out["sessions"].is_array());
    }

    /// `memory_view_inner`/`memory_write_inner_as` (like every ordinary
    /// stream read) need `context_for`'s `project.onboarded` fact — that
    /// lives on a Project's starter SOCKET stream, never on the Project
    /// record's own stream (a Project is a catalog entry, not a socket).
    fn chat_only_project_socket(state: &AppState) -> String {
        catalog::create_project(state, "Chat P", None)
            .expect("create project")
            .1
            .stream_id
    }

    #[test]
    fn is_permanent_baker_stream_is_true_only_for_the_recorded_baker_stream_id() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        state.node.lock().unwrap().baker_stream_id = Some("socket_the_real_baker".into());

        assert!(is_permanent_baker_stream(&state, "socket_the_real_baker"));
        assert!(!is_permanent_baker_stream(&state, "socket_something_else"));
        assert!(!is_permanent_baker_stream(&state, ""));
    }

    /// A task the Baker creates runs in the calling conversation's own mode
    /// unless the op names one — an Auto Baker's long-running tasks go
    /// unattended, with no node-wide gate to flip.
    #[test]
    fn task_create_op_gives_the_new_task_the_callers_mode_or_an_explicit_one() {
        use crate::agent_control::{operating_mode_for, set_operating_mode, OperatingMode};
        use crate::node::{agent_id, Seat};
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let project_id = chat_only_project(&state);
        let caller = "baker-thread-under-test";
        set_operating_mode(&state, caller, Seat::Baker, OperatingMode::Auto).unwrap();

        let task_mode = |args: serde_json::Value| {
            let out = dispatch_baker_op_as(&state, caller, "task_create", &args).unwrap();
            let resp: serde_json::Value = serde_json::from_str(&out).unwrap();
            let stream = resp["task"]["stream_id"].as_str().unwrap().to_string();
            operating_mode_for(&state, &stream, &agent_id(&stream, Seat::Orchestrator))
        };

        let inherited = task_mode(serde_json::json!({
            "parent_project_id": &project_id,
            "title": "Inherits",
            "work_order": "wo-inherit",
        }));
        assert_eq!(inherited, OperatingMode::Auto, "unset mode inherits the Baker conversation's Auto");

        let explicit = task_mode(serde_json::json!({
            "parent_project_id": &project_id,
            "title": "Explicit",
            "work_order": "wo-explicit",
            "mode": "plan",
        }));
        assert_eq!(explicit, OperatingMode::Plan, "an explicit mode wins over the caller's");
    }

    /// Stage B's headline correctness proof: an op call must produce the
    /// exact same event families/payload shape as the human route calling
    /// the same inner function — no drift, no second implementation.
    #[test]
    fn task_create_op_dispatch_produces_the_same_event_families_as_the_human_route() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let project_id = chat_only_project(&state);

        let human = crate::tasks::task_create(
            &state,
            crate::tasks::TaskCreateReq {
                parent_project_id: project_id.clone(),
                title: "Human-created task".into(),
                work_order: "wo-human".into(),
                contract: Default::default(),
            },
        )
        .unwrap();

        let op_args = serde_json::json!({
            "parent_project_id": project_id,
            "title": "Op-created task",
            "work_order": "wo-op",
        });
        let op_result = dispatch_baker_op(&state, "task_create", &op_args).unwrap();
        let op_resp: serde_json::Value = serde_json::from_str(&op_result).unwrap();

        // Same response shape.
        assert!(op_resp["task"]["task_id"].is_string());
        assert!(op_resp["created"].as_bool().unwrap());

        let events = state
            .heart
            .lock()
            .unwrap()
            .read_stream(&project_id)
            .unwrap();
        let human_task_created = events
            .iter()
            .find(|e| e.family == "task.created" && e.payload.contains(&human.task.task_id))
            .unwrap();
        let op_task_id = op_resp["task"]["task_id"].as_str().unwrap();
        let op_task_created = events
            .iter()
            .find(|e| e.family == "task.created" && e.payload.contains(op_task_id))
            .unwrap();

        // Same families, same payload SHAPE (keys) — actor is deliberately
        // allowed to differ (this op path attributes `agent:baker`).
        assert_eq!(human_task_created.family, op_task_created.family);
        let human_payload: serde_json::Value =
            serde_json::from_str(&human_task_created.payload).unwrap();
        let op_payload: serde_json::Value = serde_json::from_str(&op_task_created.payload).unwrap();
        let mut human_keys: Vec<&String> = human_payload.as_object().unwrap().keys().collect();
        let mut op_keys: Vec<&String> = op_payload.as_object().unwrap().keys().collect();
        human_keys.sort();
        op_keys.sort();
        assert_eq!(
            human_keys, op_keys,
            "task.created payload shape must not drift between the human route and the op path"
        );

        assert_eq!(
            op_task_created.actor, "agent:baker",
            "an op-triggered task.create must attribute agent:baker"
        );
        assert_eq!(human_task_created.actor, "human");

        // Both a status.changed and the full lifecycle read side agree.
        assert!(events
            .iter()
            .any(|e| e.family == "task.status.changed" && e.payload.contains(op_task_id)));
        let listed = crate::tasks::task_list(&state, &project_id);
        assert!(listed.iter().any(|t| t.task_id == op_task_id));
    }

    #[test]
    fn project_list_op_reflects_known_projects() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let project_id = chat_only_project(&state);

        let out = dispatch_baker_op(&state, "project_list", &serde_json::json!({})).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert!(v
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["project_id"] == project_id));
    }

    /// 07d-B Phase 1 item 5: agent-authored memory is only ever authored on
    /// the permanent Baker stream itself.
    #[test]
    fn memory_write_op_authors_a_note_attributed_to_agent_baker_on_the_baker_stream() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let baker_home =
            crate::routes::baker::ensure_permanent_baker(&state, None, None).expect("bootstrap");

        let out = dispatch_baker_op(
            &state,
            "memory_write",
            &serde_json::json!({ "stream_id": baker_home.stream_id, "text": "Baker learned something." }),
        )
        .unwrap();
        let ev: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(ev["family"], "memory.written");
        assert_eq!(ev["actor"], "agent:baker");
    }

    /// The defect this guards: `memory.write`'s own `stream_id` argument is
    /// model-supplied and is NOT the same thing as "the stream the calling
    /// turn runs on" — a Baker turn can still name any other socket it has
    /// seen (e.g. via `socket.list`). Phase 1 item 5 scopes agent-authored
    /// memory to the permanent Baker stream only ("General agent-authored
    /// memory stays 07d-C") — refuse rather than silently authoring a note
    /// on a socket the human never granted agent memory to.
    #[test]
    fn memory_write_op_refuses_a_stream_that_is_not_the_permanent_baker_stream() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        // Bootstraps a permanent Baker so `is_permanent_baker_stream` has a
        // real recorded id to compare against, distinct from this target.
        crate::routes::baker::ensure_permanent_baker(&state, None, None).expect("bootstrap");
        let socket_id = chat_only_project_socket(&state);

        let err = dispatch_baker_op(
            &state,
            "memory_write",
            &serde_json::json!({ "stream_id": socket_id, "text": "should never land" }),
        )
        .unwrap_err();
        assert!(err.contains("permanent Baker"), "{err}");

        // And no `memory.written` fact actually landed on that stream.
        let view = crate::routes::memory::memory_view_inner(&state, &socket_id).unwrap();
        assert!(
            view.entries.is_empty(),
            "the refused write must not have landed: {:?}",
            view.entries
        );
    }

    /// One Baker, one memory (BAKER-THREADS-03): a note a thread authors
    /// about itself lands on the permanent Baker's stream, where every one of
    /// its conversations reads it back.
    #[test]
    fn memory_write_from_a_thread_lands_in_the_one_baker_memory() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let baker = crate::routes::baker::ensure_permanent_baker(&state, None, None)
            .unwrap()
            .stream_id;
        let thread =
            crate::baker_threads::create_thread(&state, &Default::default(), "human", None, None)
                .unwrap()
                .thread
                .stream_id;

        let out = dispatch_baker_op(
            &state,
            "memory_write",
            &serde_json::json!({ "stream_id": thread, "text": "Shared fact." }),
        )
        .unwrap();
        let ev: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(ev["stream_id"], baker);
        let view = crate::routes::memory::memory_view_inner(&state, &baker).unwrap();
        assert!(view.entries.iter().any(|m| m.text == "Shared fact."));
        assert_eq!(
            crate::baker_threads::memory_stream_for(&state, &thread),
            baker
        );
    }

    /// The Baker's project ops call the sidebar's own catalog functions:
    /// rename/pin/remove, and "remove" never touches a byte on disk.
    #[test]
    fn project_ops_rename_pin_and_retire_without_touching_files() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let root = dir.path().join("notes");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("keep.txt"), "keep\n").unwrap();
        let project_id = catalog::create_project(
            &state,
            "Notes",
            Some(catalog::DirectoryRootSpec::Attach(root.to_str().unwrap())),
        )
        .unwrap()
        .0
        .project_id;
        let list = |state: &AppState| -> Vec<serde_json::Value> {
            let out = dispatch_baker_op(state, "project_list", &serde_json::json!({})).unwrap();
            serde_json::from_str::<serde_json::Value>(&out)
                .unwrap()
                .as_array()
                .unwrap()
                .clone()
        };

        tool(
            &state,
            "project_rename",
            &serde_json::json!({ "project_id": project_id, "name": "Field notes" }),
        )
        .unwrap();
        tool(
            &state,
            "project_pin",
            &serde_json::json!({ "project_id": project_id, "pinned": true }),
        )
        .unwrap();
        let row = list(&state)
            .into_iter()
            .find(|p| p["project_id"] == project_id)
            .unwrap();
        assert_eq!(row["name"], "Field notes");
        assert_eq!(row["pinned"], true);

        tool(
            &state,
            "project_retire",
            &serde_json::json!({ "project_id": project_id }),
        )
        .unwrap();
        assert!(!list(&state).iter().any(|p| p["project_id"] == project_id));
        assert_eq!(
            std::fs::read_to_string(root.join("keep.txt")).unwrap(),
            "keep\n",
            "removing a project from Bakery never touches its files"
        );
        assert!(
            tool(
                &state,
                "project_retire",
                &serde_json::json!({ "project_id": project_id }),
            )
            .is_err(),
            "a second remove is refused, not silently repeated"
        );
    }

    #[test]
    fn unknown_op_name_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let err = dispatch_baker_op(&state, "task.explode", &serde_json::json!({})).unwrap_err();
        assert!(err.contains("unknown Bakery op"), "{err}");
    }

    #[test]
    fn baker_op_tool_decls_cover_every_registered_op_exactly_once() {
        let decls = baker_op_tool_decls();
        assert_eq!(decls.len(), BAKER_OPS.len());
        for op in BAKER_OPS {
            assert_eq!(
                decls.iter().filter(|d| d.name == op.wire).count(),
                1,
                "{}",
                op.wire
            );
        }
    }

    /// The defect this guards: a Bakery op's dotted spec `id` (e.g.
    /// `"task.create"`) is not a legal tool name on the Anthropic or
    /// OpenAI-shaped function-calling wire (`^[a-zA-Z0-9_-]{1,64}$` — `.` is
    /// rejected), so an HTTP-transport permanent Baker on a Claude/Grok/GPT
    /// plug would fail its very first turn if `ToolDecl.name` ever carried
    /// `id` instead of `wire`. Every advertised name must match the pattern,
    /// `find_op`/`dispatch_baker_op` must resolve it back to the SAME op
    /// `BAKER_OPS` declares it for, and no two ops may collapse onto the same
    /// wire name.
    #[test]
    fn every_advertised_tool_name_is_wire_safe_and_maps_back_to_its_own_op() {
        let is_wire_safe = |s: &str| {
            !s.is_empty()
                && s.len() <= 64
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        };
        let decls = baker_op_tool_decls();
        assert_eq!(decls.len(), BAKER_OPS.len());
        let mut seen_wire_names = std::collections::HashSet::new();
        for op in BAKER_OPS {
            assert!(
                is_wire_safe(op.wire),
                "op {:?} has a non-wire-safe tool name {:?}",
                op.id,
                op.wire
            );
            assert!(
                !op.wire.contains('.'),
                "op {:?}'s wire name must not carry the dotted id's '.': {:?}",
                op.id,
                op.wire
            );
            assert!(
                seen_wire_names.insert(op.wire),
                "duplicate wire name {:?} (from op id {:?})",
                op.wire,
                op.id
            );
            assert!(
                decls.iter().any(|d| d.name == op.wire),
                "op {:?}'s wire name {:?} is not advertised by baker_op_tool_decls",
                op.id,
                op.wire
            );
            let found = find_op(op.wire).unwrap_or_else(|| {
                panic!(
                    "find_op could not resolve wire name {:?} back to an op",
                    op.wire
                )
            });
            assert_eq!(
                found.id, op.id,
                "wire name {:?} resolved to the wrong op",
                op.wire
            );
            assert!(is_op(op.wire));
        }
        // And the dotted `id` itself must NOT be independently dispatchable —
        // it is prose/documentation only, never a second valid wire name.
        for op in BAKER_OPS {
            if op.id != op.wire {
                assert!(
                    !is_op(op.id),
                    "dotted id {:?} must not itself be dispatchable as a tool name",
                    op.id
                );
            }
        }
    }

    /// 07d-B Phase 2 item 6 ("No prompt-injected hostnames"): `socket.list`
    /// must never leak `StreamRef.owner_node_id` — which, on a single-node
    /// dev box, is exactly `state.node.node_id`, `{hostname-slug}-{hex}`
    /// (`node::mint_node_id`) — into the op result the model reads.
    #[test]
    fn socket_list_op_never_leaks_the_owning_node_id() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        chat_only_project(&state); // claims ownership for this node on its socket

        let node_id = state.node.lock().unwrap().node_id.clone();
        assert!(!node_id.is_empty());

        let out = tool(&state, "socket_list", &serde_json::json!({})).unwrap();
        assert!(
            !out.contains(&node_id),
            "socket.list leaked the owning node_id: {out}"
        );
        assert!(
            !out.contains("owner_node_id"),
            "socket.list must not even carry the field: {out}"
        );

        // Sanity: the projection still carries the ordinary project facts a
        // Baker legitimately needs (it's a real narrowing, not an empty list).
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert!(
            v.as_array().unwrap().iter().any(|s| s["name"] == "Chat P"),
            "{out}"
        );
    }

}
