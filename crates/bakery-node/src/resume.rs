//! Resume: a turn carries on after whatever cut it off — a Bakery restart
//! or deploy, a network drop, a provider outage, the model proxy
//! restarting, its agent's process dying. It carries on from the Bakery's
//! own record (the Heart), never from a runtime's memory alone: a turn a
//! restart killed picks itself up at boot, and a passing failure is waited
//! out inside the turn instead of ending it.

use crate::node::Seat;
use crate::state::AppState;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// What cut a model call off, when it was something that passes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Cut {
    /// The network, the provider's load, a restart on the way: waited out
    /// for as long as it lasts.
    Passing,
    /// The agent's own process died or hung: tried again [`CRASH_TRIES`]
    /// times in a row only, since one that dies on this work may again.
    Crashed,
    /// The door had no sign-in to put in (signed out, or one it couldn't
    /// renew): that lasts until the operator signs in, and the runtime has
    /// already retried it for minutes by the time it reaches the Bakery, so
    /// the turn ends at once with [`SIGNED_OUT`].
    SignedOut,
}

/// Tries in a row after the agent's process died, before the turn fails.
pub(crate) const CRASH_TRIES: u32 = 3;
/// What a turn with no sign-in ends with, instead of waiting forever.
pub(crate) const SIGNED_OUT: &str = "This turn stopped: the sign-in its model uses isn't available on this Mac (signed out, or it couldn't be renewed). Sign in again, then send this again.";
/// Restarts in a row that may cut the same work off and have it carry on by
/// itself; one more, and it waits for the operator (a node that dies on
/// this work would otherwise die on it forever).
pub(crate) const RESTART_RESUMES: usize = 3;

/// Whether a failed model call is worth waiting out, and how.
pub(crate) fn cut(error: &str) -> Option<Cut> {
    if error.contains(NOT_RESUMED_ERROR) {
        return None;
    }
    let lower = error.to_ascii_lowercase();
    // The door's own refusal (swap.rs), whatever status a runtime shows it
    // under: a 503 that won't pass by itself.
    if ["sign-in isn't available", "sign-in isn\\u0027t available", "sign-in isn\u{2019}t available"]
        .iter()
        .any(|m| lower.contains(m))
    {
        return Some(Cut::SignedOut);
    }
    // Out of quota, not just busy: the turn ends, so the Baker can switch
    // its model and hand it over (`model_proxy::hand_over`), or let it wait.
    let quota = lower.contains("model_cooldown")
        || lower.contains("usage limit")
        || lower.contains("limit reached")
        || lower.contains("hit your limit")
        || (lower.contains("quota") && !lower.contains("per minute") && !lower.contains("perminute"));
    if quota {
        return None;
    }
    // A provider's HTTP status ("Anthropic API 529: …").
    if let Some(status) = api_status(&lower) {
        return matches!(status, 408 | 425 | 429 | 500..=599).then_some(Cut::Passing);
    }
    // A native runtime's own word for it (claude-agent-acp's `errorKind`).
    if let Some(kind) = error_kind(&lower) {
        match kind {
            "rate_limit" | "overloaded" | "server_error" | "api_error" | "timeout" | "connection_error" => {
                return Some(Cut::Passing)
            }
            "unknown" => {}
            _ => return None,
        }
    }
    const CRASHED: &[&str] = &[
        "transport closed",
        "closed before replying",
        "channel closed",
        "prompt timed out",
    ];
    if CRASHED.iter().any(|m| lower.contains(m)) {
        return Some(Cut::Crashed);
    }
    const PASSING: &[&str] = &[
        "stream request:",
        "stream ended",
        "connection",
        "timed out",
        "dns",
        "lookup address",
        "network",
        "unreachable",
        "reset by peer",
        "broken pipe",
        "unexpected eof",
        "overloaded",
        "rate limit",
        "bad gateway",
        "gateway timeout",
        "econnrefused",
        "econnreset",
        "enotfound",
        "etimedout",
        "eai_again",
        "socket hang up",
        "fetch failed",
    ];
    PASSING.iter().any(|m| lower.contains(m)).then_some(Cut::Passing)
}

/// "… API 503: …", "API Error: 529 …".
fn api_status(lower: &str) -> Option<u16> {
    [" api ", "api error: "].iter().find_map(|marker| {
        lower.match_indices(marker).find_map(|(at, _)| {
            let digits: String = lower[at + marker.len()..].chars().take(3).collect();
            (digits.len() == 3 && digits.bytes().all(|b| b.is_ascii_digit()))
                .then(|| digits.parse().ok())
                .flatten()
        })
    })
}

/// `"errorKind": "overloaded"` in an ACP error's data.
fn error_kind(lower: &str) -> Option<&str> {
    let rest = &lower[lower.find("\"errorkind\": \"")? + 14..];
    Some(&rest[..rest.find('"')?])
}

/// How long before the next try: 2 s, doubling to a minute.
// ponytail: no jitter; add some if many threads hammer one provider back.
pub(crate) fn backoff(attempt: u32) -> Duration {
    if cfg!(test) {
        return Duration::from_millis(10);
    }
    Duration::from_secs((2u64 << attempt.min(5)).min(60))
}

/// Waits `how_long`, or until the turn is stopped: whether it was.
pub(crate) fn wait(how_long: Duration, cancel: &AtomicBool) -> bool {
    let until = Instant::now() + how_long;
    while !cancel.load(Ordering::Acquire) {
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return false;
        }
        std::thread::sleep(left.min(Duration::from_millis(100)));
    }
    true
}

