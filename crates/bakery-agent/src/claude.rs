//! ClaudeModel — Anthropic Messages via **console API key** only.
//!
//! Clean path: `ANTHROPIC_API_KEY` → `x-api-key` → `POST /v1/messages`.
//! No Claude Code User-Agent, no Keychain OAuth, no client impersonation.
//!
//! Optional local-only OAuth (subscription) lives outside the public tree and
//! is selected only with `BAKERY_MODEL=claude-code` when present.

use crate::secrets;
use crate::shared::{effort_label, read_models_response, strip_outer_fence, user_edit_prompt, SYSTEM_EDIT_INSTRUCTIONS};
use crate::sse::{self, DriveOutcome};
use crate::tool_wire::{anthropic_messages, anthropic_tools, extract_anthropic_reply, extract_anthropic_usage, AnthropicStreamAssembler};
use crate::{
    ChatMsg, ChatReply, EditRequest, Model, ModelCatalog, ModelCatalogEntry, EffortLevel, ProposedEdit, StopReason,
    StreamEnd, ToolDecl, TurnUsage,
};
use serde_json::{json, Value};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

const API_URL: &str = "https://api.anthropic.com/v1/messages";
const MODELS_URL: &str = "https://api.anthropic.com/v1/models";
const API_VERSION: &str = "2023-06-01";
const ENV_API_KEY: &str = "ANTHROPIC_API_KEY";
const ENV_MODEL: &str = "BAKERY_CLAUDE_MODEL";
const DEFAULT_MODEL: &str = "claude-sonnet-4-6";
const MAX_TOKENS: u32 = 8192;

/// Console API-key Claude provider (ToS-clean for programmatic use).
pub struct ClaudeModel {
    model_id: String,
    /// Per-turn reasoning-effort override (07d-A3D1 Phase C) — set via
    /// `Model::set_effort`, read when building each request.
    effort: Mutex<Option<String>>,
    /// This model's own report of the last completed turn's token usage
    /// (07d-A3D1 Phase C).
    last_usage: Mutex<Option<TurnUsage>>,
    last_thinking: Mutex<Option<crate::Thinking>>,
}

impl ClaudeModel {
    pub fn from_env() -> Self {
        let s = secrets::load();
        let model_id = s
            .claude_model_id
            .or_else(|| std::env::var(ENV_MODEL).ok())
            .unwrap_or_else(|| DEFAULT_MODEL.to_string());
        Self::with_model_id(model_id)
    }

    /// A fresh instance for a specific model id — used both by
    /// [`Self::from_env`] and by [`Model::with_overridden_model_id`] to
    /// build a one-call, uncached variant of an already-resolved seat's
    /// model (07d-A3D1 Phase C).
    pub fn with_model_id(model_id: impl Into<String>) -> Self {
        Self { model_id: model_id.into(), effort: Mutex::new(None), last_usage: Mutex::new(None), last_thinking: Mutex::new(None) }
    }

    /// The `output_config` body field for a currently-set per-turn effort
    /// override, factored out as a pure function (07d-A3D1 Phase C review
    /// fix) so the exact composed JSON is directly unit-testable without a
    /// real network call — `API_URL` is hardcoded, so `call_turn`/
    /// `call_turn_streaming` themselves aren't reachable in a test.
    /// Mirrors `GeminiModel::thinking_config()`'s equivalent shape.
    fn effort_output_config(&self) -> Option<Value> {
        let effort = self.effort.lock().unwrap().clone()?;
        Some(json!({ "effort": effort }))
    }

    pub fn credentials_available() -> bool {
        crate::proxy::route("claude").is_some()
            || secrets::load()
                .anthropic_api_key
                .as_ref()
                .is_some_and(|k| !k.trim().is_empty())
            || std::env::var(ENV_API_KEY)
                .map(|k| !k.trim().is_empty())
                .unwrap_or(false)
    }

    /// Where a call to `direct` goes, and the key it sends: the model proxy
    /// with the node's token when it holds a Claude key, else Anthropic with
    /// the console key.
    fn target(&self, direct: &str) -> Result<(String, String), String> {
        match crate::proxy::via("claude", direct) {
            Some(target) => Ok(target),
            None => Ok((direct.to_string(), self.api_key()?)),
        }
    }

    fn api_key(&self) -> Result<String, String> {
        if let Some(k) = secrets::load().anthropic_api_key {
            if !k.trim().is_empty() {
                return Ok(k.trim().to_string());
            }
        }
        std::env::var(ENV_API_KEY)
            .ok()
            .map(|k| k.trim().to_string())
            .filter(|k| !k.is_empty())
            .ok_or_else(|| format!("{ENV_API_KEY} not set (and no key in client Setup)"))
    }

