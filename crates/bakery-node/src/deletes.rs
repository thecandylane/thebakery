//! Deleting, the operator's alone, once they have seen exactly what goes: a
//! project, one repository of a project, a workspace, or a thread or chat,
//! never the permanent Baker. `POST /delete/plan` lists what a delete would
//! remove and what stays; `POST /delete/confirm` carries that plan out, only
//! while a fresh plan still has the digest the operator saw, and records it
//! on the Heart (`delete.done`).
//!
//! Everything the deleted thing made goes with it: each thread is discarded
//! as `/stream/discard` discards one (its history stays in the Heart), with
//! its Bakery-made worktree, its branch and its room, the history of turns
//! Bakery kept of each folder that goes (`crate::snapshots`), and the
//! containers, volumes and networks it, or a deleted project, ran, an engine
//! that is off started for them and stopped again. Unsaved work (uncommitted
//! changes, commits in no main) is kept in git first (`crate::git_trash`),
//! and what can't be kept stays. A folder the Bakery made goes to the
//! Trash, never deleted outright; so does the operator's own project
//! folder, when they tick it. Only what something staying still uses stays,
//! so nothing that stays is broken: a folder or branch a staying thread
//! works in, a staying project's root, a staying worktree's repository, a
//! repository's main branch, and the Bakery's own data.

use crate::catalog::{ProjectRecord, RootRecord};
use crate::containers::{Backend, Kind, Thing};
use crate::git::StatusEntry;
use crate::git_trash;
use crate::node::Lifetime;
use crate::routes::lifecycle::{self, Fate};
use crate::state::{same_path, AppState, StreamRef};
use axum::{extract::State, http::StatusCode, Json};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::Duration;

type Fail = (StatusCode, String);

/// What a delete did, a [`DeleteDone`], on what it deleted: the project's
/// stream (a repo's: its project's), the workspace's orc, the thread.
pub(crate) const DELETE_DONE: &str = "delete.done";

/// How long each thread's discard waits for an attach already under way,
/// as `/stream/discard` does.
const DISCARD_WAIT: Duration = Duration::from_secs(35);

/// What to delete.
#[derive(schemars::JsonSchema, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum DeleteTarget {
    /// A project and everything the Bakery made for it.
    Project { project_id: String },
    /// One repository of a project, by its folder (as the project's rollup
    /// names it): its threads, their worktrees and its roots, a worktree the
    /// Bakery made of it that stayed when its thread went included.
    Repo { project_id: String, repo: String },
    /// A workspace: its orc (`stream_id`) and every worker under it.
    Workspace { stream_id: String },
    /// One thread or chat.
    Thread { stream_id: String },
}

#[derive(schemars::JsonSchema, Debug, Clone, Deserialize)]
pub(crate) struct DeletePlanReq {
    pub(crate) target: DeleteTarget,
    /// The operator's own project folders (the plan's `own`) go to the
    /// Trash too.
    #[serde(default)]
    pub(crate) trash_own: bool,
}

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct DeleteConfirmReq {
    pub(crate) target: DeleteTarget,
    /// As the plan was asked for.
    #[serde(default)]
    pub(crate) trash_own: bool,
    /// The digest of the plan the operator saw.
    pub(crate) digest: String,
}

/// A thread or chat that goes.
#[derive(schemars::JsonSchema, Debug, Clone, PartialEq, Serialize)]
pub(crate) struct DeleteThread {
    pub(crate) stream_id: String,
    pub(crate) title: String,
    /// Where it worked; what becomes of the folder is in the plan's lists.
    pub(crate) folder: String,
    /// A pinned (`living`) one goes too: confirming the plan is the
    /// confirmation its discard asks for. One pinned since is a new plan.
    pub(crate) lifetime: Lifetime,
}

/// A worktree the Bakery made, or a folder it made for a project.
#[derive(schemars::JsonSchema, Debug, Clone, PartialEq, Serialize)]
pub(crate) struct DeleteWorktree {
    /// The thread it was made for; none for a project's folder.
    pub(crate) stream_id: String,
    pub(crate) path: String,
    /// The branch checked out in it.
    pub(crate) branch: String,
    /// Its uncommitted changes, as `git status --porcelain` lists them.
    pub(crate) changes: Vec<StatusEntry>,
    /// Why its changes couldn't be read.
    pub(crate) error: Option<String>,
}

impl DeleteWorktree {
    fn dirty(&self) -> bool {
        !self.changes.is_empty() || self.error.is_some()
    }
}

/// A container, volume or network on one of this Mac's engines.
#[derive(schemars::JsonSchema, Debug, Clone, PartialEq, Serialize)]
pub(crate) struct DeleteEngineThing {
    pub(crate) engine: Backend,
    pub(crate) name: String,
    /// What ties it to what goes: its label, or its name.
    pub(crate) by: String,
}

/// A folder that stays, and why.
#[derive(schemars::JsonSchema, Debug, Clone, PartialEq, Serialize)]
pub(crate) struct DeleteKept {
    pub(crate) path: String,
    pub(crate) why: String,
}

/// A branch.
#[derive(schemars::JsonSchema, Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub(crate) struct DeleteBranch {
    pub(crate) name: String,
    /// The repository it is in.
    pub(crate) repo: String,
}

/// A branch the Bakery made that stays, and why.
#[derive(schemars::JsonSchema, Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub(crate) struct DeleteKeptBranch {
    pub(crate) name: String,
    pub(crate) repo: String,
    pub(crate) why: String,
}

/// Unsaved work a worktree or branch that goes holds, kept in git first: the
/// Bakery's Trash lists it until the operator empties it, and Put back
/// brings it back.
#[derive(schemars::JsonSchema, Debug, Clone, PartialEq, Serialize)]
pub(crate) struct DeleteSaved {
    pub(crate) repo: String,
    /// The branch, or the worktree's folder when it has none.
    pub(crate) name: String,
    /// Its commits that are in no main.
    pub(crate) commits: usize,
    /// Its uncommitted changes.
    pub(crate) changes: Vec<StatusEntry>,
}

/// What a delete would remove, and what stays.
#[derive(schemars::JsonSchema, Debug, Clone, PartialEq, Serialize)]
pub(crate) struct DeletePlan {
    pub(crate) target: DeleteTarget,
    /// What it is called in the lists.
    pub(crate) title: String,
    /// Why it can't go ahead now; nothing goes while it says so.
    pub(crate) refused: Option<String>,
    /// The threads and chats that go, each discarded; their history stays
    /// in the Heart.
    pub(crate) threads: Vec<DeleteThread>,
    /// Worktrees the Bakery made, removed.
    pub(crate) worktrees: Vec<DeleteWorktree>,
    /// Branches the Bakery made for what goes, deleted.
    pub(crate) branches: Vec<DeleteBranch>,
    /// The unsaved work in those worktrees and branches, kept in git first.
    pub(crate) saved: Vec<DeleteSaved>,
    /// The threads' sandbox rooms, the node's own runtime folders, removed.
    pub(crate) rooms: Vec<String>,
    /// A deleted project's build folder: cargo's `target` and crates, which
    /// its threads' rooms shared. Build output, made again by the next
    /// build: deleted outright, not sent to the Trash.
    pub(crate) builds: Vec<String>,
    pub(crate) containers: Vec<DeleteEngineThing>,
    pub(crate) volumes: Vec<DeleteEngineThing>,
    pub(crate) networks: Vec<DeleteEngineThing>,
    /// Container engines set up here that are off and that what goes could
    /// have used: each is started, what it holds that is tied to what goes
    /// removed, and stopped again.
    pub(crate) engines: Vec<Backend>,
    /// Folders moved to the Trash: a chat's own, a project folder the
    /// Bakery created, a project's cache, the operator's own once ticked.
    pub(crate) trash: Vec<String>,
    /// The folders in `trash` with uncommitted changes, or changes that
    /// can't be read: they go to the Trash with them.
    pub(crate) trash_changes: Vec<DeleteWorktree>,
    /// Every branch in a repository in `trash`: it goes to the Trash too.
    pub(crate) trash_branches: Vec<DeleteBranch>,
    /// The project roots it lets go of (`project.root.detached`), by path:
    /// a repo's, and each removed worktree's. A worktree that stays keeps
    /// its root, and a deleted project keeps its own: it leaves the lists
    /// whole.
    pub(crate) roots: Vec<String>,
    /// The operator's own project folders (never the Bakery's own
    /// repository): they stay on disk unless `trash_own`, then they are in
    /// `trash`.
    pub(crate) own: Vec<String>,
    /// Folders that stay because something staying uses them, and why.
    pub(crate) kept: Vec<DeleteKept>,
    /// Branches the Bakery made that stay because something staying uses
    /// them, or their work couldn't be read, and why.
    pub(crate) kept_branches: Vec<DeleteKeptBranch>,
    /// This plan's fingerprint: `POST /delete/confirm` carries it out only
    /// while a fresh plan still has it.
    pub(crate) digest: String,
}

/// What a delete removed, by the plan's own names.
#[derive(schemars::JsonSchema, Debug, Clone, Default, PartialEq, Serialize)]
pub(crate) struct DeleteRemoved {
    /// The threads discarded, by stream_id.
    pub(crate) threads: Vec<String>,
    pub(crate) worktrees: Vec<String>,
    pub(crate) branches: Vec<DeleteBranch>,
    pub(crate) rooms: Vec<String>,
    pub(crate) builds: Vec<String>,
    pub(crate) containers: Vec<String>,
    pub(crate) volumes: Vec<String>,
    pub(crate) networks: Vec<String>,
    pub(crate) trash: Vec<String>,
    pub(crate) roots: Vec<String>,
}

impl DeleteRemoved {
    fn sorted(mut self) -> Self {
        for list in [
            &mut self.threads,
            &mut self.worktrees,
            &mut self.rooms,
            &mut self.builds,
            &mut self.containers,
            &mut self.volumes,
            &mut self.networks,
            &mut self.trash,
            &mut self.roots,
        ] {
            list.sort();
        }
        self.branches.sort();
        self
    }

    /// A container, volume or network removed, in its list.
    fn thing(&mut self, thing: Thing) {
        match thing.kind {
            Kind::Containers => self.containers.push(thing.name),
            Kind::Volumes => self.volumes.push(thing.name),
            _ => self.networks.push(thing.name),
        }
    }
}

/// A delete carried out, as `delete.done` records it.
#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct DeleteDone {
    /// The plan it carried out.
    pub(crate) plan: DeletePlan,
    pub(crate) removed: DeleteRemoved,
    /// Each step that didn't go as the plan said, and why; empty when all of
    /// it did. A thread whose discard failed stays, and so does what it, or
    /// a project terminal that didn't end, may still use: only the roots of
    /// worktrees that went and the discarded threads' own branches,
    /// containers, volumes and networks go then, and a new plan lists the
    /// rest.
    pub(crate) errors: Vec<String>,
}

/// What carrying a plan out takes, beyond what it shows.
pub(crate) struct Steps {
    /// Each thread's discard, in the plan's order.
    fates: Vec<(String, Fate)>,
    /// Bakery-made worktrees whose threads were discarded before (a
    /// project's roots then): their repository, path and branch.
    orphans: Vec<(String, PathBuf, String)>,
    /// Branches deleted: repository, name, and the thread that used it
    /// (none for an orphan's).
    branches: Vec<(PathBuf, String, Option<String>)>,
    /// Roots let go: project, root id, path, and the worktree that must be
    /// gone first, when the root is one.
    roots: Vec<(String, String, String, Option<PathBuf>)>,
    /// Containers, then volumes, then networks.
    things: Vec<Thing>,
    /// Engines that are off, started to clear and stopped again.
    engines: Vec<Backend>,
    /// Root folders and a project's cache, to the Trash.
    trash: Vec<PathBuf>,
    /// A project's build folder, deleted outright.
    builds: Vec<PathBuf>,
    /// The project retired with it.
    retire: Option<String>,
    /// The project whose own terminals end first: the one retired, or the
    /// one whose folders go to the Trash.
    fence: Option<String>,
    /// Where `delete.done` goes.
    record_on: String,
}

/// Who and what a delete takes.
struct Scope<'a> {
    title: String,
    going: Vec<&'a StreamRef>,
    /// A project's delete: the project, retired with it.
    retire: Option<ProjectRecord>,
    /// The project whose roots' folders it decides, and those roots: all of
    /// a deleted project's, or a repo's, which leave the project.
    roots: Option<(ProjectRecord, Vec<RootRecord>)>,
    record_on: String,
}

