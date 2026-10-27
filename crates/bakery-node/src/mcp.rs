//! Loopback MCP delivery for ACP transports (07d-B Phase 2 / Stage C).
//!
//! The permanent Baker's ops and a task worker's `task_complete` are
//! delivered to an ACP-piped session over a loopback MCP server
//! (`crate::mcp_shim`, spawned by the downstream ACP CLI itself per
//! `NewSessionRequest::mcp_servers`) — the same tool set and dispatch the
//! HTTP tool loop (`routes::agent::agent_tool_turn`) already gives that
//! scope, never a second implementation.
//!
//! **Identity is server-bound, never caller-supplied** (Phase 2 item 2): a
//! per-session token minted at ACP spawn time
//! ([`mint_mcp_token`]) is the *only* thing [`mcp_call`] trusts to know which
//! stream/seat/scope a call belongs to — the request body's own `name`/
//! `arguments` never carry identity. Tokens live only in
//! `AppState::mcp_tokens` (in-memory), so a node restart clears every token
//! unconditionally ("the next ACP session mints a fresh one," matching the
//! spec) — but a token can otherwise outlive the session it was minted for
//! within one process lifetime: [`evict_mcp_tokens_for_stream`] is called
//! wherever a stream's MCP-carrying session identity is known to be retired
//! (a seat's model reassignment in `state::assign_seat_model`, a stream
//! discard's ACP fence in `state::fence_acp_with_deadline`), but this is a
//! best-effort eviction by `stream_id`, not a guarantee tied to the exact
//! token/process pairing — a token minted for a spawn that fails before
//! going `Live` is not individually tracked and is only cleared by one of
//! those two eviction points (or node restart) rather than immediately.
//!
//! **Scope enforcement is now backed by a real HTTP boundary** (the gap
//! this doc used to record is CLOSED — Checkpoint 1, 2026-09-09): every
//! non-exempt loopback route requires the operator bearer
//! (`operator_auth::require_operator`, layered over the whole router in
//! `main.rs`), and an MCP token presented on an ordinary route is refused
//! with a scope-shaped 403. An ACP child — whose Seatbelt allows network
//! and `/usr/bin` exec on purpose — can reach exactly `/health`,
//! `/mcp/tools`, and `/mcp/call` with the one credential it holds
//! (`BAKERY_MCP_TOKEN`); the operator token never enters any child's env
//! (`env -i` + allowlist, asserted by
//! `state::operator_token_never_appears_in_any_child_spawn_env`) and its
//! on-disk home (`node.json`, 0600) is on `bakery_exec`'s sandbox deny
//! list. Residual, stated honestly: this is credential isolation, not
//! process attestation — anything that DOES hold the operator token
//! (the operator's own unsandboxed processes) is the operator.

use crate::node::Seat;
use crate::state::AppState;
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    Json,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use std::sync::Mutex;

/// Which declared tool set an MCP session's token was minted for — the
/// permanent Baker's op registry, or a task worker's surface (the jailed
/// core tools rooted at its own workspace, [`WORKER_MCP_TOOLS`], plus
/// `task_complete`). Never both: a stream is either `baker_stream_id` or a
/// task socket, never both by construction (07d-B Phase 3 item 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum McpScope {
    BakerOps,
    WorkerTools,
    /// Every other agent: the Bakery's tools within its own project, or its
    /// own conversation without one (`api_tools::within_reach`).
    Agent,
}

/// One minted token's binding — set once at mint time, read (never mutated)
/// on every `/mcp/tools` or `/mcp/call` request that presents it.
#[derive(Debug, Clone)]
pub(crate) struct McpBinding {
    pub(crate) stream_id: String,
    pub(crate) seat: Seat,
    pub(crate) scope: McpScope,
}

pub(crate) type McpTokenRegistry = Mutex<HashMap<String, McpBinding>>;

/// Which scope (if any) an ACP session on `stream_id` should be minted an
/// MCP token for — the permanent Baker stream and its threads get
/// [`McpScope::BakerOps`], a task socket gets [`McpScope::WorkerTools`],
/// every other stream [`McpScope::Agent`], unless its agents were given none
/// of the Bakery's tools.
pub(crate) fn mcp_scope_for_stream(state: &AppState, stream_id: &str) -> Option<McpScope> {
    // The permanent Baker and each of its threads (BAKER-THREADS-03) — one
    // Baker, one op registry, however many conversations it has open.
    if crate::baker_ops::is_baker_scope_stream(state, stream_id) {
        return Some(McpScope::BakerOps);
    }
    let is_task = {
        let heart = state.heart.lock().unwrap();
        crate::state::socket_task_id(&heart, stream_id).is_some()
    };
    if is_task {
        return Some(McpScope::WorkerTools);
    }
    (crate::api_tools::tools_limit(state, stream_id) != crate::api_tools::ToolsLimit::None)
        .then_some(McpScope::Agent)
}

/// Whether this conversation cannot run without Bakery's MCP server: the
/// Baker's and a task worker's can't; any other agent runs without its
/// Bakery tools on a runtime that can't carry MCP.
pub(crate) fn mcp_required(state: &AppState, stream_id: &str) -> bool {
    matches!(
        mcp_scope_for_stream(state, stream_id),
        Some(McpScope::BakerOps | McpScope::WorkerTools)
    )
}

