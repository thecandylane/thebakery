//! Named terminal HTTP/WS surface (07c-STUDIO-SHELL §3) — create/list/
//! rename/close plus the ticketed WebSocket attach. The session itself
//! (process, ring buffer, live broadcast) lives in [`crate::terminals`];
//! this module is the thin wire layer over it, same split as
//! `routes::env`'s attach routes over `bakery_exec`.

use crate::state::{admit_pty, cache_root, jail_for, AppState, TerminalTicket};
use crate::terminals::{self, SpawnSpec, TerminalInfo};
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Query, State,
    },
    http::{HeaderMap, StatusCode},
    response::Response,
    Json,
};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct CreateReq {
    pub(crate) stream_id: String,
    #[serde(default)]
    pub(crate) name: Option<String>,
    pub(crate) mode: String,
    /// Set only for a system account login pane (07d-A3D1) — `stream_id`
    /// must then be [`terminals::SYSTEM_ACCOUNTS_STREAM_ID`], and `mode` is
    /// ignored (always resolved as an unsandboxed host process, same as
    /// `mode: "host"`, since the login CLI needs the human's real
    /// credentials/network, not a Project worktree or sandbox).
    #[serde(default)]
    #[schemars(with = "Option<crate::api_catalog::LoginCommandShape>")]
    pub(crate) command: Option<bakery_agent::cli_tools::LoginCommand>,
}

/// Resolves program/args/cwd for `mode` exactly like the pre-07c
/// WS-per-pty handler did, then spawns and registers a node-owned session.
pub(crate) async fn terminal_create(
    State(st): State<AppState>,
    Json(req): Json<CreateReq>,
) -> Result<Json<TerminalInfo>, (StatusCode, String)> {
    if let Some(command) = req.command {
        return terminal_create_system(st, req.stream_id, command).await;
    }
    // The operator's own shells, which no thread owns: this Mac's, from
    // anywhere, and a project's.
    if req.stream_id == terminals::HOST_STREAM_ID {
        return host_terminal(&st, req.name).await.map(Json);
    }
    if let Some(project_id) = terminals::project_of_stream(&req.stream_id) {
        return project_terminal(&st, project_id.to_string(), req.name).await.map(Json);
    }

    // The Agent terminal is the node's own read-only record of an agent's
    // commands: opened here (made if it has none yet), never spawned.
    if req.mode == terminals::AGENT_TERMINAL_MODE {
        crate::state::context_for(&st, &req.stream_id).map_err(|e| (StatusCode::NOT_FOUND, e))?;
        return terminals::agent_terminal(&st, &req.stream_id)
            .map(|handle| Json(handle.info()))
            .ok_or_else(|| {
                (
                    StatusCode::CONFLICT,
                    "stream is being discarded".to_string(),
                )
            });
    }

    spawn_shell(&st, req.stream_id, req.mode, req.name).await.map(Json)
}

/// The operator's own terminals on this Mac, which an agent never sees or
/// reaches: to an agent, [`terminals::HOST_STREAM_ID`] has none.
fn hidden_from_caller(stream_id: &str) -> bool {
    stream_id == terminals::HOST_STREAM_ID && crate::actor::agent_calling()
}

/// The answer for a terminal that isn't there, or that the caller may not see.
fn no_terminal(stream_id: &str, terminal_id: &str) -> (StatusCode, String) {
    (StatusCode::NOT_FOUND, format!("no terminal {terminal_id} on {stream_id}"))
}

fn operator_only() -> Result<(), (StatusCode, String)> {
    match crate::actor::agent_calling() {
        true => Err((StatusCode::FORBIDDEN, "the operator's own terminals are theirs alone".to_string())),
        false => Ok(()),
    }
}

/// The operator's shell, as `$SHELL` names it.
fn user_shell() -> String {
    std::env::var("SHELL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "/bin/zsh".to_string())
}

/// The operator's own shell on this Mac, from anywhere: an interactive login
/// shell in their home, on [`terminals::HOST_STREAM_ID`]. No thread owns it,
/// so it is never a burn's to refuse.
async fn host_terminal(st: &AppState, name: Option<String>) -> Result<TerminalInfo, (StatusCode, String)> {
    operator_only()?;
    let stream_id = terminals::HOST_STREAM_ID.to_string();
    let pending = admit_pty(st, &stream_id, "host")?;
    let home = std::env::var("HOME").unwrap_or_else(|_| "/".to_string());
    let shell = user_shell();
    // A login shell builds its PATH and the rest from the operator's own
    // profile, as Terminal's does: nothing of the node's environment.
    let mut env: Vec<(String, String)> = ["USER", "LOGNAME", "TMPDIR", "LANG"]
        .iter()
        .filter_map(|key| Some((key.to_string(), std::env::var(key).ok()?)))
        .collect();
    env.extend([
        ("HOME".to_string(), home.clone()),
        ("SHELL".to_string(), shell.clone()),
        ("PATH".to_string(), "/usr/bin:/bin:/usr/sbin:/sbin".to_string()),
    ]);
    let spec = SpawnSpec {
        stream_id,
        name,
        mode: "host".to_string(),
        program: PathBuf::from(shell),
        args: vec!["-l".to_string()],
        cwd: PathBuf::from(home),
        profile_path: None,
        login_command: None,
        env: Some(env),
    };
    spawn_spec(st, spec, pending).await
}

/// A project's own terminal, on its `project:<id>` stream, in its main
/// folder, always boxed to the project: its room on this Mac (no sandbox,
/// or the Mac sandbox), or a container of the seat image (containers). Only
/// [`terminals::HOST_STREAM_ID`] is ever the operator's whole Mac.
async fn project_terminal(
    st: &AppState,
    project_id: String,
    name: Option<String>,
) -> Result<TerminalInfo, (StatusCode, String)> {
    operator_only()?;
    let record = crate::catalog::project_by_id(st, &project_id)
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("no project {project_id}")))?;
    let root = record
        .roots
        .first()
        .map(|r| PathBuf::from(&r.path))
        .ok_or_else(|| (StatusCode::CONFLICT, format!("{} has no folder to open a terminal in", record.name)))?;
    let stream_id = format!("{}{project_id}", terminals::PROJECT_STREAM_PREFIX);
    let mode = match record.stacks.sandbox.as_str() {
        "container" => "container",
        _ => "room",
    };
    let pending = admit_pty(st, &stream_id, mode)?;
    let blocked = |e: tokio::task::JoinError| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
    let (program, args, profile_path, env) = match mode {
        "container" => {
            let (state, stream, folder) = (st.clone(), stream_id.clone(), root.clone());
            let (cli, args, env) = crate::actor::spawn_blocking(move || crate::seat_container::shell(&state, &stream, &folder))
                .await
                .map_err(blocked)?
                .map_err(|e| (StatusCode::CONFLICT, e))?;
            (cli, args, None, Some(env))
        }
        _ => {
            let (cache, folder, stream) = (cache_root(st), root.clone(), stream_id.clone());
            let room = crate::actor::spawn_blocking(move || bakery_exec::ensure_thin_room(&folder, &cache, &stream))
                .await
                .map_err(blocked)?
                .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
            let shell = room_shell(st, &stream_id, &room, &root).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
            (shell.program, shell.args, Some(shell.profile_path), None)
        }
    };
    let spec = SpawnSpec {
        stream_id,
        name,
        mode: mode.to_string(),
        program,
        args,
        cwd: root,
        profile_path,
        login_command: None,
        env,
    };
    spawn_spec(st, spec, pending).await
}

async fn spawn_spec(
    st: &AppState,
    spec: SpawnSpec,
    pending: crate::state::PtyPendingGuard,
) -> Result<TerminalInfo, (StatusCode, String)> {
    let (st, stream_id) = (st.clone(), spec.stream_id.clone());
    crate::actor::spawn_blocking(move || terminals::spawn_terminal(&st, spec, pending))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("{stream_id}: {e}")))
}

/// A shell on `stream_id`'s worktree, host or room. The operator may open
/// either. An agent may open a room terminal; a host terminal is refused,
/// and the agent runs the command with `run` inside the room.
pub(crate) async fn spawn_shell(
    st: &AppState,
    stream_id: String,
    mode: String,
    name: Option<String>,
) -> Result<TerminalInfo, (StatusCode, String)> {
    if mode == "host" && crate::actor::agent_calling() {
        return Err((
            StatusCode::FORBIDDEN,
            "an agent cannot open a host terminal. Run the command in the room.".to_string(),
        ));
    }
    let st = st.clone();
    let req = CreateReq { stream_id, mode, name, command: None };
    // Admits atomically with the retiring/discarded (and for host, burn)
    // check — same guarantee the old ticket-mint + WS-upgrade pair gave,
    // just now covering "spawn the process" instead of "open the socket".
    let pending = admit_pty(&st, &req.stream_id, &req.mode)?;
    let jail = jail_for(&st, &req.stream_id).map_err(|e| (StatusCode::NOT_FOUND, e))?;
    let workspace = jail.root().to_path_buf();

    let (program, args, cwd, profile_path) = match req.mode.as_str() {
        "host" => {
            let shell = std::env::var("SHELL")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "/bin/zsh".to_string());
            (
                PathBuf::from(shell),
                vec!["-i".to_string()],
                workspace,
                None,
            )
        }
        "room" => {
            let cache_root = cache_root(&st);
            let ws_root = workspace.clone();
            let stream_id = req.stream_id.clone();
            let env = crate::actor::spawn_blocking(move || {
                bakery_exec::ensure_thin_room(&ws_root, &cache_root, &stream_id)
            })
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
            .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
            let shell = room_shell(&st, &req.stream_id, &env, &workspace).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
            (
                shell.program,
                shell.args,
                workspace,
                Some(shell.profile_path),
            )
        }
        other => {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("unknown terminal mode {other:?}"),
            ))
        }
    };

    let spec = SpawnSpec {
        stream_id: req.stream_id,
        name: req.name,
        mode: req.mode,
        program,
        args,
        cwd,
        profile_path,
        login_command: None,
        env: None,
    };
    let stream_id_for_task = spec.stream_id.clone();
    crate::actor::spawn_blocking(move || terminals::spawn_terminal(&st, spec, pending))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                format!("{stream_id_for_task}: {e}"),
            )
        })
}

