//! Memory (`/memory`, `/memory/write`) and context inspector (`/context`) routes.

use crate::state::{context_for, AppState};
use axum::{
    extract::{Query, State},
    http::StatusCode,
    Json,
};
use bakery_core::{Event, NewEvent};
use serde::{Deserialize, Serialize};

// ─── NODE_API §4: memory + context inspector ─────────────────────────────────

/// Most recent refusals surfaced per stream. Bounded so a long-lived stream
/// cannot turn an inspector call into an unbounded response.
pub(crate) const REFUSALS_CAP: usize = 50;

/// What "memory" is in this node today.
///
/// - `hot`: real projection `agent_tool_turn` injects (tail scan).
/// - `entries`: durable L1 notes from `memory.written` events only
///   (BRAIN_MAP / brain.md six-month filter — caller discipline).
/// L2–L4 remain empty until those layers exist — never invent graph rows.
#[derive(schemars::JsonSchema, Debug, Clone, Serialize)]
pub(crate) struct MemoryView {
    pub(crate) stream_id: String,
    /// Durable L1 entries projected from `memory.written` (latest per id).
    pub(crate) entries: Vec<MemoryEntry>,
    pub(crate) hot: HotMemoryView,
}

#[derive(schemars::JsonSchema, Debug, Clone, Serialize)]
pub(crate) struct MemoryEntry {
    pub(crate) id: String,
    pub(crate) layer: String,
    pub(crate) sensitivity: String,
    pub(crate) text: String,
    pub(crate) written_at: String,
    pub(crate) by: String,
    pub(crate) event_hash: String,
    #[serde(flatten)]
    pub(crate) shape: NoteShape,
    /// The notes its text links to, as `[[name]]`.
    pub(crate) links: Vec<String>,
}

/// A note's shape, as the Bakery keeps it for every runtime: one fact a
/// note, named, typed, described in one line, linked to others by name.
/// Every field is optional: a plain note is just its text.
#[derive(schemars::JsonSchema, Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct NoteShape {
    /// Its name, which other notes link to as `[[name]]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) name: Option<String>,
    /// user | feedback | project | reference
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub(crate) kind: Option<String>,
    /// One line: what it holds, for the index and for deciding relevance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) description: Option<String>,
    /// Where it came from: the thread that wrote it, or the file it was
    /// imported from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) source: Option<String>,
    /// Why it holds: what happened that taught it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) why: Option<String>,
    /// How to act on it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) how_to_apply: Option<String>,
}

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct MemoryWriteReq {
    pub(crate) stream_id: String,
    /// Durable note body. Empty → refuse.
    pub(crate) text: String,
    /// Stable page id (brain.md-style). Auto-minted when omitted.
    #[serde(default)]
    pub(crate) id: Option<String>,
    /// Only `L1` accepted today; other layers → refuse (no invented L2–L4).
    #[serde(default)]
    pub(crate) layer: Option<String>,
    /// `shared` | `private` | `canon` — default `shared`.
    #[serde(default)]
    pub(crate) sensitivity: Option<String>,
    #[serde(flatten, default)]
    pub(crate) shape: NoteShape,
}

#[derive(schemars::JsonSchema, Debug, Clone, Serialize)]
pub(crate) struct HotMemoryView {
    /// Compact prior-session facts, oldest first, as injected.
    pub(crate) lines: Vec<String>,
    /// Recently touched paths, newest last.
    pub(crate) paths: Vec<String>,
    pub(crate) prior_accept_path: Option<String>,
    /// Policy that produced the above, so a reader can see the window.
    pub(crate) scan_events: usize,
    pub(crate) max_lines: usize,
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct StreamQuery {
    pub(crate) stream_id: String,
    /// Accepted and ignored: per-seat actors do not exist yet (NODE_API §2),
    /// so there is nothing seat-specific to scope a context view to. Taking
    /// the parameter keeps the client's URL stable for when they do.
    #[allow(dead_code)]
    #[serde(default)]
    pub(crate) seat: Option<String>,
}

pub(crate) async fn memory_view(
    State(st): State<AppState>,
    Query(q): Query<StreamQuery>,
) -> Result<Json<MemoryView>, (StatusCode, String)> {
    memory_view_inner(&st, &q.stream_id)
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

pub(crate) fn memory_view_inner(state: &AppState, stream_id: &str) -> Result<MemoryView, String> {
    // Full stream for durable entries (can sit far back); tail for hot only.
    let all = state
        .heart
        .lock()
        .unwrap()
        .read_stream(stream_id)
        .map_err(|e| e.to_string())?;
    let entries = project_memory_entries(&all);
    let tail_n = crate::agent_loop::HOT_MEMORY_SCAN;
    let tail: Vec<&Event> = all
        .iter()
        .rev()
        .take(tail_n)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let hot = crate::agent_loop::hot_memory_from_events(
        tail.iter().map(|e| (e.family.as_str(), e.payload.as_str())),
    );
    Ok(MemoryView {
        stream_id: stream_id.to_string(),
        entries,
        hot: HotMemoryView {
            lines: hot.lines,
            paths: hot.paths,
            prior_accept_path: hot.prior_accept_path,
            scan_events: crate::agent_loop::HOT_MEMORY_SCAN,
            max_lines: crate::agent_loop::HOT_MEMORY_MAX_LINES,
        },
    })
}

/// Latest `memory.written` per `id` (compiled_truth style). Timeline stays in Heart.
pub(crate) fn project_memory_entries(events: &[Event]) -> Vec<MemoryEntry> {
    let mut by_id: std::collections::BTreeMap<String, MemoryEntry> =
        std::collections::BTreeMap::new();
    for e in events {
        if e.family != "memory.written" {
            continue;
        }
        let p: serde_json::Value = match serde_json::from_str(&e.payload) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let id = p
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let text = p
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if id.is_empty() || text.is_empty() {
            continue;
        }
        let layer = p
            .get("layer")
            .and_then(|v| v.as_str())
            .unwrap_or("L1")
            .to_string();
        let sensitivity = p
            .get("sensitivity")
            .and_then(|v| v.as_str())
            .unwrap_or(e.sensitivity.as_str())
            .to_string();
        let shape: NoteShape = serde_json::from_value(p.clone()).unwrap_or_default();
        let said = [Some(&text), shape.why.as_ref(), shape.how_to_apply.as_ref()];
        by_id.insert(
            id.clone(),
            MemoryEntry {
                id,
                layer,
                sensitivity,
                links: links_in(&said.into_iter().flatten().map(String::as_str).collect::<Vec<_>>().join("\n")),
                text,
                written_at: e.ts.clone(),
                by: e.actor.clone(),
                event_hash: e.event_hash.clone(),
                shape,
            },
        );
    }
    by_id.into_values().collect()
}

pub(crate) fn memory_write_inner(state: &AppState, req: &MemoryWriteReq) -> Result<Event, String> {
    memory_write_inner_as(state, req, "human")
}

/// [`memory_write_inner`]'s real body, parameterized on the recorded
/// `memory.written` actor (07d-B Phase 1 item 5 / Stage B item 1: on the
/// permanent Baker stream the agent may author memory, actor
/// `"agent:baker"`, never the human route's hardcoded `"human"` — same
/// function, same event, different initiator). `memory_write_inner` is the
/// thin human-route default; `crate::baker_ops` is the only other caller.
pub(crate) fn memory_write_inner_as(
    state: &AppState,
    req: &MemoryWriteReq,
    actor: &str,
) -> Result<Event, String> {
    let text = req.text.trim();
    if text.is_empty() {
        return Err("text must not be empty".into());
    }
    // Stream must exist (context_for errors on unknown).
    let _ctx = context_for(state, &req.stream_id)?;

    let layer = req.layer.as_deref().unwrap_or("L1").trim();
    if layer != "L1" {
        return Err(format!(
            "only layer L1 is recorded today (got {layer:?}); L2–L4 not invented"
        ));
    }
    let sensitivity = match req.sensitivity.as_deref().unwrap_or("shared").trim() {
        s @ ("shared" | "private" | "canon") => s,
        other => {
            return Err(format!(
                "sensitivity must be shared|private|canon (got {other:?})"
            ));
        }
    };
    let id = req
        .id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            // Stable-ish id from time + short hash of text (not a secret).
            let h = {
                use std::collections::hash_map::DefaultHasher;
                use std::hash::{Hash, Hasher};
                let mut hasher = DefaultHasher::new();
                text.hash(&mut hasher);
                format!("{:x}", hasher.finish())
            };
            format!("note-{}", &h[..h.len().min(10)])
        });

