//! The outside window: the operator's own read-only view into the Bakery,
//! for a session outside it (their own Claude Code, say) to diagnose and
//! improve it from.
//!
//! - One token, made in Settings → Developer and shown once. The node keeps
//!   only its blake3 hash, in `~/.bakery/outside-window.json`, as `devices`
//!   does; revoking drops it, and the token stops working at once.
//! - It opens `POST /mcp` on the node's own loopback port and nothing else
//!   (`operator_auth`): the same JSON-RPC the seats' `/mcp` speaks
//!   (`mcp_shim::handle_request`), answered here. Every room's profile
//!   closes that port (`doors::room_env`) and no container reaches it; a
//!   door, the seats' gateway and `/mcp/call` know only session tokens.
//! - Its tools read what the Baker reads (`actor::Caller::outside_window`),
//!   on no thread of its own, and write nothing: a fixed list of routes
//!   graded read, run as the Baker's tools run them, and `thread_events`.

use crate::api_catalog::Risk;
use crate::state::{append, AppState};
use axum::{
    extract::State,
    http::{header::AUTHORIZATION, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// Who the window's reads are made as (`actor::Caller::of`).
pub(crate) const ACTOR: &str = "outside-window";
const STREAM: &str = "sys:outside-window";
const FILE: &str = "outside-window.json";
const PREFIX: &str = "bkw_";

/// What it may call, in the order Settings shows them. Every one but
/// [`EVENTS`] is a catalog route graded read.
const TOOLS: &[&str] = &[
    "thread_board",
    "thread_read",
    EVENTS,
    "backlog_list",
    "memory_read",
    "memory_search",
    "memory_get",
];
const EVENTS: &str = "thread_events";

/// `thread_events`' families, each the Heart families it gathers.
const FAMILIES: &[(&str, &[&str])] = &[
    (
        "turns",
        &[
            "agent.turn.started",
            "agent.turn.done",
            "agent.turn.usage",
            "agent.turn.cancelled",
            "agent.turn.exhausted",
            "agent.turn.retrying",
            "agent.operating_mode.set",
            "seat.model.assigned",
        ],
    ),
    ("sessions", &["agent.session.opened", "agent.session.not_resumed", "agent.session.chosen"]),
    ("compactions", &["agent.context.compacted", "agent.context.compaction_size"]),
    // The quiet messages between conversations, which start no turn.
    ("notes", &["message.sent", "message.received", "message.read", "message.taken_up", "message.replaced"]),
    // What starts or steers a turn: a message, a steer, a finished thread
    // telling its opener, the queue paused or resumed.
    (
        "wakes",
        &[
            "agent.follow_up.queued",
            "agent.steer.queued",
            "agent.steer.applied",
            "agent.queue.merged",
            "agent.dispatch.paused",
            "agent.dispatch.resumed",
            "thread.finished",
        ],
    ),
    (
        "errors",
        &["agent.turn.failed", "agent.queue.failed", "agent.edit.failed", "agent.snapshot.failed", "seat.model.switch_failed"],
    ),
];

/// A string in an event's payload longer than this is cut.
const CLIP: usize = 600;
/// About this many bytes of events in one answer.
const BUDGET: usize = 60 * 1024;

// ─── The token ──────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
struct Kept {
    created_at: String,
    token_hash: String,
}

/// One change to the token at a time.
fn lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(Default::default)
}

fn load(state: &AppState) -> Option<Kept> {
    let text = std::fs::read_to_string(state.base.join(FILE)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Written whole, 0600, then renamed into place.
fn save(state: &AppState, kept: &Kept) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let text = serde_json::to_string_pretty(kept).map_err(|e| e.to_string())?;
    let tmp = state.base.join(format!("{FILE}.{}", crate::node::mint_id("tmp")));
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)
        .and_then(|mut f| f.write_all(text.as_bytes()))
        .and_then(|_| std::fs::rename(&tmp, state.base.join(FILE)))
        .map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("could not save the read-only token: {e}")
        })
}

/// Each token's last call since this node started, by its hash.
fn last_used() -> &'static Mutex<HashMap<String, String>> {
    static USED: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    USED.get_or_init(Default::default)
}

