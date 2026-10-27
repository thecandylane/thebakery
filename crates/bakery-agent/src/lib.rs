//! bakery-agent — the model seam (M4 / M4b / M5.6).
//!
//! Providers:
//! - [`StubModel`] — offline
//! - [`ClaudeModel`] — console `ANTHROPIC_API_KEY` (env or secrets store)
//! - [`GrokModel`] — xAI OAuth (device flow) or `XAI_API_KEY`
//! - [`GeminiModel`] — Gemini CLI OAuth import or `GEMINI_API_KEY`
//! - optional local-only Claude Code OAuth (`BAKERY_MODEL=claude-code`)
//!
//! Secrets live in `$HOME/.bakery/secrets.json` — never in the Heart.

mod claude;
pub mod claude_code_slot;
pub mod cli_tools;
pub mod gemini;
mod grok;
pub mod openai_compat;
pub mod proxy;
pub mod secrets;
mod shared;
pub mod sse;
pub mod tool_wire;
pub mod xai_oauth;

#[cfg(has_claude_code_oauth)]
pub(crate) mod claude_code_oauth {
    include!(env!("BAKERY_CLAUDE_CODE_OAUTH_RS"));
}

pub use claude::ClaudeModel;
pub use gemini::GeminiModel;
pub use grok::GrokModel;
pub use openai_compat::OpenAiCompatModel;

use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// What the harness hands the model for one edit turn.
pub struct EditRequest {
    pub instruction: String,
    pub file_path: String,
    pub current_content: String,
}

pub struct ProposedEdit {
    pub file_path: String,
    pub new_content: String,
    pub rationale: String,
}

/// One operator-selected attachment handed to a native agent runtime.  The
/// node owns storage/lifetime and resolves opaque ids immediately before a
/// turn; providers never receive a host filesystem path.
#[derive(Debug, Clone)]
pub struct NativeAttachment {
    pub filename: String,
    pub mime_type: String,
    pub data: Vec<u8>,
}

/// One value from a native runtime's session configuration.  This deliberately
/// carries only the two ACP value shapes Bakery can validate and acknowledge;
/// it never represents authentication material or arbitrary adapter metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum NativeConfigValue {
    Select(String),
    Boolean(bool),
}

/// A configuration item captured for a single native turn.  The adapter owns
/// the option id vocabulary; Bakery only replays values it previously received
/// from that adapter's session configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeConfigSelection {
    pub id: String,
    pub value: NativeConfigValue,
}

/// The acknowledged native configuration an agent's picker shows, for one
/// runtime: a new session for the same agent and runtime replays it after
/// re-validating every option against its fresh setup. `generation` is
/// provenance for the session it came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeConfigSnapshot {
    pub runtime_id: String,
    pub generation: u64,
    #[serde(default)]
    pub values: Vec<NativeConfigSelection>,
}

/// Who said it. System prompts are passed separately (providers place them
/// differently). Tool results are a third role so native provider tool
/// calling can round-trip without stuffing JSON into the user channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatRole {
    User,
    Assistant,
    Tool,
}

/// One declared Bakery tool, handed to HTTP providers in their native schema.
#[derive(Debug, Clone)]
pub struct ToolDecl {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

impl ToolDecl {
    pub fn openai_tool(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "function",
            "function": {
                "name": self.name,
                "description": self.description,
                "parameters": self.parameters,
            }
        })
    }
}

/// One native tool call from a provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

/// One model turn: either a final text reply or native tool calls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatReply {
    Text(String),
    ToolCalls(Vec<ToolCall>),
}

/// A provider's own report of one turn's token usage (07d-A3D1 Phase C) —
/// deliberately minimal (provider-report-only), never fabricated when a
/// field is absent from the response. `context_used`/`context_window` are
/// not provider-report-only concepts (a provider's chat/completions
/// response never reports the model's context window; that only ever
/// comes from `Model::catalog()`), so they are composed on the `bakery-node`
/// side instead, not carried here.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TurnUsage {
    pub tokens_in: Option<u64>,
    pub tokens_out: Option<u64>,
}

/// What a model thought during one call (W1-T): its thoughts as its
/// provider shared them; empty when it thought without sharing them
/// (redacted thinking, hidden reasoning that only counts its tokens).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Thinking {
    pub text: String,
}

