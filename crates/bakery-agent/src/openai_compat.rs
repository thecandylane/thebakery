//! OpenAiCompatModel — an operator-configured local/self-hosted endpoint
//! that speaks OpenAI-shaped chat completions (Ollama, llama.cpp, vLLM, HF
//! TGI, an optional gateway sidecar). One more `Model` on the same closed
//! HTTP-shaped seam `claude.rs`/`grok.rs`/`gemini.rs` use — no new transport,
//! no vendored client, just a configurable base URL + model id + optional
//! bearer instead of a hardcoded provider endpoint.

use crate::secrets;
use crate::shared::{strip_outer_fence, user_edit_prompt, SYSTEM_EDIT_INSTRUCTIONS};
use crate::sse::{self, DriveOutcome};
use crate::tool_wire::{
    extract_openai_reply, extract_openai_usage, openai_messages, openai_tools, without_attachments,
    OpenAiStreamAssembler,
};
use crate::{
    ChatMsg, ChatReply, EditRequest, Model, ModelCatalog, ModelCatalogEntry, ProposedEdit, StopReason, StreamEnd,
    ToolDecl, TurnUsage,
};
use serde_json::json;
use std::sync::atomic::AtomicBool;
use std::sync::Mutex;

const MAX_TOKENS: u32 = 8192;
/// Cap on a single response body read from an openai-compat endpoint
/// (07d-A3B review #8: "A faulty local endpoint can consume unbounded
/// memory"). `ureq`'s `Response::into_string()` has no size limit of its
/// own; this reads through a bounded adapter instead and fails closed
/// rather than buffering an unbounded body first. Sized for a long code
/// completion (legitimate content) while still refusing a runaway or
/// hostile endpoint that never stops streaming.
const MAX_RESPONSE_BYTES: u64 = 4 * 1024 * 1024;

/// `base_url` must be an absolute `http://` or `https://` URL with no
/// further validation of reachability — this is a syntax check only, so a
/// caller can report "Setup needed" before ever attempting a network call
/// (the fake-listener/bad-URL proof point in 07d-A3B).
pub fn validate_base_url(base_url: &str) -> Result<(), String> {
    let trimmed = base_url.trim();
    if trimmed.is_empty() {
        return Err("Setup needed: base_url must not be empty".to_string());
    }
    if !(trimmed.starts_with("http://") || trimmed.starts_with("https://")) {
        return Err("Setup needed: base_url must start with http:// or https://".to_string());
    }
    // Reject anything that is transparently not a host (bare scheme, or a
    // scheme immediately followed by another slash/space) without pulling in
    // a URL-parsing dependency for one syntax check.
    let after_scheme = trimmed.splitn(2, "://").nth(1).unwrap_or("");
    if after_scheme.trim().is_empty() || after_scheme.starts_with('/') {
        return Err("Setup needed: base_url must include a host".to_string());
    }
    Ok(())
}

pub struct OpenAiCompatModel {
    id: String,
    base_url: String,
    model: String,
    bearer: Option<String>,
    /// Per-turn reasoning-effort override (07d-A3D1 Phase C) — forwarded
    /// speculatively as `reasoning_effort`; no effort-level concept is
    /// standardized across every openai-compat backend, so an unrecognized
    /// field is expected to be harmlessly ignored rather than rejected,
    /// matching this file's existing "an operator's picker should still
    /// work even if the endpoint can't do X" posture.
    effort: Mutex<Option<String>>,
    /// This model's own report of the last completed turn's token usage
    /// (07d-A3D1 Phase C).
    last_usage: Mutex<Option<TurnUsage>>,
    last_thinking: Mutex<Option<crate::Thinking>>,
}

impl OpenAiCompatModel {
    /// `id` is the operator-chosen plug id (`node.json`'s
    /// `openai_compat_plugs[].id`) — used as `Model::name()` so Heart events
    /// record exactly which configured plug answered, the same way a named
    /// ACP plug's id becomes its `AcpPipeModel::name()`.
    pub fn new(id: String, base_url: String, model: String, bearer: Option<String>) -> Result<Self, String> {
        validate_base_url(&base_url)?;
        if model.trim().is_empty() {
            return Err("Setup needed: openai-compat plug is missing a model id".to_string());
        }
        Ok(Self {
            id,
            base_url: base_url.trim().trim_end_matches('/').to_string(),
            model,
            bearer,
            effort: Mutex::new(None),
            last_usage: Mutex::new(None), last_thinking: Mutex::new(None),
        })
    }

