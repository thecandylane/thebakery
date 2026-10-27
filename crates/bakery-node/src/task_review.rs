//! Complete task review + sign-off (07d-B Phase 4 item 2).
//!
//! Today's Review renders recorded edit proposals only; this surface is the
//! **complete** change set against the task's own baseline
//! (`task.workspace.materialized`) — worktree vs base commit, untracked
//! files included, computed fresh on every call (never cached — see
//! `task_driver.rs`'s bounded-staleness cache for the Floor-row counters,
//! which is a *different* surface with a deliberately looser freshness bar).
//! Sign-off pins a commit that provably contains exactly what was reviewed;
//! any later change to the tree un-pins it back to `awaiting_review`.
//!
//! Plain-directory (non-git) task workspaces have no baseline snapshot yet
//! (`tasks::task_start`'s own "Known gap" doc comment — materialization for
//! that root kind is not built) — `GET /task/review` refuses honestly for
//! those and for chat-only tasks rather than fabricating an empty diff.

use crate::state::AppState;
use crate::tasks::{TaskRecord, TaskStatus, TaskStatusChanged, TASK_STATUS_FAMILY};
use axum::{
    extract::{Query, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};

pub(crate) const TASK_SIGNED_OFF_FAMILY: &str = "task.signed_off";

/// Raw-bytes cap on what sign-off will pin by hash. Past this, the file is
/// marked unreviewable and sign-off refuses — an honest refusal beats a
/// silent partial hash of something nobody can have reviewed.
const MAX_REVIEW_RAW_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TaskSignedOff {
    task_id: String,
    commit: String,
    /// The reviewed-snapshot id the human approved (see
    /// [`TaskReviewResp::snapshot`]). `None` only on events recorded before
    /// pinning existed.
    #[serde(default)]
    snapshot: Option<String>,
}

#[derive(schemars::JsonSchema, Debug, Clone, Serialize)]
pub(crate) struct ReviewFile {
    pub(crate) path: String,
    /// `"added" | "modified" | "deleted" | "unreviewable"` —
    /// `TaskReviewResp`'s spec-required name-status, absent from the
    /// client's placeholder shape (07d-B Stage D: the tree state note says
    /// to keep wire shapes compatible "or change them deliberately on both
    /// sides" — this is the deliberate addition). `"unreviewable"` is this
    /// pass's own addition: a git status letter this surface cannot render
    /// as an old/new diff (today, only `U` — unmerged) still gets a visible
    /// row instead of vanishing.
    pub(crate) status: &'static str,
    pub(crate) added: usize,
    pub(crate) removed: usize,
    pub(crate) old_content: String,
    pub(crate) new_content: String,
    /// Why the displayed content is not the complete bytes, when it isn't
    /// — `"binary"`, `"truncated for display"`, `"refused"`, `"oversize"`.
    /// The UI renders this as an explicit chip; the snapshot below pins the
    /// underlying bytes regardless of displayability.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) note: Option<String>,
    /// `false` when the actual bytes could not even be hashed (jail/
    /// credential refusal, unreadable, oversize) — sign-off refuses while
    /// any such file is in the change set, because `git add -A` would
    /// commit content nobody reviewed OR pinned. A binary file is
    /// `true`: not displayable, but its exact bytes are hashed into the
    /// snapshot, which is the explicit handling.
    pub(crate) reviewable: bool,
    /// Content identity the snapshot is built from (blake3 of raw disk
    /// bytes for the new side; the git blob OID for the base side).
    #[serde(skip)]
    pub(crate) old_id: String,
    #[serde(skip)]
    pub(crate) new_id: String,
}

#[derive(schemars::JsonSchema, Debug, Clone, Serialize)]
pub(crate) struct TaskReviewResp {
    pub(crate) task_id: String,
    pub(crate) base_commit: Option<String>,
    pub(crate) files: Vec<ReviewFile>,
    pub(crate) signed_off: bool,
    pub(crate) signoff_commit: Option<String>,
    /// Content-derived identifier of exactly this reviewed state: blake3
    /// over the base commit plus every file's (path, status, base-side blob
    /// OID, new-side raw-byte hash) in sorted order. `POST /task/signoff`
    /// requires it back and refuses when the workspace no longer matches —
    /// concurrent changes are excluded, never silently swept in by
    /// `git add -A`.
    pub(crate) snapshot: String,
    /// The project repo's currently checked-out branch — what
    /// `/task/integrate` would merge into. The client shows it and sends it
    /// back as the pinned `target_branch`; the server re-verifies at merge
    /// time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) repo_current_branch: Option<String>,
}

/// `task.workspace.materialized.base_commit` for this task, read directly
/// off the parent project's stream (the fact `tasks::task_start` appends —
/// see that module's doc comment on why baseline lives there, not on the
/// task's own socket stream).
fn materialized_base_commit(state: &AppState, project_id: &str, task_id: &str) -> Option<String> {
    let heart = state.heart.lock().unwrap();
    let events = heart.read_stream(project_id).ok()?;
    events
        .iter()
        .filter(|e| e.family == crate::tasks::TASK_WORKSPACE_MATERIALIZED_FAMILY)
        .filter_map(|e| {
            serde_json::from_str::<crate::tasks::TaskWorkspaceMaterialized>(&e.payload).ok()
        })
        .filter(|m| m.task_id == task_id)
        .last()
        .map(|m| m.base_commit)
}

/// Latest `task.signed_off` commit hash recorded for this task, or `None` if
/// it has never been signed off. `pub(crate)`: `tasks::task_start_as`'s
/// ancestry gate and the task ship ops verify against exactly this commit.
pub(crate) fn latest_signoff_commit(
    state: &AppState,
    project_id: &str,
    task_id: &str,
) -> Option<String> {
    let heart = state.heart.lock().unwrap();
    let events = heart.read_stream(project_id).ok()?;
    events
        .iter()
        .filter(|e| e.family == TASK_SIGNED_OFF_FAMILY)
        .filter_map(|e| serde_json::from_str::<TaskSignedOff>(&e.payload).ok())
        .filter(|s| s.task_id == task_id)
        .last()
        .map(|s| s.commit)
}

/// `(head, worktree is clean)` for `worktree_path` — "clean" here means no
/// uncommitted changes at all (`git status --porcelain` empty), which is the
/// exact condition sign-off equivalence needs: the signed-off commit is only
/// still "what was reviewed" when HEAD matches it AND nothing has moved
/// since.
fn worktree_head_and_clean(worktree_path: &str) -> Result<(String, bool), String> {
    let p = std::path::Path::new(worktree_path);
    let head = crate::git::run(p, &["rev-parse", "HEAD"])?
        .trim()
        .to_string();
    let clean = crate::git::porcelain_status(p)?.is_empty();
    Ok((head, clean))
}

/// One side's read result: what the UI shows, what the snapshot pins, and
/// whether the underlying bytes were actually captured.
struct SideContent {
    display: String,
    /// Content identity for the snapshot (blake3 hex of raw bytes, a git
    /// blob OID, or a marker-derived value for refused paths).
    id: String,
    note: Option<String>,
    reviewable: bool,
}

impl SideContent {
    fn absent() -> Self {
        Self {
            display: String::new(),
            id: String::new(),
            note: None,
            reviewable: true,
        }
    }
    fn refused(marker: String, note: &str) -> Self {
        let id = format!("refused:{}", blake3::hash(marker.as_bytes()).to_hex());
        Self {
            display: marker,
            id,
            note: Some(note.to_string()),
            reviewable: false,
        }
    }
}

/// Refuses a credential-shaped filename (`bakery_exec::is_credential_filename`
/// — the same fence `tools::Jail::read_file` enforces for an agent's own
/// reads) without ever opening it, and refuses a path that escapes the
/// worktree (`tools::Jail::resolve`'s lexical-then-physical check: a tracked
/// or untracked file `git` lists can still be a symlink a task worker
/// planted to point outside its own room — `ln -s` never reads the target,
/// so Manual/Auto write-mode approval never sees it, and this node process
/// itself runs unsandboxed). Both refusals render a visible marker in place
/// of content rather than silently showing nothing — a reviewer must see
/// THAT something was refused, not a blank file that reads as "no changes"
/// — and both mark the file `reviewable: false`, which blocks sign-off.
fn read_content(jail: &crate::tools::Jail, rel: &str) -> SideContent {
    let path = match jail.resolve(rel) {
        Ok(p) => p,
        Err(e) => return SideContent::refused(format!("[refused: {e}]"), "refused"),
    };
    if bakery_exec::is_credential_filename(&path) {
        return SideContent::refused(
            format!("[{}]", bakery_exec::CREDENTIAL_SURFACE_DENIED),
            "refused",
        );
    }
    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    if size > MAX_REVIEW_RAW_BYTES {
        return SideContent::refused(
            format!("[file is {size} bytes — larger than the {MAX_REVIEW_RAW_BYTES}-byte review cap; sign-off will refuse until it is removed or ignored]"),
            "oversize",
        );
    }
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) => return SideContent::refused(format!("[unreadable: {e}]"), "refused"),
    };
    let digest = blake3::hash(&bytes).to_hex().to_string();
    match String::from_utf8(bytes) {
        Ok(s) => {
            let display =
                crate::agent_loop::truncate(&s, crate::task_driver::MAX_REVIEW_FILE_BYTES);
            let note = (display.len() < s.len()).then(|| "truncated for display".to_string());
            SideContent {
                display,
                id: digest,
                note,
                reviewable: true,
            }
        }
        Err(e) => {
            let n = e.as_bytes().len();
            SideContent {
                display: format!(
                    "[binary file — {n} bytes, blake3 {} — not displayable; sign-off pins exactly these bytes]",
                    &digest[..16.min(digest.len())]
                ),
                id: digest,
                note: Some("binary".to_string()),
                reviewable: true,
            }
        }
    }
}

