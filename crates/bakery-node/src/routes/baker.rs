//! Baker assignment (`/baker/assign`) and the Baker work queue routes
//! (`/baker/work`, `/baker/work/detail`, `/baker/push`, `/baker/pr`, `/baker/merge`).

use crate::node::WriteMode;
use crate::routes::agent::baker_ops_for_turn;
use crate::routes::operator::gates_inner;
use crate::routes::project::{open_stream, OpenReq};
use crate::state::{
    append, context_for, known_streams, same_path, ActiveContext, AppState, StreamRef,
};
use axum::{
    extract::{Query, State},
    http::StatusCode,
    Json,
};
use bakery_core::Event;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// ─── Baker assign (NODE_API §8) ──────────────────────────────────────────────

pub(crate) const BAKER_ASSIGN_FAMILY: &str = "baker.assignment.accepted";

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct BakerAssignReq {
    /// Human brief for this factory assignment. Required; drives the slug.
    pub(crate) brief: String,
}

/// Dedicated Baker session: linked worktree on `baker/<slug>`, never in-place.
#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct BakerAssignResp {
    /// `baker.assignment.accepted` — the assignment record for this call.
    #[schemars(with = "crate::api_catalog::EventShape")]
    pub(crate) event: Event,
    pub(crate) context: ActiveContext,
    /// True when the Baker stream already existed (worktree reused).
    pub(crate) attached: bool,
    /// Node write gate. Default is Manual; first dogfood keeps propose→accept.
    pub(crate) write_mode: WriteMode,
    pub(crate) brief: String,
    pub(crate) bakery_source_root: String,
}

/// Write `bakery_source_root` to `node.json` when it changes so the next
/// daemon start does not require `export BAKERY_SOURCE_ROOT=…`.
pub(crate) fn persist_bakery_source_root(state: &AppState, root: &std::path::Path) {
    let s = root.to_string_lossy().into_owned();
    let mut node = state.node.lock().unwrap();
    if node.bakery_source_root.as_deref() == Some(s.as_str()) {
        return;
    }
    node.bakery_source_root = Some(s);
    if let Err(e) = node.save(state.base.as_path()) {
        eprintln!("bakery-node: could not persist bakery_source_root: {e}");
    }
}

/// Resolve The Bakery repo the Baker is allowed to work on.
///
/// Order: `env_root` (operator `BAKERY_SOURCE_ROOT` when callers pass it) →
/// `node.json` `bakery_source_root` → walk up from `detect_from` for
/// `VISION.md` + `crates/bakery-node`.
///
/// **Do not read process env inside this function** — tests pass `env_root: None`
/// so a developer shell with `BAKERY_SOURCE_ROOT` set cannot hijack tempdir
/// fixtures or leave prunable worktrees on a real checkout. HTTP handlers pass
/// `std::env::var("BAKERY_SOURCE_ROOT").ok().as_deref()`.
///
/// Env and detect paths are **persisted** to `node.json` on success.
pub(crate) fn resolve_bakery_source_root(
    state: &AppState,
    env_root: Option<&str>,
    detect_from: Option<&std::path::Path>,
) -> Result<PathBuf, String> {
    if let Some(p) = env_root.map(str::trim).filter(|s| !s.is_empty()) {
        let path = PathBuf::from(p);
        persist_bakery_source_root(state, &path);
        return Ok(path);
    }
    {
        let node = state.node.lock().unwrap();
        if let Some(p) = node
            .bakery_source_root
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return Ok(PathBuf::from(p));
        }
    }
    if let Some(from) = detect_from {
        if let Some(found) = detect_bakery_source_root(from) {
            persist_bakery_source_root(state, &found);
            return Ok(found);
        }
    }
    Err(
        "Bakery source root unknown. Start bakery-node from The Bakery repo, or set \
         BAKERY_SOURCE_ROOT once (it is saved to ~/.bakery/node.json), or set \
         bakery_source_root in node.json (directory with VISION.md and crates/bakery-node)."
            .into(),
    )
}

