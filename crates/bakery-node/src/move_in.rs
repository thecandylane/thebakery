//! The move-in: a Claude Code session the operator ran outside the Bakery
//! (the Baker's, a worker's) comes into a Bakery thread with nothing lost.
//! Its worktree becomes the thread's (adopted; the Bakery's own main
//! checkout is the permanent Baker's conversation), a copy of its transcript
//! goes where the thread's Claude runtime reads its sessions (its seat's own
//! config, which a container seat mounts too), and the session is noted, so
//! the thread's next turn resumes it (`resume`): from its last summary, as
//! Claude Code itself resumes a compacted session. The operator's to do (a
//! button), and to place: the list holds the sessions of the Bakery's own
//! folders, each with where it can go on: a new workspace (its orc), a
//! worker under a workspace in the worktree it has, the Baker's own
//! conversation, or a Baker thread; the one it fits best suggested. A
//! conversation carries on one session: moving another into it is
//! refused, never a silent swap. What the original gains after it
//! moved in comes along until the thread carries it on. The originals are
//! only ever read.

use crate::node::Seat;
use crate::state::AppState;
use axum::{
    extract::State,
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::SystemTime;

/// A session moved in: `{session_id, title, from, worktree, bytes,
/// compactions}`; again each time what the original gained came along.
pub(crate) const IMPORTED: &str = "move_in.imported";

/// The operator's own Claude Code sessions, by project folder.
fn claude_projects(state: &AppState) -> PathBuf {
    if cfg!(test) {
        return state.base.join("operator-claude").join("projects");
    }
    let home = state.base.parent().map(Path::to_path_buf).unwrap_or_default();
    home.join(".claude").join("projects")
}

/// Claude Code's folder name for a workspace: its path with every other
/// character a dash.
fn claude_project(workspace: &Path) -> String {
    workspace
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// What carries a session on.
#[derive(schemars::JsonSchema, Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Kind {
    /// A workspace's orc: beneath the Baker, on a worktree of its own.
    Workspace,
    /// A worker under a workspace's orc, in a worktree of its own.
    Worker,
    /// The permanent Baker's own conversation, on the main checkout.
    Baker,
    /// A Baker thread.
    BakerThread,
}

/// A workspace's orc, as a new worker's destination offers it.
#[derive(schemars::JsonSchema, Debug, Clone, Serialize)]
pub(crate) struct Orc {
    pub(crate) stream_id: String,
    pub(crate) title: String,
}

/// Where a session can go on: a conversation already there, or one that
/// moving it in opens.
#[derive(schemars::JsonSchema, Debug, Clone, Serialize)]
pub(crate) struct Destination {
    /// What `POST /move-in/import` takes as `destination`.
    pub(crate) key: String,
    pub(crate) kind: Kind,
    /// In the operator's words.
    pub(crate) label: String,
    /// The folder it works in there; none for a new workspace, whose
    /// worktree is made as it moves in.
    pub(crate) worktree: Option<String>,
    /// The conversation already there, which it moves into.
    pub(crate) stream_id: Option<String>,
    /// For a new worker: the workspaces it can go under. The import's `orc`
    /// picks one; with one alone, that one.
    pub(crate) orcs: Vec<Orc>,
    /// The title of the session the conversation there already carries on
    /// (its own title, for a session it began itself): moving in is
    /// refused.
    pub(crate) taken_by: Option<String>,
}

#[derive(schemars::JsonSchema, Debug, Clone, Serialize)]
pub(crate) struct ClaudeSession {
    pub(crate) session_id: String,
    /// The transcript, to import by.
    pub(crate) path: String,
    /// The folder it worked in, as it first recorded.
    pub(crate) cwd: Option<String>,
    pub(crate) modified: String,
    pub(crate) bytes: u64,
    /// How many times it was compacted: a resume picks up from the last.
    pub(crate) compactions: usize,
    /// Its newest custom title, else its newest AI title or summary, else
    /// its first message, cut short.
    pub(crate) title: Option<String>,
    /// The newest title the operator gave it in Claude Code.
    pub(crate) custom_title: Option<String>,
    /// The conversation that carries it on, while that conversation resumes
    /// it.
    pub(crate) moved_in: Option<String>,
    /// Moved in, and the original went on since while the thread hasn't
    /// carried it on, nor is it mid-turn: importing it to the same place
    /// (`suggested_destination`) brings in what is new.
    pub(crate) can_bring_in: bool,
    /// Where it can go on: a new workspace, each workspace's orc, and for
    /// each of the Bakery's folders it worked in, the most used first, the
    /// conversation there, or a worker or a Baker thread made for it.
    pub(crate) destinations: Vec<Destination>,
    /// The destination it fits best, never one that would refuse it: where
    /// it moved in; else a session of the main checkout goes on as a new
    /// workspace's orc, and one of a worktree in the conversation there, or
    /// as a worker under the one workspace there is.
    pub(crate) suggested_destination: Option<String>,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct ClaudeSessions {
    /// Newest first.
    pub(crate) sessions: Vec<ClaudeSession>,
}

/// One of the Bakery's own folders: its main checkout or a linked worktree.
struct Folder {
    path: PathBuf,
    label: String,
    main: bool,
}

/// The Bakery's folders, as git lists them from its main checkout, the
/// deepest first: a worktree inside the main checkout is its own.
fn folders(state: &AppState) -> Vec<Folder> {
    let main = crate::baker_threads::baker_pointer(state)
        .and_then(|home| crate::state::context_for(state, &home).ok())
        .and_then(|ctx| std::fs::canonicalize(ctx.worktree_path).ok());
    let Some(main) = main else {
        return Vec::new();
    };
    let listed = crate::git::run(&main, &["worktree", "list", "--porcelain"]).unwrap_or_default();
    let mut found = Vec::new();
    for block in listed.split("\n\n") {
        let field = |key: &str| block.lines().find_map(|l| l.strip_prefix(key));
        let Some(path) = field("worktree ").and_then(|p| std::fs::canonicalize(p).ok()) else {
            continue;
        };
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let label = field("branch ").map(|b| b.trim_start_matches("refs/heads/").to_string()).unwrap_or(name);
        found.push(Folder { main: path == main, path, label });
    }
    found.sort_by_key(|f| std::cmp::Reverse(f.path.components().count()));
    found
}

/// The folder `cwd` is in: the nearest one holding it.
fn folder_of(folders: &[Folder], cwd: &str) -> Option<usize> {
    let at = std::fs::canonicalize(cwd).unwrap_or_else(|_| PathBuf::from(cwd));
    folders.iter().position(|f| at.starts_with(&f.path))
}

/// A transcript entry's `cwd`, read without parsing the whole entry.
fn cwd_in(line: &str) -> Option<String> {
    let at = line.find("\"cwd\":")? + 6;
    String::deserialize(&mut serde_json::Deserializer::from_str(&line[at..])).ok()
}

/// What a transcript says of itself: its titles, how often it was
/// compacted, and how many of its entries it recorded in each folder, as
/// it named the folder.
#[derive(Clone)]
struct Described {
    session: ClaudeSession,
    cwds: Vec<(String, usize)>,
}

fn describe(path: &Path) -> Option<Described> {
    use std::io::BufRead;
    let meta = std::fs::metadata(path).ok()?;
    let session_id = path.file_stem()?.to_string_lossy().to_string();
    let mut compactions = 0;
    let (mut custom, mut ai, mut summary, mut first) = (None, None, None, None);
    let mut cwds: Vec<(String, usize)> = Vec::new();
    let file = std::io::BufReader::new(std::fs::File::open(path).ok()?);
    for line in file.lines().map_while(Result::ok) {
        if line.contains("\"compact_boundary\"") {
            compactions += 1;
        }
        if let Some(at) = cwd_in(&line) {
            match cwds.iter_mut().find(|(cwd, _)| *cwd == at) {
                Some((_, n)) => *n += 1,
                None => cwds.push((at, 1)),
            }
        }
        let titled = ["\"custom-title\"", "\"ai-title\"", "\"summary\""].iter().any(|k| line.contains(k));
        if !titled && (first.is_some() || !line.contains("\"user\"")) {
            continue;
        }
        let Ok(entry) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        let text = |key: &str| entry[key].as_str().map(str::trim).filter(|t| !t.is_empty()).map(str::to_string);
        match entry["type"].as_str() {
            Some("custom-title") => custom = text("customTitle").or(custom),
            Some("ai-title") => ai = text("aiTitle").or(ai),
            Some("summary") => summary = text("summary").or(summary),
            Some("user") if first.is_none() && entry["isMeta"] != true && entry["isCompactSummary"] != true => {
                let content = &entry["message"]["content"];
                let said = content
                    .as_str()
                    .or_else(|| content.as_array()?.iter().find(|b| b["type"] == "text")?["text"].as_str());
                first = said.map(str::trim).filter(|t| !t.is_empty()).map(|t| crate::agent_loop::clip_head(t, 120));
            }
            _ => {}
        }
    }
    let modified = meta.modified().ok().map(chrono::DateTime::<chrono::Utc>::from)?.to_rfc3339();
    let session = ClaudeSession {
        session_id,
        path: path.to_string_lossy().into_owned(),
        cwd: cwds.first().map(|(cwd, _)| cwd.clone()),
        modified,
        bytes: meta.len(),
        compactions,
        title: custom.clone().or(ai).or(summary).or(first),
        custom_title: custom,
        moved_in: None,
        can_bring_in: false,
        destinations: Vec::new(),
        suggested_destination: None,
    };
    Some(Described { session, cwds })
}

/// [`describe`], kept while the transcript stays as it was: the list reads
/// every one of the Bakery's sessions, some of them large.
fn described(path: &Path) -> Option<Described> {
    type Kept = HashMap<PathBuf, ((u64, SystemTime), Described)>;
    static KEPT: LazyLock<Mutex<Kept>> = LazyLock::new(Default::default);
    let meta = std::fs::metadata(path).ok()?;
    let stamp = (meta.len(), meta.modified().ok()?);
    if let Some((at, kept)) = KEPT.lock().unwrap().get(path) {
        if *at == stamp {
            return Some(kept.clone());
        }
    }
    let fresh = describe(path)?;
    KEPT.lock().unwrap().insert(path.to_path_buf(), (stamp, fresh.clone()));
    Some(fresh)
}

/// A conversation a session can be carried on in, and the session it
/// carries on.
struct Home {
    stream_id: String,
    title: String,
    kind: Kind,
    seat: Seat,
    /// The folder it works in.
    path: PathBuf,
    /// The Claude session it resumes, and the title it moved in with, when
    /// it moved in.
    carries: Option<(String, Option<String>)>,
}

impl Home {
    /// The title of the session it carries on instead of `session`.
    fn taken_by(&self, session: &str) -> Option<String> {
        let (_, moved) = self.carries.as_ref().filter(|(other, _)| other != session)?;
        Some(moved.clone().unwrap_or_else(|| self.title.clone()))
    }

    /// The conversation it is, in the operator's words.
    fn named(&self, folders: &[Folder]) -> String {
        let label = folders.iter().find(|f| f.path == self.path).map_or(self.title.as_str(), |f| f.label.as_str());
        match self.kind {
            Kind::Baker => "The Baker's own conversation".to_string(),
            Kind::BakerThread => format!("The thread in {label}"),
            Kind::Worker => format!("The worker in {label}"),
            Kind::Workspace => format!("The workspace “{}”", self.title),
        }
    }

    /// Its destination's key.
    fn key(&self) -> String {
        match self.kind {
            Kind::Baker => "baker".to_string(),
            _ => self.stream_id.clone(),
        }
    }

    fn destination(&self, session: &str) -> Destination {
        let label = match self.kind {
            Kind::Baker => "The Baker's own conversation".to_string(),
            Kind::Workspace => format!("Workspace “{}”", self.title),
            Kind::Worker => format!("Worker “{}”", self.title),
            Kind::BakerThread => format!("Baker thread “{}”", self.title),
        };
        Destination {
            key: self.key(),
            kind: self.kind,
            label,
            worktree: Some(self.path.to_string_lossy().into_owned()),
            stream_id: Some(self.stream_id.clone()),
            orcs: Vec::new(),
            taken_by: self.taken_by(session),
        }
    }
}

fn carries(state: &AppState, stream_id: &str, seat: Seat) -> Option<(String, Option<String>)> {
    let agent_id = crate::node::agent_id(stream_id, seat);
    // One nothing was asked in holds nothing (a fresh one replaces it), so
    // it never stands in a move-in's way.
    let session = crate::resume::last_session(state, stream_id, &agent_id, crate::claude_native::ACCOUNT_ID)
        .filter(|session| crate::resume::kept(state, stream_id, &agent_id, session))?;
    let moved = state.heart.lock().unwrap().events_of_family(stream_id, IMPORTED).unwrap_or_default();
    let title = moved.iter().rev().find_map(|e| {
        let fact: serde_json::Value = serde_json::from_str(&e.payload).ok()?;
        (fact["session_id"] == session.as_str()).then(|| fact["title"].as_str().unwrap_or(&session).to_string())
    });
    Some((session, title))
}

/// How big `session`'s transcript was when it last came into `stream_id`.
fn brought(state: &AppState, stream_id: &str, session: &str) -> Option<u64> {
    let moved = state.heart.lock().unwrap().events_of_family(stream_id, IMPORTED).unwrap_or_default();
    moved.iter().rev().find_map(|e| {
        let fact: serde_json::Value = serde_json::from_str(&e.payload).ok()?;
        (fact["session_id"] == session).then(|| fact["bytes"].as_u64()).flatten()
    })
}

/// The conversations a session can be carried on in, neither archived nor
/// discarded: the permanent Baker's own first, its threads, the
/// workspaces' orcs and their workers.
fn conversations(state: &AppState) -> Vec<Home> {
    let Some(baker) = crate::baker_threads::baker_pointer(state) else {
        return Vec::new();
    };
    let streams = crate::state::known_streams(state).unwrap_or_default();
    let found: Vec<(String, String, Kind, Seat, String)> = {
        let heart = state.heart.lock().unwrap();
        streams
            .into_iter()
            .filter(|s| !s.discarded)
            .filter_map(|s| {
                // A catalog project's are its own, never the Bakery's.
                if crate::state::socket_project(&heart, &s.stream_id).0.is_some() {
                    return None;
                }
                let own = s.title.clone().unwrap_or_else(|| s.name.clone());
                let (kind, seat, title) = match crate::baker_threads::thread_info_locked(&heart, Some(&baker), &s.stream_id) {
                    Some(thread) if thread.archived => return None,
                    Some(thread) if thread.main => (Kind::Baker, Seat::Baker, thread.title),
                    Some(thread) => (Kind::BakerThread, Seat::Baker, thread.title),
                    None if crate::agents::socket_archived_locked(&heart, &s.stream_id) => return None,
                    None if crate::workspaces::is_workspace_locked(&heart, &s.stream_id) => (Kind::Workspace, Seat::Orchestrator, own),
                    None => {
                        let orc = crate::baker_threads::opened_by_locked(&heart, &s.stream_id)?;
                        if !crate::workspaces::is_workspace_locked(&heart, &orc) {
                            return None;
                        }
                        (Kind::Worker, Seat::Orchestrator, own)
                    }
                };
                Some((s.stream_id, title, kind, seat, s.worktree_path))
            })
            .collect()
    };
    let mut homes: Vec<Home> = found
        .into_iter()
        .filter_map(|(stream_id, title, kind, seat, worktree)| {
            let path = std::fs::canonicalize(worktree).ok()?;
            let carries = carries(state, &stream_id, seat);
            Some(Home { stream_id, title, kind, seat, path, carries })
        })
        .collect();
    homes.sort_by_key(|h| h.kind != Kind::Baker);
    homes
}

/// Whether the original went on after it moved into `home`, and the thread
/// hasn't carried it on since: the thread's copy is as it was brought, and
/// the original is another size. An import's own rule for bringing in what
/// is new.
fn grew_since(state: &AppState, home: &Home, session: &ClaudeSession) -> bool {
    let config = crate::claude_native::seat_config_dir(state, &home.stream_id, home.seat.as_str());
    let copy = config.join("projects").join(claude_project(&home.path)).join(format!("{}.jsonl", session.session_id));
    let Ok(had) = std::fs::metadata(copy).map(|m| m.len()) else {
        return false;
    };
    had != session.bytes && brought(state, &home.stream_id, &session.session_id) == Some(had)
}

/// The conversation `session` moved into, while it carries it on.
fn moved_into<'a>(homes: &'a [Home], session: &str) -> Option<&'a Home> {
    homes.iter().find(|h| matches!(&h.carries, Some((s, Some(_))) if s == session))
}

