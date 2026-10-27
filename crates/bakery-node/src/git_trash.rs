//! The Bakery's Trash for unsaved work. Before a worktree or a branch the
//! Bakery made goes, work there that is in no main is kept in its own
//! repository under a hidden ref, as grok-build keeps a worktree's
//! snapshot: `refs/bakery/trash/<name>/<UTC stamp>/head`, the branch's tip,
//! and, when there were uncommitted changes, `…/changes`, a commit on top
//! of it holding them (untracked files too, ignored ones not). Nothing goes
//! unless each was written and read back. The Trash lists the entries; Put
//! back makes the branch and a worktree again, its changes uncommitted,
//! exactly as they were. An entry stays until the operator empties it, one
//! entry or all: its refs go, and git's own housekeeping drops the commits
//! nothing else holds.

use crate::git::StatusEntry;
use crate::state::{same_path, AppState};
use axum::{extract::State, http::StatusCode, Json};
use chrono::{DateTime, NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

type Fail = (StatusCode, String);

const ROOT: &str = "refs/bakery/trash";
const STAMP: &str = "%Y%m%dT%H%M%SZ";

/// Every git call here reads files as git stores them, whatever the
/// operator's config says: a snapshot and its put back agree byte for byte.
const CONFIG: &[&str] = &["-c", "core.autocrlf=false", "-c", "core.safecrlf=false", "-c", "core.fsmonitor=false", "-c", "core.symlinks=true"];

/// Who a snapshot commit is by: set per call, never written to a config.
const IDENTITY: [(&str, &str); 4] = [
    ("GIT_AUTHOR_NAME", "Bakery"),
    ("GIT_AUTHOR_EMAIL", "trash@bakery.local"),
    ("GIT_COMMITTER_NAME", "Bakery"),
    ("GIT_COMMITTER_EMAIL", "trash@bakery.local"),
];

fn git_env(dir: &Path, args: &[&str], env: &[(&str, &OsStr)]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(CONFIG)
        .args(args)
        .envs(env.iter().copied())
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("failed to run git: {e}"))?;
    if !out.status.success() {
        let said = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if said.is_empty() { format!("git {} failed", args.join(" ")) } else { said });
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    git_env(dir, args, &[])
}

/// Now, as an entry's ref names it.
pub(crate) fn stamp_now() -> String {
    Utc::now().format(STAMP).to_string()
}

/// An entry's ref, split: its name, its stamp, and which of its two refs.
fn parts(refname: &str) -> Option<(&str, &str, &str)> {
    let rest = refname.strip_prefix(ROOT)?.strip_prefix('/')?;
    let (at, kind) = rest.rsplit_once('/')?;
    let (name, stamp) = at.rsplit_once('/')?;
    when(stamp)?;
    Some((name, stamp, kind))
}

fn when(stamp: &str) -> Option<DateTime<Utc>> {
    NaiveDateTime::parse_from_str(stamp, STAMP).ok().map(|t| t.and_utc())
}

/// What a worktree is checked out at, or a branch's tip.
fn tip(repo: &Path, branch: &str, worktree: Option<&Path>) -> Result<String, String> {
    match worktree {
        Some(at) => git(at, &["rev-parse", "--verify", "HEAD^{commit}"]),
        None => git(repo, &["rev-parse", "--verify", &format!("refs/heads/{branch}^{{commit}}")]),
    }
}

/// How many commits from `tip` are in no main: the repository's local
/// default branch, or every one when it has none.
fn outside_main(repo: &Path, tip: &str) -> Result<usize, String> {
    let main = crate::git::inspect(&repo.to_string_lossy())
        .default_branch
        .map(|m| format!("refs/heads/{m}"))
        .filter(|m| git(repo, &["rev-parse", "--verify", "--quiet", m]).is_ok());
    let mut args = vec!["rev-list", "--count", tip];
    if let Some(main) = &main {
        args.extend(["--not", main.as_str()]);
    }
    git(repo, &args)?.parse().map_err(|e| format!("git rev-list: {e}"))
}

/// The worktree as it stands, as a tree: a scratch index seeded with HEAD,
/// every change added (untracked files too, ignored ones not), written
/// out. The worktree's own index is never touched.
fn tree_of(worktree: &Path) -> Result<String, String> {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    struct Scratch(PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
            let _ = std::fs::remove_file(self.0.with_extension("lock"));
        }
    }
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let index = Scratch(std::env::temp_dir().join(format!("bakery-trash-index-{}-{seq}", std::process::id())));
    let env = [("GIT_INDEX_FILE", index.0.as_os_str())];
    git_env(worktree, &["read-tree", "HEAD"], &env)?;
    git_env(worktree, &["add", "-A"], &env)?;
    git_env(worktree, &["write-tree"], &env)
}

