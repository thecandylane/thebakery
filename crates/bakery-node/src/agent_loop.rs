//! Native agent tool loop with cancellation, optional budgets, and compaction.
//!
//! One human instruction → model/tool turns until done or interrupted. HTTP seats use
//! native provider tool calls ([`bakery_agent::Model::chat_turn`]). An edit
//! becomes a *pending* `agent.file.changed` event and waits for Accept
//! (Manual mode). `ask_user` suspends the turn the same way.

use crate::tools::{self, PromptMode};
#[cfg(test)]
use bakery_agent::ChatReply;
use bakery_agent::{ChatMsg, ToolCall};
use serde_json::Value;

/// Fallback working-context byte budget — used only when the seat's model
/// has NO live catalog context window (see [`should_compact`]): an honest
/// conservative heuristic, not a fabricated provider token count.
pub const CONTEXT_BYTES: usize = 64_000;
pub const SUMMARY_BYTES: usize = 12_000;

/// Minimum token reserve held back from a known context window — room for
/// the reply, the tool declarations, and the compaction summary itself.
pub const MIN_RESERVE_TOKENS: u64 = 8_000;
/// Conservative local bytes-per-token estimate (English source text runs
/// ~3.5–4; 3 OVER-estimates tokens, so compaction fires earlier, never
/// later, than reality requires).
pub const EST_BYTES_PER_TOKEN: u64 = 3;

/// Capacity-aware compaction trigger (Checkpoint 2 item 6), adapted from
/// oh-my-pi's `compaction.ts` trigger math (MIT, Copyright (c) 2025 Mario
/// Zechner and others; THIRD_PARTY_NOTICES.md) with deepseek-harness's
/// named-fallback discipline:
/// - With a known context window (live model catalog): threshold =
///   `window − max(15% of window, MIN_RESERVE_TOKENS)`; consumption =
///   `max(provider-reported input tokens, local byte estimate)` — the
///   max-floor guards against BOTH a missing usage report and a deflated
///   one (oh-my-pi's `compactionContextTokens` trick).
/// - Without a window: the byte heuristic stays, and the trigger says so
///   (`"byte-fallback"`) instead of pretending to token accounting.
/// Returns the basis string when compaction should run, `None` otherwise.
pub fn should_compact(
    context_bytes: usize,
    reported_input_tokens: Option<u64>,
    window_tokens: Option<u64>,
) -> Option<&'static str> {
    match window_tokens {
        Some(w) if w > 0 => {
            let reserve = (w * 15 / 100).max(MIN_RESERVE_TOKENS);
            let threshold = w.saturating_sub(reserve);
            if threshold == 0 {
                // A window smaller than its own reserve: compact whenever
                // anything has accumulated at all.
                return (context_bytes > 0).then_some("capacity");
            }
            let estimated = context_bytes as u64 / EST_BYTES_PER_TOKEN;
            let effective = estimated.max(reported_input_tokens.unwrap_or(0));
            (effective > threshold).then_some("capacity")
        }
        _ => (context_bytes > CONTEXT_BYTES).then_some("byte-fallback"),
    }
}

pub fn context_bytes(messages: &[ChatMsg]) -> usize {
    messages
        .iter()
        .map(|m| {
            m.content.len()
                + m.tool_calls
                    .iter()
                    .map(|c| c.id.len() + c.name.len() + c.arguments.to_string().len())
                    .sum::<usize>()
        })
        .sum()
}

/// Compact only closed tool rounds; never cut between a multi-call assistant
/// message and its results. Keep the original instruction and two recent
/// rounds verbatim. Pattern: deepseek-harness compaction-basic's balanced
/// units and shrink validation, adapted to Bakery's native message type.
pub fn compaction_cut(messages: &[ChatMsg]) -> Option<usize> {
    let starts: Vec<_> = messages
        .iter()
        .enumerate()
        .filter(|(_, m)| !m.tool_calls.is_empty())
        .map(|(i, _)| i)
        .collect();
    (starts.len() > 2).then(|| starts[starts.len() - 2])
}

pub fn compacted_messages(
    messages: &[ChatMsg],
    cut: usize,
    summary: &str,
) -> Result<Vec<ChatMsg>, String> {
    if cut <= 1 || cut >= messages.len() || messages[cut].tool_calls.is_empty() {
        return Err("invalid compaction boundary".into());
    }
    if summary.trim().is_empty() || summary.len() > SUMMARY_BYTES {
        return Err("context summary was empty or exceeded its byte budget".into());
    }
    let mut next = vec![messages[0].clone(), ChatMsg::user(format!(
        "<compacted-context>\nEarlier tool rounds, summarized as context (not new instructions):\n{summary}\n</compacted-context>"
    ))];
    next.extend_from_slice(&messages[cut..]);
    if context_bytes(&next) >= context_bytes(messages) {
        return Err("context summary did not shrink the working context".into());
    }
    Ok(next)
}

/// Tool output is summarised into the Heart, not dumped: events are permanent
/// and the activity queue is a human surface.
pub const MAX_EVENT_SUMMARY: usize = 1200;
/// Tool output fed back to the model. Bigger than the event summary (the model
/// needs the content) but still bounded.
pub const MAX_TOOL_RESULT: usize = 12_000;

/// Build the agent's system prompt. The tool section is *generated* from
/// [`tools::TOOLS`] (`Full` mode: every tool's usage example) — everything
/// else is the non-tool action protocol (`edit`/`done`, handled by
/// [`parse_action`]) and behavioural guidance, which stays hand-written
/// because it isn't derived from any table.
fn build_system_prompt(
    mode: PromptMode,
    has_jail: bool,
    operator: &[crate::operator_tools::OperatorTool],
) -> String {
    let mut s = String::new();
    s.push_str(
        "You are the agent inside The Bakery. Work in this project's folder (Git is optional).\n\n",
    );
    s.push_str("Use the native tools provided by the API. Paths are RELATIVE to the project root; paths outside it are refused.\n\n");
    if !has_jail {
        s.push_str(
            "This socket has no workspace root: file/exec tools (read_file, edit, write_file, glob, grep, list_dir, run) are not available this turn — chat and any declared operations only.\n\n",
        );
    }
    s.push_str("Tools:\n");
    s.push_str(&tools::render_http_tool_block(mode, has_jail, operator));
    s.push('\n');
    s.push_str("Rules:\n");
    s.push_str("- `edit` is an exact string replace; `write_file` proposes the complete new file. Both are proposals — a human Accept writes to disk.\n");
    s.push_str("- `ask_user` stops this turn until the human answers. Use it when you need a real choice.\n");
    s.push_str("- `run` is argv only (no shell string). It works without a toolchain pin; the project's stack is an optional overlay.\n");
    s.push_str("- Read before you edit; do not invent file contents.\n");
    s.push_str(
        "- Stop as soon as the instruction is satisfied; reply in plain text when you are done.\n",
    );
    s.push_str(
        "- Session context (prior turns / paths) is provided when available. If the instruction is short or refers to \"again\" / \"same\" / \"that file\", prefer those paths — do not re-explore first.\n",
    );
    s.push_str(
        "- Prefer a direct read+edit of a known path over broad list/grep when the target is already clear.",
    );
    s
}

/// The agent's base system prompt (tool section generated from the
/// canonical HTTP table plus operator tools). Rebuilt each call so an
/// operator-tool dir change is visible without a node restart.
pub fn system_tools() -> String {
    system_tools_for(true)
}

/// Like [`system_tools`] but reflects whether THIS turn actually has a
/// worktree root (07d-B Phase 2 item 3: the system prompt's tool contract
/// must match what the loop can actually dispatch). `system_tools()` is the
/// `has_jail = true` default every pre-existing caller keeps getting.
pub fn system_tools_for(has_jail: bool) -> String {
    let operator = crate::operator_tools::load_from_default_dir().unwrap_or_default();
    build_system_prompt(PromptMode::Full, has_jail, &operator)
}

/// Events scanned from the tail of the stream for hot memory (bounded).
pub const HOT_MEMORY_SCAN: usize = 80;
/// Max lines injected into the opening message.
pub const HOT_MEMORY_MAX_LINES: usize = 20;
/// Maximum distinct paths written into a single `agent.turn.exhausted` event.
/// Keeps permanent event size bounded regardless of HOT_MEMORY_SCAN changes.
/// The stream's own `agent.tool.called` events are authoritative for full detail.
pub const EXHAUSTED_PATHS_CAP: usize = 20;

/// Compact prior-session facts for the model (not a full transcript dump).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HotMemory {
    pub lines: Vec<String>,
    /// Recent file paths touched by proposals / decisions (newest last).
    pub paths: Vec<String>,
    /// Last `agent.edit.accepted` file_path seen in the scan window — used as
    /// the `prior_accept_path` hint in `agent.turn.exhausted` for orc/gepa.
    pub prior_accept_path: Option<String>,
}

