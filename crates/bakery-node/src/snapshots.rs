//! Turn snapshots: what each agent's turn changed in its workspace, whatever
//! made the change — its runtime's own edits, shell commands, subagents, a
//! container seat writing through its mount. The workspace is captured
//! before and after every turn as a Git tree in a history Bakery owns
//! (`<base>/snapshots/<key>/`, an object store of its own; a Git workspace
//! lends it its objects and its index, so unchanged files cost nothing, and
//! a plain folder gets the same history without them). Each file the turn
//! changed becomes a review item for a change that has already landed
//! (`agent.file.changed` with `landed`): accepting keeps it, rejecting
//! restores the earlier version. Auto and Auto Edit accept and record at
//! once; in Manual the agent's next turn waits until its last turn's changes
//! are decided. Files the turn already had decided (Claude's native review,
//! the API loop's proposals) are not asked about twice, and neither is what a
//! merge leaves meanwhile: one the node itself makes into the checkout
//! ([`landing`]), or one made from outside it, read from the branch's own
//! merge commits ([`TurnSnapshot::merged_meanwhile`]). The workspace's own `.git` (hooks, config) is never used:
//! every Git command runs against Bakery's history, which goes with its
//! folder when a delete removes it ([`forget`]).

use crate::agent_control::OperatingMode;
use crate::node::Seat;
use crate::state::{append, ActiveContext, AppState};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Write;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Files larger than this many MB stay out of snapshots, until the operator
/// sets another limit in Settings (`snapshot_limit_mb`): their changes are
/// listed by size and can only be kept.
const DEFAULT_LIMIT_MB: NonZeroU32 = NonZeroU32::new(10).unwrap();

/// Dot-folders of caches, installs and tool state, skipped when ignored.
const SKIPPED_DOT_FOLDERS: &[&str] = &[
    ".git",
    ".cache",
    ".next",
    ".nuxt",
    ".svelte-kit",
    ".turbo",
    ".parcel-cache",
    ".vercel",
    ".venv",
    ".tox",
    ".nox",
    ".gradle",
    ".pytest_cache",
    ".mypy_cache",
    ".ruff_cache",
    ".terraform",
    ".yarn",
    ".pnpm-store",
    ".npm",
    ".direnv",
    ".idea",
];

/// A text file's content shows in review up to this size; past it, or for a
/// binary file, review names the change without showing it.
pub(crate) const SHOWN_BYTES: usize = 1 << 20;

/// Stop's decision family (`routes::agent`): it closes a review item too.
const APPROVAL_CANCELLED: &str = "agent.approval.cancelled";

/// An agent's own folder (`.claude/`, … `bakery_exec::AGENT_STATE_DIRS`) is
/// reviewed, all but these folders of caches and logs, and its `*.log`s.
const AGENT_CACHES_AND_LOGS: &[&str] = &["cache", "caches", "log", "logs", "tmp"];

/// Whether a path Git ignores is reviewed anyway. A dotfile, or a file
/// inside a dot-folder, unless a cache dot-folder holds it, or it is an
/// agent's own cache or log; never `.DS_Store`. Anything under an ignored
/// ordinary folder (node_modules/, target/) never gets here: the listing
/// folds it away.
fn reviewed_though_ignored(path: &str) -> bool {
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    let Some(name) = parts.last() else {
        return false;
    };
    let skipped = parts.iter().any(|p| SKIPPED_DOT_FOLDERS.contains(p));
    let agents_cache_or_log = parts
        .iter()
        .position(|p| bakery_exec::AGENT_STATE_DIRS.contains(p))
        .is_some_and(|at| {
            name.ends_with(".log")
                || parts[at + 1..]
                    .iter()
                    .any(|p| AGENT_CACHES_AND_LOGS.contains(p))
        });
    *name != ".DS_Store"
        && !skipped
        && !agents_cache_or_log
        && parts.iter().any(|p| p.starts_with('.'))
}

// ─── Bakery's history of a workspace ────────────────────────────────────────

struct Store {
    git_dir: PathBuf,
    workspace: PathBuf,
}

/// One snapshot at a time per workspace: they share an index.
fn store_lock(git_dir: &Path) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();
    LOCKS
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .entry(git_dir.to_path_buf())
        .or_default()
        .clone()
}

