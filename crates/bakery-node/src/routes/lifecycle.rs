//! Stream lifecycle: living / slice / burn, and discard.
//!
//! Heart is append-only and stays that way here — `04-LIFECYCLE.md`: "do not
//! delete heart.db rows." A lifetime is just the latest `stream.lifetime.set`
//! event (default `Slice` when none exists); discard is a filesystem cleanup
//! (worktree + room runtime) plus a `stream.discarded` event recording what
//! actually happened. Lineage — every event ever appended — is untouched.

use crate::node::{DiscardRecord, Lifetime, DISCARD_FAMILY};
use crate::routes::project::remove_worktree;
use crate::state::{
    append, begin_burn_transition, begin_discard, context_for, set_non_burn_lifetime,
    wait_for_one_shot_clear, ActiveContext, AppState,
};
use crate::terminals::{fence_active_ptys, fence_active_ptys_with_deadline};
use axum::{extract::State, http::StatusCode, Json};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct LifetimeReq {
    pub(crate) stream_id: String,
    pub(crate) lifetime: Lifetime,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct LifetimeResp {
    pub(crate) stream_id: String,
    pub(crate) lifetime: Lifetime,
}

/// Sets a stream's declared lifetime. Never automatic — the operator (or Baker
/// asking on his behalf) is the only caller; nothing in the tool loop or
/// project-open path invokes this on its own (STEERING: living/burn are
/// both opt-in, never a silent default).
///
/// The append itself goes through `set_non_burn_lifetime`, which holds the
/// same lifecycle lock `discard_stream`'s `begin_discard` reads the lifetime
/// under — so this can never land *between* "discard read the old lifetime"
/// and "discard committed to retiring" (04b-FENCE §2: a stream that is
/// `Living` at discard's decision instant still needs `confirm:true`, no
/// matter how a concurrent `set_lifetime` call is timed).
///
/// A transition *to* `Burn` also kills any host-mode pty session already
/// open on this stream, *and* any host attach that was merely admitted and
/// hasn't spawned yet (`admit_pty` never lets a new one start once this
/// commits, but one already in flight is still owed a kill —
/// `fence_active_ptys` now waits on pending host too, not just registered
/// pids: 04c-BURN §1). "Hostile/untrusted, no host terminal" has to hold
/// the instant the flag is set, not just for sessions opened afterward
/// (04-LIFECYCLE audit). Room sessions are left alone.
///
/// For any lifetime other than `Burn`, the commit is the whole job:
/// `set_non_burn_lifetime` is a single atomic check-and-write — and refuses
/// `Burn` outright if ever called with it (only `begin_burn_transition` may
/// set that one, per the drive-by fix in 06-SEATS.md).
///
/// `Burn` is different because it also has to fence — kill whatever's
/// running, which takes real, unbounded (if bounded) time — and a naive
/// "commit, then await the fence" sequence leaves the lifecycle lock
/// released for that whole await, during which a second `set_lifetime` call
/// could land and change the stream's declared lifetime out from under the
/// fence. An earlier version tried to let that happen and have the fence
/// *detect* it via a generation counter checked once per poll iteration —
/// that is not actually safe, because the fence still reads state under the
/// lock, releases it, and only then calls `kill_pty_session` (a real
/// subprocess spawn): a write can land in exactly that gap, after the
/// check already passed. So instead: `begin_burn_transition` commits Burn
/// *and* marks the stream's fence active under one lock acquisition, and
/// the returned guard is held for the fence's entire duration — during
/// which `set_non_burn_lifetime` refuses every other lifetime write, Burn
/// included. That makes "something else changes the lifetime mid-fence"
/// structurally impossible rather than something to notice after the fact
/// (04d-GHOSTTY-LEASE §2 audit).
///
/// If the fence still times out with a host shell alive, this returns 409,
/// not 200 — an API that reports Burn "succeeded" while a live,
/// unsandboxed host shell is still running would be a lie a caller could
/// reasonably act on (04c-BURN §2). The lifetime itself is not rolled back
/// — Burn is set either way — so a retry only needs to re-run the fence.
pub(crate) async fn set_lifetime(
    State(st): State<AppState>,
    Json(req): Json<LifetimeReq>,
) -> Result<Json<LifetimeResp>, (StatusCode, String)> {
    // 5s: the same bound the fence always uses. Split out as
    // `set_lifetime_with_burn_fence_timeout` so a test proving the abort
    // path doesn't have to wait out the real deadline to do it.
    set_lifetime_with_burn_fence_timeout(st, req, Duration::from_secs(5)).await
}

async fn set_lifetime_with_burn_fence_timeout(
    st: AppState,
    req: LifetimeReq,
    burn_fence_timeout: Duration,
) -> Result<Json<LifetimeResp>, (StatusCode, String)> {
    context_for(&st, &req.stream_id).map_err(|e| (StatusCode::NOT_FOUND, e))?;

    if req.lifetime == Lifetime::Burn {
        let guard = begin_burn_transition(&st, &req.stream_id)?;
        let cleared =
            fence_active_ptys_with_deadline(&st, &req.stream_id, Some("host"), burn_fence_timeout)
                .await;
        if !cleared {
            return Err((
                StatusCode::CONFLICT,
                "burn is set, but a host session is still shutting down — retry".to_string(),
            ));
        }
        let acp_cleared =
            crate::state::fence_acp_with_deadline(&st, &req.stream_id, burn_fence_timeout).await;
        if !acp_cleared {
            return Err((
                StatusCode::CONFLICT,
                "burn is set, but an ACP session is still shutting down — retry".to_string(),
            ));
        }
        drop(guard);
    } else {
        set_non_burn_lifetime(&st, &req.stream_id, req.lifetime)?;
    }

    Ok(Json(LifetimeResp {
        stream_id: req.stream_id,
        lifetime: req.lifetime,
    }))
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct DiscardReq {
    pub(crate) stream_id: String,
    /// Required (and checked) only when the stream's current lifetime is
    /// `Living` — a plain `Slice`/`Burn` discard needs no confirmation.
    #[serde(default)]
    pub(crate) confirm: bool,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct DiscardResp {
    pub(crate) stream_id: String,
    pub(crate) lifetime: Lifetime,
    pub(crate) worktree_removed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) worktree_error: Option<String>,
    pub(crate) room_destroyed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) room_error: Option<String>,
    /// A folder left in place, and why: it is, or holds, a project's root.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) kept: Option<String>,
    /// The chat's own folder went to the Trash.
    pub(crate) chat_trashed: bool,
}

/// Why `folder` stays, if it must: it is, or holds, a live project's root.
/// Discard never removes such a folder, whichever conversation it began in:
/// a chat's folder attached to a project is that project's root now. Fails
/// closed: a project list that can't be read, or a folder or root that can't
/// be checked for any reason but being gone, keeps the folder too.
pub(crate) fn kept_for_project(st: &AppState, folder: &Path) -> Option<String> {
    let gone = |e: &std::io::Error| e.kind() == std::io::ErrorKind::NotFound;
    let folder = match folder.canonicalize() {
        Ok(folder) => folder,
        Err(e) if gone(&e) => return None,
        Err(e) => return Some(format!("its folder stays: it couldn't be checked ({e})")),
    };
    let Ok(projects) = crate::catalog::known_projects(st) else {
        return Some("its folder stays: the project list couldn't be read".to_string());
    };
    for project in projects {
        for root in &project.roots {
            match Path::new(&root.path).canonicalize() {
                Ok(root) if root.starts_with(&folder) => {
                    return Some(format!(
                        "its folder stays: it is the root of the project {}",
                        project.name
                    ))
                }
                Ok(_) => {}
                Err(e) if gone(&e) => {}
                Err(e) => {
                    return Some(format!(
                        "its folder stays: a root of the project {} couldn't be checked ({e})",
                        project.name
                    ))
                }
            }
        }
    }
    None
}

/// Bakery only removes a worktree it can *prove* it created: `created_by_bakery`
/// is set once, on the exact `project.onboarded` write that ran
/// `git worktree add` (`routes::project::open_stream`) — never inferred from
/// where the path happens to sit. Containment under `worktree_base` is
/// checked on *canonicalized* paths (resolving symlinks), not a lexical
/// `starts_with`, so a manually attached tree that merely lives under the
/// same parent directory (or a symlink alias of it) can't be force-removed
/// by coincidence of location (04-LIFECYCLE audit).
pub(crate) fn is_bakery_owned_worktree(ctx: &ActiveContext, worktree_base: &Path) -> bool {
    if ctx.in_place || !ctx.created_by_bakery {
        return false;
    }
    match (
        Path::new(&ctx.worktree_path).canonicalize(),
        worktree_base.canonicalize(),
    ) {
        (Ok(wt), Ok(base)) => wt.starts_with(&base),
        _ => false,
    }
}

/// Throws away a slice's worktree and room runtime. Heart lineage — every
/// event this stream ever appended — is never touched; discard is a
/// filesystem action recorded as one more event, not an erasure.
///
/// Order matters: `begin_discard` atomically decides to proceed and marks
/// retiring (blocks every new attach immediately — 04b-FENCE §1), then this
/// waits for anything already mid-attach to finish, kills and waits for
/// every live interactive pty session, and only *then* touches the
/// worktree/room — otherwise a running shell or agent command can survive
/// the room being pulled out from under it, or recreate state in a
/// "removed" worktree (04-LIFECYCLE audit: "discard does not fence active
/// sessions"). If either fence times out, the discard is aborted (nothing
/// removed) rather than proceeding anyway (04b-FENCE §3).
pub(crate) async fn discard_stream(
    State(st): State<AppState>,
    Json(req): Json<DiscardReq>,
) -> Result<Json<DiscardResp>, (StatusCode, String)> {
    // 35s: bounded by `bakery_exec::attach`'s own timeout for the one-shot
    // path this waits on. Split out as `discard_with_one_shot_timeout` so
    // tests can shrink it — a genuinely stuck one-shot attach is exactly the
    // 04b-FENCE §3 scenario ("fence timeout = abort cleanup, not `rm`
    // anyway"), and a unit test can't wait 35 real seconds to prove it.
    discard_with_one_shot_timeout(st, req, Duration::from_secs(35)).await
}

async fn discard_with_one_shot_timeout(
    st: AppState,
    req: DiscardReq,
    one_shot_timeout: Duration,
) -> Result<Json<DiscardResp>, (StatusCode, String)> {
    let (lifetime, record) =
        discard_as(&st, &req.stream_id, req.confirm, None, one_shot_timeout).await?;
    if let Some(e) = record.chat_error.or(record.worktree_error).or(record.room_error) {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("discard partially failed (recorded): {e}"),
        ));
    }
    Ok(Json(DiscardResp {
        stream_id: req.stream_id,
        lifetime,
        worktree_removed: record.worktree_removed,
        worktree_error: None,
        room_destroyed: record.room_destroyed,
        room_error: None,
        kept: record.kept,
        chat_trashed: record.chat_trashed,
    }))
}