/// What a runtime reports about a turn beyond [`TurnUsage`]'s two counts —
/// today only a native (ACP) runtime does. Every field stays `None` unless
/// the runtime sent it, never 0. A separate type rather than more
/// `TurnUsage` fields, so a provider built outside this tree, which
/// constructs and copies `TurnUsage`, keeps compiling.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TurnUsageDetail {
    pub thought_tokens: Option<u64>,
    pub cache_read_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    pub cost: Option<TurnCost>,
    /// The runtime's own context reading when the turn ended (tokens in
    /// context, window size). It beats a sum of the turn's tokens, which
    /// leaves out a cached prompt.
    pub context_used: Option<u64>,
    pub context_window: Option<u64>,
}

/// What one turn cost, as the runtime priced it.
#[derive(schemars::JsonSchema, Debug, Clone, PartialEq, Serialize)]
pub struct TurnCost {
    pub amount: f64,
    /// ISO 4217 code, as the runtime sent it ("USD").
    pub currency: String,
}

/// Why a streaming [`Model::chat_turn_streaming`] call ended without
/// reaching a real completion (07d-A5) — never a normal, actionable reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    /// The caller's `cancel` flag was observed set.
    Cancelled,
    /// The stream ended before its protocol's own completion signal arrived
    /// (dropped connection, malformed frame, or a hard wall-clock cap) —
    /// but only ever reached once at least some real text had already
    /// streamed; a zero-text failure is a plain `Err` instead. Carries a
    /// short diagnostic, not a user-facing string.
    StreamDied(String),
}

/// How a [`Model::chat_turn_streaming`] call ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamEnd {
    /// The model finished this turn normally — identical to what
    /// [`Model::chat_turn`] would have returned for the same conversation.
    /// Safe to dispatch tool calls / record a real completion exactly as
    /// `chat_turn`'s result is treated today.
    Completed(ChatReply),
    /// The turn did NOT reach a legitimate completion signal. Carries only
    /// whatever text had already been handed to the delta callback — never
    /// a tool call, even a fully-formed one that happened to finish just
    /// before the stop: a cancelled or lost turn discards its actionability
    /// entirely, leaving behind only a best-effort preview of what a human
    /// already saw.
    Stopped { partial_text: String, reason: StopReason },
}

/// One turn of a tool-loop conversation.
#[derive(Debug, Clone)]
pub struct ChatMsg {
    pub role: ChatRole,
    pub content: String,
    pub tool_call_id: Option<String>,
    pub tool_name: Option<String>,
    pub tool_calls: Vec<ToolCall>,
    /// Files a user message carries for the provider to read natively — an
    /// image, or a PDF where the provider takes documents. Only types the
    /// model lists in [`Model::native_attachment_types`] are ever put here;
    /// the node turns every other file into text the model can read.
    pub attachments: Vec<NativeAttachment>,
}

impl ChatMsg {
    pub fn user(content: impl Into<String>) -> Self {
        Self::user_with_attachments(content, Vec::new())
    }
    pub fn user_with_attachments(content: impl Into<String>, attachments: Vec<NativeAttachment>) -> Self {
        Self {
            role: ChatRole::User,
            content: content.into(),
            tool_call_id: None,
            tool_name: None,
            tool_calls: Vec::new(),
            attachments,
        }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::Assistant,
            content: content.into(),
            tool_call_id: None,
            tool_name: None,
            tool_calls: Vec::new(),
            attachments: Vec::new(),
        }
    }
    pub fn assistant_tools(tool_calls: Vec<ToolCall>) -> Self {
        Self {
            role: ChatRole::Assistant,
            content: String::new(),
            tool_call_id: None,
            tool_name: None,
            tool_calls,
            attachments: Vec::new(),
        }
    }
    pub fn tool_result(id: impl Into<String>, name: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::Tool,
            content: content.into(),
            tool_call_id: Some(id.into()),
            tool_name: Some(name.into()),
            tool_calls: Vec::new(),
            attachments: Vec::new(),
        }
    }
    /// Provider-neutral role string ("user" / "assistant" / "tool").
    pub fn role_str(&self) -> &'static str {
        match self.role {
            ChatRole::User => "user",
            ChatRole::Assistant => "assistant",
            ChatRole::Tool => "tool",
        }
    }
}

pub trait Model: Send + Sync {
    fn name(&self) -> &str {
        "model"
    }

    fn propose_edit(&self, req: &EditRequest) -> Result<ProposedEdit, String>;

    /// Multi-turn text completion. HTTP seats use [`Model::chat_turn`] for the
    /// tool loop; this remains for single-shot edit and ACP-adjacent callers.
    fn chat(&self, _system: &str, _messages: &[ChatMsg]) -> Result<String, String> {
        Err(format!("model {} does not support the tool loop", self.name()))
    }

