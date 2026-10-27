//! The thread board: where every thread a conversation looks after stands,
//! in one call. For each, its state and the last line of its last result,
//! its branch, and where its work is, read from git and never from the merge
//! records: against main, the commits main lacks, whether main has its work
//! (merged) or it made none of its own (nothing to merge), and whether the
//! rest merges cleanly. A workspace's worker is read against the workspace's
//! branch too, where its work goes first.
//!
//! Whose threads: an orc's board is itself and its workers; any other
//! agent's, itself and the peers it opened; the Baker's, everything: its
//! conversations, then every other conversation in use (workspaces, their
//! workers, project threads). Every field of a row is bounded.
//! An agent reads its own board only (`agents: own`), so it sees no wider
//! than it may read already. Its git runs outside every lock, a few calls a
//! thread.

use crate::agents::ConversationStatus;
use crate::node::Seat;
use crate::state::{context_for, AppState};
use axum::{
    extract::{Query, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// The longest last line, title, branch or file name, in characters.
const LINE_CHARS: usize = 200;
/// The longest note (a state's, git's), in characters.
const NOTE_CHARS: usize = 400;
/// How many files that would conflict a standing names.
const CONFLICTS_SHOWN: usize = 20;

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct BoardQuery {
    /// The conversation whose threads it shows.
    pub(crate) stream_id: String,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct Board {
    /// The conversation itself first, then the threads it opened, most
    /// recently active first; for the Baker, its main conversation, then its
    /// threads, then every other conversation in use.
    pub(crate) threads: Vec<BoardThread>,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct BoardThread {
    pub(crate) stream_id: String,
    pub(crate) title: String,
    pub(crate) state: ConversationStatus,
    /// What it waits on, when it needs you or waits out a failure; why it
    /// is idle (stopped, interrupted).
    pub(crate) state_note: Option<String>,
    /// The last line of its last finished turn: its result's, or why it
    /// failed or was stopped.
    pub(crate) last_line: Option<String>,
    /// Its own branch.
    pub(crate) branch: String,
    /// Against main: the branch its repository's main checkout is on.
    pub(crate) main: Option<Standing>,
    /// A workspace's worker, against the workspace's branch, where its work
    /// goes first.
    pub(crate) workspace: Option<Standing>,
    /// Why a standing is missing: it works in place, its branch is gone, or
    /// git could not read it.
    pub(crate) git_note: Option<String>,
}

/// Where a thread's branch stands against a branch its work goes into.
#[derive(schemars::JsonSchema, Debug, Clone, PartialEq, Serialize)]
pub(crate) struct Standing {
    /// The branch its work goes into.
    pub(crate) into: String,
    /// Commits of its branch that `into` lacks.
    pub(crate) ahead: usize,
    pub(crate) merged: MergedState,
    /// With commits to merge: they merge into `into` without a conflict.
    pub(crate) mergeable: Option<bool>,
    /// The files that would conflict: the first few (`git_merge_preview`
    /// names them all).
    pub(crate) conflicts: Vec<String>,
    /// What merging them would change on `into`: its files, and lines added
    /// and removed.
    pub(crate) files: usize,
    pub(crate) added: usize,
    pub(crate) removed: usize,
}

/// Whether a branch has a thread's work, as git has it.
#[derive(schemars::JsonSchema, Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MergedState {
    /// It lacks commits of the thread's branch.
    No,
    /// It has every commit of the thread's branch, its own among them.
    Yes,
    /// It has the thread's branch, which has no commits of its own.
    NothingToMerge,
}

/// A conversation's board.
pub(crate) fn board(state: &AppState, viewer: &str) -> Result<Board, String> {
    let members = members(state, viewer)?;
    // Read before the Heart is locked, never under it.
    let running: HashSet<String> = {
        let lifecycle = state.lifecycle.lock().unwrap();
        lifecycle.running_turns.iter().filter(|(_, &n)| n > 0).map(|((stream_id, _), _)| stream_id.clone()).collect()
    };
    let mut gates: HashMap<String, usize> = HashMap::new();
    for gate in crate::routes::operator::gates_inner(state).unwrap_or_default() {
        *gates.entry(gate.stream_id).or_insert(0) += 1;
    }
    let awaiting = crate::agents::awaiting_by_agent(state);
    let mut threads: Vec<BoardThread> = {
        let heart = state.heart.lock().unwrap();
        members
            .into_iter()
            .map(|(stream_id, title)| {
                let waits = [Seat::Orchestrator, Seat::Worker, Seat::Baker]
                    .into_iter()
                    .find_map(|seat| awaiting.get(&crate::node::agent_id(&stream_id, seat)).copied());
                let pending = gates.get(&stream_id).copied().unwrap_or(0);
                let (status, note) =
                    crate::agents::conversation_status_locked(&heart, &stream_id, running.contains(&stream_id), pending, waits);
                // Its state says it exists and whether it works; the rest,
                // here and in `place`, what it is doing.
                BoardThread {
                    // A finished or failed turn's note is a line of its result.
                    state_note: note
                        .filter(|_| !matches!(status, ConversationStatus::Done | ConversationStatus::Failed))
                        .map(|n| clip(&n, NOTE_CHARS)),
                    last_line: last_line_locked(&heart, &stream_id),
                    state: status,
                    title: clip(&title, LINE_CHARS),
                    branch: String::new(),
                    main: None,
                    workspace: None,
                    git_note: None,
                    stream_id,
                }
            })
            .collect()
    };
    let mut mains = HashMap::new();
    for thread in &mut threads {
        place(state, thread, &mut mains);
    }
    Ok(Board { threads })
}

/// The threads on a conversation's board, with their titles. For any of
/// the Baker's conversations, everything: the Baker's own (its main one
/// first), then every other conversation in use, most recently active
/// first. Else the conversation itself and the peers it opened, an orc's
/// being its workers.
fn members(state: &AppState, viewer: &str) -> Result<Vec<(String, String)>, String> {
    if crate::baker_ops::is_baker_scope_stream(state, viewer) {
        let own = crate::baker_threads::list_threads(state, false)?;
        let mine: HashSet<String> = own.iter().map(|t| t.stream_id.clone()).collect();
        let mut rest: Vec<_> = crate::state::known_streams(state)?.into_iter().filter(|s| !s.discarded && !mine.contains(&s.stream_id)).collect();
        {
            let heart = state.heart.lock().unwrap();
            rest.retain(|s| !crate::baker_threads::archived_locked(&heart, &s.stream_id) && !crate::agents::socket_archived_locked(&heart, &s.stream_id));
        }
        rest.sort_by(|a, b| b.last_ts.cmp(&a.last_ts));
        let own = own.into_iter().map(|t| (t.stream_id, t.title));
        return Ok(own.chain(rest.into_iter().map(|s| (s.stream_id, s.title.unwrap_or(s.name)))).collect());
    }
    let own = context_for(state, viewer)?;
    let renamed = {
        let heart = state.heart.lock().unwrap();
        let fact = heart.last_of_family(viewer, crate::state::CONVERSATION_RENAMED_FAMILY).ok().flatten();
        fact.and_then(|e| serde_json::from_str::<serde_json::Value>(&e.payload).ok()?["title"].as_str().map(str::to_string))
    };
    let peers = crate::baker_threads::peers_of(state, viewer)?;
    let peers = peers.into_iter().map(|p| (p.stream_id, p.title.unwrap_or(p.name)));
    Ok(std::iter::once((viewer.to_string(), renamed.unwrap_or(own.name))).chain(peers).collect())
}

/// The last line of a conversation's last finished turn: its result's, or
/// why it failed or was stopped.
fn last_line_locked(heart: &bakery_heart::Heart, stream_id: &str) -> Option<String> {
    let end = heart.last_of_families(stream_id, &crate::baker_threads::TURN_ENDS).ok().flatten()?;
    let payload: serde_json::Value = serde_json::from_str(&end.payload).ok()?;
    let text = payload[if end.family == "agent.turn.done" { "summary" } else { "reason" }].as_str()?;
    let line = text.lines().map(str::trim).rfind(|line| !line.is_empty())?;
    Some(clip(line, LINE_CHARS))
}

/// `text` within `max` characters, an ellipsis marking the cut.
fn clip(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((cut, _)) => format!("{}…", &text[..cut]),
        None => text.to_string(),
    }
}

/// A thread's branch and standings, from git, outside every lock. `mains`
/// keeps each repository's main for the board's other threads.
fn place(state: &AppState, thread: &mut BoardThread, mains: &mut HashMap<String, Result<String, String>>) {
    let mut notes = Vec::new();
    if let Err(why) = standings(state, thread, mains, &mut notes) {
        notes.insert(0, why);
    }
    thread.git_note = (!notes.is_empty()).then(|| clip(&notes.join("; "), NOTE_CHARS));
    thread.branch = clip(&thread.branch, LINE_CHARS);
}

fn standings(
    state: &AppState,
    thread: &mut BoardThread,
    mains: &mut HashMap<String, Result<String, String>>,
    notes: &mut Vec<String>,
) -> Result<(), String> {
    let ctx = context_for(state, &thread.stream_id)?;
    thread.branch = ctx.branch.clone();
    // Main unread still leaves a worker's standing in its workspace.
    let main = mains.entry(ctx.repo_root.clone()).or_insert_with(|| main_branch(&ctx.repo_root)).clone();
    if let Err(why) = &main {
        notes.push(why.clone());
    }
    let main = main.ok();
    if ctx.in_place || ctx.branch.is_empty() || main.as_ref() == Some(&ctx.branch) || !crate::tools::own_checkout(state, &ctx) {
        return Err(format!("it works in place on {}: no branch of its own", ctx.branch));
    }
    let dir = [&ctx.worktree_path, &ctx.repo_root]
        .into_iter()
        .map(PathBuf::from)
        .find(|dir| dir.is_dir())
        .ok_or("its checkout is gone")?;
    let tip = crate::git::run(&dir, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{}^{{commit}}", ctx.branch)])
        .map(|sha| sha.trim().to_string())
        .map_err(|_| format!("its branch {} is gone", ctx.branch))?;
    // A workspace's worker merges into the workspace's branch first.
    let workspace = crate::workspaces::workspace_of(state, &thread.stream_id)
        .filter(|orc| *orc != thread.stream_id)
        .and_then(|orc| context_for(state, &orc).map_err(|why| notes.push(format!("its workspace: {why}"))).ok())
        .map(|orc| orc.branch);
    // The branch it left from: its opener's, else main.
    let left = crate::baker_threads::opened_by(state, &thread.stream_id)
        .and_then(|opener| context_for(state, &opener).ok())
        .map(|opener| opener.branch)
        .or_else(|| main.clone());
    let mut own = None;
    let mut own_commits = || -> Result<bool, String> {
        if let Some(own) = own {
            return Ok(own);
        }
        let left = left.as_deref().ok_or("no branch it left from to read against")?;
        let made = ahead(&dir, &tip, left)? > 0 || !on_its_line(&dir, &tip, left)?;
        own = Some(made);
        Ok(made)
    };
    let mut read = |into: &str| {
        standing(&dir, &tip, into, &mut own_commits).map_err(|why| notes.push(format!("against {into}: {why}"))).ok()
    };
    thread.workspace = workspace.and_then(|into| read(&into));
    thread.main = main.and_then(|main| read(&main));
    Ok(())
}

/// Main, for a repository: the branch its main checkout is on now.
fn main_branch(repo_root: &str) -> Result<String, String> {
    if repo_root.trim().is_empty() {
        return Err("it has no repository".to_string());
    }
    let branch = crate::git::run(Path::new(repo_root), &["rev-parse", "--abbrev-ref", "HEAD"])?;
    match branch.trim() {
        "HEAD" => Err(format!("{repo_root} is on no branch, so there is no main to read against")),
        branch => Ok(branch.to_string()),
    }
}

/// `tip` against branch `into`. `own_commits` says whether the thread made
/// commits of its own: asked only when `into` has all of `tip`.
fn standing(dir: &Path, tip: &str, into: &str, own_commits: &mut impl FnMut() -> Result<bool, String>) -> Result<Standing, String> {
    let ahead = ahead(dir, tip, into)?;
    let mut standing = Standing {
        into: clip(into, LINE_CHARS),
        ahead,
        merged: MergedState::No,
        mergeable: None,
        conflicts: Vec::new(),
        files: 0,
        added: 0,
        removed: 0,
    };
    if ahead == 0 {
        standing.merged = if own_commits()? { MergedState::Yes } else { MergedState::NothingToMerge };
        return Ok(standing);
    }
    let merge = crate::routes::git::merge_summary(dir, &format!("refs/heads/{into}"), tip)?;
    standing.mergeable = Some(merge.clean);
    standing.conflicts = merge.conflicts.iter().take(CONFLICTS_SHOWN).map(|f| clip(f, LINE_CHARS)).collect();
    standing.files = merge.files.len();
    (standing.added, standing.removed) = (merge.added, merge.removed);
    Ok(standing)
}

/// Commits of `tip` that branch `into` lacks.
fn ahead(dir: &Path, tip: &str, into: &str) -> Result<usize, String> {
    let count = crate::git::run(dir, &["rev-list", "--count", &format!("refs/heads/{into}..{tip}")])?;
    count.trim().parse().map_err(|e| format!("git rev-list --count: {e}"))
}

/// Whether `tip`, which branch `into` has, is a commit of `into`'s own line
/// (its first parents) rather than one a merge brought in: walking that line
/// back to what `tip` reaches, the last commit's parent is `tip` itself. A
/// fast-forward leaves no trace, so its commits read as `into`'s own; the
/// Bakery's merges never fast-forward.
fn on_its_line(dir: &Path, tip: &str, into: &str) -> Result<bool, String> {
    let walk = crate::git::run(dir, &["log", "--first-parent", "--format=%P", &format!("{tip}..refs/heads/{into}")])?;
    Ok(walk.lines().last().is_none_or(|parents| parents.split_whitespace().next() == Some(tip)))
}

/// `GET /baker/threads/board`
pub(crate) async fn board_route(
    State(st): State<AppState>,
    Query(q): Query<BoardQuery>,
) -> Result<Json<Board>, (StatusCode, String)> {
    crate::actor::spawn_blocking(move || board(&st, &q.stream_id))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_control::OperatingMode;
    use crate::testutil::*;
    use serde_json::json;
    use std::sync::Arc;

    fn commit(work: &Path, file: &str, text: &str) {
        std::fs::write(work.join(file), text).unwrap();
        run_git(work, &["add", "."]);
        run_git(work, &["commit", "-qm", file]);
    }

    fn row<'a>(board: &'a Board, stream_id: &str) -> &'a BoardThread {
        board.threads.iter().find(|t| t.stream_id == stream_id).unwrap_or_else(|| panic!("{stream_id} on {board:?}"))
    }

    /// (ahead, merged, mergeable) of a standing.
    fn read(standing: &Option<Standing>) -> (usize, MergedState, Option<bool>) {
        let s = standing.as_ref().expect("a standing");
        (s.ahead, s.merged, s.mergeable)
    }

    fn baker_home(state: &AppState) -> String {
        let home = crate::routes::baker::ensure_permanent_baker(state, None, None).unwrap().stream_id;
        crate::routes::seats::seat_assign_checked(state, &home, Seat::Baker, "stub").unwrap();
        home
    }

    /// An orc's board is itself and its workers, each worker read from git
    /// against the workspace's branch and against main: merged, nothing to
    /// merge (no commits of its own, wherever its tip went), or commits to
    /// merge and whether they merge cleanly; with its state and last line.
    #[test]
    fn an_orcs_board_reads_each_workers_work_from_git() {
        let dir = tempfile::tempdir().unwrap();
        let repo = bakery_repo_for_test(dir.path());
        run_git(&repo, &["add", "."]);
        run_git(&repo, &["commit", "-qm", "the bakery"]);
        let state = state_with_bakery_root(dir.path(), &repo);
        baker_home(&state);
        let orc = crate::workspaces::create_workspace(&state, "Board", None, "human").unwrap();
        let ws = PathBuf::from(&orc.worktree_path);
        let worker = |title: &str| {
            let req = crate::baker_threads::CreateThreadReq { title: Some(title.into()), ..Default::default() };
            let id = crate::baker_threads::create_peer(&state, &req, &orc.stream_id, "human").unwrap().thread.stream_id;
            let ctx = context_for(&state, &id).unwrap();
            (id, PathBuf::from(ctx.worktree_path), ctx.branch)
        };
        let (merged, merged_work, merged_branch) = worker("merged");
        let (idle, ..) = worker("idle");
        let (pending, pending_work, _) = worker("pending");
        let (clash, clash_work, _) = worker("clash");
        commit(&merged_work, "a.md", "a\n");
        // Taken in as the merge review takes it: never a fast-forward.
        run_git(&ws, &["merge", "-q", "--no-ff", "--no-edit", &merged_branch]);
        commit(&pending_work, "b.md", "b\n");
        commit(&clash_work, "a.md", "not a\n");
        let turn = |stream: &str, family: &str, payload: serde_json::Value| {
            crate::state::append(&state, stream, "node", family, payload.to_string(), vec![]).unwrap();
        };
        turn(&pending, "agent.turn.done", json!({"summary": "Added b.\n\nReady to merge: b.md is in.\n"}));
        turn(&merged, "agent.turn.failed", json!({"reason": "the build broke\nerror: E0308"}));
        state.lifecycle.lock().unwrap().running_turns.insert((clash.clone(), Seat::Orchestrator), 1);

        let board = board(&state, &orc.stream_id).unwrap();
        assert_eq!(board.threads[0].stream_id, orc.stream_id, "itself first");
        assert_eq!(board.threads.len(), 5, "itself and its four workers: {board:?}");
        let (ws_branch, main) = (orc.branch.as_str(), "main");

        let own = row(&board, &orc.stream_id);
        assert!(own.workspace.is_none(), "an orc's work goes to main: {own:?}");
        assert_eq!(read(&own.main), (2, MergedState::No, Some(true)), "a worker's commit and its merge");

        let m = row(&board, &merged);
        assert_eq!((m.title.as_str(), m.branch.as_str()), ("merged", merged_branch.as_str()));
        assert_eq!(m.workspace.as_ref().unwrap().into, ws_branch);
        assert_eq!(read(&m.workspace), (0, MergedState::Yes, None), "in the workspace, by git alone");
        assert_eq!(read(&m.main), (1, MergedState::No, Some(true)));
        assert_eq!(m.main.as_ref().unwrap().into, main);
        assert_eq!((m.state, m.last_line.as_deref()), (ConversationStatus::Failed, Some("error: E0308")));

        let i = row(&board, &idle);
        assert_eq!(read(&i.workspace), (0, MergedState::NothingToMerge, None), "no commits of its own");
        assert_eq!(read(&i.main), (0, MergedState::NothingToMerge, None));
        assert_eq!((i.state, i.last_line.as_deref(), i.git_note.as_deref()), (ConversationStatus::Idle, None, None));

        let p = row(&board, &pending);
        assert_eq!(read(&p.workspace), (1, MergedState::No, Some(true)));
        let s = p.workspace.as_ref().unwrap();
        assert_eq!((s.files, s.added, s.removed, s.conflicts.len()), (1, 1, 0, 0));
        assert_eq!((p.state, p.last_line.as_deref()), (ConversationStatus::Done, Some("Ready to merge: b.md is in.")));
        assert_eq!(p.state_note, None, "a finished turn's note is its result");

        let c = row(&board, &clash);
        assert_eq!(read(&c.workspace), (1, MergedState::No, Some(false)));
        assert_eq!(c.workspace.as_ref().unwrap().conflicts, vec!["a.md".to_string()]);
        assert_eq!(read(&c.main), (1, MergedState::No, Some(true)), "main has no a.md to clash with");
        assert_eq!(c.state, ConversationStatus::Working);
        state.lifecycle.lock().unwrap().running_turns.clear();

        // The workspace goes into main: its worker's work is merged there
        // too, and the idle worker, whose tip main now has through that
        // merge, still made nothing to merge.
        run_git(&repo, &["merge", "-q", "--no-ff", "--no-edit", ws_branch]);
        let board = super::board(&state, &orc.stream_id).unwrap();
        assert_eq!(read(&row(&board, &orc.stream_id).main), (0, MergedState::Yes, None));
        assert_eq!(read(&row(&board, &merged).main), (0, MergedState::Yes, None));
        assert_eq!(read(&row(&board, &idle).main), (0, MergedState::NothingToMerge, None));
        assert_eq!(read(&row(&board, &pending).main), (1, MergedState::No, Some(true)));

        // Main's checkout on no branch: no main to read against, said so,
        // and a worker still reads against its workspace.
        run_git(&repo, &["checkout", "-q", "--detach"]);
        let board = super::board(&state, &orc.stream_id).unwrap();
        let p = row(&board, &pending);
        assert!(p.main.is_none(), "{p:?}");
        assert_eq!(read(&p.workspace), (1, MergedState::No, Some(true)));
        assert!(p.git_note.as_deref().is_some_and(|n| n.contains("on no branch")), "{p:?}");
    }

    /// The Baker's board is everything: its conversations (the main one works
    /// in place, a chef on its own branch against main), then every other
    /// conversation in use, a workspace, its worker and a project's thread
    /// among them, and none put away. Any of its conversations sees the same.
    /// Every field is bounded. A chef whose branch is gone says so.
    #[test]
    fn the_bakers_board_is_everything() {
        let dir = tempfile::tempdir().unwrap();
        let repo = bakery_repo_for_test(dir.path());
        run_git(&repo, &["add", "."]);
        run_git(&repo, &["commit", "-qm", "the bakery"]);
        let state = state_with_bakery_root(dir.path(), &repo);
        let home = baker_home(&state);
        let req = crate::baker_threads::CreateThreadReq { title: Some("Fix the sidebar".into()), worktree: true, ..Default::default() };
        let chef = crate::baker_threads::create_thread(&state, &req, "human", None, None).unwrap().thread.stream_id;
        let ctx = context_for(&state, &chef).unwrap();
        commit(Path::new(&ctx.worktree_path), "sidebar.md", "fixed\n");
        let orc = crate::workspaces::create_workspace(&state, "Workspace", None, "human").unwrap().stream_id;
        let worker_req = crate::baker_threads::CreateThreadReq { title: Some("worker".into()), ..Default::default() };
        let worker = crate::baker_threads::create_peer(&state, &worker_req, &orc, "human").unwrap().thread.stream_id;
        let app = make_repo(&dir.path().join("app"));
        let root = crate::catalog::DirectoryRootSpec::Attach(&app.to_string_lossy());
        let (app_project, project) = crate::catalog::create_project(&state, "App", Some(root)).unwrap();
        let project = project.stream_id;
        let shelved = crate::catalog::DirectoryRootSpec::Attach(&app.to_string_lossy());
        let shelved = crate::catalog::attach_directory_root(&state, &app_project.project_id, "Shelved", shelved).unwrap();
        crate::agents::set_archived(&state, &shelved.stream_id, true, "human").unwrap();
        let renamed = serde_json::json!({ "title": "P".repeat(500) }).to_string();
        crate::state::append(&state, &project, "human", crate::state::CONVERSATION_RENAMED_FAMILY, renamed, vec![]).unwrap();

        let board = board(&state, &home).unwrap();
        let ids: Vec<&str> = board.threads.iter().map(|t| t.stream_id.as_str()).collect();
        assert_eq!(&ids[..2], [home.as_str(), chef.as_str()], "the Baker's own first");
        let mut rest = ids[2..].to_vec();
        rest.sort();
        let mut everything = vec![orc.as_str(), worker.as_str(), project.as_str()];
        everything.sort();
        assert_eq!(rest, everything, "then everything else in use, nothing put away");
        let main = row(&board, &home);
        assert!(main.main.is_none() && main.git_note.as_deref().unwrap().contains("in place"), "{main:?}");
        let c = row(&board, &chef);
        assert_eq!((c.title.as_str(), read(&c.main)), ("Fix the sidebar", (1, MergedState::No, Some(true))));
        assert!(c.workspace.is_none());
        assert_eq!(row(&board, &project).title.chars().count(), LINE_CHARS + 1, "a title is bounded");
        assert!(row(&board, &worker).workspace.is_some(), "a worker reads against its workspace");
        let from_the_chef = super::board(&state, &chef).unwrap();
        assert_eq!(from_the_chef.threads.len(), board.threads.len(), "one Baker, one board");

        crate::routes::project::remove_worktree(&state, &ctx.repo_root, Path::new(&ctx.worktree_path)).unwrap();
        run_git(&repo, &["branch", "-q", "-D", &ctx.branch]);
        let gone = super::board(&state, &home).unwrap();
        let note = row(&gone, &chef).git_note.clone().unwrap();
        assert!(note.contains("is gone"), "{note}");
    }

    #[test]
    fn clip_bounds_by_characters() {
        assert_eq!(clip("héllo", 3), "hél…");
        assert_eq!(clip("hé", 3), "hé");
    }

    /// An agent's board is itself and the peers it opened, read against
    /// main. It reads its own board only, its stream left out; another's is
    /// out of its reach.
    #[test]
    fn an_agent_reads_its_own_board_only() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(bakery_agent::StubModel));
        let repo = make_repo(&dir.path().join("repo"));
        let root = crate::catalog::DirectoryRootSpec::Attach(&repo.to_string_lossy());
        let (_, opener) = crate::catalog::create_project(&state, "P", Some(root)).unwrap();
        let opener = opener.stream_id;
        let req = crate::baker_threads::CreateThreadReq { title: Some("Helper".into()), ..Default::default() };
        let peer = crate::baker_threads::create_peer(&state, &req, &opener, "human").unwrap().thread.stream_id;
        commit(Path::new(&context_for(&state, &peer).unwrap().worktree_path), "g.txt", "two\n");

        let agent = |stream: &str| crate::api_tools::Caller {
            stream_id: stream.to_string(),
            seat: Seat::Orchestrator,
            turn_id: String::new(),
            mode: OperatingMode::Auto,
            wait: None,
        };
        let call = |caller: &str, args: serde_json::Value| crate::api_tools::call(&state, &agent(caller), "thread_board", &args);
        let mine: serde_json::Value = serde_json::from_str(&call(&opener, json!({})).unwrap()).unwrap();
        let ids: Vec<&str> = mine["threads"].as_array().unwrap().iter().map(|t| t["stream_id"].as_str().unwrap()).collect();
        assert_eq!(ids, vec![opener.as_str(), peer.as_str()], "itself, then the peer it opened");
        let helper = &mine["threads"][1];
        assert_eq!((helper["title"].as_str(), helper["main"]["ahead"].as_u64()), (Some("Helper"), Some(1)), "{mine}");
        assert_eq!((helper["main"]["merged"].as_str(), helper["main"]["mergeable"].as_bool()), (Some("no"), Some(true)));
        assert!(mine["threads"][0]["git_note"].as_str().unwrap().contains("in place"), "{mine}");

        let refused = call(&opener, json!({"stream_id": peer})).unwrap_err();
        assert!(refused.contains("your own conversation only"), "{refused}");
        let theirs: serde_json::Value = serde_json::from_str(&call(&peer, json!({})).unwrap()).unwrap();
        assert_eq!(theirs["threads"].as_array().unwrap().len(), 1, "the peer opened none: {theirs}");
    }
}
