//! GrokModel — xAI Grok via OpenAI-compatible chat completions.
//!
//! Auth order: secrets xAI OAuth access token → secrets/env `XAI_API_KEY`.
//! Endpoint: `https://api.x.ai/v1/chat/completions`.

use crate::secrets;
use crate::shared::{effort_label, read_models_response, strip_outer_fence, user_edit_prompt, SYSTEM_EDIT_INSTRUCTIONS};
use crate::sse::{self, DriveOutcome};
use crate::xai_oauth;
use crate::tool_wire::{extract_openai_reply, extract_openai_usage, openai_messages, openai_tools, OpenAiStreamAssembler};
use crate::{
    ChatMsg, ChatReply, EditRequest, Model, ModelCatalog, ModelCatalogEntry, EffortLevel, ProposedEdit, StopReason,
    StreamEnd, ToolDecl, TurnUsage,
};
use serde_json::json;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

const API_URL: &str = "https://api.x.ai/v1/chat/completions";
const MODELS_URL: &str = "https://api.x.ai/v1/models";
const ENV_API_KEY: &str = "XAI_API_KEY";
const ENV_MODEL: &str = "BAKERY_GROK_MODEL";
const DEFAULT_MODEL: &str = "grok-4.5";
const MAX_TOKENS: u32 = 8192;

pub struct GrokModel {
    model_id: String,
    /// Per-turn reasoning-effort override (07d-A3D1 Phase C).
    effort: Mutex<Option<String>>,
    /// This model's own report of the last completed turn's token usage
    /// (07d-A3D1 Phase C).
    last_usage: Mutex<Option<TurnUsage>>,
    last_thinking: Mutex<Option<crate::Thinking>>,
}

impl GrokModel {
    pub fn from_env() -> Self {
        let s = secrets::load();
        let model_id = s
            .grok_model_id
            .or_else(|| std::env::var(ENV_MODEL).ok())
            .unwrap_or_else(|| DEFAULT_MODEL.to_string());
        Self::with_model_id(model_id)
    }

    /// A fresh instance for a specific model id (07d-A3D1 Phase C) — see
    /// `ClaudeModel::with_model_id`'s doc for the full reasoning.
    pub fn with_model_id(model_id: impl Into<String>) -> Self {
        Self { model_id: model_id.into(), effort: Mutex::new(None), last_usage: Mutex::new(None), last_thinking: Mutex::new(None) }
    }

    /// The `reasoning_effort` body value for a currently-set per-turn effort
    /// override, factored out as a pure function (07d-A3D1 Phase C review
    /// fix) so it is directly unit-testable without a real network call —
    /// `API_URL` is hardcoded, so `call_turn`/`call_turn_streaming`
    /// themselves aren't reachable in a test. Mirrors
    /// `ClaudeModel::effort_output_config`'s equivalent shape.
    fn reasoning_effort_field(&self) -> Option<serde_json::Value> {
        let effort = self.effort.lock().unwrap().clone()?;
        Some(json!(effort))
    }

    pub fn credentials_available() -> bool {
        let s = secrets::load();
        crate::proxy::route("xai").is_some()
            || s.xai_oauth
                .as_ref()
                .is_some_and(|o| !o.access.is_empty() || !o.refresh.is_empty())
            || s.xai_api_key.as_ref().is_some_and(|k| !k.trim().is_empty())
            || std::env::var(ENV_API_KEY)
                .map(|k| !k.trim().is_empty())
                .unwrap_or(false)
    }

    /// Where a call to `direct` goes, and the bearer it sends: xAI with the
    /// node-stored OAuth first (always direct), else the model proxy with the
    /// node's token when it holds an xAI key, else xAI with the API key.
    fn target(&self, direct: &str) -> Result<(String, String), String> {
        if secrets::load().xai_oauth.is_some() {
            if let Ok(token) = xai_oauth::access_token() {
                return Ok((direct.to_string(), token));
            }
        }
        if let Some(target) = crate::proxy::via("xai", direct) {
            return Ok(target);
        }
        Ok((direct.to_string(), self.bearer()?))
    }

    fn bearer(&self) -> Result<String, String> {
        // Prefer OAuth (subscription / SuperGrok)
        if secrets::load().xai_oauth.is_some() {
            if let Ok(t) = xai_oauth::access_token() {
                return Ok(t);
            }
        }
        if let Some(k) = secrets::load().xai_api_key {
            if !k.trim().is_empty() {
                return Ok(k.trim().to_string());
            }
        }
        std::env::var(ENV_API_KEY)
            .ok()
            .map(|k| k.trim().to_string())
            .filter(|k| !k.is_empty())
            .ok_or_else(|| {
                "no xAI credentials — use client Setup (Grok login or API key)".into()
            })
    }