    /// One native tool-calling turn. HTTP providers send `tools` in their
    /// own schema and return [`ChatReply::ToolCalls`] or text. The default
    /// wraps [`Model::chat`] as text so ACP/setup models stay unchanged.
    fn chat_turn(
        &self,
        system: &str,
        messages: &[ChatMsg],
        _tools: &[ToolDecl],
    ) -> Result<ChatReply, String> {
        Ok(ChatReply::Text(self.chat(system, messages)?))
    }

    /// Whether the tool loop can run against this model at all.
    fn supports_chat(&self) -> bool {
        false
    }

    /// Same contract as [`Model::chat_turn`], but text arrives incrementally
    /// through `on_text_delta` as it's generated, and `cancel` is polled
    /// throughout generation so a caller can stop a turn early (07d-A5).
    /// Tool-call arguments are only ever handed back once fully assembled
    /// inside the returned [`StreamEnd`] — `on_text_delta` never sees a
    /// partial tool call, and never sees thinking/reasoning content of any
    /// kind for any provider (Claude's `thinking`/`redacted_thinking`
    /// blocks, xAI's `reasoning_content` delta field, Gemini's
    /// `thought: true` parts) — the same hard requirement
    /// `tool_wire::extract_anthropic_reply` already enforces for the
    /// non-streaming path, preserved exactly here.
    ///
    /// The default wraps [`Model::chat_turn`]: `cancel` is checked once
    /// before the single blocking call starts (never during it — a model
    /// with no real streaming override has nothing to interrupt mid-flight),
    /// and the entire reply is handed to `on_text_delta` in one call if it
    /// was text. This keeps ACP/Stub/SetupNeeded/any future `Model` correct
    /// with zero source changes.
    fn chat_turn_streaming(
        &self,
        system: &str,
        messages: &[ChatMsg],
        tools: &[ToolDecl],
        on_text_delta: &mut dyn FnMut(&str),
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<StreamEnd, String> {
        if cancel.load(std::sync::atomic::Ordering::Acquire) {
            return Ok(StreamEnd::Stopped { partial_text: String::new(), reason: StopReason::Cancelled });
        }
        let reply = self.chat_turn(system, messages, tools)?;
        if let ChatReply::Text(ref s) = reply {
            if !s.is_empty() {
                on_text_delta(s);
            }
        }
        Ok(StreamEnd::Completed(reply))
    }

    /// Whether [`Model::chat_turn_streaming`] gives real incremental deltas
    /// for this model, or just the default's one-shot wrap. Mirrors
    /// [`Model::supports_chat`]'s shape — a capability query, never guessed
    /// by the caller.
    fn supports_streaming(&self) -> bool {
        false
    }

    /// Revoke this resolved model instance after its seat is reassigned.
    /// Most HTTP clients are stateless and need no action; persistent model
    /// transports use this to prevent a stale `Arc<dyn Model>` from opening a
    /// new session after it has been removed from the seat cache.
    fn revoke(&self) {}

    /// Best-effort correlation hint (07d-A5 review #5), called once by a
    /// caller right before it starts a turn. Every HTTP-shaped transport's
    /// gate/tool-call bookkeeping happens synchronously inside the same
    /// `chat`/`chat_turn`/`chat_turn_streaming` call that already receives
    /// `turn_id` some other way, so the default is a no-op; only a
    /// transport whose callbacks run on a long-lived background worker
    /// outside that call stack (ACP's boxed CLI child, whose gate requests
    /// arrive on a session thread with no other way to learn which turn is
    /// current) needs to override this.
    fn set_current_turn_id(&self, _turn_id: &str) {}

    /// Per-turn reasoning-effort override (07d-A3D1 Phase C), called
    /// unconditionally on every turn — `Some` to request a specific effort
    /// level for this call only, `None` to run at the model's own default.
    /// Called every time (never only when an override is present) so the
    /// interior side channel backing this on a real provider can never go
    /// stale across turns on a shared, cached `Arc<dyn Model>` instance.
    /// Providers with no effort concept for the currently-configured model
    /// (a plug, or a Gemini model outside the 3.x family) silently no-op —
    /// unlike [`Model::with_overridden_model_id`], an unhonored effort
    /// override is a graceful degradation (the same model, at its normal
    /// default reasoning behavior), never a trust problem the caller must
    /// be warned about.
    fn set_effort(&self, _effort: Option<&str>) {}

    /// Ensure a persistent/native runtime has completed initialization and
    /// session setup. Stateless HTTP providers are already ready and use the
    /// default no-op. Account capability routes call this before projecting
    /// session-advertised controls.
    fn ensure_runtime_ready(&self) -> Result<(), String> {
        Ok(())
    }

    /// Apply a provider-native operating mode and wait for acknowledgement.
    /// The value is Bakery's durable composer mode id; adapters map only the
    /// modes they actually advertise and reject unsupported values.
    fn set_native_operating_mode(&self, _mode: Option<&str>) -> Result<(), String> {
        Ok(())
    }

    /// Replace this call's native attachment input. Called unconditionally
    /// before every native turn so a cached model can never reuse prior bytes.
    fn set_native_attachments(&self, attachments: Vec<NativeAttachment>) -> Result<(), String> {
        if attachments.is_empty() {
            Ok(())
        } else {
            Err(format!("model {} does not accept native attachments", self.name()))
        }
    }

    /// Media types this HTTP model reads natively when a user message carries
    /// them in [`ChatMsg::attachments`] — images, and PDFs where the provider
    /// takes documents. Every other file still reaches the model, as text
    /// (inline, or a note naming it); none is refused. Empty by default, so
    /// a provider that has not declared any is never sent a file it may not
    /// read.
    fn native_attachment_types(&self) -> &'static [&'static str] {
        &[]
    }