/// What stays, so that nothing removed breaks it.
struct Stays {
    /// The Bakery's own data.
    base: PathBuf,
    /// Each thread that stays: its title, its folder and its repository.
    threads: Vec<(String, Option<PathBuf>, Option<PathBuf>)>,
    /// Each thread that stays on a branch: its title, its repository and
    /// the branch.
    branches: Vec<(String, PathBuf, String)>,
    /// Each root of a project that stays: the project's name, the root, and
    /// where it is.
    roots: Vec<(String, RootRecord, PathBuf)>,
    /// The repositories of worktrees that stay.
    repos: Vec<PathBuf>,
}

impl Stays {
    /// Why `folder` must stay, if it must: it holds the Bakery's own data,
    /// a thread that stays works in it or in a worktree of it, it is or
    /// holds a staying project's root (but those in `ignore`), or a worktree
    /// that stays belongs to it. Fails closed: a folder that can't be
    /// checked stays.
    fn why(&self, folder: &Path, ignore: &BTreeSet<String>) -> Option<String> {
        let folder = match folder.canonicalize() {
            Ok(folder) => folder,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
            Err(e) => return Some(format!("it couldn't be checked ({e})")),
        };
        if self.base.starts_with(&folder) {
            return Some("it holds the Bakery's own data".to_string());
        }
        for (title, works_in, repo) in &self.threads {
            if works_in.as_ref().is_some_and(|w| w.starts_with(&folder)) || repo.as_ref() == Some(&folder) {
                return Some(format!("{title} works in it"));
            }
        }
        for (project, root, at) in &self.roots {
            if !ignore.contains(&root.root_id) && at.starts_with(&folder) {
                return Some(format!("it is the root of the project {project}"));
            }
        }
        self.repos
            .contains(&folder)
            .then(|| "a worktree that stays needs it".to_string())
    }
}

fn internal(e: impl ToString) -> Fail {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

fn canon(path: impl AsRef<Path>) -> PathBuf {
    let path = path.as_ref();
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn title_of(s: &StreamRef) -> String {
    s.title.clone().unwrap_or_else(|| s.name.clone())
}

fn shown(path: PathBuf) -> String {
    path.display().to_string()
}

/// A project by id, removed from the lists or not.
fn project_of(state: &AppState, project_id: &str) -> Result<ProjectRecord, Fail> {
    let heart = state.heart.lock().unwrap();
    crate::catalog::project_record_locked(&heart, project_id)
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("no project {project_id}")))
}

fn worktree(stream_id: &str, path: &str, branch: &str) -> DeleteWorktree {
    let (changes, error) = match crate::git::porcelain_status(Path::new(path)) {
        Ok(changes) => (changes, None),
        Err(e) => (Vec::new(), Some(e)),
    };
    DeleteWorktree {
        stream_id: stream_id.to_string(),
        path: path.to_string(),
        branch: branch.to_string(),
        changes,
        error,
    }
}

/// A folder going to the Trash whole, as it stands, and every branch in it.
/// A repository of its own shows its changes; any other folder's can't be
/// read (Git says why, or which repository holds it).
fn made_folder(folder: &str) -> (DeleteWorktree, Vec<String>) {
    let info = crate::git::inspect(folder);
    if info.root.as_deref().is_some_and(|root| same_path(root, folder)) {
        let row = worktree("", folder, info.current_branch.as_deref().unwrap_or_default());
        return (row, info.local_branches);
    }
    let why = info.error.unwrap_or_else(|| format!("it is inside the repository {}", info.root.unwrap_or_default()));
    let row = DeleteWorktree {
        stream_id: String::new(),
        path: folder.to_string(),
        branch: String::new(),
        changes: Vec::new(),
        error: Some(why),
    };
    (row, Vec::new())
}

/// The plan's row for the unsaved work in a worktree or branch, if it has
/// any.
fn saved_row(saved: &mut Vec<DeleteSaved>, repo: &Path, name: String, unsaved: &git_trash::Unsaved) {
    if unsaved.commits > 0 || !unsaved.changes.is_empty() {
        saved.push(DeleteSaved {
            repo: shown(repo.to_path_buf()),
            name,
            commits: unsaved.commits,
            changes: unsaved.changes.clone(),
        });
    }
}

/// The discarded thread a Bakery-made worktree at `path` was made for: its
/// discard left it, as a project's root then.
fn orphan_of(state: &AppState, streams: &[StreamRef], path: &str) -> Option<crate::state::ActiveContext> {
    streams
        .iter()
        .filter(|s| s.discarded && same_path(&s.worktree_path, path))
        .filter_map(|s| crate::state::context_for(state, &s.stream_id).ok())
        .find(|ctx| lifecycle::is_bakery_owned_worktree(ctx, &state.worktree_base))
}

/// Whether the Bakery made `folder`: one inside its own `projects/` or
/// `chats/`, or one it created for a project's root, as the thread opened
/// on it records.
fn bakery_made(state: &AppState, streams: &[StreamRef], folder: &str) -> bool {
    let at = canon(folder);
    let inside = |dir: &str| {
        let dir = canon(state.base.join(dir));
        at.starts_with(&dir) && at != dir
    };
    inside("projects")
        || inside("chats")
        || streams
            .iter()
            .filter(|s| same_path(&s.worktree_path, folder))
            .filter_map(|s| crate::state::context_for(state, &s.stream_id).ok())
            .any(|ctx| ctx.created_by_bakery && !lifecycle::is_bakery_owned_worktree(&ctx, &state.worktree_base))
}

/// Whether `folder` is, holds or is inside the Bakery's own repository: the
/// own-folder tick never takes it.
fn bakery_source(state: &AppState, folder: &str) -> bool {
    let source = state.node.lock().unwrap().bakery_source_root.clone();
    source.is_some_and(|source| {
        let (source, folder) = (canon(source), canon(folder));
        source.starts_with(&folder) || folder.starts_with(&source)
    })
}

/// What ties an engine's thing to what goes, if anything does: a thread's
/// label, an agent's run in one, or, deleting a project, the project's
/// label, one of its compose stacks, or its services' volumes by name.
fn tie(thing: &Thing, threads: &BTreeSet<&str>, project: Option<&str>) -> Option<String> {
    let label = |key: &str| thing.labels.get(key).map(String::as_str);
    if let Some(thread) = label("com.bakery.stream").filter(|t| threads.contains(t)) {
        return Some(format!("com.bakery.stream={thread}"));
    }
    let ran = |who: &&str| crate::messages::sender(who).is_some_and(|t| threads.contains(t.as_str()));
    if let Some(who) = label("com.bakery.run").filter(ran) {
        return Some(format!("com.bakery.run={who}"));
    }
    let project = project?;
    if label("com.bakery.project") == Some(project) {
        return Some(format!("com.bakery.project={project}"));
    }
    let stacks = format!("{}-", crate::containers::compose_name(project));
    if let Some(stack) = label("com.docker.compose.project").filter(|s| s.starts_with(&stacks)) {
        return Some(format!("com.docker.compose.project={stack}"));
    }
    let named = |service: &&&str| thing.name == crate::services::name_of(project, service);
    let service = crate::services::KINDS.iter().find(named).filter(|_| thing.kind == Kind::Volumes)?;
    Some(format!("the project's {service} volume, by its name"))
}

/// Containers first, then volumes, then networks: each goes once nothing
/// uses it.
fn order(kind: Kind) -> u8 {
    match kind {
        Kind::Containers => 0,
        Kind::Volumes => 1,
        _ => 2,
    }
}

/// Tests never touch this Mac's engines: what a test puts here stands in for
/// them,
static TEST_ENGINE: std::sync::Mutex<Vec<Thing>> = std::sync::Mutex::new(Vec::new());
/// and the engines a test has off, by its node's folder, so tests side by
/// side never see each other's.
static TEST_OFF: std::sync::Mutex<Vec<(PathBuf, Backend)>> = std::sync::Mutex::new(Vec::new());

fn test_off(base: &Path) -> Vec<Backend> {
    let off = TEST_OFF.lock().unwrap();
    off.iter().filter(|(at, _)| at == base).map(|(_, backend)| *backend).collect()
}

/// What the engines that run hold (none is started), and the ones set up
/// here that are off.
fn engine_things(base: &Path) -> (Vec<Thing>, Vec<Backend>) {
    if cfg!(test) {
        let off = test_off(base);
        let on = TEST_ENGINE.lock().unwrap().iter().filter(|t| !off.contains(&t.backend)).cloned().collect();
        return (on, off);
    }
    let (mut found, mut off) = (Vec::new(), Vec::new());
    for backend in [Backend::Apple, Backend::Docker, Backend::Orbstack] {
        match crate::containers::things(backend) {
            Some(things) => found.extend(things),
            None if crate::containers::set_up(backend) => off.push(backend),
            None => {}
        }
    }
    (found, off)
}

/// The engines what goes could have used, from the Bakery's own records
/// and config, so a delete starts no engine it never touched: Apple
/// container for a thread whose agents run in a container seat or that
/// opened Code (its editor runs in one), and a project's services on the
/// engine its setup names. Deleting a project adds the engines its stacks
/// and runs are noted on (`engine::note_project_use`), and Apple container
/// when its terminals run in one. Where it can't be told, every engine
/// counts: a thread whose agents have the container tools (they pick their
/// engine), and a compose stack brought up before engines were noted.
fn engines_used(state: &AppState, going: &[&StreamRef], retire: Option<&ProjectRecord>) -> Vec<Backend> {
    const ALL: [Backend; 3] = [Backend::Apple, Backend::Docker, Backend::Orbstack];
    let mut used = Vec::new();
    let services = |used: &mut Vec<Backend>, stacks: &crate::node::ProjectStacks| {
        if !stacks.services.is_empty() {
            used.push(crate::services::engine_of(state, stacks));
        }
    };
    let code = crate::state::cache_root(state).join("code");
    for s in going {
        let id = s.stream_id.as_str();
        if crate::machine_access::containers_on(state, id) {
            return ALL.to_vec();
        }
        let seat = matches!(
            crate::machine_access::reach_for(state, id),
            crate::machine_access::Reach::Container | crate::machine_access::Reach::Locked
        );
        if seat || code.join(crate::views::editor_name(id)).exists() {
            used.push(Backend::Apple);
        }
        services(&mut used, &crate::catalog::stacks_for_socket(state, id));
    }
    if let Some(project) = retire {
        services(&mut used, &project.stacks);
        used.extend(crate::engine::project_used(&state.base, &project.project_id));
        if project.stacks.sandbox == "container" {
            used.push(Backend::Apple);
        }
        let stacks = format!("{}-", crate::containers::compose_name(&project.project_id));
        let unnoted = std::fs::read_dir(state.base.join("compose"))
            .into_iter()
            .flatten()
            .flatten()
            .any(|f| f.file_name().to_string_lossy().starts_with(&stacks));
        if unnoted && !used.iter().any(|b| *b != Backend::Apple) {
            used.extend([Backend::Docker, Backend::Orbstack]);
        }
    }
    used
}

/// What one engine holds, once it runs.
fn things_on(backend: Backend) -> Option<Vec<Thing>> {
    if cfg!(test) {
        return Some(TEST_ENGINE.lock().unwrap().iter().filter(|t| t.backend == backend).cloned().collect());
    }
    crate::containers::things(backend)
}

fn start_engine(base: &Path, backend: Backend) -> Result<bool, String> {
    if cfg!(test) {
        TEST_OFF.lock().unwrap().retain(|(at, b)| !(at == base && *b == backend));
        return Ok(false);
    }
    crate::engine::start_for_delete(base, backend)
}

fn stop_engine(base: &Path, backend: Backend, idle_stopped: bool) -> Result<(), String> {
    if cfg!(test) {
        TEST_OFF.lock().unwrap().push((base.to_path_buf(), backend));
        return Ok(());
    }
    crate::engine::stop_after_delete(base, backend, idle_stopped)
}

fn remove_thing(base: &Path, thing: &Thing) -> Result<(), String> {
    if cfg!(test) {
        if test_off(base).contains(&thing.backend) {
            return Err("its engine is off".to_string());
        }
        TEST_ENGINE.lock().unwrap().retain(|t| t != thing);
        return Ok(());
    }
    crate::containers::remove_thing(base, thing)
}

fn remove_one(base: &Path, thing: Thing, removed: &mut DeleteRemoved, errors: &mut Vec<String>) {
    match remove_thing(base, &thing) {
        Ok(()) => removed.thing(thing),
        Err(e) => errors.push(format!("{} on {:?}: {e}", thing.name, thing.backend)),
    }
}

