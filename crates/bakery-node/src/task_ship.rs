//! Task-scoped shipping and integration (Checkpoint 1 item 3).
//!
//! The pre-existing `/baker/push|pr|merge` routes resolve the **factory
//! repo** (`baker_repo_ctx`) no matter what is being shipped and guard on
//! `baker/*` branches — a task on any other project could not ship at all,
//! and must never silently target the factory repo. These routes resolve
//! the task's OWN recorded workspace (`socket.workspace.set`: worktree,
//! branch, repo root) and operate there:
//!
//! - `POST /task/push` — push the signed-off task branch to `origin`.
//! - `POST /task/pr` — open a PR for it (requires `gh`).
//! - `POST /task/integrate` — the local, no-`gh` path: an explicit,
//!   confirmed `git merge --no-ff` of the signed-off branch into the
//!   project repo's current checkout.
//!
//! None of them is automatic, and every one refuses unless the task is
//! signed off AND its worktree still IS the signed-off state (HEAD equals
//! the recorded sign-off commit, tree clean) — "branch HEAD exists" is
//! never equated with "reviewed work is committed". Effects are events:
//! `task.pushed`, `task.pr.opened`, `task.integrated` on the parent
//! project's stream.

use crate::state::AppState;
use axum::{extract::State, http::StatusCode, Json};
use serde::{Deserialize, Serialize};

