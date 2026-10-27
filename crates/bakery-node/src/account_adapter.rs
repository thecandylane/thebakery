//! Account capability boundary for the composer.
//!
//! Model pickers must not infer subscription capabilities from a brand name.
//! HTTP providers have a live list-models adapter; subscription/ACP accounts
//! can surface a report only when their account adapter actually supplied one.
//! ACP session configuration *does* expose model/reasoning selectors when an
//! adapter advertises them; absence on one adapter/session stays unsupported
//! rather than being generalized into the stale claim that ACP cannot select
//! models.

use crate::state::AppState;
use bakery_agent::{EffortLevel, Model, ModelCatalog, ModelCatalogEntry};
use serde::Serialize;
use std::time::Duration;

/// How long one probe may take to start an adapter and read its options.
const PROBE_TIMEOUT: Duration = Duration::from_secs(40);

#[derive(schemars::JsonSchema, Debug, Clone, Serialize)]
pub(crate) struct AccountCapabilities {
    /// `live_provider`, `subscription_adapter`, `native_session`, `remembered`
    /// (what the account last reported, kept across restarts),
    /// `configured_endpoint`, or `unavailable`. This lets a caller distinguish a real enumeration from
    /// one known configured endpoint without parsing a prose reason.
    pub(crate) source: &'static str,
    pub(crate) catalog: ModelCatalog,
    /// Transport-specific prompt attachment capabilities. For a native ACP
    /// session these are the exact values advertised during setup, rather
    /// than a UI guess about images, audio, or arbitrary binaries.
    pub(crate) text_attachments: bool,
    pub(crate) image_attachments: bool,
    pub(crate) audio_attachments: bool,
    /// Bakery can enforce Plan on its HTTP tool loop. A native runtime can
    /// enter Plan only when this exact ACP session advertises that mode;
    /// otherwise Bakery refuses the selection before dispatch.
    pub(crate) plan_enforceable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) native_runtime: Option<crate::pipe::AcpRuntimeSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) availability_reason: Option<String>,
    pub(crate) metadata_provenance: &'static str,
    /// This account has a model list to show. `false` for an arbitrary or
    /// legacy id with no catalog concept at all — the picker then offers
    /// nothing and says nothing, rather than reporting a failure.
    pub(crate) catalog_expected: bool,
}

fn base_capabilities(
    source: &'static str,
    catalog: ModelCatalog,
    text_attachments: bool,
    plan_enforceable: bool,
) -> AccountCapabilities {
    AccountCapabilities {
        source,
        catalog,
        text_attachments,
        image_attachments: false,
        audio_attachments: false,
        plan_enforceable,
        native_runtime: None,
        availability_reason: None,
        metadata_provenance: source,
        catalog_expected: true,
    }
}

fn unavailable(id: &str, reason: impl Into<String>) -> ModelCatalog {
    crate::model_catalog::unavailable(id, reason.into())
}

fn declared_catalog(report: &crate::node::SubscriptionCapabilities) -> ModelCatalog {
    ModelCatalog::Available {
        models: report
            .models
            .iter()
            .map(|model| ModelCatalogEntry {
                id: model.id.clone(),
                label: model.label.clone(),
                context_window: model.context_window,
                effort_levels: model
                    .effort_levels
                    .iter()
                    .map(|effort| EffortLevel {
                        id: effort.id.clone(),
                        label: effort.label.clone(),
                    })
                    .collect(),
            })
            .collect(),
    }
}

/// How far a request for an account's capabilities may go to find its models.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Probe {
    /// Only what is already known: a live session, or the remembered list.
    Never,
    /// Also start the adapter and ask, when nothing is known — a new chat has
    /// no conversation yet, and every restart forgets every session.
    IfNeeded,
    /// Ask the adapter now, even after a recent failure (the picker's Retry).
    Force,
}

/// Resolve an account's capability truth (see [`capabilities_with`]), starting
/// the adapter to ask when nothing else knows the account's models.
pub(crate) async fn capabilities(state: &AppState, id: &str) -> AccountCapabilities {
    capabilities_with(state, id, Probe::IfNeeded).await
}

