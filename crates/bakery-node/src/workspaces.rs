//! Workspaces: the Bakery's own project is the Baker, and its parallel work
//! runs in workspaces beneath it; a catalog project's runs in workspaces of
//! its own. A workspace is an orchestrator's conversation, its orc, on a
//! worktree of its own: a new branch off its repository's main.
//! The workers it opens are its peers (`baker_threads::create_peer`), each
//! on a worktree of its branch. A worker's branch goes into the workspace's
//! when the operator approves the orc's proposal in the merge review, and
//! the workspace's branch goes to main as a chef's does. The main checkout
//! stays the Baker's, only read by agents.

use crate::node::Seat;
use crate::state::AppState;
use axum::{extract::State, http::StatusCode, Json};
use bakery_heart::Heart;
use serde::{Deserialize, Serialize};

/// An orc's conversation was opened as a workspace's:
/// `{baker_stream_id, title, project_id}`, once, when it is made.
pub(crate) const WORKSPACE_OPENED: &str = "workspace.opened";

/// Whether `stream_id` is a workspace's orc, and not discarded.
pub(crate) fn is_workspace_locked(heart: &Heart, stream_id: &str) -> bool {
    heart.first_of_family(stream_id, WORKSPACE_OPENED).ok().flatten().is_some() && !crate::state::discarded_of(heart, stream_id)
}

pub(crate) fn is_workspace(state: &AppState, stream_id: &str) -> bool {
    is_workspace_locked(&state.heart.lock().unwrap(), stream_id)
}

/// The workspace a conversation is in, by its orc: the orc itself, or the
/// orc that opened it as a worker.
pub(crate) fn workspace_of_locked(heart: &Heart, stream_id: &str) -> Option<String> {
    if is_workspace_locked(heart, stream_id) {
        return Some(stream_id.to_string());
    }
    crate::baker_threads::opened_by_locked(heart, stream_id).filter(|orc| is_workspace_locked(heart, orc))
}

/// Whether a conversation is in one of the Bakery's own workspaces (one
/// opened for no catalog project): its orc, or anything opened under it,
/// however many openings deep.
pub(crate) fn in_bakery_workspace_locked(heart: &Heart, stream_id: &str) -> bool {
    let mut at = stream_id.to_string();
    let mut seen = std::collections::HashSet::new();
    while seen.insert(at.clone()) {
        if let Some(opened) = heart.first_of_family(&at, WORKSPACE_OPENED).ok().flatten() {
            let ours = serde_json::from_str::<serde_json::Value>(&opened.payload).is_ok_and(|fact| fact["project_id"].is_null());
            return ours && !crate::state::discarded_of(heart, &at);
        }
        match crate::baker_threads::opened_by_locked(heart, &at) {
            Some(by) => at = by,
            None => return false,
        }
    }
    false
}

pub(crate) fn workspace_of(state: &AppState, stream_id: &str) -> Option<String> {
    workspace_of_locked(&state.heart.lock().unwrap(), stream_id)
}