/// Build hot memory from stream events. Pass events **excluding** the current
/// turn's `agent.turn.started` so we do not echo the instruction as "prior".
pub fn hot_memory_from_events<'a>(
    events: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> HotMemory {
    // family, payload
    let mut all: Vec<(&str, &str)> = events.into_iter().collect();
    if all.len() > HOT_MEMORY_SCAN {
        all = all.split_off(all.len() - HOT_MEMORY_SCAN);
    }

    let mut lines: Vec<String> = Vec::new();
    let mut paths: Vec<String> = Vec::new();
    let mut prior_accept_path: Option<String> = None;

    for (family, payload) in all {
        let p: Value = serde_json::from_str(payload).unwrap_or(Value::Null);
        match family {
            "agent.turn.started" => {
                if let Some(ins) = p.get("instruction").and_then(|v| v.as_str()) {
                    let t = truncate(ins.trim(), 180);
                    if !t.is_empty() {
                        lines.push(format!("human asked: {t}"));
                    }
                }
                if let Some(fp) = p.get("file_path").and_then(|v| v.as_str()) {
                    if !fp.is_empty() {
                        push_path(&mut paths, fp);
                    }
                }
            }
            "agent.file.changed" => {
                if let Some(fp) = p.get("file_path").and_then(|v| v.as_str()) {
                    let status = p
                        .get("status")
                        .and_then(|v| v.as_str())
                        .unwrap_or("pending");
                    lines.push(format!("proposal {status}: {fp}"));
                    push_path(&mut paths, fp);
                }
            }
            "agent.edit.accepted" => {
                if let Some(fp) = p.get("file_path").and_then(|v| v.as_str()) {
                    lines.push(format!("accepted write: {fp}"));
                    push_path(&mut paths, fp);
                    prior_accept_path = Some(fp.to_string());
                } else {
                    lines.push("accepted a pending proposal".into());
                }
            }
            "agent.edit.rejected" => {
                if let Some(fp) = p.get("file_path").and_then(|v| v.as_str()) {
                    lines.push(format!("rejected proposal: {fp}"));
                    push_path(&mut paths, fp);
                } else {
                    lines.push("rejected a pending proposal".into());
                }
            }
            "agent.approval.cancelled" => {
                lines.push("operator stopped a pending approval".into());
            }
            "agent.turn.done" => {
                if let Some(s) = p.get("summary").and_then(|v| v.as_str()) {
                    lines.push(format!("agent done: {}", truncate(s.trim(), 120)));
                }
            }
            "agent.turn.exhausted" => {
                lines.push("agent hit step limit (prior turn)".into());
            }
            "agent.turn.failed" => {
                if let Some(r) = p.get("reason").and_then(|v| v.as_str()) {
                    lines.push(format!("turn failed: {}", truncate(r, 100)));
                }
            }
            _ => {}
        }
    }

    if lines.len() > HOT_MEMORY_MAX_LINES {
        lines = lines.split_off(lines.len() - HOT_MEMORY_MAX_LINES);
    }
    // Keep last few unique paths, newest last.
    if paths.len() > 8 {
        paths = paths.split_off(paths.len() - 8);
    }

    HotMemory {
        lines,
        paths,
        prior_accept_path,
    }
}

fn push_path(paths: &mut Vec<String>, fp: &str) {
    let fp = fp.trim();
    if fp.is_empty() {
        return;
    }
    paths.retain(|p| p != fp);
    paths.push(fp.to_string());
}

// ─── Prior turns ─────────────────────────────────────────────────────────────

/// How one of an agent's turns ended, as Heart recorded it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnEnd {
    Done,
    Failed(String),
    Cancelled,
    Exhausted,
    /// No terminal event: the node or the runtime died mid-turn.
    Open,
}

/// One of an agent's own turns rebuilt from Heart: what was asked, what came
/// back, which tools ran, and how it ended. The ACP catch-up, the HTTP
/// opening message and Resume read this one projection, so a turn that
/// stopped half-way is remembered the same way on every runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriorTurn {
    pub turn_id: String,
    pub instruction: String,
    /// The notes from other conversations its opening carried
    /// (`messages::Reading::mark`); `instruction` is from before they were
    /// added.
    pub notes: String,
    /// A finished turn's reply; for an unfinished one, the words it had
    /// recorded before it stopped.
    pub text: String,
    /// Its tool calls in order, as "name (outcome)".
    pub tools: Vec<String>,
    pub end: TurnEnd,
}

/// Whether a native runtime's tool-call title names `tool`: the title read as
/// words and `__` / `.` / `:` / `/` parts (`mcp__bakery__thread_list`,
/// `bakery.thread_list`, "Calling thread_list"), never as a substring, so
/// `list` is not `thread_list`. The client's `titleNamesTool`, the same rule.
pub fn names_tool(title: &str, tool: &str) -> bool {
    let wanted = tool.to_lowercase();
    !wanted.is_empty()
        && title
            .to_lowercase()
            .split(|c: char| c.is_whitespace() || matches!(c, '.' | ':' | '/'))
            .flat_map(|word| word.split("__"))
            .any(|word| word == wanted)
}

/// What a native call's stop record (`pipe::STOPPED_BEFORE` / `_AFTER`)
/// says of it, in words.
pub fn stopped_outcome(event: &str) -> Option<&'static str> {
    match event {
        crate::pipe::STOPPED_BEFORE => Some("stopped before it ran"),
        crate::pipe::STOPPED_AFTER => Some("stopped after it ran"),
        _ => None,
    }
}

/// `events` are `(family, payload)` pairs already scoped to one agent, oldest
/// first. An event without a `turn_id` (older records) belongs to the turn
/// before it.
pub fn prior_turns<'a>(events: impl IntoIterator<Item = (&'a str, &'a str)>) -> Vec<PriorTurn> {
    let mut turns: Vec<PriorTurn> = Vec::new();
    // Calls a native runtime made through Bakery's MCP server, which the
    // runtime also reports itself: counted once (see below).
    let mut mcp_calls: Vec<(usize, String, &'static str)> = Vec::new();
    // A native tool call is announced once and updated after: its row, by
    // (turn, toolCallId) → (index in that turn's tools, title, status).
    let mut native: std::collections::HashMap<(usize, String), (usize, String, String)> =
        std::collections::HashMap::new();
    for (family, payload) in events {
        let p: Value = serde_json::from_str(payload).unwrap_or(Value::Null);
        let field = |key: &str| p.get(key).and_then(Value::as_str).unwrap_or("");
        if family == "agent.turn.started" {
            turns.push(PriorTurn {
                turn_id: field("turn_id").to_string(),
                instruction: field("instruction").to_string(),
                notes: String::new(),
                text: String::new(),
                tools: Vec::new(),
                end: TurnEnd::Open,
            });
            continue;
        }
        let turn_id = field("turn_id");
        let at = if turn_id.is_empty() {
            turns.len().checked_sub(1)
        } else {
            turns.iter().rposition(|t| t.turn_id == turn_id)
        };
        let Some(at) = at else { continue };
        let turn = &mut turns[at];
        match family {
            "agent.turn.done" => {
                turn.end = TurnEnd::Done;
                turn.text = field("summary").to_string();
            }
            "agent.turn.failed" => turn.end = TurnEnd::Failed(field("reason").to_string()),
            "agent.turn.cancelled" => turn.end = TurnEnd::Cancelled,
            "agent.turn.exhausted" => turn.end = TurnEnd::Exhausted,
            crate::messages::READ => turn.notes = field("text").to_string(),
            "agent.acp.text" if turn.end != TurnEnd::Done => turn.text.push_str(field("text")),
            "agent.tool.called" => {
                let outcome = if field("stopped") == "before" {
                    "stopped before it ran"
                } else if p.get("ok").and_then(Value::as_bool) == Some(false) {
                    "failed"
                } else {
                    "ok"
                };
                if field("transport") == "mcp" {
                    mcp_calls.push((at, field("tool").to_string(), outcome));
                } else {
                    turn.tools.push(format!("{} ({outcome})", field("tool")));
                }
            }
            "agent.acp.tool_call" => {
                let detail = |key: &str| {
                    p.get("detail")
                        .and_then(|d| d.get(key))
                        .and_then(Value::as_str)
                        .unwrap_or("")
                };
                let row = native
                    .entry((at, detail("toolCallId").to_string()))
                    .or_insert_with(|| {
                        turn.tools.push(String::new());
                        (turn.tools.len() - 1, String::new(), String::new())
                    });
                if !detail("title").is_empty() {
                    row.1 = detail("title").to_string();
                }
                // What Stop did to it outlasts the failure its teardown
                // reports; only finishing after all says more.
                let stopped = stopped_outcome(field("event"));
                if let Some(stopped) = stopped {
                    row.2 = stopped.to_string();
                } else if !detail("status").is_empty() && (!row.2.starts_with("stopped") || detail("status") == "completed") {
                    row.2 = detail("status").to_string();
                }
                turn.tools[row.0] = if row.2.is_empty() {
                    row.1.clone()
                } else {
                    format!("{} ({})", row.1, row.2)
                };
            }
            _ => {}
        }
    }
    // Bakery's MCP server records each call a native runtime makes through
    // it, and the runtime reports the same call as a tool call of its own:
    // one record accounts for one runtime row naming that tool, and a record
    // no runtime row accounts for (a runtime that does not report MCP calls)
    // is kept.
    let mut claimed: std::collections::HashSet<(usize, String)> = std::collections::HashSet::new();
    for (at, tool, outcome) in mcp_calls {
        let accounted = native
            .iter()
            .find(|((turn, id), (_, title, _))| {
                *turn == at && !claimed.contains(&(*turn, id.clone())) && names_tool(title, &tool)
            })
            .map(|(key, _)| key.clone());
        match accounted {
            Some(key) => {
                claimed.insert(key);
            }
            None => turns[at].tools.push(format!("{tool} ({outcome})")),
        }
    }
    turns
}

