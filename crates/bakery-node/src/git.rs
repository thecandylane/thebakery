//! Read-only git introspection for the Open wizard.
//!
//! The user picks a folder; the node — not the client — answers "what is this?".
//! Everything here shells out to real `git` (same choice as `git worktree add`
//! in `main.rs`; no git2 dependency) and **fails closed**: a path that is not a
//! repository comes back `is_git: false` with a reason, never a guess.
//!
//! Command output parsing lives in free functions so it is unit-testable without
//! a repository on disk.

use serde::Serialize;
use std::path::Path;
use std::process::Command;

/// Everything the wizard needs to offer a branch + worktree choice.
#[derive(schemars::JsonSchema, Debug, Clone, Serialize)]
pub struct ProjectInfo {
    /// The path the human picked, as given.
    pub path: String,
    pub is_git: bool,
    /// Why `is_git` is false (or a git failure). `None` when fine.
    pub error: Option<String>,
    /// Absolute toplevel of the *main* working tree (not the picked worktree).
    pub root: Option<String>,
    /// Basename of `root` — the project name shown in the context bar.
    pub name: Option<String>,
    /// Branch checked out at `path`, or `None` when detached.
    pub current_branch: Option<String>,
    /// `origin/HEAD` → else main → else master → else current.
    pub default_branch: Option<String>,
    pub local_branches: Vec<String>,
    pub worktrees: Vec<WorktreeInfo>,
}

#[derive(schemars::JsonSchema, Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorktreeInfo {
    pub path: String,
    /// Short branch name, or `None` when detached/bare.
    pub branch: Option<String>,
    pub head: Option<String>,
    /// The repository's own working tree (first entry of `git worktree list`).
    pub is_main: bool,
    pub locked: bool,
}

/// Inspect a folder. Never panics, never writes.
pub fn inspect(path: &str) -> ProjectInfo {
    let mut info = ProjectInfo {
        path: path.to_string(),
        is_git: false,
        error: None,
        root: None,
        name: None,
        current_branch: None,
        default_branch: None,
        local_branches: Vec::new(),
        worktrees: Vec::new(),
    };

    let p = Path::new(path);
    if !p.is_dir() {
        info.error = Some(format!("not a directory: {path}"));
        return info;
    }

    // `--show-toplevel` is the *picked* tree; a linked worktree's main repo is
    // the parent of the common git dir. Both matter: branches live in the
    // common dir, so we key everything off the main root.
    let root = match git(
        p,
        &["rev-parse", "--path-format=absolute", "--show-toplevel"],
    ) {
        Ok(out) => out.trim().to_string(),
        Err(e) => {
            info.error = Some(e);
            return info;
        }
    };
    let main_root = git(
        p,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .ok()
    .and_then(|d| main_root_from_common_dir(d.trim()))
    .unwrap_or_else(|| root.clone());

    info.is_git = true;
    info.name = Path::new(&main_root)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned());
    info.root = Some(main_root.clone());

    let main = Path::new(&main_root);
    info.current_branch = git(p, &["rev-parse", "--abbrev-ref", "HEAD"])
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|b| !b.is_empty() && b != "HEAD");

    info.local_branches = git(
        main,
        &["for-each-ref", "--format=%(refname:short)", "refs/heads"],
    )
    .map(|s| parse_branches(&s))
    .unwrap_or_default();

    let origin_head = git(
        main,
        &["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"],
    )
    .ok();
    info.default_branch = pick_default_branch(
        origin_head.as_deref(),
        &info.local_branches,
        info.current_branch.as_deref(),
    );

    info.worktrees = git(main, &["worktree", "list", "--porcelain"])
        .map(|s| parse_worktree_list(&s))
        .unwrap_or_default();

    info
}

/// An empty directory the node's own git uses as `core.hooksPath`. Git looks
/// for hook executables in a directory; this one has none, and it is not a
/// repository or a room. Callers that already pass the pair are left as they
/// are, so the flags are not stacked.
fn empty_hooks_dir() -> &'static Path {
    use std::sync::OnceLock;
    static DIR: OnceLock<std::path::PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("bakery-node-git-hooks-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555));
        }
        dir
    })
}

/// `-c` arguments that turn hooks and fsmonitor off, unless `args` already
/// carries that pair.
pub(crate) fn hook_guard_args(args: &[&str]) -> Vec<String> {
    let has = |needle: &str| args.windows(2).any(|pair| pair[0] == "-c" && pair[1].starts_with(needle));
    if has("core.hooksPath=") && has("core.fsmonitor=") {
        return Vec::new();
    }
    vec![
        "-c".to_string(),
        format!("core.hooksPath={}", empty_hooks_dir().display()),
        "-c".to_string(),
        "core.fsmonitor=false".to_string(),
    ]
}