/// Resolve an account's capability truth. The only awaited branches are a live
/// HTTP provider fetch and a native probe; node config is copied before
/// awaiting so its mutex is never held across I/O.
pub(crate) async fn capabilities_with(
    state: &AppState,
    id: &str,
    probe: Probe,
) -> AccountCapabilities {
    let id = id.trim();
    let (is_subscription, report, compat_model) = {
        let node = state.node.lock().unwrap();
        (
            node.acp_plugs.iter().any(|plug| plug.id == id)
                || matches!(id, "acp" | "claude-code" | "claude-native"),
            node.subscription_capabilities.get(id).cloned(),
            node.openai_compat_plugs
                .iter()
                .find(|plug| plug.id == id)
                .map(|plug| plug.model.clone()),
        )
    };
    if is_subscription {
        if let Some(report) = report.filter(|r| !r.models.is_empty()) {
            let bakery_owned_tool_loop = id == "claude-code";
            return base_capabilities(
                "subscription_adapter",
                declared_catalog(&report),
                bakery_owned_tool_loop,
                bakery_owned_tool_loop,
            );
        }
        // Claude Code is a subscription account but, unlike an opaque ACP
        // child, its installed provider participates in Bakery's Model seam.
        // Ask that adapter lazily when the picker opens and cache only the
        // metadata it returns. The public slot never exposes credentials.
        if id == "claude-code" {
            let catalog = state
                .catalog_cache
                .clone()
                .get(id, crate::model_catalog::fetch_live_provider_catalog)
                .await;
            let source = if matches!(&catalog, ModelCatalog::Available { .. }) {
                "subscription_adapter"
            } else {
                "unavailable"
            };
            return base_capabilities(source, catalog, true, true);
        }
        // Any live session on this account already knows its models and
        // effort — the catalog is the account's, not one conversation's — so
        // a picker with no conversation yet (a new chat) can offer them too.
        if let Some(snapshot) = live_snapshot_for_account(state, id) {
            remember_native_catalog(state, &snapshot);
            let mut capabilities = base_capabilities(
                "native_session",
                catalog_from_native(&snapshot),
                snapshot.prompt.embedded_context,
                crate::pipe::native_mode_for("plan", &snapshot.modes).is_some(),
            );
            capabilities.image_attachments = snapshot.prompt.image;
            capabilities.audio_attachments = snapshot.prompt.audio;
            capabilities.metadata_provenance = snapshot.provenance;
            return capabilities;
        }
        // No session now — but the account may have reported its models
        // before (an earlier session, before this restart).
        if probe != Probe::Force {
            if let Some(models) = state.catalog_cache.recall(id) {
                let mut capabilities = base_capabilities(
                    "remembered",
                    ModelCatalog::Available { models },
                    false,
                    false,
                );
                capabilities.metadata_provenance = "remembered";
                return capabilities;
            }
        }
        // Never reported: start the adapter once and ask it.
        let reason = if probe == Probe::Never {
            "this subscription adapter has not established a session or advertised model, effort, or context controls".to_string()
        } else {
            match probe_native_catalog(state, id, probe == Probe::Force).await {
                Ok(models) => {
                    let mut capabilities = base_capabilities(
                        "native_session",
                        ModelCatalog::Available { models },
                        false,
                        false,
                    );
                    capabilities.metadata_provenance = "probe";
                    return capabilities;
                }
                Err(reason) => reason,
            }
        };
        let mut capabilities =
            base_capabilities("unavailable", unavailable(id, reason.clone()), false, false);
        capabilities.availability_reason = Some(reason);
        return capabilities;
    }
    if crate::model_catalog::is_cache_eligible(id) {
        return base_capabilities(
            "live_provider",
            state
                .catalog_cache
                .clone()
                .get(id, crate::model_catalog::fetch_live_provider_catalog)
                .await,
            true,
            true,
        );
    }
    if let Some(model) = compat_model {
        // The endpoint's configured model is a true account fact, but no
        // context/effort values are invented without an adapter report.
        return base_capabilities(
            "configured_endpoint",
            ModelCatalog::Available {
                models: vec![ModelCatalogEntry {
                    id: model.clone(),
                    label: model,
                    context_window: None,
                    effort_levels: Vec::new(),
                }],
            },
            true,
            true,
        );
    }
    let mut capabilities = base_capabilities(
        "unavailable",
        // Preserve the longstanding wire wording for an arbitrary/legacy id
        // while `source` tells callers this is adapter absence, not a live
        // lookup.
        unavailable(id, "this model does not report a live catalog"),
        false,
        false,
    );
    capabilities.availability_reason =
        Some("this model does not report a live catalog".to_string());
    capabilities.catalog_expected = false;
    capabilities
}