/// An engine that was off: started, cleared of what is tied to what goes,
/// and stopped again. One that doesn't start keeps what it holds, said.
fn clear_engine(
    base: &Path,
    backend: Backend,
    threads: &BTreeSet<&str>,
    project: Option<&str>,
    removed: &mut DeleteRemoved,
    errors: &mut Vec<String>,
) {
    let idle_stopped = match start_engine(base, backend) {
        Ok(idle_stopped) => idle_stopped,
        Err(e) => return errors.push(format!("{backend:?} didn't start, so what it holds stays: {e}")),
    };
    match things_on(backend) {
        Some(found) => {
            let mut tied: Vec<Thing> = found.into_iter().filter(|t| tie(t, threads, project).is_some()).collect();
            tied.sort_by_key(|t| (order(t.kind), t.name.clone()));
            for thing in tied {
                remove_one(base, thing, removed, errors);
            }
        }
        None => errors.push(format!("{backend:?} started but didn't answer, so what it holds stays")),
    }
    if let Err(e) = stop_engine(base, backend, idle_stopped) {
        errors.push(format!("{backend:?} didn't stop again: {e}"));
    }
}

/// Who and what deleting `target` takes, and why it can't when that is the
/// permanent Baker.
fn scope_of<'a>(
    state: &AppState,
    target: &DeleteTarget,
    streams: &'a [StreamRef],
    baker: Option<&str>,
) -> Result<(Scope<'a>, Option<String>), Fail> {
    let (member, opened_by): (HashMap<&str, String>, HashMap<&str, String>) = {
        let heart = state.heart.lock().unwrap();
        let ids = || streams.iter().map(|s| s.stream_id.as_str());
        let member = ids()
            .filter_map(|id| Some((id, crate::state::socket_project(&heart, id).0?)))
            .collect();
        let opened = ids()
            .filter_map(|id| Some((id, crate::baker_threads::opened_by_locked(&heart, id)?)))
            .collect();
        (member, opened)
    };
    // Never the permanent Baker, whatever it is asked to take.
    let live = |s: &&StreamRef| !s.discarded && baker != Some(s.stream_id.as_str());
    let in_project = |s: &StreamRef, project_id: &str| member.get(s.stream_id.as_str()).is_some_and(|p| p == project_id);
    let mut refused = None;
    let scope = match target {
        DeleteTarget::Project { project_id } => {
            let project = project_of(state, project_id)?;
            Scope {
                title: project.name.clone(),
                going: streams.iter().filter(live).filter(|s| in_project(s, project_id)).collect(),
                roots: Some((project.clone(), project.roots.clone())),
                retire: Some(project),
                record_on: project_id.clone(),
            }
        }
        DeleteTarget::Repo { project_id, repo } => {
            let project = project_of(state, project_id)?;
            let of_repo = |s: &StreamRef| match s.repo_root.is_empty() {
                true => same_path(&s.worktree_path, repo),
                false => same_path(&s.repo_root, repo),
            };
            let going: Vec<&StreamRef> = streams
                .iter()
                .filter(live)
                .filter(|s| in_project(s, project_id) && of_repo(s))
                .collect();
            // Its own root, its threads' worktrees', and those of worktrees
            // the Bakery made of it that stayed when their threads went.
            let left = |r: &RootRecord| orphan_of(state, streams, &r.path).is_some_and(|ctx| same_path(&ctx.repo_root, repo));
            let roots: Vec<RootRecord> = project
                .roots
                .iter()
                .filter(|r| same_path(&r.path, repo) || going.iter().any(|s| same_path(&s.worktree_path, &r.path)) || left(r))
                .cloned()
                .collect();
            if going.is_empty() && roots.is_empty() {
                return Err((StatusCode::NOT_FOUND, format!("{repo} is no repository of {}", project.name)));
            }
            let name = Path::new(repo)
                .file_name()
                .map_or_else(|| repo.clone(), |n| n.to_string_lossy().into_owned());
            Scope {
                title: format!("{name} in {}", project.name),
                going,
                roots: Some((project, roots)),
                retire: None,
                record_on: project_id.clone(),
            }
        }
        DeleteTarget::Workspace { stream_id } => {
            if !crate::workspaces::is_workspace(state, stream_id) {
                return Err((StatusCode::NOT_FOUND, format!("{stream_id} is no workspace")));
            }
            // Its orc, the workers it opened, and what those opened.
            let mut under = BTreeSet::from([stream_id.as_str()]);
            loop {
                let more: Vec<&str> = opened_by
                    .iter()
                    .filter(|(child, orc)| under.contains(orc.as_str()) && !under.contains(*child))
                    .map(|(child, _)| *child)
                    .collect();
                if more.is_empty() {
                    break;
                }
                under.extend(more);
            }
            let going: Vec<&StreamRef> = streams
                .iter()
                .filter(live)
                .filter(|s| under.contains(s.stream_id.as_str()))
                .collect();
            let orc = going.iter().find(|s| &s.stream_id == stream_id);
            Scope {
                title: orc.map_or_else(|| stream_id.clone(), |s| title_of(s)),
                going,
                roots: None,
                retire: None,
                record_on: stream_id.clone(),
            }
        }
        DeleteTarget::Thread { stream_id } => {
            let thread = streams
                .iter()
                .find(|s| &s.stream_id == stream_id && !s.discarded)
                .ok_or_else(|| (StatusCode::NOT_FOUND, format!("no thread {stream_id}")))?;
            if baker == Some(stream_id.as_str()) {
                refused = Some("the main Baker conversation is never deleted: delete its threads instead".to_string());
            }
            Scope {
                title: title_of(thread),
                going: streams.iter().filter(live).filter(|s| &s.stream_id == stream_id).collect(),
                roots: None,
                retire: None,
                record_on: stream_id.clone(),
            }
        }
    };
    Ok((scope, refused))
}