/// What a discard does with a conversation's folders, decided before any is
/// touched: by the discard's own rules ([`fate_of`]), or by a delete's plan
/// (`crate::deletes`), which the operator saw first.
#[derive(Debug, Clone, Default)]
pub(crate) struct Fate {
    /// Its Bakery-made worktree, removed with `git worktree remove --force`
    /// once its unsaved work is kept in git (`crate::git_trash`); one whose
    /// work can't be kept stays.
    pub(crate) worktree: Option<PathBuf>,
    /// When that work is kept, as a delete names it for all it keeps; now
    /// when none.
    pub(crate) stamp: Option<String>,
    /// Its chat's own folder, to the Trash.
    pub(crate) chat: Option<PathBuf>,
    /// Its room, removed while no chat folder is still inside it.
    pub(crate) room: Option<PathBuf>,
    /// A folder left in place, and why.
    pub(crate) kept: Option<String>,
}

/// Fails closed like the project check: only a folder that is gone is
/// nothing to move.
fn gone(folder: &Path) -> bool {
    matches!(folder.try_exists(), Ok(false))
}

/// The discard's own rules: a worktree only when the Bakery made it, a
/// chat's own folder to the Trash, the room; never a folder that is, or
/// holds, a live project's root.
fn fate_of(st: &AppState, ctx: &ActiveContext) -> Fate {
    let worktree_path = Path::new(&ctx.worktree_path);
    let bakery_owned = is_bakery_owned_worktree(ctx, st.worktree_base.as_path());
    let worktree_kept = bakery_owned
        .then(|| kept_for_project(st, worktree_path))
        .flatten();
    let worktree = (bakery_owned && worktree_path.is_dir() && worktree_kept.is_none())
        .then(|| worktree_path.to_path_buf());
    // A chat's own folder goes to the Trash, never deleted outright; one a
    // project uses stays.
    let chat_folder = crate::catalog::is_chat_folder(&st.base, &ctx.worktree_path)
        .then(|| PathBuf::from(&ctx.worktree_path));
    let chat_kept = chat_folder
        .as_deref()
        .and_then(|folder| kept_for_project(st, folder));
    let chat = chat_folder.filter(|folder| chat_kept.is_none() && !gone(folder));
    let room_dir = bakery_exec::socket_runtime_dir(&st.base.join("cache"), &ctx.stream_id);
    let room_kept = kept_for_project(st, &room_dir);
    let room = (room_dir.is_dir() && room_kept.is_none()).then_some(room_dir);
    Fate {
        worktree,
        chat,
        room,
        kept: chat_kept.or(worktree_kept).or(room_kept),
        stamp: None,
    }
}