pub(crate) const TASK_PUSHED_FAMILY: &str = "task.pushed";
pub(crate) const TASK_PR_OPENED_FAMILY: &str = "task.pr.opened";
pub(crate) const TASK_INTEGRATED_FAMILY: &str = "task.integrated";

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct TaskShipReq {
    pub(crate) project_id: String,
    pub(crate) task_id: String,
    /// Required by `/task/integrate` only — merging into the operator's
    /// checkout is the one irreversible-feeling step of the three.
    #[serde(default)]
    pub(crate) confirm: bool,
    /// `/task/integrate` only, required: the branch the operator BELIEVES
    /// they are merging into. Verified against the repo's actual checkout
    /// at merge time — a checkout that switched branches between decision
    /// and request refuses instead of silently landing the merge on the
    /// wrong branch (review finding #4: no target assertion).
    #[serde(default)]
    pub(crate) target_branch: Option<String>,
    /// `/task/pr` only.
    #[serde(default)]
    pub(crate) title: Option<String>,
    #[serde(default)]
    pub(crate) body: Option<String>,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct TaskShipResp {
    pub(crate) task_id: String,
    pub(crate) branch: String,
    pub(crate) detail: String,
}

struct ShipCtx {
    task_id: String,
    branch: String,
    repo_root: String,
    worktree_path: String,
    signoff_commit: String,
}

/// Resolve and verify everything every ship op needs. Refuses: unknown or
/// retired task, a task that is not signed off (`Done` + recorded
/// `task.signed_off` commit), a workspace with no `task/*` branch, and a
/// worktree that no longer IS the signed-off state.
fn ship_ctx(state: &AppState, project_id: &str, task_id: &str) -> Result<ShipCtx, String> {
    let task = crate::tasks::find_task(state, project_id, task_id)?;
    if task.retired {
        return Err(format!("task {task_id} has been retired"));
    }
    if task.status != crate::tasks::TaskStatus::Done {
        return Err(format!(
            "task {task_id} is {:?} — only a signed-off (done) task can be shipped/integrated",
            task.status
        ));
    }
    let Some(signoff_commit) =
        crate::task_review::latest_signoff_commit(state, project_id, task_id)
    else {
        return Err(format!("task {task_id} has no recorded sign-off commit — only a signed-off task can be shipped"));
    };
    let ctx = crate::state::context_for(state, &task.stream_id)?;
    if ctx.worktree_path.is_empty() || !std::path::Path::new(&ctx.worktree_path).is_dir() {
        return Err("this task's worktree no longer exists on disk".to_string());
    }
    if !crate::git::is_task_branch(&ctx.branch) {
        return Err(format!(
            "task {task_id}'s recorded branch {:?} is not a task/* branch — refusing to ship it",
            ctx.branch
        ));
    }
    // The reviewed work must still be exactly what would ship: HEAD is the
    // sign-off commit and nothing moved since. A drifted tree means the
    // sign-off is stale (GET /task/review will also flip it back to
    // awaiting_review) — refuse rather than ship unreviewed bytes.
    let wt = std::path::Path::new(&ctx.worktree_path);
    let head = crate::git::run(wt, &["rev-parse", "HEAD"])?
        .trim()
        .to_string();
    if head != signoff_commit {
        return Err(format!(
            "refusing: the task branch HEAD ({}) is not the signed-off commit ({}) — re-review and sign off again",
            &head[..12.min(head.len())],
            &signoff_commit[..12.min(signoff_commit.len())]
        ));
    }
    if !crate::git::porcelain_status(wt)?.is_empty() {
        return Err(
            "refusing: the task worktree has uncommitted changes since sign-off — re-review and sign off again"
                .to_string(),
        );
    }
    if ctx.repo_root.is_empty() {
        return Err(format!(
            "task {task_id} has no recorded project repository root"
        ));
    }
    Ok(ShipCtx {
        task_id: task.task_id,
        branch: ctx.branch,
        repo_root: ctx.repo_root,
        worktree_path: ctx.worktree_path,
        signoff_commit,
    })
}

fn append_ship_event(
    state: &AppState,
    project_id: &str,
    family: &str,
    payload: serde_json::Value,
) -> Result<(), String> {
    crate::state::append(
        state,
        project_id,
        "human",
        family,
        payload.to_string(),
        vec![],
    )
    .map(|_| ())
}

fn task_push(state: &AppState, req: &TaskShipReq) -> Result<TaskShipResp, String> {
    let ctx = ship_ctx(state, &req.project_id, &req.task_id)?;
    // Push from the task's own worktree — it has the branch checked out and
    // shares the repo's refs/remotes.
    let out = crate::git::push_set_upstream(std::path::Path::new(&ctx.worktree_path), &ctx.branch)?;
    append_ship_event(
        state,
        &req.project_id,
        TASK_PUSHED_FAMILY,
        serde_json::json!({
            "task_id": ctx.task_id,
            "branch": ctx.branch,
            "commit": ctx.signoff_commit,
        }),
    )?;
    Ok(TaskShipResp {
        task_id: ctx.task_id,
        branch: ctx.branch,
        detail: out,
    })
}

fn task_pr(state: &AppState, req: &TaskShipReq) -> Result<TaskShipResp, String> {
    let ctx = ship_ctx(state, &req.project_id, &req.task_id)?;
    let info = crate::git::inspect(&ctx.repo_root);
    let base = info
        .default_branch
        .or(info.current_branch)
        .ok_or_else(|| "cannot determine the project repo's base branch for a PR".to_string())?;
    let title = req
        .title
        .clone()
        .unwrap_or_else(|| format!("Task: {}", ctx.task_id));
    let body = req.body.clone().unwrap_or_else(|| {
        format!(
            "Signed-off Bakery task {} (commit {}).",
            ctx.task_id, ctx.signoff_commit
        )
    });
    let out = crate::git::gh_pr_create(
        std::path::Path::new(&ctx.worktree_path),
        &base,
        &ctx.branch,
        &title,
        &body,
    )?;
    append_ship_event(
        state,
        &req.project_id,
        TASK_PR_OPENED_FAMILY,
        serde_json::json!({
            "task_id": ctx.task_id,
            "branch": ctx.branch,
            "commit": ctx.signoff_commit,
            "detail": out,
        }),
    )?;
    Ok(TaskShipResp {
        task_id: ctx.task_id,
        branch: ctx.branch,
        detail: out,
    })
}

fn task_integrate(state: &AppState, req: &TaskShipReq) -> Result<TaskShipResp, String> {
    if !req.confirm {
        return Err(
            "integration requires confirm: true — nothing lands on a base branch without you"
                .to_string(),
        );
    }
    let Some(expected_target) = req
        .target_branch
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return Err(
            "integration requires target_branch — the branch you intend the merge to land on, verified against the repo's actual checkout"
                .to_string(),
        );
    };
    let ctx = ship_ctx(state, &req.project_id, &req.task_id)?;
    let repo = std::path::Path::new(&ctx.repo_root);
    let target = crate::git::run(repo, &["rev-parse", "--abbrev-ref", "HEAD"])?
        .trim()
        .to_string();
    if target != expected_target {
        return Err(format!(
            "refusing: the project checkout is on {target:?}, not the {expected_target:?} you approved merging into — check out the intended branch (or re-approve against the current one)"
        ));
    }
    let merge_head = crate::git::merge_local(
        repo,
        &ctx.branch,
        &format!(
            "integrate task {} ({})",
            ctx.task_id,
            &ctx.signoff_commit[..12.min(ctx.signoff_commit.len())]
        ),
    )?;
    // Prove the integration rather than assume it — the exact check the
    // dependent-task ancestry gate makes before materializing.
    if !crate::git::is_ancestor(repo, &ctx.signoff_commit, &merge_head).unwrap_or(false) {
        return Err(format!(
            "merge completed but the signed-off commit {} is unexpectedly not an ancestor of {} — inspect the repo",
            &ctx.signoff_commit[..12],
            &merge_head[..12]
        ));
    }
    append_ship_event(
        state,
        &req.project_id,
        TASK_INTEGRATED_FAMILY,
        serde_json::json!({
            "task_id": ctx.task_id,
            "branch": ctx.branch,
            "commit": ctx.signoff_commit,
            "merge_commit": merge_head,
            "target_branch": target,
        }),
    )?;
    Ok(TaskShipResp {
        task_id: ctx.task_id,
        branch: ctx.branch,
        detail: format!("merged into {target} as {merge_head}"),
    })
}