/// Remembers the models a native session reports, so the account has a list
/// when no session is live (see `ModelCatalogCache::remembered`).
pub(crate) fn remember_native_catalog(state: &AppState, snapshot: &crate::pipe::AcpRuntimeSnapshot) {
    if let ModelCatalog::Available { models } = catalog_from_native(snapshot) {
        state.catalog_cache.remember(&snapshot.account_id, &models);
    }
}

/// Starts a throwaway session on `id` just to read the models, efforts and
/// controls it advertises, then closes it. One at a time node-wide (each is an
/// adapter process), and a failure is not retried for half a minute unless
/// `force` — a reopened picker must not keep starting processes. The reason
/// returned on failure is what the picker shows.
async fn probe_native_catalog(
    state: &AppState,
    id: &str,
    force: bool,
) -> Result<Vec<ModelCatalogEntry>, String> {
    let cache = state.catalog_cache.clone();
    if !force {
        if let Some(reason) = cache.recent_probe_failure(id) {
            return Err(reason);
        }
    }
    let _one_at_a_time = cache.probe_gate().lock().await;
    // Another request may have answered while this waited its turn.
    if !force {
        if let Some(models) = cache.recall(id) {
            return Ok(models);
        }
    }
    let (probe_state, account) = (state.clone(), id.to_string());
    let asked = tokio::time::timeout(
        PROBE_TIMEOUT,
        tokio::task::spawn_blocking(move || -> Result<Vec<ModelCatalogEntry>, String> {
            let model = crate::state::probe_model_for_account(&probe_state, &account)?;
            model.ensure_runtime_ready()?;
            let snapshot = crate::pipe::acp_runtime_snapshot(
                &probe_state,
                &crate::state::probe_stream_id(&account),
                crate::node::Seat::Orchestrator,
            )
            .ok_or_else(|| "the adapter started but published no capability state".to_string())?;
            match catalog_from_native(&snapshot) {
                ModelCatalog::Available { models } => Ok(models),
                ModelCatalog::Unavailable { reason, .. } => Err(reason),
            }
            // `model` drops here, which closes the session and its process.
        }),
    )
    .await;
    let outcome = match asked {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => Err(format!("the model probe failed: {error}")),
        Err(_) => Err(format!(
            "the adapter did not answer within {} seconds",
            PROBE_TIMEOUT.as_secs()
        )),
    };
    match &outcome {
        Ok(models) => {
            cache.remember(id, models);
            cache.clear_probe_failure(id);
        }
        Err(reason) => cache.note_probe_failure(id, reason),
    }
    outcome
}

/// The snapshot of any live native session signed in to `account_id`.
fn live_snapshot_for_account(
    state: &AppState,
    account_id: &str,
) -> Option<crate::pipe::AcpRuntimeSnapshot> {
    let keys: Vec<(String, crate::node::Seat)> =
        state.acp_sessions.lock().unwrap().keys().cloned().collect();
    keys.into_iter()
        .filter_map(|(stream_id, seat)| crate::pipe::acp_runtime_snapshot(state, &stream_id, seat))
        .find(|snapshot| snapshot.account_id == account_id)
}

