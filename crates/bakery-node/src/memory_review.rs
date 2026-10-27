//! The memory's write path. An agent proposes a note (`memory_propose`)
//! resting on evidence; the node checks it (no secret in it, evidence the
//! agent reaches, a scope no wider than its own) and sorts it against what
//! is kept, waiting and declined: nothing new, a new note, a new version of
//! one, or one replacing another. It then waits for the operator, who keeps
//! it, as it is or edited, or declines it; a declined proposal stays, so the
//! same one never comes back. Nothing is kept without the operator; the
//! digest keeps all that wait at once. The operator also writes, edits and
//! retires notes, and promotes an agent's scratch note into a shared one;
//! an agent rates a note it used (`memory_rate`). Each change goes out live
//! as `memory.updated`.

use crate::actor::Caller;
use crate::memory_index::{self as index, MemoryMoment, Note, NoteLine, Proposal, DECLINED, PROPOSED, RATED, RETIRED, STREAM, TYPES, WRITTEN};
use crate::state::AppState;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    Json,
};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::{Component, Path};

/// As alike as two notes get before one is the other again.
const SAME: f64 = 0.8;
/// Alike enough, of one type in one scope, to be a new version of it.
pub(crate) const CLOSE: f64 = 0.5;

fn fail(e: rusqlite::Error) -> String {
    format!("the memory index: {e}")
}

fn operator_only(what: &str) -> Result<(), String> {
    match crate::actor::agent_calling() {
        true => Err(format!("only the operator {what}: an agent proposes, with memory_propose")),
        false => Ok(()),
    }
}

/// A private key, or a word shaped like a token: never in a note.
fn secret_in(text: &str) -> bool {
    const PREFIXES: &[&str] = &["sk-", "sk_live_", "rk_live_", "ghp_", "gho_", "ghs_", "github_pat_", "xoxb-", "xoxp-", "glpat-", "AIza", "AKIA"];
    let token = |word: &str| {
        let has = |f: fn(&char) -> bool| word.chars().any(|c| f(&c));
        let shaped = word.chars().all(|c| c.is_ascii_alphanumeric() || "+/=_-.".contains(c));
        let prefixed = word.len() >= 20 && PREFIXES.iter().any(|p| word.starts_with(p));
        prefixed || (word.len() >= 32 && shaped && has(char::is_ascii_digit) && has(char::is_ascii_uppercase) && has(char::is_ascii_lowercase))
    };
    text.contains("PRIVATE KEY-----") || text.split(|c: char| c.is_whitespace() || "\"'`,;()[]{}<>".contains(c)).any(token)
}

/// A note's name from its title: lowercase words joined by `-`.
fn slug(title: &str) -> String {
    let words: Vec<String> = title.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_string).collect();
    let mut slug = words.join("-");
    slug.truncate(slug.char_indices().nth(48).map_or(slug.len(), |(at, _)| at));
    slug.trim_end_matches('-').to_string()
}

/// A note id no note has yet: its name, numbered when taken.
fn unique_id(conn: &Connection, name: &str) -> Result<String, String> {
    let base = if name.is_empty() { crate::node::mint_id("note") } else { name.to_string() };
    for n in 1.. {
        let id = if n == 1 { base.clone() } else { format!("{base}-{n}") };
        let taken = conn.query_row("SELECT 1 FROM notes WHERE id = ?1", [&id], |_| Ok(())).optional().map_err(fail)?;
        if taken.is_none() {
            return Ok(id);
        }
    }
    unreachable!("some number is free")
}

/// A whole scope as the operator, or the Baker's side, names it.
fn named_scope(state: &AppState, scope: &str) -> Result<String, String> {
    let known_project = |p: &str| crate::catalog::known_projects(state).is_ok_and(|all| all.iter().any(|k| k.project_id == p));
    let fits = match scope {
        "bakery" | "baker" => true,
        _ => match (scope.strip_prefix("project:"), scope.strip_prefix("workspace:")) {
            (Some(project), _) => known_project(project),
            (_, Some(thread)) => crate::state::context_for(state, thread).is_ok(),
            _ => false,
        },
    };
    fits.then(|| scope.to_string()).ok_or_else(|| format!("{scope}: a scope is bakery, baker, project:<id> or workspace:<thread>"))
}

/// The scope a proposal asks for, within its proposer's reach: its own
/// thread (workspace) or its project; the Baker's side names any.
fn scope_for(state: &AppState, caller: &Caller, asked: Option<&str>) -> Result<String, String> {
    let thread = caller.thread.as_deref().ok_or("only an agent proposes: the operator writes notes in the wiki")?;
    let own = format!("workspace:{thread}");
    let project = caller.project.as_ref().map(|p| format!("project:{p}"));
    match asked.map(str::trim).filter(|s| !s.is_empty()) {
        None if caller.everywhere => Ok("baker".to_string()),
        None => Ok(project.unwrap_or(own)),
        Some("workspace") => Ok(own),
        Some("project") => project.ok_or_else(|| "this thread has no project: propose it for your thread (workspace)".to_string()),
        Some(scope) if caller.everywhere => named_scope(state, scope),
        Some(_) => Err("an agent proposes for its thread (workspace) or its project; the operator may widen it".to_string()),
    }
}