    let mut payload = serde_json::json!({
        "id": id,
        "layer": layer,
        "sensitivity": sensitivity,
        "text": text,
    });
    let shape = serde_json::to_value(&req.shape).unwrap_or_default();
    for (key, value) in shape.as_object().into_iter().flatten() {
        payload[key] = value.clone();
    }
    let payload = payload.to_string();

    let new = NewEvent {
        stream_id: req.stream_id.clone(),
        actor: crate::actor::attribute(actor),
        ts: chrono::Utc::now().to_rfc3339(),
        causality: vec![],
        layer: Some(layer.to_string()),
        sensitivity: sensitivity.to_string(),
        family: "memory.written".to_string(),
        payload,
    };
    let event = state
        .heart
        .lock()
        .unwrap()
        .append(new)
        .map_err(|e| e.to_string())?;
    let _ = state
        .tx
        .send(serde_json::to_string(&event).unwrap_or_default());
    // An agent's own note straight to its thread is scratch; the Baker's
    // own goes straight in.
    let baker_side = crate::baker_ops::is_baker_scope_stream(state, &req.stream_id);
    let key = crate::memory_index::key_of(&req.stream_id, &id);
    crate::memory_index::tell(state, if baker_side { "written" } else { "scratch" }, Some(&key), None);
    Ok(event)
}

pub(crate) async fn memory_write(
    State(st): State<AppState>,
    Json(req): Json<MemoryWriteReq>,
) -> Result<Json<Event>, (StatusCode, String)> {
    memory_write_inner(&st, &req)
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))
}

/// The names a note's text links to, as `[[name]]`, in order, once each.
pub(crate) fn links_in(text: &str) -> Vec<String> {
    let mut links: Vec<String> = Vec::new();
    for piece in text.split("[[").skip(1) {
        let Some((name, _)) = piece.split_once("]]") else {
            continue;
        };
        let name = name.trim();
        if !name.is_empty() && !name.contains('\n') && !links.iter().any(|l| l == name) {
            links.push(name.to_string());
        }
    }
    links
}

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct MemoryImportReq {
    /// Whose memory: a Baker thread's goes to the one Baker memory.
    pub(crate) stream_id: String,
    /// A folder of notes in Claude Code's memory form: one Markdown file a
    /// note, with `name`, `description` and `metadata.type` in its front
    /// matter; MEMORY.md, its index, is left out.
    pub(crate) folder: String,
}

#[derive(schemars::JsonSchema, Debug, Default, Serialize)]
pub(crate) struct MemoryImported {
    /// The notes written, new or changed, by name.
    pub(crate) imported: Vec<String>,
    /// The notes already the same in the Bakery.
    pub(crate) unchanged: Vec<String>,
    /// Files not imported, each with why.
    pub(crate) skipped: Vec<String>,
}

/// A note file's front matter (`---` … `---`, `key: value`, and the
/// indented keys under `metadata:`), then its body. A byte-order mark and
/// Windows line ends are read through.
fn note_file(raw: &str) -> Option<(std::collections::HashMap<String, String>, String)> {
    let raw = raw.strip_prefix('\u{feff}').unwrap_or(raw).replace("\r\n", "\n");
    let rest = raw.strip_prefix("---\n")?;
    let (head, body) = rest.split_once("\n---")?;
    let body = body.trim_start_matches('-').trim().to_string();
    let mut keys = std::collections::HashMap::new();
    let mut under = String::new();
    for line in head.lines() {
        let indented = line.starts_with(' ') || line.starts_with('\t');
        let Some((key, value)) = line.trim().split_once(':') else {
            continue;
        };
        let value = value.trim().trim_matches('"').replace("\\\"", "\"");
        if !indented {
            under = key.trim().to_string();
            keys.insert(under.clone(), value);
        } else {
            keys.insert(format!("{under}.{}", key.trim()), value);
        }
    }
    Some((keys, body))
}

/// A note's body split into its text, its why and its how to apply: our
/// notes end with a `**Why:**` line and a `**How to apply:**` line (or the
/// plain `Why:` and `How to apply:` the Bakery renders), each section
/// running to the next. A note that is nothing but those sections keeps
/// its body whole.
fn note_sections(body: &str) -> (String, Option<String>, Option<String>) {
    let mut parts = [String::new(), String::new(), String::new()];
    let mut at = 0;
    for line in body.lines() {
        let start = line.trim_start();
        let marked = [(1, "Why:"), (2, "How to apply:")].into_iter().find_map(|(i, mark)| {
            let bold = start.strip_prefix("**").and_then(|s| s.strip_prefix(mark)).and_then(|s| s.strip_prefix("**"));
            Some((i, bold.or_else(|| start.strip_prefix(mark))?))
        });
        let said = match marked {
            Some((i, rest)) => {
                at = i;
                rest.trim_start()
            }
            None => line,
        };
        parts[at].push_str(said);
        parts[at].push('\n');
    }
    let [text, why, how] = parts.map(|p| p.trim().to_string());
    if text.is_empty() {
        return (body.to_string(), None, None);
    }
    let some = |s: String| Some(s).filter(|s| !s.is_empty());
    (text, some(why), some(how))
}

/// Imports a folder of Claude Code memory notes into the Bakery's memory,
/// keeping their shape: each note by its name (writing one again replaces
/// it), its type, description and links. A note already the same is left.
/// Anything else in the folder is reported with why it stayed out: a
/// subfolder, a file that isn't a Markdown note, and a second note with a
/// name the folder already gave.
pub(crate) fn memory_import_inner(state: &AppState, req: &MemoryImportReq) -> Result<MemoryImported, String> {
    let stream_id = crate::baker_threads::memory_stream_for(state, &req.stream_id);
    let folder = std::path::Path::new(&req.folder);
    let mut paths: Vec<_> = std::fs::read_dir(folder)
        .map_err(|e| format!("{}: {e}", folder.display()))?
        .flatten()
        .map(|e| e.path())
        .collect();
    paths.sort();
    let events = state.heart.lock().unwrap().read_stream(&stream_id).map_err(|e| e.to_string())?;
    let known = project_memory_entries(&events);
    let mut out = MemoryImported::default();
    // Each name the folder gave, with the file that gave it first.
    let mut named: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for path in paths {
        let file = path.file_name().unwrap_or_default().to_string_lossy().to_string();
        if path.is_dir() {
            out.skipped.push(format!("{file}/: a folder; notes are the Markdown files at the top of the folder"));
            continue;
        }
        if path.extension().is_none_or(|x| x != "md") {
            out.skipped.push(format!("{file}: not a Markdown note"));
            continue;
        }
        if file == "MEMORY.md" {
            continue;
        }
        let raw = match std::fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(e) => {
                out.skipped.push(format!("{file}: can't be read ({e})"));
                continue;
            }
        };
        let Some((keys, body)) = note_file(&raw) else {
            out.skipped.push(format!("{file}: no front matter"));
            continue;
        };
        if body.is_empty() {
            out.skipped.push(format!("{file}: no text"));
            continue;
        }
        let stem = file.trim_end_matches(".md").to_string();
        let name = keys.get("name").filter(|n| !n.is_empty()).cloned().unwrap_or(stem);
        if let Some(first) = named.get(&name) {
            out.skipped.push(format!("{file}: named {name}, like {first}; not imported over it"));
            continue;
        }
        named.insert(name.clone(), file.clone());
        let kind = keys.get("metadata.type").or_else(|| keys.get("type")).filter(|t| !t.is_empty()).cloned();
        // The why and how to apply go in the note's own fields, out of its
        // body: each runtime's form adds them back after the body.
        let (body, why, how_to_apply) = note_sections(&body);
        let shape = NoteShape {
            name: Some(name.clone()),
            kind,
            description: keys.get("description").filter(|d| !d.is_empty()).cloned(),
            source: Some(format!("imported from {file}")),
            why,
            how_to_apply,
        };
        let same = known.iter().any(|k| {
            let shaped = (&k.shape.kind, &k.shape.description, &k.shape.why, &k.shape.how_to_apply);
            k.id == name && k.text == body && shaped == (&shape.kind, &shape.description, &shape.why, &shape.how_to_apply)
        });
        if same {
            out.unchanged.push(name);
            continue;
        }
        let write = MemoryWriteReq {
            stream_id: stream_id.clone(),
            text: body,
            id: Some(name.clone()),
            layer: None,
            sensitivity: None,
            shape,
        };
        memory_write_inner(state, &write)?;
        out.imported.push(name);
    }
    Ok(out)
}

