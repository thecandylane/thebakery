//! The operator's GitHub, through their own `gh` login (the node holds no
//! GitHub token): their repositories and their organizations', a clone that
//! becomes a Project, and issues. Reads are free. A clone writes one new
//! folder in the operator's home. Creating an issue or a comment publishes
//! under the operator's name, so it always asks first.

use crate::git::{gh, gh_json};
use crate::state::AppState;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

const REPO_FIELDS: &str = "name,nameWithOwner,owner,visibility,defaultBranchRef,updatedAt,description,isArchived,isFork,url";
const ISSUE_FIELDS: &str = "number,title,state,author,labels,assignees,updatedAt,url";
const ISSUE_VIEW_FIELDS: &str = "number,title,state,author,body,labels,assignees,createdAt,updatedAt,url,comments";

pub(crate) fn home() -> Result<PathBuf, String> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|home| home.is_absolute())
        .ok_or_else(|| "HOME is not set".to_string())
}

pub(crate) fn text(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

fn logins(list: &Value, key: &str) -> Vec<String> {
    list.as_array().into_iter().flatten().map(|item| text(&item[key])).collect()
}

/// `owner/name`, checked: GitHub's own characters, never an option.
pub(crate) fn slug(repository: &str) -> Result<String, String> {
    let repository = repository.trim();
    let part = |p: &str| !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    match repository.trim_end_matches(".git").split_once('/') {
        Some((owner, name))
            if part(owner) && part(name) && !owner.starts_with(['-', '.']) && !name.starts_with('-') && name != "." && name != ".." =>
        {
            Ok(format!("{owner}/{name}"))
        }
        _ => Err(format!("{repository:?} is not a GitHub repository: give owner/name")),
    }
}

/// The `owner/name` a GitHub remote URL points at, lower-cased.
pub(crate) fn github_slug(url: &str) -> Option<String> {
    let url = url.trim();
    let rest = ["https://github.com/", "http://github.com/", "git@github.com:", "ssh://git@github.com/"]
        .iter()
        .find_map(|prefix| url.strip_prefix(prefix))?;
    slug(rest.trim_end_matches('/')).ok().map(|s| s.to_ascii_lowercase())
}

pub(crate) fn origin_slug(dir: &Path) -> Option<String> {
    crate::git::run(dir, &["remote", "get-url", "origin"]).ok().and_then(|url| github_slug(&url))
}

/// Where each GitHub repository is cloned on this Mac, as far as Bakery
/// knows: the repositories behind the operator's Projects.
fn project_clones(state: &AppState) -> HashMap<String, String> {
    let mut found = HashMap::new();
    for project in crate::catalog::known_projects(state).unwrap_or_default() {
        for root in project.roots.iter().filter(|root| root.is_git) {
            let main = crate::git::inspect(&root.path).root.unwrap_or_else(|| root.path.clone());
            if let Some(slug) = origin_slug(Path::new(&main)) {
                found.entry(slug).or_insert(main);
            }
        }
    }
    found
}

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct ReposQuery {
    /// Only this owner's: the operator or one of their organizations. All of
    /// them by default.
    #[serde(default)]
    pub(crate) owner: Option<String>,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct Repos {
    /// Most recently updated first.
    pub(crate) repos: Vec<Repo>,
    /// Owners GitHub couldn't list (an organization's SSO, say); the rest did.
    pub(crate) problems: Vec<String>,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct Repo {
    /// owner/name.
    pub(crate) repository: String,
    pub(crate) owner: String,
    pub(crate) name: String,
    /// public, private or internal.
    pub(crate) visibility: String,
    pub(crate) default_branch: Option<String>,
    pub(crate) updated_at: String,
    pub(crate) description: String,
    pub(crate) archived: bool,
    pub(crate) fork: bool,
    pub(crate) url: String,
    /// Where it is cloned on this Mac: a Project's repository, or the
    /// default clone folder (~/Dev/code/<name>).
    pub(crate) cloned_at: Option<String>,
}

fn repos_from(list: &Value, clones: &HashMap<String, String>, home: &Path) -> Vec<Repo> {
    list.as_array()
        .into_iter()
        .flatten()
        .map(|r| {
            let repository = text(&r["nameWithOwner"]);
            let name = text(&r["name"]);
            let key = repository.to_ascii_lowercase();
            let default_folder = home.join("Dev/code").join(&name);
            let cloned_at = clones.get(&key).cloned().or_else(|| {
                (default_folder.is_dir() && origin_slug(&default_folder).as_deref() == Some(key.as_str()))
                    .then(|| default_folder.to_string_lossy().into_owned())
            });
            Repo {
                owner: text(&r["owner"]["login"]),
                visibility: text(&r["visibility"]).to_ascii_lowercase(),
                default_branch: r["defaultBranchRef"]["name"].as_str().map(str::to_string),
                updated_at: text(&r["updatedAt"]),
                description: text(&r["description"]),
                archived: r["isArchived"] == true,
                fork: r["isFork"] == true,
                url: text(&r["url"]),
                repository,
                name,
                cloned_at,
            }
        })
        .collect()
}

pub(crate) fn repos(state: &AppState, q: &ReposQuery) -> Result<Repos, String> {
    let home = home()?;
    let mut problems = Vec::new();
    let owners: Vec<Option<String>> = match q.owner.as_deref().map(str::trim).filter(|o| !o.is_empty()) {
        Some(owner) => vec![Some(slug(&format!("{owner}/x"))?.split('/').next().unwrap_or_default().to_string())],
        None => {
            // The operator, then each of their organizations.
            let orgs = gh(&home, &["api", "user/orgs", "--paginate", "--jq=.[].login"]).unwrap_or_else(|e| {
                problems.push(format!("your organizations: {e}"));
                String::new()
            });
            std::iter::once(None).chain(orgs.lines().map(|o| Some(o.trim().to_string())).filter(|o| o.as_deref() != Some(""))).collect()
        }
    };
    let clones = project_clones(state);
    let mut all = Vec::new();
    for owner in owners {
        let mut args = vec!["repo".to_string(), "list".to_string()];
        args.extend(owner.clone());
        args.extend(["--limit=1000".to_string(), format!("--json={REPO_FIELDS}")]);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        match gh_json(&home, &args) {
            Ok(list) => all.extend(repos_from(&list, &clones, &home)),
            Err(e) if owner.is_none() => return Err(e),
            Err(e) => problems.push(format!("{}: {e}", owner.unwrap_or_default())),
        }
    }
    all.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    Ok(Repos { repos: all, problems })
}

/// A clone's folder: new, inside the operator's home, outside its hidden
/// folders and everything Bakery always blocks, and reached through no
/// link that leads elsewhere.
fn destination(home: &Path, blocked: &[PathBuf], into: &str) -> Result<PathBuf, String> {
    let into = into.trim();
    let path = match into.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None => PathBuf::from(into),
    };
    if !path.is_absolute() || path.components().any(|c| matches!(c, Component::ParentDir | Component::CurDir)) {
        return Err(format!("{into:?}: give a full path, or one starting ~/, without . or .."));
    }
    let inside = path
        .strip_prefix(home)
        .ok()
        .filter(|rest| rest.components().next().is_some())
        .ok_or_else(|| format!("{} is outside your home folder", path.display()))?;
    if inside.components().next().is_some_and(|c| c.as_os_str().to_string_lossy().starts_with('.')) {
        return Err(format!("{} is in a hidden folder of your home", path.display()));
    }
    if path.symlink_metadata().is_ok() {
        return Err(format!("{} already exists", path.display()));
    }
    let existing = path.ancestors().find(|a| a.exists()).unwrap_or(home);
    let real = existing.canonicalize().map_err(|e| format!("{}: {e}", existing.display()))?;
    let real_home = home.canonicalize().map_err(|e| format!("{}: {e}", home.display()))?;
    if !real.starts_with(&real_home) {
        return Err(format!("{} leads outside your home folder", path.display()));
    }
    let wanted = real.join(path.strip_prefix(existing).unwrap_or(Path::new("")));
    if let Some(no) = blocked.iter().find(|b| wanted.starts_with(b)) {
        return Err(format!("{} is always blocked ({})", path.display(), no.display()));
    }
    Ok(path)
}

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct CloneReq {
    /// owner/name.
    pub(crate) repository: String,
    /// A new folder in your home, as a full path or ~/…; ~/Dev/code/<name>
    /// by default.
    #[serde(default)]
    pub(crate) into: Option<String>,
    /// An existing project the clone joins as one of its roots, instead of
    /// becoming a project of its own.
    #[serde(default)]
    pub(crate) project_id: Option<String>,
}

#[derive(schemars::JsonSchema, Serialize)]
pub(crate) struct Cloned {
    pub(crate) path: String,
    pub(crate) project: crate::routes::project_catalog::ProjectView,
    pub(crate) socket: crate::state::ActiveContext,
}

fn project(state: &AppState, project_id: &str) -> Result<crate::routes::project_catalog::ProjectView, String> {
    crate::catalog::project_by_id(state, project_id)
        .map(Into::into)
        .ok_or_else(|| format!("no project {project_id}"))
}

/// A fresh clone, opened the way any folder becomes a Project, or as one
/// more root of `project_id`, the way a folder joins an existing one.
fn onboard(state: &AppState, name: &str, path: &Path, project_id: Option<&str>) -> Result<Cloned, String> {
    use crate::routes::project_catalog::{CreateProjectReq, CreateProjectRoot, OpenRootReq};
    let shown = path.to_string_lossy().into_owned();
    let root = CreateProjectRoot::Attach { path: shown.clone() };
    let failed = |e: String| format!("cloned into {shown}, but it could not be opened as a project: {e}");
    let Some(project_id) = project_id else {
        let req = CreateProjectReq { name: name.to_string(), root: Some(root), stacks: Default::default(), tools: None };
        let created = crate::routes::project_catalog::create_project(state, req).map_err(failed)?;
        return Ok(Cloned { path: shown, project: created.project, socket: created.socket });
    };
    let req = OpenRootReq { project_id: project_id.to_string(), name: name.to_string(), root, tools: None };
    let socket = crate::routes::project_catalog::open_root(state, &req).map_err(failed)?;
    Ok(Cloned { path: shown, project: project(state, project_id)?, socket })
}

pub(crate) fn clone(state: &AppState, req: &CloneReq) -> Result<Cloned, String> {
    let repository = slug(&req.repository)?;
    let name = repository.split_once('/').map(|(_, name)| name.to_string()).unwrap_or_default();
    // Before anything is cloned, so a wrong project leaves no folder behind.
    if let Some(project_id) = &req.project_id {
        project(state, project_id)?;
    }
    let home = home()?;
    let into = req.into.clone().unwrap_or_else(|| format!("~/Dev/code/{name}"));
    let path = destination(&home, &crate::machine_access::deny_list(state), &into)?;
    gh(&home, &["repo", "clone", &repository, &path.to_string_lossy()])?;
    onboard(state, &name, &path, req.project_id.as_deref())
}

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct IssuesQuery {
    /// owner/name.
    pub(crate) repository: String,
    /// open (the default), closed or all.
    #[serde(default)]
    pub(crate) state: Option<String>,
    /// At most this many, most recent first: 50 by default, 500 at most.
    #[serde(default)]
    pub(crate) limit: Option<u32>,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct Issue {
    pub(crate) number: u64,
    pub(crate) title: String,
    /// OPEN or CLOSED.
    pub(crate) state: String,
    pub(crate) author: String,
    pub(crate) labels: Vec<String>,
    pub(crate) assignees: Vec<String>,
    pub(crate) updated_at: String,
    pub(crate) url: String,
}

fn issues_from(list: &Value) -> Vec<Issue> {
    list.as_array()
        .into_iter()
        .flatten()
        .map(|i| Issue {
            number: i["number"].as_u64().unwrap_or_default(),
            title: text(&i["title"]),
            state: text(&i["state"]),
            author: text(&i["author"]["login"]),
            labels: logins(&i["labels"], "name"),
            assignees: logins(&i["assignees"], "login"),
            updated_at: text(&i["updatedAt"]),
            url: text(&i["url"]),
        })
        .collect()
}

pub(crate) fn issues(q: &IssuesQuery) -> Result<Vec<Issue>, String> {
    let repository = slug(&q.repository)?;
    let state = match q.state.as_deref().unwrap_or("open") {
        state @ ("open" | "closed" | "all") => state,
        other => return Err(format!("{other:?}: state is open, closed or all")),
    };
    let limit = q.limit.unwrap_or(50).clamp(1, 500);
    let list = gh_json(
        &home()?,
        &[
            "issue",
            "list",
            &format!("--repo={repository}"),
            &format!("--state={state}"),
            &format!("--limit={limit}"),
            &format!("--json={ISSUE_FIELDS}"),
        ],
    )?;
    Ok(issues_from(&list))
}

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct IssueQuery {
    /// owner/name.
    pub(crate) repository: String,
    pub(crate) number: u64,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct IssueComment {
    pub(crate) author: String,
    pub(crate) body: String,
    pub(crate) created_at: String,
    pub(crate) url: String,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct IssueView {
    pub(crate) number: u64,
    pub(crate) title: String,
    pub(crate) state: String,
    pub(crate) author: String,
    pub(crate) body: String,
    pub(crate) labels: Vec<String>,
    pub(crate) assignees: Vec<String>,
    pub(crate) created_at: String,
    pub(crate) updated_at: String,
    pub(crate) url: String,
    /// Oldest first.
    pub(crate) comments: Vec<IssueComment>,
}

fn issue_from(i: &Value) -> IssueView {
    IssueView {
        number: i["number"].as_u64().unwrap_or_default(),
        title: text(&i["title"]),
        state: text(&i["state"]),
        author: text(&i["author"]["login"]),
        body: text(&i["body"]),
        labels: logins(&i["labels"], "name"),
        assignees: logins(&i["assignees"], "login"),
        created_at: text(&i["createdAt"]),
        updated_at: text(&i["updatedAt"]),
        url: text(&i["url"]),
        comments: i["comments"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|c| IssueComment {
                author: text(&c["author"]["login"]),
                body: text(&c["body"]),
                created_at: text(&c["createdAt"]),
                url: text(&c["url"]),
            })
            .collect(),
    }
}

pub(crate) fn issue(q: &IssueQuery) -> Result<IssueView, String> {
    let repository = slug(&q.repository)?;
    let number = q.number.to_string();
    let issue = gh_json(
        &home()?,
        &["issue", "view", &number, &format!("--repo={repository}"), &format!("--json={ISSUE_VIEW_FIELDS}")],
    )?;
    Ok(issue_from(&issue))
}

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct IssueCreateReq {
    /// owner/name.
    pub(crate) repository: String,
    pub(crate) title: String,
    #[serde(default)]
    pub(crate) body: String,
    #[serde(default)]
    pub(crate) labels: Vec<String>,
}

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct IssueCommentReq {
    /// owner/name.
    pub(crate) repository: String,
    pub(crate) number: u64,
    pub(crate) body: String,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct Published {
    /// Where it now is on GitHub.
    pub(crate) url: String,
}

/// gh prints the new item's address last.
fn published(out: &str) -> Published {
    Published { url: out.trim().lines().last().unwrap_or_default().trim().to_string() }
}

pub(crate) fn issue_create(state: &AppState, req: &IssueCreateReq) -> Result<Published, String> {
    let repository = slug(&req.repository)?;
    let title = req.title.trim();
    if title.is_empty() {
        return Err("an issue needs a title".to_string());
    }
    let mut args = vec![
        "issue".to_string(),
        "create".to_string(),
        format!("--repo={repository}"),
        format!("--title={title}"),
        format!("--body={}", req.body),
    ];
    args.extend(req.labels.iter().map(|label| format!("--label={label}")));
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let made = gh(&home()?, &args).map(|out| published(&out))?;
    crate::remote::done(state, None, json!({"what": "issue opened", "repository": repository, "url": made.url}));
    Ok(made)
}

pub(crate) fn issue_comment(state: &AppState, req: &IssueCommentReq) -> Result<Published, String> {
    let repository = slug(&req.repository)?;
    if req.body.trim().is_empty() {
        return Err("a comment needs a body".to_string());
    }
    let number = req.number.to_string();
    let made = gh(&home()?, &["issue", "comment", &number, &format!("--repo={repository}"), &format!("--body={}", req.body)])
        .map(|out| published(&out))?;
    let line = json!({"what": "issue comment", "repository": repository, "number": req.number, "url": made.url});
    crate::remote::done(state, None, line);
    Ok(made)
}

type Reply<T> = Result<Json<T>, (StatusCode, String)>;

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T, String> + Send + 'static) -> Reply<T> {
    crate::actor::spawn_blocking(f)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))
}

pub(crate) async fn repos_route(State(st): State<AppState>, Query(q): Query<ReposQuery>) -> Reply<Repos> {
    blocking(move || repos(&st, &q)).await
}

pub(crate) async fn clone_route(State(st): State<AppState>, Json(req): Json<CloneReq>) -> Reply<Cloned> {
    blocking(move || clone(&st, &req)).await
}

pub(crate) async fn issues_route(Query(q): Query<IssuesQuery>) -> Reply<Vec<Issue>> {
    blocking(move || issues(&q)).await
}

pub(crate) async fn issue_route(Query(q): Query<IssueQuery>) -> Reply<IssueView> {
    blocking(move || issue(&q)).await
}

pub(crate) async fn issue_create_route(State(st): State<AppState>, Json(req): Json<IssueCreateReq>) -> Reply<Published> {
    blocking(move || issue_create(&st, &req)).await
}

pub(crate) async fn issue_comment_route(State(st): State<AppState>, Json(req): Json<IssueCommentReq>) -> Reply<Published> {
    blocking(move || issue_comment(&st, &req)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;
    use bakery_agent::StubModel;

    use std::sync::Arc;

    #[test]
    fn a_repository_is_owner_and_name_and_never_an_option() {
        assert_eq!(slug("acme/widget").unwrap(), "acme/widget");
        assert_eq!(slug(" acme/widget.git ").unwrap(), "acme/widget");
        assert_eq!(slug("acme/.github").unwrap(), "acme/.github");
        for bad in ["widget", "-x/widget", "acme/-x", "acme/..", "acme/a b", "acme/w/x", "../widget", ""] {
            assert!(slug(bad).is_err(), "{bad:?}");
        }
        for (url, want) in [
            ("https://github.com/Acme/Widget.git", Some("acme/widget")),
            ("git@github.com:acme/widget.git", Some("acme/widget")),
            ("ssh://git@github.com/acme/widget", Some("acme/widget")),
            ("https://gitlab.com/acme/widget.git", None),
        ] {
            assert_eq!(github_slug(url).as_deref(), want, "{url}");
        }
    }

    #[test]
    fn a_clone_goes_into_a_new_folder_in_your_home_and_nowhere_else() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir_all(home.join("Dev/code/taken")).unwrap();
        std::fs::create_dir_all(dir.path().join("elsewhere")).unwrap();
        std::os::unix::fs::symlink(dir.path().join("elsewhere"), home.join("out")).unwrap();
        let blocked = [home.canonicalize().unwrap().join("Library/Keychains")];
        let ok = |into: &str| destination(&home, &blocked, into);

        assert_eq!(ok("~/Dev/code/widget").unwrap(), home.join("Dev/code/widget"));
        assert_eq!(ok(&home.join("new/deep/widget").to_string_lossy()).unwrap(), home.join("new/deep/widget"));
        assert!(ok("~/Dev/code/.github").is_ok(), "only your home's own hidden folders are off");
        for (into, why) in [
            ("~/Dev/code/taken", "already exists"),
            ("/tmp/widget", "outside your home"),
            ("~/", "outside your home"),
            ("~/.ssh/widget", "hidden folder"),
            ("~/Dev/../.ssh/widget", "without . or .."),
            ("Dev/widget", "full path"),
            ("~/out/widget", "leads outside your home"),
            ("~/Library/Keychains/widget", "always blocked"),
        ] {
            let err = ok(into).unwrap_err();
            assert!(err.contains(why), "{into}: {err}");
        }
    }

    #[test]
    fn a_clone_asked_into_a_project_joins_it_as_a_root_and_makes_none_of_its_own() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let (p, _) = crate::catalog::create_project(&state, "workspace", None).unwrap();
        let clone_dir = make_repo(&dir.path().join("home/Dev/code/widget"));

        let done = onboard(&state, "widget", &clone_dir, Some(&p.project_id)).unwrap();
        assert_eq!(done.project.project_id, p.project_id);
        let joined = crate::state::socket_project(&state.heart.lock().unwrap(), &done.socket.stream_id).0;
        assert_eq!(joined, Some(p.project_id.clone()));
        let canonical = |path: &str| std::fs::canonicalize(path).unwrap();
        let roots = crate::catalog::project_by_id(&state, &p.project_id).unwrap().roots;
        assert!(roots.iter().any(|r| canonical(&r.path) == canonical(&clone_dir.to_string_lossy())), "{roots:?}");
        assert_eq!(crate::catalog::known_projects(&state).unwrap().len(), 1, "no second project");

        // A project that isn't there is refused before anything is cloned.
        let req = CloneReq { repository: "acme/widget".into(), into: None, project_id: Some("project_gone".into()) };
        let err = clone(&state, &req).err().unwrap();
        assert!(err.contains("no project project_gone"), "{err}");
    }

    #[test]
    fn a_fresh_clone_becomes_a_project_and_reads_as_cloned() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let clone = make_repo(&dir.path().join("home/Dev/code/widget"));
        crate::git::run(&clone, &["remote", "add", "origin", "git@github.com:acme/widget.git"]).unwrap();

        let done = onboard(&state, "widget", &clone, None).unwrap();
        assert_eq!(done.project.name, "widget");
        let listed = json!([
            {"name": "widget", "nameWithOwner": "acme/widget", "owner": {"login": "acme"}, "visibility": "PRIVATE",
             "defaultBranchRef": {"name": "main"}, "updatedAt": "2026-10-08T10:00:00Z", "description": "",
             "isArchived": false, "isFork": false, "url": "https://github.com/acme/widget"},
            {"name": "other", "nameWithOwner": "acme/other", "owner": {"login": "acme"}, "visibility": "PUBLIC",
             "defaultBranchRef": null, "updatedAt": "2026-10-07T10:00:00Z", "description": "x",
             "isArchived": true, "isFork": true, "url": "https://github.com/acme/other"}
        ]);
        let away = dir.path().join("elsewhere");
        let got = repos_from(&listed, &project_clones(&state), &away);
        let canonical = std::fs::canonicalize(&clone).unwrap();
        let cloned = got[0].cloned_at.as_deref().map(|p| std::fs::canonicalize(p).unwrap());
        assert_eq!(cloned.as_deref(), Some(canonical.as_path()), "a Project's repository is found");
        assert_eq!((got[0].visibility.as_str(), got[0].default_branch.as_deref()), ("private", Some("main")));
        assert_eq!((got[1].cloned_at.as_deref(), got[1].archived, got[1].fork), (None, true, true));

        // The default clone folder counts too, once its origin matches.
        let home = dir.path().join("home");
        let by_folder = repos_from(&listed, &HashMap::new(), &home);
        assert_eq!(by_folder[0].cloned_at.as_deref(), Some(clone.to_string_lossy().as_ref()));
    }

    #[test]
    fn issues_read_as_github_lists_them() {
        let listed = json!([{
            "number": 7, "title": "Crash", "state": "OPEN", "author": {"login": "jo"},
            "labels": [{"name": "bug"}], "assignees": [{"login": "al"}], "updatedAt": "2026-10-08", "url": "u"
        }]);
        let got = issues_from(&listed);
        assert_eq!((got[0].number, got[0].author.as_str(), got[0].labels.clone()), (7, "jo", vec!["bug".to_string()]));
        let viewed = json!({
            "number": 7, "title": "Crash", "state": "OPEN", "author": {"login": "jo"}, "body": "it crashes",
            "labels": [], "assignees": [], "createdAt": "c", "updatedAt": "u", "url": "x",
            "comments": [{"author": {"login": "al"}, "body": "seen", "createdAt": "d", "url": "y"}]
        });
        let got = issue_from(&viewed);
        assert_eq!((got.body.as_str(), got.comments[0].author.as_str(), got.comments[0].body.as_str()), ("it crashes", "al", "seen"));
        assert_eq!(published("Creating issue in acme/widget\n\nhttps://github.com/acme/widget/issues/8\n").url, "https://github.com/acme/widget/issues/8");
    }

    #[test]
    fn reads_are_free_and_publishing_always_asks() {
        use crate::api_catalog::Risk;
        for (tool, risk) in [
            ("github_repos", Risk::Read),
            ("github_issues", Risk::Read),
            ("github_issue", Risk::Read),
            ("github_clone", Risk::Write),
            ("github_issue_create", Risk::Destructive),
            ("github_issue_comment", Risk::Destructive),
        ] {
            assert_eq!(crate::api_tools::risk_of(tool), Some(risk), "{tool}");
        }
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        assert!(issue_create(&state, &IssueCreateReq { repository: "acme/w".into(), title: " ".into(), body: String::new(), labels: vec![] })
            .unwrap_err()
            .contains("needs a title"));
        assert!(issue_comment(&state, &IssueCommentReq { repository: "acme/w".into(), number: 1, body: " ".into() })
            .unwrap_err()
            .contains("needs a body"));
        assert!(issues(&IssuesQuery { repository: "acme/w".into(), state: Some("stale".into()), limit: None })
            .unwrap_err()
            .contains("open, closed or all"));
    }
}
