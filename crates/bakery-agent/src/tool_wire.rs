//! Native tool-call wire helpers for HTTP providers.
//!
//! OpenAI-shaped (Grok, openai-compat), Anthropic Messages, and Gemini
//! function-calling all map onto the same [`ToolDecl`] / [`ToolCall`] /
//! [`ChatReply`] types. The JSON-in-text toy loop is not this module.

use crate::sse::{DriveControl, SseEvent};
use crate::{ChatMsg, ChatReply, ChatRole, ToolCall, ToolDecl, TurnUsage};
use serde_json::{json, Value};

pub fn openai_tools(tools: &[ToolDecl]) -> Value {
    Value::Array(tools.iter().map(ToolDecl::openai_tool).collect())
}

pub fn openai_messages(system: &str, messages: &[ChatMsg]) -> Vec<Value> {
    let mut msgs: Vec<Value> = vec![json!({ "role": "system", "content": system })];
    for m in messages {
        msgs.push(openai_message(m));
    }
    msgs
}

/// Standard base64 (with padding), for the bytes of a file sent in a request.
pub fn encode_base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let a = chunk[0] as u32;
        let b = chunk.get(1).copied().unwrap_or(0) as u32;
        let c = chunk.get(2).copied().unwrap_or(0) as u32;
        let word = (a << 16) | (b << 8) | c;
        out.push(ALPHABET[((word >> 18) & 63) as usize] as char);
        out.push(ALPHABET[((word >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[((word >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[(word & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

fn data_url(a: &crate::NativeAttachment) -> String {
    format!("data:{};base64,{}", a.mime_type, encode_base64(&a.data))
}

/// The same conversation with every attached file taken out and named in
/// its message's text instead — for a retry when a model rejects files it
/// cannot read (a text-only local model behind an OpenAI-compatible API).
pub fn without_attachments(messages: &[ChatMsg]) -> Vec<ChatMsg> {
    messages
        .iter()
        .map(|m| {
            let mut m = m.clone();
            if !m.attachments.is_empty() {
                let names: Vec<&str> = m.attachments.iter().map(|a| a.filename.as_str()).collect();
                m.content.push_str(&format!(
                    "\n\n[Not sent: {} — this model rejected file input. Tell the operator if you need what they show described another way.]",
                    names.join(", ")
                ));
                m.attachments.clear();
            }
            m
        })
        .collect()
}

fn openai_message(m: &ChatMsg) -> Value {
    if !m.attachments.is_empty() {
        let mut parts: Vec<Value> = Vec::new();
        if !m.content.is_empty() {
            parts.push(json!({ "type": "text", "text": m.content }));
        }
        for a in &m.attachments {
            parts.push(if a.mime_type.starts_with("image/") {
                json!({ "type": "image_url", "image_url": { "url": data_url(a) } })
            } else {
                json!({ "type": "file", "file": { "filename": a.filename, "file_data": data_url(a) } })
            });
        }
        return json!({ "role": m.role_str(), "content": parts });
    }
    match m.role {
        ChatRole::Tool => {
            let mut v = json!({
                "role": "tool",
                "content": m.content,
            });
            if let Some(id) = &m.tool_call_id {
                v["tool_call_id"] = json!(id);
            }
            if let Some(name) = &m.tool_name {
                v["name"] = json!(name);
            }
            v
        }
        ChatRole::Assistant if !m.tool_calls.is_empty() => {
            let tool_calls: Vec<Value> = m
                .tool_calls
                .iter()
                .map(|c| {
                    json!({
                        "id": c.id,
                        "type": "function",
                        "function": {
                            "name": c.name,
                            "arguments": c.arguments.to_string(),
                        }
                    })
                })
                .collect();
            let content = if m.content.is_empty() {
                Value::Null
            } else {
                json!(m.content)
            };
            json!({
                "role": "assistant",
                "content": content,
                "tool_calls": tool_calls,
            })
        }
        _ => json!({ "role": m.role_str(), "content": m.content }),
    }
}

pub fn extract_openai_reply(response_json: &str) -> Result<ChatReply, String> {
    let v: Value = serde_json::from_str(response_json).map_err(|e| format!("response json: {e}"))?;
    let message = v
        .pointer("/choices/0/message")
        .ok_or_else(|| "response missing choices[0].message".to_string())?;
    if let Some(calls) = message.get("tool_calls").and_then(|c| c.as_array()) {
        if !calls.is_empty() {
            let mut out = Vec::new();
            for (i, c) in calls.iter().enumerate() {
                let id = c
                    .get("id")
                    .and_then(|x| x.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| format!("call_{i}"));
                let name = c
                    .pointer("/function/name")
                    .and_then(|x| x.as_str())
                    .ok_or_else(|| "tool_call missing function.name".to_string())?
                    .to_string();
                let args_raw = c
                    .pointer("/function/arguments")
                    .and_then(|x| x.as_str())
                    .unwrap_or("{}");
                let arguments: Value = serde_json::from_str(args_raw).unwrap_or_else(|_| json!({}));
                out.push(ToolCall { id, name, arguments });
            }
            return Ok(ChatReply::ToolCalls(out));
        }
    }
    let content = message
        .get("content")
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .to_string();
    Ok(ChatReply::Text(content))
}

/// The OpenAI-shaped `usage` object (`{prompt_tokens, completion_tokens,
/// total_tokens}`) — shared by Grok (xAI) and the generic openai-compat
/// endpoint, the same wire shape [`extract_openai_reply`] already assumes
/// (07d-A3D1 Phase C). A field absent from the response stays `None`,
/// never fabricated; malformed JSON is `TurnUsage::default()`, never an
/// `Err` — a missing/unparseable usage object must never fail an otherwise
/// successful turn.
pub fn extract_openai_usage(response_json: &str) -> TurnUsage {
    let Ok(v) = serde_json::from_str::<Value>(response_json) else { return TurnUsage::default() };
    TurnUsage {
        tokens_in: v.pointer("/usage/prompt_tokens").and_then(|n| n.as_u64()),
        tokens_out: v.pointer("/usage/completion_tokens").and_then(|n| n.as_u64()),
    }
}

pub fn anthropic_tools(tools: &[ToolDecl]) -> Value {
    Value::Array(
        tools
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.parameters,
                })
            })
            .collect(),
    )
}

pub fn anthropic_messages(messages: &[ChatMsg]) -> Vec<Value> {
    messages.iter().map(anthropic_message).collect()
}

fn anthropic_message(m: &ChatMsg) -> Value {
    if !m.attachments.is_empty() {
        // Files first, then the text: Claude reads images best before the
        // words about them.
        let mut content: Vec<Value> = m
            .attachments
            .iter()
            .map(|a| {
                let kind = if a.mime_type == "application/pdf" { "document" } else { "image" };
                json!({
                    "type": kind,
                    "source": { "type": "base64", "media_type": a.mime_type, "data": encode_base64(&a.data) },
                })
            })
            .collect();
        if !m.content.is_empty() {
            content.push(json!({ "type": "text", "text": m.content }));
        }
        return json!({ "role": m.role_str(), "content": content });
    }
    match m.role {
        ChatRole::Tool => {
            json!({
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": m.tool_call_id.as_deref().unwrap_or(""),
                    "content": m.content,
                }]
            })
        }
        ChatRole::Assistant if !m.tool_calls.is_empty() => {
            let mut content: Vec<Value> = Vec::new();
            if !m.content.is_empty() {
                content.push(json!({ "type": "text", "text": m.content }));
            }
            for c in &m.tool_calls {
                content.push(json!({
                    "type": "tool_use",
                    "id": c.id,
                    "name": c.name,
                    "input": c.arguments,
                }));
            }
            json!({ "role": "assistant", "content": content })
        }
        _ => json!({ "role": m.role_str(), "content": m.content }),
    }
}

pub fn extract_anthropic_reply(response_json: &str) -> Result<ChatReply, String> {
    let v: Value = serde_json::from_str(response_json).map_err(|e| format!("response json: {e}"))?;
    let content = v
        .get("content")
        .and_then(|c| c.as_array())
        .ok_or_else(|| "response missing content array".to_string())?;
    let mut calls = Vec::new();
    let mut texts = Vec::new();
    for (i, block) in content.iter().enumerate() {
        let ty = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
        if ty == "tool_use" {
            let id = block
                .get("id")
                .and_then(|x| x.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("call_{i}"));
            let name = block
                .get("name")
                .and_then(|x| x.as_str())
                .ok_or_else(|| "tool_use missing name".to_string())?
                .to_string();
            let arguments = block.get("input").cloned().unwrap_or_else(|| json!({}));
            calls.push(ToolCall { id, name, arguments });
        } else if ty == "text" {
            if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                texts.push(t.to_string());
            }
        }
    }
    if !calls.is_empty() {
        return Ok(ChatReply::ToolCalls(calls));
    }
    if texts.is_empty() {
        return Err("response had no text or tool_use blocks".into());
    }
    Ok(ChatReply::Text(texts.join("\n")))
}

/// Anthropic's `usage` object (`{input_tokens, output_tokens}`), top-level
/// on a non-streaming Messages API response (07d-A3D1 Phase C). A field
/// absent from the response stays `None`; malformed JSON is
/// `TurnUsage::default()`, never an `Err`.
pub fn extract_anthropic_usage(response_json: &str) -> TurnUsage {
    let Ok(v) = serde_json::from_str::<Value>(response_json) else { return TurnUsage::default() };
    TurnUsage {
        tokens_in: v.pointer("/usage/input_tokens").and_then(|n| n.as_u64()),
        tokens_out: v.pointer("/usage/output_tokens").and_then(|n| n.as_u64()),
    }
}

/// Reconstructs a full [`ChatReply`] from Anthropic's streamed
/// `content_block_start`/`_delta`/`_stop` events (07d-A5) — mirrors
/// [`extract_anthropic_reply`]'s exact field shape and its silent skip of
/// any content-block type other than `"text"`/`"tool_use"` (so streaming
/// and non-streaming produce byte-identical replies for the same
/// conversation). Blocks stream strictly sequentially per Anthropic's own
/// protocol (one `content_block_start`/`_stop` pair open at a time, never
/// interleaved), so a single "current block" slot is enough — no per-index
/// map needed.
#[derive(Default)]
pub struct AnthropicStreamAssembler {
    texts: Vec<String>,
    tool_calls: Vec<ToolCall>,
    current: AnthropicBlock,
    /// Its `thinking` blocks' text, kept apart (W1-T); empty for
    /// `redacted_thinking`.
    thinking: Option<crate::Thinking>,
    /// Captured from `message_start.message.usage.input_tokens` (07d-A3D1
    /// Phase C) — the real, full prompt count, already present at the very
    /// start of the stream per Anthropic's own docs.
    tokens_in: Option<u64>,
    /// Captured from `message_delta.usage.output_tokens`, which Anthropic's
    /// docs state is cumulative — each occurrence overwrites, so the last
    /// one observed is authoritative.
    tokens_out: Option<u64>,
}

#[derive(Default)]
enum AnthropicBlock {
    #[default]
    None,
    Text(String),
    ToolUse { id: String, name: String, json: String },
    /// `"thinking"`/`"redacted_thinking"`/anything else — tracked only so
    /// `content_block_stop` knows there's nothing to finalize; its deltas
    /// (`thinking_delta`/`signature_delta`) are never inspected.
    Other,
}

impl AnthropicStreamAssembler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one parsed SSE event — returns [`DriveControl`] directly for
    /// [`crate::sse::drive`]'s callback contract. `on_text_delta` fires
    /// only for `"text"` content blocks; a `"thinking"`/`"redacted_thinking"`
    /// block's deltas are consumed for bookkeeping only, never forwarded.
    pub fn feed(&mut self, ev: SseEvent, on_text_delta: &mut dyn FnMut(&str)) -> DriveControl {
        let v: Value = match serde_json::from_str(&ev.data) {
            Ok(v) => v,
            Err(e) => return DriveControl::Abort(format!("anthropic stream: malformed event: {e}")),
        };
        let ty = v.get("type").and_then(|t| t.as_str()).unwrap_or_default();
        match ty {
            "content_block_start" => {
                let block = v.get("content_block").cloned().unwrap_or_else(|| json!({}));
                let block_ty = block.get("type").and_then(|t| t.as_str()).unwrap_or_default();
                self.current = match block_ty {
                    "text" => AnthropicBlock::Text(
                        block.get("text").and_then(|t| t.as_str()).unwrap_or_default().to_string(),
                    ),
                    "tool_use" => AnthropicBlock::ToolUse {
                        id: block.get("id").and_then(|x| x.as_str()).unwrap_or_default().to_string(),
                        name: block.get("name").and_then(|x| x.as_str()).unwrap_or_default().to_string(),
                        json: String::new(),
                    },
                    "thinking" | "redacted_thinking" => {
                        self.thinking.get_or_insert_with(Default::default);
                        AnthropicBlock::Other
                    }
                    _ => AnthropicBlock::Other,
                };
                DriveControl::Continue
            }
            "content_block_delta" => {
                let delta = v.get("delta").cloned().unwrap_or_else(|| json!({}));
                let delta_ty = delta.get("type").and_then(|t| t.as_str()).unwrap_or_default();
                match (&mut self.current, delta_ty) {
                    (AnthropicBlock::Text(buf), "text_delta") => {
                        if let Some(t) = delta.get("text").and_then(|x| x.as_str()) {
                            buf.push_str(t);
                            on_text_delta(t);
                        }
                    }
                    (AnthropicBlock::ToolUse { json, .. }, "input_json_delta") => {
                        if let Some(p) = delta.get("partial_json").and_then(|x| x.as_str()) {
                            json.push_str(p);
                        }
                    }
                    // Kept apart for the operator (W1-T), never forwarded.
                    (AnthropicBlock::Other, "thinking_delta") => {
                        if let Some(t) = delta.get("thinking").and_then(|x| x.as_str()) {
                            self.thinking.get_or_insert_with(Default::default).text.push_str(t);
                        }
                    }
                    _ => {} // signature_delta/anything on a non-matching block
                }
                DriveControl::Continue
            }
            "content_block_stop" => {
                match std::mem::take(&mut self.current) {
                    AnthropicBlock::Text(buf) => self.texts.push(buf),
                    AnthropicBlock::ToolUse { id, name, json } => {
                        let arguments = if json.trim().is_empty() {
                            Value::Object(Default::default())
                        } else {
                            match serde_json::from_str(&json) {
                                Ok(v) => v,
                                Err(e) => {
                                    return DriveControl::Abort(format!(
                                        "anthropic stream: malformed tool_use input json: {e}"
                                    ))
                                }
                            }
                        };
                        self.tool_calls.push(ToolCall { id, name, arguments });
                    }
                    AnthropicBlock::Other | AnthropicBlock::None => {}
                }
                DriveControl::Continue
            }
            "message_start" => {
                if let Some(n) = v.pointer("/message/usage/input_tokens").and_then(|x| x.as_u64()) {
                    self.tokens_in = Some(n);
                }
                DriveControl::Continue
            }
            "message_delta" => {
                if let Some(n) = v.pointer("/usage/output_tokens").and_then(|x| x.as_u64()) {
                    self.tokens_out = Some(n);
                }
                DriveControl::Continue
            }
            "message_stop" => DriveControl::Stop,
            "error" => {
                let detail = v.get("error").map(|e| e.to_string()).unwrap_or_else(|| ev.data.clone());
                DriveControl::Abort(format!("Anthropic stream error: {detail}"))
            }
            // ping / anything else — no-op.
            _ => DriveControl::Continue,
        }
    }

    /// This turn's token usage as captured so far (07d-A3D1 Phase C) — by
    /// reference, so it can be read right after [`crate::sse::drive`]
    /// returns, before `finish`/`partial_text_so_far` consume or borrow
    /// `self` for their own purposes.
    pub fn usage(&self) -> TurnUsage {
        TurnUsage { tokens_in: self.tokens_in, tokens_out: self.tokens_out }
    }

    /// What it thought, when it did (W1-T): read like [`Self::usage`].
    pub fn thinking(&self) -> Option<crate::Thinking> {
        self.thinking.clone()
    }

    /// Every completed text block, plus whatever text block is still
    /// mid-stream (a cancellation or stream death can land while a text
    /// block is open) — the best-effort preview a [`crate::StreamEnd::
    /// Stopped`] carries.
    pub fn partial_text_so_far(&self) -> String {
        let mut all = self.texts.clone();
        if let AnthropicBlock::Text(buf) = &self.current {
            all.push(buf.clone());
        }
        all.join("\n")
    }

    /// Consumes the assembler once `message_stop` has been observed —
    /// exactly [`extract_anthropic_reply`]'s own "tool calls win, else
    /// joined text, else error" precedence. Folds in a still-open text
    /// block (mirroring [`Self::partial_text_so_far`]): a deviation from
    /// documented Anthropic protocol behavior could in principle deliver
    /// `message_stop` before the matching `content_block_stop` for the
    /// final text block, and that already-streamed text must not be lost
    /// nor mistaken for "no text at all".
    pub fn finish(self) -> Result<ChatReply, String> {
        if !self.tool_calls.is_empty() {
            return Ok(ChatReply::ToolCalls(self.tool_calls));
        }
        let mut texts = self.texts;
        if let AnthropicBlock::Text(buf) = self.current {
            texts.push(buf);
        }
        if texts.is_empty() {
            return Err("response had no text or tool_use blocks".into());
        }
        Ok(ChatReply::Text(texts.join("\n")))
    }
}

pub fn gemini_function_declarations(tools: &[ToolDecl]) -> Value {
    json!([{
        "functionDeclarations": tools.iter().map(|t| json!({
            "name": t.name,
            "description": t.description,
            "parameters": t.parameters,
        })).collect::<Vec<_>>()
    }])
}

pub fn gemini_contents(messages: &[ChatMsg]) -> Vec<Value> {
    messages
        .iter()
        .map(|m| match m.role {
            ChatRole::Assistant if !m.tool_calls.is_empty() => {
                let parts: Vec<Value> = m
                    .tool_calls
                    .iter()
                    .map(|c| {
                        json!({
                            "functionCall": {
                                "name": c.name,
                                "args": c.arguments,
                            }
                        })
                    })
                    .collect();
                json!({ "role": "model", "parts": parts })
            }
            ChatRole::Tool => json!({
                "role": "user",
                "parts": [{
                    "functionResponse": {
                        "name": m.tool_name.as_deref().unwrap_or(""),
                        "response": { "result": m.content },
                    }
                }]
            }),
            ChatRole::Assistant => json!({
                "role": "model",
                "parts": [{ "text": m.content }]
            }),
            ChatRole::User => {
                let mut parts = vec![json!({ "text": m.content })];
                for a in &m.attachments {
                    parts.push(json!({
                        "inlineData": { "mimeType": a.mime_type, "data": encode_base64(&a.data) }
                    }));
                }
                json!({ "role": "user", "parts": parts })
            }
        })
        .collect()
}

pub fn extract_gemini_reply(response_json: &str) -> Result<ChatReply, String> {
    let v: Value = serde_json::from_str(response_json).map_err(|e| format!("json: {e}"))?;
    let parts = v
        .pointer("/candidates/0/content/parts")
        .or_else(|| v.pointer("/response/candidates/0/content/parts"))
        .and_then(|p| p.as_array())
        .ok_or_else(|| "no candidates/parts".to_string())?;
    let mut calls = Vec::new();
    let mut texts = Vec::new();
    for (i, p) in parts.iter().enumerate() {
        if let Some(fc) = p.get("functionCall") {
            let name = fc
                .get("name")
                .and_then(|n| n.as_str())
                .ok_or_else(|| "functionCall missing name".to_string())?
                .to_string();
            let arguments = fc.get("args").cloned().unwrap_or_else(|| json!({}));
            calls.push(ToolCall {
                id: format!("gemini_{i}"),
                name,
                arguments,
            });
        } else if let Some(t) = p.get("text").and_then(|t| t.as_str()) {
            texts.push(t.to_string());
        }
    }
    if !calls.is_empty() {
        return Ok(ChatReply::ToolCalls(calls));
    }
    if texts.is_empty() {
        return Err("no text or functionCall parts".into());
    }
    Ok(ChatReply::Text(texts.join("")))
}

/// Gemini's `usageMetadata` (`{promptTokenCount, candidatesTokenCount, ...}`),
/// with the same `/response/usageMetadata` Code-Assist fallback
/// [`extract_gemini_reply`] already uses (07d-A3D1 Phase C). A field absent
/// from the response stays `None`; malformed JSON is `TurnUsage::default()`,
/// never an `Err`.
pub fn extract_gemini_usage(response_json: &str) -> TurnUsage {
    let Ok(v) = serde_json::from_str::<Value>(response_json) else { return TurnUsage::default() };
    let meta = v.pointer("/usageMetadata").or_else(|| v.pointer("/response/usageMetadata"));
    TurnUsage {
        tokens_in: meta.and_then(|m| m.get("promptTokenCount")).and_then(|n| n.as_u64()),
        tokens_out: meta.and_then(|m| m.get("candidatesTokenCount")).and_then(|n| n.as_u64()),
    }
}

/// Reconstructs a full [`ChatReply`] from an OpenAI-shaped chat-completions
/// stream (07d-A5) — shared by Grok (xAI) and the generic openai-compat
/// endpoint, since both speak the identical wire shape. Each `data:` chunk
/// carries `choices[0].delta`; text arrives in `delta.content`, tool-call
/// arguments arrive fragmented across chunks keyed by `delta.tool_calls[]
/// .index` (the first chunk for a call carries `id`/`function.name`,
/// subsequent chunks repeat the same `index` with those fields `null` and
/// only `function.arguments` growing) — concatenated per index and parsed
/// once at the literal `data: [DONE]` terminator. `delta.reasoning_content`
/// (xAI's reasoning models stream this as a field distinct from `content`)
/// is kept apart as its thinking (W1-T): it never reaches `on_text_delta`
/// or the final text.
#[derive(Default)]
pub struct OpenAiStreamAssembler {
    text: String,
    tool_calls: Vec<PartialOpenAiToolCall>,
    /// `reasoning_content`, or only that it reasoned (its reasoning tokens).
    thinking: Option<crate::Thinking>,
    /// Captured from a top-level `usage` object on any chunk (07d-A3D1
    /// Phase C) — the real usage-bearing chunk has empty `choices` and only
    /// arrives once, typically just before `[DONE]`, so this simply
    /// overwrites whenever seen; there is nothing to accumulate.
    usage: TurnUsage,
}

#[derive(Default, Clone)]
struct PartialOpenAiToolCall {
    id: String,
    name: String,
    arguments: String,
}

impl OpenAiStreamAssembler {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn feed(&mut self, ev: SseEvent, on_text_delta: &mut dyn FnMut(&str)) -> DriveControl {
        if ev.data.trim() == "[DONE]" {
            return DriveControl::Stop;
        }
        let v: Value = match serde_json::from_str(&ev.data) {
            Ok(v) => v,
            Err(e) => return DriveControl::Abort(format!("openai-shaped stream: malformed event: {e}")),
        };
        if let Some(prompt) = v.pointer("/usage/prompt_tokens").and_then(|n| n.as_u64()) {
            self.usage.tokens_in = Some(prompt);
        }
        if let Some(completion) = v.pointer("/usage/completion_tokens").and_then(|n| n.as_u64()) {
            self.usage.tokens_out = Some(completion);
        }
        // A reasoning model that hides its reasoning still counts it.
        if v.pointer("/usage/completion_tokens_details/reasoning_tokens").and_then(|n| n.as_u64()).unwrap_or(0) > 0 {
            self.thinking.get_or_insert_with(Default::default);
        }
        let delta = v.pointer("/choices/0/delta").cloned().unwrap_or_else(|| json!({}));
        if let Some(r) = delta.get("reasoning_content").and_then(|c| c.as_str()).filter(|r| !r.is_empty()) {
            self.thinking.get_or_insert_with(Default::default).text.push_str(r);
        }
        if let Some(t) = delta.get("content").and_then(|c| c.as_str()) {
            if !t.is_empty() {
                self.text.push_str(t);
                on_text_delta(t);
            }
        }
        if let Some(calls) = delta.get("tool_calls").and_then(|c| c.as_array()) {
            for c in calls {
                let index = c.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                while self.tool_calls.len() <= index {
                    self.tool_calls.push(PartialOpenAiToolCall::default());
                }
                let slot = &mut self.tool_calls[index];
                if let Some(id) = c.get("id").and_then(|x| x.as_str()) {
                    slot.id = id.to_string();
                }
                if let Some(name) = c.pointer("/function/name").and_then(|x| x.as_str()) {
                    slot.name = name.to_string();
                }
                if let Some(args) = c.pointer("/function/arguments").and_then(|x| x.as_str()) {
                    slot.arguments.push_str(args);
                }
            }
        }
        DriveControl::Continue
    }

    pub fn partial_text_so_far(&self) -> String {
        self.text.clone()
    }

    /// This turn's token usage as captured so far (07d-A3D1 Phase C).
    pub fn usage(&self) -> TurnUsage {
        self.usage
    }

    /// What it thought, when it did (W1-T): read like [`Self::usage`].
    pub fn thinking(&self) -> Option<crate::Thinking> {
        self.thinking.clone()
    }

    /// Consumed once `[DONE]` has been observed — same "tool calls win,
    /// else the accumulated text" precedence [`extract_openai_reply`]
    /// already uses.
    pub fn finish(self) -> ChatReply {
        if !self.tool_calls.is_empty() {
            let out = self
                .tool_calls
                .into_iter()
                .enumerate()
                .map(|(i, c)| {
                    let id = if c.id.is_empty() { format!("call_{i}") } else { c.id };
                    let arguments: Value = if c.arguments.trim().is_empty() {
                        json!({})
                    } else {
                        serde_json::from_str(&c.arguments).unwrap_or_else(|_| json!({}))
                    };
                    ToolCall { id, name: c.name, arguments }
                })
                .collect();
            return ChatReply::ToolCalls(out);
        }
        ChatReply::Text(self.text)
    }
}

/// Reconstructs a full [`ChatReply`] from Gemini's `streamGenerateContent`
/// (07d-A5) — unlike the other two providers, each `data:` line here is a
/// **complete** `GenerateContentResponse` (the exact shape
/// [`extract_gemini_reply`] already parses), not a delta fragment; this
/// re-runs that same `candidates[0].content.parts` extraction per chunk.
/// There is no completion sentinel — a clean EOF *is* the normal
/// completion signal for this provider (unlike Claude/the OpenAI shape,
/// where an EOF before their own terminator means the connection died).
/// `functionCall` parts arrive whole in a single chunk (confirmed — never
/// fragmented like the other two providers' tool-call arguments), so no
/// partial-JSON accumulation is needed here. A part with `"thought": true`
/// is kept apart as its thinking (W1-T): never forwarded, never in the
/// reply.
#[derive(Default)]
pub struct GeminiStreamAssembler {
    texts: Vec<String>,
    tool_calls: Vec<ToolCall>,
    /// Its thought parts' text, or only that it thought (its thought tokens).
    thinking: Option<crate::Thinking>,
    /// Captured from `usageMetadata` on every chunk (07d-A3D1 Phase C) —
    /// this provider has no completion sentinel, so each occurrence just
    /// overwrites and the last one observed before EOF is authoritative.
    usage: TurnUsage,
}

impl GeminiStreamAssembler {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn feed(&mut self, ev: SseEvent, on_text_delta: &mut dyn FnMut(&str)) -> DriveControl {
        let v: Value = match serde_json::from_str(&ev.data) {
            Ok(v) => v,
            Err(e) => return DriveControl::Abort(format!("gemini stream: malformed event: {e}")),
        };
        if let Some(meta) = v.pointer("/usageMetadata").or_else(|| v.pointer("/response/usageMetadata")) {
            if let Some(n) = meta.get("promptTokenCount").and_then(|x| x.as_u64()) {
                self.usage.tokens_in = Some(n);
            }
            if let Some(n) = meta.get("candidatesTokenCount").and_then(|x| x.as_u64()) {
                self.usage.tokens_out = Some(n);
            }
            if meta.get("thoughtsTokenCount").and_then(|x| x.as_u64()).unwrap_or(0) > 0 {
                self.thinking.get_or_insert_with(Default::default);
            }
        }
        let parts = match v
            .pointer("/candidates/0/content/parts")
            .or_else(|| v.pointer("/response/candidates/0/content/parts"))
            .and_then(|p| p.as_array())
        {
            Some(p) => p,
            // A chunk carrying only e.g. usageMetadata with no parts is valid.
            None => return DriveControl::Continue,
        };
        for (i, p) in parts.iter().enumerate() {
            if p.get("thought").and_then(|t| t.as_bool()).unwrap_or(false) {
                let thinking = self.thinking.get_or_insert_with(Default::default);
                thinking.text.push_str(p.get("text").and_then(|t| t.as_str()).unwrap_or_default());
                continue;
            }
            if let Some(fc) = p.get("functionCall") {
                let Some(name) = fc.get("name").and_then(|n| n.as_str()) else { continue };
                let arguments = fc.get("args").cloned().unwrap_or_else(|| json!({}));
                self.tool_calls.push(ToolCall { id: format!("gemini_{i}"), name: name.to_string(), arguments });
            } else if let Some(t) = p.get("text").and_then(|t| t.as_str()) {
                if !t.is_empty() {
                    self.texts.push(t.to_string());
                    on_text_delta(t);
                }
            }
        }
        DriveControl::Continue
    }

    pub fn partial_text_so_far(&self) -> String {
        self.texts.join("")
    }

    /// This turn's token usage as captured so far (07d-A3D1 Phase C).
    pub fn usage(&self) -> TurnUsage {
        self.usage
    }

    /// What it thought, when it did (W1-T): read like [`Self::usage`].
    pub fn thinking(&self) -> Option<crate::Thinking> {
        self.thinking.clone()
    }

    /// Consumed once the stream has cleanly ended (`DriveOutcome::Eof`) —
    /// same "tool calls win, else the joined text" precedence
    /// [`extract_gemini_reply`] already uses.
    pub fn finish(self) -> Result<ChatReply, String> {
        if !self.tool_calls.is_empty() {
            return Ok(ChatReply::ToolCalls(self.tool_calls));
        }
        if self.texts.is_empty() {
            return Err("no text or functionCall parts".into());
        }
        Ok(ChatReply::Text(self.texts.join("")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_tool_calls_and_text() {
        let tools = extract_openai_reply(
            r#"{"choices":[{"message":{"tool_calls":[{"id":"1","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"a.rs\"}"}}]}}]}"#,
        )
        .unwrap();
        match tools {
            ChatReply::ToolCalls(c) => {
                assert_eq!(c[0].name, "read_file");
                assert_eq!(c[0].arguments["path"], "a.rs");
            }
            ChatReply::Text(_) => panic!("expected tool calls"),
        }
        let text = extract_openai_reply(r#"{"choices":[{"message":{"role":"assistant","content":"hi"}}]}"#).unwrap();
        assert_eq!(text, ChatReply::Text("hi".into()));
    }

    #[test]
    fn extract_openai_usage_reads_well_formed_fields_and_defaults_on_partial_or_malformed() {
        let usage = extract_openai_usage(r#"{"usage":{"prompt_tokens":41,"completion_tokens":2,"total_tokens":43}}"#);
        assert_eq!(usage, TurnUsage { tokens_in: Some(41), tokens_out: Some(2) });

        let partial = extract_openai_usage(r#"{"usage":{"prompt_tokens":10}}"#);
        assert_eq!(partial, TurnUsage { tokens_in: Some(10), tokens_out: None });

        let missing = extract_openai_usage(r#"{"choices":[]}"#);
        assert_eq!(missing, TurnUsage::default());

        let malformed = extract_openai_usage("not json");
        assert_eq!(malformed, TurnUsage::default());
    }

    #[test]
    fn anthropic_tool_use_and_text() {
        let tools = extract_anthropic_reply(
            r#"{"content":[{"type":"tool_use","id":"t1","name":"grep","input":{"pattern":"fn"}}]}"#,
        )
        .unwrap();
        match tools {
            ChatReply::ToolCalls(c) => assert_eq!(c[0].name, "grep"),
            ChatReply::Text(_) => panic!("expected tool calls"),
        }
        let text =
            extract_anthropic_reply(r#"{"content":[{"type":"text","text":"hello"}]}"#).unwrap();
        assert_eq!(text, ChatReply::Text("hello".into()));
    }

    #[test]
    fn extract_anthropic_usage_reads_well_formed_fields_and_defaults_on_partial_or_malformed() {
        let usage = extract_anthropic_usage(r#"{"usage":{"input_tokens":100,"output_tokens":25}}"#);
        assert_eq!(usage, TurnUsage { tokens_in: Some(100), tokens_out: Some(25) });

        let partial = extract_anthropic_usage(r#"{"usage":{"input_tokens":100}}"#);
        assert_eq!(partial, TurnUsage { tokens_in: Some(100), tokens_out: None });

        let missing = extract_anthropic_usage(r#"{"content":[]}"#);
        assert_eq!(missing, TurnUsage::default());

        let malformed = extract_anthropic_usage("not json");
        assert_eq!(malformed, TurnUsage::default());
    }

    #[test]
    fn gemini_function_call_and_text() {
        let tools = extract_gemini_reply(
            r#"{"candidates":[{"content":{"parts":[{"functionCall":{"name":"list_dir","args":{"path":"."}}}]}}]}"#,
        )
        .unwrap();
        match tools {
            ChatReply::ToolCalls(c) => assert_eq!(c[0].name, "list_dir"),
            ChatReply::Text(_) => panic!("expected tool calls"),
        }
        let text = extract_gemini_reply(r#"{"candidates":[{"content":{"parts":[{"text":"hello"}]}}]}"#).unwrap();
        assert_eq!(text, ChatReply::Text("hello".into()));
    }

    #[test]
    fn extract_gemini_usage_reads_well_formed_fields_including_the_code_assist_fallback() {
        let usage = extract_gemini_usage(r#"{"usageMetadata":{"promptTokenCount":50,"candidatesTokenCount":10,"totalTokenCount":60}}"#);
        assert_eq!(usage, TurnUsage { tokens_in: Some(50), tokens_out: Some(10) });

        let code_assist = extract_gemini_usage(r#"{"response":{"usageMetadata":{"promptTokenCount":7,"candidatesTokenCount":3}}}"#);
        assert_eq!(code_assist, TurnUsage { tokens_in: Some(7), tokens_out: Some(3) });

        let missing = extract_gemini_usage(r#"{"candidates":[]}"#);
        assert_eq!(missing, TurnUsage::default());

        let malformed = extract_gemini_usage("not json");
        assert_eq!(malformed, TurnUsage::default());
    }

    fn sse(data: &str) -> SseEvent {
        SseEvent { event: None, data: data.to_string() }
    }

    #[test]
    fn anthropic_assembler_reconstructs_text_and_a_fragmented_tool_call() {
        let mut a = AnthropicStreamAssembler::new();
        let mut deltas = Vec::new();
        let mut feed = |data: &str| a.feed(sse(data), &mut |t: &str| deltas.push(t.to_string()));

        assert!(matches!(feed(r#"{"type":"message_start"}"#), DriveControl::Continue));
        assert!(matches!(
            feed(r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#),
            DriveControl::Continue
        ));
        assert!(matches!(
            feed(r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#),
            DriveControl::Continue
        ));
        assert!(matches!(feed(r#"{"type":"content_block_stop","index":0}"#), DriveControl::Continue));
        assert!(matches!(
            feed(r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"tu_1","name":"get_weather"}}"#),
            DriveControl::Continue
        ));
        assert!(matches!(
            feed(r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"location\":"}}"#),
            DriveControl::Continue
        ));
        assert!(matches!(
            feed(r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":" \"San Francisco, CA\"}"}}"#),
            DriveControl::Continue
        ));
        assert!(matches!(feed(r#"{"type":"content_block_stop","index":1}"#), DriveControl::Continue));
        assert!(matches!(feed(r#"{"type":"message_stop"}"#), DriveControl::Stop));

        assert_eq!(deltas, vec!["Hello"], "on_text_delta must fire only for the text block");
        match a.finish().unwrap() {
            ChatReply::ToolCalls(calls) => {
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].name, "get_weather");
                assert_eq!(calls[0].arguments["location"], "San Francisco, CA");
            }
            ChatReply::Text(_) => panic!("expected tool calls (a tool_use block was streamed)"),
        }
    }

    #[test]
    fn anthropic_assembler_captures_usage_from_message_start_and_message_delta() {
        let mut a = AnthropicStreamAssembler::new();

        assert_eq!(a.usage(), TurnUsage::default(), "no usage observed yet");
        a.feed(sse(r#"{"type":"message_start","message":{"usage":{"input_tokens":100,"output_tokens":1}}}"#), &mut |_| {});
        assert_eq!(a.usage(), TurnUsage { tokens_in: Some(100), tokens_out: None }, "only input_tokens is read from message_start — its own output_tokens is a small placeholder, not authoritative");
        a.feed(sse(r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#), &mut |_| {});
        a.feed(sse(r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hi"}}"#), &mut |_| {});
        a.feed(sse(r#"{"type":"content_block_stop","index":0}"#), &mut |_| {});
        a.feed(sse(r#"{"type":"message_delta","delta":{},"usage":{"output_tokens":12}}"#), &mut |_| {});
        // A second message_delta with a DIFFERENT value proves overwrite, not
        // accumulation: a reverted/broken `+=` implementation would land on
        // 12 + 7 = 19 here, not 7. The prior single-message_delta assertion
        // above can't distinguish the two, since starting from `None`/0 both
        // implementations produce the same result.
        a.feed(sse(r#"{"type":"message_delta","delta":{},"usage":{"output_tokens":7}}"#), &mut |_| {});
        assert_eq!(
            a.usage(),
            TurnUsage { tokens_in: Some(100), tokens_out: Some(7) },
            "message_delta's cumulative output_tokens must overwrite (not accumulate), input_tokens stays from message_start"
        );
        a.feed(sse(r#"{"type":"message_stop"}"#), &mut |_| {});
    }

    #[test]
    fn anthropic_assembler_never_forwards_thinking_content() {
        let mut a = AnthropicStreamAssembler::new();
        let mut deltas: Vec<String> = Vec::new();
        let mut feed = |data: &str| a.feed(sse(data), &mut |t: &str| deltas.push(t.to_string()));

        feed(r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#);
        feed(r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"reasoning about it"}}"#);
        feed(r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig"}}"#);
        feed(r#"{"type":"content_block_stop","index":0}"#);
        feed(r#"{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}"#);
        feed(r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"final answer"}}"#);
        feed(r#"{"type":"content_block_stop","index":1}"#);
        feed(r#"{"type":"message_stop"}"#);

        assert!(deltas.iter().all(|d| !d.contains("reasoning")), "thinking content must never reach on_text_delta");
        assert_eq!(a.thinking().map(|t| t.text).as_deref(), Some("reasoning about it"), "kept apart for the operator");
        assert_eq!(a.finish().unwrap(), ChatReply::Text("final answer".into()));
    }

    #[test]
    fn anthropic_assembler_treats_an_error_event_as_abort() {
        let mut a = AnthropicStreamAssembler::new();
        let control = a.feed(
            sse(r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#),
            &mut |_| {},
        );
        match control {
            DriveControl::Abort(msg) => assert!(msg.contains("Overloaded"), "{msg}"),
            _ => panic!("expected Abort"),
        }
    }

    #[test]
    fn anthropic_assembler_partial_text_so_far_includes_an_open_block() {
        let mut a = AnthropicStreamAssembler::new();
        a.feed(sse(r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#), &mut |_| {});
        a.feed(sse(r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"partial"}}"#), &mut |_| {});
        assert_eq!(a.partial_text_so_far(), "partial", "an in-progress (never stopped) block must still count");
    }

    #[test]
    fn anthropic_assembler_finish_does_not_lose_text_from_a_block_still_open_at_message_stop() {
        // A deviation from documented protocol behavior: message_stop
        // arrives with no preceding content_block_stop for the open text
        // block. finish() must still surface that already-streamed text,
        // not report "no text or tool_use blocks".
        let mut a = AnthropicStreamAssembler::new();
        let mut feed = |data: &str| a.feed(sse(data), &mut |_| {});
        feed(r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#);
        feed(r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"never closed"}}"#);
        let control = feed(r#"{"type":"message_stop"}"#);
        assert!(matches!(control, DriveControl::Stop));
        assert_eq!(a.finish().unwrap(), ChatReply::Text("never closed".into()));
    }

    #[test]
    fn openai_assembler_reconstructs_text_and_a_fragmented_tool_call_and_excludes_reasoning() {
        let mut a = OpenAiStreamAssembler::new();
        let mut deltas: Vec<String> = Vec::new();
        {
            let mut feed = |data: &str| a.feed(sse(data), &mut |t: &str| deltas.push(t.to_string()));
            feed(r#"{"choices":[{"delta":{"reasoning_content":"thinking it through"}}]}"#);
            feed(r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"get_weather","arguments":""}}]}}]}"#);
            feed(r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"location\":"}}]}}]}"#);
            feed(r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":" \"Austin\"}"}}]}}]}"#);
            let control = feed("[DONE]");
            assert!(matches!(control, DriveControl::Stop));
        }
        assert!(deltas.is_empty(), "reasoning_content must never reach on_text_delta: {deltas:?}");
        assert_eq!(a.thinking().map(|t| t.text).as_deref(), Some("thinking it through"), "kept apart for the operator");
        match a.finish() {
            ChatReply::ToolCalls(calls) => {
                assert_eq!(calls[0].id, "call_1");
                assert_eq!(calls[0].name, "get_weather");
                assert_eq!(calls[0].arguments["location"], "Austin");
            }
            ChatReply::Text(_) => panic!("expected tool calls"),
        }
    }

    #[test]
    fn openai_assembler_streams_plain_text_deltas() {
        let mut a = OpenAiStreamAssembler::new();
        let mut deltas: Vec<String> = Vec::new();
        {
            let mut feed = |data: &str| a.feed(sse(data), &mut |t: &str| deltas.push(t.to_string()));
            feed(r#"{"choices":[{"delta":{"content":"Hel"}}]}"#);
            feed(r#"{"choices":[{"delta":{"content":"lo"}}]}"#);
            feed("[DONE]");
        }
        assert_eq!(deltas, vec!["Hel", "lo"]);
        assert_eq!(a.finish(), ChatReply::Text("Hello".into()));
    }

    #[test]
    fn openai_assembler_captures_usage_from_a_final_chunk_with_empty_choices() {
        let mut a = OpenAiStreamAssembler::new();
        a.feed(sse(r#"{"choices":[{"delta":{"content":"hi"}}]}"#), &mut |_| {});
        assert_eq!(a.usage(), TurnUsage::default(), "no usage chunk observed yet");
        a.feed(sse(r#"{"choices":[],"usage":{"prompt_tokens":41,"completion_tokens":2,"total_tokens":43}}"#), &mut |_| {});
        assert_eq!(a.usage(), TurnUsage { tokens_in: Some(41), tokens_out: Some(2) });
        a.feed(sse("[DONE]"), &mut |_| {});
    }

    #[test]
    fn gemini_assembler_reconstructs_text_across_chunks_ending_in_plain_eof() {
        let mut a = GeminiStreamAssembler::new();
        let mut deltas: Vec<String> = Vec::new();
        {
            let mut feed = |data: &str| a.feed(sse(data), &mut |t: &str| deltas.push(t.to_string()));
            feed(r#"{"candidates":[{"content":{"parts":[{"text":"Hel"}]}}]}"#);
            feed(r#"{"candidates":[{"content":{"parts":[{"text":"lo"}]}}]}"#);
        }
        assert_eq!(deltas, vec!["Hel", "lo"]);
        // No terminator for Gemini — the caller treats a clean EOF as normal
        // completion and calls finish() itself; simulated here directly.
        assert_eq!(a.finish().unwrap(), ChatReply::Text("Hello".into()));
    }

    #[test]
    fn gemini_assembler_captures_usage_metadata_overwriting_on_each_chunk_so_the_last_wins() {
        let mut a = GeminiStreamAssembler::new();
        a.feed(
            sse(r#"{"candidates":[{"content":{"parts":[{"text":"Hel"}]}}],"usageMetadata":{"promptTokenCount":50,"candidatesTokenCount":1}}"#),
            &mut |_| {},
        );
        assert_eq!(a.usage(), TurnUsage { tokens_in: Some(50), tokens_out: Some(1) });
        a.feed(
            sse(r#"{"candidates":[{"content":{"parts":[{"text":"lo"}]}}],"usageMetadata":{"promptTokenCount":50,"candidatesTokenCount":3}}"#),
            &mut |_| {},
        );
        assert_eq!(a.usage(), TurnUsage { tokens_in: Some(50), tokens_out: Some(3) }, "the last chunk's usageMetadata must win");
    }

    #[test]
    fn gemini_assembler_delivers_a_function_call_whole_in_one_chunk() {
        let mut a = GeminiStreamAssembler::new();
        a.feed(
            sse(r#"{"candidates":[{"content":{"parts":[{"functionCall":{"name":"list_dir","args":{"path":"."}}}]}}]}"#),
            &mut |_| {},
        );
        match a.finish().unwrap() {
            ChatReply::ToolCalls(calls) => {
                assert_eq!(calls[0].name, "list_dir");
                assert_eq!(calls[0].arguments["path"], ".");
            }
            ChatReply::Text(_) => panic!("expected tool calls"),
        }
    }

    #[test]
    fn gemini_assembler_never_forwards_thought_marked_parts() {
        let mut a = GeminiStreamAssembler::new();
        let mut deltas: Vec<String> = Vec::new();
        {
            let mut feed = |data: &str| a.feed(sse(data), &mut |t: &str| deltas.push(t.to_string()));
            feed(r#"{"candidates":[{"content":{"parts":[{"text":"pondering...","thought":true}]}}]}"#);
            feed(r#"{"candidates":[{"content":{"parts":[{"text":"the real answer"}]}}]}"#);
        }
        assert_eq!(deltas, vec!["the real answer"], "a thought-marked part must never reach on_text_delta");
        assert_eq!(a.thinking().map(|t| t.text).as_deref(), Some("pondering..."), "kept apart for the operator");
        assert_eq!(a.finish().unwrap(), ChatReply::Text("the real answer".into()));
    }
    fn screenshot() -> crate::NativeAttachment {
        crate::NativeAttachment {
            filename: "screenshot.png".into(),
            mime_type: "image/png".into(),
            data: b"png bytes".to_vec(),
        }
    }

    /// A screenshot reaches every HTTP provider as an image, in each one's own
    /// shape: Grok/OpenAI `image_url` data URL, Anthropic image block (before
    /// the text), Gemini `inlineData`.
    #[test]
    fn a_user_message_carries_its_images_in_each_providers_shape() {
        let msg = ChatMsg::user_with_attachments("what is wrong here?", vec![screenshot()]);
        let b64 = encode_base64(b"png bytes");

        let openai = openai_messages("sys", std::slice::from_ref(&msg));
        assert_eq!(openai[1]["content"][0], json!({"type": "text", "text": "what is wrong here?"}));
        assert_eq!(
            openai[1]["content"][1],
            json!({"type": "image_url", "image_url": {"url": format!("data:image/png;base64,{b64}")}})
        );

        let anthropic = anthropic_messages(std::slice::from_ref(&msg));
        assert_eq!(
            anthropic[0]["content"][0],
            json!({"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": b64}})
        );
        assert_eq!(anthropic[0]["content"][1], json!({"type": "text", "text": "what is wrong here?"}));

        let gemini = gemini_contents(std::slice::from_ref(&msg));
        assert_eq!(gemini[0]["parts"][1], json!({"inlineData": {"mimeType": "image/png", "data": b64}}));

        // A message without files keeps its plain string shape.
        assert_eq!(openai_messages("sys", &[ChatMsg::user("hi")])[1]["content"], json!("hi"));
        assert_eq!(anthropic_messages(&[ChatMsg::user("hi")])[0]["content"], json!("hi"));
    }

    #[test]
    fn a_pdf_goes_to_anthropic_as_a_document() {
        let pdf = crate::NativeAttachment {
            filename: "spec.pdf".into(),
            mime_type: "application/pdf".into(),
            data: b"%PDF".to_vec(),
        };
        let out = anthropic_messages(&[ChatMsg::user_with_attachments("summarise", vec![pdf])]);
        assert_eq!(out[0]["content"][0]["type"], "document");
        assert_eq!(out[0]["content"][0]["source"]["media_type"], "application/pdf");
    }

    /// The text-only retry: files leave the request, and the message names
    /// them so the model can say it could not see them.
    #[test]
    fn without_attachments_names_what_it_took_out() {
        let out = without_attachments(&[ChatMsg::user_with_attachments("look", vec![screenshot()])]);
        assert!(out[0].attachments.is_empty());
        assert!(out[0].content.starts_with("look\n\n[Not sent: screenshot.png"), "{}", out[0].content);
    }

    #[test]
    fn base64_matches_the_standard_alphabet_and_padding() {
        assert_eq!(encode_base64(b""), "");
        assert_eq!(encode_base64(b"f"), "Zg==");
        assert_eq!(encode_base64(b"fo"), "Zm8=");
        assert_eq!(encode_base64(b"foo"), "Zm9v");
        assert_eq!(encode_base64(&[0xfb, 0xff]), "+/8=");
    }
}