    fn call_turn(&self, system: &str, messages: &[ChatMsg], tools: &[ToolDecl]) -> Result<ChatReply, String> {
        let (url, key) = self.target(API_URL)?;
        let mut body = json!({
            "model": crate::proxy::model_for("claude", &self.model_id),
            "max_tokens": MAX_TOKENS,
            "stream": false,
            "system": system,
            "messages": anthropic_messages(messages)
        });
        if !tools.is_empty() {
            body["tools"] = anthropic_tools(tools);
        }
        if let Some(output_config) = self.effort_output_config() {
            body["output_config"] = output_config;
        }

        let resp = ureq::post(&url)
            .set("x-api-key", &key)
            .set("anthropic-version", API_VERSION)
            .set("content-type", "application/json")
            .set("accept", "application/json")
            .timeout(std::time::Duration::from_secs(120))
            .send_json(body)
            .map_err(|e| format!("anthropic request: {e}"))?;

        let status = resp.status();
        let text = resp.into_string().map_err(|e| format!("anthropic body: {e}"))?;
        if !(200..300).contains(&status) {
            let snippet: String = text.chars().take(800).collect();
            return Err(format!("Anthropic API {status}: {snippet}"));
        }

        *self.last_usage.lock().unwrap() = Some(extract_anthropic_usage(&text));

        *self.last_thinking.lock().unwrap() = None;
        extract_anthropic_reply(&text)
    }

    fn call_messages(&self, system: &str, messages: &[ChatMsg]) -> Result<String, String> {
        match self.call_turn(system, messages, &[])? {
            ChatReply::Text(s) => Ok(s),
            ChatReply::ToolCalls(_) => Err("Anthropic returned tool_use on a text-only request".into()),
        }
    }

    /// `"stream": true` on the same endpoint `call_turn` posts to — event
    /// flow per Anthropic's current streaming docs: `message_start` → per
    /// content block `content_block_start` → N `content_block_delta` →
    /// `content_block_stop` → `message_delta`* → `message_stop`
    /// (`AnthropicStreamAssembler` owns all of that; this method only owns
    /// the HTTP/SSE plumbing and the cancel/stream-death outcome mapping).
    fn call_turn_streaming(
        &self,
        system: &str,
        messages: &[ChatMsg],
        tools: &[ToolDecl],
        on_text_delta: &mut dyn FnMut(&str),
        cancel: &AtomicBool,
    ) -> Result<StreamEnd, String> {
        let (url, key) = self.target(API_URL)?;
        let mut body = json!({
            "model": crate::proxy::model_for("claude", &self.model_id),
            "max_tokens": MAX_TOKENS,
            "stream": true,
            "system": system,
            "messages": anthropic_messages(messages)
        });
        if !tools.is_empty() {
            body["tools"] = anthropic_tools(tools);
        }
        if let Some(output_config) = self.effort_output_config() {
            body["output_config"] = output_config;
        }

        let result = sse::streaming_agent()
            .post(&url)
            .set("x-api-key", &key)
            .set("anthropic-version", API_VERSION)
            .set("content-type", "application/json")
            .set("accept", "text/event-stream")
            .send_json(body);

        let resp = match result {
            Ok(r) => r,
            Err(ureq::Error::Status(status, resp)) => {
                let text = resp.into_string().unwrap_or_default();
                let snippet: String = text.chars().take(800).collect();
                return Err(format!("Anthropic API {status}: {snippet}"));
            }
            Err(e) => return Err(format!("anthropic stream request: {e}")),
        };

        let mut reader = sse::SseReader::new(resp.into_reader());
        let mut assembler = AnthropicStreamAssembler::new();
        let outcome = sse::drive(&mut reader, cancel, |ev| assembler.feed(ev, on_text_delta));
        *self.last_usage.lock().unwrap() = Some(assembler.usage());
        *self.last_thinking.lock().unwrap() = assembler.thinking();

        match outcome {
            DriveOutcome::FinishedNormally => Ok(StreamEnd::Completed(assembler.finish()?)),
            DriveOutcome::Cancelled => Ok(StreamEnd::Stopped {
                partial_text: assembler.partial_text_so_far(),
                reason: StopReason::Cancelled,
            }),
            DriveOutcome::Eof => {
                let partial = assembler.partial_text_so_far();
                if partial.is_empty() {
                    Err("Anthropic stream ended before any content was received".to_string())
                } else {
                    Ok(StreamEnd::Stopped {
                        partial_text: partial,
                        reason: StopReason::StreamDied("connection closed before message_stop".to_string()),
                    })
                }
            }
            DriveOutcome::IoError(e) => {
                let partial = assembler.partial_text_so_far();
                if partial.is_empty() {
                    Err(format!("Anthropic stream error: {e}"))
                } else {
                    Ok(StreamEnd::Stopped { partial_text: partial, reason: StopReason::StreamDied(e) })
                }
            }
        }
    }
}