/// What a thread shows when a command is typed for the operator to run
/// (the iPhone run pane). Not a way for an agent to run a command.
pub(crate) const PREPARED_FAMILY: &str = "terminal.prepared";

/// A new shell prints its prompt before it reads what is typed: wait for
/// that, a few seconds at most.
pub(crate) async fn wait_for_prompt(handle: &terminals::TerminalHandle) {
    for _ in 0..60 {
        if !handle.text_tail(1).is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct ReadQuery {
    pub(crate) stream_id: String,
    pub(crate) terminal_id: String,
    /// The last this many bytes it printed: 8000 by default, 64000 at most.
    #[serde(default)]
    pub(crate) max_bytes: Option<usize>,
}

#[derive(schemars::JsonSchema, Serialize)]
pub(crate) struct TerminalText {
    pub(crate) text: String,
    pub(crate) live: bool,
    /// Something is running in it now.
    pub(crate) running: bool,
}

/// `GET /terminal/read`: what one of the thread's terminals printed last,
/// as text: how an agent reads the output of a command the operator ran.
pub(crate) async fn terminal_read(
    State(st): State<AppState>,
    Query(q): Query<ReadQuery>,
) -> Result<Json<TerminalText>, (StatusCode, String)> {
    let handle = st.terminals.lock().unwrap().get(&q.terminal_id).filter(|h| h.stream_id == q.stream_id && !hidden_from_caller(&h.stream_id));
    let handle = handle.ok_or_else(|| no_terminal(&q.stream_id, &q.terminal_id))?;
    let live = handle.is_alive();
    Ok(Json(TerminalText {
        text: handle.text_tail(q.max_bytes.unwrap_or(8000).min(64_000)),
        running: live && handle.mode != terminals::AGENT_TERMINAL_MODE && !handle.is_idle(),
        live,
    }))
}

/// A Codex/Cursor "Sign in" login pane (07d-A3D1) — Settings has no Project/
/// socket context to spawn a terminal under, so this always runs under the
/// reserved [`terminals::SYSTEM_ACCOUNTS_STREAM_ID`], skips `jail_for`
/// entirely (there is no worktree), and resolves program/args/name from the
/// closed [`bakery_agent::cli_tools::LoginCommand`] enum — never a
/// client-supplied argv or name (07d-A3D1 review finding 1: a terminal's
/// `name` is free-form/renameable and must never stand in for its real
/// identity).
///
/// Idempotent: holds `system_login_lock` for the whole
/// check-for-an-existing-live-one-else-spawn section, so two concurrent
/// "Sign in" clicks for the same tool return the *same* terminal rather than
/// racing two `codex login`/`cursor-agent login` processes into existence.
async fn terminal_create_system(
    st: AppState,
    stream_id: String,
    command: bakery_agent::cli_tools::LoginCommand,
) -> Result<Json<TerminalInfo>, (StatusCode, String)> {
    if stream_id != terminals::SYSTEM_ACCOUNTS_STREAM_ID {
        return Err((
            StatusCode::BAD_REQUEST,
            "command is only valid for the reserved accounts stream".to_string(),
        ));
    }

    let lock = st.system_login_lock.clone();
    let _guard = lock.lock().await;

    if let Some(existing) = st
        .terminals
        .lock()
        .unwrap()
        .find_live_login(&stream_id, command)
    {
        return Ok(Json(existing.info()));
    }

    let pending = admit_pty(&st, &stream_id, "host")?;
    let (program, args) = match command {
        bakery_agent::cli_tools::LoginCommand::ClaudeNative => {
            crate::claude_native::login_program_and_args(&st)
                .map_err(|error| (StatusCode::BAD_REQUEST, error))?
        }
        _ => bakery_agent::cli_tools::login_argv(command)
            .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?,
    };
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    // Narrowed to exactly what a login CLI needs to run and find its own
    // config dir — never bakery-node's complete process environment, which
    // may carry other providers' API keys this process has no business
    // handing to a Codex/Cursor child (07d-A3D1 review finding 3).
    let mut env = vec![
        ("HOME".to_string(), home.clone()),
        (
            "PATH".to_string(),
            if command == bakery_agent::cli_tools::LoginCommand::ClaudeNative {
                "/bin:/usr/bin".to_string()
            } else {
                std::env::var("PATH").unwrap_or_default()
            },
        ),
        (
            "USER".to_string(),
            std::env::var("USER").unwrap_or_default(),
        ),
    ];
    // Sign in INTO Bakery's own credential directory, not the operator's
    // `~/.claude`. Two reasons, both load-bearing:
    //   1. It is the directory `claude_native::status` probes and the only one
    //      a sandboxed turn is granted, so all three surfaces finally agree.
    //   2. Bakery gets its own OAuth grant. Sharing the host CLI's refresh
    //      token would mean whichever side refreshed first logged the other
    //      out — the operator's terminal `claude` must keep working untouched.
    if command == bakery_agent::cli_tools::LoginCommand::ClaudeNative {
        let surface = crate::claude_native::prepare_auth_surface(&st)
            .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error))?;
        if let Some(path) = env.iter_mut().find(|(key, _)| key == "PATH") {
            path.1 = surface.path;
        }
        env.push((
            "CLAUDE_CONFIG_DIR".to_string(),
            surface.config_dir.to_string_lossy().into_owned(),
        ));
    }
    let spec = SpawnSpec {
        stream_id: stream_id.clone(),
        name: Some(command.canonical_name().to_string()),
        mode: "host".to_string(),
        program,
        args,
        cwd: PathBuf::from(home),
        profile_path: None,
        login_command: Some(command),
        env: Some(env),
    };
    crate::actor::spawn_blocking(move || terminals::spawn_terminal(&st, spec, pending))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("{stream_id}: {e}")))
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct ListQuery {
    pub(crate) stream_id: String,
}

pub(crate) async fn terminal_list(
    State(st): State<AppState>,
    Query(q): Query<ListQuery>,
) -> Json<Vec<TerminalInfo>> {
    if hidden_from_caller(&q.stream_id) {
        return Json(Vec::new());
    }
    Json(st.terminals.lock().unwrap().list_for_stream(&q.stream_id))
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct RenameReq {
    pub(crate) stream_id: String,
    pub(crate) terminal_id: String,
    pub(crate) name: String,
}

pub(crate) async fn terminal_rename(
    State(st): State<AppState>,
    Json(req): Json<RenameReq>,
) -> Result<Json<TerminalInfo>, (StatusCode, String)> {
    let name = req.name.trim();
    if name.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "name must not be empty".to_string(),
        ));
    }
    let handle = st
        .terminals
        .lock()
        .unwrap()
        .get(&req.terminal_id)
        .filter(|h| h.stream_id == req.stream_id && !hidden_from_caller(&h.stream_id))
        .ok_or_else(|| no_terminal(&req.stream_id, &req.terminal_id))?;
    handle.rename(name.to_string());
    Ok(Json(handle.info()))
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct CloseReq {
    pub(crate) stream_id: String,
    pub(crate) terminal_id: String,
}

/// Explicit close — kills and reaps this one terminal's process tree.
/// Detaching a view (a WS just disconnecting) never reaches this; only a
/// human clicking close, or a socket discard fencing every terminal it owns.
pub(crate) async fn terminal_close(
    State(st): State<AppState>,
    Json(req): Json<CloseReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    {
        let owned = st
            .terminals
            .lock()
            .unwrap()
            .get(&req.terminal_id)
            .is_some_and(|h| h.stream_id == req.stream_id && !hidden_from_caller(&h.stream_id));
        if !owned {
            return Err(no_terminal(&req.stream_id, &req.terminal_id));
        }
    }
    terminals::kill_terminal(&st, &req.terminal_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    // A real (empty) JSON body, not 204 No Content — the client's `send()`
    // always calls `JSON.parse` on the response text, which throws on "".
    Ok(Json(serde_json::json!({})))
}

/// How long a minted ticket stays redeemable — see `TerminalTicket`'s doc.
const TERMINAL_TICKET_TTL: Duration = Duration::from_secs(30);

fn random_ticket() -> String {
    let mut buf = [0u8; 24];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut buf))
        .expect("/dev/urandom must be readable");
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

fn prune_expired_tickets(tickets: &mut std::collections::HashMap<String, TerminalTicket>) {
    let now = Instant::now();
    tickets.retain(|_, t| t.expires_at > now);
}