/// A Git command's stdout; `add --ignore-errors` skipping an unreadable file
/// (exit 1, its index still complete) counts as done.
fn finished(args: &[&str], out: std::process::Output) -> Result<Vec<u8>, String> {
    let tolerated = args.contains(&"--ignore-errors") && out.status.code() == Some(1);
    if out.status.success() || tolerated {
        Ok(out.stdout)
    } else {
        Err(format!(
            "git {}: {}",
            args.first().copied().unwrap_or_default(),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// Where Bakery keeps the history of the turns in `workspace`, a canonical
/// path.
pub(crate) fn history_of(base: &Path, workspace: &Path) -> PathBuf {
    let key = blake3::hash(workspace.to_string_lossy().as_bytes()).to_hex();
    base.join("snapshots").join(&key[..16])
}

/// A folder that goes takes its history of turns with it: the changes it
/// could undo are gone, and nothing else ever removes a history. Found by
/// where the folder was, so it works once the folder is removed. Answers
/// whether there was one.
pub(crate) fn forget(base: &Path, workspace: &Path) -> Result<bool, String> {
    let was = workspace.canonicalize().ok().or_else(|| {
        let parent = workspace.parent()?.canonicalize().ok()?;
        Some(parent.join(workspace.file_name()?))
    });
    let Some(git_dir) = was.map(|at| history_of(base, &at)).filter(|dir| dir.join("HEAD").exists()) else {
        return Ok(false);
    };
    let lock = store_lock(&git_dir);
    let _one = lock.lock().unwrap();
    claims().lock().unwrap().remove(&git_dir);
    std::fs::remove_dir_all(&git_dir).map_err(|e| format!("{}: {e}", git_dir.display()))?;
    Ok(true)
}

impl Store {
    /// The history of `workspace`, made the first time.
    fn open(base: &Path, workspace: &Path) -> Result<Store, String> {
        let workspace = workspace
            .canonicalize()
            .map_err(|e| format!("{}: {e}", workspace.display()))?;
        let store = Store {
            git_dir: history_of(base, &workspace),
            workspace,
        };
        let repo = repository(&store.workspace);
        if !store.git_dir.join("HEAD").exists() {
            std::fs::create_dir_all(&store.git_dir).map_err(|e| e.to_string())?;
            let out = Command::new("git")
                .args(["init", "-q", "--bare"])
                .arg(&store.git_dir)
                .stdin(Stdio::null())
                .output()
                .map_err(|e| format!("git: {e}"))?;
            finished(&["init"], out)?;
            for (key, value) in [
                ("core.bare", "false"),
                ("gc.auto", "0"),
                ("core.symlinks", "true"),
                ("core.logAllRefUpdates", "false"),
            ] {
                store.git(&["config", key, value], None)?;
            }
            // A Git workspace lends its objects, and its index's stat cache
            // spares hashing every unchanged file on the first snapshot.
            if let Some(repo) = &repo {
                let alternates = store.git_dir.join("objects/info/alternates");
                std::fs::write(alternates, format!("{}\n", repo.objects.display()))
                    .map_err(|e| e.to_string())?;
                if repo.root == store.workspace {
                    let _ = std::fs::copy(&repo.index, store.git_dir.join("index"));
                }
            }
        }
        // The workspace repository's own excludes count, as they do in it.
        if let Some(repo) = &repo {
            let info = store.git_dir.join("info");
            let _ = std::fs::create_dir_all(&info);
            let _ = std::fs::copy(&repo.exclude, info.join("exclude"));
        }
        Ok(store)
    }

    /// A Git command against Bakery's history of the workspace, `input` on
    /// its stdin.
    fn git(&self, args: &[&str], input: Option<&[u8]>) -> Result<Vec<u8>, String> {
        self.git_on(None, args, input)
    }

    /// The same on an index file of its own, to build a tree aside from the
    /// history's index.
    fn git_on(&self, index: Option<&Path>, args: &[&str], input: Option<&[u8]>) -> Result<Vec<u8>, String> {
        let mut cmd = Command::new("git");
        cmd.arg("--git-dir")
            .arg(&self.git_dir)
            .arg("--work-tree")
            .arg(&self.workspace)
            .args([
                "-c",
                "core.quotepath=false",
                "-c",
                "core.autocrlf=false",
                "-c",
                "core.safecrlf=false",
                "-c",
                "core.fsmonitor=false",
            ])
            .args(args)
            .current_dir(&self.workspace)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for var in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_INDEX_FILE",
            "GIT_OBJECT_DIRECTORY",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        ] {
            cmd.env_remove(var);
        }
        if let Some(index) = index {
            cmd.env("GIT_INDEX_FILE", index);
        }
        let mut child = cmd.spawn().map_err(|e| format!("git: {e}"))?;
        if let Some(mut stdin) = child.stdin.take() {
            if let Some(input) = input {
                stdin.write_all(input).map_err(|e| e.to_string())?;
            }
        }
        finished(args, child.wait_with_output().map_err(|e| e.to_string())?)
    }

    /// `tree` with these files set to a blob and mode, or taken out (`None`),
    /// as a new tree.
    fn amended(&self, tree: &str, edits: &[(String, Option<(String, String)>)]) -> Result<String, String> {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let index = self.git_dir.join(format!("index.amend-{}-{seq}", std::process::id()));
        // NUL-ended `<mode> <blob>\t<path>`; mode 0 takes a path out.
        let mut lines = Vec::new();
        for (path, side) in edits {
            let (oid, mode) = side.clone().unwrap_or_else(|| ("0".repeat(tree.len()), "0".to_string()));
            lines.extend_from_slice(format!("{mode} {oid}\t{path}\0").as_bytes());
        }
        let built = self
            .git_on(Some(&index), &["read-tree", tree], None)
            .and_then(|_| self.git_on(Some(&index), &["update-index", "-z", "--index-info"], Some(&lines)))
            .and_then(|_| self.git_on(Some(&index), &["write-tree"], None));
        let _ = std::fs::remove_file(&index);
        built.map(|out| String::from_utf8_lossy(&out).trim().to_string())
    }

    fn text(&self, args: &[&str]) -> Result<String, String> {
        Ok(String::from_utf8_lossy(&self.git(args, None)?)
            .trim()
            .to_string())
    }

    /// NUL-separated output as entries.
    fn entries(&self, args: &[&str]) -> Result<Vec<String>, String> {
        Ok(String::from_utf8_lossy(&self.git(args, None)?)
            .split('\0')
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect())
    }
}

/// Where a Git workspace keeps what Bakery's history borrows.
struct Repository {
    root: PathBuf,
    objects: PathBuf,
    index: PathBuf,
    exclude: PathBuf,
}

fn repository(workspace: &Path) -> Option<Repository> {
    let out = Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args([
            "rev-parse",
            "--path-format=absolute",
            "--show-toplevel",
            "--git-path",
            "objects",
            "--git-path",
            "index",
            "--git-path",
            "info/exclude",
        ])
        .stdin(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let mut lines = text.lines().map(PathBuf::from);
    Some(Repository {
        root: lines.next()?.canonicalize().ok()?,
        objects: lines.next()?,
        index: lines.next()?,
        exclude: lines.next()?,
    })
}

/// One moment of a workspace: its tree, and the files too big to keep in
/// it, by size and change time.
#[derive(Debug, Clone, Default)]
struct Snap {
    tree: String,
    big: BTreeMap<String, (u64, u128)>,
}

fn take(store: &Store, max_bytes: u64) -> Result<Snap, String> {
    let lock = store_lock(&store.git_dir);
    let _one = lock.lock().unwrap();
    // Ignored paths reviewed anyway (F1-1): ignored folders come folded
    // ("node_modules/"); only dot-folders are opened up.
    let mut forced = Vec::new();
    for entry in store.entries(&[
        "ls-files",
        "-z",
        "--others",
        "--ignored",
        "--exclude-standard",
        "--directory",
    ])? {
        match entry.strip_suffix('/') {
            Some(folder) => {
                let name = folder.rsplit('/').next().unwrap_or(folder);
                if name.starts_with('.') && reviewed_though_ignored(&format!("{folder}/x")) {
                    let inside = store.entries(&[
                        "ls-files",
                        "-z",
                        "--others",
                        "--ignored",
                        "--exclude-standard",
                        "--",
                        folder,
                    ])?;
                    // A folder listed whole is another repository: never
                    // added, or Git records it as a commit.
                    forced.extend(inside.into_iter().filter(|p| !p.ends_with('/') && reviewed_though_ignored(p)));
                }
            }
            None if reviewed_though_ignored(&entry) => forced.push(entry),
            None => {}
        }
    }
    // What changed since the index last saw it, plus the forced ones: the
    // only files that could be too big to snapshot.
    let candidates = store.entries(&[
        "ls-files",
        "-z",
        "--modified",
        "--others",
        "--exclude-standard",
    ])?;
    let mut big = BTreeMap::new();
    for path in candidates.iter().chain(forced.iter()) {
        let Ok(meta) = std::fs::symlink_metadata(store.workspace.join(path)) else {
            continue;
        };
        if meta.is_file() && meta.len() > max_bytes {
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos());
            big.insert(path.clone(), (meta.len(), mtime));
        }
    }
    let mut all = String::from(".\0");
    for path in big.keys() {
        all.push_str(&format!(":(exclude,literal){path}\0"));
    }
    store.git(
        &[
            "add",
            "-A",
            "--ignore-errors",
            "--pathspec-from-file=-",
            "--pathspec-file-nul",
        ],
        Some(all.as_bytes()),
    )?;
    let forced: String = forced
        .iter()
        .filter(|p| !big.contains_key(*p))
        .map(|p| format!(":(literal){p}\0"))
        .collect();
    if !forced.is_empty() {
        store.git(
            &[
                "add",
                "-f",
                "--ignore-errors",
                "--pathspec-from-file=-",
                "--pathspec-file-nul",
            ],
            Some(forced.as_bytes()),
        )?;
    }
    Ok(Snap {
        tree: store.text(&["write-tree"])?,
        big,
    })
}

/// One file a turn changed: its blob and mode on each side; `None` where it
/// didn't exist.
#[derive(Debug, Clone, PartialEq)]
struct Change {
    path: String,
    before: Option<(String, String)>,
    after: Option<(String, String)>,
}

/// Mode of another repository inside the workspace, recorded as its commit
/// (a submodule, a nested clone, a worktree in the folder): not a file.
const GITLINK: &str = "160000";

fn changes(store: &Store, before: &str, after: &str) -> Result<Vec<Change>, String> {
    let fields = store.entries(&["diff-tree", "-r", "-z", "--no-renames", before, after])?;
    let none = |oid: &str| oid.bytes().all(|b| b == b'0');
    let mut out = Vec::new();
    // `:<mode> <mode> <oid> <oid> <status>`, then the path.
    for pair in fields.chunks(2) {
        let [meta, path] = pair else { continue };
        let parts: Vec<&str> = meta.trim_start_matches(':').split(' ').collect();
        let [mode1, mode2, oid1, oid2, _] = parts[..] else {
            continue;
        };
        // Another repository's commits are its own; its thread, if it has
        // one, snapshots them.
        if mode1 == GITLINK || mode2 == GITLINK {
            continue;
        }
        out.push(Change {
            path: path.clone(),
            before: (!none(oid1)).then(|| (oid1.to_string(), mode1.to_string())),
            after: (!none(oid2)).then(|| (oid2.to_string(), mode2.to_string())),
        });
    }
    Ok(out)
}

// ─── A turn's snapshot ──────────────────────────────────────────────────────

/// A file a turn that already ended changed, and what it left. A turn still
/// running counts from there, so a file goes to the turn that changed it and
/// ended first; F1-8's guard replaces this simple rule later.
#[derive(Debug, Clone)]
struct Claim {
    path: String,
    after: Option<String>,
    at: Instant,
}

fn claims() -> &'static Mutex<HashMap<PathBuf, Vec<Claim>>> {
    static CLAIMS: OnceLock<Mutex<HashMap<PathBuf, Vec<Claim>>>> = OnceLock::new();
    CLAIMS.get_or_init(Default::default)
}

/// What a running turn counts its changes from: the workspace when it began,
/// moved on by a merge the node makes into it meanwhile ([`landing`]).
struct Baseline {
    snap: Mutex<Snap>,
    /// The size limit when the turn began, so its "after" matches its
    /// "before".
    max_bytes: u64,
}

/// The turns running in each history's workspace.
fn running() -> &'static Mutex<HashMap<PathBuf, Vec<Arc<Baseline>>>> {
    static RUNNING: OnceLock<Mutex<HashMap<PathBuf, Vec<Arc<Baseline>>>>> = OnceLock::new();
    RUNNING.get_or_init(Default::default)
}

/// A turn's "before", taken when it starts; dropping it at the turn's end
/// takes the "after" and puts each change up for review. Declared after the
/// turn's lease, so it drops first: the agent's next turn can't start before
/// the changes are recorded.
pub(crate) struct TurnSnapshot {
    state: AppState,
    stream_id: String,
    seat: Seat,
    turn_id: String,
    model_id: Option<String>,
    mode: OperatingMode,
    store: Store,
    before: Arc<Baseline>,
    began: Instant,
    /// The checkout's commit when the turn began, when it is a repository's.
    head: Option<String>,
}

/// The commit a Git workspace's checkout is on; none for a plain folder.
fn head_of(workspace: &Path) -> Option<String> {
    let head = crate::git::run(workspace, &["-c", "core.fsmonitor=false", "rev-parse", "--verify", "--quiet", "HEAD"]).ok()?;
    Some(head.trim().to_string()).filter(|h| !h.is_empty())
}

impl TurnSnapshot {
    /// The workspace before the turn; `None` for a conversation with no
    /// folder, or when the history can't be taken (said on the stream, so
    /// the turn's changes don't go unreviewed silently).
    pub(crate) fn begin(
        state: &AppState,
        ctx: &ActiveContext,
        seat: Seat,
        turn_id: &str,
        model_id: Option<String>,
        mode: OperatingMode,
    ) -> Option<TurnSnapshot> {
        let workspace = Path::new(&ctx.worktree_path);
        if ctx.worktree_path.trim().is_empty() || !workspace.is_dir() {
            return None;
        }
        let began = Instant::now();
        crate::writers::turn_began(&ctx.stream_id);
        let max_bytes = limit_bytes(state);
        let head = head_of(workspace);
        let taken = Store::open(&state.base, workspace)
            .and_then(|store| take(&store, max_bytes).map(|before| (store, before)));
        match taken {
            Ok((store, before)) => {
                let before = Arc::new(Baseline { snap: Mutex::new(before), max_bytes });
                running().lock().unwrap().entry(store.git_dir.clone()).or_default().push(before.clone());
                Some(TurnSnapshot {
                    state: state.clone(),
                    stream_id: ctx.stream_id.clone(),
                    seat,
                    turn_id: turn_id.to_string(),
                    model_id,
                    mode,
                    store,
                    before,
                    began,
                    head,
                })
            }
            Err(why) => {
                unrecorded(state, &ctx.stream_id, seat, turn_id, &why);
                None
            }
        }
    }

    fn end(&self) -> Result<(), String> {
        let after = take(&self.store, self.before.max_bytes)?;
        let before = self.before.snap.lock().unwrap().clone();
        let agent_id = crate::node::agent_id(&self.stream_id, self.seat);
        let decided = decided_in_turn(&self.state, &self.stream_id, &self.turn_id);
        let merged = self.merged_meanwhile().unwrap_or_else(|why| {
            eprintln!("snapshots: merges into the checkout stay among the turn's changes: {why}");
            HashMap::new()
        });
        let mut kept = Vec::new();
        {
            let mut all = claims().lock().unwrap();
            let held = all.entry(self.store.git_dir.clone()).or_default();
            held.retain(|c| c.at.elapsed() < Duration::from_secs(24 * 3600));
            for mut change in changes(&self.store, &before.tree, &after.tree)? {
                if decided.contains(&change.path) {
                    continue;
                }
                // A file only merges changed while the turn ran, from
                // where the turn found it: what the merges left is theirs,
                // and the turn's is what came after.
                if let Some((was, now)) = merged.get(&change.path) {
                    let oid = |side: &Option<(String, String)>| side.as_ref().map(|(oid, _)| oid.clone());
                    if oid(&change.before) == oid(was) {
                        if oid(&change.after) == oid(now) {
                            continue;
                        }
                        change.before = now.clone();
                    }
                }
                // Another turn that changed it and ended while this one ran
                // owns that change; this one counts only what came after.
                let since = held
                    .iter()
                    .filter(|c| c.path == change.path && c.at > self.began)
                    .max_by_key(|c| c.at);
                if let Some(claim) = since {
                    let after = change.after.as_ref().map(|(oid, _)| oid.clone());
                    if claim.after == after {
                        continue;
                    }
                    change.before = claim.after.clone().map(|oid| (oid, "100644".to_string()));
                }
                held.push(Claim {
                    path: change.path.clone(),
                    after: change.after.as_ref().map(|(oid, _)| oid.clone()),
                    at: Instant::now(),
                });
                kept.push(change);
            }
        }
        // A container has no name-based fence like an agent's room, which
        // never lets an agent make a credential file: one made there during
        // the turn is taken back out, and said so.
        let contained = matches!(
            crate::machine_access::reach_for(&self.state, &self.stream_id),
            crate::machine_access::Reach::Container | crate::machine_access::Reach::Locked
        );
        for change in &kept {
            let made = change.before.is_none() && change.after.is_some();
            if contained && made && bakery_exec::is_credential_filename(Path::new(&change.path)) {
                self.block(&agent_id, &change.path)?;
                continue;
            }
            self.propose(&agent_id, change)?;
            let by = crate::writers::Writer::Thread(self.stream_id.clone());
            crate::writers::wrote(by, &self.store.workspace, &change.path);
        }
        for (path, size) in big_changes(&before.big, &after.big) {
            // ponytail: a big file a merge brought is the merge's, even if
            // the turn changed it after; no copy to tell them apart.
            if !decided.contains(&path) && !merged.contains_key(&path) {
                self.propose_big(&agent_id, &path, size)?;
            }
        }
        Ok(())
    }

    /// The files merges brought onto the checkout's branch while the turn
    /// ran, a `git merge` made from outside the Bakery too, and no commit of
    /// the branch's own changed: each with its blob and mode before and
    /// after them. From the branch's first-parent line between the commit
    /// the turn began on and the one it ends on; empty when it didn't move.
    fn merged_meanwhile(&self) -> Result<HashMap<String, (Option<(String, String)>, Option<(String, String)>)>, String> {
        let (Some(began), Some(ends)) = (self.head.as_deref(), head_of(&self.store.workspace)) else {
            return Ok(HashMap::new());
        };
        if began == ends {
            return Ok(HashMap::new());
        }
        let range = format!("{began}..{ends}");
        let commits = |kind: &str| -> Result<Vec<String>, String> {
            Ok(self.store.text(&["rev-list", "--first-parent", kind, &range])?.lines().map(str::to_string).collect())
        };
        let touched = |commit: &str| self.store.entries(&["diff-tree", "--no-commit-id", "-r", "-z", "--name-only", "--no-renames", &format!("{commit}^1"), commit]);
        let mut merged = HashSet::new();
        for commit in commits("--merges")? {
            merged.extend(touched(&commit)?);
        }
        for commit in commits("--no-merges")? {
            for path in self.store.entries(&["diff-tree", "--no-commit-id", "-r", "-z", "--name-only", "--no-renames", "--root", &commit])? {
                merged.remove(&path);
            }
        }
        if merged.is_empty() {
            return Ok(HashMap::new());
        }
        let blobs = |commit: &str| -> Result<HashMap<String, (String, String)>, String> {
            // The whole tree, read as is: a path is never a pathspec here.
            Ok(self
                .store
                .entries(&["ls-tree", "-r", "-z", commit])?
                .into_iter()
                .filter_map(|entry| {
                    // `<mode> <type> <oid>\t<path>`
                    let (meta, path) = entry.split_once('\t')?;
                    if !merged.contains(path) {
                        return None;
                    }
                    let mut meta = meta.split(' ');
                    let (mode, _, oid) = (meta.next()?, meta.next()?, meta.next()?);
                    Some((path.to_string(), (oid.to_string(), mode.to_string())))
                })
                .collect())
        };
        let (was, now) = (blobs(began)?, blobs(&ends)?);
        Ok(merged.into_iter().map(|path| (path.clone(), (was.get(&path).cloned(), now.get(&path).cloned()))).collect())
    }

    /// Why a file's content never goes into the history, if it mustn't: a
    /// credential-shaped name (`.env`, `*.pem`, …), or a path under the
    /// operator's deny list or the built-in one. The Heart is read by the
    /// Baker, by paired phones and by agents' tools, so such a change is
    /// named with its sizes only; Keep and Undo work from the snapshot store,
    /// and the operator reads it on demand (`GET /agent/change`).
    fn hidden(&self, path: &str) -> Option<&'static str> {
        if bakery_exec::is_credential_filename(Path::new(path)) {
            return Some("credential");
        }
        let full = self.store.workspace.join(path);
        let mut deny = crate::machine_access::for_stream(&self.state, &self.stream_id).deny;
        deny.extend(bakery_exec::default_deny_paths());
        deny.iter()
            .any(|d| full.starts_with(d.canonicalize().unwrap_or_else(|_| d.clone())))
            .then_some("denied")
    }

    fn propose(&self, agent_id: &str, change: &Change) -> Result<(), String> {
        if let Some(why) = self.hidden(&change.path) {
            let size = |side: &Option<(String, String)>| -> Option<u64> {
                let (oid, _) = side.as_ref()?;
                self.store.text(&["cat-file", "-s", oid]).ok()?.parse().ok()
            };
            let landed = json!({
                "turn_id": self.turn_id,
                "seat": self.seat,
                "before": change.before.as_ref().map(|(oid, _)| oid),
                "before_mode": change.before.as_ref().map(|(_, mode)| mode),
                "after": change.after.as_ref().map(|(oid, _)| oid),
                "before_size": size(&change.before),
                "after_size": size(&change.after),
                "hidden": why,
                "binary": false,
                "restorable": true,
            });
            return self.record(agent_id, &change.path, String::new(), String::new(), landed);
        }
        let content = |side: &Option<(String, String)>| -> Result<(String, bool), String> {
            let Some((oid, _)) = side else {
                return Ok((String::new(), false));
            };
            let bytes = self.store.git(&["cat-file", "blob", oid], None)?;
            Ok(match shown(&bytes) {
                Some(text) => (text, false),
                None => (String::new(), true),
            })
        };
        let (old_content, old_binary) = content(&change.before)?;
        let (new_content, new_binary) = content(&change.after)?;
        let landed = json!({
            "turn_id": self.turn_id,
            "seat": self.seat,
            "before": change.before.as_ref().map(|(oid, _)| oid),
            "before_mode": change.before.as_ref().map(|(_, mode)| mode),
            "after": change.after.as_ref().map(|(oid, _)| oid),
            "binary": old_binary || new_binary,
            "restorable": true,
        });
        self.record(agent_id, &change.path, old_content, new_content, landed)
    }

    /// Takes back out a credential file an agent made in its container, and
    /// records that on its stream (the path only).
    fn block(&self, agent_id: &str, path: &str) -> Result<(), String> {
        let target = self.store.workspace.join(path);
        std::fs::remove_file(&target).map_err(|e| format!("{path}: {e}"))?;
        append(
            &self.state,
            &self.stream_id,
            "node",
            "agent.write.blocked",
            json!({
                "agent_id": agent_id,
                "turn_id": self.turn_id,
                "file_path": path,
                "why": "an agent can't make a credential file; it was taken back out",
            })
            .to_string(),
            vec![],
        )
        .map(drop)
    }

    /// The operator's saves of `path` since this turn started.
    // ponytail: counted from the turn's start event, appended just after the
    // workspace was captured; a save in that instant isn't marked.
    fn saved_by_the_operator(&self, path: &str) -> Vec<String> {
        let events = self.state.heart.lock().unwrap().read_stream(&self.stream_id).unwrap_or_default();
        let payload = |e: &bakery_core::Event| serde_json::from_str::<Value>(&e.payload).unwrap_or_default();
        let started = |e: &&bakery_core::Event| e.family == "agent.turn.started" && payload(e)["turn_id"] == self.turn_id.as_str();
        let Some(start) = events.iter().position(|e| started(&e)) else {
            return Vec::new();
        };
        events[start..]
            .iter()
            .filter(|e| e.family == crate::routes::project::OPERATOR_EDIT_FAMILY)
            .filter(|e| payload(e)["file_path"].as_str().is_some_and(|f| f.trim_start_matches("./") == path))
            .map(|e| e.event_hash.clone())
            .collect()
    }

    /// A file too big to keep a copy of: named, with its size, and only
    /// kept.
    fn propose_big(&self, agent_id: &str, path: &str, size: Option<u64>) -> Result<(), String> {
        let landed = json!({
            "turn_id": self.turn_id,
            "seat": self.seat,
            "size": size,
            "hidden": self.hidden(path),
            "binary": true,
            "restorable": false,
        });
        self.record(agent_id, path, String::new(), String::new(), landed)
    }

    fn record(
        &self,
        agent_id: &str,
        path: &str,
        old_content: String,
        new_content: String,
        landed: Value,
    ) -> Result<(), String> {
        // E-1: the operator saved this file while the turn ran. The change
        // includes their edit, so it can only be kept (`restore`).
        let mut landed = landed;
        let theirs = self.saved_by_the_operator(path);
        if !theirs.is_empty() {
            landed["includes_operator_edit"] = json!(theirs);
        }
        let payload = json!({
            "file_path": path,
            "status": "pending",
            "rationale": "changed during the agent's turn",
            "old_content": old_content,
            "new_content": new_content,
            "agent_id": agent_id,
            "model_id": self.model_id,
            "turn_id": self.turn_id,
            "call_id": "",
            "landed": landed,
        });
        let event = append(
            &self.state,
            &self.stream_id,
            &format!("agent:{}", self.seat.as_str()),
            "agent.file.changed",
            payload.to_string(),
            vec![],
        )?;
        // Auto and Auto Edit accept and record; Manual (and Plan) waits.
        if matches!(self.mode, OperatingMode::Auto | OperatingMode::AutoEdit) {
            let req = crate::routes::agent::AgentDecisionReq {
                stream_id: self.stream_id.clone(),
                event_hash: event.event_hash,
            };
            crate::routes::agent::decide_as(&self.state, &req, true, None, "policy/auto")?;
        }
        Ok(())
    }
}

