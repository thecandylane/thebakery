//! GeminiModel — Google Gemini via API key or Gemini CLI / Code Assist OAuth.
//!
//! - **API key** (`GEMINI_API_KEY` / secrets): `generativelanguage.googleapis.com`
//! - **OAuth** (import `~/.gemini/oauth_creds.json` or stored refresh): Code Assist
//!   `cloudcode-pa.googleapis.com/v1internal` (jcode / oh-my-pi pattern)

use crate::secrets::{self, OAuthTokens};
use crate::shared::{effort_label, read_models_response, strip_outer_fence, user_edit_prompt, SYSTEM_EDIT_INSTRUCTIONS};
use crate::sse::{self, DriveOutcome};
use crate::tool_wire::{extract_gemini_reply, extract_gemini_usage, gemini_contents, gemini_function_declarations, GeminiStreamAssembler};
use crate::{
    ChatMsg, ChatReply, EditRequest, Model, ModelCatalog, ModelCatalogEntry, EffortLevel, ProposedEdit, StopReason,
    StreamEnd, ToolDecl, TurnUsage,
};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const DEV_API: &str = "https://generativelanguage.googleapis.com/v1beta";
const CODE_ASSIST: &str = "https://cloudcode-pa.googleapis.com/v1internal";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
// Desktop OAuth client id from Google's Gemini CLI (same as jcode/OMP). An id
// is an identifier, not a credential, so it stays in the source.
const GEMINI_CLIENT_ID: &str =
    "681255809395-oo8ft2oprdrnp9e3aqf6av3hmdib135j.apps.googleusercontent.com";
/// The matching client secret is supplied by the operator, never hardcoded.
const ENV_CLIENT_SECRET: &str = "BAKERY_GEMINI_CLIENT_SECRET";
const ENV_MODEL: &str = "BAKERY_GEMINI_MODEL";
const DEFAULT_MODEL: &str = "gemini-2.5-flash";
const MAX_TOKENS: u32 = 8192;

pub struct GeminiModel {
    model_id: String,
    /// Per-turn reasoning-effort override (07d-A3D1 Phase C) — only ever
    /// emitted into a request when `model_id` is a Gemini 3.x id (see
    /// `effort_levels_for`'s own reasoning); stored regardless so a
    /// non-3.x model just silently no-ops instead of needing a second code
    /// path.
    effort: Mutex<Option<String>>,
    /// This model's own report of the last completed turn's token usage
    /// (07d-A3D1 Phase C).
    last_usage: Mutex<Option<TurnUsage>>,
    last_thinking: Mutex<Option<crate::Thinking>>,
}

impl GeminiModel {
    pub fn from_env() -> Self {
        let s = secrets::load();
        let model_id = s
            .gemini_model_id
            .or_else(|| std::env::var(ENV_MODEL).ok())
            .unwrap_or_else(|| DEFAULT_MODEL.to_string());
        Self::with_model_id(model_id)
    }

    /// A fresh instance for a specific model id (07d-A3D1 Phase C) — see
    /// `ClaudeModel::with_model_id`'s doc for the full reasoning.
    pub fn with_model_id(model_id: impl Into<String>) -> Self {
        Self { model_id: model_id.into(), effort: Mutex::new(None), last_usage: Mutex::new(None), last_thinking: Mutex::new(None) }
    }

    /// This turn's `generationConfig.thinkingConfig` value, or `None` when
    /// either no effort override is active or this model id has no
    /// selectable effort concept at all (07d-A3D1 Phase C) — gated on the
    /// exact same `"gemini-3"` id check `effort_levels_for` already uses,
    /// reusing that already-shipped, already-reviewed judgment call rather
    /// than re-deriving it. A non-3.x model (2.5's `thinkingBudget` is a
    /// raw token count, not a level enum) silently ignores an effort
    /// override instead of erroring.
    fn thinking_config(&self) -> Option<Value> {
        if !self.model_id.contains("gemini-3") {
            return None;
        }
        let effort = self.effort.lock().unwrap().clone()?;
        Some(json!({ "thinkingLevel": effort.to_ascii_uppercase() }))
    }

    pub fn credentials_available() -> bool {
        let s = secrets::load();
        crate::proxy::route("gemini").is_some()
            || non_empty(s.gemini_api_key.as_deref())
            || non_empty(std::env::var("GEMINI_API_KEY").ok().as_deref())
            || non_empty(std::env::var("GOOGLE_API_KEY").ok().as_deref())
            || s.gemini_oauth
                .as_ref()
                .is_some_and(|o| !o.access.is_empty() || !o.refresh.is_empty())
            || gemini_cli_creds_path().map(|p| p.exists()).unwrap_or(false)
    }