/// What deleting `req.target` would remove and what stays, now, and how to
/// carry that out.
pub(crate) fn plan(state: &AppState, req: &DeletePlanReq) -> Result<(DeletePlan, Steps), Fail> {
    let streams = crate::state::known_streams(state).map_err(internal)?;
    let baker = crate::baker_threads::baker_pointer(state);
    let (scope, mut refused) = scope_of(state, &req.target, &streams, baker.as_deref())?;
    let mut going = scope.going;
    going.sort_by_key(|s| (title_of(s), s.stream_id.clone()));
    let ids: BTreeSet<&str> = going.iter().map(|s| s.stream_id.as_str()).collect();

    // A turn in flight is never cut off.
    let busy: Vec<String> = {
        let lifecycle = state.lifecycle.lock().unwrap();
        going
            .iter()
            .filter(|s| {
                lifecycle.retiring.contains(&s.stream_id)
                    || lifecycle.running_turns.iter().any(|((id, _), n)| id == &s.stream_id && *n > 0)
            })
            .map(|s| title_of(s))
            .collect()
    };
    if refused.is_none() && !busy.is_empty() {
        refused = Some(format!(
            "still working: {}. Stop each turn or let it finish, then look again",
            busy.join(", ")
        ));
    }

    let going_roots: BTreeSet<String> = scope
        .roots
        .iter()
        .flat_map(|(_, roots)| roots.iter().map(|r| r.root_id.clone()))
        .collect();
    let live_roots: Vec<(String, String, RootRecord)> = crate::catalog::known_projects(state)
        .map_err(internal)?
        .into_iter()
        .flat_map(|p| {
            let (id, name) = (p.project_id, p.name);
            p.roots.into_iter().map(move |r| (id.clone(), name.clone(), r))
        })
        .collect();
    let staying_threads = || streams.iter().filter(|s| !s.discarded && !ids.contains(s.stream_id.as_str()));
    let at = |p: &str| (!p.is_empty()).then(|| canon(p));
    let mut stays = Stays {
        base: canon(state.base.as_path()),
        threads: staying_threads()
            .map(|s| (title_of(s), at(&s.worktree_path), at(&s.repo_root)))
            .collect(),
        branches: staying_threads()
            .filter(|s| !s.branch.is_empty() && !s.repo_root.is_empty())
            .map(|s| (title_of(s), canon(&s.repo_root), s.branch.clone()))
            .collect(),
        roots: live_roots
            .iter()
            .map(|(_, name, r)| (name.clone(), r.clone(), canon(&r.path)))
            .collect(),
        repos: Vec::new(),
    };

    let mut threads = Vec::new();
    let (mut worktrees, mut saved, mut rooms) = (Vec::new(), Vec::new(), Vec::new());
    let (mut trash, mut roots, mut kept, mut own) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let (mut trash_changes, mut trash_branches) = (Vec::new(), Vec::new());
    let (mut fates, mut orphans, mut root_steps, mut trash_steps) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    // The branches of the worktrees that go: repository, name, and the
    // thread that used it.
    let mut candidates: Vec<(PathBuf, String, Option<String>)> = Vec::new();
    // Repositories going to the Trash whole, their branches with them.
    let mut trash_repos: Vec<PathBuf> = Vec::new();
    // Folders already decided, so each is decided once.
    let mut claimed: BTreeSet<PathBuf> = BTreeSet::new();
    // Worktrees the Bakery made that stay: each keeps its root.
    let mut staying: BTreeSet<PathBuf> = BTreeSet::new();
    let cache = crate::state::cache_root(state);

    for s in &going {
        threads.push(DeleteThread {
            stream_id: s.stream_id.clone(),
            title: title_of(s),
            folder: s.worktree_path.clone(),
            lifetime: s.lifetime,
        });
        let ctx = match crate::state::context_for(state, &s.stream_id) {
            Ok(ctx) => ctx,
            Err(e) => {
                refused.get_or_insert(format!("{}'s record can't be read: {e}", title_of(s)));
                continue;
            }
        };
        let mut fate = Fate::default();
        let path = ctx.worktree_path.clone();
        let chat = crate::catalog::is_chat_folder(&state.base, &path);
        let ours = lifecycle::is_bakery_owned_worktree(&ctx, &state.worktree_base);
        // Each folder is decided once, by the first thread that goes from it.
        let decide = !path.is_empty() && (chat || ours) && claimed.insert(canon(&path));
        if !path.is_empty() && !chat && !ours {
            // Not the Bakery's: untouched, and its repository stays for it.
            if !ctx.in_place && !ctx.repo_root.is_empty() {
                stays.repos.push(canon(&ctx.repo_root));
            }
        } else if decide && chat {
            match stays.why(Path::new(&path), &going_roots) {
                Some(why) => {
                    fate.kept = Some(format!("its folder stays: {why}"));
                    kept.push(DeleteKept { path: path.clone(), why });
                }
                None if Path::new(&path).exists() => {
                    fate.chat = Some(PathBuf::from(&path));
                    trash.push(path.clone());
                }
                None => {}
            }
        } else if decide {
            // Its own roots (the worktree linked into its project) go with it.
            let own_roots: Vec<&(String, String, RootRecord)> = live_roots
                .iter()
                .filter(|(_, _, r)| !going_roots.contains(&r.root_id) && same_path(&r.path, &path))
                .collect();
            let mut ignore = going_roots.clone();
            ignore.extend(own_roots.iter().map(|(_, _, r)| r.root_id.clone()));
            let wt = Path::new(&path);
            let repo = Path::new(&ctx.repo_root);
            let unsaved = wt.is_dir().then(|| git_trash::unsaved(repo, &ctx.branch, Some(wt)));
            let why = stays.why(wt, &ignore).or_else(|| match &unsaved {
                Some(Err(e)) => Some(format!("its unsaved work couldn't be read: {e}")),
                _ => None,
            });
            match why {
                Some(why) => {
                    fate.kept = Some(format!("its worktree stays: {why}"));
                    kept.push(DeleteKept { path: path.clone(), why });
                    stays.repos.push(canon(repo));
                    staying.insert(canon(&path));
                }
                None => {
                    if let Some(Ok(unsaved)) = unsaved {
                        saved_row(&mut saved, repo, git_trash::name_of(&ctx.branch, wt), &unsaved);
                        worktrees.push(DeleteWorktree {
                            stream_id: s.stream_id.clone(),
                            path: path.clone(),
                            branch: ctx.branch.clone(),
                            changes: unsaved.changes,
                            error: None,
                        });
                        fate.worktree = Some(PathBuf::from(&path));
                    }
                    candidates.push((canon(repo), ctx.branch.clone(), Some(s.stream_id.clone())));
                    for (project_id, _, root) in own_roots {
                        roots.push(root.path.clone());
                        root_steps.push((project_id.clone(), root.root_id.clone(), root.path.clone(), Some(PathBuf::from(&path))));
                    }
                }
            }
        }
        let room = bakery_exec::socket_runtime_dir(&cache, &s.stream_id);
        if room.is_dir() {
            let at = Path::new(&path);
            let holds_chat = chat && fate.chat.is_none() && at.starts_with(&room) && at.exists();
            match stays.why(&room, &going_roots) {
                Some(why) => kept.push(DeleteKept { path: shown(room), why }),
                None if holds_chat => kept.push(DeleteKept {
                    path: shown(room),
                    why: "it holds the chat's folder, which stays".to_string(),
                }),
                None => {
                    rooms.push(shown(room.clone()));
                    fate.room = Some(room);
                }
            }
        }
        fates.push((s.stream_id.clone(), fate));
    }

    if let Some((project, project_roots)) = &scope.roots {
        let mut folders = Vec::new();
        // The worktrees among its roots first, so a repository they need stays.
        for root in project_roots {
            let at = canon(&root.path);
            if claimed.contains(&at) || !Path::new(&root.path).exists() {
                continue;
            }
            let Some(ctx) = orphan_of(state, &streams, &root.path) else {
                folders.push(root);
                continue;
            };
            claimed.insert(at);
            // Named as its thread named it, as every plan before.
            let (path, repo) = (&ctx.worktree_path, Path::new(&ctx.repo_root));
            let unsaved = git_trash::unsaved(repo, &ctx.branch, Some(Path::new(path)));
            let why = stays
                .why(Path::new(path), &going_roots)
                .or_else(|| unsaved.as_ref().err().map(|e| format!("its unsaved work couldn't be read: {e}")));
            match (why, unsaved) {
                (Some(why), _) => {
                    kept.push(DeleteKept { path: path.clone(), why });
                    stays.repos.push(canon(repo));
                    staying.insert(canon(path));
                }
                (None, Ok(unsaved)) => {
                    saved_row(&mut saved, repo, git_trash::name_of(&ctx.branch, Path::new(path)), &unsaved);
                    let row = DeleteWorktree {
                        stream_id: ctx.stream_id.clone(),
                        path: path.clone(),
                        branch: ctx.branch.clone(),
                        changes: unsaved.changes,
                        error: None,
                    };
                    worktrees.push(row);
                    orphans.push((ctx.repo_root.clone(), PathBuf::from(path), ctx.branch.clone()));
                    candidates.push((canon(repo), ctx.branch.clone(), None));
                }
                (None, Err(_)) => {}
            }
        }
        for root in folders {
            if !claimed.insert(canon(&root.path)) {
                continue;
            }
            let made = bakery_made(state, &streams, &root.path);
            // The Bakery's own repository is never offered.
            if !made && bakery_source(state, &root.path) {
                continue;
            }
            if let Some(why) = stays.why(Path::new(&root.path), &going_roots) {
                kept.push(DeleteKept { path: root.path.clone(), why });
                continue;
            }
            // The operator's own folder goes only when they tick it.
            if !made {
                own.push(root.path.clone());
                if !req.trash_own {
                    continue;
                }
            }
            // To the Trash whole: its changes and every branch in it with it.
            let (row, names) = made_folder(&root.path);
            if row.dirty() {
                trash_changes.push(row);
            }
            trash_repos.push(canon(&root.path));
            trash_branches.extend(names.into_iter().map(|name| DeleteBranch { name, repo: root.path.clone() }));
            trash.push(root.path.clone());
            trash_steps.push(PathBuf::from(&root.path));
        }
        // A repo's roots leave its project, a worktree's once it is gone;
        // one that stays keeps its root, and a deleted project keeps its
        // own, so a later look finds what stayed.
        if scope.retire.is_none() {
            for root in project_roots.iter().filter(|r| !staying.contains(&canon(&r.path))) {
                let goes = worktrees.iter().any(|w| same_path(&w.path, &root.path));
                let first = goes.then(|| PathBuf::from(&root.path));
                roots.push(root.path.clone());
                root_steps.push((project.project_id.clone(), root.root_id.clone(), root.path.clone(), first));
            }
        }
    }
    let (mut builds, mut build_steps) = (Vec::new(), Vec::new());
    if let Some(project) = &scope.retire {
        let own = crate::mac_sandbox::own(state, &project.project_id);
        if own.is_dir() {
            trash.push(shown(own.clone()));
            trash_steps.push(own);
        }
        let built = crate::builds::folder(state, &project.project_id);
        if built.is_dir() {
            builds.push(shown(built.clone()));
            build_steps.push(built);
        }
    }

    // The branches of the worktrees that go, each decided once: never a
    // repository's main, and one in a repository going to the Trash goes
    // there with it. One that something staying uses stays.
    let going_paths: BTreeSet<PathBuf> = worktrees.iter().map(|w| canon(&w.path)).collect();
    let (mut branches, mut kept_branches, mut branch_steps) = (Vec::new(), Vec::new(), Vec::new());
    let mut decided: BTreeSet<(PathBuf, String)> = BTreeSet::new();
    for (repo, name, by) in candidates {
        if name.is_empty() || trash_repos.contains(&repo) || !decided.insert((repo.clone(), name.clone())) {
            continue;
        }
        let shown_repo = shown(repo.clone());
        let info = crate::git::inspect(&shown_repo);
        if info.default_branch.as_deref() == Some(name.as_str()) || !info.local_branches.contains(&name) {
            continue;
        }
        let user = stays.branches.iter().find(|(_, r, b)| *r == repo && *b == name);
        let checked_out = info
            .worktrees
            .iter()
            .find(|w| w.branch.as_deref() == Some(name.as_str()) && !going_paths.contains(&canon(&w.path)));
        let unsaved = git_trash::unsaved(&repo, &name, None);
        let why = user
            .map(|(title, ..)| format!("{title} works on it"))
            .or_else(|| checked_out.map(|w| format!("it is checked out at {}", w.path)))
            .or_else(|| unsaved.as_ref().err().map(|e| format!("its unsaved work couldn't be read: {e}")));
        match (why, unsaved) {
            (Some(why), _) => kept_branches.push(DeleteKeptBranch { name, repo: shown_repo, why }),
            (None, Ok(unsaved)) => {
                if !saved.iter().any(|k: &DeleteSaved| k.name == name && same_path(&k.repo, &shown_repo)) {
                    saved_row(&mut saved, &repo, name.clone(), &unsaved);
                }
                branches.push(DeleteBranch { name: name.clone(), repo: shown_repo });
                branch_steps.push((repo, name, by));
            }
            (None, Err(_)) => {}
        }
    }

    let (found, off) = engine_things(&state.base);
    let used = engines_used(state, &going, scope.retire.as_ref());
    let engines: Vec<Backend> = off.into_iter().filter(|b| used.contains(b)).collect();
    let project = scope.retire.as_ref().map(|p| p.project_id.as_str());
    let mut things: Vec<(Thing, DeleteEngineThing)> = found
        .into_iter()
        .filter_map(|thing| {
            let by = tie(&thing, &ids, project)?;
            let row = DeleteEngineThing { engine: thing.backend, name: thing.name.clone(), by };
            Some((thing, row))
        })
        .collect();
    things.sort_by_key(|(t, _)| (order(t.kind), t.backend as u8, t.name.clone()));
    let of_kind = |kind: Kind| -> Vec<DeleteEngineThing> {
        things.iter().filter(|(t, _)| order(t.kind) == order(kind)).map(|(_, row)| row.clone()).collect()
    };
    let (containers, volumes, networks) = (of_kind(Kind::Containers), of_kind(Kind::Volumes), of_kind(Kind::Networks));

    worktrees.sort_by(|a: &DeleteWorktree, b| a.path.cmp(&b.path));
    trash_changes.sort_by(|a: &DeleteWorktree, b| a.path.cmp(&b.path));
    saved.sort_by(|a: &DeleteSaved, b| (&a.repo, &a.name).cmp(&(&b.repo, &b.name)));
    kept.sort_by(|a: &DeleteKept, b| a.path.cmp(&b.path));
    kept_branches.sort();
    for list in [&mut rooms, &mut builds, &mut trash, &mut roots, &mut own] {
        list.sort();
        list.dedup();
    }
    for list in [&mut branches, &mut trash_branches] {
        list.sort();
        list.dedup();
    }
    let mut plan = DeletePlan {
        target: req.target.clone(),
        title: scope.title,
        refused,
        threads,
        worktrees,
        branches,
        saved,
        rooms,
        builds,
        containers,
        volumes,
        networks,
        engines,
        trash,
        trash_changes,
        trash_branches,
        roots,
        own,
        kept,
        kept_branches,
        digest: String::new(),
    };
    plan.digest = blake3::hash(serde_json::to_string(&plan).map_err(internal)?.as_bytes()).to_hex()[..16].to_string();
    let fence = scope
        .roots
        .as_ref()
        .filter(|_| scope.retire.is_some() || !trash_steps.is_empty())
        .map(|(project, _)| project.project_id.clone());
    let steps = Steps {
        fates,
        orphans,
        branches: branch_steps,
        roots: root_steps,
        things: things.into_iter().map(|(thing, _)| thing).collect(),
        engines: plan.engines.clone(),
        trash: trash_steps,
        builds: build_steps,
        retire: scope.retire.map(|p| p.project_id),
        fence,
        record_on: scope.record_on,
    };
    Ok((plan, steps))
}