pub(crate) async fn task_push_route(
    State(st): State<AppState>,
    Json(req): Json<TaskShipReq>,
) -> Result<Json<TaskShipResp>, (StatusCode, String)> {
    crate::actor::spawn_blocking(move || task_push(&st, &req))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))
}

pub(crate) async fn task_pr_route(
    State(st): State<AppState>,
    Json(req): Json<TaskShipReq>,
) -> Result<Json<TaskShipResp>, (StatusCode, String)> {
    crate::actor::spawn_blocking(move || task_pr(&st, &req))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))
}

pub(crate) async fn task_integrate_route(
    State(st): State<AppState>,
    Json(req): Json<TaskShipReq>,
) -> Result<Json<TaskShipResp>, (StatusCode, String)> {
    crate::actor::spawn_blocking(move || task_integrate(&st, &req))
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

    fn sign_off(state: &AppState, project_id: &str, task: &crate::tasks::TaskView) {
        crate::state::append(
            state,
            project_id,
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
        crate::task_review::sign_off_for_test(state, project_id, &task.task_id);
    }

    fn write_in_worktree(
        state: &AppState,
        task: &crate::tasks::TaskView,
        name: &str,
        content: &str,
    ) {
        let ctx = crate::state::context_for(state, &task.stream_id).unwrap();
        std::fs::write(std::path::Path::new(&ctx.worktree_path).join(name), content).unwrap();
    }

    /// The full local no-`gh` loop: task A signs off → integrate lands its
    /// commit on the project's checked-out base → dependent task B, which
    /// was queued on the ancestry gate, can now start, and its materialized
    /// base actually contains A's signed-off commit.
    #[test]
    fn integrate_lands_the_signoff_commit_and_unblocks_the_dependent() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let (project_id, repo) = project_with_git_root(&state, dir.path());

        let a = start_task(&state, &project_id, "Task A", "wo-int");
        write_in_worktree(&state, &a, "a-work.txt", "task A output\n");
        sign_off(&state, &project_id, &a);
        let a_commit =
            crate::task_review::latest_signoff_commit(&state, &project_id, &a.task_id).unwrap();

        // Dependent B: created now, but its start must refuse — A is done
        // yet its commit is NOT in the base until integration.
        let b = crate::tasks::task_create(
            &state,
            TaskCreateReq {
                parent_project_id: project_id.clone(),
                title: "Task B".into(),
                work_order: "wo-int".into(),
                contract: crate::tasks::TaskContract {
                    depends_on: vec![a.task_id.clone()],
                    ..Default::default()
                },
            },
        )
        .unwrap();
        let err = crate::tasks::task_start(
            &state,
            TaskStartReq {
                project_id: project_id.clone(),
                task_id: b.task.task_id.clone(),
            },
        )
        .unwrap_err();
        assert!(
            err.contains("not in this task's base"),
            "ancestry gate must refuse: {err}"
        );
        let b_row = crate::tasks::find_task(&state, &project_id, &b.task.task_id).unwrap();
        assert_eq!(b_row.status, crate::tasks::TaskStatus::Queued);

        // Integrate A locally (no gh, no remote).
        let resp = task_integrate(
            &state,
            &TaskShipReq {
                project_id: project_id.clone(),
                task_id: a.task_id.clone(),
                confirm: true,
                target_branch: Some("main".into()),
                title: None,
                body: None,
            },
        )
        .unwrap();
        assert!(resp.detail.contains("merged into"), "{}", resp.detail);
        assert!(crate::git::is_ancestor(&repo, &a_commit, "HEAD").unwrap());

        // Now B starts, and its base contains A's signed-off commit.
        let started = crate::tasks::task_start(
            &state,
            TaskStartReq {
                project_id: project_id.clone(),
                task_id: b.task.task_id.clone(),
            },
        )
        .unwrap();
        let base = started.base_commit.expect("B must materialize");
        assert!(
            crate::git::is_ancestor(&repo, &a_commit, &base).unwrap(),
            "B's base must contain A's commit"
        );
        // And A's file is physically present in B's worktree.
        let b_wt = started.worktree_path.expect("B has a worktree");
        assert!(std::path::Path::new(&b_wt).join("a-work.txt").is_file());
    }

    #[test]
    fn integrate_refuses_without_confirm_and_when_the_tree_drifted_from_signoff() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let (project_id, _repo) = project_with_git_root(&state, dir.path());
        let a = start_task(&state, &project_id, "Task A", "wo-drift");
        write_in_worktree(&state, &a, "a.txt", "v1\n");
        sign_off(&state, &project_id, &a);

        let err = task_integrate(
            &state,
            &TaskShipReq {
                project_id: project_id.clone(),
                task_id: a.task_id.clone(),
                confirm: false,
                target_branch: Some("main".into()),
                title: None,
                body: None,
            },
        )
        .unwrap_err();
        assert!(err.contains("confirm"), "{err}");

        // Drift after sign-off: the worktree no longer IS the reviewed state.
        write_in_worktree(&state, &a, "a.txt", "v2 sneaks in\n");
        let err = task_integrate(
            &state,
            &TaskShipReq {
                project_id: project_id.clone(),
                task_id: a.task_id.clone(),
                confirm: true,
                target_branch: Some("main".into()),
                title: None,
                body: None,
            },
        )
        .unwrap_err();
        assert!(
            err.contains("uncommitted changes") || err.contains("not the signed-off commit"),
            "drift must refuse integration: {err}"
        );
    }

    #[test]
    fn ship_ops_refuse_a_task_that_is_not_signed_off() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let (project_id, _repo) = project_with_git_root(&state, dir.path());
        let a = start_task(&state, &project_id, "Task A", "wo-notdone");
        for op in [task_push, task_pr, task_integrate] {
            let err = op(
                &state,
                &TaskShipReq {
                    project_id: project_id.clone(),
                    task_id: a.task_id.clone(),
                    confirm: true,
                    target_branch: Some("main".into()),
                    title: None,
                    body: None,
                },
            )
            .unwrap_err();
            assert!(
                err.contains("signed-off") || err.contains("Running"),
                "{err}"
            );
        }
    }

    /// The repo-resolution correction: ship ops act on the TASK's recorded
    /// project repo (proven physically by the integrate test above landing
    /// the merge in the scratch project repo, never the factory checkout).
    /// The shared predicate keeps arbitrary branches refused everywhere
    /// while the factory routes stay baker/*-only.
    #[test]
    fn branch_predicates_cover_task_branches_without_widening_baker_routes() {
        assert!(!crate::git::is_baker_branch("task/x-1"));
        assert!(crate::git::is_task_branch("task/x-1"));
        assert!(crate::git::is_bakery_derived_branch("task/x-1"));
        assert!(crate::git::is_bakery_derived_branch("baker/y"));
        assert!(!crate::git::is_bakery_derived_branch("main"));
        assert!(!crate::git::is_bakery_derived_branch("feature/z"));
    }
}
