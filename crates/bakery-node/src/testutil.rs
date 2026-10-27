//! Test-only fixtures shared by every `routes::*` test module: repo/state
//! builders and the stub `Model` impls the tool-loop tests script against.

use crate::node::{NodeConfig, WriteMode};
use crate::routes::agent::{agent_turn_inner, AgentTurnReq};
use crate::routes::project::{open_stream, OpenReq};
use crate::state::AppState;
pub(crate) use bakery_agent::StubModel;
use bakery_agent::{ChatMsg, EditRequest, Model, ProposedEdit};
use bakery_heart::Heart;
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

/// Publish a tiny, immutable cache-hit fixture for node tests that exercise
/// routing/sandbox behavior rather than toolchain installation. Tests of a
/// genuine first provision deliberately do not call this helper.
pub(crate) fn seed_test_toolchain_cache(cache_root: &std::path::Path, workspace: &std::path::Path) {
    let spec = bakery_exec::Spec::load(workspace).unwrap();
    let root = cache_root.join("envs").join(spec.digest());
    let rust_bin = root.join("toolchain/rust/bin");
    std::fs::create_dir_all(&rust_bin).unwrap();

    let version = spec.rust_version().unwrap_or("test");
    let rustc = rust_bin.join("rustc");
    std::fs::write(
        &rustc,
        format!("#!/bin/sh\necho 'rustc {version} (test fixture)'\n"),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(&rustc).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&rustc, permissions).unwrap();
    }
    std::fs::write(root.join("ready"), b"").unwrap();
}

pub(crate) fn run_git(dir: &std::path::Path, args: &[&str]) {
    let ok = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?} failed");
}

/// Waits out a test process's first connection to a server of its own on
/// loopback. The Mac's network filter (LuLu) checks a new process on its
/// first connection: seconds on an idle Mac, past half a minute under a full
/// suite, while the next connection takes a millisecond. An attempt given up
/// early starts that check over, so this is one connect, up to five
/// minutes. A test calls it once its server listens, before anything of its
/// own is timed (a ticket, a short timeout).
pub(crate) fn first_connection(addr: std::net::SocketAddr) {
    let waited = crate::doors::first_connection(&format!("http://{addr}"), std::time::Duration::from_secs(300));
    waited.unwrap_or_else(|e| panic!("the test's own server never answered: {e}"));
}

/// A one-commit repo on `main` with a `f.txt`.
pub(crate) fn make_repo(at: &std::path::Path) -> std::path::PathBuf {
    std::fs::create_dir_all(at).unwrap();
    run_git(at, &["init", "-q", "-b", "main"]);
    run_git(at, &["config", "user.email", "t@example.com"]);
    run_git(at, &["config", "user.name", "t"]);
    std::fs::write(at.join("f.txt"), "line one\n").unwrap();
    run_git(at, &["add", "."]);
    run_git(at, &["commit", "-qm", "init"]);
    at.to_path_buf()
}

pub(crate) fn state_with(base: &std::path::Path, model: Arc<dyn Model>) -> AppState {
    state_with_write_mode(base, model, WriteMode::Manual)
}