/// Where a session can go on, from the folders it worked in (`used`, the
/// most used first), each one's conversation (`at`), and the workspaces.
/// The tree this node runs from takes no conversation of its own.
fn destinations(session: &str, used: &[usize], folders: &[Folder], at: &[Option<&Home>], orcs: &[&Home], running: Option<&Path>) -> Vec<Destination> {
    let new_workspace = Destination {
        key: "workspace".to_string(),
        kind: Kind::Workspace,
        label: "A new workspace".to_string(),
        worktree: None,
        stream_id: None,
        orcs: Vec::new(),
        taken_by: None,
    };
    let mut all = vec![new_workspace];
    all.extend(orcs.iter().map(|orc| orc.destination(session)));
    for &i in used {
        let folder = &folders[i];
        match at[i] {
            Some(home) if home.kind == Kind::Workspace => {}
            Some(home) => all.push(home.destination(session)),
            None if folder.main || Some(folder.path.as_path()) == running => {}
            None => {
                let path = folder.path.to_string_lossy().into_owned();
                if !orcs.is_empty() {
                    all.push(Destination {
                        key: format!("worker:{path}"),
                        kind: Kind::Worker,
                        label: format!("A worker in {}", folder.label),
                        worktree: Some(path.clone()),
                        stream_id: None,
                        orcs: orcs.iter().map(|o| Orc { stream_id: o.stream_id.clone(), title: o.title.clone() }).collect(),
                        taken_by: None,
                    });
                }
                all.push(Destination {
                    key: format!("thread:{path}"),
                    kind: Kind::BakerThread,
                    label: format!("A Baker thread in {}", folder.label),
                    worktree: Some(path),
                    stream_id: None,
                    orcs: Vec::new(),
                    taken_by: None,
                });
            }
        }
    }
    all
}