/// Writes `name` at `sha` where there's none yet, and reads it back: an
/// entry is never overwritten.
fn write_ref(repo: &Path, name: &str, sha: &str) -> Result<(), String> {
    let wrote = git(repo, &["update-ref", name, sha, ""]);
    match git(repo, &["rev-parse", "--verify", "--quiet", name]) {
        Ok(at) if at == sha => Ok(()),
        _ => Err(format!(
            "{name} couldn't be written: {}",
            wrote.err().unwrap_or_else(|| "it reads back as another commit".to_string())
        )),
    }
}

/// What a branch, or the worktree checked out on it, holds that is in no
/// main, as a plan shows it.
pub(crate) struct Unsaved {
    pub(crate) commits: usize,
    pub(crate) changes: Vec<StatusEntry>,
}

pub(crate) fn unsaved(repo: &Path, branch: &str, worktree: Option<&Path>) -> Result<Unsaved, String> {
    let commits = outside_main(repo, &tip(repo, branch, worktree)?)?;
    let changes = worktree.map(crate::git::porcelain_status).transpose()?.unwrap_or_default();
    Ok(Unsaved { commits, changes })
}

/// Keeps what `branch` (or `worktree`, checked out on it) holds that is in
/// no main as the entry `name` at `stamp`; answers whether there was any.
/// Fails when it can't be read, written or read back, and the caller then
/// keeps the worktree or branch.
pub(crate) fn keep(repo: &Path, name: &str, branch: &str, worktree: Option<&Path>, stamp: &str) -> Result<bool, String> {
    let kept = || -> Result<bool, String> {
        let tip = tip(repo, branch, worktree)?;
        let tree = worktree.map(tree_of).transpose()?;
        let at_tip = git(repo, &["rev-parse", &format!("{tip}^{{tree}}")])?;
        let changed = tree.filter(|t| *t != at_tip);
        if changed.is_none() && outside_main(repo, &tip)? == 0 {
            return Ok(false);
        }
        let at = format!("{ROOT}/{name}/{stamp}");
        write_ref(repo, &format!("{at}/head"), &tip)?;
        if let Some(tree) = changed {
            let message = format!("Bakery Trash: {name}'s uncommitted changes, {stamp}");
            let env = IDENTITY.map(|(k, v)| (k, OsStr::new(v)));
            let snap = git_env(repo, &["commit-tree", "--no-gpg-sign", &tree, "-p", &tip, "-m", &message], &env)?;
            write_ref(repo, &format!("{at}/changes"), &snap)?;
        }
        Ok(true)
    };
    kept().map_err(|e| format!("its unsaved work couldn't be kept in git, so it stays: {e}"))
}

/// The name an entry goes by: its branch, or its worktree's folder.
pub(crate) fn name_of(branch: &str, worktree: &Path) -> String {
    match branch.is_empty() {
        true => worktree.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        false => branch.to_string(),
    }
}

/// Deletes a branch whose work is kept; git refuses one checked out anywhere.
pub(crate) fn drop_branch(repo: &Path, branch: &str) -> Result<(), String> {
    git(repo, &["branch", "-D", branch]).map(|_| ())
}

/// One entry of the Trash.
#[derive(schemars::JsonSchema, Debug, Clone, PartialEq, Serialize)]
pub(crate) struct TrashEntry {
    /// The repository it is kept in.
    pub(crate) repo: String,
    /// The branch it was, or its worktree's folder when it had none.
    pub(crate) name: String,
    /// When it was kept, UTC, as its ref names it.
    pub(crate) stamp: String,
    /// Its commits that are in no main.
    pub(crate) commits: usize,
    /// Its uncommitted changes, by path: A added, M modified, D deleted.
    pub(crate) changes: Vec<StatusEntry>,
}