pub(crate) fn state_with_write_mode(
    base: &std::path::Path,
    model: Arc<dyn Model>,
    write_mode: WriteMode,
) -> AppState {
    let wt_base = base.join("worktrees");
    std::fs::create_dir_all(&wt_base).unwrap();
    let (tx, _rx) = broadcast::channel(64);
    let mut node = NodeConfig::load_or_create(base);
    node.write_mode = write_mode;
    let mut heart = Heart::open_in_memory().unwrap();
    let (studio_input_gen, studio_cache) = crate::routes::studio::studio_watch(&mut heart);
    AppState {
        heart: Arc::new(Mutex::new(heart)),
        tx,
        base: Arc::new(base.to_path_buf()),
        worktree_base: Arc::new(wt_base),
        model: Arc::new(Mutex::new(model.clone())),
        node: Arc::new(Mutex::new(node)),
        pty_tickets: Arc::new(Mutex::new(Default::default())),
        lifecycle: Arc::new(Mutex::new(Default::default())),
        terminals: Arc::new(Mutex::new(Default::default())),
        seats: Arc::new(Mutex::new(Default::default())),
        child_tool_allow_once: Arc::new(Mutex::new(Default::default())),
        acp_sessions: Arc::new(Mutex::new(Default::default())),
        system_login_lock: Arc::new(tokio::sync::Mutex::new(())),
        edit_decision_lock: Arc::new(Mutex::new(())),
        baker_bootstrap_lock: Arc::new(Mutex::new(())),
        catalog_cache: Arc::new(crate::model_catalog::ModelCatalogCache::default()),
        agent_activity: Arc::new(Mutex::new(Default::default())),
        queue_dispatchers: Arc::new(Mutex::new(Default::default())),
        mcp_tokens: Arc::new(Mutex::new(Default::default())),
        task_drivers: Arc::new(Default::default()),
        task_diff_cache: Arc::new(Mutex::new(Default::default())),
        studio_input_gen,
        studio_cache,
        compile_jobs: crate::builds::compile_jobserver(),
        operator_token: Arc::new("test-operator-token".to_string()),
        decision_waiters: Arc::new(Mutex::new(Default::default())),
        events_tickets: Arc::new(Mutex::new(Default::default())),
        // Lets the ~30 tests written before seats existed keep calling
        // `state_with(dir, model)` unmodified — see `state::resolve_seat`.
        test_default_model: Some(model),
    }
}

/// Like `state_with`, but leaves `test_default_model` unset — the one seam
/// `resolve_seat` falls back to for pre-seat tests. Used to prove a turn
/// with nothing assigned to its resolved seat fails closed, same as a real
/// node with no seat configured (06-SEATS.md §4: "a turn without a
/// configured model fails").
pub(crate) fn state_with_no_default_model(base: &std::path::Path) -> AppState {
    let wt_base = base.join("worktrees");
    std::fs::create_dir_all(&wt_base).unwrap();
    let (tx, _rx) = broadcast::channel(64);
    let node = NodeConfig::load_or_create(base);
    let mut heart = Heart::open_in_memory().unwrap();
    let (studio_input_gen, studio_cache) = crate::routes::studio::studio_watch(&mut heart);
    AppState {
        heart: Arc::new(Mutex::new(heart)),
        tx,
        base: Arc::new(base.to_path_buf()),
        worktree_base: Arc::new(wt_base),
        model: Arc::new(Mutex::new(Arc::new(StubModel))),
        node: Arc::new(Mutex::new(node)),
        pty_tickets: Arc::new(Mutex::new(Default::default())),
        lifecycle: Arc::new(Mutex::new(Default::default())),
        terminals: Arc::new(Mutex::new(Default::default())),
        seats: Arc::new(Mutex::new(Default::default())),
        child_tool_allow_once: Arc::new(Mutex::new(Default::default())),
        acp_sessions: Arc::new(Mutex::new(Default::default())),
        system_login_lock: Arc::new(tokio::sync::Mutex::new(())),
        edit_decision_lock: Arc::new(Mutex::new(())),
        baker_bootstrap_lock: Arc::new(Mutex::new(())),
        catalog_cache: Arc::new(crate::model_catalog::ModelCatalogCache::default()),
        agent_activity: Arc::new(Mutex::new(Default::default())),
        queue_dispatchers: Arc::new(Mutex::new(Default::default())),
        mcp_tokens: Arc::new(Mutex::new(Default::default())),
        task_drivers: Arc::new(Default::default()),
        task_diff_cache: Arc::new(Mutex::new(Default::default())),
        studio_input_gen,
        studio_cache,
        compile_jobs: crate::builds::compile_jobserver(),
        operator_token: Arc::new("test-operator-token".to_string()),
        decision_waiters: Arc::new(Mutex::new(Default::default())),
        events_tickets: Arc::new(Mutex::new(Default::default())),
        test_default_model: None,
    }
}

