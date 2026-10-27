//! bakery-node — the persistent daemon.
//!
//! A standalone process (NOT the Tauri client) that owns the SQLite Heart and the
//! event streams. Exposes a localhost HTTP API for commands + a WebSocket that
//! pushes every appended event to connected clients (the thin-client projection).
//!
//! Scope so far: project inspection + open/attach (Project → Branch → Worktree),
//! recents, a bounded agent tool loop jailed to the worktree, and the Manual
//! accept/reject write gate. This node is the **local execution plane**: it may
//! read any path the OS user can, while *agent* tools stay jailed (`tools.rs`).
//! Multi-node handoff is stance + hooks only — see `node.rs` and `MULTI_NODE.md`.

mod account_adapter;
mod actor;
mod adapter_pin;
mod agent_control;
mod agents;
mod agent_loop;
mod api_catalog;
#[cfg(test)]
mod api_client;
mod api_tools;
mod ask;
mod attachments;
mod baker_ops;
mod baker_threads;
mod builds;
mod catalog;
mod claude_native;
mod claude_providers;
mod claude_sign_in;
mod control_plane;
mod containers;
mod control;
mod deletes;
mod desktop;
mod doors;
mod editor_style;
mod devices;
mod disk;
mod doctor;
mod engine;
mod git;
mod grok_cli;
mod git_hooks;
mod git_trash;
mod grants;
mod host_list;
mod iphone;
mod secret_reads;
mod service;
mod services;
mod swap;
mod swap_tls;
mod this_mac;
mod provenance;
mod jobs;
mod mac_sandbox;
mod machine_access;
mod mcp;
mod mcp_shim;
mod model_catalog;
mod network;
mod node;
mod operator_auth;
mod operator_tools;
mod outside_window;
mod overlaps;
mod pipe;
mod remote;
mod remote_ask;
mod remote_helpers;
mod backlog;
mod briefs;
mod learning;
mod memory_index;
mod memory_review;
mod messages;
mod move_in;
mod workspaces;
mod move_in_history;
mod fresh_start;
mod toolkit;
mod toolkit_port;
mod resume;
mod routes;
mod seat_container;
mod views;
mod writers;
mod model_proxy;
mod snapshots;
mod search;
mod selection;
mod seat_gateway;
mod state;
mod task_driver;
mod task_review;
mod task_ship;
mod tasks;
mod terminals;
#[cfg(test)]
pub mod testutil;
mod thread_board;
mod tools;
mod versions;
mod xcode_env;