/// Mint a fresh per-session token bound to `(stream_id, seat, scope)`.
/// 32 bytes from `/dev/urandom`, hex-encoded. The token is not derived
/// from the pid, the clock, or the stream id.
pub(crate) fn mint_mcp_token(
    state: &AppState,
    stream_id: &str,
    seat: Seat,
    scope: McpScope,
) -> String {
    use std::io::Read;
    let mut bytes = [0u8; 32];
    let drew = std::fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .is_ok();
    if !drew {
        // urandom is the source. A second read is the only retry. There is
        // no fallback that a sibling could predict.
        let _ = std::fs::File::open("/dev/urandom").and_then(|mut file| file.read_exact(&mut bytes));
    }
    let token: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    state.mcp_tokens.lock().unwrap().insert(
        token.clone(),
        McpBinding {
            stream_id: stream_id.to_string(),
            seat,
            scope,
        },
    );
    token
}

/// Drop every minted token bound to `stream_id`, from whichever seat/scope —
/// called wherever that stream's MCP-carrying session identity is known to
/// be retired (see this module's doc comment for exactly which call sites
/// and the honest limits of this being a by-`stream_id` sweep rather than a
/// single-token teardown). Returns how many were evicted, purely so callers
/// and tests can observe it happened; never itself an error — evicting zero
/// (a stream that never had a token) is the ordinary case for most streams.
pub(crate) fn evict_mcp_tokens_for_stream(state: &AppState, stream_id: &str) -> usize {
    let mut tokens = state.mcp_tokens.lock().unwrap();
    let before = tokens.len();
    tokens.retain(|_, binding| binding.stream_id != stream_id);
    before - tokens.len()
}

/// The jailed core tools a task worker's MCP session may call — the same
/// read/search/exec surface the HTTP tool loop dispatches, rooted at the
/// task's own workspace. Deliberately NOT `edit`/`write_file`: an ACP child
/// edits through its own native tools (its Seatbelt workspace is writable),
/// and a second, proposal-based write path via MCP would race it; the
/// task's baseline Review diff covers every write either way. Not
/// `ask_user`/`todo_write` either — those are turn-lifecycle tools that
/// belong to the harness driving the conversation, not a side channel.
pub(crate) const WORKER_MCP_TOOLS: &[&str] = &["read_file", "glob", "grep", "list_dir", "run"];

/// The declared tool set a token's scope may see — exactly `tools/list`'s
/// answer (never more than what `dispatch_mcp_call` below would actually
/// run for the same scope).
pub(crate) fn tool_decls_for_scope(scope: McpScope) -> Vec<bakery_agent::ToolDecl> {
    match scope {
        McpScope::BakerOps => crate::api_tools::decls(),
        McpScope::Agent => crate::api_tools::agent_decls(crate::api_tools::ToolsLimit::All),
        McpScope::WorkerTools => {
            let mut decls: Vec<bakery_agent::ToolDecl> = crate::tools::http_tool_decls(true, &[])
                .into_iter()
                .filter(|d| WORKER_MCP_TOOLS.contains(&d.name.as_str()))
                .collect();
            decls.push(crate::routes::agent::task_complete_tool_decl());
            decls
        }
    }
}

/// [`tool_decls_for_scope`] for one conversation: a task worker also gets
/// the container tools while its containers switch is on.
pub(crate) fn tool_decls_for(
    state: &AppState,
    stream_id: &str,
    scope: McpScope,
) -> Vec<bakery_agent::ToolDecl> {
    let limit = crate::api_tools::tools_limit(state, stream_id);
    let mut decls = match scope {
        McpScope::Agent => crate::api_tools::agent_decls(limit),
        McpScope::BakerOps => crate::api_tools::baker_decls(limit),
        McpScope::WorkerTools => {
            let mut d = tool_decls_for_scope(scope);
            d.extend(crate::api_tools::agent_decls(limit));
            if crate::machine_access::containers_on(state, stream_id) {
                d.extend(crate::containers::worker_decls());
            }
            d
        }
    };
    decls.extend(crate::toolkit::custom_tool_decls(
        &crate::toolkit::custom_tools(state, stream_id),
    ));
    decls
}

/// Dispatch one MCP `tools/call` for an already-resolved binding — EXACTLY
/// the same functions the HTTP tool loop's own dispatch step
/// (`routes::agent::agent_tool_turn`) would call for this scope:
/// `api_tools::call` for `BakerOps`, `dispatch_task_complete`
/// for `WorkerTools`. A worker token can never reach a Baker op (and vice
/// versa) because the `match` below has no arm that crosses scopes — there
/// is no argument a caller could supply to cross it.
fn dispatch_mcp_call(
    state: &AppState,
    binding: &McpBinding,
    name: &str,
    args: &serde_json::Value,
) -> Result<String, String> {
    // ponytail: streams with native hooks (Claude seats) fire hooks in-process
    // via settings.json; all others get Bakery-side pre/post hooks here
    let fire_hooks = !crate::toolkit::has_native_hooks(&binding.stream_id);
    if fire_hooks {
        if let Some(reason) = crate::toolkit::run_pre_hooks(state, &binding.stream_id, name, args) {
            return Err(format!("hook blocked {name}: {reason}"));
        }
    }
    let result = dispatch_mcp_call_inner(state, binding, name, args);
    if fire_hooks {
        crate::toolkit::run_post_hooks(
            state, &binding.stream_id, name, args,
            result.as_ref().map(|s| s.as_str()).unwrap_or("(error)"),
        );
    }
    result
}