/// A turn waiting out a passing failure: what cut it off, and when it tries
/// again (`{agent_id, turn_id, model_id, reason, attempt, next_try_at}`).
pub(crate) const RETRYING: &str = "agent.turn.retrying";

/// A passing failure inside a turn, said on its conversation (what cut it
/// off, when it tries again), then waited out. Whether the turn was stopped
/// meanwhile.
#[allow(clippy::too_many_arguments)]
pub(crate) fn wait_out(
    state: &AppState,
    stream_id: &str,
    agent_id: &str,
    turn_id: &str,
    model_id: &str,
    why: &str,
    attempt: u32,
    cancel: &AtomicBool,
) -> bool {
    let wait_for = backoff(attempt);
    let next = chrono::Utc::now() + chrono::Duration::from_std(wait_for).unwrap_or_default();
    let said = serde_json::json!({
        "agent_id": agent_id,
        "turn_id": turn_id,
        "model_id": model_id,
        "reason": crate::agent_loop::clip_head(why.trim(), 300),
        "attempt": attempt + 1,
        "next_try_at": next.to_rfc3339(),
    });
    let _ = crate::state::append(state, stream_id, "node", RETRYING, said.to_string(), vec![]);
    // Its live text so far goes: the step is asked again from the start.
    crate::state::set_agent_phase(state, agent_id, crate::state::ActivityPhase::ContactingModel);
    wait(wait_for, cancel)
}

/// Every conversation's turn a restart cut off carries on by itself, as
/// Resume does it: closed, then continued by a fresh turn briefed from its
/// durable record. Returns the agents it took: their queued messages
/// follow once it has (the caller leaves them be).
pub(crate) fn on_boot(state: &AppState) -> HashSet<String> {
    let mut taken = HashSet::new();
    for (stream_id, seat) in cut_off(state) {
        taken.insert(crate::node::agent_id(&stream_id, seat));
        let state = state.clone();
        std::thread::spawn(move || {
            if let Err(e) = crate::routes::agent::resume_after_restart(&state, &stream_id, seat) {
                eprintln!("bakery-node: {stream_id} did not carry on after the restart: {e}");
            }
            crate::agent_control::schedule_dispatch(&state, &stream_id, seat);
        });
    }
    taken
}

/// A native runtime's session the Bakery holds: `{agent_id, model_id,
/// session_id, resumed}` on the agent's conversation, each time one opens.
pub(crate) const SESSION_OPENED: &str = "agent.session.opened";

/// Notes the native session `agent_id` runs on `model_id` now.
pub(crate) fn note_session(
    state: &AppState,
    stream_id: &str,
    agent_id: &str,
    model_id: &str,
    session_id: &str,
    resumed: bool,
) {
    let said = serde_json::json!({
        "agent_id": agent_id,
        "model_id": model_id,
        "session_id": session_id,
        "resumed": resumed,
    });
    let _ = crate::state::append(state, stream_id, "node", SESSION_OPENED, said.to_string(), vec![]);
}

/// A native session that did not resume: its turn ends there and waits for
/// the operator's choice ([`SESSION_CHOSEN`]): `{agent_id, model_id,
/// session_id, reason, awaiting: true}`.
pub(crate) const NOT_RESUMED: &str = "agent.session.not_resumed";
/// The operator's answer to [`NOT_RESUMED`]: `{agent_id, session_id,
/// choice}`, `retry` (resume it again) or `fresh` (a new session, briefed
/// from the thread).
pub(crate) const SESSION_CHOSEN: &str = "agent.session.chosen";
/// What a turn whose session didn't resume ends with. Never waited out:
/// the operator chooses.
pub(crate) const NOT_RESUMED_ERROR: &str = "Claude's last session didn't resume";

/// Notes `session_id` didn't resume, once, and only while the agent would
/// still resume it: once the operator answers, or a newer session takes
/// over, nothing asks about it again. A retry that fails asks anew.
pub(crate) fn note_not_resumed(state: &AppState, stream_id: &str, agent_id: &str, model_id: &str, session_id: &str, reason: &str) {
    let waiting = state.heart.lock().unwrap().read_stream(stream_id).ok().is_some_and(|events| {
        events
            .iter()
            .rev()
            .filter(|e| [NOT_RESUMED, SESSION_CHOSEN, SESSION_OPENED].contains(&e.family.as_str()))
            .find_map(|e| {
                let p: serde_json::Value = serde_json::from_str(&e.payload).ok()?;
                (p["agent_id"] == agent_id).then(|| e.family == NOT_RESUMED && p["session_id"] == session_id)
            })
            .unwrap_or(false)
    });
    if waiting || last_session(state, stream_id, agent_id, model_id).as_deref() != Some(session_id) {
        return;
    }
    let said = serde_json::json!({
        "agent_id": agent_id,
        "model_id": model_id,
        "session_id": session_id,
        "reason": crate::agent_loop::clip_head(reason.trim(), 300),
        "awaiting": true,
    });
    let _ = crate::state::append(state, stream_id, "node", NOT_RESUMED, said.to_string(), vec![]);
}

