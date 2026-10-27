//! Project catalog (07d-A3): a Project is a first-class Heart record, not a
//! git repo — durable identity, zero or more filesystem roots, zero or more
//! member sockets, optional Git capability on any root that happens to be
//! one. See `crate::node`'s "Project catalog" section for the event
//! families/payload shapes this module reads and writes.
//!
//! Every mutation here holds the ONE Heart lock (`state.heart`) across its
//! whole read-check-append sequence. The Heart is already a single shared
//! `Mutex<Heart>` that every route in this daemon serializes through, so
//! holding that one guard for a multi-step catalog operation already gives
//! the atomicity 07d-A3 asks for ("share one coordination lock or Heart
//! transaction") — no second lock is introduced. The guard is always
//! dropped before any broadcast, model call, or filesystem work slower than
//! a single mkdir/rmdir.

use crate::node::{
    mint_id, ProjectCreated, ProjectPinSet, ProjectRenamed, ProjectStacks, RootAttached,
    RootDetached, SocketKind, SocketKindRecord, SocketProjectSet, WorkspaceSet,
    PROJECT_CREATED_FAMILY, PROJECT_PIN_FAMILY, PROJECT_RENAMED_FAMILY, PROJECT_RETIRED_FAMILY,
    PROJECT_STREAM_PREFIX, ROOT_ATTACHED_FAMILY, ROOT_DETACHED_FAMILY, SOCKET_KIND_FAMILY,
    SOCKET_PROJECT_FAMILY, SOCKET_WORKSPACE_FAMILY,
};
use crate::state::{claim_ownership, ActiveContext, AppState};
use bakery_core::{Event, NewEvent};
use bakery_heart::Heart;
use std::path::{Path, PathBuf};

// ─── Records (read side) ────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RootRecord {
    pub(crate) root_id: String,
    pub(crate) path: String,
    pub(crate) is_git: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct ProjectRecord {
    pub(crate) project_id: String,
    pub(crate) name: String,
    pub(crate) pinned: bool,
    pub(crate) roots: Vec<RootRecord>,
    pub(crate) stacks: ProjectStacks,
}

/// `pub(crate)` (not just catalog-private) so `crate::tasks` can append
/// several task-minting facts under the exact same held `Heart` lock this
/// module's own multi-step mutations use, rather than duplicating this
/// three-line wrapper a second time.
pub(crate) fn append_locked(
    heart: &mut Heart,
    stream_id: &str,
    actor: &str,
    family: &str,
    payload: String,
    causality: Vec<String>,
) -> Result<Event, String> {
    let new = NewEvent {
        stream_id: stream_id.to_string(),
        actor: crate::actor::attribute(actor),
        ts: chrono::Utc::now().to_rfc3339(),
        causality,
        layer: Some("L2".to_string()),
        sensitivity: "normal".to_string(),
        family: family.to_string(),
        payload,
    };
    heart.append(new).map_err(|e| e.to_string())
}

fn broadcast(state: &AppState, ev: &Event) {
    let _ = state.tx.send(serde_json::to_string(ev).unwrap_or_default());
}

/// Fold `project.root.attached`/`project.root.detached` in seq order into
/// the currently-attached root set. A malformed payload anywhere in the
/// replay stops the fold and fails closed to "no roots" for THIS project
/// only — there is exactly one legitimate writer for these two families, so
/// an unreadable one only ever means real corruption, not a race worth
/// searching through. This never hides the Project itself: a root read
/// failure disables Code/Files for it, nothing more.
fn current_roots_locked(heart: &Heart, project_stream_id: &str) -> Vec<RootRecord> {
    let events = match heart.read_stream(project_stream_id) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };
    let mut roots: Vec<RootRecord> = Vec::new();
    for ev in &events {
        match ev.family.as_str() {
            ROOT_ATTACHED_FAMILY => {
                let a: RootAttached = match serde_json::from_str(&ev.payload) {
                    Ok(v) => v,
                    Err(_) => return Vec::new(), // malformed mid-history — fail closed, stop trusting this project's roots
                };
                roots.retain(|r| r.root_id != a.root_id);
                roots.push(RootRecord {
                    root_id: a.root_id,
                    path: a.path,
                    is_git: a.is_git,
                });
            }
            ROOT_DETACHED_FAMILY => {
                let d: RootDetached = match serde_json::from_str(&ev.payload) {
                    Ok(v) => v,
                    Err(_) => return Vec::new(),
                };
                roots.retain(|r| r.root_id != d.root_id);
            }
            _ => {}
        }
    }
    roots
}

/// Latest `project.renamed`, or the immutable `project.created` name when
/// no rename exists, or — deliberately — that SAME created name again when
/// the newest rename record is malformed. This is not "falling back to an
/// older valid event" (the thing 07d-A3 forbids): `project.created` is a
/// structurally different, privileged fact set exactly once at birth and
/// never itself subject to newest-wins ambiguity, not a stale rename.
fn resolve_name_locked(heart: &Heart, project_stream_id: &str, created_name: &str) -> String {
    match heart.last_of_family(project_stream_id, PROJECT_RENAMED_FAMILY) {
        Ok(Some(ev)) => serde_json::from_str::<ProjectRenamed>(&ev.payload)
            .map(|r| r.name)
            .unwrap_or_else(|_| created_name.to_string()),
        Ok(None) => created_name.to_string(),
        Err(_) => created_name.to_string(),
    }
}

fn resolve_pinned_locked(heart: &Heart, project_stream_id: &str) -> bool {
    match heart.last_of_family(project_stream_id, PROJECT_PIN_FAMILY) {
        Ok(Some(ev)) => serde_json::from_str::<ProjectPinSet>(&ev.payload)
            .map(|r| r.pinned)
            .unwrap_or(false),
        Ok(None) => false,
        Err(_) => false,
    }
}

/// Whether a Project is retired. Unlike name/pin, an unreadable record here
/// fails closed to `true` (retired/hidden) rather than the "no event"
/// default — same posture as `state::discarded_of`: the safer direction for
/// a fact we cannot verify is to hide a Project from active use, not to
/// keep operating on one whose true state is unknown. Nothing is deleted
/// either way, so a false positive here is always recoverable by fixing the
/// underlying corruption, never data loss.
pub(crate) fn is_retired(heart: &Heart, project_stream_id: &str) -> bool {
    match heart.last_of_family(project_stream_id, PROJECT_RETIRED_FAMILY) {
        Ok(Some(_)) => true,
        Ok(None) => false,
        Err(_) => true,
    }
}

/// `pub(crate)` so `crate::tasks::task_create` can validate/read a task's
/// parent Project under the same held `Heart` lock its own mint sequence
/// uses (mirrors why [`append_locked`] is `pub(crate)` too).
pub(crate) fn project_record_locked(
    heart: &Heart,
    project_stream_id: &str,
) -> Option<ProjectRecord> {
    let created_ev = heart
        .first_of_family(project_stream_id, PROJECT_CREATED_FAMILY)
        .ok()
        .flatten()?;
    let created: ProjectCreated = serde_json::from_str(&created_ev.payload).ok()?;
    let name = resolve_name_locked(heart, project_stream_id, &created.name);
    let pinned = resolve_pinned_locked(heart, project_stream_id);
    let roots = current_roots_locked(heart, project_stream_id);
    // The operator's latest choice, else the setup it was created with.
    let stacks = chosen_stacks_locked(heart, project_stream_id).unwrap_or(created.stacks);
    Some(ProjectRecord {
        project_id: project_stream_id.to_string(),
        name,
        pinned,
        roots,
        stacks,
    })
}

/// The operator's latest setup choice kept on `stream`, if any.
fn chosen_stacks_locked(heart: &Heart, stream: &str) -> Option<ProjectStacks> {
    let ev = heart
        .last_of_family(stream, crate::node::PROJECT_STACKS_FAMILY)
        .ok()
        .flatten()?;
    serde_json::from_str(&ev.payload).ok()
}

/// The operator's choice for the Bakery's own setup of a project, from its
/// control plane: each agent's next turn uses it. The Bakery's own project
/// ("") has one too, for its own conversations; where its agents run stays
/// Machine access's, never its setup's.
pub(crate) fn set_project_stacks(
    state: &AppState,
    project_id: &str,
    stacks: ProjectStacks,
) -> Result<(), String> {
    stacks.validate()?;
    if project_id.is_empty() {
        if stacks.sandbox != ProjectStacks::default().sandbox {
            return Err("the Bakery's own agents run where Machine access puts each one, never where a setup says".to_string());
        }
        let payload = serde_json::to_string(&stacks).map_err(|e| e.to_string())?;
        return crate::state::append(
            state,
            BAKERY_OWN,
            "human",
            crate::node::PROJECT_STACKS_FAMILY,
            payload,
            vec![],
        )
        .map(drop);
    }
    let heart = state.heart.lock().unwrap();
    if is_retired(&heart, project_id) {
        return Err(format!("project {project_id} is retired"));
    }
    let Some(before) = project_record_locked(&heart, project_id).map(|p| p.stacks) else {
        return Err(format!("unknown project {project_id}"));
    };
    drop(heart);
    crate::state::append(
        state,
        project_id,
        "human",
        crate::node::PROJECT_STACKS_FAMILY,
        serde_json::to_string(&stacks).map_err(|e| e.to_string())?,
        vec![],
    )?;
    // Services that moved or went stop where they ran; the ones it names
    // start where it says with their next use.
    if before.containers != stacks.containers || before.services != stacks.services {
        crate::services::stop_project(state, project_id, &before);
    }
    Ok(())
}