/// One-time: a successful match removes the ticket regardless of whether the
/// `stream_id`/`terminal_id` it was minted for actually match — a
/// guessed-but-wrong ticket still gets burned rather than left live for a
/// second try.
fn consume_terminal_ticket(
    st: &AppState,
    stream_id: &str,
    terminal_id: &str,
    ticket: &str,
) -> bool {
    let mut tickets = st.pty_tickets.lock().unwrap();
    prune_expired_tickets(&mut tickets);
    match tickets.remove(ticket) {
        Some(t) => t.stream_id == stream_id && t.terminal_id == terminal_id,
        None => false,
    }
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct TicketReq {
    pub(crate) stream_id: String,
    pub(crate) terminal_id: String,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct TicketResp {
    pub(crate) ticket: String,
    pub(crate) expires_in_secs: u64,
}

/// The only way to get a ticket: a normal CORS-governed POST — same shape as
/// `routes::env`'s (now-removed) pty ticket mint, scoped to one terminal.
pub(crate) async fn terminal_ticket(
    State(st): State<AppState>,
    Json(req): Json<TicketReq>,
) -> Result<Json<TicketResp>, (StatusCode, String)> {
    let owned = st
        .terminals
        .lock()
        .unwrap()
        .get(&req.terminal_id)
        .is_some_and(|h| h.stream_id == req.stream_id && !hidden_from_caller(&h.stream_id));
    if !owned {
        return Err(no_terminal(&req.stream_id, &req.terminal_id));
    }
    let ticket = random_ticket();
    let mut tickets = st.pty_tickets.lock().unwrap();
    prune_expired_tickets(&mut tickets);
    tickets.insert(
        ticket.clone(),
        TerminalTicket {
            stream_id: req.stream_id,
            terminal_id: req.terminal_id,
            expires_at: Instant::now() + TERMINAL_TICKET_TTL,
        },
    );
    Ok(Json(TicketResp {
        ticket,
        expires_in_secs: TERMINAL_TICKET_TTL.as_secs(),
    }))
}

#[derive(Deserialize)]
pub(crate) struct WsQuery {
    pub(crate) stream_id: String,
    pub(crate) terminal_id: String,
    pub(crate) ticket: String,
    /// Replay everything from this byte offset onward — `0` for a fresh
    /// attach, or the last offset this client already has for a resume.
    #[serde(default)]
    pub(crate) from_offset: u64,
    /// The app's own view counts what it gets: a JSON text frame
    /// `{"at": offset, "dropped": bytes}` goes before the replay, and again
    /// wherever output it was due is no longer kept, so a reconnect resumes
    /// exactly where it left off and a gap is never silent. Without it, a
    /// gap is one line of text in the terminal.
    #[serde(default)]
    pub(crate) offsets: bool,
}

#[derive(Deserialize)]
struct WsCtrl {
    resize: Option<WsResize>,
}

#[derive(Deserialize)]
struct WsResize {
    cols: u16,
    rows: u16,
}

/// Attaches a WebSocket view to an already-running terminal — never spawns
/// anything. Browsers do not apply CORS to the WS handshake, so the same
/// two independent gates `routes::env`'s old pty WS used still apply here:
/// Origin allowlist, then a one-time ticket minted only via the CORS-
/// governed `POST /terminal/ticket` above.
pub(crate) async fn terminal_ws(
    State(st): State<AppState>,
    Query(q): Query<WsQuery>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Result<Response, (StatusCode, String)> {
    if !crate::state::origin_allowed(&headers) {
        return Err((StatusCode::FORBIDDEN, "origin not allowed".to_string()));
    }
    if !consume_terminal_ticket(&st, &q.stream_id, &q.terminal_id, &q.ticket) {
        return Err((
            StatusCode::FORBIDDEN,
            "missing or invalid ticket".to_string(),
        ));
    }
    // 04b-FENCE §1: a discard that starts the instant before a client
    // attaches must win, not race it — the terminal is about to be killed
    // by the fence either way, so an attach here would just watch it die.
    if st.lifecycle.lock().unwrap().retiring.contains(&q.stream_id) {
        return Err((
            StatusCode::CONFLICT,
            "stream is being discarded".to_string(),
        ));
    }
    let handle = st
        .terminals
        .lock()
        .unwrap()
        .get(&q.terminal_id)
        .filter(|h| h.stream_id == q.stream_id)
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                format!("no terminal {} on {}", q.terminal_id, q.stream_id),
            )
        })?;
    if !handle.is_alive() {
        return Err((StatusCode::GONE, "terminal has exited".to_string()));
    }

    Ok(ws.on_upgrade(move |socket| attach_socket(socket, handle, q.from_offset, q.offsets)))
}

/// Sends a view what is still kept from where it is (`sent`), after word of
/// anything it was due that is gone, and moves `sent` to the end of it.
/// `opening` is the attach's first send, which tells an offset-counting view
/// where it starts even when nothing is gone. False once the socket is gone.
async fn send_kept(socket: &mut WebSocket, kept: terminals::Replay, offsets: bool, opening: bool, sent: &mut u64) -> bool {
    let word = if offsets && (opening || kept.dropped > 0) {
        Some(serde_json::json!({ "at": kept.start, "dropped": kept.dropped }).to_string())
    } else if kept.dropped > 0 && opening {
        Some("\r\n[bakery: earlier output was truncated by the ring buffer]\r\n".to_string())
    } else if kept.dropped > 0 {
        Some(format!("\r\n[bakery: {} bytes of output dropped here]\r\n", kept.dropped))
    } else {
        None
    };
    if let Some(word) = word {
        if socket.send(Message::Text(word.into())).await.is_err() {
            return false;
        }
    }
    *sent = kept.start + kept.bytes.len() as u64;
    kept.bytes.is_empty() || socket.send(Message::Binary(kept.bytes.into())).await.is_ok()
}

/// Bridges one WebSocket to an already-running terminal for the life of the
/// connection only. The replay and the live subscription are taken together
/// ([`terminals::TerminalHandle::attach_since`]), and every live chunk says
/// where it starts, so the view gets each byte once: none twice, none lost
/// in between, and none skipped when it falls behind. Dropping this
/// connection (pane closed, client reloaded) does not touch the process at
/// all: that is the entire point of the ownership inversion
/// (07c-STUDIO-SHELL §3).
async fn attach_socket(
    mut socket: WebSocket,
    handle: std::sync::Arc<terminals::TerminalHandle>,
    from_offset: u64,
    offsets: bool,
) {
    // `live_rx`'s `Closed` arm below is aspirational, not reachable in
    // practice while this function holds its own `handle` clone: `output_tx`
    // lives *inside* the shared `TerminalHandle`, so the broadcast channel
    // only closes once every `Arc` (including this one) is gone. A process
    // exiting naturally while attached is instead detected via this liveness
    // watch, which is what actually unblocks the loop below.
    let mut alive_rx = handle.watch_alive();
    let (mut live_rx, replay) = handle.attach_since(from_offset);
    // Where the view is: the offset of the next byte it is due.
    let mut sent = replay.start;
    if !send_kept(&mut socket, replay, offsets, true, &mut sent).await {
        return;
    }

    let mut exited = false;
    loop {
        tokio::select! {
            live = live_rx.recv() => match live {
                Ok(chunk) => {
                    let end = chunk.start + chunk.bytes.len() as u64;
                    if end <= sent {
                        continue; // already sent, in a catch-up
                    }
                    if chunk.start > sent {
                        // Bytes between went past this view: send them from the ring.
                        if !send_kept(&mut socket, handle.kept_since(sent), offsets, false, &mut sent).await {
                            break;
                        }
                        continue;
                    }
                    let fresh = &chunk.bytes[(sent - chunk.start) as usize..];
                    if socket.send(Message::Binary(fresh.to_vec().into())).await.is_err() {
                        break;
                    }
                    sent = end;
                }
                // A slow view missed live chunks to a full broadcast buffer:
                // it catches up from the ring, told of any part the ring no
                // longer keeps, never skipped in silence.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    if !send_kept(&mut socket, handle.kept_since(sent), offsets, false, &mut sent).await {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => { exited = true; break; }
            },
            changed = alive_rx.changed() => {
                // `Err` means the sender (the session's background task) is
                // gone too — either way, the process is no longer live.
                if changed.is_err() || !*alive_rx.borrow() {
                    exited = true;
                    break;
                }
            },
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Binary(bytes))) => {
                    let h = handle.clone();
                    let bytes = bytes.to_vec();
                    if crate::actor::spawn_blocking(move || h.write_input(&bytes)).await.is_err() {
                        break;
                    }
                }
                Some(Ok(Message::Text(txt))) => {
                    if let Ok(ctrl) = serde_json::from_str::<WsCtrl>(&txt) {
                        if let Some(r) = ctrl.resize {
                            handle.resize(r.cols, r.rows);
                        }
                    }
                }
                Some(Ok(Message::Close(_))) | None => break,
                Some(Ok(_)) => {}
                Some(Err(_)) => break,
            },
        }
    }

    if exited {
        let code = handle
            .exit_code()
            .map(|c| c.to_string())
            .unwrap_or_else(|| "?".to_string());
        let _ = socket
            .send(Message::Close(Some(axum::extract::ws::CloseFrame {
                code: 1000,
                reason: format!("exited:{code}").into(),
            })))
            .await;
        return;
    }

    // A raw drop closes the TCP connection without a WS close frame, which
    // xterm/browser clients surface as an abnormal 1006 disconnect even on
    // an ordinary detach.
    let _ = socket.send(Message::Close(None)).await;
}