/// The native session `agent_id` last ran on `model_id`, to resume: none
/// once the operator chose to start fresh after it.
pub(crate) fn last_session(state: &AppState, stream_id: &str, agent_id: &str, model_id: &str) -> Option<String> {
    let events = state.heart.lock().unwrap().read_stream(stream_id).ok()?;
    events.iter().rev().filter(|e| e.family == SESSION_OPENED || e.family == SESSION_CHOSEN).find_map(|e| {
        let p: serde_json::Value = serde_json::from_str(&e.payload).ok()?;
        if p["agent_id"] != agent_id {
            return None;
        }
        match e.family.as_str() {
            SESSION_CHOSEN if p["choice"] == "fresh" => Some(None),
            SESSION_OPENED if p["model_id"] == model_id => Some(p["session_id"].as_str().map(str::to_string)),
            _ => None,
        }
    })?
}

/// Whether `session_id` can hold a conversation its runtime kept: it moved
/// in, or was resumed, or something was asked in it (a turn was under way
/// when it opened, or one ended or was cut off since). A runtime keeps a
/// session only from its first message (Claude Code saves nothing before),
/// so resuming one never asked anything could only fail: a fresh one takes
/// its place, with nothing to ask. Read from the Bakery's own turn records,
/// which every runtime's turns leave alike.
pub(crate) fn kept(state: &AppState, stream_id: &str, agent_id: &str, session_id: &str) -> bool {
    let Ok(events) = state.heart.lock().unwrap().read_stream(stream_id) else {
        return true;
    };
    // The agent's turn under way, and whether the session has opened yet.
    let (mut running, mut opened) = (None, false);
    for e in &events {
        let Ok(p) = serde_json::from_str::<serde_json::Value>(&e.payload) else {
            continue;
        };
        if e.family == crate::move_in::IMPORTED && p["session_id"] == session_id {
            return true;
        }
        if p["agent_id"] != agent_id {
            continue;
        }
        let ended = matches!(
            e.family.as_str(),
            "agent.turn.done" | "agent.turn.failed" | "agent.turn.exhausted" | "agent.turn.cancelled"
        );
        match e.family.as_str() {
            SESSION_OPENED if p["session_id"] == session_id => {
                if running.is_some() || p["resumed"] == true {
                    return true;
                }
                opened = true;
            }
            _ if opened && (ended || e.family == RETRYING) => return true,
            "agent.turn.started" => running = p["turn_id"].as_str().map(str::to_string),
            _ if ended && running.as_deref() == p["turn_id"].as_str() => running = None,
            _ => {}
        }
    }
    false
}

/// Where a native runtime keeps its sessions: its seat's home, and the
/// session folders its environment points outside it (Claude's one shared
/// config, Codex's home), each by the name it has there.
pub(crate) fn session_roots(seat_home: &Path, env: &[(String, String)]) -> Vec<(String, PathBuf)> {
    let mut roots = vec![("HOME".to_string(), seat_home.to_path_buf())];
    for (key, value) in env {
        if !["CLAUDE_CONFIG_DIR", "CODEX_HOME", "AGY_CONVERSATIONS_DIR"].contains(&key.as_str()) {
            continue;
        }
        let path = PathBuf::from(value.replace(bakery_exec::ROOM_HOME_ENV_VALUE, &seat_home.to_string_lossy()));
        if path.is_absolute() && !path.starts_with(seat_home) {
            roots.push((key.clone(), path));
        }
    }
    roots
}

/// The Bakery's own copy of one native session: `<base>/sessions/<agent>/<session>/<root name>/…`.
pub(crate) fn session_copy(state: &AppState, agent_id: &str, session_id: &str) -> PathBuf {
    let safe = |s: &str| -> String {
        s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect()
    };
    state.base.join("sessions").join(safe(agent_id)).join(safe(session_id))
}

/// Every file under `root` a runtime keeps for `session_id`: one named for
/// it, or anything in a folder named for it. Bounded, and past the folders
/// that only hold tools and caches.
fn session_files(root: &Path, session_id: &str) -> Vec<PathBuf> {
    const PASSED: &[&str] = &["node_modules", ".git", ".cache", ".npm", ".cargo", ".rustup", "target", "Library", "tmp"];
    let mut found = Vec::new();
    let mut seen = 0;
    let mut todo = vec![(root.to_path_buf(), 0, false)];
    while let Some((dir, depth, inside)) = todo.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            seen += 1;
            if seen > 20_000 {
                return found;
            }
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            let named = inside || name.contains(session_id);
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() && depth < 7 && !PASSED.contains(&name.as_str()) {
                todo.push((path, depth + 1, named));
            } else if kind.is_file() && named {
                if let Ok(rel) = path.strip_prefix(root) {
                    found.push(rel.to_path_buf());
                }
            }
        }
    }
    found
}

/// Copies what a runtime keeps of `session_id` into the Bakery's own copy,
/// where it has changed: the Bakery holds the session, not only the runtime.
pub(crate) fn keep_session(state: &AppState, agent_id: &str, session_id: &str, roots: &[(String, PathBuf)]) {
    let copy = session_copy(state, agent_id, session_id);
    for (name, root) in roots {
        for rel in session_files(root, session_id) {
            let (from, to) = (root.join(&rel), copy.join(name).join(&rel));
            let size = |p: &Path| std::fs::metadata(p).map(|m| m.len()).ok();
            if size(&to).is_some() && size(&to) == size(&from) {
                continue;
            }
            if let Some(parent) = to.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::copy(&from, &to);
        }
    }
}