/// A helper binary `scripts/test.sh` built before the tests and names in
/// `var`, so no test spends its time limit building it: None when the tests
/// run without it, and they build their own.
fn prebuilt(var: &str) -> Option<std::path::PathBuf> {
    let path = std::path::PathBuf::from(std::env::var_os(var)?);
    assert!(path.is_file(), "{var} names {}, which isn't there", path.display());
    Some(path)
}

/// Build (once, cached) the fake ACP agent binary used by every ACP
/// round-trip test — shared by `pipe.rs` and `routes::agent`'s tests so a
/// named-plug turn test can drive the same fixture as the low-level pipe
/// tests without a second cargo-build helper.
pub(crate) fn fake_acp_agent_binary() -> std::path::PathBuf {
    static BINARY: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    BINARY
        .get_or_init(|| {
            if let Some(path) = prebuilt("BAKERY_FAKE_ACP_AGENT_BIN_FOR_TEST") {
                return path;
            }
            let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
            let status = std::process::Command::new(cargo)
                .args([
                    "build",
                    "-p",
                    "bakery-node",
                    "--bin",
                    "bakery-fake-acp-agent",
                ])
                .status()
                .expect("run cargo build for the fake ACP agent binary");
            assert!(status.success(), "building bakery-fake-acp-agent failed");

            let mut path = std::env::current_exe().expect("current test binary path");
            path.pop();
            if path.ends_with("deps") {
                path.pop();
            }
            path.push("bakery-fake-acp-agent");
            assert!(
                path.is_file(),
                "fake ACP agent binary not found at {path:?}"
            );
            path
        })
        .clone()
}

/// Build (once, cached) the REAL `bakery-node` binary — used only by the
/// 07d-B Phase 2 Stage C MCP e2e proof (`pipe.rs`), which needs a process
/// that actually implements the `mcp-shim` subcommand: `cargo test`'s own
/// test binary compiles `main.rs` too, but its generated test-harness
/// entrypoint never runs the crate's real `fn main()`/subcommand dispatch,
/// so `std::env::current_exe()` from inside a test cannot stand in for it.
pub(crate) fn bakery_node_binary() -> std::path::PathBuf {
    static BINARY: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    BINARY
        .get_or_init(|| {
            if let Some(path) = prebuilt("BAKERY_NODE_BIN_FOR_TEST") {
                return path;
            }
            let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
            let status = std::process::Command::new(cargo)
                .args(["build", "-p", "bakery-node", "--bin", "bakery-node"])
                .status()
                .expect("run cargo build for the real bakery-node binary");
            assert!(status.success(), "building bakery-node failed");

            let mut path = std::env::current_exe().expect("current test binary path");
            path.pop();
            if path.ends_with("deps") {
                path.pop();
            }
            path.push("bakery-node");
            assert!(path.is_file(), "bakery-node binary not found at {path:?}");
            path
        })
        .clone()
}

pub(crate) fn open_req(repo: &std::path::Path, name: &str) -> OpenReq {
    OpenReq {
        repo: repo.to_string_lossy().into_owned(),
        name: Some(name.to_string()),
        ..Default::default()
    }
}

/// Records the same durable `socket.kind.recorded: factory` fact
/// `baker_assign_inner` writes for real — the only server-supported way a
/// stream becomes factory-kind (07d-A1 correction: no longer inferable from
/// a `baker-`-shaped name/branch/id). Tests use this instead of naming a
/// stream `baker-...` and relying on that to fake factory authority.
pub(crate) fn mark_factory(state: &AppState, stream_id: &str) {
    crate::state::append(
        state,
        stream_id,
        "human",
        crate::node::SOCKET_KIND_FAMILY,
        serde_json::to_string(&crate::node::SocketKindRecord {
            kind: crate::node::SocketKind::Factory,
        })
        .unwrap(),
        vec![],
    )
    .unwrap();
}

