//! A moved-in conversation's old conversation, to read: the transcript copy
//! its move-in made (the Claude Code session as it came in) as messages,
//! newest first, a page at a time. A page is read from the end of the file
//! backwards, so it costs its own size however long the transcript is. The
//! operator's alone, and read-only. Nothing passes unaccounted: what a page
//! reads past without showing is counted by kind, a line that can't be read
//! says so, a text cut short says how long it was. Keys and tokens are left
//! out of every text, and what a tool answered is never shown, only counted.

use crate::state::AppState;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Items a page holds when it isn't asked for a number, and the most.
const DEFAULT_LIMIT: usize = 40;
const MAX_LIMIT: usize = 200;
/// The longest a message is shown, in characters; a summary, a tool call's
/// short form.
const TEXT_MAX: usize = 6000;
const SUMMARY_MAX: usize = 12_000;
const CALL_MAX: usize = 200;
/// The most one request reads before it gives back what it has, so a long
/// run of lines with nothing to show cannot hold it up.
const SCAN_MAX: u64 = 64 << 20;
/// One read of the file, and the longest line read.
const CHUNK: u64 = 1 << 20;
const LINE_MAX: usize = 64 << 20;

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct MoveInHistoryQuery {
    /// The conversation that moved in.
    pub(crate) stream_id: String,
    /// The `earlier` of the page before this one; leave out for the newest.
    #[serde(default)]
    pub(crate) before: Option<u64>,
    /// About how many items (default 40, at most 200): a page ends on a
    /// whole transcript line, so it can hold a few more.
    #[serde(default)]
    pub(crate) limit: Option<usize>,
}

/// What an item is.
#[derive(schemars::JsonSchema, Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum HistoryKind {
    /// What the operator said.
    User,
    /// What Claude said.
    Assistant,
    /// A tool Claude called: its name, and its command, file or pattern.
    Tool,
    /// The conversation was compacted here.
    Compaction,
    /// The summary Claude carries on from after a compaction.
    Summary,
    /// A line that can't be read.
    Unreadable,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct HistoryItem {
    /// Unique in the transcript: where its line starts, and its place in it.
    pub(crate) id: String,
    pub(crate) kind: HistoryKind,
    /// When Claude Code recorded it.
    pub(crate) at: Option<String>,
    /// The words, keys and tokens left out.
    pub(crate) text: String,
    /// A tool call's tool; a compaction's trigger (auto or manual).
    pub(crate) name: Option<String>,
    /// How long the text was before it was cut, in characters.
    pub(crate) chars: Option<usize>,
    /// The text is cut short.
    pub(crate) cut: bool,
    /// How many keys and tokens were left out of the text.
    pub(crate) left_out: usize,
    /// A compaction's size before it, in tokens.
    pub(crate) tokens: Option<u64>,
}

/// What a page read past without showing.
#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct HistorySkipped {
    /// The entry's kind in the transcript: attachment, thinking, meta...
    pub(crate) kind: String,
    pub(crate) count: usize,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct MoveInHistory {
    pub(crate) stream_id: String,
    /// The Claude Code session this conversation moved in with; none when
    /// it never moved in.
    pub(crate) session_id: Option<String>,
    /// What it moved in as.
    pub(crate) title: Option<String>,
    /// Newest first.
    pub(crate) items: Vec<HistoryItem>,
    /// What this page read past without showing, by kind.
    pub(crate) skipped: Vec<HistorySkipped>,
    /// Pass as `before` for the page of earlier ones; none at the start.
    pub(crate) earlier: Option<u64>,
}

impl HistoryItem {
    fn new(at: u64, n: usize, kind: HistoryKind, when: Option<&str>) -> Self {
        HistoryItem {
            id: format!("{at}.{n}"),
            kind,
            at: when.map(str::to_string),
            text: String::new(),
            name: None,
            chars: None,
            cut: false,
            left_out: 0,
            tokens: None,
        }
    }