    fn call_turn(&self, system: &str, messages: &[ChatMsg], tools: &[ToolDecl]) -> Result<ChatReply, String> {
        let (url, token) = self.target(API_URL)?;
        let mut body = json!({
            "model": self.model_id,
            "max_tokens": MAX_TOKENS,
            "stream": false,
            "messages": openai_messages(system, messages)
        });
        if !tools.is_empty() {
            body["tools"] = openai_tools(tools);
            body["tool_choice"] = json!("auto");
        }
        if let Some(reasoning_effort) = self.reasoning_effort_field() {
            body["reasoning_effort"] = reasoning_effort;
        }

        let resp = ureq::post(&url)
            .set("Authorization", &format!("Bearer {token}"))
            .set("content-type", "application/json")
            .set("accept", "application/json")
            .timeout(std::time::Duration::from_secs(120))
            .send_json(body)
            .map_err(|e| format!("xAI request: {e}"))?;

        let status = resp.status();
        let text = resp.into_string().map_err(|e| format!("xAI body: {e}"))?;
        if !(200..300).contains(&status) {
            let snippet: String = text.chars().take(800).collect();
            return Err(format!("xAI API {status}: {snippet}"));
        }

        *self.last_usage.lock().unwrap() = Some(extract_openai_usage(&text));

        *self.last_thinking.lock().unwrap() = None;
        extract_openai_reply(&text)
    }

    fn call_chat(&self, system: &str, messages: &[ChatMsg]) -> Result<String, String> {
        match self.call_turn(system, messages, &[])? {
            ChatReply::Text(s) => Ok(s),
            ChatReply::ToolCalls(_) => Err("xAI returned tool calls on a text-only request".into()),
        }
    }

    /// `"stream": true` on the same endpoint `call_turn` posts to — SSE
    /// framing is `data: {...}` chunks terminated by the literal `data:
    /// [DONE]` (docs.x.ai/developers/model-capabilities/text/streaming);
    /// `OpenAiStreamAssembler` owns the delta/tool-call reconstruction
    /// (including excluding `reasoning_content`, which xAI's reasoning
    /// models stream as a field distinct from `content` — confirmed via
    /// xAI's own reasoning docs), this method only owns the HTTP/SSE
    /// plumbing and the cancel/stream-death outcome mapping.
    fn call_turn_streaming(
        &self,
        system: &str,
        messages: &[ChatMsg],
        tools: &[ToolDecl],
        on_text_delta: &mut dyn FnMut(&str),
        cancel: &AtomicBool,
    ) -> Result<StreamEnd, String> {
        let (url, token) = self.target(API_URL)?;
        let mut body = json!({
            "model": self.model_id,
            "max_tokens": MAX_TOKENS,
            "stream": true,
            "messages": openai_messages(system, messages)
        });
        if !tools.is_empty() {
            body["tools"] = openai_tools(tools);
            body["tool_choice"] = json!("auto");
        }
        if let Some(reasoning_effort) = self.reasoning_effort_field() {
            body["reasoning_effort"] = reasoning_effort;
        }

        let result = sse::streaming_agent()
            .post(&url)
            .set("Authorization", &format!("Bearer {token}"))
            .set("content-type", "application/json")
            .set("accept", "text/event-stream")
            .send_json(body);

        let resp = match result {
            Ok(r) => r,
            Err(ureq::Error::Status(status, resp)) => {
                let text = resp.into_string().unwrap_or_default();
                let snippet: String = text.chars().take(800).collect();
                return Err(format!("xAI API {status}: {snippet}"));
            }
            Err(e) => return Err(format!("xAI stream request: {e}")),
        };

        let mut reader = sse::SseReader::new(resp.into_reader());
        let mut assembler = OpenAiStreamAssembler::new();
        let outcome = sse::drive(&mut reader, cancel, |ev| assembler.feed(ev, on_text_delta));
        *self.last_usage.lock().unwrap() = Some(assembler.usage());
        *self.last_thinking.lock().unwrap() = assembler.thinking();

        match outcome {
            DriveOutcome::FinishedNormally => Ok(StreamEnd::Completed(assembler.finish())),
            DriveOutcome::Cancelled => Ok(StreamEnd::Stopped {
                partial_text: assembler.partial_text_so_far(),
                reason: StopReason::Cancelled,
            }),
            DriveOutcome::Eof => {
                let partial = assembler.partial_text_so_far();
                if partial.is_empty() {
                    Err("xAI stream ended before any content was received".to_string())
                } else {
                    Ok(StreamEnd::Stopped {
                        partial_text: partial,
                        reason: StopReason::StreamDied("connection closed before [DONE]".to_string()),
                    })
                }
            }
            DriveOutcome::IoError(e) => {
                let partial = assembler.partial_text_so_far();
                if partial.is_empty() {
                    Err(format!("xAI stream error: {e}"))
                } else {
                    Ok(StreamEnd::Stopped { partial_text: partial, reason: StopReason::StreamDied(e) })
                }
            }
        }
    }
}