/// Base-side content: display via `git show` (capped), identity via the
/// blob OID `git ls-tree` reports — the OID pins the exact base bytes
/// without this process re-hashing them, and it is what the snapshot uses.
fn show_content(repo_dir: &std::path::Path, rev: &str, path: &str) -> SideContent {
    let oid = crate::git::run(repo_dir, &["rev-parse", &format!("{rev}:{path}")])
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    if oid.is_empty() {
        return SideContent::absent(); // did not exist at `rev` — an Added file
    }
    match crate::git::run(repo_dir, &["show", &format!("{rev}:{path}")]) {
        Ok(s) => {
            let display =
                crate::agent_loop::truncate(&s, crate::task_driver::MAX_REVIEW_FILE_BYTES);
            let note = (display.len() < s.len()).then(|| "truncated for display".to_string());
            let binary = display.contains('\u{0}');
            SideContent {
                display: if binary {
                    format!(
                        "[binary base content — blob {} — not displayable]",
                        &oid[..12]
                    )
                } else {
                    display
                },
                id: oid,
                note: if binary {
                    Some("binary".to_string())
                } else {
                    note
                },
                reviewable: true,
            }
        }
        Err(_) => SideContent {
            display: format!(
                "[base content unavailable — blob {}]",
                &oid[..12.min(oid.len())]
            ),
            id: oid,
            note: Some("binary".to_string()),
            reviewable: true,
        },
    }
}