/// Run git in `dir`, returning stdout on success and stderr on failure.
/// Hooks and fsmonitor are off: this git has the node's reach, so a hook
/// planted in the repository does not run. Git inside a room, and git the
/// operator types in a host terminal, do not come through here.
pub fn run(dir: &Path, args: &[&str]) -> Result<String, String> {
    let guards = hook_guard_args(args);
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(guards.iter().map(String::as_str))
        .args(args)
        .output()
        .map_err(|e| format!("failed to run git: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if err.is_empty() {
            format!("git {args:?} failed")
        } else {
            err
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Internal alias — call sites that predate the public `run` name.
fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    run(dir, args)
}

/// Raw bytes of a blob (`<rev>:<path>`), never lossy-decoded — `run`'s
/// `from_utf8_lossy` would corrupt binary content, and sign-off's
/// commit-tree verification hashes exactly these bytes.
pub fn cat_blob(repo: &Path, spec: &str) -> Result<Vec<u8>, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["cat-file", "blob", spec])
        .output()
        .map_err(|e| format!("failed to run git cat-file: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if err.is_empty() {
            format!("git cat-file blob {spec} failed")
        } else {
            err
        });
    }
    Ok(out.stdout)
}

/// For a **linked** git worktree (whose `.git` is a `gitdir: <path>` pointer
/// file), the two grants a sandboxed room needs so `git status`/`diff` work
/// inside it: read of the repository's shared `.git` directory, and write of
/// this worktree's own gitdir (`<repo>/.git/worktrees/<name>` — the
/// per-worktree index and its locks live there). `None` for an in-place
/// checkout (`.git` is a real directory inside the root — already covered by
/// the workspace grant) and for non-git roots. Commits still happen
/// host-side: the shared object store stays read-only on purpose.
pub fn linked_worktree_git_dirs(
    worktree_root: &Path,
) -> Option<(std::path::PathBuf, std::path::PathBuf)> {
    let dot_git = worktree_root.join(".git");
    if !dot_git.is_file() {
        return None;
    }
    let content = std::fs::read_to_string(&dot_git).ok()?;
    let gitdir = content.trim().strip_prefix("gitdir:")?.trim();
    let gitdir = if Path::new(gitdir).is_absolute() {
        std::path::PathBuf::from(gitdir)
    } else {
        worktree_root.join(gitdir)
    };
    let gitdir = gitdir.canonicalize().ok()?;
    // `<repo>/.git/worktrees/<name>` → the shared `.git` two levels up.
    let shared = gitdir.parent()?.parent()?;
    if shared.file_name()?.to_str()? != ".git" {
        return None;
    }
    Some((shared.to_path_buf(), gitdir.clone()))
}

/// True only for Baker factory branches (`baker/…`). Never `main` / `master`.
pub fn is_baker_branch(branch: &str) -> bool {
    let b = branch.trim();
    b.starts_with("baker/") && b.len() > "baker/".len()
}

/// True for a task socket's own branch (`task/*`). Never `main`/`master`.
pub fn is_task_branch(branch: &str) -> bool {
    let b = branch.trim();
    b.starts_with("task/") && b.len() > "task/".len()
}

/// The one shared predicate the ship plumbing guards on: a branch Bakery
/// itself derived (`baker/*` for factory jobs, `task/*` for task sockets).
/// Route-level guards stay narrower on purpose — `/baker/push` still
/// refuses a task branch and vice versa; this only widens the inner
/// plumbing (`push_set_upstream`, `gh_pr_create`, `gh_pr_merge`) enough
/// that both route families can use it while arbitrary branches stay
/// refused everywhere.
pub fn is_bakery_derived_branch(branch: &str) -> bool {
    is_baker_branch(branch) || is_task_branch(branch)
}

/// `git merge-base --is-ancestor ancestor descendant` — exit 0 = yes,
/// exit 1 = no, anything else (unknown revision, not a repo) is a real
/// error. Implemented directly on `Command` because `run`'s Err collapses
/// "no" and "broken" into one shape.
pub fn is_ancestor(repo: &Path, ancestor: &str, descendant: &str) -> Result<bool, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["merge-base", "--is-ancestor", ancestor, descendant])
        .output()
        .map_err(|e| format!("failed to run git merge-base: {e}"))?;
    match out.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => {
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            Err(if stderr.is_empty() {
                format!("git merge-base --is-ancestor {ancestor} {descendant} failed")
            } else {
                stderr
            })
        }
    }
}

/// Best-effort `git fetch --all` — used before an ancestry re-check when a
/// predecessor's sign-off may have been integrated remotely. A repo with no
/// remotes fetches nothing and that is fine.
pub fn fetch_all(repo: &Path) -> Result<String, String> {
    run(repo, &["fetch", "--all", "--quiet"])
}