/// The current token's hash, when `token` is it.
fn admitted(state: &AppState, token: &str) -> Option<String> {
    let kept = load(state)?;
    // blake3::Hash compares in constant time.
    let held = blake3::Hash::from_hex(&kept.token_hash).ok()?;
    (token.starts_with(PREFIX) && held == blake3::hash(token.as_bytes())).then_some(kept.token_hash)
}

/// Whether `token` is the window's current token.
pub(crate) fn admits(state: &AppState, token: &str) -> bool {
    admitted(state, token).is_some()
}

#[derive(schemars::JsonSchema, Serialize)]
pub(crate) struct OutsideWindow {
    /// When the current token was made; none while there is none.
    pub(crate) created_at: Option<String>,
    /// Its last call since this node started.
    pub(crate) last_used: Option<String>,
    /// Where an outside session reaches it.
    pub(crate) url: String,
    /// What it may call.
    pub(crate) tools: Vec<String>,
}

#[derive(schemars::JsonSchema, Serialize)]
pub(crate) struct Created {
    /// The token, shown this once: the node keeps only its hash.
    pub(crate) token: String,
    pub(crate) window: OutsideWindow,
}

fn status(state: &AppState) -> OutsideWindow {
    let kept = load(state);
    let last_used = kept.as_ref().and_then(|k| last_used().lock().unwrap().get(&k.token_hash).cloned());
    let addr = std::env::var("BAKERY_NODE_ADDR").unwrap_or_else(|_| "127.0.0.1:7717".to_string());
    OutsideWindow {
        created_at: kept.map(|k| k.created_at),
        last_used,
        url: format!("http://{addr}/mcp"),
        tools: TOOLS.iter().map(|t| t.to_string()).collect(),
    }
}

fn create(state: &AppState) -> Result<Created, (StatusCode, String)> {
    let token = {
        let _one = lock().lock().unwrap();
        if load(state).is_some() {
            return Err((StatusCode::CONFLICT, "revoke the current read-only token first".to_string()));
        }
        use std::io::Read;
        let mut bytes = [0u8; 32];
        std::fs::File::open("/dev/urandom")
            .and_then(|mut f| f.read_exact(&mut bytes))
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("/dev/urandom: {e}")))?;
        let token = format!("{PREFIX}{}", bytes.iter().map(|b| format!("{b:02x}")).collect::<String>());
        let kept = Kept {
            created_at: chrono::Utc::now().to_rfc3339(),
            token_hash: blake3::hash(token.as_bytes()).to_hex().to_string(),
        };
        save(state, &kept).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
        token
    };
    let _ = append(state, STREAM, "human", "outside_window.created", "{}".to_string(), vec![]);
    Ok(Created { token, window: status(state) })
}

fn revoke(state: &AppState) -> Result<OutsideWindow, String> {
    {
        let _one = lock().lock().unwrap();
        match std::fs::remove_file(state.base.join(FILE)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                return Err(format!("could not revoke the read-only token: {e}"))
            }
            _ => {}
        }
    }
    let _ = append(state, STREAM, "human", "outside_window.revoked", "{}".to_string(), vec![]);
    Ok(status(state))
}

