//! Which recorded write put each hunk of a branch's diff there, for the merge
//! review's provenance chips and its "Ask": the conversation, turn and tool
//! call, or none. The diff is `base...HEAD`, what the branch committed since
//! it left base. The writes are the conversation's own Heart: every edit
//! Bakery reviewed (`agent.file.changed`, once `agent.edit.accepted`), every
//! native tool's completed edit that came with a diff (`agent.acp.tool_call`),
//! and every save the operator made in the Bakery (`operator.file.edited`). A
//! hunk goes to the write that introduced the most of its added lines;
//! `lines_matched` says how many, so a hunk two writes made reads as "partly".
//! A shell command, an edit made outside the Bakery or code from base has no
//! recorded write, and its hunk no source.

use crate::state::AppState;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    Json,
};
use bakery_core::Event;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct ProvenanceQuery {
    pub(crate) stream_id: String,
    /// The branch it left: the diff is base...HEAD, what this branch
    /// committed since, never uncommitted work.
    pub(crate) base: String,
    /// One file only.
    #[serde(default)]
    pub(crate) path: Option<String>,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct Provenance {
    pub(crate) hunks: Vec<HunkSource>,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct HunkSource {
    /// The file, by its new path.
    pub(crate) file: String,
    /// From the hunk's @@ … +new_start,new_lines @@ header.
    pub(crate) new_start: u32,
    pub(crate) new_lines: u32,
    /// Its added lines, not counting blank ones.
    pub(crate) added: usize,
    /// The recorded write that introduced most of them; null when none did.
    pub(crate) source: Option<Source>,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct Source {
    pub(crate) stream_id: String,
    pub(crate) turn_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) call_id: Option<String>,
    /// agent.file.changed (an edit Bakery reviewed), agent.acp.tool_call (a
    /// native tool's edit) or operator.file.edited (the operator's own save).
    pub(crate) family: String,
    pub(crate) event_hash: String,
    /// When it was written.
    pub(crate) at: String,
    pub(crate) actor: String,
    /// The turn's first message, at most 500 characters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) instruction: Option<String>,
    /// How many of the hunk's added lines this write introduced; fewer than
    /// `added` means the hunk mixes writes (or hand edits).
    pub(crate) lines_matched: usize,
}

#[derive(Debug, PartialEq)]
struct Hunk {
    file: String,
    new_start: u32,
    new_lines: u32,
    added: Vec<String>,
}

/// The hunks of a unified diff, by their new file and range. A deleted file
/// has none; a renamed one goes by its new path.
fn parse_hunks(diff: &str) -> Vec<Hunk> {
    let mut hunks = Vec::new();
    let (mut file, mut header, mut open) = (None::<String>, false, false);
    for line in diff.lines() {
        if line.starts_with("diff --git ") {
            (file, header, open) = (None, true, false);
            continue;
        }
        if header {
            if let Some(path) = line.strip_prefix("+++ ") {
                file = path.strip_prefix("b/").map(str::to_string);
            }
            if !line.starts_with("@@") {
                continue;
            }
            header = false;
        }
        if let Some(rest) = line.strip_prefix("@@ ") {
            let range = rest.split_whitespace().find_map(|t| t.strip_prefix('+')).unwrap_or("0");
            let (start, len) = range.split_once(',').unwrap_or((range, "1"));
            open = file.is_some();
            if let Some(file) = &file {
                hunks.push(Hunk {
                    file: file.clone(),
                    new_start: start.parse().unwrap_or(0),
                    new_lines: len.parse().unwrap_or(1),
                    added: Vec::new(),
                });
            }
            continue;
        }
        if let (true, Some(added), Some(hunk)) = (open, line.strip_prefix('+'), hunks.last_mut()) {
            hunk.added.push(added.to_string());
        }
    }
    hunks
}

/// One recorded write: what a file held before and after it.
struct Write {
    file: String,
    old: String,
    new: String,
    turn_id: String,
    call_id: Option<String>,
    family: &'static str,
    event_hash: String,
    at: String,
    actor: String,
}

fn payload(event: &Event) -> Value {
    serde_json::from_str(&event.payload).unwrap_or_default()
}

fn text(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

/// A native tool call as its events tell it: its first event and turn, the
/// diffs it carried, whether it completed, and when it was last heard of.
struct Call<'e> {
    first: &'e Event,
    turn_id: String,
    diffs: Vec<Value>,
    completed: bool,
    at: String,
}

/// The conversation's writes, oldest first: reviewed edits once accepted,
/// and native tools' edits with a diff once their call completed.
fn writes(state: &AppState, stream_id: &str, events: &[Event]) -> Vec<Write> {
    let mut proposals: HashMap<&str, (&Event, Value)> = HashMap::new();
    let mut operator_saves: HashMap<&str, String> = HashMap::new();
    let mut calls: HashMap<String, Call> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut out = Vec::new();
    for event in events {
        match event.family.as_str() {
            "agent.file.changed" => {
                proposals.insert(event.event_hash.as_str(), (event, payload(event)));
            }
            "agent.edit.accepted" => {
                let accepted = payload(event);
                let Some((proposal, p)) = accepted["decided_event"].as_str().and_then(|h| proposals.get(h)) else {
                    continue;
                };
                // What was written: the operator's edit when they made one.
                let new = accepted.get("new_content").filter(|v| v.is_string()).unwrap_or(&p["new_content"]);
                // A turn's change that includes the operator's saves (E-1)
                // doesn't claim the lines those saves brought.
                let mut old = text(&p["old_content"]);
                let included = p["landed"]["includes_operator_edit"].as_array().into_iter().flatten();
                for theirs in included.filter_map(|h| h.as_str()).filter_map(|h| operator_saves.get(h)) {
                    old.push('\n');
                    old.push_str(theirs);
                }
                out.push(Write {
                    file: text(&p["file_path"]),
                    old,
                    new: text(new),
                    turn_id: text(&p["turn_id"]),
                    call_id: p["call_id"].as_str().map(str::to_string),
                    family: "agent.file.changed",
                    event_hash: proposal.event_hash.clone(),
                    at: event.ts.clone(),
                    actor: proposal.actor.clone(),
                });
            }
            crate::routes::project::OPERATOR_EDIT_FAMILY => {
                let p = payload(event);
                operator_saves.insert(event.event_hash.as_str(), text(&p["new_content"]));
                out.push(Write {
                    file: text(&p["file_path"]),
                    old: text(&p["old_content"]),
                    new: text(&p["new_content"]),
                    turn_id: String::new(),
                    call_id: None,
                    family: crate::routes::project::OPERATOR_EDIT_FAMILY,
                    event_hash: event.event_hash.clone(),
                    at: event.ts.clone(),
                    actor: event.actor.clone(),
                });
            }
            crate::pipe::ACP_TOOL_EVENT_FAMILY => {
                let p = payload(event);
                // The Heart copy is bounded; the whole one is kept beside it.
                let detail = p["full"]["sha"]
                    .as_str()
                    .and_then(|sha| crate::pipe::read_full_tool_payload(state, stream_id, sha).ok())
                    .and_then(|full| serde_json::from_str::<Value>(&full).ok())
                    .unwrap_or_else(|| p["detail"].clone());
                let Some(id) = detail["toolCallId"].as_str().map(str::to_string) else {
                    continue;
                };
                let call = calls.entry(id.clone()).or_insert_with(|| {
                    order.push(id);
                    Call { first: event, turn_id: text(&p["turn_id"]), diffs: Vec::new(), completed: false, at: String::new() }
                });
                for item in detail["content"].as_array().into_iter().flatten() {
                    let diff = item.get("diff").unwrap_or(item);
                    if item["type"] == "diff" && diff["path"].is_string() {
                        call.diffs.push(diff.clone());
                    }
                }
                call.completed |= detail["status"] == "completed";
                call.at = event.ts.clone();
            }
            _ => {}
        }
    }
    for id in order {
        let call = &calls[&id];
        if !call.completed {
            continue;
        }
        for diff in &call.diffs {
            out.push(Write {
                file: text(&diff["path"]),
                old: text(&diff["oldText"]),
                new: text(&diff["newText"]),
                turn_id: call.turn_id.clone(),
                call_id: Some(id.clone()),
                family: crate::pipe::ACP_TOOL_EVENT_FAMILY,
                event_hash: call.first.event_hash.clone(),
                at: call.at.clone(),
                actor: call.first.actor.clone(),
            });
        }
    }
    out.sort_by(|a, b| a.at.cmp(&b.at));
    out
}

/// A write's path names the hunk's file: the same relative path, or an
/// absolute one (the host checkout, a container's mount) that ends in it.
fn same_file(written: &str, file: &str) -> bool {
    let written = written.trim_start_matches("./");
    written == file || written.ends_with(&format!("/{file}"))
}

/// The write that introduced the most of the hunk's non-blank added lines
/// (the newest on a tie), and how many.
fn attribute<'w>(hunk: &Hunk, writes: &'w [Write]) -> Option<(&'w Write, usize)> {
    let added: Vec<&str> = hunk.added.iter().map(|l| l.trim_end()).filter(|l| !l.is_empty()).collect();
    let mut best: Option<(&Write, usize)> = None;
    for write in writes.iter().filter(|w| same_file(&w.file, &hunk.file)) {
        let new: HashSet<&str> = write.new.lines().map(str::trim_end).collect();
        let old: HashSet<&str> = write.old.lines().map(str::trim_end).collect();
        let introduced = added.iter().filter(|l| new.contains(*l) && !old.contains(*l)).count();
        if introduced > 0 && best.is_none_or(|(_, most)| introduced >= most) {
            best = Some((write, introduced));
        }
    }
    best
}

pub(crate) fn provenance(state: &AppState, q: &ProvenanceQuery) -> Result<Provenance, String> {
    let dir = crate::routes::git::checkout(state, &q.stream_id)?;
    let range = format!("{}...HEAD", crate::routes::git::rev(&q.base)?);
    let mut args = vec!["-c", "core.quotePath=false", "diff", "--no-color", "--no-ext-diff", "-M", &range, "--"];
    if let Some(path) = q.path.as_deref().filter(|p| !p.trim().is_empty()) {
        args.push(path);
    }
    let diff = crate::routes::git::git(&dir, &args)?;
    let events = state.heart.lock().unwrap().read_stream(&q.stream_id).map_err(|e| e.to_string())?;
    let writes = writes(state, &q.stream_id, &events);
    let instruction = |turn_id: &str| {
        events
            .iter()
            .filter(|e| e.family == "agent.turn.started")
            .map(payload)
            .find(|p| p["turn_id"] == turn_id)
            .and_then(|p| p["instruction"].as_str().map(|i| i.chars().take(500).collect::<String>()))
    };
    let hunks = parse_hunks(&diff)
        .into_iter()
        .map(|hunk| {
            let source = attribute(&hunk, &writes).map(|(write, lines_matched)| Source {
                stream_id: q.stream_id.clone(),
                turn_id: write.turn_id.clone(),
                call_id: write.call_id.clone(),
                family: write.family.to_string(),
                event_hash: write.event_hash.clone(),
                at: write.at.clone(),
                actor: write.actor.clone(),
                instruction: instruction(&write.turn_id),
                lines_matched,
            });
            HunkSource {
                added: hunk.added.iter().filter(|l| !l.trim().is_empty()).count(),
                file: hunk.file,
                new_start: hunk.new_start,
                new_lines: hunk.new_lines,
                source,
            }
        })
        .collect();
    Ok(Provenance { hunks })
}

pub(crate) async fn provenance_route(
    State(st): State<AppState>,
    Query(q): Query<ProvenanceQuery>,
) -> Result<Json<Provenance>, (StatusCode, String)> {
    crate::actor::spawn_blocking(move || provenance(&st, &q))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::append;
    use crate::testutil::*;
    use bakery_agent::StubModel;
    use std::sync::Arc;

    #[test]
    fn a_unified_diff_parses_into_hunks_by_their_new_file_and_range() {
        let diff = "\
diff --git a/old.rs b/new.rs
similarity index 90%
rename from old.rs
rename to new.rs
--- a/old.rs
+++ b/new.rs
@@ -1,2 +1,3 @@ fn main()
 keep
--- not a header, a removed line
+--- not a header, an added line
+second
diff --git a/gone.txt b/gone.txt
deleted file mode 100644
--- a/gone.txt
+++ /dev/null
@@ -1 +0,0 @@
-bye
diff --git a/new.txt b/new.txt
new file mode 100644
--- /dev/null
+++ b/new.txt
@@ -0,0 +1 @@
+only
\\ No newline at end of file
";
        let hunks = parse_hunks(diff);
        assert_eq!(
            hunks,
            vec![
                Hunk { file: "new.rs".into(), new_start: 1, new_lines: 3, added: vec!["--- not a header, an added line".into(), "second".into()] },
                Hunk { file: "new.txt".into(), new_start: 1, new_lines: 1, added: vec!["only".into()] },
            ]
        );
    }

    #[test]
    fn each_committed_hunk_names_the_recorded_write_that_introduced_it() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let ctx = crate::routes::project::open_stream(&state, &open_req(&make_repo(&dir.path().join("p")), "p"))
            .unwrap()
            .context;
        let (stream, checkout) = (ctx.stream_id.clone(), std::path::PathBuf::from(&ctx.worktree_path));
        let fact = |actor: &str, family: &str, payload: Value| {
            append(&state, &stream, actor, family, payload.to_string(), vec![]).unwrap()
        };
        fact("human", "agent.turn.started", serde_json::json!({"turn_id": "t1", "instruction": "add a greeting"}));
        let proposal = fact("agent:orchestrator", "agent.file.changed", serde_json::json!({
            "file_path": "app.txt", "old_content": "", "new_content": "hello\nworld\n",
            "turn_id": "t1", "call_id": "c1", "status": "pending",
        }));
        fact("policy/auto", "agent.edit.accepted", serde_json::json!({"decided_event": proposal.event_hash, "file_path": "app.txt", "status": "accepted"}));
        // A native tool's edit, recorded with its diff, completed.
        fact("agent:orchestrator", "agent.acp.tool_call", serde_json::json!({"turn_id": "t2", "detail": {
            "toolCallId": "n1", "status": "pending",
            "content": [{"type": "diff", "path": format!("{}/notes.txt", checkout.display()), "oldText": null, "newText": "from the native tool\n"}],
        }}));
        fact("agent:orchestrator", "agent.acp.tool_call", serde_json::json!({"turn_id": "t2", "detail": {"toolCallId": "n1", "status": "completed"}}));
        // What the branch commits: those writes, plus a line by hand.
        std::fs::write(checkout.join("app.txt"), "hello\nworld\nby hand\n").unwrap();
        std::fs::write(checkout.join("notes.txt"), "from the native tool\n").unwrap();
        std::fs::write(checkout.join("hand.txt"), "nobody recorded this\n").unwrap();
        // The operator's own save in the Bakery.
        let save = fact("human", crate::routes::project::OPERATOR_EDIT_FAMILY, serde_json::json!({
            "file_path": "mine.txt", "old_content": null, "new_content": "the operator wrote this\n",
        }));
        std::fs::write(checkout.join("mine.txt"), "the operator wrote this\n").unwrap();
        // The operator saved mid-turn; the turn's change to that file includes
        // their line and is kept.
        let theirs = fact("human", crate::routes::project::OPERATOR_EDIT_FAMILY, serde_json::json!({
            "file_path": "shared.txt", "old_content": null, "new_content": "the operator's line\n",
        }));
        let turn_change = fact("agent:orchestrator", "agent.file.changed", serde_json::json!({
            "file_path": "shared.txt", "old_content": "", "new_content": "the operator's line\n",
            "turn_id": "t3", "call_id": "", "status": "pending",
            "landed": {"includes_operator_edit": [theirs.event_hash]},
        }));
        fact("policy/auto", "agent.edit.accepted", serde_json::json!({"decided_event": turn_change.event_hash, "file_path": "shared.txt", "status": "accepted"}));
        std::fs::write(checkout.join("shared.txt"), "the operator's line\n").unwrap();
        crate::git::run(&checkout, &["add", "."]).unwrap();
        crate::git::run(&checkout, &["commit", "-qm", "work"]).unwrap();
        // Uncommitted work is not in the review.
        std::fs::write(checkout.join("app.txt"), "hello\nworld\nby hand\nuncommitted\n").unwrap();

        let q = ProvenanceQuery { stream_id: stream.clone(), base: "HEAD~1".into(), path: None };
        let found = provenance(&state, &q).unwrap();
        let hunk = |file: &str| {
            found.hunks.iter().find(|h| h.file == file).unwrap_or_else(|| panic!("no hunk for {file}: {found:?}"))
        };
        let app = hunk("app.txt");
        assert_eq!((app.new_start, app.added), (1, 3), "the committed lines only, not the uncommitted one");
        let source = app.source.as_ref().unwrap();
        assert_eq!(
            (source.turn_id.as_str(), source.call_id.as_deref(), source.lines_matched),
            ("t1", Some("c1"), 2),
            "partly: two of its three lines"
        );
        assert_eq!((source.event_hash.as_str(), source.actor.as_str()), (proposal.event_hash.as_str(), "agent:orchestrator"));
        assert_eq!(source.instruction.as_deref(), Some("add a greeting"));
        let native = hunk("notes.txt").source.as_ref().unwrap();
        assert_eq!(
            (native.family.as_str(), native.call_id.as_deref(), native.lines_matched),
            ("agent.acp.tool_call", Some("n1"), 1)
        );
        assert!(hunk("hand.txt").source.is_none(), "no recorded write, no source");
        let mine = hunk("mine.txt").source.as_ref().unwrap();
        assert_eq!((mine.family.as_str(), mine.actor.as_str()), ("operator.file.edited", "human"));
        assert_eq!(mine.event_hash, save.event_hash);
        let shared = hunk("shared.txt").source.as_ref().unwrap();
        assert_eq!(shared.family, "operator.file.edited", "a turn's change that includes the operator's save leaves them their lines");
    }
}