impl Drop for TurnSnapshot {
    fn drop(&mut self) {
        {
            let mut all = running().lock().unwrap();
            if let Some(turns) = all.get_mut(&self.store.git_dir) {
                turns.retain(|t| !Arc::ptr_eq(t, &self.before));
                if turns.is_empty() {
                    all.remove(&self.store.git_dir);
                }
            }
        }
        if let Err(why) = self.end() {
            unrecorded(&self.state, &self.stream_id, self.seat, &self.turn_id, &why);
        }
        // Git rule 5: which other threads on this repository now change the
        // same files.
        crate::overlaps::check(&self.state, &self.stream_id);
        // Rule 3: a rebase this turn made without a stated reason.
        let since = chrono::Utc::now().timestamp() - self.began.elapsed().as_secs() as i64 - 1;
        let agent_id = crate::node::agent_id(&self.stream_id, self.seat);
        crate::git_hooks::check_rebases(&self.state, &self.stream_id, &agent_id, &self.turn_id, since);
    }
}

// ─── A merge the node makes while a turn runs ───────────────────────────────

/// A merge the node is about to make into a checkout (`thread_merge`, the
/// operator's merge in the app): not an agent's edit, though it changes
/// files while a turn runs there. [`Landing::landed`], once the merge went
/// through, moves each running turn's "before" on to what the merge left,
/// so its review lists the turn's own changes only. A file the turn had
/// changed itself stays its change, the merge's lines in it too; a save
/// from outside the Bakery (an editor) can't be told apart and is still
/// counted, as is a merge under way when a turn begins or ends.
pub(crate) struct Landing(Option<Merging>);