pub(crate) async fn status_route(State(st): State<AppState>) -> Result<Json<OutsideWindow>, (StatusCode, String)> {
    crate::actor::spawn_blocking(move || status(&st))
        .await
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

pub(crate) async fn create_route(State(st): State<AppState>) -> Result<Json<Created>, (StatusCode, String)> {
    crate::actor::spawn_blocking(move || create(&st))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map(Json)
}

pub(crate) async fn revoke_route(State(st): State<AppState>) -> Result<Json<OutsideWindow>, (StatusCode, String)> {
    crate::actor::spawn_blocking(move || revoke(&st))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

// ─── The tools ──────────────────────────────────────────────────────────────

fn decls() -> Vec<bakery_agent::ToolDecl> {
    TOOLS
        .iter()
        .filter_map(|name| {
            if *name == EVENTS {
                return Some(events_decl());
            }
            let tool = crate::api_catalog::find(name)?;
            let mut input = tool.input.clone();
            // The Baker's own board, every thread, unless it names another.
            if *name == "thread_board" {
                if let Some(required) = input.get_mut("required").and_then(Value::as_array_mut) {
                    required.retain(|k| k != "stream_id");
                }
                input["properties"]["stream_id"]["description"] =
                    json!("The conversation whose threads it shows; the Baker's, every thread, when left out.");
            }
            Some(bakery_agent::ToolDecl { name: tool.name.clone(), description: tool.summary.to_string(), parameters: input })
        })
        .collect()
}

fn events_decl() -> bakery_agent::ToolDecl {
    let families: Vec<&str> = FAMILIES.iter().map(|(name, _)| *name).collect();
    bakery_agent::ToolDecl {
        name: EVENTS.to_string(),
        description: "One thread's events by family, oldest first: turns (model, effort, mode, token use), sessions \
                      (opened, resumed or fresh), compactions, notes (quiet messages between conversations), wakes \
                      (what starts or steers a turn) and errors. The newest without since; the first after since \
                      (a seq) with it."
            .to_string(),
        parameters: json!({
            "type": "object",
            "properties": {
                "stream_id": {"type": "string", "description": "The thread, by its stream_id."},
                "families": {"type": "array", "items": {"type": "string", "enum": families}, "description": "Only these; every family when absent."},
                "since": {"type": "integer", "description": "Only events after this seq."},
                "limit": {"type": "integer", "description": "At most this many events; 50 when absent, at most 500."},
            },
            "required": ["stream_id"],
        }),
    }
}

/// Runs one of the window's tools; nothing else, whatever it is asked.
fn call(state: &AppState, name: &str, args: &Value) -> Result<String, String> {
    if name == EVENTS {
        return thread_events(state, args);
    }
    if !TOOLS.contains(&name) {
        return Err(format!("{name} is not one of the outside window's tools: {}", TOOLS.join(", ")));
    }
    let tool = crate::api_catalog::find(name)
        .filter(|t| t.risk == Risk::Read)
        .ok_or_else(|| format!("{name} is not a read"))?;
    let mut args = if args.is_null() { json!({}) } else { args.clone() };
    if let Some(obj) = args.as_object_mut() {
        // Who acted is the window, never an argument.
        obj.remove("actor");
        if name == "thread_board" && obj.get("stream_id").and_then(Value::as_str).is_none_or(str::is_empty) {
            let baker = state.node.lock().unwrap().baker_stream_id.clone().ok_or("there is no Baker yet")?;
            obj.insert("stream_id".into(), json!(baker));
        }
    }
    crate::api_tools::call_route(state, ACTOR.to_string(), tool, args)
}

fn thread_events(state: &AppState, args: &Value) -> Result<String, String> {
    let stream_id = args["stream_id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or("thread_events needs \"stream_id\"")?;
    let names: Vec<&str> = match args["families"].as_array() {
        Some(asked) if !asked.is_empty() => asked.iter().map(|v| v.as_str().unwrap_or_default()).collect(),
        _ => FAMILIES.iter().map(|(name, _)| *name).collect(),
    };
    let mut families = Vec::new();
    for name in names {
        let (_, of) = FAMILIES.iter().find(|(family, _)| *family == name).ok_or_else(|| {
            let known: Vec<&str> = FAMILIES.iter().map(|(family, _)| *family).collect();
            format!("no family {name:?}: {}", known.join(", "))
        })?;
        families.extend_from_slice(of);
    }
    let since = args["since"].as_u64();
    let limit = args["limit"].as_u64().map_or(50, |n| n.clamp(1, 500) as usize);
    let events = state
        .heart
        .lock()
        .unwrap()
        .events_of_families(stream_id, &families, since, limit)
        .map_err(|e| e.to_string())?;
    let mut rows: Vec<Value> = events
        .into_iter()
        .map(|e| {
            let family = FAMILIES.iter().find(|(_, of)| of.contains(&e.family.as_str())).map(|(name, _)| *name);
            let mut payload = clip(serde_json::from_str(&e.payload).unwrap_or(Value::String(e.payload)));
            crate::api_tools::scrub(&mut payload);
            json!({"seq": e.seq, "ts": e.ts, "family": family, "event": e.family, "actor": e.actor, "payload": payload})
        })
        .collect();
    // Within the budget: the newest without since, the first after it.
    if since.is_none() {
        rows.reverse();
    }
    let mut spent = 0;
    let fits = rows
        .iter()
        .take_while(|row| {
            spent += row.to_string().len();
            spent <= BUDGET
        })
        .count()
        .max(1);
    let cut = fits < rows.len();
    rows.truncate(fits);
    if since.is_none() {
        rows.reverse();
    }
    Ok(json!({"stream_id": stream_id, "events": rows, "cut": cut}).to_string())
}

fn clip(value: Value) -> Value {
    match value {
        Value::String(s) if s.chars().count() > CLIP => Value::String(format!("{}…", s.chars().take(CLIP).collect::<String>())),
        Value::Array(items) => Value::Array(items.into_iter().map(clip).collect()),
        Value::Object(map) => Value::Object(map.into_iter().map(|(k, v)| (k, clip(v))).collect()),
        other => other,
    }
}

/// The node, as an MCP client sees it through the window.
struct Window(AppState);

impl crate::mcp_shim::NodeClient for Window {
    fn list_tools(&self) -> Result<Vec<crate::mcp_shim::ToolInfo>, String> {
        Ok(decls()
            .into_iter()
            .map(|d| crate::mcp_shim::ToolInfo { name: d.name, description: d.description, input_schema: d.parameters })
            .collect())
    }

    fn call_tool(&self, name: &str, arguments: &Value) -> Result<crate::mcp_shim::CallResult, String> {
        let (ok, text) = match call(&self.0, name, arguments) {
            Ok(text) => (true, text),
            Err(e) => (false, e),
        };
        Ok(crate::mcp_shim::CallResult { ok, text, image_png: None })
    }
}

/// `POST /mcp`: one JSON-RPC message, for the window's token only.
pub(crate) async fn mcp_route(State(st): State<AppState>, headers: HeaderMap, body: String) -> Response {
    let token = headers
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|v| v.trim().to_string())
        .unwrap_or_default();
    let Some(hash) = admitted(&st, &token) else {
        return (StatusCode::UNAUTHORIZED, "unknown or revoked read-only token").into_response();
    };
    let Ok(request) = serde_json::from_str::<Value>(&body) else {
        return (StatusCode::BAD_REQUEST, "not JSON").into_response();
    };
    last_used().lock().unwrap().insert(hash, chrono::Utc::now().to_rfc3339());
    match tokio::task::spawn_blocking(move || crate::mcp_shim::handle_request(&Window(st), &request)).await {
        Ok(Some(answer)) => Json(answer).into_response(),
        // A notification gets no answer.
        Ok(None) => StatusCode::ACCEPTED.into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;
    use axum::body::Body;
    use axum::http::Request;
    use bakery_agent::StubModel;
    use std::sync::Arc;
    use tower::ServiceExt;

    /// One request to the whole app, auth and all, as `token`.
    async fn send(state: &AppState, method: &str, path: &str, token: &str, body: Value) -> (StatusCode, Value) {
        let req = Request::builder()
            .method(method)
            .uri(path)
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(if method == "GET" { Body::empty() } else { Body::from(body.to_string()) })
            .unwrap();
        let res = crate::build_app(state.clone()).oneshot(req).await.unwrap();
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    async fn rpc(state: &AppState, token: &str, method: &str, params: Value) -> (StatusCode, Value) {
        send(state, "POST", "/mcp", token, json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params})).await
    }

    /// A tool's answer through the window: whether it failed, and its text.
    async fn tool(state: &AppState, token: &str, name: &str, args: Value) -> (bool, String) {
        let (status, answer) = rpc(state, token, "tools/call", json!({"name": name, "arguments": args})).await;
        assert_eq!(status, StatusCode::OK, "{name}: {answer}");
        let failed = answer["result"]["isError"].as_bool().unwrap_or(true);
        (failed, answer["result"]["content"][0]["text"].as_str().unwrap_or_default().to_string())
    }

    fn tip(state: &AppState) -> Option<(i64, String)> {
        state.heart.lock().unwrap().tip().unwrap()
    }

    /// Every tool it lists is a read, and a call to each, through the
    /// whole app, leaves the Heart as it was: nothing is logged, not even a
    /// note read. It reads what the Baker reads, never the operator's
    /// private notes; the node keeps only the token's hash, 0600.
    #[tokio::test(flavor = "multi_thread")]
    async fn every_call_only_reads() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let baker = crate::routes::baker::ensure_permanent_baker(&state, None, None).unwrap().stream_id;
        for (family, payload) in [
            ("agent.turn.started", json!({"instruction": "look", "mode": "auto", "turn_id": "t1"})),
            ("agent.turn.usage", json!({"model_id": "opus", "effort": "high", "tokens_in": 10, "turn_id": "t1"})),
            ("agent.turn.failed", json!({"reason": "connection reset", "turn_id": "t1"})),
        ] {
            append(&state, &baker, "human", family, payload.to_string(), vec![]).unwrap();
        }
        let note = |id: &str, extra: Value| {
            let mut fact = json!({"id": id, "scope": "bakery", "type": "lesson", "title": "Deploys", "body": "Deploys go through bakery.sh."});
            fact.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            append(&state, crate::memory_index::STREAM, "human", crate::memory_index::WRITTEN, fact.to_string(), vec![]).unwrap();
        };
        note("deploys", json!({}));
        note("mine", json!({"sensitivity": "private"}));
        crate::backlog::add(&state, &crate::backlog::AddReq { title: "Tidy".into(), why: String::new(), scope: None }).unwrap();
        let made = create(&state).unwrap();
        let kept = std::fs::read_to_string(dir.path().join(FILE)).unwrap();
        assert!(!kept.contains(&made.token), "only the hash is kept");
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(dir.path().join(FILE)).unwrap().permissions().mode() & 0o777, 0o600);

        let before = tip(&state);
        let (status, listed) = rpc(&state, &made.token, "tools/list", json!({})).await;
        assert_eq!(status, StatusCode::OK, "{listed}");
        let names: Vec<&str> = listed["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(names, TOOLS);
        for name in TOOLS.iter().filter(|n| **n != EVENTS) {
            assert_eq!(crate::api_catalog::find(name).map(|t| t.risk), Some(Risk::Read), "{name}");
        }
        for (name, args) in [
            ("thread_board", json!({})),
            ("thread_read", json!({"stream_id": baker})),
            ("thread_events", json!({"stream_id": baker})),
            ("backlog_list", json!({})),
            ("memory_read", json!({"stream_id": baker})),
            ("memory_search", json!({"query": "deploys"})),
            ("memory_get", json!({"ids": ["deploys", "mine"]})),
        ] {
            let (failed, text) = tool(&state, &made.token, name, args).await;
            assert!(!failed, "{name}: {text}");
            match name {
                "thread_events" => assert!(text.contains("connection reset") && text.contains("\"effort\":\"high\""), "{text}"),
                "backlog_list" => assert!(text.contains("Tidy"), "{text}"),
                "memory_search" => assert!(text.contains("deploys") && !text.contains("\"mine\""), "{text}"),
                "memory_get" => assert!(text.contains("bakery.sh") && text.contains("\"unknown\":[\"mine\"]"), "{text}"),
                _ => {}
            }
        }
        let (failed, text) = tool(&state, &made.token, "backlog_add", json!({"title": "x", "why": "y"})).await;
        assert!(failed && text.contains("not one of"), "{text}");
        assert_eq!(tip(&state), before, "the window wrote nothing");
    }

    /// A revoked token is refused at once, and a new one is a new token; one
    /// token at a time.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_revoked_token_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let made = create(&state).unwrap();
        assert_eq!(create(&state).err().map(|e| e.0), Some(StatusCode::CONFLICT));
        assert_eq!(rpc(&state, &made.token, "tools/list", json!({})).await.0, StatusCode::OK);
        assert!(status(&state).last_used.is_some());
        revoke(&state).unwrap();
        assert_eq!(rpc(&state, &made.token, "tools/list", json!({})).await.0, StatusCode::UNAUTHORIZED);
        assert!(status(&state).created_at.is_none());
        let again = create(&state).unwrap();
        assert_ne!(again.token, made.token);
        assert_eq!(rpc(&state, &made.token, "tools/list", json!({})).await.0, StatusCode::UNAUTHORIZED);
        assert_eq!(rpc(&state, &again.token, "tools/list", json!({})).await.0, StatusCode::OK);
        assert_eq!(rpc(&state, "bkw_made-up", "tools/list", json!({})).await.0, StatusCode::UNAUTHORIZED);
    }

    /// A room can't use it. The token opens nothing but `/mcp`: not
    /// `/mcp/tools` or `/mcp/call`, the only routes a door or the seats'
    /// gateway relays to, nor any operator route. A room's own session
    /// token, and the operator's, open nothing at `/mcp`. And every room's
    /// profile closes the port it is on.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_room_cannot_use_it() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let baker = crate::routes::baker::ensure_permanent_baker(&state, None, None).unwrap().stream_id;
        let window = create(&state).unwrap().token;
        let session = crate::mcp::mint_mcp_token(&state, &baker, crate::node::Seat::Orchestrator, crate::mcp::McpScope::BakerOps);

        assert_eq!(send(&state, "GET", "/mcp/tools", &window, Value::Null).await.0, StatusCode::UNAUTHORIZED);
        let call = json!({"name": "thread_board", "arguments": {}});
        assert_eq!(send(&state, "POST", "/mcp/call", &window, call).await.0, StatusCode::UNAUTHORIZED);
        assert_eq!(send(&state, "GET", "/node", &window, Value::Null).await.0, StatusCode::FORBIDDEN);
        assert_eq!(send(&state, "GET", "/outside-window", &window, Value::Null).await.0, StatusCode::FORBIDDEN);
        assert_eq!(rpc(&state, &session, "tools/list", json!({})).await.0, StatusCode::UNAUTHORIZED);
        assert_eq!(rpc(&state, "test-operator-token", "tools/list", json!({})).await.0, StatusCode::UNAUTHORIZED);
        assert_eq!(rpc(&state, &window, "tools/list", json!({})).await.0, StatusCode::OK);

        let api = std::env::var("BAKERY_NODE_ADDR")
            .ok()
            .and_then(|addr| addr.rsplit_once(':')?.1.parse::<u16>().ok())
            .unwrap_or(7717);
        let env = crate::doors::room_env(&state, "wt:outside-window");
        let shut = env.iter().find(|(name, _)| name == bakery_exec::SHUT_ENV).map(|(_, ports)| ports.clone()).unwrap();
        assert!(shut.split(',').any(|p| p == api.to_string()), "{shut}");
    }

    /// `thread_events` gives one thread's events by family: the newest
    /// first unless `since` names a seq, at most `limit`.
    #[test]
    fn thread_events_by_family_since_and_limit() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        for family in ["agent.turn.started", "agent.session.opened", "message.received", "agent.turn.failed", "agent.turn.done", "agent.context.compacted"] {
            append(&state, "wt:t", "human", family, json!({"say": family}).to_string(), vec![]).unwrap();
        }
        let events = |args: Value| -> Vec<(u64, String)> {
            let answer: Value = serde_json::from_str(&call(&state, EVENTS, &args).unwrap()).unwrap();
            answer["events"].as_array().unwrap().iter().map(|e| (e["seq"].as_u64().unwrap(), e["family"].as_str().unwrap().to_string())).collect()
        };
        let all = events(json!({"stream_id": "wt:t"}));
        let named: Vec<&str> = all.iter().map(|(_, f)| f.as_str()).collect();
        assert_eq!(named, ["turns", "sessions", "notes", "errors", "turns", "compactions"]);
        assert_eq!(events(json!({"stream_id": "wt:t", "families": ["turns"], "limit": 1})), [(4, "turns".to_string())]);
        assert_eq!(events(json!({"stream_id": "wt:t", "families": ["turns", "errors"], "since": 0})).len(), 2);
        assert!(call(&state, EVENTS, &json!({"stream_id": "wt:t", "families": ["secrets"]})).unwrap_err().contains("no family"));
        assert!(call(&state, EVENTS, &json!({})).is_err());
    }
}