    /// The Developer API's base URL and the key it takes: the model proxy's
    /// with the node's token when it holds a Gemini key, else Google's with
    /// the API key. (Code Assist OAuth always calls Google directly.)
    fn dev_api() -> Option<(String, String)> {
        crate::proxy::via("gemini", DEV_API)
            .or_else(|| Self::api_key().map(|key| (DEV_API.to_string(), key)))
    }

    fn api_key() -> Option<String> {
        let s = secrets::load();
        s.gemini_api_key
            .filter(|k| !k.trim().is_empty())
            .or_else(|| std::env::var("GEMINI_API_KEY").ok())
            .or_else(|| std::env::var("GOOGLE_API_KEY").ok())
            .map(|k| k.trim().to_string())
            .filter(|k| !k.is_empty())
    }

    fn call(&self, system: &str, messages: &[ChatMsg]) -> Result<String, String> {
        match self.call_turn(system, messages, &[])? {
            ChatReply::Text(s) => Ok(s),
            ChatReply::ToolCalls(_) => Err("gemini returned functionCall on a text-only request".into()),
        }
    }

    fn call_turn(&self, system: &str, messages: &[ChatMsg], tools: &[ToolDecl]) -> Result<ChatReply, String> {
        if let Some((base, key)) = Self::dev_api() {
            return self.call_dev_api(&base, &key, system, messages, tools);
        }
        let (access, project) = ensure_oauth_access()?;
        self.call_code_assist(&access, project.as_deref(), system, messages, tools)
    }

    fn call_dev_api(
        &self,
        base: &str,
        api_key: &str,
        system: &str,
        messages: &[ChatMsg],
        tools: &[ToolDecl],
    ) -> Result<ChatReply, String> {
        let url = format!("{base}/models/{}:generateContent", self.model_id);
        let mut body = json!({
            "system_instruction": { "parts": [{ "text": system }] },
            "contents": gemini_contents(messages),
            "generationConfig": { "maxOutputTokens": MAX_TOKENS }
        });
        if !tools.is_empty() {
            body["tools"] = gemini_function_declarations(tools);
        }
        if let Some(thinking_config) = self.thinking_config() {
            body["generationConfig"]["thinkingConfig"] = thinking_config;
        }
        let resp = ureq::post(&url)
            .set("x-goog-api-key", api_key)
            .set("content-type", "application/json")
            .timeout(Duration::from_secs(120))
            .send_json(body)
            .map_err(|e| format!("gemini: {e}"))?;
        let status = resp.status();
        let text = resp.into_string().map_err(|e| e.to_string())?;
        if !(200..300).contains(&status) {
            return Err(format!(
                "Gemini API {status}: {}",
                text.chars().take(600).collect::<String>()
            ));
        }
        *self.last_usage.lock().unwrap() = Some(extract_gemini_usage(&text));
        *self.last_thinking.lock().unwrap() = None;
        extract_gemini_reply(&text)
    }

