//! Grants ("push for me"): the operator lets one conversation do one kind
//! of remote action on one exact target without asking each time. Only the
//! operator gives one, from a held card or the conversation's menu, never
//! from words in a chat. It names the exact repository and branch when it is
//! given, and the node matches each action against it exactly; the agent
//! never judges what it covers. It ends when the operator takes it back,
//! when the conversation's own agent gives it up, or when the conversation
//! is discarded. A push to a repository's default branch, main or master
//! always asks, whatever is granted, and so does a delete in Google Cloud.
//!
//! Grants are facts on the conversation, so its history shows each one
//! given, used and ended.

use crate::state::AppState;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub(crate) const GIVEN: &str = "git.grant.given";
pub(crate) const ENDED: &str = "git.grant.ended";
pub(crate) const USED: &str = "git.grant.used";
/// Push this branch, never forced (`/git/push`).
const PUSH: &str = "push";
/// Change this Google Cloud project with the operator's own sign-in, from
/// the conversation's rooms (`/google/change`): never a delete.
const GOOGLE: &str = "google";

#[derive(schemars::JsonSchema, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct PushGrant {
    pub(crate) grant_id: String,
    /// push: the conversation pushes this branch without asking, never
    /// forced. google: its rooms change this Google Cloud project without
    /// asking, never deleting.
    pub(crate) kind: String,
    /// owner/name on GitHub, else the remote's address; for google, the
    /// Google Cloud project.
    pub(crate) repository: String,
    /// Empty for google.
    pub(crate) branch: String,
    /// Always the operator.
    pub(crate) given_by: String,
    pub(crate) at: String,
}

#[derive(Deserialize)]
struct Ended {
    grant_id: String,
}

/// The conversation's grants given and not ended, by its facts.
fn given(heart: &bakery_heart::Heart, stream_id: &str) -> Vec<PushGrant> {
    let facts = |family| heart.events_of_family(stream_id, family).unwrap_or_default();
    let ended: HashSet<String> = facts(ENDED)
        .iter()
        .filter_map(|e| serde_json::from_str::<Ended>(&e.payload).ok())
        .map(|e| e.grant_id)
        .collect();
    facts(GIVEN)
        .iter()
        .filter_map(|e| serde_json::from_str::<PushGrant>(&e.payload).ok())
        .filter(|g| !ended.contains(&g.grant_id))
        .collect()
}

/// The conversation's grants in force; none once it is discarded.
pub(crate) fn live(state: &AppState, stream_id: &str) -> Vec<PushGrant> {
    let heart = state.heart.lock().unwrap();
    if crate::state::discarded_of(&heart, stream_id) {
        return Vec::new();
    }
    given(&heart, stream_id)
}

/// Where a conversation's checkout pushes, and which branch: the one named,
/// else its current one.
fn target(state: &AppState, stream_id: &str, branch: Option<&str>) -> Result<(PathBuf, String, String), String> {
    let dir = crate::routes::git::checkout(state, stream_id)?;
    let url = crate::git::run(&dir, &["remote", "get-url", "origin"]).map_err(|_| "no remote named origin".to_string())?;
    let url = url.trim();
    let repository = crate::routes::github::github_slug(url).unwrap_or_else(|| url.to_string());
    let branch = match branch.map(str::trim).filter(|b| !b.is_empty()) {
        Some(branch) => branch.to_string(),
        None => crate::routes::git::current_branch(state, stream_id)?,
    };
    Ok((dir, repository, branch))
}