impl Model for GrokModel {
    fn name(&self) -> &str {
        "grok"
    }

    /// xAI image understanding takes JPEG and PNG, as `image_url` data URLs.
    fn native_attachment_types(&self) -> &'static [&'static str] {
        &["image/png", "image/jpeg"]
    }

    fn propose_edit(&self, req: &EditRequest) -> Result<ProposedEdit, String> {
        if req.file_path.trim().is_empty() {
            return Err("file_path required".to_string());
        }

        let user_text = user_edit_prompt(&req.instruction, &req.file_path, &req.current_content);
        let mut raw = self.call_chat(SYSTEM_EDIT_INSTRUCTIONS, &[ChatMsg::user(user_text)])?;
        raw = strip_outer_fence(&raw);
        if raw.is_empty() {
            return Err("model returned empty content".into());
        }

        Ok(ProposedEdit {
            file_path: req.file_path.clone(),
            new_content: raw,
            rationale: format!("grok:{}", self.model_id),
        })
    }

    fn supports_chat(&self) -> bool {
        true
    }

    fn chat(&self, system: &str, messages: &[ChatMsg]) -> Result<String, String> {
        self.call_chat(system, messages)
    }

    fn chat_turn(
        &self,
        system: &str,
        messages: &[ChatMsg],
        tools: &[ToolDecl],
    ) -> Result<ChatReply, String> {
        self.call_turn(system, messages, tools)
    }

    fn chat_turn_streaming(
        &self,
        system: &str,
        messages: &[ChatMsg],
        tools: &[ToolDecl],
        on_text_delta: &mut dyn FnMut(&str),
        cancel: &AtomicBool,
    ) -> Result<StreamEnd, String> {
        self.call_turn_streaming(system, messages, tools, on_text_delta, cancel)
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    fn set_effort(&self, effort: Option<&str>) {
        *self.effort.lock().unwrap() = effort.map(str::to_string);
    }

    fn with_overridden_model_id(&self, model_id: &str) -> Option<Arc<dyn Model>> {
        Some(Arc::new(Self::with_model_id(model_id)))
    }

    fn last_turn_usage(&self) -> Option<TurnUsage> {
        *self.last_usage.lock().unwrap()
    }


    fn last_turn_thinking(&self) -> Option<crate::Thinking> {

        self.last_thinking.lock().unwrap().clone()

    }

    fn catalog(&self) -> Result<ModelCatalog, String> {
        let Ok((url, token)) = self.target(MODELS_URL) else {
            return Ok(ModelCatalog::Unavailable {
                default: ModelCatalogEntry {
                    id: self.model_id.clone(),
                    label: self.model_id.clone(),
                    context_window: None,
                    effort_levels: Vec::new(),
                },
                reason: "no xAI credentials configured".to_string(),
            });
        };
        let result = ureq::get(&url)
            .set("Authorization", &format!("Bearer {token}"))
            .set("accept", "application/json")
            .timeout(std::time::Duration::from_secs(20))
            .call();
        let text = read_models_response(result, "xAI")?;
        parse_catalog(&text)
    }
}

/// `GET /v1/models`'s effort-support isn't reported per-model in the
/// response itself — inferred from the model id (docs.x.ai/developers/
/// model-capabilities/text/reasoning, Sep 2026: `grok-4.x` accepts a
/// top-level `reasoning_effort` on `/v1/chat/completions` —
/// low/medium/high/xhigh, `xhigh` silently downgraded to `high` on 4.5).
/// Inferred from xAI's own docs, not guessed — never applied to a
/// non-reasoning model id.
fn effort_levels_for(model_id: &str) -> Vec<EffortLevel> {
    if model_id.contains("grok-4") {
        ["low", "medium", "high", "xhigh"]
            .into_iter()
            .map(|id| EffortLevel { id: id.to_string(), label: effort_label(id) })
            .collect()
    } else {
        Vec::new()
    }
}