    /// `:streamGenerateContent?alt=sse` instead of `:generateContent` — the
    /// URL choice (not a request-body flag, unlike the other three
    /// providers) is what switches this endpoint into SSE mode; without
    /// `?alt=sse` it buffers the whole response and returns one top-level
    /// JSON array instead. Each `data:` line is a **complete**
    /// `GenerateContentResponse` (`GeminiStreamAssembler` re-runs the same
    /// extraction `extract_gemini_reply` does, per chunk) — there is no
    /// completion sentinel, a clean EOF is this provider's own normal
    /// completion signal. API-key path only, matching `catalog()`'s own
    /// api-key-vs-OAuth branch: Code Assist has no confirmed public
    /// streaming-equivalent endpoint.
    fn call_dev_api_streaming(
        &self,
        base: &str,
        api_key: &str,
        system: &str,
        messages: &[ChatMsg],
        tools: &[ToolDecl],
        on_text_delta: &mut dyn FnMut(&str),
        cancel: &AtomicBool,
    ) -> Result<StreamEnd, String> {
        let url = format!("{base}/models/{}:streamGenerateContent?alt=sse", self.model_id);
        let mut body = json!({
            "system_instruction": { "parts": [{ "text": system }] },
            "contents": gemini_contents(messages),
            "generationConfig": { "maxOutputTokens": MAX_TOKENS }
        });
        if !tools.is_empty() {
            body["tools"] = gemini_function_declarations(tools);
        }
        if let Some(thinking_config) = self.thinking_config() {
            body["generationConfig"]["thinkingConfig"] = thinking_config;
        }

        let result = sse::streaming_agent()
            .post(&url)
            .set("x-goog-api-key", api_key)
            .set("content-type", "application/json")
            .set("accept", "text/event-stream")
            .send_json(body);

        let resp = match result {
            Ok(r) => r,
            Err(ureq::Error::Status(status, resp)) => {
                let text = resp.into_string().unwrap_or_default();
                return Err(format!("Gemini API {status}: {}", text.chars().take(600).collect::<String>()));
            }
            Err(e) => return Err(format!("gemini stream request: {e}")),
        };

        let mut reader = sse::SseReader::new(resp.into_reader());
        let mut assembler = GeminiStreamAssembler::new();
        let outcome = sse::drive(&mut reader, cancel, |ev| assembler.feed(ev, on_text_delta));
        *self.last_usage.lock().unwrap() = Some(assembler.usage());
        *self.last_thinking.lock().unwrap() = assembler.thinking();

        match outcome {
            // No completion sentinel exists for this provider — a clean
            // EOF is the normal way this stream ends, so `assembler.feed`
            // never itself returns `Stop`; `FinishedNormally` is handled
            // anyway for exhaustiveness.
            DriveOutcome::FinishedNormally | DriveOutcome::Eof => {
                let reply = assembler.finish()?;
                Ok(StreamEnd::Completed(reply))
            }
            DriveOutcome::Cancelled => Ok(StreamEnd::Stopped {
                partial_text: assembler.partial_text_so_far(),
                reason: StopReason::Cancelled,
            }),
            DriveOutcome::IoError(e) => {
                let partial = assembler.partial_text_so_far();
                if partial.is_empty() {
                    Err(format!("gemini stream error: {e}"))
                } else {
                    Ok(StreamEnd::Stopped { partial_text: partial, reason: StopReason::StreamDied(e) })
                }
            }
        }
    }

    fn call_code_assist(
        &self,
        access: &str,
        project: Option<&str>,
        system: &str,
        messages: &[ChatMsg],
        tools: &[ToolDecl],
    ) -> Result<ChatReply, String> {
        let project = match project {
            Some(p) if !p.is_empty() => p.to_string(),
            _ => discover_project(access)?,
        };
        // Persist project for next time
        let mut s = secrets::load();
        if let Some(ref mut o) = s.gemini_oauth {
            if o.project.as_deref() != Some(project.as_str()) {
                o.project = Some(project.clone());
                let _ = secrets::save(&s);
            }
        }

        let mut request = json!({
            "contents": gemini_contents(messages),
            "systemInstruction": {
                "role": "user",
                "parts": [{ "text": system }]
            },
            "generationConfig": { "maxOutputTokens": MAX_TOKENS }
        });
        if !tools.is_empty() {
            request["tools"] = gemini_function_declarations(tools);
        }
        if let Some(thinking_config) = self.thinking_config() {
            request["generationConfig"]["thinkingConfig"] = thinking_config;
        }
        let body = json!({
            "project": project,
            "model": self.model_id,
            "request": request
        });
        let url = format!("{CODE_ASSIST}:generateContent");
        let resp = ureq::post(&url)
            .set("Authorization", &format!("Bearer {access}"))
            .set("content-type", "application/json")
            .timeout(Duration::from_secs(120))
            .send_json(body)
            .map_err(|e| format!("gemini code assist: {e}"))?;
        let status = resp.status();
        let text = resp.into_string().map_err(|e| e.to_string())?;
        if !(200..300).contains(&status) {
            return Err(format!(
                "Gemini Code Assist {status}: {}",
                text.chars().take(600).collect::<String>()
            ));
        }
        *self.last_usage.lock().unwrap() = Some(extract_gemini_usage(&text));
        *self.last_thinking.lock().unwrap() = None;
        if let Ok(t) = extract_gemini_reply(&text) {
            return Ok(t);
        }
        let v: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        if let Some(inner) = v.get("response") {
            return extract_gemini_reply(&inner.to_string());
        }
        Err("gemini: no text in code-assist response".into())
    }
}

impl Model for GeminiModel {
    fn name(&self) -> &str {
        "gemini"
    }