pub(crate) async fn memory_import(
    State(st): State<AppState>,
    Json(req): Json<MemoryImportReq>,
) -> Result<Json<MemoryImported>, (StatusCode, String)> {
    crate::actor::spawn_blocking(move || memory_import_inner(&st, &req))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))
}

/// A note's text in a runtime's file: its body, why, and how to apply.
fn note_text(note: &crate::memory_index::Note) -> String {
    let mut text = format!("{}\n", note.body.trim());
    if !note.why.is_empty() {
        text.push_str(&format!("\nWhy: {}\n", note.why.trim()));
    }
    if !note.how_to_apply.is_empty() {
        text.push_str(&format!("\nHow to apply: {}\n", note.how_to_apply.trim()));
    }
    text
}

fn hashed(text: &str) -> String {
    blake3::hash(text.trim().as_bytes()).to_hex()[..16].to_string()
}

/// A note in Claude Code's own memory form. The hash of its text tells, at
/// the next session, whether Claude changed it since.
fn claude_note(note: &crate::memory_index::Note) -> String {
    let what = if note.summary.is_empty() { &note.title } else { &note.summary };
    let kind = if note.kind.is_empty() { "project" } else { &note.kind };
    let text = note_text(note);
    format!(
        "---\nname: {}\ndescription: {}\nmetadata:\n  type: {kind}\n  source: bakery\n  id: {}\n  hash: {}\n---\n\n{text}",
        note.name,
        what.replace('\n', " "),
        note.id,
        hashed(&text)
    )
}

/// A file or folder name from a note's id or name.
fn plain(name: &str) -> String {
    name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect()
}

/// The Bakery's own files in a runtime's folder carry this.
fn ours(path: &std::path::Path) -> bool {
    std::fs::read_to_string(path).is_ok_and(|t| t.starts_with("---\n") && t.contains("\n  source: bakery\n"))
}

/// The memory in Claude Code's memory folder for one workspace: a file a
/// note, and MEMORY.md, the index Claude loads, the notes to follow first.
/// Notes the Bakery no longer has go; files the agent wrote itself, and
/// their index lines, stay.
fn render_claude(dir: &std::path::Path, notes: &[crate::memory_index::Note]) {
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let mut notes: Vec<&crate::memory_index::Note> = notes.iter().collect();
    notes.sort_by_key(|n| !matches!(n.kind.as_str(), "user" | "feedback"));
    let mut index = Vec::new();
    let mut written = Vec::new();
    for note in notes {
        let file = format!("{}.md", plain(&note.id));
        let _ = std::fs::write(dir.join(&file), claude_note(note));
        let what = if note.summary.is_empty() { &note.title } else { &note.summary };
        index.push(format!("- [{}]({file}) — {}", note.name, what.replace('\n', " ")));
        written.push(file);
    }
    for gone in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = gone.path();
        let file = gone.file_name().to_string_lossy().to_string();
        if file != "MEMORY.md" && !written.contains(&file) && ours(&path) {
            let _ = std::fs::remove_file(path);
        }
    }
    let theirs: Vec<String> = std::fs::read_to_string(dir.join("MEMORY.md"))
        .unwrap_or_default()
        .lines()
        .filter(|line| {
            let target = line.split_once("](").and_then(|(_, rest)| rest.split_once(')')).map(|(f, _)| f);
            target.is_some_and(|f| !written.iter().any(|w| w == f) && dir.join(f).exists() && !ours(&dir.join(f)))
        })
        .map(str::to_string)
        .collect();
    index.extend(theirs);
    let _ = std::fs::write(dir.join("MEMORY.md"), format!("{}\n", index.join("\n")));
}

const SECTION_FROM: &str = "<!-- bakery:memory -->";
const SECTION_TO: &str = "<!-- /bakery:memory -->";

/// The memory's index as a marked section of an instructions file the
/// runtime loads (Codex's AGENTS.md, Gemini's GEMINI.md); the rest of the
/// file stays as it was. No index, no section.
fn render_section(path: &std::path::Path, index: &str) {
    let section = match index.trim().is_empty() {
        true => String::new(),
        false => format!("{SECTION_FROM}\n{}\n{SECTION_TO}", index.trim()),
    };
    let had = std::fs::read_to_string(path).unwrap_or_default();
    let next = match (had.find(SECTION_FROM), had.find(SECTION_TO)) {
        (Some(from), Some(to)) if to > from => format!("{}{section}{}", &had[..from], &had[to + SECTION_TO.len()..]),
        _ if section.is_empty() => had.clone(),
        _ if had.trim().is_empty() => format!("{section}\n"),
        _ => format!("{}\n\n{section}\n", had.trim_end()),
    };
    if next == had {
        return;
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, next);
}

/// Skill notes as Agent Skills in `dir`: a folder each, with its SKILL.md.
/// The Bakery's that are no longer notes go; the runtime's own stay.
fn render_skills(dir: &std::path::Path, skills: &[crate::memory_index::Note]) {
    let named = |n: &crate::memory_index::Note| {
        let name: String = n.name.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
        name.trim_matches('-').chars().take(64).collect::<String>()
    };
    let mut written = Vec::new();
    for skill in skills {
        let name = named(skill);
        if name.is_empty() {
            continue;
        }
        let what = if skill.summary.is_empty() { &skill.title } else { &skill.summary };
        let mut text = format!(
            "---\nname: {name}\ndescription: {}\nmetadata:\n  source: bakery\n  id: {}\n---\n\n{}\n",
            what.replace('\n', " "),
            skill.id,
            skill.body.trim()
        );
        if !skill.how_to_apply.is_empty() {
            text.push_str(&format!("\nHow to apply: {}\n", skill.how_to_apply.trim()));
        }
        let folder = dir.join(&name);
        if std::fs::create_dir_all(&folder).is_ok() {
            let _ = std::fs::write(folder.join("SKILL.md"), text);
        }
        written.push(name);
    }
    for gone in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = gone.file_name().to_string_lossy().to_string();
        if !written.contains(&name) && ours(&gone.path().join("SKILL.md")) {
            let _ = std::fs::remove_dir_all(gone.path());
        }
    }
}