    /// Read the plug's own configured bearer from the secrets store — the
    /// caller (bakery-node's seat resolver) may also pass one explicitly at
    /// construction (tests, or a future per-turn override); this helper is
    /// for the common "load from `node.json` + `secrets.json`" path.
    pub fn bearer_from_secrets(plug_id: &str) -> Option<String> {
        secrets::plug_secret(plug_id)
    }

    fn call_turn(&self, system: &str, messages: &[ChatMsg], tools: &[ToolDecl]) -> Result<ChatReply, String> {
        // (The text-only retry wraps this and `call_turn_streaming` in the
        // `Model` impl below.)
        let url = format!("{}/chat/completions", self.base_url);
        let mut body = json!({
            "model": self.model,
            "max_tokens": MAX_TOKENS,
            "stream": false,
            "messages": openai_messages(system, messages)
        });
        if !tools.is_empty() {
            body["tools"] = openai_tools(tools);
            body["tool_choice"] = json!("auto");
        }
        if let Some(effort) = self.effort.lock().unwrap().clone() {
            body["reasoning_effort"] = json!(effort);
        }

        let mut req = ureq::post(&url)
            .set("content-type", "application/json")
            .set("accept", "application/json")
            .timeout(std::time::Duration::from_secs(120));
        if let Some(token) = &self.bearer {
            req = req.set("Authorization", &format!("Bearer {token}"));
        }
        let resp = req.send_json(body).map_err(|e| format!("openai-compat request: {e}"))?;

        let status = resp.status();
        let text = read_bounded_body(resp)?;
        if !(200..300).contains(&status) {
            let snippet: String = text.chars().take(800).collect();
            return Err(format!("openai-compat API {status}: {snippet}"));
        }

        *self.last_usage.lock().unwrap() = Some(extract_openai_usage(&text));

        *self.last_thinking.lock().unwrap() = None;
        extract_openai_reply(&text)
    }

    fn call_chat(&self, system: &str, messages: &[ChatMsg]) -> Result<String, String> {
        match self.call_turn(system, messages, &[])? {
            ChatReply::Text(s) => Ok(s),
            ChatReply::ToolCalls(_) => Err("openai-compat returned tool calls on a text-only request".into()),
        }
    }