/// The repository's default branch: GitHub's answer, else the remote's HEAD
/// as last fetched.
fn default_branch(dir: &Path, repository: &str) -> Option<String> {
    let on_github = crate::routes::github::slug(repository).is_ok();
    let asked = || {
        let jq = ".defaultBranchRef.name";
        crate::git::gh(dir, &["repo", "view", repository, "--json", "defaultBranchRef", "--jq", jq]).ok()
    };
    on_github
        .then(asked)
        .flatten()
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .or_else(|| {
            let head = crate::git::run(dir, &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"]).ok()?;
            head.trim().strip_prefix("origin/").map(str::to_string)
        })
}

/// Why a push to this branch always asks, whatever is granted; None when it
/// may be granted. A default branch nobody can tell fails closed.
fn always_asks(dir: &Path, repository: &str, branch: &str) -> Option<String> {
    if matches!(branch, "main" | "master") {
        return Some(format!("a push to {branch} always asks"));
    }
    match default_branch(dir, repository) {
        Some(default) if default == branch => Some(format!("a push to the default branch ({default}) always asks")),
        Some(_) => None,
        None => Some(format!("{repository}'s default branch can't be told, so every push to it asks")),
    }
}

/// The Google Cloud project a request to Google's APIs changes: the one its
/// path names (`projects/<id>`), else its `project` query. None when it
/// names none (a Cloud Storage object, say), so it always asks.
pub(crate) fn google_project(url: &str) -> Option<String> {
    let url = url.split('#').next().unwrap_or_default();
    let (path, query) = url.split_once('?').unwrap_or((url, ""));
    let mut segments = path.split('/');
    let in_path = segments
        .by_ref()
        .find(|s| *s == "projects")
        .and_then(|_| segments.next())
        .map(|id| id.split(':').next().unwrap_or_default());
    in_path
        .or_else(|| query.split('&').find_map(|pair| pair.strip_prefix("project=")))
        .filter(|id| !id.is_empty())
        .map(str::to_string)
}

/// What a Google change may be granted on: its project, unless it deletes.
fn google_target(args: &Value) -> Option<String> {
    let method = args["method"].as_str()?;
    if method.eq_ignore_ascii_case("DELETE") {
        return None;
    }
    google_project(args["url"].as_str()?)
}

/// The grant a card offers: for a push, this conversation pushing this
/// branch of its own from now on; for a Google change, its rooms changing
/// that project from now on. None when the call is another conversation's,
/// or always asks.
pub(crate) fn offer(state: &AppState, stream_id: &str, tool: &str, args: &Value) -> Option<Value> {
    if args["stream_id"].as_str() != Some(stream_id) {
        return None;
    }
    match tool {
        "git_push" => {
            let (dir, repository, branch) = target(state, stream_id, args["branch"].as_str()).ok()?;
            always_asks(&dir, &repository, &branch)
                .is_none()
                .then(|| json!({ "kind": PUSH, "repository": repository, "branch": branch }))
        }
        "google_change" => {
            google_target(args).map(|project| json!({ "kind": GOOGLE, "repository": project, "branch": "" }))
        }
        _ => None,
    }
}

/// The grant a call goes ahead under, if one covers it exactly and nothing
/// makes it always ask.
pub(crate) fn covering(state: &AppState, stream_id: &str, tool: &str, args: &Value) -> Option<PushGrant> {
    if args["stream_id"].as_str() != Some(stream_id) || !matches!(tool, "git_push" | "google_change") {
        return None;
    }
    let grants = live(state, stream_id);
    if grants.is_empty() {
        return None;
    }
    if tool == "google_change" {
        let project = google_target(args)?;
        return grants.into_iter().find(|g| g.kind == GOOGLE && g.repository == project);
    }
    let (dir, repository, branch) = target(state, stream_id, args["branch"].as_str()).ok()?;
    let grant = grants.into_iter().find(|g| g.kind == PUSH && g.repository == repository && g.branch == branch)?;
    always_asks(&dir, &repository, &branch).is_none().then_some(grant)
}

/// A push done under a grant, on the conversation's history.
pub(crate) fn used(state: &AppState, stream_id: &str, grant: &PushGrant) {
    let fact = json!({ "grant_id": grant.grant_id, "kind": grant.kind, "repository": grant.repository, "branch": grant.branch });
    let _ = crate::state::append(state, stream_id, "node", USED, fact.to_string(), vec![]);
}

fn record(state: &AppState, stream_id: &str, kind: &str, repository: String, branch: String) -> Result<PushGrant, String> {
    let same = |g: &PushGrant| g.kind == kind && g.repository == repository && g.branch == branch;
    if let Some(grant) = live(state, stream_id).into_iter().find(same) {
        return Ok(grant);
    }
    let grant = PushGrant {
        grant_id: crate::node::mint_id("grant"),
        kind: kind.to_string(),
        repository,
        branch,
        given_by: "operator".to_string(),
        at: chrono::Utc::now().to_rfc3339(),
    };
    let fact = serde_json::to_string(&grant).map_err(|e| e.to_string())?;
    crate::state::append(state, stream_id, "human", GIVEN, fact, vec![])?;
    Ok(grant)
}

/// The grant a card offered, given with its approval
/// (`decide_child_tool_gate`, the operator's own route): recorded as
/// offered, since the card already checked it may be granted.
pub(crate) fn give_offered(state: &AppState, stream_id: &str, offered: &Value) -> Result<PushGrant, String> {
    match (offered["kind"].as_str(), offered["repository"].as_str(), offered["branch"].as_str()) {
        (Some(kind @ (PUSH | GOOGLE)), Some(repository), Some(branch)) => {
            record(state, stream_id, kind, repository.to_string(), branch.to_string())
        }
        _ => Err("the card offered no grant".to_string()),
    }
}

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct GrantsQuery {
    pub(crate) stream_id: String,
}

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct GiveReq {
    pub(crate) stream_id: String,
    /// push, the one kind so far.
    pub(crate) kind: String,
    /// Default: the conversation's current branch.
    #[serde(default)]
    pub(crate) branch: Option<String>,
}

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct EndReq {
    pub(crate) stream_id: String,
    pub(crate) grant_id: String,
}

/// From the conversation's menu: fixed to its repository and the branch,
/// now. The operator's alone.
pub(crate) fn give(state: &AppState, req: &GiveReq) -> Result<PushGrant, (StatusCode, String)> {
    let bad = |e: String| (StatusCode::BAD_REQUEST, e);
    if crate::actor::agent_calling() {
        return Err((StatusCode::FORBIDDEN, "only the operator gives a grant".to_string()));
    }
    if req.kind != PUSH {
        return Err(bad(format!("{:?}: push is the one kind of grant", req.kind)));
    }
    let (dir, repository, branch) = target(state, &req.stream_id, req.branch.as_deref()).map_err(bad)?;
    if let Some(why) = always_asks(&dir, &repository, &branch) {
        return Err(bad(why));
    }
    record(state, &req.stream_id, PUSH, repository, branch).map_err(bad)
}

/// Ends one: the operator takes it back, or the conversation's own agent
/// gives it up (its reach is its own conversation).
pub(crate) fn end(state: &AppState, req: &EndReq) -> Result<Vec<PushGrant>, (StatusCode, String)> {
    if !live(state, &req.stream_id).iter().any(|g| g.grant_id == req.grant_id) {
        return Err((StatusCode::NOT_FOUND, format!("no grant {} in force here", req.grant_id)));
    }
    let why = if crate::actor::agent_calling() { "given up by its agent" } else { "taken back by the operator" };
    let fact = json!({ "grant_id": req.grant_id, "why": why });
    crate::state::append(state, &req.stream_id, "human", ENDED, fact.to_string(), vec![])
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(live(state, &req.stream_id))
}

/// The conversation was discarded: each grant ends with it, on its history.
pub(crate) fn end_all(state: &AppState, stream_id: &str, why: &str) {
    let grants = given(&state.heart.lock().unwrap(), stream_id);
    for grant in grants {
        let fact = json!({ "grant_id": grant.grant_id, "why": why });
        let _ = crate::state::append(state, stream_id, "node", ENDED, fact.to_string(), vec![]);
    }
}

type Reply<T> = Result<Json<T>, (StatusCode, String)>;

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T, (StatusCode, String)> + Send + 'static) -> Reply<T> {
    crate::actor::spawn_blocking(f).await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?.map(Json)
}