/// The operator's own shell in a thread's room: the room's profile, with
/// the toolchains its agents run by name (mise's, rustup's, Homebrew's).
fn room_shell(st: &AppState, stream_id: &str, env: &bakery_exec::SessionEnvironment, workspace: &std::path::Path) -> Result<bakery_exec::RoomShell, String> {
    let tools = crate::machine_access::working_tools(st, stream_id);
    let given = bakery_exec::ExtraGrants { read: &tools.read, write: &tools.write, exec: &tools.exec, path: &tools.path, env: &tools.env, ..Default::default() };
    bakery_exec::interactive_room_shell(env, workspace, &given)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::Lifetime;
    use crate::routes::project::open_stream;
    use crate::testutil::*;
    use bakery_agent::StubModel;
    use std::sync::Arc;

    // ─── ticket mechanics (unit-level) ──────────────────────────────────────

    #[test]
    fn ticket_is_one_time_use() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        state.pty_tickets.lock().unwrap().insert(
            "tok".to_string(),
            TerminalTicket {
                stream_id: "wt:a".to_string(),
                terminal_id: "t1".to_string(),
                expires_at: Instant::now() + Duration::from_secs(30),
            },
        );

        assert!(consume_terminal_ticket(&state, "wt:a", "t1", "tok"));
        assert!(
            !consume_terminal_ticket(&state, "wt:a", "t1", "tok"),
            "a ticket must not be usable twice"
        );
    }

    #[test]
    fn ticket_bound_to_a_different_stream_is_refused_and_still_burned() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        state.pty_tickets.lock().unwrap().insert(
            "tok".to_string(),
            TerminalTicket {
                stream_id: "wt:a".to_string(),
                terminal_id: "t1".to_string(),
                expires_at: Instant::now() + Duration::from_secs(30),
            },
        );

        assert!(!consume_terminal_ticket(&state, "wt:b", "t1", "tok"));
        assert!(
            !consume_terminal_ticket(&state, "wt:a", "t1", "tok"),
            "even a wrong-stream guess must burn the ticket"
        );
    }

    #[test]
    fn ticket_bound_to_a_different_terminal_is_refused_and_still_burned() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        state.pty_tickets.lock().unwrap().insert(
            "tok".to_string(),
            TerminalTicket {
                stream_id: "wt:a".to_string(),
                terminal_id: "t1".to_string(),
                expires_at: Instant::now() + Duration::from_secs(30),
            },
        );

        assert!(!consume_terminal_ticket(&state, "wt:a", "t2", "tok"));
        assert!(
            !consume_terminal_ticket(&state, "wt:a", "t1", "tok"),
            "even a wrong-terminal guess must burn the ticket"
        );
    }

    #[test]
    fn an_expired_ticket_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        state.pty_tickets.lock().unwrap().insert(
            "tok".to_string(),
            TerminalTicket {
                stream_id: "wt:a".to_string(),
                terminal_id: "t1".to_string(),
                expires_at: Instant::now() - Duration::from_secs(1),
            },
        );

        assert!(!consume_terminal_ticket(&state, "wt:a", "t1", "tok"));
    }

    #[test]
    fn an_unknown_ticket_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        assert!(!consume_terminal_ticket(
            &state,
            "wt:a",
            "t1",
            "does-not-exist"
        ));
    }

    // ─── /terminal/create, /terminal/list, /terminal/rename, /terminal/ticket ──

    #[tokio::test]
    async fn terminal_create_refuses_an_unknown_stream() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let err = terminal_create(
            State(state),
            Json(CreateReq {
                stream_id: "wt:nope".into(),
                name: None,
                mode: "host".into(),
                command: None,
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn terminal_create_refuses_an_unknown_mode() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id_term = open_stream(&state, &open_req(&repo, "term"))
            .unwrap()
            .context
            .stream_id;

        let err = terminal_create(
            State(state),
            Json(CreateReq {
                stream_id: id_term.clone(),
                name: None,
                mode: "sudo".into(),
                command: None,
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    async fn terminal_create_room_mode_works_without_a_mise_toml() {
        // 07b: the room terminal is the thin room — no mise.toml gate.
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id_term = open_stream(&state, &open_req(&repo, "term"))
            .unwrap()
            .context
            .stream_id;

        let info = terminal_create(
            State(state.clone()),
            Json(CreateReq {
                stream_id: id_term.clone(),
                name: None,
                mode: "room".into(),
                command: None,
            }),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(info.mode, "room");
        assert_eq!(info.name, "room");
        assert!(info.live);
        terminals::kill_terminal(&state, &info.terminal_id)
            .await
            .unwrap();
    }

    #[tokio::test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    async fn terminal_create_room_mode_works_when_mise_toml_is_present() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        std::fs::write(repo.join("mise.toml"), "[tools]\nrust = \"1.0.0\"\n").unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id_term = open_stream(&state, &open_req(&repo, "term"))
            .unwrap()
            .context
            .stream_id;

        let info = terminal_create(
            State(state.clone()),
            Json(CreateReq {
                stream_id: id_term.clone(),
                name: None,
                mode: "room".into(),
                command: None,
            }),
        )
        .await
        .unwrap()
        .0;
        assert!(info.live);
        terminals::kill_terminal(&state, &info.terminal_id)
            .await
            .unwrap();
    }

    // ─── system account login terminals (07d-A3D1) ─────────────────────────

    #[tokio::test]
    async fn a_login_command_is_refused_for_a_real_project_stream() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id_term = open_stream(&state, &open_req(&repo, "term"))
            .unwrap()
            .context
            .stream_id;

        let err = terminal_create(
            State(state),
            Json(CreateReq {
                stream_id: id_term,
                name: None,
                mode: "host".into(),
                command: Some(bakery_agent::cli_tools::LoginCommand::Codex),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }

    /// Dumps its own environment to `dump_path` (baked into the script body
    /// itself, since a narrowed child env can't be trusted to carry an
    /// `ENV_DUMP_PATH` var through) before sleeping, so a test can inspect
    /// exactly what a login child actually saw.
    fn fixture_env_probe_script(dump_path: &std::path::Path) -> tempfile::TempPath {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(f, "#!/bin/sh\nenv > '{}'\nsleep 30\n", dump_path.display()).unwrap();
        let path = f.into_temp_path();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// 07d-A3D1 review finding 3: a login child must not inherit
    /// bakery-node's own complete process environment (other providers'
    /// API keys, arbitrary operator secrets) — only the narrow allowlist
    /// (`HOME`/`PATH`/`USER`/`TERM`) it actually needs to run and find its
    /// own config dir.
    #[test]
    fn a_login_terminal_gets_a_narrow_environment_not_the_full_node_process_env() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let dump = dir.path().join("env_dump.txt");
        let script = fixture_env_probe_script(&dump);
        let rt = tokio::runtime::Runtime::new().unwrap();

        std::env::set_var(
            "BAKERY_TEST_CANARY_SECRET",
            "must-not-leak-to-a-login-child",
        );
        let info =
            bakery_agent::cli_tools::test_support::with_test_bins(Some(&script), None, || {
                rt.block_on(terminal_create(
                    State(state.clone()),
                    Json(CreateReq {
                        stream_id: terminals::SYSTEM_ACCOUNTS_STREAM_ID.into(),
                        name: None,
                        mode: "host".into(),
                        command: Some(bakery_agent::cli_tools::LoginCommand::Codex),
                    }),
                ))
            })
            .unwrap()
            .0;
        std::env::remove_var("BAKERY_TEST_CANARY_SECRET");

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut dumped = String::new();
        while Instant::now() < deadline {
            if let Ok(s) = std::fs::read_to_string(&dump) {
                if !s.is_empty() {
                    dumped = s;
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }

        assert!(
            !dumped.is_empty(),
            "the fixture script must have written its env dump within 5s"
        );
        assert!(
            !dumped.contains("BAKERY_TEST_CANARY_SECRET"),
            "a login child must not inherit bakery-node's own process env:\n{dumped}"
        );
        assert!(
            dumped.contains("HOME="),
            "HOME must survive — the CLI needs it to find its own config dir:\n{dumped}"
        );
        assert!(
            dumped.contains("PATH="),
            "PATH must survive — needed to resolve the CLI binary itself:\n{dumped}"
        );

        rt.block_on(terminals::kill_terminal(&state, &info.terminal_id))
            .unwrap();
    }

    /// Fixture executable that ignores whatever args `login_argv` passes
    /// (`login`) and just sleeps — long enough to prove it spawned and is
    /// listed under the reserved stream id, killed explicitly at the end.
    fn fixture_sleep_script() -> tempfile::TempPath {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(f, "#!/bin/sh\nsleep 30\n").unwrap();
        let path = f.into_temp_path();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// A plain `#[test]` (not `#[tokio::test]`), matching
    /// `routes::auth`'s `claude_code_account_routes_complete_without_returning_credentials`
    /// exactly — `cli_tools::test_support::with_test_bins` serializes on a
    /// process-wide lock shared with every other crate's tests touching the
    /// same env vars (07d-A3D1 review finding 6), and a `std::sync::Mutex`
    /// is not reentrant, so the closure it runs must not itself be an
    /// already-running tokio task; a fresh `Runtime::block_on` inside is the
    /// established way to run async code there.
    #[test]
    fn a_login_command_spawns_under_the_reserved_accounts_stream_and_is_listed() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let script = fixture_sleep_script();
        let rt = tokio::runtime::Runtime::new().unwrap();

        let info =
            bakery_agent::cli_tools::test_support::with_test_bins(Some(&script), None, || {
                rt.block_on(terminal_create(
                    State(state.clone()),
                    Json(CreateReq {
                        stream_id: terminals::SYSTEM_ACCOUNTS_STREAM_ID.into(),
                        name: Some("this-is-ignored".into()),
                        mode: "host".into(),
                        command: Some(bakery_agent::cli_tools::LoginCommand::Codex),
                    }),
                ))
            })
            .unwrap()
            .0;

        assert_eq!(info.stream_id, terminals::SYSTEM_ACCOUNTS_STREAM_ID);
        assert_eq!(info.mode, "host");
        assert_eq!(
            info.name, "codex-login",
            "name is server-derived from the command, never the client's"
        );

        let listed = rt
            .block_on(terminal_list(
                State(state.clone()),
                Query(ListQuery {
                    stream_id: terminals::SYSTEM_ACCOUNTS_STREAM_ID.into(),
                }),
            ))
            .0;
        assert!(listed.iter().any(|t| t.terminal_id == info.terminal_id));

        rt.block_on(terminals::kill_terminal(&state, &info.terminal_id))
            .unwrap();
    }

    /// 07d-A3D1 review finding 1, core guarantee: identity is `LoginCommand`,
    /// not `name` or count — a second "Sign in" for the same tool while one
    /// is already live must reuse it (no duplicate `codex login` process),
    /// while a different tool gets its own terminal.
    #[test]
    fn a_second_sign_in_for_the_same_tool_reuses_the_live_terminal_not_a_duplicate() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let codex_script = fixture_sleep_script();
        let cursor_script = fixture_sleep_script();
        let rt = tokio::runtime::Runtime::new().unwrap();

        let create = |command| {
            terminal_create(
                State(state.clone()),
                Json(CreateReq {
                    stream_id: terminals::SYSTEM_ACCOUNTS_STREAM_ID.into(),
                    name: None,
                    mode: "host".into(),
                    command: Some(command),
                }),
            )
        };

        let (first, second, cursor_info) = bakery_agent::cli_tools::test_support::with_test_bins(
            Some(&codex_script),
            Some(&cursor_script),
            || {
                let first = rt
                    .block_on(create(bakery_agent::cli_tools::LoginCommand::Codex))
                    .unwrap()
                    .0;
                let second = rt
                    .block_on(create(bakery_agent::cli_tools::LoginCommand::Codex))
                    .unwrap()
                    .0;
                let cursor_info = rt
                    .block_on(create(bakery_agent::cli_tools::LoginCommand::Cursor))
                    .unwrap()
                    .0;
                (first, second, cursor_info)
            },
        );

        assert_eq!(
            first.terminal_id, second.terminal_id,
            "a second Sign-in for the same tool must reuse the live terminal, not spawn a duplicate process"
        );
        assert_ne!(
            first.terminal_id, cursor_info.terminal_id,
            "a different tool's login must be its own terminal"
        );

        let listed = rt
            .block_on(terminal_list(
                State(state.clone()),
                Query(ListQuery {
                    stream_id: terminals::SYSTEM_ACCOUNTS_STREAM_ID.into(),
                }),
            ))
            .0;
        assert_eq!(
            listed.len(),
            2,
            "exactly one live terminal per distinct login command, no duplicates: {listed:?}"
        );

        rt.block_on(terminals::kill_terminal(&state, &first.terminal_id))
            .unwrap();
        rt.block_on(terminals::kill_terminal(&state, &cursor_info.terminal_id))
            .unwrap();
    }

    /// 07d-A3D1 review finding 1, the exact attack it named: a Cursor
    /// terminal renamed to look like "codex-login" must never be handed
    /// back to a later Codex sign-in — identity is `login_command`, name is
    /// cosmetic only.
    #[test]
    fn renaming_a_login_terminal_does_not_let_it_be_reused_as_a_different_tool() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let codex_script = fixture_sleep_script();
        let cursor_script = fixture_sleep_script();
        let rt = tokio::runtime::Runtime::new().unwrap();

        let (cursor_info, codex_info) = bakery_agent::cli_tools::test_support::with_test_bins(
            Some(&codex_script),
            Some(&cursor_script),
            || {
                let cursor_info = rt
                    .block_on(terminal_create(
                        State(state.clone()),
                        Json(CreateReq {
                            stream_id: terminals::SYSTEM_ACCOUNTS_STREAM_ID.into(),
                            name: None,
                            mode: "host".into(),
                            command: Some(bakery_agent::cli_tools::LoginCommand::Cursor),
                        }),
                    ))
                    .unwrap()
                    .0;

                // A confused human (or a hostile one) renames the live
                // Cursor pane to look like Codex's.
                rt.block_on(terminal_rename(
                    State(state.clone()),
                    Json(RenameReq {
                        stream_id: terminals::SYSTEM_ACCOUNTS_STREAM_ID.into(),
                        terminal_id: cursor_info.terminal_id.clone(),
                        name: "codex-login".into(),
                    }),
                ))
                .unwrap()
                .0;

                let codex_info = rt
                    .block_on(terminal_create(
                        State(state.clone()),
                        Json(CreateReq {
                            stream_id: terminals::SYSTEM_ACCOUNTS_STREAM_ID.into(),
                            name: None,
                            mode: "host".into(),
                            command: Some(bakery_agent::cli_tools::LoginCommand::Codex),
                        }),
                    ))
                    .unwrap()
                    .0;
                (cursor_info, codex_info)
            },
        );

        assert_ne!(
            cursor_info.terminal_id, codex_info.terminal_id,
            "a renamed Cursor terminal must never be reused as Codex just because its name now matches"
        );

        rt.block_on(terminals::kill_terminal(&state, &cursor_info.terminal_id))
            .unwrap();
        rt.block_on(terminals::kill_terminal(&state, &codex_info.terminal_id))
            .unwrap();
    }

    #[tokio::test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    async fn a_second_named_terminal_defaults_to_a_numbered_name() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id_term = open_stream(&state, &open_req(&repo, "term"))
            .unwrap()
            .context
            .stream_id;

        let a = terminal_create(
            State(state.clone()),
            Json(CreateReq {
                stream_id: id_term.clone(),
                name: None,
                mode: "room".into(),
                command: None,
            }),
        )
        .await
        .unwrap()
        .0;
        let b = terminal_create(
            State(state.clone()),
            Json(CreateReq {
                stream_id: id_term.clone(),
                name: None,
                mode: "room".into(),
                command: None,
            }),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(a.name, "room");
        assert_eq!(b.name, "room-2");
        terminals::kill_terminal(&state, &a.terminal_id)
            .await
            .unwrap();
        terminals::kill_terminal(&state, &b.terminal_id)
            .await
            .unwrap();
    }

    /// 07c-STUDIO-SHELL's acceptance bar: three named sessions at once, all
    /// independently listed and renameable.
    #[tokio::test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    async fn three_named_terminals_can_run_at_once_and_are_all_listed() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id_term = open_stream(&state, &open_req(&repo, "term"))
            .unwrap()
            .context
            .stream_id;

        let mut ids = Vec::new();
        for name in ["flutter", "fastapi", "celery"] {
            let info = terminal_create(
                State(state.clone()),
                Json(CreateReq {
                    stream_id: id_term.clone(),
                    name: Some(name.to_string()),
                    mode: "room".into(),
                    command: None,
                }),
            )
            .await
            .unwrap()
            .0;
            assert_eq!(info.name, name);
            assert!(info.live);
            ids.push(info.terminal_id);
        }

        let listed = terminal_list(
            State(state.clone()),
            Query(ListQuery {
                stream_id: id_term.clone(),
            }),
        )
        .await
        .0;
        assert_eq!(listed.len(), 3);
        let mut names: Vec<&str> = listed.iter().map(|i| i.name.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["celery", "fastapi", "flutter"]);
        assert!(listed.iter().all(|i| i.live));

        let renamed = terminal_rename(
            State(state.clone()),
            Json(RenameReq {
                stream_id: id_term.clone(),
                terminal_id: ids[0].clone(),
                name: "flutter-dev".into(),
            }),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(renamed.name, "flutter-dev");

        for id in &ids {
            terminals::kill_terminal(&state, id).await.unwrap();
        }
        let after = terminal_list(
            State(state),
            Query(ListQuery {
                stream_id: id_term.clone(),
            }),
        )
        .await
        .0;
        assert!(
            after.is_empty(),
            "explicit close must remove every closed terminal from the listing"
        );
    }

    #[tokio::test]
    async fn terminal_ticket_route_refuses_an_unknown_terminal() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id_term = open_stream(&state, &open_req(&repo, "term"))
            .unwrap()
            .context
            .stream_id;

        let err = terminal_ticket(
            State(state),
            Json(TicketReq {
                stream_id: id_term.clone(),
                terminal_id: "nope".into(),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::NOT_FOUND);
    }

    /// 04b-FENCE §1 analogue for the new split: admission now happens at
    /// create time, not WS-attach time — the stream must already read as
    /// occupied the instant `terminal_create` returns, no polling needed.
    #[tokio::test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    async fn terminal_create_registers_activity_before_returning() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id_term = open_stream(&state, &open_req(&repo, "term"))
            .unwrap()
            .context
            .stream_id;

        let info = terminal_create(
            State(state.clone()),
            Json(CreateReq {
                stream_id: id_term.clone(),
                name: None,
                mode: "room".into(),
                command: None,
            }),
        )
        .await
        .unwrap()
        .0;

        assert!(
            state
                .lifecycle
                .lock()
                .unwrap()
                .activity
                .get(&id_term)
                .is_some_and(|a| !a.ptys.is_empty()),
            "the stream must already read as occupied the instant terminal_create returns"
        );
        terminals::kill_terminal(&state, &info.terminal_id)
            .await
            .unwrap();
    }

    // ─── Real WebSocket handshakes — Origin/ticket rejection is enforced ────
    // in `WebSocketUpgrade`'s response, which only a live HTTP round trip can
    // observe; a direct function call can't exercise the handshake itself.

    async fn create_test_terminal(state: &AppState, stream_id: &str, mode: &str) -> TerminalInfo {
        terminal_create(
            State(state.clone()),
            Json(CreateReq {
                stream_id: stream_id.into(),
                name: None,
                mode: mode.into(),
                command: None,
            }),
        )
        .await
        .unwrap()
        .0
    }

    fn mint_ticket_for(state: &AppState, stream_id: &str, terminal_id: &str) -> String {
        let ticket = random_ticket();
        state.pty_tickets.lock().unwrap().insert(
            ticket.clone(),
            TerminalTicket {
                stream_id: stream_id.to_string(),
                terminal_id: terminal_id.to_string(),
                expires_at: Instant::now() + TERMINAL_TICKET_TTL,
            },
        );
        ticket
    }

    async fn spawn_terminal_ws_server(state: AppState) -> std::net::SocketAddr {
        let app = axum::Router::new()
            .route("/terminal/ws", axum::routing::get(terminal_ws))
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        crate::testutil::first_connection(addr);
        addr
    }

    /// The test's socket on its terminal, or a panic saying why it was
    /// refused.
    async fn attach(
        req: tokio_tungstenite::tungstenite::handshake::client::Request,
    ) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
        match tokio_tungstenite::connect_async(req).await {
            Ok((ws, _)) => ws,
            Err(tokio_tungstenite::tungstenite::Error::Http(resp)) => {
                let body = resp.body().as_deref().map(String::from_utf8_lossy).unwrap_or_default();
                panic!("the terminal socket was refused: {} {body}", resp.status())
            }
            Err(e) => panic!("the terminal socket never opened: {e}"),
        }
    }

    type TestSocket = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

    /// The next frame on a test's socket, within a minute.
    async fn next_frame(ws: &mut TestSocket) -> tokio_tungstenite::tungstenite::Message {
        use futures_util::StreamExt;
        tokio::time::timeout(Duration::from_secs(60), ws.next())
            .await
            .expect("a frame within 60s")
            .expect("the socket stayed open")
            .unwrap()
    }

    /// The app's view counts bytes (`offsets=true`): the attach says where
    /// it starts, a resume from where it left off gets exactly the rest and
    /// then follows live, and one from before what the ring keeps is told
    /// how much is gone, never skipped in silence.
    #[tokio::test]
    async fn a_view_resumes_exactly_where_it_left_off() {
        use tokio_tungstenite::tungstenite::Message as WsMessage;
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream = "wt:resume";
        terminals::agent_terminal_write(&state, stream, "c1", "echo hi", "hi\n", false);
        let handle = terminals::agent_terminal(&state, stream).unwrap();
        let (all, _, end) = handle.replay_since(0);
        let addr = spawn_terminal_ws_server(state.clone()).await;
        let open = |from: u64| {
            let ticket = mint_ticket_for(&state, stream, &handle.terminal_id);
            let mut req = terminal_ws_request(addr, stream, &handle.terminal_id, &ticket, Some("http://localhost:1420"));
            *req.uri_mut() = format!("{}&offsets=true&from_offset={from}", req.uri()).parse().unwrap();
            attach(req)
        };
        let at = |msg: WsMessage| match msg {
            WsMessage::Text(t) => serde_json::from_str::<serde_json::Value>(&t).unwrap(),
            other => panic!("expected where it starts, got {other:?}"),
        };
        let bytes = |msg: WsMessage| match msg {
            WsMessage::Binary(b) => b.to_vec(),
            other => panic!("expected output, got {other:?}"),
        };

        let mut ws = open(0).await;
        assert_eq!(at(next_frame(&mut ws).await), serde_json::json!({"at": 0, "dropped": 0}));
        assert_eq!(bytes(next_frame(&mut ws).await), all);
        drop(ws);

        // A view that doesn't count bytes gets the replay alone, as before.
        let ticket = mint_ticket_for(&state, stream, &handle.terminal_id);
        let mut ws = attach(terminal_ws_request(addr, stream, &handle.terminal_id, &ticket, Some("http://localhost:1420"))).await;
        assert_eq!(bytes(next_frame(&mut ws).await), all);
        drop(ws);

        let mut ws = open(end - 3).await;
        assert_eq!(at(next_frame(&mut ws).await), serde_json::json!({"at": end - 3, "dropped": 0}));
        assert_eq!(bytes(next_frame(&mut ws).await), all[all.len() - 3..].to_vec());
        terminals::agent_terminal_write(&state, stream, "c1", "echo hi", "more", false);
        assert_eq!(bytes(next_frame(&mut ws).await), b"more".to_vec(), "then live, nothing twice");
        drop(ws);

        terminals::agent_terminal_write(&state, stream, "c1", "echo hi", &"x".repeat(300 * 1024), false);
        let mut ws = open(0).await;
        let word = at(next_frame(&mut ws).await);
        let gone = word["dropped"].as_u64().unwrap();
        assert!(gone > 0 && word["at"].as_u64() == Some(gone), "{word}");
        assert!(bytes(next_frame(&mut ws).await).ends_with(b"xxx"));
    }

    /// The tests that boot real shells take turns: under a full suite's
    /// load, several at once slow each past its waits.
    async fn shell_turn() -> tokio::sync::MutexGuard<'static, ()> {
        static TURN: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
        TURN.get_or_init(Default::default).lock().await
    }

    fn terminal_ws_request(
        addr: std::net::SocketAddr,
        stream_id: &str,
        terminal_id: &str,
        ticket: &str,
        origin: Option<&str>,
    ) -> tokio_tungstenite::tungstenite::handshake::client::Request {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        let url = format!("ws://{addr}/terminal/ws?stream_id={stream_id}&terminal_id={terminal_id}&ticket={ticket}");
        let mut req = url.into_client_request().unwrap();
        if let Some(o) = origin {
            req.headers_mut().insert("Origin", o.parse().unwrap());
        }
        req
    }

    fn assert_ws_forbidden(err: tokio_tungstenite::tungstenite::Error) {
        match err {
            tokio_tungstenite::tungstenite::Error::Http(resp) => {
                assert_eq!(
                    resp.status(),
                    StatusCode::FORBIDDEN,
                    "body: {:?}",
                    resp.body()
                );
            }
            other => panic!("expected an HTTP-level rejection, got {other:?}"),
        }
    }

    #[tokio::test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    async fn ws_upgrade_is_refused_without_a_valid_ticket() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id_term = open_stream(&state, &open_req(&repo, "term"))
            .unwrap()
            .context
            .stream_id;
        let info = create_test_terminal(&state, &id_term, "room").await;
        let addr = spawn_terminal_ws_server(state.clone()).await;

        let req = terminal_ws_request(
            addr,
            &id_term,
            &info.terminal_id,
            "not-a-real-ticket",
            Some("http://localhost:1420"),
        );
        let err = tokio_tungstenite::connect_async(req).await.unwrap_err();
        assert_ws_forbidden(err);
    }

    #[tokio::test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    async fn ws_upgrade_is_refused_from_a_disallowed_origin_even_with_a_good_ticket() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id_term = open_stream(&state, &open_req(&repo, "term"))
            .unwrap()
            .context
            .stream_id;
        let info = create_test_terminal(&state, &id_term, "room").await;
        let addr = spawn_terminal_ws_server(state.clone()).await;
        let ticket = mint_ticket_for(&state, &id_term, &info.terminal_id);

        let req = terminal_ws_request(
            addr,
            &id_term,
            &info.terminal_id,
            &ticket,
            Some("https://evil.example"),
        );
        let err = tokio_tungstenite::connect_async(req).await.unwrap_err();
        assert_ws_forbidden(err);
    }

    #[tokio::test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    async fn ws_upgrade_succeeds_once_then_the_same_ticket_is_refused_a_second_time() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id_term = open_stream(&state, &open_req(&repo, "term"))
            .unwrap()
            .context
            .stream_id;
        let info = create_test_terminal(&state, &id_term, "room").await;
        let addr = spawn_terminal_ws_server(state.clone()).await;
        let ticket = mint_ticket_for(&state, &id_term, &info.terminal_id);

        let req = terminal_ws_request(
            addr,
            &id_term,
            &info.terminal_id,
            &ticket,
            Some("http://localhost:1420"),
        );
        let mut ws = attach(req).await;
        let _ = futures_util::SinkExt::close(&mut ws).await;

        let req2 = terminal_ws_request(
            addr,
            &id_term,
            &info.terminal_id,
            &ticket,
            Some("http://localhost:1420"),
        );
        let err = tokio_tungstenite::connect_async(req2).await.unwrap_err();
        assert_ws_forbidden(err);
    }

    #[tokio::test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    async fn ws_upgrade_is_refused_once_retiring_is_set() {
        let _turn = shell_turn().await;
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id_term = open_stream(&state, &open_req(&repo, "term"))
            .unwrap()
            .context
            .stream_id;
        let info = create_test_terminal(&state, &id_term, "room").await;
        let addr = spawn_terminal_ws_server(state.clone()).await;
        let ticket = mint_ticket_for(&state, &id_term, &info.terminal_id);
        state
            .lifecycle
            .lock()
            .unwrap()
            .retiring
            .insert(id_term.clone());

        let req = terminal_ws_request(
            addr,
            &id_term,
            &info.terminal_id,
            &ticket,
            Some("http://localhost:1420"),
        );
        let err = tokio_tungstenite::connect_async(req).await.unwrap_err();
        match err {
            tokio_tungstenite::tungstenite::Error::Http(resp) => {
                assert_eq!(
                    resp.status(),
                    StatusCode::CONFLICT,
                    "body: {:?}",
                    resp.body()
                );
            }
            other => panic!("expected an HTTP-level rejection, got {other:?}"),
        }
    }

    /// 07b-B proof: room mode boots a real Seatbelt-boxed shell over the WS
    /// even though the workspace has no `mise.toml` at all.
    #[tokio::test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    async fn room_mode_ws_boots_a_real_shell_with_no_mise_toml() {
        let _turn = shell_turn().await;
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message as WsMessage;

        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo")); // deliberately no mise.toml
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id_term = open_stream(&state, &open_req(&repo, "term"))
            .unwrap()
            .context
            .stream_id;
        let info = create_test_terminal(&state, &id_term, "room").await;
        let addr = spawn_terminal_ws_server(state.clone()).await;
        let ticket = mint_ticket_for(&state, &id_term, &info.terminal_id);

        let req = terminal_ws_request(
            addr,
            &id_term,
            &info.terminal_id,
            &ticket,
            Some("http://localhost:1420"),
        );
        let mut ws = attach(req).await;

        ws.send(WsMessage::Binary(b"echo THIN_ROOM_OK\n".to_vec().into()))
            .await
            .unwrap();

        let mut out = String::new();
        loop {
            let msg = tokio::time::timeout(Duration::from_secs(60), ws.next())
                .await
                .unwrap_or_else(|_| {
                    panic!("room shell must echo output within 60s; got so far: {out:?}")
                })
                .expect("socket closed before the echo was seen")
                .unwrap();
            if let WsMessage::Binary(b) = msg {
                out.push_str(&String::from_utf8_lossy(&b));
            }
            if out.lines().any(|l| l.trim() == "THIN_ROOM_OK") {
                break;
            }
        }

        let _ = ws.close(None).await;
        terminals::kill_terminal(&state, &info.terminal_id)
            .await
            .unwrap();
    }

    /// 07d-A3D1: a process exiting on its own while a WS is attached must
    /// close with the real exit code, not hang — `output_tx` lives inside
    /// the shared `TerminalHandle`, so this WS's own `Arc` clone keeps the
    /// broadcast channel from ever reporting `Closed` on its own; the
    /// liveness watch is what actually unblocks `attach_socket`'s loop.
    #[tokio::test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs a macOS PTY/shell path")]
    async fn a_natural_exit_while_attached_closes_the_socket_with_the_real_exit_code() {
        let _turn = shell_turn().await;
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message as WsMessage;

        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id_term = open_stream(&state, &open_req(&repo, "term"))
            .unwrap()
            .context
            .stream_id;
        let info = create_test_terminal(&state, &id_term, "host").await;
        let addr = spawn_terminal_ws_server(state.clone()).await;
        let ticket = mint_ticket_for(&state, &id_term, &info.terminal_id);

        let req = terminal_ws_request(
            addr,
            &id_term,
            &info.terminal_id,
            &ticket,
            Some("http://localhost:1420"),
        );
        let mut ws = attach(req).await;

        ws.send(WsMessage::Binary(b"exit 7\n".to_vec().into()))
            .await
            .unwrap();

        let close = tokio::time::timeout(Duration::from_secs(60), async {
            loop {
                match ws.next().await {
                    Some(Ok(WsMessage::Close(frame))) => return frame,
                    Some(Ok(_)) => continue,
                    Some(Err(e)) => panic!("ws error waiting for close: {e}"),
                    None => panic!("socket ended with no close frame at all"),
                }
            }
        })
        .await
        .expect("must close within 60s of the shell exiting on its own, not hang forever");

        let frame = close.expect("must carry a real close reason, not a bare close");
        assert_eq!(
            frame.reason.to_string(),
            "exited:7",
            "must name the real exit code"
        );
    }

    /// The operator's own shell on this Mac opens from anywhere: a login
    /// shell in their home on `sys:host`, listed there, and never an agent's
    /// to open, see or reach: to an agent, `sys:host` has no terminals.
    #[tokio::test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs a macOS PTY/shell path")]
    async fn a_host_terminal_opens_from_anywhere_in_home() {
        let _turn = shell_turn().await;
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let host = || terminals::HOST_STREAM_ID.to_string();
        let ask = || CreateReq { stream_id: host(), name: None, mode: "host".into(), command: None };
        let info = terminal_create(State(state.clone()), Json(ask())).await.unwrap().0;
        assert_eq!((info.stream_id.as_str(), info.mode.as_str()), (terminals::HOST_STREAM_ID, "host"));
        assert_eq!(info.cwd, std::env::var("HOME").unwrap());
        let listed = terminal_list(State(state.clone()), Query(ListQuery { stream_id: host() })).await.0;
        assert!(listed.iter().any(|t| t.terminal_id == info.terminal_id));
        let agent = crate::actor::agent("wt:x", crate::node::Seat::Orchestrator);
        let refused = crate::actor::scope(agent.clone(), terminal_create(State(state.clone()), Json(ask()))).await;
        assert_eq!(refused.unwrap_err().0, StatusCode::FORBIDDEN);

        let seen = crate::actor::scope(agent.clone(), terminal_list(State(state.clone()), Query(ListQuery { stream_id: host() }))).await.0;
        assert!(seen.is_empty(), "an agent sees none: {seen:?}");
        let id = info.terminal_id.clone();
        let read = crate::actor::scope(
            agent.clone(),
            terminal_read(State(state.clone()), Query(ReadQuery { stream_id: host(), terminal_id: id.clone(), max_bytes: None })),
        )
        .await;
        assert_eq!(read.err().map(|e| e.0), Some(StatusCode::NOT_FOUND));
        let ticket = crate::actor::scope(agent.clone(), terminal_ticket(State(state.clone()), Json(TicketReq { stream_id: host(), terminal_id: id.clone() }))).await;
        assert_eq!(ticket.err().map(|e| e.0), Some(StatusCode::NOT_FOUND));
        let renamed = crate::actor::scope(
            agent.clone(),
            terminal_rename(State(state.clone()), Json(RenameReq { stream_id: host(), terminal_id: id.clone(), name: "x".into() })),
        )
        .await;
        assert_eq!(renamed.err().map(|e| e.0), Some(StatusCode::NOT_FOUND));
        let closed = crate::actor::scope(agent.clone(), terminal_close(State(state.clone()), Json(CloseReq { stream_id: host(), terminal_id: id.clone() }))).await;
        assert_eq!(closed.err().map(|e| e.0), Some(StatusCode::NOT_FOUND));
        let still = state.terminals.lock().unwrap().get(&id).unwrap();
        assert!(still.is_alive() && still.info().name == info.name, "still the operator's, untouched");
        terminals::kill_terminal(&state, &info.terminal_id).await.unwrap();
    }

    /// A project's own terminal opens in its main folder, always boxed to
    /// the project: its room with no sandbox set and with the Mac sandbox,
    /// where its own folder lists and its parent doesn't. Listed on its
    /// `project:<id>` stream; an unknown project has none.
    #[tokio::test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    async fn a_project_terminal_opens_where_its_sandbox_runs_work() {
        let _turn = shell_turn().await;
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let app = dir.path().join("app");
        std::fs::create_dir_all(&app).unwrap();
        let root = crate::catalog::DirectoryRootSpec::Attach(app.to_str().unwrap());
        let (project, _) = crate::catalog::create_project(&state, "app", Some(root)).unwrap();
        let folder = crate::catalog::project_by_id(&state, &project.project_id).unwrap().roots[0].path.clone();
        let stream = format!("project:{}", project.project_id);
        let open = |stream: String| {
            let req = CreateReq { stream_id: stream, name: None, mode: "host".into(), command: None };
            terminal_create(State(state.clone()), Json(req))
        };
        let plain = open(stream.clone()).await.unwrap().0;
        assert_eq!((plain.mode.as_str(), plain.cwd.as_str()), ("room", folder.as_str()), "never the whole Mac");
        let stacks = crate::node::ProjectStacks { sandbox: "mac".to_string(), ..Default::default() };
        crate::catalog::set_project_stacks(&state, &project.project_id, stacks).unwrap();
        let room = open(stream.clone()).await.unwrap().0;
        assert_eq!((room.mode.as_str(), room.cwd.as_str()), ("room", folder.as_str()));

        // The room it opens: its own folder lists, its parent doesn't.
        let at = std::path::Path::new(&folder);
        let env = bakery_exec::ensure_thin_room(at, &cache_root(&state), &stream).unwrap();
        let shell = room_shell(&state, &stream, &env, at).unwrap();
        std::fs::write(at.join("mine.txt"), "").unwrap();
        let list = |what: &str| {
            let mut args = shell.args[..shell.args.len() - 2].to_vec();
            args.extend(["/bin/ls".to_string(), what.to_string()]);
            std::process::Command::new(&shell.program).args(&args).current_dir(at).output().unwrap()
        };
        let own = list(".");
        assert!(String::from_utf8_lossy(&own.stdout).contains("mine.txt"), "{}", String::from_utf8_lossy(&own.stderr));
        let parent = list("..");
        assert!(!parent.status.success(), "the parent listed: {}", String::from_utf8_lossy(&parent.stdout));
        assert!(String::from_utf8_lossy(&parent.stderr).contains("Operation not permitted"), "{}", String::from_utf8_lossy(&parent.stderr));
        let listed = terminal_list(State(state.clone()), Query(ListQuery { stream_id: stream.clone() })).await.0;
        assert_eq!(listed.len(), 2);
        assert_eq!(open("project:nope".into()).await.unwrap_err().0, StatusCode::NOT_FOUND);

        // Removed, the project takes its terminals with it.
        let retire = crate::routes::project_catalog::RetireProjectReq { project_id: project.project_id.clone() };
        let _ = crate::routes::project_catalog::project_retire(State(state.clone()), Json(retire)).await.unwrap();
        let alive = |id: &str| state.terminals.lock().unwrap().get(id).is_some_and(|t| t.is_alive());
        assert!(!alive(&plain.terminal_id) && !alive(&room.terminal_id), "no shell outlives its project");
        assert!(terminal_list(State(state.clone()), Query(ListQuery { stream_id: stream })).await.0.iter().all(|t| !t.live));
    }

    // ─── The ownership inversion itself ─────────────────────────────────────

    async fn background_hup_ignoring_job(
        ws: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    ) -> libc::pid_t {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message as WsMessage;

        let cmd = "(trap '' HUP; sleep 30) & echo GOT_PID=$!\n";
        ws.send(WsMessage::Binary(cmd.as_bytes().to_vec().into()))
            .await
            .unwrap();

        let mut out = String::new();
        loop {
            let msg = tokio::time::timeout(Duration::from_secs(60), ws.next())
                .await
                .unwrap_or_else(|_| {
                    panic!("pty must echo the backgrounded pid within 60s; got so far: {out:?}")
                })
                .expect("socket closed before pid was seen")
                .unwrap();
            if let WsMessage::Binary(b) = msg {
                out.push_str(&String::from_utf8_lossy(&b));
            }
            if let Some(line) = out.lines().find(|l| {
                l.rsplit("GOT_PID=")
                    .next()
                    .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()))
            }) {
                let digits: String = line
                    .rsplit("GOT_PID=")
                    .next()
                    .unwrap()
                    .chars()
                    .take_while(|c| c.is_ascii_digit())
                    .collect();
                if let Ok(p) = digits.parse() {
                    return p;
                }
            }
        }
    }

    fn is_alive(pid: libc::pid_t) -> bool {
        unsafe { libc::kill(pid, 0) == 0 }
    }

    async fn wait_until_dead(pid: libc::pid_t) {
        for _ in 0..300 {
            if !is_alive(pid) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// The whole point of 07c-STUDIO-SHELL §3: closing the pane (a WS
    /// disconnect) must NOT kill the session — inverse of the pre-07c
    /// behavior, where the pty died with its one WebSocket.
    #[tokio::test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs a macOS PTY/shell path")]
    async fn detaching_the_websocket_does_not_kill_a_backgrounded_descendant() {
        let _turn = shell_turn().await;
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id_term = open_stream(&state, &open_req(&repo, "term"))
            .unwrap()
            .context
            .stream_id;
        let info = create_test_terminal(&state, &id_term, "host").await;
        let addr = spawn_terminal_ws_server(state.clone()).await;
        let ticket = mint_ticket_for(&state, &id_term, &info.terminal_id);

        let req = terminal_ws_request(
            addr,
            &id_term,
            &info.terminal_id,
            &ticket,
            Some("http://localhost:1420"),
        );
        let mut ws = attach(req).await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        let pid = background_hup_ignoring_job(&mut ws).await;

        let _ = ws.close(None).await;
        drop(ws);
        tokio::time::sleep(Duration::from_millis(500)).await;

        assert!(
            is_alive(pid),
            "detaching a view must never kill the terminal's process tree"
        );
        assert!(
            state
                .terminals
                .lock()
                .unwrap()
                .get(&info.terminal_id)
                .is_some_and(|h| h.is_alive()),
            "the session must still be registered and alive after a plain disconnect"
        );

        terminals::kill_terminal(&state, &info.terminal_id)
            .await
            .unwrap();
        wait_until_dead(pid).await;
        assert!(
            !is_alive(pid),
            "sanity: an explicit close afterward must still be able to kill it"
        );
    }

    /// The other half: reattaching after a detach replays what was missed,
    /// then keeps following live — a pane hidden while `celery`/`fastapi`
    /// kept printing must not have lost any of it.
    #[tokio::test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    async fn reattaching_after_detach_replays_buffered_output_then_continues_live() {
        let _turn = shell_turn().await;
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message as WsMessage;

        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id_term = open_stream(&state, &open_req(&repo, "term"))
            .unwrap()
            .context
            .stream_id;
        let info = create_test_terminal(&state, &id_term, "room").await;
        let addr = spawn_terminal_ws_server(state.clone()).await;

        // First attach: print something, then detach without closing the terminal.
        let ticket1 = mint_ticket_for(&state, &id_term, &info.terminal_id);
        let req1 = terminal_ws_request(
            addr,
            &id_term,
            &info.terminal_id,
            &ticket1,
            Some("http://localhost:1420"),
        );
        let mut ws1 = attach(req1).await;
        ws1.send(WsMessage::Binary(
            b"echo BUFFERED_WHILE_HIDDEN\n".to_vec().into(),
        ))
        .await
        .unwrap();
        let mut seen_buffered = false;
        while !seen_buffered {
            let msg = tokio::time::timeout(Duration::from_secs(60), ws1.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            if let WsMessage::Binary(b) = msg {
                if String::from_utf8_lossy(&b)
                    .lines()
                    .any(|l| l.trim() == "BUFFERED_WHILE_HIDDEN")
                {
                    seen_buffered = true;
                }
            }
        }
        let _ = ws1.close(None).await;
        drop(ws1);

        // Second attach (a fresh ws — "reopen the pane"): must replay the
        // line printed while nobody was attached, since the shell can go on
        // producing prompt/output even without a live viewer.
        tokio::time::sleep(Duration::from_millis(200)).await;
        let ticket2 = mint_ticket_for(&state, &id_term, &info.terminal_id);
        let req2 = terminal_ws_request(
            addr,
            &id_term,
            &info.terminal_id,
            &ticket2,
            Some("http://localhost:1420"),
        );
        let mut ws2 = attach(req2).await;

        let mut out = String::new();
        while !out.contains("BUFFERED_WHILE_HIDDEN") {
            let msg = tokio::time::timeout(Duration::from_secs(60), ws2.next())
                .await
                .unwrap_or_else(|_| {
                    panic!("reattach must replay buffered output within 60s; got so far: {out:?}")
                })
                .unwrap()
                .unwrap();
            if let WsMessage::Binary(b) = msg {
                out.push_str(&String::from_utf8_lossy(&b));
            }
        }

        // Now prove it keeps following live too.
        ws2.send(WsMessage::Binary(
            b"echo LIVE_AFTER_REATTACH\n".to_vec().into(),
        ))
        .await
        .unwrap();
        let mut saw_live = false;
        let mut out2 = String::new();
        while !saw_live {
            let msg = tokio::time::timeout(Duration::from_secs(60), ws2.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            if let WsMessage::Binary(b) = msg {
                out2.push_str(&String::from_utf8_lossy(&b));
            }
            if out2.lines().any(|l| l.trim() == "LIVE_AFTER_REATTACH") {
                saw_live = true;
            }
        }

        let _ = ws2.close(None).await;
        terminals::kill_terminal(&state, &info.terminal_id)
            .await
            .unwrap();
    }

    /// Explicit close is the one thing that's actually supposed to kill it.
    #[tokio::test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs a macOS PTY/shell path")]
    async fn explicit_close_kills_a_backgrounded_descendant() {
        let _turn = shell_turn().await;
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id_term = open_stream(&state, &open_req(&repo, "term"))
            .unwrap()
            .context
            .stream_id;
        let info = create_test_terminal(&state, &id_term, "host").await;
        let addr = spawn_terminal_ws_server(state.clone()).await;
        let ticket = mint_ticket_for(&state, &id_term, &info.terminal_id);

        let req = terminal_ws_request(
            addr,
            &id_term,
            &info.terminal_id,
            &ticket,
            Some("http://localhost:1420"),
        );
        let mut ws = attach(req).await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        let pid = background_hup_ignoring_job(&mut ws).await;

        terminal_close(
            State(state.clone()),
            Json(CloseReq {
                stream_id: id_term.clone(),
                terminal_id: info.terminal_id.clone(),
            }),
        )
        .await
        .unwrap()
        .0;

        wait_until_dead(pid).await;
        assert!(
            !is_alive(pid),
            "explicit close must kill the whole backgrounded job, not just detach"
        );
        assert!(
            state
                .terminals
                .lock()
                .unwrap()
                .get(&info.terminal_id)
                .is_none(),
            "closed terminal must leave the registry"
        );

        let _ = ws.close(None).await;
    }

    // ─── discard / burn integration ─────────────────────────────────────────

    #[tokio::test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs a macOS PTY/shell path")]
    async fn discard_kills_a_live_terminal_session_before_removing_the_worktree() {
        let _turn = shell_turn().await;
        use crate::routes::lifecycle::{discard_stream, DiscardReq};

        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let resp = open_stream(&state, &open_req(&repo, "term")).unwrap();
        let worktree_path = std::path::PathBuf::from(&resp.context.worktree_path);
        let info = create_test_terminal(&state, &resp.context.stream_id, "host").await;
        let addr = spawn_terminal_ws_server(state.clone()).await;
        let ticket = mint_ticket_for(&state, &resp.context.stream_id, &info.terminal_id);

        let req = terminal_ws_request(
            addr,
            &resp.context.stream_id,
            &info.terminal_id,
            &ticket,
            Some("http://localhost:1420"),
        );
        let mut ws = attach(req).await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        let pid = background_hup_ignoring_job(&mut ws).await;
        assert!(
            is_alive(pid),
            "sanity: the backgrounded job must be alive before discard"
        );

        let discard = discard_stream(
            State(state.clone()),
            Json(DiscardReq {
                stream_id: resp.context.stream_id.clone(),
                confirm: false,
            }),
        )
        .await
        .unwrap()
        .0;

        assert!(discard.worktree_removed);
        assert!(!worktree_path.is_dir(), "worktree must actually be gone");
        wait_until_dead(pid).await;
        assert!(
            !is_alive(pid),
            "the live terminal session's whole job must be dead before/because of discard"
        );
        assert!(
            state
                .terminals
                .lock()
                .unwrap()
                .get(&info.terminal_id)
                .is_none(),
            "the discarded terminal must have deregistered itself"
        );
        assert!(
            terminal_list(
                State(state.clone()),
                Query(ListQuery {
                    stream_id: resp.context.stream_id.clone()
                })
            )
            .await
            .0
            .is_empty(),
            "no leftover row for a discarded socket's terminals"
        );

        let _ = ws.close(None).await;
    }

    #[tokio::test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs a macOS PTY/shell path")]
    async fn setting_lifetime_to_burn_kills_an_existing_host_terminal() {
        let _turn = shell_turn().await;
        use crate::routes::lifecycle::{set_lifetime, LifetimeReq};

        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id_term = open_stream(&state, &open_req(&repo, "term"))
            .unwrap()
            .context
            .stream_id;
        let info = create_test_terminal(&state, &id_term, "host").await;
        let addr = spawn_terminal_ws_server(state.clone()).await;
        let ticket = mint_ticket_for(&state, &id_term, &info.terminal_id);

        let req = terminal_ws_request(
            addr,
            &id_term,
            &info.terminal_id,
            &ticket,
            Some("http://localhost:1420"),
        );
        let mut ws = attach(req).await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        let pid = background_hup_ignoring_job(&mut ws).await;

        let _ = set_lifetime(
            State(state.clone()),
            Json(LifetimeReq {
                stream_id: id_term.clone(),
                lifetime: Lifetime::Burn,
            }),
        )
        .await
        .unwrap();

        wait_until_dead(pid).await;
        assert!(
            !is_alive(pid),
            "an existing host terminal must die the instant the stream becomes burn"
        );

        let _ = ws.close(None).await;
    }
}

#[cfg(test)]
mod prepared_tests {
    use super::*;
    use crate::routes::project::open_stream;
    use crate::testutil::*;
    use bakery_agent::StubModel;
    use std::sync::Arc;

    /// An agent that asks for a host terminal is refused before a shell
    /// starts. The operator's own call is not that refusal.
    #[tokio::test]
    async fn an_agent_cannot_open_a_host_terminal() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let refused = crate::actor::scope(
            "agent:wt:s:orchestrator".into(),
            spawn_shell(&state, "wt:s".into(), "host".into(), None),
        )
        .await
        .unwrap_err();
        assert_eq!(refused.0, StatusCode::FORBIDDEN);
        let room = crate::actor::scope(
            "agent:wt:s:orchestrator".into(),
            spawn_shell(&state, "no-such-stream".into(), "room".into(), None),
        )
        .await
        .unwrap_err();
        assert_ne!(room.0, StatusCode::FORBIDDEN, "a room terminal is not the host refusal");
    }

    /// The operator's shell in a thread's room finds what the thread's
    /// agents run by name: git, and Homebrew's rg and fd wherever this Mac
    /// has them.
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn a_room_shell_runs_the_agents_toolchains_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let sid = open_stream(&state, &open_req(&repo, "tools")).unwrap().context.stream_id;
        let workspace = jail_for(&state, &sid).unwrap().root().to_path_buf();
        let env = bakery_exec::ensure_thin_room(&workspace, &cache_root(&state), &sid).unwrap();
        let shell = room_shell(&state, &sid, &env, &workspace).unwrap();

        let brew = ["/opt/homebrew/bin", "/usr/local/bin"].map(std::path::Path::new).into_iter().find(|b| b.join("brew").is_file());
        let mine = |tool: &&str| brew.is_some_and(|b| b.join(tool).is_file());
        let wanted: Vec<&str> = ["git"].into_iter().chain(["rg", "fd"].into_iter().filter(|t| mine(t))).collect();
        let script: Vec<String> = wanted.iter().map(|t| format!("{t} --version >/dev/null && echo {t}")).collect();
        let mut args = shell.args[..shell.args.len() - 2].to_vec();
        args.extend(["/bin/sh".to_string(), "-c".to_string(), script.join("; ")]);
        let out = std::process::Command::new(&shell.program).args(&args).current_dir(&workspace).output().unwrap();
        let found: Vec<String> = String::from_utf8_lossy(&out.stdout).lines().map(str::to_string).collect();
        assert_eq!(found, wanted, "{}", String::from_utf8_lossy(&out.stderr));
    }
}