/// One delete at a time: the next one plans again after it.
static DELETING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Carries out the plan the operator saw: refused while it is refused, and
/// when a fresh plan's digest isn't the one they saw.
pub(crate) async fn confirm(state: &AppState, req: DeleteConfirmReq) -> Result<DeleteDone, Fail> {
    let _one = DELETING.lock().await;
    let ask = DeletePlanReq { target: req.target, trash_own: req.trash_own };
    let st = state.clone();
    let (plan, steps) = crate::actor::spawn_blocking(move || plan(&st, &ask)).await.map_err(internal)??;
    if let Some(why) = &plan.refused {
        return Err((StatusCode::CONFLICT, why.clone()));
    }
    if plan.digest != req.digest {
        return Err((
            StatusCode::CONFLICT,
            "the plan changed since it was shown: look again".to_string(),
        ));
    }
    let Steps { fates, mut orphans, mut branches, mut roots, mut things, engines, mut trash, mut builds, mut retire, fence, record_on } = steps;
    let mut removed = DeleteRemoved::default();
    let mut errors = Vec::new();
    // One stamp names all the unsaved work this delete keeps in git.
    let stamp = git_trash::stamp_now();
    let mut ties: BTreeSet<String> = fates.iter().map(|(id, _)| id.clone()).collect();
    // The folders that went: their history of turns goes with them.
    let mut folders_gone: Vec<PathBuf> = Vec::new();

    for (stream_id, mut fate) in fates {
        fate.stamp = Some(stamp.clone());
        let planned = fate.worktree.clone();
        let (chat, room) = (fate.chat.clone(), fate.room.clone());
        let record = match lifecycle::discard_as(state, &stream_id, true, Some(fate), DISCARD_WAIT).await {
            Ok((_, record)) => record,
            Err((_, e)) => {
                errors.push(format!("{stream_id}: {e}"));
                continue;
            }
        };
        let failed: Vec<&String> = [&record.worktree_error, &record.chat_error, &record.room_error]
            .into_iter()
            .flatten()
            .collect();
        if failed.is_empty() {
            removed.threads.push(stream_id.clone());
        }
        errors.extend(failed.into_iter().map(|e| format!("{stream_id}: {e}")));
        if let (Some(path), true) = (planned, record.worktree_removed) {
            folders_gone.push(path.clone());
            removed.worktrees.push(shown(path));
        }
        if record.chat_trashed {
            folders_gone.extend(chat.clone());
            removed.trash.extend(chat.map(shown));
        }
        if record.room_destroyed {
            removed.rooms.extend(room.map(shown));
        }
    }
    let mut stuck = !errors.is_empty();
    if let (false, Some(project_id)) = (stuck, &fence) {
        // Its own terminals end first: no shell is left in a folder that goes.
        let terminals = format!("{}{project_id}", crate::terminals::PROJECT_STREAM_PREFIX);
        if !crate::terminals::fence_active_ptys(state, &terminals, None).await {
            errors.push(format!("{terminals}: a terminal didn't end in time"));
            stuck = true;
        }
    }
    // A thread that didn't go as its plan said (a discard with an error
    // leaves it live), a folder of one that stayed, or a terminal of the
    // project still running may still use what the rest of the plan takes:
    // the project, its folders, its containers. That waits for a new look;
    // only what was the discarded threads' alone still goes.
    if stuck {
        ties = removed.threads.iter().cloned().collect();
        let went: BTreeSet<&str> = ties.iter().map(String::as_str).collect();
        things.retain(|thing| tie(thing, &went, None).is_some());
        branches.retain(|(.., by)| by.as_deref().is_some_and(|t| went.contains(t)));
        roots.retain(|(.., worktree)| worktree.is_some());
        orphans.clear();
        trash.clear();
        builds.clear();
        retire = None;
        errors.push("the rest of the plan waits: look again".to_string());
    }

    let st = state.clone();
    let (removed, errors) = crate::actor::spawn_blocking(move || {
        for (repo, path, branch) in orphans {
            if !path.is_dir() {
                continue;
            }
            let name = git_trash::name_of(&branch, &path);
            let gone = git_trash::keep(Path::new(&repo), &name, &branch, Some(&path), &stamp)
                .and_then(|_| crate::routes::project::remove_worktree(&st, &repo, &path));
            match gone {
                Ok(()) => {
                    folders_gone.push(path.clone());
                    removed.worktrees.push(shown(path));
                }
                Err(e) => errors.push(format!("{}: {e}", path.display())),
            }
        }
        // Each branch once its worktrees are gone, its work kept first.
        for (repo, name, _) in branches {
            let gone = git_trash::keep(&repo, &name, &name, None, &stamp).and_then(|_| git_trash::drop_branch(&repo, &name));
            match gone {
                Ok(()) => removed.branches.push(DeleteBranch { name, repo: shown(repo) }),
                Err(e) => errors.push(format!("{name} in {}: {e}", repo.display())),
            }
        }
        for (project_id, root_id, path, worktree) in roots {
            // A worktree that stayed keeps its root.
            if worktree.as_deref().is_some_and(Path::exists) {
                continue;
            }
            let fact = serde_json::to_string(&crate::node::RootDetached { root_id }).unwrap_or_default();
            match crate::state::append(&st, &project_id, "human", crate::node::ROOT_DETACHED_FAMILY, fact, vec![]) {
                Ok(_) => removed.roots.push(path),
                Err(e) => errors.push(format!("{path}: {e}")),
            }
        }
        if let Some(project_id) = &retire {
            let retired = crate::catalog::is_retired(&st.heart.lock().unwrap(), project_id);
            if !retired {
                if let Err(e) = crate::catalog::retire_project(&st, project_id) {
                    errors.push(e);
                }
            }
        }
        for thing in things {
            remove_one(&st.base, thing, &mut removed, &mut errors);
        }
        let threads: BTreeSet<&str> = ties.iter().map(String::as_str).collect();
        for backend in engines {
            clear_engine(&st.base, backend, &threads, retire.as_deref(), &mut removed, &mut errors);
        }
        // Build output is made again by the next build: deleted outright.
        for folder in builds {
            if !folder.exists() {
                continue;
            }
            match std::fs::remove_dir_all(&folder) {
                Ok(()) => removed.builds.push(shown(folder)),
                Err(e) => errors.push(format!("{}: {e}", folder.display())),
            }
        }
        for folder in trash {
            if !folder.exists() {
                continue;
            }
            match lifecycle::trash(&folder, &st.base) {
                Ok(()) => {
                    folders_gone.push(folder.clone());
                    removed.trash.push(shown(folder));
                }
                Err(e) => errors.push(e),
            }
        }
        for folder in &folders_gone {
            if let Err(e) = crate::snapshots::forget(&st.base, folder) {
                errors.push(format!("{}: its history of turns stays: {e}", folder.display()));
            }
        }
        (removed.sorted(), errors)
    })
    .await
    .map_err(internal)?;

    let mut done = DeleteDone { plan, removed, errors };
    let record = serde_json::to_string(&done)
        .map_err(|e| e.to_string())
        .and_then(|fact| crate::state::append(state, &record_on, "human", DELETE_DONE, fact, vec![]));
    if let Err(e) = record {
        done.errors.push(format!("its record: {e}"));
    }
    Ok(done)
}

fn operator_only() -> Result<(), Fail> {
    match crate::actor::agent_calling() {
        true => Err((StatusCode::FORBIDDEN, "deleting is the operator's alone".to_string())),
        false => Ok(()),
    }
}

/// `POST /delete/plan`: what a delete would remove and what stays.
pub(crate) async fn plan_route(
    State(st): State<AppState>,
    Json(req): Json<DeletePlanReq>,
) -> Result<Json<DeletePlan>, Fail> {
    operator_only()?;
    crate::actor::spawn_blocking(move || plan(&st, &req).map(|(plan, _)| Json(plan)))
        .await
        .map_err(internal)?
}