    /// Builds a fresh, uncached model instance for a specific catalog
    /// variant within this same provider, for one call only (07d-A3D1 Phase
    /// C) — e.g. picking a different Claude model id than the seat's
    /// persisted assignment. Returns `None` when this transport has no
    /// per-turn variant concept (ACP plugs, openai-compat plugs, Claude
    /// Code, Stub/SetupNeeded) — the caller must fail the turn clearly in
    /// that case rather than silently ignoring the override, since running
    /// the wrong model with no signal is a trust problem an unsupported
    /// effort override is not (see [`Model::set_effort`]). Never a
    /// cross-provider swap: only ever a different id within whichever
    /// provider the seat is already assigned to.
    fn with_overridden_model_id(&self, _model_id: &str) -> Option<Arc<dyn Model>> {
        None
    }

    /// This model's own report of the token usage for whichever
    /// `chat_turn`/`chat_turn_streaming` call just completed (07d-A3D1
    /// Phase C) — `None` when the provider's response didn't report it, or
    /// when this transport doesn't track usage at all (Stub, SetupNeeded).
    /// Read once by the caller immediately after the call returns; safe
    /// against a shared cached instance because at most one turn is ever in
    /// flight per agent at a time.
    fn last_turn_usage(&self) -> Option<TurnUsage> {
        None
    }

    /// The rest of that same report — what [`Model::last_turn_usage`]'s two
    /// counts leave out. `None` for every transport that reports no more
    /// (all but ACP today). Read at the same moment, on the same terms.
    fn last_turn_usage_detail(&self) -> Option<TurnUsageDetail> {
        None
    }

    /// What the model thought during that same call, when it did (W1-T):
    /// for the operator's view only, never fed back to a model. Read at the
    /// same moment, on the same terms.
    fn last_turn_thinking(&self) -> Option<Thinking> {
        None
    }

    /// The closed transport set a caller uses to decide dispatch (07d-A3B):
    /// `Http` (the default — Grok/Claude/Gemini/openai-compat all speak the
    /// same `chat_turn()` native-tool contract), or `Acp` for a boxed CLI
    /// child, which already runs its own native tool loop and must never be
    /// wrapped in Bakery's HTTP tool table.
    fn transport(&self) -> Transport {
        Transport::Http
    }

    /// This provider's live model catalog — models, context windows, effort
    /// levels — read straight from its own API (07d-A3D1 Phase B). Node owns
    /// caching/refresh (`bakery_node::model_catalog`); a caller here must
    /// never be called on every picker render — one call per cache miss or
    /// background refresh, nothing more.
    ///
    /// The default below is what every `Model` with nothing to enumerate
    /// falls back to unless it overrides this — a subscription CLI, an ACP
    /// transport, the offline stub, or any private/operator-local impl this
    /// public tree can't edit (e.g. Claude Code): one entry named after
    /// [`Model::name`], reported `Unavailable`. Never a guessed list.
    fn catalog(&self) -> Result<ModelCatalog, String> {
        Ok(ModelCatalog::Unavailable {
            default: ModelCatalogEntry {
                id: self.name().to_string(),
                label: self.name().to_string(),
                context_window: None,
                effort_levels: Vec::new(),
            },
            reason: "this model does not report a live catalog".to_string(),
        })
    }
}