fn entries(repo: &Path) -> Result<Vec<TrashEntry>, String> {
    let listed = git(repo, &["for-each-ref", "--format=%(refname)", ROOT])?;
    // Each entry, and whether it holds uncommitted changes.
    let mut found: BTreeMap<(&str, &str), (bool, bool)> = BTreeMap::new();
    for (name, stamp, kind) in listed.lines().filter_map(parts) {
        let seen = found.entry((name, stamp)).or_default();
        match kind {
            "head" => seen.0 = true,
            "changes" => seen.1 = true,
            _ => {}
        }
    }
    let mut out = Vec::new();
    for ((name, stamp), (head, changed)) in found {
        if !head {
            continue;
        }
        let at = format!("{ROOT}/{name}/{stamp}");
        let changes = match changed {
            false => Vec::new(),
            true => git(repo, &["diff", "--name-status", "--no-renames", &format!("{at}/head"), &format!("{at}/changes")])?
                .lines()
                .filter_map(|l| l.split_once('\t'))
                .map(|(code, path)| StatusEntry { code: code.to_string(), path: path.to_string() })
                .collect(),
        };
        out.push(TrashEntry {
            repo: repo.display().to_string(),
            name: name.to_string(),
            stamp: stamp.to_string(),
            commits: outside_main(repo, &format!("{at}/head"))?,
            changes,
        });
    }
    Ok(out)
}

/// Every repository a thread of the Bakery worked in: where entries are.
fn repos(state: &AppState) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = Vec::new();
    for s in crate::state::known_streams(state).unwrap_or_default() {
        let Ok(at) = Path::new(&s.repo_root).canonicalize() else {
            continue;
        };
        if !s.repo_root.is_empty() && !found.contains(&at) {
            found.push(at);
        }
    }
    found
}

/// Empties the Trash of one entry; answers whether there was one. Only refs
/// the Trash lists are touched, whatever the name given, and the changes go
/// before the tip: an entry that stays whole is still listed and can be
/// emptied again.
pub(crate) fn empty(repo: &Path, name: &str, stamp: &str) -> Result<bool, String> {
    let listed = git(repo, &["for-each-ref", "--format=%(refname)", ROOT])?;
    let at = format!("{ROOT}/{name}/{stamp}");
    let mut found = false;
    for kind in ["changes", "head"] {
        let refname = format!("{at}/{kind}");
        if listed.lines().any(|l| l == refname) {
            git(repo, &["update-ref", "-d", &refname])?;
            found = true;
        }
    }
    Ok(found)
}

/// A branch and worktree put back.
#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct PutBack {
    pub(crate) branch: String,
    pub(crate) worktree: String,
}

/// Makes the entry's branch again at its tip, and a worktree on it under
/// `under`, its changes uncommitted, exactly as they were (read back as a
/// tree); the entry then leaves the Trash. One that doesn't come back
/// whole stays in it.
pub(crate) fn put_back(repo: &Path, name: &str, stamp: &str, under: &Path) -> Result<PutBack, String> {
    let at = format!("{ROOT}/{name}/{stamp}");
    let head = git(repo, &["rev-parse", "--verify", &format!("{at}/head^{{commit}}")])
        .map_err(|_| format!("nothing is kept as {name} at {stamp}"))?;
    let changes = git(repo, &["rev-parse", "--verify", "--quiet", &format!("{at}/changes^{{commit}}")]).ok();
    match git(repo, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{name}")]) {
        Ok(tip) if tip != head => return Err(format!("a branch {name} is there already, at another commit")),
        Ok(_) => {}
        Err(_) => {
            git(repo, &["branch", name, &head])?;
        }
    }
    let folder = crate::git::sanitize_name(name);
    let path = (1..)
        .map(|n| under.join(if n == 1 { folder.clone() } else { format!("{folder}-{n}") }))
        .find(|p| !p.exists())
        .expect("a free folder");
    git(repo, &["worktree", "add", &path.to_string_lossy(), name])?;
    if let Some(snap) = &changes {
        // The snapshot's files in place, the index back at the branch's tip:
        // its changes show as uncommitted, untracked files as untracked.
        git(&path, &["read-tree", "--reset", "-u", snap])?;
        git(&path, &["reset", "-q"])?;
        let want = git(repo, &["rev-parse", &format!("{snap}^{{tree}}")])?;
        if tree_of(&path)? != want {
            return Err(format!("{name}'s changes didn't come back exactly in {}; it stays in the Trash", path.display()));
        }
    }
    for kind in ["head", "changes"] {
        let _ = git(repo, &["update-ref", "-d", &format!("{at}/{kind}")]);
    }
    Ok(PutBack { branch: name.to_string(), worktree: path.display().to_string() })
}