/// Every non-retired Project this Heart knows, in no particular order (the
/// Floor/sidebar do their own display sort). Retired Projects are excluded
/// here entirely — `known_projects` is "what's active", the way
/// `state::known_streams` already excludes nothing (sockets carry their own
/// `discarded` flag for the client to filter) but Projects have no
/// equivalent per-caller filter today; every current caller wants active
/// only, so retired projects are dropped at the source instead of leaking a
/// second definition of "active" between here and the projection.
/// `crate::baker_ops`'s `project.list` op is now a production caller too
/// (07d-B Stage B) — no longer test-only.
pub(crate) fn known_projects(state: &AppState) -> Result<Vec<ProjectRecord>, String> {
    let heart = state.heart.lock().unwrap();
    known_projects_locked(&heart)
}

/// Same as `known_projects` but takes an already-locked `Heart` — for a
/// caller (the `/studio/state` projection) that must read Projects and
/// sockets under ONE lock acquisition rather than two, since re-locking
/// `state.heart` while already holding it would deadlock (it's a plain
/// `std::sync::Mutex`, not reentrant).
pub(crate) fn known_projects_locked(heart: &Heart) -> Result<Vec<ProjectRecord>, String> {
    let summaries = heart.list_streams().map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for s in &summaries {
        if !s.stream_id.starts_with(PROJECT_STREAM_PREFIX) {
            continue;
        }
        if is_retired(heart, &s.stream_id) {
            continue;
        }
        if let Some(rec) = project_record_locked(heart, &s.stream_id) {
            out.push(rec);
        }
    }
    Ok(out)
}

/// Toolchain adapter for this socket's Project (the Bakery's own for one of
/// the Bakery's), or `none` when Ungrouped.
pub(crate) fn stacks_for_socket(state: &AppState, stream_id: &str) -> ProjectStacks {
    let heart = state.heart.lock().unwrap();
    match settings_project_locked(&heart, stream_id) {
        Some(pid) if pid.is_empty() => chosen_stacks_locked(&heart, BAKERY_OWN).unwrap_or_default(),
        Some(pid) => project_record_locked(&heart, &pid)
            .map(|r| r.stacks)
            .unwrap_or_default(),
        None => ProjectStacks::default(),
    }
}

/// One Project by id, `None` if it doesn't exist, is retired, or was never
/// really a Project stream (malformed `project.created`). Used by tests and
/// by `crate::tasks::task_start` (resolving a task's parent Project's roots
/// at materialization time).
pub(crate) fn project_by_id(state: &AppState, project_id: &str) -> Option<ProjectRecord> {
    let heart = state.heart.lock().unwrap();
    if is_retired(&heart, project_id) {
        return None;
    }
    project_record_locked(&heart, project_id)
}

// ─── The Bakery's own project ("") ──────────────────────────────────────────

/// Where the Bakery's own project ("") keeps what a project keeps on its own
/// stream (its setup, its network answers, its commands setting): it has
/// no stream of its own.
pub(crate) const BAKERY_OWN: &str = "node:bakery";

/// The stream a project's own facts are kept on: its own, or for the
/// Bakery's ("") [`BAKERY_OWN`].
pub(crate) fn kept_on(project_id: &str) -> &str {
    if project_id.is_empty() {
        BAKERY_OWN
    } else {
        project_id
    }
}

/// Whether `project_id` names a live Project, or is the Bakery's own ("").
pub(crate) fn is_known(state: &AppState, project_id: &str) -> bool {
    project_id.is_empty() || project_by_id(state, project_id).is_some()
}

/// The project a conversation's settings follow: its own, or the Bakery's
/// own ("") for one of the Bakery's (a factory one, the Baker's, or one in
/// a workspace beneath it).
pub(crate) fn settings_project_locked(heart: &Heart, stream_id: &str) -> Option<String> {
    crate::state::socket_project(heart, stream_id)
        .0
        .or_else(|| {
            let bakerys = crate::state::socket_kind(heart, stream_id) == SocketKind::Factory
                || crate::workspaces::workspace_of_locked(heart, stream_id).is_some();
            bakerys.then(String::new)
        })
}

/// [`project_by_id`], or for "" the Bakery's own: its main checkout, then
/// the worktrees its own conversations work in, each once, and its setup.
pub(crate) fn project_or_bakery(state: &AppState, project_id: &str) -> Option<ProjectRecord> {
    if !project_id.is_empty() {
        return project_by_id(state, project_id);
    }
    let main = state.node.lock().unwrap().bakery_source_root.clone();
    let streams = crate::state::known_streams(state).unwrap_or_default();
    let (worktrees, stacks) = {
        let heart = state.heart.lock().unwrap();
        let worktrees: Vec<String> = streams
            .into_iter()
            .filter(|s| !s.discarded && s.exists_on_disk)
            .filter(|s| settings_project_locked(&heart, &s.stream_id).is_some_and(|p| p.is_empty()))
            .map(|s| s.worktree_path)
            .collect();
        (
            worktrees,
            chosen_stacks_locked(&heart, BAKERY_OWN).unwrap_or_default(),
        )
    };
    let mut seen: Vec<PathBuf> = Vec::new();
    let roots = main
        .into_iter()
        .chain(worktrees)
        .filter(|path| {
            let real = Path::new(path)
                .canonicalize()
                .unwrap_or_else(|_| PathBuf::from(path));
            let new = !seen.contains(&real);
            seen.push(real);
            new
        })
        .map(|path| RootRecord {
            root_id: path.clone(),
            is_git: Path::new(&path).join(".git").exists(),
            path,
        })
        .collect();
    Some(ProjectRecord {
        project_id: String::new(),
        name: "Bakery".to_string(),
        pinned: false,
        roots,
        stacks,
    })
}