    /// Its text from `raw`: keys and tokens left out, cut at `max` characters.
    fn said(mut self, raw: &str, max: usize) -> Self {
        let total = raw.chars().count();
        // Cleaned before it is cut, so no key shows by its first half; a
        // text far past the cut is only read as far as it could show.
        let head: String = raw.chars().take(max * 8).collect();
        let (clean, left_out) = clean(&head);
        self.cut = total > max * 8 || clean.chars().count() > max;
        self.text = clean.chars().take(max).collect();
        self.chars = Some(total);
        self.left_out = left_out;
        self
    }
}

/// `text` without the keys and tokens in it, and how many were left out: a
/// credential's shape, the value of a secret-named flag, header or
/// assignment, a private key. By name and shape, not by entropy: a key with
/// no known prefix, under no secret name, is not caught.
fn clean(text: &str) -> (String, usize) {
    use crate::toolkit_port::{redact, secret_words, LEFT_OUT};
    // Shapes and private keys first, whole; their marks become one word, so
    // the named values after them don't tear a mark apart.
    const MARK: &str = "\u{e000}";
    let (shaped, mut count) = redact(text);
    let shaped = shaped.replace(LEFT_OUT, MARK);
    let mut out = String::with_capacity(shaped.len());
    for line in shaped.split_inclusive('\n') {
        let mut at: Vec<(usize, usize)> = Vec::new();
        let mut from = None;
        for (i, c) in line.char_indices() {
            match (c.is_whitespace(), from) {
                (false, None) => from = Some(i),
                (true, Some(start)) => {
                    at.push((start, i));
                    from = None;
                }
                _ => {}
            }
        }
        if let Some(start) = from {
            at.push((start, line.len()));
        }
        let words: Vec<String> = at.iter().map(|&(a, b)| line[a..b].to_string()).collect();
        let (mut left, _) = secret_words(&words);
        left.sort_unstable();
        left.dedup();
        let mut last = 0;
        for i in left {
            let Some(&(a, b)) = at.get(i) else { continue };
            out.push_str(&line[last..a]);
            out.push_str(MARK);
            last = b;
            // A word that already holds a mark was counted with it.
            count += usize::from(!line[a..b].contains(MARK));
        }
        out.push_str(&line[last..]);
    }
    (out.replace(MARK, LEFT_OUT), count)
}

/// A message's words: a string, or the text of its blocks.
fn plain(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks.iter().filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join("\n\n"),
        _ => String::new(),
    }
}

/// A tool call's short form: its command, file, pattern or the like, the
/// first line of it.
fn call_form(input: &Value) -> String {
    const KEYS: [&str; 9] = ["command", "file_path", "notebook_path", "pattern", "url", "query", "description", "prompt", "path"];
    let Some(fields) = input.as_object() else {
        return String::new();
    };
    let said = KEYS.iter().find_map(|k| fields.get(*k)?.as_str()).or_else(|| fields.values().find_map(Value::as_str));
    said.and_then(|s| s.lines().next()).unwrap_or_default().to_string()
}