/// One selectable model within a provider's catalog — populated only from
/// that provider's own live list-models response, never fabricated.
#[derive(schemars::JsonSchema, Debug, Clone, Serialize, Deserialize)]
pub struct ModelCatalogEntry {
    pub id: String,
    pub label: String,
    /// Tokens, when the provider's own response reports one — `None` when it
    /// doesn't (never backfilled from a static table).
    pub context_window: Option<u64>,
    /// Empty when this specific model has no reasoning-effort concept.
    pub effort_levels: Vec<EffortLevel>,
}

/// One selectable reasoning-effort level. Ids are provider-specific (e.g.
/// Anthropic/xAI: `low`/`medium`/`high`/`xhigh`/`max`) — never a shared
/// cross-provider enum, since the set differs per provider and per model.
#[derive(schemars::JsonSchema, Debug, Clone, Serialize, Deserialize)]
pub struct EffortLevel {
    pub id: String,
    pub label: String,
}

/// A provider's model catalog, as [`Model::catalog`] reports it. `Available`
/// only ever holds models/limits/effort levels the provider's own live API
/// actually returned. `Unavailable` covers a subscription CLI or any
/// provider with nothing to enumerate — `default` is that model's own one
/// known id, never a guess at what else might exist.
///
/// `#[serde(tag = "status", ...)]` gives the client a clean discriminated
/// union (`{"status":"available","models":[...]}` /
/// `{"status":"unavailable","default":{...},"reason":"..."}`) rather than an
/// awkward tuple encoding.
#[derive(schemars::JsonSchema, Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ModelCatalog {
    Available { models: Vec<ModelCatalogEntry> },
    Unavailable { default: ModelCatalogEntry, reason: String },
}

/// See [`Model::transport`]. A plain enum, not a bare string: a dispatch
/// `match` on this is exhaustive at compile time, so a future third
/// transport is a compiler error at every call site until it's explicitly
/// handled, never a typo'd/unrecognized value that silently falls through
/// to the HTTP tool loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    Http,
    Acp,
}

pub struct StubModel;

impl Model for StubModel {
    fn name(&self) -> &str {
        "stub"
    }

    fn supports_chat(&self) -> bool {
        true
    }

    /// Offline script so the tool loop is click-testable with no credentials:
    /// list the worktree once, then finish. Native tool calls, same as HTTP.
    fn chat(&self, _system: &str, messages: &[ChatMsg]) -> Result<String, String> {
        match self.chat_turn(_system, messages, &[])? {
            ChatReply::Text(s) => Ok(s),
            ChatReply::ToolCalls(calls) => Ok(format!(
                "{{\"tool\":{},\"args\":{}}}",
                serde_json::to_string(&calls[0].name).unwrap_or_else(|_| "\"list_dir\"".into()),
                calls.first().map(|c| c.arguments.to_string()).unwrap_or_else(|| "{}".into())
            )),
        }
    }

    fn chat_turn(
        &self,
        _system: &str,
        messages: &[ChatMsg],
        _tools: &[ToolDecl],
    ) -> Result<ChatReply, String> {
        let saw_tool_result = messages.iter().any(|m| {
            m.role == ChatRole::Tool
                || (m.role == ChatRole::User && m.content.starts_with("TOOL RESULT"))
        });
        if saw_tool_result {
            Ok(ChatReply::Text(
                "stub agent: inspected the worktree listing and stopped (no edit proposed)".into(),
            ))
        } else {
            Ok(ChatReply::ToolCalls(vec![ToolCall {
                id: "stub_1".into(),
                name: "list_dir".into(),
                arguments: serde_json::json!({ "path": "." }),
            }]))
        }
    }

    fn propose_edit(&self, req: &EditRequest) -> Result<ProposedEdit, String> {
        if req.file_path.trim().is_empty() {
            return Err("file_path required".to_string());
        }
        let marker = format!("// bakery stub agent: {}\n", req.instruction.trim());
        let new_content = if req.current_content.is_empty() {
            format!("// created by bakery stub agent\n{marker}")
        } else {
            let sep = if req.current_content.ends_with('\n') {
                ""
            } else {
                "\n"
            };
            format!("{}{}{}", req.current_content, sep, marker)
        };
        Ok(ProposedEdit {
            file_path: req.file_path.clone(),
            new_content,
            rationale: format!(
                "stub: appended a marker line for instruction {:?}",
                req.instruction.trim()
            ),
        })
    }
}