/// What Claude wrote to its own memory folder itself, taken in as the
/// thread's scratch notes (the Baker's own notes, on the Baker's side) before
/// the folder is written again: a note of its own (named `claude-<file>`),
/// or a change it made to a note the Bakery wrote there. A change to the
/// thread's own scratch note is that note's next version; to a shared note,
/// a scratch note of its own, while the shared one is written back. The
/// Bakery's copy then stands in for each file, so what the operator edits
/// or retires is what Claude reads.
fn take_in(state: &AppState, stream_id: &str, seat: crate::node::Seat, dir: &std::path::Path) {
    let memory = crate::baker_threads::memory_stream_for(state, stream_id);
    let notes = crate::memory_index::notes_for(state, stream_id);
    let files = std::fs::read_dir(dir).into_iter().flatten().flatten().map(|f| f.path());
    for path in files.filter(|p| p.extension().is_some_and(|x| x == "md") && p.file_name().is_some_and(|n| n != "MEMORY.md")) {
        let Some((keys, body)) = std::fs::read_to_string(&path).ok().as_deref().and_then(note_file) else {
            continue;
        };
        let stem = path.file_stem().unwrap_or_default().to_string_lossy().to_string();
        let mine = keys.get("metadata.id").and_then(|id| notes.iter().find(|n| &n.id == id));
        let id = match (keys.get("metadata.source").map(String::as_str), mine) {
            (Some("bakery"), _) if keys.get("metadata.hash").is_none_or(|h| *h == hashed(&body)) => continue,
            (Some("bakery"), Some(note)) if note.status == "scratch" && note.stream_id == memory => {
                note.id.strip_prefix(&format!("{memory}/")).unwrap_or(&note.id).to_string()
            }
            (Some("bakery"), _) => format!("claude-{}", stem.rsplit('_').next().unwrap_or(&stem)),
            _ => format!("claude-{stem}"),
        };
        if !body.trim().is_empty() {
            let kind = keys.get("metadata.type").or_else(|| keys.get("type")).cloned().unwrap_or_default();
            let fact = serde_json::json!({
                "id": id, "name": keys.get("name").cloned().unwrap_or_else(|| id.clone()), "type": kind,
                "description": keys.get("description").cloned().unwrap_or_default(), "text": body,
                "source": format!("Claude's memory: {stem}.md"),
            });
            let actor = crate::actor::agent(stream_id, seat);
            if crate::state::append(state, &memory, &actor, "memory.written", fact.to_string(), vec![]).is_ok() {
                let baker_side = crate::baker_ops::is_baker_scope_stream(state, &memory);
                let key = crate::memory_index::key_of(&memory, &id);
                crate::memory_index::tell(state, if baker_side { "written" } else { "scratch" }, Some(&key), None);
            }
        }
        let _ = std::fs::remove_file(&path);
    }
}

/// Claude Code's folder name for a workspace: its path with every other
/// character a dash.
fn claude_project(workspace: &std::path::Path) -> String {
    workspace
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Each native runtime's own form of the memory a thread's agent sees,
/// written as its session starts. Claude: its memory folder for the
/// workspace, a file a note and MEMORY.md, after what Claude wrote there
/// itself becomes the thread's scratch notes (its config folder is shared
/// by every seat, so skills stay notes there). Codex, Gemini and Grok: the
/// memory's index in the instructions each loads (AGENTS.md, GEMINI.md, a
/// rules file) and skill notes as Agent Skills in the seat's home. Only
/// into folders that are the Bakery's or the seat's, never the operator's
/// own.
pub(crate) fn render_native(
    state: &AppState,
    stream_id: &str,
    seat: crate::node::Seat,
    workspace: &std::path::Path,
    seat_home: &std::path::Path,
    env: &[(String, String)],
) {
    let ours = |dir: &std::path::Path| dir.starts_with(seat_home) || dir.starts_with(state.base.as_path());
    let dir = |key: &str| {
        env.iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| std::path::PathBuf::from(v.replace(bakery_exec::ROOM_HOME_ENV_VALUE, &seat_home.to_string_lossy())))
            .filter(|d| ours(d))
    };
    if let Some(config) = dir("CLAUDE_CONFIG_DIR") {
        let memory = config.join("projects").join(claude_project(workspace)).join("memory");
        take_in(state, stream_id, seat, &memory);
        render_claude(&memory, &crate::memory_index::notes_for(state, stream_id));
    }
    let notes = crate::memory_index::notes_for(state, stream_id);
    let index = crate::memory_index::index_of(&notes, crate::memory_index::INDEX_TOKENS).unwrap_or_default();
    let skills: Vec<crate::memory_index::Note> = notes.into_iter().filter(|n| n.kind == "skill").collect();
    let codex = dir("CODEX_HOME").or_else(|| Some(seat_home.join(".codex")).filter(|d| d.is_dir()));
    let gemini = Some(seat_home.join(".gemini")).filter(|d| d.is_dir());
    if let Some(codex) = &codex {
        render_section(&codex.join("AGENTS.md"), &index);
    }
    if let Some(gemini) = &gemini {
        render_section(&gemini.join("GEMINI.md"), &index);
    }
    if codex.is_some() || gemini.is_some() {
        render_skills(&seat_home.join(".agents").join("skills"), &skills);
    }
    let grok = seat_home.join(".grok");
    if grok.is_dir() {
        let rules = grok.join("rules");
        let _ = std::fs::create_dir_all(&rules);
        let _ = match index.is_empty() {
            true => std::fs::remove_file(rules.join("bakery-memory.md")),
            false => std::fs::write(rules.join("bakery-memory.md"), &index),
        };
        render_skills(&grok.join("skills"), &skills);
    }
}

/// Context composition for one stream.
///
/// NODE_API §4 asks for a token window (`total/used/system/files/transcript/
/// memory`). The node has no tokenizer and records no usage, so those numbers
/// do not exist and are **not** reported — a plausible token count is worse
/// than none, because it would be trusted. What is measured instead is
/// characters of the actual assembled prompt pieces, named as such.
///
/// The context cap (`total`) is a property of whichever model is configured,
/// which the node does not know either, so there is no "used / total" bar
/// here. That field returns when usage recording lands.
#[derive(schemars::JsonSchema, Debug, Clone, Serialize)]
pub(crate) struct ContextView {
    pub(crate) stream_id: String,
    /// Real character counts. Not tokens — see the type comment.
    pub(crate) prompt_chars: PromptChars,
    pub(crate) limits: ContextLimits,
    pub(crate) files_in_scope: Vec<FileScope>,
    /// Count of refusals before the cap — so a client can render
    /// "showing 50 of 213" instead of mistaking the capped list for complete.
    pub(crate) refusals_total: usize,
    /// The jail refusing things, newest last (at most `REFUSALS_CAP`).
    /// The highest-value part of this view: it is the jail proving it works.
    pub(crate) refusals: Vec<Refusal>,
}

#[derive(schemars::JsonSchema, Debug, Clone, Serialize)]
pub(crate) struct PromptChars {
    /// The tool-loop system prompt, verbatim length.
    pub(crate) system: usize,
    /// Hot-memory block as it would be injected this turn.
    pub(crate) memory: usize,
    pub(crate) total: usize,
}

#[derive(schemars::JsonSchema, Debug, Clone, Serialize)]
pub(crate) struct ContextLimits {
    pub(crate) max_tool_steps: Option<usize>,
    pub(crate) hot_memory_scan_events: usize,
    pub(crate) hot_memory_max_lines: usize,
}

#[derive(schemars::JsonSchema, Debug, Clone, Serialize)]
pub(crate) struct FileScope {
    pub(crate) glob: String,
    /// `read+propose`: agent tools read inside the jail, and a write is only
    /// ever a proposal. Nothing the agent does writes directly.
    pub(crate) mode: &'static str,
}