/// Model that replays a fixed script of chat replies (the tool loop's input).
pub(crate) struct ScriptedModel {
    pub(crate) replies: Mutex<std::collections::VecDeque<String>>,
    pub(crate) seen: Mutex<Vec<usize>>,
    /// First system prompt + first user message seen (for Baker ops assertions).
    pub(crate) captured: Mutex<Vec<(String, String)>>,
    /// What it reports thinking on every call (W1-T).
    pub(crate) thinking: Option<String>,
}

impl ScriptedModel {
    pub(crate) fn with_thinking(mut self, text: &str) -> Self {
        self.thinking = Some(text.to_string());
        self
    }

    pub(crate) fn new(replies: &[&str]) -> Self {
        Self {
            thinking: None,
            replies: Mutex::new(replies.iter().map(|s| s.to_string()).collect()),
            seen: Mutex::new(Vec::new()),
            captured: Mutex::new(Vec::new()),
        }
    }
}

impl Model for ScriptedModel {
    fn name(&self) -> &str {
        "scripted"
    }

    fn last_turn_thinking(&self) -> Option<bakery_agent::Thinking> {
        self.thinking.clone().map(|text| bakery_agent::Thinking { text })
    }
    fn propose_edit(&self, _req: &EditRequest) -> Result<ProposedEdit, String> {
        Err("scripted model is chat-only".into())
    }
    fn supports_chat(&self) -> bool {
        true
    }
    fn chat(&self, system: &str, messages: &[ChatMsg]) -> Result<String, String> {
        self.seen.lock().unwrap().push(messages.len());
        if let Some(first) = messages.first() {
            self.captured
                .lock()
                .unwrap()
                .push((system.to_string(), first.content.clone()));
        }
        let reply = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| "script exhausted".to_string())?;
        // "ERR:<why>": this call fails with <why>.
        match reply.strip_prefix("ERR:") {
            Some(why) => Err(why.to_string()),
            None => Ok(reply),
        }
    }

    fn chat_turn(
        &self,
        system: &str,
        messages: &[ChatMsg],
        _tools: &[bakery_agent::ToolDecl],
    ) -> Result<bakery_agent::ChatReply, String> {
        let raw = self.chat(system, messages)?;
        Ok(crate::agent_loop::chat_reply_from_scripted_text(&raw))
    }
}

/// Model that never stops calling tools — the bound is what must stop it.
/// Loops forever issuing tool calls — for exhaustion/budget tests. The
/// args VARY per call (a counter) on purpose: this fixture exercises "the
/// model keeps legitimately working", which must not trip the stall
/// guard's identical-call detection (that guard has its own fixtures).
pub(crate) struct LoopingModel;

pub(crate) static LOOPING_CALLS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