/// What a production node hands back instead of [`StubModel`] when nothing
/// resolves — honestly refuses every call with the real reason, rather than
/// faking an edit (06-SEATS audit item 1: "No product StubModel... no creds
/// and no local model → Setup error"). `supports_chat` is `true` so the tool
/// loop's "model does not support the tool loop" message never masks the
/// actual Setup reason carried here.
pub struct SetupNeededModel(pub String);

impl Model for SetupNeededModel {
    fn name(&self) -> &str {
        "setup-needed"
    }

    fn supports_chat(&self) -> bool {
        true
    }

    fn chat(&self, _system: &str, _messages: &[ChatMsg]) -> Result<String, String> {
        Err(self.0.clone())
    }

    fn propose_edit(&self, _req: &EditRequest) -> Result<ProposedEdit, String> {
        Err(self.0.clone())
    }
}

/// Resolve model from secrets store `active_model`, else `BAKERY_MODEL`, else auto.
///
/// Delegates to [`resolve_seat_model`] for every choice except `claude-code`
/// (its own local-only OAuth path) — the same fail-closed provider
/// resolution the per-seat path uses, so this legacy node-global surface
/// (`/auth/*`, `/policy/write-mode`'s node startup) gets the identical
/// guarantee: no credentials and no local model wraps the real reason in
/// [`SetupNeededModel`], never [`StubModel`] (06-SEATS audit item 1 —
/// "node startup / missing Claude Code path must not return stub"). Kept
/// infallible (unlike `resolve_seat_model`) because this is called from
/// `main()` at boot: the node must always come up with *some* `Model`, even
/// with zero configured credentials, and the first real interaction is
/// where the Setup message actually surfaces.
pub fn resolve_model() -> Arc<dyn Model> {
    let secrets = secrets::load();
    let choice = secrets
        .active_model
        .clone()
        .or_else(|| std::env::var("BAKERY_MODEL").ok())
        .unwrap_or_else(|| "auto".into())
        .to_ascii_lowercase();

    if matches!(choice.as_str(), "claude-code" | "claude_code" | "claudecode") {
        return resolve_claude_code();
    }
    resolve_seat_model(&choice).unwrap_or_else(|e| {
        eprintln!("bakery-agent: {e}");
        Arc::new(SetupNeededModel(e))
    })
}

/// Resolve a **seat's** configured model id to a live client (06-SEATS.md).
/// Unlike [`resolve_model`]'s `auto` arm, this never substitutes `StubModel`
/// when nothing is configured: a seat with no credentials and no local model
/// must fail the turn with a Setup-shaped message, not silently run a fake
/// edit loop (STEERING.md § Models: "`auto` never silently becomes stub so
/// the studio 'works' as a fake"). `"stub"` itself is accepted only under
/// `#[cfg(test)]` — a production build has no arm that can ever resolve it,
/// even if a Heart event somehow named it.
pub fn resolve_seat_model(model_id: &str) -> Result<Arc<dyn Model>, String> {
    let choice = model_id.trim().to_ascii_lowercase();
    match choice.as_str() {
        #[cfg(test)]
        "stub" => Ok(Arc::new(StubModel)),
        "claude" => {
            if ClaudeModel::credentials_available() {
                Ok(Arc::new(ClaudeModel::from_env()))
            } else {
                Err(setup_needed("claude"))
            }
        }
        "claude-code" | "claude_code" | "claudecode" => {
            if !claude_code_slot::is_built() {
                return Err(
                    "Setup needed: Claude Code login is not built on this node".into(),
                );
            }
            if claude_code_slot::credentials_available() {
                #[cfg(has_claude_code_oauth)]
                {
                    return Ok(Arc::new(claude_code_oauth::ClaudeCodeModel::from_env()));
                }
                #[cfg(not(has_claude_code_oauth))]
                {
                    return Err("Setup needed: Claude Code login is not built on this node".into());
                }
            }
            Err(setup_needed("claude-code"))
        }
        "grok" | "xai" => {
            if GrokModel::credentials_available() {
                Ok(Arc::new(GrokModel::from_env()))
            } else {
                Err(setup_needed("grok"))
            }
        }
        "gemini" | "google" => {
            if GeminiModel::credentials_available() {
                Ok(Arc::new(GeminiModel::from_env()))
            } else {
                Err(setup_needed("gemini"))
            }
        }
        "auto" => {
            if GrokModel::credentials_available() {
                Ok(Arc::new(GrokModel::from_env()))
            } else if GeminiModel::credentials_available() {
                Ok(Arc::new(GeminiModel::from_env()))
            } else if ClaudeModel::credentials_available() {
                Ok(Arc::new(ClaudeModel::from_env()))
            } else {
                Err(setup_needed("auto"))
            }
        }
        other => Err(format!(
            "Setup: no client for model id {other:?} — pick grok, claude, gemini, or a configured local model id"
        )),
    }
}