/// This agent's own turns on `stream_id`, oldest first, without the one in
/// flight (`current_turn_id`; empty excludes nothing). Scoped by `agent_id`
/// like hot memory: never another agent's thread on the same socket.
pub fn agent_prior_turns(
    state: &crate::state::AppState,
    stream_id: &str,
    agent_id: &str,
    current_turn_id: &str,
) -> Vec<PriorTurn> {
    let events = match state.heart.lock().unwrap().read_stream(stream_id) {
        Ok(events) => events,
        Err(_) => return Vec::new(),
    };
    let owned = events.iter().filter(|e| {
        serde_json::from_str::<Value>(&e.payload)
            .ok()
            .and_then(|p| p.get("agent_id").and_then(Value::as_str).map(|a| a == agent_id))
            .unwrap_or(false)
    });
    let mut turns = prior_turns(owned.map(|e| (e.family.as_str(), e.payload.as_str())));
    if !current_turn_id.is_empty() {
        turns.retain(|t| t.turn_id != current_turn_id);
    }
    turns
}

/// The first `max` chars of `s`, marked when cut.
pub fn clip_head(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max).collect();
    format!("{head}… [{} more chars cut]", n - max)
}

/// The last `max` chars of `s`, marked when cut: where a reply's conclusion,
/// or the last thing an unfinished turn was doing, lives.
pub fn clip_tail(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    let tail: String = s.chars().skip(n - max).collect();
    format!("[{} earlier chars cut] …{tail}", n - max)
}

/// Why an unfinished turn stopped, in words; `None` for a finished one.
fn why_unfinished(end: &TurnEnd) -> Option<String> {
    match end {
        TurnEnd::Done => None,
        TurnEnd::Failed(reason) if reason.trim().is_empty() => Some("it failed".to_string()),
        TurnEnd::Failed(reason) => Some(format!("it failed: {}", clip_head(reason.trim(), 300))),
        TurnEnd::Cancelled => Some("the operator stopped it".to_string()),
        TurnEnd::Exhausted => Some("it hit its step limit".to_string()),
        TurnEnd::Open => Some("it was cut off mid-turn".to_string()),
    }
}

/// One turn as transcript text, about `max_chars` long at most: the request
/// keeps its start, after the notes it opened with, the reply its end. An
/// unfinished turn says why it stopped, what it had written, and which tools
/// it ran.
pub fn render_prior_turn(turn: &PriorTurn, max_chars: usize) -> String {
    let asked = clip_head(turn.instruction.trim(), max_chars / 2);
    let mut out = match turn.notes.trim() {
        "" => format!("Human: {asked}\n"),
        notes => format!("Human: {}\n\n{asked}\n", clip_head(notes, max_chars / 4)),
    };
    let text = turn.text.trim();
    match why_unfinished(&turn.end) {
        None => {
            let room = max_chars.saturating_sub(out.chars().count() + 11);
            out.push_str(&format!("Assistant: {}", clip_tail(text, room)));
        }
        // A command (`/compact`) runs only as sent: one that did not finish
        // is no work of the agent's to pick back up.
        Some(_) if crate::pipe::is_command(&turn.instruction) => {
            out.push_str("(A command that did not finish. It runs again only when it is sent again: nothing to pick up.)");
        }
        Some(why) => {
            let tools = match turn.tools.len() {
                0 => String::new(),
                n if n <= 12 => format!("\nTools it ran: {}", turn.tools.join(", ")),
                n => format!(
                    "\nTools it ran: {} earlier, then {}",
                    n - 12,
                    turn.tools[n - 12..].join(", ")
                ),
            };
            let head = format!("Assistant (did not finish — {why}): ");
            let room = max_chars
                .saturating_sub(out.chars().count() + head.chars().count() + tools.chars().count());
            let text = if text.is_empty() {
                "(nothing written yet)".to_string()
            } else {
                clip_tail(text, room)
            };
            out.push_str(&head);
            out.push_str(&text);
            out.push_str(&tools);
        }
    }
    out
}

/// Prior turns as a transcript, oldest first, within `max_turns` and about
/// `max_chars`. The budget is filled newest first, so the turn the
/// conversation just came from is never the one dropped, and no single turn
/// takes more than half of it.
pub fn render_prior_turns(turns: &[PriorTurn], max_turns: usize, max_chars: usize) -> String {
    let mut pieces: Vec<String> = Vec::new();
    let mut left = max_chars;
    for turn in turns.iter().rev().take(max_turns) {
        if !pieces.is_empty() && left < 200 {
            break;
        }
        let piece = render_prior_turn(turn, (max_chars / 2).min(left));
        let len = piece.chars().count() + 2;
        if len > left && !pieces.is_empty() {
            break;
        }
        left = left.saturating_sub(len);
        pieces.push(piece);
    }
    pieces.iter().rev().map(|p| format!("{p}\n\n")).collect()
}

/// What an HTTP turn is told when this agent's previous turn never finished:
/// that turn's request in full and what it got done. A runtime that keeps no
/// session of its own otherwise sees only a one-line memory of it, so
/// "continue" would have nothing to continue.
pub fn unfinished_previous_turn_note(turns: &[PriorTurn]) -> Option<String> {
    let last = turns.last()?;
    // A command that did not finish is sent again, never picked back up.
    if last.end == TurnEnd::Done || crate::pipe::is_command(&last.instruction) {
        return None;
    }
    Some(format!(
        "[Bakery: your previous turn in this conversation did not finish. It is here for \
         reference: follow the new request below, and pick that work back up when it asks you \
         to.]\n\n{}\n\n[End of the unfinished turn.]",
        render_prior_turn(last, 12_000)
    ))
}

/// One decoded model action. Only the scripted-model test fixture path
/// ([`chat_reply_from_scripted_text`]) still decodes this; production HTTP
/// providers use native tool calls.
#[cfg(test)]
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Tool {
        name: String,
        args: Value,
    },
    Edit {
        file_path: String,
        new_content: String,
        rationale: String,
    },
    Done {
        summary: String,
    },
}

/// Decode one model reply. Tolerant of fences and of leading/trailing prose:
/// models wrap JSON more often than they get the protocol wrong.
#[cfg(test)]
pub fn parse_action(raw: &str) -> Result<Action, String> {
    let text = strip_fence(raw.trim());
    let json = slice_outer_object(text).ok_or("reply contained no JSON object")?;
    let v: Value =
        serde_json::from_str(json).map_err(|e| format!("reply is not valid JSON: {e}"))?;

    if let Some(tool) = v.get("tool").and_then(|t| t.as_str()) {
        let args = v
            .get("args")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));
        if !args.is_object() {
            return Err("\"args\" must be an object".to_string());
        }
        if tool == "edit" || tool == "write_file" {
            return edit_action_from_tool(tool, &args);
        }
        return Ok(Action::Tool {
            name: tool.to_string(),
            args,
        });
    }
    if let Some(edit) = v.get("edit") {
        let file_path = edit
            .get("file_path")
            .and_then(|x| x.as_str())
            .filter(|s| !s.trim().is_empty())
            .ok_or("edit needs a non-empty \"file_path\"")?;
        let new_content = edit
            .get("new_content")
            .and_then(|x| x.as_str())
            .ok_or("edit needs \"new_content\" (the complete new file)")?;
        return Ok(Action::Edit {
            file_path: file_path.trim().to_string(),
            new_content: new_content.to_string(),
            rationale: edit
                .get("rationale")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
        });
    }
    if let Some(done) = v.get("done") {
        let summary = done
            .get("summary")
            .and_then(|x| x.as_str())
            .or_else(|| done.as_str())
            .unwrap_or("done")
            .to_string();
        return Ok(Action::Done { summary });
    }
    Err("JSON had none of \"tool\", \"edit\", \"done\"".to_string())
}

#[cfg(test)]
fn edit_action_from_tool(tool: &str, args: &Value) -> Result<Action, String> {
    if tool == "write_file" {
        let file_path = args
            .get("path")
            .and_then(|x| x.as_str())
            .filter(|s| !s.trim().is_empty())
            .ok_or("write_file needs a non-empty \"path\"")?;
        let new_content = args
            .get("content")
            .and_then(|x| x.as_str())
            .ok_or("write_file needs \"content\"")?;
        return Ok(Action::Edit {
            file_path: file_path.trim().to_string(),
            new_content: new_content.to_string(),
            rationale: args
                .get("rationale")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
        });
    }
    // `edit` with old_string/new_string stays a Tool so the loop can read
    // the file and apply a single exact replace.
    let args = args.clone();
    Ok(Action::Tool {
        name: tool.to_string(),
        args,
    })
}

/// The first `{` to the last `}` — enough to survive a model that adds a
/// sentence around its JSON.
#[cfg(test)]
fn slice_outer_object(s: &str) -> Option<&str> {
    let start = s.find('{')?;
    let end = s.rfind('}')?;
    if end <= start {
        return None;
    }
    Some(&s[start..=end])
}

#[cfg(test)]
fn strip_fence(s: &str) -> &str {
    let t = s.trim();
    let Some(rest) = t.strip_prefix("```") else {
        return t;
    };
    // Drop the info string (```json) then the closing fence.
    let rest = rest.split_once('\n').map(|(_, r)| r).unwrap_or("");
    rest.trim_end().strip_suffix("```").unwrap_or(rest).trim()
}

