//! The operator's git client over HTTP, and so the Baker's (the route
//! catalog): status, branches, log, diff and fetch to read; stage, commit,
//! branch, switch and pull to change; push, pull request and merge, which
//! leave the machine or rewrite a branch, so the Baker's gate always asks
//! first.
//!
//! Every route works on one conversation's checkout, named by `stream_id`.
//! Nothing here forces: no force-push, no reset, no rebase; a merge that
//! conflicts is aborted and reported, leaving the branch as it was. Git runs
//! in the node, outside any sandbox. `git::run` turns the repository's hooks
//! and fsmonitor off for every such command, so a hook planted in the
//! checkout never runs with the node's reach. Git a Baker, a Baker thread,
//! or a chef runs inside its room is a different path, and that git may run
//! the project's hooks. Git the operator types in a host terminal is too.

use crate::state::{context_for, AppState};
use axum::{
    extract::{Query, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const MAX_DIFF_BYTES: usize = 256 * 1024;
/// The most of a merge preview's diff, and of its files, an answer carries:
/// within what an agent's tool result holds.
const MERGE_DIFF_BYTES: usize = 32 * 1024;
const MERGE_FILES_SHOWN: usize = 200;

type Reply<T> = Result<Json<T>, (StatusCode, String)>;

fn bad(e: String) -> (StatusCode, String) {
    (StatusCode::BAD_REQUEST, e)
}

/// The conversation's checkout.
pub(crate) fn checkout(state: &AppState, stream_id: &str) -> Result<PathBuf, String> {
    let ctx = context_for(state, stream_id)?;
    if ctx.worktree_path.trim().is_empty() {
        return Err("this conversation has no checkout".to_string());
    }
    let dir = PathBuf::from(&ctx.worktree_path);
    crate::git::run(&dir, &["rev-parse", "--is-inside-work-tree"])
        .map_err(|_| format!("{} is not a git checkout", dir.display()))?;
    Ok(dir)
}

/// [`checkout`], for a route that changes it. An agent in place on the
/// Bakery's own repo only reads it (rule 1): changing it takes a thread with
/// its own worktree. The operator's own changes go through.
fn writable_checkout(state: &AppState, stream_id: &str) -> Result<PathBuf, String> {
    if crate::actor::agent_calling() && !crate::tools::commands_write_workspace(state, stream_id) {
        return Err("this conversation only reads its checkout here: change it from a thread with its own worktree".to_string());
    }
    checkout(state, stream_id)
}

/// An agent's call (the Baker's too) is driving this git, not the operator.
fn agent_driven() -> bool {
    crate::actor::attribute("human") != "human"
}

/// git through [`crate::git::run`], which turns hooks and fsmonitor off once.
/// This wrapper does not add that pair again.
pub(crate) fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    crate::git::run(dir, args)
}

/// An agent's git never checks out a change to an agent runtime's own
/// settings (`bakery_exec::AGENT_STATE_DIRS`): its room keeps them from it,
/// and this git runs outside that room. The operator's own git may. `diff`
/// names what the checkout would bring, as `git diff` takes it.
fn agents_settings_kept(dir: &Path, diff: &[&str]) -> Result<(), String> {
    if !agent_driven() {
        return Ok(());
    }
    let mut args = vec!["diff", "--name-only"];
    args.extend(diff);
    args.push("--");
    args.extend(bakery_exec::AGENT_STATE_DIRS);
    let changed = git(dir, &args)?;
    if changed.trim().is_empty() {
        return Ok(());
    }
    Err(format!(
        "it would change {}: an agent runtime's own settings, which only the operator changes",
        changed.split_whitespace().collect::<Vec<_>>().join(", ")
    ))
}

/// A ref or branch argument, never an option.
pub(crate) fn rev(value: &str) -> Result<&str, String> {
    let value = value.trim();
    if value.is_empty() || value.starts_with('-') {
        return Err(format!("{value:?} is not a branch or revision"));
    }
    Ok(value)
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct GitQuery {
    pub(crate) stream_id: String,
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct LogQuery {
    pub(crate) stream_id: String,
    /// How many commits (default 30, at most 200).
    #[serde(default)]
    pub(crate) limit: Option<usize>,
    /// A branch or revision (default: the current branch).
    #[serde(default)]
    pub(crate) rev: Option<String>,
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct DiffQuery {
    pub(crate) stream_id: String,
    /// One file only.
    #[serde(default)]
    pub(crate) path: Option<String>,
    /// What is staged, instead of what is not.
    #[serde(default)]
    pub(crate) staged: bool,
    /// One commit's changes instead.
    #[serde(default)]
    pub(crate) commit: Option<String>,
    /// Everything this checkout's branch has committed since it left `base`
    /// (`git diff <base>...HEAD`), e.g. a whole agent branch against main.
    #[serde(default)]
    pub(crate) base: Option<String>,
    /// With `base`: what is not committed yet too, so all of the branch's
    /// changes since it left `base` show together
    /// (`git diff $(git merge-base <base> HEAD)`).
    #[serde(default)]
    pub(crate) worktree: bool,
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct StageReq {
    pub(crate) stream_id: String,
    #[serde(default)]
    pub(crate) paths: Vec<String>,
    /// Every change, new files included.
    #[serde(default)]
    pub(crate) all: bool,
    /// Take these paths out of the staged set instead.
    #[serde(default)]
    pub(crate) unstage: bool,
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct CommitReq {
    pub(crate) stream_id: String,
    pub(crate) message: String,
}

fn yes() -> bool {
    true
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct BranchReq {
    pub(crate) stream_id: String,
    pub(crate) name: String,
    /// Where it starts (default: the current commit).
    #[serde(default)]
    pub(crate) from: Option<String>,
    /// Switch to it (default true).
    #[serde(default = "yes")]
    pub(crate) switch: bool,
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct SwitchReq {
    pub(crate) stream_id: String,
    pub(crate) branch: String,
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct PushReq {
    pub(crate) stream_id: String,
    /// Default: the current branch.
    #[serde(default)]
    pub(crate) branch: Option<String>,
    /// The commit to send, on that branch: the one its card was approved
    /// for. Default: the branch's newest.
    #[serde(default)]
    pub(crate) commit: Option<String>,
}

#[derive(schemars::JsonSchema, Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PushCommit {
    pub(crate) sha: String,
    pub(crate) subject: String,
}

/// Exactly what a push sends, as its card shows it; the card's approval
/// sends that commit and nothing newer.
#[derive(schemars::JsonSchema, Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PushPreview {
    pub(crate) branch: String,
    /// The commit it sends.
    pub(crate) head: String,
    /// The remote's branch, as just fetched; none when it is new there.
    pub(crate) remote_head: Option<String>,
    /// What it is compared with: the remote's branch, or the branch it
    /// forked from when it is new there (origin's main for the local main).
    pub(crate) base: String,
    /// The commits it adds, newest first; at most 50.
    pub(crate) commits: Vec<PushCommit>,
    pub(crate) diff: String,
    pub(crate) diff_truncated: bool,
    /// The diff shows a credential-shaped file.
    pub(crate) secret: bool,
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct FetchReq {
    pub(crate) stream_id: String,
    /// Default: origin.
    #[serde(default)]
    pub(crate) remote: Option<String>,
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct PrReq {
    pub(crate) stream_id: String,
    pub(crate) title: String,
    #[serde(default)]
    pub(crate) body: Option<String>,
    /// The branch it merges into (default: the repository's default).
    #[serde(default)]
    pub(crate) base: Option<String>,
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct MergeReq {
    pub(crate) stream_id: String,
    /// The branch to merge into the current one.
    pub(crate) branch: String,
    #[serde(default)]
    pub(crate) message: Option<String>,
}

#[derive(schemars::JsonSchema, Debug, Serialize, Default)]
pub(crate) struct FileStatus {
    pub(crate) path: String,
    /// A rename's old path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) from: Option<String>,
    /// What is staged for it: modified, added, deleted, renamed, copied,
    /// type changed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) staged: Option<&'static str>,
    /// What is changed and not staged.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) unstaged: Option<&'static str>,
    pub(crate) untracked: bool,
    pub(crate) conflicted: bool,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct GitStatus {
    /// `None` on a detached HEAD.
    pub(crate) branch: Option<String>,
    pub(crate) upstream: Option<String>,
    pub(crate) ahead: u32,
    pub(crate) behind: u32,
    pub(crate) clean: bool,
    pub(crate) files: Vec<FileStatus>,
}

fn change(code: u8) -> Option<&'static str> {
    match code {
        b'M' => Some("modified"),
        b'A' => Some("added"),
        b'D' => Some("deleted"),
        b'R' => Some("renamed"),
        b'C' => Some("copied"),
        b'T' => Some("type changed"),
        _ => None,
    }
}

/// `git status --porcelain=v2 --branch -z`.
fn parse_status(raw: &str) -> GitStatus {
    let mut status = GitStatus {
        branch: None,
        upstream: None,
        ahead: 0,
        behind: 0,
        clean: true,
        files: Vec::new(),
    };
    let mut records = raw.split('\0');
    while let Some(record) = records.next() {
        if let Some(head) = record.strip_prefix("# branch.head ") {
            status.branch = (head != "(detached)").then(|| head.to_string());
        } else if let Some(upstream) = record.strip_prefix("# branch.upstream ") {
            status.upstream = Some(upstream.to_string());
        } else if let Some(ab) = record.strip_prefix("# branch.ab ") {
            for part in ab.split(' ') {
                if let Some(n) = part.strip_prefix('+') {
                    status.ahead = n.parse().unwrap_or(0);
                } else if let Some(n) = part.strip_prefix('-') {
                    status.behind = n.parse().unwrap_or(0);
                }
            }
        } else if let Some(path) = record.strip_prefix("? ") {
            status.files.push(FileStatus {
                path: path.to_string(),
                untracked: true,
                ..Default::default()
            });
        } else if let Some(kind) = record.chars().next().filter(|k| matches!(k, '1' | '2' | 'u')) {
            // Fields before the path: 8 for an ordinary change, 9 for a
            // rename or copy (its score), 10 for a conflict.
            let before = match kind {
                '1' => 8,
                '2' => 9,
                _ => 10,
            };
            let fields: Vec<&str> = record.splitn(before + 1, ' ').collect();
            let (Some(xy), Some(path)) = (fields.get(1), fields.get(before)) else {
                continue;
            };
            let xy = xy.as_bytes();
            status.files.push(FileStatus {
                path: path.to_string(),
                from: (kind == '2').then(|| records.next().unwrap_or_default().to_string()),
                staged: change(xy[0]),
                unstaged: change(xy[1]),
                untracked: false,
                conflicted: kind == 'u',
            });
        }
    }
    status.clean = status.files.is_empty();
    status
}

pub(crate) fn status(state: &AppState, stream_id: &str) -> Result<GitStatus, String> {
    let dir = checkout(state, stream_id)?;
    git(&dir, &["status", "--porcelain=v2", "--branch", "-z"]).map(|raw| parse_status(&raw))
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct Branch {
    pub(crate) name: String,
    pub(crate) current: bool,
    pub(crate) upstream: Option<String>,
    /// e.g. "ahead 2, behind 1"; empty when even with its upstream.
    pub(crate) track: String,
    pub(crate) commit: String,
    pub(crate) date: String,
    pub(crate) subject: String,
}

pub(crate) fn branches(state: &AppState, stream_id: &str) -> Result<Vec<Branch>, String> {
    let dir = checkout(state, stream_id)?;
    let format = "--format=%(refname:short)%09%(HEAD)%09%(upstream:short)%09%(upstream:track,nobracket)%09%(objectname:short)%09%(committerdate:iso-strict)%09%(contents:subject)";
    let raw = git(&dir, &["for-each-ref", format, "refs/heads"])?;
    Ok(raw
        .lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.splitn(7, '\t').collect();
            (f.len() == 7).then(|| Branch {
                name: f[0].to_string(),
                current: f[1] == "*",
                upstream: (!f[2].is_empty()).then(|| f[2].to_string()),
                track: f[3].to_string(),
                commit: f[4].to_string(),
                date: f[5].to_string(),
                subject: f[6].to_string(),
            })
        })
        .collect())
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct Commit {
    pub(crate) sha: String,
    pub(crate) short: String,
    pub(crate) author: String,
    pub(crate) date: String,
    pub(crate) subject: String,
}

pub(crate) fn log(state: &AppState, q: &LogQuery) -> Result<Vec<Commit>, String> {
    let dir = checkout(state, &q.stream_id)?;
    let limit = format!("-n{}", q.limit.unwrap_or(30).clamp(1, 200));
    let mut args = vec!["log", "--format=%H%x09%h%x09%an%x09%aI%x09%s", limit.as_str()];
    if let Some(r) = q.rev.as_deref() {
        args.push(rev(r)?);
    }
    args.push("--");
    let raw = git(&dir, &args)?;
    Ok(raw
        .lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.splitn(5, '\t').collect();
            (f.len() == 5).then(|| Commit {
                sha: f[0].to_string(),
                short: f[1].to_string(),
                author: f[2].to_string(),
                date: f[3].to_string(),
                subject: f[4].to_string(),
            })
        })
        .collect())
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct Diff {
    pub(crate) diff: String,
    pub(crate) truncated: bool,
    /// It shows lines of a secret file (`machine_access::is_secret`): no
    /// desktop screenshot while the app shows it.
    pub(crate) secret: bool,
}

/// `diff` shows lines of a secret file (`machine_access::is_secret`).
fn shows_a_secret(state: &AppState, dir: &Path, diff: &str) -> bool {
    shown_paths(diff).any(|path| crate::machine_access::is_secret(state, &dir.join(path), dir))
}

/// The files a diff shows lines of, from its `---`/`+++` headers; a binary
/// or a pure rename shows none.
pub(crate) fn shown_paths(diff: &str) -> impl Iterator<Item = &str> {
    const HEADERS: [&str; 4] = ["+++ b/", "--- a/", "+++ \"b/", "--- \"a/"];
    diff.lines()
        .filter_map(|line| HEADERS.iter().find_map(|h| line.strip_prefix(h)))
        .map(|path| path.trim_end_matches(['\t', '"']))
}

pub(crate) fn diff(state: &AppState, q: &DiffQuery) -> Result<Diff, String> {
    let dir = checkout(state, &q.stream_id)?;
    let range = match q.base.as_deref() {
        Some(_) if q.commit.is_some() => return Err("ask for one commit or a base, not both".into()),
        Some(base) if q.worktree => Some(git(&dir, &["merge-base", rev(base)?, "HEAD"])?.trim().to_string()),
        Some(base) => Some(format!("{}...HEAD", rev(base)?)),
        None if q.worktree => return Err("the working tree against what? name a base".into()),
        None => None,
    };
    let mut args: Vec<&str> = match (q.commit.as_deref(), range.as_deref()) {
        (Some(commit), _) => vec!["show", "--format=", "--patch", rev(commit)?],
        (None, Some(range)) => vec!["diff", range],
        (None, None) if q.staged => vec!["diff", "--cached"],
        (None, None) => vec!["diff"],
    };
    args.push("--");
    if let Some(path) = q.path.as_deref().filter(|p| !p.trim().is_empty()) {
        args.push(path);
    }
    let mut diff = git(&dir, &args)?;
    let truncated = diff.len() > MAX_DIFF_BYTES;
    if truncated {
        let cut = (0..=MAX_DIFF_BYTES)
            .rev()
            .find(|&i| diff.is_char_boundary(i))
            .unwrap_or(0);
        diff.truncate(cut);
    }
    let secret = shows_a_secret(state, &dir, &diff);
    Ok(Diff { diff, truncated, secret })
}

pub(crate) fn stage(state: &AppState, req: &StageReq) -> Result<GitStatus, String> {
    let dir = writable_checkout(state, &req.stream_id)?;
    let paths: Vec<&str> = req
        .paths
        .iter()
        .map(String::as_str)
        .filter(|p| !p.trim().is_empty())
        .collect();
    if !req.all && paths.is_empty() {
        return Err("name the paths to stage, or all: true".to_string());
    }
    let mut args: Vec<&str> = match (req.unstage, req.all) {
        (false, true) => vec!["add", "-A"],
        (false, false) => vec!["add"],
        (true, _) => vec!["restore", "--staged"],
    };
    args.push("--");
    if req.unstage && req.all {
        args.push(".");
    }
    args.extend(paths);
    git(&dir, &args)?;
    status(state, &req.stream_id)
}

pub(crate) fn commit(state: &AppState, req: &CommitReq) -> Result<Commit, String> {
    let dir = writable_checkout(state, &req.stream_id)?;
    let message = req.message.trim();
    if message.is_empty() {
        return Err("a commit needs a message".to_string());
    }
    if git(&dir, &["diff", "--cached", "--quiet"]).is_ok() {
        return Err("nothing is staged: stage changes first (git_stage)".to_string());
    }
    // An agent's commit: the project's hooks, in git's order, in its room.
    let hook = |name: &str, args: &[&str]| crate::git_hooks::run_project_hook(state, &req.stream_id, name, args);
    hook("pre-commit", &[])?;
    let msg = git(&dir, &["rev-parse", "--path-format=absolute", "--git-path", "COMMIT_EDITMSG"])?.trim().to_string();
    std::fs::write(&msg, format!("{message}\n")).map_err(|e| format!("{msg}: {e}"))?;
    hook("prepare-commit-msg", &[&msg, "message"])?;
    hook("commit-msg", &[&msg])?;
    git(&dir, &["commit", "-q", "-F", &msg])?;
    let _ = hook("post-commit", &[]);
    let latest = LogQuery {
        stream_id: req.stream_id.clone(),
        limit: Some(1),
        rev: None,
    };
    log(state, &latest)?
        .pop()
        .ok_or_else(|| "the commit landed but cannot be read back".to_string())
}

pub(crate) fn branch(state: &AppState, req: &BranchReq) -> Result<GitStatus, String> {
    let dir = writable_checkout(state, &req.stream_id)?;
    let name = rev(&req.name)?;
    git(&dir, &["check-ref-format", "--branch", name])
        .map_err(|_| format!("{name:?} is not a valid branch name"))?;
    let from = req.from.as_deref().map(rev).transpose()?;
    let mut args = if req.switch {
        if let Some(from) = from {
            agents_settings_kept(&dir, &["HEAD", from])?;
        }
        vec!["switch", "-c", name]
    } else {
        vec!["branch", name]
    };
    args.extend(from);
    let before = git(&dir, &["rev-parse", "HEAD"])?.trim().to_string();
    git(&dir, &args)?;
    if req.switch {
        checked_out(state, &req.stream_id, &dir, &before);
    }
    status(state, &req.stream_id)
}

pub(crate) fn switch(state: &AppState, req: &SwitchReq) -> Result<GitStatus, String> {
    let dir = writable_checkout(state, &req.stream_id)?;
    let branch = rev(&req.branch)?;
    agents_settings_kept(&dir, &["HEAD", branch])?;
    let before = git(&dir, &["rev-parse", "HEAD"])?.trim().to_string();
    git(&dir, &["switch", branch])?;
    checked_out(state, &req.stream_id, &dir, &before);
    status(state, &req.stream_id)
}

/// After an agent's checkout: the project's post-checkout, in its room.
fn checked_out(state: &AppState, stream_id: &str, dir: &Path, before: &str) {
    let after = git(dir, &["rev-parse", "HEAD"]).unwrap_or_default();
    let _ = crate::git_hooks::run_project_hook(state, stream_id, "post-checkout", &[before, after.trim(), "1"]);
}

pub(crate) fn current_branch(state: &AppState, stream_id: &str) -> Result<String, String> {
    status(state, stream_id)?
        .branch
        .ok_or_else(|| "HEAD is detached: switch to a branch first".to_string())
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct Done {
    pub(crate) branch: String,
    pub(crate) output: String,
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).trim().to_string()
}

/// The newest commit of a conversation's branch (default: its current one).
pub(crate) fn branch_head(state: &AppState, stream_id: &str, branch: Option<&str>) -> Result<(PathBuf, String, String), String> {
    let dir = checkout(state, stream_id)?;
    let branch = match branch {
        Some(b) => rev(b)?.to_string(),
        None => current_branch(state, stream_id)?,
    };
    let head = git(&dir, &["rev-parse", "--verify", &format!("refs/heads/{branch}^{{commit}}")])?;
    Ok((dir, branch, head.trim().to_string()))
}

/// What pushing a conversation's branch would send now, against the
/// remote's branch as just fetched.
pub(crate) fn push_preview(state: &AppState, stream_id: &str, branch: Option<&str>) -> Result<PushPreview, String> {
    let (dir, branch, head) = branch_head(state, stream_id, branch)?;
    // A branch new on the remote has nothing to fetch.
    let _ = git(&dir, &["fetch", "-q", "origin", &branch]);
    let tracking = format!("refs/remotes/origin/{branch}");
    let remote_head = git(&dir, &["rev-parse", "--verify", "-q", &tracking]).ok().map(|h| h.trim().to_string());
    let remote_head = remote_head.filter(|h| !h.is_empty());
    let base = if remote_head.is_some() {
        tracking
    } else {
        // New there, it is compared with origin's main, not the local main
        // it forked from: that main's own commits go out with it.
        let fork = fork_base(state, stream_id, &dir);
        crate::git::remote_main(&dir).filter(|origin| *origin == format!("origin/{fork}")).unwrap_or(fork)
    };
    let commits = git(&dir, &["log", "-n", "50", "--format=%H%x09%s", &format!("{base}..{head}")])?
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .map(|(sha, subject)| PushCommit { sha: sha.to_string(), subject: subject.to_string() })
        .collect();
    let mut diff = git(&dir, &["diff", &format!("{base}...{head}")])?;
    let diff_truncated = cap(&mut diff);
    let secret = shows_a_secret(state, &dir, &diff);
    Ok(PushPreview { branch, head, remote_head, base, commits, diff, diff_truncated, secret })
}

/// `git push -u origin <branch>`, never forced; with `commit`, exactly that
/// commit of the branch.
pub(crate) fn push(state: &AppState, req: &PushReq) -> Result<Done, String> {
    let dir = checkout(state, &req.stream_id)?;
    let branch = match req.branch.as_deref() {
        Some(b) => rev(b)?.to_string(),
        None => current_branch(state, &req.stream_id)?,
    };
    if !crate::git::has_remote(&dir, "origin") {
        return Err("no remote named origin".to_string());
    }
    let output = match req.commit.as_deref() {
        None => git(&dir, &["push", "-u", "origin", &branch])?,
        Some(commit) => {
            let commit = rev(commit)?;
            git(&dir, &["merge-base", "--is-ancestor", commit, &format!("refs/heads/{branch}")])
                .map_err(|_| format!("{commit} is not a commit of {branch}"))?;
            let output = git(&dir, &["push", "origin", &format!("{commit}:refs/heads/{branch}")])?;
            let _ = git(&dir, &["branch", "--set-upstream-to", &format!("origin/{branch}"), &branch]);
            output
        }
    };
    let url = git(&dir, &["remote", "get-url", "origin"]).unwrap_or_default();
    let repository = crate::routes::github::github_slug(&url).unwrap_or_else(|| url.trim().to_string());
    let sent = match req.commit.as_deref() {
        Some(commit) => commit.to_string(),
        None => git(&dir, &["rev-parse", &format!("refs/heads/{branch}")]).unwrap_or_default().trim().to_string(),
    };
    let line = serde_json::json!({"what": "push", "repository": repository, "branch": branch, "commit": sent});
    crate::remote::done(state, Some(&req.stream_id), line);
    Ok(Done {
        output: output.trim().to_string(),
        branch,
    })
}

/// `git fetch <remote>`: the remote's branches, without touching the
/// checkout.
pub(crate) fn fetch(state: &AppState, req: &FetchReq) -> Result<GitStatus, String> {
    let dir = checkout(state, &req.stream_id)?;
    match rev(req.remote.as_deref().unwrap_or("origin"))? {
        // Remembered for the "fetched 2 min ago" beside "N behind" (rule 2).
        "origin" => crate::git::fetch_origin(&dir)?,
        remote => drop(git(&dir, &["fetch", remote])?),
    }
    status(state, &req.stream_id)
}

/// The branch a thread forked from (rule 3): what Bakery recorded when it
/// made the worktree, else its repository's main; either main means the
/// newer one as last fetched (`git::follows`).
pub(crate) fn fork_base(state: &AppState, stream_id: &str, dir: &Path) -> String {
    let onboarded = state.heart.lock().unwrap().first_of_family(stream_id, "project.onboarded");
    let recorded = onboarded.ok().flatten().and_then(|e| {
        serde_json::from_str::<serde_json::Value>(&e.payload).ok()?["base_branch"].as_str().map(str::to_string)
    });
    crate::git::follows(dir, recorded.as_deref())
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct Updated {
    /// What it merged in: the branch this thread forked from.
    pub(crate) base: String,
    /// It merged cleanly, or there was nothing to bring in.
    pub(crate) merged: bool,
    /// Files in conflict. The merge is left in progress for the operator or
    /// an agent to resolve, then commit (or `git merge --abort`).
    pub(crate) conflicts: Vec<String>,
    pub(crate) output: String,
}

/// Brings a thread up to the branch it forked from by a merge, never a
/// rebase (rule 3), fetching it first. A conflict stops it, listing the
/// files and leaving them to resolve.
pub(crate) fn update(state: &AppState, req: &GitQuery) -> Result<Updated, String> {
    let dir = writable_checkout(state, &req.stream_id)?;
    // Fetched before its base is read: which main is newer can change with
    // it. Offline, it merges what it last fetched; the rollup says how old.
    if crate::git::has_remote(&dir, "origin") {
        let _ = crate::git::fetch_origin(&dir);
    }
    let base = fork_base(state, &req.stream_id, &dir);
    agents_settings_kept(&dir, &[&format!("HEAD...{base}")])?;
    match merge_with_hooks(state, &req.stream_id, &dir, &["merge", "--no-edit", &base]) {
        Ok(output) => Ok(Updated { base, merged: true, conflicts: Vec::new(), output: output.trim().to_string() }),
        Err(e) => {
            let conflicts: Vec<String> = git(&dir, &["diff", "--name-only", "--diff-filter=U"])
                .unwrap_or_default()
                .lines()
                .map(str::to_string)
                .collect();
            if conflicts.is_empty() {
                return Err(e);
            }
            Ok(Updated { base, merged: false, conflicts, output: e })
        }
    }
}

pub(crate) async fn update_route(State(st): State<AppState>, Json(req): Json<GitQuery>) -> Reply<Updated> {
    blocking(move || update(&st, &req)).await
}

/// Rule 2's timer: every repository with an open thread is fetched every
/// five minutes, so "N behind" stays true; one that couldn't be is tried
/// again after a growing gap, up to half an hour, so being offline costs
/// little. Opening a thread and every fork fetch too (`git::fresh_main`).
pub(crate) fn keep_fetched(state: AppState) {
    std::thread::spawn(move || {
        let mut due = std::collections::HashMap::new();
        loop {
            std::thread::sleep(std::time::Duration::from_secs(60));
            fetch_tick(&state, &mut due, std::time::Instant::now());
        }
    });
}

/// The wait before fetching a repository again after `failures` failed
/// fetches in a row: five minutes, then 10, 20, and at most 30.
fn fetch_gap(failures: u32) -> std::time::Duration {
    std::time::Duration::from_secs((5 * 60 * 2u64.pow(failures.min(3))).min(30 * 60))
}

/// One pass: each repository with an open thread and an origin, once its
/// time has come. `due` keeps, per repository, its failures in a row and
/// when it is next due.
fn fetch_tick(
    state: &AppState,
    due: &mut std::collections::HashMap<String, (u32, std::time::Instant)>,
    now: std::time::Instant,
) {
    let streams = state.heart.lock().unwrap().list_streams().unwrap_or_default();
    let mut seen = std::collections::HashSet::new();
    for stream in streams {
        let Ok(ctx) = context_for(state, &stream.stream_id) else { continue };
        let dir = PathBuf::from(&ctx.worktree_path);
        if ctx.worktree_path.trim().is_empty() || !dir.is_dir() {
            continue;
        }
        let key = crate::git::repo_key(&dir);
        let (failures, next) = due.get(&key).copied().unwrap_or((0, now));
        if !seen.insert(key.clone()) || now < next {
            continue;
        }
        let has_origin = crate::git::run(&dir, &["remote"]).is_ok_and(|r| r.lines().any(|l| l.trim() == "origin"));
        if !has_origin {
            continue;
        }
        let failures = if crate::git::fetch_origin(&dir).is_ok() { 0 } else { failures + 1 };
        due.insert(key, (failures, now + fetch_gap(failures)));
    }
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct RebaseReq {
    pub(crate) stream_id: String,
    /// The branch or revision to rebase onto.
    pub(crate) onto: String,
    /// Why a rebase and not a merge (`git_update`): recorded in History.
    pub(crate) reason: String,
}

/// A rebase with its reason recorded first (rule 3): an agent merges its base
/// in by default (`update`). Hooks off for an agent, as with every agent git
/// tool here. A conflict stops it, listing the files, with the rebase left in
/// progress (`git rebase --continue` or `--abort`).
pub(crate) fn rebase(state: &AppState, req: &RebaseReq) -> Result<Updated, String> {
    let dir = writable_checkout(state, &req.stream_id)?;
    let onto = rev(&req.onto)?.to_string();
    reviewed_only(state, &req.stream_id, &dir, &onto)?;
    let reason = req.reason.trim();
    if reason.is_empty() {
        return Err("a rebase needs a stated reason; or merge the base in with git_update".to_string());
    }
    agents_settings_kept(&dir, &[&format!("HEAD...{onto}")])?;
    let payload = serde_json::json!({ "onto": onto, "reason": reason }).to_string();
    crate::state::append(state, &req.stream_id, "human", crate::git_hooks::REASONED_FAMILY, payload, vec![])?;
    crate::git_hooks::run_project_hook(state, &req.stream_id, "pre-rebase", &[&onto])?;
    match git(&dir, &["rebase", &onto]) {
        Ok(output) => Ok(Updated { base: onto, merged: true, conflicts: Vec::new(), output: output.trim().to_string() }),
        Err(e) => {
            let conflicts: Vec<String> = git(&dir, &["diff", "--name-only", "--diff-filter=U"])
                .unwrap_or_default()
                .lines()
                .map(str::to_string)
                .collect();
            if conflicts.is_empty() {
                return Err(e);
            }
            Ok(Updated { base: onto, merged: false, conflicts, output: e })
        }
    }
}

pub(crate) async fn rebase_route(State(st): State<AppState>, Json(req): Json<RebaseReq>) -> Reply<Updated> {
    blocking(move || rebase(&st, &req)).await
}

/// Brings the current branch up to its upstream with a merge, never a
/// rebase. A conflict aborts it, as [`merge`] does.
pub(crate) fn pull(state: &AppState, req: &GitQuery) -> Result<GitStatus, String> {
    let dir = writable_checkout(state, &req.stream_id)?;
    if status(state, &req.stream_id)?.upstream.is_none() {
        return Err("this branch tracks no upstream: push it first (git_push)".to_string());
    }
    if agent_driven() {
        // Fetched first, so what the merge brings is checked before it lands.
        git(&dir, &["fetch"])?;
        agents_settings_kept(&dir, &["HEAD...@{upstream}"])?;
        merged_or_aborted(state, &req.stream_id, &dir, &["merge", "--no-edit", "@{upstream}"], "the upstream")?;
    } else {
        merged_or_aborted(state, &req.stream_id, &dir, &["pull", "--no-rebase", "--no-edit"], "the upstream")?;
    }
    status(state, &req.stream_id)
}

/// Runs a merging git command; on a conflict, aborts the merge and names
/// the files, so nothing changes.
/// A merge, with the project's hooks run in an agent's room in git's order:
/// the merge is made without committing, then pre-merge-commit and the
/// message hooks run, the node commits with hooks off, and post-merge runs.
/// A hook that fails aborts the merge. The operator's own merge runs as
/// given, hooks and all.
fn merge_with_hooks(state: &AppState, stream_id: &str, dir: &Path, args: &[&str]) -> Result<String, String> {
    if !crate::actor::agent_calling() {
        return git(dir, args);
    }
    let mut no_commit = args.to_vec();
    no_commit.insert(1, "--no-commit");
    let output = git(dir, &no_commit)?;
    if git(dir, &["rev-parse", "-q", "--verify", "MERGE_HEAD"]).is_ok() {
        let hook = |name: &str, args: &[&str]| crate::git_hooks::run_project_hook(state, stream_id, name, args);
        let msg = git(dir, &["rev-parse", "--path-format=absolute", "--git-path", "MERGE_MSG"])?.trim().to_string();
        let hooked = hook("pre-merge-commit", &[])
            .and_then(|()| hook("prepare-commit-msg", &[&msg, "merge"]))
            .and_then(|()| hook("commit-msg", &[&msg]));
        if let Err(why) = hooked {
            let _ = git(dir, &["merge", "--abort"]);
            return Err(why);
        }
        git(dir, &["commit", "--no-edit", "-q"])?;
    }
    let _ = crate::git_hooks::run_project_hook(state, stream_id, "post-merge", &["0"]);
    Ok(output)
}

pub(crate) fn merged_or_aborted(state: &AppState, stream_id: &str, dir: &Path, args: &[&str], theirs: &str) -> Result<String, String> {
    match merge_with_hooks(state, stream_id, dir, args) {
        Ok(output) => Ok(output),
        Err(e) => {
            let conflicts =
                git(dir, &["diff", "--name-only", "--diff-filter=U"]).unwrap_or_default();
            if conflicts.trim().is_empty() {
                return Err(e);
            }
            let _ = git(dir, &["merge", "--abort"]);
            Err(format!(
                "merge stopped: {theirs} conflicts in {}; nothing changed",
                conflicts.split_whitespace().collect::<Vec<_>>().join(", ")
            ))
        }
    }
}

/// `gh pr create` for the current branch.
pub(crate) fn pr(state: &AppState, req: &PrReq) -> Result<Done, String> {
    let dir = checkout(state, &req.stream_id)?;
    let branch = current_branch(state, &req.stream_id)?;
    let title = req.title.trim();
    if title.is_empty() {
        return Err("a pull request needs a title".to_string());
    }
    if !crate::git::gh_available() {
        return Err("the GitHub CLI (gh) is not installed or not on PATH".to_string());
    }
    let mut cmd = std::process::Command::new("gh");
    cmd.current_dir(&dir)
        .args(["pr", "create", "--head", &branch, "--title", title, "--body"])
        .arg(req.body.as_deref().unwrap_or(""));
    if let Some(base) = req.base.as_deref() {
        cmd.args(["--base", rev(base)?]);
    }
    let out = cmd.output().map_err(|e| format!("failed to run gh: {e}"))?;
    if !out.status.success() {
        return Err(text(&out.stderr));
    }
    let output = text(&out.stdout);
    // The pull request's thread, for the inbox; the request is already
    // open on GitHub, so a lost record never fails it.
    if let Some((repository, number, url)) = crate::routes::github_prs::pr_address(&output) {
        let opened = serde_json::json!({"repository": repository, "number": number, "url": url, "branch": branch});
        let _ = crate::state::append(state, &req.stream_id, "human", crate::routes::github_prs::PR_OPENED, opened.to_string(), vec![]);
        let line = serde_json::json!({"what": "pull request opened", "repository": repository, "number": number, "url": url, "branch": branch});
        crate::remote::done(state, Some(&req.stream_id), line);
    }
    Ok(Done { output, branch })
}

/// A workspace's work (its orc's branch, or a worker's) goes anywhere only
/// through the merge review, on the operator's approval: an agent never
/// merges or rebases it into another checkout by itself, by name or by
/// its tip.
fn reviewed_only(state: &AppState, into: &str, dir: &Path, theirs: &str) -> Result<(), String> {
    if !crate::actor::agent_calling() {
        return Ok(());
    }
    let commit = |rev: &str| git(dir, &["rev-parse", "--verify", "--quiet", &format!("{rev}^{{commit}}")]).map(|sha| sha.trim().to_string());
    let theirs_tip = commit(theirs).ok();
    let streams = crate::state::known_streams(state).unwrap_or_default();
    for s in streams.iter().filter(|s| !s.discarded && s.stream_id != into && !s.branch.is_empty()) {
        if crate::workspaces::workspace_of(state, &s.stream_id).is_none() {
            continue;
        }
        let named = theirs.trim_start_matches("refs/heads/") == s.branch;
        if named || (theirs_tip.is_some() && commit(&format!("refs/heads/{}", s.branch)).ok() == theirs_tip) {
            return Err(format!(
                "{theirs} is a workspace's work: it goes in only through the merge review, never by a merge or rebase here (git_update brings in the branch you forked from)"
            ));
        }
    }
    Ok(())
}

/// Merges `branch` into the current one. A conflict aborts the merge and
/// names the files, leaving the branch as it was.
pub(crate) fn merge(state: &AppState, req: &MergeReq) -> Result<Done, String> {
    let dir = writable_checkout(state, &req.stream_id)?;
    let current = current_branch(state, &req.stream_id)?;
    let theirs = rev(&req.branch)?;
    reviewed_only(state, &req.stream_id, &dir, theirs)?;
    let message = req
        .message
        .as_deref()
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("Merge branch '{theirs}' into {current}"));
    let output = merged_or_aborted(
        state,
        &req.stream_id,
        &dir,
        &["merge", "--no-ff", "--no-edit", "-m", &message, theirs],
        theirs,
    )?;
    Ok(Done {
        output: output.trim().to_string(),
        branch: current,
    })
}

/// Runs one of the functions above off the async workers (git is blocking).
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Reply<T> {
    crate::actor::spawn_blocking(f)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map(Json)
        .map_err(bad)
}

pub(crate) async fn status_route(
    State(st): State<AppState>,
    Query(q): Query<GitQuery>,
) -> Reply<GitStatus> {
    blocking(move || status(&st, &q.stream_id)).await
}

pub(crate) async fn branches_route(
    State(st): State<AppState>,
    Query(q): Query<GitQuery>,
) -> Reply<Vec<Branch>> {
    blocking(move || branches(&st, &q.stream_id)).await
}

pub(crate) async fn log_route(
    State(st): State<AppState>,
    Query(q): Query<LogQuery>,
) -> Reply<Vec<Commit>> {
    blocking(move || log(&st, &q)).await
}

pub(crate) async fn diff_route(
    State(st): State<AppState>,
    Query(q): Query<DiffQuery>,
) -> Reply<Diff> {
    blocking(move || diff(&st, &q)).await
}

pub(crate) async fn stage_route(
    State(st): State<AppState>,
    Json(req): Json<StageReq>,
) -> Reply<GitStatus> {
    blocking(move || stage(&st, &req)).await
}

pub(crate) async fn commit_route(
    State(st): State<AppState>,
    Json(req): Json<CommitReq>,
) -> Reply<Commit> {
    blocking(move || commit(&st, &req)).await
}

pub(crate) async fn branch_route(
    State(st): State<AppState>,
    Json(req): Json<BranchReq>,
) -> Reply<GitStatus> {
    blocking(move || branch(&st, &req)).await
}

pub(crate) async fn switch_route(
    State(st): State<AppState>,
    Json(req): Json<SwitchReq>,
) -> Reply<GitStatus> {
    blocking(move || switch(&st, &req)).await
}

pub(crate) async fn fetch_route(
    State(st): State<AppState>,
    Json(req): Json<FetchReq>,
) -> Reply<GitStatus> {
    blocking(move || fetch(&st, &req)).await
}

pub(crate) async fn pull_route(
    State(st): State<AppState>,
    Json(req): Json<GitQuery>,
) -> Reply<GitStatus> {
    blocking(move || pull(&st, &req)).await
}

pub(crate) async fn push_route(State(st): State<AppState>, Json(req): Json<PushReq>) -> Reply<Done> {
    blocking(move || push(&st, &req)).await
}

pub(crate) async fn pr_route(State(st): State<AppState>, Json(req): Json<PrReq>) -> Reply<Done> {
    blocking(move || pr(&st, &req)).await
}

pub(crate) async fn merge_route(
    State(st): State<AppState>,
    Json(req): Json<MergeReq>,
) -> Reply<Done> {
    blocking(move || merge(&st, &req)).await
}

// ─── Before a merge ─────────────────────────────────────────────────────────

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct MergePreviewQuery {
    pub(crate) stream_id: String,
    /// The branch this one would merge into.
    pub(crate) base: String,
    /// With the diff too (cut at 32 KiB). Without it, the files and their
    /// lines, and the conflicts.
    #[serde(default)]
    pub(crate) diff: bool,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct MergePreview {
    /// It merges without a conflict.
    pub(crate) clean: bool,
    /// The files that would conflict; their conflict markers are in `diff`.
    pub(crate) conflicts: Vec<String>,
    /// Each file the merge would change on base, with its lines: the first
    /// 200.
    pub(crate) files: Vec<ChangedFile>,
    /// How many files it would change, all of them.
    pub(crate) files_total: usize,
    /// Lines it would add and remove on base, over every file.
    pub(crate) added: usize,
    pub(crate) removed: usize,
    /// What the merge would change on base: base against the merged result,
    /// cut at 32 KiB. Empty unless asked for (`diff`).
    pub(crate) diff: String,
    /// The diff was cut.
    pub(crate) truncated: bool,
    /// It shows lines of a secret file: no desktop screenshot meanwhile.
    /// Without the diff, one of its files is a secret file.
    pub(crate) secret: bool,
}

/// A file a merge would change, as `git diff --numstat` counts it.
#[derive(schemars::JsonSchema, Debug, Clone, PartialEq, Serialize)]
pub(crate) struct ChangedFile {
    pub(crate) path: String,
    /// A rename's old path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) from: Option<String>,
    pub(crate) added: usize,
    pub(crate) removed: usize,
    /// No lines to count.
    pub(crate) binary: bool,
}

/// One merge as git would make it, without its diff.
#[derive(Debug)]
pub(crate) struct MergeSummary {
    /// The merged tree, conflict markers included.
    pub(crate) tree: String,
    pub(crate) clean: bool,
    pub(crate) conflicts: Vec<String>,
    pub(crate) files: Vec<ChangedFile>,
    pub(crate) added: usize,
    pub(crate) removed: usize,
}

/// At most [`MAX_DIFF_BYTES`], cut on a character boundary.
pub(crate) fn cap(diff: &mut String) -> bool {
    cap_at(diff, MAX_DIFF_BYTES)
}

/// At most `max` bytes, cut on a character boundary; whether it was cut.
fn cap_at(diff: &mut String, max: usize) -> bool {
    if diff.len() <= max {
        return false;
    }
    let cut = (0..=max).rev().find(|&i| diff.is_char_boundary(i)).unwrap_or(0);
    diff.truncate(cut);
    true
}

/// One merge, in git's object store only: `git merge-tree --write-tree`
/// (its objects stay loose until git collects them). The merged tree,
/// whether it was clean, and the files that conflict.
fn merge_tree(dir: &Path, ours: &str, theirs: &str) -> Result<(String, bool, Vec<String>), String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "core.quotePath=false", "merge-tree", "--write-tree", "--name-only", "--no-messages", ours, theirs])
        .output()
        .map_err(|e| format!("failed to run git: {e}"))?;
    // 0 is clean and 1 has conflicts, both with the merged tree; else it failed.
    if !matches!(out.status.code(), Some(0 | 1)) {
        return Err(text(&out.stderr));
    }
    let listing = String::from_utf8_lossy(&out.stdout).into_owned();
    let mut lines = listing.lines();
    let tree = lines.next().unwrap_or_default().trim().to_string();
    let conflicts = lines.map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect();
    Ok((tree, out.status.code() == Some(0), conflicts))
}

/// base against a merged tree, conflict markers included, within
/// [`MERGE_DIFF_BYTES`].
fn merged_diff(dir: &Path, base: &str, tree: &str) -> Result<(String, bool), String> {
    let mut diff = git(dir, &["-c", "core.quotePath=false", "diff", "--no-color", "--no-ext-diff", base, tree, "--"])?;
    let truncated = cap_at(&mut diff, MERGE_DIFF_BYTES);
    Ok((diff, truncated))
}

/// `git diff --numstat -z`: a file per record, a rename's as an empty path
/// followed by its old and new paths; a binary file's counts are `-`.
fn parse_numstat(raw: &str) -> Vec<ChangedFile> {
    let mut files = Vec::new();
    let mut records = raw.split('\0');
    while let Some(record) = records.next() {
        let mut fields = record.splitn(3, '\t');
        let (Some(added), Some(removed), Some(path)) = (fields.next(), fields.next(), fields.next()) else {
            continue;
        };
        let (from, path) = match path {
            "" => (records.next().map(str::to_string), records.next().unwrap_or_default().to_string()),
            path => (None, path.to_string()),
        };
        files.push(ChangedFile {
            path,
            from,
            added: added.parse().unwrap_or(0),
            removed: removed.parse().unwrap_or(0),
            binary: added == "-" && removed == "-",
        });
    }
    files
}

/// Each file a merged tree changes on base, with its lines.
fn changed_files(dir: &Path, base: &str, tree: &str) -> Result<Vec<ChangedFile>, String> {
    let numstat = git(dir, &["diff", "--numstat", "-z", "-M", base, tree, "--"])?;
    Ok(parse_numstat(&numstat))
}

/// Lines added and removed, over every file.
fn totals(files: &[ChangedFile]) -> (usize, usize) {
    files.iter().fold((0, 0), |(added, removed), f| (added + f.added, removed + f.removed))
}

/// What merging `theirs` into `base` would change on base, without the diff
/// and without touching a checkout: whether it conflicts and where, and each
/// file with its lines. Two git calls, `merge-tree` and `diff --numstat`.
pub(crate) fn merge_summary(dir: &Path, base: &str, theirs: &str) -> Result<MergeSummary, String> {
    let (tree, clean, conflicts) = merge_tree(dir, base, theirs)?;
    let files = changed_files(dir, base, &tree)?;
    let (added, removed) = totals(&files);
    Ok(MergeSummary { tree, clean, conflicts, files, added, removed })
}

/// One of `files` is a secret file (`machine_access::is_secret`), by its
/// paths: a summary has no diff to read it from.
fn names_a_secret(state: &AppState, dir: &Path, files: &[ChangedFile]) -> bool {
    files
        .iter()
        .flat_map(|f| std::iter::once(&f.path).chain(&f.from))
        .any(|path| crate::machine_access::is_secret(state, &dir.join(path), dir))
}

/// A preview's `diff`, `truncated` and `secret`: base against the merged
/// tree when asked for (`with_diff`), else no diff, its secret read from the
/// paths.
fn merged_view(
    state: &AppState,
    dir: &Path,
    base: &str,
    tree: &str,
    files: &[ChangedFile],
    with_diff: bool,
) -> Result<(String, bool, bool), String> {
    if !with_diff {
        return Ok((String::new(), false, names_a_secret(state, dir, files)));
    }
    let (diff, truncated) = merged_diff(dir, base, tree)?;
    let secret = shows_a_secret(state, dir, &diff);
    Ok((diff, truncated, secret))
}

/// What merging the current branch into `base` would give, without touching
/// the checkout.
pub(crate) fn merge_preview(state: &AppState, q: &MergePreviewQuery) -> Result<MergePreview, String> {
    let dir = checkout(state, &q.stream_id)?;
    let base = rev(&q.base)?;
    let MergeSummary { tree, clean, conflicts, mut files, added, removed } = merge_summary(&dir, base, "HEAD")?;
    let (diff, truncated, secret) = merged_view(state, &dir, base, &tree, &files, q.diff)?;
    let files_total = files.len();
    files.truncate(MERGE_FILES_SHOWN);
    Ok(MergePreview { clean, conflicts, files, files_total, added, removed, diff, truncated, secret })
}

/// A query's list, as either client sends one: JSON (`["a","b"]`, the
/// Baker's tools) or comma-separated (`a,b`, the app's client).
fn query_list<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    let raw = String::deserialize(d)?;
    if raw.trim_start().starts_with('[') {
        return serde_json::from_str(&raw).map_err(serde::de::Error::custom);
    }
    Ok(raw.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect())
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct MergeChainQuery {
    pub(crate) stream_id: String,
    /// The branch they would all merge into.
    pub(crate) base: String,
    /// The branches to merge into it, in order.
    #[serde(deserialize_with = "query_list")]
    #[schemars(with = "Vec<String>")]
    pub(crate) branches: Vec<String>,
    /// With the diff too (cut at 32 KiB). Without it, the files and their
    /// lines, and each step's conflicts.
    #[serde(default)]
    pub(crate) diff: bool,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct ChainStep {
    pub(crate) branch: String,
    /// It merges into base and every branch before it without a conflict.
    pub(crate) clean: bool,
    pub(crate) conflicts: Vec<String>,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct MergeChain {
    /// One per branch, in order.
    pub(crate) steps: Vec<ChainStep>,
    /// Each file every merge together would change on base, with its
    /// lines: the first 200.
    pub(crate) files: Vec<ChangedFile>,
    /// How many files they would change, all of them.
    pub(crate) files_total: usize,
    /// Lines they would add and remove on base, over every file.
    pub(crate) added: usize,
    pub(crate) removed: usize,
    /// base against the result of every merge, conflict markers included,
    /// cut at 32 KiB. Empty unless asked for (`diff`).
    pub(crate) diff: String,
    /// The diff was cut.
    pub(crate) truncated: bool,
    /// It shows lines of a secret file: no desktop screenshot meanwhile.
    /// Without the diff, one of its files is a secret file.
    pub(crate) secret: bool,
}

/// Merging every branch into base in turn, without touching the checkout:
/// base with the first, that result with the second, and so on, so branches
/// that conflict only with each other show up before any merge. Each step
/// after the first merges from an unreferenced commit of the last result,
/// which git collects later like the trees.
pub(crate) fn merge_chain(state: &AppState, q: &MergeChainQuery) -> Result<MergeChain, String> {
    if q.branches.is_empty() {
        return Err("name at least one branch to merge".to_string());
    }
    let dir = checkout(state, &q.stream_id)?;
    let base = rev(&q.base)?;
    let mut head = git(&dir, &["rev-parse", "--verify", &format!("{base}^{{commit}}")])?.trim().to_string();
    let mut tree = String::new();
    let mut steps = Vec::new();
    for branch in &q.branches {
        let branch = rev(branch)?;
        let (merged, clean, conflicts) = merge_tree(&dir, &head, branch)?;
        let identity = ["-c", "user.name=Bakery", "-c", "user.email=bakery@localhost"];
        let commit = [&identity[..], &["commit-tree", &merged, "-p", &head, "-p", branch, "-m", "merge preview"]].concat();
        head = git(&dir, &commit)?.trim().to_string();
        tree = merged;
        steps.push(ChainStep { branch: branch.to_string(), clean, conflicts });
    }
    let mut files = changed_files(&dir, base, &tree)?;
    let (added, removed) = totals(&files);
    let (diff, truncated, secret) = merged_view(state, &dir, base, &tree, &files, q.diff)?;
    let files_total = files.len();
    files.truncate(MERGE_FILES_SHOWN);
    Ok(MergeChain { steps, files, files_total, added, removed, diff, truncated, secret })
}

pub(crate) async fn merge_chain_route(
    State(st): State<AppState>,
    Query(q): Query<MergeChainQuery>,
) -> Reply<MergeChain> {
    blocking(move || merge_chain(&st, &q)).await
}

pub(crate) async fn merge_preview_route(
    State(st): State<AppState>,
    Query(q): Query<MergePreviewQuery>,
) -> Reply<MergePreview> {
    blocking(move || merge_preview(&st, &q)).await
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct PrChecksQuery {
    pub(crate) stream_id: String,
}

#[derive(schemars::JsonSchema, Debug, Serialize, Deserialize)]
pub(crate) struct PrCheck {
    pub(crate) name: String,
    /// GitHub's state: SUCCESS, FAILURE, PENDING, SKIPPED…
    pub(crate) state: String,
    /// pass, fail, pending, skipping or cancel.
    #[serde(default)]
    pub(crate) bucket: String,
    #[serde(default)]
    pub(crate) workflow: String,
    #[serde(default)]
    pub(crate) link: String,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct PrChecks {
    pub(crate) branch: String,
    pub(crate) checks: Vec<PrCheck>,
}

/// The checks on the current branch's pull request (`gh pr checks`).
pub(crate) fn pr_checks(state: &AppState, q: &PrChecksQuery) -> Result<PrChecks, String> {
    let dir = checkout(state, &q.stream_id)?;
    let branch = current_branch(state, &q.stream_id)?;
    if !crate::git::gh_available() {
        return Err("the GitHub CLI (gh) is not installed or not on PATH".to_string());
    }
    let out = std::process::Command::new("gh")
        .current_dir(&dir)
        .args(["pr", "checks", &branch, "--json", "name,state,bucket,workflow,link"])
        .output()
        .map_err(|e| format!("failed to run gh: {e}"))?;
    // gh exits non-zero while a check fails or is pending; its JSON answers anyway.
    match serde_json::from_slice::<Vec<PrCheck>>(&out.stdout) {
        Ok(checks) => Ok(PrChecks { branch, checks }),
        Err(_) => Err(match text(&out.stderr) {
            e if e.is_empty() => format!("gh pr checks failed ({})", out.status),
            e => e,
        }),
    }
}

pub(crate) async fn pr_checks_route(
    State(st): State<AppState>,
    Query(q): Query<PrChecksQuery>,
) -> Reply<PrChecks> {
    blocking(move || pr_checks(&st, &q)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::project::open_stream;
    use crate::testutil::*;
    use bakery_agent::StubModel;
    use std::sync::Arc;

    fn stage_paths(sid: &str, paths: &[&str]) -> StageReq {
        StageReq {
            stream_id: sid.to_string(),
            paths: paths.iter().map(|p| p.to_string()).collect(),
            all: false,
            unstage: false,
        }
    }

    fn message(sid: &str, message: &str) -> CommitReq {
        CommitReq {
            stream_id: sid.to_string(),
            message: message.to_string(),
        }
    }

    fn latest(sid: &str, n: usize) -> LogQuery {
        LogQuery {
            stream_id: sid.to_string(),
            limit: Some(n),
            rev: None,
        }
    }

    #[test]
    fn a_change_goes_from_status_to_a_pushed_merge() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let remote = dir.path().join("remote.git");
        run_git(dir.path(), &["init", "-q", "--bare", remote.to_str().unwrap()]);
        run_git(&repo, &["remote", "add", "origin", remote.to_str().unwrap()]);
        let state = state_with(dir.path(), Arc::new(StubModel));
        let sid = open_stream(&state, &open_req(&repo, "git"))
            .unwrap()
            .context
            .stream_id;
        let work = checkout(&state, &sid).unwrap();
        let main = status(&state, &sid).unwrap().branch.unwrap();
        assert!(status(&state, &sid).unwrap().clean);

        std::fs::write(work.join("new.txt"), "hello\n").unwrap();
        std::fs::write(work.join("f.txt"), "line one\nline two\n").unwrap();
        let file = |st: &GitStatus, p: &str| {
            st.files
                .iter()
                .find(|f| f.path == p)
                .map(|f| (f.staged, f.unstaged, f.untracked))
        };
        let st = status(&state, &sid).unwrap();
        assert_eq!(file(&st, "new.txt"), Some((None, None, true)));
        assert_eq!(file(&st, "f.txt"), Some((None, Some("modified"), false)));
        let q = DiffQuery {
            stream_id: sid.clone(),
            path: Some("f.txt".into()),
            staged: false,
            commit: None,
            base: None,
            worktree: false,
        };
        assert!(diff(&state, &q).unwrap().diff.contains("+line two"));

        let refused = commit(&state, &message(&sid, "x")).unwrap_err();
        assert!(refused.contains("nothing is staged"), "{refused}");
        let all = StageReq {
            all: true,
            ..stage_paths(&sid, &[])
        };
        let st = stage(&state, &all).unwrap();
        assert_eq!(file(&st, "new.txt"), Some((Some("added"), None, false)));
        assert_eq!(commit(&state, &message(&sid, "add new")).unwrap().subject, "add new");

        let feature = BranchReq {
            stream_id: sid.clone(),
            name: "feature".into(),
            from: None,
            switch: true,
        };
        branch(&state, &feature).unwrap();
        std::fs::write(work.join("feature.txt"), "f\n").unwrap();
        stage(&state, &stage_paths(&sid, &["feature.txt"])).unwrap();
        commit(&state, &message(&sid, "feature work")).unwrap();
        assert!(branches(&state, &sid)
            .unwrap()
            .iter()
            .any(|b| b.name == "feature" && b.current));

        let back = SwitchReq {
            stream_id: sid.clone(),
            branch: main.clone(),
        };
        switch(&state, &back).unwrap();
        let merge_req = MergeReq {
            stream_id: sid.clone(),
            branch: "feature".into(),
            message: None,
        };
        merge(&state, &merge_req).unwrap();
        let subjects: Vec<String> = log(&state, &latest(&sid, 5))
            .unwrap()
            .into_iter()
            .map(|c| c.subject)
            .collect();
        assert!(subjects[0].starts_with("Merge branch 'feature'"), "{subjects:?}");
        assert!(subjects.contains(&"feature work".to_string()));

        let push_req = PushReq {
            stream_id: sid.clone(),
            branch: None,
            commit: None,
        };
        push(&state, &push_req).unwrap();
        let pushed = crate::git::run(&remote, &["log", "--format=%s", "-n1", &main]).unwrap();
        assert!(pushed.starts_with("Merge branch 'feature'"), "{pushed}");
        let st = status(&state, &sid).unwrap();
        let upstream = format!("origin/{main}");
        assert_eq!((st.upstream.as_deref(), st.ahead), (Some(upstream.as_str()), 0));
        assert!(rev("--force").is_err(), "a flag is never taken for a branch");

        // Someone else pushes; fetch sees it, pull brings it in.
        let other = dir.path().join("other");
        run_git(
            dir.path(),
            &["clone", "-q", "-b", &main, remote.to_str().unwrap(), other.to_str().unwrap()],
        );
        run_git(&other, &["config", "user.email", "o@example.com"]);
        run_git(&other, &["config", "user.name", "o"]);
        std::fs::write(other.join("theirs.txt"), "t\n").unwrap();
        run_git(&other, &["add", "."]);
        run_git(&other, &["commit", "-qm", "their work"]);
        run_git(&other, &["push", "-q", "origin", &main]);
        let fetch_req = FetchReq {
            stream_id: sid.clone(),
            remote: None,
        };
        assert_eq!(fetch(&state, &fetch_req).unwrap().behind, 1);
        let pulled = pull(&state, &GitQuery { stream_id: sid.clone() }).unwrap();
        assert_eq!((pulled.behind, pulled.ahead), (0, 0));
        assert!(work.join("theirs.txt").exists());
    }

    #[test]
    fn a_merge_preview_shows_the_merged_change_and_its_conflicts_without_touching_the_checkout() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let sid = open_stream(&state, &open_req(&repo, "gitp")).unwrap().context.stream_id;
        let work = checkout(&state, &sid).unwrap();
        let main = status(&state, &sid).unwrap().branch.unwrap();
        let edit = |text: &str, subject: &str| {
            std::fs::write(work.join("f.txt"), text).unwrap();
            stage(&state, &stage_paths(&sid, &["f.txt"])).unwrap();
            commit(&state, &message(&sid, subject)).unwrap()
        };
        let new_branch = |name: &str, switch| BranchReq { stream_id: sid.clone(), name: name.into(), from: None, switch };
        branch(&state, &new_branch("untouched", false)).unwrap();
        branch(&state, &new_branch("other", true)).unwrap();
        edit("theirs\n", "theirs");
        switch(&state, &SwitchReq { stream_id: sid.clone(), branch: main.clone() }).unwrap();
        let head = edit("ours\n", "ours");
        let query = |base: &str| MergePreviewQuery { stream_id: sid.clone(), base: base.into(), diff: true };
        let preview = |base: &str| merge_preview(&state, &query(base)).unwrap();

        let clean = preview("untouched");
        assert!(clean.clean && clean.conflicts.is_empty(), "{clean:?}");
        assert!(clean.diff.contains("+ours"), "what merging into it would change: {}", clean.diff);
        let conflicted = preview("other");
        assert!(!conflicted.clean, "{conflicted:?}");
        assert_eq!(conflicted.conflicts, vec!["f.txt".to_string()]);
        assert!(conflicted.diff.contains("<<<<<<<"), "the conflict is in the diff: {}", conflicted.diff);
        assert!(status(&state, &sid).unwrap().clean, "the checkout was never touched");
        assert_eq!(log(&state, &latest(&sid, 1)).unwrap()[0].sha, head.sha);
        let err = merge_preview(&state, &query("--evil")).unwrap_err();
        assert!(err.contains("not a branch"), "{err}");
    }

    /// The default is a summary: each file a merge would change, with its
    /// lines, and the conflicts, without the diff. Asked for, the diff comes
    /// too, and counts the same. Both stay within their bounds.
    #[test]
    fn a_merge_previews_summary_counts_each_files_lines_without_the_diff() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let sid = open_stream(&state, &open_req(&repo, "gitsum")).unwrap().context.stream_id;
        let work = checkout(&state, &sid).unwrap();
        let main = status(&state, &sid).unwrap().branch.unwrap();
        let commit_all = |subject: &str| {
            stage(&state, &StageReq { all: true, ..stage_paths(&sid, &[]) }).unwrap();
            commit(&state, &message(&sid, subject)).unwrap();
        };
        let new_branch = |name: &str, switch| BranchReq { stream_id: sid.clone(), name: name.into(), from: None, switch };
        std::fs::write(work.join("old.txt"), "one\ntwo\nthree\nfour\nfive\n").unwrap();
        commit_all("old");
        branch(&state, &new_branch("untouched", false)).unwrap();
        branch(&state, &new_branch("other", true)).unwrap();
        std::fs::write(work.join("f.txt"), "theirs\n").unwrap();
        commit_all("theirs");
        switch(&state, &SwitchReq { stream_id: sid.clone(), branch: main.clone() }).unwrap();
        std::fs::write(work.join("f.txt"), "ours\nand more\n").unwrap();
        std::fs::rename(work.join("old.txt"), work.join("moved.txt")).unwrap();
        std::fs::write(work.join("new.txt"), "a\nb\nc\n").unwrap();
        std::fs::write(work.join("pic.bin"), [0u8, 159, 146, 150, 0, 1]).unwrap();
        commit_all("ours");
        let query = |base: &str, diff| MergePreviewQuery { stream_id: sid.clone(), base: base.into(), diff };

        let mut summary = merge_preview(&state, &query("untouched", false)).unwrap();
        summary.files.sort_by(|a, b| a.path.cmp(&b.path));
        assert!(summary.clean && summary.conflicts.is_empty(), "{summary:?}");
        assert!(summary.diff.is_empty() && !summary.truncated && !summary.secret, "{summary:?}");
        let file = |path: &str, from: Option<&str>, added, removed, binary| ChangedFile {
            path: path.into(),
            from: from.map(str::to_string),
            added,
            removed,
            binary,
        };
        assert_eq!(
            summary.files,
            vec![
                file("f.txt", None, 2, 1, false),
                file("moved.txt", Some("old.txt"), 0, 0, false),
                file("new.txt", None, 3, 0, false),
                file("pic.bin", None, 0, 0, true),
            ]
        );
        assert_eq!((summary.added, summary.removed, summary.files_total), (5, 1, 4));
        let mut full = merge_preview(&state, &query("untouched", true)).unwrap();
        full.files.sort_by(|a, b| a.path.cmp(&b.path));
        assert!(full.diff.contains("+and more"), "asked for, the diff: {}", full.diff);
        assert_eq!((full.files, full.added, full.removed), (summary.files, 5, 1), "and it counts the same");

        let conflicted = merge_preview(&state, &query("other", false)).unwrap();
        assert!(!conflicted.clean && conflicted.diff.is_empty(), "{conflicted:?}");
        assert_eq!(conflicted.conflicts, vec!["f.txt".to_string()]);
        assert!(conflicted.files.iter().any(|f| f.path == "f.txt"), "{:?}", conflicted.files);
        assert!(status(&state, &sid).unwrap().clean, "the checkout was never touched");

        // A merge too big for an answer: the first files, all of them
        // counted, and the diff cut at its bound.
        for i in 0..MERGE_FILES_SHOWN + 5 {
            std::fs::write(work.join(format!("many-{i:03}.txt")), format!("{}\n", "x".repeat(400))).unwrap();
        }
        commit_all("many");
        let big = merge_preview(&state, &query("untouched", false)).unwrap();
        assert_eq!((big.files.len(), big.files_total), (MERGE_FILES_SHOWN, MERGE_FILES_SHOWN + 5 + 4));
        assert_eq!(big.added, 5 + MERGE_FILES_SHOWN + 5, "every file's lines");
        let big = merge_preview(&state, &query("untouched", true)).unwrap();
        assert!(big.truncated && big.diff.len() <= MERGE_DIFF_BYTES, "{}", big.diff.len());
    }

    /// `git diff --numstat -z`'s records: a rename's paths come after an
    /// empty one; a binary file counts no lines.
    #[test]
    fn numstat_records_read_renames_and_binaries() {
        let raw = "1\t2\tsrc/a b.rs\0-\t-\tlogo.png\x003\t0\t\0old\tname.txt\0new name.txt\0";
        let files = parse_numstat(raw);
        let summary: Vec<(&str, Option<&str>, usize, usize, bool)> =
            files.iter().map(|f| (f.path.as_str(), f.from.as_deref(), f.added, f.removed, f.binary)).collect();
        assert_eq!(
            summary,
            vec![
                ("src/a b.rs", None, 1, 2, false),
                ("logo.png", None, 0, 0, true),
                ("new name.txt", Some("old\tname.txt"), 3, 0, false),
            ]
        );
        assert!(parse_numstat("").is_empty());
    }

    #[test]
    fn a_chained_preview_shows_branches_that_conflict_only_with_each_other() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let sid = open_stream(&state, &open_req(&repo, "gitch")).unwrap().context.stream_id;
        let work = checkout(&state, &sid).unwrap();
        let main = status(&state, &sid).unwrap().branch.unwrap();
        let on_branch = |name: &str, file: &str, text: &str| {
            branch(&state, &BranchReq { stream_id: sid.clone(), name: name.into(), from: Some(main.clone()), switch: true }).unwrap();
            std::fs::write(work.join(file), text).unwrap();
            stage(&state, &stage_paths(&sid, &[file])).unwrap();
            commit(&state, &message(&sid, name)).unwrap();
            switch(&state, &SwitchReq { stream_id: sid.clone(), branch: main.clone() }).unwrap();
        };
        on_branch("a", "a.txt", "from a\n");
        on_branch("b", "f.txt", "from b\n");
        on_branch("c", "f.txt", "from c\n");
        let refs = || crate::git::run(&work, &["for-each-ref"]).unwrap();
        let (refs_before, head) = (refs(), log(&state, &latest(&sid, 1)).unwrap()[0].sha.clone());

        let query = |diff| MergeChainQuery {
            stream_id: sid.clone(),
            base: main.clone(),
            branches: vec!["a".into(), "b".into(), "c".into()],
            diff,
        };
        let chain = merge_chain(&state, &query(true)).unwrap();
        let steps: Vec<(&str, bool)> = chain.steps.iter().map(|s| (s.branch.as_str(), s.clean)).collect();
        assert_eq!(steps, vec![("a", true), ("b", true), ("c", false)], "each merges into base alone; c conflicts with b");
        assert_eq!(chain.steps[2].conflicts, vec!["f.txt".to_string()]);
        assert!(chain.diff.contains("+from a") && chain.diff.contains("<<<<<<<"), "{}", chain.diff);
        assert!(status(&state, &sid).unwrap().clean, "the checkout was never touched");
        assert_eq!((refs(), log(&state, &latest(&sid, 1)).unwrap()[0].sha.clone()), (refs_before, head), "no ref and no HEAD moved");

        // The default, a summary: the same steps, every file the chain
        // changes, no diff.
        let summary = merge_chain(&state, &query(false)).unwrap();
        let summed: Vec<(&str, bool)> = summary.steps.iter().map(|s| (s.branch.as_str(), s.clean)).collect();
        assert_eq!(summed, steps);
        let paths: Vec<&str> = summary.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["a.txt", "f.txt"]);
        assert_eq!(summary.files, chain.files, "the full chain counts the same");
        assert!(summary.diff.is_empty() && !summary.truncated && !summary.secret, "{summary:?}");
        assert!(merge_chain(&state, &MergeChainQuery { branches: vec![], ..query(false) }).is_err());

        // Both clients' list encodings, the diff flag, and the summary flag
        // an older client sends, which is the default now.
        for query in ["branches=a,b", "branches=%5B%22a%22%2C%22b%22%5D"] {
            let uri: axum::http::Uri = format!("http://node/?stream_id=s&base=main&{query}&diff=true").parse().unwrap();
            let Query(parsed) = Query::<MergeChainQuery>::try_from_uri(&uri).unwrap();
            assert_eq!(parsed.branches, vec!["a".to_string(), "b".to_string()], "{query}");
            assert!(parsed.diff, "{query}");
        }
        let uri: axum::http::Uri = "http://node/?stream_id=s&base=main&branches=a&summary=true".parse().unwrap();
        assert!(!Query::<MergeChainQuery>::try_from_uri(&uri).unwrap().0.diff);
    }

    /// With no diff, a summary still says when the merge changes a secret
    /// file, from its path. The file is only ever in git's objects: a room
    /// keeps credential-named files from being written.
    #[test]
    fn a_summary_that_names_a_secret_file_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let sid = open_stream(&state, &open_req(&repo, "gitsumsecret")).unwrap().context.stream_id;
        let work = checkout(&state, &sid).unwrap();
        let main = status(&state, &sid).unwrap().branch.unwrap();
        let out = |args: &[&str], input: &str| {
            use std::io::Write;
            let mut child = std::process::Command::new("git")
                .arg("-C")
                .arg(&work)
                .args(args)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
            let done = child.wait_with_output().unwrap();
            assert!(done.status.success(), "git {args:?}");
            String::from_utf8(done.stdout).unwrap().trim().to_string()
        };
        let blob = out(&["hash-object", "-w", "--stdin"], "{}\n");
        let listing = format!("{}100644 blob {blob}\tsecrets.json\n", crate::git::run(&work, &["ls-tree", "HEAD"]).unwrap());
        let tree = out(&["mktree"], &listing);
        let leak = out(&["commit-tree", &tree, "-p", "HEAD", "-m", "leak"], "");
        run_git(&work, &["update-ref", "refs/heads/leaky", &leak]);

        let q = MergeChainQuery { stream_id: sid.clone(), base: main, branches: vec!["leaky".into()], diff: false };
        let summary = merge_chain(&state, &q).unwrap();
        assert_eq!(summary.files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), vec!["secrets.json"]);
        assert!(summary.secret && summary.diff.is_empty(), "{summary:?}");
        let renamed = [ChangedFile { path: "notes.txt".into(), from: Some("secrets.json".into()), added: 0, removed: 0, binary: false }];
        assert!(names_a_secret(&state, &work, &renamed), "a rename's old path counts too");
        assert!(!names_a_secret(&state, &work, &[ChangedFile { from: None, ..renamed[0].clone() }]));
    }

    /// Rule 2's timer: a repository with an open thread is fetched, then not
    /// again for five minutes; one that can't be reached waits longer each
    /// time, up to half an hour.
    #[test]
    fn open_threads_repos_are_fetched_every_five_minutes_and_less_when_offline() {
        use std::time::Duration;
        assert_eq!([0, 1, 2, 3, 9].map(fetch_gap), [5, 10, 20, 30, 30].map(|m| Duration::from_secs(m * 60)));
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let online = crate::git::tests::behind_its_origin(dir.path());
        let offline = make_repo(&dir.path().join("offline"));
        crate::testutil::run_git(&offline, &["remote", "add", "origin", "/nowhere/at/all.git"]);
        for (repo, name) in [(&online, "online"), (&offline, "offline")] {
            let root = crate::catalog::DirectoryRootSpec::Attach(&repo.to_string_lossy());
            crate::catalog::create_project(&state, name, Some(root)).unwrap();
        }
        let mut due = std::collections::HashMap::new();
        let now = std::time::Instant::now();
        fetch_tick(&state, &mut due, now);
        let at = |repo: &Path| due[&crate::git::repo_key(repo)];
        assert_eq!(at(&online), (0, now + Duration::from_secs(5 * 60)));
        assert_eq!(at(&offline), (1, now + Duration::from_secs(10 * 60)));
        let fetched = crate::git::last_fetch(&online).unwrap().at;
        fetch_tick(&state, &mut due, now + Duration::from_secs(60));
        assert_eq!(crate::git::last_fetch(&online).unwrap().at, fetched, "not again within five minutes");
    }

    /// An agent's commit through the Bakery runs the project's own hooks in
    /// its room, in git's order: a failing pre-commit stops it with what it
    /// printed; one that rewrites and re-adds a file (lint-staged) changes
    /// what is committed; a commit-msg hook edits the message.
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn an_agents_commit_runs_the_projects_hooks_in_its_room() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let sid = open_stream(&state, &open_req(&repo, "hooks")).unwrap().context.stream_id;
        let work = checkout(&state, &sid).unwrap();
        let hooks = repo.join(".git/hooks");
        let hook = |name: &str, body: &str| {
            let path = hooks.join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        let agent = |f: &dyn Fn() -> Result<Commit, String>| crate::actor::with("agent:orchestrator".into(), f);
        std::fs::write(work.join("f.txt"), "unformatted\n").unwrap();
        stage(&state, &stage_paths(&sid, &["f.txt"])).unwrap();

        hook("pre-commit", "echo 'lint failed' >&2; exit 1");
        let refused = agent(&|| commit(&state, &message(&sid, "blocked"))).unwrap_err();
        assert!(refused.contains("pre-commit") && refused.contains("lint failed"), "{refused}");

        hook("pre-commit", "printf 'formatted\\n' > f.txt && git add f.txt");
        hook("commit-msg", "printf '\\nChecked-by: project hook\\n' >> \"$1\"");
        let made = agent(&|| commit(&state, &message(&sid, "formatted by the hook"))).unwrap();
        let show = |what: &str| crate::git::run(&work, &["show", "-s", &format!("--format={what}"), &made.sha]).unwrap();
        assert!(show("%B").contains("Checked-by: project hook"), "{}", show("%B"));
        let content = crate::git::run(&work, &["show", &format!("{}:f.txt", made.sha)]).unwrap();
        assert_eq!(content, "formatted\n", "the hook's re-added file is what was committed");
    }

    /// A rebase states its reason first: without one it is refused; with one
    /// it runs, the reason is in History, and the turn's end finds nothing to
    /// mark.
    #[test]
    fn a_rebase_records_its_reason_first() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let sid = open_stream(&state, &open_req(&repo, "rebase")).unwrap().context.stream_id;
        let work = checkout(&state, &sid).unwrap();
        let base = String::from_utf8(std::process::Command::new("git").current_dir(&repo).args(["branch", "--show-current"]).output().unwrap().stdout).unwrap();
        let base = base.trim().to_string();
        std::fs::write(work.join("thread.txt"), "thread\n").unwrap();
        stage(&state, &stage_paths(&sid, &["thread.txt"])).unwrap();
        commit(&state, &message(&sid, "thread")).unwrap();
        std::fs::write(repo.join("later.txt"), "later\n").unwrap();
        crate::testutil::run_git(&repo, &["add", "."]);
        crate::testutil::run_git(&repo, &["commit", "-qm", "later"]);

        let since = chrono::Utc::now().timestamp() - 1;
        let req = |reason: &str| RebaseReq { stream_id: sid.clone(), onto: base.clone(), reason: reason.to_string() };
        assert!(rebase(&state, &req("  ")).unwrap_err().contains("stated reason"));
        let done = rebase(&state, &req("a linear history for the release")).unwrap();
        assert!(done.merged && done.conflicts.is_empty(), "{}", done.output);
        assert!(work.join("later.txt").is_file(), "rebased onto the base");
        crate::git_hooks::check_rebases(&state, &sid, "a1", "t1", since);
        let heart = state.heart.lock().unwrap();
        let reason = heart.last_of_family(&sid, crate::git_hooks::REASONED_FAMILY).unwrap().unwrap();
        assert!(reason.payload.contains("a linear history"), "{}", reason.payload);
        assert!(heart.last_of_family(&sid, crate::git_hooks::UNEXPLAINED_FAMILY).unwrap().is_none(), "nothing to mark");
    }

    /// A diff that shows a credential file's lines is marked secret, so the
    /// app takes no screenshot while it shows it.
    #[test]
    fn a_diff_showing_a_secret_files_lines_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let sid = open_stream(&state, &open_req(&repo, "gitsecret")).unwrap().context.stream_id;
        let work = checkout(&state, &sid).unwrap();
        let unstaged = DiffQuery { stream_id: sid.clone(), path: None, staged: false, commit: None, base: None, worktree: false };
        std::fs::write(work.join("f.txt"), "changed\n").unwrap();
        assert!(!diff(&state, &unstaged).unwrap().secret);
        std::fs::write(work.join("secrets.json"), "{}\n").unwrap();
        stage(&state, &stage_paths(&sid, &["secrets.json"])).unwrap();
        assert!(diff(&state, &DiffQuery { staged: true, ..unstaged }).unwrap().secret);
    }

    #[test]
    fn a_branch_diffs_against_its_base_as_one_change() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let sid = open_stream(&state, &open_req(&repo, "gitb"))
            .unwrap()
            .context
            .stream_id;
        let work = checkout(&state, &sid).unwrap();
        let main = status(&state, &sid).unwrap().branch.unwrap();
        let feature = BranchReq {
            stream_id: sid.clone(),
            name: "feature".into(),
            from: None,
            switch: true,
        };
        branch(&state, &feature).unwrap();
        for (file, subject) in [("a.txt", "first"), ("b.txt", "second")] {
            std::fs::write(work.join(file), format!("{subject}\n")).unwrap();
            stage(&state, &stage_paths(&sid, &[file])).unwrap();
            commit(&state, &message(&sid, subject)).unwrap();
        }
        let q = DiffQuery {
            stream_id: sid.clone(),
            path: None,
            staged: false,
            commit: None,
            base: Some(main.clone()),
            worktree: false,
        };
        let whole = diff(&state, &q).unwrap().diff;
        assert!(whole.contains("+first") && whole.contains("+second"), "{whole}");

        std::fs::write(work.join("a.txt"), "first\nnot committed yet\n").unwrap();
        let with_worktree = |base: Option<String>| DiffQuery {
            stream_id: sid.clone(),
            path: None,
            staged: false,
            commit: None,
            base,
            worktree: true,
        };
        let all = diff(&state, &with_worktree(Some(main.clone()))).unwrap().diff;
        assert!(all.contains("+second") && all.contains("+not committed yet"), "{all}");
        assert!(!diff(&state, &q).unwrap().diff.contains("not committed yet"), "without it, the commits alone");
        assert!(diff(&state, &with_worktree(None)).is_err(), "the working tree needs a base");

        let both = DiffQuery {
            commit: Some("HEAD".into()),
            base: Some(main),
            ..q
        };
        assert!(diff(&state, &both).is_err(), "one commit or a base, never both");
        let flag = DiffQuery {
            commit: None,
            base: Some("--output=/tmp/x".into()),
            ..both
        };
        assert!(diff(&state, &flag).is_err(), "a flag is never taken for a base");
    }

    #[test]
    fn a_conflicting_merge_changes_nothing_and_an_agents_hooks_run_only_in_its_room() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let sid = open_stream(&state, &open_req(&repo, "gitc"))
            .unwrap()
            .context
            .stream_id;
        let work = checkout(&state, &sid).unwrap();
        let main = status(&state, &sid).unwrap().branch.unwrap();
        let edit = |text: &str, subject: &str| {
            std::fs::write(work.join("f.txt"), text).unwrap();
            stage(&state, &stage_paths(&sid, &["f.txt"])).unwrap();
            commit(&state, &message(&sid, subject)).unwrap()
        };
        let other = BranchReq {
            stream_id: sid.clone(),
            name: "other".into(),
            from: None,
            switch: true,
        };
        branch(&state, &other).unwrap();
        edit("theirs\n", "theirs");
        let back = SwitchReq {
            stream_id: sid.clone(),
            branch: main.clone(),
        };
        switch(&state, &back).unwrap();
        let before = edit("ours\n", "ours");
        let merge_req = MergeReq {
            stream_id: sid.clone(),
            branch: "other".into(),
            message: None,
        };
        let err = merge(&state, &merge_req).unwrap_err();
        assert!(err.contains("f.txt") && err.contains("nothing changed"), "{err}");
        assert!(status(&state, &sid).unwrap().clean, "the merge was aborted");
        assert_eq!(log(&state, &latest(&sid, 1)).unwrap()[0].sha, before.sha);

        // A hook planted in the repository's own hooks folder, put there
        // directly: the node's git points its hooks elsewhere (`git::run`).
        let common = std::process::Command::new("git")
            .arg("-C")
            .arg(&work)
            .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
            .output()
            .unwrap();
        let hooks = PathBuf::from(String::from_utf8(common.stdout).unwrap().trim()).join("hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        let marker = dir.path().join("hook-ran");
        let hook = hooks.join("pre-commit");
        std::fs::write(&hook, format!("#!/bin/sh\ntouch {}\n", marker.display())).unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        // The operator's commit through the app runs no project hook: the
        // node's git has them off, and none runs in a room for them.
        let theirs = edit("operator\n", "by the operator");
        assert_eq!(log(&state, &latest(&sid, 1)).unwrap()[0].sha, theirs.sha, "the operator's commit landed");
        assert!(!marker.exists(), "the operator's commit through the app ran a project hook");
        // An agent's commit runs the hook in the agent's room, which can't
        // write outside it: the hook fails there and stops the commit.
        std::fs::write(work.join("f.txt"), "agent\n").unwrap();
        stage(&state, &stage_paths(&sid, &["f.txt"])).unwrap();
        let refused = crate::actor::with("agent:baker".into(), || commit(&state, &message(&sid, "by an agent"))).unwrap_err();
        assert!(refused.contains("pre-commit"), "{refused}");
        assert!(!marker.exists(), "an agent's commit ran a project hook outside its room");
        assert_eq!(log(&state, &latest(&sid, 1)).unwrap()[0].sha, theirs.sha, "the agent's commit was stopped");
    }

    /// An agent's switch, new branch or pull never brings in a change to a
    /// runtime's own settings; the operator's does.
    #[test]
    fn an_agents_git_never_checks_out_a_runtimes_settings() {
        fn as_agent<T>(f: impl FnOnce() -> T) -> T {
            crate::actor::with("agent:orchestrator".into(), f)
        }
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let remote = dir.path().join("remote.git");
        run_git(dir.path(), &["init", "-q", "--bare", remote.to_str().unwrap()]);
        run_git(&repo, &["remote", "add", "origin", remote.to_str().unwrap()]);
        let state = state_with(dir.path(), Arc::new(StubModel));
        let sid = open_stream(&state, &open_req(&repo, "gits"))
            .unwrap()
            .context
            .stream_id;
        let work = checkout(&state, &sid).unwrap();
        let main = status(&state, &sid).unwrap().branch.unwrap();
        let settings = work.join(".claude/settings.json");
        let to = |branch: &str| SwitchReq {
            stream_id: sid.clone(),
            branch: branch.into(),
        };
        let new = |name: &str, from: &str| BranchReq {
            stream_id: sid.clone(),
            name: name.into(),
            from: Some(from.into()),
            switch: true,
        };
        branch(&state, &new("loose", &main)).unwrap();
        std::fs::create_dir_all(work.join(".claude")).unwrap();
        std::fs::write(&settings, "{\"permissions\":{\"allow\":[\"Bash(*)\"]}}\n").unwrap();
        stage(&state, &stage_paths(&sid, &[".claude/settings.json"])).unwrap();
        commit(&state, &message(&sid, "loosen")).unwrap();
        switch(&state, &to(&main)).unwrap();
        assert!(!settings.exists());

        for err in [
            as_agent(|| switch(&state, &to("loose"))).unwrap_err(),
            as_agent(|| branch(&state, &new("copy", "loose"))).unwrap_err(),
        ] {
            assert!(err.contains(".claude/settings.json"), "{err}");
        }
        assert!(!settings.exists());
        assert_eq!(status(&state, &sid).unwrap().branch, Some(main.clone()));
        as_agent(|| branch(&state, &new("fine", &main))).unwrap();
        as_agent(|| switch(&state, &to(&main))).unwrap();

        run_git(&work, &["push", "-q", "-u", "origin", &main]);
        run_git(&work, &["push", "-q", "origin", &format!("loose:{main}")]);
        let pull_req = GitQuery {
            stream_id: sid.clone(),
        };
        let err = as_agent(|| pull(&state, &pull_req)).unwrap_err();
        assert!(err.contains(".claude/settings.json"), "{err}");
        assert!(!settings.exists());
        pull(&state, &pull_req).unwrap();
        assert!(settings.exists(), "the operator's pull brings them in");
    }
}