/// Whether `orc` is a workspace's orc and `stream_id` one of its workers.
pub(crate) fn is_orc_of(state: &AppState, orc: &str, stream_id: &str) -> bool {
    orc != stream_id && workspace_of(state, stream_id).as_deref() == Some(orc)
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct CreateWorkspaceReq {
    /// What it is called; its branch is named from it.
    pub(crate) title: String,
    /// A catalog project's workspace, on a worktree of the project's
    /// repository; the Bakery's own, beneath the Baker, when absent.
    #[serde(default)]
    pub(crate) project_id: Option<String>,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct Workspace {
    /// Its orc's conversation.
    pub(crate) stream_id: String,
    pub(crate) title: String,
    pub(crate) worktree_path: String,
    pub(crate) branch: String,
}

/// A new workspace: its orc's conversation on a worktree of its own, a new
/// branch off its repository's main, freshly fetched, on the runtime the
/// Baker is on. The Bakery's own beneath the Baker, or a catalog project's,
/// in that project.
pub(crate) fn create_workspace(state: &AppState, title: &str, project_id: Option<&str>, actor: &str) -> Result<Workspace, String> {
    let title = crate::baker_threads::clean_title(title).ok_or("a workspace needs a name")?;
    let home = crate::baker_threads::baker_pointer(state).ok_or("there is no Baker yet: open it first")?;
    let repo = match project_id {
        None => crate::state::context_for(state, &home)?.repo_root,
        Some(project_id) => project_repo(state, project_id)?,
    };
    let id = crate::node::mint_id("workspace");
    let slug: String = crate::git::sanitize_name(&title.to_lowercase()).chars().take(40).collect();
    let name = format!("{slug}-{}", &id[id.len().saturating_sub(6)..]);
    let opened = crate::routes::project::open_stream(
        state,
        &crate::routes::project::OpenReq {
            repo,
            name: Some(name.clone()),
            branch: Some(format!("workspace/{name}")),
            base_branch: None,
            worktree_path: None,
            project_id: project_id.map(str::to_string),
            tools: None,
        },
    )?;
    if opened.attached {
        return Err("a conversation by that name is already open: try again".to_string());
    }
    let orc = opened.context.stream_id.clone();
    if let Some(project_id) = project_id {
        crate::catalog::link_socket_to_project(state, &orc, project_id, &opened.context.worktree_path, true)?;
    }
    let fact = serde_json::json!({ "baker_stream_id": home, "title": title, "project_id": project_id });
    crate::state::append(state, &orc, actor, WORKSPACE_OPENED, fact.to_string(), vec![opened.event.event_hash.clone()])?;
    let named = serde_json::json!({ "title": title });
    crate::state::append(state, &orc, actor, crate::state::CONVERSATION_RENAMED_FAMILY, named.to_string(), vec![])?;
    crate::baker_threads::inherit_seat(state, (&home, Seat::Baker), (&orc, Seat::Orchestrator));
    Ok(Workspace { stream_id: orc, title, worktree_path: opened.context.worktree_path, branch: opened.context.branch })
}

/// The git repository a catalog project's workspaces work on: its first
/// root that is one.
fn project_repo(state: &AppState, project_id: &str) -> Result<String, String> {
    let heart = state.heart.lock().unwrap();
    if crate::catalog::is_retired(&heart, project_id) {
        return Err(format!("project {project_id} is retired"));
    }
    let project = crate::catalog::project_record_locked(&heart, project_id).ok_or_else(|| format!("unknown project {project_id}"))?;
    let repo = project.roots.into_iter().find(|root| root.is_git).map(|root| root.path);
    repo.ok_or_else(|| format!("{} has no git repository: a workspace works on a worktree of one", project.name))
}

/// The operator opens a workspace, or the Baker does; no other agent.
fn may_open(state: &AppState) -> Result<(), String> {
    let baker = crate::messages::sender(&crate::actor::attribute("human")).is_some_and(|s| crate::baker_ops::is_baker_scope_stream(state, &s));
    if crate::actor::agent_calling() && !baker {
        return Err("opening a workspace is the operator's or the Baker's to do".to_string());
    }
    Ok(())
}

/// `POST /workspace`: the operator, or the Baker, opens a workspace.
pub(crate) async fn create_route(
    State(st): State<AppState>,
    Json(req): Json<CreateWorkspaceReq>,
) -> Result<Json<Workspace>, (StatusCode, String)> {
    crate::actor::spawn_blocking(move || {
        may_open(&st).map_err(|e| (StatusCode::FORBIDDEN, e))?;
        create_workspace(&st, &req.title, req.project_id.as_deref(), "human").map_err(|e| (StatusCode::BAD_REQUEST, e))
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map(Json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;

    /// A workspace's orc works on a worktree of its own, a new branch off
    /// main, as an orchestrator on the Baker's runtime, never in the main
    /// checkout; its workers are its peers, each on a worktree of its branch,
    /// and an agent's peer may not adopt a worktree that already exists.
    #[test]
    fn a_workspace_is_an_orc_on_its_own_branch_and_its_workers_are_its_peers() {
        let dir = tempfile::tempdir().unwrap();
        let repo = bakery_repo_for_test(dir.path());
        run_git(&repo, &["add", "."]);
        run_git(&repo, &["commit", "-qm", "the bakery"]);
        let state = state_with_bakery_root(dir.path(), &repo);
        let home = crate::routes::baker::ensure_permanent_baker(&state, None, None).unwrap().stream_id;
        crate::routes::seats::seat_assign_checked(&state, &home, Seat::Baker, "stub").unwrap();

        let made = create_workspace(&state, "Bakery", None, "human").unwrap();
        assert!(is_workspace(&state, &made.stream_id));
        assert!(made.branch.starts_with("workspace/bakery-"), "{}", made.branch);
        let main = crate::state::context_for(&state, &home).unwrap();
        assert!(!crate::state::same_path(&made.worktree_path, &main.worktree_path), "never the main checkout");
        assert!(crate::tools::commands_write_workspace(&state, &made.stream_id), "its own worktree, written by its agents");
        let ctx = crate::state::context_for(&state, &made.stream_id).unwrap();
        assert_eq!(crate::routes::agent::default_seat_for(&state, &ctx), Seat::Orchestrator);
        let model = crate::state::latest_seat_model_id(&state.heart.lock().unwrap(), &made.stream_id, Seat::Orchestrator).unwrap();
        assert_eq!(model.as_deref(), Some("stub"), "on the Baker's runtime");
        let listed = crate::state::known_streams(&state).unwrap();
        assert_eq!(listed.iter().find(|s| s.stream_id == made.stream_id).unwrap().title.as_deref(), Some("Bakery"));
        assert!(create_workspace(&state, "  ", None, "human").is_err());

        let req = crate::baker_threads::CreateThreadReq { title: Some("worker one".into()), ..Default::default() };
        let worker = crate::baker_threads::create_peer(&state, &req, &made.stream_id, "human").unwrap().thread.stream_id;
        assert_eq!(crate::baker_threads::opened_by(&state, &worker).as_deref(), Some(made.stream_id.as_str()));
        let theirs = crate::state::context_for(&state, &worker).unwrap();
        let fork = crate::git::run(std::path::Path::new(&theirs.worktree_path), &["merge-base", "--is-ancestor", &made.branch, "HEAD"]);
        assert!(fork.is_ok(), "cut from the workspace's branch");

        let w1 = dir.path().join("w1");
        run_git(&repo, &["worktree", "add", "-q", "-b", "w1/baker-hands", w1.to_str().unwrap()]);
        let adopt = crate::baker_threads::CreateThreadReq { adopt: Some(w1.display().to_string()), ..Default::default() };
        let by_agent = crate::actor::with("agent:orchestrator".into(), || crate::baker_threads::create_peer(&state, &adopt, &made.stream_id, "human"));
        assert!(by_agent.unwrap_err().contains("only the operator"), "an agent never adopts a worktree");
        let adopted = crate::baker_threads::create_peer(&state, &adopt, &made.stream_id, "human").unwrap().thread.stream_id;
        let kept = crate::state::context_for(&state, &adopted).unwrap();
        assert_eq!(std::fs::canonicalize(&kept.worktree_path).unwrap(), std::fs::canonicalize(&w1).unwrap());
        assert_eq!(kept.branch, "w1/baker-hands", "on the branch it has");
        assert!(crate::tools::commands_write_workspace(&state, &adopted), "its own, written by its agents");
    }

    /// A catalog project has workspaces too: an orc on a worktree of the
    /// project's repository, its workers in the project, a worker's branch
    /// into the workspace on the operator's word, and the workspace's into
    /// the project's own main, never the Bakery's. The Baker opens a
    /// workspace's agents; no agent but the Baker opens a workspace.
    #[test]
    fn a_projects_workspace_merges_into_its_own_main_and_the_baker_opens_them() {
        use std::path::{Path, PathBuf};
        let dir = tempfile::tempdir().unwrap();
        let repo = bakery_repo_for_test(dir.path());
        run_git(&repo, &["add", "."]);
        run_git(&repo, &["commit", "-qm", "the bakery"]);
        let state = state_with_bakery_root(dir.path(), &repo);
        let home = crate::routes::baker::ensure_permanent_baker(&state, None, None).unwrap().stream_id;
        let local = make_repo(&dir.path().join("local"));
        let root = crate::catalog::DirectoryRootSpec::Attach(local.to_str().unwrap());
        let (project, _) = crate::catalog::create_project(&state, "Local", Some(root)).unwrap();

        let made = create_workspace(&state, "Styling", Some(&project.project_id), "human").unwrap();
        let project_of = |s: &str| crate::state::socket_project(&state.heart.lock().unwrap(), s).0;
        assert_eq!(project_of(&made.stream_id), Some(project.project_id.clone()), "in the project");
        assert!(crate::git::same_repository(&local, Path::new(&made.worktree_path)), "a worktree of the project's repository");
        assert!(made.branch.starts_with("workspace/styling-"), "{}", made.branch);

        let args = serde_json::json!({"stream_id": made.stream_id, "title": "buttons"});
        let created: serde_json::Value = serde_json::from_str(&crate::baker_ops::dispatch_baker_op_as(&state, &home, "thread_create", &args).unwrap()).unwrap();
        let worker = created["thread"]["stream_id"].as_str().unwrap().to_string();
        assert_eq!(crate::baker_threads::opened_by(&state, &worker).as_deref(), Some(made.stream_id.as_str()), "the Baker opened it under the orc");
        assert_eq!(project_of(&worker), Some(project.project_id.clone()));

        let theirs = PathBuf::from(crate::state::context_for(&state, &worker).unwrap().worktree_path);
        std::fs::write(theirs.join("buttons.css"), "a {}\n").unwrap();
        run_git(&theirs, &["add", "."]);
        run_git(&theirs, &["commit", "-qm", "buttons"]);
        crate::baker_threads::ready_to_merge(&state, &worker, None, "agent:orchestrator").unwrap();
        crate::baker_threads::merge_thread(&state, &worker, None, "human").unwrap();
        let up = crate::baker_threads::ready_to_merge(&state, &made.stream_id, None, "agent:orchestrator").unwrap();
        assert_eq!((up.base.as_str(), up.orc), ("main", None));
        assert!(!up.into_stream_id.is_empty(), "the project's own conversation on its main checkout");
        let baker = crate::baker_threads::merge_thread(&state, &made.stream_id, None, "agent:baker").unwrap_err();
        assert!(baker.contains("operator approves"), "{baker}");
        let merged = crate::baker_threads::merge_thread(&state, &made.stream_id, None, "human").unwrap();
        assert_eq!(merged.into, "main");
        assert!(local.join("buttons.css").exists(), "into the project's main");
        assert!(!repo.join("buttons.css").exists(), "never the Bakery's");

        let as_orc = crate::actor::with(crate::actor::agent(&made.stream_id, Seat::Orchestrator), || may_open(&state));
        assert!(as_orc.unwrap_err().contains("the operator's or the Baker's"));
        assert!(crate::actor::with(crate::actor::agent(&home, Seat::Baker), || may_open(&state)).is_ok(), "the Baker");
        assert!(may_open(&state).is_ok(), "the operator");
    }

    /// No agent brings a workspace's work in by itself: the orc's rebase
    /// onto its worker's branch, or the Baker's merge of it, by name or by
    /// its tip, is refused; the operator's own git is not. A workspace's
    /// peers count toward the operator's limit together.
    #[test]
    fn a_workspaces_work_goes_in_only_through_the_review() {
        let dir = tempfile::tempdir().unwrap();
        let repo = bakery_repo_for_test(dir.path());
        run_git(&repo, &["add", "."]);
        run_git(&repo, &["commit", "-qm", "the bakery"]);
        let state = state_with_bakery_root(dir.path(), &repo);
        let home = crate::routes::baker::ensure_permanent_baker(&state, None, None).unwrap().stream_id;
        let made = create_workspace(&state, "Bakery", None, "human").unwrap();
        let req = crate::baker_threads::CreateThreadReq { title: Some("one".into()), ..Default::default() };
        let worker = crate::baker_threads::create_peer(&state, &req, &made.stream_id, "human").unwrap().thread.stream_id;
        let theirs = crate::state::context_for(&state, &worker).unwrap();
        std::fs::write(std::path::Path::new(&theirs.worktree_path).join("w.md"), "w\n").unwrap();
        run_git(std::path::Path::new(&theirs.worktree_path), &["add", "."]);
        run_git(std::path::Path::new(&theirs.worktree_path), &["commit", "-qm", "w"]);
        let tip = crate::git::run(std::path::Path::new(&theirs.worktree_path), &["rev-parse", "HEAD"]).unwrap().trim().to_string();

        let as_orc = |onto: &str| {
            let req = crate::routes::git::RebaseReq { stream_id: made.stream_id.clone(), onto: onto.to_string(), reason: "catch up".into() };
            crate::actor::with(crate::actor::agent(&made.stream_id, Seat::Orchestrator), || crate::routes::git::rebase(&state, &req))
        };
        assert!(as_orc(&theirs.branch).unwrap_err().contains("a workspace's work"), "by name");
        assert!(as_orc(&tip).unwrap_err().contains("a workspace's work"), "by its tip");
        let merge = crate::routes::git::MergeReq { stream_id: made.stream_id.clone(), branch: theirs.branch.clone(), message: None };
        let by_baker = crate::actor::with(crate::actor::agent(&home, Seat::Baker), || crate::routes::git::merge(&state, &merge));
        assert!(by_baker.unwrap_err().contains("a workspace's work"), "the Baker's merge too");
        assert!(crate::routes::git::merge(&state, &merge).is_ok(), "the operator's own git");

        state.node.lock().unwrap().peer_limit = Some(1);
        let more = crate::baker_threads::CreateThreadReq { title: Some("two".into()), ..Default::default() };
        let full = crate::baker_threads::create_peer(&state, &more, &made.stream_id, "human").unwrap_err();
        assert!(full.contains("this workspace is at the operator's limit of 1 open peers"), "{full}");
    }
}