struct Merging {
    store: Store,
    turns: Vec<Arc<Baseline>>,
    /// The workspace just before the merge.
    before: Snap,
    max_bytes: u64,
}

/// Called before the merge: costs nothing unless a turn is running in
/// `workspace`.
pub(crate) fn landing(state: &AppState, workspace: &Path) -> Landing {
    let merging = || -> Option<Merging> {
        let at = workspace.canonicalize().ok()?;
        let max_bytes = limit_bytes(state);
        let turns: Vec<Arc<Baseline>> = running()
            .lock()
            .unwrap()
            .get(&history_of(&state.base, &at))?
            .iter()
            .filter(|t| t.max_bytes == max_bytes)
            .cloned()
            .collect();
        if turns.is_empty() {
            return None;
        }
        let store = Store::open(&state.base, &at).ok()?;
        let before = take(&store, max_bytes).ok()?;
        Some(Merging { store, turns, before, max_bytes })
    };
    Landing(merging())
}

impl Landing {
    /// The merge went through. A failure here leaves it counted among the
    /// turns' edits, as it would have been.
    pub(crate) fn landed(self) {
        if let Some(merging) = self.0 {
            if let Err(why) = merging.rebaseline() {
                eprintln!("snapshots: a merge stays among the running turns' changes: {why}");
            }
        }
    }
}

impl Merging {
    fn rebaseline(&self) -> Result<(), String> {
        let after = take(&self.store, self.max_bytes)?;
        let merged = changes(&self.store, &self.before.tree, &after.tree)?;
        for turn in &self.turns {
            if let Err(why) = self.move_on(turn, &merged, &after) {
                eprintln!("snapshots: a merge stays among a running turn's changes: {why}");
            }
        }
        Ok(())
    }

    fn move_on(&self, turn: &Baseline, merged: &[Change], after: &Snap) -> Result<(), String> {
        let mut snap = turn.snap.lock().unwrap();
        // What the turn changed itself before the merge stays its change.
        let own: HashSet<String> = changes(&self.store, &snap.tree, &self.before.tree)?
            .into_iter()
            .map(|c| c.path)
            .collect();
        let moved: Vec<(String, Option<(String, String)>)> = merged
            .iter()
            .filter(|c| !own.contains(&c.path))
            .map(|c| (c.path.clone(), c.after.clone()))
            .collect();
        if !moved.is_empty() {
            snap.tree = self.store.amended(&snap.tree, &moved)?;
        }
        for path in self.before.big.keys().chain(after.big.keys()) {
            let (was, now) = (self.before.big.get(path), after.big.get(path));
            if was != now && snap.big.get(path) == was {
                match now {
                    Some(stat) => snap.big.insert(path.clone(), *stat),
                    None => snap.big.remove(path),
                };
            }
        }
        Ok(())
    }
}

/// A turn whose changes couldn't be put up for review: said on its stream.
fn unrecorded(state: &AppState, stream_id: &str, seat: Seat, turn_id: &str, why: &str) {
    let _ = append(
        state,
        stream_id,
        "node",
        "agent.snapshot.failed",
        json!({
            "agent_id": crate::node::agent_id(stream_id, seat),
            "turn_id": turn_id,
            "why": why,
        })
        .to_string(),
        vec![],
    );
}

/// Text to show in review, or `None` for a binary or too-large file.
fn shown(bytes: &[u8]) -> Option<String> {
    if bytes.len() > SHOWN_BYTES || bytes.iter().take(8000).any(|b| *b == 0) {
        return None;
    }
    String::from_utf8(bytes.to_vec()).ok()
}

/// Big files that appeared, changed or went: their new size, `None` if gone.
fn big_changes(
    before: &BTreeMap<String, (u64, u128)>,
    after: &BTreeMap<String, (u64, u128)>,
) -> Vec<(String, Option<u64>)> {
    let mut out: Vec<(String, Option<u64>)> = after
        .iter()
        .filter(|(path, stat)| before.get(*path) != Some(*stat))
        .map(|(path, (size, _))| (path.clone(), Some(*size)))
        .collect();
    out.extend(
        before
            .keys()
            .filter(|path| !after.contains_key(*path))
            .map(|path| (path.clone(), None)),
    );
    out
}

/// Paths this turn already had decided in review (native review, the API
/// loop's proposals): not asked about again.
fn decided_in_turn(state: &AppState, stream_id: &str, turn_id: &str) -> HashSet<String> {
    let events = state
        .heart
        .lock()
        .unwrap()
        .read_stream_tail(stream_id, 2000)
        .unwrap_or_default();
    events
        .iter()
        .filter(|e| e.family == "agent.file.changed")
        .filter_map(|e| serde_json::from_str::<Value>(&e.payload).ok())
        .filter(|p| p["turn_id"] == turn_id && p.get("landed").is_none())
        .filter_map(|p| p["file_path"].as_str().map(str::to_string))
        .collect()
}

// ─── Review ─────────────────────────────────────────────────────────────────

/// An agent's landed changes still waiting for a decision.
fn undecided(state: &AppState, stream_id: &str, agent_id: &str) -> Vec<String> {
    let events = state
        .heart
        .lock()
        .unwrap()
        .read_stream(stream_id)
        .unwrap_or_default();
    let closed: HashSet<&str> = events
        .iter()
        .filter(|e| {
            matches!(
                e.family.as_str(),
                "agent.edit.accepted" | "agent.edit.rejected" | APPROVAL_CANCELLED
            )
        })
        .flat_map(|e| e.causality.iter().map(String::as_str))
        .collect();
    events
        .iter()
        .filter(|e| e.family == "agent.file.changed" && !closed.contains(e.event_hash.as_str()))
        .filter(|e| {
            serde_json::from_str::<Value>(&e.payload).is_ok_and(|p| {
                p["agent_id"] == agent_id && p["status"] == "pending" && p["landed"].is_object()
            })
        })
        .map(|e| e.event_hash.clone())
        .collect()
}

/// Why the agent's next turn waits, in Manual: its last turn's changes are
/// still up for review. A queued instruction stays queued until they're
/// decided.
pub(crate) fn waiting_for_review(
    state: &AppState,
    stream_id: &str,
    agent_id: &str,
) -> Option<String> {
    match undecided(state, stream_id, agent_id).len() {
        0 => None,
        1 => Some("1 change from its last turn is waiting for review".to_string()),
        n => Some(format!(
            "{n} changes from its last turn are waiting for review"
        )),
    }
}

/// Rejecting a landed change: the file goes back to what it was before the
/// turn (or away, if the turn made it). Refused when the file changed again
/// since, and for a file too big to have been kept.
pub(crate) fn restore(
    state: &AppState,
    stream_id: &str,
    file_path: &str,
    landed: &Value,
) -> Result<(), String> {
    if landed["restorable"] != json!(true) {
        return Err(format!(
            "{file_path} was too big to keep a copy of, so it can only be kept"
        ));
    }
    if landed["includes_operator_edit"].as_array().is_some_and(|saves| !saves.is_empty()) {
        return Err(format!("{file_path} includes your edit, so it can only be kept"));
    }
    let ctx = crate::state::context_for(state, stream_id)?;
    let target = crate::tools::Jail::new(Path::new(&ctx.worktree_path))?.resolve(file_path)?;
    let store = Store::open(&state.base, Path::new(&ctx.worktree_path))?;
    let current = match std::fs::symlink_metadata(&target) {
        Ok(_) => Some(store.text(&["hash-object", "--no-filters", "--", file_path])?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("{file_path}: {e}")),
    };
    let after = landed["after"].as_str().map(str::to_string);
    if current != after {
        return Err(format!(
            "{file_path} changed again since that turn, so it wasn't restored"
        ));
    }
    match landed["before"].as_str() {
        Some(oid) => {
            let bytes = store.git(&["cat-file", "blob", oid], None)?;
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            std::fs::write(&target, bytes).map_err(|e| format!("{file_path}: {e}"))?;
            if landed["before_mode"] == "100755" {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755));
            }
        }
        None => match std::fs::remove_file(&target) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("{file_path}: {e}")),
        },
    }
    Ok(())
}

/// After a landed change is decided: the agent's queued instruction may go.
pub(crate) fn decided(state: &AppState, stream_id: &str, landed: &Value) {
    if let Ok(seat) = serde_json::from_value::<Seat>(landed["seat"].clone()) {
        crate::agent_control::schedule_dispatch(state, stream_id, seat);
    }
}