    /// `inlineData` parts: images, and PDFs (document understanding).
    fn native_attachment_types(&self) -> &'static [&'static str] {
        &["image/png", "image/jpeg", "image/webp", "image/heic", "image/heif", "application/pdf"]
    }

    fn propose_edit(&self, req: &EditRequest) -> Result<ProposedEdit, String> {
        if req.file_path.trim().is_empty() {
            return Err("file_path required".to_string());
        }
        let user_text = user_edit_prompt(&req.instruction, &req.file_path, &req.current_content);
        let mut raw = self.call(SYSTEM_EDIT_INSTRUCTIONS, &[ChatMsg::user(user_text)])?;
        raw = strip_outer_fence(&raw);
        if raw.is_empty() {
            return Err("model returned empty content".into());
        }
        Ok(ProposedEdit {
            file_path: req.file_path.clone(),
            new_content: raw,
            rationale: format!("gemini:{}", self.model_id),
        })
    }

    fn supports_chat(&self) -> bool {
        true
    }

    fn chat(&self, system: &str, messages: &[ChatMsg]) -> Result<String, String> {
        self.call(system, messages)
    }

    fn chat_turn(
        &self,
        system: &str,
        messages: &[ChatMsg],
        tools: &[ToolDecl],
    ) -> Result<ChatReply, String> {
        self.call_turn(system, messages, tools)
    }

    /// API-key path only, mirroring `catalog()`'s own branch exactly: Code
    /// Assist (OAuth) has no confirmed public streaming endpoint, so that
    /// path degrades to one blocking `chat_turn` call wrapped as a single
    /// `Completed` — the same thing the trait's own default does, just
    /// applied conditionally rather than for every call.
    fn chat_turn_streaming(
        &self,
        system: &str,
        messages: &[ChatMsg],
        tools: &[ToolDecl],
        on_text_delta: &mut dyn FnMut(&str),
        cancel: &AtomicBool,
    ) -> Result<StreamEnd, String> {
        // Cleared unconditionally before any work starts (review fix): the
        // Code-Assist/OAuth branch below can return `Stopped{Cancelled}`
        // without ever reaching a real network call when `cancel` is already
        // true at entry, and `last_turn_usage()`'s contract promises usage
        // "for whichever call just completed" — leaving a prior turn's
        // value in place here would misreport it as belonging to this
        // cancelled turn. Harmless on every other path: a real completion a
        // few lines below (`call_dev_api`/`call_dev_api_streaming`/
        // `call_code_assist`, via `chat_turn`) always overwrites this with a
        // fresh value on success.
        *self.last_usage.lock().unwrap() = None;
        let Some((base, key)) = Self::dev_api() else {
            if cancel.load(Ordering::Acquire) {
                return Ok(StreamEnd::Stopped { partial_text: String::new(), reason: StopReason::Cancelled });
            }
            let reply = self.chat_turn(system, messages, tools)?;
            if let ChatReply::Text(ref s) = reply {
                if !s.is_empty() {
                    on_text_delta(s);
                }
            }
            return Ok(StreamEnd::Completed(reply));
        };
        self.call_dev_api_streaming(&base, &key, system, messages, tools, on_text_delta, cancel)
    }

    fn supports_streaming(&self) -> bool {
        Self::dev_api().is_some()
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
        // Only the API-key ("dev API") path is confirmed public and
        // documented (ai.google.dev/api/models, Sep 2026). Code Assist
        // (`CODE_ASSIST`, the OAuth path) is Google's internal Cloud Code
        // backend with no confirmed equivalent list-models endpoint — rather
        // than guess at an undocumented internal API, an OAuth session
        // reports Unavailable, same as any other subscription CLI with
        // nothing this code can safely enumerate.
        let Some((base, key)) = Self::dev_api() else {
            return Ok(ModelCatalog::Unavailable {
                default: ModelCatalogEntry {
                    id: self.model_id.clone(),
                    label: self.model_id.clone(),
                    context_window: None,
                    effort_levels: Vec::new(),
                },
                reason: "no Gemini API key configured — a Code Assist OAuth session has no confirmed public models-list endpoint".to_string(),
            });
        };
        let url = format!("{base}/models?pageSize=1000");
        let result = ureq::get(&url)
            .set("x-goog-api-key", &key)
            .set("accept", "application/json")
            .timeout(Duration::from_secs(20))
            .call();
        let text = read_models_response(result, "Gemini")?;
        parse_catalog(&text)
    }
}

/// `models.list`'s effort-support isn't reported per-model in the response
/// itself — inferred from the model id (ai.google.dev/gemini-api/docs/
/// thinking, Sep 2026): Gemini 3.x exposes a fixed `thinkingLevel` enum
/// (exact set varies per model — some add `medium`/`minimal`; `low`/`high`
/// is the safe common denominator every 3.x model supports). Gemini 2.5's
/// `thinkingBudget` is a raw token count, not a level enum, so it gets no
/// synthesized levels here rather than fabricated ones.
fn effort_levels_for(model_id: &str) -> Vec<EffortLevel> {
    if model_id.contains("gemini-3") {
        ["low", "high"]
            .into_iter()
            .map(|id| EffortLevel { id: id.to_string(), label: effort_label(id) })
            .collect()
    } else {
        Vec::new()
    }
}