/// When a repository was last fetched, and why it failed if it did: the
/// "fetched 2 min ago" beside "N behind".
#[derive(schemars::JsonSchema, Debug, Clone, Serialize)]
pub struct Fetched {
    pub at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// One key for a repository and all its worktrees: their shared git folder.
pub(crate) fn repo_key(repo: &Path) -> String {
    run(repo, &["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .map(|dir| dir.trim().to_string())
        .unwrap_or_else(|_| repo.canonicalize().unwrap_or_else(|_| repo.to_path_buf()).to_string_lossy().into_owned())
}

/// `a` and `b` are checkouts of one repository: they share its git common dir.
pub(crate) fn same_repository(a: &Path, b: &Path) -> bool {
    repo_key(a) == repo_key(b)
}

fn fetches() -> &'static std::sync::Mutex<std::collections::HashMap<String, Fetched>> {
    static FETCHED: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, Fetched>>> =
        std::sync::OnceLock::new();
    FETCHED.get_or_init(Default::default)
}

/// The last fetch of `repo` since the node started, if any.
pub fn last_fetch(repo: &Path) -> Option<Fetched> {
    fetches().lock().unwrap().get(&repo_key(repo)).cloned()
}

/// `git fetch origin` (rule 2): one at a time per repository, never waiting
/// on a credential prompt, and given up after a minute. Remembered for
/// `last_fetch`.
pub fn fetch_origin(repo: &Path) -> Result<(), String> {
    use std::io::Read;
    static ONE: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, std::sync::Arc<std::sync::Mutex<()>>>>,
    > = std::sync::OnceLock::new();
    let key = repo_key(repo);
    let lock = ONE.get_or_init(Default::default).lock().unwrap().entry(key.clone()).or_default().clone();
    let _one = lock.lock().unwrap_or_else(|e| e.into_inner());
    let fetched = (|| {
        // The Bakery's own fetch: the repository's hooks never run for it.
        // One pair, the same empty directory `run` uses. This command does
        // not go through `run` (it has its own timeout), so the pair is set
        // here and not stacked on top of another.
        let guards = hook_guard_args(&[]);
        let mut child = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(guards.iter().map(String::as_str))
            .args(["fetch", "--quiet", "--prune", "origin"])
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("failed to run git: {e}"))?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        loop {
            if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
                if status.success() {
                    return Ok(());
                }
                let mut err = String::new();
                if let Some(mut stderr) = child.stderr.take() {
                    let _ = stderr.read_to_string(&mut err);
                }
                return Err(Some(err.trim()).filter(|e| !e.is_empty()).unwrap_or("git fetch failed").to_string());
            }
            if std::time::Instant::now() > deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err("git fetch took over a minute".to_string());
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    })();
    let at = chrono::Utc::now().to_rfc3339();
    fetches().lock().unwrap().insert(key, Fetched { at, error: fetched.as_ref().err().cloned() });
    fetched
}

/// Where a new thread forks (rule 2): `base`, a ref git resolves.
#[derive(Debug, Clone, PartialEq)]
pub struct ForkPoint {
    pub base: String,
    /// Why it isn't origin's main.
    pub stale: Option<String>,
}

/// The newer main, freshly fetched (`newer_main`); with no remote, or when
/// the fetch fails, the local main, and why.
pub fn fresh_main(repo: &Path) -> ForkPoint {
    if !has_remote(repo, "origin") {
        return ForkPoint { base: local_main(repo), stale: Some("the repository has no remote".to_string()) };
    }
    if let Err(e) = fetch_origin(repo) {
        return ForkPoint { base: local_main(repo), stale: Some(format!("couldn't fetch origin: {e}")) };
    }
    newer_main(repo, local_main(repo))
}

/// Of the local main and origin's as last fetched, the newer (the
/// operator's rule): origin's only when it is strictly ahead, so new work
/// never starts without local work that isn't pushed; the local one when it
/// is even, ahead, or split from origin's. Only a split is noted: working
/// ahead of origin is the operator's normal way to work.
fn newer_main(repo: &Path, local: String) -> ForkPoint {
    let Some(origin) = remote_main(repo) else {
        return ForkPoint { base: local, stale: Some("origin has no main branch".to_string()) };
    };
    let stale = match (is_ancestor(repo, &origin, &local), is_ancestor(repo, &local, &origin)) {
        (Ok(true), _) => None,
        (Ok(false), Ok(false)) => Some("the local main and origin's have split"),
        // Strictly ahead, or there is no local main to compare it with.
        _ => return ForkPoint { base: origin, stale: None },
    };
    ForkPoint { base: local, stale: stale.map(str::to_string) }
}

fn local_main(repo: &Path) -> String {
    inspect(&repo.to_string_lossy()).default_branch.unwrap_or_else(|| "HEAD".to_string())
}

/// origin's main as last fetched (`origin/main`), if there is one.
pub(crate) fn remote_main(repo: &Path) -> Option<String> {
    run(repo, &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"])
        .map(|r| r.trim().to_string())
        .ok()
        .filter(|r| !r.is_empty())
        .or_else(|| {
            let named = format!("origin/{}", local_main(repo));
            run(repo, &["rev-parse", "--verify", "--quiet", &format!("refs/remotes/{named}")]).ok().map(|_| named)
        })
}

/// What work forked from `base` follows, without fetching: the newer main
/// when `base` is either main, or none was recorded, as new work forks; any
/// other branch stays its own.
// ponytail: about nine git commands a call, once per thread in the rollup
// and the turn-end overlap check; resolve it once per repository there if
// many open threads make those slow.
pub fn follows(repo: &Path, base: Option<&str>) -> String {
    let local = local_main(repo);
    match base {
        Some(b) if b != local && b != format!("origin/{local}") => b.to_string(),
        _ => newer_main(repo, local).base,
    }
}

/// Explicit, human-confirmed local merge of a signed-off Bakery-derived
/// branch into the repo's CURRENT branch — the no-`gh` integration path.
/// Refuses: a non-Bakery branch, merging a branch into itself, and a dirty
/// target tree (the merge must land on exactly what the operator sees).
/// `--no-ff` so the integration is a real, visible merge commit.
pub fn merge_local(repo: &Path, branch: &str, message: &str) -> Result<String, String> {
    if !is_bakery_derived_branch(branch) {
        return Err(format!(
            "merge refused: {branch:?} is not a Bakery-derived (baker/* or task/*) branch"
        ));
    }
    let current = run(repo, &["rev-parse", "--abbrev-ref", "HEAD"])?
        .trim()
        .to_string();
    if current == branch.trim() {
        return Err(format!(
            "merge refused: {branch:?} is already the checked-out branch"
        ));
    }
    if !porcelain_status(repo)?.is_empty() {
        return Err(format!(
            "merge refused: the target checkout ({current}) has uncommitted changes — commit or stash them first"
        ));
    }
    run(repo, &["merge", "--no-ff", "-m", message, branch])?;
    Ok(run(repo, &["rev-parse", "HEAD"])?.trim().to_string())
}

/// Local heads matching `baker/*` (short names, sorted).
pub fn baker_local_branches(repo: &Path) -> Result<Vec<String>, String> {
    let out = run(
        repo,
        &[
            "for-each-ref",
            "--format=%(refname:short)",
            "--sort=refname",
            "refs/heads/baker",
        ],
    )?;
    Ok(out
        .lines()
        .map(str::trim)
        .filter(|l| is_baker_branch(l))
        .map(str::to_string)
        .collect())
}

/// One commit on `branch` that is not reachable from `base`.
#[derive(schemars::JsonSchema, Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CommitSummary {
    pub sha: String,
    pub subject: String,
}

/// Commits on `branch` not in `base` (`base..branch`), oldest first.
pub fn commits_not_in_base(
    repo: &Path,
    branch: &str,
    base: &str,
) -> Result<Vec<CommitSummary>, String> {
    let range = format!("{base}..{branch}");
    let out = run(repo, &["log", "--reverse", "--format=%h\t%s", &range])?;
    Ok(out
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .filter_map(|l| {
            let (sha, subject) = l.split_once('\t')?;
            Some(CommitSummary {
                sha: sha.to_string(),
                subject: subject.to_string(),
            })
        })
        .collect())
}

/// `git diff --stat base...branch` (three-dot: merge-base). Empty string when identical.
pub fn diff_stat(repo: &Path, base: &str, branch: &str) -> Result<String, String> {
    let range = format!("{base}...{branch}");
    run(repo, &["diff", "--stat", &range]).map(|s| s.trim_end().to_string())
}

/// Uncommitted changes in a worktree (`git status --porcelain`).
#[derive(schemars::JsonSchema, Debug, Clone, Serialize, PartialEq, Eq)]
pub struct StatusEntry {
    /// Two-letter porcelain code, e.g. ` M`, `M `, `??`.
    pub code: String,
    pub path: String,
}

pub fn porcelain_status(worktree: &Path) -> Result<Vec<StatusEntry>, String> {
    let out = run(worktree, &["status", "--porcelain"])?;
    Ok(out
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.is_empty())
        .filter_map(|l| {
            if l.len() < 4 {
                return None;
            }
            // XY SPACE path — or rename "XY old -> new"
            let code = l[..2].to_string();
            let path = l[3..].trim().to_string();
            if path.is_empty() {
                return None;
            }
            Some(StatusEntry { code, path })
        })
        .collect())
}

