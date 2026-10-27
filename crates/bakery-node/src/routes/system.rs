//! Small host-only conveniences that sit outside the room contract: whether
//! `ghostty` is on this node's PATH, and opening a stream's worktree cwd in
//! it. Optional and quiet — a machine without Ghostty installed must not see
//! an error, just a hidden button in the studio (STEERING: "Failure is
//! quiet/disabled, not a slice name").

use crate::state::{admit_pty, jail_for, AppState};
use axum::{extract::State, http::StatusCode, Json};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

fn ghostty_path() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("ghostty"))
        .find(|p| p.is_file())
}

#[derive(schemars::JsonSchema, Serialize)]
pub(crate) struct GhosttyAvailable {
    pub(crate) available: bool,
}

pub(crate) async fn ghostty_available() -> Json<GhosttyAvailable> {
    Json(GhosttyAvailable {
        available: ghostty_path().is_some(),
    })
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct GhosttyOpenReq {
    pub(crate) stream_id: String,
}

#[derive(schemars::JsonSchema, Serialize)]
pub(crate) struct GhosttyOpenResp {
    pub(crate) opened: bool,
}

/// Best-effort only: a stream whose worktree just vanished, or a Ghostty
/// that disappeared from PATH between the status check and this call, both
/// come back `opened: false` rather than an error the studio has to render.
/// Same for a `burn` stream (or one whose lifetime can't be read at all —
/// 04b-FENCE §5, "reject host PTY and Ghostty" on an unknown fact, not just
/// a confirmed `Burn`): an unsandboxed external terminal is exactly the host
/// access STEERING says hostile/untrusted never gets, so this stays
/// quiet-refused rather than a client-only button hide (04-LIFECYCLE audit).
/// A retiring/discarded stream refuses too — new opens on a stream whose
/// worktree is gone or about to be make no sense (04b-FENCE §4).
///
/// `admit_pty(.., "host")` is held from admission all the way through
/// `spawn()` returning — success or failure — not just through the checks
/// beforehand. Checking retiring/discarded/burn and then spawning
/// afterward, with nothing held in between, left a gap for discard or a
/// burn transition to commit right after the checks passed and right
/// before the external process actually started (04d-GHOSTTY-LEASE §1).
/// This is the exact same host-launch gate a host PTY attach goes through
/// — one atomic admission, not three separate checks with room to race
/// between them. The lease is dropped, never promoted, the instant
/// `spawn()` returns: the resulting Ghostty window is never tracked and
/// never killed after it opens (04c-BURN §4 already decided that on
/// purpose — this reuses the same reservation mechanics, not the
/// kill-a-live-session ones).
pub(crate) async fn ghostty_open(
    State(st): State<AppState>,
    Json(req): Json<GhosttyOpenReq>,
) -> Result<Json<GhosttyOpenResp>, (StatusCode, String)> {
    let Some(ghostty) = ghostty_path() else {
        return Ok(Json(GhosttyOpenResp { opened: false }));
    };
    let lease = match admit_pty(&st, &req.stream_id, "host") {
        Ok(lease) => lease,
        // Burn (or an unreadable lifetime) stays a quiet `opened: false`,
        // same as "no Ghostty on PATH" — a hidden button, not an error the
        // studio has to render.
        Err((StatusCode::FORBIDDEN, _)) => return Ok(Json(GhosttyOpenResp { opened: false })),
        Err(e) => return Err(e),
    };
    let jail = jail_for(&st, &req.stream_id).map_err(|e| (StatusCode::NOT_FOUND, e))?;
    let opened = std::process::Command::new(ghostty)
        .arg(format!("--working-directory={}", jail.root().display()))
        .spawn()
        .is_ok();
    drop(lease);
    Ok(Json(GhosttyOpenResp { opened }))
}