fn dispatch_mcp_call_inner(
    state: &AppState,
    binding: &McpBinding,
    name: &str,
    args: &serde_json::Value,
) -> Result<String, String> {
    let custom = crate::toolkit::custom_tools(state, &binding.stream_id);
    if let Some(ct) = custom.iter().find(|t| t.name == name) {
        return crate::toolkit::dispatch_custom_tool(state, &binding.stream_id, ct, args);
    }
    match binding.scope {
        McpScope::Agent => {
            // Re-checked live, like the arms below: still an agent's.
            if mcp_scope_for_stream(state, &binding.stream_id) != Some(McpScope::Agent) {
                return Err("this MCP session's token is no longer bound to this agent's tools".to_string());
            }
            if !crate::api_tools::is_agent_tool(name) {
                return Err(format!("{name} is not one of your Bakery tools"));
            }
            let caller = crate::api_tools::Caller::mcp(state, &binding.stream_id, binding.seat);
            crate::api_tools::call(state, &caller, name, args)
        }
        McpScope::BakerOps => {
            // Defense in depth, matching `baker_ops::dispatch_baker_op_as`'s
            // own doc comment ("callers MUST have already confirmed..."): the
            // binding was correct when minted, but re-check the LIVE fact
            // rather than trust a token that could outlive a reassignment,
            // a re-minted Baker, or a thread losing membership.
            if !crate::baker_ops::is_baker_scope_stream(state, &binding.stream_id) {
                return Err(
                    "this MCP session's token is no longer bound to a Baker conversation"
                        .to_string(),
                );
            }
            if !crate::api_tools::is_tool(name) {
                return Err(format!("unknown Bakery op {name:?}"));
            }
            let caller = crate::api_tools::Caller::mcp(state, &binding.stream_id, binding.seat);
            crate::api_tools::call(state, &caller, name, args)
        }
        McpScope::WorkerTools => {
            // Defense in depth, mirroring the BakerOps arm above: re-check
            // the LIVE fact (is this still a task socket, and not the
            // permanent Baker?) rather than trust a token minted earlier.
            if mcp_scope_for_stream(state, &binding.stream_id) != Some(McpScope::WorkerTools) {
                return Err(
                    "this MCP session's token is no longer bound to a task socket".to_string(),
                );
            }
            if name == "task_complete" {
                return crate::routes::agent::dispatch_task_complete(
                    state,
                    &binding.stream_id,
                    args,
                );
            }
            if crate::containers::is_worker_tool(name) || crate::api_tools::is_agent_tool(name) {
                // Behind this conversation's containers switch, graded and
                // gated like the Baker's own calls (`containers::agent_may`);
                // its project tools within its reach (`api_tools::within_reach`).
                let caller = crate::api_tools::Caller::mcp(state, &binding.stream_id, binding.seat);
                return crate::api_tools::call(state, &caller, name, args);
            }
            if !WORKER_MCP_TOOLS.contains(&name) {
                return Err(format!(
                    "tool {name:?} is not available on a task worker's MCP session — available: {}, task_complete",
                    WORKER_MCP_TOOLS.join(", ")
                ));
            }
            // The same jailed dispatch the HTTP tool loop uses, rooted at
            // THIS task's recorded workspace — a chat-only task refuses
            // honestly instead of pretending to have files.
            let ctx = crate::state::context_for(state, &binding.stream_id)
                .map_err(|e| format!("cannot resolve this task's workspace: {e}"))?;
            if ctx.worktree_path.is_empty() {
                return Err(format!(
                    "{name} needs a workspace root — this task is chat-only (no materialized workspace)"
                ));
            }
            let toolchain = crate::catalog::stacks_for_socket(state, &binding.stream_id).toolchain;
            let jail = crate::tools::Jail::new(std::path::Path::new(&ctx.worktree_path))?
                .with_exec(
                    state.clone(),
                    binding.stream_id.clone(),
                    crate::state::cache_root(state),
                    toolchain,
                );
            crate::actor::with(format!("agent:{}", binding.seat.as_str()), || {
                crate::tools::dispatch_http_tool(Some(&jail), &[], name, args)
            })
        }
    }
}

/// Decide `handle_mcp_call`'s final return given the op's own dispatch
/// outcome and whether the `agent.tool.called` audit append actually landed.
/// Pulled out as a pure function so the audit-failure branch — which the HTTP
/// tool loop's own sibling append (`routes/agent.rs`) treats as terminal —
/// is directly unit-testable with a synthetic append error; a real `Heart`
/// write cannot be forced to fail without reaching into sqlite internals
/// (07d-B Phase 2 item 3: "Every op is an event … No silent state").
fn mcp_call_final_result(
    dispatch_outcome: Result<String, String>,
    append_outcome: Result<(), String>,
) -> Result<String, String> {
    match append_outcome {
        Ok(()) => dispatch_outcome,
        Err(append_err) => Err(match dispatch_outcome {
            Ok(body) => format!(
                "the op ran (result: {}) but its agent.tool.called audit record could not be appended: {append_err}",
                crate::agent_loop::truncate(&body, crate::agent_loop::MAX_EVENT_SUMMARY)
            ),
            Err(dispatch_err) => format!(
                "the op failed ({dispatch_err}) and its agent.tool.called audit record also could not be appended: {append_err}"
            ),
        }),
    }
}