/// The destination a session fits best, from the folder it worked in
/// most, never one that would refuse it: the main checkout's goes on as a
/// new workspace's orc; a worktree's, in its conversation there (unless it
/// carries another or is mid-turn), or as a worker under the one workspace
/// there is (never in the tree this node runs from).
fn suggest(folder: &Folder, home: Option<&Home>, orcs: &[&Home], session: &str, busy: &HashSet<String>, running: Option<&Path>) -> Option<String> {
    if folder.main {
        return Some("workspace".to_string());
    }
    match home {
        Some(home) => (home.taken_by(session).is_none() && !busy.contains(&home.stream_id)).then(|| home.key()),
        None if Some(folder.path.as_path()) == running => None,
        None if orcs.len() == 1 => Some(format!("worker:{}", folder.path.to_string_lossy())),
        None => None,
    }
}

/// Where an import lands: a conversation already there, or one to open.
enum Landing {
    At(usize),
    NewWorkspace,
    NewWorker { path: PathBuf, orc: String },
    NewThread { path: PathBuf },
}

/// The folder `raw` names, one of the Bakery's, else why it isn't.
fn folder_named(folders: &[Folder], raw: &str) -> Result<usize, (StatusCode, String)> {
    let path = std::fs::canonicalize(raw).map_err(|e| bad(format!("{raw}: {e}")))?;
    if let Some(i) = folders.iter().position(|f| f.path == path) {
        return Ok(i);
    }
    Err(bad(match folder_of(folders, &path.to_string_lossy()).map(|i| &folders[i]) {
        Some(Folder { main: true, path, .. }) => format!("{raw} is a folder inside the main checkout ({}): pick the main checkout itself", path.display()),
        Some(Folder { path, label, .. }) => format!("{raw} is a folder inside the worktree {label} ({}): pick that worktree itself", path.display()),
        None => format!("{raw} is not one of the Bakery's folders: its main checkout or a worktree"),
    }))
}

