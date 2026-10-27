//! Provider plug CRUD (07d-A3B review #4): Settings → Providers previously
//! could only list configured ACP/openai-compat plugs and set/clear an
//! openai-compat bearer — adding, editing, or removing a plug definition
//! itself required hand-editing `~/.bakery/node.json`. These routes are the
//! supported mutation path; hand-editing the file still works (nothing here
//! stops it) but only this path enforces id uniqueness/reserved-name
//! validation (`NodeConfig::validate_new_plug_id`).
//!
//! A plug's `id` is immutable once created — the stable identity a seat
//! assignment (`SeatAssignment.model_id`) and every Heart event since then
//! points at by that exact string. Update changes everything else
//! (`label`/`argv`/`config`, or `base_url`/`model`); it never renames.

use crate::node::{AcpConfigCopy, AcpPlugConfig, OpenAiCompatPlugConfig};
use crate::state::AppState;
use axum::{extract::State, http::StatusCode, Json};
use serde::{Deserialize, Serialize};

#[derive(schemars::JsonSchema, Serialize)]
pub(crate) struct PlugListResp {
    pub(crate) acp_plugs: Vec<AcpPlugConfig>,
    pub(crate) openai_compat_plugs: Vec<OpenAiCompatPlugConfig>,
}

pub(crate) async fn providers_list(State(st): State<AppState>) -> Json<PlugListResp> {
    let node = st.node.lock().unwrap();
    Json(PlugListResp {
        acp_plugs: node.acp_plugs.clone(),
        openai_compat_plugs: node.openai_compat_plugs.clone(),
    })
}

/// Reload-model-shaped invalidation: a plug CRUD write can change which
/// live `Arc<dyn Model>` a seat should be answering with (a newly added
/// plug, an edited argv/base_url, a removed one) — evict every cached seat
/// resolution the same way `routes::auth::reload_model` does for the fixed
/// HTTP providers, rather than leaving a stale cache entry to answer with
/// the pre-edit configuration until some unrelated reassignment clears it.
fn evict_seat_cache(st: &AppState) {
    st.seats.lock().unwrap().clear();
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct AcpPlugCreateReq {
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) label: Option<String>,
    pub(crate) argv: Vec<String>,
    #[serde(default)]
    pub(crate) config: Option<AcpConfigCopy>,
    #[serde(default)]
    pub(crate) env: std::collections::HashMap<String, String>,
    /// 07d-B Phase 2 Stage C item 4 — `"stdio"` (default) or `"none"`.
    #[serde(default = "crate::node::default_mcp_transport")]
    pub(crate) mcp_transport: String,
}

pub(crate) async fn acp_plug_create(
    State(st): State<AppState>,
    Json(req): Json<AcpPlugCreateReq>,
) -> Result<Json<AcpPlugConfig>, (StatusCode, String)> {
    if req.argv.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "argv must not be empty — argv[0] is the binary".to_string(),
        ));
    }
    crate::state::validate_acp_plug_env(&req.env).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    let plug = AcpPlugConfig {
        id: req.id.trim().to_string(),
        label: req.label,
        argv: req.argv,
        config: req.config,
        env: req.env,
        mcp_transport: req.mcp_transport,
    };
    {
        let mut node = st.node.lock().unwrap();
        node.validate_new_plug_id(&plug.id)
            .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
        node.acp_plugs.push(plug.clone());
        node.save(&st.base)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    }
    evict_seat_cache(&st);
    Ok(Json(plug))
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct AcpPlugUpdateReq {
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) label: Option<String>,
    pub(crate) argv: Vec<String>,
    #[serde(default)]
    pub(crate) config: Option<AcpConfigCopy>,
    #[serde(default)]
    pub(crate) env: std::collections::HashMap<String, String>,
    /// 07d-B Phase 2 Stage C item 4 — `"stdio"` or `"none"`. `None` here
    /// (never a `"stdio"` default) means "the caller didn't send this field
    /// at all" — `bakery-client`'s `acpPlugUpdate` today posts only
    /// `{id, argv, label, config, env}` (no Providers-panel UI for this
    /// field yet), and there is no other supported way to set
    /// `mcp_transport: "none"` besides hand-editing `node.json`. Defaulting
    /// a MISSING field to `"stdio"` used to silently overwrite that
    /// hand-set `"none"` on the next unrelated edit (rename/argv fix) —
    /// review finding — so a missing field must leave the stored value
    /// alone; only an explicitly sent value may change it.
    #[serde(default)]
    pub(crate) mcp_transport: Option<String>,
}