/// Whether `name` is configured as a remote (`git remote`).
pub fn has_remote(repo: &Path, name: &str) -> bool {
    run(repo, &["remote"])
        .map(|s| s.lines().any(|l| l.trim() == name))
        .unwrap_or(false)
}

/// `git push -u origin <branch>` — never `--force`. Caller must pre-check baker/*.
pub fn push_set_upstream(repo: &Path, branch: &str) -> Result<String, String> {
    if !is_bakery_derived_branch(branch) {
        return Err(format!(
            "push refused: {branch:?} is not a Bakery-derived (baker/* or task/*) branch"
        ));
    }
    if !has_remote(repo, "origin") {
        return Err("no remote named origin".into());
    }
    // Capture both streams — push writes progress to stderr even on success.
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["push", "-u", "origin", branch])
        .output()
        .map_err(|e| format!("failed to run git push: {e}"))?;
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() {
        return Err(if stderr.is_empty() {
            format!("git push failed for {branch}")
        } else {
            stderr
        });
    }
    Ok(if stderr.is_empty() { stdout } else { stderr })
}

/// True when `gh` is on PATH and answers `gh --version`.
pub fn gh_available() -> bool {
    Command::new("gh")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// `gh` with `args` in `dir`: its output, or why it failed. It runs on the
/// operator's own gh login; the node holds no GitHub token.
pub fn gh(dir: &Path, args: &[&str]) -> Result<String, String> {
    if !gh_available() {
        return Err("the GitHub CLI (gh) is not installed or not on PATH".to_string());
    }
    let out = Command::new("gh")
        .current_dir(dir)
        .args(args)
        .output()
        .map_err(|e| format!("failed to run gh: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// [`gh`] answering JSON (`--json`).
pub fn gh_json(dir: &Path, args: &[&str]) -> Result<serde_json::Value, String> {
    serde_json::from_str(&gh(dir, args)?).map_err(|e| format!("gh answered oddly: {e}"))
}

/// `gh pr create --base <base> --head <branch> --title … --body …`
/// Fails closed when `gh` is missing or the branch is not baker/*.
pub fn gh_pr_create(
    repo: &Path,
    base: &str,
    branch: &str,
    title: &str,
    body: &str,
) -> Result<String, String> {
    if !is_bakery_derived_branch(branch) {
        return Err(format!(
            "pr create refused: {branch:?} is not a Bakery-derived (baker/* or task/*) branch"
        ));
    }
    if !gh_available() {
        return Err("gh not available".into());
    }
    let title = if title.trim().is_empty() {
        format!("Baker: {branch}")
    } else {
        title.trim().to_string()
    };
    let body = if body.trim().is_empty() {
        format!("Baker branch `{branch}` → `{base}`.")
    } else {
        body.trim().to_string()
    };
    let out = Command::new("gh")
        .current_dir(repo)
        .args([
            "pr", "create", "--base", base, "--head", branch, "--title", &title, "--body", &body,
        ])
        .output()
        .map_err(|e| format!("failed to run gh: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    if !out.status.success() {
        return Err(if stderr.is_empty() {
            "gh pr create failed".into()
        } else {
            stderr
        });
    }
    Ok(if stdout.is_empty() { stderr } else { stdout })
}

/// Merge the open PR for `branch` via `gh pr merge --merge` (no force, no admin).
/// Caller must require human confirm. Refuses any branch that is not
/// Bakery-derived (`baker/*` or `task/*` — route-level guards stay narrower).
pub fn gh_pr_merge(repo: &Path, branch: &str) -> Result<String, String> {
    if !is_bakery_derived_branch(branch) {
        return Err(format!(
            "merge refused: {branch:?} is not a Bakery-derived (baker/* or task/*) branch"
        ));
    }
    if branch == "main" || branch == "master" {
        return Err("merge refused: cannot merge main/master as a Baker head".into());
    }
    if !gh_available() {
        return Err("gh not available".into());
    }
    let out = Command::new("gh")
        .current_dir(repo)
        .args(["pr", "merge", branch, "--merge"])
        .output()
        .map_err(|e| format!("failed to run gh: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    if !out.status.success() {
        return Err(if stderr.is_empty() {
            "gh pr merge failed".into()
        } else {
            stderr
        });
    }
    Ok(if stdout.is_empty() { stderr } else { stdout })
}

/// `/repo/.git` → `/repo`; a bare `/repo.git` → `/repo.git` (already the root).
fn main_root_from_common_dir(common_dir: &str) -> Option<String> {
    let p = Path::new(common_dir);
    if p.file_name().map(|n| n == ".git").unwrap_or(false) {
        p.parent().map(|r| r.to_string_lossy().into_owned())
    } else {
        Some(common_dir.to_string())
    }
}

fn parse_branches(out: &str) -> Vec<String> {
    out.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

/// `refs/remotes/origin/HEAD` → `main`, else the conventional names, else HEAD's branch.
fn pick_default_branch(
    origin_head: Option<&str>,
    local: &[String],
    current: Option<&str>,
) -> Option<String> {
    if let Some(h) = origin_head {
        if let Some(name) = h.trim().strip_prefix("refs/remotes/origin/") {
            if !name.is_empty() && name != "HEAD" {
                return Some(name.to_string());
            }
        }
    }
    for candidate in ["main", "master"] {
        if local.iter().any(|b| b == candidate) {
            return Some(candidate.to_string());
        }
    }
    current
        .map(str::to_string)
        .or_else(|| local.first().cloned())
}

/// Parse `git worktree list --porcelain`. Records are blank-line separated;
/// the first record is always the main working tree.
fn parse_worktree_list(out: &str) -> Vec<WorktreeInfo> {
    let mut found: Vec<WorktreeInfo> = Vec::new();
    let mut cur: Option<WorktreeInfo> = None;

    for line in out.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            if let Some(w) = cur.take() {
                found.push(w);
            }
            continue;
        }
        let (key, rest) = match line.split_once(' ') {
            Some((k, r)) => (k, r),
            None => (line, ""),
        };
        match key {
            "worktree" => {
                if let Some(w) = cur.take() {
                    found.push(w);
                }
                cur = Some(WorktreeInfo {
                    path: rest.to_string(),
                    branch: None,
                    head: None,
                    is_main: false,
                    locked: false,
                });
            }
            "HEAD" => {
                if let Some(w) = cur.as_mut() {
                    w.head = Some(rest.to_string());
                }
            }
            "branch" => {
                if let Some(w) = cur.as_mut() {
                    w.branch = Some(rest.trim_start_matches("refs/heads/").to_string());
                }
            }
            "locked" => {
                if let Some(w) = cur.as_mut() {
                    w.locked = true;
                }
            }
            _ => {}
        }
    }
    if let Some(w) = cur.take() {
        found.push(w);
    }
    if let Some(first) = found.first_mut() {
        first.is_main = true;
    }
    found
}

/// Suggest a worktree name from the repo basename, suffixing `-2`, `-3`, … until
/// it is free. `taken` is whatever the caller considers occupied (stream ids
/// already onboarded plus directories already on disk).
pub fn suggest_name(repo_name: &str, taken: &[String]) -> String {
    first_free(repo_name, taken)
}

/// Suggest a branch name for a **new** worktree, sanitized the same way as a
/// worktree name and suffixed `-2`, `-3`, … until it does not collide with an
/// existing local branch.
///
/// WHY collisions matter here specifically: `add_worktree` checks out an
/// *existing* local branch by name rather than passing `-b` when one already
/// exists (re-`-b`ing a live branch fails). A suggestion that happened to
/// match a pre-existing, unrelated branch would therefore silently attach a
/// fresh worktree to someone else's old work instead of starting clean.
pub fn suggest_branch(base: &str, local_branches: &[String]) -> String {
    first_free(base, local_branches)
}

/// Sanitize `base`, then return it — or `base-2`, `base-3`, … — as soon as one
/// is not in `taken`. Shared by worktree names and branch names because both
/// must be free and both feed the same boring alphabet.
fn first_free(base: &str, taken: &[String]) -> String {
    let base = sanitize_name(base);
    if !taken.iter().any(|t| t == &base) {
        return base;
    }
    (2..1000)
        .map(|n| format!("{base}-{n}"))
        .find(|c| !taken.iter().any(|t| t == c))
        .unwrap_or_else(|| format!("{base}-x"))
}

/// Worktree names become directory names, branch suffixes and stream ids, so
/// keep them to a boring alphabet.
pub fn sanitize_name(raw: &str) -> String {
    let mut cleaned = String::new();
    for c in raw.trim().chars() {
        let c = if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' { c } else { '-' };
        // ".." never makes a branch name.
        if !(c == '.' && cleaned.ends_with('.')) {
            cleaned.push(c);
        }
    }
    let cleaned = cleaned.trim_matches(['-', '.'].as_slice()).to_string();
    if cleaned.is_empty() {
        "worktree".to_string()
    } else {
        cleaned
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A repository and its origin, with one commit on origin only: the
    /// local main is behind.
    pub(crate) fn behind_its_origin(dir: &Path) -> std::path::PathBuf {
        use crate::testutil::{make_repo, run_git};
        let up = make_repo(&dir.join("up"));
        let origin = dir.join("origin.git");
        run_git(dir, &["clone", "-q", "--bare", &up.to_string_lossy(), &origin.to_string_lossy()]);
        run_git(dir, &["clone", "-q", &origin.to_string_lossy(), &dir.join("local").to_string_lossy()]);
        std::fs::write(up.join("only-on-origin.txt"), "new\n").unwrap();
        run_git(&up, &["add", "."]);
        run_git(&up, &["commit", "-qm", "on origin"]);
        run_git(&up, &["push", "-q", &origin.to_string_lossy(), "main"]);
        let local = dir.join("local");
        run_git(&local, &["config", "user.email", "t@example.com"]);
        run_git(&local, &["config", "user.name", "t"]);
        local
    }

    /// Rule 2: a fork starts from the newer main, just fetched: origin's when
    /// it is strictly ahead, else the local one (even, ahead, or split, and
    /// only a split says so); with no remote, or one that can't be reached,
    /// from the local main, and why.
    #[test]
    fn a_fork_starts_from_the_newer_main_just_fetched() {
        use crate::testutil::run_git;
        let dir = tempfile::tempdir().unwrap();
        let local = behind_its_origin(dir.path());
        assert_eq!(fresh_main(&local), ForkPoint { base: "origin/main".into(), stale: None }, "origin ahead");
        assert!(run(&local, &["cat-file", "-e", "origin/main:only-on-origin.txt"]).is_ok(), "fetched");
        assert!(last_fetch(&local).is_some_and(|f| f.error.is_none()));

        let mine = |stale: Option<&str>| ForkPoint { base: "main".into(), stale: stale.map(str::to_string) };
        run_git(&local, &["merge", "-q", "--ff-only", "origin/main"]);
        assert_eq!(fresh_main(&local), mine(None), "even");
        run_git(&local, &["commit", "-q", "--allow-empty", "-m", "only here"]);
        assert_eq!(fresh_main(&local), mine(None), "ahead");
        let up = dir.path().join("up");
        run_git(&up, &["commit", "-q", "--allow-empty", "-m", "only there"]);
        run_git(&up, &["push", "-q", &dir.path().join("origin.git").to_string_lossy(), "main"]);
        assert_eq!(fresh_main(&local), mine(Some("the local main and origin's have split")));

        let alone = crate::testutil::make_repo(&dir.path().join("alone"));
        let fork = fresh_main(&alone);
        assert_eq!((fork.base.as_str(), fork.stale.as_deref()), ("main", Some("the repository has no remote")));
        crate::testutil::run_git(&alone, &["remote", "add", "origin", "/nowhere/at/all.git"]);
        let fork = fresh_main(&alone);
        assert!(fork.base == "main" && fork.stale.as_deref().is_some_and(|s| s.starts_with("couldn't fetch origin")), "{fork:?}");
        assert!(last_fetch(&alone).is_some_and(|f| f.error.is_some()), "a failed fetch is remembered too");
    }

    const PORCELAIN: &str = "\
worktree /Users/x/dev/app
HEAD abc123
branch refs/heads/main

worktree /Users/x/.bakery/worktrees/feature
HEAD def456
branch refs/heads/bakery/feature
locked

worktree /Users/x/detached
HEAD 999aaa
detached
";

    #[test]
    fn parses_porcelain_worktree_list() {
        let w = parse_worktree_list(PORCELAIN);
        assert_eq!(w.len(), 3);
        assert_eq!(w[0].path, "/Users/x/dev/app");
        assert_eq!(w[0].branch.as_deref(), Some("main"));
        assert!(w[0].is_main, "first record is the main working tree");
        assert_eq!(w[1].branch.as_deref(), Some("bakery/feature"));
        assert!(w[1].locked);
        assert!(!w[1].is_main);
        assert_eq!(w[2].branch, None, "detached worktree has no branch");
    }

    #[test]
    fn parses_last_record_without_trailing_blank_line() {
        let w = parse_worktree_list("worktree /a\nHEAD 1\nbranch refs/heads/b");
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].branch.as_deref(), Some("b"));
    }

    #[test]
    fn empty_worktree_list_is_empty() {
        assert!(parse_worktree_list("").is_empty());
    }

    #[test]
    fn default_branch_prefers_origin_head() {
        let local = vec!["main".to_string(), "trunk".to_string()];
        assert_eq!(
            pick_default_branch(Some("refs/remotes/origin/trunk\n"), &local, Some("main")),
            Some("trunk".to_string())
        );
    }

    #[test]
    fn default_branch_falls_back_to_main_then_master() {
        let with_main = vec!["dev".to_string(), "main".to_string()];
        assert_eq!(
            pick_default_branch(None, &with_main, Some("dev")),
            Some("main".into())
        );
        let with_master = vec!["dev".to_string(), "master".to_string()];
        assert_eq!(
            pick_default_branch(None, &with_master, Some("dev")),
            Some("master".into())
        );
    }

    #[test]
    fn default_branch_falls_back_to_current_when_unconventional() {
        let local = vec!["dev".to_string()];
        assert_eq!(
            pick_default_branch(None, &local, Some("dev")),
            Some("dev".into())
        );
        assert_eq!(pick_default_branch(None, &[], None), None);
    }

    #[test]
    fn common_dir_maps_to_main_root() {
        assert_eq!(
            main_root_from_common_dir("/Users/x/dev/app/.git"),
            Some("/Users/x/dev/app".into())
        );
        assert_eq!(
            main_root_from_common_dir("/srv/bare.git"),
            Some("/srv/bare.git".into())
        );
    }

    #[test]
    fn suggest_name_uses_basename_then_suffixes() {
        assert_eq!(suggest_name("thebakery", &[]), "thebakery");
        assert_eq!(
            suggest_name("thebakery", &["thebakery".into()]),
            "thebakery-2"
        );
        assert_eq!(
            suggest_name("thebakery", &["thebakery".into(), "thebakery-2".into()]),
            "thebakery-3"
        );
    }

    #[test]
    fn suggest_branch_uses_the_sanitized_base_when_free() {
        assert_eq!(suggest_branch("widget", &[]), "widget");
        assert_eq!(suggest_branch("my repo!", &[]), "my-repo");
    }

    #[test]
    fn suggest_branch_avoids_an_existing_local_branch() {
        let local = vec!["widget".to_string()];
        assert_eq!(suggest_branch("widget", &local), "widget-2");
        let local2 = vec!["widget".to_string(), "widget-2".to_string()];
        assert_eq!(suggest_branch("widget", &local2), "widget-3");
    }

    #[test]
    fn sanitize_name_strips_path_and_shell_hostile_chars() {
        assert_eq!(sanitize_name("my repo/../etc"), "my-repo-.-etc");
        assert_eq!(sanitize_name("Wait... what"), "Wait.-what", "no .. in a branch name");
        assert_eq!(sanitize_name("  ok_name-1 "), "ok_name-1");
        assert_eq!(sanitize_name("///"), "worktree");
        assert!(
            !sanitize_name("a/b").contains('/'),
            "no path separators survive"
        );
    }

    #[test]
    fn inspect_non_repo_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let info = inspect(dir.path().to_str().unwrap());
        assert!(!info.is_git);
        assert!(info.error.is_some(), "a non-repo must say why, not guess");
        assert!(info.local_branches.is_empty());
    }

    #[test]
    fn inspect_missing_path_fails_closed() {
        let info = inspect("/definitely/not/here/at/all");
        assert!(!info.is_git);
        assert!(info.error.unwrap().contains("not a directory"));
    }

    #[test]
    fn hook_guards_are_one_pair_and_not_stacked() {
        let fresh = hook_guard_args(&[]);
        assert_eq!(fresh.len(), 4);
        assert!(fresh[1].starts_with("core.hooksPath="));
        assert!(!fresh[1].contains("/dev/null"), "hooksPath is an empty directory");
        assert_eq!(fresh[3], "core.fsmonitor=false");
        let path = fresh[1].trim_start_matches("core.hooksPath=");
        assert!(std::path::Path::new(path).is_dir());
        let again = hook_guard_args(&["-c", &fresh[1], "-c", "core.fsmonitor=false", "status"]);
        assert!(again.is_empty(), "a caller that already passes the pair does not get a second: {again:?}");
    }

    /// A hook in the repository does not run when the node is the one
    /// invoking git. The empty hooks directory is the reason.
    #[test]
    fn node_git_does_not_run_a_repository_hook() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git").arg("-C").arg(&repo).args(args).output().unwrap();
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.email", "t@example.com"]);
        git(&["config", "user.name", "t"]);
        std::fs::write(repo.join("a.txt"), "a\n").unwrap();
        git(&["add", "a.txt"]);
        git(&["commit", "-qm", "one"]);
        let hooks = repo.join(".git/hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        let marker = dir.path().join("hook-ran");
        std::fs::write(
            hooks.join("post-checkout"),
            format!("#!/bin/sh\necho ran > '{}'\n", marker.display()),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(hooks.join("post-checkout"), std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        run(&repo, &["checkout", "-q", "-B", "side"]).unwrap();
        assert!(!marker.exists(), "the node's git ran the repository hook");
    }
}