/// Parses `models.list`'s `{"models": [{"name": "models/<id>", ...}]}` —
/// `name` always carries a `models/` prefix over the wire; stripped here so
/// `id` matches the bare id this file uses everywhere else (`DEFAULT_MODEL`,
/// `generateContent` URLs, ...).
///
/// The real endpoint mixes chat models in with non-chat ones (embedding-*,
/// text-embedding-*, imagen-*, veo-*, aqa, ...) in the same array,
/// distinguished only by `supportedGenerationMethods` (ai.google.dev/api/
/// models, Sep 2026) — this file only ever calls `generateContent`
/// (`call_dev_api`'s `{model}:generateContent` URL), so a model missing that
/// method from its supported list would fail at chat time if ever selected
/// (07d-A3D1 review). Skipped here instead. A model whose response happens
/// to omit the field entirely (not expected from the real API, but not worth
/// failing over) is kept rather than dropped — permissive, not silently
/// hiding a model over a schema surprise.
fn parse_catalog(text: &str) -> Result<ModelCatalog, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("gemini models parse: {e}"))?;
    let data = v
        .get("models")
        .and_then(|d| d.as_array())
        .ok_or("gemini models: response had no \"models\" array")?;
    let mut models = Vec::new();
    for m in data {
        let supports_chat = m
            .get("supportedGenerationMethods")
            .and_then(|x| x.as_array())
            .map(|methods| methods.iter().any(|v| v.as_str() == Some("generateContent")))
            .unwrap_or(true);
        if !supports_chat {
            continue;
        }
        let Some(raw_name) = m.get("name").and_then(|x| x.as_str()) else { continue };
        let id = raw_name.strip_prefix("models/").unwrap_or(raw_name).to_string();
        let label = m
            .get("displayName")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| id.clone());
        let context_window = m.get("inputTokenLimit").and_then(|x| x.as_u64());
        let effort_levels = effort_levels_for(&id);
        models.push(ModelCatalogEntry { id, label, context_window, effort_levels });
    }
    if models.is_empty() {
        return Err("gemini models: catalog came back empty".to_string());
    }
    Ok(ModelCatalog::Available { models })
}

/// Import Gemini CLI OAuth credentials from `~/.gemini/oauth_creds.json`.
pub fn import_cli_credentials() -> Result<String, String> {
    let path = gemini_cli_creds_path().ok_or("HOME not set")?;
    if !path.exists() {
        return Err(format!(
            "Gemini CLI credentials not found at {} — run `gemini` once to log in",
            path.display()
        ));
    }
    let raw = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let v: Value = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
    let access = v
        .get("access_token")
        .and_then(|a| a.as_str())
        .unwrap_or("")
        .to_string();
    let refresh = v
        .get("refresh_token")
        .and_then(|a| a.as_str())
        .unwrap_or("")
        .to_string();
    if access.is_empty() && refresh.is_empty() {
        return Err("oauth_creds.json has no tokens".into());
    }
    let expires = v
        .get("expiry_date")
        .or_else(|| v.get("expires_at"))
        .and_then(|e| e.as_i64())
        .unwrap_or(0);
    let mut tokens = OAuthTokens {
        access,
        refresh,
        expires,
        email: None,
        project: std::env::var("GOOGLE_CLOUD_PROJECT")
            .ok()
            .or_else(|| std::env::var("GOOGLE_CLOUD_PROJECT_ID").ok()),
    };
    // Refresh if needed to get a usable access token
    if !is_fresh(tokens.expires) && !tokens.refresh.is_empty() {
        tokens = refresh_oauth(&tokens.refresh)?;
    }
    secrets::set_gemini_oauth(tokens)?;
    Ok(format!("imported Gemini CLI OAuth from {}", path.display()))
}

fn ensure_oauth_access() -> Result<(String, Option<String>), String> {
    // Prefer secrets store; optionally auto-import CLI file.
    let mut s = secrets::load();
    if s.gemini_oauth.is_none() {
        if gemini_cli_creds_path().map(|p| p.exists()).unwrap_or(false) {
            import_cli_credentials()?;
            s = secrets::load();
        }
    }
    let mut oauth = s
        .gemini_oauth
        .clone()
        .ok_or("no Gemini OAuth — import CLI login or set GEMINI_API_KEY")?;
    if !is_fresh(oauth.expires) && !oauth.refresh.is_empty() {
        oauth = refresh_oauth(&oauth.refresh)?;
        let project = oauth.project.clone();
        secrets::set_gemini_oauth(oauth.clone())?;
        return Ok((oauth.access, project));
    }
    if oauth.access.is_empty() {
        return Err("Gemini OAuth has empty access token".into());
    }
    Ok((oauth.access, oauth.project))
}