pub(crate) async fn list_route(State(st): State<AppState>, Query(q): Query<GrantsQuery>) -> Reply<Vec<PushGrant>> {
    blocking(move || Ok(live(&st, &q.stream_id))).await
}

pub(crate) async fn give_route(State(st): State<AppState>, Json(req): Json<GiveReq>) -> Reply<PushGrant> {
    blocking(move || give(&st, &req)).await
}

pub(crate) async fn end_route(State(st): State<AppState>, Json(req): Json<EndReq>) -> Reply<Vec<PushGrant>> {
    blocking(move || end(&st, &req)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::project::open_stream;
    use crate::testutil::*;
    use bakery_agent::StubModel;
    use std::sync::Arc;

    /// A conversation on its own branch, in a clone of a local origin whose
    /// default branch is main.
    fn conversation(dir: &Path) -> (AppState, crate::state::ActiveContext) {
        let state = state_with(dir, Arc::new(StubModel));
        let local = crate::git::tests::behind_its_origin(dir);
        let ctx = open_stream(&state, &open_req(&local, "feature")).unwrap().context;
        (state, ctx)
    }

    fn as_its_agent<R>(stream_id: &str, f: impl FnOnce() -> R) -> R {
        crate::actor::with(crate::actor::agent(stream_id, crate::node::Seat::Orchestrator), f)
    }

    #[test]
    fn only_the_operator_gives_a_grant_and_never_for_a_default_branch() {
        let dir = tempfile::tempdir().unwrap();
        let (state, ctx) = conversation(dir.path());
        let sid = ctx.stream_id.clone();
        let req = |branch: Option<&str>| GiveReq { stream_id: sid.clone(), kind: "push".into(), branch: branch.map(str::to_string) };

        let refused = as_its_agent(&sid, || give(&state, &req(None))).unwrap_err();
        assert_eq!(refused.0, StatusCode::FORBIDDEN, "{refused:?}");
        let grant = give(&state, &req(None)).unwrap();
        let origin = dir.path().join("origin.git").to_string_lossy().into_owned();
        assert_eq!((grant.kind.as_str(), grant.branch.as_str(), grant.repository.as_str()), ("push", ctx.branch.as_str(), origin.as_str()));
        assert_eq!(give(&state, &req(None)).unwrap().grant_id, grant.grant_id, "the same grant twice is one");
        for always in ["main", "master"] {
            let err = give(&state, &req(Some(always))).unwrap_err().1;
            assert!(err.contains("always asks"), "{err}");
        }
        // Whatever the remote calls its default branch.
        crate::git::run(Path::new(&ctx.repo_root), &["symbolic-ref", "refs/remotes/origin/HEAD", "refs/remotes/origin/trunk"]).unwrap();
        let err = give(&state, &req(Some("trunk"))).unwrap_err().1;
        assert!(err.contains("default branch (trunk)"), "{err}");
        let err = give(&state, &GiveReq { kind: "merge".into(), ..req(None) }).unwrap_err().1;
        assert!(err.contains("push is the one kind"), "{err}");
    }

    /// Every conversation's own agent may ask to push its own branch: the
    /// card asks the operator unless a grant covers it; another
    /// conversation's branch is out of its reach.
    #[test]
    fn an_agent_asks_to_push_its_own_branch_and_no_other() {
        use crate::agent_control::OperatingMode;
        use crate::api_tools::{call, Caller, WAITING};
        let dir = tempfile::tempdir().unwrap();
        let (state, ctx) = conversation(dir.path());
        let other = open_stream(&state, &open_req(Path::new(&ctx.repo_root), "other")).unwrap().context.stream_id;
        let caller = Caller {
            stream_id: ctx.stream_id.clone(),
            seat: crate::node::Seat::Orchestrator,
            turn_id: String::new(),
            mode: OperatingMode::Manual,
            wait: Some(std::time::Duration::from_millis(200)),
        };
        let own = json!({ "stream_id": ctx.stream_id });
        assert_eq!(call(&state, &caller, "git_push", &own).unwrap_err(), WAITING, "its own: the operator decides");
        let err = call(&state, &caller, "git_push", &json!({ "stream_id": other })).unwrap_err();
        assert!(err.contains("your own conversation only"), "{err}");
    }

    #[test]
    fn a_grant_covers_exactly_its_branch_until_it_ends() {
        let dir = tempfile::tempdir().unwrap();
        let (state, ctx) = conversation(dir.path());
        let sid = ctx.stream_id.clone();
        let wt = Path::new(&ctx.worktree_path);
        let own = json!({ "stream_id": sid });
        let give_one = || give(&state, &GiveReq { stream_id: sid.clone(), kind: "push".into(), branch: None }).unwrap();

        assert_eq!(covering(&state, &sid, "git_push", &own), None, "nothing granted yet");
        assert_eq!(offer(&state, &sid, "git_push", &own).unwrap()["branch"], ctx.branch, "a card offers its own branch");
        assert_eq!(offer(&state, &sid, "git_push", &json!({ "stream_id": "wt:other" })), None, "never another conversation's");
        assert_eq!(offer(&state, &sid, "git_push", &json!({ "stream_id": sid, "branch": "main" })), None, "never main");

        let grant = give_one();
        assert_eq!(covering(&state, &sid, "git_push", &own), Some(grant.clone()));
        assert_eq!(covering(&state, &sid, "git_push", &json!({ "stream_id": sid, "branch": "elsewhere" })), None);
        assert_eq!(covering(&state, "wt:other", "git_push", &json!({ "stream_id": "wt:other" })), None);
        // Fixed to the name it was given for: another branch checked out asks.
        run_git(wt, &["switch", "-q", "-c", "next"]);
        assert_eq!(covering(&state, &sid, "git_push", &own), None);
        run_git(wt, &["switch", "-q", &ctx.branch]);

        // Its own agent gives it up; the operator gives one again, then takes
        // it back; the conversation's end ends a third.
        let end_req = |grant: &PushGrant| EndReq { stream_id: sid.clone(), grant_id: grant.grant_id.clone() };
        assert!(as_its_agent(&sid, || end(&state, &end_req(&grant))).unwrap().is_empty());
        assert_eq!(end(&state, &end_req(&grant)).unwrap_err().0, StatusCode::NOT_FOUND, "ended once");
        let again = give_one();
        assert_ne!(again.grant_id, grant.grant_id);
        assert!(end(&state, &end_req(&again)).unwrap().is_empty());
        give_one();
        end_all(&state, &sid, "the conversation was discarded");
        assert!(live(&state, &sid).is_empty());
        assert_eq!(covering(&state, &sid, "git_push", &own), None);
        let whys: Vec<String> = state.heart.lock().unwrap().events_of_family(&sid, ENDED).unwrap().iter()
            .map(|e| serde_json::from_str::<Value>(&e.payload).unwrap()["why"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(whys, ["given up by its agent", "taken back by the operator", "the conversation was discarded"]);
    }

    #[test]
    fn a_google_grant_covers_its_project_and_never_a_delete() {
        let project = |url| google_project(url);
        assert_eq!(project("https://run.googleapis.com/v2/projects/farm-dev/locations/us/services?serviceId=api").as_deref(), Some("farm-dev"));
        assert_eq!(project("https://cloudresourcemanager.googleapis.com/v3/projects/farm-dev:setIamPolicy").as_deref(), Some("farm-dev"));
        assert_eq!(project("https://storage.googleapis.com/storage/v1/b?project=farm-dev").as_deref(), Some("farm-dev"));
        assert_eq!(project("https://storage.googleapis.com/upload/storage/v1/b/farm-src/o?name=x"), None, "no project: always asks");

        let dir = tempfile::tempdir().unwrap();
        let (state, ctx) = conversation(dir.path());
        let sid = ctx.stream_id.clone();
        let change = |method: &str, url: &str| json!({ "stream_id": sid, "method": method, "url": url });
        let deploy = change("POST", "https://run.googleapis.com/v2/projects/farm-dev/locations/us/services");
        assert_eq!(covering(&state, &sid, "google_change", &deploy), None, "nothing granted yet");
        let offered = offer(&state, &sid, "google_change", &deploy).unwrap();
        let grant = give_offered(&state, &sid, &offered).unwrap();
        assert_eq!((grant.kind.as_str(), grant.repository.as_str()), ("google", "farm-dev"));
        assert_eq!(covering(&state, &sid, "google_change", &deploy), Some(grant));
        let elsewhere = change("PATCH", "https://run.googleapis.com/v2/projects/other/locations/us/services/api");
        assert_eq!(covering(&state, &sid, "google_change", &elsewhere), None, "its project only");
        let delete = change("DELETE", "https://run.googleapis.com/v2/projects/farm-dev/locations/us/services/api");
        assert_eq!(offer(&state, &sid, "google_change", &delete), None, "a delete always asks");
        assert_eq!(covering(&state, &sid, "google_change", &delete), None);
        assert_eq!(covering(&state, &sid, "git_push", &json!({ "stream_id": sid })), None, "never a push");
    }
}