use axum::{
    http::{HeaderValue, Method},
    routing::{get, post},
    Router,
};
use bakery_agent::resolve_model;
use node::NodeConfig;
use state::AppState;
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tokio::sync::broadcast;
use tower_http::cors::{AllowOrigin, Any, CorsLayer};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // `bakery-node mcp-shim` (07d-B Phase 2 Stage C item 1): a loopback MCP
    // server over stdio, spawned by a downstream ACP CLI per
    // `NewSessionRequest::mcp_servers` — not the daemon. Checked before any
    // daemon setup (HOME, Heart, the HTTP listener) so the shim never pays
    // for state it does not use.
    if std::env::args().nth(1).as_deref() == Some("mcp-shim") {
        return mcp_shim::run_from_env();
    }
    // In an agent's room, this binary also stands in for a push and for gh
    // (`remote_ask`), started under those names.
    let invoked = std::env::args().next().unwrap_or_default();
    match std::path::Path::new(&invoked).file_name().and_then(|n| n.to_str()) {
        Some(remote_ask::HELPER) => return remote_helpers::git_remote_main(),
        Some("gh") => return remote_helpers::gh_main(),
        _ => {}
    }

    versions::mark_started();
    let home = std::env::var("HOME").map_err(|_| "HOME not set")?;
    let base = PathBuf::from(home).join(".bakery");
    std::fs::create_dir_all(&base)?;
    // From here on, what it prints goes to ~/.bakery/logs/node.log.
    if let Err(e) = service::capture_output(&base) {
        eprintln!("bakery-node: its own log could not start: {e}");
    }
    let worktree_base = base.join("worktrees");
    std::fs::create_dir_all(&worktree_base)?;

    // The port first: it is the one-node-at-a-time gate, so a second node
    // stops here, before it touches the Heart. Loopback 7717 by default;
    // `BAKERY_NODE_ADDR` runs a second node side by side (smoke tests, and
    // eventually a real second machine).
    let addr = std::env::var("BAKERY_NODE_ADDR").unwrap_or_else(|_| "127.0.0.1:7717".to_string());
    let listener = tokio::net::TcpListener::bind(&addr).await?;

    // A fresh start the operator armed (`scripts/bakery.sh fresh-start`):
    // the old Heart set aside, closed, before this node opens one. A
    // failure after that puts everything back and stops the node.
    let fresh = fresh_start::set_aside(&base);
    let mut heart = match bakery_heart::Heart::open(base.join("heart.db").to_string_lossy().as_ref()) {
        Ok(heart) => heart,
        Err(e) => {
            if let Some(pending) = &fresh {
                fresh_start::roll_back(&base, pending, &e.to_string());
            }
            return Err(e.into());
        }
    };
    let (studio_input_gen, studio_cache) = routes::studio::studio_watch(&mut heart);
    // Room for a burst (a long native turn's tool events); a socket that
    // still falls behind is told (`events-lagged`) and reloads.
    let (tx, _rx) = broadcast::channel::<String>(8192);

    let model = resolve_model();
    println!("bakery-node model: {}", model.name());

    let mut node_cfg = NodeConfig::load_or_create(&base);
    // One-time (or re-detect) pin of Baker's repo so the operator never has to
    // re-export BAKERY_SOURCE_ROOT every shell. Env still wins for this process
    // and is persisted below on first baker assign / resolve.
    if node_cfg
        .bakery_source_root
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .is_empty()
    {
        let from_env = std::env::var("BAKERY_SOURCE_ROOT")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let pinned = from_env.or_else(|| {
            std::env::current_dir()
                .ok()
                .and_then(|cwd| routes::baker::detect_bakery_source_root(&cwd))
                .map(|p| p.to_string_lossy().into_owned())
        });
        if let Some(root) = pinned {
            node_cfg.bakery_source_root = Some(root.clone());
            if let Err(e) = node_cfg.save(&base) {
                eprintln!("bakery-node: could not persist bakery_source_root: {e}");
            } else {
                println!("bakery-node bakery_source_root: {root} (saved to node.json)");
            }
        }
    } else if let Some(r) = node_cfg.bakery_source_root.as_deref() {
        println!("bakery-node bakery_source_root: {r}");
    }

    println!(
        "bakery-node id: {} ({}) — stream_fallback: {:?}",
        node_cfg.node_id, node_cfg.hostname, node_cfg.stream_fallback
    );

    let operator_token = state::resolve_operator_token(&node_cfg);
    if operator_token.is_empty() {
        eprintln!(
            "bakery-node: WARNING — empty operator token; every authenticated route will refuse"
        );
    } else if std::env::var("BAKERY_OPERATOR_TOKEN").is_ok() {
        println!("bakery-node operator token: from BAKERY_OPERATOR_TOKEN env");
    }

    let catalog_cache = Arc::new(model_catalog::ModelCatalogCache::persisted(
        base.join(model_catalog::MEMORY_FILE),
    ));
    let state = AppState {
        heart: Arc::new(Mutex::new(heart)),
        tx,
        base: Arc::new(base),
        worktree_base: Arc::new(worktree_base),
        model: Arc::new(Mutex::new(model)),
        node: Arc::new(Mutex::new(node_cfg)),
        pty_tickets: Arc::new(Mutex::new(Default::default())),
        lifecycle: Arc::new(Mutex::new(Default::default())),
        terminals: Arc::new(Mutex::new(Default::default())),
        seats: Arc::new(Mutex::new(Default::default())),
        child_tool_allow_once: Arc::new(Mutex::new(Default::default())),
        acp_sessions: Arc::new(Mutex::new(Default::default())),
        system_login_lock: Arc::new(tokio::sync::Mutex::new(())),
        edit_decision_lock: Arc::new(Mutex::new(())),
        baker_bootstrap_lock: Arc::new(Mutex::new(())),
        catalog_cache,
        agent_activity: Arc::new(Mutex::new(Default::default())),
        queue_dispatchers: Arc::new(Mutex::new(Default::default())),
        mcp_tokens: Arc::new(Mutex::new(Default::default())),
        task_drivers: Arc::new(Default::default()),
        task_diff_cache: Arc::new(Mutex::new(Default::default())),
        studio_input_gen,
        studio_cache,
        compile_jobs: builds::compile_jobserver(),
        operator_token: Arc::new(operator_token),
        decision_waiters: Arc::new(Mutex::new(Default::default())),
        events_tickets: Arc::new(Mutex::new(Default::default())),
        // `main.rs`'s own binary target is still compiled (not run) under
        // `cargo test`, where `AppState::test_default_model` exists — see
        // that field's doc comment on why a live node has no real value here.
        #[cfg(test)]
        test_default_model: None,
    };

    if let Some(pending) = fresh {
        match fresh_start::carry(&state, &pending) {
            Ok(()) => {
                fresh_start::finished(&state.base);
                println!("bakery-node: a fresh start: a new Heart, with the kept thread carried over");
            }
            Err(e) => {
                fresh_start::roll_back(&state.base, &pending, &e);
                std::process::exit(1);
            }
        }
    }

    // 07d-B Phase 4: every task recorded `running` from a prior process is
    // honestly dead — mark it `blocked` before this node accepts any
    // traffic (Checkpoint 2 item 7: "restart Bakery... running turns that
    // died with the process are honestly blocked/restartable, not silently
    // lost or falsely running").
    task_driver::recover_on_boot(&state);
    // Conversations' turns the restart cut off carry on by themselves; their
    // queued messages follow them.
    let resumed = resume::on_boot(&state);
    // Composer follow-ups are Heart facts, not websocket state. Recreate
    // their tiny in-memory dispatch guards after a node restart.
    agent_control::recover_on_boot(&state, &resumed);
    // The operator's phones and nodes, over Tailscale, when they turned it on.
    tokio::spawn(devices::start_on_boot(state.clone()));
    // All day: the Mac stays awake while agents work, and a restart waits
    // for their turns.
    service::keep_awake_while_working(state.clone());
    service::restart_on_signal(state.clone());
    // A secret-named file a room was refused becomes the operator's card.
    secret_reads::watch(state.clone());
    // Copies of a service a restart cut off from their turns go.
    services::sweep_copies(&state);
    // Agents' new turns wait while this Mac runs hot.
    this_mac::guard(state.clone());
    // The resource pill's meters, read every few seconds.
    this_mac::meters_watch(state.clone());
    // The app hears when free disk space runs low.
    disk::guard(state.clone());
    // Colima's default VM: its peaks kept, and stopped while idle.
    engine::watch(state.clone());
    // The model proxy runs once the operator has installed it (Settings).
    model_proxy::start(state.clone());
    // The seat image is rebuilt in the background when it's wanted, after
    // any deploy that started this node.
    seat_container::keep_image_current(state.clone());
    // Rule 2: every repository with an open thread stays fetched.
    routes::git::keep_fetched(state.clone());
    // The GitHub inbox asks again on its own, faster while a window shows.
    routes::github_prs::keep_fresh(state.clone());
    // The memory's learning loop: thread-end reviews and the nightly run.
    learning::keep_learning(state.clone());

    // No pairing step and nothing to copy: Bakery's desktop shell reads the
    // operator credential from this node's own `node.json` natively (see
    // `bakery-client/src-tauri/src/connect.rs`) and proves it before the
    // window opens. The credential is never printed, never put in a URL,
    // and never handed to an agent.
    let app = build_app(state);

    println!("bakery-node listening on http://{addr}  (ws: /events)");
    axum::serve(listener, app).await?;
    Ok(())
}