/// What one transcript line shows, in the order it was said; what it reads
/// past is counted in `skipped`.
fn read_line(at: u64, line: &[u8], skipped: &mut BTreeMap<String, usize>) -> Vec<HistoryItem> {
    let mut skip = |what: &str| *skipped.entry(what.to_string()).or_default() += 1;
    let Ok(entry) = serde_json::from_slice::<Value>(line) else {
        let said = format!("a line that can't be read as JSON ({} bytes)", line.len());
        return vec![HistoryItem::new(at, 0, HistoryKind::Unreadable, None).said(&said, TEXT_MAX)];
    };
    let (when, kind, content) = (entry["timestamp"].as_str(), entry["type"].as_str().unwrap_or(""), &entry["message"]["content"]);
    let mut out: Vec<HistoryItem> = Vec::new();
    if entry["isSidechain"] == true {
        skip("sidechain");
        return out;
    }
    match kind {
        "user" if entry["isCompactSummary"] == true => out.push(HistoryItem::new(at, 0, HistoryKind::Summary, when).said(&plain(content), SUMMARY_MAX)),
        "user" if entry["isMeta"] == true => skip("meta"),
        "user" => match content {
            Value::String(s) => out.push(HistoryItem::new(at, 0, HistoryKind::User, when).said(s, TEXT_MAX)),
            Value::Array(blocks) => {
                let mut said: Vec<String> = Vec::new();
                for block in blocks {
                    match block["type"].as_str() {
                        Some("text") => said.push(block["text"].as_str().unwrap_or_default().to_string()),
                        // What a tool answered is never shown, only counted.
                        Some("tool_result") if block["is_error"] == true => skip("tool result (error)"),
                        Some("tool_result") => skip("tool result"),
                        Some(other) => said.push(format!("[{other}]")),
                        None => skip("user block without a type"),
                    }
                }
                if !said.is_empty() {
                    out.push(HistoryItem::new(at, out.len(), HistoryKind::User, when).said(&said.join("\n\n"), TEXT_MAX));
                }
            }
            _ => skip("user without content"),
        },
        "assistant" => match content {
            Value::String(s) => out.push(HistoryItem::new(at, 0, HistoryKind::Assistant, when).said(s, TEXT_MAX)),
            Value::Array(blocks) => {
                for block in blocks {
                    match block["type"].as_str() {
                        Some("text") if block["text"].as_str().is_some_and(|t| !t.trim().is_empty()) => {
                            out.push(HistoryItem::new(at, out.len(), HistoryKind::Assistant, when).said(block["text"].as_str().unwrap_or_default(), TEXT_MAX));
                        }
                        Some("text") => skip("empty text"),
                        Some("tool_use") => {
                            let mut item = HistoryItem::new(at, out.len(), HistoryKind::Tool, when).said(&call_form(&block["input"]), CALL_MAX);
                            item.name = block["name"].as_str().map(str::to_string);
                            out.push(item);
                        }
                        Some("thinking" | "redacted_thinking") => skip("thinking"),
                        Some(other) => skip(&format!("assistant {other}")),
                        None => skip("assistant block without a type"),
                    }
                }
            }
            _ => skip("assistant without content"),
        },
        "system" if entry["subtype"] == "compact_boundary" => {
            let mut item = HistoryItem::new(at, 0, HistoryKind::Compaction, when).said(entry["content"].as_str().unwrap_or_default(), CALL_MAX);
            item.name = entry["compactMetadata"]["trigger"].as_str().map(str::to_string);
            item.tokens = entry["compactMetadata"]["preTokens"].as_u64();
            out.push(item);
        }
        "system" => skip(&format!("system {}", entry["subtype"].as_str().unwrap_or("entry"))),
        "" => skip("entry without a type"),
        other => skip(other),
    }
    out
}

/// A file's lines from the end, the last first, each with where it starts.
struct Backwards {
    file: File,
    /// Where `held` starts in the file.
    start: u64,
    /// The bytes from `start` to the end of the lines not given yet, the
    /// newline that ends the last of them cut off.
    held: Vec<u8>,
    begun: bool,
    done: bool,
}

impl Backwards {
    /// From `end`: the file's end, or where a line starts.
    fn new(file: File, end: u64) -> Self {
        Backwards { file, start: end, held: Vec::new(), begun: false, done: false }
    }

    /// Takes in the chunk before what is held; false at the start of the file.
    fn more(&mut self) -> std::io::Result<bool> {
        if self.start == 0 {
            return Ok(false);
        }
        let n = self.start.min(CHUNK);
        let mut chunk = vec![0; n as usize];
        self.file.seek(SeekFrom::Start(self.start - n))?;
        self.file.read_exact(&mut chunk)?;
        chunk.extend_from_slice(&self.held);
        self.held = chunk;
        self.start -= n;
        Ok(true)
    }