impl Model for ClaudeModel {
    fn name(&self) -> &str {
        "claude"
    }

    /// Messages API image blocks (JPEG, PNG, GIF, WebP) and PDF documents.
    fn native_attachment_types(&self) -> &'static [&'static str] {
        &["image/png", "image/jpeg", "image/gif", "image/webp", "application/pdf"]
    }

    fn propose_edit(&self, req: &EditRequest) -> Result<ProposedEdit, String> {
        if req.file_path.trim().is_empty() {
            return Err("file_path required".to_string());
        }

        let user_text = user_edit_prompt(&req.instruction, &req.file_path, &req.current_content);
        let mut raw = self.call_messages(SYSTEM_EDIT_INSTRUCTIONS, &[ChatMsg::user(user_text)])?;
        raw = strip_outer_fence(&raw);
        if raw.is_empty() {
            return Err("model returned empty content".into());
        }

        Ok(ProposedEdit {
            file_path: req.file_path.clone(),
            new_content: raw,
            rationale: format!("claude:{}", self.model_id),
        })
    }

    fn supports_chat(&self) -> bool {
        true
    }

    fn chat(&self, system: &str, messages: &[ChatMsg]) -> Result<String, String> {
        self.call_messages(system, messages)
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
        let Ok((url, key)) = self.target(MODELS_URL) else {
            return Ok(ModelCatalog::Unavailable {
                default: ModelCatalogEntry {
                    id: self.model_id.clone(),
                    label: self.model_id.clone(),
                    context_window: None,
                    effort_levels: Vec::new(),
                },
                reason: "no Anthropic API key configured".to_string(),
            });
        };
        let result = ureq::get(&format!("{url}?limit=1000"))
            .set("x-api-key", &key)
            .set("anthropic-version", API_VERSION)
            .set("accept", "application/json")
            .timeout(std::time::Duration::from_secs(20))
            .call();
        let text = read_models_response(result, "Anthropic")?;
        parse_catalog(&text)
    }
}