fn looping_reply() -> String {
    let n = LOOPING_CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!(r#"{{"tool":"list_files","args":{{"path":".","probe":{n}}}}}"#)
}

impl Model for LoopingModel {
    fn name(&self) -> &str {
        "looping"
    }
    fn propose_edit(&self, _req: &EditRequest) -> Result<ProposedEdit, String> {
        Err("chat only".into())
    }
    fn supports_chat(&self) -> bool {
        true
    }
    fn chat(&self, _system: &str, _messages: &[ChatMsg]) -> Result<String, String> {
        Ok(looping_reply())
    }

    fn chat_turn(
        &self,
        _system: &str,
        _messages: &[ChatMsg],
        _tools: &[bakery_agent::ToolDecl],
    ) -> Result<bakery_agent::ChatReply, String> {
        Ok(crate::agent_loop::chat_reply_from_scripted_text(
            &looping_reply(),
        ))
    }
}

/// Chat-capable model whose call always errors — proves a mid-loop model
/// failure is reported as `agent.turn.failed` with phase "model".
pub(crate) struct FailingChatModel;

impl Model for FailingChatModel {
    fn name(&self) -> &str {
        "failing-chat"
    }
    fn propose_edit(&self, _req: &EditRequest) -> Result<ProposedEdit, String> {
        Err("failing-chat model is chat-only".into())
    }
    fn supports_chat(&self) -> bool {
        true
    }
    fn chat(&self, _system: &str, _messages: &[ChatMsg]) -> Result<String, String> {
        Err("simulated model failure".into())
    }
}

/// Streams a fixed script of [`bakery_agent::ChatReply`]s via a real
/// `chat_turn_streaming` override (07d-A5) — unlike `ScriptedModel`/
/// `LoopingModel` above, which only override `chat_turn` and so exercise
/// nothing but the trait's one-shot default for the streaming path, this
/// double proves `agent_tool_turn`'s actual streaming/cancellation wiring.
/// A `Text` reply is emitted as several small deltas (not one bulk flush)
/// so a test can observe more than one `Streaming{text}` update.
pub(crate) struct StreamingScriptedModel {
    pub(crate) replies: Mutex<std::collections::VecDeque<bakery_agent::ChatReply>>,
    /// Fires exactly once, after the first scripted call's deltas (if any)
    /// have been emitted but before that call's cancellation check — lets a
    /// test deterministically coordinate a concurrent cancel request with
    /// this model's own progress, instead of guessing with a sleep.
    pub(crate) on_first_call: Mutex<Option<Box<dyn FnMut() + Send>>>,
}

impl StreamingScriptedModel {
    pub(crate) fn new(replies: Vec<bakery_agent::ChatReply>) -> Self {
        Self {
            replies: Mutex::new(replies.into()),
            on_first_call: Mutex::new(None),
        }
    }

    pub(crate) fn with_first_call_hook(
        replies: Vec<bakery_agent::ChatReply>,
        hook: impl FnMut() + Send + 'static,
    ) -> Self {
        Self {
            replies: Mutex::new(replies.into()),
            on_first_call: Mutex::new(Some(Box::new(hook))),
        }
    }
}

impl Model for StreamingScriptedModel {
    fn name(&self) -> &str {
        "streaming-scripted"
    }
    fn propose_edit(&self, _req: &EditRequest) -> Result<ProposedEdit, String> {
        Err("streaming-scripted model is chat-only".into())
    }
    fn supports_chat(&self) -> bool {
        true
    }
    fn supports_streaming(&self) -> bool {
        true
    }
    fn chat(&self, _system: &str, _messages: &[ChatMsg]) -> Result<String, String> {
        Err("streaming-scripted only supports the streaming call".into())
    }
    fn chat_turn(
        &self,
        _system: &str,
        _messages: &[ChatMsg],
        _tools: &[bakery_agent::ToolDecl],
    ) -> Result<bakery_agent::ChatReply, String> {
        self.replies
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| "script exhausted".to_string())
    }
    fn chat_turn_streaming(
        &self,
        _system: &str,
        _messages: &[ChatMsg],
        _tools: &[bakery_agent::ToolDecl],
        on_text_delta: &mut dyn FnMut(&str),
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<bakery_agent::StreamEnd, String> {
        let reply = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| "script exhausted".to_string())?;
        if let bakery_agent::ChatReply::Text(text) = &reply {
            for chunk in text.as_bytes().chunks(3) {
                on_text_delta(std::str::from_utf8(chunk).unwrap_or(""));
            }
        }
        if let Some(mut hook) = self.on_first_call.lock().unwrap().take() {
            hook();
        }
        if cancel.load(std::sync::atomic::Ordering::Acquire) {
            let partial_text = match &reply {
                bakery_agent::ChatReply::Text(t) => t.clone(),
                bakery_agent::ChatReply::ToolCalls(_) => String::new(),
            };
            return Ok(bakery_agent::StreamEnd::Stopped {
                partial_text,
                reason: bakery_agent::StopReason::Cancelled,
            });
        }
        Ok(bakery_agent::StreamEnd::Completed(reply))
    }
}