/// First user message: instruction + optional path hint + hot session context.
///
/// `factory_ops` is a short measured block for Baker streams only (assembled
/// server-side). Prefer also attaching it to the system prompt via
/// [`system_with_ops`] so the model sees factory state without cluttering the
/// human instruction channel.
/// `memory` is the memory's index (`memory_index::turn_index`), placed
/// before the instruction so the opening's start stays the same from turn
/// to turn.
pub fn opening_message(
    instruction: &str,
    project: &str,
    branch: &str,
    hint: Option<&str>,
    hot: Option<&HotMemory>,
    memory: Option<&str>,
    factory_ops: Option<&str>,
) -> ChatMsg {
    let mut text = format!("Project: {project}\nBranch: {branch}\n");
    if let Some(memory) = memory.filter(|m| !m.trim().is_empty()) {
        text.push_str(memory);
    }
    text.push_str(&format!("Instruction: {}\n", instruction.trim()));
    if let Some(h) = hint.map(str::trim).filter(|h| !h.is_empty()) {
        text.push_str(&format!("The human suggests starting at: {h}\n"));
    }
    if let Some(mem) = hot {
        if !mem.lines.is_empty() || !mem.paths.is_empty() {
            text.push_str("\nSession context (recent; prefer over blind re-exploration):\n");
            for line in &mem.lines {
                text.push_str(&format!("- {line}\n"));
            }
            if !mem.paths.is_empty() {
                text.push_str(&format!("- recent paths: {}\n", mem.paths.join(", ")));
            }
        }
    }
    // Baker ops also live on the system prompt; a one-line pointer here keeps
    // the opening message honest when only the user channel is inspected.
    if let Some(ops) = factory_ops.map(str::trim).filter(|s| !s.is_empty()) {
        text.push_str("\n");
        text.push_str(ops);
        if !ops.ends_with('\n') {
            text.push('\n');
        }
    }
    if hot
        .map(|m| m.lines.is_empty() && m.paths.is_empty())
        .unwrap_or(true)
        && hint.map(str::trim).filter(|h| !h.is_empty()).is_none()
        && factory_ops
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .is_none()
    {
        text.push_str("\nStart by exploring if you are unsure of the layout.");
    } else {
        text.push_str(
            "\nIf the instruction is clear from context, act (read target → edit or done). Avoid redundant exploration.",
        );
    }
    ChatMsg::user(text)
}

/// System prompt for a turn: base tools contract plus optional Baker ops block.
/// Superseded in the HTTP tool loop by [`system_with_ops_for_turn`] (07d-B
/// Phase 2 item 3 needs the `has_jail`/`task_complete` context this simpler
/// form doesn't carry) — kept live and tested as the ACP/simple-caller
/// shape (no per-turn jail/task-socket distinction to make).
#[allow(dead_code)]
pub fn system_with_ops(factory_ops: Option<&str>) -> String {
    match factory_ops.map(str::trim).filter(|s| !s.is_empty()) {
        Some(ops) => format!("{}\n\n{ops}", system_tools()),
        None => system_tools(),
    }
}

/// [`system_with_ops`], but honest about what THIS turn's actual tool set is
/// (07d-B Phase 2 items 3/4/6): `has_jail` omits jailed file/exec tools from
/// the base contract for a rootless socket; `task_complete` appends the
/// worker-scoped completion tool's contract for a task socket. The permanent
/// Baker's own op-registry line already lives inside `factory_ops`
/// (`crate::routes::agent::baker_ops_for_turn` appends
/// [`format_baker_op_registry`] there) — this function only decides where
/// that block lands in the prompt, same as `system_with_ops` does today.
pub fn system_with_ops_for_turn(
    factory_ops: Option<&str>,
    has_jail: bool,
    task_complete: bool,
) -> String {
    let mut s = system_tools_for(has_jail);
    if task_complete {
        s.push_str(
            "\n\nYou also have a declared `task_complete` tool: call it with {\"summary\": \"...\"} once this \
             task's scope is done. It is the ONLY signal that moves this task toward proof/review — finishing a \
             chat reply alone does not complete the task.\n",
        );
    }
    if let Some(ops) = factory_ops.map(str::trim).filter(|s| !s.is_empty()) {
        s.push_str("\n\n");
        s.push_str(ops);
    }
    s
}

/// Which persona [`persona_preamble`] renders — the permanent Baker, or one
/// task socket's worker.
pub enum PersonaRole<'a> {
    Baker,
    TaskWorker { task_id: &'a str },
    ProjectAgent { role: &'a str },
}

/// Persona instructions for the permanent Baker or a task worker (07d-B
/// Phase 2 Stage C item 5) — the ONE function both transports read, so their
/// instructions can never drift apart: the HTTP tool loop prepends this to
/// its system prompt (`routes::agent::agent_tool_turn`), and ACP prepends it
/// to the first prompt of each session (`pipe::session_main`, the same
/// injection point as `acp_catchup_context` — re-sent after every respawn).
/// Deliberately short and factual: no invented capability claims beyond
/// "acts through tools, every operation is recorded."
pub fn persona_preamble(role: PersonaRole) -> String {
    // Identity rule (07d-B contract, and A6-C's "provider identity is
    // metadata, never the answer to 'who are you?'"): the agent's name is
    // its identity; the model behind it is a replaceable engine. Without
    // this stated explicitly, the underlying model's own self-knowledge
    // wins the introduction — dogfood run one opened with "I'm Baker —
    // built on top of Claude / Model: Claude", which is exactly the
    // collapse STUDIO.md § Identities forbids.
    match role {
        PersonaRole::Baker => "You are Baker, the permanent factory agent of The Bakery. Baker is your \
             identity; the model backing you is a replaceable engine, not who you are — introduce \
             yourself only as Baker, and do not volunteer the underlying model or provider. If asked \
             directly what model backs you, name it in one short sentence as configuration, then \
             continue as Baker. You act through the provided tools (declared Bakery operations over \
             MCP or the HTTP tool loop); every operation is recorded, and your file edits are \
             proposals that wait for human review — never present a proposed edit as already applied. \
             Work like a senior engineer on this codebase: answer directly and concisely, no emoji, no \
             self-portraits or capability tours unless asked. State facts from the Bakery session lines \
             below or from a tool result, never from assumption — check (git, files, your Bakery ops) \
             before claiming branch, working-tree, project, or thread state.\n"
            .to_string(),
        PersonaRole::TaskWorker { task_id } => format!(
            "You are the task worker for task {task_id}, an agent in The Bakery. That is your \
             identity; the model backing you is a replaceable engine, not who you are — do not \
             volunteer the underlying model or provider. You act through the provided tools (over \
             MCP or the HTTP tool loop); every operation is recorded.\n"
        ),
        PersonaRole::ProjectAgent { role } => format!(
            "You are the {role} agent for this project in The Bakery, not the Baker and not a vendor CLI persona. \
             Introduce yourself by this role. The underlying model/provider is replaceable configuration; \
             identify it honestly if asked directly, but do not introduce yourself as Claude Code or another vendor application. \
             Use the actual project/root context supplied below; Git and branches are optional. \
             Work through the available tools, preserve existing functionality when editing, and verify your changes. \
             A pending proposal is not an applied change: report the actual tool result.\n"
        ),
    }
}

/// Identity selection is shared too, so HTTP and ACP conversation paths
/// cannot omit ordinary project roles while covering only factory tasks.
pub fn persona_for_agent(
    state: &crate::state::AppState,
    stream_id: &str,
    seat: crate::node::Seat,
) -> String {
    // One Baker persona across the main conversation and every thread
    // (BAKER-THREADS-03) — a thread is the Baker, not a new agent.
    if crate::baker_ops::is_baker_scope_stream(state, stream_id) {
        return persona_preamble(PersonaRole::Baker);
    }
    let task_id = crate::state::socket_task_id(&state.heart.lock().unwrap(), stream_id);
    persona_preamble(match task_id.as_deref() {
        Some(task_id) => PersonaRole::TaskWorker { task_id },
        None => PersonaRole::ProjectAgent {
            role: seat.as_str(),
        },
    })
}

/// Format the measured Baker factory snapshot for the **model** (characters, not tokens).
///
/// Deliberately **no** `node_live` / hostname / node_id — those identify the
/// operator machine and ship to the model provider. Audit surfaces (e.g.
/// `baker.assignment.accepted`) may still record node identity locally.
/// Callers supply only real measurements — never invent spend/email/cost.
pub fn format_baker_ops(
    write_mode: &str,
    stream_count: usize,
    pending_gates: usize,
    baker_branch: &str,
) -> String {
    format!(
        "Factory ops (measured at turn start; counts are events/streams, not tokens):\n\
         - write_mode: {write_mode}\n\
         - stream_count: {stream_count}\n\
         - pending_gates: {pending_gates}\n\
         - baker_branch: {baker_branch}\n"
    )
}

/// Names the permanent Baker's declared op registry in-prompt (07d-B Phase 2
/// item 6): every compiled-in op the Baker seat may call, plus the audit
/// guarantee that backs them. Appended (via
/// `crate::routes::agent::baker_ops_for_turn`) only for the permanent
/// Baker's own turns — never for an old `/baker/assign` branch-job socket,
/// which gets [`format_baker_ops`] alone with no op registry line. Same "no
/// hostnames" rule as [`format_baker_ops`]: op ids are static strings, never
/// operator-machine identity.
pub fn format_baker_op_registry(op_ids: &[&str]) -> String {
    let mut out =
        String::from("You act through declared operations; every op is recorded.\nDeclared ops:\n");
    for id in op_ids {
        out.push_str(&format!("  - {id}\n"));
    }
    out.push_str(
        "Every other Bakery route is a tool too: api_find {query} lists them with their inputs, \
         api_call {tool, args} runs one. Each call has a risk grade (read, write, run, \
         destructive); your mode decides whether the operator is asked first, and destructive \
         calls always ask. If a call answers that it is waiting for approval, call it again \
         after the operator decides.\n",
    );
    out
}

/// Companion to [`format_baker_op_registry`] for a turn whose transport
/// cannot actually dispatch a single one of these ops yet (07d-B Phase 2
/// item 2's ACP path — no `mcp_servers` wiring exists — and the direct
/// single-file-edit path, neither of which ever reaches
/// `crate::baker_ops::dispatch_baker_op`). Naming the ops but instructing
/// the model NOT to call them is the honest alternative Phase 2 item 2
/// itself names: "a plug that cannot deliver the required transport is
/// honestly marked unable to host Baker … not silently degraded" — telling
/// the model it has 14 tools it cannot actually invoke is exactly the
/// silent-degradation outcome that forbids.
pub fn format_baker_op_registry_unavailable(op_ids: &[&str]) -> String {
    let mut out = String::from(
        "Declared Bakery operations exist for this Baker but are NOT callable on this turn's \
         transport (no tool-dispatch channel is wired up here yet) — do not attempt to call any \
         of them; a human must drive them via the HTTP routes instead.\n\
         Declared-but-unavailable-here ops:\n",
    );
    for id in op_ids {
        out.push_str(&format!("  - {id}\n"));
    }
    out
}

/// Tool output as the model sees it (native `role: tool` when `call_id` is set).
#[cfg(test)]
pub fn tool_result_message(tool: &str, ok: bool, body: &str) -> ChatMsg {
    let status = if ok { "ok" } else { "error" };
    ChatMsg::user(format!(
        "TOOL RESULT [{tool}: {status}]\n{}",
        truncate(body, MAX_TOOL_RESULT)
    ))
}

// ─── Stall detection (Checkpoint 2 item 5) ──────────────────────────────
//
// Adapted from hermes-agent's `ToolCallGuardrailController`
// (agent/tool_guardrails.py; MIT License, Copyright (c) 2025 Nous Research,
// full text in THIRD_PARTY_NOTICES.md): three independent
// counters — exact-signature failures, same-tool failures, and identical
// no-progress results — with an escalation ladder of warn-in-band →
// diagnose (a visible replanning demand + durable event) → halt (a
// specific, explained stop). The signature hashes canonical-JSON args so
// key order cannot defeat detection (their `ToolCallSignature` trick).
// This is exactly the guard that replaces arbitrary iteration cutoffs: an
// unbounded loop stays unbounded while it makes progress, and stops with a
// diagnosis when it provably is not.

/// What the loop should do about the call it just observed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StallAction {
    Proceed,
    /// Append this guidance to the tool result fed back to the model.
    Warn(String),
    /// Also inject a replanning demand and record `agent.stall.detected`.
    Diagnose(String),
    /// End the turn with this reason — a detected condition, not a quota.
    Halt(String),
}

