//! Setup / auth routes: API-key and OAuth login flows. Secrets never enter
//! the Heart — see `bakery_agent::secrets`.

use crate::state::AppState;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    Json,
};
use bakery_agent::gemini as gemini_auth;
use bakery_agent::{claude_code_slot, cli_tools, resolve_model, secrets, xai_oauth};
use serde::{Deserialize, Serialize};

// ─── M5.6 auth / setup (secrets never enter the Heart) ───────────────────────

/// Reload the legacy node-global model AND evict every cached seat
/// resolution (06-SEATS audit item 4). A seat's cached `Arc<dyn Model>` can
/// outlive the credentials it was built from — most concretely, an `auto`
/// seat resolved to (say) Grok before a Gemini key was just added would
/// otherwise keep answering with Grok forever, never re-consulting the now-
/// changed provider preference. Clearing the cache doesn't touch the
/// durable `seat.model.assigned` Heart fact; the next turn just re-resolves
/// it fresh, the same as after a node restart.
pub(crate) fn reload_model(state: &AppState) {
    let m = resolve_model();
    println!("bakery-node model reloaded: {}", m.name());
    *state.model.lock().unwrap() = m;
    state.seats.lock().unwrap().clear();
}

pub(crate) async fn auth_status() -> Json<secrets::AuthStatus> {
    Json(secrets::status())
}

/// Codex/Cursor account status (07d-A3D1) — deliberately **not** folded into
/// [`auth_status`]/[`secrets::AuthStatus`]: that function is pure/sync and
/// asserted on directly by existing tests, and both these checks shell out
/// to the tool's own status subcommand, so a hung `codex`/`cursor-agent`
/// binary must never slow down every other provider row's poll too.
#[derive(schemars::JsonSchema, Deserialize, Default)]
pub(crate) struct CliStatusQuery {
    #[serde(default)]
    pub(crate) force: Option<String>,
}

#[derive(schemars::JsonSchema, Serialize)]
pub(crate) struct CliAccountsStatus {
    #[schemars(with = "crate::api_catalog::CliAccountShape")]
    pub(crate) codex: cli_tools::CliAccountStatus,
    #[schemars(with = "crate::api_catalog::CliAccountShape")]
    pub(crate) cursor: cli_tools::CliAccountStatus,
}