/// One edit row's record (`agent.file.changed`) and the file's text before
/// and after it: a landed change's from the snapshot store, never the
/// history; a proposal's from its own record. A side is `None` where the
/// file wasn't there, or isn't text.
pub(crate) fn edit_sides(
    st: &AppState,
    stream_id: &str,
    event_hash: &str,
) -> Result<(Value, Option<String>, Option<String>), String> {
    let event = st
        .heart
        .lock()
        .unwrap()
        .read_stream(stream_id)
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|e| e.event_hash == event_hash && e.family == "agent.file.changed")
        .ok_or_else(|| format!("no change {event_hash} in {stream_id}"))?;
    let p: Value = serde_json::from_str(&event.payload).map_err(|e| e.to_string())?;
    let landed = &p["landed"];
    if !landed.is_object() {
        let side = |key: &str| p[key].as_str().map(str::to_string);
        let (before, after) = (side("old_content"), side("new_content"));
        return Ok((p, before, after));
    }
    let ctx = crate::state::context_for(st, stream_id)?;
    let store = Store::open(&st.base, Path::new(&ctx.worktree_path))?;
    let side = |key: &str| -> Option<String> {
        let oid = landed[key].as_str()?;
        shown(&store.git(&["cat-file", "blob", oid], None).ok()?)
    };
    let (before, after) = (side("before"), side("after"));
    Ok((p, before, after))
}

#[derive(schemars::JsonSchema, serde::Deserialize)]
pub(crate) struct ChangeQuery {
    pub(crate) stream_id: String,
    /// The change's review item (`agent.file.changed`).
    pub(crate) event_hash: String,
}

#[derive(schemars::JsonSchema, serde::Serialize)]
pub(crate) struct ChangeContent {
    pub(crate) file_path: String,
    /// The file before the turn; `None` when the turn made it, or when it
    /// isn't text.
    pub(crate) before: Option<String>,
    /// The file the turn left; `None` when the turn removed it, or when it
    /// isn't text.
    pub(crate) after: Option<String>,
    /// A change kept out of the history (a credential, a denied path): no
    /// desktop screenshot while the app shows it.
    pub(crate) secret: bool,
}