pub(crate) async fn acp_plug_update(
    State(st): State<AppState>,
    Json(req): Json<AcpPlugUpdateReq>,
) -> Result<Json<AcpPlugConfig>, (StatusCode, String)> {
    if req.argv.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "argv must not be empty — argv[0] is the binary".to_string(),
        ));
    }
    let id = req.id.trim().to_string();
    let updated = {
        let mut node = st.node.lock().unwrap();
        let existing = node
            .acp_plugs
            .iter_mut()
            .find(|p| p.id == id)
            .ok_or_else(|| {
                (
                    StatusCode::NOT_FOUND,
                    format!("no ACP plug configured with id '{id}'"),
                )
            })?;
        crate::state::validate_acp_plug_env(&req.env).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
        existing.label = req.label;
        existing.argv = req.argv;
        existing.config = req.config;
        existing.env = req.env;
        if let Some(mcp_transport) = req.mcp_transport {
            existing.mcp_transport = mcp_transport;
        }
        let updated = existing.clone();
        node.save(&st.base)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
        updated
    };
    evict_seat_cache(&st);
    Ok(Json(updated))
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct PlugIdReq {
    pub(crate) id: String,
}

pub(crate) async fn acp_plug_delete(
    State(st): State<AppState>,
    Json(req): Json<PlugIdReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    {
        let mut node = st.node.lock().unwrap();
        let before = node.acp_plugs.len();
        node.acp_plugs.retain(|p| p.id != req.id);
        if node.acp_plugs.len() == before {
            return Err((
                StatusCode::NOT_FOUND,
                format!("no ACP plug configured with id '{}'", req.id),
            ));
        }
        node.save(&st.base)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    }
    evict_seat_cache(&st);
    // A real (empty) JSON body, not 204 No Content — the client's `send()`
    // always calls `JSON.parse` on the response text, which throws on "".
    Ok(Json(serde_json::json!({})))
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct OpenAiCompatPlugCreateReq {
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) label: Option<String>,
    pub(crate) base_url: String,
    pub(crate) model: String,
}

pub(crate) async fn openai_compat_plug_create(
    State(st): State<AppState>,
    Json(req): Json<OpenAiCompatPlugCreateReq>,
) -> Result<Json<OpenAiCompatPlugConfig>, (StatusCode, String)> {
    bakery_agent::openai_compat::validate_base_url(&req.base_url)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    if req.model.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "model must not be empty".to_string(),
        ));
    }
    let plug = OpenAiCompatPlugConfig {
        id: req.id.trim().to_string(),
        label: req.label,
        base_url: req.base_url,
        model: req.model,
    };
    {
        let mut node = st.node.lock().unwrap();
        node.validate_new_plug_id(&plug.id)
            .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
        node.openai_compat_plugs.push(plug.clone());
        node.save(&st.base)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    }
    evict_seat_cache(&st);
    Ok(Json(plug))
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct OpenAiCompatPlugUpdateReq {
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) label: Option<String>,
    pub(crate) base_url: String,
    pub(crate) model: String,
}

pub(crate) async fn openai_compat_plug_update(
    State(st): State<AppState>,
    Json(req): Json<OpenAiCompatPlugUpdateReq>,
) -> Result<Json<OpenAiCompatPlugConfig>, (StatusCode, String)> {
    bakery_agent::openai_compat::validate_base_url(&req.base_url)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    if req.model.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "model must not be empty".to_string(),
        ));
    }
    let id = req.id.trim().to_string();
    let updated = {
        let mut node = st.node.lock().unwrap();
        let existing = node
            .openai_compat_plugs
            .iter_mut()
            .find(|p| p.id == id)
            .ok_or_else(|| {
                (
                    StatusCode::NOT_FOUND,
                    format!("no openai-compat plug configured with id '{id}'"),
                )
            })?;
        existing.label = req.label;
        existing.base_url = req.base_url;
        existing.model = req.model;
        let updated = existing.clone();
        node.save(&st.base)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
        updated
    };
    evict_seat_cache(&st);
    Ok(Json(updated))
}