    /// `"stream": true` on the same endpoint `call_turn` posts to —
    /// standard OpenAI-shaped SSE (`data: {...}` chunks terminated by the
    /// literal `data: [DONE]`), reusing the same `OpenAiStreamAssembler`
    /// `grok.rs` does (identical wire shape). A self-hosted backend that
    /// silently ignores `"stream": true` and answers with a complete JSON
    /// body instead degrades gracefully via the response's `Content-Type`
    /// — never a hard error, matching this file's existing "an operator's
    /// picker should still work even if the endpoint can't do X" posture.
    fn call_turn_streaming(
        &self,
        system: &str,
        messages: &[ChatMsg],
        tools: &[ToolDecl],
        on_text_delta: &mut dyn FnMut(&str),
        cancel: &AtomicBool,
    ) -> Result<StreamEnd, String> {
        let url = format!("{}/chat/completions", self.base_url);
        let mut body = json!({
            "model": self.model,
            "max_tokens": MAX_TOKENS,
            "stream": true,
            "messages": openai_messages(system, messages)
        });
        if !tools.is_empty() {
            body["tools"] = openai_tools(tools);
            body["tool_choice"] = json!("auto");
        }
        if let Some(effort) = self.effort.lock().unwrap().clone() {
            body["reasoning_effort"] = json!(effort);
        }
        // Strict-OpenAI-shaped backends only include a usage-bearing final
        // chunk when explicitly asked (07d-A3D1 Phase C) — xAI (grok.rs)
        // already includes it unconditionally, so this is added here only,
        // not there. Harmless for a backend that doesn't recognize the
        // field at all.
        body["stream_options"] = json!({ "include_usage": true });

        let mut req = sse::streaming_agent()
            .post(&url)
            .set("content-type", "application/json")
            .set("accept", "text/event-stream");
        if let Some(token) = &self.bearer {
            req = req.set("Authorization", &format!("Bearer {token}"));
        }

        let resp = match req.send_json(body) {
            Ok(r) => r,
            Err(ureq::Error::Status(status, resp)) => {
                let body = read_bounded_body(resp).unwrap_or_default();
                let snippet: String = body.chars().take(800).collect();
                return Err(format!("openai-compat API {status}: {snippet}"));
            }
            Err(e) => return Err(format!("openai-compat stream request: {e}")),
        };

        let content_type = resp.header("content-type").unwrap_or_default().to_ascii_lowercase();
        if !content_type.contains("text/event-stream") {
            // The backend answered with a plain (non-chunked) body despite
            // "stream": true — parse it exactly like the non-streaming
            // path and hand the whole text to on_text_delta once.
            let text = read_bounded_body(resp)?;
            *self.last_usage.lock().unwrap() = Some(extract_openai_usage(&text));
            *self.last_thinking.lock().unwrap() = None;
            let reply = extract_openai_reply(&text)?;
            if let ChatReply::Text(ref s) = reply {
                if !s.is_empty() {
                    on_text_delta(s);
                }
            }
            return Ok(StreamEnd::Completed(reply));
        }

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
                    Err("openai-compat stream ended before any content was received".to_string())
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
                    Err(format!("openai-compat stream error: {e}"))
                } else {
                    Ok(StreamEnd::Stopped { partial_text: partial, reason: StopReason::StreamDied(e) })
                }
            }
        }
    }

    /// Best-effort `GET {base_url}/models` — a self-hosted/local endpoint may
    /// not implement this at all (some don't even speak the OpenAI-shaped
    /// list endpoint), so any failure degrades to `Unavailable` with this
    /// plug's single configured model rather than propagating an error: an
    /// operator's picker should still work even if the endpoint has no
    /// catalog to offer. No effort-level concept exists for a generic
    /// self-hosted endpoint — never guessed.
    fn fetch_catalog(&self) -> Result<ModelCatalog, String> {
        let url = format!("{}/models", self.base_url);
        let mut req = ureq::get(&url)
            .set("accept", "application/json")
            .timeout(std::time::Duration::from_secs(20));
        if let Some(token) = &self.bearer {
            req = req.set("Authorization", &format!("Bearer {token}"));
        }
        // ureq already returns `Err(Error::Status(status, response))` for
        // any status >= 400 rather than `Ok` (07d-A3D1 review: a plain
        // `.map_err(Display)?` right after `.call()` would lose the real
        // response body — Anthropic/xAI/Gemini's shared fix is
        // `shared::read_models_response`, but this endpoint's body still
        // needs the same bounded read every other path here uses, so it's
        // handled inline instead of through that helper).
        let text = match req.call() {
            Ok(resp) => read_bounded_body(resp)?,
            Err(ureq::Error::Status(status, resp)) => {
                let body = read_bounded_body(resp).unwrap_or_default();
                let snippet: String = body.chars().take(400).collect();
                return Err(format!("openai-compat models API {status}: {snippet}"));
            }
            Err(e) => return Err(format!("openai-compat models request: {e}")),
        };
        parse_catalog(&text)
    }
}

/// Read a response body up to [`MAX_RESPONSE_BYTES`] and fail closed if the
/// server keeps sending past that — never buffer an unbounded body first
/// and only reject it after the fact.
/// The endpoint refused a request that carried files as input it cannot take
/// (400/413/415/422 — e.g. a text-only local model given an image): worth one
/// retry without them, so the turn still runs instead of failing on what the
/// model cannot see. Auth and rate-limit refusals are not about the files.
fn rejected_files(error: &str, messages: &[ChatMsg]) -> bool {
    ["400", "413", "415", "422"]
        .iter()
        .any(|status| error.starts_with(&format!("openai-compat API {status}")))
        && messages.iter().any(|m| !m.attachments.is_empty())
}

fn read_bounded_body(resp: ureq::Response) -> Result<String, String> {
    use std::io::Read;
    let mut limited = resp.into_reader().take(MAX_RESPONSE_BYTES + 1);
    let mut buf = Vec::new();
    limited.read_to_end(&mut buf).map_err(|e| format!("openai-compat body: {e}"))?;
    if buf.len() as u64 > MAX_RESPONSE_BYTES {
        return Err(format!(
            "openai-compat response exceeded the {}-byte cap",
            MAX_RESPONSE_BYTES
        ));
    }
    String::from_utf8(buf).map_err(|e| format!("openai-compat body: not valid UTF-8: {e}"))
}

impl Model for OpenAiCompatModel {
    fn name(&self) -> &str {
        &self.id
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
            rationale: format!("openai-compat:{}", self.id),
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
        match self.call_turn(system, messages, tools) {
            Err(e) if rejected_files(&e, messages) => {
                self.call_turn(system, &without_attachments(messages), tools)
            }
            other => other,
        }
    }