/// A model double reporting configurable usage and recording the last
/// `set_effort` call (07d-A3D1 Phase C) — proves the per-turn override
/// plumbing (`model_id` override, `effort`, the resulting
/// `agent.turn.usage` event) without any real provider or network.
/// `with_overridden_model_id` returns a distinctly-marked instance (its own
/// reply text names the overridden id) so a test can tell which instance
/// actually answered a turn.
pub(crate) struct UsageReportingModel {
    reply: String,
    usage: Mutex<Option<bakery_agent::TurnUsage>>,
    pub(crate) last_effort: Mutex<Option<String>>,
}

impl UsageReportingModel {
    pub(crate) fn new(reply: impl Into<String>, usage: Option<bakery_agent::TurnUsage>) -> Self {
        Self {
            reply: reply.into(),
            usage: Mutex::new(usage),
            last_effort: Mutex::new(None),
        }
    }
}

impl Model for UsageReportingModel {
    fn name(&self) -> &str {
        "usage-double"
    }
    fn propose_edit(&self, _req: &EditRequest) -> Result<ProposedEdit, String> {
        Err("usage-double is chat-only".into())
    }
    fn supports_chat(&self) -> bool {
        true
    }
    fn chat_turn(
        &self,
        _system: &str,
        _messages: &[ChatMsg],
        _tools: &[bakery_agent::ToolDecl],
    ) -> Result<bakery_agent::ChatReply, String> {
        Ok(bakery_agent::ChatReply::Text(self.reply.clone()))
    }
    fn set_effort(&self, effort: Option<&str>) {
        *self.last_effort.lock().unwrap() = effort.map(str::to_string);
    }
    fn last_turn_usage(&self) -> Option<bakery_agent::TurnUsage> {
        *self.usage.lock().unwrap()
    }
    fn with_overridden_model_id(&self, model_id: &str) -> Option<Arc<dyn Model>> {
        Some(Arc::new(UsageReportingModel::new(
            format!("overridden reply from {model_id}"),
            *self.usage.lock().unwrap(),
        )))
    }
}

// ─── Manual gate: propose / accept / reject ──────────────────────────────

/// Open a worktree and produce one pending edit through the normal agent
/// loop — a scripted model that proposes an edit to the selected file,
/// exactly the flow a selected file now takes (Checkpoint 2 item 1 removed
/// the legacy single-shot `propose_edit` dispatch; a `file_path` is
/// context, not a different code path). Returns the real minted
/// `stream_id` too (07d-A3: opaque, not derivable from the "wtA" name
/// passed to `open_req` below) — every caller that needs to address this
/// stream uses that, never a `wt:`-literal guess.
pub(crate) fn setup_with_pending() -> (
    tempfile::TempDir,
    AppState,
    std::path::PathBuf,
    String,
    String,
) {
    let dir = tempfile::tempdir().unwrap();
    let repo = make_repo(&dir.path().join("repo"));
    let model = Arc::new(ScriptedModel::new(&[
        r#"{"edit":{"file_path":"f.txt","new_content":"line one\nbakery stub agent: add marker\n","rationale":"add marker"}}"#,
        "continuing after the decision",
    ]));
    let state = state_with(dir.path(), model);
    let resp = open_stream(&state, &open_req(&repo, "wtA")).unwrap();
    let wt = std::path::PathBuf::from(&resp.context.worktree_path);
    let stream_id = resp.context.stream_id;
    // Continue-after-decision (Checkpoint 2 item 2): a proposing turn now
    // PARKS awaiting the decision instead of returning — run it on its own
    // thread (exactly what `spawn_blocking` does in production) and hand
    // back the pending proposal's hash once it lands on the stream. The
    // parked turn resumes and finishes when the test decides.
    let pending_hash =
        spawn_turn_and_await_pending(&state, &stream_id, Some("f.txt"), "add marker");
    (dir, state, wt, pending_hash, stream_id)
}