/// `force=1`/`force=true` bypasses the 10s per-tool cache — called once
/// right after a login terminal's WS close reason arrives, so the row
/// updates immediately instead of waiting out the TTL.
pub(crate) async fn auth_cli_status(
    Query(q): Query<CliStatusQuery>,
) -> Result<Json<CliAccountsStatus>, (StatusCode, String)> {
    let force = matches!(q.force.as_deref(), Some("1") | Some("true"));
    tokio::task::spawn_blocking(move || CliAccountsStatus {
        codex: cli_tools::codex_status(force),
        cursor: cli_tools::cursor_status(force),
    })
    .await
    .map(Json)
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// Pinned Claude-native adapter + native CLI account status. This probe
/// intentionally has its own route: it distinguishes subscription billing
/// from API-key/external billing and projects no email, organization, token,
/// argv, or credential path.
pub(crate) async fn auth_claude_native_status(
    State(st): State<AppState>,
) -> Result<Json<crate::claude_native::ClaudeNativeStatus>, (StatusCode, String)> {
    tokio::task::spawn_blocking(move || crate::claude_native::status(&st))
        .await
        .map(Json)
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct ApiKeyReq {
    pub(crate) provider: String,
    pub(crate) key: String,
}

pub(crate) async fn auth_set_api_key(
    State(st): State<AppState>,
    Json(req): Json<ApiKeyReq>,
) -> Result<Json<secrets::AuthStatus>, (StatusCode, String)> {
    secrets::set_api_key(&req.provider, &req.key).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    reload_model(&st);
    Ok(Json(secrets::status()))
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct ClearReq {
    pub(crate) provider: String,
}

pub(crate) async fn auth_clear(
    State(st): State<AppState>,
    Json(req): Json<ClearReq>,
) -> Result<Json<secrets::AuthStatus>, (StatusCode, String)> {
    secrets::clear_provider(&req.provider).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    reload_model(&st);
    Ok(Json(secrets::status()))
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct ModelReq {
    pub(crate) model: String,
}

pub(crate) async fn auth_set_model(
    State(st): State<AppState>,
    Json(req): Json<ModelReq>,
) -> Result<Json<secrets::AuthStatus>, (StatusCode, String)> {
    secrets::set_active_model(&req.model).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    reload_model(&st);
    Ok(Json(secrets::status()))
}

pub(crate) async fn auth_xai_device_start(
) -> Result<Json<xai_oauth::DeviceStart>, (StatusCode, String)> {
    // Blocking HTTP — run off async runtime via spawn_blocking
    let result = tokio::task::spawn_blocking(xai_oauth::device_start)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map_err(|e| (StatusCode::BAD_GATEWAY, e))?;
    Ok(Json(result))
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct DevicePollReq {
    pub(crate) device_code: String,
}

pub(crate) async fn auth_xai_device_poll(
    State(st): State<AppState>,
    Json(req): Json<DevicePollReq>,
) -> Result<Json<xai_oauth::DevicePollResult>, (StatusCode, String)> {
    let code = req.device_code.clone();
    let result = tokio::task::spawn_blocking(move || xai_oauth::device_poll(&code))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map_err(|e| (StatusCode::BAD_GATEWAY, e))?;
    if result.status == "complete" {
        // Prefer grok after successful OAuth login
        let _ = secrets::set_active_model("grok");
        reload_model(&st);
    }
    Ok(Json(result))
}

#[derive(schemars::JsonSchema, Serialize)]
pub(crate) struct ImportResp {
    pub(crate) message: String,
    pub(crate) status: secrets::AuthStatus,
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct PlugSecretReq {
    pub(crate) plug_id: String,
    pub(crate) key: String,
}

#[derive(schemars::JsonSchema, Serialize)]
pub(crate) struct PlugSecretStatus {
    pub(crate) plug_id: String,
    pub(crate) secret_set: bool,
}

/// Set (or replace) the bearer token for one openai-compat plug
/// (`node.json`'s `openai_compat_plugs[].id`). Same discipline as
/// `auth_set_api_key`: the key is written straight to `secrets.json` and
/// never echoed back — only the boolean is. Unlike `auth_set_api_key`, this
/// does not validate `plug_id` against the configured plug list: an operator
/// may set a plug's secret before adding it to `node.json`, or after
/// removing it (harmless — an orphaned secret answers nothing).
pub(crate) async fn auth_set_plug_secret(
    State(st): State<AppState>,
    Json(req): Json<PlugSecretReq>,
) -> Result<Json<PlugSecretStatus>, (StatusCode, String)> {
    secrets::set_plug_secret(&req.plug_id, &req.key).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    // A plug secret can change which cached `Arc<dyn Model>` a seat should
    // be answering with (e.g. a previously-unauthenticated local endpoint
    // now has a bearer) — evict every cached seat resolution the same way
    // `reload_model` does for the three fixed HTTP providers.
    st.seats.lock().unwrap().clear();
    Ok(Json(PlugSecretStatus {
        plug_id: req.plug_id,
        secret_set: true,
    }))
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct PlugSecretClearReq {
    pub(crate) plug_id: String,
}

pub(crate) async fn auth_clear_plug_secret(
    State(st): State<AppState>,
    Json(req): Json<PlugSecretClearReq>,
) -> Result<Json<PlugSecretStatus>, (StatusCode, String)> {
    secrets::clear_plug_secret(&req.plug_id).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    st.seats.lock().unwrap().clear();
    Ok(Json(PlugSecretStatus {
        plug_id: req.plug_id,
        secret_set: false,
    }))
}

pub(crate) async fn auth_claude_code_login_start(
) -> Result<Json<claude_code_slot::LoginStart>, (StatusCode, String)> {
    let result = tokio::task::spawn_blocking(claude_code_slot::login_start)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    Ok(Json(result))
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct ClaudeCodeCompleteReq {
    pub(crate) callback: String,
}

pub(crate) async fn auth_claude_code_login_complete(
    State(st): State<AppState>,
    Json(req): Json<ClaudeCodeCompleteReq>,
) -> Result<Json<claude_code_slot::LoginResult>, (StatusCode, String)> {
    let callback = req.callback.clone();
    let result = tokio::task::spawn_blocking(move || claude_code_slot::login_complete(&callback))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map_err(|e| (StatusCode::BAD_GATEWAY, e))?;
    if result.status == "complete" {
        let _ = secrets::set_active_model("claude-code");
        reload_model(&st);
    }
    Ok(Json(result))
}

pub(crate) async fn auth_gemini_import_cli(
    State(st): State<AppState>,
) -> Result<Json<ImportResp>, (StatusCode, String)> {
    let message = tokio::task::spawn_blocking(gemini_auth::import_cli_credentials)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    let _ = secrets::set_active_model("gemini");
    reload_model(&st);
    Ok(Json(ImportResp {
        message,
        status: secrets::status(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;
    use bakery_agent::StubModel;
    use std::sync::Arc;

    /// 06-SEATS audit item 4: a credential change must not leave a stale
    /// cached seat resolution behind, or a seat resolved before the change
    /// would keep answering with whatever it resolved to beforehand.
    #[test]
    fn reload_model_clears_every_cached_seat_resolution() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));

        crate::state::seed_seat_model_for_test(
            &state,
            "wt:some-stream",
            crate::node::Seat::Orchestrator,
            Arc::new(StubModel),
        );
        assert!(!state.seats.lock().unwrap().is_empty());

        reload_model(&state);

        assert!(
            state.seats.lock().unwrap().is_empty(),
            "reload_model must evict every cached seat resolution"
        );
    }

    #[test]
    fn plug_secret_set_and_clear_round_trip_through_the_routes_and_evict_the_seat_cache() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let secrets_path = dir.path().join("secrets.json");
        bakery_agent::secrets::test_support::with_isolated_credentials(&secrets_path, || {
            crate::state::seed_seat_model_for_test(
                &state,
                "wt:some-stream",
                crate::node::Seat::Orchestrator,
                Arc::new(StubModel),
            );
            assert!(!state.seats.lock().unwrap().is_empty());

            let rt = tokio::runtime::Runtime::new().unwrap();
            let status = rt
                .block_on(auth_set_plug_secret(
                    axum::extract::State(state.clone()),
                    axum::Json(PlugSecretReq {
                        plug_id: "local:ollama".into(),
                        key: "test-only-bearer".into(),
                    }),
                ))
                .unwrap()
                .0;
            assert!(status.secret_set);
            assert!(bakery_agent::secrets::plug_secret_set("local:ollama"));
            assert!(
                state.seats.lock().unwrap().is_empty(),
                "setting a plug secret must evict the seat cache"
            );

            let status = rt
                .block_on(auth_clear_plug_secret(
                    axum::extract::State(state.clone()),
                    axum::Json(PlugSecretClearReq {
                        plug_id: "local:ollama".into(),
                    }),
                ))
                .unwrap()
                .0;
            assert!(!status.secret_set);
            assert!(!bakery_agent::secrets::plug_secret_set("local:ollama"));
        });
    }

    #[test]
    fn claude_code_account_routes_complete_without_returning_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let secrets_path = dir.path().join("secrets.json");
        bakery_agent::secrets::test_support::with_isolated_credentials(&secrets_path, || {
            std::env::set_var("BAKERY_CLAUDE_CODE_PROVIDER_FOR_TEST", "1");
            crate::state::seed_seat_model_for_test(
                &state,
                "wt:some-stream",
                crate::node::Seat::Orchestrator,
                Arc::new(StubModel),
            );

            let rt = tokio::runtime::Runtime::new().unwrap();
            let start = rt.block_on(auth_claude_code_login_start()).unwrap().0;
            assert!(start.built);
            assert_eq!(start.verification_uri, "https://claude.ai/test-only-login");

            let done = rt
                .block_on(auth_claude_code_login_complete(
                    axum::extract::State(state.clone()),
                    axum::Json(ClaudeCodeCompleteReq {
                        callback: "test-only-callback".into(),
                    }),
                ))
                .unwrap()
                .0;
            assert_eq!(done.status, "complete");
            assert!(done.error.is_none());
            assert!(state.seats.lock().unwrap().is_empty());

            let public = serde_json::to_string(&bakery_agent::secrets::status()).unwrap();
            assert!(!public.contains("test-only-callback"));
            assert!(!public.contains("test-only-claude-access"));
            assert!(!public.contains("test-only-claude-refresh"));
            assert!(bakery_agent::secrets::status().anthropic.oauth_set);
            assert_eq!(bakery_agent::secrets::status().active_model, "claude-code");
        });
    }

    /// A plain `#[test]` (not `#[tokio::test]`) — `cli_tools::test_support::with_test_bins`
    /// serializes on a process-wide lock shared with `routes::terminal`'s own
    /// tests, which also point `BAKERY_CODEX_BIN_FOR_TEST`/
    /// `BAKERY_CURSOR_BIN_FOR_TEST` at fixtures (07d-A3D1 review finding 6 —
    /// two tests each mutating the same env vars with no shared lock is a
    /// real, if intermittent, flake). A `std::sync::Mutex` is not reentrant,
    /// so the closure it runs must not itself be an already-running tokio
    /// task — a fresh `Runtime::block_on` inside is this crate's established
    /// way to run async code there (see `claude_code_account_routes_complete_without_returning_credentials` above).
    #[test]
    fn cli_status_reports_not_installed_for_a_missing_binary() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let status = cli_tools::test_support::with_test_bins(
            Some(std::path::Path::new("/nonexistent/bin/not-codex")),
            Some(std::path::Path::new("/nonexistent/bin/not-cursor")),
            || {
                rt.block_on(auth_cli_status(Query(CliStatusQuery {
                    force: Some("1".into()),
                })))
                .unwrap()
                .0
            },
        );

        assert_eq!(
            status.codex,
            cli_tools::CliAccountStatus {
                installed: false,
                linked: false
            }
        );
        assert_eq!(
            status.cursor,
            cli_tools::CliAccountStatus {
                installed: false,
                linked: false
            }
        );
    }
}