    fn chat_turn_streaming(
        &self,
        system: &str,
        messages: &[ChatMsg],
        tools: &[ToolDecl],
        on_text_delta: &mut dyn FnMut(&str),
        cancel: &AtomicBool,
    ) -> Result<StreamEnd, String> {
        match self.call_turn_streaming(system, messages, tools, on_text_delta, cancel) {
            Err(e) if rejected_files(&e, messages) => self.call_turn_streaming(
                system,
                &without_attachments(messages),
                tools,
                on_text_delta,
                cancel,
            ),
            other => other,
        }
    }

    /// Images go as `image_url` parts. Whether the model behind this
    /// endpoint can see is not knowable up front; one that cannot is retried
    /// without them (see `rejected_files`).
    fn native_attachment_types(&self) -> &'static [&'static str] {
        &["image/png", "image/jpeg", "image/gif", "image/webp"]
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    fn set_effort(&self, effort: Option<&str>) {
        *self.effort.lock().unwrap() = effort.map(str::to_string);
    }

    fn last_turn_usage(&self) -> Option<TurnUsage> {
        *self.last_usage.lock().unwrap()
    }


    fn last_turn_thinking(&self) -> Option<crate::Thinking> {

        self.last_thinking.lock().unwrap().clone()

    }

    fn catalog(&self) -> Result<ModelCatalog, String> {
        self.fetch_catalog().or_else(|reason| {
            Ok(ModelCatalog::Unavailable {
                default: ModelCatalogEntry {
                    id: self.model.clone(),
                    label: self.model.clone(),
                    context_window: None,
                    effort_levels: Vec::new(),
                },
                reason,
            })
        })
    }
}