/// Issue a turn on a background thread and block until its pending
/// proposal/question event appears on the stream, returning that event's
/// hash. Panics after 10s — a missing proposal is a broken fixture, not a
/// slow one.
pub(crate) fn spawn_turn_and_await_pending(
    state: &AppState,
    stream_id: &str,
    file_path: Option<&str>,
    instruction: &str,
) -> String {
    let before: usize = state
        .heart
        .lock()
        .unwrap()
        .read_stream(stream_id)
        .map(|e| e.len())
        .unwrap_or(0);
    let state2 = state.clone();
    let req = AgentTurnReq {
        model_id: None,
        effort: None,
        actor: None,
        stream_id: stream_id.to_string(),
        file_path: file_path.map(str::to_string),
        instruction: instruction.to_string(),
        seat: None,
    };
    std::thread::spawn(move || {
        let _ = agent_turn_inner(&state2, &req);
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        {
            let events = state
                .heart
                .lock()
                .unwrap()
                .read_stream(stream_id)
                .unwrap_or_default();
            if let Some(ev) = events.iter().skip(before).find(|e| {
                matches!(
                    e.family.as_str(),
                    "agent.file.changed" | "agent.ask.pending"
                )
            }) {
                return ev.event_hash.clone();
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no pending proposal/question appeared within 10s"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

// ─── Baker assign (B1) ───────────────────────────────────────────────────

/// Point the node at a temp "bakery" git repo (VISION.md + crates/bakery-node).
pub(crate) fn bakery_repo_for_test(dir: &std::path::Path) -> std::path::PathBuf {
    let repo = make_repo(&dir.join("thebakery"));
    std::fs::write(repo.join("VISION.md"), "# The Bakery\n").unwrap();
    std::fs::create_dir_all(repo.join("crates/bakery-node")).unwrap();
    repo
}

pub(crate) fn state_with_bakery_root(base: &std::path::Path, repo: &std::path::Path) -> AppState {
    let state = state_with(base, Arc::new(StubModel));
    state.node.lock().unwrap().bakery_source_root = Some(repo.to_string_lossy().into_owned());
    state
}

// ─── The operator's approvals ────────────────────────────────────────────

/// Runs `f` while the operator allows each card `stream_id` shows for the
/// Bakery tool `name`: one that always asks (`api_tools::gate`) runs in any
/// mode once allowed.
pub(crate) fn allowing<T>(state: &AppState, stream_id: &str, name: &str, f: impl FnOnce() -> T) -> T {
    use crate::pipe::{ChildToolGateDecisionKind, CHILD_TOOL_DECIDED_FAMILY, CHILD_TOOL_PENDING_FAMILY};
    use std::sync::atomic::{AtomicBool, Ordering};
    let done = Arc::new(AtomicBool::new(false));
    let operator = {
        let (state, stream_id, name, done) = (state.clone(), stream_id.to_string(), name.to_string(), done.clone());
        std::thread::spawn(move || {
            while !done.load(Ordering::Relaxed) {
                let events = state.heart.lock().unwrap().read_stream(&stream_id).unwrap_or_default();
                let open = |card: &&bakery_core::Event| {
                    let payload: serde_json::Value = serde_json::from_str(&card.payload).unwrap_or_default();
                    card.family == CHILD_TOOL_PENDING_FAMILY
                        && payload["bakery_tool"]["name"] == name
                        && !events.iter().any(|e| e.family == CHILD_TOOL_DECIDED_FAMILY && e.causality.contains(&card.event_hash))
                };
                for card in events.iter().filter(open) {
                    let _ = crate::pipe::decide_child_tool_gate(&state, &stream_id, &card.event_hash, ChildToolGateDecisionKind::AllowOnce);
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        })
    };
    let out = f();
    done.store(true, Ordering::Relaxed);
    operator.join().unwrap();
    out
}