fn setup_needed(wanted: &str) -> String {
    format!("Setup needed: no credentials for {wanted} and no local model configured — open Setup to sign in")
}

#[cfg(has_claude_code_oauth)]
fn resolve_claude_code() -> Arc<dyn Model> {
    let _ = claude_code_oauth::ClaudeCodeModel::credentials_available();
    eprintln!("bakery-agent: local-only Claude Code OAuth (BAKERY_MODEL=claude-code)");
    Arc::new(claude_code_oauth::ClaudeCodeModel::from_env())
}

#[cfg(not(has_claude_code_oauth))]
fn resolve_claude_code() -> Arc<dyn Model> {
    let msg = "Setup needed: local Claude Code OAuth support is not built into this node — \
               sign in with an API key (grok, claude, or gemini) instead"
        .to_string();
    eprintln!("bakery-agent: {msg}");
    Arc::new(SetupNeededModel(msg))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(instr: &str, path: &str, cur: &str) -> EditRequest {
        EditRequest {
            instruction: instr.to_string(),
            file_path: path.to_string(),
            current_content: cur.to_string(),
        }
    }

    #[test]
    fn stub_works() {
        let a = StubModel
            .propose_edit(&req("hi", "a.txt", "line\n"))
            .unwrap();
        assert!(a.new_content.contains("bakery stub agent"));
    }

    /// 06-SEATS audit item 1: the legacy node-global `resolve_model()` must
    /// not fake an edit when nothing resolves either — it delegates to
    /// `resolve_seat_model` and wraps a genuine failure in
    /// `SetupNeededModel`, never `StubModel`. Isolated via
    /// `secrets::test_support` (06b-PIPE.md: "Do not: Live secrets.json in
    /// tests") rather than branching on whatever this developer machine
    /// happens to have configured — also scrubs `BAKERY_MODEL` since
    /// `resolve_model` (unlike `resolve_seat_model`) reads it directly.
    #[test]
    fn resolve_model_never_fakes_an_edit_when_nothing_resolves() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        secrets::test_support::with_isolated_credentials(&path, || {
            let prev = std::env::var("BAKERY_MODEL").ok();
            std::env::remove_var("BAKERY_MODEL");

            let m = resolve_model();
            assert_eq!(m.name(), "setup-needed", "no credentials configured must fail closed, never stub");
            let err = m.propose_edit(&req("hi", "a.txt", "")).map(|_| ()).unwrap_err();
            assert!(err.to_ascii_lowercase().contains("setup") || err.to_ascii_lowercase().contains("credentials"), "{err}");

            match prev {
                Some(v) => std::env::set_var("BAKERY_MODEL", v),
                None => std::env::remove_var("BAKERY_MODEL"),
            }
        });
    }

    #[test]
    fn resolve_seat_model_accepts_stub_only_under_cfg_test() {
        // This arm only exists to compile at all inside `#[cfg(test)]` — its
        // presence here IS the assertion that a production build has none.
        assert_eq!(resolve_seat_model("stub").unwrap().name(), "stub");
        assert_eq!(resolve_seat_model("STUB").unwrap().name(), "stub", "case-insensitive");
    }

    #[test]
    fn resolve_seat_model_refuses_an_unrecognized_id_regardless_of_environment() {
        let err = resolve_seat_model("some-local-model-nobody-configured").map(|_| ()).unwrap_err();
        assert!(err.contains("no client for model id"), "{err}");
    }

    /// Isolated secrets state (`secrets::test_support::with_isolated_credentials`)
    /// so this is deterministic regardless of what this developer machine
    /// actually has configured — replaces the old self-consistent-with-
    /// ambient-environment shape (06b-PIPE.md: "Do not: Live secrets.json in
    /// tests"). Gemini's `credentials_available()` also checks the real
    /// `~/.gemini/oauth_creds.json` for existence, which isn't isolated here
    /// (that's a pre-existing local file, not `secrets.json`, and CLAUDE.md
    /// forbids this agent from reading or redirecting it) — its own test
    /// below tolerates that one residual, self-consistent branch.
    #[test]
    fn resolve_seat_model_claude_fails_closed_without_credentials_never_stub() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        secrets::test_support::with_isolated_credentials(&path, || {
            let err = resolve_seat_model("claude").map(|_| ()).unwrap_err();
            assert!(err.contains("Setup needed"), "{err}");
            assert!(!err.to_ascii_lowercase().contains("stub"), "{err}");
        });
    }

    #[test]
    fn claude_code_id_is_setup_without_login_and_auto_never_selects_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        secrets::test_support::with_isolated_credentials(&path, || {
            let err = resolve_seat_model("claude-code").map(|_| ()).unwrap_err();
            assert!(err.to_ascii_lowercase().contains("setup"), "{err}");
            assert!(!err.to_ascii_lowercase().contains("stub"), "{err}");
            let auto = resolve_seat_model("auto").map(|_| ()).unwrap_err();
            assert!(auto.contains("Setup needed"), "{auto}");
            assert!(!auto.to_ascii_lowercase().contains("claude-code"), "{auto}");
        });
    }

    #[test]
    fn resolve_seat_model_claude_succeeds_once_an_isolated_api_key_is_configured() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        secrets::test_support::with_isolated_credentials(&path, || {
            let mut s = secrets::SecretsFile::default();
            s.anthropic_api_key = Some("test-only-api-key-not-a-real-credential".into());
            secrets::save_to(&path, &s).unwrap();
            assert!(ClaudeModel::credentials_available());
            assert_eq!(resolve_seat_model("claude").unwrap().name(), "claude");
        });
    }

    #[test]
    fn resolve_seat_model_grok_fails_closed_without_credentials_never_stub() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        secrets::test_support::with_isolated_credentials(&path, || {
            let err = resolve_seat_model("grok").map(|_| ()).unwrap_err();
            assert!(err.contains("Setup needed"), "{err}");
            assert!(!err.to_ascii_lowercase().contains("stub"), "{err}");
        });
    }

    #[test]
    fn resolve_seat_model_grok_succeeds_once_an_isolated_api_key_is_configured() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        secrets::test_support::with_isolated_credentials(&path, || {
            let mut s = secrets::SecretsFile::default();
            s.xai_api_key = Some("test-only-api-key-not-a-real-credential".into());
            secrets::save_to(&path, &s).unwrap();
            assert!(GrokModel::credentials_available());
            assert_eq!(resolve_seat_model("grok").unwrap().name(), "grok");
        });
    }

    /// Gemini's `credentials_available()` also checks whether
    /// `~/.gemini/oauth_creds.json` exists on this real machine — a
    /// pre-existing local file this agent must not read or redirect
    /// (CLAUDE.md), so unlike the Claude/Grok siblings this one test keeps a
    /// narrow self-consistent branch for that one signal while everything
    /// else (secrets.json, the API-key env vars) is fully isolated.
    #[test]
    fn resolve_seat_model_gemini_fails_closed_without_credentials_never_stub() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        secrets::test_support::with_isolated_credentials(&path, || {
            let result = resolve_seat_model("gemini");
            if GeminiModel::credentials_available() {
                assert_eq!(result.unwrap().name(), "gemini");
            } else {
                let err = result.map(|_| ()).unwrap_err();
                assert!(err.contains("Setup needed"), "{err}");
                assert!(!err.to_ascii_lowercase().contains("stub"), "{err}");
            }
        });
    }

    /// The one behavioral difference from `resolve_model`'s `auto` arm: no
    /// credentials anywhere must fail the turn, never quietly hand back
    /// `StubModel` "so the studio works" (STEERING.md § Models). Isolated
    /// the same way as the Claude/Grok tests above; tolerates the same
    /// narrow Gemini-CLI-file residual.
    #[test]
    fn resolve_seat_model_auto_fails_closed_without_any_credentials_never_stub() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        secrets::test_support::with_isolated_credentials(&path, || {
            let result = resolve_seat_model("auto");
            if GeminiModel::credentials_available() {
                assert!(result.is_ok());
            } else {
                let err = result.map(|_| ()).unwrap_err();
                assert!(err.contains("Setup needed"), "{err}");
                assert!(!err.to_ascii_lowercase().contains("stub"), "{err}");
            }
        });
    }

    #[test]
    fn resolve_seat_model_auto_prefers_grok_when_multiple_are_configured() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        secrets::test_support::with_isolated_credentials(&path, || {
            let mut s = secrets::SecretsFile::default();
            s.xai_api_key = Some("test-only-api-key-not-a-real-credential".into());
            s.anthropic_api_key = Some("test-only-api-key-not-a-real-credential".into());
            secrets::save_to(&path, &s).unwrap();
            assert_eq!(resolve_seat_model("auto").unwrap().name(), "grok");
        });
    }
}