/// `dispatch_mcp_call`, plus recording the exact same `agent.tool.called`
/// event shape the HTTP tool loop appends for every tool call (07d-B Phase 2
/// item 3: "MCP-served calls are recorded as events exactly like HTTP-path
/// calls") — actor and `agent_id` come from the token's own binding, never
/// from the request body.
///
/// An append failure is surfaced to the MCP caller as an error result
/// ([`mcp_call_final_result`]) rather than silently discarded: the domain
/// event the op itself appended (if any) already landed either way, so a
/// dropped audit append would otherwise tell the caller "succeeded" while
/// leaving no record on the Baker's own stream of who called the tool or
/// with what arguments — exactly the silent state Phase 2 item 3 forbids,
/// and the same failure the HTTP tool loop's identical append already
/// treats as terminal (`routes/agent.rs`'s `agent_tool_turn`).
pub(crate) fn handle_mcp_call(
    state: &AppState,
    token: &str,
    name: &str,
    args: &serde_json::Value,
) -> Result<String, String> {
    let binding = state
        .mcp_tokens
        .lock()
        .unwrap()
        .get(token)
        .cloned()
        .ok_or_else(|| "unknown or expired MCP token".to_string())?;
    let outcome = match dispatch_mcp_call(state, &binding, name, args) {
        // A command that ran and exited non-zero is a failed call: the MCP
        // client gets it as an error result, with the same output.
        Ok(body) if crate::tools::command_failed(name, &body) => Err(body),
        other => other,
    };
    let (ok, body) = match &outcome {
        Ok(s) => (true, s.clone()),
        Err(e) => (false, e.clone()),
    };
    let agent_id = crate::node::agent_id(&binding.stream_id, binding.seat);
    let actor = format!("agent:{}", binding.seat.as_str());
    let payload = serde_json::json!({
        "tool": name,
        "args": args,
        "ok": ok,
        "summary": crate::agent_loop::event_summary(name, &body),
        "agent_id": agent_id,
        "transport": "mcp",
    })
    .to_string();
    let append_outcome = crate::state::append(
        state,
        &binding.stream_id,
        &actor,
        "agent.tool.called",
        payload,
        vec![],
    )
    .map(|_| ());
    mcp_call_final_result(outcome, append_outcome)
}

fn bearer_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(str::to_string)
}

/// Wire shape for one tool in `GET /mcp/tools` — a local, Serialize-able
/// mirror of `bakery_agent::ToolDecl` (which deliberately carries no
/// `Serialize` impl of its own).
#[derive(schemars::JsonSchema, Serialize)]
pub(crate) struct McpToolWire {
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) parameters: serde_json::Value,
}

impl From<bakery_agent::ToolDecl> for McpToolWire {
    fn from(d: bakery_agent::ToolDecl) -> Self {
        Self {
            name: d.name,
            description: d.description,
            parameters: d.parameters,
        }
    }
}

/// `GET /mcp/tools`: the declared tool set for the presented bearer token's
/// scope — exactly what `tools/list` in `crate::mcp_shim` relays verbatim.
pub(crate) async fn mcp_tools(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<McpToolWire>>, (StatusCode, String)> {
    let token = bearer_token(&headers)
        .ok_or((StatusCode::UNAUTHORIZED, "missing bearer token".to_string()))?;
    let binding = state
        .mcp_tokens
        .lock()
        .unwrap()
        .get(&token)
        .cloned()
        .ok_or((
            StatusCode::UNAUTHORIZED,
            "unknown or expired MCP token".to_string(),
        ))?;
    Ok(Json(
        tool_decls_for(&state, &binding.stream_id, binding.scope)
            .into_iter()
            .map(Into::into)
            .collect(),
    ))
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct McpCallReq {
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) arguments: serde_json::Value,
}

#[derive(schemars::JsonSchema, Serialize)]
pub(crate) struct McpCallResp {
    pub(crate) ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) result: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<String>,
    /// A screenshot the call took, base64 PNG, for the shim to hand over as
    /// MCP image content; never part of `result`, which Heart records.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) image_png: Option<String>,
}