/// The complete production router — every route plus the CORS and
/// operator-auth layers — extracted so integration tests can spawn the
/// EXACT same app on an ephemeral port and prove the auth layer over real
/// HTTP (`operator_auth`'s route-level tests), instead of testing a
/// hand-built lookalike that could drift.
///
/// Loopback bind stops remote hosts; CORS stops drive-by browser pages on
/// the same machine from calling /agent/accept or /policy/write-mode
/// (Origin is browser-set and not forgeable by page JS — explicit
/// allowlist; `state::allowed_origins()` is the one list the PTY WS check
/// reads too). The operator-auth layer inside it is what stops arbitrary
/// LOCAL processes — including sandboxed ACP children, whose Seatbelt
/// allows network — from calling anything beyond /health and the
/// self-guarded MCP/ticketed-WS surfaces.
pub(crate) fn build_app(state: AppState) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::list(
            state::allowed_origins()
                .iter()
                .map(|o| o.parse::<HeaderValue>().unwrap()),
        ))
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
        .allow_headers(Any);

    api_routes()
        // Auth INSIDE CORS: preflight OPTIONS must succeed unauthenticated,
        // and every real request then passes the operator/agent-scope check
        // in `operator_auth` before any handler runs.
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            operator_auth::require_operator,
        ))
        .layer(cors)
        .with_state(state)
}