/// Parses `GET /v1/models`'s `{"data": [...]}` into real, live catalog
/// entries — `capabilities.effort.{low,medium,high,xhigh,max}` (whichever
/// keys are actually present) becomes this model's effort levels, only when
/// `capabilities.effort.supported` is true. Never a hardcoded model list —
/// every entry here came from this exact response.
fn parse_catalog(text: &str) -> Result<ModelCatalog, String> {
    let v: serde_json::Value = serde_json::from_str(text).map_err(|e| format!("anthropic models parse: {e}"))?;
    let data = v
        .get("data")
        .and_then(|d| d.as_array())
        .ok_or("anthropic models: response had no \"data\" array")?;
    let mut models = Vec::new();
    for m in data {
        let Some(id) = m.get("id").and_then(|x| x.as_str()) else { continue };
        let label = m.get("display_name").and_then(|x| x.as_str()).unwrap_or(id).to_string();
        let context_window = m.get("max_input_tokens").and_then(|x| x.as_u64());
        let mut effort_levels = Vec::new();
        if let Some(effort) = m.get("capabilities").and_then(|c| c.get("effort")) {
            if effort.get("supported").and_then(|s| s.as_bool()) == Some(true) {
                for level in ["low", "medium", "high", "xhigh", "max"] {
                    if effort.get(level).is_some() {
                        effort_levels.push(EffortLevel { id: level.to_string(), label: effort_label(level) });
                    }
                }
            }
        }
        models.push(ModelCatalogEntry { id: id.to_string(), label, context_window, effort_levels });
    }
    if models.is_empty() {
        return Err("anthropic models: catalog came back empty".to_string());
    }
    Ok(ModelCatalog::Available { models })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_wire::extract_anthropic_reply;

    #[test]
    fn extract_text_blocks() {
        let resp = r#"{"content":[{"type":"thinking","thinking":"hmm"},{"type":"text","text":"hello\nworld"}]}"#;
        let t = extract_anthropic_reply(resp).unwrap();
        assert_eq!(t, ChatReply::Text("hello\nworld".into()));
    }

    #[test]
    fn api_key_required_message() {
        // Clear env for this process if present would race; just unit-test extractor.
        let j = r#"{"content":[{"type":"text","text":"ok"}]}"#;
        assert_eq!(extract_anthropic_reply(j).unwrap(), ChatReply::Text("ok".into()));
    }

    #[test]
    fn parse_catalog_reads_context_window_and_present_effort_levels() {
        let body = r#"{
            "data": [
                {
                    "id": "claude-opus-5",
                    "display_name": "Claude Opus 5",
                    "max_input_tokens": 1000000,
                    "max_tokens": 128000,
                    "capabilities": {
                        "effort": {"supported": true, "low": {}, "medium": {}, "high": {}, "xhigh": {}, "max": {}}
                    }
                },
                {
                    "id": "claude-haiku-4-5",
                    "display_name": "Claude Haiku 4.5",
                    "max_input_tokens": 200000,
                    "capabilities": {"effort": {"supported": false}}
                }
            ],
            "has_more": false
        }"#;
        let catalog = parse_catalog(body).unwrap();
        let ModelCatalog::Available { models } = catalog else { panic!("expected Available") };
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "claude-opus-5");
        assert_eq!(models[0].label, "Claude Opus 5");
        assert_eq!(models[0].context_window, Some(1_000_000));
        assert_eq!(
            models[0].effort_levels.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            vec!["low", "medium", "high", "xhigh", "max"]
        );
        assert_eq!(models[0].effort_levels[3].label, "Extra high");
        assert_eq!(models[1].id, "claude-haiku-4-5");
        assert_eq!(models[1].context_window, Some(200_000));
        assert!(models[1].effort_levels.is_empty(), "supported:false must yield no effort levels");
    }

    #[test]
    fn parse_catalog_errors_on_an_empty_data_array() {
        assert!(parse_catalog(r#"{"data": []}"#).is_err());
    }

    #[test]
    fn parse_catalog_errors_when_data_is_missing() {
        assert!(parse_catalog(r#"{"has_more": false}"#).is_err());
    }

    /// `API_URL` is a hardcoded constant pointing at the real Anthropic
    /// endpoint (unlike `openai_compat.rs`'s runtime `base_url`), so a real
    /// fake-local-server round trip isn't reachable here without a new
    /// test-only URL override this file has never had (the non-streaming
    /// `call_turn`/`catalog` paths were never given one either — this
    /// mirrors that same existing precedent exactly). What's provable
    /// without network: the no-credentials short-circuit never attempts a
    /// connection at all. The actual HTTP+SSE+assembler pipeline this
    /// method shares with `grok.rs`/`openai_compat.rs` (via
    /// `sse::drive`/`AnthropicStreamAssembler`) is proven generically in
    /// `sse.rs`/`tool_wire.rs`'s own tests, plus a full real-network round
    /// trip through the shared `OpenAiStreamAssembler` in
    /// `openai_compat.rs` (whose `base_url` genuinely is a runtime
    /// parameter).
    #[test]
    fn chat_turn_streaming_fails_closed_with_no_credentials_and_touches_no_network() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        crate::secrets::test_support::with_isolated_credentials(&path, || {
            let model = ClaudeModel::with_model_id("claude-test");
            let cancel = std::sync::atomic::AtomicBool::new(false);
            let err = model
                .chat_turn_streaming("system", &[], &[], &mut |_| {}, &cancel)
                .map(|_| ())
                .unwrap_err();
            assert!(err.contains(ENV_API_KEY), "{err}");
        });
    }

    /// `API_URL` is hardcoded (see the doc comment above), so the actual
    /// request this feeds into isn't reachable here without network — this
    /// only proves the interior side channel itself round-trips and
    /// correctly clears, matching every other provider's identical
    /// no-network-reachable limitation for this constant.
    #[test]
    fn set_effort_round_trips_through_the_interior_side_channel_and_clears_on_none() {
        let model = ClaudeModel::with_model_id("claude-test");
        assert_eq!(*model.effort.lock().unwrap(), None);
        model.set_effort(Some("high"));
        assert_eq!(*model.effort.lock().unwrap(), Some("high".to_string()));
        model.set_effort(None);
        assert_eq!(*model.effort.lock().unwrap(), None, "a later call with None must clear a prior value");
    }

    /// Closes the gap the round-trip test above admits it leaves (review
    /// fix): this directly asserts the exact JSON `call_turn`/
    /// `call_turn_streaming` merge into the outgoing request body, without
    /// needing network — mirrors `GeminiModel::thinking_config`'s own
    /// directly-tested pure-function pattern.
    #[test]
    fn effort_output_config_reflects_the_currently_set_effort() {
        let model = ClaudeModel::with_model_id("claude-test");
        assert_eq!(model.effort_output_config(), None, "no effort set yet");
        model.set_effort(Some("high"));
        assert_eq!(model.effort_output_config(), Some(json!({ "effort": "high" })));
        model.set_effort(None);
        assert_eq!(model.effort_output_config(), None, "a later call with None must clear it");
    }

    #[test]
    fn with_overridden_model_id_builds_a_distinct_instance_with_its_own_fresh_state() {
        let model = ClaudeModel::with_model_id("claude-original");
        model.set_effort(Some("high"));
        let overridden = model.with_overridden_model_id("claude-variant").expect("Claude supports per-turn overrides");
        assert_eq!(overridden.name(), "claude", "Model::name() is the provider bucket, not the specific variant");
        assert!(overridden.last_turn_usage().is_none(), "a fresh override instance starts with no usage");
    }
}