/// Canonical (key-sorted) JSON so `{"a":1,"b":2}` and `{"b":2,"a":1}`
/// produce one signature.
fn canonical_json(v: &Value) -> String {
    match v {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let inner: Vec<String> = keys
                .into_iter()
                .map(|k| format!("{}:{}", k, canonical_json(&map[k])))
                .collect();
            format!("{{{}}}", inner.join(","))
        }
        Value::Array(items) => {
            let inner: Vec<String> = items.iter().map(canonical_json).collect();
            format!("[{}]", inner.join(","))
        }
        other => other.to_string(),
    }
}

/// Per-turn stall counters. Any success with a fresh result resets its own
/// signature and its tool's failure streak — only genuine repetition climbs
/// the ladder.
#[derive(Default)]
pub struct StallGuard {
    exact_failures: std::collections::HashMap<String, u32>,
    same_tool_failures: std::collections::HashMap<String, u32>,
    /// signature → (result hash, identical-result repeat count)
    no_progress: std::collections::HashMap<String, (String, u32)>,
}

/// Thresholds: (warn, diagnose, halt). Halting is a *detected* stall with a
/// named cause — never "you used N steps".
const EXACT_FAILURE_LADDER: (u32, u32, u32) = (2, 3, 6);
const SAME_TOOL_LADDER: (u32, u32, u32) = (4, 6, 10);
const NO_PROGRESS_LADDER: (u32, u32, u32) = (3, 4, 7);

impl StallGuard {
    pub fn observe(&mut self, tool: &str, args: &Value, ok: bool, result: &str) -> StallAction {
        let sig = format!(
            "{tool}\u{1f}{}",
            blake3::hash(canonical_json(args).as_bytes()).to_hex()
        );
        if ok {
            self.same_tool_failures.remove(tool);
            self.exact_failures.remove(&sig);
            let result_hash = blake3::hash(result.as_bytes()).to_hex().to_string();
            let entry = self
                .no_progress
                .entry(sig)
                .or_insert((result_hash.clone(), 0));
            if entry.0 == result_hash {
                entry.1 += 1;
            } else {
                *entry = (result_hash, 1);
            }
            let n = entry.1;
            let (warn, diagnose, halt) = NO_PROGRESS_LADDER;
            if n >= halt {
                return StallAction::Halt(format!(
                    "stalled: {tool} was called {n} times with identical arguments and got byte-identical results every time — repeating it cannot make progress"
                ));
            }
            if n >= diagnose {
                return StallAction::Diagnose(format!(
                    "{tool} has now returned the exact same result {n} times for the same arguments. Stop repeating it. State what you were trying to learn, what the result actually says, and take a DIFFERENT next step."
                ));
            }
            if n >= warn {
                return StallAction::Warn(format!(
                    "[note: this is the {n}th identical call with an identical result — the answer will not change; use what you already have]"
                ));
            }
            return StallAction::Proceed;
        }
        let exact = self.exact_failures.entry(sig).or_insert(0);
        *exact += 1;
        let exact_n = *exact;
        let tool_n = {
            let t = self.same_tool_failures.entry(tool.to_string()).or_insert(0);
            *t += 1;
            *t
        };
        let (ew, ed, eh) = EXACT_FAILURE_LADDER;
        let (tw, td, th) = SAME_TOOL_LADDER;
        if exact_n >= eh || tool_n >= th {
            return StallAction::Halt(format!(
                "stalled: {tool} keeps failing ({exact_n} identical failures, {tool_n} failures of this tool this turn) — the same call cannot start succeeding by repetition"
            ));
        }
        if exact_n >= ed || tool_n >= td {
            return StallAction::Diagnose(format!(
                "{tool} has failed {exact_n} times with the SAME arguments ({tool_n} failures of this tool overall this turn). Do not retry it again. Diagnose: read the error, state the cause in one sentence, and change the arguments or the approach."
            ));
        }
        if exact_n >= ew || tool_n >= tw {
            return StallAction::Warn(format!(
                "[note: {exact_n} identical failures of this call — re-read the error before trying again; blind retries will be stopped]"
            ));
        }
        StallAction::Proceed
    }
}

pub fn native_tool_result(call: &ToolCall, ok: bool, body: &str) -> ChatMsg {
    let status = if ok { "ok" } else { "error" };
    ChatMsg::tool_result(
        call.id.clone(),
        call.name.clone(),
        format!("{status}\n{}", tool_result_body(&call.name, body)),
    )
}

/// A tool's output as the model gets it, within [`MAX_TOOL_RESULT`]. A
/// `run` result keeps both ends: a head-only cut let a long stdout push
/// stderr and the test summary, which come last, out of view. Every other
/// tool keeps its head.
pub fn tool_result_body(tool: &str, body: &str) -> String {
    if tool == "run" || tool == "bash" {
        clip_middle(body, MAX_TOOL_RESULT)
    } else {
        truncate(body, MAX_TOOL_RESULT)
    }
}

/// A tool result as Heart and the transcript keep it, within
/// [`MAX_EVENT_SUMMARY`]. Like [`tool_result_body`], a command keeps both
/// ends: its exit code is at the top and its verdict — stderr, the failing
/// test, the summary line — at the bottom, which a head-only cut dropped.
pub fn event_summary(tool: &str, body: &str) -> String {
    if tool == "run" || tool == "bash" {
        clip_middle(body, MAX_EVENT_SUMMARY)
    } else {
        truncate(body, MAX_EVENT_SUMMARY)
    }
}

/// The head and the tail of `s` within `max` chars, the middle replaced by
/// an explicit `[… N chars omitted …]` line. The tail gets two thirds,
/// because a command's verdict is at its end.
pub fn clip_middle(s: &str, max: usize) -> String {
    let total = s.chars().count();
    if total <= max {
        return s.to_string();
    }
    // Room for the marker line, whatever the width of its count.
    let keep = max.saturating_sub(48);
    let head = keep / 3;
    let tail = keep - head;
    let head_text: String = s.chars().take(head).collect();
    let tail_text: String = s.chars().skip(total - tail).collect();
    format!(
        "{head_text}\n[… {} chars omitted …]\n{tail_text}",
        total - head - tail
    )
}