    fn next(&mut self) -> std::io::Result<Option<(u64, Vec<u8>)>> {
        if self.done {
            return Ok(None);
        }
        if !self.begun {
            self.begun = true;
            self.more()?;
            if self.held.last() == Some(&b'\n') {
                self.held.pop();
            }
        }
        loop {
            if let Some(i) = self.held.iter().rposition(|&b| b == b'\n') {
                let line = self.held.split_off(i + 1);
                self.held.pop();
                return Ok(Some((self.start + i as u64 + 1, line)));
            }
            if self.held.len() > LINE_MAX {
                return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "a line over 64 MiB"));
            }
            if !self.more()? {
                self.done = true;
                return Ok(Some((self.start, std::mem::take(&mut self.held))));
            }
        }
    }
}

fn failed(path: &Path, e: std::io::Error) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, format!("{}: {e}", path.display()))
}

/// A page: its items, newest first; what it read past; where the page
/// before it begins.
type Page = (Vec<HistoryItem>, BTreeMap<String, usize>, Option<u64>);

/// One page of `path`: about `limit` items from before byte `before` (the
/// end of what came in, `top`, when none).
fn read_page(path: &Path, top: Option<u64>, before: Option<u64>, limit: usize) -> Result<Page, (StatusCode, String)> {
    let mut file = File::open(path).map_err(|e| failed(path, e))?;
    let len = file.metadata().map_err(|e| failed(path, e))?.len();
    let top = top.map_or(len, |b| b.min(len));
    let end = before.map_or(top, |b| b.min(top));
    if before.is_some() && end > 0 && end < len {
        let mut prev = [0u8];
        file.seek(SeekFrom::Start(end - 1)).and_then(|_| file.read_exact(&mut prev)).map_err(|e| failed(path, e))?;
        if prev[0] != b'\n' {
            return Err((StatusCode::BAD_REQUEST, "`before` is not where a page of this transcript begins".to_string()));
        }
    }
    let mut lines = Backwards::new(file, end);
    let (mut items, mut skipped, mut read) = (Vec::new(), BTreeMap::new(), 0u64);
    let earlier = loop {
        let (at, line) = match lines.next() {
            Ok(Some(found)) => found,
            Ok(None) => break None,
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                let said = format!("the history before this point can't be read: {e}");
                items.push(HistoryItem::new(lines.start, 0, HistoryKind::Unreadable, None).said(&said, TEXT_MAX));
                break None;
            }
            Err(e) => return Err(failed(path, e)),
        };
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        read += line.len() as u64 + 1;
        let mut said = read_line(at, &line, &mut skipped);
        said.reverse();
        items.extend(said);
        if items.len() >= limit || read >= SCAN_MAX {
            break (at > 0).then_some(at);
        }
    };
    Ok((items, skipped, earlier))
}

/// Where a conversation's Claude runtime keeps `session`'s transcript: in
/// one of its seats' own config folders, never the sign-in vault.
fn copy_of(state: &AppState, stream_id: &str, session: &str) -> Option<PathBuf> {
    let seats = crate::claude_native::seat_config_dir(state, stream_id, crate::node::Seat::Baker.as_str());
    let file = format!("{session}.jsonl");
    std::fs::read_dir(seats.parent()?)
        .ok()?
        .flatten()
        .find_map(|seat| std::fs::read_dir(seat.path().join("projects")).ok()?.flatten().map(|d| d.path().join(&file)).find(|p| p.is_file()))
}

/// A conversation's move-in: the session, what it moved in as, how big the
/// session was when it came in, and its copy while that is there.
struct Moved {
    session: String,
    title: Option<String>,
    bytes: Option<u64>,
    copy: Option<PathBuf>,
}

/// The newest move-in of a conversation whose copy is still there, else the
/// newest.
fn moved_in(state: &AppState, stream_id: &str) -> Option<Moved> {
    let facts = state.heart.lock().unwrap().events_of_family(stream_id, crate::move_in::IMPORTED).ok()?;
    let mut newest = None;
    for fact in facts.iter().rev().filter_map(|e| serde_json::from_str::<Value>(&e.payload).ok()) {
        let Some(session) = fact["session_id"].as_str().filter(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_hexdigit() || c == '-')) else {
            continue;
        };
        let copy = copy_of(state, stream_id, session);
        let moved = Moved { session: session.to_string(), title: fact["title"].as_str().map(str::to_string), bytes: fact["bytes"].as_u64(), copy };
        if moved.copy.is_some() {
            return Some(moved);
        }
        newest.get_or_insert(moved);
    }
    newest
}