/// Recompute the snapshot from a COMMIT's own immutable tree (never the
/// mutable checkout): name-status of `base..commit`, base-side blob OIDs,
/// and blake3 over the commit's actual blob bytes — the same identity
/// basis `compute_files` derives from disk at review time, so a commit
/// that captured exactly the reviewed bytes reproduces the approved
/// snapshot and anything else (a racer's write staged into the commit,
/// even one later "restored" in the working directory) cannot.
fn snapshot_at_commit(
    dir: &std::path::Path,
    base_commit: &str,
    commit: &str,
) -> Result<String, String> {
    let mut files: Vec<ReviewFile> = Vec::new();
    let blank = |path: &str, status: &'static str, old_id: String, new_id: String| ReviewFile {
        path: path.to_string(),
        status,
        added: 0,
        removed: 0,
        old_content: String::new(),
        new_content: String::new(),
        note: None,
        reviewable: true,
        old_id,
        new_id,
    };
    let old_oid = |path: &str| {
        crate::git::run(dir, &["rev-parse", &format!("{base_commit}:{path}")])
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    };
    let new_hash = |path: &str| -> Result<String, String> {
        let bytes = crate::git::cat_blob(dir, &format!("{commit}:{path}"))?;
        Ok(blake3::hash(&bytes).to_hex().to_string())
    };
    let name_status = crate::git::run(dir, &["diff", "--name-status", base_commit, commit])?;
    for line in name_status.lines() {
        let mut parts = line.split('\t');
        let Some(code) = parts.next() else { continue };
        let code = code.trim();
        if code.is_empty() {
            continue;
        }
        let first = code.chars().next().unwrap_or(' ');
        match first {
            'A' | 'M' | 'D' | 'T' => {
                let Some(path) = parts.next() else { continue };
                let status = match first {
                    'A' => "added",
                    'D' => "deleted",
                    _ => "modified",
                };
                let old_id = if first == 'A' {
                    String::new()
                } else {
                    old_oid(path)
                };
                let new_id = if first == 'D' {
                    String::new()
                } else {
                    new_hash(path)?
                };
                files.push(blank(path, status, old_id, new_id));
            }
            'R' | 'C' => {
                let (Some(old_path), Some(new_path)) = (parts.next(), parts.next()) else {
                    continue;
                };
                files.push(blank(old_path, "deleted", old_oid(old_path), String::new()));
                files.push(blank(new_path, "added", String::new(), new_hash(new_path)?));
            }
            _ => {
                let path = parts.next().unwrap_or(code);
                files.push(blank(
                    path,
                    "unreviewable",
                    String::new(),
                    format!("unreviewable:{first}"),
                ));
            }
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(snapshot_of(base_commit, &files))
}

/// The reviewed-state identifier: base commit + every file's identity, in
/// the sorted order `compute_files` returns. Deterministic, content-derived
/// — equal iff the reviewable state is byte-identical.
fn snapshot_of(base_commit: &str, files: &[ReviewFile]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(base_commit.as_bytes());
    for f in files {
        hasher.update(b"\x1f");
        hasher.update(f.path.as_bytes());
        hasher.update(b"\x1e");
        hasher.update(f.status.as_bytes());
        hasher.update(b"\x1e");
        hasher.update(f.old_id.as_bytes());
        hasher.update(b"\x1e");
        hasher.update(f.new_id.as_bytes());
    }
    hasher.finalize().to_hex().to_string()
}

/// The complete baseline diff for a materialized git task, computed fresh
/// from disk + git on every call — worktree vs `base_commit`, tracked AND
/// untracked changes alike (`git diff <base_commit>` alone never shows
/// untracked files; they are unioned in separately).
fn compute_files(worktree_path: &str, base_commit: &str) -> Result<Vec<ReviewFile>, String> {
    let dir = std::path::Path::new(worktree_path);
    // Every on-disk read below goes through this jail — see `read_capped`'s
    // doc comment on why a raw `dir.join(path)` is not safe for a path `git`
    // merely *lists* (a symlink `git ls-files`/`diff` names is never
    // resolved by git itself).
    let jail = crate::tools::Jail::new(dir)
        .map_err(|e| format!("cannot jail task worktree for review: {e}"))?;
    let mut files: Vec<ReviewFile> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    let name_status = crate::git::run(dir, &["diff", "--name-status", base_commit])?;
    for line in name_status.lines() {
        let mut parts = line.split('\t');
        let Some(code) = parts.next() else { continue };
        let code = code.trim();
        if code.is_empty() {
            continue;
        }
        let first = code.chars().next().unwrap_or(' ');
        match first {
            'A' | 'M' | 'D' | 'T' => {
                let Some(path) = parts.next() else { continue };
                let status = match first {
                    'A' => "added",
                    'D' => "deleted",
                    // `M` and `T` (typechange — e.g. a tracked file replaced
                    // by a symlink) alike: both have a real old side and a
                    // real new side, and both must be VISIBLE in review —
                    // `T` silently falling into a catch-all used to mean
                    // sign-off's `git add -A` could commit a change the
                    // human never saw (the exact bug this arm now fixes).
                    _ => "modified",
                };
                let old = if first == 'A' {
                    SideContent::absent()
                } else {
                    show_content(dir, base_commit, path)
                };
                let new = if first == 'D' {
                    SideContent::absent()
                } else {
                    read_content(&jail, path)
                };
                let (added, removed) =
                    crate::routes::operator::change_counts(&old.display, &new.display);
                seen.insert(path.to_string());
                files.push(ReviewFile {
                    path: path.to_string(),
                    status,
                    added,
                    removed,
                    note: new.note.clone().or(old.note.clone()),
                    reviewable: old.reviewable && new.reviewable,
                    old_id: old.id,
                    new_id: new.id,
                    old_content: old.display,
                    new_content: new.display,
                });
            }
            'R' | 'C' => {
                // Rename/copy: two name-status fields follow (old, new).
                // Rendered honestly as a delete of the old path plus an add
                // of the new one — a reviewer sees both sides, not a
                // synthetic third "renamed" shape this crate's diff viewer
                // does not otherwise support.
                let (Some(old_path), Some(new_path)) = (parts.next(), parts.next()) else {
                    continue;
                };
                let old = show_content(dir, base_commit, old_path);
                let (added_d, removed_d) = crate::routes::operator::change_counts(&old.display, "");
                seen.insert(old_path.to_string());
                files.push(ReviewFile {
                    path: old_path.to_string(),
                    status: "deleted",
                    added: added_d,
                    removed: removed_d,
                    note: old.note.clone(),
                    reviewable: old.reviewable,
                    old_id: old.id,
                    new_id: String::new(),
                    old_content: old.display,
                    new_content: String::new(),
                });
                let new = read_content(&jail, new_path);
                let (added_a, removed_a) = crate::routes::operator::change_counts("", &new.display);
                seen.insert(new_path.to_string());
                files.push(ReviewFile {
                    path: new_path.to_string(),
                    status: "added",
                    added: added_a,
                    removed: removed_a,
                    note: new.note.clone(),
                    reviewable: new.reviewable,
                    old_id: String::new(),
                    new_id: new.id,
                    old_content: String::new(),
                    new_content: new.display,
                });
            }
            _ => {
                // Any other status letter (`U` — unmerged; this workflow
                // never merges, so this is purely defensive) is rendered as
                // an explicit, visible "cannot diff this" row rather than
                // vanishing — the same "never silently drop a change"
                // principle as the `T` fix above, for a shape this surface
                // genuinely does not know how to render as a diff.
                let path = parts.next().unwrap_or(code).to_string();
                if !path.is_empty() && seen.insert(path.clone()) {
                    let marker = format!(
                        "[git reports this path with status '{first}', which this review surface does not know how to diff — inspect it manually with git before signing off]"
                    );
                    files.push(ReviewFile {
                        path,
                        status: "unreviewable",
                        added: 0,
                        removed: 0,
                        note: Some("refused".to_string()),
                        reviewable: false,
                        old_id: String::new(),
                        new_id: format!("unreviewable:{first}"),
                        old_content: String::new(),
                        new_content: marker,
                    });
                }
            }
        }
    }

    let untracked = crate::git::run(dir, &["ls-files", "--others", "--exclude-standard"])?;
    for path in untracked.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if !seen.insert(path.to_string()) {
            continue;
        }
        let new = read_content(&jail, path);
        let (added, removed) = crate::routes::operator::change_counts("", &new.display);
        files.push(ReviewFile {
            path: path.to_string(),
            status: "added",
            added,
            removed,
            note: new.note.clone(),
            reviewable: new.reviewable,
            old_id: String::new(),
            new_id: new.id,
            old_content: String::new(),
            new_content: new.display,
        });
    }

    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

/// How many times [`diff_counts`] has been entered. Studio's snapshot test
/// uses this to prove a repeat `GET /studio/state` did not spawn git.
#[cfg(test)]
pub(crate) static DIFF_COUNTS_CALLS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Counts-only baseline diff (added, removed) — what `SocketInfo`'s Floor
/// row counters cache (`task_driver::changed_counts_cached`). Cheaper than
/// [`compute_files`]: tracked changes come straight from `git diff --numstat`
/// (no `git show`/file-read per changed file), and only untracked files
/// still need their content read (there is no baseline to diff against, so
/// every one of their lines counts as added — matching `compute_files`'s own
/// treatment of an untracked file as wholly "added"). Never cached itself;
/// [`changed_counts_cached`](crate::task_driver::changed_counts_cached) is
/// the caller that adds the bounded-staleness layer for Floor rows — this
/// function is also what the three "always fresh" surfaces (`compute_review`
/// itself uses `compute_files`, not this) never route through.
pub(crate) fn diff_counts(
    worktree_path: &str,
    base_commit: &str,
) -> Result<(usize, usize), String> {
    #[cfg(test)]
    DIFF_COUNTS_CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::path::Path::new(worktree_path);
    let numstat = crate::git::run(dir, &["diff", "--numstat", base_commit])?;
    let (mut added, mut removed) = (0usize, 0usize);
    for line in numstat.lines() {
        let mut parts = line.split('\t');
        added += parts
            .next()
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(0);
        removed += parts
            .next()
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(0);
    }
    // `git diff --numstat` above never touches the filesystem itself (it
    // diffs git's own index/objects vs the worktree through git's own
    // machinery) — only untracked files need a real on-disk read, and only
    // those go through the same jail `compute_files` uses (same escape this
    // module's top finding covers: `git ls-files` merely LISTS a path, it
    // never vouches that it isn't a symlink out of the worktree).
    let jail = crate::tools::Jail::new(dir)
        .map_err(|e| format!("cannot jail task worktree for diff counts: {e}"))?;
    let untracked = crate::git::run(dir, &["ls-files", "--others", "--exclude-standard"])?;
    for path in untracked.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if let Ok(resolved) = jail.resolve(path) {
            if !bakery_exec::is_credential_filename(&resolved) {
                if let Ok(content) = std::fs::read_to_string(&resolved) {
                    added += crate::routes::operator::split_lines(&content).len();
                }
            }
        }
    }
    Ok((added, removed))
}