/// Parses the standard OpenAI-compatible `{"data": [...]}` list shape —
/// `context_length` becomes this model's context window when present.
fn parse_catalog(text: &str) -> Result<ModelCatalog, String> {
    let v: serde_json::Value = serde_json::from_str(text).map_err(|e| format!("xAI models parse: {e}"))?;
    let data = v
        .get("data")
        .and_then(|d| d.as_array())
        .ok_or("xAI models: response had no \"data\" array")?;
    let mut models = Vec::new();
    for m in data {
        let Some(id) = m.get("id").and_then(|x| x.as_str()) else { continue };
        let context_window = m.get("context_length").and_then(|x| x.as_u64());
        models.push(ModelCatalogEntry {
            id: id.to_string(),
            label: id.to_string(),
            context_window,
            effort_levels: effort_levels_for(id),
        });
    }
    if models.is_empty() {
        return Err("xAI models: catalog came back empty".to_string());
    }
    Ok(ModelCatalog::Available { models })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_wire::extract_openai_reply;

    #[test]
    fn extract_openai_shaped_content() {
        let resp = r#"{"choices":[{"message":{"role":"assistant","content":"line1\nline2"}}]}"#;
        assert_eq!(extract_openai_reply(resp).unwrap(), ChatReply::Text("line1\nline2".into()));
    }

    #[test]
    fn extract_missing_content_errors() {
        assert!(extract_openai_reply(r#"{"choices":[]}"#).is_err());
    }

    #[test]
    fn parse_catalog_reads_context_length_and_infers_effort_for_grok_4_family() {
        let body = r#"{
            "object": "list",
            "data": [
                {"id": "grok-4.6", "object": "model", "created": 1, "owned_by": "xai", "context_length": 2000000},
                {"id": "grok-3-mini", "object": "model", "created": 1, "owned_by": "xai", "context_length": 131072}
            ]
        }"#;
        let catalog = parse_catalog(body).unwrap();
        let ModelCatalog::Available { models } = catalog else { panic!("expected Available") };
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "grok-4.6");
        assert_eq!(models[0].context_window, Some(2_000_000));
        assert_eq!(
            models[0].effort_levels.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            vec!["low", "medium", "high", "xhigh"]
        );
        assert_eq!(models[1].id, "grok-3-mini");
        assert_eq!(models[1].context_window, Some(131_072));
        assert!(models[1].effort_levels.is_empty(), "non grok-4 ids must get no effort levels");
    }

    #[test]
    fn parse_catalog_errors_on_an_empty_data_array() {
        assert!(parse_catalog(r#"{"object": "list", "data": []}"#).is_err());
    }

    /// `API_URL` is a hardcoded constant pointing at the real xAI endpoint
    /// (unlike `openai_compat.rs`'s runtime `base_url`) — see the identical
    /// note on `claude.rs`'s equivalent test for why a real fake-server
    /// round trip isn't reachable here, and where the shared
    /// `OpenAiStreamAssembler`/`sse::drive` machinery this method uses IS
    /// proven end-to-end (via `openai_compat.rs`, which genuinely can point
    /// at a fake local listener).
    #[test]
    fn chat_turn_streaming_fails_closed_with_no_credentials_and_touches_no_network() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        crate::secrets::test_support::with_isolated_credentials(&path, || {
            let model = GrokModel::with_model_id("grok-test");
            let cancel = std::sync::atomic::AtomicBool::new(false);
            let err = model
                .chat_turn_streaming("system", &[], &[], &mut |_| {}, &cancel)
                .map(|_| ())
                .unwrap_err();
            assert!(err.contains("no xAI credentials"), "{err}");
        });
    }

    #[test]
    fn set_effort_round_trips_through_the_interior_side_channel_and_clears_on_none() {
        let model = GrokModel::with_model_id("grok-test");
        assert_eq!(*model.effort.lock().unwrap(), None);
        model.set_effort(Some("high"));
        assert_eq!(*model.effort.lock().unwrap(), Some("high".to_string()));
        model.set_effort(None);
        assert_eq!(*model.effort.lock().unwrap(), None, "a later call with None must clear a prior value");
    }

    /// Closes the gap the round-trip test above admits it leaves (review
    /// fix): this directly asserts the exact JSON `call_turn`/
    /// `call_turn_streaming` merge into the outgoing request body, without
    /// needing network.
    #[test]
    fn reasoning_effort_field_reflects_the_currently_set_effort() {
        let model = GrokModel::with_model_id("grok-test");
        assert_eq!(model.reasoning_effort_field(), None, "no effort set yet");
        model.set_effort(Some("high"));
        assert_eq!(model.reasoning_effort_field(), Some(json!("high")));
        model.set_effort(None);
        assert_eq!(model.reasoning_effort_field(), None, "a later call with None must clear it");
    }

    /// Review fix (test-coverage gap): unlike `ClaudeModel`, `GrokModel` had
    /// no direct test of its own `with_overridden_model_id` — only an
    /// unrelated node-side test double exercised the trait method's
    /// contract at all.
    #[test]
    fn with_overridden_model_id_builds_a_distinct_instance_with_its_own_fresh_state() {
        let model = GrokModel::with_model_id("grok-original");
        model.set_effort(Some("high"));
        let overridden = model.with_overridden_model_id("grok-variant").expect("Grok supports per-turn overrides");
        assert_eq!(overridden.name(), "grok", "Model::name() is the provider bucket, not the specific variant");
        assert!(overridden.last_turn_usage().is_none(), "a fresh override instance starts with no usage");
    }
}