/// One compaction a native session's own transcript records (Claude Code's
/// `compact_boundary`): its id there, when, and the context's size in
/// tokens when it was compacted, when the transcript says.
pub(crate) struct KeptCompaction {
    pub(crate) uuid: String,
    pub(crate) at: chrono::DateTime<chrono::Utc>,
    pub(crate) tokens: Option<u64>,
}

/// The compactions the Bakery's copy of `session_id` records, oldest first:
/// its transcript (`<session>.jsonl`) read as Claude Code writes it, as a
/// moved-in session's history reads it (`move_in_history`).
pub(crate) fn kept_compactions(state: &AppState, agent_id: &str, session_id: &str) -> Vec<KeptCompaction> {
    let copy = session_copy(state, agent_id, session_id);
    let transcript = std::ffi::OsString::from(format!("{session_id}.jsonl"));
    let mut found = Vec::new();
    for rel in session_files(&copy, "") {
        if rel.file_name() != Some(transcript.as_os_str()) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(copy.join(&rel)) else {
            continue;
        };
        for line in text.lines().filter(|line| line.contains("compact_boundary")) {
            let Ok(entry) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            let at = entry["timestamp"].as_str().and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok());
            let (Some(uuid), Some(at)) = (entry["uuid"].as_str(), at) else {
                continue;
            };
            if entry["type"] == "system" && entry["subtype"] == "compact_boundary" {
                let tokens = entry["compactMetadata"]["preTokens"].as_u64();
                found.push(KeptCompaction { uuid: uuid.to_string(), at: at.with_timezone(&chrono::Utc), tokens });
            }
        }
    }
    found.sort_by_key(|c| c.at);
    found
}

/// Puts back what the runtime lost of `session_id` (its files deleted, its
/// folders moved) from the Bakery's own copy, before it resumes it.
pub(crate) fn restore_session(state: &AppState, agent_id: &str, session_id: &str, roots: &[(String, PathBuf)]) {
    let copy = session_copy(state, agent_id, session_id);
    for (name, root) in roots {
        let kept = copy.join(name);
        for rel in session_files(&kept, "") {
            let to = root.join(&rel);
            if to.exists() {
                continue;
            }
            if let Some(parent) = to.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::copy(kept.join(&rel), &to);
        }
    }
}

/// How a turn ended, for counting restarts in a row.
#[derive(Clone, Copy, PartialEq)]
enum Ending {
    /// A restart cut it off, and it carried on by itself.
    Restarted,
    Other,
}

/// An agent's newest turn with no end: a restart killed it.
struct Dead {
    agent_id: String,
    seat: Seat,
    turn_id: String,
    /// Restarts in a row that cut this agent's work off before this one.
    streak: usize,
}

/// Each agent's turn a restart killed, in one stream's events.
fn dead_turns(events: &[bakery_core::Event]) -> Vec<Dead> {
    // Per agent: its seat, and its turns in order; how each one ended.
    let mut agents: HashMap<String, (Seat, Vec<String>)> = HashMap::new();
    let mut endings: HashMap<String, Ending> = HashMap::new();
    for e in events {
        let Ok(p) = serde_json::from_str::<serde_json::Value>(&e.payload) else {
            continue;
        };
        let Some(turn) = p["turn_id"].as_str() else {
            continue;
        };
        match e.family.as_str() {
            "agent.turn.started" => {
                let (Some(agent), Ok(seat)) = (
                    p["agent_id"].as_str(),
                    serde_json::from_value::<Seat>(p["seat"].clone()),
                ) else {
                    continue;
                };
                let entry = agents.entry(agent.to_string()).or_insert((seat, Vec::new()));
                entry.1.push(turn.to_string());
            }
            "agent.turn.done" | "agent.turn.failed" | "agent.turn.exhausted" | "agent.turn.cancelled" => {
                let restarted = p["closed_by_resume"] == true && p["after_restart"] == true;
                let ending = if restarted { Ending::Restarted } else { Ending::Other };
                // Any other end of the same turn wins: it truly ended.
                let had = endings.entry(turn.to_string()).or_insert(ending);
                if ending == Ending::Other {
                    *had = Ending::Other;
                }
            }
            _ => {}
        }
    }
    let mut dead = Vec::new();
    for (agent_id, (seat, turns)) in agents {
        let Some((newest, before)) = turns.split_last() else {
            continue;
        };
        if endings.contains_key(newest) {
            continue;
        }
        let streak = before
            .iter()
            .rev()
            .take_while(|t| endings.get(*t) == Some(&Ending::Restarted))
            .count();
        dead.push(Dead { agent_id, seat, turn_id: newest.clone(), streak });
    }
    dead
}

/// Each conversation agent whose newest turn has no end: a restart killed
/// it. Not a task's (its driver carries on for it), nor one in a thread put
/// away, nor one restarts already cut off [`RESTART_RESUMES`] times in a
/// row, which waits for the operator.
fn cut_off(state: &AppState) -> Vec<(String, Seat)> {
    let heart = state.heart.lock().unwrap();
    let mut found = Vec::new();
    for stream in heart.list_streams().unwrap_or_default() {
        let sid = stream.stream_id;
        if sid.starts_with("sys:")
            || sid.starts_with("probe:")
            || crate::state::socket_task_id(&heart, &sid).is_some()
            || crate::state::discarded_of(&heart, &sid)
            || crate::agents::socket_archived_locked(&heart, &sid)
            || crate::baker_threads::archived_locked(&heart, &sid)
        {
            continue;
        }
        let events = heart.read_stream(&sid).unwrap_or_default();
        for dead in dead_turns(&events) {
            if dead.streak < RESTART_RESUMES {
                found.push((sid.clone(), dead.seat));
            }
        }
    }
    found
}