/// `Ok(None)` for a task with no materialized git baseline (chat-only, or a
/// plain-dir root — not built, see this module's top doc comment): a real
/// refusal, not a fabricated empty diff.
fn compute_review(
    state: &AppState,
    project_id: &str,
    task_id: &str,
) -> Result<(TaskRecord, TaskReviewResp), String> {
    let task = crate::tasks::find_task(state, project_id, task_id)?;
    if task.retired {
        return Err(format!("task {task_id} has been retired"));
    }
    let Some(base_commit) = materialized_base_commit(state, project_id, task_id) else {
        return Err("this task has no materialized git baseline to review yet (chat-only task, or not yet started)".to_string());
    };
    let ctx = crate::state::context_for(state, &task.stream_id)?;
    if ctx.worktree_path.is_empty() || !std::path::Path::new(&ctx.worktree_path).is_dir() {
        return Err("this task's worktree no longer exists on disk".to_string());
    }
    let files = compute_files(&ctx.worktree_path, &base_commit)?;

    let signoff_commit = latest_signoff_commit(state, project_id, task_id);
    let signed_off = match &signoff_commit {
        None => false,
        Some(commit) => match worktree_head_and_clean(&ctx.worktree_path) {
            Ok((head, clean)) => &head == commit && clean,
            Err(_) => false,
        },
    };
    // Sign-off pinning (Phase 4 item 2): a later change un-pins it — never
    // silently stays `done`. Reconciled here (review fetch is one of the
    // spec's two named reconciliation points; driver-wake reconciliation is
    // not additionally wired in this pass — see this crate's remaining_work).
    if !signed_off && task.status == TaskStatus::Done {
        let _ = crate::state::append(
            state,
            project_id,
            "task-review",
            TASK_STATUS_FAMILY,
            serde_json::to_string(&TaskStatusChanged {
                task_id: task_id.to_string(),
                status: TaskStatus::AwaitingReview,
                reason: Some("changed after sign-off".to_string()),
            })
            .unwrap(),
            vec![],
        );
    }

    let snapshot = snapshot_of(&base_commit, &files);
    let repo_current_branch = (!ctx.repo_root.is_empty())
        .then(|| {
            crate::git::run(
                std::path::Path::new(&ctx.repo_root),
                &["rev-parse", "--abbrev-ref", "HEAD"],
            )
            .ok()
            .map(|s| s.trim().to_string())
        })
        .flatten();
    Ok((
        task,
        TaskReviewResp {
            task_id: task_id.to_string(),
            base_commit: Some(base_commit),
            files,
            signed_off,
            signoff_commit,
            snapshot,
            repo_current_branch,
        },
    ))
}

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct TaskReviewQuery {
    pub(crate) project_id: String,
    pub(crate) task_id: String,
}