pub(crate) fn history(state: &AppState, q: &MoveInHistoryQuery) -> Result<MoveInHistory, (StatusCode, String)> {
    crate::state::context_for(state, &q.stream_id).map_err(|e| (StatusCode::NOT_FOUND, e))?;
    let mut found = MoveInHistory { stream_id: q.stream_id.clone(), session_id: None, title: None, items: Vec::new(), skipped: Vec::new(), earlier: None };
    let Some(moved) = moved_in(state, &q.stream_id) else {
        return Ok(found);
    };
    found.session_id = Some(moved.session);
    found.title = moved.title;
    let Some(copy) = moved.copy else {
        found.items.push(HistoryItem::new(0, 0, HistoryKind::Unreadable, None).said("the copy of this session is not there any more", TEXT_MAX));
        return Ok(found);
    };
    // The conversation as it came in: later turns are the thread's own.
    let (items, skipped, earlier) = read_page(&copy, moved.bytes, q.before, q.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT))?;
    found.items = items;
    found.skipped = skipped.into_iter().map(|(kind, count)| HistorySkipped { kind, count }).collect();
    found.earlier = earlier;
    Ok(found)
}

pub(crate) async fn history_route(State(st): State<AppState>, Query(q): Query<MoveInHistoryQuery>) -> Result<Json<MoveInHistory>, (StatusCode, String)> {
    if crate::actor::agent_calling() {
        return Err((StatusCode::FORBIDDEN, "a moved-in conversation's history is the operator's to read".to_string()));
    }
    crate::actor::spawn_blocking(move || history(&st, &q))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map(Json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;
    use serde_json::json;

    const SID: &str = "5eed0000-0000-4000-8000-000000000001";

    /// Planted for these tests alone; never a real key.
    fn fake_key() -> String {
        format!("sk-{}", "a1b2c3d4".repeat(6))
    }
    fn fake_token() -> String {
        format!("ghp_{}", "Ab1".repeat(12))
    }

    /// A transcript shaped like Claude Code's, oldest first.
    fn transcript() -> Vec<String> {
        let t = |n: u32| format!("2026-10-12T00:00:{n:02}.000Z");
        let msg = |kind: &str, n: u32, content: Value| json!({"type": kind, "timestamp": t(n), "isSidechain": false, "message": {"role": kind, "content": content}}).to_string();
        vec![
            json!({"type": "queue-operation", "operation": "enqueue"}).to_string(),
            msg("user", 1, json!("Build the catching")),
            msg("assistant", 2, json!([{"type": "thinking", "thinking": "hm"}, {"type": "text", "text": "Looking."}, {"type": "tool_use", "id": "toolu_1", "name": "Bash", "input": {"command": "ls -la\nsecond line", "description": "list"}}])),
            msg("user", 3, json!([{"type": "tool_result", "tool_use_id": "toolu_1", "content": "file1\nfile2"}])),
            json!({"type": "attachment", "attachment": {"type": "skill_listing"}}).to_string(),
            json!({"type": "system", "subtype": "compact_boundary", "content": "Conversation compacted", "timestamp": t(4), "compactMetadata": {"trigger": "auto", "preTokens": 170000}}).to_string(),
            json!({"type": "user", "isCompactSummary": true, "timestamp": t(5), "message": {"role": "user", "content": "Summary: the catching is built"}}).to_string(),
            msg("user", 6, json!([{"type": "text", "text": "Next: the doors"}, {"type": "image", "source": {"type": "base64", "data": "AAAA"}}])),
            msg("assistant", 7, json!([{"type": "tool_use", "id": "toolu_2", "name": "Edit", "input": {"file_path": "/x/y.rs", "old_string": "a", "new_string": "b"}}])),
            msg("user", 8, json!([{"type": "tool_result", "tool_use_id": "toolu_2", "content": [{"type": "text", "text": "no"}], "is_error": true}])),
            "{not json".to_string(),
            msg("assistant", 9, json!([{"type": "text", "text": format!("Use {} for the call", fake_key())}])),
            json!({"type": "custom-title", "customTitle": "worker"}).to_string(),
            msg("user", 10, json!(format!("I set API_KEY={} and token {}\n-----BEGIN PRIVATE KEY-----\nFAKEFAKEFAKE\n-----END PRIVATE KEY-----\ndone", fake_token(), fake_token()))),
        ]
    }

    /// Where the move-in's copy sits: the seat's own config folder; the
    /// sign-in vault, which no route reads; or nowhere any more.
    #[derive(Clone, Copy)]
    enum At {
        Seat,
        Vault,
        Gone,
    }

    /// A node with a moved-in Baker: the copy, the fact the move-in records
    /// (with the size it came in with), and then whatever the thread's own
    /// turns appended to the copy.
    fn came_in(lines: &[String], at: At, later: &[String]) -> (tempfile::TempDir, AppState, String) {
        let dir = tempfile::tempdir().unwrap();
        let repo = bakery_repo_for_test(dir.path());
        let state = state_with_bakery_root(dir.path(), &repo);
        let home = crate::routes::baker::ensure_permanent_baker(&state, None, Some(&repo)).unwrap().stream_id;
        let config = match at {
            At::Vault | At::Gone => crate::claude_native::credential_dir(&state),
            At::Seat => crate::claude_native::seat_config_dir(&state, &home, "baker"),
        };
        let folder = config.join("projects/-Users-op-thebakery");
        let came = lines.join("\n") + "\n";
        if !matches!(at, At::Gone) {
            std::fs::create_dir_all(&folder).unwrap();
            let copy = folder.join(format!("{SID}.jsonl"));
            std::fs::write(&copy, &came).unwrap();
            if !later.is_empty() {
                use std::io::Write;
                let mut open = std::fs::OpenOptions::new().append(true).open(&copy).unwrap();
                open.write_all((later.join("\n") + "\n").as_bytes()).unwrap();
            }
        }
        let fact = json!({"session_id": SID, "title": "bakery-worker-1", "bytes": came.len(), "compactions": 1});
        crate::state::append(&state, &home, "human", crate::move_in::IMPORTED, fact.to_string(), vec![]).unwrap();
        (dir, state, home)
    }

    fn ask(stream_id: &str, before: Option<u64>, limit: usize) -> MoveInHistoryQuery {
        MoveInHistoryQuery { stream_id: stream_id.to_string(), before, limit: Some(limit) }
    }

    /// The conversation, newest first: messages, tool calls in short form,
    /// the compaction marked with its summary; what is not shown, a tool's
    /// answers among it, is counted; a bad line says so; and no key or token
    /// reaches a text.
    #[test]
    fn the_conversation_reads_newest_first_and_nothing_passes_unaccounted() {
        let (_dir, state, home) = came_in(&transcript(), At::Seat, &[]);
        let page = history(&state, &ask(&home, None, 200)).unwrap();
        assert_eq!((page.session_id.as_deref(), page.title.as_deref(), page.earlier), (Some(SID), Some("bakery-worker-1"), None));
        let kinds: Vec<HistoryKind> = page.items.iter().map(|i| i.kind).collect();
        use HistoryKind::*;
        assert_eq!(kinds, [User, Assistant, Unreadable, Tool, User, Summary, Compaction, Tool, Assistant, User]);
        let text = |n: usize| page.items[n].text.as_str();
        assert_eq!(text(4), "Next: the doors\n\n[image]");
        assert_eq!((text(3), page.items[3].name.as_deref()), ("/x/y.rs", Some("Edit")));
        assert_eq!((text(7), page.items[7].name.as_deref()), ("ls -la", Some("Bash")), "a command shows its first line");
        assert_eq!((text(5), text(8), text(9)), ("Summary: the catching is built", "Looking.", "Build the catching"));
        let compaction = &page.items[6];
        assert_eq!((compaction.name.as_deref(), compaction.tokens), (Some("auto"), Some(170000)));
        assert!(text(2).contains("can't be read"), "{}", text(2));
        assert_eq!(page.items[9].at.as_deref(), Some("2026-10-12T00:00:01.000Z"));
        let skipped: Vec<(&str, usize)> = page.skipped.iter().map(|s| (s.kind.as_str(), s.count)).collect();
        let counted = [("attachment", 1), ("custom-title", 1), ("queue-operation", 1), ("thinking", 1), ("tool result", 1), ("tool result (error)", 1)];
        assert_eq!(skipped, counted);
        // The planted values, a key in prose, a name=key beside a bare token and a private key, are left out.
        let everything = serde_json::to_string(&page.items).unwrap();
        assert!(!everything.contains(&fake_key()) && !everything.contains(&fake_token()) && !everything.contains("FAKEFAKE"), "{everything}");
        assert_eq!((page.items[1].left_out, page.items[0].left_out), (1, 3));
        assert_eq!(text(0), "I set [key left out] and token [key left out]\n[key left out]\ndone");
    }

    /// Pages of any size put the same conversation back together, newest
    /// first, with no item twice and none lost, and every skipped entry
    /// counted once; the cursor is where a line starts, and a place that
    /// isn't one is refused.
    #[test]
    fn pages_of_any_size_make_the_whole_conversation() {
        let (_dir, state, home) = came_in(&transcript(), At::Seat, &[]);
        let whole: Vec<String> = history(&state, &ask(&home, None, 200)).unwrap().items.into_iter().map(|i| i.id).collect();
        assert_eq!(whole.len(), 10);
        for limit in 1..=5 {
            let (mut ids, mut before, mut skipped) = (Vec::new(), None, 0);
            loop {
                let page = history(&state, &ask(&home, before, limit)).unwrap();
                ids.extend(page.items.into_iter().map(|i| i.id));
                skipped += page.skipped.iter().map(|s| s.count).sum::<usize>();
                before = page.earlier;
                if before.is_none() {
                    break;
                }
            }
            assert_eq!(ids, whole, "limit {limit}");
            assert_eq!(skipped, 6, "limit {limit}");
        }
        let refused = history(&state, &ask(&home, Some(7), 5)).unwrap_err();
        assert_eq!(refused.0, StatusCode::BAD_REQUEST);
    }

    /// A conversation that never moved in has no history; an unknown one is
    /// not found; an agent may not read it.
    #[tokio::test]
    async fn only_a_moved_in_conversation_has_history_and_only_for_the_operator() {
        let (_dir, state, home) = came_in(&transcript(), At::Seat, &[]);
        let created = crate::baker_threads::create_thread(&state, &Default::default(), "human", None, None).unwrap().thread.stream_id;
        let none = history(&state, &ask(&created, None, 5)).unwrap();
        assert!((none.session_id.is_none(), none.items.len(), none.earlier) == (true, 0, None));
        assert_eq!(history(&state, &ask("socket_nope", None, 5)).unwrap_err().0, StatusCode::NOT_FOUND);
        let query = || Query(ask(&home, None, 5));
        assert!(history_route(State(state.clone()), query()).await.is_ok());
        let refused = crate::actor::scope("agent:orchestrator".into(), history_route(State(state.clone()), query())).await.unwrap_err();
        assert_eq!(refused.0, StatusCode::FORBIDDEN);
    }

    /// The history is the conversation as it came in: what the thread's own
    /// turns appended to the seat's copy after the move-in is the thread's
    /// own and not in it. A copy that is gone says so, and so does one only
    /// the sign-in vault holds: no route reads in there.
    #[test]
    fn the_history_stops_where_the_move_in_did_and_a_missing_copy_says_so() {
        let later = vec![json!({"type": "user", "timestamp": "2026-10-12T01:00:00.000Z", "message": {"role": "user", "content": "after the move-in"}}).to_string()];
        let (_dir, state, home) = came_in(&transcript(), At::Seat, &later);
        let page = history(&state, &ask(&home, None, 200)).unwrap();
        assert_eq!(page.items.len(), 10, "nothing from after the move-in");
        assert!(page.items.iter().all(|i| i.text != "after the move-in"));
        assert_eq!(page.earlier, None);
        for at in [At::Gone, At::Vault] {
            let (_dir, state, home) = came_in(&transcript(), at, &[]);
            let page = history(&state, &ask(&home, None, 5)).unwrap();
            assert_eq!(page.session_id.as_deref(), Some(SID));
            assert_eq!(page.items.len(), 1);
            assert!((page.items[0].kind, page.items[0].text.as_str()) == (HistoryKind::Unreadable, "the copy of this session is not there any more"));
        }
    }

    /// A key or token is left out wherever it sits: bare, in JSON, in a
    /// header, a flag, an assignment, a private key. Plain words, and the
    /// short hashes of a commit, stay.
    #[test]
    fn keys_and_tokens_are_left_out_wherever_they_sit() {
        let (key, token) = (fake_key(), fake_token());
        let pem = "-----BEGIN RSA PRIVATE KEY-----\nSECRETBODYSECRETBODY\n-----END RSA PRIVATE KEY-----";
        let hidden = [
            (format!("{{\"token\":\"{key}\"}}"), key.clone()),
            (format!("see {token}."), token.clone()),
            ("Authorization: Bearer opaque-value-123456".to_string(), "opaque-value-123456".to_string()),
            ("curl --token abc123def456 https://example.invalid".to_string(), "abc123def456".to_string()),
            ("PASSWORD=hunter2hunter2 and on".to_string(), "hunter2hunter2".to_string()),
            (format!("export OPENAI_API_KEY=\"{key}\""), key.clone()),
            (format!("before\n{pem}\nafter"), "SECRETBODY".to_string()),
        ];
        for (text, secret) in hidden {
            let (out, left) = clean(&text);
            assert!(!out.contains(&secret) && left > 0, "{text:?} became {out:?}");
        }
        let plain = "merged 0403c79 and e162bb1fa1b2 into main, then ran cargo test: all green";
        assert_eq!(clean(plain), (plain.to_string(), 0));
    }

    /// A long text is cut and says how long it was; a transcript's lines
    /// come back from the end across the file's chunk edges, a line longer
    /// than a chunk among them, each with where it starts.
    #[test]
    fn long_texts_say_they_are_cut_and_lines_cross_chunk_edges() {
        let long = "word ".repeat(2000);
        let item = HistoryItem::new(0, 0, HistoryKind::Assistant, None).said(&long, TEXT_MAX);
        assert_eq!((item.cut, item.chars, item.text.chars().count()), (true, Some(long.chars().count()), TEXT_MAX));
        let short = HistoryItem::new(0, 0, HistoryKind::Assistant, None).said("fine", TEXT_MAX);
        assert_eq!((short.cut, short.chars), (false, Some(4)));

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        let (mut body, mut starts, mut lines) = (Vec::new(), Vec::new(), Vec::new());
        for n in 0..60usize {
            let size = if n == 30 { CHUNK as usize + CHUNK as usize / 2 } else { 40_000 + n };
            let line = format!("{n}:{}", "x".repeat(size));
            starts.push(body.len() as u64);
            body.extend_from_slice(line.as_bytes());
            body.push(b'\n');
            lines.push(line);
        }
        std::fs::write(&path, &body).unwrap();
        let mut back = Backwards::new(File::open(&path).unwrap(), body.len() as u64);
        let mut got = Vec::new();
        while let Some((at, line)) = back.next().unwrap() {
            got.push((at, String::from_utf8(line).unwrap()));
        }
        let want: Vec<(u64, String)> = starts.into_iter().zip(lines).rev().collect();
        assert_eq!(got, want);
        // No newline at the end: the last line is the unfinished one.
        std::fs::write(&path, b"a\nb\nc").unwrap();
        let mut back = Backwards::new(File::open(&path).unwrap(), 5);
        let all: Vec<_> = std::iter::from_fn(|| back.next().unwrap()).collect();
        assert_eq!(all, [(4, b"c".to_vec()), (2, b"b".to_vec()), (0, b"a".to_vec())]);
    }
}