/// The Gemini desktop-client secret, from the operator's environment or the
/// node-side secrets store — never from the source tree.
///
/// RULE: no credential material is hardcoded in this repository, and nothing an
/// agent can read ever holds one. `~/.bakery/secrets.json` sits outside every
/// worktree, so the agent tool jail cannot reach it; this function is the only
/// way the value enters the process.
fn client_secret() -> Result<String, String> {
    secrets::load()
        .gemini_client_secret
        .or_else(|| std::env::var(ENV_CLIENT_SECRET).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            format!(
                "no Gemini OAuth client secret — set {ENV_CLIENT_SECRET} or \
                 gemini_client_secret in ~/.bakery/secrets.json. It is the desktop \
                 client secret Google ships with the Gemini CLI; this repo does not \
                 carry credential material. An API key (GEMINI_API_KEY) avoids OAuth entirely."
            )
        })
}

fn refresh_oauth(refresh_token: &str) -> Result<OAuthTokens, String> {
    let form = format!(
        "client_id={}&client_secret={}&refresh_token={}&grant_type=refresh_token",
        form_enc(GEMINI_CLIENT_ID),
        form_enc(&client_secret()?),
        form_enc(refresh_token),
    );
    let resp = ureq::post(TOKEN_URL)
        .set("Content-Type", "application/x-www-form-urlencoded")
        .timeout(Duration::from_secs(30))
        .send_string(&form)
        .map_err(|e| format!("gemini refresh: {e}"))?;
    let status = resp.status();
    let text = resp.into_string().map_err(|e| e.to_string())?;
    if !(200..300).contains(&status) {
        return Err(format!(
            "gemini refresh {status}: {}",
            text.chars().take(300).collect::<String>()
        ));
    }
    let v: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let access = v
        .get("access_token")
        .and_then(|a| a.as_str())
        .ok_or("missing access_token")?
        .to_string();
    let refresh = v
        .get("refresh_token")
        .and_then(|a| a.as_str())
        .unwrap_or(refresh_token)
        .to_string();
    let expires_in = v.get("expires_in").and_then(|e| e.as_i64()).unwrap_or(3600);
    Ok(OAuthTokens {
        access,
        refresh,
        expires: now_ms() + expires_in * 1000 - 60_000,
        email: None,
        project: secrets::load().gemini_oauth.and_then(|o| o.project),
    })
}

fn discover_project(access: &str) -> Result<String, String> {
    if let Ok(p) = std::env::var("GOOGLE_CLOUD_PROJECT") {
        if !p.is_empty() {
            return Ok(p);
        }
    }
    if let Ok(p) = std::env::var("GOOGLE_CLOUD_PROJECT_ID") {
        if !p.is_empty() {
            return Ok(p);
        }
    }
    let body = json!({
        "metadata": {
            "ideType": "IDE_UNSPECIFIED",
            "platform": "PLATFORM_UNSPECIFIED",
            "pluginType": "GEMINI"
        }
    });
    let url = format!("{CODE_ASSIST}:loadCodeAssist");
    let resp = ureq::post(&url)
        .set("Authorization", &format!("Bearer {access}"))
        .set("content-type", "application/json")
        .timeout(Duration::from_secs(30))
        .send_json(body)
        .map_err(|e| format!("loadCodeAssist: {e}"))?;
    let status = resp.status();
    let text = resp.into_string().map_err(|e| e.to_string())?;
    if !(200..300).contains(&status) {
        return Err(format!(
            "loadCodeAssist {status}: {} — set GOOGLE_CLOUD_PROJECT or complete Gemini CLI onboarding",
            text.chars().take(400).collect::<String>()
        ));
    }
    let v: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    v.get("cloudaicompanionProject")
        .and_then(|p| p.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .ok_or_else(|| {
            "no cloudaicompanionProject — set GOOGLE_CLOUD_PROJECT or finish Gemini CLI setup"
                .into()
        })
}

fn form_enc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn gemini_cli_creds_path() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join(".gemini/oauth_creds.json"))
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn is_fresh(expires: i64) -> bool {
    if expires <= 0 {
        return true;
    }
    expires > now_ms()
}