/// Every route this node serves, without the network layers. `build_app`
/// wraps it in operator auth and CORS; the Baker's tools (`api_tools`) call
/// the same handlers in-process, behind their own gate.
pub(crate) fn api_routes() -> Router<AppState> {
    Router::new()
        .route("/health", get(routes::operator::health))
        .route("/node", get(routes::operator::node_info))
        .route("/versions", get(versions::versions))
        .route("/agents", get(agents::agents))
        .route(
            "/agents/tool-policy",
            get(agents::tool_policy_view).post(agents::set_tool_policy),
        )
        .route("/agents/peer-limit", get(agents::peer_limit_view).post(agents::set_peer_limit))
        .route("/agents/always-allowed", get(agents::always_allowed))
        .route("/agents/always-allowed/remove", post(agents::remove_always_allowed))
        .route("/chat/create", post(routes::project_catalog::chat_create))
        .route("/chat/make-project", post(routes::project_catalog::chat_make_project))
        .route("/conversation/archive", post(agents::archive_conversation))
        .route("/conversation/rename", post(agents::rename_conversation))
        .route("/conversation/restart", post(agents::restart_runtime))
        .route("/agents/storage", get(agents::storage))
        .route("/agents/storage/list", get(agents::storage_list))
        .route("/agents/storage/file", get(agents::storage_file))
        .route("/agents/storage/delete", post(agents::storage_delete))
        .route(
            "/machine-access",
            get(machine_access::machine_access).post(machine_access::set_machine_access),
        )
        .route(
            "/machine-access/check",
            post(machine_access::check_machine_access),
        )
        .route(
            "/machine-access/requests/decide",
            post(machine_access::decide_access_request),
        )
        .route("/machine-access/reach", post(machine_access::set_reach))
        .route("/machine-access/containers", post(machine_access::set_containers))
        .route("/machine-access/seat-size", post(machine_access::set_seat_size))
        .route(
            "/machine-access/claude-container/sign-in",
            post(seat_container::sign_in),
        )
        .route("/containers/seat-image", get(seat_container::seat_image))
        .route("/containers/seat-image/build", post(seat_container::build_seat_image))
        .route("/containers", get(containers::list))
        .route("/containers/inspect", get(containers::inspect))
        .route("/containers/logs", get(containers::logs))
        .route("/containers/stats", get(containers::stats))
        .route("/containers/start", post(containers::start))
        .route("/containers/stop", post(containers::stop))
        .route("/containers/restart", post(containers::restart))
        .route("/containers/remove", post(containers::remove))
        .route("/containers/prune", post(containers::prune))
        .route("/containers/compose/up", post(containers::compose_up))
        .route("/containers/run", post(containers::run_container))
        .route("/code/open", post(views::code_open))
        .route("/code/close", post(views::code_close))
        .route("/code/appearance", post(editor_style::appearance))
        .route("/code/goto", post(views::code_goto))
        .route("/preview/open", post(views::preview_open))
        .route("/model-proxy", get(model_proxy::proxy_status))
        .route("/model-proxy/install", post(model_proxy::install_proxy))
        .route("/model-proxy/start", post(model_proxy::start_proxy))
        .route("/model-proxy/keys", get(model_proxy::list_keys).post(model_proxy::add_key_route))
        .route("/model-proxy/keys/remove", post(model_proxy::remove_key_route))
        .route("/metrics", get(model_proxy::metrics))
        .route("/agents/map", get(agents::agent_map))
        .route("/versions/check", post(versions::versions_check))
        .route(
            "/control/bakery",
            get(control::control_status).post(control::control_run),
        )
        .route("/control/restart/now", post(service::restart_now))
        .route("/mac/amphetamine", get(this_mac::amphetamine_route).post(this_mac::amphetamine_act))
        .route("/mac/fans/open", post(this_mac::open_fans))
        .route("/mac/heat", get(this_mac::heat_route))
        .route("/mac/meters", get(this_mac::meters_route))
        .route("/engine", get(engine::engine_route))
        .route("/engine/colima/idle-stop", post(engine::idle_stop_route))
        .route("/engine/colima/size", post(engine::size_route))
        .route("/engine/docker-default", post(engine::docker_default_route))
        .route("/engine/orbstack/install", post(engine::orbstack_install_route))
        .route("/disk", get(disk::disk_route))
        .route("/disk/clean", post(disk::clean_route))
        .route("/project/service/copy", post(services::copy_route))
        .route("/project/service/copy/stop", post(services::stop_route))
        .route("/files/read/asks", get(secret_reads::list))
        .route("/files/read/decide", post(secret_reads::decide_route))
        .route("/client/log", post(control::client_log))
        .route("/policy/write-mode", post(routes::agent::set_write_mode))
        .route("/project/inspect", post(routes::project::project_inspect))
        .route("/project/open", post(routes::project::project_open))
        .route(
            "/project/create",
            post(routes::project_catalog::project_create),
        )
        .route(
            "/project/rename",
            post(routes::project_catalog::project_rename),
        )
        .route("/project/pin", post(routes::project_catalog::project_pin))
        .route("/project/control", get(control_plane::control))
        .route("/project/setup", post(control_plane::set_setup))
        .route("/network/hosts", get(host_list::list))
        .route("/network/host/decide", post(host_list::decide_host))
        .route("/project/toolchain/keep", post(mac_sandbox::keep_route))
        .route("/project/toolchain/request", post(mac_sandbox::request_route))
        .route("/project/toolchain/decide", post(mac_sandbox::decide_route))
        .route("/thread/door", get(doors::door_route))
        .route("/thread/iphone", get(iphone::build_route))
        .route("/thread/toolchains", get(doctor::toolchains_route))
        .route("/thread/iphone/install", post(iphone::install_route))
        .route("/thread/iphone/run", post(iphone::run_route))
        .route(
            "/project/retire",
            post(routes::project_catalog::project_retire),
        )
        .route(
            "/project/root/open",
            post(routes::project_catalog::project_root_open),
        )
        .route(
            "/project/socket/attach",
            post(routes::project_catalog::project_socket_attach),
        )
        .route(
            "/project/socket/detach",
            post(routes::project_catalog::project_socket_detach),
        )
        .route("/project/rollup", get(routes::rollup::rollup_route))
        .route("/streams", get(routes::project::list_streams))
        .route("/gates", get(routes::operator::list_gates))
        .route("/heart", get(routes::operator::heart_stats))
        .route("/memory", get(routes::memory::memory_view))
        .route("/memory/write", post(routes::memory::memory_write))
        .route("/memory/import", post(routes::memory::memory_import))
        .route("/saved", get(routes::saved::list_route).post(routes::saved::save_route))
        .route("/saved/remove", post(routes::saved::unsave_route))
        .route("/saved/tick", post(routes::saved::tick_route))
        .route("/message/send", post(messages::send_route))
        .route("/merge/waiting", get(baker_threads::merge_waiting_route))
        .route("/thread/merge", post(baker_threads::merge_thread_route))
        .route("/merge/ready", post(baker_threads::merge_ready_route))
        .route("/workspace", post(workspaces::create_route))
        .route("/delete/plan", post(deletes::plan_route))
        .route("/delete/confirm", post(deletes::confirm_route))
        .route("/trash", get(git_trash::list_route))
        .route("/trash/put-back", post(git_trash::put_back_route))
        .route("/trash/empty", post(git_trash::empty_route))
        .route("/move-in/sessions", get(move_in::sessions_route))
        .route("/move-in/import", post(move_in::import_route))
        .route("/move-in/history", get(move_in_history::history_route))
        .route("/fresh-start/plan", get(fresh_start::plan_route))
        .route("/conversation/brief", get(briefs::brief_route).post(briefs::set_route))
        .route("/conversation/brief/decide", post(briefs::decide_route))
        .route("/toolkit", get(toolkit::list_route).post(toolkit::set_route))
        .route("/toolkit/remove", post(toolkit::remove_route))
        .route("/toolkit/enable", post(toolkit::enable_route))
        .route("/toolkit/skill", get(toolkit::skill_route))
        .route("/toolkit/port", get(toolkit_port::list_route).post(toolkit_port::import_route))
        .route("/learning", get(learning::learning_route).post(learning::set_route))
        .route("/learning/run", post(learning::run_route))
        .route("/learning/runs", get(learning::runs_route))
        .route("/memory/search", post(memory_index::search_route))
        .route("/memory/get", post(memory_index::get_route))
        .route("/memory/related", post(memory_index::related_route))
        .route("/memory/notes", get(memory_index::notes_route))
        .route("/memory/note", get(memory_index::page_route))
        .route("/memory/propose", post(memory_review::propose_route))
        .route("/memory/rate", post(memory_review::rate_route))
        .route("/memory/promote", post(memory_review::promote_route))
        .route("/memory/proposals", get(memory_review::proposals_route))
        .route("/memory/review", post(memory_review::review_route))
        .route("/memory/keep-all", post(memory_review::keep_all_route))
        .route("/memory/edit", post(memory_review::edit_route))
        .route("/memory/retire", post(memory_review::retire_route))
        .route("/memory/digest", get(memory_review::digest_route))
        .route("/memory/timeline", get(memory_review::timeline_route))
        .route("/backlog", get(backlog::list_route))
        .route("/backlog/add", post(backlog::add_route))
        .route("/backlog/edit", post(backlog::edit_route))
        .route("/backlog/status", post(backlog::status_route))
        .route("/backlog/move", post(backlog::move_route))
        .route("/backlog/remove", post(backlog::remove_route))
        .route("/context", get(routes::memory::context_view))
        .route("/worktree", post(routes::project::open_worktree))
        .route("/env/provision", post(routes::env::env_provision))
        .route("/env/exec", post(routes::env::env_exec))
        .route("/env/pty", post(routes::env::env_pty))
        .route("/env", get(routes::env::env_status))
        .route("/terminal/create", post(routes::terminal::terminal_create))
        .route("/terminal/list", get(routes::terminal::terminal_list))
        .route("/terminal/rename", post(routes::terminal::terminal_rename))
        .route("/terminal/read", get(routes::terminal::terminal_read))
        .route("/terminal/close", post(routes::terminal::terminal_close))
        .route("/terminal/ticket", post(routes::terminal::terminal_ticket))
        .route("/terminal/ws", get(routes::terminal::terminal_ws))
        .route("/jobs", get(jobs::job_list_route))
        .route("/jobs/start", post(jobs::job_start_route))
        .route("/jobs/output", get(jobs::job_output_route))
        .route("/jobs/stop", post(jobs::job_stop_route))
        // The operator's git client (routes::git).
        .route("/git/status", get(routes::git::status_route))
        .route("/git/branches", get(routes::git::branches_route))
        .route("/git/log", get(routes::git::log_route))
        .route("/git/diff", get(routes::git::diff_route))
        .route("/git/stage", post(routes::git::stage_route))
        .route("/git/commit", post(routes::git::commit_route))
        .route("/git/branch", post(routes::git::branch_route))
        .route("/git/switch", post(routes::git::switch_route))
        .route("/git/fetch", post(routes::git::fetch_route))
        .route("/git/pull", post(routes::git::pull_route))
        .route("/git/update", post(routes::git::update_route))
        .route("/git/rebase", post(routes::git::rebase_route))
        .route("/git/push", post(routes::git::push_route))
        .route("/google/change", post(swap::change_route))
        .route("/git/pr", post(routes::git::pr_route))
        .route("/git/merge", post(routes::git::merge_route))
        .route("/git/provenance", get(provenance::provenance_route))
        .route("/git/merge-preview", get(routes::git::merge_preview_route))
        .route("/git/merge-preview/chain", get(routes::git::merge_chain_route))
        .route("/git/pr/checks", get(routes::git::pr_checks_route))
        .route("/github/repos", get(routes::github::repos_route))
        .route("/github/clone", post(routes::github::clone_route))
        .route("/github/issues", get(routes::github::issues_route))
        .route("/github/issue", get(routes::github::issue_route))
        .route("/github/issue/create", post(routes::github::issue_create_route))
        .route("/github/issue/comment", post(routes::github::issue_comment_route))
        .route("/github/scope", get(routes::github_prs::scope_route).post(routes::github_prs::set_scope_route))
        .route("/github/prs", get(routes::github_prs::prs_route))
        .route("/github/prs/refresh", post(routes::github_prs::refresh_route))
        .route("/github/pr", get(routes::github_prs::pr_route))
        .route("/github/pr/review", post(routes::github_prs::review_route))
        .route("/github/pr/comment", post(routes::github_prs::comment_route))
        .route("/github/pr/merge", post(routes::github_prs::merge_route))
        .route("/github/pr/ready", post(routes::github_prs::ready_route))
        .route("/github/pr/thread/resolve", post(routes::github_prs::resolve_route))
        .route("/github/runs", get(routes::github_prs::runs_route))
        .route("/github/run", get(routes::github_prs::run_route))
        .route("/github/run/log", get(routes::github_prs::run_log_route))
        .route("/github/run/rerun", post(routes::github_prs::rerun_route))
        .route("/grants", get(grants::list_route))
        .route("/grants/give", post(grants::give_route))
        .route("/grants/revoke", post(grants::end_route))
        .route("/remote/waiting", get(remote::waiting_route))
        .route("/remote/history", get(remote::history_route))
        // Paired phones and nodes (devices); the tailnet listener serves these
        // same routes behind device tokens.
        .route("/pair", post(devices::pair_route))
        .route("/devices", get(devices::devices_route))
        .route("/devices/pair-code", post(devices::pair_code_route))
        .route("/devices/revoke", post(devices::revoke_route))
        .route("/devices/remote", post(devices::remote_route))
        .route("/outside-window", get(outside_window::status_route))
        .route("/outside-window/create", post(outside_window::create_route))
        .route("/outside-window/revoke", post(outside_window::revoke_route))
        // The desktop app's link, and the previews agents drive through it (desktop).
        .route("/desktop/ws", get(desktop::link_ws))
        .route("/preview/snapshot", post(desktop::snapshot_route))
        .route("/preview/navigate", post(desktop::navigate_route))
        .route("/preview/click", post(desktop::click_route))
        .route("/preview/eval", post(desktop::eval_route))
        .route("/desktop/screenshot", post(desktop::screenshot_route))
        // The app's own state, kept by the node (routes::ui).
        .route("/ui/layout", get(routes::ui::layout_route).post(routes::ui::save_layout_route))
        .route("/ui/appearance", get(routes::ui::appearance_route).post(routes::ui::save_appearance_route))
        .route("/ui/bundle", get(routes::ui_bundle::bundle_route))
        .route("/ui/bundle/install", post(routes::ui_bundle::install_route))
        .route("/ui/bundle/rollback", post(routes::ui_bundle::rollback_route))
        .route("/system/ghostty", get(routes::system::ghostty_available))
        .route("/system/ghostty/open", post(routes::system::ghostty_open))
        .route("/stream/{id}", get(routes::project::get_stream))
        .route("/seat/assign", post(routes::seats::assign_seat))
        .route(
            "/agent/pipe/gate/decide",
            post(routes::pipe::decide_child_tool),
        )
        .route("/stream/lifetime", post(routes::lifecycle::set_lifetime))
        .route("/stream/discard", post(routes::lifecycle::discard_stream))
        .route("/files/list", get(routes::project::files_list))
        .route("/files/read", get(routes::project::files_read))
        .route("/files/write", post(routes::project::files_write))
        .route("/events", get(routes::operator::ws_events))
        .route("/agent/turn", post(routes::agent::agent_turn))
        .route(
            "/agent/attachments",
            post(attachments::upload_attachment).layer(axum::extract::DefaultBodyLimit::max(
                attachments::MAX_UPLOAD_BODY_BYTES,
            )),
        )
        .route("/attachment", get(attachments::get_attachment))
        .route("/agent/turn/queue", post(routes::agent::agent_turn_queue))
        .route("/agent/turn/queue/withdraw", post(routes::agent::withdraw_queued))
        .route("/agent/turn/review", post(snapshots::review_turn))
        .route("/agent/change", get(snapshots::change_content))
        .route("/agent/snapshot-limit", get(snapshots::snapshot_limit).post(snapshots::set_snapshot_limit))
        .route("/search", post(search::search))
        .route("/search/reveal", post(search::reveal))
        .route(
            "/agent/turn/queue/steer-all",
            post(routes::agent::steer_all_queued),
        )
        .route("/agent/tool-payload", get(routes::pipe::tool_payload))
        .route("/agent/turn/steer", post(routes::agent::agent_turn_steer))
        .route("/agent/turn/cancel", post(routes::agent::agent_turn_cancel))
        .route(
            "/agent/dispatch/resume",
            post(routes::agent::resume_dispatch),
        )
        .route("/agent/mode", post(routes::agent::set_operating_mode))
        .route("/agent/tools", get(api_tools::tools_route).post(api_tools::set_tools_route))
        .route(
            "/agent/runtime/config",
            post(routes::agent::set_native_config),
        )
        .route("/agent/selection", post(routes::agent::set_selection))
        .route("/agent/activity", get(routes::agent::agent_activity))
        .route("/agent/accept", post(routes::agent::agent_accept))
        .route("/agent/answer", post(routes::agent::agent_answer))
        .route("/agent/resume", post(routes::agent::agent_resume))
        .route("/agent/session/choose", post(routes::agent::agent_session_choose))
        .route("/agent/reject", post(routes::agent::agent_reject))
        .route("/baker", get(routes::baker::baker_home))
        .route(
            "/baker/threads",
            get(baker_threads::threads_list).post(baker_threads::thread_create),
        )
        .route(
            "/baker/threads/rename",
            post(baker_threads::thread_rename),
        )
        .route(
            "/baker/threads/archive",
            post(baker_threads::thread_archive),
        )
        .route("/baker/threads/read", get(baker_threads::thread_read_route))
        .route("/baker/threads/wait", post(baker_threads::thread_wait_route))
        .route("/baker/threads/board", get(thread_board::board_route))
        .route("/baker/assign", post(routes::baker::baker_assign))
        .route("/baker/work", get(routes::baker::baker_work_list))
        .route("/baker/work/detail", get(routes::baker::baker_work_detail))
        .route("/baker/push", post(routes::baker::baker_push))
        .route("/baker/pr", post(routes::baker::baker_pr))
        .route("/baker/merge", post(routes::baker::baker_merge))
        .route(
            "/baker/capabilities",
            get(routes::baker::baker_capabilities),
        )
        .route("/task/create", post(tasks::task_create_route))
        .route("/task/start", post(tasks::task_start_route))
        .route("/task/message", post(tasks::task_message_route))
        .route("/task/cancel", post(tasks::task_cancel_route))
        .route("/task/retire", post(tasks::task_retire_route))
        .route("/task/list", get(tasks::task_list_route))
        .route("/task/review", get(task_review::task_review_route))
        .route("/task/signoff", post(task_review::task_signoff_route))
        .route("/task/push", post(task_ship::task_push_route))
        .route("/task/pr", post(task_ship::task_pr_route))
        .route("/task/integrate", post(task_ship::task_integrate_route))
        .route(
            "/operator/snapshot",
            get(routes::operator::operator_snapshot),
        )
        .route("/auth/status", get(routes::auth::auth_status))
        .route("/auth/cli-status", get(routes::auth::auth_cli_status))
        .route(
            "/auth/claude-native/status",
            get(routes::auth::auth_claude_native_status),
        )
        .route("/auth/api-key", post(routes::auth::auth_set_api_key))
        .route("/auth/clear", post(routes::auth::auth_clear))
        .route("/auth/model", post(routes::auth::auth_set_model))
        .route(
            "/auth/xai/device/start",
            post(routes::auth::auth_xai_device_start),
        )
        .route(
            "/auth/xai/device/poll",
            post(routes::auth::auth_xai_device_poll),
        )
        .route(
            "/auth/gemini/import-cli",
            post(routes::auth::auth_gemini_import_cli),
        )
        .route(
            "/auth/claude-code/login/start",
            post(routes::auth::auth_claude_code_login_start),
        )
        .route(
            "/auth/claude-code/login/complete",
            post(routes::auth::auth_claude_code_login_complete),
        )
        .route(
            "/auth/plug-secret",
            post(routes::auth::auth_set_plug_secret),
        )
        .route(
            "/auth/plug-secret/clear",
            post(routes::auth::auth_clear_plug_secret),
        )
        .route("/providers", get(routes::providers::providers_list))
        .route(
            "/providers/acp-plug",
            post(routes::providers::acp_plug_create),
        )
        .route(
            "/providers/acp-plug/update",
            post(routes::providers::acp_plug_update),
        )
        .route(
            "/providers/acp-plug/delete",
            post(routes::providers::acp_plug_delete),
        )
        .route(
            "/providers/openai-compat-plug",
            post(routes::providers::openai_compat_plug_create),
        )
        .route(
            "/providers/openai-compat-plug/update",
            post(routes::providers::openai_compat_plug_update),
        )
        .route(
            "/providers/openai-compat-plug/delete",
            post(routes::providers::openai_compat_plug_delete),
        )
        .route("/studio/state", get(routes::studio::studio_state))
        .route("/models/catalog", get(routes::studio::model_catalog))
        .route(
            "/accounts/capabilities",
            get(routes::studio::account_capabilities),
        )
        .route("/mcp/tools", get(mcp::mcp_tools))
        .route("/mcp/call", post(mcp::mcp_call))
        .route("/mcp", post(outside_window::mcp_route))
        .route("/events/ticket", post(routes::operator::events_ticket))
}