/// Operator env for Baker root — only for live HTTP/startup, never tests.
pub(crate) fn bakery_source_root_from_env() -> Option<String> {
    std::env::var("BAKERY_SOURCE_ROOT")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Walk parents looking for the Bakery monorepo markers.
pub(crate) fn detect_bakery_source_root(from: &std::path::Path) -> Option<PathBuf> {
    let mut cur = from.to_path_buf();
    for _ in 0..12 {
        if cur.join("VISION.md").is_file() && cur.join("crates").join("bakery-node").is_dir() {
            return Some(cur);
        }
        if !cur.pop() {
            break;
        }
    }
    None
}

/// First line of the brief → boring slug (directory / branch suffix safe).
pub(crate) fn baker_slug(brief: &str) -> String {
    let line = brief.lines().next().unwrap_or(brief).trim();
    let clipped: String = line.chars().take(48).collect();
    crate::git::sanitize_name(&clipped)
}

/// Open or reuse a Baker session: linked worktree under `worktree_base`, branch
/// `baker/<slug>`, stream `wt:baker-<slug>`. Never in-place on the primary
/// checkout. Appends `baker.assignment.accepted` every call.
pub(crate) fn baker_assign_inner(
    state: &AppState,
    brief: &str,
    env_root: Option<&str>,
    detect_from: Option<&std::path::Path>,
) -> Result<BakerAssignResp, String> {
    let brief = brief.trim();
    if brief.is_empty() {
        return Err("brief must not be empty".into());
    }

    let root = resolve_bakery_source_root(state, env_root, detect_from)?;
    if !root.is_dir() {
        return Err(format!(
            "Bakery source root does not exist: {}",
            root.display()
        ));
    }
    let root_str = root.to_string_lossy().into_owned();
    let info = crate::git::inspect(&root_str);
    if !info.is_git {
        return Err(format!(
            "Bakery source root is not a git repository: {}{}",
            root.display(),
            info.error.map(|e| format!(" ({e})")).unwrap_or_default()
        ));
    }
    let repo_root = info
        .root
        .ok_or_else(|| "could not resolve bakery repository root".to_string())?;

    let slug = baker_slug(brief);
    let name = format!("baker-{slug}");
    let branch = format!("baker/{slug}");

    // Planned path must never be the primary checkout of the factory repo.
    let planned_wt = state.worktree_base.join(&name);
    if same_path(planned_wt.to_string_lossy().as_ref(), &repo_root) {
        return Err(
            "Baker refuses a worktree path that is the primary bakery checkout — \
             worktrees must live under the node worktree base (~/.bakery/worktrees)"
                .into(),
        );
    }

    let open = open_stream(
        state,
        &OpenReq {
            repo: repo_root.clone(),
            name: Some(name),
            branch: Some(branch),
            base_branch: info.default_branch.clone(),
            worktree_path: None, // never attach primary checkout
            project_id: None,    // Baker jobs are factory-kind, never Project-linked
            tools: None,
        },
    )?;

    if open.context.in_place {
        return Err("Baker refuses in-place sessions on the primary checkout — \
             assignment must use a linked worktree under the node worktree base"
            .into());
    }
    if !open.context.branch.starts_with("baker/") {
        return Err(format!(
            "Baker stream branch must start with baker/, got {:?}",
            open.context.branch
        ));
    }

    let write_mode = state.node.lock().unwrap().write_mode;
    // Measured factory ops at assign time (same fields later injected into turns).
    // Stored on the event for audit — not invented metrics.
    // A `/baker/assign` branch-job socket is never the permanent Baker
    // (07d-B Phase 2 item 2) — it gets the measured ops text but no op
    // registry line.
    let ops_text = baker_ops_for_turn(
        state,
        &open.context.stream_id,
        &open.context.branch,
        false,
        false,
    );
    let payload = serde_json::json!({
        "brief": brief,
        "stream_id": open.context.stream_id,
        "branch": open.context.branch,
        "worktree_path": open.context.worktree_path,
        "repo_root": open.context.repo_root,
        "slug": slug,
        "attached": open.attached,
        "write_mode": write_mode,
        "ops": {
            "node_live": state.node.lock().unwrap().node_id.clone(),
            "write_mode": match write_mode {
                WriteMode::Manual => "manual",
                WriteMode::Rules => "rules",
                WriteMode::Auto => "auto",
            },
            "stream_count": state.heart.lock().unwrap().list_streams().map(|s| s.len()).unwrap_or(0),
            "pending_gates": gates_inner(state).map(|g| g.len()).unwrap_or(0),
            "baker_branch": open.context.branch,
            "text": ops_text,
        },
    })
    .to_string();

    let event = append(
        state,
        &open.context.stream_id,
        "human",
        BAKER_ASSIGN_FAMILY,
        payload,
        vec![open.event.event_hash.clone()],
    )?;

    // The durable fact that gives this stream factory authority (07d-A1
    // correction: this used to be inferred from the branch/name/stream_id
    // matching a `baker-`/`baker/` convention, so any ordinary project a
    // human happened to name that way silently inherited the Baker
    // boundary). Recorded only here and in `ensure_permanent_baker`
    // (07d-B) — every other route only reads it. Required, not
    // best-effort (07d-A1 correction #2): a caller must never see this
    // call report success while the stream is left classified as an
    // ordinary project — `baker.assignment.accepted` already landed and
    // stays on the stream either way, but the whole call now fails if this
    // second append does not, so the human retries rather than silently
    // dogfooding a misclassified Baker socket.
    append(
        state,
        &open.context.stream_id,
        "human",
        crate::node::SOCKET_KIND_FAMILY,
        serde_json::to_string(&crate::node::SocketKindRecord {
            kind: crate::node::SocketKind::Factory,
        })
        .unwrap(),
        vec![event.event_hash.clone()],
    )?;

    Ok(BakerAssignResp {
        event,
        context: open.context,
        attached: open.attached,
        write_mode,
        brief: brief.to_string(),
        bakery_source_root: repo_root,
    })
}

pub(crate) async fn baker_assign(
    State(st): State<AppState>,
    Json(req): Json<BakerAssignReq>,
) -> Result<Json<BakerAssignResp>, (StatusCode, String)> {
    let detect = std::env::current_dir().ok();
    let env_root = bakery_source_root_from_env();
    // git worktree add is blocking; stay off the async workers.
    let st2 = st.clone();
    let brief = req.brief;
    crate::actor::spawn_blocking(move || {
        baker_assign_inner(&st2, &brief, env_root.as_deref(), detect.as_deref())
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map(Json)
    .map_err(|e| (StatusCode::BAD_REQUEST, e))
}

// ─── Baker work middleman (B2): list / review / land baker/* ─────────────────

/// One baker/* branch, optionally joined to a Heart stream.
#[derive(schemars::JsonSchema, Debug, Clone, Serialize)]
pub(crate) struct BakerWorkItem {
    pub(crate) branch: String,
    pub(crate) stream_id: Option<String>,
    pub(crate) worktree_path: Option<String>,
    pub(crate) worktree_exists: bool,
    pub(crate) brief: Option<String>,
    /// True when this branch's most recent known stream has been discarded —
    /// distinct from `stream_id.is_none()` alone, which is also true for a
    /// branch that never had any stream at all. 07c-LIVE-STATE: lets the
    /// client tell the two apart without re-deriving repo-root equality
    /// itself (see the comment at this field's call site in
    /// `baker_work_list_inner`).
    pub(crate) discarded: bool,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct BakerWorkList {
    pub(crate) bakery_source_root: String,
    /// Default branch used as the merge base (e.g. main).
    pub(crate) base: Option<String>,
    pub(crate) items: Vec<BakerWorkItem>,
    /// Set when listing git branches failed — items may still include streams.
    pub(crate) git_error: Option<String>,
    pub(crate) has_origin: bool,
    pub(crate) gh_available: bool,
}

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct BakerWorkDetailQuery {
    /// `baker/<slug>` — preferred selector.
    #[serde(default)]
    pub(crate) branch: Option<String>,
    /// Alternative: resolve branch from stream onboard payload.
    #[serde(default)]
    pub(crate) stream_id: Option<String>,
}

/// Status + commits + diff vs base for one baker branch. No invented ahead/behind.
#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct BakerWorkDetail {
    pub(crate) branch: String,
    pub(crate) base: Option<String>,
    pub(crate) stream_id: Option<String>,
    pub(crate) worktree_path: Option<String>,
    pub(crate) worktree_exists: bool,
    pub(crate) brief: Option<String>,
    /// Commits on branch not reachable from base. Empty when none or git error.
    pub(crate) commits: Vec<crate::git::CommitSummary>,
    pub(crate) commits_error: Option<String>,
    /// `git diff --stat base...branch`.
    pub(crate) diff_stat: Option<String>,
    pub(crate) diff_error: Option<String>,
    /// Uncommitted files in the linked worktree (if it exists on disk).
    pub(crate) status: Vec<crate::git::StatusEntry>,
    pub(crate) status_error: Option<String>,
    pub(crate) has_origin: bool,
    pub(crate) gh_available: bool,
    pub(crate) bakery_source_root: String,
}

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct BakerBranchReq {
    pub(crate) branch: String,
}

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct BakerPrReq {
    pub(crate) branch: String,
    #[serde(default)]
    pub(crate) title: Option<String>,
    #[serde(default)]
    pub(crate) body: Option<String>,
}

/// Merge requires an explicit human `confirm: true` — never agent-clickable text alone.
#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct BakerMergeReq {
    pub(crate) branch: String,
    /// Must be true. Missing/false → refuse.
    #[serde(default)]
    pub(crate) confirm: bool,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct BakerActionResp {
    pub(crate) ok: bool,
    pub(crate) message: String,
    pub(crate) branch: String,
}

pub(crate) fn baker_repo_ctx(
    state: &AppState,
    env_root: Option<&str>,
    detect_from: Option<&std::path::Path>,
) -> Result<(PathBuf, String, Option<String>), String> {
    let root = resolve_bakery_source_root(state, env_root, detect_from)?;
    if !root.is_dir() {
        return Err(format!(
            "Bakery source root does not exist: {}",
            root.display()
        ));
    }
    let root_str = root.to_string_lossy().into_owned();
    let info = crate::git::inspect(&root_str);
    if !info.is_git {
        return Err(format!(
            "Bakery source root is not a git repository: {}{}",
            root.display(),
            info.error.map(|e| format!(" ({e})")).unwrap_or_default()
        ));
    }
    let repo_root = info
        .root
        .ok_or_else(|| "could not resolve bakery repository root".to_string())?;
    Ok((PathBuf::from(&repo_root), repo_root, info.default_branch))
}

/// Heart streams for the bakery repo whose branch is baker/* (or name baker-*).
pub(crate) fn baker_streams_for_repo(
    state: &AppState,
    repo_root: &str,
) -> Result<Vec<StreamRef>, String> {
    let known = known_streams(state)?;
    Ok(known
        .into_iter()
        .filter(|s| same_path(&s.repo_root, repo_root))
        .filter(|s| crate::git::is_baker_branch(&s.branch) || s.name.starts_with("baker-"))
        .collect())
}

pub(crate) fn latest_baker_brief(state: &AppState, stream_id: &str) -> Option<String> {
    let heart = state.heart.lock().unwrap();
    let events = heart.read_stream(stream_id).ok()?;
    events
        .into_iter()
        .rev()
        .find(|e| e.family == BAKER_ASSIGN_FAMILY)
        .and_then(|e| {
            let p: serde_json::Value = serde_json::from_str(&e.payload).ok()?;
            p.get("brief")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        })
}

pub(crate) fn baker_work_list_inner(
    state: &AppState,
    env_root: Option<&str>,
    detect_from: Option<&std::path::Path>,
) -> Result<BakerWorkList, String> {
    let (repo_path, repo_root, base) = baker_repo_ctx(state, env_root, detect_from)?;
    let has_origin = crate::git::has_remote(&repo_path, "origin");
    let gh_available = crate::git::gh_available();

    let mut git_error = None;
    let mut branch_set: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    match crate::git::baker_local_branches(&repo_path) {
        Ok(bs) => {
            for b in bs {
                branch_set.insert(b);
            }
        }
        Err(e) => git_error = Some(e),
    }

    // Discarded streams must not surface as open Baker work (07c-STUDIO-SHELL
    // §4): their worktree is already gone, so linking one back in here would
    // draw a "still running" row for retired work. The branch itself (if git
    // still has it) still appears as an orphan, unlinked row — but tagged
    // `discarded` (07c-LIVE-STATE) so the client can tell "never had a
    // stream" apart from "had one, now retired" without redoing a repo_root
    // comparison itself: `baker_streams_for_repo` already canonicalizes that
    // via `same_path` below, and the client has no filesystem access to do
    // the equivalent safely (a browser can't resolve symlinks) — a raw `==`
    // there would silently reintroduce exactly the `/var` vs `/private/var`
    // class of bug `same_path` exists to avoid.
    let all_streams = baker_streams_for_repo(state, &repo_root).unwrap_or_default();
    let mut by_branch: std::collections::BTreeMap<String, StreamRef> =
        std::collections::BTreeMap::new();
    let mut discarded_branches: std::collections::BTreeSet<String> =
        std::collections::BTreeSet::new();
    for s in all_streams {
        let br = if crate::git::is_baker_branch(&s.branch) {
            s.branch.clone()
        } else if let Some(slug) = s.name.strip_prefix("baker-") {
            format!("baker/{slug}")
        } else {
            continue;
        };
        branch_set.insert(br.clone());
        if s.discarded {
            discarded_branches.insert(br);
        } else {
            by_branch.entry(br).or_insert(s);
        }
    }

    let mut items = Vec::new();
    for branch in branch_set {
        let stream = by_branch.get(&branch);
        let worktree_path = stream.map(|s| s.worktree_path.clone());
        let worktree_exists = worktree_path
            .as_ref()
            .map(|p| std::path::Path::new(p).is_dir())
            .unwrap_or(false);
        let stream_id = stream.map(|s| s.stream_id.clone());
        let brief = stream_id
            .as_deref()
            .and_then(|id| latest_baker_brief(state, id));
        let discarded = discarded_branches.contains(&branch);
        items.push(BakerWorkItem {
            branch,
            stream_id,
            worktree_path,
            worktree_exists,
            brief,
            discarded,
        });
    }

    Ok(BakerWorkList {
        bakery_source_root: repo_root,
        base,
        items,
        git_error,
        has_origin,
        gh_available,
    })
}

pub(crate) fn resolve_baker_branch(
    state: &AppState,
    repo_root: &str,
    branch: Option<&str>,
    stream_id: Option<&str>,
) -> Result<(String, Option<StreamRef>), String> {
    if let Some(b) = branch.map(str::trim).filter(|s| !s.is_empty()) {
        if !crate::git::is_baker_branch(b) {
            return Err(format!("{b:?} is not a baker/* branch"));
        }
        let stream = baker_streams_for_repo(state, repo_root)?
            .into_iter()
            .find(|s| {
                s.branch == b || s.name == format!("baker-{}", b.trim_start_matches("baker/"))
            });
        return Ok((b.to_string(), stream));
    }
    if let Some(sid) = stream_id.map(str::trim).filter(|s| !s.is_empty()) {
        let ctx = context_for(state, sid)?;
        if !same_path(&ctx.repo_root, repo_root) {
            return Err(format!("stream {sid} is not on the bakery source root"));
        }
        if !crate::git::is_baker_branch(&ctx.branch) {
            return Err(format!(
                "stream {sid} branch {:?} is not baker/*",
                ctx.branch
            ));
        }
        let stream = known_streams(state)?
            .into_iter()
            .find(|s| s.stream_id == sid);
        return Ok((ctx.branch, stream));
    }
    Err("branch or stream_id required".into())
}

pub(crate) fn baker_work_detail_inner(
    state: &AppState,
    q: &BakerWorkDetailQuery,
    env_root: Option<&str>,
    detect_from: Option<&std::path::Path>,
) -> Result<BakerWorkDetail, String> {
    let (repo_path, repo_root, base) = baker_repo_ctx(state, env_root, detect_from)?;
    let (branch, stream) = resolve_baker_branch(
        state,
        &repo_root,
        q.branch.as_deref(),
        q.stream_id.as_deref(),
    )?;

    let worktree_path = stream.as_ref().map(|s| s.worktree_path.clone());
    let worktree_exists = worktree_path
        .as_ref()
        .map(|p| std::path::Path::new(p).is_dir())
        .unwrap_or(false);
    let stream_id = stream.as_ref().map(|s| s.stream_id.clone());
    let brief = stream_id
        .as_deref()
        .and_then(|id| latest_baker_brief(state, id));

    let (commits, commits_error) = match &base {
        Some(b) => match crate::git::commits_not_in_base(&repo_path, &branch, b) {
            Ok(c) => (c, None),
            Err(e) => (Vec::new(), Some(e)),
        },
        None => (
            Vec::new(),
            Some("default branch unknown — cannot list commits vs base".into()),
        ),
    };

    let (diff_stat, diff_error) = match &base {
        Some(b) => match crate::git::diff_stat(&repo_path, b, &branch) {
            Ok(s) => (Some(s).filter(|x| !x.is_empty()), None),
            Err(e) => (None, Some(e)),
        },
        None => (None, Some("default branch unknown — cannot diff".into())),
    };

    let (status, status_error) = if worktree_exists {
        let p = worktree_path.as_deref().unwrap();
        match crate::git::porcelain_status(std::path::Path::new(p)) {
            Ok(s) => (s, None),
            Err(e) => (Vec::new(), Some(e)),
        }
    } else {
        (Vec::new(), None)
    };

    Ok(BakerWorkDetail {
        branch,
        base,
        stream_id,
        worktree_path,
        worktree_exists,
        brief,
        commits,
        commits_error,
        diff_stat,
        diff_error,
        status,
        status_error,
        has_origin: crate::git::has_remote(&repo_path, "origin"),
        gh_available: crate::git::gh_available(),
        bakery_source_root: repo_root,
    })
}

pub(crate) fn baker_push_inner(
    state: &AppState,
    branch: &str,
    env_root: Option<&str>,
    detect_from: Option<&std::path::Path>,
) -> Result<BakerActionResp, String> {
    let branch = branch.trim();
    if !crate::git::is_baker_branch(branch) {
        return Err(format!("push refused: {branch:?} is not a baker/* branch"));
    }
    let (repo_path, _, _) = baker_repo_ctx(state, env_root, detect_from)?;
    if !crate::git::has_remote(&repo_path, "origin") {
        return Err("no remote — origin is not configured".into());
    }
    let message = crate::git::push_set_upstream(&repo_path, branch)?;
    Ok(BakerActionResp {
        ok: true,
        message: if message.is_empty() {
            format!("pushed {branch} to origin")
        } else {
            message
        },
        branch: branch.to_string(),
    })
}

pub(crate) fn baker_pr_inner(
    state: &AppState,
    req: &BakerPrReq,
    env_root: Option<&str>,
    detect_from: Option<&std::path::Path>,
) -> Result<BakerActionResp, String> {
    let branch = req.branch.trim();
    if !crate::git::is_baker_branch(branch) {
        return Err(format!("pr refused: {branch:?} is not a baker/* branch"));
    }
    if !crate::git::gh_available() {
        return Err("gh not available".into());
    }
    let (repo_path, _, base) = baker_repo_ctx(state, env_root, detect_from)?;
    let base = base.ok_or_else(|| "default branch unknown — cannot open PR".to_string())?;
    let title = req.title.clone().unwrap_or_default();
    let body = req.body.clone().unwrap_or_default();
    let message = crate::git::gh_pr_create(&repo_path, &base, branch, &title, &body)?;
    Ok(BakerActionResp {
        ok: true,
        message,
        branch: branch.to_string(),
    })
}

pub(crate) fn baker_merge_inner(
    state: &AppState,
    req: &BakerMergeReq,
    env_root: Option<&str>,
    detect_from: Option<&std::path::Path>,
) -> Result<BakerActionResp, String> {
    if !req.confirm {
        return Err(
            "merge refused: explicit confirm required (human UI gate — set confirm: true)".into(),
        );
    }
    let branch = req.branch.trim();
    if branch == "main" || branch == "master" {
        return Err("merge refused: never merge main/master as a Baker head".into());
    }
    if !crate::git::is_baker_branch(branch) {
        return Err(format!("merge refused: {branch:?} is not a baker/* branch"));
    }
    if !crate::git::gh_available() {
        return Err("gh not available — merge not wired without gh".into());
    }
    let (repo_path, _, _) = baker_repo_ctx(state, env_root, detect_from)?;
    // gh pr merge --merge only; never --admin / --force / force-push main.
    let message = crate::git::gh_pr_merge(&repo_path, branch)?;
    Ok(BakerActionResp {
        ok: true,
        message,
        branch: branch.to_string(),
    })
}

pub(crate) async fn baker_work_list(
    State(st): State<AppState>,
) -> Result<Json<BakerWorkList>, (StatusCode, String)> {
    let detect = std::env::current_dir().ok();
    let env_root = bakery_source_root_from_env();
    let st2 = st.clone();
    crate::actor::spawn_blocking(move || {
        baker_work_list_inner(&st2, env_root.as_deref(), detect.as_deref())
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map(Json)
    .map_err(|e| (StatusCode::BAD_REQUEST, e))
}

pub(crate) async fn baker_work_detail(
    State(st): State<AppState>,
    Query(q): Query<BakerWorkDetailQuery>,
) -> Result<Json<BakerWorkDetail>, (StatusCode, String)> {
    let detect = std::env::current_dir().ok();
    let env_root = bakery_source_root_from_env();
    let st2 = st.clone();
    crate::actor::spawn_blocking(move || {
        baker_work_detail_inner(&st2, &q, env_root.as_deref(), detect.as_deref())
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map(Json)
    .map_err(|e| (StatusCode::BAD_REQUEST, e))
}

pub(crate) async fn baker_push(
    State(st): State<AppState>,
    Json(req): Json<BakerBranchReq>,
) -> Result<Json<BakerActionResp>, (StatusCode, String)> {
    let detect = std::env::current_dir().ok();
    let env_root = bakery_source_root_from_env();
    let st2 = st.clone();
    crate::actor::spawn_blocking(move || {
        baker_push_inner(&st2, &req.branch, env_root.as_deref(), detect.as_deref())
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map(Json)
    .map_err(|e| (StatusCode::BAD_REQUEST, e))
}

pub(crate) async fn baker_pr(
    State(st): State<AppState>,
    Json(req): Json<BakerPrReq>,
) -> Result<Json<BakerActionResp>, (StatusCode, String)> {
    let detect = std::env::current_dir().ok();
    let env_root = bakery_source_root_from_env();
    let st2 = st.clone();
    crate::actor::spawn_blocking(move || {
        baker_pr_inner(&st2, &req, env_root.as_deref(), detect.as_deref())
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map(Json)
    .map_err(|e| (StatusCode::BAD_REQUEST, e))
}

pub(crate) async fn baker_merge(
    State(st): State<AppState>,
    Json(req): Json<BakerMergeReq>,
) -> Result<Json<BakerActionResp>, (StatusCode, String)> {
    let detect = std::env::current_dir().ok();
    let env_root = bakery_source_root_from_env();
    let st2 = st.clone();
    crate::actor::spawn_blocking(move || {
        baker_merge_inner(&st2, &req, env_root.as_deref(), detect.as_deref())
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map(Json)
    .map_err(|e| (StatusCode::BAD_REQUEST, e))
}

// ─── Permanent Baker (07d-B Phase 1) ─────────────────────────────────────────

/// The permanent Baker's durable coordinates — what `GET /baker` returns
/// and what the Baker chrome renders. One per node, minted once by
/// [`ensure_permanent_baker`] and pointed at by `NodeConfig.baker_stream_id`.
#[derive(schemars::JsonSchema, Debug, Clone, Serialize)]
pub(crate) struct BakerHome {
    pub(crate) stream_id: String,
    /// `{stream_id}:baker` — stable across restarts and model reassignment.
    pub(crate) agent_id: String,
    /// The read-scoped root this socket binds (the recorded
    /// `bakery_source_root`), or `""` when the source root is unknown —
    /// the identity exists either way (07d-B: identity is independent of
    /// its execution environment).
    pub(crate) root: String,
    /// Latest `seat.model.assigned` for the baker seat on this stream, or
    /// `None` when the human has not configured one yet. Never invented.
    pub(crate) assigned_model_id: Option<String>,
    /// WIRE names (never the dotted spec `id` — see `BakerOpSpec`'s doc
    /// comment) of every declared Bakery op this stream's turns can actually
    /// call (07d-B Phase 2 item 5) — the same [`crate::baker_ops::BAKER_OPS`]
    /// table `GET /baker/capabilities` and the turn's own system prompt
    /// (`crate::agent_loop::format_baker_op_registry`) render. Names only;
    /// summaries live at `/baker/capabilities`.
    pub(crate) ops: Vec<&'static str>,
}

fn baker_home_for(state: &AppState, stream_id: &str) -> Result<BakerHome, String> {
    let ctx = context_for(state, stream_id)?;
    let assigned_model_id = crate::state::latest_seat_model_id(
        &state.heart.lock().unwrap(),
        stream_id,
        crate::node::Seat::Baker,
    )
    .map_err(|()| {
        "seat.model.assigned on the Baker stream is unreadable — refusing to guess".to_string()
    })?;
    Ok(BakerHome {
        stream_id: stream_id.to_string(),
        agent_id: crate::node::agent_id(stream_id, crate::node::Seat::Baker),
        root: ctx.worktree_path,
        assigned_model_id,
        ops: crate::baker_ops::BAKER_OPS.iter().map(|o| o.wire).collect(),
    })
}

/// Mint-or-return the permanent Baker socket (07d-B Phase 1).
///
/// Atomic: the whole check→mint→persist section runs under
/// `AppState::baker_bootstrap_lock`, so two concurrent first opens converge
/// on one stream — the loser of the race re-reads the winner's pointer.
///
/// The socket is factory-kind and binds the recorded `bakery_source_root`
/// in place as a read-scoped root when one is known (writes still gate
/// through ordinary proposals); when none is known the socket is minted
/// rootless — the identity must exist either way, and a root can bind
/// later without touching the conversation.
///
/// A `baker_stream_id` in `node.json` that Heart does not know (a wiped
/// heart.db beside a stale config) is re-minted rather than served as a
/// dead pointer.
pub(crate) fn ensure_permanent_baker(
    state: &AppState,
    env_root: Option<&str>,
    detect_from: Option<&std::path::Path>,
) -> Result<BakerHome, String> {
    let _guard = state.baker_bootstrap_lock.lock().unwrap();

    let recorded = state.node.lock().unwrap().baker_stream_id.clone();
    if let Some(id) = recorded {
        let exists = state
            .heart
            .lock()
            .unwrap()
            .first_of_family(&id, "project.onboarded")
            .map_err(|e| e.to_string())?
            .is_some();
        if exists {
            return baker_home_for(state, &id);
        }
        eprintln!(
            "bakery-node: baker_stream_id {id} not found in Heart — re-minting the permanent Baker"
        );
    }

    // Root resolution is best-effort here, unlike `/baker/assign`: the
    // permanent identity must not depend on knowing the source root.
    let root = resolve_bakery_source_root(state, env_root, detect_from)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (repo_root, branch) = if root.is_empty() {
        (String::new(), String::new())
    } else {
        let info = crate::git::inspect(&root);
        (
            info.root.clone().unwrap_or_default(),
            info.current_branch.clone().unwrap_or_default(),
        )
    };

    let stream_id = crate::node::mint_id("socket");
    let onboarded_payload = serde_json::json!({
        "repo": repo_root,
        "repo_root": repo_root,
        "project": "Baker",
        "name": "Baker",
        "worktree_path": root,
        "branch": branch,
        "opened_by_node": state.node.lock().unwrap().node_id.clone(),
        "created_by_bakery": false,
    })
    .to_string();
    let onboarded = append(
        state,
        &stream_id,
        "human",
        "project.onboarded",
        onboarded_payload,
        vec![],
    )?;
    // Factory authority, same required-second-append contract as
    // `baker_assign_inner` above: never report success with the stream
    // left classified as an ordinary project.
    append(
        state,
        &stream_id,
        "human",
        crate::node::SOCKET_KIND_FAMILY,
        serde_json::to_string(&crate::node::SocketKindRecord {
            kind: crate::node::SocketKind::Factory,
        })
        .unwrap(),
        vec![onboarded.event_hash.clone()],
    )?;
    crate::state::claim_ownership(state, &stream_id)?;

    {
        let mut node = state.node.lock().unwrap();
        node.baker_stream_id = Some(stream_id.clone());
        if let Err(e) = node.save(state.base.as_path()) {
            // The Heart facts are the durable truth; a failed pointer write
            // means the next boot re-mints. Say so rather than failing the
            // request that already has a working Baker.
            eprintln!("bakery-node: could not persist baker_stream_id: {e}");
        }
    }

    baker_home_for(state, &stream_id)
}

/// `GET /baker` — the permanent Baker surface (closes OPERATOR.md's TODO
/// row). Bootstraps on first hit; idempotent ever after.
pub(crate) async fn baker_home(
    State(st): State<AppState>,
) -> Result<Json<BakerHome>, (StatusCode, String)> {
    let detect = std::env::current_dir().ok();
    let env_root = bakery_source_root_from_env();
    let st2 = st.clone();
    crate::actor::spawn_blocking(move || {
        ensure_permanent_baker(&st2, env_root.as_deref(), detect.as_deref())
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map(Json)
    .map_err(|e| (StatusCode::BAD_REQUEST, e))
}

// ─── Capabilities ────────────────────────────────────────────────────────────

/// One row of `GET /baker/capabilities`'s rendered table (name/summary/gate
/// class) — the same shape whether the row came from the canonical HTTP
/// core table or the Bakery op registry, so a client renders one list
/// without caring which table a row came from.
#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct CapabilityRow {
    pub(crate) name: String,
    pub(crate) summary: String,
    pub(crate) gate_class: crate::tools::GateClass,
}

/// One row of the Bakery op half of the table — `sense`/`act` rather than a
/// [`crate::tools::GateClass`]: none of that enum's four variants honestly
/// describes "executes directly today, audited, reversible-by-retire" (07d-B
/// Phase 2 item 4's approval-routing for destructive ops is not built in
/// this pass — see `crate::baker_ops`'s module doc comment), so this row
/// makes no claim about gating it cannot back up.
#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct BakerOpCapabilityRow {
    pub(crate) name: String,
    pub(crate) summary: String,
    pub(crate) sense: bool,
}

/// `GET /baker/capabilities` response (07d-B Phase 2 item 5 — "render the
/// real table"): the canonical HTTP core tool set every HTTP/openai-compat
/// seat gets, plus (only when `stream_id` names the permanent Baker) the
/// declared Bakery op registry — "HTTP core set + ops the caller context
/// would get", not the legacy 5-tool `crate::tools::TOOLS` array this route
/// used to serialize verbatim.
#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct CapabilitiesView {
    pub(crate) http_core: Vec<CapabilityRow>,
    /// Empty unless `stream_id` was given and names the permanent Baker
    /// stream (`crate::baker_ops::is_permanent_baker_stream`) — an old
    /// `/baker/assign` branch-job socket or a project socket gets none,
    /// same authorization rule the tool loop itself enforces.
    pub(crate) baker_ops: Vec<BakerOpCapabilityRow>,
}

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct CapabilitiesQuery {
    /// Optional: the stream whose granted ops to render. Omitted (or naming
    /// anything other than the permanent Baker) → `baker_ops` is empty.
    #[serde(default)]
    pub(crate) stream_id: Option<String>,
}

pub(crate) async fn baker_capabilities(
    State(st): State<AppState>,
    Query(q): Query<CapabilitiesQuery>,
) -> Json<CapabilitiesView> {
    // Anchor the full gate ladder (Read..Elevated) so reserved variants stay
    // live until Exec/Elevated tools land as HTTP_CORE_ADS rows.
    let _ = crate::tools::ALL_GATE_CLASSES;
    let http_core = crate::tools::http_core_capability_rows()
        .into_iter()
        .map(|(name, summary, gate_class)| CapabilityRow {
            name: name.to_string(),
            summary: summary.to_string(),
            gate_class,
        })
        .collect();
    let is_baker = q
        .stream_id
        .as_deref()
        .map(|sid| crate::baker_ops::is_baker_scope_stream(&st, sid))
        .unwrap_or(false);
    let baker_ops = if is_baker {
        crate::baker_ops::BAKER_OPS
            .iter()
            .map(|op| BakerOpCapabilityRow {
                name: op.wire.to_string(),
                summary: op.summary.to_string(),
                sense: op.risk == crate::api_catalog::Risk::Read,
            })
            .collect()
    } else {
        Vec::new()
    };
    Json(CapabilitiesView {
        http_core,
        baker_ops,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::NodeConfig;
    use crate::routes::agent::{agent_turn_inner, AgentTurnReq};
    use crate::testutil::*;
    use std::sync::Arc;

    #[test]
    fn baker_assign_creates_branch_with_baker_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let repo = bakery_repo_for_test(dir.path());
        let state = state_with_bakery_root(dir.path(), &repo);

        // No cwd detection — config alone is enough.
        let resp = baker_assign_inner(&state, "shell polish", None, None).expect("assign");

        assert!(
            resp.context.branch.starts_with("baker/"),
            "branch must use baker/ prefix: {}",
            resp.context.branch
        );
        assert_eq!(resp.context.branch, "baker/shell-polish");
        // 07d-A3: stream ids are opaque and server-minted — the branch slug
        // above still derives from the brief, but identity no longer does.
        assert!(
            resp.context.stream_id.starts_with("socket_"),
            "got {}",
            resp.context.stream_id
        );
        // Pin the fixture repo — process env must not redirect assign (B2 must-fix).
        // Canonicalize both sides: assign resolves the root, and on macOS the
        // temp dir is `/var/...` symlinked to `/private/var/...`, so comparing
        // raw paths fails on that platform alone.
        assert_eq!(
            PathBuf::from(&resp.bakery_source_root)
                .canonicalize()
                .unwrap(),
            repo.canonicalize().unwrap(),
            "assign must use test bakery_source_root, not process env"
        );
        assert!(!resp.attached);
        assert!(!resp.context.in_place);
        assert!(
            resp.context.worktree_path.contains("worktrees"),
            "must live under worktree base: {}",
            resp.context.worktree_path
        );
        assert!(std::path::Path::new(&resp.context.worktree_path).is_dir());
        assert_eq!(resp.event.family, BAKER_ASSIGN_FAMILY);
        assert_eq!(resp.write_mode, WriteMode::Manual);

        let p: serde_json::Value = serde_json::from_str(&resp.event.payload).unwrap();
        assert_eq!(p["brief"], "shell polish");
        assert_eq!(p["branch"], "baker/shell-polish");
    }

    #[test]
    fn baker_assign_second_call_reuses_same_stream() {
        let dir = tempfile::tempdir().unwrap();
        let repo = bakery_repo_for_test(dir.path());
        let state = state_with_bakery_root(dir.path(), &repo);

        let first = baker_assign_inner(&state, "same brief again", None, None).unwrap();
        let second = baker_assign_inner(&state, "same brief again", None, None).unwrap();

        assert_eq!(first.context.stream_id, second.context.stream_id);
        assert_eq!(first.context.worktree_path, second.context.worktree_path);
        assert!(second.attached, "second assign must attach, not recreate");

        let assigns = state
            .heart
            .lock()
            .unwrap()
            .read_stream(&first.context.stream_id)
            .unwrap()
            .into_iter()
            .filter(|e| e.family == BAKER_ASSIGN_FAMILY)
            .count();
        assert_eq!(assigns, 2, "each assign appends baker.assignment.accepted");
    }

    #[test]
    fn baker_assign_new_brief_opens_a_new_stream() {
        let dir = tempfile::tempdir().unwrap();
        let repo = bakery_repo_for_test(dir.path());
        let state = state_with_bakery_root(dir.path(), &repo);

        let a = baker_assign_inner(&state, "first mission", None, None).unwrap();
        let b = baker_assign_inner(&state, "second mission", None, None).unwrap();

        assert_ne!(a.context.stream_id, b.context.stream_id);
        assert_ne!(a.context.branch, b.context.branch);
        assert!(a.context.branch.starts_with("baker/"));
        assert!(b.context.branch.starts_with("baker/"));
        assert!(!b.attached);
    }

    #[test]
    fn baker_assign_refuses_in_place_primary_checkout() {
        let dir = tempfile::tempdir().unwrap();
        let repo = bakery_repo_for_test(dir.path());
        let state = state_with_bakery_root(dir.path(), &repo);

        // Pre-onboard the baker stream on the primary checkout (in-place).
        open_stream(
            &state,
            &OpenReq {
                repo: repo.to_string_lossy().into_owned(),
                name: Some("baker-force-inplace".into()),
                branch: Some("baker/force-inplace".into()),
                worktree_path: Some(repo.to_string_lossy().into_owned()),
                ..Default::default()
            },
        )
        .unwrap();

        let err = baker_assign_inner(&state, "force inplace", None, None).unwrap_err();
        assert!(
            err.contains("in-place") || err.contains("primary"),
            "must refuse in-place baker session: {err}"
        );
    }

    #[test]
    fn baker_assign_errors_when_source_root_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        // No env (leave alone if set — override with empty node + no detect_from).
        state.node.lock().unwrap().bakery_source_root = None;

        // env_root is threaded in — process env cannot hijack this (Opus B2 must-fix).
        let err = resolve_bakery_source_root(&state, None, None).unwrap_err();
        assert!(err.contains("unknown"), "{err}");

        let err = baker_assign_inner(&state, "", None, None).unwrap_err();
        assert!(err.contains("empty"), "{err}");
    }

    #[test]
    fn bakery_source_root_persist_round_trips_node_json() {
        let dir = tempfile::tempdir().unwrap();
        let repo = bakery_repo_for_test(dir.path());
        let state = state_with(dir.path(), Arc::new(StubModel));
        state.node.lock().unwrap().bakery_source_root = None;

        // Detect markers without env (resolve may see BAKERY_SOURCE_ROOT in CI).
        let found = detect_bakery_source_root(&repo).expect("VISION.md + crates/bakery-node");
        assert_eq!(found, repo);
        persist_bakery_source_root(&state, &found);

        let in_mem = state
            .node
            .lock()
            .unwrap()
            .bakery_source_root
            .clone()
            .expect("must pin after persist");
        assert_eq!(PathBuf::from(&in_mem), repo);

        // Survives a fresh load of node.json (no export needed next start).
        let reloaded = NodeConfig::load_or_create(dir.path());
        assert_eq!(
            reloaded
                .bakery_source_root
                .as_deref()
                .map(PathBuf::from)
                .as_ref(),
            Some(&repo)
        );
    }

    #[test]
    fn baker_slug_sanitizes_brief() {
        // Non-alnum → `-`; adjacent punctuation can yield double dashes (ok for paths).
        assert_eq!(baker_slug("Hello, World!"), "Hello--World");
        assert_eq!(baker_slug("  multi\nline  "), "multi");
    }

    // ─── Baker work middleman (B2) ───────────────────────────────────────────

    #[test]
    fn baker_work_list_empty_when_no_baker_branches() {
        let dir = tempfile::tempdir().unwrap();
        let repo = bakery_repo_for_test(dir.path());
        let state = state_with_bakery_root(dir.path(), &repo);

        let list = baker_work_list_inner(&state, None, None).expect("list");
        assert!(
            list.items.is_empty(),
            "fresh bakery repo has no baker/*: {:?}",
            list.items
        );
        assert_eq!(list.base.as_deref(), Some("main"));
        assert!(list.git_error.is_none(), "{:?}", list.git_error);
    }

    #[test]
    fn baker_work_list_includes_branch_from_assign() {
        let dir = tempfile::tempdir().unwrap();
        let repo = bakery_repo_for_test(dir.path());
        let state = state_with_bakery_root(dir.path(), &repo);

        let assigned = baker_assign_inner(&state, "wire review", None, None).unwrap();
        assert_eq!(assigned.context.branch, "baker/wire-review");

        let list = baker_work_list_inner(&state, None, None).unwrap();
        let item = list
            .items
            .iter()
            .find(|i| i.branch == "baker/wire-review")
            .expect("assign branch must appear in list");
        assert_eq!(
            item.stream_id.as_deref(),
            Some(assigned.context.stream_id.as_str())
        );
        assert!(item.worktree_exists);
        assert_eq!(item.brief.as_deref(), Some("wire review"));
    }

    #[tokio::test]
    async fn baker_work_list_excludes_a_discarded_stream() {
        use crate::routes::lifecycle::{discard_stream, DiscardReq};

        let dir = tempfile::tempdir().unwrap();
        let repo = bakery_repo_for_test(dir.path());
        let state = state_with_bakery_root(dir.path(), &repo);

        let assigned = baker_assign_inner(&state, "gone by discard", None, None).unwrap();
        assert_eq!(assigned.context.branch, "baker/gone-by-discard");

        let discard = discard_stream(
            State(state.clone()),
            Json(DiscardReq {
                stream_id: assigned.context.stream_id.clone(),
                confirm: true,
            }),
        )
        .await
        .unwrap();
        assert!(
            discard.worktree_removed,
            "sanity: discard must actually remove the worktree"
        );

        let list = baker_work_list_inner(&state, None, None).unwrap();
        let item = list
            .items
            .iter()
            .find(|i| i.branch == "baker/gone-by-discard")
            .expect("the git branch itself is untouched by discard, so it still enumerates");
        assert!(
            item.stream_id.is_none(),
            "a discarded stream must not still be linked as the open job for its branch: {item:?}"
        );
        assert!(!item.worktree_exists);
        // 07c-LIVE-STATE: the orphaned row is distinguishable from a branch
        // that never had a stream — the client cross-references this instead
        // of an un-canonicalized repo_root comparison it can't safely do.
        assert!(
            item.discarded,
            "an orphan branch whose stream was discarded must be flagged, not indistinguishable from a never-linked branch: {item:?}"
        );
    }

    #[tokio::test]
    async fn baker_work_list_never_had_a_stream_is_not_flagged_discarded() {
        let dir = tempfile::tempdir().unwrap();
        let repo = bakery_repo_for_test(dir.path());
        let state = state_with_bakery_root(dir.path(), &repo);

        run_git(&repo, &["branch", "baker/hand-made"]);

        let list = baker_work_list_inner(&state, None, None).unwrap();
        let item = list
            .items
            .iter()
            .find(|i| i.branch == "baker/hand-made")
            .expect("a manually created baker/* branch still enumerates");
        assert!(item.stream_id.is_none());
        assert!(
            !item.discarded,
            "a branch that never had any stream must not be flagged discarded: {item:?}"
        );
    }

    #[test]
    fn baker_work_detail_lists_commits_not_in_base() {
        let dir = tempfile::tempdir().unwrap();
        let repo = bakery_repo_for_test(dir.path());
        let state = state_with_bakery_root(dir.path(), &repo);
        let assigned = baker_assign_inner(&state, "extra commit", None, None).unwrap();
        let wt = &assigned.context.worktree_path;

        // One commit only on the baker branch.
        std::fs::write(std::path::Path::new(wt).join("baker-note.txt"), "hi\n").unwrap();
        run_git(std::path::Path::new(wt), &["add", "."]);
        run_git(std::path::Path::new(wt), &["commit", "-qm", "baker note"]);

        let detail = baker_work_detail_inner(
            &state,
            &BakerWorkDetailQuery {
                branch: Some("baker/extra-commit".into()),
                stream_id: None,
            },
            None,
            None,
        )
        .unwrap();

        assert_eq!(detail.base.as_deref(), Some("main"));
        assert!(detail.commits_error.is_none(), "{:?}", detail.commits_error);
        assert_eq!(detail.commits.len(), 1, "{:?}", detail.commits);
        assert_eq!(detail.commits[0].subject, "baker note");
        assert!(
            detail
                .diff_stat
                .as_deref()
                .unwrap_or("")
                .contains("baker-note"),
            "stat should mention the file: {:?}",
            detail.diff_stat
        );
        assert!(!detail.has_origin, "temp repo has no origin");
    }

    #[test]
    fn baker_merge_refuses_without_confirm() {
        let dir = tempfile::tempdir().unwrap();
        let repo = bakery_repo_for_test(dir.path());
        let state = state_with_bakery_root(dir.path(), &repo);
        baker_assign_inner(&state, "merge gate", None, None).unwrap();

        let err = baker_merge_inner(
            &state,
            &BakerMergeReq {
                branch: "baker/merge-gate".into(),
                confirm: false,
            },
            None,
            None,
        )
        .unwrap_err();
        assert!(err.contains("confirm"), "must require human confirm: {err}");
    }

    #[test]
    fn baker_merge_refuses_main_even_with_confirm() {
        let dir = tempfile::tempdir().unwrap();
        let repo = bakery_repo_for_test(dir.path());
        let state = state_with_bakery_root(dir.path(), &repo);

        let err = baker_merge_inner(
            &state,
            &BakerMergeReq {
                branch: "main".into(),
                confirm: true,
            },
            None,
            None,
        )
        .unwrap_err();
        assert!(
            err.contains("main") || err.contains("refused"),
            "must never merge main: {err}"
        );
    }

    #[test]
    fn baker_push_refuses_non_baker_branch() {
        let dir = tempfile::tempdir().unwrap();
        let repo = bakery_repo_for_test(dir.path());
        let state = state_with_bakery_root(dir.path(), &repo);

        let err = baker_push_inner(&state, "main", None, None).unwrap_err();
        assert!(err.contains("baker"), "{err}");
    }

    #[test]
    fn git_is_baker_branch_helper() {
        assert!(crate::git::is_baker_branch("baker/foo"));
        assert!(!crate::git::is_baker_branch("baker/"));
        assert!(!crate::git::is_baker_branch("main"));
        assert!(!crate::git::is_baker_branch("feature/baker/x"));
    }

    // ─── Baker ops context on turns ──────────────────────────────────────────

    #[test]
    fn baker_turn_injects_measured_ops_into_system_and_opening() {
        let dir = tempfile::tempdir().unwrap();
        let repo = bakery_repo_for_test(dir.path());
        let model = Arc::new(ScriptedModel::new(&[r#"{"done":{"summary":"ok"}}"#]));
        let state = state_with(dir.path(), model.clone());
        state.node.lock().unwrap().bakery_source_root = Some(repo.to_string_lossy().into_owned());
        state.node.lock().unwrap().write_mode = WriteMode::Manual;

        let assigned = baker_assign_inner(&state, "ops inject", None, None).unwrap();
        // Open a second ordinary stream so stream_count is > 1 (measured).
        open_stream(&state, &open_req(&repo, "other")).unwrap();

        agent_turn_inner(
            &state,
            &AgentTurnReq {
                model_id: None,
                effort: None,
                actor: None,
                stream_id: assigned.context.stream_id.clone(),
                file_path: None,
                instruction: "what is the write mode?".into(),
                seat: None,
            },
        )
        .expect("baker turn");

        let cap = model.captured.lock().unwrap();
        assert!(!cap.is_empty(), "model must see at least one chat call");
        let (system, user) = &cap[0];
        for field in [
            "write_mode: manual",
            "stream_count:",
            "pending_gates:",
            "baker_branch: baker/ops-inject",
        ] {
            assert!(system.contains(field), "system missing {field:?}: {system}");
            assert!(user.contains(field), "opening missing {field:?}: {user}");
        }
        // node_live / hostname must stay off the provider wire (local Heart only).
        assert!(
            !system.contains("node_live") && !user.contains("node_live"),
            "node_live must not reach the model: system={system} user={user}"
        );
        // Real stream count includes baker + other (+ any node:policy?).
        assert!(
            system.contains("stream_count: 2") || system.contains("stream_count: 3"),
            "stream_count should reflect onboarded streams: {system}"
        );
        let lower = format!("{system}\n{user}").to_ascii_lowercase();
        for ban in [
            "email",
            "spend",
            "cost_usd",
            "oauth",
            "token_window",
            "node_live",
        ] {
            assert!(!lower.contains(ban), "must not invent/leak {ban}: {lower}");
        }
        // node_id is hostname-slug + hex; neither half belongs in the model prompt.
        let node_id = state.node.lock().unwrap().node_id.clone();
        assert!(
            !lower.contains(&node_id.to_ascii_lowercase()),
            "full node_id must not reach the model: {node_id}"
        );
        if let Some(slug) = node_id.rsplit_once('-').map(|(s, _)| s) {
            if slug.len() >= 4 {
                assert!(
                    !lower.contains(&slug.to_ascii_lowercase()),
                    "hostname slug {slug:?} must not appear in model ops: {lower}"
                );
            }
        }
    }

    #[test]
    fn non_baker_turn_does_not_inject_factory_ops() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let model = Arc::new(ScriptedModel::new(&[r#"{"done":{"summary":"ok"}}"#]));
        let state = state_with(dir.path(), model.clone());
        let stream_id = open_stream(&state, &open_req(&repo, "featureA"))
            .unwrap()
            .context
            .stream_id;

        agent_turn_inner(
            &state,
            &AgentTurnReq {
                model_id: None,
                effort: None,
                actor: None,
                stream_id,
                file_path: None,
                instruction: "hello".into(),
                seat: None,
            },
        )
        .unwrap();

        let cap = model.captured.lock().unwrap();
        let (system, user) = &cap[0];
        assert!(
            !system.contains("Factory ops"),
            "non-baker system must not get ops: {system}"
        );
        assert!(
            !user.contains("Factory ops"),
            "non-baker opening must not get ops: {user}"
        );
        assert!(system.contains("orchestrator agent for this project"));
        assert!(system.ends_with(&crate::agent_loop::system_tools()));
    }

    #[test]
    fn baker_assign_event_carries_measured_ops_not_spend() {
        let dir = tempfile::tempdir().unwrap();
        let repo = bakery_repo_for_test(dir.path());
        let state = state_with_bakery_root(dir.path(), &repo);
        let resp = baker_assign_inner(&state, "assign ops", None, None).unwrap();
        let p: serde_json::Value = serde_json::from_str(&resp.event.payload).unwrap();
        let ops = p.get("ops").expect("ops on assignment event");
        assert!(ops.get("node_live").and_then(|v| v.as_str()).is_some());
        assert_eq!(
            ops.get("write_mode").and_then(|v| v.as_str()),
            Some("manual")
        );
        assert!(ops.get("stream_count").and_then(|v| v.as_u64()).is_some());
        assert!(ops.get("pending_gates").and_then(|v| v.as_u64()).is_some());
        assert_eq!(
            ops.get("baker_branch").and_then(|v| v.as_str()),
            Some("baker/assign-ops")
        );
        let wire = resp.event.payload.to_ascii_lowercase();
        assert!(!wire.contains("email"));
        assert!(!wire.contains("spend"));
        assert!(!wire.contains("cost_usd"));
    }

    /// 07d-A1 correction: `baker_assign_inner` is the only place that ever
    /// records `socket.kind.recorded: factory` — proves it actually lands
    /// on the stream, and that a turn on it defaults to the baker seat
    /// purely from that durable fact (never from the `baker-`/`baker/`
    /// shaped name/branch this same call also happens to produce).
    #[test]
    fn baker_assign_records_the_durable_factory_kind_fact_and_turns_read_it() {
        let dir = tempfile::tempdir().unwrap();
        let repo = bakery_repo_for_test(dir.path());
        let state = state_with_bakery_root(dir.path(), &repo);
        let resp = baker_assign_inner(&state, "kind fact", None, None).unwrap();

        let events = state
            .heart
            .lock()
            .unwrap()
            .read_stream(&resp.context.stream_id)
            .unwrap();
        let kind_ev = events
            .iter()
            .find(|e| e.family == crate::node::SOCKET_KIND_FAMILY)
            .expect("baker_assign_inner must record socket.kind.recorded");
        let p: serde_json::Value = serde_json::from_str(&kind_ev.payload).unwrap();
        assert_eq!(p["kind"], "factory");

        crate::state::seed_seat_model_for_test(
            &state,
            &resp.context.stream_id,
            crate::node::Seat::Baker,
            Arc::new(StubModel),
        );
        let ev = agent_turn_inner(
            &state,
            &AgentTurnReq {
                model_id: None,
                effort: None,
                actor: None,
                stream_id: resp.context.stream_id.clone(),
                file_path: None,
                instruction: "hi".into(),
                seat: None,
            },
        )
        .unwrap();
        assert_eq!(
            ev.actor, "agent:baker",
            "a real Baker-assigned stream must default to the baker seat"
        );
    }

    /// 07d-B Phase 2 item 5: capabilities renders the REAL table — the
    /// canonical HTTP core set (not the legacy 5-tool `crate::tools::TOOLS`
    /// array this route used to serialize verbatim) — and, without a
    /// `stream_id`, no Bakery ops (nobody's authorization was proven).
    #[tokio::test]
    async fn capabilities_renders_the_http_core_table_and_no_ops_without_a_stream_id() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let Json(view) =
            baker_capabilities(State(state), Query(CapabilitiesQuery { stream_id: None })).await;
        assert_eq!(view.http_core.len(), crate::tools::BAKERY_HTTP_CORE.len());
        for (row, name) in view.http_core.iter().zip(crate::tools::BAKERY_HTTP_CORE) {
            assert_eq!(&row.name, name);
        }
        assert!(
            view.baker_ops.is_empty(),
            "no stream_id given — nothing proven to be the permanent Baker"
        );
    }

    /// The permanent Baker's own stream_id grants the full Bakery op
    /// registry; an old `/baker/assign` branch-job socket (also
    /// `SocketKind::Factory`, but not `baker_stream_id`) gets none.
    #[tokio::test]
    async fn capabilities_renders_baker_ops_only_for_the_permanent_baker_stream_id() {
        let dir = tempfile::tempdir().unwrap();
        let repo = bakery_repo_for_test(dir.path());
        let state = state_with_bakery_root(dir.path(), &repo);
        let home = ensure_permanent_baker(&state, None, None).unwrap();

        let Json(baker_view) = baker_capabilities(
            State(state.clone()),
            Query(CapabilitiesQuery {
                stream_id: Some(home.stream_id.clone()),
            }),
        )
        .await;
        assert_eq!(
            baker_view.baker_ops.len(),
            crate::baker_ops::BAKER_OPS.len()
        );
        assert!(baker_view.baker_ops.iter().any(|o| o.name == "task_create"));

        let old_branch_job = baker_assign_inner(&state, "old job", None, None).expect("assign");
        let Json(branch_job_view) = baker_capabilities(
            State(state),
            Query(CapabilitiesQuery {
                stream_id: Some(old_branch_job.context.stream_id),
            }),
        )
        .await;
        assert!(
            branch_job_view.baker_ops.is_empty(),
            "a branch-job socket is factory-kind but not the permanent Baker"
        );
    }

    // ─── Permanent Baker (07d-B Phase 1) ─────────────────────────────────

    #[test]
    fn permanent_baker_bootstrap_is_idempotent_and_the_pointer_is_durable() {
        let dir = tempfile::tempdir().unwrap();
        let repo = bakery_repo_for_test(dir.path());
        let state = state_with_bakery_root(dir.path(), &repo);

        let a = ensure_permanent_baker(&state, None, None).expect("bootstrap");
        let b = ensure_permanent_baker(&state, None, None).expect("second call");
        assert_eq!(a.stream_id, b.stream_id, "one Baker, not one per call");
        assert!(
            a.stream_id.starts_with("socket_"),
            "opaque server-minted id: {}",
            a.stream_id
        );
        assert_eq!(a.agent_id, format!("{}:baker", a.stream_id));

        // Factory authority is a durable Heart fact, not an inference.
        assert_eq!(
            crate::state::socket_kind(&state.heart.lock().unwrap(), &a.stream_id),
            crate::node::SocketKind::Factory
        );

        // Binds the recorded source root in place, read-scoped.
        assert_eq!(
            PathBuf::from(&a.root).canonicalize().unwrap(),
            repo.canonicalize().unwrap(),
            "permanent Baker binds bakery_source_root as its root"
        );

        // No model configured yet → honestly None, never invented.
        assert_eq!(a.assigned_model_id, None);

        // The pointer survives a restart: a fresh config load from disk
        // (what a new daemon boot does) finds the same stream id.
        let reloaded = NodeConfig::load_or_create(dir.path());
        assert_eq!(
            reloaded.baker_stream_id.as_deref(),
            Some(a.stream_id.as_str())
        );
    }

    #[test]
    fn concurrent_permanent_baker_bootstraps_converge_on_one_stream() {
        let dir = tempfile::tempdir().unwrap();
        let repo = bakery_repo_for_test(dir.path());
        let state = state_with_bakery_root(dir.path(), &repo);

        let s2 = state.clone();
        let racer =
            std::thread::spawn(move || ensure_permanent_baker(&s2, None, None).expect("racer"));
        let a = ensure_permanent_baker(&state, None, None).expect("main");
        let b = racer.join().unwrap();
        assert_eq!(
            a.stream_id, b.stream_id,
            "a lost race must read the winner's stream, not mint its own"
        );

        // Exactly one Baker stream exists in Heart — the real proof the
        // section is atomic, beyond the two calls agreeing.
        let streams = crate::state::known_streams(&state).unwrap();
        let bakers: Vec<_> = streams.iter().filter(|s| s.name == "Baker").collect();
        assert_eq!(
            bakers.len(),
            1,
            "exactly one permanent Baker stream, got {}",
            bakers.len()
        );
    }

    #[test]
    fn a_stale_baker_pointer_missing_from_heart_is_reminted_not_served() {
        let dir = tempfile::tempdir().unwrap();
        let repo = bakery_repo_for_test(dir.path());
        let state = state_with_bakery_root(dir.path(), &repo);

        // A wiped heart.db beside a stale node.json: the config names a
        // stream Heart has never heard of.
        state.node.lock().unwrap().baker_stream_id = Some("socket_gone".into());

        let a = ensure_permanent_baker(&state, None, None).expect("bootstrap");
        assert_ne!(
            a.stream_id, "socket_gone",
            "a dead pointer must not be served"
        );
        assert_eq!(
            state.node.lock().unwrap().baker_stream_id.as_deref(),
            Some(a.stream_id.as_str()),
            "the pointer is repaired in config, not just worked around"
        );
    }

    #[test]
    fn permanent_baker_exists_even_when_the_source_root_is_unknown() {
        let dir = tempfile::tempdir().unwrap();
        // No bakery_source_root configured, no env, no cwd detection.
        let state = state_with(dir.path(), Arc::new(crate::testutil::StubModel));

        let a = ensure_permanent_baker(&state, None, None)
            .expect("identity must not depend on the execution environment");
        assert_eq!(a.root, "", "rootless, honestly — not a fabricated path");
        assert_eq!(
            crate::state::socket_kind(&state.heart.lock().unwrap(), &a.stream_id),
            crate::node::SocketKind::Factory
        );

        // A second call still converges instead of re-minting because a
        // root remains unknown.
        let b = ensure_permanent_baker(&state, None, None).expect("second call");
        assert_eq!(a.stream_id, b.stream_id);
    }
}