/// Scripted-model helper: turn the old JSON-in-text fixture language into a
/// native [`ChatReply`]. Production HTTP providers never go through this.
#[cfg(test)]
pub fn chat_reply_from_scripted_text(raw: &str) -> ChatReply {
    match parse_action(raw) {
        Ok(Action::Tool { name, args }) => ChatReply::ToolCalls(vec![ToolCall {
            id: "script".into(),
            name,
            arguments: args,
        }]),
        Ok(Action::Edit {
            file_path,
            new_content,
            rationale,
        }) => ChatReply::ToolCalls(vec![ToolCall {
            id: "script".into(),
            name: "write_file".into(),
            arguments: serde_json::json!({
                "path": file_path,
                "content": new_content,
                "rationale": rationale,
            }),
        }]),
        Ok(Action::Done { summary }) => ChatReply::Text(summary),
        Err(_) => ChatReply::Text(raw.to_string()),
    }
}

/// Truncate on a char boundary, marking that it happened.
pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max).collect();
    format!("{head}\n… truncated ({} chars total)", s.chars().count())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compaction_keeps_whole_multi_tool_rounds_and_rejects_bad_summaries() {
        let mut messages = vec![ChatMsg::user("original task")];
        for round in 0..5 {
            let calls: Vec<_> = (0..2)
                .map(|n| ToolCall {
                    id: format!("{round}-{n}"),
                    name: "read_file".into(),
                    arguments: serde_json::json!({"path":"x"}),
                })
                .collect();
            messages.push(ChatMsg::assistant_tools(calls.clone()));
            for call in calls {
                messages.push(native_tool_result(&call, true, &"x".repeat(5000)));
            }
        }
        let before = context_bytes(&messages);
        let cut = compaction_cut(&messages).unwrap();
        let next = compacted_messages(
            &messages,
            cut,
            "Earlier reads verified x; continue the original task.",
        )
        .unwrap();
        assert_eq!(next[0].content, "original task");
        assert_eq!(next[2].tool_calls[0].id, "3-0");
        assert_eq!(next[3].tool_call_id.as_deref(), Some("3-0"));
        assert_eq!(next[4].tool_call_id.as_deref(), Some("3-1"));
        assert_eq!(next.last().unwrap().tool_call_id.as_deref(), Some("4-1"));
        assert!(context_bytes(&next) < before);
        assert!(compacted_messages(&messages, cut, "").is_err());
        assert!(compacted_messages(&messages, cut, &"x".repeat(SUMMARY_BYTES + 1)).is_err());
        assert!(compacted_messages(&messages, cut + 1, "summary").is_err());
        assert_eq!(
            context_bytes(&messages),
            before,
            "failures never mutate original context"
        );
    }

    #[test]
    fn hot_memory_prefers_recent_paths_and_instructions() {
        let evs = [
            (
                "agent.turn.started",
                r#"{"instruction":"change hello to hello.txt","file_path":null}"#,
            ),
            (
                "agent.file.changed",
                r#"{"file_path":"hello.txt","status":"pending"}"#,
            ),
            (
                "agent.edit.accepted",
                r#"{"file_path":"hello.txt","status":"accepted"}"#,
            ),
            (
                "agent.turn.started",
                r#"{"instruction":"do the same thing again","file_path":null}"#,
            ),
        ];
        // Caller excludes current turn.started — pass only prior three.
        let hot = hot_memory_from_events(evs[..3].iter().copied());
        assert!(
            hot.paths.iter().any(|p| p == "hello.txt"),
            "paths: {:?}",
            hot.paths
        );
        assert!(
            hot.lines.iter().any(|l| l.contains("accepted write")),
            "lines: {:?}",
            hot.lines
        );
        assert!(
            hot.lines.iter().any(|l| l.contains("change hello")),
            "lines: {:?}",
            hot.lines
        );
    }

    #[test]
    fn opening_message_includes_session_context() {
        let hot = HotMemory {
            lines: vec!["accepted write: hello.txt".into()],
            paths: vec!["hello.txt".into()],
            prior_accept_path: Some("hello.txt".into()),
        };
        let msg = opening_message(
            "do it again",
            "thebakery",
            "main",
            None,
            Some(&hot),
            None,
            None,
        );
        assert!(msg.content.contains("Session context"));
        assert!(msg.content.contains("hello.txt"));
        assert!(msg.content.contains("Avoid redundant exploration"));
        assert!(!msg.content.contains("Start by exploring if you are unsure"));
    }

    #[test]
    fn format_baker_ops_has_real_fields_not_spend_or_email() {
        let s = format_baker_ops("manual", 3, 1, "baker/shell");
        assert!(
            !s.contains("node_live"),
            "node_id must not go to the model: {s}"
        );
        assert!(s.contains("write_mode: manual"));
        assert!(s.contains("stream_count: 3"));
        assert!(s.contains("pending_gates: 1"));
        assert!(s.contains("baker_branch: baker/shell"));
        assert!(s.contains("not tokens"));
        let lower = s.to_ascii_lowercase();
        assert!(!lower.contains("email"), "{s}");
        assert!(!lower.contains("spend"), "{s}");
        assert!(!lower.contains("cost_usd"), "{s}");
        assert!(!lower.contains("oauth"), "{s}");
    }

    #[test]
    fn format_baker_ops_never_embeds_hostname_slug() {
        // mint_node_id("Sam's MacBook Pro.local") → sam-s-macbook-pro-local-…
        // That must never appear in model-facing ops (provider PII surface).
        let s = format_baker_ops("rules", 1, 0, "baker/job");
        for banned in [
            "node_live",
            "macbook",
            "work-laptop",
            "hostname",
            "sam-s",
            "local-",
        ] {
            assert!(
                !s.to_ascii_lowercase().contains(banned),
                "ops must not contain {banned:?}: {s}"
            );
        }
    }

    #[test]
    fn system_with_ops_appends_only_when_present() {
        let bare = system_with_ops(None);
        assert_eq!(bare, system_tools());
        let with = system_with_ops(Some("Factory ops:\n- write_mode: manual\n"));
        assert!(with.contains(&system_tools()));
        assert!(with.contains("write_mode: manual"));
    }

    /// Criterion: every canonical HTTP tool name appears in the generated prompt.
    #[test]
    fn every_registered_tool_name_appears_in_the_generated_prompt() {
        for name in tools::BAKERY_HTTP_CORE {
            assert!(
                system_tools().contains(name),
                "{name:?} missing from generated prompt"
            );
        }
    }

    /// Criterion: the prompt advertises the HTTP core table (plus any
    /// executable operator tools from the test env, which defaults to empty).
    #[test]
    fn generated_prompt_advertises_the_registered_tool_names() {
        let prompt = system_tools();
        for name in tools::BAKERY_HTTP_CORE {
            assert!(prompt.contains(name), "{name:?} missing from prompt");
        }
    }

    #[test]
    fn http_core_names_are_identical_for_every_http_seat() {
        let a = tools::advertised_http_tool_names(&[]);
        let b = tools::advertised_http_tool_names(&[]);
        assert_eq!(a, b);
        assert_eq!(
            a,
            tools::BAKERY_HTTP_CORE
                .iter()
                .map(|s| (*s).to_string())
                .collect::<Vec<_>>()
        );
    }

    /// 07d-B Phase 2 item 6: the permanent Baker's op registry names itself
    /// in-prompt and states the audit guarantee.
    #[test]
    fn format_baker_op_registry_names_ops_and_states_the_audit_guarantee() {
        let s = format_baker_op_registry(&["task.create", "project.list"]);
        assert!(s.contains("declared operations"));
        assert!(s.contains("every op is recorded"));
        assert!(s.contains("task.create"));
        assert!(s.contains("project.list"));
    }

    /// 07d-B Phase 2 item 2: a transport that cannot dispatch a single op
    /// (ACP, direct edit) must say so honestly rather than listing ops as if
    /// they were callable — the model must be told not to call them.
    #[test]
    fn format_baker_op_registry_unavailable_names_ops_but_tells_the_model_not_to_call_them() {
        let s = format_baker_op_registry_unavailable(&["task.create", "project.list"]);
        assert!(s.contains("task.create"));
        assert!(s.contains("project.list"));
        assert!(s.to_ascii_lowercase().contains("not callable"), "{s}");
        assert!(s.to_ascii_lowercase().contains("do not attempt"), "{s}");
        assert_ne!(
            s,
            format_baker_op_registry(&["task.create", "project.list"]),
            "the unavailable variant must read differently from the dispatchable one"
        );
    }

    /// 07d-B Phase 2 item 3: a rootless turn's system prompt must not
    /// promise jailed file tools it cannot run.
    #[test]
    fn system_with_ops_for_turn_omits_jailed_tools_when_rootless() {
        let jailed = system_with_ops_for_turn(None, true, false);
        let rootless = system_with_ops_for_turn(None, false, false);
        assert!(jailed.contains("read_file"));
        assert!(!rootless.contains("\"tool\":\"read_file\""));
        assert!(rootless.contains("no workspace root"));
    }

    /// 07d-B Phase 4-adjacent (Stage B item 4): a task socket's prompt names
    /// `task_complete` and its completion contract.
    #[test]
    fn system_with_ops_for_turn_names_task_complete_when_requested() {
        let s = system_with_ops_for_turn(None, true, true);
        assert!(s.contains("task_complete"));
        assert!(s.contains("ONLY signal"));
    }

    #[test]
    fn opening_message_embeds_factory_ops() {
        let ops = format_baker_ops("manual", 0, 0, "baker/x");
        let msg = opening_message("hi", "p", "baker/x", None, None, None, Some(&ops));
        assert!(!msg.content.contains("node_live"));
        assert!(msg.content.contains("baker_branch: baker/x"));
    }

    #[test]
    fn parses_write_file_tool_as_an_edit_proposal() {
        let a = parse_action(
            r#"{"tool":"write_file","args":{"path":"a.rs","content":"hi","rationale":"n"}}"#,
        )
        .unwrap();
        match a {
            Action::Edit {
                file_path,
                new_content,
                rationale,
            } => {
                assert_eq!(file_path, "a.rs");
                assert_eq!(new_content, "hi");
                assert_eq!(rationale, "n");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parses_tool_call() {
        let a = parse_action(r#"{"tool":"read_file","args":{"path":"a.rs"}}"#).unwrap();
        match a {
            Action::Tool { name, args } => {
                assert_eq!(name, "read_file");
                assert_eq!(args["path"], "a.rs");
            }
            other => panic!("expected tool, got {other:?}"),
        }
    }

    #[test]
    fn parses_tool_call_without_args() {
        assert!(matches!(
            parse_action(r#"{"tool":"list_files"}"#).unwrap(),
            Action::Tool { .. }
        ));
    }

    #[test]
    fn parses_edit_and_done() {
        let e =
            parse_action(r#"{"edit":{"file_path":"a.rs","new_content":"x\n","rationale":"r"}}"#)
                .unwrap();
        assert_eq!(
            e,
            Action::Edit {
                file_path: "a.rs".into(),
                new_content: "x\n".into(),
                rationale: "r".into()
            }
        );
        assert_eq!(
            parse_action(r#"{"done":{"summary":"all set"}}"#).unwrap(),
            Action::Done {
                summary: "all set".into()
            }
        );
    }

    #[test]
    fn tolerates_fences_and_surrounding_prose() {
        let fenced = "```json\n{\"tool\":\"grep\",\"args\":{\"pattern\":\"fn\"}}\n```";
        assert!(matches!(parse_action(fenced).unwrap(), Action::Tool { .. }));
        let chatty = "Sure, here you go:\n{\"done\":{\"summary\":\"ok\"}}\nHope that helps.";
        assert!(matches!(parse_action(chatty).unwrap(), Action::Done { .. }));
    }

    #[test]
    fn rejects_malformed_replies() {
        assert!(parse_action("no json here").is_err());
        assert!(parse_action("{not json}").is_err());
        assert!(parse_action(r#"{"chat":"hi"}"#).is_err());
        assert!(parse_action(r#"{"edit":{"file_path":"","new_content":"x"}}"#).is_err());
        assert!(parse_action(r#"{"edit":{"file_path":"a"}}"#).is_err());
    }

    /// Capacity-aware trigger (Checkpoint 2 item 6): a known window drives
    /// the decision from real capacity; no window falls back to bytes and
    /// says so; the max-floor lets EITHER the provider report OR the local
    /// estimate trip it.
    #[test]
    fn should_compact_uses_capacity_when_known_and_bytes_as_named_fallback() {
        // No window: byte heuristic, honestly labeled.
        assert_eq!(
            should_compact(CONTEXT_BYTES + 1, None, None),
            Some("byte-fallback")
        );
        assert_eq!(should_compact(CONTEXT_BYTES - 1, None, None), None);

        // 100k window → reserve 15k → threshold 85k tokens.
        let w = Some(100_000);
        // Provider-reported consumption trips it even with tiny local bytes.
        assert_eq!(should_compact(1_000, Some(90_000), w), Some("capacity"));
        // Local estimate trips it even with NO provider report
        // (300k bytes / 3 = 100k estimated tokens > 85k).
        assert_eq!(should_compact(300_000, None, w), Some("capacity"));
        // Under threshold on both signals: no compaction — and in
        // particular the old 64KB byte constant does NOT fire when the
        // window says there is plenty of room (150k bytes ≈ 50k tokens).
        assert_eq!(should_compact(150_000, Some(50_000), w), None);

        // Degenerate tiny window: anything at all compacts.
        assert_eq!(should_compact(10, None, Some(1_000)), Some("capacity"));
    }

    /// The stall ladder: identical failures climb warn → diagnose → halt;
    /// a success resets; canonical-JSON signatures ignore key order.
    #[test]
    fn stall_guard_ladder_escalates_and_resets() {
        let mut g = StallGuard::default();
        let args = serde_json::json!({"path": "x.txt", "limit": 5});
        let args_reordered = serde_json::json!({"limit": 5, "path": "x.txt"});

        assert_eq!(
            g.observe("read_file", &args, false, "no such file"),
            StallAction::Proceed
        );
        // Key order must not evade detection (hermes' canonical-json trick).
        assert!(matches!(
            g.observe("read_file", &args_reordered, false, "no such file"),
            StallAction::Warn(_)
        ));
        assert!(matches!(
            g.observe("read_file", &args, false, "no such file"),
            StallAction::Diagnose(_)
        ));
        // A success on this signature resets its failure streak.
        assert_eq!(
            g.observe("read_file", &args, true, "content"),
            StallAction::Proceed
        );
        assert_eq!(
            g.observe("read_file", &args, false, "no such file"),
            StallAction::Proceed
        );

        // Straight run to halt.
        let mut g = StallGuard::default();
        let mut last = StallAction::Proceed;
        for _ in 0..6 {
            last = g.observe("grep", &args, false, "boom");
        }
        match last {
            StallAction::Halt(msg) => assert!(msg.contains("stalled"), "{msg}"),
            other => panic!("6 identical failures must halt, got {other:?}"),
        }
    }

    /// Identical ok-calls with byte-identical results are no-progress:
    /// tolerated briefly, then diagnosed, then halted — while a CHANGING
    /// result never climbs the ladder at all (progress is progress).
    #[test]
    fn stall_guard_flags_identical_results_but_not_changing_ones() {
        let mut g = StallGuard::default();
        let args = serde_json::json!({"path": "log.txt"});
        for i in 0..10 {
            let out = format!("line {i}");
            assert_eq!(
                g.observe("read_file", &args, true, &out),
                StallAction::Proceed,
                "changing results are progress"
            );
        }

        let mut g = StallGuard::default();
        let mut saw_diagnose = false;
        let mut halted = None;
        for _ in 0..7 {
            match g.observe("read_file", &args, true, "same bytes") {
                StallAction::Diagnose(_) => saw_diagnose = true,
                StallAction::Halt(m) => {
                    halted = Some(m);
                    break;
                }
                _ => {}
            }
        }
        assert!(saw_diagnose, "identical results must reach a diagnose step");
        assert!(halted.is_some(), "and eventually halt with a named cause");
    }

    #[test]
    fn truncate_marks_and_respects_char_boundaries() {
        assert_eq!(truncate("abc", 10), "abc");
        let out = truncate(&"é".repeat(50), 10);
        assert!(out.starts_with(&"é".repeat(10)));
        assert!(out.contains("truncated"));
    }

    #[test]
    fn a_long_run_result_keeps_its_head_and_its_tail_around_an_exact_marker() {
        let body = format!(
            "exit_code: 1\nstdout:\n{}\ntest result: FAILED. 1 failed\nstderr:\nerror: boom",
            "x".repeat(50_000)
        );
        let out = tool_result_body("run", &body);
        let total = body.chars().count();
        let (head, tail) = (3984, 7968);
        assert!(out.chars().count() <= MAX_TOOL_RESULT);
        assert!(out.starts_with(&body[..head]), "head kept");
        assert!(out.ends_with(&body[body.len() - tail..]), "tail kept");
        assert!(out.ends_with("test result: FAILED. 1 failed\nstderr:\nerror: boom"));
        assert!(out.contains(&format!(
            "\n[… {} chars omitted …]\n",
            total - head - tail
        )));
    }

    #[test]
    fn short_output_is_untouched_and_other_tools_keep_their_head() {
        let short = "exit_code: 0\nstdout:\nok\nstderr:\n";
        assert_eq!(tool_result_body("run", short), short);
        let exact = "y".repeat(MAX_TOOL_RESULT);
        assert_eq!(clip_middle(&exact, MAX_TOOL_RESULT), exact);
        let read = tool_result_body("read_file", &"z".repeat(MAX_TOOL_RESULT + 5));
        assert!(read.starts_with(&"z".repeat(MAX_TOOL_RESULT)));
        assert!(read.ends_with("truncated (12005 chars total)"));
    }

    #[test]
    fn tool_result_prefix_is_stable() {
        // The offline stub model detects tool results by this prefix.
        assert!(tool_result_message("list_files", true, "a\nb")
            .content
            .starts_with("TOOL RESULT"));
    }

    /// The failure that started this: a native turn died on a network error
    /// half-way, and the next session knew nothing of it. Its request, its
    /// words so far, its tool calls (folded by id; Bakery's own MCP record of
    /// the same call counted once) and its failure must all survive into the
    /// projection.
    #[test]
    fn prior_turns_keep_an_unfinished_turn_with_what_it_wrote_and_ran() {
        let events = [
            ("agent.turn.started", r#"{"instruction":"say hi","turn_id":"t1"}"#),
            ("agent.turn.done", r#"{"summary":"hi","turn_id":"t1"}"#),
            ("agent.turn.started", r#"{"instruction":"hide the tab strip","turn_id":"t2"}"#),
            ("agent.acp.text", r#"{"text":"Reading the shell. ","turn_id":"t2"}"#),
            ("agent.acp.tool_call", r#"{"turn_id":"t2","detail":{"toolCallId":"c1","title":"Read TitleBar.svelte","status":"pending"}}"#),
            ("agent.acp.tool_call", r#"{"turn_id":"t2","detail":{"toolCallId":"c2","title":"mcp__bakery__memory_read","status":"completed"}}"#),
            ("agent.tool.called", r#"{"tool":"memory_read","ok":true,"transport":"mcp"}"#),
            ("agent.acp.tool_call", r#"{"turn_id":"t2","detail":{"toolCallId":"c1","status":"completed"}}"#),
            ("agent.acp.text", r#"{"text":"Now editing.","turn_id":"t2"}"#),
            ("agent.turn.failed", r#"{"reason":"API Error: Can't reach the API server (ENOTFOUND)","turn_id":"t2"}"#),
        ];
        let turns = prior_turns(events);
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].end, TurnEnd::Done);
        assert_eq!(turns[0].text, "hi");
        let failed = &turns[1];
        assert_eq!(failed.instruction, "hide the tab strip");
        assert_eq!(failed.text, "Reading the shell. Now editing.");
        assert_eq!(
            failed.tools,
            ["Read TitleBar.svelte (completed)", "mcp__bakery__memory_read (completed)"]
        );
        assert!(matches!(&failed.end, TurnEnd::Failed(r) if r.contains("ENOTFOUND")));

        let note = unfinished_previous_turn_note(&turns).expect("the failed turn is briefed");
        assert!(note.contains("Human: hide the tab strip"), "{note}");
        assert!(note.contains("did not finish — it failed: API Error"), "{note}");
        assert!(note.contains("Now editing."), "{note}");
        assert!(
            note.contains("Tools it ran: Read TitleBar.svelte (completed), mcp__bakery__memory_read (completed)"),
            "{note}"
        );
        assert!(unfinished_previous_turn_note(&turns[..1]).is_none(), "a finished turn needs no note");
    }

    /// The Resume briefing and the catch-up must not tell a model about one
    /// call twice: Bakery's MCP record and the runtime's own row for the same
    /// call count once, and an MCP record nothing accounts for is kept.
    #[test]
    fn a_native_tool_call_and_its_mcp_record_are_counted_once() {
        let turns = prior_turns([
            ("agent.turn.started", r#"{"instruction":"look around","turn_id":"t1"}"#),
            ("agent.tool.called", r#"{"tool":"thread_list","ok":true,"transport":"mcp","turn_id":"t1"}"#),
            ("agent.acp.tool_call", r#"{"turn_id":"t1","detail":{"toolCallId":"c1","title":"mcp__bakery__thread_list","status":"completed"}}"#),
            ("agent.tool.called", r#"{"tool":"gates_list","ok":false,"transport":"mcp","turn_id":"t1"}"#),
        ]);
        assert_eq!(
            turns[0].tools,
            ["mcp__bakery__thread_list (completed)", "gates_list (failed)"],
            "the accounted call shows once; the unaccounted one is kept"
        );
        assert!(names_tool("mcp__bakery__thread_list", "thread_list"));
        assert!(names_tool("Calling thread_list", "thread_list"));
        assert!(!names_tool("mcp__bakery__thread_list", "list"));
        assert!(!names_tool("anything", ""));
    }

    /// A turn's notes from other conversations come back with it: its
    /// `agent.turn.started` holds the instruction from before they were
    /// added, so the catch-up after a session restart and the unfinished-turn
    /// brief read them from the turn's `message.read`, ahead of its request.
    #[test]
    fn a_prior_turn_keeps_the_notes_its_opening_carried() {
        let turns = prior_turns([
            ("agent.turn.started", r#"{"instruction":"carry on","turn_id":"t1"}"#),
            ("message.read", r#"{"turn_id":"t1","text":"[Note from w1 (s1), id msg_1]\nmain moved"}"#),
            ("agent.turn.failed", r#"{"turn_id":"t1","reason":"the network went away"}"#),
        ]);
        assert_eq!(turns[0].notes, "[Note from w1 (s1), id msg_1]\nmain moved");
        let shown = render_prior_turn(&turns[0], 4_000);
        assert!(shown.starts_with("Human: [Note from w1 (s1), id msg_1]\nmain moved\n\ncarry on\n"), "{shown}");
        let brief = unfinished_previous_turn_note(&turns).unwrap();
        assert!(brief.contains("main moved\n\ncarry on"), "{brief}");
        let plain = prior_turns([("agent.turn.started", r#"{"instruction":"carry on","turn_id":"t2"}"#)]);
        assert!(render_prior_turn(&plain[0], 4_000).starts_with("Human: carry on\n"));
    }

    /// What Stop did to each call is what the catch-up and the unfinished-
    /// turn brief say of it: never ran, or had started, whatever its teardown
    /// reported after; a call that finished after all finished.
    #[test]
    fn prior_turns_say_what_stop_did_to_each_call() {
        let turns = prior_turns([
            ("agent.turn.started", r#"{"instruction":"build","turn_id":"t1"}"#),
            ("agent.acp.tool_call", r#"{"event":"tool_call","turn_id":"t1","detail":{"toolCallId":"a","title":"make all","status":"in_progress"}}"#),
            ("agent.acp.tool_call", r#"{"event":"stopped_after","turn_id":"t1","detail":{"toolCallId":"a"}}"#),
            ("agent.acp.tool_call", r#"{"event":"stopped_before","turn_id":"t1","detail":{"toolCallId":"b","title":"rm -rf build"}}"#),
            ("agent.acp.tool_call", r#"{"event":"tool_call_update","turn_id":"t1","detail":{"toolCallId":"b","status":"failed"}}"#),
            ("agent.acp.tool_call", r#"{"event":"tool_call","turn_id":"t1","detail":{"toolCallId":"c","title":"Read a.rs","status":"pending"}}"#),
            ("agent.acp.tool_call", r#"{"event":"stopped_after","turn_id":"t1","detail":{"toolCallId":"c"}}"#),
            ("agent.acp.tool_call", r#"{"event":"tool_call_update","turn_id":"t1","detail":{"toolCallId":"c","status":"completed"}}"#),
            ("agent.tool.called", r#"{"tool":"run","ok":false,"stopped":"before","turn_id":"t1"}"#),
            ("agent.turn.cancelled", r#"{"turn_id":"t1"}"#),
        ]);
        assert_eq!(
            turns[0].tools,
            ["make all (stopped after it ran)", "rm -rf build (stopped before it ran)", "Read a.rs (completed)", "run (stopped before it ran)"]
        );
        let brief = unfinished_previous_turn_note(&turns).unwrap();
        assert!(brief.contains("the operator stopped it") && brief.contains("make all (stopped after it ran)"), "{brief}");
    }

    /// A command that did not finish (`/compact` a restart cut off) is no
    /// work of the agent's to pick back up: the catch-up says so, and the
    /// unfinished-turn brief leaves it out.
    #[test]
    fn a_command_that_did_not_finish_is_no_work_to_pick_up() {
        let turns = prior_turns([
            ("agent.turn.started", r#"{"instruction":"/compact keep the plan","turn_id":"t1"}"#),
            ("agent.turn.cancelled", r#"{"turn_id":"t1","reason":"The Bakery restarted while /compact was running"}"#),
        ]);
        let shown = render_prior_turn(&turns[0], 4_000);
        assert!(shown.starts_with("Human: /compact keep the plan\n(A command that did not finish."), "{shown}");
        assert!(!shown.contains("did not finish —"), "{shown}");
        assert_eq!(unfinished_previous_turn_note(&turns), None);
        let path = prior_turns([("agent.turn.started", r#"{"instruction":"/Users/me/notes.md says what to do","turn_id":"t2"}"#)]);
        assert!(unfinished_previous_turn_note(&path).is_some(), "a path is no command");
    }

    /// Older records carry no `turn_id`; they still pair, in order.
    #[test]
    fn prior_turns_pair_records_without_turn_ids_in_order() {
        let turns = prior_turns([
            ("agent.turn.started", r#"{"instruction":"one"}"#),
            ("agent.turn.done", r#"{"summary":"first"}"#),
            ("agent.turn.started", r#"{"instruction":"two"}"#),
        ]);
        assert_eq!(turns.len(), 2);
        assert_eq!((turns[0].text.as_str(), &turns[0].end), ("first", &TurnEnd::Done));
        assert_eq!(turns[1].end, TurnEnd::Open);
    }

    /// The old catch-up filled its budget oldest first, so one long reply
    /// early on pushed every newer turn out. Newest first now: the turn the
    /// conversation just came from always makes it, clipped if it must be.
    #[test]
    fn render_prior_turns_fills_the_budget_newest_first() {
        let turn = |n: usize, text: String| PriorTurn {
            turn_id: format!("t{n}"),
            instruction: format!("request {n}"),
            notes: String::new(),
            text,
            tools: Vec::new(),
            end: TurnEnd::Done,
        };
        let turns = vec![
            turn(1, "x".repeat(50_000)),
            turn(2, "second reply".into()),
            turn(3, "y".repeat(30_000)),
        ];
        let out = render_prior_turns(&turns, 10, 20_000);
        assert!(out.contains("request 3"), "the newest turn is kept");
        assert!(out.contains("Human: request 2\nAssistant: second reply"), "{}", &out[..200]);
        assert!(out.find("request 2") < out.find("request 3"), "oldest first on the page");
        assert!(out.chars().count() <= 20_100, "{}", out.chars().count());
        assert!(out.contains("earlier chars cut"), "a long reply keeps its end");
        assert_eq!(render_prior_turns(&[], 10, 20_000), "");
    }
}