/// The discard itself, for the route and for a delete: `fate`, a delete's
/// plan for the conversation's folders, or the discard's own rules when
/// none. Answers what it did, as recorded, a step's failure included.
pub(crate) async fn discard_as(
    st: &AppState,
    stream_id: &str,
    confirm: bool,
    fate: Option<Fate>,
    one_shot_timeout: Duration,
) -> Result<(Lifetime, DiscardRecord), (StatusCode, String)> {
    let ctx = context_for(st, stream_id).map_err(|e| (StatusCode::NOT_FOUND, e))?;
    // The permanent Baker is the identity every Baker thread hangs off:
    // discarding it would orphan all of them. Threads are deletable (this
    // same route); the main conversation is not.
    if crate::baker_ops::is_permanent_baker_stream(st, stream_id) {
        return Err((
            StatusCode::CONFLICT,
            "the main Baker conversation cannot be deleted — delete or archive its threads instead"
                .to_string(),
        ));
    }
    let (_retiring_guard, lifetime) = begin_discard(st, stream_id, confirm)?;

    // A one-shot exec/pty that started just before `begin_discard` closed
    // the door won't be refused by it — wait for it to finish rather than
    // race the worktree/room out from under it.
    if !wait_for_one_shot_clear(st, stream_id, one_shot_timeout).await {
        return Err((
            StatusCode::CONFLICT,
            "timed out waiting for an in-flight attach to finish — discard aborted, nothing was removed".to_string(),
        ));
    }

    // Every interactive terminal on this stream, host or room, dies before
    // the filesystem changes underneath it.
    if !fence_active_ptys(st, stream_id, None).await {
        return Err((
            StatusCode::CONFLICT,
            "timed out waiting for an interactive session to end — discard aborted, nothing was removed".to_string(),
        ));
    }

    // Kill every boxed ACP child for this stream and wait (bounded) for
    // pending handshakes to resolve — same contract as the PTY fence.
    if !crate::state::fence_acp_with_deadline(st, stream_id, Duration::from_secs(5)).await {
        return Err((
            StatusCode::CONFLICT,
            "timed out waiting for ACP child to exit — discard aborted, nothing was removed"
                .to_string(),
        ));
    }

    // Its previews, editor and service copies end with it.
    crate::views::stop_all(stream_id);
    crate::services::stop_copies(stream_id);

    let fate = fate.unwrap_or_else(|| fate_of(st, &ctx));
    let kept = fate.kept;
    let (worktree_removed, worktree_error) = match &fate.worktree {
        Some(path) if !path.is_dir() => (false, None),
        Some(path) => {
            let stamp = fate.stamp.clone().unwrap_or_else(crate::git_trash::stamp_now);
            let name = crate::git_trash::name_of(&ctx.branch, path);
            let repo = Path::new(&ctx.repo_root);
            match crate::git_trash::keep(repo, &name, &ctx.branch, Some(path), &stamp)
                .and_then(|_| remove_worktree(st, &ctx.repo_root, path))
            {
                Ok(()) => (true, None),
                Err(e) => (false, Some(e)),
            }
        }
        None => (false, None),
    };

    // An older chat's folder sits inside its room, so it leaves before the
    // room goes, and the room stays while it can't.
    let (chat_trashed, chat_error) = match &fate.chat {
        Some(folder) if !gone(folder) => match trash(folder, &st.base) {
            Ok(()) => (true, None),
            Err(e) => (false, Some(e)),
        },
        _ => (false, None),
    };

    // Attempted independently of the worktree step above: a failure removing
    // the worktree must never skip room cleanup, and vice versa — both are
    // Bakery's own responsibility regardless of how the other went.
    let chat_folder = Path::new(&ctx.worktree_path);
    let holds_chat = |room: &Path| {
        crate::catalog::is_chat_folder(&st.base, &ctx.worktree_path)
            && chat_folder.starts_with(room)
            && !gone(chat_folder)
    };
    let (room_destroyed, room_error) = match &fate.room {
        Some(room) if room.is_dir() && !holds_chat(room) => match std::fs::remove_dir_all(room) {
            Ok(()) => (true, None),
            Err(e) => (false, Some(e.to_string())),
        },
        _ => (false, None),
    };

    // Recorded unconditionally — even a fully-failed discard leaves a
    // lineage entry saying so, rather than vanishing without a trace because
    // an early `?` gave up before either step was attempted.
    let record = DiscardRecord {
        worktree_removed,
        worktree_error,
        room_destroyed,
        room_error,
        kept,
        chat_trashed,
        chat_error,
    };
    let payload = serde_json::to_string(&record)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    append(st, stream_id, "human", DISCARD_FAMILY, payload, vec![])
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    crate::grants::end_all(st, stream_id, "the conversation was discarded");
    Ok((lifetime, record))
}