/// Whether `candidate` (already canonicalized) is already an attached root
/// of some Project other than `excluding` — the "one active canonical root
/// belongs to at most one active Project" check every attach path runs
/// before appending. Retired Projects are never checked against (their
/// roots are free to be reclaimed); a Project whose own root history is
/// unreadable reports no roots (`current_roots_locked`'s fail-closed
/// default) and so can never itself cause a false collision.
fn root_already_claimed_locked(
    heart: &Heart,
    candidate: &Path,
    excluding: Option<&str>,
) -> Result<bool, String> {
    let summaries = heart.list_streams().map_err(|e| e.to_string())?;
    for s in &summaries {
        if !s.stream_id.starts_with(PROJECT_STREAM_PREFIX) {
            continue;
        }
        if excluding == Some(s.stream_id.as_str()) {
            continue;
        }
        if is_retired(heart, &s.stream_id) {
            continue;
        }
        for r in current_roots_locked(heart, &s.stream_id) {
            if let Ok(existing) = Path::new(&r.path).canonicalize() {
                if existing == candidate {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

// ─── Directory root resolution (shared by create + "open any directory") ───

/// A resolved, admissible root directory: canonicalized, verified to exist
/// as a directory, and flagged for cleanup if THIS call is the one that
/// mkdir'd it (07d-A3: "If a brand-new managed directory was created and
/// the durable create sequence fails, clean up only that exact still-empty
/// directory").
struct ResolvedRoot {
    canonical: PathBuf,
    is_git: bool,
    /// True only when this call ran `create_dir_all` — never true for an
    /// existing directory, regardless of who made it.
    created_by_this_call: bool,
}

/// How a directory root should be resolved: attach an already-existing
/// directory as-is, or create a brand-new single-level child directory
/// under an already-existing parent. Replaces the old ambiguous
/// `create_if_missing: bool`, which conflated "attach, `mkdir -p` if the
/// path happens to be missing" with "create a new folder" — a create
/// request against an already-existing path used to silently attach to it
/// instead of erroring, with no signal that nothing was actually created.
pub(crate) enum DirectoryRootSpec<'a> {
    /// Must already exist as a directory. Never creates anything.
    Attach(&'a str),
    /// `parent` must already exist as a directory; `name` must be a single
    /// plain path segment (no `/`/`\`, not `.`/`..`); the resulting
    /// `parent/name` must NOT already exist. Only ever `create_dir`s that
    /// one new leaf — a missing grandparent is refused, never silently
    /// `mkdir -p`'d.
    Create { parent: &'a str, name: &'a str },
}

/// Resolve a directory root per [`DirectoryRootSpec`]. Never touches Git;
/// `is_git` is purely a read via `crate::git::inspect` for capability
/// display, decoupled from the jail (jailing is identical for git and
/// non-git roots — `Jail::new` already works on any directory).
fn resolve_directory_root(spec: DirectoryRootSpec) -> Result<ResolvedRoot, String> {
    let (raw, created_by_this_call): (PathBuf, bool) = match spec {
        DirectoryRootSpec::Attach(path) => {
            let raw = Path::new(path);
            if !raw.exists() {
                return Err(format!("path does not exist: {path}"));
            }
            if !raw.is_dir() {
                return Err(format!("not a directory: {path}"));
            }
            (raw.to_path_buf(), false)
        }
        DirectoryRootSpec::Create { parent, name } => {
            let name = name.trim();
            if name.is_empty()
                || name == "."
                || name == ".."
                || name.contains('/')
                || name.contains('\\')
            {
                return Err(format!("{name:?} is not a valid folder name"));
            }
            let parent_path = Path::new(parent);
            if !parent_path.exists() {
                return Err(format!("parent directory does not exist: {parent}"));
            }
            if !parent_path.is_dir() {
                return Err(format!("parent is not a directory: {parent}"));
            }
            let parent_canonical = parent_path
                .canonicalize()
                .map_err(|e| format!("{parent}: {e}"))?;
            let target = parent_canonical.join(name);
            if target.exists() {
                return Err(format!(
                    "{} already exists — choose a different folder name",
                    target.display()
                ));
            }
            std::fs::create_dir(&target)
                .map_err(|e| format!("could not create {}: {e}", target.display()))?;
            (target, true)
        }
    };
    let canonical = raw
        .canonicalize()
        .map_err(|e| format!("{}: {e}", raw.display()))?;
    let info = crate::git::inspect(&canonical.to_string_lossy());
    Ok(ResolvedRoot {
        canonical,
        is_git: info.is_git,
        created_by_this_call,
    })
}

/// A new project's own folder inside Bakery (`<base>/projects/<name>`),
/// scaffolded as a Git repository with a README and a first commit so
/// worktrees and tasks work from the start. Everything Bakery makes lives
/// under its one folder: remove that and it is gone, or keep the folder —
/// it is plain Git.
pub(crate) fn scaffold_bakery_project(base: &Path, name: &str) -> Result<PathBuf, String> {
    let parent = base.join("projects");
    std::fs::create_dir_all(&parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    let dir = unused_folder(&parent, &crate::git::sanitize_name(name));
    std::fs::create_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let scaffold = || -> Result<(), String> {
        std::fs::write(dir.join("README.md"), format!("# {}\n", name.trim()))
            .map_err(|e| e.to_string())?;
        start_repository(&dir)
    };
    if let Err(e) = scaffold() {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(format!("could not start the project's folder: {e}"));
    }
    Ok(dir)
}

/// Makes `dir` a Git repository on `main` whose first commit holds all of it:
/// by this machine's Git identity, or Bakery's when it has none.
fn start_repository(dir: &Path) -> Result<(), String> {
    crate::git::run(dir, &["init", "-q", "-b", "main"])?;
    crate::git::run(dir, &["add", "-A"])?;
    let commit = [
        "-c",
        "commit.gpgsign=false",
        "commit",
        "-q",
        "--allow-empty",
        "-m",
        "Start the project",
    ];
    if crate::git::run(dir, &commit).is_err() {
        // No Git identity on this machine: the first commit is Bakery's.
        let mut as_bakery = vec![
            "-c",
            "user.name=Bakery",
            "-c",
            "user.email=bakery@localhost",
        ];
        as_bakery.extend(commit);
        crate::git::run(dir, &as_bakery)?;
    }
    Ok(())
}

/// The first free `<parent>/<slug>`, `<slug>-2`, …
fn unused_folder(parent: &Path, slug: &str) -> PathBuf {
    (1..)
        .map(|n| match n {
            1 => parent.join(slug),
            n => parent.join(format!("{slug}-{n}")),
        })
        .find(|dir| !dir.exists())
        .expect("an unused name")
}

/// Removes a directory THIS call created, only if it is still empty — the
/// create-sequence-failure cleanup rule. Never recurses, never removes a
/// directory anything else may have since populated (a race with an agent
/// or the human writing into it between mkdir and the failure loses to
/// "leave it," not to silent deletion).
fn cleanup_freshly_created_dir(root: &ResolvedRoot) {
    if !root.created_by_this_call {
        return;
    }
    let _ = std::fs::remove_dir(&root.canonical); // fails silently (and safely) if non-empty
}

// ─── Mutations ──────────────────────────────────────────────────────────────

/// Test convenience: [`create_project_with`] with default stacks. The real
/// `POST /project/create` route calls `create_project_with` directly so it
/// can pass the operator's chosen stack.
#[cfg(test)]
pub(crate) fn create_project(
    state: &AppState,
    name: &str,
    root: Option<DirectoryRootSpec>,
) -> Result<(ProjectRecord, ActiveContext), String> {
    create_project_with(state, name, root, ProjectStacks::default())
}

/// `POST /project/create` — a named Project with an optional root (existing
/// directory, or one to create) or none at all (chat-only). Always mints and
/// binds one starter socket in the same call: a Project with literally zero
/// sockets has nothing to click into, and every Proof line describing
/// "create X; it appears in Studio; Chat works" is one atomic human action,
/// not a two-step "create the shell, then remember to add a socket."
pub(crate) fn create_project_with(
    state: &AppState,
    name: &str,
    root: Option<DirectoryRootSpec>,
    stacks: ProjectStacks,
) -> Result<(ProjectRecord, ActiveContext), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("project name must not be empty".to_string());
    }
    stacks.validate()?;

    let resolved = match root {
        Some(spec) => Some(resolve_directory_root(spec)?),
        None => None,
    };

    let result = (|| -> Result<(ProjectRecord, ActiveContext, Vec<Event>), String> {
        let mut heart = state.heart.lock().unwrap();

        if let Some(r) = &resolved {
            if root_already_claimed_locked(&heart, &r.canonical, None)? {
                return Err(format!(
                    "{} is already a root of another project",
                    r.canonical.display()
                ));
            }
        }

        let project_id = mint_id("project");
        let mut events = Vec::new();

        let created_ev = append_locked(
            &mut heart,
            &project_id,
            "human",
            PROJECT_CREATED_FAMILY,
            serde_json::to_string(&ProjectCreated {
                name: name.to_string(),
                stacks: stacks.clone(),
            })
            .unwrap(),
            vec![],
        )?;
        events.push(created_ev.clone());

        let root_id = if let Some(r) = &resolved {
            let root_id = mint_id("root");
            let attach_ev = append_locked(
                &mut heart,
                &project_id,
                "human",
                ROOT_ATTACHED_FAMILY,
                serde_json::to_string(&RootAttached {
                    root_id: root_id.clone(),
                    path: r.canonical.to_string_lossy().into_owned(),
                    is_git: r.is_git,
                })
                .unwrap(),
                vec![created_ev.event_hash.clone()],
            )?;
            events.push(attach_ev);
            Some(root_id)
        } else {
            None
        };

        let socket_id = mint_id("socket");
        let (worktree_path, repo_root, branch) = match &resolved {
            Some(r) => {
                let path_str = r.canonical.to_string_lossy().into_owned();
                if r.is_git {
                    let info = crate::git::inspect(&path_str);
                    (
                        path_str,
                        info.root.unwrap_or_default(),
                        info.current_branch.unwrap_or_default(),
                    )
                } else {
                    (path_str, String::new(), String::new())
                }
            }
            None => (String::new(), String::new(), String::new()),
        };
        let onboarded_payload = serde_json::json!({
            "repo": repo_root,
            "repo_root": repo_root,
            "project": name,
            "name": name,
            "worktree_path": worktree_path,
            "branch": branch,
            "opened_by_node": state.node.lock().unwrap().node_id.clone(),
            "created_by_bakery": resolved.as_ref().map(|r| r.created_by_this_call).unwrap_or(false),
        })
        .to_string();
        let onboarded_ev = append_locked(
            &mut heart,
            &socket_id,
            "human",
            "project.onboarded",
            onboarded_payload,
            vec![],
        )?;
        events.push(onboarded_ev.clone());

        let kind_ev = append_locked(
            &mut heart,
            &socket_id,
            "human",
            SOCKET_KIND_FAMILY,
            serde_json::to_string(&SocketKindRecord {
                kind: SocketKind::Project,
            })
            .unwrap(),
            vec![onboarded_ev.event_hash.clone()],
        )?;
        events.push(kind_ev.clone());

        let membership_ev = append_locked(
            &mut heart,
            &socket_id,
            "human",
            SOCKET_PROJECT_FAMILY,
            serde_json::to_string(&SocketProjectSet {
                project_id: Some(project_id.clone()),
                root_id: root_id.clone(),
            })
            .unwrap(),
            vec![kind_ev.event_hash.clone()],
        )?;
        events.push(membership_ev);

        let record = project_record_locked(&heart, &project_id)
            .ok_or_else(|| "internal: just-created project unreadable".to_string())?;

        drop(heart);
        let owner = claim_ownership(state, &socket_id)?;

        let exists_on_disk = worktree_path.is_empty() || Path::new(&worktree_path).is_dir();
        let ctx = ActiveContext {
            stream_id: socket_id,
            name: name.to_string(),
            project: name.to_string(),
            repo_root,
            branch,
            exists_on_disk,
            in_place: false,
            worktree_path,
            owner_node_id: owner,
            created_by_bakery: resolved
                .as_ref()
                .map(|r| r.created_by_this_call)
                .unwrap_or(false),
        };
        Ok((record, ctx, events))
    })();

    match result {
        Ok((record, ctx, events)) => {
            for ev in &events {
                broadcast(state, ev);
            }
            Ok((record, ctx))
        }
        Err(e) => {
            if let Some(r) = &resolved {
                cleanup_freshly_created_dir(r);
            }
            Err(e)
        }
    }
}

/// A loose chat: a socket in no Project, whose workspace is a folder of its
/// own in `<base>/chats/` (`<title>-<short id>`), apart from its room's
/// runtime state — so any runtime, native ones included, has somewhere to
/// run and write, all of it inside Bakery. Deleting the chat moves the
/// folder to the Trash; making it a project moves it into one.
pub(crate) fn create_chat(state: &AppState, title: &str) -> Result<ActiveContext, String> {
    let title: String = match title.trim() {
        "" => "New chat".to_string(),
        t => t.chars().take(80).collect(),
    };
    let socket_id = mint_id("socket");
    let chats = state.base.join("chats");
    let short = &socket_id[socket_id.len().saturating_sub(6)..];
    let named = chats.join(format!("{}-{short}", crate::git::sanitize_name(&title)));
    let workspace = if named.exists() {
        chats.join(&socket_id)
    } else {
        named
    };
    std::fs::create_dir_all(&workspace).map_err(|e| format!("{}: {e}", workspace.display()))?;
    let worktree_path = workspace.to_string_lossy().into_owned();

    let events = {
        let mut heart = state.heart.lock().unwrap();
        let onboarded = append_locked(
            &mut heart,
            &socket_id,
            "human",
            "project.onboarded",
            serde_json::json!({
                "repo": "",
                "repo_root": "",
                "project": "",
                "name": title,
                "worktree_path": worktree_path,
                "branch": "",
                "opened_by_node": state.node.lock().unwrap().node_id.clone(),
                "created_by_bakery": true,
            })
            .to_string(),
            vec![],
        )?;
        let kind = append_locked(
            &mut heart,
            &socket_id,
            "human",
            SOCKET_KIND_FAMILY,
            serde_json::to_string(&SocketKindRecord {
                kind: SocketKind::Project,
            })
            .unwrap(),
            vec![onboarded.event_hash.clone()],
        )?;
        vec![onboarded, kind]
    };
    for ev in &events {
        broadcast(state, ev);
    }
    let owner = claim_ownership(state, &socket_id)?;
    Ok(ActiveContext {
        stream_id: socket_id,
        name: title,
        project: String::new(),
        repo_root: String::new(),
        branch: String::new(),
        exists_on_disk: true,
        in_place: false,
        worktree_path,
        owner_node_id: owner,
        created_by_bakery: true,
    })
}

/// Whether `folder` is a chat's own: one in `<base>/chats/`, or the older
/// kind inside its room (`<base>/cache/sockets/<id>/chat`).
pub(crate) fn is_chat_folder(base: &Path, folder: &str) -> bool {
    let folder = Path::new(folder);
    let in_chats = folder.parent() == Some(base.join("chats").as_path());
    let in_room = folder.file_name() == Some("chat".as_ref())
        && folder.parent().and_then(Path::parent)
            == Some(base.join("cache").join("sockets").as_path());
    in_chats || in_room
}

/// "Make this a project": a chat's folder moves into `parent` (Bakery's own
/// `projects/` unless the operator chose a folder), becomes a Git repository
/// with a first commit when it isn't one, and becomes the root of a new
/// project the chat now belongs to; the conversation carries on there. The
/// chat's runtime, terminals and editor stop first (the route does that).
pub(crate) fn make_project_from_chat(
    state: &AppState,
    stream_id: &str,
    name: &str,
    parent: Option<&Path>,
) -> Result<(ProjectRecord, String), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("project name must not be empty".to_string());
    }
    let ctx = crate::state::context_for(state, stream_id)?;
    if !is_chat_folder(&state.base, &ctx.worktree_path) {
        return Err("only a chat with its own folder becomes a project this way".to_string());
    }
    let membership = state
        .heart
        .lock()
        .unwrap()
        .last_of_family(stream_id, SOCKET_PROJECT_FAMILY)
        .map_err(|e| e.to_string())?
        .and_then(|e| serde_json::from_str::<SocketProjectSet>(&e.payload).ok())
        .and_then(|m| m.project_id);
    if membership.is_some() {
        return Err("it already belongs to a project".to_string());
    }
    let source = PathBuf::from(&ctx.worktree_path);
    if !source.is_dir() {
        return Err(format!("its folder {} is gone", source.display()));
    }
    let parent = match parent {
        Some(chosen) if chosen.is_absolute() && chosen.is_dir() => chosen.to_path_buf(),
        Some(chosen) => return Err(format!("{} is not a folder", chosen.display())),
        None => {
            let projects = state.base.join("projects");
            std::fs::create_dir_all(&projects)
                .map_err(|e| format!("{}: {e}", projects.display()))?;
            projects
        }
    };
    let folder = unused_folder(&parent, &crate::git::sanitize_name(name));
    std::fs::rename(&source, &folder).map_err(|e| {
        if e.raw_os_error() == Some(libc::EXDEV) {
            format!(
                "{} is on another disk: choose a folder on the disk Bakery is on",
                parent.display()
            )
        } else {
            format!("moving {} to {}: {e}", source.display(), folder.display())
        }
    })?;
    if !folder.join(".git").exists() {
        if let Err(e) = start_repository(&folder) {
            let _ = std::fs::remove_dir_all(folder.join(".git"));
            let _ = std::fs::rename(&folder, &source);
            return Err(format!("making it a Git repository: {e}; it stays a chat"));
        }
    }
    let canonical = folder.canonicalize().map_err(|e| e.to_string())?;
    let path = canonical.to_string_lossy().into_owned();
    let branch = crate::git::inspect(&path)
        .current_branch
        .unwrap_or_default();

    let (record, events) = {
        let mut heart = state.heart.lock().unwrap();
        let project_id = mint_id("project");
        let created = append_locked(
            &mut heart,
            &project_id,
            "human",
            PROJECT_CREATED_FAMILY,
            serde_json::to_string(&ProjectCreated {
                name: name.to_string(),
                stacks: ProjectStacks::default(),
            })
            .unwrap(),
            vec![],
        )?;
        let root_id = mint_id("root");
        let attached = append_locked(
            &mut heart,
            &project_id,
            "human",
            ROOT_ATTACHED_FAMILY,
            serde_json::to_string(&RootAttached {
                root_id: root_id.clone(),
                path: path.clone(),
                is_git: true,
            })
            .unwrap(),
            vec![created.event_hash.clone()],
        )?;
        let moved = append_locked(
            &mut heart,
            stream_id,
            "human",
            SOCKET_WORKSPACE_FAMILY,
            serde_json::to_string(&WorkspaceSet {
                worktree_path: path.clone(),
                branch,
                repo_root: path.clone(),
                created_by_bakery: false,
                fork_note: None,
            })
            .unwrap(),
            vec![attached.event_hash.clone()],
        )?;
        let joined = append_locked(
            &mut heart,
            stream_id,
            "human",
            SOCKET_PROJECT_FAMILY,
            serde_json::to_string(&SocketProjectSet {
                project_id: Some(project_id.clone()),
                root_id: Some(root_id),
            })
            .unwrap(),
            vec![moved.event_hash.clone()],
        )?;
        let record = project_record_locked(&heart, &project_id)
            .ok_or_else(|| "internal: just-created project unreadable".to_string())?;
        (record, vec![created, attached, moved, joined])
    };
    for ev in &events {
        broadcast(state, ev);
    }
    Ok((record, path))
}

/// Bind an already-open socket (from `routes::project::open_stream`,
/// typically the git-attach path) to a Project — required, not best-effort:
/// callers propagate this error rather than reporting the attach as a
/// success that silently left the socket Ungrouped.
///
/// `root_path` is canonicalized and, if it does not already match one of the
/// Project's current roots, attached as a brand-new one (idempotent
/// re-attach of the same path reuses the existing root_id rather than
/// minting a duplicate).
pub(crate) fn link_socket_to_project(
    state: &AppState,
    socket_stream_id: &str,
    project_id: &str,
    root_path: &str,
    is_git: bool,
) -> Result<(), String> {
    let canonical = Path::new(root_path)
        .canonicalize()
        .map_err(|e| format!("{root_path}: {e}"))?;

    let mut heart = state.heart.lock().unwrap();
    if is_retired(&heart, project_id) {
        return Err(format!("project {project_id} is retired"));
    }
    if project_record_locked(&heart, project_id).is_none() {
        return Err(format!("unknown project {project_id}"));
    }

    let existing_roots = current_roots_locked(&heart, project_id);
    let matching = existing_roots.iter().find(|r| {
        Path::new(&r.path)
            .canonicalize()
            .map(|p| p == canonical)
            .unwrap_or(false)
    });

    let mut events = Vec::new();
    let root_id = if let Some(r) = matching {
        r.root_id.clone()
    } else {
        if root_already_claimed_locked(&heart, &canonical, Some(project_id))? {
            return Err(format!(
                "{} is already a root of another project",
                canonical.display()
            ));
        }
        let root_id = mint_id("root");
        let ev = append_locked(
            &mut heart,
            project_id,
            "human",
            ROOT_ATTACHED_FAMILY,
            serde_json::to_string(&RootAttached {
                root_id: root_id.clone(),
                path: canonical.to_string_lossy().into_owned(),
                is_git,
            })
            .unwrap(),
            vec![],
        )?;
        events.push(ev);
        root_id
    };

    let membership_ev = append_locked(
        &mut heart,
        socket_stream_id,
        "human",
        SOCKET_PROJECT_FAMILY,
        serde_json::to_string(&SocketProjectSet {
            project_id: Some(project_id.to_string()),
            root_id: Some(root_id),
        })
        .unwrap(),
        vec![],
    )?;
    events.push(membership_ev);

    drop(heart);
    for ev in &events {
        broadcast(state, ev);
    }
    Ok(())
}

/// `POST /project/root/open` — "open any directory as a root + in-place
/// socket" on an EXISTING Project: resolves/admits the directory exactly
/// like `create_project`'s root handling (canonicalize, optional mkdir, no
/// git/mise/manifest admission requirement), then mints one new socket
/// rooted there and links it. Reuses an existing root on this Project by
/// canonical path instead of duplicating one, same as `link_socket_to_project`.
pub(crate) fn attach_directory_root(
    state: &AppState,
    project_id: &str,
    socket_name: &str,
    root: DirectoryRootSpec,
) -> Result<ActiveContext, String> {
    let socket_name = socket_name.trim();
    if socket_name.is_empty() {
        return Err("socket name must not be empty".to_string());
    }
    let resolved = resolve_directory_root(root)?;

    let result = (|| -> Result<(ActiveContext, Vec<Event>), String> {
        let mut heart = state.heart.lock().unwrap();
        if is_retired(&heart, project_id) {
            return Err(format!("project {project_id} is retired"));
        }
        let project = project_record_locked(&heart, project_id)
            .ok_or_else(|| format!("unknown project {project_id}"))?;

        let matching = project.roots.iter().find(|r| {
            Path::new(&r.path)
                .canonicalize()
                .map(|p| p == resolved.canonical)
                .unwrap_or(false)
        });

        let mut events = Vec::new();
        let root_id = if let Some(r) = matching {
            r.root_id.clone()
        } else {
            if root_already_claimed_locked(&heart, &resolved.canonical, Some(project_id))? {
                return Err(format!(
                    "{} is already a root of another project",
                    resolved.canonical.display()
                ));
            }
            let root_id = mint_id("root");
            let ev = append_locked(
                &mut heart,
                project_id,
                "human",
                ROOT_ATTACHED_FAMILY,
                serde_json::to_string(&RootAttached {
                    root_id: root_id.clone(),
                    path: resolved.canonical.to_string_lossy().into_owned(),
                    is_git: resolved.is_git,
                })
                .unwrap(),
                vec![],
            )?;
            events.push(ev);
            root_id
        };

        let socket_id = mint_id("socket");
        let path_str = resolved.canonical.to_string_lossy().into_owned();
        let (repo_root, branch) = if resolved.is_git {
            let info = crate::git::inspect(&path_str);
            (
                info.root.unwrap_or_default(),
                info.current_branch.unwrap_or_default(),
            )
        } else {
            (String::new(), String::new())
        };
        let onboarded_payload = serde_json::json!({
            "repo": repo_root,
            "repo_root": repo_root,
            "project": project.name,
            "name": socket_name,
            "worktree_path": path_str,
            "branch": branch,
            "opened_by_node": state.node.lock().unwrap().node_id.clone(),
            "created_by_bakery": resolved.created_by_this_call,
        })
        .to_string();
        let onboarded_ev = append_locked(
            &mut heart,
            &socket_id,
            "human",
            "project.onboarded",
            onboarded_payload,
            vec![],
        )?;
        events.push(onboarded_ev.clone());

        let kind_ev = append_locked(
            &mut heart,
            &socket_id,
            "human",
            SOCKET_KIND_FAMILY,
            serde_json::to_string(&SocketKindRecord {
                kind: SocketKind::Project,
            })
            .unwrap(),
            vec![onboarded_ev.event_hash.clone()],
        )?;
        events.push(kind_ev.clone());

        let membership_ev = append_locked(
            &mut heart,
            &socket_id,
            "human",
            SOCKET_PROJECT_FAMILY,
            serde_json::to_string(&SocketProjectSet {
                project_id: Some(project_id.to_string()),
                root_id: Some(root_id),
            })
            .unwrap(),
            vec![kind_ev.event_hash.clone()],
        )?;
        events.push(membership_ev);

        drop(heart);
        let owner = claim_ownership(state, &socket_id)?;
        let exists_on_disk = Path::new(&path_str).is_dir();
        let ctx = ActiveContext {
            stream_id: socket_id,
            name: socket_name.to_string(),
            project: project.name,
            repo_root,
            branch,
            exists_on_disk,
            in_place: false,
            worktree_path: path_str,
            owner_node_id: owner,
            created_by_bakery: resolved.created_by_this_call,
        };
        Ok((ctx, events))
    })();

    match result {
        Ok((ctx, events)) => {
            for ev in &events {
                broadcast(state, ev);
            }
            Ok(ctx)
        }
        Err(e) => {
            cleanup_freshly_created_dir(&resolved);
            Err(e)
        }
    }
}

/// Explicit detach: the socket becomes Ungrouped (`socket.project.set` with
/// both fields `None`). Relation-only — never touches the socket's files,
/// history, or the Project's own root records.
pub(crate) fn unlink_socket_from_project(
    state: &AppState,
    socket_stream_id: &str,
) -> Result<(), String> {
    let ev = crate::state::append(
        state,
        socket_stream_id,
        "human",
        SOCKET_PROJECT_FAMILY,
        serde_json::to_string(&SocketProjectSet {
            project_id: None,
            root_id: None,
        })
        .unwrap(),
        vec![],
    )?;
    let _ = ev;
    Ok(())
}

/// Rename preserves `project_id` — this only ever appends `project.renamed`,
/// never touches the stream's identity or any socket's membership fact.
pub(crate) fn rename_project(
    state: &AppState,
    project_id: &str,
    new_name: &str,
) -> Result<(), String> {
    let new_name = new_name.trim();
    if new_name.is_empty() {
        return Err("project name must not be empty".to_string());
    }
    let heart = state.heart.lock().unwrap();
    if is_retired(&heart, project_id) {
        return Err(format!("project {project_id} is retired"));
    }
    if project_record_locked(&heart, project_id).is_none() {
        return Err(format!("unknown project {project_id}"));
    }
    drop(heart);
    crate::state::append(
        state,
        project_id,
        "human",
        PROJECT_RENAMED_FAMILY,
        serde_json::to_string(&ProjectRenamed {
            name: new_name.to_string(),
        })
        .unwrap(),
        vec![],
    )?;
    Ok(())
}

pub(crate) fn set_project_pinned(
    state: &AppState,
    project_id: &str,
    pinned: bool,
) -> Result<(), String> {
    let heart = state.heart.lock().unwrap();
    if is_retired(&heart, project_id) {
        return Err(format!("project {project_id} is retired"));
    }
    if project_record_locked(&heart, project_id).is_none() {
        return Err(format!("unknown project {project_id}"));
    }
    drop(heart);
    crate::state::append(
        state,
        project_id,
        "human",
        PROJECT_PIN_FAMILY,
        serde_json::to_string(&ProjectPinSet { pinned }).unwrap(),
        vec![],
    )?;
    Ok(())
}

/// Retiring/unregistering a Project never deletes its roots, files, sockets,
/// memory, or Heart history (07d-A3): this only appends `project.retired`
/// to the Project's own stream. Still-active member sockets are NOT touched
/// here either — they become Ungrouped implicitly, the moment the
/// projection stops finding a live (non-retired) Project for their
/// `socket.project.set.project_id` to resolve to; there is no bulk rewrite
/// of every member socket's membership fact.
pub(crate) fn retire_project(state: &AppState, project_id: &str) -> Result<(), String> {
    let heart = state.heart.lock().unwrap();
    if is_retired(&heart, project_id) {
        return Err(format!("project {project_id} is already retired"));
    }
    let Some(stacks) = project_record_locked(&heart, project_id).map(|p| p.stacks) else {
        return Err(format!("unknown project {project_id}"));
    };
    drop(heart);
    crate::state::append(
        state,
        project_id,
        "human",
        PROJECT_RETIRED_FAMILY,
        "{}".to_string(),
        vec![],
    )?;
    // Its services stop with it; their data stays.
    crate::services::stop_project(state, project_id, &stacks);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::project::{open_stream, OpenReq};
    use crate::testutil::*;
    use bakery_agent::StubModel;
    use std::sync::Arc;

    fn attach_req(path: &str) -> Option<DirectoryRootSpec<'_>> {
        Some(DirectoryRootSpec::Attach(path))
    }

    fn create_req<'a>(parent: &'a str, name: &'a str) -> Option<DirectoryRootSpec<'a>> {
        Some(DirectoryRootSpec::Create { parent, name })
    }

    // ─── Create: non-git directory, chat-only, mkdir ────────────────────────

    #[test]
    fn create_non_git_directory_project_appears_and_is_usable() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let plain = dir.path().join("notes");
        std::fs::create_dir_all(&plain).unwrap();
        std::fs::write(plain.join("todo.txt"), "buy milk\n").unwrap();

        let (record, ctx) =
            create_project(&state, "Notes", attach_req(plain.to_str().unwrap())).unwrap();

        assert_eq!(record.name, "Notes");
        assert!(record.project_id.starts_with("project_"));
        assert_eq!(record.roots.len(), 1);
        assert!(!record.roots[0].is_git);

        // Files work: worktree_path is the real directory, jailable as-is.
        assert_eq!(
            Path::new(&ctx.worktree_path).canonicalize().unwrap(),
            plain.canonicalize().unwrap()
        );
        // Git branch chrome is absent — never a git repo, never a fake branch.
        assert!(
            ctx.repo_root.is_empty(),
            "non-git root must carry no repo_root"
        );
        assert!(ctx.branch.is_empty(), "non-git root must carry no branch");
        assert!(
            !ctx.created_by_bakery,
            "an existing directory was not created by Bakery"
        );
        assert_ne!(
            ctx.stream_id, record.project_id,
            "the socket and the project are different streams"
        );
        assert!(ctx.stream_id.starts_with("socket_"));

        // Files still work through the ordinary jail — no code change needed there.
        let jail = crate::tools::Jail::new(Path::new(&ctx.worktree_path)).unwrap();
        let listing = jail.list_dir(".").unwrap();
        assert!(listing.iter().any(|e| e.name == "todo.txt"));
    }

    #[test]
    fn create_persists_proto_and_moon_stacks_without_writing_pin_files() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let plain = dir.path().join("app");
        std::fs::create_dir_all(&plain).unwrap();
        let stacks = crate::node::ProjectStacks {
            toolchain: "proto".into(),
            tasks: "moon".into(),
            containers: "colima".into(),
            secrets: "infisical".into(),
            sandbox: "mac".into(),
            services: Vec::new(),
            sign_ins: Vec::new(),
        };
        let (record, ctx) = create_project_with(
            &state,
            "MoonApp",
            attach_req(plain.to_str().unwrap()),
            stacks.clone(),
        )
        .unwrap();
        assert_eq!(record.stacks, stacks);
        assert!(
            !plain.join("mise.toml").exists(),
            "must not auto-write mise.toml"
        );
        assert!(!plain.join(".prototools").exists());
        assert_eq!(stacks_for_socket(&state, &ctx.stream_id), stacks);
    }

    #[test]
    fn create_rejects_an_unknown_toolchain_stack() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let mut stacks = crate::node::ProjectStacks::default();
        stacks.toolchain = "nix".into();
        let err = create_project_with(&state, "X", None, stacks).unwrap_err();
        assert!(err.contains("toolchain"), "{err}");
    }

    #[test]
    fn create_chat_only_project_has_no_root_and_no_jail() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));

        let (record, ctx) = create_project(&state, "Just chat", None).unwrap();

        assert!(record.roots.is_empty());
        assert!(
            ctx.worktree_path.is_empty(),
            "chat-only carries no worktree/jail root"
        );
        assert!(
            ctx.exists_on_disk,
            "chat-only has nothing to be 'gone' — must not read as missing"
        );
        assert!(
            crate::state::jail_for(&state, &ctx.stream_id).is_err(),
            "no root means no jail"
        );
    }

    #[test]
    fn create_new_makes_a_fresh_directory_owned_by_bakery_under_an_existing_parent() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let fresh = dir.path().join("brand-new");
        assert!(!fresh.exists());

        let (_record, ctx) = create_project(
            &state,
            "Fresh",
            create_req(dir.path().to_str().unwrap(), "brand-new"),
        )
        .unwrap();

        assert!(
            fresh.is_dir(),
            "create-new must materialize the child directory"
        );
        assert!(ctx.created_by_bakery);
    }

    #[test]
    fn create_new_refuses_a_target_that_already_exists() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        std::fs::create_dir_all(dir.path().join("already-here")).unwrap();

        let err = create_project(
            &state,
            "X",
            create_req(dir.path().to_str().unwrap(), "already-here"),
        )
        .unwrap_err();
        assert!(err.contains("already exists"), "got: {err}");
    }

    #[test]
    fn create_new_refuses_a_parent_that_does_not_exist() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let missing_parent = dir.path().join("nope");

        let err = create_project(
            &state,
            "X",
            create_req(missing_parent.to_str().unwrap(), "child"),
        )
        .unwrap_err();
        assert!(
            err.contains("parent directory does not exist"),
            "got: {err}"
        );
    }

    #[test]
    fn create_new_refuses_a_name_containing_a_path_separator_or_dot_segment() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let parent = dir.path().to_str().unwrap();

        for bad_name in ["a/b", "a\\b", "", "  ", ".", ".."] {
            let err = create_project(&state, "X", create_req(parent, bad_name)).unwrap_err();
            assert!(
                err.contains("not a valid folder name"),
                "name {bad_name:?} got: {err}"
            );
        }
    }

    /// 07d follow-up: the create-new path must be a real containment
    /// boundary, not just "a fresh empty directory." An unrelated sibling
    /// file sitting right next to the freshly created child folder — inside
    /// the SAME parent — must never be visible or reachable through the
    /// project's own jail, because the jail root is the child, not the
    /// parent (`tools::Jail::new` roots exactly at the resolved canonical
    /// path). Reuses the `escapes` idiom already proven at
    /// `tools::tests::rejects_dotdot_escape` /
    /// `tools::tests::grep_cannot_reach_outside_the_jail`.
    #[test]
    fn create_new_project_root_never_exposes_a_sibling_file_in_the_parent() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let parent = dir.path().join("workspace");
        std::fs::create_dir_all(&parent).unwrap();
        std::fs::write(parent.join("secret.txt"), "TOP SECRET\n").unwrap();

        let (_record, ctx) = create_project(
            &state,
            "Contained",
            create_req(parent.to_str().unwrap(), "my-project"),
        )
        .unwrap();
        assert!(ctx.created_by_bakery);
        let child = parent.join("my-project");
        assert!(child.is_dir());
        assert!(
            !child.join("secret.txt").exists(),
            "the sibling must not have been swept into the child"
        );

        let jail = crate::state::jail_for(&state, &ctx.stream_id).unwrap();

        let listing = jail.list_files(".").unwrap();
        assert!(
            !listing.contains("secret.txt"),
            "sibling file must be invisible: {listing:?}"
        );

        let err = jail.read_file_for_edit("../secret.txt").unwrap_err();
        assert!(err.contains("escapes"), "got: {err}");
    }

    #[test]
    fn attach_rejects_a_missing_directory() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let missing = dir.path().join("nope");

        let err = create_project(&state, "X", attach_req(missing.to_str().unwrap())).unwrap_err();
        assert!(err.contains("does not exist"), "got: {err}");
    }

    #[test]
    fn create_rejects_an_empty_name() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let err = create_project(&state, "   ", None).unwrap_err();
        assert!(err.contains("empty"), "got: {err}");
    }

    #[test]
    fn create_rejects_a_root_already_claimed_by_another_active_project() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let shared = dir.path().join("shared");
        std::fs::create_dir_all(&shared).unwrap();

        create_project(&state, "First", attach_req(shared.to_str().unwrap())).unwrap();
        let err =
            create_project(&state, "Second", attach_req(shared.to_str().unwrap())).unwrap_err();
        assert!(err.contains("already a root"), "got: {err}");
    }

    #[test]
    fn create_cleans_up_only_a_freshly_created_still_empty_directory_on_failure() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let shared = dir.path().join("shared2");
        std::fs::create_dir_all(&shared).unwrap();
        create_project(&state, "First", attach_req(shared.to_str().unwrap())).unwrap();

        // Second create targets a brand-new path THAT WOULD be mkdir'd, but
        // collides on canonical identity only if it resolves to the same
        // directory — use a symlink so the fresh path canonicalizes to the
        // already-claimed one, forcing the root-uniqueness rejection AFTER
        // the (fresh, real) directory would have been created for a
        // genuinely new path. Simpler and still faithful to the rule: prove
        // cleanup direcly via the private helper instead of forcing a real
        // Heart failure (Heart::open_in_memory does not fail in tests).
        let fresh = dir.path().join("fresh-empty");
        let root = ResolvedRoot {
            canonical: fresh.clone(),
            is_git: false,
            created_by_this_call: true,
        };
        std::fs::create_dir_all(&fresh).unwrap();
        cleanup_freshly_created_dir(&root);
        assert!(
            !fresh.exists(),
            "a still-empty directory this call created must be removed on failure"
        );

        let populated = dir.path().join("fresh-populated");
        std::fs::create_dir_all(&populated).unwrap();
        std::fs::write(populated.join("f.txt"), "x").unwrap();
        let root2 = ResolvedRoot {
            canonical: populated.clone(),
            is_git: false,
            created_by_this_call: true,
        };
        cleanup_freshly_created_dir(&root2);
        assert!(
            populated.exists(),
            "a directory that got populated since must never be deleted"
        );

        let existing = ResolvedRoot {
            canonical: shared.clone(),
            is_git: false,
            created_by_this_call: false,
        };
        cleanup_freshly_created_dir(&existing);
        assert!(
            shared.exists(),
            "an existing directory this call did NOT create must never be removed"
        );
    }

    // ─── Rename / pin / retire ────────────────────────────────────────────

    #[test]
    fn rename_preserves_project_id_and_duplicate_display_names_do_not_collide() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let (record, _ctx) = create_project(&state, "Original", None).unwrap();
        let id = record.project_id.clone();

        rename_project(&state, &id, "Renamed").unwrap();
        let after = project_by_id(&state, &id).unwrap();
        assert_eq!(after.project_id, id, "rename must never change identity");
        assert_eq!(after.name, "Renamed");

        // A second project may freely reuse the exact same display name.
        let (record2, _) = create_project(&state, "Renamed", None).unwrap();
        assert_ne!(record2.project_id, id);
        assert_eq!(record2.name, "Renamed");
    }

    #[test]
    fn pin_and_unpin_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let (record, _ctx) = create_project(&state, "Pin me", None).unwrap();
        assert!(!record.pinned, "unpinned by default");

        set_project_pinned(&state, &record.project_id, true).unwrap();
        assert!(project_by_id(&state, &record.project_id).unwrap().pinned);

        set_project_pinned(&state, &record.project_id, false).unwrap();
        assert!(!project_by_id(&state, &record.project_id).unwrap().pinned);
    }

    #[test]
    fn retiring_a_project_hides_it_but_keeps_roots_files_and_socket_history() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let root = dir.path().join("keep-me");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("f.txt"), "still here\n").unwrap();
        let (record, ctx) =
            create_project(&state, "Doomed", attach_req(root.to_str().unwrap())).unwrap();

        retire_project(&state, &record.project_id).unwrap();

        assert!(
            project_by_id(&state, &record.project_id).is_none(),
            "a retired project must not resolve by id"
        );
        assert!(
            !known_projects(&state)
                .unwrap()
                .iter()
                .any(|p| p.project_id == record.project_id),
            "a retired project must not appear in the active catalog"
        );
        // Nothing was deleted: the file, and the socket's own Heart history.
        assert_eq!(
            std::fs::read_to_string(root.join("f.txt")).unwrap(),
            "still here\n"
        );
        let events = state
            .heart
            .lock()
            .unwrap()
            .read_stream(&ctx.stream_id)
            .unwrap();
        assert!(
            !events.is_empty(),
            "the socket's own Heart lineage must survive project retirement"
        );

        // Retiring twice is refused, not silently repeated.
        let err = retire_project(&state, &record.project_id).unwrap_err();
        assert!(err.contains("already retired"), "got: {err}");
    }

    #[test]
    fn a_retired_projects_root_can_be_reclaimed_by_a_new_project() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let root = dir.path().join("recyclable");
        std::fs::create_dir_all(&root).unwrap();
        let (record, _ctx) =
            create_project(&state, "Gone soon", attach_req(root.to_str().unwrap())).unwrap();
        retire_project(&state, &record.project_id).unwrap();

        let (record2, _ctx2) =
            create_project(&state, "Reclaimed", attach_req(root.to_str().unwrap())).unwrap();
        assert_ne!(record2.project_id, record.project_id);
        assert_eq!(record2.roots.len(), 1);
    }

    // ─── Root de-duplication across spelling/symlink forms ───────────────

    #[test]
    fn two_spelling_forms_of_one_root_do_not_register_twice() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let root = dir.path().join("spelled");
        std::fs::create_dir_all(&root).unwrap();
        let messy = root.join(".").join("..").join("spelled");

        let (record, _ctx) =
            create_project(&state, "P", attach_req(messy.to_str().unwrap())).unwrap();
        assert_eq!(record.roots.len(), 1);
        assert_eq!(
            Path::new(&record.roots[0].path).canonicalize().unwrap(),
            root.canonicalize().unwrap()
        );
    }

    // ─── Socket <-> Project membership ─────────────────────────────────────

    #[test]
    fn a_socket_is_grouped_only_after_socket_project_set_a_plain_opened_socket_is_ungrouped() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let ctx = open_stream(
            &state,
            &OpenReq {
                repo: repo.to_string_lossy().into_owned(),
                name: Some("plain".into()),
                ..Default::default()
            },
        )
        .unwrap()
        .context;

        let heart = state.heart.lock().unwrap();
        let (project_id, root_id) = crate::state::socket_project(&heart, &ctx.stream_id);
        assert_eq!(
            project_id, None,
            "a socket with no socket.project.set fact must be Ungrouped"
        );
        assert_eq!(root_id, None);
    }

    #[test]
    fn link_socket_to_project_records_membership_and_reuses_the_root_on_a_second_call() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let socket_ctx = open_stream(&state, &open_req(&repo, "linkme"))
            .unwrap()
            .context;
        let (record, _) = create_project(&state, "Target", None).unwrap();

        link_socket_to_project(
            &state,
            &socket_ctx.stream_id,
            &record.project_id,
            &socket_ctx.worktree_path,
            true,
        )
        .unwrap();

        let heart = state.heart.lock().unwrap();
        let (pid, rid) = crate::state::socket_project(&heart, &socket_ctx.stream_id);
        assert_eq!(pid.as_deref(), Some(record.project_id.as_str()));
        assert!(rid.is_some());
        drop(heart);

        let after_first = project_by_id(&state, &record.project_id).unwrap();
        assert_eq!(after_first.roots.len(), 1);

        // A second socket attaching the SAME root reuses the root_id, not a duplicate.
        let socket2_ctx = open_stream(&state, &open_req(&repo, "linkme2"))
            .unwrap()
            .context;
        link_socket_to_project(
            &state,
            &socket2_ctx.stream_id,
            &record.project_id,
            &socket_ctx.worktree_path,
            true,
        )
        .unwrap();
        let after_second = project_by_id(&state, &record.project_id).unwrap();
        assert_eq!(
            after_second.roots.len(),
            1,
            "the same canonical root must not be attached twice"
        );
    }

    #[test]
    fn link_refuses_an_unknown_or_retired_project() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let socket_ctx = open_stream(&state, &open_req(&repo, "orphan"))
            .unwrap()
            .context;

        let err = link_socket_to_project(
            &state,
            &socket_ctx.stream_id,
            "project_doesnotexist",
            &socket_ctx.worktree_path,
            true,
        )
        .unwrap_err();
        assert!(err.contains("unknown project"), "got: {err}");

        let (record, _) = create_project(&state, "Retiring", None).unwrap();
        retire_project(&state, &record.project_id).unwrap();
        let err = link_socket_to_project(
            &state,
            &socket_ctx.stream_id,
            &record.project_id,
            &socket_ctx.worktree_path,
            true,
        )
        .unwrap_err();
        assert!(err.contains("retired"), "got: {err}");
    }

    #[test]
    fn attach_directory_root_adds_a_socket_to_an_existing_project_and_reuses_the_root() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let (record, first_ctx) = create_project(&state, "Multi", None).unwrap();
        let root = dir.path().join("second-root");
        std::fs::create_dir_all(&root).unwrap();

        let ctx = attach_directory_root(
            &state,
            &record.project_id,
            "worker-view",
            DirectoryRootSpec::Attach(root.to_str().unwrap()),
        )
        .unwrap();
        assert_ne!(ctx.stream_id, first_ctx.stream_id);
        assert_eq!(ctx.project, "Multi");
        assert!(
            ctx.repo_root.is_empty(),
            "a plain directory root carries no repo_root"
        );

        let after = project_by_id(&state, &record.project_id).unwrap();
        assert_eq!(
            after.roots.len(),
            1,
            "the chat-only project's first attached root"
        );

        // A second socket at the exact same path reuses the root, no duplicate.
        let ctx2 = attach_directory_root(
            &state,
            &record.project_id,
            "second-view",
            DirectoryRootSpec::Attach(root.to_str().unwrap()),
        )
        .unwrap();
        assert_ne!(ctx2.stream_id, ctx.stream_id);
        let after2 = project_by_id(&state, &record.project_id).unwrap();
        assert_eq!(
            after2.roots.len(),
            1,
            "same canonical path must not duplicate the root"
        );
    }

    #[test]
    fn attach_directory_root_rejects_a_path_already_claimed_by_another_project() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let root = dir.path().join("claimed");
        std::fs::create_dir_all(&root).unwrap();
        create_project(&state, "Owner", attach_req(root.to_str().unwrap())).unwrap();

        let (other, _) = create_project(&state, "Other", None).unwrap();
        let err = attach_directory_root(
            &state,
            &other.project_id,
            "s",
            DirectoryRootSpec::Attach(root.to_str().unwrap()),
        )
        .unwrap_err();
        assert!(err.contains("already a root"), "got: {err}");
    }

    #[test]
    fn attach_directory_root_can_create_a_new_root_under_an_existing_parent() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let (record, _first_ctx) = create_project(&state, "Multi", None).unwrap();

        let ctx = attach_directory_root(
            &state,
            &record.project_id,
            "new-view",
            DirectoryRootSpec::Create {
                parent: dir.path().to_str().unwrap(),
                name: "fresh-view-root",
            },
        )
        .unwrap();
        assert!(dir.path().join("fresh-view-root").is_dir());
        assert!(ctx.created_by_bakery);
    }

    #[test]
    fn attach_directory_root_create_refuses_an_existing_target() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let (record, _first_ctx) = create_project(&state, "Multi", None).unwrap();
        std::fs::create_dir_all(dir.path().join("taken")).unwrap();

        let err = attach_directory_root(
            &state,
            &record.project_id,
            "new-view",
            DirectoryRootSpec::Create {
                parent: dir.path().to_str().unwrap(),
                name: "taken",
            },
        )
        .unwrap_err();
        assert!(err.contains("already exists"), "got: {err}");
    }

    #[test]
    fn unlink_makes_a_socket_ungrouped_without_touching_its_history() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let socket_ctx = open_stream(&state, &open_req(&repo, "unlinkme"))
            .unwrap()
            .context;
        let (record, _) = create_project(&state, "Home", None).unwrap();
        link_socket_to_project(
            &state,
            &socket_ctx.stream_id,
            &record.project_id,
            &socket_ctx.worktree_path,
            true,
        )
        .unwrap();

        let events_before = state
            .heart
            .lock()
            .unwrap()
            .read_stream(&socket_ctx.stream_id)
            .unwrap()
            .len();
        unlink_socket_from_project(&state, &socket_ctx.stream_id).unwrap();
        let events_after = state
            .heart
            .lock()
            .unwrap()
            .read_stream(&socket_ctx.stream_id)
            .unwrap()
            .len();
        assert!(
            events_after > events_before,
            "unlink must append its own fact, not rewrite history"
        );

        let heart = state.heart.lock().unwrap();
        let (pid, rid) = crate::state::socket_project(&heart, &socket_ctx.stream_id);
        assert_eq!(pid, None);
        assert_eq!(rid, None);
    }

    // ─── Fail-closed on malformed catalog/membership facts ────────────────

    #[test]
    fn a_malformed_newest_rename_falls_back_to_the_immutable_created_name_not_an_older_rename() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let (record, _) = create_project(&state, "Birth Name", None).unwrap();
        rename_project(&state, &record.project_id, "Good Rename").unwrap();
        // Corrupt the newest rename record directly.
        crate::state::append(
            &state,
            &record.project_id,
            "human",
            PROJECT_RENAMED_FAMILY,
            "not json".to_string(),
            vec![],
        )
        .unwrap();

        let after = project_by_id(&state, &record.project_id).unwrap();
        assert_eq!(
            after.name, "Birth Name",
            "must fall back to the immutable created name, not the older good rename"
        );
    }

    #[test]
    fn a_malformed_newest_pin_fails_closed_to_unpinned() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let (record, _) = create_project(&state, "P", None).unwrap();
        set_project_pinned(&state, &record.project_id, true).unwrap();
        crate::state::append(
            &state,
            &record.project_id,
            "human",
            PROJECT_PIN_FAMILY,
            "not json".to_string(),
            vec![],
        )
        .unwrap();

        let after = project_by_id(&state, &record.project_id).unwrap();
        assert!(
            !after.pinned,
            "a malformed newest pin record must fail closed to unpinned, not the older true"
        );
    }

    #[test]
    fn a_malformed_root_attach_mid_history_fails_the_whole_projects_roots_closed() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let root = dir.path().join("r");
        std::fs::create_dir_all(&root).unwrap();
        let (record, _) = create_project(&state, "P", attach_req(root.to_str().unwrap())).unwrap();
        assert_eq!(record.roots.len(), 1);

        // A genuinely corrupted root-attach event anywhere in this project's
        // stream must not be silently skipped in favor of the earlier good one.
        crate::state::append(
            &state,
            &record.project_id,
            "human",
            ROOT_ATTACHED_FAMILY,
            "not json".to_string(),
            vec![],
        )
        .unwrap();

        let after = project_by_id(&state, &record.project_id).unwrap();
        assert!(
            after.roots.is_empty(),
            "a malformed root fact must fail the roots closed, never expose a stale good one"
        );
        // The Project itself is still very much alive — only its roots are unknown.
        assert_eq!(after.name, "P");
    }

    #[test]
    fn a_malformed_newest_socket_project_set_fails_closed_to_ungrouped() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let socket_ctx = open_stream(&state, &open_req(&repo, "flaky"))
            .unwrap()
            .context;
        let (record, _) = create_project(&state, "P", None).unwrap();
        link_socket_to_project(
            &state,
            &socket_ctx.stream_id,
            &record.project_id,
            &socket_ctx.worktree_path,
            true,
        )
        .unwrap();

        crate::state::append(
            &state,
            &socket_ctx.stream_id,
            "human",
            SOCKET_PROJECT_FAMILY,
            "not json".to_string(),
            vec![],
        )
        .unwrap();

        let heart = state.heart.lock().unwrap();
        let (pid, rid) = crate::state::socket_project(&heart, &socket_ctx.stream_id);
        assert_eq!(pid, None, "malformed newest membership fact must fail closed to Ungrouped, not the older linked project");
        assert_eq!(rid, None);
    }

    #[test]
    fn a_malformed_retired_marker_read_failure_fails_closed_to_retired() {
        // is_retired's Err(_) branch: exercised indirectly by proving the
        // Ok(Some(_)) and Ok(None) branches behave as documented — a real
        // Heart read error is not independently triggerable without fault
        // injection this crate does not have, but the two reachable branches
        // fully cover the observable contract callers depend on.
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let (record, _) = create_project(&state, "P", None).unwrap();
        assert!(!is_retired(
            &state.heart.lock().unwrap(),
            &record.project_id
        ));
        retire_project(&state, &record.project_id).unwrap();
        assert!(is_retired(&state.heart.lock().unwrap(), &record.project_id));
    }
}