/// Where `req` sends the session: its destination.
fn landing(req: &ImportReq, folders: &[Folder], convs: &[Home]) -> Result<Landing, (StatusCode, String)> {
    let at = |i: usize| convs.iter().position(|h| h.path == folders[i].path);
    // A worktree no conversation works in yet.
    let free = |raw: &str| -> Result<PathBuf, (StatusCode, String)> {
        let i = folder_named(folders, raw)?;
        if folders[i].main {
            return Err(bad("the main checkout is the Baker's own conversation: a worker or a thread of its own works in a worktree"));
        }
        if let Some(c) = at(i) {
            return Err(conflict(format!("{} already works there: move the session into it instead", convs[c].named(folders))));
        }
        Ok(folders[i].path.clone())
    };
    match req.destination.trim() {
        "" => Err(bad("say where it goes on: one of its destinations")),
        "workspace" => Ok(Landing::NewWorkspace),
        "baker" => convs.iter().position(|h| h.kind == Kind::Baker).map(Landing::At).ok_or_else(|| bad("there is no Baker yet: open it first")),
        key => {
            if let Some(raw) = key.strip_prefix("worker:") {
                let path = free(raw)?;
                let orcs: Vec<&Home> = convs.iter().filter(|h| h.kind == Kind::Workspace).collect();
                let orc = match (req.orc.as_deref(), orcs.as_slice()) {
                    (Some(orc), _) => orcs.iter().find(|h| h.stream_id == orc).ok_or_else(|| bad("that is not a workspace's orc"))?,
                    (None, [one]) => one,
                    (None, []) => return Err(bad("there is no workspace yet: open one, or move its orc's session in first")),
                    (None, _) => return Err(bad("say which workspace it goes under: its orc")),
                };
                return Ok(Landing::NewWorker { path, orc: orc.stream_id.clone() });
            }
            if let Some(raw) = key.strip_prefix("thread:") {
                return Ok(Landing::NewThread { path: free(raw)? });
            }
            let known = convs.iter().position(|h| h.stream_id == key).map(Landing::At);
            known.ok_or_else(|| bad(format!("{key} is not a place a session can go on")))
        }
    }
}

