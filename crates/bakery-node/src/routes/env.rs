//! Room provisioning + one-shot exec/pty attach, over HTTP — the room and its
//! attach contract live in `bakery-exec`; these routes are a thin wire
//! wrapper (and anything else that wants to poke a room without writing
//! Rust). The interactive terminal WS surface lives in
//! `routes::terminal`/`crate::terminals` now, not here — see those modules
//! for the node-owned named-session model (07c-STUDIO-SHELL §3).
//!
//! Every route here takes a `stream_id`, never a raw filesystem path: the
//! workspace is resolved from the stream's own `project.onboarded` event
//! (`context_for`), same as every other agent-facing surface in this crate.
//! A client cannot point provision/exec at an arbitrary folder just by
//! naming one — localhost is not authentication, and an onboarded stream is
//! the only thing that counts as "a workspace this node was told about."
//!
//! `/env/exec` (agent pipe) and `/env/pty` (human pipe, one-shot argv) are
//! separate routes, not one route with a JSON switch: a JSON body is
//! attacker-controlled input from anything on localhost, so which attach
//! function runs is a decision the node makes by which endpoint was called,
//! never a field a caller sets.

use crate::state::{admit_one_shot, cache_root, jail_for, AppState};
use axum::{
    extract::{Query, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct ProvisionReq {
    pub(crate) stream_id: String,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct ProvisionResp {
    pub(crate) digest: String,
    pub(crate) root: String,
    pub(crate) cache_hit: bool,
    pub(crate) rustc: Option<String>,
}

/// Explicitly the optional, pinned-tools route (07b) — never the thin room's
/// admission door. A workspace with no `mise.toml`, or one pinning something
/// this backend cannot provision, fails *this* call; it must never be read
/// as "this socket has no room at all" (`resolve_room` is that door).
pub(crate) fn provision_inner(
    workspace: &Path,
    cache_root: &Path,
    stream_id: &str,
) -> Result<ProvisionResp, String> {
    let env = bakery_exec::provision(workspace, cache_root, stream_id)?;
    let rustc = bakery_exec::attach_exec(
        &env,
        &["rustc".to_string(), "--version".to_string()],
        &[],
        Some(workspace),
    )
    .ok()
    .filter(|o| o.exit_code == 0)
    .map(|o| o.stdout.trim().to_string());
    Ok(ProvisionResp {
        digest: env.digest,
        root: env.toolchain_root.to_string_lossy().into_owned(),
        cache_hit: env.cache_hit,
        rustc,
    })
}

pub(crate) async fn env_provision(
    State(st): State<AppState>,
    Json(req): Json<ProvisionReq>,
) -> Result<Json<ProvisionResp>, (StatusCode, String)> {
    let _guard = admit_one_shot(&st, &req.stream_id)?;
    let jail = jail_for(&st, &req.stream_id).map_err(|e| (StatusCode::NOT_FOUND, e))?;
    let cache_root = cache_root(&st);
    let workspace = jail.root().to_path_buf();
    let stream_id = req.stream_id.clone();
    crate::actor::spawn_blocking(move || provision_inner(&workspace, &cache_root, &stream_id))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct AttachReq {
    pub(crate) stream_id: String,
    pub(crate) argv: Vec<String>,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct ExecResp {
    pub(crate) stdout: String,
    pub(crate) stderr: String,
    pub(crate) exit_code: i32,
}

/// The room a caller that just wants *some* box attaches to (07b/07b-B): the
/// pinned project toolchain when it is **already materialized** in the
/// cache, otherwise the always-available thin room. Deliberately never
/// triggers a fresh provision as a side effect of exec/PTY — a cold pinned
/// toolchain can take minutes to clone, and running a command must not
/// silently pay that cost. Materializing pinned tools on purpose is
/// `/env/provision`'s job alone (a later socket setting can make this path
/// opt into auto-provisioning; it does not today).
///
/// Uses `open_cached_project_tools` rather than a `status()`-then-`provision()`
/// pair: that would read the manifest and compute the digest twice, and a
/// `mise.toml` edit or a cache eviction landing between the two reads could
/// make the second call provision a digest the first never actually
/// confirmed was warm. `open_cached_project_tools` reads the spec once and
/// has no code path that can ever provision.
fn resolve_room(
    workspace: &Path,
    cache_root: &Path,
    stream_id: &str,
) -> Result<bakery_exec::SessionEnvironment, String> {
    if let Some(env) = bakery_exec::open_cached_project_tools(workspace, cache_root, stream_id) {
        return Ok(env);
    }
    bakery_exec::ensure_thin_room(workspace, cache_root, stream_id)
}

/// The agent pipe: always `attach_exec`. Workspace is cwd + read only —
/// writes land in the stream's own runtime dir, never the worktree. Also
/// where `bakery_exec::attach_exec`'s credential-filename fence applies.
pub(crate) fn exec_inner(
    cache_root: &Path,
    workspace: &Path,
    stream_id: &str,
    argv: &[String],
) -> Result<ExecResp, String> {
    let env = resolve_room(workspace, cache_root, stream_id)?;
    let out = bakery_exec::attach_exec(&env, argv, &[], Some(workspace))?;
    Ok(ExecResp {
        stdout: out.stdout,
        stderr: out.stderr,
        exit_code: out.exit_code,
    })
}

pub(crate) async fn env_exec(
    State(st): State<AppState>,
    Json(req): Json<AttachReq>,
) -> Result<Json<ExecResp>, (StatusCode, String)> {
    let _guard = admit_one_shot(&st, &req.stream_id)?;
    let jail = jail_for(&st, &req.stream_id).map_err(|e| (StatusCode::NOT_FOUND, e))?;
    let cache_root = cache_root(&st);
    let workspace = jail.root().to_path_buf();
    let stream_id = req.stream_id.clone();
    crate::actor::spawn_blocking(move || exec_inner(&cache_root, &workspace, &stream_id, &req.argv))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))
}

/// The human pipe: always `attach_pty`. Workspace is cwd, readable *and*
/// writable — this is the studio Terminal widget's only first-party caller,
/// standing in for an operator at a real keyboard.
pub(crate) fn pty_inner(
    cache_root: &Path,
    workspace: &Path,
    stream_id: &str,
    argv: &[String],
) -> Result<ExecResp, String> {
    let env = resolve_room(workspace, cache_root, stream_id)?;
    let out = bakery_exec::attach_pty(&env, argv, &[], Some(workspace))?;
    Ok(ExecResp {
        stdout: out.stdout,
        stderr: out.stderr,
        exit_code: out.exit_code,
    })
}

pub(crate) async fn env_pty(
    State(st): State<AppState>,
    Json(req): Json<AttachReq>,
) -> Result<Json<ExecResp>, (StatusCode, String)> {
    let _guard = admit_one_shot(&st, &req.stream_id)?;
    let jail = jail_for(&st, &req.stream_id).map_err(|e| (StatusCode::NOT_FOUND, e))?;
    let cache_root = cache_root(&st);
    let workspace = jail.root().to_path_buf();
    let stream_id = req.stream_id.clone();
    crate::actor::spawn_blocking(move || pty_inner(&cache_root, &workspace, &stream_id, &req.argv))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct EnvQuery {
    pub(crate) stream_id: String,
}

/// The room capability plane (07b/07b-B): `thin_ready` is true for any
/// resolvable stream (thin admission has no prerequisites), and
/// `project_tools` reports the *optional* pinned-toolchain capability
/// honestly, without triggering a real provision from a GET:
///
/// - `not_configured` — no `mise.toml` at all. Not an error.
/// - `invalid` — a `mise.toml` exists but does not parse (malformed TOML, or
///   a tool value that isn't a version string). Distinct from
///   `not_configured`: there IS a manifest, it's just broken.
/// - `unsupported` — parses fine but pins a tool this backend cannot
///   provision (anything but `rust`).
/// - `configured` — pins only supported tools but is not yet cached; using
///   it would pay a real (possibly multi-minute) toolchain clone. `resolve_room`
///   (exec/PTY) never does that as a side effect — only `/env/provision` does.
/// - `ready` — cached and warm; `resolve_room` will actually attach it.
#[derive(schemars::JsonSchema, Serialize)]
pub(crate) struct EnvStatusResp {
    pub(crate) thin_ready: bool,
    pub(crate) project_tools: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reason: Option<String>,
}

pub(crate) async fn env_status(
    State(st): State<AppState>,
    Query(q): Query<EnvQuery>,
) -> Result<Json<EnvStatusResp>, (StatusCode, String)> {
    let jail = jail_for(&st, &q.stream_id).map_err(|e| (StatusCode::NOT_FOUND, e))?;
    let cache_root = cache_root(&st);

    let (project_tools, reason) = if !jail.root().join("mise.toml").is_file() {
        ("not_configured".to_string(), None)
    } else {
        match bakery_exec::Spec::load(jail.root()) {
            // Never echo the parse error verbatim: `Spec::load`'s own message
            // embeds the absolute workspace path, which must not cross the
            // wire — a generic, still-actionable reason is enough here.
            Err(_) => (
                "invalid".to_string(),
                Some(
                    "mise.toml exists but could not be parsed — check its [tools] table"
                        .to_string(),
                ),
            ),
            Ok(spec) => {
                let unsupported = spec.unsupported_tools();
                if !unsupported.is_empty() {
                    (
                        "unsupported".to_string(),
                        Some(format!(
                            "mise.toml pins {} — only \"rust\" is provisioned",
                            unsupported.join(", ")
                        )),
                    )
                } else {
                    let cached = bakery_exec::status(jail.root(), &cache_root)
                        .map(|s| s.ready)
                        .unwrap_or(false);
                    if cached {
                        ("ready".to_string(), None)
                    } else {
                        ("configured".to_string(), None)
                    }
                }
            }
        }
    };

    Ok(Json(EnvStatusResp {
        thin_ready: true,
        project_tools,
        reason,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::project::open_stream;
    use crate::testutil::*;
    use bakery_agent::StubModel;
    use std::sync::Arc;

    fn host_rustc_short_version() -> String {
        let out = std::process::Command::new("rustc")
            .arg("--version")
            .output()
            .unwrap();
        let line = String::from_utf8_lossy(&out.stdout).trim().to_string();
        line.split_whitespace().nth(1).unwrap().to_string()
    }

    /// A repo (with a commit) pinned to the host's own rustc — `make_repo` is
    /// the crate's usual git fixture; this just adds `mise.toml` on top.
    fn repo_pinned_to_host_rust(at: &std::path::Path) -> std::path::PathBuf {
        let repo = make_repo(at);
        std::fs::write(
            repo.join("mise.toml"),
            format!("[tools]\nrust = \"{}\"\n", host_rustc_short_version()),
        )
        .unwrap();
        run_git(&repo, &["add", "mise.toml"]);
        run_git(&repo, &["commit", "-qm", "pin rust"]);
        repo
    }

    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn provision_then_exec_reports_the_pinned_rustc() {
        let dir = tempfile::tempdir().unwrap();
        let repo = repo_pinned_to_host_rust(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let resp = open_stream(&state, &open_req(&repo, "term")).unwrap();
        let workspace = std::path::PathBuf::from(&resp.context.worktree_path);
        let cache_root = cache_root(&state);

        let first = provision_inner(&workspace, &cache_root, "wt:term").unwrap();
        assert!(!first.cache_hit);
        assert!(first.rustc.as_deref().unwrap_or("").starts_with("rustc"));

        let second = provision_inner(&workspace, &cache_root, "wt:term").unwrap();
        assert!(second.cache_hit, "second provision must be a cache hit");
        assert_eq!(second.digest, first.digest);

        let exec = exec_inner(
            &cache_root,
            &workspace,
            "wt:term",
            &["rustc".to_string(), "--version".to_string()],
        )
        .unwrap();
        assert_eq!(exec.exit_code, 0, "stderr: {}", exec.stderr);
        assert_eq!(exec.stdout.trim(), first.rustc.unwrap());
    }

    /// 07b-B: agent exec must attach a room even without any `mise.toml` —
    /// an honest degrade (no pinned rustc on PATH), never a hard error.
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn exec_inner_attaches_thin_when_there_is_no_mise_toml() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let resp = open_stream(&state, &open_req(&repo, "term")).unwrap();
        let workspace = std::path::PathBuf::from(&resp.context.worktree_path);
        let cache_root = cache_root(&state);

        let out = exec_inner(
            &cache_root,
            &workspace,
            "wt:term",
            &["echo".to_string(), "thin ok".to_string()],
        )
        .unwrap();
        assert_eq!(out.exit_code, 0, "stderr: {}", out.stderr);
        assert_eq!(out.stdout.trim(), "thin ok");
    }

    /// 07b-B regression proof: a supported, matching pin that is NOT yet
    /// cached must NOT be provisioned as a side effect of exec — that would
    /// silently pay a real (possibly multi-minute) toolchain clone just for
    /// running a command. Exec must fall back to the thin room instead.
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn exec_inner_does_not_auto_provision_a_cold_but_supported_pin() {
        let dir = tempfile::tempdir().unwrap();
        let repo = repo_pinned_to_host_rust(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let resp = open_stream(&state, &open_req(&repo, "term")).unwrap();
        let workspace = std::path::PathBuf::from(&resp.context.worktree_path);
        let cache_root = cache_root(&state);

        // Confirm the cache really is cold before exec.
        let before = bakery_exec::status(&workspace, &cache_root).unwrap();
        assert!(
            !before.ready,
            "test setup: the toolchain cache must start cold"
        );

        let out = exec_inner(
            &cache_root,
            &workspace,
            "wt:term",
            &["echo".to_string(), "thin ok".to_string()],
        )
        .unwrap();
        assert_eq!(out.exit_code, 0, "stderr: {}", out.stderr);
        assert_eq!(out.stdout.trim(), "thin ok");

        // Exec must not have provisioned anything — the cache is still cold.
        let after = bakery_exec::status(&workspace, &cache_root).unwrap();
        assert!(
            !after.ready,
            "exec must never provision a cold pinned toolchain as a side effect"
        );
    }

    /// Same for the one-shot human pipe.
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn pty_inner_attaches_thin_when_there_is_no_mise_toml() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let resp = open_stream(&state, &open_req(&repo, "term")).unwrap();
        let workspace = std::path::PathBuf::from(&resp.context.worktree_path);
        let cache_root = cache_root(&state);

        let out = pty_inner(
            &cache_root,
            &workspace,
            "wt:term",
            &["echo".to_string(), "thin ok".to_string()],
        )
        .unwrap();
        assert_eq!(out.exit_code, 0, "stderr: {}", out.stderr);
        assert_eq!(out.stdout.trim(), "thin ok");
    }

    /// An unsupported pin degrades to thin rather than failing exec closed.
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn exec_inner_attaches_thin_when_mise_toml_pins_an_unsupported_tool() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        std::fs::write(repo.join("mise.toml"), "[tools]\nnode = \"22\"\n").unwrap();
        run_git(&repo, &["add", "mise.toml"]);
        run_git(&repo, &["commit", "-qm", "pin node"]);
        let state = state_with(dir.path(), Arc::new(StubModel));
        let resp = open_stream(&state, &open_req(&repo, "term")).unwrap();
        let workspace = std::path::PathBuf::from(&resp.context.worktree_path);
        let cache_root = cache_root(&state);

        let out = exec_inner(
            &cache_root,
            &workspace,
            "wt:term",
            &["echo".to_string(), "thin ok".to_string()],
        )
        .unwrap();
        assert_eq!(out.exit_code, 0, "stderr: {}", out.stderr);
        assert_eq!(out.stdout.trim(), "thin ok");
    }

    /// 07b-B: `GET /env` reports the full 5-state capability plane without
    /// ever provisioning anything itself.
    #[tokio::test]
    async fn env_status_reports_the_five_capability_states() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));

        let bare = make_repo(&dir.path().join("bare"));
        let bare_id = open_stream(&state, &open_req(&bare, "bare"))
            .unwrap()
            .context
            .stream_id;
        let resp = env_status(State(state.clone()), Query(EnvQuery { stream_id: bare_id }))
            .await
            .unwrap();
        assert!(resp.0.thin_ready);
        assert_eq!(resp.0.project_tools, "not_configured");
        assert!(resp.0.reason.is_none());

        // Regression proof: a supported pin that is NOT yet cached must read
        // as "configured", never "ready" — status() alone must not lie about
        // a cache that was never warmed.
        let cold = repo_pinned_to_host_rust(&dir.path().join("cold"));
        let cold_id = open_stream(&state, &open_req(&cold, "cold"))
            .unwrap()
            .context
            .stream_id;
        let resp = env_status(
            State(state.clone()),
            Query(EnvQuery {
                stream_id: cold_id.clone(),
            }),
        )
        .await
        .unwrap();
        assert!(resp.0.thin_ready);
        assert_eq!(
            resp.0.project_tools, "configured",
            "a cold pin must not be reported ready"
        );

        // Once actually warmed (via the explicit /env/provision route, never
        // as an exec/pty side effect), the same workspace reads as ready.
        let cache_root = cache_root(&state);
        provision_inner(&cold, &cache_root, &cold_id).unwrap();
        let resp = env_status(State(state.clone()), Query(EnvQuery { stream_id: cold_id }))
            .await
            .unwrap();
        assert_eq!(resp.0.project_tools, "ready");

        let unsupported = make_repo(&dir.path().join("unsupported"));
        std::fs::write(unsupported.join("mise.toml"), "[tools]\nnode = \"22\"\n").unwrap();
        run_git(&unsupported, &["add", "mise.toml"]);
        run_git(&unsupported, &["commit", "-qm", "pin node"]);
        let unsupported_id = open_stream(&state, &open_req(&unsupported, "unsupported"))
            .unwrap()
            .context
            .stream_id;
        let resp = env_status(
            State(state.clone()),
            Query(EnvQuery {
                stream_id: unsupported_id,
            }),
        )
        .await
        .unwrap();
        assert!(resp.0.thin_ready);
        assert_eq!(resp.0.project_tools, "unsupported");
        assert!(resp.0.reason.as_deref().unwrap_or("").contains("node"));

        // Regression proof: a present-but-malformed mise.toml is "invalid",
        // never conflated with "not_configured" (no manifest at all).
        let malformed = make_repo(&dir.path().join("malformed"));
        std::fs::write(malformed.join("mise.toml"), "this is not valid toml [[[").unwrap();
        run_git(&malformed, &["add", "mise.toml"]);
        run_git(&malformed, &["commit", "-qm", "broken manifest"]);
        let malformed_id = open_stream(&state, &open_req(&malformed, "malformed"))
            .unwrap()
            .context
            .stream_id;
        let resp = env_status(
            State(state.clone()),
            Query(EnvQuery {
                stream_id: malformed_id,
            }),
        )
        .await
        .unwrap();
        assert!(resp.0.thin_ready);
        assert_eq!(
            resp.0.project_tools, "invalid",
            "a broken manifest must not read as not_configured"
        );
        assert!(resp.0.reason.is_some());
        // Sanitized: the parse-error reason must never echo the absolute workspace path.
        let workspace_root = &state.worktree_base;
        assert!(
            !resp
                .0
                .reason
                .as_deref()
                .unwrap_or("")
                .contains(&workspace_root.to_string_lossy().into_owned()),
            "reason must not leak the workspace path, got: {:?}",
            resp.0.reason
        );
    }

    #[test]
    fn status_route_does_not_provision() {
        let dir = tempfile::tempdir().unwrap();
        let repo = repo_pinned_to_host_rust(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let resp = open_stream(&state, &open_req(&repo, "term")).unwrap();
        let workspace = std::path::PathBuf::from(&resp.context.worktree_path);
        let cache_root = cache_root(&state);

        let before = bakery_exec::status(&workspace, &cache_root).unwrap();
        assert!(!before.ready);

        seed_test_toolchain_cache(&cache_root, &workspace);
        provision_inner(&workspace, &cache_root, "wt:term").unwrap();
        let after = bakery_exec::status(&workspace, &cache_root).unwrap();
        assert!(after.ready);
    }

    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn exec_cannot_write_a_workspace_file_but_pty_can() {
        let dir = tempfile::tempdir().unwrap();
        let repo = repo_pinned_to_host_rust(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let resp = open_stream(&state, &open_req(&repo, "term")).unwrap();
        let workspace = std::path::PathBuf::from(&resp.context.worktree_path);
        let cache_root = cache_root(&state);
        seed_test_toolchain_cache(&cache_root, &workspace);

        let marker = workspace.join("from-exec.txt");
        let exec = exec_inner(
            &cache_root,
            &workspace,
            "wt:term",
            &[
                "/usr/bin/tee".to_string(),
                marker.to_string_lossy().into_owned(),
            ],
        )
        .unwrap();
        assert_ne!(
            exec.exit_code, 0,
            "the agent pipe must not be able to write the worktree"
        );
        assert!(!marker.exists());

        let marker2 = workspace.join("from-pty.txt");
        let pty = pty_inner(
            &cache_root,
            &workspace,
            "wt:term",
            &[
                "/usr/bin/tee".to_string(),
                marker2.to_string_lossy().into_owned(),
            ],
        )
        .unwrap();
        assert_eq!(
            pty.exit_code, 0,
            "the human pipe must be able to write the worktree: {}",
            pty.stderr
        );
        assert!(marker2.exists());
    }

    #[tokio::test]
    async fn http_routes_refuse_an_unknown_stream() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));

        let err = env_provision(
            State(state.clone()),
            Json(ProvisionReq {
                stream_id: "wt:does-not-exist".into(),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::NOT_FOUND);

        let err = env_exec(
            State(state.clone()),
            Json(AttachReq {
                stream_id: "wt:does-not-exist".into(),
                argv: vec!["rustc".into(), "--version".into()],
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::NOT_FOUND);

        let err = env_pty(
            State(state.clone()),
            Json(AttachReq {
                stream_id: "wt:does-not-exist".into(),
                argv: vec!["rustc".into(), "--version".into()],
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn a_second_concurrent_attach_on_the_same_stream_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let repo = repo_pinned_to_host_rust(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        open_stream(&state, &open_req(&repo, "term")).unwrap();

        let _held = admit_one_shot(&state, "wt:term").unwrap();
        let err = env_exec(
            State(state.clone()),
            Json(AttachReq {
                stream_id: "wt:term".into(),
                argv: vec!["rustc".into(), "--version".into()],
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::CONFLICT);
    }

    /// A Heart `project.onboarded` event is trusted input in the sense that
    /// only this node writes it — but a stream whose recorded worktree is
    /// broad enough to reach `~/.bakery` must still be refused the same way
    /// `Jail::new` refuses it for the agent tool loop. Forges the event
    /// directly (no real git repo needed) to point `worktree_path` at a
    /// stand-in "home" containing `.bakery`.
    #[tokio::test]
    async fn env_routes_refuse_a_stream_whose_worktree_would_expose_bakery_home() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let fake_home = dir.path().join("fake-home");
        std::fs::create_dir_all(fake_home.join(".bakery")).unwrap();

        let payload = serde_json::json!({
            "repo": fake_home.to_string_lossy(),
            "repo_root": fake_home.to_string_lossy(),
            "project": "broad",
            "name": "broad",
            "worktree_path": fake_home.to_string_lossy(),
            "branch": "main",
        })
        .to_string();
        crate::state::append(
            &state,
            "wt:broad",
            "human",
            "project.onboarded",
            payload,
            vec![],
        )
        .unwrap();

        // `Jail::new`'s guard compares against the *real* `$HOME` — point it
        // at our stand-in for the duration of the check.
        let real_home = std::env::var("HOME").unwrap();
        std::env::set_var("HOME", &fake_home);

        let provision_err = env_provision(
            State(state.clone()),
            Json(ProvisionReq {
                stream_id: "wt:broad".into(),
            }),
        )
        .await;
        let exec_err = env_exec(
            State(state.clone()),
            Json(AttachReq {
                stream_id: "wt:broad".into(),
                argv: vec!["rustc".into(), "--version".into()],
            }),
        )
        .await;

        std::env::set_var("HOME", real_home);

        assert!(provision_err.unwrap_err().1.contains("would expose"));
        assert!(exec_err.unwrap_err().1.contains("would expose"));
    }

    #[tokio::test]
    async fn env_exec_denies_a_planted_dotenv_the_same_way_the_files_jail_does() {
        let dir = tempfile::tempdir().unwrap();
        let repo = repo_pinned_to_host_rust(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let resp = open_stream(&state, &open_req(&repo, "term")).unwrap();
        std::fs::write(
            std::path::Path::new(&resp.context.worktree_path).join(".env"),
            "SECRET=leak\n",
        )
        .unwrap();

        let (status, body) = env_exec(
            State(state.clone()),
            Json(AttachReq {
                stream_id: resp.context.stream_id.clone(),
                argv: vec!["/bin/cat".into(), ".env".into()],
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body, "denied: credential surface");
    }
}