fn catalog_from_native(snapshot: &crate::pipe::AcpRuntimeSnapshot) -> ModelCatalog {
    let model_option = snapshot
        .config_options
        .iter()
        .find(|option| option.category.as_deref() == Some("model"));
    let effort_option = snapshot
        .config_options
        .iter()
        .find(|option| option.category.as_deref() == Some("thought_level"));
    let Some(model_option) = model_option else {
        return unavailable(
            &snapshot.account_id,
            "this native session did not advertise a model configuration option",
        );
    };
    let effective = snapshot.effective_model.as_deref();
    ModelCatalog::Available {
        models: model_option
            .values
            .iter()
            .map(|model| ModelCatalogEntry {
                id: model.id.clone(),
                label: model.name.clone(),
                // A usage update describes the active session/model only;
                // never project that capacity onto every other choice.
                context_window: (effective == Some(model.id.as_str()))
                    .then(|| snapshot.usage.as_ref().map(|usage| usage.size))
                    .flatten(),
                effort_levels: if effective == Some(model.id.as_str()) {
                    effort_option
                        .map(|option| {
                            option
                                .values
                                .iter()
                                .map(|effort| EffortLevel {
                                    id: effort.id.clone(),
                                    label: effort.name.clone(),
                                })
                                .collect()
                        })
                        .unwrap_or_default()
                } else {
                    Vec::new()
                },
            })
            .collect(),
    }
}

/// Resolve the account boundary for one concrete agent/session generation.
/// Session initialization is process work, so the caller invokes this from
/// an async route and this function keeps its blocking portion explicit.
pub(crate) async fn capabilities_for_agent(
    state: &AppState,
    id: &str,
    stream_id: &str,
    seat: crate::node::Seat,
) -> AccountCapabilities {
    // The agent's own session answers below, so no probe is started here.
    let mut capabilities = capabilities_with(state, id, Probe::Never).await;
    let state2 = state.clone();
    let stream_id2 = stream_id.to_string();
    let runtime = tokio::task::spawn_blocking(move || {
        let model = crate::state::resolve_seat(&state2, &stream_id2, seat)?;
        if model.transport() != bakery_agent::Transport::Acp {
            return Ok(None);
        }
        model.ensure_runtime_ready()?;
        crate::pipe::acp_runtime_snapshot(&state2, &stream_id2, seat)
            .map(Some)
            .ok_or_else(|| {
                "ACP session completed setup but published no capability state".to_string()
            })
    })
    .await;
    match runtime {
        Ok(Ok(Some(snapshot))) if snapshot.account_id == id => {
            remember_native_catalog(state, &snapshot);
            capabilities.source = "native_session";
            capabilities.catalog = catalog_from_native(&snapshot);
            capabilities.text_attachments = snapshot.prompt.embedded_context;
            capabilities.image_attachments = snapshot.prompt.image;
            capabilities.audio_attachments = snapshot.prompt.audio;
            capabilities.plan_enforceable =
                crate::pipe::native_mode_for("plan", &snapshot.modes).is_some();
            capabilities.availability_reason = None;
            capabilities.metadata_provenance = snapshot.provenance;
            capabilities.native_runtime = Some(snapshot);
        }
        Ok(Ok(Some(snapshot))) => {
            capabilities.availability_reason = Some(format!(
                "agent session belongs to account '{}', not requested account '{id}'",
                snapshot.account_id
            ));
        }
        Ok(Ok(None)) => {}
        Ok(Err(reason)) => capabilities.availability_reason = Some(reason),
        Err(error) => capabilities.availability_reason = Some(error.to_string()),
    }
    capabilities
}