/// `POST /delete/confirm`: the plan the operator saw, carried out.
pub(crate) async fn confirm_route(
    State(st): State<AppState>,
    Json(req): Json<DeleteConfirmReq>,
) -> Result<Json<DeleteDone>, Fail> {
    operator_only()?;
    confirm(&st, req).await.map(Json)
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::DirectoryRootSpec;
    use crate::git_trash::{TrashEntry, TrashList};
    use crate::state::stream_discarded;
    use crate::testutil::*;
    use std::sync::Arc;

    /// A thread opened on `repo` in a project, as `/project/open` opens one:
    /// a worktree of its own, linked into the project as a root.
    fn thread_in(state: &AppState, repo: &Path, name: &str, project_id: &str) -> crate::state::ActiveContext {
        let ctx = crate::routes::project::open_stream(state, &open_req(repo, name)).unwrap().context;
        crate::catalog::link_socket_to_project(state, &ctx.stream_id, project_id, &ctx.worktree_path, true).unwrap();
        ctx
    }

    /// A container, volume or network on the stand-in engine.
    fn on_engine(backend: Backend, kind: Kind, name: &str, labels: &[(&str, &str)]) -> Thing {
        let labels = labels.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        let thing = Thing { backend, kind, name: name.to_string(), labels };
        TEST_ENGINE.lock().unwrap().push(thing.clone());
        thing
    }

    fn still_there(thing: &Thing) -> bool {
        TEST_ENGINE.lock().unwrap().contains(thing)
    }

    async fn plan_for(state: &AppState, target: DeleteTarget, trash_own: bool) -> Result<DeletePlan, Fail> {
        plan_route(State(state.clone()), Json(DeletePlanReq { target, trash_own }))
            .await
            .map(|Json(plan)| plan)
    }

    async fn carry_out(state: &AppState, plan: &DeletePlan, trash_own: bool) -> Result<DeleteDone, Fail> {
        let req = DeleteConfirmReq { target: plan.target.clone(), trash_own, digest: plan.digest.clone() };
        confirm_route(State(state.clone()), Json(req)).await.map(|Json(done)| done)
    }

    /// What carrying `plan` out removes when every step goes.
    fn all_of(plan: &DeletePlan) -> DeleteRemoved {
        let names = |rows: &[DeleteEngineThing]| rows.iter().map(|r| r.name.clone()).collect();
        DeleteRemoved {
            threads: plan.threads.iter().map(|t| t.stream_id.clone()).collect(),
            worktrees: paths(&plan.worktrees),
            branches: plan.branches.clone(),
            rooms: plan.rooms.clone(),
            builds: plan.builds.clone(),
            containers: names(&plan.containers),
            volumes: names(&plan.volumes),
            networks: names(&plan.networks),
            trash: plan.trash.clone(),
            roots: plan.roots.clone(),
        }
        .sorted()
    }

    fn paths(worktrees: &[DeleteWorktree]) -> Vec<String> {
        worktrees.iter().map(|w| w.path.clone()).collect()
    }

    fn ids(plan: &DeletePlan) -> BTreeSet<String> {
        plan.threads.iter().map(|t| t.stream_id.clone()).collect()
    }

    fn branch_names(branches: &[DeleteBranch]) -> BTreeSet<&str> {
        branches.iter().map(|b| b.name.as_str()).collect()
    }

    fn has_branch(repo: &Path, branch: &str) -> bool {
        let at = format!("refs/heads/{branch}");
        crate::git::run(repo, &["rev-parse", "--verify", "--quiet", &at]).is_ok()
    }

    fn tip(at: &Path) -> String {
        crate::git::run(at, &["rev-parse", "HEAD"]).unwrap().trim().to_string()
    }

    fn commit(at: &Path, file: &str) {
        std::fs::write(at.join(file), format!("{file}\n")).unwrap();
        run_git(at, &["add", file]);
        run_git(at, &["commit", "-qm", file]);
    }

    async fn trash_list(state: &AppState) -> Vec<TrashEntry> {
        let Json(TrashList { entries }) = crate::git_trash::list_route(State(state.clone())).await.unwrap();
        entries
    }

    async fn put_back(state: &AppState, entry: &TrashEntry) -> Result<crate::git_trash::PutBack, Fail> {
        let req = crate::git_trash::TrashRef { repo: entry.repo.clone(), name: entry.name.clone(), stamp: entry.stamp.clone() };
        crate::git_trash::put_back_route(State(state.clone()), Json(req)).await.map(|Json(back)| back)
    }

    /// A project's delete removes exactly what its plan listed: its threads,
    /// the worktrees the Bakery made and their branches, its room, its
    /// containers, volume and network. The uncommitted work in one is kept
    /// in git first, listed, and put back from the Trash whole. The
    /// operator's repo folder and another project's volume stay, and
    /// neither is listed as staying; the project leaves the lists, recorded.
    #[tokio::test]
    async fn a_project_goes_exactly_as_its_plan_said() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("app"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let root = DirectoryRootSpec::Attach(repo.to_str().unwrap());
        let (project, starter) = crate::catalog::create_project(&state, "App", Some(root)).unwrap();
        let pid = project.project_id.clone();
        let clean = thread_in(&state, &repo, "clean", &pid);
        let dirty = thread_in(&state, &repo, "dirty", &pid);
        std::fs::write(Path::new(&dirty.worktree_path).join("wip.txt"), "unsaved\n").unwrap();
        let room = bakery_exec::socket_runtime_dir(&crate::state::cache_root(&state), &clean.stream_id);
        std::fs::create_dir_all(room.join("home")).unwrap();
        let seat_labels = [("com.bakery", "seat"), ("com.bakery.stream", clean.stream_id.as_str())];
        let seat = on_engine(Backend::Apple, Kind::Containers, &format!("seat-{pid}"), &seat_labels);
        let service_labels = [("com.bakery", "service"), ("com.bakery.project", pid.as_str())];
        let redis = crate::services::name_of(&pid, "redis");
        let service = on_engine(Backend::Apple, Kind::Containers, &redis, &service_labels);
        let data = on_engine(Backend::Docker, Kind::Volumes, &crate::services::name_of(&pid, "postgres"), &[]);
        let network = on_engine(Backend::Apple, Kind::Networks, &format!("net-{pid}"), &service_labels);
        let elsewhere = [("com.bakery.project", "project_else")];
        let theirs = on_engine(Backend::Apple, Kind::Volumes, &format!("theirs-{pid}"), &elsewhere);

        let target = DeleteTarget::Project { project_id: pid.clone() };
        let plan = plan_for(&state, target.clone(), false).await.unwrap();
        assert_eq!(plan.refused, None);
        let going = BTreeSet::from([starter.stream_id.clone(), clean.stream_id.clone(), dirty.stream_id.clone()]);
        assert_eq!(ids(&plan), going);
        let mut both = vec![clean.worktree_path.clone(), dirty.worktree_path.clone()];
        both.sort();
        assert_eq!(paths(&plan.worktrees), both);
        assert_eq!(branch_names(&plan.branches), BTreeSet::from([clean.branch.as_str(), dirty.branch.as_str()]));
        let wip = StatusEntry { code: "??".into(), path: "wip.txt".into() };
        assert_eq!(plan.saved.len(), 1, "{:?}", plan.saved);
        assert_eq!((plan.saved[0].name.as_str(), plan.saved[0].commits), (dirty.branch.as_str(), 0));
        assert_eq!(plan.saved[0].changes, [wip], "listed with what is kept");
        assert_eq!(plan.rooms, [room.display().to_string()]);
        let names = |rows: &[DeleteEngineThing]| rows.iter().map(|r| r.name.clone()).collect::<BTreeSet<_>>();
        assert_eq!(names(&plan.containers), BTreeSet::from([seat.name.clone(), service.name.clone()]));
        assert_eq!(names(&plan.volumes), BTreeSet::from([data.name.clone()]));
        assert_eq!(names(&plan.networks), BTreeSet::from([network.name.clone()]));
        assert!(plan.trash.is_empty() && plan.roots.is_empty() && plan.engines.is_empty(), "{plan:?}");
        assert!(plan.kept.is_empty() && plan.kept_branches.is_empty(), "nothing staying uses them: {plan:?}");
        assert_eq!(plan.own.len(), 1, "its own folder is offered: {:?}", plan.own);
        assert!(same_path(&plan.own[0], repo.to_str().unwrap()));
        let again = plan_for(&state, target.clone(), false).await.unwrap();
        assert_eq!(again.digest, plan.digest, "the same delete plans the same");

        let done = carry_out(&state, &plan, false).await.unwrap();
        assert!(done.errors.is_empty(), "{:?}", done.errors);
        assert_eq!(done.removed, all_of(&plan), "it did exactly the plan");
        assert!(!Path::new(&clean.worktree_path).exists() && !Path::new(&dirty.worktree_path).exists() && !room.exists());
        assert!(!has_branch(&repo, &clean.branch) && !has_branch(&repo, &dirty.branch), "its branches go");
        assert!(has_branch(&repo, "main"), "the repository's main stays");
        let own = std::fs::read_to_string(repo.join("f.txt")).unwrap();
        assert_eq!(own, "line one\n", "the operator's folder stays");
        assert!(![&seat, &service, &data, &network].into_iter().any(still_there));
        assert!(still_there(&theirs), "another project's volume stays");
        assert!(crate::catalog::project_by_id(&state, &pid).is_none(), "it leaves the lists");
        assert!(going.iter().all(|id| stream_discarded(&state, id)));
        let fact = state.heart.lock().unwrap().last_of_family(&pid, DELETE_DONE).unwrap();
        assert!(fact.expect("recorded").payload.contains(&plan.digest));

        let trash = trash_list(&state).await;
        assert_eq!(trash.len(), 1, "{trash:?}");
        assert_eq!(trash[0].name, dirty.branch);
        assert_eq!(trash[0].changes, [StatusEntry { code: "A".into(), path: "wip.txt".into() }]);
        let back = put_back(&state, &trash[0]).await.unwrap();
        assert!(has_branch(&repo, &dirty.branch));
        let wip = std::fs::read_to_string(Path::new(&back.worktree).join("wip.txt")).unwrap();
        assert_eq!(wip, "unsaved\n", "put back as it was, uncommitted");
        let status = crate::git::porcelain_status(Path::new(&back.worktree)).unwrap();
        assert_eq!(status, [StatusEntry { code: "??".into(), path: "wip.txt".into() }]);
        assert!(trash_list(&state).await.is_empty(), "it left the Trash");
    }

    /// A branch with nothing outside main just goes; one with commits in no
    /// main is kept in git first, and put back at the same commit.
    #[tokio::test]
    async fn a_merged_branch_goes_and_an_unmerged_one_is_kept_first() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("app"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let root = DirectoryRootSpec::Attach(repo.to_str().unwrap());
        let (project, _) = crate::catalog::create_project(&state, "App", Some(root)).unwrap();
        let merged = thread_in(&state, &repo, "merged", &project.project_id);
        let unmerged = thread_in(&state, &repo, "unmerged", &project.project_id);
        commit(Path::new(&merged.worktree_path), "done.txt");
        run_git(&repo, &["merge", "-q", "--ff-only", &merged.branch]);
        commit(Path::new(&unmerged.worktree_path), "wip.txt");
        let unmerged_tip = tip(Path::new(&unmerged.worktree_path));

        let plan = plan_for(&state, DeleteTarget::Project { project_id: project.project_id.clone() }, false).await.unwrap();
        assert_eq!(branch_names(&plan.branches), BTreeSet::from([merged.branch.as_str(), unmerged.branch.as_str()]));
        assert_eq!(plan.saved.len(), 1, "{:?}", plan.saved);
        assert_eq!((plan.saved[0].name.as_str(), plan.saved[0].commits), (unmerged.branch.as_str(), 1));
        assert!(plan.saved[0].changes.is_empty());
        let done = carry_out(&state, &plan, false).await.unwrap();
        assert!(done.errors.is_empty(), "{:?}", done.errors);
        assert_eq!(done.removed, all_of(&plan));
        assert!(!has_branch(&repo, &merged.branch) && !has_branch(&repo, &unmerged.branch));

        let trash = trash_list(&state).await;
        assert_eq!(trash.len(), 1, "only the unmerged one is kept: {trash:?}");
        assert_eq!((trash[0].name.as_str(), trash[0].commits), (unmerged.branch.as_str(), 1));
        let back = put_back(&state, &trash[0]).await.unwrap();
        assert_eq!(tip(Path::new(&back.worktree)), unmerged_tip);
        assert!(crate::git::porcelain_status(Path::new(&back.worktree)).unwrap().is_empty());
    }

    /// When unsaved work can't be kept in git, nothing of it goes: the
    /// worktree, its changes, its branch and its thread stay, and the
    /// delete says why.
    #[tokio::test]
    async fn work_that_cant_be_kept_refuses_its_delete() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("app"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let thread = crate::routes::project::open_stream(&state, &open_req(&repo, "fix")).unwrap().context;
        let wt = Path::new(&thread.worktree_path);
        std::fs::write(wt.join("wip.txt"), "unsaved\n").unwrap();
        // A ref where the entry's folder of refs would go: git can't write it.
        run_git(&repo, &["update-ref", &format!("refs/bakery/trash/{}", thread.branch), "HEAD"]);

        let plan = plan_for(&state, DeleteTarget::Thread { stream_id: thread.stream_id.clone() }, false).await.unwrap();
        assert_eq!(plan.saved.len(), 1);
        let done = carry_out(&state, &plan, false).await.unwrap();
        assert!(done.errors.iter().any(|e| e.contains("couldn't be kept in git")), "{:?}", done.errors);
        assert!(done.removed.worktrees.is_empty() && done.removed.branches.is_empty(), "{:?}", done.removed);
        assert_eq!(std::fs::read_to_string(wt.join("wip.txt")).unwrap(), "unsaved\n");
        assert!(has_branch(&repo, &thread.branch) && !stream_discarded(&state, &thread.stream_id));
    }

    /// A branch a staying thread works on stays, listed with why; the
    /// worktree that goes still goes.
    #[tokio::test]
    async fn a_branch_a_staying_thread_uses_stays() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("app"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let one = crate::routes::project::open_stream(&state, &open_req(&repo, "one")).unwrap().context;
        let theirs = dir.path().join("theirs");
        run_git(&repo, &["worktree", "add", "-q", "--detach", &theirs.to_string_lossy()]);
        let mut req = open_req(&repo, "two");
        req.worktree_path = Some(theirs.to_string_lossy().into_owned());
        req.branch = Some(one.branch.clone());
        crate::routes::project::open_stream(&state, &req).unwrap();

        let plan = plan_for(&state, DeleteTarget::Thread { stream_id: one.stream_id.clone() }, false).await.unwrap();
        assert_eq!(paths(&plan.worktrees), [one.worktree_path.as_str()]);
        assert!(plan.branches.is_empty(), "{:?}", plan.branches);
        assert_eq!(plan.kept_branches.len(), 1, "{:?}", plan.kept_branches);
        assert_eq!((plan.kept_branches[0].name.as_str(), plan.kept_branches[0].why.as_str()), (one.branch.as_str(), "two works on it"));
        let done = carry_out(&state, &plan, false).await.unwrap();
        assert!(done.errors.is_empty(), "{:?}", done.errors);
        assert_eq!(done.removed, all_of(&plan));
        assert!(!Path::new(&one.worktree_path).exists() && has_branch(&repo, &one.branch));
    }

    /// The operator's own project folder goes to the Trash only when they
    /// tick it, whole, with every branch in it; the Bakery's own repository
    /// is never offered.
    #[tokio::test]
    async fn the_own_folder_goes_to_the_trash_only_when_ticked() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("app"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let root = DirectoryRootSpec::Attach(repo.to_str().unwrap());
        let (project, _) = crate::catalog::create_project(&state, "App", Some(root)).unwrap();
        let target = DeleteTarget::Project { project_id: project.project_id.clone() };
        let path = repo.to_string_lossy().into_owned();

        let off = plan_for(&state, target.clone(), false).await.unwrap();
        assert_eq!(off.own.len(), 1);
        assert!(off.trash.is_empty() && off.trash_branches.is_empty(), "{off:?}");
        let on = plan_for(&state, target.clone(), true).await.unwrap();
        assert_eq!(on.own, off.own);
        assert!(on.trash.iter().any(|t| same_path(t, &path)), "{on:?}");
        assert_eq!(branch_names(&on.trash_branches), BTreeSet::from(["main"]));
        assert_ne!(on.digest, off.digest);
        let stale = carry_out(&state, &off, true).await.unwrap_err();
        assert_eq!(stale.0, StatusCode::CONFLICT, "the tick is part of the plan");
        let done = carry_out(&state, &on, true).await.unwrap();
        assert!(done.errors.is_empty(), "{:?}", done.errors);
        assert!(!repo.exists());
        let trashed = state.base.join("test-trash").join("app").join("f.txt");
        assert_eq!(std::fs::read_to_string(trashed).unwrap(), "line one\n", "in the Trash, not deleted");

        let other = tempfile::tempdir().unwrap();
        let bakery = bakery_repo_for_test(other.path());
        let state = state_with_bakery_root(other.path(), &bakery);
        let root = DirectoryRootSpec::Attach(bakery.to_str().unwrap());
        let (project, _) = crate::catalog::create_project(&state, "Bakery", Some(root)).unwrap();
        let plan = plan_for(&state, DeleteTarget::Project { project_id: project.project_id }, true).await.unwrap();
        assert!(plan.own.is_empty() && plan.trash.is_empty(), "{plan:?}");
    }

    /// An engine that is off is started, cleared of what is tied to what
    /// goes, and stopped again; what isn't tied stays on it.
    #[tokio::test]
    async fn an_engine_that_is_off_is_started_cleared_and_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("app"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let thread = crate::routes::project::open_stream(&state, &open_req(&repo, "boxed")).unwrap().context;
        let id = thread.stream_id.as_str();
        let seat = on_engine(Backend::Orbstack, Kind::Containers, &format!("seat-{id}"), &[("com.bakery.stream", id)]);
        let other = on_engine(Backend::Orbstack, Kind::Volumes, &format!("other-{id}"), &[]);
        TEST_OFF.lock().unwrap().push((state.base.to_path_buf(), Backend::Orbstack));
        // Its agents have the container tools: any engine may hold their runs.
        state.node.lock().unwrap().machine_access.containers.insert(id.to_string());

        let plan = plan_for(&state, DeleteTarget::Thread { stream_id: id.to_string() }, false).await.unwrap();
        assert_eq!(plan.engines, [Backend::Orbstack]);
        assert!(plan.containers.is_empty(), "what it holds can't be read: {:?}", plan.containers);
        let done = carry_out(&state, &plan, false).await.unwrap();
        assert!(done.errors.is_empty(), "{:?}", done.errors);
        assert_eq!(done.removed.containers, [seat.name.clone()]);
        assert!(!still_there(&seat) && still_there(&other));
        assert_eq!(test_off(&state.base), [Backend::Orbstack], "off again");
        TEST_ENGINE.lock().unwrap().retain(|t| t != &other);
    }

    /// Every engine is set up and off.
    fn all_off(state: &AppState) {
        let mut off = TEST_OFF.lock().unwrap();
        off.extend([Backend::Apple, Backend::Docker, Backend::Orbstack].map(|b| (state.base.to_path_buf(), b)));
    }

    /// A project's setup, as `set_project_stacks` records it, without
    /// stopping any service on this Mac's engines.
    fn set_stacks(state: &AppState, project_id: &str, stacks: crate::node::ProjectStacks) {
        let fact = serde_json::to_string(&stacks).unwrap();
        crate::state::append(state, project_id, "human", crate::node::PROJECT_STACKS_FAMILY, fact, vec![]).unwrap();
    }

    /// A workspace that never used a container starts no engine, and none is
    /// listed.
    #[tokio::test]
    async fn a_workspace_that_never_used_containers_starts_no_engine() {
        let dir = tempfile::tempdir().unwrap();
        let repo = bakery_repo_for_test(dir.path());
        run_git(&repo, &["add", "."]);
        run_git(&repo, &["commit", "-qm", "the bakery"]);
        let state = state_with_bakery_root(dir.path(), &repo);
        crate::routes::baker::ensure_permanent_baker(&state, None, None).unwrap();
        let made = crate::workspaces::create_workspace(&state, "Quiet", None, "human").unwrap();
        all_off(&state);
        let plan = plan_for(&state, DeleteTarget::Workspace { stream_id: made.stream_id.clone() }, false).await.unwrap();
        assert!(plan.engines.is_empty(), "{:?}", plan.engines);
        let done = carry_out(&state, &plan, false).await.unwrap();
        assert!(done.errors.is_empty(), "{:?}", done.errors);
        assert_eq!(test_off(&state.base).len(), 3, "none was started");
    }

    /// A project whose services run on Apple container starts Apple
    /// container alone; one whose stack ran on OrbStack starts OrbStack
    /// alone.
    #[tokio::test]
    async fn a_project_starts_only_the_engines_it_used() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        all_off(&state);
        let (services, _) = crate::catalog::create_project(&state, "Services", None).unwrap();
        let stacks = crate::node::ProjectStacks { services: vec!["redis".into()], ..Default::default() };
        set_stacks(&state, &services.project_id, stacks);
        let plan = plan_for(&state, DeleteTarget::Project { project_id: services.project_id.clone() }, false).await.unwrap();
        assert_eq!(plan.engines, [Backend::Apple]);

        let (stacked, _) = crate::catalog::create_project(&state, "Stacked", None).unwrap();
        crate::engine::note_project_use(&state.base, &stacked.project_id, Backend::Orbstack);
        let plan = plan_for(&state, DeleteTarget::Project { project_id: stacked.project_id.clone() }, false).await.unwrap();
        assert_eq!(plan.engines, [Backend::Orbstack]);
        let done = carry_out(&state, &plan, false).await.unwrap();
        assert!(done.errors.is_empty(), "{:?}", done.errors);
        let off = test_off(&state.base);
        assert!(off.len() == 3 && off.contains(&Backend::Orbstack), "started and stopped again: {off:?}");
    }

    /// A thread's delete takes its worktree, its branch and the root it was
    /// in its project, never the project's repository. A plan that changed
    /// since the operator saw it is refused with nothing gone. A thread
    /// pinned since the plan is a changed plan, and the new one shows it
    /// pinned.
    #[tokio::test]
    async fn a_thread_goes_only_as_the_plan_the_operator_saw() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("app"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let root = DirectoryRootSpec::Attach(repo.to_str().unwrap());
        let (project, _) = crate::catalog::create_project(&state, "App", Some(root)).unwrap();
        let thread = thread_in(&state, &repo, "fix", &project.project_id);
        let wt = Path::new(&thread.worktree_path);
        let target = DeleteTarget::Thread { stream_id: thread.stream_id.clone() };

        let plan = plan_for(&state, target.clone(), false).await.unwrap();
        assert_eq!(paths(&plan.worktrees), [thread.worktree_path.as_str()]);
        assert_eq!(plan.roots.len(), 1, "the root its worktree was: {:?}", plan.roots);
        assert!(same_path(&plan.roots[0], &thread.worktree_path) && plan.saved.is_empty());
        std::fs::write(wt.join("late.txt"), "late\n").unwrap();
        let stale = carry_out(&state, &plan, false).await.unwrap_err();
        assert_eq!(stale.0, StatusCode::CONFLICT);
        assert!(stale.1.contains("changed"), "{}", stale.1);
        assert!(wt.join("late.txt").exists() && !stream_discarded(&state, &thread.stream_id), "nothing went");

        let plan = plan_for(&state, target.clone(), false).await.unwrap();
        let late = StatusEntry { code: "??".into(), path: "late.txt".into() };
        assert_eq!(plan.worktrees[0].changes, [late.clone()]);
        assert_eq!(plan.saved[0].changes, [late]);
        assert_eq!(plan.threads[0].lifetime, Lifetime::Slice);
        crate::state::set_non_burn_lifetime(&state, &thread.stream_id, Lifetime::Living).unwrap();
        let pinned = carry_out(&state, &plan, false).await.unwrap_err();
        assert_eq!(pinned.0, StatusCode::CONFLICT, "{}", pinned.1);
        assert!(wt.exists() && !stream_discarded(&state, &thread.stream_id), "nothing went");
        let plan = plan_for(&state, target, false).await.unwrap();
        assert_eq!(plan.threads[0].lifetime, Lifetime::Living, "the plan shows it pinned");
        let done = carry_out(&state, &plan, false).await.unwrap();
        assert!(done.errors.is_empty(), "{:?}", done.errors);
        assert_eq!(done.removed, all_of(&plan));
        assert!(!wt.exists() && !has_branch(&repo, &thread.branch), "its branch goes");
        let roots = crate::catalog::project_by_id(&state, &project.project_id).unwrap().roots;
        assert_eq!(roots.len(), 1, "only the repository's own root is left: {roots:?}");
        assert!(same_path(&roots[0].path, repo.to_str().unwrap()) && repo.join("f.txt").exists());
    }

    /// A delete takes the history of turns of the folders that go with it, a
    /// worktree and a chat's folder, and no other folder's.
    #[tokio::test]
    async fn a_delete_takes_the_turn_history_of_the_folders_that_go() {
        use crate::agent_control::OperatingMode;
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("app"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let root = DirectoryRootSpec::Attach(repo.to_str().unwrap());
        let (project, _) = crate::catalog::create_project(&state, "App", Some(root)).unwrap();
        let going = thread_in(&state, &repo, "going", &project.project_id);
        let staying = thread_in(&state, &repo, "staying", &project.project_id);
        let chat = crate::catalog::create_chat(&state, "notes").unwrap();
        // A turn in each folder, so each has a history.
        for ctx in [&going, &staying, &chat] {
            let seat = crate::node::Seat::Orchestrator;
            let turn = crate::snapshots::TurnSnapshot::begin(&state, ctx, seat, "turn_x", None, OperatingMode::Auto).expect("a snapshot");
            std::fs::write(Path::new(&ctx.worktree_path).join("turn.txt"), "a turn\n").unwrap();
            drop(turn);
        }
        let [going_at, staying_at, chat_at] = [&going, &staying, &chat]
            .map(|ctx| crate::snapshots::history_of(&state.base, &Path::new(&ctx.worktree_path).canonicalize().unwrap()));
        assert!([&going_at, &staying_at, &chat_at].iter().all(|at| at.join("HEAD").exists()));

        for (ctx, at) in [(&going, &going_at), (&chat, &chat_at)] {
            let target = DeleteTarget::Thread { stream_id: ctx.stream_id.clone() };
            let plan = plan_for(&state, target, false).await.unwrap();
            let done = carry_out(&state, &plan, false).await.unwrap();
            assert!(done.errors.is_empty(), "{:?}", done.errors);
            assert!(!at.exists(), "its history went with its folder");
            assert!(staying_at.join("HEAD").exists(), "a folder that stays keeps its history");
        }
        assert!(!going_at.exists() && !chat_at.exists());
    }

    /// The permanent Baker is never deleted: its plan says so, and carrying
    /// it out is refused with nothing gone.
    #[tokio::test]
    async fn the_main_baker_is_never_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let repo = bakery_repo_for_test(dir.path());
        let state = state_with_bakery_root(dir.path(), &repo);
        let baker = crate::routes::baker::ensure_permanent_baker(&state, None, None).unwrap().stream_id;
        let plan = plan_for(&state, DeleteTarget::Thread { stream_id: baker.clone() }, false).await.unwrap();
        assert!(plan.refused.as_deref().is_some_and(|why| why.contains("main Baker")), "{:?}", plan.refused);
        assert!(plan.threads.is_empty() && plan.kept.is_empty(), "{plan:?}");
        let refused = carry_out(&state, &plan, false).await.unwrap_err();
        assert_eq!(refused.0, StatusCode::CONFLICT);
        assert!(!stream_discarded(&state, &baker) && repo.join(".git").exists());
    }

    /// Only the operator deletes or opens the Trash: an agent can neither
    /// ask for a plan, carry one out, list the Trash nor put anything back.
    /// A turn in flight holds a delete off. The operator's delete of a chat
    /// moves its folder to the Trash, whole.
    #[tokio::test]
    async fn an_agent_is_refused_and_a_chats_folder_goes_to_the_trash() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let chat = crate::catalog::create_chat(&state, "scratch").unwrap();
        let folder = PathBuf::from(&chat.worktree_path);
        std::fs::write(folder.join("notes.md"), "mine\n").unwrap();
        let target = DeleteTarget::Thread { stream_id: chat.stream_id.clone() };
        let plan = plan_for(&state, target.clone(), false).await.unwrap();
        assert_eq!(plan.trash, [chat.worktree_path.as_str()]);

        let agent = crate::actor::agent(&chat.stream_id, crate::node::Seat::Orchestrator);
        let asked = crate::actor::scope(agent.clone(), plan_for(&state, target.clone(), false)).await;
        assert_eq!(asked.unwrap_err().0, StatusCode::FORBIDDEN);
        let carried = crate::actor::scope(agent.clone(), carry_out(&state, &plan, false)).await;
        assert_eq!(carried.unwrap_err().0, StatusCode::FORBIDDEN);
        let listed = crate::actor::scope(agent.clone(), crate::git_trash::list_route(State(state.clone()))).await;
        assert_eq!(listed.unwrap_err().0, StatusCode::FORBIDDEN);
        let entry = TrashEntry { repo: String::new(), name: "x".into(), stamp: "20261014T120000Z".into(), commits: 0, changes: vec![] };
        let back = crate::actor::scope(agent, put_back(&state, &entry)).await;
        assert_eq!(back.unwrap_err().0, StatusCode::FORBIDDEN);
        assert!(folder.join("notes.md").exists() && !stream_discarded(&state, &chat.stream_id), "nothing went");

        let turn = (chat.stream_id.clone(), crate::node::Seat::Orchestrator);
        state.lifecycle.lock().unwrap().running_turns.insert(turn.clone(), 1);
        let held = plan_for(&state, target, false).await.unwrap();
        assert!(held.refused.as_deref().is_some_and(|why| why.contains("still working")), "{:?}", held.refused);
        assert_eq!(carry_out(&state, &held, false).await.unwrap_err().0, StatusCode::CONFLICT);
        state.lifecycle.lock().unwrap().running_turns.remove(&turn);

        let done = carry_out(&state, &plan, false).await.unwrap();
        assert!(done.errors.is_empty(), "{:?}", done.errors);
        assert_eq!(done.removed, all_of(&plan));
        let trashed = state.base.join("test-trash").join(folder.file_name().unwrap()).join("notes.md");
        assert_eq!(std::fs::read_to_string(trashed).unwrap(), "mine\n", "in the Trash, not deleted");
    }

    /// A project folder the Bakery made goes to the Trash, whole, with its
    /// uncommitted changes, the project's cache and every branch in it,
    /// each listed as going there and none as staying. A chat that isn't
    /// the project's stays.
    #[tokio::test]
    async fn a_folder_the_bakery_made_goes_to_the_trash() {
        use crate::routes::project_catalog::{create_project, CreateProjectReq, CreateProjectRoot};
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let req = CreateProjectReq {
            name: "Notes".into(),
            root: Some(CreateProjectRoot::Bakery {}),
            stacks: Default::default(),
            tools: None,
        };
        let made = create_project(&state, req).unwrap().project;
        let path = made.roots[0].path.clone();
        let folder = PathBuf::from(&path);
        let side = thread_in(&state, &folder, "side", &made.project_id);
        std::fs::write(folder.join("idea.md"), "keep me findable\n").unwrap();
        let idea = StatusEntry { code: "??".into(), path: "idea.md".into() };
        let cache = crate::mac_sandbox::own(&state, &made.project_id);
        std::fs::create_dir_all(&cache).unwrap();
        let built = crate::builds::folder(&state, &made.project_id);
        std::fs::create_dir_all(built.join("target/debug")).unwrap();
        std::fs::write(built.join("target/debug/out"), "build output\n").unwrap();
        let chat = crate::catalog::create_chat(&state, "elsewhere").unwrap();

        let repo = DeleteTarget::Repo { project_id: made.project_id.clone(), repo: path.clone() };
        let alone = plan_for(&state, repo, false).await.unwrap();
        assert!(alone.builds.is_empty(), "one repository going leaves the project's builds");
        assert_eq!(alone.trash, [path.clone()]);
        assert_eq!(alone.trash_changes[0].changes, [idea.clone()], "its changes go with it");

        let target = DeleteTarget::Project { project_id: made.project_id.clone() };
        let plan = plan_for(&state, target, false).await.unwrap();
        let mut want = vec![path.clone(), cache.display().to_string()];
        want.sort();
        assert_eq!(plan.trash, want);
        assert_eq!(plan.builds, [built.display().to_string()], "its build folder goes, listed as its own step");
        assert_eq!(paths(&plan.trash_changes), [path.as_str()]);
        assert!(plan.kept.is_empty() && plan.own.is_empty(), "{plan:?}");
        assert_eq!(branch_names(&plan.trash_branches), BTreeSet::from(["main", side.branch.as_str()]));
        assert!(plan.branches.is_empty() && plan.kept_branches.is_empty(), "they go to the Trash with it: {plan:?}");
        let done = carry_out(&state, &plan, false).await.unwrap();
        assert!(done.errors.is_empty(), "{:?}", done.errors);
        assert_eq!(done.removed, all_of(&plan));
        let trashed = state.base.join("test-trash").join(folder.file_name().unwrap()).join("idea.md");
        assert_eq!(std::fs::read_to_string(trashed).unwrap(), "keep me findable\n", "in the Trash, not deleted");
        assert!(!folder.exists() && !cache.exists());
        assert!(!built.exists(), "its build folder is deleted");
        assert!(!state.base.join("test-trash").join(built.file_name().unwrap()).join("target").exists(), "outright, not to the Trash");
        assert!(Path::new(&chat.worktree_path).exists(), "a chat that isn't the project's stays");
    }

    /// A folder the Bakery made that is no repository goes to the Trash
    /// whole, Git's reason for not reading its changes shown.
    #[tokio::test]
    async fn a_made_folder_that_isnt_a_repository_goes_to_the_trash() {
        use crate::routes::project_catalog::{create_project, CreateProjectReq, CreateProjectRoot};
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let parent = dir.path().to_string_lossy().into_owned();
        let req = CreateProjectReq {
            name: "Plain".into(),
            root: Some(CreateProjectRoot::Create { parent, name: "plain".into() }),
            stacks: Default::default(),
            tools: None,
        };
        let made = create_project(&state, req).unwrap().project;
        let path = made.roots[0].path.clone();
        std::fs::write(Path::new(&path).join("notes.txt"), "mine\n").unwrap();

        let plan = plan_for(&state, DeleteTarget::Project { project_id: made.project_id.clone() }, false).await.unwrap();
        assert_eq!(plan.trash, [path.clone()]);
        assert!(plan.trash_changes[0].error.is_some(), "{plan:?}");
        let done = carry_out(&state, &plan, false).await.unwrap();
        assert!(done.errors.is_empty(), "{:?}", done.errors);
        assert!(!Path::new(&path).exists(), "to the Trash");
    }

    /// A thread whose discard doesn't go as its plan said stays, and holds
    /// the rest back, saying so: the project stays listed, with the folder
    /// the Bakery made (its worktree needs it) and its service. A volume
    /// that was only a discarded thread's still goes. Looked at again, the
    /// thread and the rest go.
    #[tokio::test]
    async fn a_thread_that_sticks_holds_the_rest_back() {
        use crate::routes::project_catalog::{create_project, CreateProjectReq, CreateProjectRoot};
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let req = CreateProjectReq {
            name: "Held".into(),
            root: Some(CreateProjectRoot::Bakery {}),
            stacks: Default::default(),
            tools: None,
        };
        let made = create_project(&state, req).unwrap().project;
        let pid = made.project_id.clone();
        let folder = PathBuf::from(&made.roots[0].path);
        let went = thread_in(&state, &folder, "went", &pid);
        let stuck = thread_in(&state, &folder, "stuck", &pid);
        // `git worktree remove --force` refuses a locked worktree.
        run_git(&folder, &["worktree", "lock", &stuck.worktree_path]);
        let home_labels = [("com.bakery.stream", went.stream_id.as_str())];
        let home = on_engine(Backend::Apple, Kind::Volumes, &format!("home-{pid}"), &home_labels);
        let service_labels = [("com.bakery", "service"), ("com.bakery.project", pid.as_str())];
        let service = on_engine(Backend::Apple, Kind::Containers, &crate::services::name_of(&pid, "redis"), &service_labels);

        let target = DeleteTarget::Project { project_id: pid.clone() };
        let plan = plan_for(&state, target.clone(), false).await.unwrap();
        assert!(plan.trash.contains(&made.roots[0].path), "{plan:?}");
        let done = carry_out(&state, &plan, false).await.unwrap();
        assert!(done.errors.iter().any(|e| e.contains(&stuck.stream_id)), "{:?}", done.errors);
        assert!(!still_there(&home) && still_there(&service), "{:?}", done.removed);
        assert!(done.removed.trash.is_empty() && Path::new(&stuck.worktree_path).exists());
        crate::git::run(Path::new(&stuck.worktree_path), &["status"]).expect("its repository stays in place");
        assert!(!stream_discarded(&state, &stuck.stream_id) && stream_discarded(&state, &went.stream_id));
        assert!(crate::catalog::project_by_id(&state, &pid).is_some(), "the project stays listed");

        run_git(&folder, &["worktree", "unlock", &stuck.worktree_path]);
        let again = plan_for(&state, target, false).await.unwrap();
        assert_eq!(ids(&again), BTreeSet::from([stuck.stream_id.clone()]));
        assert_eq!(paths(&again.worktrees), [stuck.worktree_path.as_str()]);
        let done = carry_out(&state, &again, false).await.unwrap();
        assert!(done.errors.is_empty(), "{:?}", done.errors);
        assert_eq!(done.removed, all_of(&again));
        assert!(!folder.exists() && !still_there(&service));
        assert!(crate::catalog::project_by_id(&state, &pid).is_none(), "it leaves the lists");
    }

    /// A repository's folder the Bakery made goes to the Trash only once
    /// the project's own terminals have ended: one that doesn't end in time
    /// holds the rest back, and a new look takes it.
    #[tokio::test]
    async fn a_project_terminal_that_doesnt_end_keeps_its_folder() {
        use crate::routes::project_catalog::{create_project, CreateProjectReq, CreateProjectRoot};
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let req = CreateProjectReq {
            name: "Shell".into(),
            root: Some(CreateProjectRoot::Bakery {}),
            stacks: Default::default(),
            tools: None,
        };
        let made = create_project(&state, req).unwrap().project;
        let folder = made.roots[0].path.clone();
        let target = DeleteTarget::Repo { project_id: made.project_id.clone(), repo: folder.clone() };
        let plan = plan_for(&state, target.clone(), false).await.unwrap();
        assert_eq!(plan.trash, [folder.as_str()]);
        // A terminal of the project, admitted and never ending.
        let terminals = format!("{}{}", crate::terminals::PROJECT_STREAM_PREFIX, made.project_id);
        let shell = crate::state::admit_pty(&state, &terminals, "room").unwrap();
        let done = carry_out(&state, &plan, false).await.unwrap();
        assert!(done.errors.iter().any(|e| e.starts_with(&terminals)), "{:?}", done.errors);
        assert!(done.removed.trash.is_empty() && done.removed.roots.is_empty(), "{:?}", done.removed);
        assert!(Path::new(&folder).join("README.md").exists());

        drop(shell);
        let again = plan_for(&state, target, false).await.unwrap();
        let done = carry_out(&state, &again, false).await.unwrap();
        assert!(done.errors.is_empty(), "{:?}", done.errors);
        assert_eq!(done.removed, all_of(&again));
        assert!(!Path::new(&folder).exists());
    }

    /// A repository leaves its project with its threads, the worktrees the
    /// Bakery made of it and their branches, uncommitted work kept in git
    /// first; its folder stays, offered to the Trash, and the project's
    /// other repository is untouched.
    #[tokio::test]
    async fn a_repo_leaves_its_project_and_its_folder_stays() {
        let dir = tempfile::tempdir().unwrap();
        let app = make_repo(&dir.path().join("app"));
        let api = make_repo(&dir.path().join("api"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let root = DirectoryRootSpec::Attach(app.to_str().unwrap());
        let (project, _) = crate::catalog::create_project(&state, "Both", Some(root)).unwrap();
        let pid = project.project_id.clone();
        let on_app = thread_in(&state, &app, "on-app", &pid);
        let mut in_place = open_req(&api, "api-main");
        in_place.worktree_path = Some(api.to_string_lossy().into_owned());
        let api_main = crate::routes::project::open_stream(&state, &in_place).unwrap().context;
        crate::catalog::link_socket_to_project(&state, &api_main.stream_id, &pid, &api_main.worktree_path, true).unwrap();
        let on_api = thread_in(&state, &api, "on-api", &pid);
        let wip = thread_in(&state, &api, "wip", &pid);
        std::fs::write(Path::new(&wip.worktree_path).join("wip.txt"), "unsaved\n").unwrap();
        let repo = api.canonicalize().unwrap().to_string_lossy().into_owned();

        let target = DeleteTarget::Repo { project_id: pid.clone(), repo: repo.clone() };
        let plan = plan_for(&state, target.clone(), false).await.unwrap();
        let going = [&api_main.stream_id, &on_api.stream_id, &wip.stream_id];
        assert_eq!(ids(&plan), going.into_iter().cloned().collect());
        let mut both = vec![on_api.worktree_path.clone(), wip.worktree_path.clone()];
        both.sort();
        assert_eq!(paths(&plan.worktrees), both);
        assert_eq!(branch_names(&plan.branches), BTreeSet::from([on_api.branch.as_str(), wip.branch.as_str()]));
        assert_eq!(plan.saved.len(), 1, "{:?}", plan.saved);
        assert_eq!(plan.roots.len(), 3, "the repository's root and its worktrees': {:?}", plan.roots);
        assert!(plan.own.len() == 1 && same_path(&plan.own[0], &repo) && plan.kept.is_empty(), "{plan:?}");
        let done = carry_out(&state, &plan, false).await.unwrap();
        assert!(done.errors.is_empty(), "{:?}", done.errors);
        assert_eq!(done.removed, all_of(&plan));
        assert!(api.join("f.txt").exists() && !has_branch(&api, &on_api.branch) && !has_branch(&api, &wip.branch));
        assert!(!Path::new(&on_api.worktree_path).exists() && !Path::new(&wip.worktree_path).exists());
        assert!(Path::new(&on_app.worktree_path).exists() && !stream_discarded(&state, &on_app.stream_id));
        assert!(has_branch(&app, &on_app.branch), "the other repository is untouched");
        let left = crate::catalog::project_by_id(&state, &pid).expect("the project stays").roots;
        assert_eq!(left.len(), 2, "app's root and on-app's worktree: {left:?}");
        assert!(left.iter().all(|r| !same_path(&r.path, &repo)), "{left:?}");
    }

    /// A workspace goes with its orc and the workers under it, their
    /// worktrees and branches with them; a worker alone is no workspace,
    /// and the Baker and the Bakery's own main stay.
    #[tokio::test]
    async fn a_workspace_goes_with_its_workers() {
        let dir = tempfile::tempdir().unwrap();
        let repo = bakery_repo_for_test(dir.path());
        run_git(&repo, &["add", "."]);
        run_git(&repo, &["commit", "-qm", "the bakery"]);
        let state = state_with_bakery_root(dir.path(), &repo);
        let home = crate::routes::baker::ensure_permanent_baker(&state, None, None).unwrap().stream_id;
        let made = crate::workspaces::create_workspace(&state, "Tidy", None, "human").unwrap();
        let req = crate::baker_threads::CreateThreadReq { title: Some("worker".into()), ..Default::default() };
        let worker = crate::baker_threads::create_peer(&state, &req, &made.stream_id, "human").unwrap();
        let worker = crate::state::context_for(&state, &worker.thread.stream_id).unwrap();
        let alone = plan_for(&state, DeleteTarget::Workspace { stream_id: worker.stream_id.clone() }, false).await;
        assert_eq!(alone.unwrap_err().0, StatusCode::NOT_FOUND);

        let target = DeleteTarget::Workspace { stream_id: made.stream_id.clone() };
        let plan = plan_for(&state, target, false).await.unwrap();
        assert_eq!(plan.title, "Tidy");
        assert_eq!(ids(&plan), BTreeSet::from([made.stream_id.clone(), worker.stream_id.clone()]));
        let mut want = vec![made.worktree_path.clone(), worker.worktree_path.clone()];
        want.sort();
        assert_eq!(paths(&plan.worktrees), want);
        let done = carry_out(&state, &plan, false).await.unwrap();
        assert!(done.errors.is_empty(), "{:?}", done.errors);
        assert_eq!(done.removed, all_of(&plan));
        assert!(want.iter().all(|wt| !Path::new(wt).exists()));
        assert!(!has_branch(&repo, &made.branch) && !has_branch(&repo, &worker.branch), "their branches go");
        assert!(has_branch(&repo, "main"), "the Bakery's own main stays");
        assert!(!stream_discarded(&state, &home) && repo.join("VISION.md").exists(), "the Baker stays");
    }
}