/// Parses the standard OpenAI-compatible `{"data": [...]}` list shape.
/// `max_model_len` (a field some self-hosted backends, e.g. vLLM, add) is
/// read as the context window when present; nothing here is standardized
/// across every openai-compat backend, so anything absent is just left out
/// rather than guessed.
fn parse_catalog(text: &str) -> Result<ModelCatalog, String> {
    let v: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("openai-compat models parse: {e}"))?;
    let data = v
        .get("data")
        .and_then(|d| d.as_array())
        .ok_or("openai-compat models: response had no \"data\" array")?;
    let mut models = Vec::new();
    for m in data {
        let Some(id) = m.get("id").and_then(|x| x.as_str()) else { continue };
        let context_window = m.get("max_model_len").and_then(|x| x.as_u64());
        models.push(ModelCatalogEntry {
            id: id.to_string(),
            label: id.to_string(),
            context_window,
            effort_levels: Vec::new(),
        });
    }
    if models.is_empty() {
        return Err("openai-compat models: catalog came back empty".to_string());
    }
    Ok(ModelCatalog::Available { models })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_openai_shaped_content() {
        let resp = r#"{"choices":[{"message":{"role":"assistant","content":"line1\nline2"}}]}"#;
        assert_eq!(
            extract_openai_reply(resp).unwrap(),
            ChatReply::Text("line1\nline2".into())
        );
    }

    #[test]
    fn validate_base_url_accepts_http_and_https_with_a_host() {
        assert!(validate_base_url("http://localhost:11434/v1").is_ok());
        assert!(validate_base_url("https://example.internal:8080").is_ok());
    }

    #[test]
    fn validate_base_url_rejects_empty_missing_scheme_and_missing_host() {
        assert!(validate_base_url("").is_err());
        assert!(validate_base_url("localhost:11434").is_err());
        assert!(validate_base_url("ftp://localhost").is_err());
        assert!(validate_base_url("http://").is_err());
        assert!(validate_base_url("http:///no-host").is_err());
    }

    #[test]
    fn new_fails_setup_needed_on_a_bad_url_without_any_network_call() {
        let err = OpenAiCompatModel::new("local:x".into(), "not-a-url".into(), "m".into(), None)
            .map(|_| ())
            .unwrap_err();
        assert!(err.contains("Setup needed"), "{err}");
    }

    #[test]
    fn new_fails_setup_needed_on_an_empty_model_id() {
        let err = OpenAiCompatModel::new("local:x".into(), "http://localhost:1".into(), "  ".into(), None)
            .map(|_| ())
            .unwrap_err();
        assert!(err.contains("Setup needed"), "{err}");
    }

    /// One-request fake listener built on `tiny_http` — a small, purpose-
    /// built HTTP/1.1 test server (07d-A3B: "Fake HTTP server in tests, not
    /// live Hugging Face"), rather than a hand-rolled raw-socket responder:
    /// an early hand-rolled version of this fixture was measurably flaky
    /// under heavy parallel `cargo test` load (intermittent client-side
    /// transport errors reading the response), which `tiny_http`'s real
    /// HTTP/1.1 framing does not reproduce.
    /// The test binary's first loopback connection can wait on a network
    /// filter (LuLu holds a fresh binary's first one): it is made here, with
    /// a long deadline, so a model call's own short connect timeout never
    /// meets the wait. A bare connection sends no request, so the server's
    /// one `recv` still gets the real one.
    fn first_contact(addr: std::net::SocketAddr) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
        while let Err(e) = std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_secs(120)) {
            assert!(std::time::Instant::now() < deadline, "the test server never answered: {e}");
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    }

    fn spawn_fake_openai_server(body: &'static str) -> String {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let addr = server.server_addr().to_ip().unwrap();
        first_contact(addr);
        std::thread::spawn(move || {
            if let Ok(request) = server.recv() {
                let response = tiny_http::Response::from_string(body).with_header(
                    tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap(),
                );
                let _ = request.respond(response);
            }
        });
        format!("http://{addr}")
    }

    #[test]
    fn chat_round_trips_against_a_fake_openai_compat_listener() {
        let body = r#"{"choices":[{"message":{"role":"assistant","content":"hello from the fake local model"}}]}"#;
        let base_url = spawn_fake_openai_server(body);
        let model = OpenAiCompatModel::new("local:test".into(), base_url, "test-model".into(), None).unwrap();
        let out = model.chat("system", &[ChatMsg::user("hi")]).unwrap();
        assert_eq!(out, "hello from the fake local model");
        assert_eq!(model.name(), "local:test");
    }

    #[test]
    fn chat_sends_the_configured_bearer() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let addr = server.server_addr().to_ip().unwrap();
        first_contact(addr);
        let handle = std::thread::spawn(move || {
            let request = server.recv().unwrap();
            let header_line = request
                .headers()
                .iter()
                .find(|h| h.field.equiv("Authorization"))
                .map(|h| format!("{}: {}", h.field, h.value))
                .unwrap_or_default();
            let body = r#"{"choices":[{"message":{"role":"assistant","content":"ok"}}]}"#;
            let response = tiny_http::Response::from_string(body);
            let _ = request.respond(response);
            header_line
        });
        let base_url = format!("http://{addr}");
        let model =
            OpenAiCompatModel::new("local:auth".into(), base_url, "test-model".into(), Some("test-only-bearer".into()))
                .unwrap();
        model.chat("system", &[ChatMsg::user("hi")]).unwrap();
        let request = handle.join().unwrap();
        assert!(request.contains("Authorization: Bearer test-only-bearer"), "{request}");
    }

    /// 07d-A3B review #8: a faulty/hostile local endpoint sending a body
    /// past the cap must fail the call, not be buffered in full first.
    #[test]
    fn chat_fails_closed_on_a_response_past_the_byte_cap() {
        let oversized = "x".repeat(MAX_RESPONSE_BYTES as usize + 1024);
        let body: &'static str = Box::leak(oversized.into_boxed_str());
        let base_url = spawn_fake_openai_server(body);
        let model = OpenAiCompatModel::new("local:oversized".into(), base_url, "test-model".into(), None).unwrap();
        let err = model.chat("system", &[ChatMsg::user("hi")]).map(|_| ()).unwrap_err();
        assert!(err.contains("exceeded"), "{err}");
    }

    #[test]
    fn parse_catalog_reads_ids_and_the_optional_max_model_len() {
        let body = r#"{
            "object": "list",
            "data": [
                {"id": "llama-3.1-70b", "object": "model", "max_model_len": 131072},
                {"id": "mistral-small", "object": "model"}
            ]
        }"#;
        let catalog = parse_catalog(body).unwrap();
        let ModelCatalog::Available { models } = catalog else { panic!("expected Available") };
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "llama-3.1-70b");
        assert_eq!(models[0].context_window, Some(131_072));
        assert!(models[0].effort_levels.is_empty());
        assert_eq!(models[1].id, "mistral-small");
        assert_eq!(models[1].context_window, None);
    }

    #[test]
    fn parse_catalog_errors_on_an_empty_data_array() {
        assert!(parse_catalog(r#"{"object": "list", "data": []}"#).is_err());
    }

    #[test]
    fn catalog_round_trips_against_a_fake_models_listener() {
        let body = r#"{"object": "list", "data": [{"id": "local-model", "object": "model"}]}"#;
        let base_url = spawn_fake_openai_server(body);
        let model = OpenAiCompatModel::new("local:test".into(), base_url, "local-model".into(), None).unwrap();
        let catalog = model.catalog().unwrap();
        let ModelCatalog::Available { models } = catalog else { panic!("expected Available") };
        assert_eq!(models[0].id, "local-model");
    }

    /// A local endpoint that doesn't implement `/models` at all (connection
    /// refused, since nothing is listening) must degrade to `Unavailable`
    /// with this plug's own configured model — never propagate the error or
    /// break the picker.
    /// 07d-A3D1 review: `catalog()` degrades a failed fetch to `Unavailable`
    /// rather than erroring, but the `reason` inside it must still be the
    /// real provider-reported body, not just ureq's terse "status code N" —
    /// confirmed against a real (fake, local) HTTP 401 response.
    #[test]
    fn catalog_reason_surfaces_the_real_error_body_on_a_4xx_status() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let addr = server.server_addr().to_ip().unwrap();
        first_contact(addr);
        std::thread::spawn(move || {
            let request = server.recv().unwrap();
            let body = r#"{"error":"invalid API key"}"#;
            let response = tiny_http::Response::from_string(body).with_status_code(401);
            let _ = request.respond(response);
        });
        let model =
            OpenAiCompatModel::new("local:unauthorized".into(), format!("http://{addr}"), "m".into(), None).unwrap();
        match model.catalog().unwrap() {
            ModelCatalog::Unavailable { reason, .. } => {
                assert!(reason.contains("401"), "{reason}");
                assert!(reason.contains("invalid API key"), "the real provider error body must survive: {reason}");
            }
            ModelCatalog::Available { .. } => panic!("expected Unavailable"),
        }
    }

    #[test]
    fn catalog_falls_back_to_unavailable_when_the_endpoint_is_unreachable() {
        let model =
            OpenAiCompatModel::new("local:down".into(), "http://127.0.0.1:1".into(), "configured-model".into(), None)
                .unwrap();
        let catalog = model.catalog().unwrap();
        match catalog {
            ModelCatalog::Unavailable { default, reason } => {
                assert_eq!(default.id, "configured-model");
                assert!(!reason.is_empty());
            }
            ModelCatalog::Available { .. } => panic!("expected Unavailable"),
        }
    }

    /// `base_url` is a genuine runtime parameter here (unlike `claude.rs`/
    /// `grok.rs`/`gemini.rs`'s hardcoded provider endpoints), so this file
    /// is where the full HTTP+SSE+assembler pipeline all four streaming
    /// providers share gets proven end to end against a real fake local
    /// listener — its `OpenAiStreamAssembler` is the identical one
    /// `grok.rs` uses.
    fn spawn_fake_sse_server(body: &'static str) -> String {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let addr = server.server_addr().to_ip().unwrap();
        first_contact(addr);
        std::thread::spawn(move || {
            if let Ok(request) = server.recv() {
                let response = tiny_http::Response::from_string(body).with_header(
                    tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"text/event-stream"[..]).unwrap(),
                );
                let _ = request.respond(response);
            }
        });
        format!("http://{addr}")
    }

    #[test]
    fn chat_turn_streaming_round_trips_a_real_multi_chunk_sse_response() {
        let body = "data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\n\
                     data: {\"choices\":[{\"delta\":{\"content\":\"lo\"}}]}\n\n\
                     data: [DONE]\n\n";
        let base_url = spawn_fake_sse_server(body);
        let model = OpenAiCompatModel::new("local:stream".into(), base_url, "test-model".into(), None).unwrap();
        let mut deltas: Vec<String> = Vec::new();
        let cancel = AtomicBool::new(false);
        let outcome = model
            .chat_turn_streaming("system", &[ChatMsg::user("hi")], &[], &mut |t| deltas.push(t.to_string()), &cancel)
            .unwrap();
        assert_eq!(deltas, vec!["Hel", "lo"]);
        match outcome {
            StreamEnd::Completed(ChatReply::Text(t)) => assert_eq!(t, "Hello"),
            other => panic!("expected Completed(Text), got {other:?}"),
        }
    }

    #[test]
    fn chat_turn_streaming_reconstructs_a_fragmented_tool_call_over_real_http() {
        let body = "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"get_weather\",\"arguments\":\"\"}}]}}]}\n\n\
                     data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"c\\\":\"}}]}}]}\n\n\
                     data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"NYC\\\"}\"}}]}}]}\n\n\
                     data: [DONE]\n\n";
        let base_url = spawn_fake_sse_server(body);
        let model = OpenAiCompatModel::new("local:tools".into(), base_url, "test-model".into(), None).unwrap();
        let cancel = AtomicBool::new(false);
        let outcome = model.chat_turn_streaming("system", &[ChatMsg::user("hi")], &[], &mut |_| {}, &cancel).unwrap();
        match outcome {
            StreamEnd::Completed(ChatReply::ToolCalls(calls)) => {
                assert_eq!(calls[0].id, "call_1");
                assert_eq!(calls[0].name, "get_weather");
                assert_eq!(calls[0].arguments["c"], "NYC");
            }
            other => panic!("expected Completed(ToolCalls), got {other:?}"),
        }
    }

    #[test]
    fn chat_turn_streaming_stops_immediately_once_cancelled_mid_stream() {
        let body = "data: {\"choices\":[{\"delta\":{\"content\":\"one\"}}]}\n\n\
                     data: {\"choices\":[{\"delta\":{\"content\":\"two\"}}]}\n\n\
                     data: {\"choices\":[{\"delta\":{\"content\":\"three\"}}]}\n\n\
                     data: [DONE]\n\n";
        let base_url = spawn_fake_sse_server(body);
        let model = OpenAiCompatModel::new("local:cancel".into(), base_url, "test-model".into(), None).unwrap();
        let cancel = AtomicBool::new(false);
        let mut seen: Vec<String> = Vec::new();
        let outcome = model
            .chat_turn_streaming(
                "system",
                &[ChatMsg::user("hi")],
                &[],
                &mut |t| {
                    seen.push(t.to_string());
                    if seen.len() == 2 {
                        cancel.store(true, std::sync::atomic::Ordering::Release);
                    }
                },
                &cancel,
            )
            .unwrap();
        assert_eq!(seen, vec!["one", "two"], "the third chunk must never be dispatched once cancelled");
        match outcome {
            StreamEnd::Stopped { partial_text, reason: StopReason::Cancelled } => {
                assert_eq!(partial_text, "onetwo");
            }
            other => panic!("expected Stopped(Cancelled), got {other:?}"),
        }
    }

    #[test]
    fn chat_turn_streaming_reports_the_real_error_body_on_a_connect_stage_4xx() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let addr = server.server_addr().to_ip().unwrap();
        first_contact(addr);
        std::thread::spawn(move || {
            let request = server.recv().unwrap();
            let body = r#"{"error":"invalid API key"}"#;
            let response = tiny_http::Response::from_string(body).with_status_code(401);
            let _ = request.respond(response);
        });
        let model =
            OpenAiCompatModel::new("local:unauthorized".into(), format!("http://{addr}"), "m".into(), None).unwrap();
        let cancel = AtomicBool::new(false);
        let err = model
            .chat_turn_streaming("system", &[ChatMsg::user("hi")], &[], &mut |_| {}, &cancel)
            .map(|_| ())
            .unwrap_err();
        assert!(err.contains("401"), "{err}");
        assert!(err.contains("invalid API key"), "the real provider error body must survive: {err}");
    }

    /// A backend that silently ignores `"stream": true` (answers with a
    /// complete, non-chunked JSON body instead) must degrade gracefully to
    /// one `Completed` call — never a hard error.
    #[test]
    fn chat_turn_streaming_falls_back_to_completed_when_the_backend_ignores_stream_true() {
        let body = r#"{"choices":[{"message":{"role":"assistant","content":"whole reply at once"}}]}"#;
        let base_url = spawn_fake_openai_server(body);
        let model = OpenAiCompatModel::new("local:nostream".into(), base_url, "test-model".into(), None).unwrap();
        let mut deltas: Vec<String> = Vec::new();
        let cancel = AtomicBool::new(false);
        let outcome = model
            .chat_turn_streaming("system", &[ChatMsg::user("hi")], &[], &mut |t| deltas.push(t.to_string()), &cancel)
            .unwrap();
        assert_eq!(deltas, vec!["whole reply at once"]);
        match outcome {
            StreamEnd::Completed(ChatReply::Text(t)) => assert_eq!(t, "whole reply at once"),
            other => panic!("expected Completed(Text), got {other:?}"),
        }
    }

    #[test]
    fn chat_captures_prompt_and_completion_tokens_from_a_non_streaming_response() {
        let body = r#"{"choices":[{"message":{"role":"assistant","content":"ok"}}],"usage":{"prompt_tokens":10,"completion_tokens":4,"total_tokens":14}}"#;
        let base_url = spawn_fake_openai_server(body);
        let model = OpenAiCompatModel::new("local:usage".into(), base_url, "test-model".into(), None).unwrap();
        assert_eq!(model.last_turn_usage(), None, "no usage before any call");
        model.chat("system", &[ChatMsg::user("hi")]).unwrap();
        assert_eq!(model.last_turn_usage(), Some(TurnUsage { tokens_in: Some(10), tokens_out: Some(4) }));
    }

    #[test]
    fn chat_turn_streaming_captures_usage_from_a_final_chunk_before_done() {
        let body = "data: {\"choices\":[{\"delta\":{\"content\":\"Hi\"}}]}\n\n\
                     data: {\"choices\":[],\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":2,\"total_tokens\":9}}\n\n\
                     data: [DONE]\n\n";
        let base_url = spawn_fake_sse_server(body);
        let model = OpenAiCompatModel::new("local:usagestream".into(), base_url, "test-model".into(), None).unwrap();
        let cancel = AtomicBool::new(false);
        model
            .chat_turn_streaming("system", &[ChatMsg::user("hi")], &[], &mut |_| {}, &cancel)
            .unwrap();
        assert_eq!(model.last_turn_usage(), Some(TurnUsage { tokens_in: Some(7), tokens_out: Some(2) }));
    }

    #[test]
    fn chat_turn_streaming_request_includes_stream_options_include_usage() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let addr = server.server_addr().to_ip().unwrap();
        first_contact(addr);
        let handle = std::thread::spawn(move || {
            let mut request = server.recv().unwrap();
            let mut body = String::new();
            request.as_reader().read_to_string(&mut body).unwrap();
            let response = tiny_http::Response::from_string("data: [DONE]\n\n").with_header(
                tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"text/event-stream"[..]).unwrap(),
            );
            let _ = request.respond(response);
            body
        });
        let base_url = format!("http://{addr}");
        let model = OpenAiCompatModel::new("local:streamopts".into(), base_url, "test-model".into(), None).unwrap();
        let cancel = AtomicBool::new(false);
        let _ = model.chat_turn_streaming("system", &[ChatMsg::user("hi")], &[], &mut |_| {}, &cancel);
        let body = handle.join().unwrap();
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["stream_options"]["include_usage"], true, "{v}");
    }

    #[test]
    fn chat_forwards_the_configured_effort_as_reasoning_effort() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let addr = server.server_addr().to_ip().unwrap();
        first_contact(addr);
        let handle = std::thread::spawn(move || {
            let mut request = server.recv().unwrap();
            let mut body = String::new();
            request.as_reader().read_to_string(&mut body).unwrap();
            let response =
                tiny_http::Response::from_string(r#"{"choices":[{"message":{"role":"assistant","content":"ok"}}]}"#);
            let _ = request.respond(response);
            body
        });
        let base_url = format!("http://{addr}");
        let model = OpenAiCompatModel::new("local:effort".into(), base_url, "test-model".into(), None).unwrap();
        model.set_effort(Some("high"));
        model.chat("system", &[ChatMsg::user("hi")]).unwrap();
        let body = handle.join().unwrap();
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["reasoning_effort"], "high", "{v}");
    }

    #[test]
    fn set_effort_none_after_a_prior_call_clears_reasoning_effort_from_the_next_request() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let addr = server.server_addr().to_ip().unwrap();
        first_contact(addr);
        let handle = std::thread::spawn(move || {
            let mut bodies = Vec::new();
            for _ in 0..2 {
                let mut request = server.recv().unwrap();
                let mut body = String::new();
                request.as_reader().read_to_string(&mut body).unwrap();
                let response = tiny_http::Response::from_string(
                    r#"{"choices":[{"message":{"role":"assistant","content":"ok"}}]}"#,
                );
                let _ = request.respond(response);
                bodies.push(body);
            }
            bodies
        });
        let base_url = format!("http://{addr}");
        let model = OpenAiCompatModel::new("local:effortclear".into(), base_url, "test-model".into(), None).unwrap();
        model.set_effort(Some("high"));
        model.chat("system", &[ChatMsg::user("first")]).unwrap();
        model.set_effort(None);
        model.chat("system", &[ChatMsg::user("second")]).unwrap();
        let bodies = handle.join().unwrap();
        let first: serde_json::Value = serde_json::from_str(&bodies[0]).unwrap();
        let second: serde_json::Value = serde_json::from_str(&bodies[1]).unwrap();
        assert_eq!(first["reasoning_effort"], "high", "{first}");
        assert!(
            second.get("reasoning_effort").is_none(),
            "a later call with effort:None must not carry a stale reasoning_effort: {second}"
        );
    }
}