/// Context data is reused by the running tool loop without triggering any
/// network fetch. A reported subscription model wins; live provider cache is
/// the existing fallback for fixed HTTP accounts.
pub(crate) fn cached_context_window(
    state: &AppState,
    account_id: &str,
    fallback_provider: &str,
    model_id: &str,
) -> Option<u64> {
    let reported = state
        .node
        .lock()
        .unwrap()
        .subscription_capabilities
        .get(account_id)
        .and_then(|report| report.models.iter().find(|model| model.id == model_id))
        .and_then(|model| model.context_window);
    reported.or_else(|| {
        state
            .catalog_cache
            .cached_context_window(fallback_provider, model_id)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::{
        AcpPlugConfig, SubscriptionCapabilities, SubscriptionEffortCapability,
        SubscriptionModelCapability,
    };
    use crate::testutil::{state_with, StubModel};
    use std::sync::Arc;

    #[tokio::test]
    async fn subscription_adapter_exposes_reported_model_effort_and_context_without_guessing() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let mut node = state.node.lock().unwrap();
        node.acp_plugs.push(AcpPlugConfig {
            id: "acp:subscription".to_string(),
            label: Some("Subscription adapter".to_string()),
            argv: vec!["subscription-agent".to_string()],
            config: None,
            env: Default::default(),
            mcp_transport: "stdio".to_string(),
        });
        node.subscription_capabilities.insert(
            "acp:subscription".to_string(),
            SubscriptionCapabilities {
                models: vec![SubscriptionModelCapability {
                    id: "actual-subscription-model".to_string(),
                    label: "Actual subscription model".to_string(),
                    context_window: Some(777_000),
                    effort_levels: vec![SubscriptionEffortCapability {
                        id: "max".to_string(),
                        label: "Max".to_string(),
                    }],
                }],
            },
        );
        drop(node);

        let capabilities = capabilities(&state, "acp:subscription").await;
        assert_eq!(capabilities.source, "subscription_adapter");
        let ModelCatalog::Available { models } = capabilities.catalog else {
            panic!("expected reported capabilities")
        };
        assert_eq!(models[0].id, "actual-subscription-model");
        assert_eq!(models[0].context_window, Some(777_000));
        assert_eq!(models[0].effort_levels[0].id, "max");
        assert_eq!(
            cached_context_window(
                &state,
                "acp:subscription",
                "acp",
                "actual-subscription-model"
            ),
            Some(777_000),
        );
    }

    fn fake_plug(state: &AppState, id: &str) {
        let mut node = state.node.lock().unwrap();
        node.acp_plugs.push(AcpPlugConfig {
            id: id.to_string(),
            label: None,
            argv: vec![crate::testutil::fake_acp_agent_binary().to_string_lossy().into_owned()],
            config: None,
            // Turns the fake agent's capability fixture on (see its
            // `capability_fixture_enabled`): a plug may pass only this key.
            env: [("CODEX_HOME".to_string(), "/tmp".to_string())].into(),
            mcp_transport: "stdio".to_string(),
        });
    }

    fn model_ids(capabilities: &AccountCapabilities) -> Vec<String> {
        match &capabilities.catalog {
            ModelCatalog::Available { models } => models.iter().map(|m| m.id.clone()).collect(),
            ModelCatalog::Unavailable { reason, .. } => panic!("expected models, got: {reason}"),
        }
    }

    /// The operator's report: "I can't select models in chat". A node that
    /// has just restarted has no session on a native account, so its model
    /// list was empty and the picker never appeared. Asking an account with
    /// no session now starts the adapter once, reads its options, closes it,
    /// and remembers them — durably, so a restart does not forget again.
    #[tokio::test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    async fn an_account_with_no_session_reports_its_models_by_probing_and_remembers_them() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = state_with(dir.path(), Arc::new(StubModel));
        state.catalog_cache = Arc::new(crate::model_catalog::ModelCatalogCache::persisted(
            dir.path().join(crate::model_catalog::MEMORY_FILE),
        ));
        fake_plug(&state, "acp:fake");

        let asked = capabilities(&state, "acp:fake").await;
        assert_eq!(asked.source, "native_session", "{:?}", asked.availability_reason);
        assert_eq!(asked.metadata_provenance, "probe");
        assert_eq!(model_ids(&asked), vec!["fake-opus", "fake-sonnet"]);

        // The probe's session is closed (its process exit is noticed a
        // moment later), and it is nobody's conversation.
        assert!(crate::pipe::acp_runtime_snapshot(
            &state,
            &crate::state::probe_stream_id("acp:fake"),
            crate::node::Seat::Orchestrator,
        )
        .is_none());
        let mut waited = 0;
        while !state.acp_sessions.lock().unwrap().is_empty() {
            assert!(waited < 100, "the probe session never cleared from the registry");
            waited += 1;
            std::thread::sleep(std::time::Duration::from_millis(50));
        }

        // Remembered on disk: a fresh cache (a restarted node) answers with
        // no adapter running at all.
        let restarted = crate::model_catalog::ModelCatalogCache::persisted(
            dir.path().join(crate::model_catalog::MEMORY_FILE),
        );
        let recalled = restarted.recall("acp:fake").expect("remembered across a restart");
        assert_eq!(recalled.len(), 2);
        let mut after_restart = state_with(dir.path(), Arc::new(StubModel));
        after_restart.catalog_cache = Arc::new(restarted);
        fake_plug(&after_restart, "acp:fake"); // node.json brings its plugs back
        let again = capabilities_with(&after_restart, "acp:fake", Probe::Never).await;
        assert_eq!(again.source, "remembered");
        assert_eq!(model_ids(&again), vec!["fake-opus", "fake-sonnet"]);
    }

    /// An adapter that cannot start (not signed in, Node missing) says why,
    /// once: a picker that is reopened must not start another process each
    /// time. Retry (`Force`) tries again regardless.
    #[tokio::test]
    async fn a_failed_probe_says_why_and_is_not_repeated_until_retried() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        state.node.lock().unwrap().acp_plugs.push(AcpPlugConfig {
            id: "acp:missing".to_string(),
            label: None,
            argv: vec!["/definitely/not/a/binary".to_string()],
            config: None,
            env: Default::default(),
            mcp_transport: "stdio".to_string(),
        });
        let first = capabilities(&state, "acp:missing").await;
        assert_eq!(first.source, "unavailable");
        let reason = first.availability_reason.clone().expect("a reason the picker can show");
        assert!(reason.contains("binary not found"), "{reason}");
        assert_eq!(
            state.catalog_cache.recent_probe_failure("acp:missing").as_deref(),
            Some(reason.as_str())
        );
        // Within the retry window the same failure is returned without a new attempt.
        let second = capabilities(&state, "acp:missing").await;
        assert_eq!(second.availability_reason, Some(reason));
    }

    /// An HTTP provider that cannot be reached (offline, an outage) falls back
    /// to the list it last reported instead of an empty picker.
    #[tokio::test]
    async fn an_unreachable_provider_falls_back_to_its_last_reported_models() {
        fn offline(_id: &str) -> Result<ModelCatalog, String> {
            Err("network unreachable".to_string())
        }
        let cache = Arc::new(crate::model_catalog::ModelCatalogCache::default());
        let reported = vec![ModelCatalogEntry {
            id: "grok-4".to_string(),
            label: "Grok 4".to_string(),
            context_window: Some(256_000),
            effort_levels: Vec::new(),
        }];
        cache.remember("grok", &reported);
        match cache.clone().get("grok", offline).await {
            ModelCatalog::Available { models } => assert_eq!(models[0].id, "grok-4"),
            ModelCatalog::Unavailable { reason, .. } => panic!("fell through: {reason}"),
        }
        // With nothing remembered the failure is reported as it is.
        match Arc::new(crate::model_catalog::ModelCatalogCache::default())
            .get("gemini", offline)
            .await
        {
            ModelCatalog::Unavailable { reason, .. } => assert_eq!(reason, "network unreachable"),
            ModelCatalog::Available { .. } => panic!("nothing was remembered"),
        }
    }

    #[tokio::test]
    async fn claude_code_uses_its_subscription_report_and_bakery_tool_controls() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        state.node.lock().unwrap().subscription_capabilities.insert(
            "claude-code".to_string(),
            SubscriptionCapabilities {
                models: vec![SubscriptionModelCapability {
                    id: "claude-opus-reported".to_string(),
                    label: "Reported Opus".to_string(),
                    context_window: Some(1_000_000),
                    effort_levels: vec![SubscriptionEffortCapability {
                        id: "max".to_string(),
                        label: "Max".to_string(),
                    }],
                }],
            },
        );

        let capabilities = capabilities(&state, "claude-code").await;
        assert_eq!(capabilities.source, "subscription_adapter");
        assert!(capabilities.text_attachments);
        assert!(capabilities.plan_enforceable);
        let ModelCatalog::Available { models } = capabilities.catalog else {
            panic!("expected the Claude Code adapter's reported catalog")
        };
        assert_eq!(models[0].id, "claude-opus-reported");
        assert_eq!(models[0].context_window, Some(1_000_000));
        assert_eq!(models[0].effort_levels[0].id, "max");
    }
}