/// The evidence a proposal rests on, checked: a file in the proposer's
/// workspace (hashed now, so a later change shows), a Heart fact it
/// reaches, a commit, or a page.
fn evidence(state: &AppState, caller: &Caller, items: &[Value]) -> Result<Vec<Value>, String> {
    if items.is_empty() {
        return Err("a proposal rests on evidence: a file {\"path\"}, a fact {\"event\"}, a commit {\"commit\"} or a page {\"url\"}".to_string());
    }
    let thread = caller.thread.clone().unwrap_or_default();
    items
        .iter()
        .map(|item| {
            let text = |key: &str| item[key].as_str().map(str::trim).filter(|v| !v.is_empty());
            if let Some(path) = text("path") {
                let rel = Path::new(path);
                if !rel.components().all(|c| matches!(c, Component::Normal(_))) {
                    return Err(format!("{path}: a path in your workspace, from its top"));
                }
                if bakery_exec::is_credential_filename(rel) {
                    return Err(format!("{path}: a credential file is never evidence"));
                }
                let ctx = crate::state::context_for(state, &thread).map_err(|_| "a file is evidence from your thread's workspace".to_string())?;
                let bytes = std::fs::read(Path::new(&ctx.worktree_path).join(rel)).map_err(|e| format!("{path}: {e}"))?;
                return Ok(json!({"path": path, "hash": blake3::hash(&bytes).to_hex().to_string(), "thread": thread}));
            }
            if let Some(hash) = text("event") {
                let found = state.heart.lock().unwrap().find(hash).map_err(|e| e.to_string())?;
                let found = found.ok_or_else(|| format!("no fact {hash}"))?;
                let project = crate::state::socket_project(&state.heart.lock().unwrap(), &found.stream_id).0;
                let reached = caller.everywhere || found.stream_id == thread || (project.is_some() && project == caller.project);
                return match reached {
                    true => Ok(json!({"event": hash, "thread": found.stream_id})),
                    false => Err(format!("{hash} is a fact outside your project")),
                };
            }
            if let Some(sha) = text("commit") {
                let fits = (7..=64).contains(&sha.len()) && sha.chars().all(|c| c.is_ascii_hexdigit());
                return fits.then(|| json!({"commit": sha})).ok_or_else(|| format!("{sha} is not a commit"));
            }
            if let Some(url) = text("url") {
                let fits = (url.starts_with("https://") || url.starts_with("http://")) && url.len() <= 500;
                return fits.then(|| json!({"url": url})).ok_or_else(|| format!("{url} is not a page's address"));
            }
            Err("evidence is a path, an event, a commit or a url".to_string())
        })
        .collect()
}

fn words(text: &str) -> HashSet<String> {
    text.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| w.len() > 2).map(str::to_string).collect()
}

/// How alike two notes are: the words they share, of all they use.
pub(crate) fn alike(a: &Note, b: &Note) -> f64 {
    let gist = |n: &Note| words(&format!("{} {} {}", n.title, n.summary, n.body));
    let (a, b) = (gist(a), gist(b));
    match a.is_empty() || b.is_empty() {
        true => 0.0,
        false => a.intersection(&b).count() as f64 / a.union(&b).count() as f64,
    }
}

/// What a proposal is, against what is kept, waiting and declined: the
/// action and its target, or what it repeats, that one's state (kept,
/// waiting or declined), and why it is nothing new.
type Sorted = Result<(&'static str, Option<String>), (String, &'static str, String)>;