/// Moves `path` to the macOS Trash (`/usr/bin/trash`, macOS 15 and later),
/// so a deleted chat's files can still be put back. Never deletes: when the
/// Trash can't take it, the error says so and the files stay.
#[cfg(not(test))]
pub(crate) fn trash(path: &Path, _base: &Path) -> Result<(), String> {
    let out = std::process::Command::new("/usr/bin/trash")
        .arg("-s")
        .arg(path)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("the Trash couldn't take {}: {e}", path.display()))?;
    if out.status.success() && !path.exists() {
        Ok(())
    } else {
        Err(format!(
            "the Trash couldn't take {}: {}",
            path.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// Tests never touch the operator's Trash: a folder beside the node's own.
#[cfg(test)]
pub(crate) fn trash(path: &Path, base: &Path) -> Result<(), String> {
    let bin = base.join("test-trash");
    std::fs::create_dir_all(&bin).map_err(|e| e.to_string())?;
    let name = path.file_name().unwrap_or_default();
    std::fs::rename(path, bin.join(name)).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::LIFETIME_FAMILY;
    use crate::routes::project::open_stream;
    use crate::state::{stream_discarded, stream_lifetime};
    use crate::testutil::*;
    use bakery_agent::StubModel;
    use std::sync::Arc;

    fn create_room_runtime(state: &AppState, stream_id: &str) -> std::path::PathBuf {
        let room = bakery_exec::socket_runtime_dir(&state.base.join("cache"), stream_id);
        std::fs::create_dir_all(room.join("home")).unwrap();
        std::fs::create_dir_all(room.join("tmp")).unwrap();
        room
    }

    /// Baker threads delete through the ordinary discard — their in-place
    /// factory root is never removed, and a deleted thread loses membership
    /// (and with it every Baker op). The main Baker conversation, which every
    /// thread hangs off, refuses.
    /// A loose chat is a socket in no Project with a folder of its own in
    /// `chats/`, named for its title: listed as a chat, ready for native
    /// runtimes (its folder exists), and on delete the folder goes to the
    /// Trash, whole, never deleted outright.
    #[tokio::test]
    async fn a_loose_chat_has_its_own_workspace_and_leaves_nothing_when_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let chat = crate::catalog::create_chat(&state, "  What is in my repo?  ").unwrap();
        assert_eq!(chat.name, "What is in my repo?");
        let workspace = std::path::PathBuf::from(&chat.worktree_path);
        assert!(workspace.is_dir());
        assert_eq!(workspace.parent(), Some(state.base.join("chats").as_path()));
        let folder_name = workspace
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(
            folder_name.starts_with("What-is-in-my-repo-"),
            "{folder_name}"
        );

        let axum::Json(view) = crate::routes::studio::studio_state(State(state.clone()))
            .await
            .unwrap();
        let listed = view
            .ungrouped_sockets
            .iter()
            .find(|s| s.stream_id == chat.stream_id)
            .expect("a loose chat is ungrouped");
        assert!(listed.exists_on_disk && listed.baker_thread.is_none() && listed.chat);
        assert!(view
            .factory_sockets
            .iter()
            .all(|s| s.stream_id != chat.stream_id));

        std::fs::write(workspace.join("notes.md"), "scratch").unwrap();
        let gone = discard_stream(
            State(state.clone()),
            Json(DiscardReq {
                stream_id: chat.stream_id.clone(),
                confirm: false,
            }),
        )
        .await
        .unwrap()
        .0;
        assert!(stream_discarded(&state, &chat.stream_id));
        assert!(gone.chat_trashed && !workspace.exists());
        let trashed = state
            .base
            .join("test-trash")
            .join(&folder_name)
            .join("notes.md");
        assert_eq!(
            std::fs::read_to_string(trashed).unwrap(),
            "scratch",
            "in the Trash, not deleted"
        );
        assert_eq!(
            crate::catalog::create_chat(&state, "   ").unwrap().name,
            "New chat"
        );
    }

    /// A chat whose folder became a project's root keeps that folder when
    /// it is deleted: the conversation goes, the project's root stays, and
    /// the record says why. A plain chat's folder still goes.
    #[tokio::test]
    async fn deleting_a_chat_never_removes_a_projects_root() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let attached = crate::catalog::create_chat(&state, "plans").unwrap();
        let folder = std::path::PathBuf::from(&attached.worktree_path);
        std::fs::write(folder.join("plan.md"), "keep me").unwrap();
        let (project, _) = crate::catalog::create_project(&state, "Plans", None).unwrap();
        crate::catalog::link_socket_to_project(
            &state,
            &attached.stream_id,
            &project.project_id,
            &attached.worktree_path,
            false,
        )
        .unwrap();
        let plain = crate::catalog::create_chat(&state, "scratch").unwrap();
        let discard = |stream_id: String| {
            discard_stream(
                State(state.clone()),
                Json(DiscardReq {
                    stream_id,
                    confirm: false,
                }),
            )
        };

        let kept = discard(attached.stream_id.clone()).await.unwrap().0;
        assert!(stream_discarded(&state, &attached.stream_id));
        assert_eq!(
            std::fs::read_to_string(folder.join("plan.md")).unwrap(),
            "keep me"
        );
        assert!(!kept.room_destroyed);
        assert!(
            kept.kept.as_deref().is_some_and(|k| k.contains("Plans")),
            "{:?}",
            kept.kept
        );

        let gone = discard(plain.stream_id.clone()).await.unwrap().0;
        assert!(gone.chat_trashed && gone.kept.is_none());
        assert!(!std::path::Path::new(&plain.worktree_path).exists());

        // Fails closed: a root that can't be checked keeps a folder too.
        // SAFETY: geteuid only reads this process's effective uid.
        if unsafe { libc::geteuid() } != 0 {
            use std::os::unix::fs::PermissionsExt;
            let room = folder.parent().unwrap().to_path_buf();
            let other = crate::catalog::create_chat(&state, "other").unwrap();
            std::fs::set_permissions(&room, std::fs::Permissions::from_mode(0o000)).unwrap();
            let held = discard(other.stream_id.clone()).await;
            std::fs::set_permissions(&room, std::fs::Permissions::from_mode(0o755)).unwrap();
            let held = held.unwrap().0;
            assert!(
                held.kept
                    .as_deref()
                    .is_some_and(|k| k.contains("couldn't be checked")),
                "{:?}",
                held.kept
            );
            assert!(std::path::Path::new(&other.worktree_path).exists());
        }
    }

    /// A chat from before chats had a folder of their own keeps it inside
    /// its room: on delete the folder goes to the Trash first, then the room.
    #[tokio::test]
    async fn an_older_chats_folder_in_its_room_goes_to_the_trash_too() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let chat = crate::catalog::create_chat(&state, "older").unwrap();
        let room = create_room_runtime(&state, &chat.stream_id);
        let inside = room.join("chat");
        std::fs::rename(&chat.worktree_path, &inside).unwrap();
        std::fs::write(inside.join("old.md"), "from before").unwrap();
        append(
            &state,
            &chat.stream_id,
            "human",
            crate::node::SOCKET_WORKSPACE_FAMILY,
            serde_json::to_string(&crate::node::WorkspaceSet {
                worktree_path: inside.to_string_lossy().into_owned(),
                branch: String::new(),
                repo_root: String::new(),
                created_by_bakery: true,
                fork_note: None,
            })
            .unwrap(),
            vec![],
        )
        .unwrap();

        let gone = discard_stream(
            State(state.clone()),
            Json(DiscardReq {
                stream_id: chat.stream_id.clone(),
                confirm: false,
            }),
        )
        .await
        .unwrap()
        .0;
        assert!(gone.chat_trashed && gone.room_destroyed, "{:?}", gone.kept);
        assert!(!room.exists());
        let trashed = state.base.join("test-trash").join("chat").join("old.md");
        assert_eq!(std::fs::read_to_string(trashed).unwrap(), "from before");
    }

    #[tokio::test]
    async fn a_baker_thread_discards_but_the_main_baker_conversation_never_does() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("bakery"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        state.node.lock().unwrap().bakery_source_root = Some(repo.to_string_lossy().into_owned());
        let baker = crate::routes::baker::ensure_permanent_baker(&state, None, None)
            .unwrap()
            .stream_id;
        let thread =
            crate::baker_threads::create_thread(&state, &Default::default(), "human", None, None)
                .unwrap()
                .thread
                .stream_id;

        let err = discard_stream(
            State(state.clone()),
            Json(DiscardReq {
                stream_id: baker.clone(),
                confirm: true,
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::CONFLICT);
        assert!(!stream_discarded(&state, &baker));

        let resp = discard_stream(
            State(state.clone()),
            Json(DiscardReq {
                stream_id: thread.clone(),
                confirm: false,
            }),
        )
        .await
        .unwrap();
        assert!(
            !resp.0.worktree_removed,
            "a thread's in-place factory root is never removed"
        );
        assert!(repo.join(".git").exists());
        assert!(stream_discarded(&state, &thread));
        assert!(!crate::baker_threads::is_baker_thread_stream(
            &state, &thread
        ));
    }

    #[tokio::test]
    async fn a_fresh_stream_defaults_to_slice_lifetime_and_is_not_discarded() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id_slice1 = open_stream(&state, &open_req(&repo, "slice1"))
            .unwrap()
            .context
            .stream_id;

        assert_eq!(stream_lifetime(&state, &id_slice1), Lifetime::Slice);
        assert!(!stream_discarded(&state, &id_slice1));
    }

    #[tokio::test]
    async fn set_lifetime_round_trips_and_known_streams_reflects_it() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id_keep = open_stream(&state, &open_req(&repo, "keep"))
            .unwrap()
            .context
            .stream_id;

        let resp = set_lifetime(
            State(state.clone()),
            Json(LifetimeReq {
                stream_id: id_keep.clone(),
                lifetime: Lifetime::Living,
            }),
        )
        .await
        .unwrap();
        assert_eq!(resp.0.lifetime, Lifetime::Living);
        assert_eq!(stream_lifetime(&state, &id_keep), Lifetime::Living);

        let streams = crate::state::known_streams(&state).unwrap();
        let row = streams.iter().find(|s| s.stream_id == id_keep).unwrap();
        assert_eq!(row.lifetime, Lifetime::Living);
        assert!(!row.discarded);
    }

    #[tokio::test]
    async fn set_lifetime_refuses_an_unknown_stream() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let err = set_lifetime(
            State(state),
            Json(LifetimeReq {
                stream_id: "wt:nope".into(),
                lifetime: Lifetime::Burn,
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn discard_removes_a_bakery_owned_worktree_and_the_room_runtime() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let resp = open_stream(&state, &open_req(&repo, "bug1")).unwrap();
        let worktree_path = std::path::PathBuf::from(&resp.context.worktree_path);
        assert!(
            worktree_path.is_dir(),
            "sanity: worktree exists before discard"
        );

        // Give it a room runtime to destroy too. Provisioning the compiler is
        // unrelated to lifecycle cleanup and would clone the whole host
        // sysroot just to create these two directories.
        let room_dir = create_room_runtime(&state, &resp.context.stream_id);
        assert!(
            room_dir.is_dir(),
            "sanity: room runtime exists before discard"
        );

        let before_events = crate::state::known_streams(&state)
            .unwrap()
            .into_iter()
            .find(|s| s.stream_id == resp.context.stream_id)
            .unwrap()
            .events;

        let discard = discard_stream(
            State(state.clone()),
            Json(DiscardReq {
                stream_id: resp.context.stream_id.clone(),
                confirm: false,
            }),
        )
        .await
        .unwrap()
        .0;

        assert!(discard.worktree_removed);
        assert!(discard.room_destroyed);
        assert!(!worktree_path.is_dir(), "worktree must actually be gone");
        assert!(!room_dir.is_dir(), "room runtime must actually be gone");

        // Lineage: never fewer events, always at least the discard event more.
        let after = crate::state::known_streams(&state)
            .unwrap()
            .into_iter()
            .find(|s| s.stream_id == resp.context.stream_id)
            .unwrap();
        assert!(
            after.events > before_events,
            "discard must never delete Heart rows, only add one"
        );
        assert!(after.discarded);
    }

    #[tokio::test]
    async fn discard_refuses_a_living_stream_without_confirm_but_succeeds_with_it() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id_pinned = open_stream(&state, &open_req(&repo, "pinned"))
            .unwrap()
            .context
            .stream_id;
        let _ = set_lifetime(
            State(state.clone()),
            Json(LifetimeReq {
                stream_id: id_pinned.clone(),
                lifetime: Lifetime::Living,
            }),
        )
        .await
        .unwrap();

        let err = discard_stream(
            State(state.clone()),
            Json(DiscardReq {
                stream_id: id_pinned.clone(),
                confirm: false,
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert!(
            !stream_discarded(&state, &id_pinned),
            "a refused discard must not be recorded"
        );

        let ok = discard_stream(
            State(state.clone()),
            Json(DiscardReq {
                stream_id: id_pinned.clone(),
                confirm: true,
            }),
        )
        .await
        .unwrap();
        assert!(ok.0.worktree_removed);
        assert!(stream_discarded(&state, &id_pinned));
    }

    #[tokio::test]
    async fn discard_never_removes_an_in_place_checkout_but_still_destroys_its_room() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        // Opening with worktree_path == repo attaches in place (see
        // `open_in_place_attaches_the_repos_own_checkout` in routes::project).
        let mut req = open_req(&repo, "inplace");
        req.worktree_path = Some(repo.to_string_lossy().into_owned());
        let id_inplace = open_stream(&state, &req).unwrap().context.stream_id;

        create_room_runtime(&state, &id_inplace);

        let discard = discard_stream(
            State(state.clone()),
            Json(DiscardReq {
                stream_id: id_inplace,
                confirm: false,
            }),
        )
        .await
        .unwrap()
        .0;

        assert!(
            !discard.worktree_removed,
            "an in-place checkout must never be `git worktree remove`d"
        );
        assert!(
            repo.join("f.txt").is_file(),
            "the human's own repo must be untouched"
        );
        assert!(
            discard.room_destroyed,
            "the room runtime is Bakery's own cache dir — still fine to destroy"
        );
    }

    #[tokio::test]
    async fn discarding_twice_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id_once = open_stream(&state, &open_req(&repo, "once"))
            .unwrap()
            .context
            .stream_id;

        let _ = discard_stream(
            State(state.clone()),
            Json(DiscardReq {
                stream_id: id_once.clone(),
                confirm: false,
            }),
        )
        .await
        .unwrap();
        let err = discard_stream(
            State(state.clone()),
            Json(DiscardReq {
                stream_id: id_once.clone(),
                confirm: false,
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn discard_refuses_an_unknown_stream() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let err = discard_stream(
            State(state),
            Json(DiscardReq {
                stream_id: "wt:nope".into(),
                confirm: false,
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::NOT_FOUND);
    }

    /// 04-LIFECYCLE audit, issue 1: the window between "discard started" and
    /// "the `stream.discarded` event landed" must not let a new attach in.
    #[tokio::test]
    async fn retiring_flag_refuses_new_attach_routes() {
        use crate::routes::env::{env_provision, ProvisionReq};

        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id_busy = open_stream(&state, &open_req(&repo, "busy"))
            .unwrap()
            .context
            .stream_id;

        state
            .lifecycle
            .lock()
            .unwrap()
            .retiring
            .insert(id_busy.clone());

        let err = env_provision(
            State(state.clone()),
            Json(ProvisionReq {
                stream_id: id_busy.clone(),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::CONFLICT);
    }

    /// The stream stays refused *after* discard too, even though an
    /// in-place stream's worktree survives on disk — the surviving directory
    /// must not read as "still attachable" (04-LIFECYCLE audit: "Discarded
    /// in-place streams also remain usable through backend routes").
    #[tokio::test]
    async fn a_discarded_stream_refuses_new_attach_routes() {
        use crate::routes::env::{env_provision, ProvisionReq};

        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let mut req = open_req(&repo, "gone");
        req.worktree_path = Some(repo.to_string_lossy().into_owned());
        let id_gone = open_stream(&state, &req).unwrap().context.stream_id;

        let _ = discard_stream(
            State(state.clone()),
            Json(DiscardReq {
                stream_id: id_gone.clone(),
                confirm: false,
            }),
        )
        .await
        .unwrap();
        assert!(
            repo.is_dir(),
            "sanity: the in-place checkout itself must survive discard"
        );

        let err = env_provision(
            State(state.clone()),
            Json(ProvisionReq { stream_id: id_gone }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert!(err.1.contains("discarded"), "{}", err.1);
    }

    /// 04-LIFECYCLE audit, issue 2: a failure removing the worktree must not
    /// skip room cleanup, and the attempt must be recorded (not silently
    /// dropped by an early `?`) so the stream stays retriable rather than
    /// vanishing into an undiscoverable half-state.
    #[tokio::test]
    async fn partial_failure_discard_is_recorded_and_retriable() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let resp = open_stream(&state, &open_req(&repo, "flaky")).unwrap();
        let worktree_path = std::path::PathBuf::from(&resp.context.worktree_path);

        let room_dir = create_room_runtime(&state, &resp.context.stream_id);
        assert!(room_dir.is_dir(), "sanity: room exists before discard");

        // Break the main repo so `git worktree remove` fails deterministically.
        // The linked worktree directory itself is untouched, so `is_dir()`
        // still lets discard attempt (and fail) the removal.
        std::fs::remove_dir_all(repo.join(".git")).unwrap();

        let err = discard_stream(
            State(state.clone()),
            Json(DiscardReq {
                stream_id: resp.context.stream_id.clone(),
                confirm: false,
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(err.1.contains("partially failed"), "{}", err.1);

        assert!(
            !room_dir.is_dir(),
            "room destruction must still be attempted independently of the worktree failure"
        );
        assert!(
            worktree_path.is_dir(),
            "git refused the removal — it must still be there, not deleted some other way"
        );
        assert!(
            !stream_discarded(&state, &resp.context.stream_id),
            "a partial failure must be retriable, not treated as done"
        );

        // Retry must actually retry — not bounce off "already discarded".
        let err2 = discard_stream(
            State(state.clone()),
            Json(DiscardReq {
                stream_id: resp.context.stream_id.clone(),
                confirm: false,
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err2.0, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(!err2.1.contains("already discarded"), "{}", err2.1);
    }

    /// 04-LIFECYCLE audit, issue 4: a worktree living under Bakery's own
    /// `worktree_base` is not automatically "ours" — only `created_by_bakery`
    /// (set once, when `add_worktree` actually ran) proves that. This one
    /// was attached via an explicit `worktree_path` (a human picking an
    /// existing tree out of `git worktree list`), so it must never be force-
    /// removed even though it happens to sit under the same parent dir.
    #[tokio::test]
    async fn a_manually_attached_worktree_under_the_base_dir_is_never_removed() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));

        let manual_path = state.worktree_base.join("manual");
        std::fs::create_dir_all(state.worktree_base.as_path()).unwrap();
        run_git(
            &repo,
            &[
                "worktree",
                "add",
                "-b",
                "manual",
                &manual_path.to_string_lossy(),
            ],
        );

        let mut req = open_req(&repo, "attached");
        req.worktree_path = Some(manual_path.to_string_lossy().into_owned());
        let id_attached = open_stream(&state, &req).unwrap().context.stream_id;

        let discard = discard_stream(
            State(state.clone()),
            Json(DiscardReq {
                stream_id: id_attached,
                confirm: false,
            }),
        )
        .await
        .unwrap()
        .0;

        assert!(
            !discard.worktree_removed,
            "unproven provenance must refuse removal even under worktree_base"
        );
        assert!(
            manual_path.is_dir(),
            "the manually attached worktree must survive discard"
        );
    }

    /// Harden against a malformed/unreadable fact: "no event at all" (the
    /// documented `Slice`/not-discarded default) must not be confused with
    /// "an event exists but can't be parsed" (fail closed instead).
    #[tokio::test]
    async fn malformed_lifetime_and_discard_facts_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id_garbled = open_stream(&state, &open_req(&repo, "garbled"))
            .unwrap()
            .context
            .stream_id;

        append(
            &state,
            &id_garbled,
            "human",
            LIFETIME_FAMILY,
            "not json".to_string(),
            vec![],
        )
        .unwrap();
        assert_eq!(
            stream_lifetime(&state, &id_garbled),
            Lifetime::Living,
            "an unparseable lifetime fact must fail closed, not silently waive Living's confirm gate"
        );

        append(
            &state,
            &id_garbled,
            "human",
            DISCARD_FAMILY,
            "not json".to_string(),
            vec![],
        )
        .unwrap();
        assert!(
            stream_discarded(&state, &id_garbled),
            "an unparseable discard fact must fail closed to 'discarded/blocked', not 'clean'"
        );
    }

    // ─── 04b-FENCE: attach/discard fencing ──────────────────────────────────

    /// §1: the whole point of `begin_discard` and `admit_one_shot`/`admit_pty`
    /// sharing one lock is that "discard has committed to retiring" and "a
    /// new attach can start" can never both be true. No sleep, no timing
    /// dependency — the ordering is structural (one mutex), not a race to
    /// win.
    #[tokio::test]
    async fn admission_is_refused_the_instant_a_discard_has_committed_to_retiring() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id_race = open_stream(&state, &open_req(&repo, "race"))
            .unwrap()
            .context
            .stream_id;

        let (_guard, _lifetime) = crate::state::begin_discard(&state, &id_race, false).unwrap();

        let err = crate::state::admit_one_shot(&state, &id_race)
            .map(|_| ())
            .unwrap_err();
        assert_eq!(err.0, StatusCode::CONFLICT);
        let err = crate::state::admit_pty(&state, &id_race, "host")
            .map(|_| ())
            .unwrap_err();
        assert_eq!(err.0, StatusCode::CONFLICT);
    }

    /// §2, the other direction of the same guarantee: `set_lifetime` cannot
    /// land while a discard has committed to retiring — the window between
    /// "discard read the old lifetime" and "a concurrent set_lifetime
    /// applied a new one" cannot exist, because both hold the same lock to
    /// do their own read-then-write.
    #[tokio::test]
    async fn set_lifetime_is_refused_while_a_discard_has_committed_to_retiring() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id_race2 = open_stream(&state, &open_req(&repo, "race2"))
            .unwrap()
            .context
            .stream_id;

        let (_guard, _lifetime) = crate::state::begin_discard(&state, &id_race2, false).unwrap();

        let err = set_lifetime(
            State(state.clone()),
            Json(LifetimeReq {
                stream_id: id_race2.clone(),
                lifetime: Lifetime::Living,
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::CONFLICT);
    }

    /// §3: a fence that never clears must abort the whole discard, not
    /// proceed to delete anyway. A genuinely stuck attach is simulated by
    /// admitting a one-shot and never releasing it; the production 35s
    /// timeout would make this test take 35 real seconds, so it goes through
    /// `discard_with_one_shot_timeout` with a timeout small enough to run
    /// fast.
    #[tokio::test]
    async fn discard_aborts_without_removing_anything_when_a_one_shot_attach_never_clears() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let resp = open_stream(&state, &open_req(&repo, "stuck")).unwrap();
        let worktree_path = std::path::PathBuf::from(&resp.context.worktree_path);

        let wedge = crate::state::admit_one_shot(&state, &resp.context.stream_id).unwrap();

        let err = discard_with_one_shot_timeout(
            state.clone(),
            DiscardReq {
                stream_id: resp.context.stream_id.clone(),
                confirm: false,
            },
            Duration::from_millis(200),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::CONFLICT);
        assert!(err.1.contains("aborted"), "{}", err.1);
        assert!(
            worktree_path.is_dir(),
            "a timed-out fence must leave the worktree untouched"
        );
        assert!(
            !stream_discarded(&state, &resp.context.stream_id),
            "an aborted discard must not be recorded as discarded"
        );

        // Retry after the wedge clears must actually work — the abort is
        // recoverable, not a permanent refusal.
        drop(wedge);
        let ok = discard_with_one_shot_timeout(
            state.clone(),
            DiscardReq {
                stream_id: resp.context.stream_id.clone(),
                confirm: false,
            },
            Duration::from_millis(200),
        )
        .await
        .unwrap();
        assert!(ok.0.worktree_removed);
    }

    /// §5: an unreadable lifetime fact must refuse host-mode access the same
    /// as a confirmed `Burn` — reusing `Lifetime::Living`'s fail-closed
    /// default here would read backwards ("unreadable, therefore Living,
    /// therefore host is fine"). Discard's own confirm gate is the other,
    /// correct use of that same fail-closed default: an unreadable fact
    /// still only needs `confirm:true`, exactly like a real `Living` stream.
    #[tokio::test]
    async fn unreadable_lifetime_refuses_host_access_but_discard_still_just_needs_confirm() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id_unreadable = open_stream(&state, &open_req(&repo, "unreadable"))
            .unwrap()
            .context
            .stream_id;
        append(
            &state,
            &id_unreadable,
            "human",
            LIFETIME_FAMILY,
            "not json".to_string(),
            vec![],
        )
        .unwrap();

        assert!(
            !crate::state::stream_host_allowed(&state, &id_unreadable),
            "an unreadable lifetime must refuse host access, not fall back to Living-allows-host"
        );

        let err = discard_stream(
            State(state.clone()),
            Json(DiscardReq {
                stream_id: id_unreadable.clone(),
                confirm: false,
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert!(err.1.contains("confirm"), "{}", err.1);

        let ok = discard_stream(
            State(state.clone()),
            Json(DiscardReq {
                stream_id: id_unreadable.clone(),
                confirm: true,
            }),
        )
        .await
        .unwrap();
        assert!(ok.0.worktree_removed);
    }

    // ─── 04c-BURN ────────────────────────────────────────────────────────────

    /// §1/§2: a Host WS admitted but not yet spawned must not be ignored by
    /// Burn's fence. Admits directly via `admit_pty` (exactly "Host WS
    /// admits, don't spawn yet" from the audit) rather than a real network
    /// round trip — deterministic, no sleep needed to "win" a race, since
    /// admission is a synchronous, atomic reservation either way.
    #[tokio::test]
    async fn burn_refuses_to_report_success_while_a_pending_host_admission_has_not_resolved() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id_pendinghost = open_stream(&state, &open_req(&repo, "pendinghost"))
            .unwrap()
            .context
            .stream_id;

        let pending = crate::state::admit_pty(&state, &id_pendinghost, "host").unwrap();

        // The one real timing cost in this test: the burn fence actually
        // waits (bounded) for the pending host to resolve. Shrunk from the
        // production 5s so the test itself stays fast.
        let err = set_lifetime_with_burn_fence_timeout(
            state.clone(),
            LifetimeReq {
                stream_id: id_pendinghost.clone(),
                lifetime: Lifetime::Burn,
            },
            Duration::from_millis(200),
        )
        .await
        .unwrap_err();
        assert_eq!(
            err.0,
            StatusCode::CONFLICT,
            "the API must not report Burn a success with a host attach still unresolved"
        );
        assert!(err.1.contains("retry"), "{}", err.1);

        // The lifetime is already committed to Burn even though the fence
        // failed (04c-BURN §2: "commit Burn in Heart first... lifetime is
        // already Burn" — the 409 is about the fence, not the transition).
        assert_eq!(stream_lifetime(&state, &id_pendinghost), Lifetime::Burn);

        // No *new* Host can start now that Burn has landed.
        let err2 = crate::state::admit_pty(&state, &id_pendinghost, "host")
            .map(|_| ())
            .unwrap_err();
        assert_eq!(err2.0, StatusCode::FORBIDDEN);

        // The pending session ends without ever spawning (same as a spawn
        // failure would abandon it) — the fence now has nothing to wait for.
        drop(pending);
        let ok = set_lifetime_with_burn_fence_timeout(
            state.clone(),
            LifetimeReq {
                stream_id: id_pendinghost.clone(),
                lifetime: Lifetime::Burn,
            },
            Duration::from_millis(200),
        )
        .await
        .unwrap();
        assert_eq!(ok.0.lifetime, Lifetime::Burn);
    }

    /// §1: pending is tracked per mode — a burn fence must not wait on (or
    /// be satisfied by) a pending *room* attach, which has nothing to do
    /// with burn.
    #[tokio::test]
    async fn burn_fence_ignores_pending_room_admissions() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id_pendingroom = open_stream(&state, &open_req(&repo, "pendingroom"))
            .unwrap()
            .context
            .stream_id;

        let _room_pending = crate::state::admit_pty(&state, &id_pendingroom, "room").unwrap();

        let ok = set_lifetime(
            State(state.clone()),
            Json(LifetimeReq {
                stream_id: id_pendingroom.clone(),
                lifetime: Lifetime::Burn,
            }),
        )
        .await
        .unwrap();
        assert_eq!(
            ok.0.lifetime,
            Lifetime::Burn,
            "a pending room attach must never block or fail a burn transition"
        );
    }

    /// §4: `/agent/turn`, `/agent/accept`, `/agent/reject`, and `/files/*`
    /// now hold a counted activity lease for their duration, same as env's
    /// one-shot attaches — a long-running turn must not let discard rm the
    /// worktree out from under it.
    #[tokio::test]
    async fn discard_aborts_without_removing_anything_while_an_activity_lease_is_held() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let resp = open_stream(&state, &open_req(&repo, "turning")).unwrap();
        let worktree_path = std::path::PathBuf::from(&resp.context.worktree_path);

        let lease = crate::state::admit_activity(&state, &resp.context.stream_id).unwrap();

        let err = discard_with_one_shot_timeout(
            state.clone(),
            DiscardReq {
                stream_id: resp.context.stream_id.clone(),
                confirm: false,
            },
            Duration::from_millis(200),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::CONFLICT);
        assert!(
            worktree_path.is_dir(),
            "an in-flight agent turn must not be raced by discard"
        );

        drop(lease);
        let ok = discard_with_one_shot_timeout(
            state.clone(),
            DiscardReq {
                stream_id: resp.context.stream_id.clone(),
                confirm: false,
            },
            Duration::from_millis(200),
        )
        .await
        .unwrap();
        assert!(ok.0.worktree_removed);
    }

    // ─── 04d-GHOSTTY-LEASE ───────────────────────────────────────────────────

    /// §2 (revised after audit): the fix is not "let a second write land and
    /// have the fence notice it later" — a generation counter re-checked
    /// under a freshly re-acquired lock can only ever detect a race after
    /// it already happened, because the fence's actual kill call runs
    /// outside any lock. The fix is to make the second write impossible in
    /// the first place: while `begin_burn_transition`'s guard is held,
    /// *every* other lifetime write — including a second `Burn` — is
    /// refused, and only accepted again once the fence has fully ended.
    #[tokio::test]
    async fn set_lifetime_refuses_other_writes_while_a_burn_fence_is_active() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id_fencing = open_stream(&state, &open_req(&repo, "fencing"))
            .unwrap()
            .context
            .stream_id;

        let guard = crate::state::begin_burn_transition(&state, &id_fencing).unwrap();
        // The commit itself isn't gated on the fence finishing — only
        // further writes are, for as long as the guard is held.
        assert_eq!(stream_lifetime(&state, &id_fencing), Lifetime::Burn);

        let err = set_non_burn_lifetime(&state, &id_fencing, Lifetime::Living)
            .map(|_| ())
            .unwrap_err();
        assert_eq!(
            err.0,
            StatusCode::CONFLICT,
            "a write must not land while this stream's burn fence is active"
        );

        let err = crate::state::begin_burn_transition(&state, &id_fencing)
            .map(|_| ())
            .unwrap_err();
        assert_eq!(
            err.0,
            StatusCode::CONFLICT,
            "a second Burn must not start a second fence on top of the first"
        );

        drop(guard);
        // The fence has ended — writes are accepted again.
        set_non_burn_lifetime(&state, &id_fencing, Lifetime::Living).unwrap();
        assert_eq!(stream_lifetime(&state, &id_fencing), Lifetime::Living);
    }

    // ─── 06-SEATS drive-by: only begin_burn_transition may set Burn ────────

    /// `set_non_burn_lifetime` must refuse `Burn` directly, not merely rely
    /// on `set_lifetime`'s HTTP-layer branch never calling it that way — a
    /// future caller reaching for "the function that sets a lifetime" must
    /// not be able to commit an unfenced Burn by accident.
    #[tokio::test]
    async fn set_non_burn_lifetime_rejects_burn_directly() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id_notburn = open_stream(&state, &open_req(&repo, "notburn"))
            .unwrap()
            .context
            .stream_id;

        let err = set_non_burn_lifetime(&state, &id_notburn, Lifetime::Burn)
            .map(|_| ())
            .unwrap_err();
        assert_eq!(err.0, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(err.1.contains("begin_burn_transition"), "{}", err.1);
        assert_eq!(
            stream_lifetime(&state, &id_notburn),
            Lifetime::Slice,
            "the rejected call must not have written anything"
        );
    }

    // ─── 07d-A3: root deletion rule ────────────────────────────────────────

    /// A Project root Bakery itself mkdir'd (07d-A3's `create_if_missing`) is
    /// still never force-removed on discard — it lives wherever the human
    /// pointed, not under `state.worktree_base`, so it fails the EXISTING
    /// `is_bakery_owned_worktree` gate (`created_by_bakery && under base`) on
    /// the base-containment half alone. No lifecycle.rs change was needed
    /// for this: the existing rule was already narrow enough to exclude
    /// every Project-catalog root, git or not — this proves that rather
    /// than assuming it.
    #[tokio::test]
    async fn discard_never_removes_a_project_root_even_one_bakery_itself_created() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let root = dir.path().join("fresh-project-root");
        let (_record, ctx) = crate::catalog::create_project(
            &state,
            "Rootful",
            Some(crate::catalog::DirectoryRootSpec::Create {
                parent: dir.path().to_str().unwrap(),
                name: "fresh-project-root",
            }),
        )
        .unwrap();
        assert!(root.is_dir());
        assert!(
            ctx.created_by_bakery,
            "sanity: this directory really was mkdir'd by the create call"
        );

        let discard = discard_stream(
            State(state.clone()),
            Json(DiscardReq {
                stream_id: ctx.stream_id,
                confirm: false,
            }),
        )
        .await
        .unwrap()
        .0;

        assert!(
            !discard.worktree_removed,
            "a Project root must never be force-removed, even one Bakery created"
        );
        assert!(root.is_dir(), "the directory itself must still be on disk");
    }

    /// Retiring the Project (not discarding the socket) is the same story,
    /// from the other direction: it never touches the filesystem at all.
    #[tokio::test]
    async fn retiring_a_project_never_touches_its_root_directory() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let root = dir.path().join("retire-me-not-my-files");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("keep.txt"), "still here\n").unwrap();
        let (record, _ctx) = crate::catalog::create_project(
            &state,
            "Retiring",
            Some(crate::catalog::DirectoryRootSpec::Attach(
                root.to_str().unwrap(),
            )),
        )
        .unwrap();

        crate::catalog::retire_project(&state, &record.project_id).unwrap();

        assert_eq!(
            std::fs::read_to_string(root.join("keep.txt")).unwrap(),
            "still here\n"
        );
    }
}
