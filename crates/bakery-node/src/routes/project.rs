//! Open / attach / inspect / recents / file browsing routes.

use crate::state::{
    admit_activity, append, claim_ownership, jail_for, known_streams, same_path, str_field,
    ActiveContext, AppState, StreamRef,
};
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use bakery_core::Event;
use serde::{Deserialize, Serialize};

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct OpenWorktreeReq {
    /// Path to an existing git repository.
    pub(crate) repo: String,
    /// Worktree name (also the branch suffix and stream key). Must be unique.
    pub(crate) name: String,
}

/// The Open wizard's request. Everything except `repo` is optional: the node
/// suggests the name, picks the default branch, and decides create-vs-attach.
#[derive(schemars::JsonSchema, Debug, Default, Deserialize)]
pub(crate) struct OpenReq {
    /// Any path inside the repository — the node resolves the real root.
    pub(crate) repo: String,
    /// Worktree name; also the branch suffix and stream key. Auto-suggested when absent.
    #[serde(default)]
    pub(crate) name: Option<String>,
    /// Branch to check out in the worktree. Defaults to a free branch derived
    /// from `{name}` (see `crate::git::suggest_branch`) — not a `bakery/`-prefixed one.
    #[serde(default)]
    pub(crate) branch: Option<String>,
    /// Branch to fork from when creating `branch`. Defaults to the detected default branch.
    #[serde(default)]
    pub(crate) base_branch: Option<String>,
    /// Attach this already-materialised worktree instead of running `git worktree add`.
    #[serde(default)]
    pub(crate) worktree_path: Option<String>,
    /// 07d-A3: bind the resulting socket to this Project as one of its roots
    /// (root path = the socket's own `worktree_path`, the actual working
    /// directory — never the bare `repo_root`, since a Project's root is
    /// "a working directory", not "a git repository", and one root may be
    /// shared by more than one socket/worktree). `None` preserves today's
    /// behavior exactly: a bare git-worktree open with no Project membership
    /// (Ungrouped) — "today's open/worktree path becomes this capability,
    /// not creating the project."
    #[serde(default)]
    pub(crate) project_id: Option<String>,
    /// What a new conversation's agents may use of the Bakery's tools: all
    /// (the default), read or none.
    #[serde(default)]
    pub(crate) tools: Option<crate::api_tools::ToolsLimit>,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct OpenResp {
    /// The stream's `project.onboarded` event (existing one when attaching).
    #[schemars(with = "crate::api_catalog::EventShape")]
    pub(crate) event: Event,
    pub(crate) context: ActiveContext,
    /// True when this call attached to a stream that was already onboarded.
    pub(crate) attached: bool,
}

/// Legacy M2 shape: name is mandatory, only the event comes back. The wizard
/// uses `/project/open`; this stays so older clients keep working.
pub(crate) async fn open_worktree(
    State(st): State<AppState>,
    Json(req): Json<OpenWorktreeReq>,
) -> Result<Json<Event>, (StatusCode, String)> {
    open_stream(
        &st,
        &OpenReq {
            repo: req.repo,
            name: Some(req.name),
            ..Default::default()
        },
    )
    .map(|r| Json(r.event))
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

/// Read-only: what is this folder? Answers the wizard's first question so the
/// human never has to type a branch or worktree name blind.
pub(crate) async fn project_inspect(
    State(st): State<AppState>,
    Json(req): Json<InspectReq>,
) -> Result<Json<InspectResp>, (StatusCode, String)> {
    let info = crate::git::inspect(&req.path);
    let worktree_base = st.worktree_base.to_string_lossy().into_owned();
    if !info.is_git {
        // Not an error over the wire: the wizard renders the reason inline.
        return Ok(Json(InspectResp {
            suggested_name: String::new(),
            suggested_branch: String::new(),
            worktree_base,
            bakery_streams: vec![],
            project: info,
        }));
    }
    let root = info.root.clone().unwrap_or_default();
    let known = known_streams(&st).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let taken: Vec<String> = known.iter().map(|s| s.name.clone()).collect();
    let mut taken = taken;
    // A directory already sitting in the worktree base counts as taken too.
    if let Ok(entries) = std::fs::read_dir(st.worktree_base.as_path()) {
        for e in entries.flatten() {
            taken.push(e.file_name().to_string_lossy().into_owned());
        }
    }
    let suggested_name =
        crate::git::suggest_name(info.name.as_deref().unwrap_or("worktree"), &taken);
    let suggested_branch = crate::git::suggest_branch(&suggested_name, &info.local_branches);
    let bakery_streams = known.into_iter().filter(|s| s.repo_root == root).collect();
    Ok(Json(InspectResp {
        project: info,
        suggested_name,
        suggested_branch,
        worktree_base,
        bakery_streams,
    }))
}

pub(crate) async fn project_open(
    State(st): State<AppState>,
    Json(req): Json<OpenReq>,
) -> Result<Json<OpenResp>, (StatusCode, String)> {
    let resp = open_stream(&st, &req).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    // 07d-A3: linking is REQUIRED when requested, never best-effort — a
    // caller that asked for Project membership and got a bare 200 back must
    // never end up with a socket that silently stayed Ungrouped (same
    // discipline as `socket.kind.recorded` in `baker_assign_inner`).
    if let Some(project_id) = &req.project_id {
        let is_git = !resp.context.repo_root.is_empty();
        crate::catalog::link_socket_to_project(
            &st,
            &resp.context.stream_id,
            project_id,
            &resp.context.worktree_path,
            is_git,
        )
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    }
    Ok(Json(resp))
}

/// Recents: every stream this node knows, most recently active first.
pub(crate) async fn list_streams(
    State(st): State<AppState>,
) -> Result<Json<Vec<StreamRef>>, (StatusCode, String)> {
    known_streams(&st)
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct InspectReq {
    pub(crate) path: String,
}

#[derive(schemars::JsonSchema, Serialize)]
pub(crate) struct InspectResp {
    pub(crate) project: crate::git::ProjectInfo,
    /// Free worktree name derived from the repo basename.
    pub(crate) suggested_name: String,
    /// Free branch name for `suggested_name` — lets the wizard render an exact
    /// preview (name, branch, path) before anything is created.
    pub(crate) suggested_branch: String,
    /// This node's worktree base directory — the wizard shows it as the
    /// WORKTREE PATH preview, distinct from the source repo path.
    pub(crate) worktree_base: String,
    /// Streams this node already has for this repository.
    pub(crate) bakery_streams: Vec<StreamRef>,
}

pub(crate) async fn get_stream(
    State(st): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Vec<Event>>, (StatusCode, String)> {
    let events = st
        .heart
        .lock()
        .unwrap()
        .read_stream(&id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(events))
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct FilesQuery {
    pub(crate) stream_id: String,
    #[serde(default)]
    pub(crate) path: Option<String>,
}

#[derive(schemars::JsonSchema, Serialize)]
pub(crate) struct FileReadResp {
    pub(crate) path: String,
    pub(crate) text: String,
    /// `blake3` hex digest of `text`'s bytes — the baseline a human save
    /// echoes back as `expected_digest` so a conflicting external change is
    /// caught instead of overwritten (see `files_write`).
    pub(crate) digest: String,
    /// `text` is a secret (`machine_access::is_secret`): no desktop
    /// screenshot while the app shows it.
    pub(crate) secret: bool,
}

/// Lazy UI file tree: one directory level under the stream's worktree jail.
pub(crate) async fn files_list(
    State(st): State<AppState>,
    Query(q): Query<FilesQuery>,
) -> Result<Json<Vec<crate::tools::DirEntryInfo>>, (StatusCode, String)> {
    let path = q.path.as_deref().unwrap_or(".");
    files_list_inner(&st, &q.stream_id, path)
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

pub(crate) fn files_list_inner(
    state: &AppState,
    stream_id: &str,
    path: &str,
) -> Result<Vec<crate::tools::DirEntryInfo>, String> {
    // Held for the whole listing, not just this check (04c-BURN §4). A
    // discarded (or mid-discard) stream's worktree may already be gone —
    // browsing it must fail with "discarded", not a raw filesystem error
    // (04b-FENCE §4).
    let _activity = admit_activity(state, stream_id).map_err(|(_, msg)| msg)?;
    let jail = jail_for(state, stream_id)?;
    jail.list_dir(path)
}

/// Read a file's text under the stream's worktree jail — the editor's open
/// path. Untruncated (unlike the agent's `read_file` tool) and carries a
/// content digest; writes are a separate route (`files_write`), gated
/// against that digest so an external change is never silently overwritten.
pub(crate) async fn files_read(
    State(st): State<AppState>,
    Query(q): Query<FilesQuery>,
) -> Result<Json<FileReadResp>, (StatusCode, String)> {
    let path = q.path.clone().unwrap_or_else(|| ".".to_string());
    let (text, digest) = files_read_inner(&st, &q.stream_id, &path).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let jail = jail_for(&st, &q.stream_id).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let secret = jail.resolve_read(&path).is_ok_and(|real| crate::machine_access::is_secret(&st, &real, jail.root()));
    Ok(Json(FileReadResp { path, text, digest, secret }))
}

pub(crate) fn files_read_inner(
    state: &AppState,
    stream_id: &str,
    path: &str,
) -> Result<(String, String), String> {
    let _activity = admit_activity(state, stream_id).map_err(|(_, msg)| msg)?;
    let jail = jail_for(state, stream_id)?;
    // An agent's read keeps off the always-blocked list, as its own
    // read_file does; the list limits agents only.
    if crate::actor::agent_calling() {
        let real = jail.resolve_read(path)?;
        if let Some(blocked) = crate::machine_access::always_blocked(state, &real, jail.root()) {
            return Err(format!("{path} is always blocked ({}): never work around it", blocked.display()));
        }
    }
    jail.read_file_for_edit(path)
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct FileWriteReq {
    pub(crate) stream_id: String,
    pub(crate) path: String,
    pub(crate) text: String,
    /// The digest last read for this file, or `None` for a file the caller
    /// never read (a brand-new file). A `Some` that no longer matches what's
    /// on disk comes back as a `conflict` response instead of overwriting it.
    #[serde(default)]
    pub(crate) expected_digest: Option<String>,
}

/// Tagged so a conflict is an ordinary 200 body the client inspects (it
/// needs `current_text` to offer reload/diff), not an HTTP error status —
/// "someone else changed this file" is an expected outcome here, not a
/// route failure the way a jail escape or a discarded stream is.
#[derive(schemars::JsonSchema, Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum FileWriteResp {
    Ok {
        path: String,
        digest: String,
    },
    Conflict {
        path: String,
        current_digest: Option<String>,
        current_text: Option<String>,
        /// Who changed it since the save's base, in words ("the agent in
        /// “Fix login”"); none when the Bakery doesn't know.
        changed_by: Option<String>,
    },
}

/// Human save. This is the *only* write path in the crate — the agent's tool
/// table (`crate::tools::TOOLS`) has no write entry, and `Jail::call`
/// structurally refuses anything not `GateClass::Read` (see `tools.rs`), so
/// this route is unreachable from the agent loop. Writes an agent proposes
/// still go through `agent.file.changed` → human accept (`/agent/accept`);
/// this is the operator typing and saving, not a second way for an agent to land a
/// file change.
pub(crate) async fn files_write(
    State(st): State<AppState>,
    Json(req): Json<FileWriteReq>,
) -> Result<Json<FileWriteResp>, (StatusCode, String)> {
    let _activity = admit_activity(&st, &req.stream_id)?;
    let _decision = st.edit_decision_lock.lock().unwrap();
    let jail = jail_for(&st, &req.stream_id).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let target = jail.resolve(&req.path).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    // Text files only (E-5): anything else opens in Code or another editor.
    let before = match std::fs::read(&target) {
        Ok(bytes) => Some(String::from_utf8(bytes).ok().filter(|t| !t.contains('\0')).ok_or_else(|| {
            (StatusCode::BAD_REQUEST, format!("{} isn't a text file: open it in Code or another editor", req.path))
        })?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err((StatusCode::INTERNAL_SERVER_ERROR, format!("{}: {e}", req.path))),
    };
    let outcome = jail
        .write_file(&req.path, &req.text, req.expected_digest.as_deref())
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(Json(match outcome {
        crate::tools::WriteOutcome::Written { digest } => {
            let hidden = crate::machine_access::always_blocked(&st, &target, jail.root()).is_some();
            record_operator_edit(&st, &req.stream_id, &req.path, before.as_deref(), &req.text, hidden)
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
            if let Ok(rel) = target.strip_prefix(jail.root()) {
                crate::writers::wrote(
                    crate::writers::Writer::Operator,
                    jail.root(),
                    &rel.to_string_lossy(),
                );
            }
            FileWriteResp::Ok {
                path: req.path,
                digest,
            }
        }
        crate::tools::WriteOutcome::Conflict {
            current_digest,
            current_text,
        } => FileWriteResp::Conflict {
            changed_by: target.strip_prefix(jail.root()).ok().and_then(|rel| {
                crate::writers::changed_by(&st, jail.root(), &rel.to_string_lossy())
            }),
            path: req.path,
            current_digest,
            current_text,
        },
    }))
}

/// The operator's own save (E-3), for History, the code view's "who wrote
/// what" (`provenance`) and the agent's next turn (`routes::agent`).
pub(crate) const OPERATOR_EDIT_FAMILY: &str = "operator.file.edited";

/// Records a save with both sides, each up to the size a turn's changes show
/// (`snapshots::SHOWN_BYTES`); a path on the deny list gets its sizes only,
/// its content never reaching the history.
fn record_operator_edit(
    st: &AppState,
    stream_id: &str,
    file_path: &str,
    before: Option<&str>,
    after: &str,
    hidden: bool,
) -> Result<(), String> {
    let shown = |side: Option<&str>| side.filter(|t| !hidden && t.len() <= crate::snapshots::SHOWN_BYTES).map(str::to_string);
    // E-2: an agent change to this file still waiting for Keep or Undo stays
    // as it is; the save is noted as made over it.
    let waiting = waiting_changes(st, stream_id, file_path);
    let payload = serde_json::json!({
        "file_path": file_path,
        "old_content": shown(before),
        "new_content": shown(Some(after)),
        "old_size": before.map(str::len),
        "new_size": after.len(),
        "hidden": hidden.then_some("denied"),
        "over_changes": (!waiting.is_empty()).then_some(waiting),
    });
    append(st, stream_id, "human", OPERATOR_EDIT_FAMILY, payload.to_string(), vec![]).map(drop)
}

/// The landed agent changes to `file_path` no one has kept or undone yet.
fn waiting_changes(st: &AppState, stream_id: &str, file_path: &str) -> Vec<String> {
    let events = st.heart.lock().unwrap().read_stream(stream_id).unwrap_or_default();
    let decided: std::collections::HashSet<&str> = events
        .iter()
        .filter(|e| matches!(e.family.as_str(), "agent.edit.accepted" | "agent.edit.rejected"))
        .flat_map(|e| e.causality.iter().map(String::as_str))
        .collect();
    let same = |p: &str| p.trim_start_matches("./") == file_path.trim_start_matches("./");
    events
        .iter()
        .filter(|e| e.family == "agent.file.changed" && !decided.contains(e.event_hash.as_str()))
        .filter(|e| {
            let p: serde_json::Value = serde_json::from_str(&e.payload).unwrap_or_default();
            p["landed"].is_object() && p["file_path"].as_str().is_some_and(same)
        })
        .map(|e| e.event_hash.clone())
        .collect()
}

/// Whether the operator saved over this waiting change (E-2): its Undo then
/// restores nothing and says so.
pub(crate) fn saved_over(st: &AppState, stream_id: &str, change: &str) -> bool {
    let saves = st.heart.lock().unwrap().events_of_family(stream_id, OPERATOR_EDIT_FAMILY).unwrap_or_default();
    saves.iter().any(|e| {
        let p: serde_json::Value = serde_json::from_str(&e.payload).unwrap_or_default();
        p["over_changes"].as_array().is_some_and(|over| over.iter().any(|h| h == change))
    })
}

/// Open a project: resolve the repo, pick/create the worktree, and record the
/// stream's first event. Idempotent — re-opening an onboarded name **attaches**
/// (no `git worktree add`, no second onboarding event).
///
/// Synchronous (git + DB + broadcast are all sync); callable from tests without HTTP.
pub(crate) fn open_stream(state: &AppState, req: &OpenReq) -> Result<OpenResp, String> {
    let info = crate::git::inspect(&req.repo);
    if !info.is_git {
        return Err(info
            .error
            .unwrap_or_else(|| format!("{} is not a git repository", req.repo)));
    }
    let repo_root = info
        .root
        .clone()
        .ok_or("could not resolve repository root")?;
    let project = info.name.clone().unwrap_or_else(|| "project".to_string());

    let name = match req.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
        Some(n) => crate::git::sanitize_name(n),
        None => {
            let taken: Vec<String> = known_streams(state)?.into_iter().map(|s| s.name).collect();
            crate::git::suggest_name(&project, &taken)
        }
    };

    // ── attach path: a socket already carries this name ─────────────────────
    // 07d-A3: stream ids are opaque and server-minted, independent of `name`
    // — they can no longer double as the lookup key the way `wt:{name}` did.
    // "Does a socket by this name already exist" is a scan over known
    // sockets instead of a direct key read. Fine at this scale (a personal
    // node's socket count is dozens, not millions), and it preserves the
    // exact same idempotent-reopen behavior the direct lookup used to give.
    let existing = known_streams(state)?.into_iter().find(|s| s.name == name);
    if let Some(existing) = existing {
        let stream_id = existing.stream_id;
        let ev = state
            .heart
            .lock()
            .unwrap()
            .first_of_family(&stream_id, "project.onboarded")
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("stream {stream_id} has no project.onboarded event"))?;
        let p: serde_json::Value = serde_json::from_str(&ev.payload).map_err(|e| e.to_string())?;
        let existing_root = str_field(&p, "repo_root")
            .or_else(|| str_field(&p, "repo"))
            .unwrap_or_default();
        // Two different repos wanting the same name is a collision, not an
        // attach — fail closed.
        if !existing_root.is_empty() && existing_root != repo_root {
            return Err(format!(
                "worktree name \"{name}\" is already open for {existing_root} — choose another name"
            ));
        }
        let wt_path = str_field(&p, "worktree_path").unwrap_or_default();
        // Same repo, same name, but the caller named a DIFFERENT tree: attaching
        // would silently hand back the first worktree while the human believes
        // they opened the one they clicked. Two worktrees of one repo can share
        // a branch basename (`feature/x`, `hotfix/x`), so this is reachable.
        // Fail closed here rather than trusting every client to pick unique names.
        if let Some(asked) = req
            .worktree_path
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
        {
            if !same_path(asked, &wt_path) {
                return Err(format!(
                    "worktree name \"{name}\" is already open at {wt_path} — \
                     pick another name for {asked}"
                ));
            }
        }
        let owner = claim_ownership(state, &stream_id)?;
        let in_place = same_path(&wt_path, &existing_root);
        let created_by_bakery = p
            .get("created_by_bakery")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let context = ActiveContext {
            stream_id,
            name,
            project: str_field(&p, "project").unwrap_or(project),
            repo_root: existing_root,
            branch: str_field(&p, "branch").unwrap_or_default(),
            exists_on_disk: std::path::Path::new(&wt_path).is_dir(),
            in_place,
            worktree_path: wt_path,
            owner_node_id: owner,
            created_by_bakery,
        };
        return Ok(OpenResp {
            event: ev,
            context,
            attached: true,
        });
    }

    // ── create / adopt path ─────────────────────────────────────────────────
    // `created_by_bakery` is true in exactly one place below: the branch that
    // actually ran `add_worktree`. Everywhere else — an explicit path the
    // human picked, or a directory adopted because it was already there — is
    // provenance we don't have proof of, so it stays `false`. This is what
    // discard's ownership check reads instead of inferring "ours" from where
    // the path happens to sit (04-LIFECYCLE audit).
    // What a new worktree forks from, recorded so its changes are counted
    // against it (the sidebar's rollup).
    let mut base = req.base_branch.clone();
    // Why a new branch forked the local main instead of the remote's.
    let mut fork_note: Option<String> = None;
    let (wt_path, branch, created_by_bakery) = match req.worktree_path.as_deref().map(str::trim) {
        // Attach an existing worktree the human picked out of `git worktree list`.
        Some(existing) if !existing.is_empty() => {
            let picked = std::path::Path::new(existing);
            if !picked.is_dir() {
                return Err(format!("worktree path does not exist: {existing}"));
            }
            let sub = crate::git::inspect(existing);
            if sub.root.as_deref() != Some(repo_root.as_str()) {
                return Err(format!(
                    "{existing} is not a worktree of {repo_root} (resolved to {:?})",
                    sub.root
                ));
            }
            let branch = req
                .branch
                .clone()
                .or(sub.current_branch)
                .ok_or("worktree is detached — check out a branch first")?;
            (picked.to_path_buf(), branch, false)
        }
        _ => {
            let wt_path = state.worktree_base.join(&name);
            let explicit_branch = req
                .branch
                .as_deref()
                .map(str::trim)
                .filter(|b| !b.is_empty())
                .map(str::to_string);
            let (branch, created_by_bakery) = if wt_path.is_dir() {
                // Adopt a directory left by an earlier run rather than
                // re-running `git worktree add`, which would fail. No Heart
                // record exists yet for this stream_id (we're in the
                // create/adopt path, not the attach path above), so there is
                // no way to prove *this* node actually created it — treat it
                // the same as any other pre-existing directory.
                let existing = crate::git::inspect(&wt_path.to_string_lossy());
                if !existing.is_git {
                    return Err(format!(
                        "{} exists but is not a git worktree — remove it or pick another name",
                        wt_path.display()
                    ));
                }
                // The directory's real checked-out branch, not a freshly
                // suggested one — `suggest_branch` is only valid at creation
                // time; by adoption time its own branch already exists locally
                // and would no longer look "free" to a second suggestion.
                let branch = explicit_branch
                    .or(existing.current_branch)
                    .ok_or("adopted worktree is detached — check out a branch first")?;
                (branch, false)
            } else {
                let branch = explicit_branch.unwrap_or_else(|| {
                    // WHY: "bakery/" made every project's branch read as Bakery
                    // product branding instead of the user's own project.
                    // `suggest_branch` also avoids existing local branches so a
                    // fresh worktree never silently lands on an unrelated one
                    // (`add_worktree` checks out an existing branch by name
                    // rather than passing `-b` when it already exists).
                    crate::git::suggest_branch(&name, &info.local_branches)
                });
                // Rule 2: a new branch forks the newer main, just fetched,
                // unless the caller named its base (a helper: its creator's
                // HEAD).
                if base.is_none() && !info.local_branches.contains(&branch) {
                    let fork = crate::git::fresh_main(std::path::Path::new(&repo_root));
                    fork_note = fork.stale;
                    base = Some(fork.base);
                }
                add_worktree(
                    &repo_root,
                    &wt_path,
                    &branch,
                    base.as_deref(),
                    &info.local_branches,
                )?;
                (branch, true)
            };
            (wt_path, branch, created_by_bakery)
        }
    };

    // 07d-A3: opaque and server-minted — the name scanned for above is
    // display/dedup metadata now, never identity.
    let stream_id = crate::node::mint_id("socket");

    let payload = serde_json::json!({
        // `repo` kept for older readers; `repo_root` is the resolved truth.
        "repo": req.repo,
        "repo_root": repo_root,
        "project": project,
        "name": name,
        "worktree_path": wt_path.to_string_lossy(),
        "branch": branch,
        "opened_by_node": state.node.lock().unwrap().node_id.clone(),
        "created_by_bakery": created_by_bakery,
        "base_branch": base.or_else(|| info.default_branch.clone()).filter(|_| created_by_bakery),
        "fork_note": fork_note,
    })
    .to_string();

    let event = append(
        state,
        &stream_id,
        "human",
        "project.onboarded",
        payload,
        vec![],
    )?;
    let owner = claim_ownership(state, &stream_id)?;

    let worktree_path = wt_path.to_string_lossy().into_owned();
    let in_place = same_path(&worktree_path, &repo_root);
    let context = ActiveContext {
        stream_id,
        name,
        project,
        repo_root,
        branch,
        exists_on_disk: wt_path.is_dir(),
        in_place,
        worktree_path,
        owner_node_id: owner,
        created_by_bakery,
    };
    crate::api_tools::set_tools_at_creation(state, &context.stream_id, req.tools)?;
    Ok(OpenResp {
        event,
        context,
        attached: false,
    })
}

/// `git worktree add`, reusing an existing local branch instead of `-b` when the
/// branch already exists (re-`-b` would fail).
pub(crate) fn add_worktree(
    repo_root: &str,
    wt_path: &std::path::Path,
    branch: &str,
    base: Option<&str>,
    local_branches: &[String],
) -> Result<(), String> {
    let mut cmd = std::process::Command::new("git");
    cmd.arg("-C").arg(repo_root).arg("worktree").arg("add");
    if local_branches.iter().any(|b| b == branch) {
        cmd.arg(wt_path).arg(branch);
    } else {
        // A branch of its own: never one whose upstream is the remote main
        // it forked (a push would aim at main).
        cmd.arg("--no-track").arg("-b").arg(branch).arg(wt_path);
        if let Some(b) = base.filter(|b| !b.is_empty()) {
            cmd.arg(b);
        }
    }
    let out = cmd
        .output()
        .map_err(|e| format!("failed to run git: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git worktree add failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

/// `git worktree remove --force` — the discard-side counterpart to
/// `add_worktree`. `--force` because discard is an explicit "throw this
/// slice away" action; uncommitted scratch in a worktree the operator is discarding
/// on purpose must not block it. Callers are responsible for never pointing
/// this at an in-place checkout (`repo_root` itself) — this function does
/// not re-check that; it only runs the command it's told to. The worktree's
/// cargo target goes with the directory. The shared cargo home stays.
pub(crate) fn remove_worktree(state: &crate::state::AppState, repo_root: &str, wt_path: &std::path::Path) -> Result<(), String> {
    // The target's name is the checkout's path. Resolve it while the
    // directory is still there.
    let target = crate::builds::worktree_target(state, wt_path);
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .arg("worktree")
        .arg("remove")
        .arg("--force")
        .arg(wt_path)
        .output()
        .map_err(|e| format!("failed to run git: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git worktree remove failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    crate::builds::drop_worktree_target(state, &target);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::OWNERSHIP_FAMILY;
    use crate::testutil::*;
    use std::sync::Arc;

    // ─── A: open / attach / inspect / recents ────────────────────────────────

    #[test]
    fn open_creates_worktree_first_event_and_ownership() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));

        let resp = open_stream(&state, &open_req(&repo, "featureA")).expect("open should succeed");

        assert_eq!(resp.event.family, "project.onboarded");
        // 07d-A3: stream ids are opaque and server-minted — assert the shape,
        // not a name-derived literal.
        assert!(
            resp.event.stream_id.starts_with("socket_"),
            "got {}",
            resp.event.stream_id
        );
        assert_eq!(resp.event.stream_id, resp.context.stream_id);
        assert_eq!(resp.event.seq, 0);
        assert!(!resp.attached);
        // Plain name, not `bakery/`-prefixed: a linked worktree's branch must
        // read as the user's project, not as Bakery-product branding.
        assert_eq!(resp.context.branch, "featureA");
        assert!(resp.context.exists_on_disk);
        assert!(std::path::Path::new(&resp.context.worktree_path).is_dir());
        assert_eq!(resp.context.project, "repo");
        assert!(
            !resp.context.in_place,
            "a linked worktree is not the repo's own checkout"
        );

        // Ownership is claimed by this node, on the stream itself.
        let heart = state.heart.lock().unwrap();
        let claim = heart
            .last_of_family(&resp.context.stream_id, OWNERSHIP_FAMILY)
            .unwrap()
            .expect("opening locally must claim ownership");
        let p: serde_json::Value = serde_json::from_str(&claim.payload).unwrap();
        assert_eq!(p["node_id"], state.node.lock().unwrap().node_id);
        heart.verify_stream(&resp.context.stream_id).unwrap();
    }

    #[test]
    fn open_twice_attaches_without_recreating() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));

        let first = open_stream(&state, &open_req(&repo, "dup")).unwrap();
        let second = open_stream(&state, &open_req(&repo, "dup")).unwrap();

        assert_eq!(first.event.event_hash, second.event.event_hash);
        assert_eq!(
            first.context.stream_id, second.context.stream_id,
            "attach must return the same socket"
        );
        assert!(second.attached, "second open must attach");
        let onboarded = state
            .heart
            .lock()
            .unwrap()
            .read_stream(&first.context.stream_id)
            .unwrap()
            .into_iter()
            .filter(|e| e.family == "project.onboarded")
            .count();
        assert_eq!(
            onboarded, 1,
            "attach must not append a second onboarding event"
        );
    }

    #[test]
    fn reopen_after_daemon_restart_does_not_rerun_worktree_add() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));

        // First daemon: opens and materialises the worktree.
        let first = state_with(dir.path(), Arc::new(StubModel));
        let a = open_stream(&first, &open_req(&repo, "restart")).unwrap();
        assert!(std::path::Path::new(&a.context.worktree_path).is_dir());

        // Second daemon: empty Heart, worktree already on disk. `git worktree
        // add` would fail here, so adoption is what makes re-open work.
        let second = state_with(dir.path(), Arc::new(StubModel));
        let b = open_stream(&second, &open_req(&repo, "restart")).expect("must adopt existing dir");
        assert_eq!(b.context.worktree_path, a.context.worktree_path);
        assert!(!b.attached, "fresh Heart means a new onboarding event");
    }

    #[test]
    fn same_name_for_a_different_repo_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let repo_a = make_repo(&dir.path().join("a"));
        let repo_b = make_repo(&dir.path().join("b"));
        let state = state_with(dir.path(), Arc::new(StubModel));

        open_stream(&state, &open_req(&repo_a, "shared")).unwrap();
        let err = open_stream(&state, &open_req(&repo_b, "shared")).unwrap_err();
        assert!(err.contains("already open for"), "got: {err}");
    }