fn operator_only() -> Result<(), Fail> {
    match crate::actor::agent_calling() {
        true => Err((StatusCode::FORBIDDEN, "the Trash is the operator's alone".to_string())),
        false => Ok(()),
    }
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct TrashList {
    /// Newest first.
    pub(crate) entries: Vec<TrashEntry>,
}

/// `GET /trash`: the unsaved work deletes kept in git, in every repository
/// the Bakery worked in.
pub(crate) async fn list_route(State(st): State<AppState>) -> Result<Json<TrashList>, Fail> {
    operator_only()?;
    crate::actor::spawn_blocking(move || {
        let mut entries = Vec::new();
        for repo in repos(&st) {
            entries.extend(entries_or_fail(&repo)?);
        }
        entries.sort_by(|a: &TrashEntry, b| b.stamp.cmp(&a.stamp).then_with(|| a.name.cmp(&b.name)));
        Ok(Json(TrashList { entries }))
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
}

fn entries_or_fail(repo: &Path) -> Result<Vec<TrashEntry>, Fail> {
    entries(repo).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{}: {e}", repo.display())))
}

/// An entry of the Trash, as it lists it.
#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct TrashRef {
    pub(crate) repo: String,
    pub(crate) name: String,
    pub(crate) stamp: String,
}

/// `POST /trash/put-back`: an entry's branch and a worktree made again.
pub(crate) async fn put_back_route(State(st): State<AppState>, Json(req): Json<TrashRef>) -> Result<Json<PutBack>, Fail> {
    operator_only()?;
    crate::actor::spawn_blocking(move || {
        let repo = repos(&st)
            .into_iter()
            .find(|r| same_path(&r.to_string_lossy(), &req.repo))
            .ok_or_else(|| (StatusCode::NOT_FOUND, format!("{} is no repository the Bakery worked in", req.repo)))?;
        put_back(&repo, &req.name, &req.stamp, &st.worktree_base)
            .map(Json)
            .map_err(|e| (StatusCode::CONFLICT, e))
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
}

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct EmptyReq {
    /// The entries to empty, as the Trash lists them: one, or every one it
    /// shows. An entry kept since is not touched.
    pub(crate) entries: Vec<TrashRef>,
    /// Must be true: emptying can't be undone, and the operator was asked
    /// first.
    #[serde(default)]
    pub(crate) confirm: bool,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct Emptied {
    /// Entries that went.
    pub(crate) emptied: usize,
    /// Entries that couldn't, each with why.
    pub(crate) refused: Vec<String>,
}

/// `POST /trash/empty`: entries of the Trash gone for good, the unsaved work
/// in them with them.
pub(crate) async fn empty_route(State(st): State<AppState>, Json(req): Json<EmptyReq>) -> Result<Json<Emptied>, Fail> {
    operator_only()?;
    if !req.confirm {
        return Err((StatusCode::BAD_REQUEST, "emptying the Trash can't be undone: confirm it first".to_string()));
    }
    crate::actor::spawn_blocking(move || {
        let known = repos(&st);
        let mut out = Emptied { emptied: 0, refused: Vec::new() };
        for entry in &req.entries {
            let Some(repo) = known.iter().find(|r| same_path(&r.to_string_lossy(), &entry.repo)) else {
                out.refused.push(format!("{} is no repository the Bakery worked in", entry.repo));
                continue;
            };
            match empty(repo, &entry.name, &entry.stamp) {
                Ok(true) => out.emptied += 1,
                Ok(false) => {}
                Err(e) => out.refused.push(format!("{} in {}: {e}", entry.name, entry.repo)),
            }
        }
        Ok(Json(out))
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{make_repo, run_git};

    fn porcelain(at: &Path) -> Vec<String> {
        let mut lines: Vec<String> = crate::git::run(at, &["status", "--porcelain"]).unwrap().lines().map(str::to_string).collect();
        lines.sort();
        lines
    }

    /// A worktree's commits and uncommitted changes (modified, staged, new,
    /// deleted) are kept, ignored files not; put back, the branch is at its
    /// tip and every file is exactly as it was, the changes uncommitted.
    #[test]
    fn kept_work_comes_back_exactly() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("app"));
        std::fs::write(repo.join(".gitignore"), "*.log\n").unwrap();
        std::fs::write(repo.join("gone.txt"), "bye\n").unwrap();
        run_git(&repo, &["add", "."]);
        run_git(&repo, &["commit", "-qm", "more"]);
        let wt = dir.path().join("wt");
        run_git(&repo, &["worktree", "add", "-q", "-b", "feat", &wt.to_string_lossy()]);
        std::fs::write(wt.join("c.txt"), "committed\n").unwrap();
        run_git(&wt, &["add", "c.txt"]);
        run_git(&wt, &["commit", "-qm", "on feat"]);
        let tip = git(&wt, &["rev-parse", "HEAD"]).unwrap();
        std::fs::write(wt.join("f.txt"), "line one\nline two\n").unwrap();
        std::fs::write(wt.join("staged.txt"), "staged\n").unwrap();
        run_git(&wt, &["add", "staged.txt"]);
        std::fs::write(wt.join("new.txt"), "new\n").unwrap();
        std::fs::remove_file(wt.join("gone.txt")).unwrap();
        std::fs::write(wt.join("build.log"), "noise\n").unwrap();

        let stamp = "20261014T120000Z";
        assert!(keep(&repo, "feat", "feat", Some(&wt), stamp).unwrap());
        run_git(&repo, &["worktree", "remove", "--force", &wt.to_string_lossy()]);
        drop_branch(&repo, "feat").unwrap();

        let listed = entries(&repo).unwrap();
        assert_eq!(listed.len(), 1, "{listed:?}");
        assert_eq!((listed[0].name.as_str(), listed[0].stamp.as_str(), listed[0].commits), ("feat", stamp, 1));
        let changed: Vec<(String, String)> = listed[0].changes.iter().map(|c| (c.code.clone(), c.path.clone())).collect();
        let want = [("M", "f.txt"), ("D", "gone.txt"), ("A", "new.txt"), ("A", "staged.txt")];
        assert_eq!(changed, want.map(|(c, p)| (c.to_string(), p.to_string())), "ignored files aren't kept");

        let under = dir.path().join("worktrees");
        let back = put_back(&repo, "feat", stamp, &under).unwrap();
        let at = Path::new(&back.worktree);
        assert_eq!(git(at, &["rev-parse", "HEAD"]).unwrap(), tip);
        assert_eq!(git(at, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap(), "feat");
        assert_eq!(std::fs::read_to_string(at.join("f.txt")).unwrap(), "line one\nline two\n");
        assert_eq!(std::fs::read_to_string(at.join("new.txt")).unwrap(), "new\n");
        assert_eq!(std::fs::read_to_string(at.join("staged.txt")).unwrap(), "staged\n");
        assert_eq!(std::fs::read_to_string(at.join("c.txt")).unwrap(), "committed\n");
        assert!(!at.join("gone.txt").exists() && !at.join("build.log").exists());
        assert_eq!(porcelain(at), [" D gone.txt", " M f.txt", "?? new.txt", "?? staged.txt"], "uncommitted, as they were");
        assert!(entries(&repo).unwrap().is_empty(), "it left the Trash");
    }

    /// A branch with nothing outside main keeps nothing; one with commits
    /// keeps its tip alone, and an entry is never overwritten.
    #[test]
    fn only_work_outside_main_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("app"));
        run_git(&repo, &["branch", "merged"]);
        assert!(!keep(&repo, "merged", "merged", None, "20261014T120000Z").unwrap());
        run_git(&repo, &["checkout", "-q", "-b", "ahead"]);
        std::fs::write(repo.join("g.txt"), "more\n").unwrap();
        run_git(&repo, &["add", "."]);
        run_git(&repo, &["commit", "-qm", "ahead"]);
        run_git(&repo, &["checkout", "-q", "main"]);
        assert!(keep(&repo, "ahead", "ahead", None, "20261014T120000Z").unwrap());
        assert!(keep(&repo, "ahead", "ahead", None, "20261014T120000Z").unwrap(), "the same tip again is the same entry");
        let listed = entries(&repo).unwrap();
        assert_eq!(listed.len(), 1);
        assert!(listed[0].changes.is_empty() && listed[0].commits == 1);
        run_git(&repo, &["checkout", "-q", "-b", "other"]);
        std::fs::write(repo.join("h.txt"), "other\n").unwrap();
        run_git(&repo, &["add", "."]);
        run_git(&repo, &["commit", "-qm", "other"]);
        let other = keep(&repo, "ahead", "other", None, "20261014T120000Z").unwrap_err();
        assert!(other.contains("couldn't be kept"), "{other}");
    }

    fn refs(repo: &Path) -> Vec<String> {
        git(repo, &["for-each-ref", "--format=%(refname)", ROOT]).unwrap().lines().map(str::to_string).collect()
    }

    /// Emptying takes an entry's changes and its tip and nothing else: not
    /// another entry, and not a ref a crafted name points at.
    #[test]
    fn emptying_an_entry_takes_its_refs_and_only_those() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("app"));
        for stamp in ["20250101T000000Z", "20261014T120000Z"] {
            run_git(&repo, &["update-ref", &format!("{ROOT}/x/{stamp}/head"), "HEAD"]);
            run_git(&repo, &["update-ref", &format!("{ROOT}/x/{stamp}/changes"), "HEAD"]);
        }
        run_git(&repo, &["update-ref", &format!("{ROOT}/y/20261014T120000Z/head"), "HEAD"]);
        assert_eq!(entries(&repo).unwrap().len(), 3, "an entry of any age is listed");

        assert!(!empty(&repo, "../../heads/main", "20261014T120000Z").unwrap(), "no such entry");
        assert!(!empty(&repo, "x", "20261015T000000Z").unwrap(), "no such entry");
        assert!(git(&repo, &["rev-parse", "--verify", "refs/heads/main"]).is_ok(), "no ref but the Trash's");
        assert!(empty(&repo, "x", "20250101T000000Z").unwrap());
        assert_eq!(
            refs(&repo),
            [
                format!("{ROOT}/x/20261014T120000Z/changes"),
                format!("{ROOT}/x/20261014T120000Z/head"),
                format!("{ROOT}/y/20261014T120000Z/head"),
            ]
        );
        assert!(!empty(&repo, "x", "20250101T000000Z").unwrap(), "already gone");
    }

    /// The operator empties one entry, or every entry the Trash shows, only
    /// after confirming; an agent can't. An entry kept since the Trash was
    /// shown is not touched.
    #[tokio::test]
    async fn the_operator_empties_one_entry_or_all_after_confirming() {
        use crate::testutil::{open_req, state_with};
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("app"));
        let state = state_with(dir.path(), std::sync::Arc::new(bakery_agent::StubModel));
        let ctx = crate::routes::project::open_stream(&state, &open_req(&repo, "trash")).unwrap().context;
        let repo = Path::new(&ctx.repo_root).canonicalize().unwrap();
        let (old, mid, new) = ("20250101T000000Z", "20261013T120000Z", "20261014T120000Z");
        for (name, stamp) in [("a", old), ("b", mid), ("c", new)] {
            run_git(&repo, &["update-ref", &format!("{ROOT}/{name}/{stamp}/head"), "HEAD"]);
        }
        let listed = || -> Vec<TrashEntry> { entries(&repo).unwrap() };
        let entry = |name: &str, stamp: &str| TrashRef { repo: repo.display().to_string(), name: name.into(), stamp: stamp.into() };
        let call = |entries: Vec<TrashRef>, confirm: bool| empty_route(State(state.clone()), Json(EmptyReq { entries, confirm }));

        let agent = crate::actor::agent(&ctx.stream_id, crate::node::Seat::Orchestrator);
        let refused = crate::actor::scope(agent, call(vec![entry("a", old)], true)).await;
        assert_eq!(refused.unwrap_err().0, StatusCode::FORBIDDEN);
        let unasked = call(vec![entry("a", old)], false).await;
        assert_eq!(unasked.unwrap_err().0, StatusCode::BAD_REQUEST);
        assert_eq!(listed().len(), 3, "nothing went");

        let Json(one) = call(vec![entry("a", old)], true).await.unwrap();
        assert_eq!((one.emptied, one.refused.len()), (1, 0));
        let strangers = TrashRef { repo: dir.path().display().to_string(), name: "b".into(), stamp: mid.into() };
        let Json(none) = call(vec![strangers, entry("a", old)], true).await.unwrap();
        assert_eq!(none.emptied, 0, "the first is no repository of the Bakery's, the second already went");
        assert!(none.refused.len() == 1 && none.refused[0].contains("no repository"), "{:?}", none.refused);
        assert_eq!(listed().len(), 2);

        // The Trash showed b and c; d was kept after.
        run_git(&repo, &["update-ref", &format!("{ROOT}/d/{new}/head"), "HEAD"]);
        let Json(all) = call(vec![entry("b", mid), entry("c", new)], true).await.unwrap();
        assert_eq!((all.emptied, all.refused.len()), (2, 0));
        assert_eq!(listed().iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["d"]);
    }
}