/// `GET /agent/change`: one landed change's before and after, read from the
/// snapshot store, never the history — how the operator's review shows a
/// change whose content stays out of the Heart (a credential, a denied
/// path). Hidden from agents, and no paired phone reaches it.
pub(crate) async fn change_content(
    axum::extract::State(st): axum::extract::State<AppState>,
    axum::extract::Query(q): axum::extract::Query<ChangeQuery>,
) -> Result<axum::Json<ChangeContent>, (axum::http::StatusCode, String)> {
    let not_found = |why: String| (axum::http::StatusCode::NOT_FOUND, why);
    let read = crate::actor::spawn_blocking(move || -> Result<ChangeContent, String> {
        let (p, before, after) = edit_sides(&st, &q.stream_id, &q.event_hash)?;
        if !p["landed"].is_object() {
            return Err("that change isn't one an agent's turn made".to_string());
        }
        Ok(ChangeContent {
            file_path: p["file_path"].as_str().unwrap_or_default().to_string(),
            before,
            after,
            secret: p["landed"]["hidden"].is_string(),
        })
    })
    .await
    .map_err(|e| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map_err(not_found)?;
    Ok(axum::Json(read))
}

#[derive(schemars::JsonSchema, serde::Deserialize)]
pub(crate) struct ReviewTurnReq {
    pub(crate) stream_id: String,
    pub(crate) turn_id: String,
    /// Keep every change (true), or restore every one (false).
    pub(crate) accept: bool,
}

#[derive(schemars::JsonSchema, serde::Serialize)]
pub(crate) struct TurnReviewed {
    /// Changes decided now.
    pub(crate) decided: usize,
    /// Changes that couldn't be, each with why.
    pub(crate) refused: Vec<String>,
}

/// `POST /agent/turn/review`: every undecided change one turn left, kept or
/// restored at once. Hidden from agents.
pub(crate) async fn review_turn(
    axum::extract::State(st): axum::extract::State<AppState>,
    axum::Json(req): axum::Json<ReviewTurnReq>,
) -> Result<axum::Json<TurnReviewed>, (axum::http::StatusCode, String)> {
    let reviewed = crate::actor::spawn_blocking(move || {
        let events = st
            .heart
            .lock()
            .unwrap()
            .read_stream(&req.stream_id)
            .unwrap_or_default();
        let of_turn: Vec<String> = events
            .iter()
            .filter(|e| e.family == "agent.file.changed")
            .filter(|e| {
                serde_json::from_str::<Value>(&e.payload)
                    .is_ok_and(|p| p["landed"]["turn_id"] == req.turn_id.as_str())
            })
            .map(|e| e.event_hash.clone())
            .collect();
        let mut out = TurnReviewed {
            decided: 0,
            refused: Vec::new(),
        };
        for event_hash in of_turn {
            let decision = crate::routes::agent::AgentDecisionReq {
                stream_id: req.stream_id.clone(),
                event_hash,
            };
            match crate::routes::agent::decide(&st, &decision, req.accept) {
                Ok(_) => out.decided += 1,
                Err(why) if why.contains("already has a terminal decision") => {}
                Err(why) => out.refused.push(why),
            }
        }
        out
    })
    .await
    .map_err(|e| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(axum::Json(reviewed))
}

/// The size limit now, in bytes.
fn limit_bytes(state: &AppState) -> u64 {
    let mb = state.node.lock().unwrap().snapshot_limit_mb;
    u64::from(mb.unwrap_or(DEFAULT_LIMIT_MB).get()) << 20
}

#[derive(schemars::JsonSchema, serde::Serialize, serde::Deserialize)]
pub(crate) struct SnapshotLimit {
    /// A file bigger than this many MB gets no copy when a turn's changes
    /// are recorded, so its change can only be kept, never undone.
    pub(crate) max_mb: NonZeroU32,
}

/// `GET /agent/snapshot-limit`: the size limit, 10 MB until the operator
/// sets one.
pub(crate) async fn snapshot_limit(
    axum::extract::State(st): axum::extract::State<AppState>,
) -> axum::Json<SnapshotLimit> {
    let set = st.node.lock().unwrap().snapshot_limit_mb;
    axum::Json(SnapshotLimit {
        max_mb: set.unwrap_or(DEFAULT_LIMIT_MB),
    })
}

/// `POST /agent/snapshot-limit`: the operator's, in Settings. A turn
/// already running keeps the limit it began with.
pub(crate) async fn set_snapshot_limit(
    axum::extract::State(st): axum::extract::State<AppState>,
    axum::Json(req): axum::Json<SnapshotLimit>,
) -> Result<axum::Json<SnapshotLimit>, (axum::http::StatusCode, String)> {
    let mut node = st.node.lock().unwrap();
    node.snapshot_limit_mb = Some(req.max_mb);
    node.save(&st.base)
        .map_err(|e| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(axum::Json(req))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{make_repo, open_req, run_git, state_with};
    use std::sync::Arc;

    #[test]
    fn ignored_dotfiles_and_dot_config_are_reviewed_caches_and_installs_are_not() {
        for path in [
            ".env",
            "app/.env.local",
            ".vscode/settings.json",
            ".cargo/config.toml",
            ".claude/settings.local.json",
            ".gemini/commands/x.toml",
            ".claude",
        ] {
            assert!(reviewed_though_ignored(path), "{path}");
        }
        for path in [
            "app.log",
            "build/out.js",
            ".next/cache/x",
            ".venv/lib/site.py",
            "src/.DS_Store",
            ".vscode/.cache/x",
            ".continue/logs/core.log",
            ".claude/cache/x.json",
            ".gemini/tmp/chat.json",
            ".codex/run.log",
        ] {
            assert!(!reviewed_though_ignored(path), "{path}");
        }
    }

    /// The rehearsal's failed snapshot ("git cat-file <sha>: bad file"): a
    /// checkout holding another repository (the Baker's main checkout holds
    /// its workers' worktrees; a project, its submodules or a cloned tool)
    /// had it recorded as a commit, and a turn that moved that repository on
    /// read the commit as a file. Another repository's commits aren't files
    /// this turn changed: the snapshot passes them by, and the turn's own
    /// change still comes up for review.
    #[test]
    fn a_repository_inside_the_workspace_is_passed_by_not_read_as_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        std::fs::write(repo.join(".gitignore"), ".tools/\n").unwrap();
        run_git(&repo, &["add", ".gitignore"]);
        run_git(&repo, &["commit", "-qm", "ignore tools"]);
        let state = state_with(dir.path(), Arc::new(bakery_agent::StubModel));
        let ctx = crate::routes::project::open_stream(&state, &open_req(&repo, "nested"))
            .unwrap()
            .context;
        let ws = std::path::PathBuf::from(&ctx.worktree_path);
        // One the workspace doesn't ignore, and one in an ignored dot-folder,
        // which snapshots open up.
        let inner = [make_repo(&ws.join("vendor/lib")), make_repo(&ws.join(".tools/helper"))];

        let turn = TurnSnapshot::begin(&state, &ctx, Seat::Orchestrator, "turn_n", None, OperatingMode::Manual)
            .expect("a snapshot");
        for other in &inner {
            std::fs::write(other.join("more.txt"), "moved on\n").unwrap();
            run_git(other, &["add", "."]);
            run_git(other, &["commit", "-qm", "moved on"]);
        }
        std::fs::write(ws.join("f.txt"), "line one\nchanged\n").unwrap();
        drop(turn);

        let failed: Vec<String> = state
            .heart
            .lock()
            .unwrap()
            .read_stream(&ctx.stream_id)
            .unwrap()
            .into_iter()
            .filter(|e| e.family == "agent.snapshot.failed")
            .map(|e| e.payload)
            .collect();
        assert!(failed.is_empty(), "{failed:?}");
        let paths: Vec<String> = landed(&state, &ctx.stream_id)
            .iter()
            .map(|(_, p)| p["file_path"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(paths, ["f.txt"]);
    }

    fn landed(state: &AppState, stream_id: &str) -> Vec<(String, Value)> {
        state
            .heart
            .lock()
            .unwrap()
            .read_stream(stream_id)
            .unwrap()
            .into_iter()
            .filter(|e| e.family == "agent.file.changed")
            .filter_map(|e| {
                let p: Value = serde_json::from_str(&e.payload).ok()?;
                p["landed"].is_object().then_some((e.event_hash, p))
            })
            .collect()
    }

    fn by_path<'a>(all: &'a [(String, Value)], path: &str) -> &'a (String, Value) {
        all.iter()
            .find(|(_, p)| p["file_path"] == path)
            .unwrap_or_else(|| panic!("no change for {path}"))
    }

    fn decide(state: &AppState, stream_id: &str, hash: &str, accept: bool) -> Result<(), String> {
        let req = crate::routes::agent::AgentDecisionReq {
            stream_id: stream_id.to_string(),
            event_hash: hash.to_string(),
        };
        crate::routes::agent::decide(state, &req, accept).map(drop)
    }

    /// The operator saves a file while the agent's turn runs (E-1): the save
    /// goes through, and the turn's change to that file says it includes
    /// their edit, so it can only be kept.
    #[tokio::test]
    async fn a_save_during_a_turn_marks_its_change_as_including_the_operators_edit() {
        use crate::routes::project::{files_read_inner, files_write, FileWriteReq, OPERATOR_EDIT_FAMILY};
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(bakery_agent::StubModel));
        let ctx = crate::routes::project::open_stream(&state, &open_req(&repo, "during")).unwrap().context;
        let ws = std::path::PathBuf::from(&ctx.worktree_path);
        let turn = TurnSnapshot::begin(&state, &ctx, Seat::Orchestrator, "turn_e1", None, OperatingMode::Manual)
            .expect("a snapshot");
        let agent = crate::node::agent_id(&ctx.stream_id, Seat::Orchestrator);
        let started = json!({"turn_id": "turn_e1", "agent_id": agent});
        append(&state, &ctx.stream_id, "human", "agent.turn.started", started.to_string(), vec![]).unwrap();
        std::fs::write(ws.join("f.txt"), "line one\nby the agent\n").unwrap();
        let (_, digest) = files_read_inner(&state, &ctx.stream_id, "f.txt").unwrap();
        let save = FileWriteReq {
            stream_id: ctx.stream_id.clone(),
            path: "f.txt".into(),
            text: "line one\nby the agent\nand me\n".into(),
            expected_digest: Some(digest),
        };
        let saved = files_write(axum::extract::State(state.clone()), axum::Json(save)).await.unwrap().0;
        assert!(matches!(saved, crate::routes::project::FileWriteResp::Ok { .. }), "the save goes through");
        drop(turn);

        let saved = state.heart.lock().unwrap().events_of_family(&ctx.stream_id, OPERATOR_EDIT_FAMILY).unwrap();
        let (change, p) = by_path(&landed(&state, &ctx.stream_id), "f.txt").clone();
        assert_eq!(p["landed"]["includes_operator_edit"], json!([saved[0].event_hash]), "{p}");
        let undo = decide(&state, &ctx.stream_id, &change, false).unwrap_err();
        assert!(undo.contains("includes your edit"), "{undo}");
    }

    /// The operator's save over a change still waiting for review keeps that
    /// change (E-2): History notes the save went over it, and its Undo
    /// restores nothing and says the operator's edit stays.
    #[tokio::test]
    async fn the_operators_save_over_a_waiting_change_keeps_it_and_its_undo_says_so() {
        use crate::routes::project::{files_read_inner, files_write, FileWriteReq, OPERATOR_EDIT_FAMILY};
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(bakery_agent::StubModel));
        let ctx = crate::routes::project::open_stream(&state, &open_req(&repo, "over")).unwrap().context;
        let ws = std::path::PathBuf::from(&ctx.worktree_path);
        let turn = TurnSnapshot::begin(&state, &ctx, Seat::Orchestrator, "turn_a", None, OperatingMode::Manual)
            .expect("a snapshot");
        std::fs::write(ws.join("f.txt"), "line one\nby the agent\n").unwrap();
        drop(turn);
        let change = by_path(&landed(&state, &ctx.stream_id), "f.txt").0.clone();

        let (_, digest) = files_read_inner(&state, &ctx.stream_id, "f.txt").unwrap();
        let mine = "line one\nby the agent\nand me\n";
        let save = FileWriteReq {
            stream_id: ctx.stream_id.clone(),
            path: "f.txt".into(),
            text: mine.into(),
            expected_digest: Some(digest),
        };
        let saved = files_write(axum::extract::State(state.clone()), axum::Json(save)).await.unwrap().0;
        assert!(matches!(saved, crate::routes::project::FileWriteResp::Ok { .. }), "the save goes through");
        let saves = state.heart.lock().unwrap().events_of_family(&ctx.stream_id, OPERATOR_EDIT_FAMILY).unwrap();
        let noted: Value = serde_json::from_str(&saves[0].payload).unwrap();
        assert_eq!(noted["over_changes"], json!([change]), "{noted}");

        let undo = decide(&state, &ctx.stream_id, &change, false).unwrap_err();
        assert!(undo.contains("you edited f.txt") && undo.contains("your edit stays"), "{undo}");
        assert_eq!(std::fs::read_to_string(ws.join("f.txt")).unwrap(), mine);
    }

    /// A Git worktree in Manual: every change a turn made, however it was
    /// made, comes up for review; ignored installs and logs don't, ignored
    /// dotfiles do. Keeping writes nothing, undoing restores, a file that
    /// changed again or was too big isn't restored, and the agent's next turn
    /// waits until all are decided.
    #[test]
    fn a_turns_changes_come_up_for_review_and_undo_restores_them() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        std::fs::write(repo.join(".gitignore"), "node_modules/\n*.log\n.env\n").unwrap();
        std::fs::write(repo.join("gone.txt"), "bye\n").unwrap();
        run_git(&repo, &["add", "."]);
        run_git(&repo, &["commit", "-qm", "more"]);
        let state = state_with(dir.path(), Arc::new(bakery_agent::StubModel));
        let ctx = crate::routes::project::open_stream(&state, &open_req(&repo, "snap"))
            .unwrap()
            .context;
        let ws = std::path::PathBuf::from(&ctx.worktree_path);
        let seat = Seat::Orchestrator;
        // The operator's limit (Settings): 1 MB.
        state.node.lock().unwrap().snapshot_limit_mb = Some(NonZeroU32::MIN);

        let turn = TurnSnapshot::begin(&state, &ctx, seat, "turn_a", None, OperatingMode::Manual)
            .expect("a snapshot");
        std::fs::write(ws.join("f.txt"), "line one\nline two\n").unwrap();
        std::fs::write(ws.join("new.txt"), "fresh\n").unwrap();
        std::fs::write(ws.join(".env"), "TOKEN=x\n").unwrap();
        std::fs::create_dir_all(ws.join("node_modules/pkg")).unwrap();
        std::fs::write(ws.join("node_modules/pkg/index.js"), "x").unwrap();
        std::fs::write(ws.join("run.log"), "noise").unwrap();
        std::fs::remove_file(ws.join("gone.txt")).unwrap();
        let big = vec![b'x'; (1 << 20) + 1];
        std::fs::write(ws.join("big.bin"), &big).unwrap();
        drop(turn);

        let all = landed(&state, &ctx.stream_id);
        let mut paths: Vec<&str> = all
            .iter()
            .map(|(_, p)| p["file_path"].as_str().unwrap())
            .collect();
        paths.sort();
        assert_eq!(paths, [".env", "big.bin", "f.txt", "gone.txt", "new.txt"]);
        let (_, f) = by_path(&all, "f.txt");
        assert_eq!(
            (f["old_content"].as_str(), f["new_content"].as_str()),
            (Some("line one\n"), Some("line one\nline two\n"))
        );
        assert_eq!(by_path(&all, "big.bin").1["landed"]["restorable"], false);
        let agent = crate::node::agent_id(&ctx.stream_id, seat);
        assert!(waiting_for_review(&state, &ctx.stream_id, &agent)
            .is_some_and(|w| w.starts_with("5 changes")));
        let next = crate::routes::agent::AgentTurnReq {
            stream_id: ctx.stream_id.clone(),
            file_path: None,
            instruction: "go on".into(),
            seat: Some(seat),
            model_id: None,
            effort: None,
            actor: None,
        };
        let held = crate::routes::agent::agent_turn_inner(&state, &next).unwrap_err();
        assert_eq!(held.0, axum::http::StatusCode::CONFLICT, "{}", held.1);

        decide(&state, &ctx.stream_id, &by_path(&all, "f.txt").0, false).unwrap();
        assert_eq!(
            std::fs::read_to_string(ws.join("f.txt")).unwrap(),
            "line one\n",
            "undone"
        );
        decide(&state, &ctx.stream_id, &by_path(&all, "gone.txt").0, false).unwrap();
        assert_eq!(
            std::fs::read_to_string(ws.join("gone.txt")).unwrap(),
            "bye\n",
            "back"
        );
        decide(&state, &ctx.stream_id, &by_path(&all, ".env").0, true).unwrap();
        assert!(ws.join(".env").exists(), "kept");
        let refused =
            decide(&state, &ctx.stream_id, &by_path(&all, "big.bin").0, false).unwrap_err();
        assert!(refused.contains("too big"), "{refused}");
        std::fs::write(ws.join("new.txt"), "changed again\n").unwrap();
        let refused =
            decide(&state, &ctx.stream_id, &by_path(&all, "new.txt").0, false).unwrap_err();
        assert!(refused.contains("changed again"), "{refused}");
        assert_eq!(
            std::fs::read_to_string(ws.join("new.txt")).unwrap(),
            "changed again\n"
        );

        decide(&state, &ctx.stream_id, &by_path(&all, "new.txt").0, true).unwrap();
        decide(&state, &ctx.stream_id, &by_path(&all, "big.bin").0, true).unwrap();
        assert_eq!(
            waiting_for_review(&state, &ctx.stream_id, &agent),
            None,
            "all decided"
        );
    }

    const SECRET: &str = "w2-SENTINEL-env-secret-never-in-history";

    /// An agent's turn appends to a `.env` it was never allowed to read: the
    /// change comes up for review, but no byte of the file reaches the
    /// history, the routes agents read, or the one phones read. Only the
    /// operator's own route shows it; Undo still restores it.
    #[tokio::test]
    async fn a_credential_files_content_never_reaches_the_history() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        std::fs::write(repo.join(".gitignore"), ".env\n").unwrap();
        run_git(&repo, &["add", "."]);
        run_git(&repo, &["commit", "-qm", "ignore"]);
        let state = state_with(dir.path(), Arc::new(bakery_agent::StubModel));
        let ctx = crate::routes::project::open_stream(&state, &open_req(&repo, "env"))
            .unwrap()
            .context;
        let ws = std::path::PathBuf::from(&ctx.worktree_path);
        let original = format!("API_KEY={SECRET}\n");
        std::fs::write(ws.join(".env"), &original).unwrap();

        let turn = TurnSnapshot::begin(
            &state,
            &ctx,
            Seat::Orchestrator,
            "turn_env",
            None,
            OperatingMode::Manual,
        )
        .unwrap();
        std::fs::write(ws.join(".env"), format!("{original}X=1\n")).unwrap();
        drop(turn);

        let all = landed(&state, &ctx.stream_id);
        let (hash, change) = by_path(&all, ".env");
        assert_eq!(change["landed"]["hidden"], "credential");
        assert_eq!(change["old_content"], "");
        assert_eq!(change["landed"]["before_size"], original.len());
        let streams = state.heart.lock().unwrap().list_streams().unwrap();
        for summary in streams {
            for event in state
                .heart
                .lock()
                .unwrap()
                .read_stream(&summary.stream_id)
                .unwrap()
            {
                assert!(
                    !event.payload.contains(SECRET),
                    "{} carries it",
                    event.family
                );
            }
        }
        let axum::Json(stream) = crate::routes::project::get_stream(
            axum::extract::State(state.clone()),
            axum::extract::Path(ctx.stream_id.clone()),
        )
        .await
        .unwrap();
        assert!(
            !serde_json::to_string(&stream).unwrap().contains(SECRET),
            "the phone's route"
        );
        let axum::Json(gates) =
            crate::routes::operator::list_gates(axum::extract::State(state.clone()))
                .await
                .unwrap();
        assert!(
            !serde_json::to_string(&gates).unwrap().contains(SECRET),
            "the agents' gates list"
        );

        let axum::Json(shown) = change_content(
            axum::extract::State(state.clone()),
            axum::extract::Query(ChangeQuery {
                stream_id: ctx.stream_id.clone(),
                event_hash: hash.clone(),
            }),
        )
        .await
        .unwrap();
        assert!(shown.secret, "no desktop screenshot while it shows");
        assert!(
            shown.before.unwrap().contains(SECRET),
            "the operator can look"
        );
        assert!(!crate::api_catalog::remote_allows(
            crate::api_catalog::Method::Get,
            "/agent/change",
            "phone"
        ));
        assert!(
            crate::api_catalog::tools()
                .iter()
                .all(|t| t.path != "/agent/change"),
            "no agent tool"
        );

        let req = crate::routes::agent::AgentDecisionReq {
            stream_id: ctx.stream_id.clone(),
            event_hash: hash.clone(),
        };
        let edited = crate::routes::agent::decide_with_content(&state, &req, true, Some("A=2\n"));
        assert!(edited.is_err(), "an edit would put content in the history");
        decide(&state, &ctx.stream_id, hash, false).unwrap();
        assert_eq!(
            std::fs::read_to_string(ws.join(".env")).unwrap(),
            original,
            "undone"
        );
        for event in state
            .heart
            .lock()
            .unwrap()
            .read_stream(&ctx.stream_id)
            .unwrap()
        {
            assert!(
                !event.payload.contains(SECRET),
                "{} carries it",
                event.family
            );
        }
    }

    /// In a container, a credential file the agent made during its turn is
    /// taken back out, never put up for review; its other changes are.
    #[test]
    fn a_credential_file_made_in_a_container_is_taken_back_out() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(bakery_agent::StubModel));
        let ctx = crate::routes::project::open_stream(&state, &open_req(&repo, "box"))
            .unwrap()
            .context;
        state.node.lock().unwrap().machine_access.reach.insert(
            ctx.stream_id.clone(),
            crate::machine_access::Reach::Container,
        );
        let ws = std::path::PathBuf::from(&ctx.worktree_path);
        let turn = TurnSnapshot::begin(
            &state,
            &ctx,
            Seat::Orchestrator,
            "turn_box",
            None,
            OperatingMode::Manual,
        )
        .unwrap();
        std::fs::write(ws.join(".env.local"), "PLANTED=1\n").unwrap();
        std::fs::write(ws.join("notes.md"), "kept for review\n").unwrap();
        drop(turn);

        assert!(!ws.join(".env.local").exists(), "taken back out");
        let all = landed(&state, &ctx.stream_id);
        assert_eq!(all.len(), 1, "{all:?}");
        assert_eq!(all[0].1["file_path"], "notes.md");
        let blocked = state
            .heart
            .lock()
            .unwrap()
            .read_stream(&ctx.stream_id)
            .unwrap()
            .into_iter()
            .find(|e| e.family == "agent.write.blocked")
            .unwrap();
        assert!(blocked.payload.contains(".env.local") && !blocked.payload.contains("PLANTED"));
    }

    /// Auto accepts and records; a plain folder (a chat) gets the same
    /// history; a file the turn already had decided isn't asked about again.
    #[test]
    fn auto_keeps_and_records_and_a_plain_folder_is_covered() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(bakery_agent::StubModel));
        let chat = crate::catalog::create_chat(&state, "notes").unwrap();
        let ws = std::path::PathBuf::from(&chat.worktree_path);
        let seat = Seat::Orchestrator;
        let turn = TurnSnapshot::begin(&state, &chat, seat, "turn_b", None, OperatingMode::Auto)
            .expect("a snapshot of a plain folder");
        std::fs::write(ws.join("a.md"), "one\n").unwrap();
        std::fs::write(ws.join("b.md"), "two\n").unwrap();
        // b.md went through review inside the turn already.
        append(
            &state,
            &chat.stream_id,
            "agent:orchestrator",
            "agent.file.changed",
            json!({ "file_path": "b.md", "status": "pending", "turn_id": "turn_b",
                    "agent_id": crate::node::agent_id(&chat.stream_id, seat) })
            .to_string(),
            vec![],
        )
        .unwrap();
        drop(turn);

        let all = landed(&state, &chat.stream_id);
        assert_eq!(all.len(), 1, "{all:?}");
        assert_eq!(all[0].1["file_path"], "a.md");
        let agent = crate::node::agent_id(&chat.stream_id, seat);
        assert_eq!(
            waiting_for_review(&state, &chat.stream_id, &agent),
            None,
            "Auto kept it"
        );
        let accepted = state
            .heart
            .lock()
            .unwrap()
            .read_stream(&chat.stream_id)
            .unwrap()
            .into_iter()
            .find(|e| e.family == "agent.edit.accepted")
            .unwrap();
        assert_eq!(accepted.actor, "policy/auto");
    }

    /// The node's merge into the checkout a turn runs in (the Baker's
    /// `thread_merge`, the operator's merge in the app) is not that turn's
    /// edit: what it made, changed or removed stays out of the turn's review,
    /// a file too big to keep a copy of included, and so does a second merge
    /// in the same turn, one that changes what the first brought. What the
    /// agent changed stays in it: a file it changed before the merges, their
    /// lines in it too, and, in a file a merge made, the lines it changed
    /// after.
    #[test]
    fn a_merge_the_node_makes_during_a_turn_is_not_the_turns_edit() {
        use crate::baker_threads::{create_thread, merge_thread, CreateThreadReq};
        use crate::testutil::{bakery_repo_for_test, state_with_bakery_root};
        let dir = tempfile::tempdir().unwrap();
        let repo = bakery_repo_for_test(dir.path());
        std::fs::write(repo.join("VISION.md"), "v1\nv2\nv3\nv4\nv5\nv6\nv7\n").unwrap();
        std::fs::write(repo.join("shared.md"), "a\nb\nc\nd\ne\nf\ng\nh\ni\n").unwrap();
        std::fs::write(repo.join("gone.md"), "bye\n").unwrap();
        run_git(&repo, &["add", "."]);
        run_git(&repo, &["commit", "-qm", "the bakery"]);
        let state = state_with_bakery_root(dir.path(), &repo);
        // The operator's limit (Settings): 1 MB.
        state.node.lock().unwrap().snapshot_limit_mb = Some(NonZeroU32::MIN);
        let home = crate::routes::baker::ensure_permanent_baker(&state, None, None).unwrap().stream_id;
        let main = crate::state::context_for(&state, &home).unwrap();

        // Two chefs' committed work, each a branch of its own off main.
        let chef = |title: &str, work: &dyn Fn(&Path)| {
            let req = CreateThreadReq { title: Some(title.into()), worktree: true, ..Default::default() };
            let stream = create_thread(&state, &req, "human", None, None).unwrap().thread.stream_id;
            let at = PathBuf::from(crate::state::context_for(&state, &stream).unwrap().worktree_path);
            work(&at);
            run_git(&at, &["add", "-A"]);
            run_git(&at, &["commit", "-qm", title]);
            stream
        };
        // A file made, one changed, one removed, a line of shared.md, and a
        // file too big to keep a copy of.
        let first = chef("Chef", &|at| {
            std::fs::write(at.join("sidebar.md"), "fixed\n").unwrap();
            std::fs::write(at.join("VISION.md"), "v1, by the chef\nv2\nv3\nv4\nv5\nv6\nv7\n").unwrap();
            std::fs::remove_file(at.join("gone.md")).unwrap();
            std::fs::write(at.join("shared.md"), "a\nb\nc\nd\ne\nf\ng\nh\ni, by the chef\n").unwrap();
            std::fs::write(at.join("big.bin"), vec![b'x'; (1 << 20) + 1]).unwrap();
        });
        // Another file made, and another place in VISION.md and shared.md.
        let second = chef("Chef two", &|at| {
            std::fs::write(at.join("notes.md"), "notes\n").unwrap();
            std::fs::write(at.join("VISION.md"), "v1\nv2\nv3\nv4\nv5\nv6\nv7, by chef two\n").unwrap();
            std::fs::write(at.join("shared.md"), "a\nb\nc\nd\ne, by chef two\nf\ng\nh\ni\n").unwrap();
        });

        // The main Baker's turn commits a change of its own, merges both
        // chefs (the checkout is clean each time), then touches up what the
        // first made.
        let turn = TurnSnapshot::begin(&state, &main, Seat::Baker, "turn_m", None, OperatingMode::Manual).expect("a snapshot");
        std::fs::write(repo.join("mine.md"), "the Baker's own\n").unwrap();
        std::fs::write(repo.join("shared.md"), "a, by the Baker\nb\nc\nd\ne\nf\ng\nh\ni\n").unwrap();
        run_git(&repo, &["add", "-A"]);
        run_git(&repo, &["commit", "-qm", "the Baker's own"]);
        merge_thread(&state, &first, None, "human").unwrap();
        merge_thread(&state, &second, None, "human").unwrap();
        assert!(repo.join("big.bin").exists() && repo.join("notes.md").exists() && !repo.join("gone.md").exists(), "the merges landed");
        let vision = std::fs::read_to_string(repo.join("VISION.md")).unwrap();
        assert_eq!(vision, "v1, by the chef\nv2\nv3\nv4\nv5\nv6\nv7, by chef two\n", "both chefs' lines");
        std::fs::write(repo.join("sidebar.md"), "fixed\nand touched up\n").unwrap();
        drop(turn);

        let all = landed(&state, &home);
        let mut paths: Vec<&str> = all.iter().map(|(_, p)| p["file_path"].as_str().unwrap()).collect();
        paths.sort();
        assert_eq!(paths, ["mine.md", "shared.md", "sidebar.md"], "the merges' own files are no edit of the turn");
        let sides = |path: &str| {
            let (_, p) = by_path(&all, path);
            (p["old_content"].as_str().unwrap().to_string(), p["new_content"].as_str().unwrap().to_string())
        };
        assert_eq!(sides("sidebar.md"), ("fixed\n".to_string(), "fixed\nand touched up\n".to_string()), "only what came after");
        assert_eq!(
            sides("shared.md"),
            (
                "a\nb\nc\nd\ne\nf\ng\nh\ni\n".to_string(),
                "a, by the Baker\nb\nc\nd\ne, by chef two\nf\ng\nh\ni, by the chef\n".to_string()
            ),
            "its own change, the merges' lines in it too"
        );
    }

    /// A merge made into the checkout from outside the Bakery while a turn
    /// runs (`git merge` in a terminal) is no edit of the turn either, read
    /// from the branch's merge commits: a file only the merges changed is
    /// theirs, the turn's is what came after, and a file the branch's own
    /// commit changed stays the turn's, the merges' lines in it too.
    #[test]
    fn a_merge_made_from_outside_during_a_turn_is_not_the_turns_edit() {
        use crate::baker_threads::{create_thread, CreateThreadReq};
        use crate::testutil::{bakery_repo_for_test, state_with_bakery_root};
        let dir = tempfile::tempdir().unwrap();
        let repo = bakery_repo_for_test(dir.path());
        std::fs::write(repo.join("VISION.md"), "v1\nv2\nv3\nv4\nv5\nv6\nv7\n").unwrap();
        std::fs::write(repo.join("shared.md"), "a\nb\nc\nd\ne\nf\ng\nh\ni\n").unwrap();
        std::fs::write(repo.join("gone.md"), "bye\n").unwrap();
        run_git(&repo, &["add", "."]);
        run_git(&repo, &["commit", "-qm", "the bakery"]);
        let state = state_with_bakery_root(dir.path(), &repo);
        state.node.lock().unwrap().snapshot_limit_mb = Some(NonZeroU32::MIN);
        let home = crate::routes::baker::ensure_permanent_baker(&state, None, None).unwrap().stream_id;
        let main = crate::state::context_for(&state, &home).unwrap();
        let chef = |title: &str, work: &dyn Fn(&Path)| {
            let req = CreateThreadReq { title: Some(title.into()), worktree: true, ..Default::default() };
            let stream = create_thread(&state, &req, "human", None, None).unwrap().thread.stream_id;
            let ctx = crate::state::context_for(&state, &stream).unwrap();
            let at = PathBuf::from(&ctx.worktree_path);
            work(&at);
            run_git(&at, &["add", "-A"]);
            run_git(&at, &["commit", "-qm", title]);
            ctx.branch
        };
        let first = chef("Chef", &|at| {
            std::fs::write(at.join("sidebar.md"), "fixed\n").unwrap();
            std::fs::write(at.join("VISION.md"), "v1, by the chef\nv2\nv3\nv4\nv5\nv6\nv7\n").unwrap();
            std::fs::remove_file(at.join("gone.md")).unwrap();
            std::fs::write(at.join("shared.md"), "a\nb\nc\nd\ne\nf\ng\nh\ni, by the chef\n").unwrap();
            std::fs::write(at.join("big.bin"), vec![b'x'; (1 << 20) + 1]).unwrap();
        });
        let second = chef("Chef two", &|at| {
            std::fs::write(at.join("notes.md"), "notes\n").unwrap();
            std::fs::write(at.join("VISION.md"), "v1\nv2\nv3\nv4\nv5\nv6\nv7, by chef two\n").unwrap();
        });

        // The Baker's turn commits a change of its own; meanwhile someone
        // merges both chefs in a terminal; the turn then touches up what the
        // first made.
        let turn = TurnSnapshot::begin(&state, &main, Seat::Baker, "turn_o", None, OperatingMode::Manual).expect("a snapshot");
        std::fs::write(repo.join("mine.md"), "the Baker's own\n").unwrap();
        std::fs::write(repo.join("shared.md"), "a, by the Baker\nb\nc\nd\ne\nf\ng\nh\ni\n").unwrap();
        run_git(&repo, &["add", "-A"]);
        run_git(&repo, &["commit", "-qm", "the Baker's own"]);
        run_git(&repo, &["merge", "-q", "--no-ff", "--no-edit", &first]);
        run_git(&repo, &["merge", "-q", "--no-ff", "--no-edit", &second]);
        std::fs::write(repo.join("sidebar.md"), "fixed\nand touched up\n").unwrap();
        drop(turn);

        let all = landed(&state, &home);
        let mut paths: Vec<&str> = all.iter().map(|(_, p)| p["file_path"].as_str().unwrap()).collect();
        paths.sort();
        assert_eq!(paths, ["mine.md", "shared.md", "sidebar.md"], "the merges' own files are no edit of the turn");
        let sides = |path: &str| {
            let (_, p) = by_path(&all, path);
            (p["old_content"].as_str().unwrap().to_string(), p["new_content"].as_str().unwrap().to_string())
        };
        assert_eq!(sides("sidebar.md"), ("fixed\n".to_string(), "fixed\nand touched up\n".to_string()), "only what came after");
        assert_eq!(
            sides("shared.md"),
            ("a\nb\nc\nd\ne\nf\ng\nh\ni\n".to_string(), "a, by the Baker\nb\nc\nd\ne\nf\ng\nh\ni, by the chef\n".to_string()),
            "its own commit's file stays its change, the merge's lines in it too"
        );
    }

    /// A folder's history of turns goes by where the folder was, even once
    /// the folder itself is gone; another folder's history stays.
    #[test]
    fn a_folders_history_is_forgotten_by_where_it_was() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(bakery_agent::StubModel));
        let chats = ["a", "b"].map(|name| crate::catalog::create_chat(&state, name).unwrap());
        for chat in &chats {
            let turn = TurnSnapshot::begin(&state, chat, Seat::Orchestrator, "turn_h", None, OperatingMode::Auto)
                .expect("a snapshot");
            std::fs::write(Path::new(&chat.worktree_path).join("n.md"), "x\n").unwrap();
            drop(turn);
        }
        let [a, b] = chats.each_ref().map(|chat| history_of(&state.base, &Path::new(&chat.worktree_path).canonicalize().unwrap()));
        assert!(a.join("HEAD").exists() && b.join("HEAD").exists());

        std::fs::remove_dir_all(&chats[0].worktree_path).unwrap();
        assert!(forget(&state.base, Path::new(&chats[0].worktree_path)).unwrap());
        assert!(!a.exists() && b.join("HEAD").exists(), "only the folder that went");
        assert!(!forget(&state.base, Path::new(&chats[0].worktree_path)).unwrap(), "nothing left of it");
        assert!(!forget(&state.base, &dir.path().join("never-had-a-turn")).unwrap());
    }

    /// Two turns in one worktree at once: a file goes to the turn that
    /// changed it and ended first. Then all of a turn's changes are undone
    /// at once.
    #[tokio::test]
    async fn overlapping_turns_and_reviewing_a_whole_turn() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(bakery_agent::StubModel));
        let ctx = crate::routes::project::open_stream(&state, &open_req(&repo, "two"))
            .unwrap()
            .context;
        let ws = std::path::PathBuf::from(&ctx.worktree_path);
        let first = TurnSnapshot::begin(
            &state,
            &ctx,
            Seat::Orchestrator,
            "turn_1",
            None,
            OperatingMode::Manual,
        )
        .unwrap();
        let second = TurnSnapshot::begin(
            &state,
            &ctx,
            Seat::Worker,
            "turn_2",
            None,
            OperatingMode::Manual,
        )
        .unwrap();
        std::fs::write(ws.join("f.txt"), "first\n").unwrap();
        drop(first);
        std::fs::write(ws.join("w.txt"), "second\n").unwrap();
        drop(second);

        let all = landed(&state, &ctx.stream_id);
        let turn_of = |path: &str| by_path(&all, path).1["landed"]["turn_id"].clone();
        assert_eq!(all.len(), 2, "{all:?}");
        assert_eq!(turn_of("f.txt"), "turn_1");
        assert_eq!(turn_of("w.txt"), "turn_2");

        let reviewed = review_turn(
            axum::extract::State(state.clone()),
            axum::Json(ReviewTurnReq {
                stream_id: ctx.stream_id.clone(),
                turn_id: "turn_1".into(),
                accept: false,
            }),
        )
        .await
        .unwrap()
        .0;
        assert_eq!((reviewed.decided, reviewed.refused.len()), (1, 0));
        assert_eq!(
            std::fs::read_to_string(ws.join("f.txt")).unwrap(),
            "line one\n"
        );
        assert!(
            ws.join("w.txt").exists(),
            "the other turn's change is untouched"
        );
    }
}