fn non_empty(s: Option<&str>) -> bool {
    s.is_some_and(|v| !v.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_parts() {
        let j = r#"{"candidates":[{"content":{"parts":[{"text":"hello"}]}}]}"#;
        assert_eq!(extract_gemini_reply(j).unwrap(), ChatReply::Text("hello".into()));
    }

    #[test]
    fn parse_catalog_strips_the_models_prefix_and_infers_effort_for_gemini_3() {
        let body = r#"{
            "models": [
                {"name": "models/gemini-3-pro-preview", "displayName": "Gemini 3 Pro Preview", "inputTokenLimit": 1048576, "outputTokenLimit": 65536, "supportedGenerationMethods": ["generateContent", "countTokens"]},
                {"name": "models/gemini-2.5-flash", "displayName": "Gemini 2.5 Flash", "inputTokenLimit": 1048576, "outputTokenLimit": 65536, "supportedGenerationMethods": ["generateContent"]}
            ],
            "nextPageToken": ""
        }"#;
        let catalog = parse_catalog(body).unwrap();
        let ModelCatalog::Available { models } = catalog else { panic!("expected Available") };
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "gemini-3-pro-preview", "the models/ prefix must be stripped");
        assert_eq!(models[0].label, "Gemini 3 Pro Preview");
        assert_eq!(models[0].context_window, Some(1_048_576));
        assert_eq!(
            models[0].effort_levels.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            vec!["low", "high"]
        );
        assert_eq!(models[1].id, "gemini-2.5-flash");
        assert!(models[1].effort_levels.is_empty(), "2.5's thinkingBudget is a token count, not a level enum");
    }

    /// 07d-A3D1 review: the real `models.list` response mixes non-chat
    /// models (embeddings, imagen, ...) into the same array — a model whose
    /// `supportedGenerationMethods` doesn't include `generateContent` must
    /// never appear in the catalog, since this file only ever calls that
    /// method.
    #[test]
    fn parse_catalog_filters_out_non_chat_models() {
        let body = r#"{
            "models": [
                {"name": "models/gemini-2.5-flash", "displayName": "Gemini 2.5 Flash", "supportedGenerationMethods": ["generateContent"]},
                {"name": "models/text-embedding-004", "displayName": "Text Embedding 004", "supportedGenerationMethods": ["embedContent"]},
                {"name": "models/aqa", "displayName": "Model that performs Attributed Question Answering", "supportedGenerationMethods": ["generateAnswer"]}
            ]
        }"#;
        let catalog = parse_catalog(body).unwrap();
        let ModelCatalog::Available { models } = catalog else { panic!("expected Available") };
        assert_eq!(models.len(), 1, "only the generateContent-capable model should survive");
        assert_eq!(models[0].id, "gemini-2.5-flash");
    }

    /// A model missing `supportedGenerationMethods` entirely (not expected
    /// from the real API, but not worth failing over) is kept rather than
    /// silently dropped.
    #[test]
    fn parse_catalog_keeps_a_model_missing_the_generation_methods_field() {
        let body = r#"{"models": [{"name": "models/gemini-2.5-flash", "displayName": "Gemini 2.5 Flash"}]}"#;
        let catalog = parse_catalog(body).unwrap();
        let ModelCatalog::Available { models } = catalog else { panic!("expected Available") };
        assert_eq!(models.len(), 1);
    }

    #[test]
    fn parse_catalog_errors_on_an_empty_models_array() {
        assert!(parse_catalog(r#"{"models": []}"#).is_err());
    }

    /// `supports_streaming()` must reflect the same api-key-vs-OAuth branch
    /// `catalog()` already uses — Code Assist (OAuth) has no confirmed
    /// public streaming endpoint. Conditional on the ambient
    /// `GEMINI_API_KEY`/`GOOGLE_API_KEY` env vars (not isolated by
    /// `with_isolated_credentials`, same residual this crate's other
    /// Gemini tests already tolerate) so this stays deterministic
    /// regardless of what a developer machine happens to export.
    #[test]
    fn supports_streaming_reflects_whether_an_api_key_is_configured() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        secrets::test_support::with_isolated_credentials(&path, || {
            let model = GeminiModel::with_model_id("gemini-test");
            if GeminiModel::api_key().is_none() {
                assert!(!model.supports_streaming());
            }
            let mut s = secrets::SecretsFile::default();
            s.gemini_api_key = Some("test-only-not-real-gemini-key".into());
            secrets::save_to(&path, &s).unwrap();
            assert!(model.supports_streaming(), "an isolated api key must make supports_streaming true");
        });
    }

    /// `API_URL`/`DEV_API` streaming endpoint construction is a hardcoded
    /// constant (same as `claude.rs`/`grok.rs` — see their equivalent
    /// tests' notes on why a real fake-server round trip isn't reachable
    /// here). What's provable without network or real OAuth credentials:
    /// with no API key configured, `chat_turn_streaming` degrades to the
    /// no-streaming-endpoint fallback path rather than attempting one, and
    /// still fails closed exactly like `chat_turn` does when neither an API
    /// key nor a usable OAuth session exists.
    #[test]
    fn chat_turn_streaming_falls_back_and_still_fails_closed_without_any_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        secrets::test_support::with_isolated_credentials(&path, || {
            let model = GeminiModel::with_model_id("gemini-test");
            let cancel = std::sync::atomic::AtomicBool::new(false);
            let result = model.chat_turn_streaming("system", &[], &[], &mut |_| {}, &cancel);
            if !GeminiModel::credentials_available() {
                let err = result.map(|_| ()).unwrap_err();
                assert!(
                    err.to_ascii_lowercase().contains("gemini") || err.to_ascii_lowercase().contains("oauth"),
                    "{err}"
                );
            }
        });
    }

    /// Review fix: a Code-Assist/OAuth-path call that's already cancelled at
    /// entry must not leave a *previous* turn's usage sitting in
    /// `last_turn_usage()` — it never reached the network, so it reported
    /// nothing.
    #[test]
    fn chat_turn_streaming_clears_stale_usage_when_cancelled_before_any_network_call() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        secrets::test_support::with_isolated_credentials(&path, || {
            let model = GeminiModel::with_model_id("gemini-test");
            assert!(GeminiModel::api_key().is_none(), "isolated credentials must leave no dev API key configured");
            *model.last_usage.lock().unwrap() = Some(TurnUsage { tokens_in: Some(500), tokens_out: Some(120) });

            let cancel = std::sync::atomic::AtomicBool::new(true);
            let result = model.chat_turn_streaming("system", &[], &[], &mut |_| {}, &cancel);

            assert!(
                matches!(result, Ok(StreamEnd::Stopped { reason: StopReason::Cancelled, .. })),
                "an already-cancelled call with no API key must take the early-return branch, {result:?}"
            );
            assert_eq!(
                model.last_turn_usage(),
                None,
                "a cancelled-before-any-call turn must not report a previous turn's stale usage"
            );
        });
    }

    #[test]
    fn set_effort_round_trips_through_the_interior_side_channel_and_clears_on_none() {
        let model = GeminiModel::with_model_id("gemini-3-test");
        assert_eq!(*model.effort.lock().unwrap(), None);
        model.set_effort(Some("high"));
        assert_eq!(*model.effort.lock().unwrap(), Some("high".to_string()));
        model.set_effort(None);
        assert_eq!(*model.effort.lock().unwrap(), None, "a later call with None must clear a prior value");
    }

    /// 07d-A3D1 Phase C: `thinking_config` must only ever apply to a Gemini
    /// 3.x model id — reusing the exact same id check `effort_levels_for`
    /// already uses for the catalog, not a fresh guess. A 2.5 model (whose
    /// `thinkingBudget` is a raw token count, not a level enum) silently
    /// ignores an effort override rather than emitting a nonsensical field.
    #[test]
    fn thinking_config_only_applies_to_gemini_3_model_ids() {
        let gemini3 = GeminiModel::with_model_id("gemini-3-pro-preview");
        gemini3.set_effort(Some("high"));
        assert_eq!(gemini3.thinking_config(), Some(json!({ "thinkingLevel": "HIGH" })));

        let gemini25 = GeminiModel::with_model_id("gemini-2.5-flash");
        gemini25.set_effort(Some("high"));
        assert_eq!(gemini25.thinking_config(), None, "a non-3.x model id must silently no-op");

        let gemini3_no_effort = GeminiModel::with_model_id("gemini-3-pro-preview");
        assert_eq!(gemini3_no_effort.thinking_config(), None, "no effort override set must also be None");
    }

    /// Review fix (test-coverage gap): unlike `ClaudeModel`, `GeminiModel`
    /// had no direct test of its own `with_overridden_model_id` — only an
    /// unrelated node-side test double exercised the trait method's
    /// contract at all.
    #[test]
    fn with_overridden_model_id_builds_a_distinct_instance_with_its_own_fresh_state() {
        let model = GeminiModel::with_model_id("gemini-original");
        model.set_effort(Some("high"));
        let overridden = model.with_overridden_model_id("gemini-variant").expect("Gemini supports per-turn overrides");
        assert_eq!(overridden.name(), "gemini", "Model::name() is the provider bucket, not the specific variant");
        assert!(overridden.last_turn_usage().is_none(), "a fresh override instance starts with no usage");
    }
}