    #[test]
    fn open_without_a_name_suggests_one_from_the_repo() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("myproj"));
        let state = state_with(dir.path(), Arc::new(StubModel));

        let first = open_stream(
            &state,
            &OpenReq {
                repo: repo.to_string_lossy().into_owned(),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(first.context.name, "myproj");

        // A second unnamed open of the same repo must not collide.
        let second = open_stream(
            &state,
            &OpenReq {
                repo: repo.to_string_lossy().into_owned(),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(second.context.name, "myproj-2");
        assert!(!second.attached);
    }

    #[test]
    fn open_attaches_an_existing_worktree_path_without_adding_one() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        // A worktree created outside Bakery, the way a user already works.
        let outside = dir.path().join("manual-wt");
        run_git(
            &repo,
            &["worktree", "add", "-b", "manual", outside.to_str().unwrap()],
        );

        let state = state_with(dir.path(), Arc::new(StubModel));
        let resp = open_stream(
            &state,
            &OpenReq {
                repo: repo.to_string_lossy().into_owned(),
                name: Some("manual".into()),
                worktree_path: Some(outside.to_string_lossy().into_owned()),
                ..Default::default()
            },
        )
        .expect("attaching an existing worktree must work");

        assert_eq!(resp.context.branch, "manual");
        assert_eq!(
            std::path::Path::new(&resp.context.worktree_path)
                .canonicalize()
                .unwrap(),
            outside.canonicalize().unwrap()
        );
        // Nothing was materialised under the bakery worktree base.
        assert!(!state.worktree_base.join("manual").exists());
    }

    #[test]
    fn attaching_a_second_tree_under_a_taken_name_fails_closed() {
        // Same repo, same name, different tree: silently returning the first
        // worktree would open something other than what the human clicked.
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let a = dir.path().join("tree-a");
        let b = dir.path().join("tree-b");
        run_git(
            &repo,
            &["worktree", "add", "-b", "feature/x", a.to_str().unwrap()],
        );
        run_git(
            &repo,
            &["worktree", "add", "-b", "hotfix/x", b.to_str().unwrap()],
        );

        let state = state_with(dir.path(), Arc::new(StubModel));
        let attach = |path: &std::path::Path| OpenReq {
            repo: repo.to_string_lossy().into_owned(),
            name: Some("x".into()), // both branches share the basename "x"
            worktree_path: Some(path.to_string_lossy().into_owned()),
            ..Default::default()
        };
        let first = open_stream(&state, &attach(&a)).unwrap();
        assert_eq!(
            std::path::Path::new(&first.context.worktree_path)
                .canonicalize()
                .unwrap(),
            a.canonicalize().unwrap()
        );

        let err = open_stream(&state, &attach(&b)).unwrap_err();
        assert!(err.contains("already open at"), "got: {err}");

        // Re-attaching the SAME tree under that name is still fine.
        let again = open_stream(&state, &attach(&a)).unwrap();
        assert!(again.attached);
    }

    #[test]
    fn attaching_a_worktree_of_another_repo_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let repo_a = make_repo(&dir.path().join("a"));
        let repo_b = make_repo(&dir.path().join("b"));
        let state = state_with(dir.path(), Arc::new(StubModel));

        let err = open_stream(
            &state,
            &OpenReq {
                repo: repo_a.to_string_lossy().into_owned(),
                name: Some("wrong".into()),
                worktree_path: Some(repo_b.to_string_lossy().into_owned()),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(err.contains("is not a worktree of"), "got: {err}");
    }

    #[test]
    fn new_worktree_branch_is_the_plain_name_not_bakery_prefixed() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("widget"));
        let state = state_with(dir.path(), Arc::new(StubModel));

        let resp = open_stream(&state, &open_req(&repo, "widget")).unwrap();

        assert_eq!(resp.context.branch, "widget");
        assert!(
            !resp.context.branch.starts_with("bakery/"),
            "the bakery/ prefix reads as Bakery-product branding, not the user's project"
        );
    }

    #[test]
    fn open_in_place_attaches_the_repos_own_checkout() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        run_git(&repo, &["checkout", "-q", "-b", "topic"]);
        let state = state_with(dir.path(), Arc::new(StubModel));

        // "Open folder as worktree (attach in place)": worktree_path == repo.
        let resp = open_stream(
            &state,
            &OpenReq {
                repo: repo.to_string_lossy().into_owned(),
                name: Some("repo".into()),
                worktree_path: Some(repo.to_string_lossy().into_owned()),
                ..Default::default()
            },
        )
        .expect("opening the repo's own checkout in place must work");

        assert!(
            resp.context.in_place,
            "worktree_path == repo_root must be in_place"
        );
        assert_eq!(
            resp.context.branch, "topic",
            "records the repo's real current branch"
        );
        assert_eq!(
            std::path::Path::new(&resp.context.worktree_path)
                .canonicalize()
                .unwrap(),
            repo.canonicalize().unwrap()
        );
        // Nothing was materialised under the bakery worktree base.
        assert!(!state.worktree_base.join("repo").exists());
    }

    #[test]
    fn a_normal_linked_worktree_is_not_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));

        let resp = open_stream(&state, &open_req(&repo, "featureB")).unwrap();
        assert!(!resp.context.in_place);
    }

    #[test]
    fn open_rejects_a_non_git_folder() {
        let dir = tempfile::tempdir().unwrap();
        let plain = dir.path().join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        assert!(open_stream(&state, &open_req(&plain, "nope")).is_err());
    }

    #[test]
    fn inspect_reports_branches_default_and_worktrees() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        run_git(&repo, &["branch", "feature-x"]);

        let info = crate::git::inspect(repo.to_str().unwrap());
        assert!(info.is_git);
        assert_eq!(info.name.as_deref(), Some("repo"));
        assert_eq!(info.current_branch.as_deref(), Some("main"));
        assert_eq!(info.default_branch.as_deref(), Some("main"));
        assert!(info.local_branches.contains(&"feature-x".to_string()));
        assert_eq!(info.worktrees.len(), 1);
        assert!(info.worktrees[0].is_main);

        // After opening, the new worktree shows up in the same listing.
        let state = state_with(dir.path(), Arc::new(StubModel));
        open_stream(&state, &open_req(&repo, "wt1")).unwrap();
        let after = crate::git::inspect(repo.to_str().unwrap());
        assert_eq!(after.worktrees.len(), 2);
        assert!(after
            .worktrees
            .iter()
            .any(|w| w.branch.as_deref() == Some("wt1")));
    }

    #[test]
    fn recents_list_streams_for_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let opened_one = open_stream(&state, &open_req(&repo, "one")).unwrap();
        open_stream(&state, &open_req(&repo, "two")).unwrap();

        let recents = known_streams(&state).unwrap();
        assert_eq!(recents.len(), 2);
        let one = recents.iter().find(|s| s.name == "one").unwrap();
        assert_eq!(one.stream_id, opened_one.context.stream_id);
        assert_eq!(one.project, "repo");
        assert_eq!(one.branch, "one");
        assert!(one.exists_on_disk);
        assert!(
            !one.in_place,
            "a linked worktree carries in_place == false in recents"
        );
        let expected_node_id = state.node.lock().unwrap().node_id.clone();
        assert_eq!(
            one.owner_node_id.as_deref(),
            Some(expected_node_id.as_str())
        );
    }

    #[test]
    fn recents_flag_a_worktree_that_vanished_from_disk() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let resp = open_stream(&state, &open_req(&repo, "gone")).unwrap();
        std::fs::remove_dir_all(&resp.context.worktree_path).unwrap();

        let recents = known_streams(&state).unwrap();
        assert!(
            !recents[0].exists_on_disk,
            "a deleted worktree must not look openable"
        );
    }

    #[test]
    fn recents_distinguish_in_place_from_linked_worktrees() {
        let dir = tempfile::tempdir().unwrap();
        let repo_a = make_repo(&dir.path().join("a"));
        let repo_b = make_repo(&dir.path().join("b"));
        let state = state_with(dir.path(), Arc::new(StubModel));

        open_stream(
            &state,
            &OpenReq {
                repo: repo_a.to_string_lossy().into_owned(),
                name: Some("a".into()),
                worktree_path: Some(repo_a.to_string_lossy().into_owned()),
                ..Default::default()
            },
        )
        .unwrap();
        open_stream(&state, &open_req(&repo_b, "b")).unwrap();

        let recents = known_streams(&state).unwrap();
        let a = recents.iter().find(|s| s.name == "a").unwrap();
        let b = recents.iter().find(|s| s.name == "b").unwrap();
        assert!(
            a.in_place,
            "recents must carry in_place == true for the repo's own checkout"
        );
        assert!(
            !b.in_place,
            "recents must carry in_place == false for a linked worktree"
        );
    }

    // ─── C: read-only file endpoints ─────────────────────────────────────────

    #[test]
    fn files_list_inner_lists_the_worktree_root_and_refuses_outside_paths() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id = open_stream(&state, &open_req(&repo, "browse"))
            .unwrap()
            .context
            .stream_id;

        let entries = files_list_inner(&state, &id, ".").unwrap();
        assert!(
            entries.iter().any(|e| e.name == "f.txt" && !e.is_dir),
            "{entries:?}"
        );

        let err = files_list_inner(&state, &id, "../../etc").unwrap_err();
        assert!(err.contains("escapes"), "got: {err}");
    }

    #[test]
    fn files_read_inner_returns_file_text_and_a_matching_digest() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id = open_stream(&state, &open_req(&repo, "readback"))
            .unwrap()
            .context
            .stream_id;

        let (text, digest) = files_read_inner(&state, &id, "f.txt").unwrap();
        assert_eq!(text, "line one\n");
        assert_eq!(digest, crate::tools::digest_hex(text.as_bytes()));
    }

    /// A file on the always-blocked list comes back marked secret, so the
    /// app takes no screenshot while it shows it; an ordinary one doesn't.
    #[tokio::test]
    async fn a_blocked_files_read_is_marked_secret() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let ctx = open_stream(&state, &open_req(&repo, "secretread")).unwrap().context;
        let private = std::path::Path::new(&ctx.worktree_path).join("private.txt");
        std::fs::write(&private, "mine\n").unwrap();
        let blocked = private.canonicalize().unwrap().display().to_string();
        state.node.lock().unwrap().machine_access.deny = vec![blocked];
        let read = |path: &str| files_read(State(state.clone()), Query(FilesQuery { stream_id: ctx.stream_id.clone(), path: Some(path.into()) }));
        assert!(read("private.txt").await.unwrap().secret);
        assert!(!read("f.txt").await.unwrap().secret);
        // An agent's read of it is refused: the list limits agents only.
        let agent = crate::actor::scope("agent:orchestrator".into(), read("private.txt"));
        let (_, why) = agent.await.err().unwrap();
        assert!(why.contains("always blocked"), "{why}");
    }

    // ─── D: human save (files_write) ─────────────────────────────────────────

    fn write_req(
        stream_id: &str,
        path: &str,
        text: &str,
        expected_digest: Option<&str>,
    ) -> FileWriteReq {
        FileWriteReq {
            stream_id: stream_id.into(),
            path: path.into(),
            text: text.into(),
            expected_digest: expected_digest.map(String::from),
        }
    }

    /// Rule 2: a new conversation's worktree forks the newer main, just
    /// fetched (here origin's, which is ahead), on a branch of its own that
    /// doesn't track that main.
    #[test]
    fn a_new_conversation_forks_the_newer_main_just_fetched() {
        let dir = tempfile::tempdir().unwrap();
        let local = crate::git::tests::behind_its_origin(dir.path());
        let state = state_with(dir.path(), Arc::new(StubModel));
        let opened = open_stream(&state, &open_req(&local, "fresh")).unwrap();
        let wt = std::path::Path::new(&opened.context.worktree_path);
        assert!(wt.join("only-on-origin.txt").is_file(), "it starts from what origin has now");
        let p: serde_json::Value = serde_json::from_str(&opened.event.payload).unwrap();
        assert_eq!((p["base_branch"].as_str(), p["fork_note"].is_null()), (Some("origin/main"), true), "{p}");
        let upstream = crate::git::run(wt, &["rev-parse", "--abbrev-ref", "@{upstream}"]);
        assert!(upstream.is_err(), "no upstream, so a push never aims at main: {upstream:?}");
    }

    /// The operator's save is the operator's in History, with both sides; a
    /// file on the deny list is named by size only; a file that isn't text
    /// isn't saved here.
    #[tokio::test]
    async fn files_write_records_the_operators_edit_and_saves_text_only() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let ctx = open_stream(&state, &open_req(&repo, "mine")).unwrap().context;
        let (_text, digest) = files_read_inner(&state, &ctx.stream_id, "f.txt").unwrap();
        let save = |path: &str, text: &str, digest: Option<String>| {
            files_write(State(state.clone()), Json(write_req(&ctx.stream_id, path, text, digest.as_deref())))
        };
        assert!(matches!(save("f.txt", "line one\nmine\n", Some(digest)).await.unwrap().0, FileWriteResp::Ok { .. }));
        let edits = |state: &AppState| {
            state.heart.lock().unwrap().events_of_family(&ctx.stream_id, OPERATOR_EDIT_FAMILY).unwrap()
        };
        let first = &edits(&state)[0];
        assert_eq!(first.actor, "human");
        let p: serde_json::Value = serde_json::from_str(&first.payload).unwrap();
        assert_eq!((p["old_content"].as_str(), p["new_content"].as_str()), (Some("line one\n"), Some("line one\nmine\n")));

        let secret = std::path::Path::new(&ctx.worktree_path).join("secret");
        std::fs::create_dir_all(&secret).unwrap();
        state.node.lock().unwrap().machine_access.deny.push(secret.to_string_lossy().into_owned());
        assert!(matches!(save("secret/key.txt", "s3cr3t\n", None).await.unwrap().0, FileWriteResp::Ok { .. }));
        let hidden: serde_json::Value = serde_json::from_str(&edits(&state)[1].payload).unwrap();
        assert_eq!((hidden["hidden"].as_str(), hidden["new_size"].as_u64()), (Some("denied"), Some(7)));
        assert!(hidden["new_content"].is_null() && !edits(&state)[1].payload.contains("s3cr3t"), "{hidden}");

        std::fs::write(std::path::Path::new(&ctx.worktree_path).join("logo.bin"), b"\x89PNG\0\0").unwrap();
        let err = save("logo.bin", "text over an image\n", None).await.unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST, "{}", err.1);
        assert_eq!(edits(&state).len(), 2, "nothing recorded for a refused save");
    }

    #[tokio::test]
    async fn files_write_saves_new_bytes_when_the_digest_matches() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let ctx = open_stream(&state, &open_req(&repo, "save"))
            .unwrap()
            .context;
        let (_text, digest) = files_read_inner(&state, &ctx.stream_id, "f.txt").unwrap();

        let resp = files_write(
            State(state.clone()),
            Json(write_req(
                &ctx.stream_id,
                "f.txt",
                "new bytes\n",
                Some(&digest),
            )),
        )
        .await
        .unwrap()
        .0;
        match resp {
            FileWriteResp::Ok {
                digest: new_digest, ..
            } => {
                assert_eq!(new_digest, crate::tools::digest_hex(b"new bytes\n"));
            }
            FileWriteResp::Conflict { .. } => panic!("expected a clean save"),
        }
        assert_eq!(
            std::fs::read_to_string(std::path::Path::new(&ctx.worktree_path).join("f.txt"))
                .unwrap(),
            "new bytes\n"
        );
    }

    #[tokio::test]
    async fn files_write_refuses_a_save_over_a_change_it_never_saw() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let ctx = open_stream(&state, &open_req(&repo, "conflict"))
            .unwrap()
            .context;
        let (_text, stale_digest) = files_read_inner(&state, &ctx.stream_id, "f.txt").unwrap();

        // Something else (an agent accept, a git pull, another tab) changes the file.
        std::fs::write(
            std::path::Path::new(&ctx.worktree_path).join("f.txt"),
            "changed out from under the operator\n",
        )
        .unwrap();

        let resp = files_write(
            State(state.clone()),
            Json(write_req(
                &ctx.stream_id,
                "f.txt",
                "The operator's edit\n",
                Some(&stale_digest),
            )),
        )
        .await
        .unwrap()
        .0;
        match resp {
            FileWriteResp::Conflict { current_text, .. } => {
                assert_eq!(
                    current_text.as_deref(),
                    Some("changed out from under the operator\n")
                );
            }
            FileWriteResp::Ok { .. } => panic!("a stale save must never win silently"),
        }
        // Neither side was overwritten.
        assert_eq!(
            std::fs::read_to_string(std::path::Path::new(&ctx.worktree_path).join("f.txt"))
                .unwrap(),
            "changed out from under the operator\n"
        );
    }

    #[tokio::test]
    async fn files_write_refuses_a_discarded_stream() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id = open_stream(&state, &open_req(&repo, "gone"))
            .unwrap()
            .context
            .stream_id;
        let _ = crate::routes::lifecycle::discard_stream(
            State(state.clone()),
            Json(crate::routes::lifecycle::DiscardReq {
                stream_id: id.clone(),
                confirm: false,
            }),
        )
        .await
        .unwrap();

        let err = files_write(
            State(state.clone()),
            Json(write_req(&id, "f.txt", "x", None)),
        )
        .await
        .unwrap_err();
        assert!(err.1.contains("discarded"), "{err:?}");
    }

    /// 04b-FENCE §4: a discarded stream's worktree may already be gone —
    /// browsing it must fail with "discarded", not a raw filesystem error.
    #[tokio::test]
    async fn files_list_and_read_refuse_a_discarded_stream() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id = open_stream(&state, &open_req(&repo, "browsed"))
            .unwrap()
            .context
            .stream_id;

        let _ = crate::routes::lifecycle::discard_stream(
            State(state.clone()),
            Json(crate::routes::lifecycle::DiscardReq {
                stream_id: id.clone(),
                confirm: false,
            }),
        )
        .await
        .unwrap();

        let err = files_list_inner(&state, &id, ".").unwrap_err();
        assert!(err.contains("discarded"), "{err}");
        let err = files_read_inner(&state, &id, "f.txt").unwrap_err();
        assert!(err.contains("discarded"), "{err}");
    }
}