pub(crate) async fn task_review_route(
    State(st): State<AppState>,
    Query(q): Query<TaskReviewQuery>,
) -> Result<Json<TaskReviewResp>, (StatusCode, String)> {
    let st2 = st.clone();
    crate::actor::spawn_blocking(move || {
        compute_review(&st2, &q.project_id, &q.task_id).map(|(_, resp)| resp)
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map(Json)
    .map_err(|e| (StatusCode::BAD_REQUEST, e))
}

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct TaskSignoffReq {
    pub(crate) project_id: String,
    pub(crate) task_id: String,
    #[serde(default)]
    pub(crate) confirm: bool,
    /// The `TaskReviewResp.snapshot` the human actually reviewed. Required:
    /// sign-off validates the workspace still matches it and refuses
    /// otherwise — approving "whatever is on disk right now" is exactly the
    /// unpinned behavior this field removes.
    #[serde(default)]
    pub(crate) snapshot: String,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct TaskSignoffResp {
    pub(crate) task: crate::tasks::TaskView,
    pub(crate) signoff_commit: String,
}

/// Validates the human's reviewed snapshot against a FRESH recompute under
/// the task's own execution locks, refuses on any mismatch or undecided
/// proposal or unreviewable file, then commits exactly the approved state
/// (`git add -A` + commit — safe now, because the compare just proved the
/// tree IS the reviewed bytes and the locks exclude the task's own agent/
/// proof machinery until the commit lands) and records
/// `task.status.changed{done}` with that exact commit hash.
fn task_signoff(state: &AppState, req: TaskSignoffReq) -> Result<TaskSignoffResp, String> {
    if !req.confirm {
        return Err("sign-off requires confirm: true".to_string());
    }
    if req.snapshot.trim().is_empty() {
        return Err(
            "sign-off requires the reviewed snapshot id — GET /task/review first and send back its `snapshot`"
                .to_string(),
        );
    }
    let task_probe = crate::tasks::find_task(state, &req.project_id, &req.task_id)?;
    // Exclusion for the task's OWN machinery: hold the one-shot lease
    // (proof runs, `run` tool) and the agent-turn lease across
    // compare→add→commit. These leases do NOT cover every writer —
    // `/files/write` and open terminals hold different, non-exclusive
    // counters — which is why the post-commit snapshot verification below
    // exists: any racer is DETECTED and the sign-off record refused, even
    // though it cannot be prevented here. A running turn/proof means the
    // leases are taken — refuse, don't wait.
    let _one_shot = crate::state::admit_one_shot(state, &task_probe.stream_id)
        .map_err(|(_, m)| format!("cannot sign off while the task's room is busy: {m}"))?;
    let _turn = crate::state::admit_turn(
        state,
        &task_probe.stream_id,
        crate::node::Seat::Orchestrator,
    )
    .map_err(|(_, m)| format!("cannot sign off while the task's agent is mid-turn: {m}"))?;

    let (task, review) = compute_review(state, &req.project_id, &req.task_id)?;
    if !matches!(task.status, TaskStatus::AwaitingReview | TaskStatus::Done) {
        return Err(format!(
            "task {} is {:?} — sign-off is only for a task awaiting review",
            task.task_id, task.status
        ));
    }
    if crate::task_driver::has_undecided_gate(state, &task.stream_id) {
        return Err(
            "a pending proposal on this task is still undecided — decide it before signing off"
                .to_string(),
        );
    }
    let unreviewable: Vec<&str> = review
        .files
        .iter()
        .filter(|f| !f.reviewable)
        .map(|f| f.path.as_str())
        .collect();
    if !unreviewable.is_empty() {
        return Err(format!(
            "sign-off refused — {} file(s) in the change set could not be reviewed or pinned (refused/oversize/unreadable): {}",
            unreviewable.len(),
            unreviewable.join(", ")
        ));
    }
    if review.snapshot != req.snapshot {
        return Err(format!(
            "the workspace changed since the review you approved — re-open Review and approve the current state (reviewed snapshot {}, current {})",
            &req.snapshot[..12.min(req.snapshot.len())],
            &review.snapshot[..12]
        ));
    }
    let worktree_path = crate::state::context_for(state, &task.stream_id)?.worktree_path;
    if worktree_path.is_empty() || !std::path::Path::new(&worktree_path).is_dir() {
        return Err("this task's worktree no longer exists on disk".to_string());
    }
    let dir = std::path::Path::new(&worktree_path);

    let pre_head = crate::git::run(dir, &["rev-parse", "HEAD"])?
        .trim()
        .to_string();
    crate::git::run(dir, &["add", "-A"])?;
    // Only actually commit when there is something staged — `git commit`
    // with a clean index exits non-zero and writes its "nothing to commit"
    // explanation to STDOUT, which `git::run`'s error path does not capture
    // (it surfaces stderr only, per its own doc comment); checking status
    // first is honest either way and avoids depending on parsing a message
    // this crate's `git::run` never gives access to.
    let made_commit = if !crate::git::porcelain_status(dir)?.is_empty() {
        crate::git::run(dir, &["commit", "-m", "task sign-off"])?;
        true
    } else {
        false
    };
    let commit = crate::git::run(dir, &["rev-parse", "HEAD"])?
        .trim()
        .to_string();

    // Verify the COMMIT's immutable tree, never the mutable checkout
    // (confirmed gap 2): a racer's write staged into the commit and then
    // "restored" in the working directory would defeat a worktree
    // recompute; hashing the commit's own blobs cannot be defeated that
    // way. On mismatch the just-created commit is rolled back softly
    // (`reset --soft` — HEAD returns to pre-sign-off, no bytes are lost,
    // no unapproved commit is left behind) and nothing is recorded.
    let base = review.base_commit.as_deref().unwrap_or_default();
    let committed_snapshot = snapshot_at_commit(dir, base, &commit)?;
    if committed_snapshot != req.snapshot {
        if made_commit {
            let _ = crate::git::run(dir, &["reset", "--soft", &pre_head]);
        }
        return Err(
            "the commit's content does not match the review you approved (another writer — an editor save \
             or terminal — raced the sign-off, or this repo's gitattributes filters rewrite content on add); \
             the sign-off was refused and the commit rolled back — re-open Review and sign off again"
                .to_string(),
        );
    }

    crate::state::append(
        state,
        &req.project_id,
        "human",
        TASK_SIGNED_OFF_FAMILY,
        serde_json::to_string(&TaskSignedOff {
            task_id: task.task_id.clone(),
            commit: commit.clone(),
            snapshot: Some(review.snapshot.clone()),
        })
        .unwrap(),
        vec![],
    )?;
    crate::state::append(
        state,
        &req.project_id,
        "human",
        TASK_STATUS_FAMILY,
        serde_json::to_string(&TaskStatusChanged {
            task_id: task.task_id.clone(),
            status: TaskStatus::Done,
            reason: Some(format!("signed off {commit}")),
        })
        .unwrap(),
        vec![],
    )?;

    let updated = crate::tasks::find_task(state, &req.project_id, &req.task_id)?;
    Ok(TaskSignoffResp {
        task: updated.into(),
        signoff_commit: commit,
    })
}

/// Test-only convenience used by sibling modules' tests (`task_ship`):
/// fetch the current snapshot and sign off against it in one step.
#[cfg(test)]
pub(crate) fn sign_off_for_test(state: &AppState, project_id: &str, task_id: &str) {
    let (_, review) =
        compute_review(state, project_id, task_id).expect("compute review for test sign-off");
    task_signoff(
        state,
        TaskSignoffReq {
            project_id: project_id.to_string(),
            task_id: task_id.to_string(),
            confirm: true,
            snapshot: review.snapshot,
        },
    )
    .expect("test sign-off");
}

pub(crate) async fn task_signoff_route(
    State(st): State<AppState>,
    Json(req): Json<TaskSignoffReq>,
) -> Result<Json<TaskSignoffResp>, (StatusCode, String)> {
    crate::actor::spawn_blocking(move || task_signoff(&st, req))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::{TaskCreateReq, TaskStartReq};
    use crate::testutil::*;
    use bakery_agent::StubModel;
    use std::sync::Arc;

    fn project_with_git_root(
        state: &AppState,
        dir: &std::path::Path,
    ) -> (String, std::path::PathBuf) {
        let repo = make_repo(&dir.join("repo"));
        let created = crate::catalog::create_project(
            state,
            "P",
            Some(crate::catalog::DirectoryRootSpec::Attach(
                &repo.to_string_lossy(),
            )),
        )
        .expect("create project");
        (created.0.project_id, repo)
    }

    fn fresh_snapshot(state: &AppState, project_id: &str, task_id: &str) -> String {
        compute_review(state, project_id, task_id)
            .unwrap()
            .1
            .snapshot
    }

    fn start_task(
        state: &AppState,
        project_id: &str,
        title: &str,
        work_order: &str,
    ) -> crate::tasks::TaskView {
        let created = crate::tasks::task_create(
            state,
            TaskCreateReq {
                parent_project_id: project_id.to_string(),
                title: title.to_string(),
                work_order: work_order.to_string(),
                contract: Default::default(),
            },
        )
        .unwrap();
        crate::tasks::task_start(
            state,
            TaskStartReq {
                project_id: project_id.to_string(),
                task_id: created.task.task_id.clone(),
            },
        )
        .unwrap();
        crate::tasks::find_task(state, project_id, &created.task.task_id)
            .unwrap()
            .into()
    }

    #[test]
    fn review_includes_an_untracked_file_created_directly_on_disk_with_correct_counts() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let (project_id, _repo) = project_with_git_root(&state, dir.path());
        let task = start_task(&state, &project_id, "Do work", "wo-1");

        let ctx = crate::state::context_for(&state, &task.stream_id).unwrap();
        std::fs::write(
            std::path::Path::new(&ctx.worktree_path).join("new_file.txt"),
            "line a\nline b\n",
        )
        .unwrap();

        let (_, resp) = compute_review(&state, &project_id, &task.task_id).unwrap();
        let f = resp
            .files
            .iter()
            .find(|f| f.path == "new_file.txt")
            .expect("untracked file must appear in review");
        assert_eq!(f.status, "added");
        assert_eq!(
            f.added, 2,
            "two new lines, no proposal event involved at all"
        );
        assert_eq!(f.removed, 0);
        assert_eq!(f.new_content, "line a\nline b\n");
    }

    #[test]
    fn review_refuses_honestly_for_a_chat_only_task_with_no_baseline() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let project_id = crate::catalog::create_project(&state, "Chat P", None)
            .unwrap()
            .0
            .project_id;
        let created = crate::tasks::task_create(
            &state,
            TaskCreateReq {
                parent_project_id: project_id.clone(),
                title: "Research".into(),
                work_order: "wo-r".into(),
                contract: Default::default(),
            },
        )
        .unwrap();
        crate::tasks::task_start(
            &state,
            TaskStartReq {
                project_id: project_id.clone(),
                task_id: created.task.task_id.clone(),
            },
        )
        .unwrap();

        let err = compute_review(&state, &project_id, &created.task.task_id).unwrap_err();
        assert!(err.contains("no materialized"), "got: {err}");
    }

    #[test]
    fn signoff_commits_exactly_the_reviewed_tree_and_records_the_hash() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let (project_id, _repo) = project_with_git_root(&state, dir.path());
        let task = start_task(&state, &project_id, "Do work", "wo-2");
        let ctx = crate::state::context_for(&state, &task.stream_id).unwrap();
        std::fs::write(
            std::path::Path::new(&ctx.worktree_path).join("added.txt"),
            "hello\n",
        )
        .unwrap();

        // Sign-off requires `awaiting_review` (or already `done`) — get there
        // the same way the driver would, without going through a real turn.
        crate::state::append(
            &state,
            &project_id,
            "test",
            crate::tasks::TASK_STATUS_FAMILY,
            serde_json::to_string(&crate::tasks::TaskStatusChanged {
                task_id: task.task_id.clone(),
                status: crate::tasks::TaskStatus::AwaitingReview,
                reason: None,
            })
            .unwrap(),
            vec![],
        )
        .unwrap();

        let resp = task_signoff(
            &state,
            TaskSignoffReq {
                project_id: project_id.clone(),
                task_id: task.task_id.clone(),
                confirm: true,
                snapshot: fresh_snapshot(&state, &project_id, &task.task_id),
            },
        )
        .unwrap();
        assert_eq!(resp.task.status, crate::tasks::TaskStatus::Done);

        let head = crate::git::run(
            std::path::Path::new(&ctx.worktree_path),
            &["rev-parse", "HEAD"],
        )
        .unwrap()
        .trim()
        .to_string();
        assert_eq!(
            head, resp.signoff_commit,
            "signed-off hash must be the tree's actual HEAD"
        );
        let status =
            crate::git::porcelain_status(std::path::Path::new(&ctx.worktree_path)).unwrap();
        assert!(
            status.is_empty(),
            "sign-off must commit everything reviewed, leaving a clean tree"
        );

        let (_, review) = compute_review(&state, &project_id, &task.task_id).unwrap();
        assert!(review.signed_off);
        assert_eq!(review.signoff_commit.as_deref(), Some(head.as_str()));
    }

    #[test]
    fn a_post_signoff_edit_flips_status_back_to_awaiting_review() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let (project_id, _repo) = project_with_git_root(&state, dir.path());
        let task = start_task(&state, &project_id, "Do work", "wo-3");

        crate::state::append(
            &state,
            &project_id,
            "test",
            crate::tasks::TASK_STATUS_FAMILY,
            serde_json::to_string(&crate::tasks::TaskStatusChanged {
                task_id: task.task_id.clone(),
                status: crate::tasks::TaskStatus::AwaitingReview,
                reason: None,
            })
            .unwrap(),
            vec![],
        )
        .unwrap();
        let resp = task_signoff(
            &state,
            TaskSignoffReq {
                project_id: project_id.clone(),
                task_id: task.task_id.clone(),
                confirm: true,
                snapshot: fresh_snapshot(&state, &project_id, &task.task_id),
            },
        )
        .unwrap();
        assert_eq!(resp.task.status, crate::tasks::TaskStatus::Done);

        let ctx = crate::state::context_for(&state, &task.stream_id).unwrap();
        std::fs::write(
            std::path::Path::new(&ctx.worktree_path).join("after.txt"),
            "surprise\n",
        )
        .unwrap();

        let (_, review) = compute_review(&state, &project_id, &task.task_id).unwrap();
        assert!(!review.signed_off, "a change after sign-off must un-pin it");

        let row = crate::tasks::find_task(&state, &project_id, &task.task_id).unwrap();
        assert_eq!(
            row.status,
            crate::tasks::TaskStatus::AwaitingReview,
            "GET /task/review must flip status back on drift"
        );
    }

    #[test]
    fn signoff_refuses_when_a_proposal_is_still_undecided() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let (project_id, _repo) = project_with_git_root(&state, dir.path());
        let task = start_task(&state, &project_id, "Do work", "wo-4");

        crate::state::append(
            &state,
            &task.stream_id,
            "agent:orchestrator",
            "agent.file.changed",
            serde_json::json!({
                "file_path": "f.txt",
                "status": "pending",
                "rationale": "test",
                "old_content": "a",
                "new_content": "b",
                "agent_id": "x",
                "model_id": "x",
                "turn_id": "t1",
                "call_id": "c1",
            })
            .to_string(),
            vec![],
        )
        .unwrap();
        crate::state::append(
            &state,
            &project_id,
            "test",
            crate::tasks::TASK_STATUS_FAMILY,
            serde_json::to_string(&crate::tasks::TaskStatusChanged {
                task_id: task.task_id.clone(),
                status: crate::tasks::TaskStatus::AwaitingReview,
                reason: None,
            })
            .unwrap(),
            vec![],
        )
        .unwrap();

        let err = task_signoff(
            &state,
            TaskSignoffReq {
                project_id: project_id.clone(),
                task_id: task.task_id.clone(),
                confirm: true,
                snapshot: fresh_snapshot(&state, &project_id, &task.task_id),
            },
        )
        .unwrap_err();
        assert!(err.contains("undecided"), "got: {err}");
    }

    /// The exact hole this pass closes: a write landing AFTER the human
    /// fetched Review but BEFORE sign-off must refuse — never be silently
    /// swept into the commit by `git add -A`.
    #[test]
    fn signoff_refuses_a_stale_snapshot_and_accepts_the_fresh_one() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let (project_id, _repo) = project_with_git_root(&state, dir.path());
        let task = start_task(&state, &project_id, "Do work", "wo-stale");
        let ctx = crate::state::context_for(&state, &task.stream_id).unwrap();
        let wt = std::path::Path::new(&ctx.worktree_path);
        std::fs::write(wt.join("reviewed.txt"), "the reviewed content\n").unwrap();
        crate::state::append(
            &state,
            &project_id,
            "test",
            crate::tasks::TASK_STATUS_FAMILY,
            serde_json::to_string(&crate::tasks::TaskStatusChanged {
                task_id: task.task_id.clone(),
                status: crate::tasks::TaskStatus::AwaitingReview,
                reason: None,
            })
            .unwrap(),
            vec![],
        )
        .unwrap();

        // Human reviews this state…
        let reviewed = fresh_snapshot(&state, &project_id, &task.task_id);
        // …then something else writes into the worktree.
        std::fs::write(wt.join("sneaky.txt"), "landed after review\n").unwrap();

        let err = task_signoff(
            &state,
            TaskSignoffReq {
                project_id: project_id.clone(),
                task_id: task.task_id.clone(),
                confirm: true,
                snapshot: reviewed,
            },
        )
        .unwrap_err();
        assert!(err.contains("changed since the review"), "got: {err}");
        // Nothing was committed — the sneaky file is still uncommitted.
        assert!(
            !crate::git::porcelain_status(wt).unwrap().is_empty(),
            "refusal must not commit anything"
        );

        // Approving the CURRENT state (sneaky file included) works.
        let resp = task_signoff(
            &state,
            TaskSignoffReq {
                project_id: project_id.clone(),
                task_id: task.task_id.clone(),
                confirm: true,
                snapshot: fresh_snapshot(&state, &project_id, &task.task_id),
            },
        )
        .unwrap();
        assert_eq!(resp.task.status, crate::tasks::TaskStatus::Done);
    }

    /// An empty/missing snapshot is not "approve whatever is there" — it is
    /// a refusal with instructions.
    #[test]
    fn signoff_refuses_without_a_snapshot_id() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let (project_id, _repo) = project_with_git_root(&state, dir.path());
        let task = start_task(&state, &project_id, "Do work", "wo-nosnap");
        let err = task_signoff(
            &state,
            TaskSignoffReq {
                project_id: project_id.clone(),
                task_id: task.task_id.clone(),
                confirm: true,
                snapshot: String::new(),
            },
        )
        .unwrap_err();
        assert!(err.contains("snapshot"), "got: {err}");
    }

    /// Binary content is explicit, pinned, and signable: the row says
    /// "binary" instead of rendering an empty diff, the snapshot moves when
    /// the binary's bytes move, and sign-off commits it.
    #[test]
    fn a_binary_file_is_explicitly_marked_pinned_by_hash_and_signable() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let (project_id, _repo) = project_with_git_root(&state, dir.path());
        let task = start_task(&state, &project_id, "Do work", "wo-bin");
        let ctx = crate::state::context_for(&state, &task.stream_id).unwrap();
        let wt = std::path::Path::new(&ctx.worktree_path);
        std::fs::write(wt.join("blob.bin"), [0xff, 0xfe, 0x00, 0x01, 0x02]).unwrap();

        let (_, review) = compute_review(&state, &project_id, &task.task_id).unwrap();
        let row = review
            .files
            .iter()
            .find(|f| f.path == "blob.bin")
            .expect("binary row must be visible");
        assert_eq!(row.note.as_deref(), Some("binary"));
        assert!(row.reviewable, "binary is reviewable-by-hash, not blocked");
        assert!(
            row.new_content.contains("binary file"),
            "display must SAY binary: {}",
            row.new_content
        );
        let snap_before = review.snapshot.clone();

        // Different bytes → different snapshot (the hash really pins bytes).
        std::fs::write(wt.join("blob.bin"), [0xff, 0xfe, 0x00, 0x01, 0x03]).unwrap();
        let snap_after = fresh_snapshot(&state, &project_id, &task.task_id);
        assert_ne!(
            snap_before, snap_after,
            "changing binary bytes must change the snapshot"
        );

        crate::state::append(
            &state,
            &project_id,
            "test",
            crate::tasks::TASK_STATUS_FAMILY,
            serde_json::to_string(&crate::tasks::TaskStatusChanged {
                task_id: task.task_id.clone(),
                status: crate::tasks::TaskStatus::AwaitingReview,
                reason: None,
            })
            .unwrap(),
            vec![],
        )
        .unwrap();
        let resp = task_signoff(
            &state,
            TaskSignoffReq {
                project_id: project_id.clone(),
                task_id: task.task_id.clone(),
                confirm: true,
                snapshot: snap_after,
            },
        )
        .unwrap();
        assert_eq!(resp.task.status, crate::tasks::TaskStatus::Done);
    }

    /// A change set containing a file whose bytes could not even be hashed
    /// (here: a credential-shaped filename the reader refuses) blocks
    /// sign-off entirely — `git add -A` must never commit content nobody
    /// reviewed or pinned.
    #[test]
    fn signoff_refuses_while_an_unreviewable_file_is_in_the_change_set() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let (project_id, _repo) = project_with_git_root(&state, dir.path());
        let task = start_task(&state, &project_id, "Do work", "wo-cred");
        let ctx = crate::state::context_for(&state, &task.stream_id).unwrap();
        let wt = std::path::Path::new(&ctx.worktree_path);
        std::fs::write(wt.join("auth.json"), "{\"secret\":\"nope\"}").unwrap();
        crate::state::append(
            &state,
            &project_id,
            "test",
            crate::tasks::TASK_STATUS_FAMILY,
            serde_json::to_string(&crate::tasks::TaskStatusChanged {
                task_id: task.task_id.clone(),
                status: crate::tasks::TaskStatus::AwaitingReview,
                reason: None,
            })
            .unwrap(),
            vec![],
        )
        .unwrap();
        let err = task_signoff(
            &state,
            TaskSignoffReq {
                project_id: project_id.clone(),
                task_id: task.task_id.clone(),
                confirm: true,
                snapshot: fresh_snapshot(&state, &project_id, &task.task_id),
            },
        )
        .unwrap_err();
        assert!(
            err.contains("auth.json"),
            "the refusal must name the file: {err}"
        );
        assert!(
            !crate::git::porcelain_status(wt).unwrap().is_empty(),
            "nothing may be committed"
        );
    }

    /// Sign-off excludes the task's own machinery, not just compares: while
    /// the agent-turn lease is held (a turn mid-flight), sign-off refuses
    /// instead of racing the write that turn is about to make.
    #[test]
    fn signoff_refuses_while_the_tasks_agent_turn_lease_is_held() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let (project_id, _repo) = project_with_git_root(&state, dir.path());
        let task = start_task(&state, &project_id, "Do work", "wo-lease");
        crate::state::append(
            &state,
            &project_id,
            "test",
            crate::tasks::TASK_STATUS_FAMILY,
            serde_json::to_string(&crate::tasks::TaskStatusChanged {
                task_id: task.task_id.clone(),
                status: crate::tasks::TaskStatus::AwaitingReview,
                reason: None,
            })
            .unwrap(),
            vec![],
        )
        .unwrap();
        let snapshot = fresh_snapshot(&state, &project_id, &task.task_id);
        let _lease =
            crate::state::admit_turn(&state, &task.stream_id, crate::node::Seat::Orchestrator)
                .unwrap();
        let err = task_signoff(
            &state,
            TaskSignoffReq {
                project_id: project_id.clone(),
                task_id: task.task_id.clone(),
                confirm: true,
                snapshot,
            },
        )
        .unwrap_err();
        assert!(err.contains("mid-turn"), "got: {err}");
    }

    /// Confirmed gap 2, unit half: the verification basis is the COMMIT's
    /// immutable tree. A commit whose blobs match the reviewed disk bytes
    /// reproduces the approved snapshot; amending the commit to different
    /// content while RESTORING the working file (the exact worktree-check
    /// defeat) must NOT reproduce it.
    #[test]
    fn snapshot_at_commit_matches_the_review_and_catches_a_worktree_restored_tamper() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let (project_id, _repo) = project_with_git_root(&state, dir.path());
        let task = start_task(&state, &project_id, "Do work", "wo-tree");
        let ctx = crate::state::context_for(&state, &task.stream_id).unwrap();
        let wt = std::path::Path::new(&ctx.worktree_path);
        std::fs::write(wt.join("work.txt"), "the reviewed bytes\n").unwrap();

        let (_, review) = compute_review(&state, &project_id, &task.task_id).unwrap();
        let base = review.base_commit.clone().unwrap();

        // An honest commit of exactly the reviewed state reproduces the id.
        crate::git::run(wt, &["add", "-A"]).unwrap();
        crate::git::run(wt, &["commit", "-m", "honest"]).unwrap();
        let honest = crate::git::run(wt, &["rev-parse", "HEAD"])
            .unwrap()
            .trim()
            .to_string();
        assert_eq!(
            snapshot_at_commit(wt, &base, &honest).unwrap(),
            review.snapshot
        );

        // Tamper: amend different bytes into the commit, then RESTORE the
        // working file — a worktree recompute would read the reviewed bytes
        // and pass; the commit-tree recompute cannot be fooled.
        std::fs::write(wt.join("work.txt"), "tampered bytes\n").unwrap();
        crate::git::run(wt, &["add", "-A"]).unwrap();
        crate::git::run(wt, &["commit", "--amend", "-m", "tampered"]).unwrap();
        std::fs::write(wt.join("work.txt"), "the reviewed bytes\n").unwrap();
        let tampered = crate::git::run(wt, &["rev-parse", "HEAD"])
            .unwrap()
            .trim()
            .to_string();
        assert_ne!(
            snapshot_at_commit(wt, &base, &tampered).unwrap(),
            review.snapshot
        );
    }

    /// Confirmed gap 2, route half: when `git add` rewrites content (a
    /// gitattributes clean filter — the deterministic stand-in for any
    /// staged-content divergence), sign-off refuses, records nothing, and
    /// ROLLS the just-created commit back — no unapproved commit is left
    /// behind and no bytes are lost.
    #[test]
    fn signoff_rolls_back_a_commit_whose_tree_diverged_from_the_review() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let (project_id, _repo) = project_with_git_root(&state, dir.path());
        let task = start_task(&state, &project_id, "Do work", "wo-tamper");
        let ctx = crate::state::context_for(&state, &task.stream_id).unwrap();
        let wt = std::path::Path::new(&ctx.worktree_path);
        // A clean filter that rewrites on add: the committed blob will
        // differ from the reviewed disk bytes.
        crate::git::run(
            wt,
            &["config", "filter.tamper.clean", "sed s/reviewed/rewritten/"],
        )
        .unwrap();
        std::fs::write(wt.join(".gitattributes"), "*.txt filter=tamper\n").unwrap();
        std::fs::write(wt.join("work.txt"), "the reviewed bytes\n").unwrap();
        crate::state::append(
            &state,
            &project_id,
            "test",
            crate::tasks::TASK_STATUS_FAMILY,
            serde_json::to_string(&crate::tasks::TaskStatusChanged {
                task_id: task.task_id.clone(),
                status: crate::tasks::TaskStatus::AwaitingReview,
                reason: None,
            })
            .unwrap(),
            vec![],
        )
        .unwrap();
        let pre_head = crate::git::run(wt, &["rev-parse", "HEAD"])
            .unwrap()
            .trim()
            .to_string();

        let err = task_signoff(
            &state,
            TaskSignoffReq {
                project_id: project_id.clone(),
                task_id: task.task_id.clone(),
                confirm: true,
                snapshot: fresh_snapshot(&state, &project_id, &task.task_id),
            },
        )
        .unwrap_err();
        assert!(err.contains("rolled back"), "{err}");
        // No unapproved commit left behind; bytes intact; nothing recorded.
        let head = crate::git::run(wt, &["rev-parse", "HEAD"])
            .unwrap()
            .trim()
            .to_string();
        assert_eq!(
            head, pre_head,
            "the tampered commit must not survive a refused sign-off"
        );
        assert_eq!(
            std::fs::read_to_string(wt.join("work.txt")).unwrap(),
            "the reviewed bytes\n"
        );
        assert!(latest_signoff_commit(&state, &project_id, &task.task_id).is_none());
    }

    /// The empirically-verified escape this review surface must refuse:
    /// `ln -s` inside the worktree, then `git ls-files --others` lists the
    /// link, and a raw `read_to_string(dir.join(path))` (the pre-fix code)
    /// follows it straight out of the jail — in the UNSANDBOXED node
    /// process, unlike a task worker's own sandboxed reads.
    #[cfg(unix)]
    #[test]
    fn a_symlink_pointing_outside_the_worktree_is_refused_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let (project_id, _repo) = project_with_git_root(&state, dir.path());
        let task = start_task(&state, &project_id, "Do work", "wo-sym1");
        let ctx = crate::state::context_for(&state, &task.stream_id).unwrap();
        let wt = std::path::Path::new(&ctx.worktree_path);

        let outside = dir.path().join("outside_secret.txt");
        std::fs::write(&outside, "TOP SECRET\n").unwrap();
        std::os::unix::fs::symlink(&outside, wt.join("leak.txt")).unwrap();

        let (_, resp) = compute_review(&state, &project_id, &task.task_id).unwrap();
        let f = resp
            .files
            .iter()
            .find(|f| f.path == "leak.txt")
            .expect("the symlink path itself must still be listed — git does list it");
        assert!(
            !f.new_content.contains("TOP SECRET"),
            "must never follow a symlink out of the worktree, got: {}",
            f.new_content
        );
    }

    /// `tools::Jail::read_file`'s credential fence (`bakery_exec::
    /// is_credential_filename`) must also cover this surface — a file
    /// entirely inside the worktree is still refused by name, not read.
    #[test]
    fn a_credential_shaped_filename_is_refused_even_from_inside_the_worktree() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let (project_id, _repo) = project_with_git_root(&state, dir.path());
        let task = start_task(&state, &project_id, "Do work", "wo-cred1");
        let ctx = crate::state::context_for(&state, &task.stream_id).unwrap();
        let wt = std::path::Path::new(&ctx.worktree_path);
        std::fs::write(wt.join("secrets.json"), "{\"key\":\"sk-should-not-leak\"}").unwrap();

        let (_, resp) = compute_review(&state, &project_id, &task.task_id).unwrap();
        let f = resp
            .files
            .iter()
            .find(|f| f.path == "secrets.json")
            .expect("the row itself must still be visible");
        assert!(
            !f.new_content.contains("sk-should-not-leak"),
            "credential-shaped filenames must never have their contents rendered: {}",
            f.new_content
        );
    }

    /// `diff_counts` (the Floor-row counters' own cheaper diff — finding:
    /// "reads it a second time for the Floor counters") must apply the same
    /// jail to its untracked-file reads as `compute_files` does — an
    /// escaping symlink must contribute nothing to the count, not the line
    /// count of whatever it points at outside the worktree.
    #[cfg(unix)]
    #[test]
    fn diff_counts_does_not_follow_a_symlink_out_of_the_worktree() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let (project_id, _repo) = project_with_git_root(&state, dir.path());
        let task = start_task(&state, &project_id, "Do work", "wo-sym2");
        let ctx = crate::state::context_for(&state, &task.stream_id).unwrap();
        let wt = std::path::Path::new(&ctx.worktree_path);

        let outside = dir.path().join("outside_big.txt");
        std::fs::write(&outside, "a\nb\nc\nd\ne\n").unwrap();
        std::os::unix::fs::symlink(&outside, wt.join("leak.txt")).unwrap();
        let base_commit = materialized_base_commit(&state, &project_id, &task.task_id).unwrap();

        let (added, _removed) = diff_counts(&ctx.worktree_path, &base_commit).unwrap();
        assert_eq!(added, 0, "an escaping symlink must contribute nothing to the Floor counters, got {added} added lines");
    }

    /// Empirically verified against a real repo (this review's own finding):
    /// replacing a tracked regular file with a symlink makes `git diff
    /// --name-status <base>` emit `T`, which used to fall into a silent
    /// `_ => {}` and vanish from `TaskReviewResp.files` while `diff_counts`
    /// (numstat-based) counted it anyway — the exact split this test guards
    /// against.
    #[cfg(unix)]
    #[test]
    fn a_typechange_from_tracked_file_to_symlink_appears_in_review_as_modified() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let (project_id, _repo) = project_with_git_root(&state, dir.path());
        // `make_repo` commits a tracked `f.txt` before the task ever starts
        // — its base_commit already has it as a regular file.
        let task = start_task(&state, &project_id, "Do work", "wo-t1");
        let ctx = crate::state::context_for(&state, &task.stream_id).unwrap();
        let wt = std::path::Path::new(&ctx.worktree_path);

        std::fs::remove_file(wt.join("f.txt")).unwrap();
        std::os::unix::fs::symlink("/etc/hosts", wt.join("f.txt")).unwrap();

        let (_, resp) = compute_review(&state, &project_id, &task.task_id).unwrap();
        let f = resp
            .files
            .iter()
            .find(|f| f.path == "f.txt")
            .expect("a typechange must appear in review, not be silently dropped");
        assert_eq!(f.status, "modified", "a typechange (tracked file -> symlink) must render as modified — sign-off's `git add -A` will commit it either way");
    }
}
