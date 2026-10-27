//! The sidebar's control panel, one call per Project: each of its repos'
//! open pull requests and latest CI run per branch and workflow, through the
//! operator's own `gh` login (the node holds no GitHub token); and each of
//! its worktrees' branch, the lines added and removed since it forked from
//! its repo's default branch (uncommitted work included), and whether it has
//! uncommitted changes. Reads only.

use crate::routes::git::{checkout, git, status};
use crate::state::AppState;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeSet, HashSet};
use std::path::Path;

const PR_FIELDS: &str = "number,title,headRefName,isDraft,statusCheckRollup,reviewDecision,updatedAt,url";
const RUN_FIELDS: &str = "headBranch,workflowName,displayTitle,status,conclusion,createdAt,url";

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct RollupQuery {
    pub(crate) project_id: String,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct ProjectRollup {
    pub(crate) project_id: String,
    pub(crate) repos: Vec<RepoRollup>,
    pub(crate) worktrees: Vec<WorktreeRollup>,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct RepoRollup {
    /// The repo's own working tree; each worktree names it as its `repo`.
    pub(crate) path: String,
    /// Its open pull requests, newest first.
    pub(crate) prs: Vec<PullRequest>,
    /// The latest run of each workflow on each branch, newest first.
    pub(crate) runs: Vec<Run>,
    /// Why GitHub couldn't answer for this repo (gh missing or signed out,
    /// no GitHub remote); the rest of the rollup still did.
    pub(crate) problem: Option<String>,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct PullRequest {
    pub(crate) number: u64,
    pub(crate) title: String,
    pub(crate) branch: String,
    pub(crate) draft: bool,
    /// pass, fail, pending, or none (no checks).
    pub(crate) checks: &'static str,
    /// APPROVED, CHANGES_REQUESTED, REVIEW_REQUIRED, or empty.
    pub(crate) review: String,
    pub(crate) updated_at: String,
    pub(crate) url: String,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct Run {
    pub(crate) branch: String,
    pub(crate) workflow: String,
    pub(crate) title: String,
    /// queued, in_progress or completed.
    pub(crate) status: String,
    /// success, failure, cancelled…; empty until it completes.
    pub(crate) conclusion: String,
    pub(crate) created_at: String,
    pub(crate) url: String,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct WorktreeRollup {
    pub(crate) stream_id: String,
    /// The repo it belongs to (a `repos[].path`).
    pub(crate) repo: Option<String>,
    /// None on a detached HEAD.
    pub(crate) branch: Option<String>,
    /// The branch it is counted against: the one it forked from, or its
    /// repo's main for a worktree Bakery didn't make (or made before it kept
    /// a record of this).
    pub(crate) base: Option<String>,
    pub(crate) added: usize,
    pub(crate) removed: usize,
    /// It has uncommitted changes.
    pub(crate) dirty: bool,
    /// Commits on its base it doesn't have yet (rule 3: "N behind"),
    /// counted as of `fetched`.
    pub(crate) behind: usize,
    /// When its repository was last fetched, and why it failed if it did;
    /// none since the node started.
    pub(crate) fetched: Option<crate::git::Fetched>,
    /// Other open threads on its repository changing some of the same files
    /// (git rule 5), as of the last turn's end.
    pub(crate) also_changing: Vec<crate::overlaps::Overlap>,
}

fn text(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

/// One verdict for a pull request's checks: any failure fails it; else
/// anything unfinished leaves it pending; else it passed.
fn checks_verdict(checks: &Value) -> &'static str {
    let checks = checks.as_array().map(Vec::as_slice).unwrap_or_default();
    if checks.is_empty() {
        return "none";
    }
    // A check run has a status and, once done, a conclusion; a commit
    // status has only a state.
    fn outcome(c: &Value) -> &str {
        c["conclusion"].as_str().filter(|s| !s.is_empty()).or(c["state"].as_str()).unwrap_or("")
    }
    let failed = |c: &Value| {
        matches!(outcome(c), "FAILURE" | "ERROR" | "CANCELLED" | "TIMED_OUT" | "ACTION_REQUIRED" | "STARTUP_FAILURE")
    };
    let unfinished = |c: &Value| {
        matches!(outcome(c), "" | "PENDING" | "EXPECTED") || c["status"].as_str().is_some_and(|s| s != "COMPLETED")
    };
    if checks.iter().any(failed) {
        "fail"
    } else if checks.iter().any(unfinished) {
        "pending"
    } else {
        "pass"
    }
}

fn pull_requests(list: &Value) -> Vec<PullRequest> {
    list.as_array()
        .into_iter()
        .flatten()
        .map(|p| PullRequest {
            number: p["number"].as_u64().unwrap_or_default(),
            title: text(&p["title"]),
            branch: text(&p["headRefName"]),
            draft: p["isDraft"] == true,
            checks: checks_verdict(&p["statusCheckRollup"]),
            review: text(&p["reviewDecision"]),
            updated_at: text(&p["updatedAt"]),
            url: text(&p["url"]),
        })
        .collect()
}

/// `gh run list` answers newest first, so the first run of each workflow on
/// each branch is its latest; one workflow's failure can't hide behind
/// another's later pass.
fn latest_runs(list: &Value) -> Vec<Run> {
    let mut seen = HashSet::new();
    list.as_array()
        .into_iter()
        .flatten()
        .filter(|r| seen.insert((text(&r["headBranch"]), text(&r["workflowName"]))))
        .map(|r| Run {
            branch: text(&r["headBranch"]),
            workflow: text(&r["workflowName"]),
            title: text(&r["displayTitle"]),
            status: text(&r["status"]),
            conclusion: text(&r["conclusion"]),
            created_at: text(&r["createdAt"]),
            url: text(&r["url"]),
        })
        .collect()
}

fn repo(path: String) -> RepoRollup {
    use crate::git::gh_json as gh;
    let dir = Path::new(&path);
    let found = gh(dir, &["pr", "list", "--state", "open", "--limit", "100", "--json", PR_FIELDS])
        .and_then(|prs| Ok((prs, gh(dir, &["run", "list", "--limit", "100", "--json", RUN_FIELDS])?)));
    let (prs, runs, problem) = match found {
        Ok((prs, runs)) => (pull_requests(&prs), latest_runs(&runs), None),
        Err(problem) => (Vec::new(), Vec::new(), Some(problem)),
    };
    RepoRollup { path, prs, runs, problem }
}

/// None for a member with no checkout (a chat, or a worktree since removed).
fn worktree(state: &AppState, stream_id: String) -> Option<WorktreeRollup> {
    let status = status(state, &stream_id).ok()?;
    let dir = checkout(state, &stream_id).ok()?;
    let path = dir.to_string_lossy();
    let info = crate::git::inspect(&path);
    // What it forked from (rule 3).
    let base = crate::routes::git::fork_base(state, &stream_id, &dir);
    let (added, removed) = git(&dir, &["merge-base", "HEAD", &base])
        .ok()
        .and_then(|fork| crate::task_review::diff_counts(&path, fork.trim()).ok())
        .unwrap_or((0, 0));
    let behind = git(&dir, &["rev-list", "--count", &format!("HEAD..{base}")])
        .ok()
        .and_then(|n| n.trim().parse().ok())
        .unwrap_or(0);
    let also_changing = crate::overlaps::current(state, &stream_id);
    Some(WorktreeRollup {
        stream_id,
        repo: info.root,
        branch: status.branch,
        base: Some(base),
        added,
        removed,
        dirty: !status.clean,
        behind,
        fetched: crate::git::last_fetch(&dir),
        also_changing,
    })
}

pub(crate) fn rollup(state: &AppState, q: &RollupQuery) -> Result<ProjectRollup, String> {
    crate::catalog::project_by_id(state, &q.project_id).ok_or(format!("no project {}", q.project_id))?;
    let members: Vec<String> = {
        let heart = state.heart.lock().unwrap();
        let streams = heart.list_streams().map_err(|e| e.to_string())?;
        streams
            .into_iter()
            .map(|s| s.stream_id)
            .filter(|sid| crate::state::socket_project(&heart, sid).0.as_deref() == Some(q.project_id.as_str()))
            .collect()
    };
    let worktrees: Vec<WorktreeRollup> = members.into_iter().filter_map(|sid| worktree(state, sid)).collect();
    // gh once per repo, however many of its worktrees the Project holds.
    let repos: BTreeSet<String> = worktrees.iter().filter_map(|w| w.repo.clone()).collect();
    Ok(ProjectRollup { project_id: q.project_id.clone(), repos: repos.into_iter().map(repo).collect(), worktrees })
}

pub(crate) async fn rollup_route(
    State(st): State<AppState>,
    Query(q): Query<RollupQuery>,
) -> Result<Json<ProjectRollup>, (StatusCode, String)> {
    crate::actor::spawn_blocking(move || rollup(&st, &q))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::project::{open_stream, OpenReq};
    use crate::testutil::*;
    use bakery_agent::StubModel;
    use serde_json::json;
    use std::sync::Arc;

    /// Rule 3: a thread counts what the branch it forked from gained since as
    /// "N behind", as of its repository's last fetch; Update merges it in,
    /// and a conflict stops it with the files to resolve.
    #[test]
    fn a_thread_counts_how_far_behind_it_is_and_updates_by_a_merge() {
        use crate::routes::git::{fetch, update, FetchReq, GitQuery};
        let dir = tempfile::tempdir().unwrap();
        let local = crate::git::tests::behind_its_origin(dir.path());
        let (up, origin) = (dir.path().join("up"), dir.path().join("origin.git").to_string_lossy().into_owned());
        let state = state_with(dir.path(), Arc::new(StubModel));
        let ctx = open_stream(&state, &open_req(&local, "behind")).unwrap().context;
        let sid = ctx.stream_id.clone();
        let row = || worktree(&state, sid.clone()).unwrap();
        assert_eq!((row().behind, row().base.as_deref()), (0, Some("origin/main")));

        std::fs::write(up.join("later.txt"), "later\n").unwrap();
        run_git(&up, &["add", "."]);
        run_git(&up, &["commit", "-qm", "later"]);
        run_git(&up, &["push", "-q", &origin, "main"]);
        fetch(&state, &FetchReq { stream_id: sid.clone(), remote: None }).unwrap();
        let after = row();
        assert_eq!(after.behind, 1, "counted after a fetch");
        assert!(after.fetched.is_some_and(|f| f.error.is_none()), "and when it was fetched");

        let done = update(&state, &GitQuery { stream_id: sid.clone() }).unwrap();
        assert!(done.merged && done.conflicts.is_empty(), "{done:?}");
        assert_eq!(row().behind, 0);

        let wt = std::path::Path::new(&ctx.worktree_path);
        std::fs::write(wt.join("f.txt"), "mine\n").unwrap();
        run_git(wt, &["commit", "-qam", "mine"]);
        std::fs::write(up.join("f.txt"), "theirs\n").unwrap();
        run_git(&up, &["commit", "-qam", "theirs"]);
        run_git(&up, &["push", "-q", &origin, "main"]);
        let stopped = update(&state, &GitQuery { stream_id: sid.clone() }).unwrap();
        assert_eq!((stopped.merged, stopped.conflicts.as_slice()), (false, &["f.txt".to_string()][..]), "{stopped:?}");
        assert!(crate::git::run(wt, &["rev-parse", "-q", "--verify", "MERGE_HEAD"]).is_ok(), "left to resolve");
    }

    /// A thread that forked origin's main follows the newer main, as new work
    /// forks: once the local main is ahead with work not pushed, it counts
    /// itself behind that and Update merges it in. Its push is still shown
    /// against origin's main, since that work goes out with it.
    #[test]
    fn a_thread_forked_from_origins_main_follows_the_local_main_once_it_is_ahead() {
        use crate::routes::git::{push_preview, update, GitQuery};
        let dir = tempfile::tempdir().unwrap();
        let local = crate::git::tests::behind_its_origin(dir.path());
        let state = state_with(dir.path(), Arc::new(StubModel));
        let ctx = open_stream(&state, &open_req(&local, "ui-qa")).unwrap().context;
        let sid = ctx.stream_id.clone();
        run_git(&local, &["merge", "-q", "--ff-only", "origin/main"]);
        std::fs::write(local.join("only-here.txt"), "mine\n").unwrap();
        run_git(&local, &["add", "."]);
        run_git(&local, &["commit", "-qm", "not pushed"]);

        let row = worktree(&state, sid.clone()).unwrap();
        assert_eq!((row.behind, row.base.as_deref()), (1, Some("main")));
        let done = update(&state, &GitQuery { stream_id: sid.clone() }).unwrap();
        assert_eq!((done.base.as_str(), done.merged), ("main", true), "{done:?}");
        assert!(Path::new(&ctx.worktree_path).join("only-here.txt").is_file());
        assert_eq!(worktree(&state, sid.clone()).unwrap().behind, 0);

        let push = push_preview(&state, &sid, None).unwrap();
        assert_eq!(push.base, "origin/main");
        assert!(push.commits.iter().any(|c| c.subject == "not pushed"), "{:?}", push.commits);
    }

    #[test]
    fn a_pull_requests_checks_come_down_to_one_verdict() {
        let run = |status: &str, conclusion: &str| json!({"__typename": "CheckRun", "status": status, "conclusion": conclusion});
        let context = |state: &str| json!({"__typename": "StatusContext", "state": state});
        assert_eq!(checks_verdict(&json!([])), "none");
        assert_eq!(checks_verdict(&json!([run("COMPLETED", "SUCCESS"), context("SUCCESS")])), "pass");
        assert_eq!(checks_verdict(&json!([run("COMPLETED", "SUCCESS"), run("IN_PROGRESS", "")])), "pending");
        assert_eq!(checks_verdict(&json!([run("IN_PROGRESS", ""), context("FAILURE")])), "fail", "a failure outranks pending");
        assert_eq!(checks_verdict(&json!([run("COMPLETED", "SKIPPED"), context("PENDING")])), "pending");
    }

    #[test]
    fn each_workflow_on_each_branch_keeps_only_its_latest_run() {
        let run = |branch: &str, workflow: &str, conclusion: &str| {
            json!({"headBranch": branch, "workflowName": workflow, "conclusion": conclusion, "status": "completed"})
        };
        // Newest first, as gh lists them.
        let runs = latest_runs(&json!([
            run("w2", "CI", "failure"),
            run("w2", "Lint", "success"),
            run("w2", "CI", "success"),
            run("main", "CI", "success"),
        ]));
        let got: Vec<String> = runs.iter().map(|r| format!("{} {} {}", r.branch, r.workflow, r.conclusion)).collect();
        assert_eq!(got, ["w2 CI failure", "w2 Lint success", "main CI success"]);
    }

    #[test]
    fn a_project_rollup_counts_each_worktrees_changes_since_it_forked() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let ctx = open_stream(&state, &open_req(&repo, "roll")).unwrap().context;
        // A Project also mints a chat socket with no checkout; it isn't listed.
        let (project, _) = crate::catalog::create_project(&state, "Work", None).unwrap();
        crate::catalog::link_socket_to_project(&state, &ctx.stream_id, &project.project_id, &ctx.worktree_path, true).unwrap();
        let work = Path::new(&ctx.worktree_path);
        std::fs::write(work.join("new.txt"), "one\ntwo\n").unwrap();
        crate::git::run(work, &["add", "new.txt"]).unwrap();
        crate::git::run(work, &["commit", "-qm", "two lines"]).unwrap();
        std::fs::write(work.join("f.txt"), "line one\nmore\n").unwrap();

        let got = rollup(&state, &RollupQuery { project_id: project.project_id.clone() }).unwrap();
        assert_eq!(got.worktrees.len(), 1, "{got:?}");
        let wt = &got.worktrees[0];
        assert_eq!((wt.branch.as_deref(), wt.base.as_deref()), (Some("roll"), Some("main")));
        assert_eq!((wt.added, wt.removed, wt.dirty), (3, 0, true), "two committed lines and one uncommitted");
        assert_eq!(got.repos.len(), 1);
        assert_eq!(Some(&got.repos[0].path), wt.repo.as_ref());
        assert!(got.repos[0].problem.is_some(), "no GitHub remote: said, not failed: {:?}", got.repos[0]);
        assert!(rollup(&state, &RollupQuery { project_id: "nope".into() }).is_err());

        // One forked from another branch is counted against that branch.
        crate::git::run(&repo, &["branch", "dev"]).unwrap();
        let req = OpenReq { base_branch: Some("dev".into()), ..open_req(&repo, "from-dev") };
        let dev = open_stream(&state, &req).unwrap().context;
        crate::catalog::link_socket_to_project(&state, &dev.stream_id, &project.project_id, &dev.worktree_path, true).unwrap();
        let got = rollup(&state, &RollupQuery { project_id: project.project_id.clone() }).unwrap();
        let wt = got.worktrees.iter().find(|w| w.stream_id == dev.stream_id).unwrap();
        assert_eq!(wt.base.as_deref(), Some("dev"));
    }
}