fn sort(conn: &Connection, caller: &Caller, note: &Note, supersedes: Option<&str>) -> Result<Sorted, String> {
    let proposals: Vec<Proposal> = conn
        .prepare("SELECT doc FROM proposals WHERE scope = ?1 AND status IN ('pending', 'declined')")
        .map_err(fail)?
        .query_map([&note.scope], |r| r.get::<_, String>(0))
        .map_err(fail)?
        .filter_map(|doc| doc.ok().and_then(|doc| serde_json::from_str(&doc).ok()))
        .collect();
    if let Some(p) = proposals.iter().find(|p| alike(&p.note, note) >= SAME) {
        return Ok(Err(match p.status.as_str() {
            "declined" => (p.id.clone(), "declined", format!("the operator declined this before ({}); it stays declined", p.id)),
            _ => (p.id.clone(), "waiting", format!("this already waits for the operator ({})", p.id)),
        }));
    }
    let kept: Vec<Note> = match index::terms(&format!("{} {}", note.title, note.summary)) {
        None => vec![],
        Some(terms) => conn
            .prepare(
                "SELECT n.doc FROM notes_fts JOIN notes n ON n.rowid = notes_fts.rowid
                 WHERE notes_fts MATCH ?1 AND n.status = 'kept' AND (?2 IS NULL OR EXISTS (SELECT 1 FROM json_each(?2) WHERE value = n.scope OR (value = '*' AND n.scope NOT LIKE 'private:%')))
                 ORDER BY bm25(notes_fts) LIMIT 20",
            )
            .map_err(fail)?
            .query_map(params![terms, index::visible(caller)], |r| r.get::<_, String>(0))
            .map_err(fail)?
            .filter_map(|doc| doc.ok().and_then(|doc| serde_json::from_str(&doc).ok()))
            .collect(),
    };
    // Kept already: alike, in its scope or the whole Bakery's (a project's
    // lesson doesn't make the Bakery-wide one a repeat).
    if let Some(same) = kept.iter().find(|k| (k.scope == note.scope || k.scope == "bakery") && alike(k, note) >= SAME) {
        return Ok(Err((same.id.clone(), "kept", format!("this is kept already, as {}", same.id))));
    }
    if let Some(target) = supersedes {
        return match index::visible_note(conn, caller, target)? {
            Some(_) => Ok(Ok(("supersede", Some(target.to_string())))),
            None => Err(format!("no note {target} you see to supersede")),
        };
    }
    let close = kept.iter().find(|k| k.kind == note.kind && k.scope == note.scope && (k.name == note.name || alike(k, note) >= CLOSE));
    Ok(Ok(match close {
        Some(k) => ("update", Some(k.id.clone())),
        None => ("insert", None),
    }))
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct ProposeReq {
    /// user, feedback, project, reference, skill or lesson.
    #[serde(rename = "type")]
    pub(crate) kind: String,
    pub(crate) title: String,
    /// One line: what it holds, for deciding whether to read it.
    pub(crate) summary: String,
    /// Plain facts, not orders to yourself ("deploys go through
    /// bakery.sh", not "always deploy with bakery.sh"); the steps go in
    /// how_to_apply.
    pub(crate) body: String,
    /// What happened that taught it.
    #[serde(default)]
    pub(crate) why: String,
    /// How to apply it next time, with {variables} where it varies.
    #[serde(default)]
    pub(crate) how_to_apply: String,
    /// What it rests on, at least one: a file in your workspace
    /// ({"path": …}), a Heart fact ({"event": hash}), a commit ({"commit":
    /// sha}) or a page ({"url": …}).
    pub(crate) evidence: Vec<Value>,
    /// The note it replaces, by id.
    #[serde(default)]
    pub(crate) supersedes: Option<String>,
    /// workspace (your thread only) or project (your project): your project
    /// when absent, or your thread when it has none. The operator may widen
    /// it.
    #[serde(default)]
    pub(crate) scope: Option<String>,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct Proposed {
    /// insert, update or supersede; none when it adds nothing.
    pub(crate) action: String,
    /// The proposal now waiting for the operator.
    pub(crate) proposal: Option<String>,
    /// The note it changes, or what it repeats.
    pub(crate) note: Option<String>,
    /// What it repeats is kept, waiting or declined.
    pub(crate) repeats: Option<String>,
    pub(crate) said: String,
}

/// An agent proposes a note; it waits for the operator.
pub(crate) fn propose(state: &AppState, req: &ProposeReq) -> Result<Proposed, String> {
    let caller = Caller::of(state);
    let scope = scope_for(state, &caller, req.scope.as_deref())?;
    let kind = req.kind.trim();
    if !TYPES.contains(&kind) {
        return Err(format!("{kind:?}: a note is one of {}", TYPES.join(", ")));
    }
    let (title, summary, body) = (req.title.trim(), req.summary.trim(), req.body.trim());
    if title.is_empty() || summary.is_empty() || body.is_empty() {
        return Err("a note has a title, a one-line summary and a body".to_string());
    }
    if title.len() > 120 || summary.len() > 300 || summary.contains('\n') {
        return Err("a title fits one line (120 characters), and the summary is one line (300)".to_string());
    }
    let note = Note {
        name: slug(title),
        kind: kind.to_string(),
        scope,
        title: title.to_string(),
        summary: summary.to_string(),
        body: body.to_string(),
        why: req.why.trim().to_string(),
        how_to_apply: req.how_to_apply.trim().to_string(),
        evidence: evidence(state, &caller, &req.evidence)?,
        trust: "agent".to_string(),
        ..Note::default()
    };
    propose_note(state, &caller, note, req.supersedes.as_deref().map(str::trim), "human")
}

/// A note the learning loop proposes (a lesson from reflection, or one seen
/// in more than one project): sorted like any against everything kept,
/// waiting and declined, and waiting for the operator.
pub(crate) fn propose_from_loop(state: &AppState, note: Note) -> Result<Proposed, String> {
    let everything = Caller::operator();
    propose_note(state, &everything, note, None, crate::learning::ACTOR)
}

/// A note proposed by `actor` (an agent's call, or the learning loop),
/// sorted against what is kept, waiting and declined, and waiting for the
/// operator unless it repeats one.
fn propose_note(state: &AppState, caller: &Caller, note: Note, supersedes: Option<&str>, actor: &str) -> Result<Proposed, String> {
    if [&note.title, &note.summary, &note.body, &note.why, &note.how_to_apply].iter().any(|t| secret_in(t)) {
        return Err("a note never holds a secret: take the key or token out".to_string());
    }
    let conn = index::open(state)?;
    let (action, target) = match sort(&conn, caller, &note, supersedes)? {
        Ok(sorted) => sorted,
        Err((repeats, state_of, said)) => {
            let repeats_state = Some(state_of.to_string());
            return Ok(Proposed { action: "none".to_string(), proposal: None, note: Some(repeats), repeats: repeats_state, said });
        }
    };
    let id = crate::node::mint_id("proposal");
    let fact = json!({"id": id, "action": action, "target": target, "note": note});
    crate::state::append(state, STREAM, actor, PROPOSED, fact.to_string(), vec![])?;
    index::tell(state, "proposed", target.as_deref(), Some(&id));
    let said = "it waits for the operator, who keeps it (as it is or edited) or declines it".to_string();
    Ok(Proposed { action: action.to_string(), proposal: Some(id), note: target, repeats: None, said })
}

/// The operator's changes to a note.
#[derive(schemars::JsonSchema, Debug, Clone, Default, Deserialize)]
pub(crate) struct NoteEdits {
    #[serde(default, rename = "type")]
    pub(crate) kind: Option<String>,
    /// bakery, baker, project:<id> or workspace:<thread>.
    #[serde(default)]
    pub(crate) scope: Option<String>,
    #[serde(default)]
    pub(crate) title: Option<String>,
    #[serde(default)]
    pub(crate) summary: Option<String>,
    #[serde(default)]
    pub(crate) body: Option<String>,
    #[serde(default)]
    pub(crate) why: Option<String>,
    #[serde(default)]
    pub(crate) how_to_apply: Option<String>,
    /// When it stops holding.
    #[serde(default)]
    pub(crate) valid_to: Option<String>,
}

fn edit_into(state: &AppState, note: &mut Note, edits: &NoteEdits) -> Result<(), String> {
    let set = |field: &mut String, to: &Option<String>| {
        if let Some(to) = to {
            *field = to.trim().to_string();
        }
    };
    set(&mut note.title, &edits.title);
    set(&mut note.summary, &edits.summary);
    set(&mut note.body, &edits.body);
    set(&mut note.why, &edits.why);
    set(&mut note.how_to_apply, &edits.how_to_apply);
    set(&mut note.kind, &edits.kind);
    if let Some(scope) = &edits.scope {
        note.scope = named_scope(state, scope.trim())?;
    }
    if let Some(to) = &edits.valid_to {
        note.valid_to = Some(to.trim().to_string()).filter(|to| !to.is_empty());
    }
    if !note.kind.is_empty() && !TYPES.contains(&note.kind.as_str()) {
        return Err(format!("{:?}: a note is one of {}, or plain", note.kind, TYPES.join(", ")));
    }
    if note.title.is_empty() || note.body.is_empty() {
        return Err("a note has a title and a body".to_string());
    }
    if note.name.is_empty() {
        note.name = slug(&note.title);
    }
    Ok(())
}

/// The thread a note lives on and its id there: the Bakery's memory stream
/// for a new one, else where it was written.
fn home_of(conn: &Connection, id: &str) -> Result<(String, String), String> {
    let doc = conn.query_row("SELECT doc FROM notes WHERE id = ?1", [id], |r| r.get::<_, String>(0)).optional().map_err(fail)?;
    let stream = doc.and_then(|doc| serde_json::from_str::<Note>(&doc).ok()).map_or(STREAM.to_string(), |n| n.stream_id);
    let raw = id.strip_prefix(&format!("{stream}/")).unwrap_or(id).to_string();
    Ok((stream, raw))
}

/// Writes a note's version as the operator: kept from a proposal, edited,
/// or written.
fn write(state: &AppState, conn: &Connection, id: &str, note: &Note, how: &str, proposal: Option<&str>) -> Result<(), String> {
    let (stream, raw) = home_of(conn, id)?;
    let links: Vec<&str> = note.links.iter().filter(|l| l.kind == "link").map(|l| l.to.as_str()).collect();
    let fact = json!({
        "id": raw, "name": note.name, "type": note.kind, "scope": note.scope, "title": note.title, "summary": note.summary,
        "body": note.body, "text": note.body, "why": note.why, "how_to_apply": note.how_to_apply, "links": links,
        "evidence": note.evidence, "supersedes": note.supersedes, "valid_from": note.valid_from, "valid_to": note.valid_to,
        "trust": note.trust, "proposed_by": note.proposed_by, "source": note.source, "how": how, "proposal": proposal,
        "status": if note.status.is_empty() { "kept" } else { &note.status },
    });
    crate::state::append(state, &stream, "human", WRITTEN, fact.to_string(), vec![]).map(|_| ())
}

fn proposal_of(conn: &Connection, id: &str) -> Result<Proposal, String> {
    let doc = conn.query_row("SELECT doc FROM proposals WHERE id = ?1", [id], |r| r.get::<_, String>(0)).optional().map_err(fail)?;
    let proposal: Proposal = doc.and_then(|doc| serde_json::from_str(&doc).ok()).ok_or_else(|| format!("no proposal {id}"))?;
    match proposal.status.as_str() {
        "pending" => Ok(proposal),
        status => Err(format!("{id} was {status} already")),
    }
}

/// Keeps a proposal as a note, with the operator's changes: the note it
/// becomes.
fn keep_one(state: &AppState, conn: &Connection, proposal: Proposal, edits: &NoteEdits) -> Result<String, String> {
    let mut note = proposal.note.clone();
    edit_into(state, &mut note, edits)?;
    note.proposed_by = Some(proposal.proposed_by.clone());
    let id = match (proposal.action.as_str(), &proposal.target) {
        ("update", Some(target)) => target.clone(),
        _ => unique_id(conn, &note.name)?,
    };
    if proposal.action == "supersede" {
        note.supersedes = proposal.target.clone();
    }
    note.status = "kept".to_string();
    write(state, conn, &id, &note, "kept", Some(&proposal.id))?;
    index::tell(state, "kept", Some(&id), Some(&proposal.id));
    Ok(id)
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct ReviewReq {
    pub(crate) proposal: String,
    /// keep or decline.
    pub(crate) decision: String,
    /// What the operator changes as they keep it.
    #[serde(default)]
    pub(crate) edits: NoteEdits,
    /// Why it is declined.
    #[serde(default)]
    pub(crate) why: String,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct Reviewed {
    pub(crate) proposal: String,
    /// kept or declined.
    pub(crate) status: String,
    /// The note it became.
    pub(crate) note: Option<String>,
}

/// The operator keeps a proposal, as it is or edited, or declines it.
pub(crate) fn review(state: &AppState, req: &ReviewReq) -> Result<Reviewed, String> {
    operator_only("keeps or declines what agents propose")?;
    let conn = index::open(state)?;
    let proposal = proposal_of(&conn, req.proposal.trim())?;
    let id = proposal.id.clone();
    match req.decision.as_str() {
        "keep" => {
            let note = keep_one(state, &conn, proposal, &req.edits)?;
            Ok(Reviewed { proposal: id, status: "kept".to_string(), note: Some(note) })
        }
        "decline" => {
            let fact = json!({"proposal": id, "why": req.why.trim()});
            crate::state::append(state, STREAM, "human", DECLINED, fact.to_string(), vec![])?;
            index::tell(state, "declined", None, Some(&id));
            Ok(Reviewed { proposal: id, status: "declined".to_string(), note: None })
        }
        other => Err(format!("{other:?}: a proposal is kept or declined (keep, decline)")),
    }
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct KeepAllReq {
    /// These proposals; every one waiting when empty.
    #[serde(default)]
    pub(crate) proposals: Vec<String>,
}

#[derive(schemars::JsonSchema, Debug, Default, Serialize)]
pub(crate) struct KeptAll {
    /// Each proposal and the note it became.
    pub(crate) kept: Vec<Reviewed>,
    /// Those not kept, and why.
    pub(crate) skipped: Vec<String>,
}

/// The operator keeps the waiting proposals at once, as they are.
pub(crate) fn keep_all(state: &AppState, req: &KeepAllReq) -> Result<KeptAll, String> {
    operator_only("keeps what agents propose")?;
    let ids: Vec<String> = match req.proposals.is_empty() {
        false => req.proposals.clone(),
        true => proposals_in(&index::open(state)?, "pending")?.into_iter().map(|p| p.id).collect(),
    };
    let mut out = KeptAll::default();
    for id in ids {
        // Each one kept is in the index before the next.
        let conn = index::open(state)?;
        match proposal_of(&conn, &id).and_then(|p| keep_one(state, &conn, p, &NoteEdits::default())) {
            Ok(note) => out.kept.push(Reviewed { proposal: id, status: "kept".to_string(), note: Some(note) }),
            Err(why) => out.skipped.push(format!("{id}: {why}")),
        }
    }
    Ok(out)
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct EditNoteReq {
    /// The note to change; a new note of the operator's when absent (then
    /// its type, scope, title and body).
    #[serde(default)]
    pub(crate) id: Option<String>,
    #[serde(flatten)]
    pub(crate) edits: NoteEdits,
}

/// The operator edits a note (a new version) or writes one.
pub(crate) fn edit(state: &AppState, req: &EditNoteReq) -> Result<index::NoteView, String> {
    operator_only("edits notes")?;
    let conn = index::open(state)?;
    let operator = Caller::operator();
    let (id, mut note, how) = match req.id.as_deref().map(str::trim).filter(|id| !id.is_empty()) {
        Some(id) => {
            let note = index::visible_note(&conn, &operator, id)?.ok_or_else(|| format!("no note {id}"))?;
            (id.to_string(), note, "edited")
        }
        None => {
            if req.edits.scope.is_none() {
                return Err("a new note needs its scope: bakery, baker, project:<id> or workspace:<thread>".to_string());
            }
            let title = req.edits.title.as_deref().unwrap_or_default();
            let note = Note { trust: "operator".to_string(), status: "kept".to_string(), ..Note::default() };
            (unique_id(&conn, &slug(title))?, note, "written")
        }
    };
    edit_into(state, &mut note, &req.edits)?;
    if [&note.title, &note.summary, &note.body, &note.why, &note.how_to_apply].iter().any(|t| secret_in(t)) {
        return Err("a note never holds a secret: take the key or token out".to_string());
    }
    write(state, &conn, &id, &note, how, None)?;
    index::tell(state, how, Some(&id), None);
    let conn = index::open(state)?;
    let note = index::visible_note(&conn, &operator, &id)?.ok_or_else(|| format!("{id} went missing"))?;
    Ok(index::view(&conn, state, note))
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct RetireReq {
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) why: String,
    /// The note it was merged into.
    #[serde(default)]
    pub(crate) merged_into: Option<String>,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct Retired {
    pub(crate) id: String,
}

/// The operator retires a note: it leaves the wiki and every search; its
/// history stays.
pub(crate) fn retire(state: &AppState, req: &RetireReq) -> Result<Retired, String> {
    operator_only("retires notes")?;
    let conn = index::open(state)?;
    let operator = Caller::operator();
    let id = req.id.trim();
    index::visible_note(&conn, &operator, id)?.ok_or_else(|| format!("no kept note {id}"))?;
    let fact = json!({"id": id, "why": req.why.trim(), "merged_into": req.merged_into});
    crate::state::append(state, STREAM, "human", RETIRED, fact.to_string(), vec![])?;
    index::tell(state, "retired", Some(id), None);
    Ok(Retired { id: id.to_string() })
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct PromoteReq {
    /// The scratch note.
    pub(crate) id: String,
    /// Where it is shared: bakery, baker, project:<id> or workspace:<thread>.
    pub(crate) scope: String,
    #[serde(flatten)]
    pub(crate) edits: NoteEdits,
}

/// The operator promotes an agent's scratch note into a shared, kept one:
/// the same note (its versions stay), resting on its scratch version.
pub(crate) fn promote(state: &AppState, req: &PromoteReq) -> Result<index::NoteView, String> {
    operator_only("promotes scratch notes")?;
    let conn = index::open(state)?;
    let operator = Caller::operator();
    let id = req.id.trim();
    let mut note = index::visible_note(&conn, &operator, id)?.ok_or_else(|| format!("no note {id}"))?;
    if note.status != "scratch" {
        return Err(format!("{id} is {} already, not a scratch note", note.status));
    }
    let edits = NoteEdits { scope: Some(req.scope.clone()), ..NoteEdits { ..req.edits.clone() } };
    if note.kind.is_empty() && edits.kind.is_none() {
        note.kind = "project".to_string();
    }
    edit_into(state, &mut note, &edits)?;
    if [&note.title, &note.summary, &note.body, &note.why, &note.how_to_apply].iter().any(|t| secret_in(t)) {
        return Err("a note never holds a secret: take the key or token out".to_string());
    }
    note.evidence.push(json!({"event": note.event_hash, "thread": note.stream_id}));
    (note.status, note.trust) = ("kept".to_string(), "operator".to_string());
    write(state, &conn, id, &note, "promoted", None)?;
    index::tell(state, "promoted", Some(id), None);
    let conn = index::open(state)?;
    let note = index::visible_note(&conn, &operator, id)?.ok_or_else(|| format!("{id} went missing"))?;
    Ok(index::view(&conn, state, note))
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct RateReq {
    pub(crate) id: String,
    /// helped or hurt: how the note did in the turn that used it.
    pub(crate) rating: String,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct Rated {
    pub(crate) id: String,
    pub(crate) uses: index::NoteUses,
}

/// A caller says how a note it used did; its latest word on a note counts.
pub(crate) fn rate(state: &AppState, req: &RateReq) -> Result<Rated, String> {
    if !matches!(req.rating.as_str(), "helped" | "hurt") {
        return Err(format!("{:?}: a note helped or hurt", req.rating));
    }
    let caller = Caller::of(state);
    let id = req.id.trim();
    index::visible_note(&index::open(state)?, &caller, id)?.ok_or_else(|| format!("no note {id} you see"))?;
    let on = caller.thread.clone().unwrap_or_else(|| STREAM.to_string());
    crate::state::append(state, &on, "human", RATED, json!({"id": id, "rating": req.rating}).to_string(), vec![])?;
    index::tell(state, "rated", Some(id), None);
    let conn = index::open(state)?;
    let note = index::visible_note(&conn, &caller, id)?.ok_or_else(|| format!("{id} went missing"))?;
    Ok(Rated { id: id.to_string(), uses: index::view(&conn, state, note).uses })
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct ProposalsQuery {
    /// pending, kept or declined; pending when absent.
    #[serde(default)]
    pub(crate) status: Option<String>,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct MemoryProposals {
    /// The oldest first.
    pub(crate) proposals: Vec<Proposal>,
}

fn proposals_in(conn: &Connection, status: &str) -> Result<Vec<Proposal>, String> {
    conn.prepare("SELECT doc FROM proposals WHERE status = ?1 ORDER BY json_extract(doc, '$.proposed_at')")
        .map_err(fail)?
        .query_map([status], |r| r.get::<_, String>(0))
        .map_err(fail)?
        .map(|doc| doc.map_err(fail).and_then(|doc| serde_json::from_str(&doc).map_err(|e| e.to_string())))
        .collect()
}

pub(crate) fn proposals(state: &AppState, q: &ProposalsQuery) -> Result<MemoryProposals, String> {
    let conn = index::open(state)?;
    Ok(MemoryProposals { proposals: proposals_in(&conn, q.status.as_deref().unwrap_or("pending"))? })
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct TimelineQuery {
    /// Only this note's moments.
    #[serde(default)]
    pub(crate) note: Option<String>,
    /// At most this many; 200 when absent.
    #[serde(default)]
    pub(crate) limit: Option<usize>,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct MemoryTimeline {
    /// The newest first.
    pub(crate) moments: Vec<MemoryMoment>,
}

fn moments(conn: &Connection, note: Option<&str>, since: Option<&str>, limit: usize) -> Result<Vec<MemoryMoment>, String> {
    conn.prepare(
        "SELECT at, what, note, proposal, by, title FROM timeline
         WHERE (?1 IS NULL OR note = ?1) AND (?2 IS NULL OR at >= ?2) ORDER BY position DESC LIMIT ?3",
    )
    .map_err(fail)?
    .query_map(params![note, since, limit as i64], |r| {
        Ok(MemoryMoment { at: r.get(0)?, what: r.get(1)?, note: r.get(2)?, proposal: r.get(3)?, by: r.get(4)?, title: r.get(5)? })
    })
    .map_err(fail)?
    .collect::<Result<_, _>>()
    .map_err(fail)
}

/// What happened in the memory: proposed, kept, edited, written, declined
/// and retired.
pub(crate) fn timeline(state: &AppState, q: &TimelineQuery) -> Result<MemoryTimeline, String> {
    let conn = index::open(state)?;
    Ok(MemoryTimeline { moments: moments(&conn, q.note.as_deref(), None, q.limit.unwrap_or(200).clamp(1, 1000))? })
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct MemoryDigest {
    /// Proposals waiting for the operator, the oldest first: Keep all keeps
    /// them (POST /memory/keep-all).
    pub(crate) pending: Vec<Proposal>,
    /// Kept notes gone stale.
    pub(crate) stale: Vec<NoteLine>,
    /// The last day's moments, the newest first.
    pub(crate) day: Vec<MemoryMoment>,
}

/// The memory's digest: what waits for the operator, what went stale, and
/// the last day.
pub(crate) fn digest(state: &AppState) -> Result<MemoryDigest, String> {
    let conn = index::open(state)?;
    let notes: Vec<Note> = conn
        .prepare("SELECT doc FROM notes WHERE status = 'kept'")
        .map_err(fail)?
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(fail)?
        .filter_map(|doc| doc.ok().and_then(|doc| serde_json::from_str(&doc).ok()))
        .collect();
    let stale = notes.iter().filter(|n| index::stale(&conn, state, n)).map(|n| index::line(n, true)).collect();
    let day = (chrono::Utc::now() - chrono::Duration::days(1)).to_rfc3339();
    Ok(MemoryDigest { pending: proposals_in(&conn, "pending")?, stale, day: moments(&conn, None, Some(&day), 500)? })
}

async fn blocking<T: Send + 'static>(work: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<Json<T>, (StatusCode, String)> {
    crate::actor::spawn_blocking(work)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map(Json)
        .map_err(|e| match e.starts_with("only the operator") {
            true => (StatusCode::FORBIDDEN, e),
            false => (StatusCode::BAD_REQUEST, e),
        })
}

pub(crate) async fn propose_route(State(st): State<AppState>, Json(req): Json<ProposeReq>) -> Result<Json<Proposed>, (StatusCode, String)> {
    blocking(move || propose(&st, &req)).await
}

pub(crate) async fn review_route(State(st): State<AppState>, Json(req): Json<ReviewReq>) -> Result<Json<Reviewed>, (StatusCode, String)> {
    blocking(move || review(&st, &req)).await
}

pub(crate) async fn keep_all_route(State(st): State<AppState>, Json(req): Json<KeepAllReq>) -> Result<Json<KeptAll>, (StatusCode, String)> {
    blocking(move || keep_all(&st, &req)).await
}

pub(crate) async fn edit_route(State(st): State<AppState>, Json(req): Json<EditNoteReq>) -> Result<Json<index::NoteView>, (StatusCode, String)> {
    blocking(move || edit(&st, &req)).await
}

pub(crate) async fn retire_route(State(st): State<AppState>, Json(req): Json<RetireReq>) -> Result<Json<Retired>, (StatusCode, String)> {
    blocking(move || retire(&st, &req)).await
}

pub(crate) async fn promote_route(State(st): State<AppState>, Json(req): Json<PromoteReq>) -> Result<Json<index::NoteView>, (StatusCode, String)> {
    blocking(move || promote(&st, &req)).await
}

pub(crate) async fn rate_route(State(st): State<AppState>, Json(req): Json<RateReq>) -> Result<Json<Rated>, (StatusCode, String)> {
    blocking(move || rate(&st, &req)).await
}

pub(crate) async fn proposals_route(State(st): State<AppState>, Query(q): Query<ProposalsQuery>) -> Result<Json<MemoryProposals>, (StatusCode, String)> {
    blocking(move || proposals(&st, &q)).await
}

pub(crate) async fn timeline_route(State(st): State<AppState>, Query(q): Query<TimelineQuery>) -> Result<Json<MemoryTimeline>, (StatusCode, String)> {
    blocking(move || timeline(&st, &q)).await
}

pub(crate) async fn digest_route(State(st): State<AppState>) -> Result<Json<MemoryDigest>, (StatusCode, String)> {
    blocking(move || digest(&st)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory_index::{search, MemorySearchReq};
    use crate::node::Seat;
    use crate::testutil::*;
    use std::sync::Arc;

    /// An agent's proposal waits for the operator; a repeat of it, of a
    /// kept note or of a declined one is nothing new; one naming another
    /// replaces it. Only the operator keeps (widening its scope if they
    /// choose), declines, keeps all, edits and retires; each is a moment in
    /// the timeline. No secret, no proposal without evidence, and an agent
    /// proposes for its thread or its project only.
    #[test]
    fn proposals_wait_for_the_operator_and_never_come_back_declined() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(bakery_agent::StubModel));
        let project = |name: &str| {
            let root = dir.path().join(name);
            std::fs::create_dir_all(&root).unwrap();
            crate::catalog::create_project(&state, name, Some(crate::catalog::DirectoryRootSpec::Attach(root.to_str().unwrap()))).unwrap()
        };
        let (app, chef) = project("app");
        let (_, outsider) = project("other");
        let workspace = crate::state::context_for(&state, &chef.stream_id).unwrap().worktree_path;
        std::fs::write(Path::new(&workspace).join("deploy.sh"), "bakery.sh deploy\n").unwrap();
        let as_agent = |thread: &str| crate::actor::agent(thread, Seat::Orchestrator);
        let req = |title: &str, body: &str| ProposeReq {
            kind: "lesson".into(),
            title: title.into(),
            summary: format!("{title}, in one line"),
            body: body.into(),
            why: "a deploy broke".into(),
            how_to_apply: "before {step}, check the script".into(),
            evidence: vec![json!({"path": "deploy.sh"})],
            supersedes: None,
            scope: None,
        };
        let propose_as = |thread: &str, r: &ProposeReq| crate::actor::with(as_agent(thread), || propose(&state, r));
        let rate_as = |rating: &str| {
            let r = RateReq { id: "deploy-through-the-bakery".into(), rating: rating.into() };
            crate::actor::with(as_agent(&chef.stream_id), || rate(&state, &r)).unwrap().uses
        };

        let first = propose_as(&chef.stream_id, &req("Deploy with the script", "Always deploy with bakery.sh deploy; never by hand.")).unwrap();
        assert_eq!(first.action, "insert");
        let waiting = proposals(&state, &ProposalsQuery { status: None }).unwrap().proposals;
        assert_eq!((waiting.len(), waiting[0].note.scope.clone()), (1, format!("project:{}", app.project_id)));
        assert_eq!(waiting[0].note.evidence[0]["hash"], blake3::hash(b"bakery.sh deploy\n").to_hex().to_string());
        let again = propose_as(&chef.stream_id, &req("Deploy with the script", "Always deploy with bakery.sh deploy; never by hand.")).unwrap();
        assert_eq!((again.action.as_str(), again.note.as_deref()), ("none", first.proposal.as_deref()), "it already waits");

        let token = req("Token", "Use ghp_0123456789abcdefghijABCDEFGHIJ to push.");
        assert!(propose_as(&chef.stream_id, &token).unwrap_err().contains("secret"));
        assert!(propose_as(&chef.stream_id, &ProposeReq { evidence: vec![], ..req("Bare", "No evidence.") }).unwrap_err().contains("evidence"));
        let wide = ProposeReq { scope: Some("bakery".into()), ..req("Everyone", "For every project.") };
        assert!(propose_as(&chef.stream_id, &wide).unwrap_err().contains("may widen"));

        let keep = ReviewReq { proposal: first.proposal.clone().unwrap(), decision: "keep".into(), edits: NoteEdits::default(), why: String::new() };
        assert!(crate::actor::with(as_agent(&chef.stream_id), || review(&state, &keep)).unwrap_err().starts_with("only the operator"));
        let widened = ReviewReq { edits: NoteEdits { scope: Some("bakery".into()), ..NoteEdits::default() }, ..keep };
        let note = review(&state, &widened).unwrap().note.unwrap();
        assert_eq!(note, "deploy-with-the-script");
        let find = MemorySearchReq { query: "deploy script".into(), types: vec![], scopes: vec![], k: None, budget_tokens: None };
        let seen = crate::actor::with(as_agent(&outsider.stream_id), || search(&state, &find)).unwrap();
        assert_eq!(seen.notes[0].id, note, "widened to the Bakery by the operator");

        let close = ProposeReq { scope: Some("project".into()), ..req("Deploy with the script", "Deploy with bakery.sh deploy, and watch it finish.") };
        let close = propose_as(&chef.stream_id, &close).unwrap();
        assert_eq!(close.action, "insert", "another scope is another note");
        let declined = ReviewReq { proposal: close.proposal.clone().unwrap(), decision: "decline".into(), edits: NoteEdits::default(), why: "one is enough".into() };
        review(&state, &declined).unwrap();
        let back = ProposeReq { scope: Some("project".into()), ..req("Deploy with the script", "Deploy with bakery.sh deploy, and watch it finish.") };
        let back = propose_as(&chef.stream_id, &back).unwrap();
        assert!(back.action == "none" && back.said.contains("declined"), "{}", back.said);

        let replacing = ProposeReq { supersedes: Some(note.clone()), ..req("Deploy through the Bakery", "Deploys go through the Bakery's own deploy button now.") };
        let replacing = propose_as(&chef.stream_id, &replacing).unwrap();
        assert_eq!((replacing.action.as_str(), replacing.note.as_deref()), ("supersede", Some(note.as_str())));
        let all = keep_all(&state, &KeepAllReq { proposals: vec![] }).unwrap();
        assert_eq!((all.kept.len(), all.skipped.len()), (1, 0));
        assert!(index::page(&state, &note).unwrap().note.stale, "replaced");
        assert!(!index::page(&state, "deploy-through-the-bakery").unwrap().note.stale);
        std::fs::write(Path::new(&workspace).join("deploy.sh"), "bakery deploy --now\n").unwrap();
        assert!(index::page(&state, "deploy-through-the-bakery").unwrap().note.stale, "the file it rests on changed");

        let body = NoteEdits { body: Some("Replaced by the Bakery's deploy.".into()), ..NoteEdits::default() };
        assert_eq!(edit(&state, &EditNoteReq { id: Some(note.clone()), edits: body }).unwrap().note.version, 2);
        retire(&state, &RetireReq { id: note.clone(), why: "replaced".into(), merged_into: None }).unwrap();
        assert!(index::page(&state, &note).is_err(), "retired: out of the wiki");

        assert_eq!(rate_as("helped").helped, 1);
        let uses = rate_as("hurt");
        assert_eq!((uses.helped, uses.hurt), (0, 1), "a thread's latest word counts");

        let whats: Vec<String> = timeline(&state, &TimelineQuery { note: None, limit: None }).unwrap().moments.into_iter().map(|m| m.what).collect();
        for what in ["proposed", "kept", "declined", "edited", "retired"] {
            assert!(whats.iter().any(|w| w == what), "{what} in {whats:?}");
        }
        let digest = digest(&state).unwrap();
        assert!(digest.pending.is_empty() && !digest.day.is_empty());
    }

    /// An agent's own note, straight to its thread, is scratch: its thread
    /// alone sees it (not a peer, not the Baker's side), the operator sees
    /// all of them, and promotes one into a shared note that keeps its
    /// versions and rests on its scratch version. Each change goes out live.
    #[test]
    fn a_scratch_note_is_its_threads_alone_until_the_operator_promotes_it() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(bakery_agent::StubModel));
        let root = dir.path().join("app");
        std::fs::create_dir_all(&root).unwrap();
        let (project, chef) = crate::catalog::create_project(&state, "app", Some(crate::catalog::DirectoryRootSpec::Attach(root.to_str().unwrap()))).unwrap();
        let peer = crate::catalog::attach_directory_root(&state, &project.project_id, "peer", crate::catalog::DirectoryRootSpec::Attach(root.to_str().unwrap())).unwrap();
        let other = dir.path().join("baker");
        std::fs::create_dir_all(&other).unwrap();
        let (_, baker) = crate::catalog::create_project(&state, "baker", Some(crate::catalog::DirectoryRootSpec::Attach(other.to_str().unwrap()))).unwrap();
        state.node.lock().unwrap().baker_stream_id = Some(baker.stream_id.clone());
        let mut events = state.tx.subscribe();
        let write = crate::routes::memory::MemoryWriteReq {
            stream_id: chef.stream_id.clone(),
            text: "The flaky test needs a second run on CI.".into(),
            id: Some("flaky".into()),
            layer: None,
            sensitivity: None,
            shape: Default::default(),
        };
        crate::actor::with(crate::actor::agent(&chef.stream_id, Seat::Orchestrator), || crate::routes::memory::memory_write_inner(&state, &write)).unwrap();
        let id = format!("{}/flaky", chef.stream_id);
        let find = MemorySearchReq { query: "flaky test".into(), types: vec![], scopes: vec![], k: None, budget_tokens: None };
        let seen_by = |thread: &str| crate::actor::with(crate::actor::agent(thread, Seat::Orchestrator), || search(&state, &find)).unwrap().notes.len();
        assert_eq!((seen_by(&chef.stream_id), seen_by(&peer.stream_id), seen_by(&baker.stream_id)), (1, 0, 0), "its thread's alone");
        assert_eq!(search(&state, &find).unwrap().notes.len(), 1, "the operator sees it");
        let scratch = index::notes(&state, &index::MemoryNotesQuery { status: Some("scratch".into()), kind: None, scope: None }).unwrap().notes;
        assert_eq!(scratch.iter().map(|v| v.note.id.as_str()).collect::<Vec<_>>(), [id.as_str()]);
        assert!(index::notes(&state, &index::MemoryNotesQuery { status: None, kind: None, scope: None }).unwrap().notes.is_empty(), "the kept pages, without scratch");

        let promoted = promote(&state, &PromoteReq { id: id.clone(), scope: "bakery".into(), edits: NoteEdits { title: Some("Flaky CI test".into()), ..NoteEdits::default() } }).unwrap();
        assert_eq!((promoted.note.status.as_str(), promoted.note.scope.as_str(), promoted.note.trust.as_str(), promoted.note.version), ("kept", "bakery", "operator", 2));
        assert!(promoted.note.evidence.iter().any(|e| e["thread"] == chef.stream_id.as_str()), "it rests on its scratch version");
        assert_eq!(seen_by(&peer.stream_id), 1, "shared now");
        assert!(promote(&state, &PromoteReq { id: id.clone(), scope: "bakery".into(), edits: NoteEdits::default() }).unwrap_err().contains("not a scratch note"));
        let moments: Vec<String> = timeline(&state, &TimelineQuery { note: Some(id.clone()), limit: None }).unwrap().moments.into_iter().map(|m| m.what).collect();
        assert_eq!(moments, ["promoted", "scratch"]);
        let told: Vec<Value> = std::iter::from_fn(|| events.try_recv().ok()).filter_map(|e| serde_json::from_str::<Value>(&e).ok()).filter(|e| e["kind"] == "memory.updated").collect();
        assert_eq!(told.iter().map(|e| e["what"].as_str().unwrap_or_default()).collect::<Vec<_>>(), ["scratch", "promoted"]);

        let baker_note = crate::routes::memory::MemoryWriteReq { stream_id: baker.stream_id.clone(), id: Some("own".into()), ..write };
        crate::actor::with(crate::actor::agent(&baker.stream_id, Seat::Baker), || crate::routes::memory::memory_write_inner(&state, &baker_note)).unwrap();
        let own = index::page(&state, &format!("{}/own", baker.stream_id)).unwrap();
        assert_eq!((own.note.note.status.as_str(), own.note.note.scope.as_str()), ("kept", "baker"), "the Baker's own go straight in");
    }
}