#[derive(schemars::JsonSchema, Debug, Clone, Serialize)]
pub(crate) struct Refusal {
    pub(crate) at: String,
    /// Tool that was refused, or `edit` for a refused proposal path.
    pub(crate) tool: String,
    /// Path the agent reached for, when the call carried one.
    pub(crate) path: Option<String>,
    pub(crate) reason: String,
}

pub(crate) async fn context_view(
    State(st): State<AppState>,
    Query(q): Query<StreamQuery>,
) -> Result<Json<ContextView>, (StatusCode, String)> {
    context_view_inner(&st, &q.stream_id)
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))
}

pub(crate) fn context_view_inner(state: &AppState, stream_id: &str) -> Result<ContextView, String> {
    // Needs the worktree root, so an unknown stream is an error here rather
    // than an empty view that would read as "nothing in scope".
    let ctx = context_for(state, stream_id)?;

    let events = state
        .heart
        .lock()
        .unwrap()
        .read_stream(stream_id)
        .map_err(|e| e.to_string())?;

    // Same projection the agent gets, measured rather than guessed.
    let hot = crate::agent_loop::hot_memory_from_events(
        events
            .iter()
            .rev()
            .take(crate::agent_loop::HOT_MEMORY_SCAN)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .map(|e| (e.family.as_str(), e.payload.as_str())),
    );
    // Hot lines PLUS the memory's index a turn opens with (never the
    // operator's private notes, which reach no model).
    let durable_chars: usize = crate::memory_index::turn_index(state, stream_id, crate::memory_index::INDEX_TOKENS)
        .map_or(0, |index| index.chars().count());
    let memory_chars: usize = hot
        .lines
        .iter()
        .map(|l| l.chars().count() + 1)
        .sum::<usize>()
        + durable_chars;
    let system = crate::agent_loop::system_tools().chars().count();

    let mut refusals = collect_refusals(&events);
    let refusals_total = refusals.len();
    if refusals.len() > REFUSALS_CAP {
        refusals.drain(..refusals.len() - REFUSALS_CAP); // keep the newest
    }

    Ok(ContextView {
        stream_id: stream_id.to_string(),
        prompt_chars: PromptChars {
            system,
            memory: memory_chars,
            total: system + memory_chars,
        },
        limits: ContextLimits {
            max_tool_steps: state.node.lock().unwrap().agent_max_steps.map(|n| n.get()),
            hot_memory_scan_events: crate::agent_loop::HOT_MEMORY_SCAN,
            hot_memory_max_lines: crate::agent_loop::HOT_MEMORY_MAX_LINES,
        },
        // The jail is the whole worktree, not a glob list — say that plainly
        // instead of synthesizing per-pattern rows with invented file counts.
        files_in_scope: vec![FileScope {
            glob: format!("{}/**", ctx.worktree_path),
            mode: "read+propose",
        }],
        refusals_total,
        refusals,
    })
}