/// A task's turn a restart cut off, closed as Resume closes one so its
/// driver carries the task on. The restarts in a row that cut its work off
/// before this one; none when no turn was running.
pub(crate) fn close_after_restart(state: &AppState, stream_id: &str) -> Option<usize> {
    let events = state.heart.lock().unwrap().read_stream(stream_id).ok()?;
    let mut streak = None;
    for dead in dead_turns(&events) {
        let said = serde_json::json!({
            "reason": "the Bakery restarted while this turn was running; the task carries on from its record",
            "phase": "tools",
            "agent_id": dead.agent_id,
            "turn_id": dead.turn_id,
            "closed_by_resume": true,
            "after_restart": true,
        });
        let _ = crate::state::append(state, stream_id, "node", "agent.turn.failed", said.to_string(), vec![]);
        streak = streak.max(Some(dead.streak));
    }
    streak
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_passes_is_waited_out_and_what_is_wrong_is_not() {
        for passing in [
            "Anthropic API 529: {\"type\":\"overloaded_error\"}",
            "xAI API 503: upstream unavailable",
            "Gemini API 429: Quota exceeded for metric: requests per minute",
            "Anthropic API 429: rate_limit_error: would exceed your rate limit of 40000 input tokens per minute",
            "anthropic stream request: Connection Failed: Connect error: Connection refused (os error 61)",
            "gemini stream request: Dns Failed: resolve dns name 'generativelanguage.googleapis.com:443'",
            "model stream ended unexpectedly: reset (partial reply: \"Hel\")",
            "ACP session/prompt failed: Internal error: {\n  \"errorKind\": \"overloaded\"\n}",
            "ACP session/prompt failed: Internal error: API Error: Connection error.",
            "ACP session/prompt failed: Internal error: API Error: 500 {\"type\":\"api_error\"}",
        ] {
            assert_eq!(cut(passing), Some(Cut::Passing), "{passing}");
        }
        for crashed in [
            "ACP transport closed during prompt",
            "ACP session closed before replying",
            "ACP session/prompt timed out after 3600s",
        ] {
            assert_eq!(cut(crashed), Some(Cut::Crashed), "{crashed}");
        }
        for signed_out in [
            "ACP session/prompt failed: Internal error: API Error: 503 the operator's claude sign-in isn't available · This is usually temporary",
            "Anthropic API 503: the operator's claude sign-in isn't available",
            "ACP session/prompt failed: Internal error: {\"message\": \"the operator's claude sign-in isn\\u0027t available\"}",
        ] {
            assert_eq!(cut(signed_out), Some(Cut::SignedOut), "{signed_out}");
        }
        for wrong in [
            "Anthropic API 400: prompt is too long",
            "Anthropic API 401: invalid x-api-key",
            "xAI API 404: model not found",
            "ACP session/prompt failed: Internal error: {\n  \"errorKind\": \"authentication_failed\"\n}",
            "ACP session/prompt failed: Internal error: {\n  \"errorKind\": \"billing_error\"\n}",
            "ACP output cap exceeded",
            "Setup needed: add an Anthropic key",
            "xAI API 429: {\"error\":{\"code\":\"model_cooldown\"}}",
            "Gemini API 429: RESOURCE_EXHAUSTED: Quota exceeded for metric: requests per day",
            "ACP session/prompt failed: Internal error: Claude AI usage limit reached|1760000000: {\n  \"errorKind\": \"rate_limit\"\n}",
        ] {
            assert_eq!(cut(wrong), None, "{wrong}");
        }
    }

    #[test]
    fn a_stopped_turn_stops_waiting() {
        assert!(wait(Duration::from_secs(60), &AtomicBool::new(true)));
        assert!(!wait(Duration::from_millis(20), &AtomicBool::new(false)));
    }

    use crate::routes::project::open_stream;
    use crate::testutil::*;
    use std::sync::Arc;

    fn started(state: &AppState, sid: &str, turn: &str, instruction: &str) {
        let agent_id = crate::node::agent_id(sid, Seat::Orchestrator);
        let p = serde_json::json!({"instruction": instruction, "agent_id": agent_id, "turn_id": turn, "mode": "manual", "seat": "orchestrator"});
        crate::state::append(state, sid, "human", "agent.turn.started", p.to_string(), vec![]).unwrap();
    }

    fn closed_after_restart(state: &AppState, sid: &str, turn: &str) {
        let agent_id = crate::node::agent_id(sid, Seat::Orchestrator);
        let p = serde_json::json!({"reason": "restart", "agent_id": agent_id, "turn_id": turn, "closed_by_resume": true, "after_restart": true});
        crate::state::append(state, sid, "node", "agent.turn.failed", p.to_string(), vec![]).unwrap();
    }

    /// A turn the Bakery's restart killed carries on by itself, briefed from
    /// its record, and a message queued for it before the restart follows.
    #[test]
    fn a_turn_a_restart_cut_off_carries_on_and_its_queue_follows() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let model = Arc::new(ScriptedModel::new(&["carried on", "tidied"]));
        let state = state_with(dir.path(), model.clone());
        let sid = open_stream(&state, &open_req(&repo, "resume")).unwrap().context.stream_id;
        let agent_id = crate::node::agent_id(&sid, Seat::Orchestrator);
        started(&state, &sid, "turn_dead", "refactor the parser");
        let mode = serde_json::to_value(crate::agent_control::OperatingMode::Manual).unwrap();
        let queued = serde_json::json!({"agent_id": agent_id, "seat": "orchestrator", "instruction": "then tidy the tests", "mode": mode});
        let family = crate::agent_control::FOLLOW_UP_QUEUED;
        crate::state::append(&state, &sid, "human", family, queued.to_string(), vec![]).unwrap();

        let taken = on_boot(&state);
        assert_eq!(taken, HashSet::from([agent_id.clone()]));
        crate::agent_control::recover_on_boot(&state, &taken);
        let events = |family: &str| state.heart.lock().unwrap().events_of_family(&sid, family).unwrap();
        // What the thread did, and when: where the time went if it ran out.
        let timeline = || {
            let all = state.heart.lock().unwrap().read_stream(&sid).unwrap();
            all.iter().map(|e| format!("{} {}", e.ts, e.family)).collect::<Vec<_>>().join("\n")
        };
        let deadline = Instant::now() + Duration::from_secs(60);
        while events("agent.turn.done").len() < 2 {
            assert!(Instant::now() < deadline, "both turns never finished:\n{}", timeline());
            std::thread::sleep(Duration::from_millis(50));
        }
        let asked: Vec<String> = events("agent.turn.started")
            .iter()
            .map(|e| serde_json::from_str::<serde_json::Value>(&e.payload).unwrap()["instruction"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(asked.len(), 3, "{asked:?}");
        assert!(asked[1].starts_with("RESUME after") && asked[1].contains("refactor the parser"), "{}", asked[1]);
        assert_eq!(asked[2], "then tidy the tests", "the queue waited for it");
        let closer = events("agent.turn.failed").into_iter().find(|e| e.payload.contains("turn_dead")).unwrap();
        let closer: serde_json::Value = serde_json::from_str(&closer.payload).unwrap();
        assert_eq!((closer["closed_by_resume"].as_bool(), closer["after_restart"].as_bool()), (Some(true), Some(true)));
    }

    /// The turn a restart cut off carries on with the model and effort its
    /// selector shows, on the Bakery's own loop too, where no session keeps
    /// them between turns.
    #[test]
    fn a_turn_a_restart_cut_off_carries_on_with_its_selectors_model_and_effort() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(bakery_agent::StubModel));
        let sid = open_stream(&state, &open_req(&repo, "resume-effort")).unwrap().context.stream_id;
        let usage = bakery_agent::TurnUsage { tokens_in: Some(1), tokens_out: Some(1) };
        crate::state::seed_seat_model_for_test(&state, &sid, Seat::Orchestrator, Arc::new(UsageReportingModel::new("ok", Some(usage))));
        let req = crate::routes::agent::AgentTurnReq {
            stream_id: sid.clone(),
            file_path: None,
            instruction: "refactor the parser".to_string(),
            seat: Some(Seat::Orchestrator),
            model_id: None,
            effort: None,
            actor: None,
        };
        for (category, value) in [(crate::selection::MODEL, "opus"), (crate::selection::EFFORT, "high")] {
            crate::selection::choose(&state, &sid, Seat::Orchestrator, "", category, Some(value), None, "human").unwrap();
        }
        crate::routes::agent::agent_turn_inner(&state, &req).unwrap();
        // The same turn again, as it started; the restart killed it there.
        let events = |family: &str| state.heart.lock().unwrap().events_of_family(&sid, family).unwrap();
        let mut cut: serde_json::Value = serde_json::from_str(&events("agent.turn.started")[0].payload).unwrap();
        cut["turn_id"] = "turn_cut".into();
        crate::state::append(&state, &sid, "human", "agent.turn.started", cut.to_string(), vec![]).unwrap();

        let taken = on_boot(&state);
        crate::agent_control::recover_on_boot(&state, &taken);
        let deadline = Instant::now() + Duration::from_secs(60);
        while events("agent.turn.usage").len() < 2 {
            assert!(Instant::now() < deadline, "the cut-off turn never carried on");
            std::thread::sleep(Duration::from_millis(50));
        }
        let ran: serde_json::Value = serde_json::from_str(&events("agent.turn.usage")[1].payload).unwrap();
        assert_eq!((ran["model_id"].as_str(), ran["effort"].as_str()), (Some("opus"), Some("high")), "{ran}");
    }

    /// A command (`/compact`) the restart cut off is not carried on: no
    /// briefing runs it as text. Its turn ends with a note to send it again,
    /// and the message queued behind it follows.
    #[test]
    fn a_command_a_restart_cut_off_is_sent_again_not_carried_on() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let model = Arc::new(ScriptedModel::new(&["tidied"]));
        let state = state_with(dir.path(), model.clone());
        let sid = open_stream(&state, &open_req(&repo, "resume-command")).unwrap().context.stream_id;
        let agent_id = crate::node::agent_id(&sid, Seat::Orchestrator);
        started(&state, &sid, "turn_compact", "/compact keep the plan");
        let mode = serde_json::to_value(crate::agent_control::OperatingMode::Manual).unwrap();
        let queued = serde_json::json!({"agent_id": agent_id, "seat": "orchestrator", "instruction": "then tidy the tests", "mode": mode});
        crate::state::append(&state, &sid, "human", crate::agent_control::FOLLOW_UP_QUEUED, queued.to_string(), vec![]).unwrap();

        let taken = on_boot(&state);
        crate::agent_control::recover_on_boot(&state, &taken);
        let events = |family: &str| state.heart.lock().unwrap().events_of_family(&sid, family).unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        while events("agent.turn.done").is_empty() {
            assert!(Instant::now() < deadline, "the queued message never ran");
            std::thread::sleep(Duration::from_millis(50));
        }
        let asked: Vec<String> = events("agent.turn.started")
            .iter()
            .map(|e| serde_json::from_str::<serde_json::Value>(&e.payload).unwrap()["instruction"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(asked, ["/compact keep the plan", "then tidy the tests"], "nothing carried the command on as text");
        let closed = events("agent.turn.cancelled");
        assert_eq!(closed.len(), 1);
        let closed: serde_json::Value = serde_json::from_str(&closed[0].payload).unwrap();
        assert_eq!((closed["turn_id"].as_str(), closed["after_restart"].as_bool()), (Some("turn_compact"), Some(true)));
        assert!(closed["reason"].as_str().unwrap().contains("send /compact again"), "{closed}");
        assert!(events("agent.turn.failed").is_empty(), "no resume closer either");
    }

    /// A model call cut off by the network or the provider's load is waited
    /// out inside the turn, which then finishes; a wrong request still fails
    /// at once.
    #[test]
    fn a_passing_failure_is_waited_out_inside_the_turn() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let model = Arc::new(ScriptedModel::new(&[
            "ERR:Anthropic API 529: overloaded",
            "ERR:anthropic stream request: Connection Failed: Connect error: Connection refused",
            "done after the outage",
            "ERR:Anthropic API 401: invalid x-api-key",
        ]));
        let state = state_with(dir.path(), model.clone());
        let sid = open_stream(&state, &open_req(&repo, "outage")).unwrap().context.stream_id;
        let ask = |instruction: &str| {
            crate::routes::agent::agent_turn_inner(
                &state,
                &crate::routes::agent::AgentTurnReq {
                    stream_id: sid.clone(),
                    file_path: None,
                    instruction: instruction.to_string(),
                    seat: None,
                    model_id: None,
                    effort: None,
                    actor: None,
                },
            )
        };
        let done = ask("ship it").unwrap();
        assert_eq!(done.family, "agent.turn.done");
        let retries = state.heart.lock().unwrap().events_of_family(&sid, RETRYING).unwrap();
        let attempts: Vec<u64> = retries
            .iter()
            .map(|e| serde_json::from_str::<serde_json::Value>(&e.payload).unwrap()["attempt"].as_u64().unwrap())
            .collect();
        assert_eq!(attempts, vec![1, 2]);
        assert!(retries[0].payload.contains("529"), "{}", retries[0].payload);

        assert!(ask("again").is_err(), "a wrong key fails at once");
        assert_eq!(model.seen.lock().unwrap().len(), 4);
        let retries = state.heart.lock().unwrap().events_of_family(&sid, RETRYING).unwrap();
        assert_eq!(retries.len(), 2, "no wait for a wrong request");
    }

    /// The rehearsal's hang: with no sign-in the door answers 503, which used
    /// to be waited out forever, then tried again on top of the runtime's own
    /// minutes of retries. Now the turn ends at the first such answer, with a
    /// plain error.
    #[test]
    fn a_missing_sign_in_ends_the_turn_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let refused = "ERR:Anthropic API 503: the operator's claude sign-in isn't available";
        let model = Arc::new(ScriptedModel::new(&[refused; 6]));
        let state = state_with(dir.path(), model.clone());
        let sid = open_stream(&state, &open_req(&repo, "signed-out")).unwrap().context.stream_id;
        let (_, failed) = crate::routes::agent::agent_turn_inner(
            &state,
            &crate::routes::agent::AgentTurnReq {
                stream_id: sid.clone(),
                file_path: None,
                instruction: "ship it".to_string(),
                seat: None,
                model_id: None,
                effort: None,
                actor: None,
            },
        )
        .unwrap_err();
        assert!(failed.contains(SIGNED_OUT), "{failed}");
        let retries = state.heart.lock().unwrap().events_of_family(&sid, RETRYING).unwrap();
        assert!(retries.is_empty(), "no wait on top of the runtime's own");
        assert_eq!(model.seen.lock().unwrap().len(), 1);
    }

    /// A conversation handed from another model to one the Bakery runs
    /// itself gets its prior turns in full, not hot memory's one line.
    #[test]
    fn a_conversation_handed_over_brings_its_prior_turns() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let model = Arc::new(ScriptedModel::new(&["picked it up", "and again"]));
        let state = state_with(dir.path(), model.clone());
        let sid = open_stream(&state, &open_req(&repo, "handover")).unwrap().context.stream_id;
        let agent_id = crate::node::agent_id(&sid, Seat::Orchestrator);
        started(&state, &sid, "t0", "design the schema");
        let done = serde_json::json!({"agent_id": agent_id, "turn_id": "t0", "model_id": "acp:claude", "summary": "Schema has three tables: farms, fields, crops."});
        crate::state::append(&state, &sid, "agent:orchestrator", "agent.turn.done", done.to_string(), vec![]).unwrap();
        let ask = |text: &str| {
            let req = crate::routes::agent::AgentTurnReq {
                stream_id: sid.clone(),
                file_path: None,
                instruction: text.to_string(),
                seat: None,
                model_id: None,
                effort: None,
                actor: None,
            };
            crate::routes::agent::agent_turn_inner(&state, &req).unwrap();
        };
        ask("carry on");
        ask("next");
        let captured = model.captured.lock().unwrap();
        assert!(captured[0].1.contains("comes to you from another model"), "{}", captured[0].1);
        assert!(captured[0].1.contains("three tables: farms, fields, crops"), "{}", captured[0].1);
        assert!(!captured[1].1.contains("comes to you from another model"), "only once: the same model since");
    }

    /// A session that didn't resume is asked about once, and only while its
    /// answer can still be taken: answered fresh, or taken over by a newer
    /// session, it is never asked about again; a retry that fails asks anew,
    /// and so does the agent's own session on its model after another
    /// model's ran.
    #[test]
    fn a_session_is_asked_about_once_and_only_while_it_is_current() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(bakery_agent::StubModel));
        let sid = "wt:asked";
        let agent_id = crate::node::agent_id(sid, Seat::Orchestrator);
        let asked = || state.heart.lock().unwrap().events_of_family(sid, NOT_RESUMED).unwrap().len();
        let lost = |session: &str| note_not_resumed(&state, sid, &agent_id, "claude", session, "Resource not found");
        let chose = |choice: &str| {
            let p = serde_json::json!({"agent_id": agent_id, "session_id": "s1", "choice": choice});
            crate::state::append(&state, sid, "human", SESSION_CHOSEN, p.to_string(), vec![]).unwrap();
        };
        note_session(&state, sid, &agent_id, "claude", "s1", false);
        lost("s1");
        lost("s1");
        assert_eq!(asked(), 1, "one failure, one question");
        chose("retry");
        lost("s1");
        assert_eq!(asked(), 2, "a retry that fails asks anew");
        chose("fresh");
        lost("s1");
        assert_eq!(asked(), 2, "answered fresh");
        note_session(&state, sid, &agent_id, "claude", "s2", false);
        lost("s1");
        assert_eq!(asked(), 2, "a newer session took over");
        note_session(&state, sid, &agent_id, "grok-acp", "g1", false);
        lost("s2");
        assert_eq!(asked(), 3, "Claude's own session, after Grok's");
    }

    /// A session holds a conversation once something was asked in it, or it
    /// was resumed, or it moved in; one only opened (by the pane, say) holds
    /// none, even while the turn that would resume it starts.
    #[test]
    fn a_session_holds_a_conversation_once_something_was_asked_in_it() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(bakery_agent::StubModel));
        let sid = "wt:kept";
        let agent_id = crate::node::agent_id(sid, Seat::Orchestrator);
        let fact = |family: &str, p: serde_json::Value| {
            crate::state::append(&state, sid, "node", family, p.to_string(), vec![]).unwrap();
        };
        let turn = |family: &str, id: &str| fact(family, serde_json::json!({"agent_id": agent_id, "turn_id": id}));
        let has = |session: &str| kept(&state, sid, &agent_id, session);

        note_session(&state, sid, &agent_id, "claude", "pane", false);
        turn("agent.turn.started", "t1");
        assert!(!has("pane"), "opened by the pane; its first turn only starting");
        turn("agent.turn.done", "t1");
        assert!(has("pane"), "a turn ran in it");
        turn("agent.turn.started", "t2");
        note_session(&state, sid, &agent_id, "claude", "for-a-turn", false);
        assert!(has("for-a-turn"), "opened for the turn under way");
        turn("agent.turn.done", "t2");
        note_session(&state, sid, &agent_id, "claude", "cut", false);
        turn("agent.turn.started", "t3");
        turn(RETRYING, "t3");
        assert!(has("cut"), "a turn in it was cut off, and waited out");
        note_session(&state, sid, &agent_id, "claude", "back", true);
        assert!(has("back"), "resumed");
        fact(crate::move_in::IMPORTED, serde_json::json!({"session_id": "moved"}));
        assert!(has("moved"), "moved in");
    }

    /// Work that restarts cut off three times in a row waits for the
    /// operator instead of carrying on a fourth.
    #[test]
    fn restarts_in_a_row_leave_the_work_for_the_operator() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(bakery_agent::StubModel));
        let sid = open_stream(&state, &open_req(&repo, "loop")).unwrap().context.stream_id;
        for n in 0..2 {
            started(&state, &sid, &format!("t{n}"), "work");
            closed_after_restart(&state, &sid, &format!("t{n}"));
        }
        started(&state, &sid, "t2", "work");
        assert_eq!(cut_off(&state).len(), 1, "two in a row: it carries on");
        closed_after_restart(&state, &sid, "t2");
        started(&state, &sid, "t3", "work");
        assert!(cut_off(&state).is_empty(), "three in a row: the operator's");

        let done = serde_json::json!({"agent_id": crate::node::agent_id(&sid, Seat::Orchestrator), "turn_id": "t3", "summary": "ok"});
        crate::state::append(&state, &sid, "agent:orchestrator", "agent.turn.done", done.to_string(), vec![]).unwrap();
        started(&state, &sid, "t4", "next");
        assert_eq!(cut_off(&state).len(), 1, "a finished turn starts the count again");
    }
}