pub(crate) async fn openai_compat_plug_delete(
    State(st): State<AppState>,
    Json(req): Json<PlugIdReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    {
        let mut node = st.node.lock().unwrap();
        let before = node.openai_compat_plugs.len();
        node.openai_compat_plugs.retain(|p| p.id != req.id);
        if node.openai_compat_plugs.len() == before {
            return Err((
                StatusCode::NOT_FOUND,
                format!("no openai-compat plug configured with id '{}'", req.id),
            ));
        }
        node.save(&st.base)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    }
    evict_seat_cache(&st);
    Ok(Json(serde_json::json!({})))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::state_with;
    use bakery_agent::StubModel;
    use std::sync::Arc;

    fn acp_req(id: &str, argv: Vec<&str>) -> AcpPlugCreateReq {
        AcpPlugCreateReq {
            id: id.into(),
            label: None,
            argv: argv.into_iter().map(String::from).collect(),
            config: None,
            env: Default::default(),
            mcp_transport: crate::node::default_mcp_transport(),
        }
    }

    fn oc_req(id: &str, base_url: &str, model: &str) -> OpenAiCompatPlugCreateReq {
        OpenAiCompatPlugCreateReq {
            id: id.into(),
            label: None,
            base_url: base_url.into(),
            model: model.into(),
        }
    }

    #[tokio::test]
    async fn acp_plug_create_list_update_delete_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));

        let created = acp_plug_create(
            State(state.clone()),
            Json(acp_req("acp:codex", vec!["codex", "acp"])),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(created.id, "acp:codex");

        let list = providers_list(State(state.clone())).await.0;
        assert_eq!(list.acp_plugs.len(), 1);
        assert_eq!(list.acp_plugs[0].argv, vec!["codex", "acp"]);

        let updated = acp_plug_update(
            State(state.clone()),
            Json(AcpPlugUpdateReq {
                id: "acp:codex".into(),
                label: Some("Codex".into()),
                argv: vec!["codex".into(), "agent".into(), "stdio".into()],
                config: None,
                env: Default::default(),
                mcp_transport: None,
            }),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(updated.label.as_deref(), Some("Codex"));
        assert_eq!(updated.argv, vec!["codex", "agent", "stdio"]);

        acp_plug_delete(
            State(state.clone()),
            Json(PlugIdReq {
                id: "acp:codex".into(),
            }),
        )
        .await
        .unwrap()
        .0;
        assert!(providers_list(State(state.clone()))
            .await
            .0
            .acp_plugs
            .is_empty());
    }

    #[tokio::test]
    async fn acp_plug_create_refuses_a_reserved_id() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let err = acp_plug_create(State(state), Json(acp_req("grok", vec!["codex"])))
            .await
            .unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert!(err.1.contains("reserved"), "{}", err.1);
    }

    #[tokio::test]
    async fn acp_plug_create_refuses_a_duplicate_id_across_both_plug_kinds() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let _ = openai_compat_plug_create(
            State(state.clone()),
            Json(oc_req("shared-id", "http://127.0.0.1:1", "m")),
        )
        .await
        .unwrap();
        let err = acp_plug_create(State(state), Json(acp_req("shared-id", vec!["codex"])))
            .await
            .unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert!(err.1.contains("already configured"), "{}", err.1);
    }

    #[tokio::test]
    async fn acp_plug_create_refuses_a_non_allowlisted_env_key() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let mut req = acp_req("acp:agy", vec!["agy-acp"]);
        req.env.insert("SECRET_TOKEN".into(), "nope".into());
        let err = acp_plug_create(State(state), Json(req)).await.unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert!(
            err.1.to_ascii_lowercase().contains("setup needed"),
            "{}",
            err.1
        );
    }

    #[tokio::test]
    async fn acp_plug_create_refuses_empty_argv() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let err = acp_plug_create(State(state), Json(acp_req("acp:empty", vec![])))
            .await
            .unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn acp_plug_update_and_delete_refuse_an_unknown_id() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let err = acp_plug_update(
            State(state.clone()),
            Json(AcpPlugUpdateReq {
                id: "acp:nope".into(),
                label: None,
                argv: vec!["x".into()],
                config: None,
                env: Default::default(),
                mcp_transport: None,
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::NOT_FOUND);
        let err = acp_plug_delete(
            State(state),
            Json(PlugIdReq {
                id: "acp:nope".into(),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn openai_compat_plug_create_refuses_a_bad_base_url() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let err =
            openai_compat_plug_create(State(state), Json(oc_req("local:x", "not-a-url", "m")))
                .await
                .unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn openai_compat_plug_create_list_update_delete_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));

        let created = openai_compat_plug_create(
            State(state.clone()),
            Json(oc_req(
                "local:ollama",
                "http://127.0.0.1:11434/v1",
                "llama3",
            )),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(created.id, "local:ollama");

        let updated = openai_compat_plug_update(
            State(state.clone()),
            Json(OpenAiCompatPlugUpdateReq {
                id: "local:ollama".into(),
                label: Some("Ollama".into()),
                base_url: "http://127.0.0.1:11435/v1".into(),
                model: "llama3.1".into(),
            }),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(updated.model, "llama3.1");

        openai_compat_plug_delete(
            State(state.clone()),
            Json(PlugIdReq {
                id: "local:ollama".into(),
            }),
        )
        .await
        .unwrap()
        .0;
        assert!(providers_list(State(state))
            .await
            .0
            .openai_compat_plugs
            .is_empty());
    }

    /// A CRUD write must evict any cached seat resolution — otherwise an
    /// already-resolved ACP plug's model keeps answering with a pre-edit
    /// argv/base_url until an unrelated reassignment happens to clear it.
    #[tokio::test]
    async fn a_plug_write_evicts_the_seat_cache() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        crate::state::seed_seat_model_for_test(
            &state,
            "wt:some-stream",
            crate::node::Seat::Orchestrator,
            Arc::new(StubModel),
        );
        assert!(!state.seats.lock().unwrap().is_empty());

        let _ = acp_plug_create(
            State(state.clone()),
            Json(acp_req("acp:cache-test", vec!["codex"])),
        )
        .await
        .unwrap();
        assert!(state.seats.lock().unwrap().is_empty());
    }

    /// The defect this guards (review finding): the Providers panel's own
    /// update route (`bakery-client`'s `acpPlugUpdate` posts only
    /// `{id, argv, label, config, env}` — no `mcp_transport` field exists in
    /// its UI) must never silently rewrite a hand-set `mcp_transport:
    /// "none"` back to `"stdio"` just because a later, unrelated edit
    /// (rename/argv fix) omitted the field. A plug in that state is meant to
    /// stay unusable for the permanent Baker/a task socket
    /// (`routes::studio::model_ready_for_socket`) until a human explicitly
    /// re-enables MCP — an update that "forgets" the field must not do that
    /// silently.
    #[tokio::test]
    async fn updating_a_plug_without_sending_mcp_transport_leaves_a_hand_set_none_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let _ = acp_plug_create(
            State(state.clone()),
            Json(acp_req("acp:no-mcp", vec!["some-cli"])),
        )
        .await
        .unwrap();
        // Simulate the only supported way to set this today — hand-editing
        // `node.json` — directly on the in-memory config.
        {
            let mut node = state.node.lock().unwrap();
            node.acp_plugs
                .iter_mut()
                .find(|p| p.id == "acp:no-mcp")
                .unwrap()
                .mcp_transport = "none".to_string();
        }

        // An ordinary edit (fixing argv) that does NOT mention mcp_transport
        // at all — exactly what the client's real request body looks like.
        let updated = acp_plug_update(
            State(state.clone()),
            Json(AcpPlugUpdateReq {
                id: "acp:no-mcp".into(),
                label: None,
                argv: vec!["some-cli".into(), "--fixed-flag".into()],
                config: None,
                env: Default::default(),
                mcp_transport: None,
            }),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(
            updated.mcp_transport, "none",
            "an omitted field must not silently re-enable MCP transport"
        );
        assert!(!updated.supports_mcp_stdio());

        // An explicit value, by contrast, DOES take effect.
        let re_enabled = acp_plug_update(
            State(state.clone()),
            Json(AcpPlugUpdateReq {
                id: "acp:no-mcp".into(),
                label: None,
                argv: vec!["some-cli".into()],
                config: None,
                env: Default::default(),
                mcp_transport: Some("stdio".to_string()),
            }),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(
            re_enabled.mcp_transport, "stdio",
            "an explicitly sent value must still take effect"
        );
    }
}