/// Refusals already on the stream. Two sources, both real:
/// a tool call the jail rejected (`agent.tool.called` with `ok: false`), and
/// a turn that died in the jail phase (a proposal path that escaped).
///
/// Only failed calls are read. A successful `agent.tool.called` summary can
/// contain file content, and this view must never become a way to read file
/// bodies out of the jail through the back door.
pub(crate) fn collect_refusals(events: &[Event]) -> Vec<Refusal> {
    let mut out = Vec::new();
    for ev in events {
        match ev.family.as_str() {
            "agent.tool.called" => {
                let Ok(p) = serde_json::from_str::<serde_json::Value>(&ev.payload) else {
                    continue;
                };
                if p.get("ok").and_then(|v| v.as_bool()) != Some(false) {
                    continue; // succeeded — its summary may hold file content
                }
                out.push(Refusal {
                    at: ev.ts.clone(),
                    tool: p
                        .get("tool")
                        .and_then(|v| v.as_str())
                        .unwrap_or("tool")
                        .to_string(),
                    path: p
                        .get("args")
                        .and_then(|a| a.get("path"))
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    reason: crate::agent_loop::truncate(
                        p.get("summary")
                            .and_then(|v| v.as_str())
                            .unwrap_or("refused"),
                        300,
                    ),
                });
            }
            "agent.turn.failed" => {
                let Ok(p) = serde_json::from_str::<serde_json::Value>(&ev.payload) else {
                    continue;
                };
                if p.get("phase").and_then(|v| v.as_str()) != Some("jail") {
                    continue; // model/tools failures are not refusals
                }
                out.push(Refusal {
                    at: ev.ts.clone(),
                    tool: "edit".to_string(),
                    path: None,
                    reason: crate::agent_loop::truncate(
                        p.get("reason")
                            .and_then(|v| v.as_str())
                            .unwrap_or("refused"),
                        300,
                    ),
                });
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::agent::{agent_turn_inner, decide, AgentDecisionReq, AgentTurnReq};
    use crate::routes::project::open_stream;
    use crate::state::append;
    use crate::testutil::*;
    use std::sync::Arc;

    // ─── GET /memory + GET /context: inspector (NODE_API §4) ─────────────────

    #[test]
    fn memory_of_an_empty_or_unknown_stream_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));

        let m = memory_view_inner(&state, "wt:nope").unwrap();
        assert!(m.entries.is_empty());
        assert!(m.hot.lines.is_empty() && m.hot.paths.is_empty(), "{m:?}");
        assert!(m.hot.prior_accept_path.is_none());
        // Policy is still reported so a reader sees the window that produced it.
        assert_eq!(m.hot.scan_events, crate::agent_loop::HOT_MEMORY_SCAN);
    }

    #[test]
    fn memory_entries_empty_without_memory_written_events() {
        let (_dir, state, _wt, hash, stream_id) = setup_with_pending();
        decide(
            &state,
            &AgentDecisionReq {
                stream_id: stream_id.clone(),
                event_hash: hash,
            },
            true,
        )
        .unwrap();

        let m = memory_view_inner(&state, &stream_id).unwrap();
        // Real activity, no memory.written → entries stay empty (no invent from tools).
        assert!(m.entries.is_empty(), "{m:?}");
        let wire = serde_json::to_string(&m).unwrap();
        for invented in ["\"L2\"", "\"L3\"", "\"L4\""] {
            assert!(!wire.contains(invented), "invented {invented} in {wire}");
        }
    }

    #[test]
    fn memory_write_projects_l1_entry_and_refuses_l2() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = open_stream(&state, &open_req(&repo, "mem1"))
            .unwrap()
            .context
            .stream_id;

        let ev = memory_write_inner(
            &state,
            &MemoryWriteReq {
                stream_id: stream_id.clone(),
                text: "Prefer Manual gate for bakery core crates".into(),
                id: Some("decision-manual-core".into()),
                layer: Some("L1".into()),
                sensitivity: Some("shared".into()),
                shape: Default::default(),
            },
        )
        .unwrap();
        assert_eq!(ev.family, "memory.written");
        assert_eq!(ev.layer.as_deref(), Some("L1"));

        let m = memory_view_inner(&state, &stream_id).unwrap();
        assert_eq!(m.entries.len(), 1, "{m:?}");
        assert_eq!(m.entries[0].id, "decision-manual-core");
        assert_eq!(m.entries[0].layer, "L1");
        assert!(m.entries[0].text.contains("Manual gate"));

        // Second write same id → compiled_truth replace (one entry).
        memory_write_inner(
            &state,
            &MemoryWriteReq {
                stream_id: stream_id.clone(),
                text: "Updated: Manual for crates/** only".into(),
                id: Some("decision-manual-core".into()),
                layer: None,
                sensitivity: None,
                shape: Default::default(),
            },
        )
        .unwrap();
        let m2 = memory_view_inner(&state, &stream_id).unwrap();
        assert_eq!(m2.entries.len(), 1);
        assert!(m2.entries[0].text.contains("Updated"));

        let err = memory_write_inner(
            &state,
            &MemoryWriteReq {
                stream_id: stream_id.clone(),
                text: "nope".into(),
                id: None,
                layer: Some("L2".into()),
                sensitivity: None,
                shape: Default::default(),
            },
        )
        .unwrap_err();
        assert!(err.contains("L1"), "{err}");
    }

    #[test]
    fn memory_hot_reflects_the_real_projection() {
        let (_dir, state, _wt, hash, stream_id) = setup_with_pending();
        decide(
            &state,
            &AgentDecisionReq {
                stream_id: stream_id.clone(),
                event_hash: hash,
            },
            true,
        )
        .unwrap();

        let m = memory_view_inner(&state, &stream_id).unwrap();
        assert!(
            !m.hot.lines.is_empty(),
            "an accepted write should leave a hot line: {m:?}"
        );
        assert!(m.hot.paths.iter().any(|p| p == "f.txt"), "{m:?}");
        assert_eq!(m.hot.prior_accept_path.as_deref(), Some("f.txt"), "{m:?}");
    }

    #[test]
    fn context_of_an_unknown_stream_is_an_error_not_an_empty_scope() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        // An empty files_in_scope would read as "the jail allows nothing".
        assert!(context_view_inner(&state, "wt:nope").is_err());
    }

    #[test]
    fn context_reports_measured_chars_and_real_limits_but_no_tokens() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let resp = open_stream(&state, &open_req(&repo, "ctx")).unwrap();

        let c = context_view_inner(&state, &resp.context.stream_id).unwrap();
        assert_eq!(
            c.prompt_chars.system,
            crate::agent_loop::system_tools().chars().count()
        );
        assert_eq!(
            c.prompt_chars.total,
            c.prompt_chars.system + c.prompt_chars.memory
        );
        assert_eq!(c.limits.max_tool_steps, None);

        // The jail is the worktree, stated as one scope with no fake count.
        assert_eq!(c.files_in_scope.len(), 1);
        assert!(
            c.files_in_scope[0]
                .glob
                .starts_with(&resp.context.worktree_path),
            "{c:?}"
        );
        assert_eq!(c.files_in_scope[0].mode, "read+propose");

        // No token figure anywhere — the node has no tokenizer and records no
        // usage, so a number here would be fabricated.
        let wire = serde_json::to_string(&c).unwrap();
        for faked in ["token", "tokens", "\"used\"", "cost"] {
            assert!(!wire.contains(faked), "fabricated {faked} in {wire}");
        }
    }

    #[test]
    fn context_lists_a_refused_tool_call_with_its_reason() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let model = Arc::new(ScriptedModel::new(&[
            r#"{"tool":"read_file","args":{"path":"../../../etc/passwd"}}"#,
            r#"{"done":{"summary":"stopped"}}"#,
        ]));
        let state = state_with(dir.path(), model);
        let stream_id = open_stream(&state, &open_req(&repo, "refuse"))
            .unwrap()
            .context
            .stream_id;

        agent_turn_inner(
            &state,
            &AgentTurnReq {
                model_id: None,
                effort: None,
                actor: None,
                stream_id: stream_id.clone(),
                file_path: None,
                instruction: "read the passwd file".into(),
                seat: None,
            },
        )
        .unwrap();

        let c = context_view_inner(&state, &stream_id).unwrap();
        assert_eq!(c.refusals.len(), 1, "{:?}", c.refusals);
        let r = &c.refusals[0];
        assert_eq!(r.tool, "read_file");
        assert_eq!(r.path.as_deref(), Some("../../../etc/passwd"));
        assert!(
            r.reason.contains("escapes"),
            "the reason must name the refusal: {r:?}"
        );
        assert!(!r.at.is_empty());
    }

    #[test]
    fn context_refusals_never_leak_a_successful_reads_file_body() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        // f.txt contains "line one\n"; a successful read's summary carries it.
        let model = Arc::new(ScriptedModel::new(&[
            r#"{"tool":"read_file","args":{"path":"f.txt"}}"#,
            r#"{"done":{"summary":"read it"}}"#,
        ]));
        let state = state_with(dir.path(), model);
        let stream_id = open_stream(&state, &open_req(&repo, "noleak"))
            .unwrap()
            .context
            .stream_id;

        agent_turn_inner(
            &state,
            &AgentTurnReq {
                model_id: None,
                effort: None,
                actor: None,
                stream_id: stream_id.clone(),
                file_path: None,
                instruction: "read f.txt".into(),
                seat: None,
            },
        )
        .unwrap();

        let c = context_view_inner(&state, &stream_id).unwrap();
        assert!(
            c.refusals.is_empty(),
            "a successful call is not a refusal: {:?}",
            c.refusals
        );
        let wire = serde_json::to_string(&c).unwrap();
        assert!(
            !wire.contains("line one"),
            "a successful read's body must not reach the inspector: {wire}"
        );
    }

    #[test]
    fn context_refusal_list_is_capped() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = open_stream(&state, &open_req(&repo, "manyref"))
            .unwrap()
            .context
            .stream_id;

        for i in 0..(REFUSALS_CAP + 7) {
            append(
                &state,
                &stream_id,
                "agent:test",
                "agent.tool.called",
                serde_json::json!({
                    "tool": "read_file",
                    "args": { "path": format!("../escape-{i}") },
                    "ok": false,
                    "step": 1,
                    "summary": "path escapes the worktree root",
                })
                .to_string(),
                vec![],
            )
            .unwrap();
        }

        let c = context_view_inner(&state, &stream_id).unwrap();
        assert_eq!(c.refusals.len(), REFUSALS_CAP, "list must be bounded");
        assert_eq!(
            c.refusals_total,
            REFUSALS_CAP + 7,
            "total must flag the cap — silent truncation is a lie on the jail surface",
        );
        // Newest kept, oldest dropped.
        let last = c.refusals.last().unwrap();
        assert_eq!(
            last.path.as_deref(),
            Some(&*format!("../escape-{}", REFUSALS_CAP + 6))
        );
    }

    #[test]
    fn context_counts_a_jail_phase_turn_failure_as_a_refusal() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = open_stream(&state, &open_req(&repo, "jailref"))
            .unwrap()
            .context
            .stream_id;

        agent_turn_inner(
            &state,
            &AgentTurnReq {
                model_id: None,
                effort: None,
                actor: None,
                stream_id: stream_id.clone(),
                file_path: Some("../../../etc/hosts".into()),
                instruction: "escape".into(),
                seat: None,
            },
        )
        .unwrap_err();

        let c = context_view_inner(&state, &stream_id).unwrap();
        assert_eq!(c.refusals.len(), 1, "{:?}", c.refusals);
        assert_eq!(c.refusals[0].tool, "edit");
        assert!(
            c.refusals[0].reason.contains("escapes"),
            "{:?}",
            c.refusals[0]
        );
    }

    /// Pins HOT_MEMORY_SCAN + 1 at the call site to hot_memory_from_events'
    /// internal window: full read and tail must yield the same HotMemory.
    #[test]
    fn hot_memory_from_full_stream_matches_tail_window() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id = open_stream(&state, &open_req(&repo, "hotwin"))
            .unwrap()
            .context
            .stream_id;
        let id = id.as_str();
        let n = crate::agent_loop::HOT_MEMORY_SCAN + 5;
        for i in 0..n {
            append(
                &state,
                id,
                "human",
                "agent.turn.started",
                serde_json::json!({ "instruction": format!("instr-{i}"), "file_path": null })
                    .to_string(),
                vec![],
            )
            .unwrap();
            if i % 3 == 0 {
                append(
                    &state,
                    id,
                    "agent:test",
                    "agent.file.changed",
                    serde_json::json!({ "file_path": format!("f{i}.txt"), "status": "pending" })
                        .to_string(),
                    vec![],
                )
                .unwrap();
            }
        }
        let heart = state.heart.lock().unwrap();
        let full = heart.read_stream(id).unwrap();
        let tail = heart
            .read_stream_tail(id, crate::agent_loop::HOT_MEMORY_SCAN + 1)
            .unwrap();
        drop(heart);
        assert!(
            full.len() > crate::agent_loop::HOT_MEMORY_SCAN + 1,
            "need a stream longer than the hot window"
        );
        let hot_full = crate::agent_loop::hot_memory_from_events(
            full.iter().map(|e| (e.family.as_str(), e.payload.as_str())),
        );
        let hot_tail = crate::agent_loop::hot_memory_from_events(
            tail.iter().map(|e| (e.family.as_str(), e.payload.as_str())),
        );
        assert_eq!(
            hot_full, hot_tail,
            "full read and HOT_MEMORY_SCAN+1 tail must produce the same HotMemory"
        );
    }

    /// A folder of Claude Code notes comes in with its shape: each note by
    /// its name, typed, described, linked. MEMORY.md, its index, stays out;
    /// a file with no front matter is named, not guessed at. Bringing it in
    /// again changes nothing.
    #[test]
    fn our_memory_files_come_in_with_their_shape() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = open_stream(&state, &open_req(&repo, "mem-import")).unwrap().context.stream_id;
        let notes = dir.path().join("notes");
        std::fs::create_dir_all(&notes).unwrap();
        std::fs::write(
            notes.join("feedback_no_estimates.md"),
            "---\nname: feedback-no-estimates\ndescription: never give time estimates\nmetadata:\n  type: feedback\n---\n\nNo durations, ever. See [[project-movein]].\n",
        )
        .unwrap();
        std::fs::write(
            notes.join("project_movein.md"),
            "---\nname: project-movein\ndescription: \"the order, after resume\"\nmetadata:\n  node_type: memory\n  type: project\n---\n\nMemory, then the move-in.\n",
        )
        .unwrap();
        std::fs::write(notes.join("loose.md"), "no front matter here\n").unwrap();
        std::fs::write(notes.join("MEMORY.md"), "- [x](x.md) — index\n").unwrap();
        let req = MemoryImportReq { stream_id: stream_id.clone(), folder: notes.display().to_string() };

        let first = memory_import_inner(&state, &req).unwrap();
        assert_eq!(first.imported, vec!["feedback-no-estimates", "project-movein"]);
        assert_eq!(first.skipped, vec!["loose.md: no front matter"]);
        let entries = memory_view_inner(&state, &stream_id).unwrap().entries;
        let rule = entries.iter().find(|e| e.id == "feedback-no-estimates").unwrap();
        assert_eq!(rule.shape.kind.as_deref(), Some("feedback"));
        assert_eq!(rule.shape.description.as_deref(), Some("never give time estimates"));
        assert_eq!(rule.links, vec!["project-movein"]);
        assert_eq!(rule.text, "No durations, ever. See [[project-movein]].");
        let order = entries.iter().find(|e| e.id == "project-movein").unwrap();
        assert_eq!(order.shape.description.as_deref(), Some("the order, after resume"));

        let again = memory_import_inner(&state, &req).unwrap();
        assert!(again.imported.is_empty() && again.unchanged.len() == 2, "{again:?}");
    }

    /// A note's `**Why:**` and `**How to apply:**` lines come in as its
    /// own why and how to apply, out of its body, links in them kept;
    /// bringing it in again changes nothing, and a changed why comes in.
    #[test]
    fn a_notes_why_and_how_to_apply_fill_its_own_fields() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = open_stream(&state, &open_req(&repo, "mem-sections")).unwrap().context.stream_id;
        let notes = dir.path().join("notes");
        std::fs::create_dir_all(&notes).unwrap();
        let note = |why: &str| {
            format!(
                "---\nname: feedback-short\ndescription: d\nmetadata:\n  type: feedback\n---\n\nKeep replies short.\nTwo lines.\n\n**Why:** {why}\n\n**How to apply:**\n- trim filler\n- see [[user-terse]]\n"
            )
        };
        std::fs::write(notes.join("feedback_short.md"), note("the operator said so")).unwrap();
        std::fs::write(notes.join("only.md"), "---\nname: only-why\n---\n\nWhy: nothing else\n").unwrap();
        let req = MemoryImportReq { stream_id: stream_id.clone(), folder: notes.display().to_string() };

        assert_eq!(memory_import_inner(&state, &req).unwrap().imported, vec!["feedback-short", "only-why"]);
        let notes_now = || crate::memory_index::notes_for(&state, &stream_id);
        let short = notes_now().into_iter().find(|n| n.name == "feedback-short").unwrap();
        assert_eq!(short.body, "Keep replies short.\nTwo lines.");
        assert_eq!((short.why.as_str(), short.how_to_apply.as_str()), ("the operator said so", "- trim filler\n- see [[user-terse]]"));
        assert!(short.links.iter().any(|l| l.to == "user-terse"), "a link in how to apply counts");
        let only = notes_now().into_iter().find(|n| n.name == "only-why").unwrap();
        assert_eq!((only.body.as_str(), only.why.as_str()), ("Why: nothing else", ""), "nothing but a why keeps its body");

        let again = memory_import_inner(&state, &req).unwrap();
        assert!(again.imported.is_empty() && again.unchanged.len() == 2, "{again:?}");
        std::fs::write(notes.join("feedback_short.md"), note("a new reason")).unwrap();
        assert_eq!(memory_import_inner(&state, &req).unwrap().imported, vec!["feedback-short"]);
        let short = notes_now().into_iter().find(|n| n.name == "feedback-short").unwrap();
        assert_eq!(short.why, "a new reason");
    }

    /// The rehearsal's move-in: a second note with a name already given is
    /// reported, not written over the first; a note saved with a byte-order
    /// mark or Windows line ends comes in; a subfolder and a file that isn't
    /// a Markdown note are each reported with why.
    #[test]
    fn a_folders_other_files_are_reported_never_lost() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = open_stream(&state, &open_req(&repo, "mem-import-odd")).unwrap().context.stream_id;
        let notes = dir.path().join("notes");
        std::fs::create_dir_all(notes.join("old")).unwrap();
        let note = |name: &str, text: &str| format!("---\nname: {name}\ndescription: d\nmetadata:\n  type: user\n---\n\n{text}\n");
        std::fs::write(notes.join("a_first.md"), note("twin", "The first.")).unwrap();
        std::fs::write(notes.join("b_second.md"), note("twin", "The second.")).unwrap();
        std::fs::write(notes.join("c_windows.md"), format!("\u{feff}{}", note("windows", "Saved on Windows.").replace('\n', "\r\n"))).unwrap();
        std::fs::write(notes.join("notes.txt"), "plain text\n").unwrap();
        std::fs::write(notes.join("old").join("x.md"), note("old", "In a folder.")).unwrap();
        let req = MemoryImportReq { stream_id: stream_id.clone(), folder: notes.display().to_string() };

        let out = memory_import_inner(&state, &req).unwrap();
        assert_eq!(out.imported, vec!["twin", "windows"]);
        assert_eq!(
            out.skipped,
            vec![
                "b_second.md: named twin, like a_first.md; not imported over it",
                "notes.txt: not a Markdown note",
                "old/: a folder; notes are the Markdown files at the top of the folder",
            ]
        );
        let entries = memory_view_inner(&state, &stream_id).unwrap().entries;
        assert_eq!(entries.iter().find(|e| e.id == "twin").unwrap().text, "The first.");
        assert_eq!(entries.iter().find(|e| e.id == "windows").unwrap().text, "Saved on Windows.");
    }

    /// Each runtime gets the memory its thread sees in its own form: Claude
    /// a file a note and MEMORY.md in its memory folder for the workspace,
    /// Codex the index in AGENTS.md and skill notes as Agent Skills. What
    /// Claude wrote there itself becomes the thread's scratch notes, never a
    /// proposal, and the Bakery's copy stands in for it: the operator's edit
    /// reaches the file Claude reads, their retiring takes it away. Claude's
    /// change to its thread's scratch note is that note's next version; to a
    /// shared note, a scratch note of its own while the shared one is
    /// written back. The operator's private note, and a folder outside the
    /// Bakery's and the seat's own, are never written.
    #[test]
    fn each_runtime_gets_the_notes_in_its_own_form() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = open_stream(&state, &open_req(&repo, "mem-render")).unwrap().context.stream_id;
        let note = |id: &str, kind: &str, text: &str, sensitivity: Option<&str>| {
            let shape = NoteShape {
                name: Some(id.to_string()),
                kind: Some(kind.to_string()),
                description: Some(format!("about {id}")),
                ..Default::default()
            };
            let req = MemoryWriteReq {
                stream_id: stream_id.clone(),
                text: text.to_string(),
                id: Some(id.to_string()),
                layer: None,
                sensitivity: sensitivity.map(str::to_string),
                shape,
            };
            memory_write_inner(&state, &req).unwrap();
        };
        note("rule-one", "feedback", "Always the first rule.", None);
        note("fact-two", "project", "The second fact.", None);
        let skill = serde_json::json!({"id": "release-notes", "name": "release-notes", "scope": "bakery", "type": "skill",
                                       "title": "Release notes", "summary": "How to write release notes", "body": "Lead with what changed."});
        crate::state::append(&state, crate::memory_index::STREAM, "human", "memory.written", skill.to_string(), vec![]).unwrap();
        let seat_home = dir.path().join("seat");
        let claude = state.base.join("cache/claude-native/config");
        let workspace = std::path::Path::new("/w/project.one");
        let memory = claude.join("projects/-w-project-one/memory");
        std::fs::create_dir_all(&memory).unwrap();
        std::fs::write(memory.join("mine.md"), "---\nname: mine\ndescription: the agent's own\n---\n\nThe agent's own finding.\n").unwrap();
        std::fs::write(memory.join("MEMORY.md"), "- [mine](mine.md) — the agent's own\n").unwrap();
        let codex = seat_home.join(".codex");
        std::fs::create_dir_all(&codex).unwrap();
        std::fs::write(codex.join("AGENTS.md"), "# The operator's own\n").unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let outside = elsewhere.path().join("operators-own-claude");
        let env = |claude: &std::path::Path| {
            vec![
                ("CLAUDE_CONFIG_DIR".to_string(), claude.display().to_string()),
                ("CODEX_HOME".to_string(), format!("{}/.codex", bakery_exec::ROOM_HOME_ENV_VALUE)),
            ]
        };
        let render = |claude: &std::path::Path| render_native(&state, &stream_id, crate::node::Seat::Orchestrator, workspace, &seat_home, &env(claude));
        let file = |id: &str| memory.join(format!("{}.md", plain(&format!("{stream_id}/{id}"))));
        let read = |path: &std::path::Path| std::fs::read_to_string(path).unwrap_or_default();
        let operator = |f: &dyn Fn() -> Result<crate::memory_index::NoteView, String>| f().unwrap();

        render(&claude);
        let index = read(&memory.join("MEMORY.md"));
        let first = format!("- [rule-one]({}) — about rule-one", file("rule-one").file_name().unwrap().to_string_lossy());
        assert_eq!(index.lines().next(), Some(first.as_str()), "the notes to follow first: {index}");
        assert!(index.contains("- [release-notes](release-notes.md) — How to write release notes") && index.contains("- [mine]("), "{index}");
        assert!(!memory.join("mine.md").exists(), "the Bakery's copy stands in for Claude's own file");
        assert!(read(&file("claude-mine")).contains("The agent's own finding."));
        let scratch = crate::memory_index::page(&state, &format!("{stream_id}/claude-mine")).unwrap();
        assert_eq!((scratch.note.note.status.as_str(), scratch.note.note.scope.clone()), ("scratch", format!("workspace:{stream_id}")));
        assert_eq!(scratch.note.note.source.as_deref(), Some("Claude's memory: mine.md"), "where it came from, for the wiki");
        assert!(crate::memory_review::proposals(&state, &crate::memory_review::ProposalsQuery { status: None }).unwrap().proposals.is_empty(), "scratch, never a proposal");
        let rule = read(&file("rule-one"));
        assert!(rule.contains("name: rule-one\n") && rule.contains("  type: feedback\n") && rule.ends_with("Always the first rule.\n"), "{rule}");
        let agents = read(&codex.join("AGENTS.md"));
        assert!(agents.starts_with("# The operator's own\n") && agents.contains("[Memory:"), "{agents}");
        assert!(!agents.contains("Always the first rule.") && agents.contains("Skills (read with memory_get): release-notes"), "a line a note: {agents}");
        let skill = read(&seat_home.join(".agents/skills/release-notes/SKILL.md"));
        assert!(skill.starts_with("---\nname: release-notes\ndescription: How to write release notes\n") && skill.contains("Lead with what changed."), "{skill}");

        let edits = crate::memory_review::NoteEdits { body: Some("The operator's word on it.".into()), ..Default::default() };
        operator(&|| crate::memory_review::edit(&state, &crate::memory_review::EditNoteReq { id: Some(format!("{stream_id}/claude-mine")), edits: edits.clone() }));
        std::fs::write(file("rule-one"), read(&file("rule-one")).replace("Always the first rule.", "Always the first rule, and the second.")).unwrap();
        std::fs::write(memory.join("release-notes.md"), read(&memory.join("release-notes.md")).replace("Lead with what changed.", "Lead with thanks.")).unwrap();
        render(&claude);
        assert!(read(&file("claude-mine")).contains("The operator's word on it."), "the operator's edit reaches the file Claude reads");
        let edited = crate::memory_index::page(&state, &format!("{stream_id}/claude-mine")).unwrap();
        assert_eq!(edited.note.note.source.as_deref(), Some("Claude's memory: mine.md"), "an edit keeps where it came from");
        let rule = crate::memory_index::page(&state, &format!("{stream_id}/rule-one")).unwrap();
        assert_eq!((rule.note.note.version, rule.note.note.body.as_str()), (2, "Always the first rule, and the second."), "Claude's change to its own scratch note");
        assert!(read(&memory.join("release-notes.md")).contains("Lead with what changed."), "the shared note is written back");
        let theirs = crate::memory_index::page(&state, &format!("{stream_id}/claude-release-notes")).unwrap();
        assert!(theirs.note.note.status == "scratch" && theirs.note.note.body.contains("Lead with thanks."), "Claude's change to a shared note is scratch of its own");

        crate::memory_review::retire(&state, &crate::memory_review::RetireReq { id: format!("{stream_id}/claude-mine"), why: String::new(), merged_into: None }).unwrap();
        note("fact-two", "project", "The second fact, the operator's alone.", Some("private"));
        render(&claude);
        assert!(!file("claude-mine").exists(), "retired: out of the file Claude reads");
        assert!(!file("fact-two").exists(), "the operator's own note leaves the agent's folder");
        let agents_again = read(&codex.join("AGENTS.md"));
        assert_eq!(agents_again.matches("bakery:memory -->").count(), 2, "one section, replaced: {agents_again}");
        assert!(!agents_again.contains("fact-two"));

        render(&outside);
        assert!(!outside.exists(), "never a folder outside the Bakery's and the seat's");
    }
}