/// `POST /mcp/call`: the one dispatch endpoint every MCP `tools/call`
/// forwards to. Never returns an HTTP error for a refused tool call (a scope
/// violation is an ordinary `{ok:false, error:...}` — the MCP shim relays
/// that as an `isError` tool result, not a transport failure); HTTP status
/// is reserved for "the token itself is missing/unknown."
///
/// `handle_mcp_call` runs inside `spawn_blocking` — dispatch can reach
/// `task.start`'s `git worktree add`, exactly the blocking call
/// `tasks::task_start_route` already keeps off the async workers ("same
/// discipline `baker_assign` uses"); this route is the MCP-transport sibling
/// of that same op and must stay off them too, or a single `task.start`
/// (plus any `gates.list`/`socket.list` scan) can stall every other axum
/// worker — `/events`, `/studio/state`, `/agent/turn` admission — for its
/// duration.
pub(crate) async fn mcp_call(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<McpCallReq>,
) -> Result<Json<McpCallResp>, (StatusCode, String)> {
    let token = bearer_token(&headers)
        .ok_or((StatusCode::UNAUTHORIZED, "missing bearer token".to_string()))?;
    if !state.mcp_tokens.lock().unwrap().contains_key(&token) {
        return Err((
            StatusCode::UNAUTHORIZED,
            "unknown or expired MCP token".to_string(),
        ));
    }
    let agent_id = state
        .mcp_tokens
        .lock()
        .unwrap()
        .get(&token)
        .map(|b| crate::node::agent_id(&b.stream_id, b.seat));
    let outcome = tokio::task::spawn_blocking(move || {
        handle_mcp_call(&state, &token, &req.name, &req.arguments)
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let image_png = agent_id
        .and_then(|a| crate::desktop::take_screenshot(&a, None))
        .map(|png| bakery_agent::tool_wire::encode_base64(&png));
    Ok(Json(match outcome {
        Ok(result) => McpCallResp {
            ok: true,
            result: Some(result),
            error: None,
            image_png,
        },
        Err(error) => McpCallResp {
            ok: false,
            result: None,
            error: Some(error),
            image_png: None,
        },
    }))
}

/// Spin up `GET /mcp/tools` / `POST /mcp/call` as a real, listening HTTP
/// server on an ephemeral loopback port, bound to `state`, on its OWN OS
/// thread with its own single-threaded Tokio runtime — matching
/// `pipe::session_main`'s own "each long-lived thing gets its own runtime"
/// shape, so a plain (non-`#[tokio::test]`) test can call this and then make
/// a fully synchronous, blocking call (e.g. `AcpPipeModel::chat`) without
/// starving the server for a poll. Test-only: real ACP sessions reach these
/// same two routes through the daemon's one shared `axum::serve` in
/// `main.rs`, never through a second listener like this one.
#[cfg(test)]
pub(crate) fn spawn_mcp_server_for_test(state: AppState) -> std::net::SocketAddr {
    let (addr_tx, addr_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().expect("mcp test server runtime");
        rt.block_on(async move {
            let app = axum::Router::new()
                .route("/mcp/tools", axum::routing::get(mcp_tools))
                .route("/mcp/call", axum::routing::post(mcp_call))
                .with_state(state);
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind mcp test server");
            let addr = listener.local_addr().expect("mcp test server local_addr");
            addr_tx.send(addr).expect("send mcp test server addr");
            let _ = axum::serve(listener, app).await;
        });
    });
    let addr = addr_rx.recv().expect("receive mcp test server addr");
    crate::testutil::first_connection(addr);
    // Readiness barrier — same load-only flake and fix as
    // `operator_auth::tests::spawn_full_app_for_test`: any HTTP answer
    // (including the expected 401 without a bearer) proves the server is
    // accepting; only transport-level failures retry.
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(2))
        .timeout(std::time::Duration::from_secs(5))
        .build();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    loop {
        match agent.get(&format!("http://{addr}/mcp/tools")).call() {
            Ok(_) | Err(ureq::Error::Status(_, _)) => break,
            Err(_) => {
                assert!(
                    std::time::Instant::now() < deadline,
                    "mcp test server never became reachable"
                );
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
    }
    addr
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;
    use bakery_agent::StubModel;
    use std::sync::Arc;

    #[test]
    fn mcp_scope_for_stream_is_baker_ops_on_the_baker_and_agent_tools_elsewhere_unless_given_none() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let baker_home = crate::routes::baker::ensure_permanent_baker(&state, None, None).unwrap();
        assert_eq!(
            mcp_scope_for_stream(&state, &baker_home.stream_id),
            Some(McpScope::BakerOps)
        );

        let ordinary = crate::catalog::create_project(&state, "P", None)
            .unwrap()
            .1
            .stream_id;
        assert_eq!(mcp_scope_for_stream(&state, &ordinary), Some(McpScope::Agent));
        assert!(!mcp_required(&state, &ordinary), "it runs without them on a runtime that can't carry MCP");
        crate::api_tools::set_tools_limit(&state, &ordinary, crate::api_tools::ToolsLimit::None).unwrap();
        assert_eq!(mcp_scope_for_stream(&state, &ordinary), None);
    }

    #[test]
    fn mcp_scope_for_stream_is_worker_tools_on_a_task_socket() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let project_id = crate::catalog::create_project(&state, "P", None)
            .unwrap()
            .0
            .project_id;
        let created = crate::tasks::task_create(
            &state,
            crate::tasks::TaskCreateReq {
                parent_project_id: project_id.clone(),
                title: "T".into(),
                work_order: "wo".into(),
                contract: Default::default(),
            },
        )
        .unwrap();
        crate::tasks::task_start(
            &state,
            crate::tasks::TaskStartReq {
                project_id,
                task_id: created.task.task_id.clone(),
            },
        )
        .unwrap();
        assert_eq!(
            mcp_scope_for_stream(&state, &created.task.stream_id),
            Some(McpScope::WorkerTools)
        );
    }

    /// A session token is 32 random bytes, hex-encoded. Two mints differ, and
    /// the stream id is not mixed into the token.
    #[test]
    fn mcp_session_tokens_are_32_random_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let a = mint_mcp_token(&state, "wt:a", Seat::Orchestrator, McpScope::Agent);
        let b = mint_mcp_token(&state, "wt:a", Seat::Orchestrator, McpScope::Agent);
        assert_eq!(a.len(), 64, "{a}");
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()), "{a}");
        assert_ne!(a, b);
        assert!(!a.contains("wt:a") && !b.contains("wt:a"));
    }

    /// Headline correctness proof: a Baker-scoped token can call a Baker op
    /// end to end, and the exact same `agent.tool.called` event shape the
    /// HTTP tool loop appends lands on the stream, attributed to
    /// `agent:baker`.
    #[test]
    fn baker_scoped_token_calls_task_create_and_records_agent_tool_called() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let baker_home = crate::routes::baker::ensure_permanent_baker(&state, None, None).unwrap();
        // Creating a task asks the operator in every mode: they allow it.
        crate::agent_control::set_operating_mode(&state, &baker_home.stream_id, Seat::Baker, crate::agent_control::OperatingMode::Auto).unwrap();
        let project_id = crate::catalog::create_project(&state, "Target", None)
            .unwrap()
            .0
            .project_id;

        let token = mint_mcp_token(
            &state,
            &baker_home.stream_id,
            Seat::Baker,
            McpScope::BakerOps,
        );
        let args = serde_json::json!({"parent_project_id": project_id, "title": "via mcp", "work_order": "wo-mcp"});
        let result = allowing(&state, &baker_home.stream_id, "task_create", || handle_mcp_call(&state, &token, "task_create", &args)).unwrap();
        let v: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert!(v["created"].as_bool().unwrap());

        let events = state
            .heart
            .lock()
            .unwrap()
            .read_stream(&baker_home.stream_id)
            .unwrap();
        let called = events
            .iter()
            .find(|e| e.family == "agent.tool.called")
            .unwrap();
        assert_eq!(called.actor, "agent:baker");
        let p: serde_json::Value = serde_json::from_str(&called.payload).unwrap();
        assert_eq!(p["tool"], "task_create");
        assert_eq!(p["transport"], "mcp");
        assert!(p["ok"].as_bool().unwrap());
    }

    /// A worker-scoped token must never dispatch a Baker-only op — the exact
    /// Checkpoint-1-adjacent refusal the spec's e2e proof requires.
    #[test]
    fn worker_scoped_token_is_refused_a_baker_only_op() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let project_id = crate::catalog::create_project(&state, "P", None)
            .unwrap()
            .0
            .project_id;
        let created = crate::tasks::task_create(
            &state,
            crate::tasks::TaskCreateReq {
                parent_project_id: project_id.clone(),
                title: "T".into(),
                work_order: "wo".into(),
                contract: Default::default(),
            },
        )
        .unwrap();
        crate::tasks::task_start(
            &state,
            crate::tasks::TaskStartReq {
                project_id: project_id.clone(),
                task_id: created.task.task_id.clone(),
            },
        )
        .unwrap();
        let token = mint_mcp_token(
            &state,
            &created.task.stream_id,
            Seat::Orchestrator,
            McpScope::WorkerTools,
        );

        let err =
            handle_mcp_call(&state, &token, "task_retire", &serde_json::json!({})).unwrap_err();
        assert!(err.contains("not available"), "{err}");

        // task_complete still works for the same token — the refusal above
        // is scope-specific, not a broken token.
        let ok = handle_mcp_call(
            &state,
            &token,
            "task_complete",
            &serde_json::json!({"summary": "done"}),
        )
        .unwrap();
        assert_eq!(ok, "done");
    }

    /// A Baker-scoped token must never dispatch the worker-only
    /// `task_complete` tool (there is no task to complete on the Baker's own
    /// stream in the first place, but the refusal must be scope-shaped, not
    /// an incidental "not a task socket" error from deeper in the stack).
    #[test]
    fn baker_scoped_token_is_refused_task_complete() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let baker_home = crate::routes::baker::ensure_permanent_baker(&state, None, None).unwrap();
        let token = mint_mcp_token(
            &state,
            &baker_home.stream_id,
            Seat::Baker,
            McpScope::BakerOps,
        );

        let err = handle_mcp_call(
            &state,
            &token,
            "task_complete",
            &serde_json::json!({"summary": "nope"}),
        )
        .unwrap_err();
        assert!(err.contains("unknown Bakery op"), "{err}");
    }

    /// BAKER-THREADS-03: a Baker thread's ACP session is minted the same
    /// BakerOps scope, dispatches with its OWN stream as the caller (so a
    /// thread queueing a message onto itself is refused), and the live
    /// re-check drops the token the moment the thread stops being a member.
    #[test]
    fn a_baker_threads_token_is_baker_scoped_caller_aware_and_rechecked_live() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        crate::routes::baker::ensure_permanent_baker(&state, None, None).unwrap();
        let thread =
            crate::baker_threads::create_thread(&state, &Default::default(), "human", None, None)
                .unwrap()
                .thread
                .stream_id;
        assert_eq!(
            mcp_scope_for_stream(&state, &thread),
            Some(McpScope::BakerOps)
        );
        let token = mint_mcp_token(&state, &thread, Seat::Baker, McpScope::BakerOps);
        // These ops act; Auto runs them without an approval card.
        crate::agent_control::set_operating_mode(&state, &thread, Seat::Baker, crate::agent_control::OperatingMode::Auto).unwrap();

        let listed =
            handle_mcp_call(&state, &token, "thread_list", &serde_json::json!({})).unwrap();
        assert!(listed.contains(&thread), "{listed}");

        let err = handle_mcp_call(
            &state,
            &token,
            "thread_message",
            &serde_json::json!({"stream_id": thread, "text": "again"}),
        )
        .unwrap_err();
        assert!(err.contains("this conversation"), "{err}");

        crate::state::append(
            &state,
            &thread,
            "human",
            crate::node::SOCKET_KIND_FAMILY,
            serde_json::to_string(&crate::node::SocketKindRecord {
                kind: crate::node::SocketKind::Project,
            })
            .unwrap(),
            vec![],
        )
        .unwrap();
        let err =
            handle_mcp_call(&state, &token, "thread_list", &serde_json::json!({})).unwrap_err();
        assert!(
            err.contains("no longer bound to a Baker conversation"),
            "{err}"
        );
    }

    /// A bogus/unknown token is refused outright — no binding to fall back to.
    #[test]
    fn bogus_token_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let err = handle_mcp_call(
            &state,
            "not-a-real-token",
            "task_create",
            &serde_json::json!({}),
        )
        .unwrap_err();
        assert!(err.contains("unknown or expired"), "{err}");
    }

    /// The defect this guards: a dropped `agent.tool.called` append used to
    /// be silently discarded (`let _ = crate::state::append(...)`), so a
    /// caller was told "succeeded" with zero audit record ever landing. A
    /// real `Heart` write cannot be forced to fail without reaching into
    /// sqlite internals, so this drives the pulled-out decision function
    /// directly with a synthetic append error — the successful-dispatch
    /// case, proving the op's own result is not silently swallowed either.
    #[test]
    fn a_successful_op_whose_audit_append_fails_surfaces_the_append_failure_not_a_silent_success() {
        let result = mcp_call_final_result(
            Ok("op result body".to_string()),
            Err("disk full".to_string()),
        );
        let err = result.unwrap_err();
        assert!(err.contains("disk full"), "{err}");
        assert!(
            err.contains("op result body"),
            "the real dispatch result must still be visible: {err}"
        );
        assert!(err.contains("could not be appended"), "{err}");
    }

    /// Same defect, the already-failed-dispatch case: the append failure
    /// must not silently hide behind (or overwrite) the op's own error.
    #[test]
    fn a_failed_op_whose_audit_append_also_fails_reports_both_failures() {
        let result = mcp_call_final_result(
            Err("op refused: bad args".to_string()),
            Err("disk full".to_string()),
        );
        let err = result.unwrap_err();
        assert!(err.contains("op refused: bad args"), "{err}");
        assert!(err.contains("disk full"), "{err}");
    }

    /// The ordinary path — audit append succeeds — must be a complete
    /// pass-through with no wrapping, regression-proofing the common case.
    #[test]
    fn a_successful_audit_append_passes_the_dispatch_outcome_through_unchanged() {
        assert_eq!(
            mcp_call_final_result(Ok("body".to_string()), Ok(())),
            Ok("body".to_string())
        );
        assert_eq!(
            mcp_call_final_result(Err("nope".to_string()), Ok(())),
            Err("nope".to_string())
        );
    }

    #[test]
    fn tool_decls_for_scope_matches_each_scopes_real_dispatchable_set() {
        let baker_decls = tool_decls_for_scope(McpScope::BakerOps);
        // Every op, the menu routes, and api_find/api_call for every other route.
        assert_eq!(baker_decls.len(), crate::api_tools::decls().len());
        let menu = crate::api_catalog::tools().iter().filter(|t| t.menu).count();
        assert_eq!(baker_decls.len(), crate::baker_ops::BAKER_OPS.len() + menu + 2);
        let worker_decls = tool_decls_for_scope(McpScope::WorkerTools);
        let names: Vec<&str> = worker_decls.iter().map(|d| d.name.as_str()).collect();
        for want in WORKER_MCP_TOOLS {
            assert!(
                names.contains(want),
                "worker scope must advertise {want}: {names:?}"
            );
        }
        assert!(names.contains(&"task_complete"));
        assert_eq!(worker_decls.len(), WORKER_MCP_TOOLS.len() + 1, "{names:?}");
        for never in [
            "edit",
            "write_file",
            "ask_user",
            "todo_write",
            "task_create",
            "seat_assign",
        ] {
            assert!(
                !names.contains(&never),
                "worker scope must not advertise {never}"
            );
        }
    }

    /// The capability that used to be missing: a worker token can read and
    /// search its OWN task workspace through the jailed core tools — and the
    /// jail still refuses an escape. One tool surface, contained at the
    /// boundary, instead of a single-tool scope pretending to be isolation.
    #[test]
    fn worker_scoped_token_reads_its_own_workspace_and_is_jailed_to_it() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let repo = make_repo(&dir.path().join("repo"));
        std::fs::write(dir.path().join("outside-secret.txt"), "outside\n").unwrap();
        let created = crate::catalog::create_project(
            &state,
            "P",
            Some(crate::catalog::DirectoryRootSpec::Attach(
                &repo.to_string_lossy(),
            )),
        )
        .unwrap();
        let project_id = created.0.project_id;
        let task = crate::tasks::task_create(
            &state,
            crate::tasks::TaskCreateReq {
                parent_project_id: project_id.clone(),
                title: "T".into(),
                work_order: "wo".into(),
                contract: Default::default(),
            },
        )
        .unwrap();
        crate::tasks::task_start(
            &state,
            crate::tasks::TaskStartReq {
                project_id: project_id.clone(),
                task_id: task.task.task_id.clone(),
            },
        )
        .unwrap();
        let token = mint_mcp_token(
            &state,
            &task.task.stream_id,
            Seat::Orchestrator,
            McpScope::WorkerTools,
        );

        // The task worktree contains the repo's files — read one.
        let listed = handle_mcp_call(
            &state,
            &token,
            "list_dir",
            &serde_json::json!({"path": "."}),
        )
        .unwrap();
        assert!(!listed.is_empty());
        // Escapes are refused by the jail, not by tool absence.
        let err = handle_mcp_call(
            &state,
            &token,
            "read_file",
            &serde_json::json!({"path": "../../outside-secret.txt"}),
        )
        .unwrap_err();
        assert!(err.contains("escapes"), "{err}");
    }

    /// A task worker sees and reaches the container tools only while its
    /// conversation's containers switch is on, and then under the same rules
    /// as the Baker: compose and the seat image stay the operator's, and a
    /// mount reaches only what the worker reaches.
    #[test]
    fn a_task_worker_gets_container_tools_only_behind_its_switch() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let repo = make_repo(&dir.path().join("repo"));
        let created = crate::catalog::create_project(
            &state,
            "P",
            Some(crate::catalog::DirectoryRootSpec::Attach(
                &repo.to_string_lossy(),
            )),
        )
        .unwrap();
        let project_id = created.0.project_id;
        let task = crate::tasks::task_create(
            &state,
            crate::tasks::TaskCreateReq {
                parent_project_id: project_id.clone(),
                title: "T".into(),
                work_order: "wo".into(),
                contract: Default::default(),
            },
        )
        .unwrap();
        crate::tasks::task_start(
            &state,
            crate::tasks::TaskStartReq {
                project_id,
                task_id: task.task.task_id.clone(),
            },
        )
        .unwrap();
        let stream = task.task.stream_id.clone();
        let token = mint_mcp_token(&state, &stream, Seat::Orchestrator, McpScope::WorkerTools);
        let names = || -> Vec<String> {
            tool_decls_for(&state, &stream, McpScope::WorkerTools)
                .into_iter()
                .map(|d| d.name)
                .collect()
        };
        let outside = tempfile::tempdir().unwrap();
        let run = serde_json::json!({
            "backend": "apple",
            "image": "alpine",
            "mounts": [{ "source": outside.path().display().to_string(), "target": "/w" }],
        });

        assert!(
            !names().iter().any(|n| n.starts_with("containers_")),
            "{:?}",
            names()
        );
        let err = handle_mcp_call(&state, &token, "containers_run", &run).unwrap_err();
        assert!(err.contains("containers are off"), "{err}");

        state
            .node
            .lock()
            .unwrap()
            .machine_access
            .containers
            .insert(stream.clone());
        let on = names();
        assert!(
            on.iter().any(|n| n == "containers_run") && on.iter().any(|n| n == "containers_list"),
            "{on:?}"
        );
        for never in [
            "containers_compose_up",
            "containers_seat_image",
            "seat_image_build",
        ] {
            assert!(!on.iter().any(|n| n == never), "{never}: {on:?}");
        }
        let err = handle_mcp_call(&state, &token, "containers_run", &run).unwrap_err();
        assert!(err.contains("can't be mounted"), "{err}");
        let err = handle_mcp_call(
            &state,
            &token,
            "containers_compose_up",
            &serde_json::json!({"file": "/x"}),
        )
        .unwrap_err();
        assert!(err.contains("not available"), "{err}");
    }

    /// A worker token on a stream that is not (or is no longer) a task
    /// socket is refused at call time — the live fact wins over the minted
    /// binding, mirroring the BakerOps re-check.
    #[test]
    fn worker_scoped_token_is_refused_when_its_stream_is_not_a_task_socket() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        // A worker-scoped token whose stream carries no task fact (a plain
        // stream, or one whose socket identity was retired underneath the
        // token): the live re-check refuses regardless of the binding.
        let token = mint_mcp_token(
            &state,
            "wt:plain",
            Seat::Orchestrator,
            McpScope::WorkerTools,
        );
        let err = handle_mcp_call(
            &state,
            &token,
            "list_dir",
            &serde_json::json!({"path": "."}),
        )
        .unwrap_err();
        assert!(err.contains("no longer bound to a task socket"), "{err}");
    }

    // ─── HTTP route round-trip (in-process axum server) ────────────────────

    /// `GET /mcp/tools` / `POST /mcp/call` over a real HTTP round trip
    /// (`crate::mcp_shim`'s own tests cover the JSON-RPC framing on top of
    /// this same pair of routes) — proves the routes themselves, header
    /// parsing included, not just the dispatch functions they wrap.
    ///
    /// Deliberately a plain `#[test]`, not `#[tokio::test]`: `spawn_mcp_server_for_test`
    /// runs the server on its OWN OS thread/runtime (see its doc comment) so
    /// this test's fully synchronous, blocking `ureq` calls below cannot
    /// starve it the way they would on a single-threaded `#[tokio::test]`
    /// runtime sharing one thread with a `tokio::spawn`-ed server task.
    #[test]
    fn mcp_tools_and_mcp_call_routes_round_trip_over_real_http() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let baker_home = crate::routes::baker::ensure_permanent_baker(&state, None, None).unwrap();
        // These ops act; Auto runs them without an approval card.
        crate::agent_control::set_operating_mode(&state, &baker_home.stream_id, Seat::Baker, crate::agent_control::OperatingMode::Auto).unwrap();
        let project_id = crate::catalog::create_project(&state, "Target", None)
            .unwrap()
            .0
            .project_id;
        let token = mint_mcp_token(
            &state,
            &baker_home.stream_id,
            Seat::Baker,
            McpScope::BakerOps,
        );
        let addr = spawn_mcp_server_for_test(state.clone());

        let tools: Vec<McpToolWireForTest> = ureq::get(&format!("http://{addr}/mcp/tools"))
            .set("Authorization", &format!("Bearer {token}"))
            .call()
            .unwrap()
            .into_json()
            .unwrap();
        assert!(tools.iter().any(|t| t.name == "task_create"));

        // Creating a task asks the operator in every mode: they allow it.
        let resp: serde_json::Value = allowing(&state, &baker_home.stream_id, "task_create", || {
            ureq::post(&format!("http://{addr}/mcp/call"))
                .set("Authorization", &format!("Bearer {token}"))
                .send_json(serde_json::json!({
                    "name": "task_create",
                    "arguments": {"parent_project_id": project_id, "title": "via http", "work_order": "wo-http"},
                }))
                .unwrap()
                .into_json()
                .unwrap()
        });
        assert!(resp["ok"].as_bool().unwrap(), "{resp}");

        // A refused dispatch must still round-trip as an ordinary
        // `{ok:false, error:...}` body (HTTP 200) once `mcp_call` runs the
        // dispatch inside `spawn_blocking` — proves the blocking wrapper
        // doesn't turn an application-level error into a transport failure
        // or a panic.
        let refused: serde_json::Value = ureq::post(&format!("http://{addr}/mcp/call"))
            .set("Authorization", &format!("Bearer {token}"))
            .send_json(serde_json::json!({ "name": "task_explode", "arguments": {} }))
            .unwrap()
            .into_json()
            .unwrap();
        assert_eq!(refused["ok"].as_bool(), Some(false), "{refused}");
        assert!(
            refused["error"]
                .as_str()
                .unwrap()
                .contains("unknown Bakery op"),
            "{refused}"
        );

        // No bearer token at all → 401, not a panic or a silent empty list.
        let err = ureq::get(&format!("http://{addr}/mcp/tools"))
            .call()
            .unwrap_err();
        match err {
            ureq::Error::Status(code, _) => assert_eq!(code, 401),
            other => panic!("expected an HTTP 401, got {other:?}"),
        }
    }

    #[derive(Deserialize)]
    struct McpToolWireForTest {
        name: String,
    }
}