/// The operator's Claude Code sessions of the Bakery's own folders, to pick
/// from and place: Claude Code files a session under the folder it started
/// in, and it counts when it worked in one of the Bakery's.
pub(crate) fn sessions(state: &AppState) -> ClaudeSessions {
    let folders = folders(state);
    let names: Vec<String> = folders.iter().map(|f| claude_project(&f.path)).collect();
    let ours = |name: &str| names.iter().any(|n| name.strip_prefix(n.as_str()).is_some_and(|rest| rest.is_empty() || rest.starts_with('-')));
    let mut found: Vec<(SystemTime, PathBuf)> = Vec::new();
    for project in std::fs::read_dir(claude_projects(state)).into_iter().flatten().flatten() {
        if !ours(&project.file_name().to_string_lossy()) {
            continue;
        }
        for file in std::fs::read_dir(project.path()).into_iter().flatten().flatten() {
            let path = file.path();
            if path.extension().is_some_and(|x| x == "jsonl") {
                let at = file.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
                found.push((at, path));
            }
        }
    }
    found.sort_by(|a, b| b.0.cmp(&a.0));
    let convs = conversations(state);
    let at: Vec<Option<&Home>> = folders.iter().map(|f| convs.iter().find(|h| h.path == f.path)).collect();
    let orcs: Vec<&Home> = convs.iter().filter(|h| h.kind == Kind::Workspace).collect();
    let busy: HashSet<String> = state.lifecycle.lock().unwrap().running_turns.iter().filter(|(_, n)| **n > 0).map(|((s, _), _)| s.clone()).collect();
    let running = crate::versions::source_root(state).and_then(|root| std::fs::canonicalize(root).ok());
    let mut seen: HashMap<String, Option<usize>> = HashMap::new();
    let sessions = found
        .into_iter()
        .filter_map(|(_, path)| described(&path))
        .filter_map(|Described { mut session, cwds }| {
            let mut uses = vec![0; folders.len()];
            for (cwd, n) in &cwds {
                if let Some(i) = *seen.entry(cwd.clone()).or_insert_with(|| folder_of(&folders, cwd)) {
                    uses[i] += n;
                }
            }
            let mut used: Vec<usize> = (0..uses.len()).filter(|&i| uses[i] > 0).collect();
            if used.is_empty() {
                return None;
            }
            used.sort_by_key(|&i| std::cmp::Reverse(uses[i]));
            let id = session.session_id.clone();
            let moved = moved_into(&convs, &id);
            session.moved_in = moved.map(|h| h.stream_id.clone());
            session.can_bring_in = moved.is_some_and(|home| !busy.contains(&home.stream_id) && grew_since(state, home, &session));
            session.destinations = destinations(&id, &used, &folders, &at, &orcs, running.as_deref());
            session.suggested_destination = match moved {
                Some(home) => (!busy.contains(&home.stream_id)).then(|| home.key()),
                None => suggest(&folders[used[0]], at[used[0]], &orcs, &id, &busy, running.as_deref()),
            };
            Some(session)
        })
        .collect();
    ClaudeSessions { sessions }
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct ImportReq {
    /// The session's transcript, as `GET /move-in/sessions` lists it.
    pub(crate) transcript: String,
    /// Where it goes on: one of its destinations' keys.
    pub(crate) destination: String,
    /// For a new worker: the workspace it goes under, by its orc's
    /// stream_id; the one workspace there is when absent.
    #[serde(default)]
    pub(crate) orc: Option<String>,
    /// The name of a conversation it opens; the session's title when absent.
    #[serde(default)]
    pub(crate) title: Option<String>,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct Imported {
    /// The conversation that resumes it.
    pub(crate) stream_id: String,
    pub(crate) session_id: String,
    pub(crate) bytes: u64,
    pub(crate) compactions: usize,
}

/// Copies `from` (a file or a folder) to `to`, all of it, leaving `from` as
/// it was. Each file lands whole: it is written beside, then renamed.
fn copy_all(from: &Path, to: &Path) -> std::io::Result<()> {
    if from.is_dir() {
        std::fs::create_dir_all(to)?;
        for entry in std::fs::read_dir(from)?.flatten() {
            copy_all(&entry.path(), &to.join(entry.file_name()))?;
        }
        return Ok(());
    }
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut part = to.as_os_str().to_owned();
    part.push(".part");
    std::fs::copy(from, &part)?;
    std::fs::rename(&part, to)
}

fn bad(e: impl Into<String>) -> (StatusCode, String) {
    (StatusCode::BAD_REQUEST, e.into())
}

fn conflict(e: impl Into<String>) -> (StatusCode, String) {
    (StatusCode::CONFLICT, e.into())
}

/// Moves a Claude Code session into the conversation of the folder the
/// operator picked: a copy of its transcript (and what it keeps beside it)
/// where the thread's Claude runtime reads sessions, the thread on Claude,
/// and the session noted for its next turn to resume. Again, what the
/// original gained since comes along while the thread has not carried it
/// on; nothing new, nothing changes. Refused, with nothing changed (409): a
/// conversation that carries on another session or is mid-turn, a session
/// already carried on elsewhere, and a session both sides went on with.
pub(crate) fn import(state: &AppState, req: &ImportReq) -> Result<Imported, (StatusCode, String)> {
    let projects = std::fs::canonicalize(claude_projects(state)).map_err(|e| bad(format!("no Claude Code sessions here: {e}")))?;
    let transcript = std::fs::canonicalize(&req.transcript).map_err(|e| bad(format!("{}: {e}", req.transcript)))?;
    if !transcript.starts_with(&projects) || transcript.extension().is_none_or(|x| x != "jsonl") {
        return Err(bad("that is not one of the operator's Claude Code sessions"));
    }
    if crate::baker_threads::baker_pointer(state).is_none() {
        return Err(bad("there is no Baker yet: open it first"));
    }
    let session = described(&transcript).ok_or_else(|| bad("that transcript can't be read"))?.session;
    let well_formed = !session.session_id.is_empty() && session.session_id.chars().all(|c| c.is_ascii_hexdigit() || c == '-');
    if !well_formed {
        return Err(bad(format!("{} is not a Claude Code session id", session.session_id)));
    }
    let folders = folders(state);
    let convs = conversations(state);
    let landing = landing(req, &folders, &convs)?;
    let here = match &landing {
        Landing::At(i) => Some(&convs[*i]),
        _ => None,
    };
    if let Some(there) = moved_into(&convs, &session.session_id).filter(|h| here.is_none_or(|here| here.stream_id != h.stream_id)) {
        return Err(conflict(format!("This session already carries on in “{}”: a session moves in once.", there.title)));
    }
    if let Some(home) = here {
        if let Some((_, moved)) = home.carries.as_ref().filter(|(other, _)| *other != session.session_id) {
            let what = moved.as_ref().map_or("a session of its own".to_string(), |title| format!("“{title}”"));
            let refused = format!("{} already carries on {what}: moving this session in would leave it behind. Pick another place.", home.named(&folders));
            return Err(conflict(refused));
        }
        let turns = state.lifecycle.lock().unwrap().running_turns.get(&(home.stream_id.clone(), home.seat)).copied().unwrap_or(0);
        if turns > 0 {
            return Err(conflict(format!("{} is in the middle of a turn: let it finish, or stop it, then move the session in.", home.named(&folders))));
        }
    }
    let title = req
        .title
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .or_else(|| session.title.clone())
        .unwrap_or_else(|| "Moved in".to_string());
    let named = crate::agent_loop::clip_head(&title, 60);
    let adopt = |path: &Path| crate::baker_threads::CreateThreadReq {
        title: Some(named.clone()),
        adopt: Some(path.to_string_lossy().into_owned()),
        ..Default::default()
    };
    let (stream_id, seat, worktree) = match landing {
        Landing::At(i) => (convs[i].stream_id.clone(), convs[i].seat, convs[i].path.clone()),
        Landing::NewWorkspace => {
            let made = crate::workspaces::create_workspace(state, &named, None, "human").map_err(bad)?;
            let path = std::fs::canonicalize(&made.worktree_path).map_err(|e| bad(format!("{}: {e}", made.worktree_path)))?;
            (made.stream_id, Seat::Orchestrator, path)
        }
        Landing::NewWorker { path, orc } => {
            let peer = crate::baker_threads::create_peer(state, &adopt(&path), &orc, "human").map_err(bad)?;
            (peer.thread.stream_id, Seat::Orchestrator, path)
        }
        Landing::NewThread { path } => {
            let thread = crate::baker_threads::create_thread(state, &adopt(&path), "human", None, None).map_err(bad)?;
            (thread.thread.stream_id, Seat::Baker, path)
        }
    };

    // Where its runtime reads sessions: its seat's own config, which a
    // container seat mounts, its worktree at the same path.
    let config = crate::claude_native::seat_config_dir(state, &stream_id, seat.as_str());
    let into = config.join("projects").join(claude_project(&worktree));
    let to = into.join(format!("{}.jsonl", session.session_id));
    let brought = brought(state, &stream_id, &session.session_id);
    let copy = match std::fs::metadata(&to).map(|m| m.len()).ok() {
        None => true,
        Some(had) if had == session.bytes => false,
        // The original went on after it moved in; the thread has not
        // carried it on yet: what is new comes along.
        Some(had) if Some(had) == brought => true,
        // The thread carried it on; the original stayed as it came in.
        Some(_) if brought == Some(session.bytes) => false,
        Some(_) if brought.is_none() => {
            return Err(conflict("The thread already holds another copy of this session: nothing was overwritten."));
        }
        Some(_) => {
            return Err(conflict(
                "Both went on after it moved in: the original grew, and the thread has carried the session on since. Nothing was overwritten.",
            ));
        }
    };
    let model = crate::claude_native::ACCOUNT_ID;
    let agent_id = crate::node::agent_id(&stream_id, seat);
    let resumes = crate::resume::last_session(state, &stream_id, &agent_id, model).as_deref() == Some(session.session_id.as_str());
    if !copy && resumes {
        // Already here, and nothing new: nothing changes.
        return Ok(Imported { stream_id, session_id: session.session_id, bytes: session.bytes, compactions: session.compactions });
    }
    if copy {
        copy_all(&transcript, &to).map_err(|e| bad(format!("copying the transcript: {e}")))?;
        let beside = transcript.with_extension("");
        if beside.is_dir() {
            copy_all(&beside, &into.join(&session.session_id)).map_err(|e| bad(format!("copying its folder: {e}")))?;
        }
    }
    // The thread's copy as it landed, noted at once: its own size (the
    // original may have grown while it copied), so what a later import
    // brings in is measured from it, whatever happens next.
    let bytes = std::fs::metadata(&to).map(|m| m.len()).map_err(|e| bad(format!("the copy: {e}")))?;
    let fact = serde_json::json!({
        "session_id": session.session_id,
        "title": session.title.clone().unwrap_or(title),
        "from": transcript.to_string_lossy(),
        "worktree": worktree.to_string_lossy(),
        "bytes": bytes,
        "compactions": session.compactions,
    });
    crate::state::append(state, &stream_id, "human", IMPORTED, fact.to_string(), vec![]).map_err(bad)?;
    // Its runtime starts again, on Claude, and reads the session afresh.
    crate::routes::seats::seat_assign_checked(state, &stream_id, seat, model).map_err(|(_, e)| bad(e))?;
    if !resumes {
        crate::resume::note_session(state, &stream_id, &agent_id, model, &session.session_id, false);
    }
    let roots = [("CLAUDE_CONFIG_DIR".to_string(), config)];
    crate::resume::keep_session(state, &agent_id, &session.session_id, &roots);
    Ok(Imported { stream_id, session_id: session.session_id, bytes, compactions: session.compactions })
}

fn operator_only() -> Result<(), (StatusCode, String)> {
    if crate::actor::agent_calling() {
        return Err((StatusCode::FORBIDDEN, "moving sessions in is the operator's to do".to_string()));
    }
    Ok(())
}

pub(crate) async fn sessions_route(State(st): State<AppState>) -> Result<Json<ClaudeSessions>, (StatusCode, String)> {
    operator_only()?;
    crate::actor::spawn_blocking(move || Json(sessions(&st)))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

pub(crate) async fn import_route(State(st): State<AppState>, Json(req): Json<ImportReq>) -> Result<Json<Imported>, (StatusCode, String)> {
    operator_only()?;
    crate::actor::spawn_blocking(move || import(&st, &req))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map(Json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;

    /// The Bakery's repo with the permanent Baker on its main checkout:
    /// the state, the repo, the Baker's conversation and the main checkout.
    fn bakery(dir: &Path) -> (AppState, PathBuf, String, PathBuf) {
        let repo = bakery_repo_for_test(dir);
        run_git(&repo, &["add", "."]);
        run_git(&repo, &["commit", "-qm", "the bakery"]);
        let state = state_with_bakery_root(dir, &repo);
        let home = crate::routes::baker::ensure_permanent_baker(&state, None, None).unwrap().stream_id;
        let main = std::fs::canonicalize(crate::state::context_for(&state, &home).unwrap().worktree_path).unwrap();
        (state, repo, home, main)
    }

    /// A transcript as Claude Code files it, under the folder it started
    /// in, with a subagent's beside it.
    fn transcript(state: &AppState, started: &Path, sid: &str, lines: &[String]) -> PathBuf {
        let folder = claude_projects(state).join(claude_project(started));
        std::fs::create_dir_all(folder.join(sid).join("subagents")).unwrap();
        std::fs::write(folder.join(sid).join("subagents/agent-a1.jsonl"), "{}\n").unwrap();
        let path = folder.join(format!("{sid}.jsonl"));
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();
        path
    }

    fn said(cwd: &Path, text: &str) -> String {
        format!(r#"{{"type":"user","cwd":"{}","message":{{"role":"user","content":"{text}"}}}}"#, cwd.display())
    }

    fn grow(path: &Path, line: &str) {
        use std::io::Write;
        writeln!(std::fs::OpenOptions::new().append(true).open(path).unwrap(), "{line}").unwrap();
    }

    fn to(transcript: &Path, destination: &str) -> ImportReq {
        ImportReq { transcript: transcript.display().to_string(), destination: destination.to_string(), orc: None, title: None }
    }

    /// A Baker thread made for a worktree no conversation works in yet.
    fn thread_in(worktree: &Path) -> String {
        format!("thread:{}", worktree.display())
    }

    /// A worker's session, recorded by Claude Code outside the Bakery,
    /// moves into a thread that adopts its worktree: the transcript and its
    /// folder copied where the thread's runtime reads them, the session
    /// noted to resume, the original untouched. Again, nothing changes;
    /// what the original gained comes along until the thread carries it on;
    /// once both went on, nothing is overwritten. A file outside the
    /// operator's sessions, and a folder inside a worktree, are refused
    /// with the reason.
    #[test]
    fn a_workers_session_moves_into_a_thread_with_nothing_lost_and_what_is_new_comes_along() {
        let dir = tempfile::tempdir().unwrap();
        let (state, repo, _, main) = bakery(dir.path());
        let w1 = main.with_file_name("w1");
        run_git(&repo, &["worktree", "add", "-q", "-b", "w1/baker-hands", w1.to_str().unwrap()]);
        std::fs::create_dir_all(w1.join("crates/node")).unwrap();
        let sid = "2ad5c475-501e-4b27-a18e-07fbb80451c2";
        let lines = [
            said(&main, "Build the catching"),
            said(&w1, "build it"),
            r#"{"type":"system","subtype":"compact_boundary"}"#.to_string(),
            r#"{"type":"user","isCompactSummary":true,"message":{"role":"user","content":"Summary: the catching is built"}}"#.to_string(),
            r#"{"type":"summary","summary":"The catching, on doors"}"#.to_string(),
            said(&w1, "test it"),
        ];
        let original = transcript(&state, &main, sid, &lines);
        let before = std::fs::read(&original).unwrap();

        let listed = sessions(&state).sessions;
        assert_eq!((listed.len(), listed[0].compactions), (1, 1));
        assert_eq!(listed[0].title.as_deref(), Some("The catching, on doors"));
        let keys: Vec<&str> = listed[0].destinations.iter().map(|d| d.key.as_str()).collect();
        assert_eq!(keys, ["workspace", thread_in(&w1).as_str(), "baker"], "where it worked, most used first");
        let moved = import(&state, &to(&original, &thread_in(&w1))).unwrap();
        assert_eq!((moved.session_id.as_str(), moved.compactions), (sid, 1));
        let ctx = crate::state::context_for(&state, &moved.stream_id).unwrap();
        assert_eq!(std::fs::canonicalize(&ctx.worktree_path).unwrap(), w1, "the thread adopted the worktree");
        let config = crate::claude_native::seat_config_dir(&state, &moved.stream_id, "baker");
        let into = config.join("projects").join(claude_project(&w1));
        let copy = into.join(format!("{sid}.jsonl"));
        assert_eq!(std::fs::read(&copy).unwrap(), before, "where its runtime reads sessions");
        assert!(into.join(sid).join("subagents/agent-a1.jsonl").exists(), "its folder too");
        let agent_id = crate::node::agent_id(&moved.stream_id, Seat::Baker);
        assert_eq!(crate::resume::last_session(&state, &moved.stream_id, &agent_id, crate::claude_native::ACCOUNT_ID).as_deref(), Some(sid));
        assert!(crate::resume::kept(&state, &moved.stream_id, &agent_id, sid), "resumed, though no turn ran here yet");
        assert_eq!(std::fs::read(&original).unwrap(), before, "the original as it was");

        let facts = || {
            let heart = state.heart.lock().unwrap();
            [crate::resume::SESSION_OPENED, crate::node::SEAT_ASSIGNED_FAMILY, IMPORTED]
                .map(|family| heart.events_of_family(&moved.stream_id, family).unwrap().len())
        };
        let before_again = facts();
        let again = import(&state, &to(&original, &moved.stream_id)).unwrap();
        assert_eq!(again.stream_id, moved.stream_id, "the same thread");
        assert_eq!(facts(), before_again, "again, nothing changes: no seat assigned, nothing noted");
        assert_eq!(before_again[0], 1, "noted once");

        let can_bring_in = || sessions(&state).sessions[0].can_bring_in;
        assert!(!can_bring_in(), "nothing new yet");
        grow(&original, &said(&w1, "one more thing"));
        assert!(can_bring_in(), "the original went on: what is new can come in");
        import(&state, &to(&original, &moved.stream_id)).unwrap();
        assert!(!can_bring_in(), "brought in");
        assert_eq!(std::fs::read(&copy).unwrap(), std::fs::read(&original).unwrap(), "what is new came along");
        assert_eq!(brought(&state, &moved.stream_id, sid), Some(std::fs::metadata(&original).unwrap().len()));

        grow(&copy, &said(&w1, "carried on in the Bakery"));
        let carried = std::fs::read(&copy).unwrap();
        import(&state, &to(&original, &moved.stream_id)).unwrap();
        assert_eq!(std::fs::read(&copy).unwrap(), carried, "nothing new: the thread's own copy stays");
        grow(&original, &said(&w1, "and outside it too"));
        assert!(!can_bring_in(), "both went on: nothing to bring in without losing some");
        let both = import(&state, &to(&original, &moved.stream_id)).unwrap_err();
        assert_eq!(both.0, StatusCode::CONFLICT);
        assert!(both.1.contains("Both went on"), "{}", both.1);
        assert_eq!(std::fs::read(&copy).unwrap(), carried, "nothing was overwritten");

        let outside = ImportReq { transcript: repo.join("VISION.md").display().to_string(), ..to(&original, &moved.stream_id) };
        assert!(import(&state, &outside).unwrap_err().1.contains("not one of the operator's"));
        let inside = import(&state, &to(&original, &thread_in(&w1.join("crates/node")))).unwrap_err().1;
        assert!(inside.contains("is a folder inside the worktree w1/baker-hands"), "{inside}");
    }

    /// The Baker's own session moves into the permanent Baker's
    /// conversation, on the main checkout, never while it is mid-turn.
    /// Another one there is refused, and so is one over a session the Baker
    /// began itself, which no longer leaves the first reading as moved in;
    /// one it began and never asked anything in stands in no one's way.
    #[test]
    fn the_bakers_session_moves_into_the_bakers_own_conversation_and_never_over_another() {
        let dir = tempfile::tempdir().unwrap();
        let (state, _, home, main) = bakery(dir.path());
        let first = transcript(&state, &main, "11111111-2222-3333-4444-555555555555", &[said(&main, "plan")]);
        let second = transcript(&state, &main, "66666666-7777-8888-9999-000000000000", &[said(&main, "plan")]);
        state.lifecycle.lock().unwrap().running_turns.insert((home.clone(), Seat::Baker), 1);
        let busy = import(&state, &to(&first, "baker")).unwrap_err();
        assert!(busy.1.contains("The Baker's own conversation is in the middle of a turn"), "{}", busy.1);
        state.lifecycle.lock().unwrap().running_turns.clear();
        assert_eq!(import(&state, &to(&first, "baker")).unwrap().stream_id, home);
        let refused = import(&state, &to(&second, "baker")).unwrap_err();
        assert_eq!(refused.0, StatusCode::CONFLICT);
        assert!(refused.1.contains("The Baker's own conversation already carries on “plan”"), "{}", refused.1);

        let offered = |sid: &str| {
            let listed = sessions(&state).sessions;
            let session = listed.into_iter().find(|s| s.session_id == sid).unwrap();
            let baker = session.destinations.into_iter().find(|d| d.key == "baker").unwrap();
            (session.suggested_destination, baker.taken_by)
        };
        assert_eq!(offered("11111111-2222-3333-4444-555555555555"), (Some("baker".into()), None), "where it is");
        assert_eq!(offered("66666666-7777-8888-9999-000000000000"), (Some("workspace".into()), Some("plan".into())), "never a place that would refuse it");

        // A session the Baker opened and was never asked anything in holds
        // nothing: another moves in over it.
        let agent_id = crate::node::agent_id(&home, Seat::Baker);
        crate::resume::note_session(&state, &home, &agent_id, crate::claude_native::ACCOUNT_ID, "never-asked", false);
        assert_eq!(import(&state, &to(&second, "baker")).unwrap().stream_id, home);
        // One a turn ran in is the Baker's own conversation.
        let turn = |family: &str| {
            let p = serde_json::json!({"agent_id": agent_id, "turn_id": "t1"});
            crate::state::append(&state, &home, "node", family, p.to_string(), vec![]).unwrap();
        };
        turn("agent.turn.started");
        crate::resume::note_session(&state, &home, &agent_id, crate::claude_native::ACCOUNT_ID, "its-own", false);
        turn("agent.turn.done");
        let listed = sessions(&state).sessions;
        assert!(listed.iter().all(|s| s.moved_in.is_none()), "moved in only while resumed");
        let taken = |s: &ClaudeSession| s.destinations.iter().find(|d| d.key == "baker").and_then(|d| d.taken_by.clone());
        assert!(listed.iter().all(|s| taken(s).as_deref() == Some("Baker")), "the Baker carries on its own session");
        assert!(import(&state, &to(&second, "baker")).unwrap_err().1.contains("a session of its own"));
    }

    /// Sessions shaped like the operator's own: each began in the main
    /// checkout, then worked mostly in a worktree, deep in its folders, with
    /// compactions and subagents. Only the Bakery's own are listed, newest
    /// first. Each is suggested where it worked most
    /// and goes by its newest custom title. A conversation carries on one
    /// session: a second one there is refused with the reason, and only the
    /// one carried on reads as moved in. A session moves in once.
    #[test]
    fn each_session_is_placed_where_it_worked_most_and_one_conversation_carries_one() {
        let dir = tempfile::tempdir().unwrap();
        let (state, repo, _, main) = bakery(dir.path());
        // Inside the main checkout, as the operator's are.
        let worktree = |name: &str, branch: &str| {
            let at = main.join(".claude/worktrees").join(name);
            run_git(&repo, &["worktree", "add", "-q", "-b", branch, at.to_str().unwrap()]);
            std::fs::create_dir_all(at.join("crates/node/src")).unwrap();
            std::fs::canonicalize(at).unwrap()
        };
        let (w1, w2) = (worktree("w1", "w1/baker-hands"), worktree("w2", "w2/containers"));
        let deep = |w: &Path| w.join("crates/node/src");
        let (one, two) = ("aaaaaaaa-0000-0000-0000-000000000001", "bbbbbbbb-0000-0000-0000-000000000002");
        let elsewhere = main.with_file_name("thebakery-old");
        transcript(&state, &dir.path().join("other"), "cccccccc-0000-0000-0000-000000000003", &[said(&w1, "another project's")]);
        transcript(&state, &elsewhere, "dddddddd-0000-0000-0000-000000000004", &[said(&elsewhere, "only a like name")]);
        let second = transcript(&state, &main, two, &[said(&main, "start"), said(&deep(&w1), "in w1"), said(&deep(&w1), "still"), said(&w2, "a look at w2")]);
        let worker = transcript(
            &state,
            &main,
            one,
            &[
                r#"{"type":"queue-operation"}"#.to_string(),
                said(&main, "Continue from where you left off."),
                r#"{"type":"ai-title","aiTitle":"Continue the work"}"#.to_string(),
                said(&w1, "build"),
                r#"{"type":"system","subtype":"compact_boundary"}"#.to_string(),
                said(&deep(&w1), "test"),
                said(&deep(&w1), "commit"),
                r#"{"type":"custom-title","customTitle":"worker one"}"#.to_string(),
                r#"{"type":"custom-title","customTitle":"bakery-worker-1"}"#.to_string(),
            ],
        );
        let newest = std::time::SystemTime::now() + std::time::Duration::from_secs(60);
        std::fs::File::options().write(true).open(&worker).unwrap().set_modified(newest).unwrap();

        let listed = sessions(&state).sessions;
        let ids: Vec<&str> = listed.iter().map(|s| s.session_id.as_str()).collect();
        assert_eq!(ids, [one, two], "the Bakery's own, newest first; subagents are not sessions");
        let first = listed[0].clone();
        assert_eq!((first.title.as_deref(), first.custom_title.as_deref()), (Some("bakery-worker-1"), Some("bakery-worker-1")));
        assert_eq!(first.compactions, 1);
        let keys: Vec<(&str, &str)> = first.destinations.iter().map(|d| (d.key.as_str(), d.label.as_str())).collect();
        let (w1_thread, w2_thread) = (thread_in(&w1), thread_in(&w2));
        let expected = [("workspace", "A new workspace"), (w1_thread.as_str(), "A Baker thread in w1/baker-hands"), ("baker", "The Baker's own conversation")];
        assert_eq!(keys, expected, "where it worked most first, not where it began");
        assert_eq!(first.suggested_destination, None, "no workspace yet to work under");
        assert_eq!(listed[1].destinations.len(), 4, "the new workspace, and the three folders it worked in");

        let moved = import(&state, &to(&worker, &w1_thread)).unwrap();
        let refused = import(&state, &to(&second, &moved.stream_id)).unwrap_err();
        assert_eq!(refused.0, StatusCode::CONFLICT);
        assert!(refused.1.contains("The thread in w1/baker-hands already carries on “bakery-worker-1”"), "{}", refused.1);
        let listed = sessions(&state).sessions;
        assert_eq!(listed[0].moved_in, Some(moved.stream_id.clone()));
        assert_eq!(listed[0].suggested_destination.as_deref(), Some(moved.stream_id.as_str()), "its own place");
        assert_eq!(listed[1].moved_in, None, "refused, it did not move in");
        assert_eq!(listed[1].suggested_destination, None, "where it worked most is taken, and nowhere else fits");
        let taken: Vec<Option<&str>> = listed[1].destinations.iter().filter(|d| d.stream_id.is_some()).map(|d| d.taken_by.as_deref()).collect();
        assert_eq!(taken, [Some("bakery-worker-1"), None], "the picker shows who is there: w1's thread, and the Baker's own");

        let twice = import(&state, &to(&worker, &w2_thread)).unwrap_err();
        assert_eq!(twice.0, StatusCode::CONFLICT);
        assert!(twice.1.contains("a session moves in once"), "{}", twice.1);
        assert_ne!(import(&state, &to(&second, &w2_thread)).unwrap().stream_id, moved.stream_id, "its own thread");
    }

    /// The operator's own, as they move in: the Baker's session, which
    /// worked mostly in the main checkout, goes on as a new workspace's
    /// orc; a worker's goes on under that orc, in the worktree it has; a
    /// moved-in session's place is where it is, and a worktree already
    /// worked in offers its conversation, never a second one.
    #[test]
    fn the_bakers_session_opens_a_workspace_and_the_workers_go_under_its_orc() {
        let dir = tempfile::tempdir().unwrap();
        let (state, repo, _, main) = bakery(dir.path());
        let w1 = main.join(".claude/worktrees/w1");
        run_git(&repo, &["worktree", "add", "-q", "-b", "w1/baker-hands", w1.to_str().unwrap()]);
        let w1 = std::fs::canonicalize(w1).unwrap();
        let baker = transcript(&state, &main, "aaaaaaaa-1111-0000-0000-000000000001", &[said(&main, "plan"), said(&main, "merge"), said(&w1, "a look")]);
        let worker = transcript(&state, &main, "bbbbbbbb-2222-0000-0000-000000000002", &[said(&main, "start"), said(&w1, "build"), said(&w1, "test")]);
        let other = transcript(&state, &main, "cccccccc-3333-0000-0000-000000000003", &[said(&w1, "also w1")]);
        let find = |sid: &str| sessions(&state).sessions.into_iter().find(|s| s.session_id == sid).unwrap();
        let keys = |s: &ClaudeSession| s.destinations.iter().map(|d| d.key.clone()).collect::<Vec<_>>();
        let w1_key = |kind: &str| format!("{kind}:{}", w1.display());

        let first = find("aaaaaaaa-1111-0000-0000-000000000001");
        assert_eq!(first.suggested_destination.as_deref(), Some("workspace"), "never the Baker's own conversation");
        assert_eq!(keys(&first), ["workspace".to_string(), "baker".to_string(), w1_key("thread")], "no worker without a workspace");
        let orc = import(&state, &ImportReq { title: Some("Bakery".into()), ..to(&baker, "workspace") }).unwrap().stream_id;
        assert!(crate::workspaces::is_workspace(&state, &orc));
        let ctx = crate::state::context_for(&state, &orc).unwrap();
        let orc_dir = std::fs::canonicalize(&ctx.worktree_path).unwrap();
        let copy = crate::claude_native::seat_config_dir(&state, &orc, "orchestrator").join("projects").join(claude_project(&orc_dir));
        assert!(copy.join("aaaaaaaa-1111-0000-0000-000000000001.jsonl").exists(), "where the orc's runtime reads sessions");
        let agent = crate::node::agent_id(&orc, Seat::Orchestrator);
        let resumes = crate::resume::last_session(&state, &orc, &agent, crate::claude_native::ACCOUNT_ID);
        assert_eq!(resumes.as_deref(), Some("aaaaaaaa-1111-0000-0000-000000000001"));
        let first = find("aaaaaaaa-1111-0000-0000-000000000001");
        assert_eq!((first.moved_in.as_deref(), first.suggested_destination.as_deref()), (Some(orc.as_str()), Some(orc.as_str())));

        let second = find("bbbbbbbb-2222-0000-0000-000000000002");
        assert_eq!(second.suggested_destination, Some(w1_key("worker")), "under the one workspace");
        let offered = second.destinations.iter().find(|d| d.key == w1_key("worker")).unwrap();
        assert_eq!((offered.kind, offered.orcs.len(), offered.label.as_str()), (Kind::Worker, 1, "A worker in w1/baker-hands"));
        assert_eq!(second.destinations.iter().find(|d| d.key == orc).unwrap().taken_by.as_deref(), Some("plan"));
        let peer = import(&state, &to(&worker, &w1_key("worker"))).unwrap().stream_id;
        assert_eq!(crate::baker_threads::opened_by(&state, &peer).as_deref(), Some(orc.as_str()), "under its orc");
        let theirs = crate::state::context_for(&state, &peer).unwrap();
        assert_eq!((std::fs::canonicalize(&theirs.worktree_path).unwrap(), theirs.branch.as_str()), (w1.clone(), "w1/baker-hands"));
        let agent = crate::node::agent_id(&peer, Seat::Orchestrator);
        let resumes = crate::resume::last_session(&state, &peer, &agent, crate::claude_native::ACCOUNT_ID);
        assert_eq!(resumes.as_deref(), Some("bbbbbbbb-2222-0000-0000-000000000002"));

        assert_eq!(find("bbbbbbbb-2222-0000-0000-000000000002").suggested_destination.as_deref(), Some(peer.as_str()), "where it is");
        state.lifecycle.lock().unwrap().running_turns.insert((peer.clone(), Seat::Orchestrator), 1);
        assert_eq!(find("bbbbbbbb-2222-0000-0000-000000000002").suggested_destination, None, "never a conversation mid-turn");
        state.lifecycle.lock().unwrap().running_turns.clear();
        let third = find("cccccccc-3333-0000-0000-000000000003");
        assert_eq!(third.suggested_destination, None, "w1's worker carries another session");
        assert!(!keys(&third).contains(&w1_key("worker")), "never a second conversation in w1");
        let twice = import(&state, &to(&other, &w1_key("worker"))).unwrap_err();
        assert!(twice.1.contains("The worker in w1/baker-hands already works there"), "{}", twice.1);
        let taken = import(&state, &to(&other, &peer)).unwrap_err();
        assert!(taken.1.contains("The worker in w1/baker-hands already carries on"), "{}", taken.1);
    }
}
