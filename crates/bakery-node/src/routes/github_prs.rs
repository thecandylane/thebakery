//! The operator's pull request inbox, across the GitHub owners and
//! repositories they choose: open pull requests, each one in full, the
//! latest CI runs and their logs. All of it goes through the operator's own
//! `gh` login (the node holds no GitHub token). Reads follow the usual agent
//! reach. What changes GitHub (a review, a comment, a merge, ready for
//! review, resolving a thread, re-running a workflow) and the inbox's scope
//! are the operator's alone: hidden from agents and refused when one calls.
//!
//! A read answers at once with the node's last answer, kept on disk across
//! restarts (a cache: GitHub's answers about pull requests and runs, never a
//! token). An answer a minute old, or asked for before the operator's last
//! action, is asked for again behind the read; only the first answer ever
//! and the operator's refresh wait on GitHub. Each new answer is compared
//! with the one before: what changed goes out live as `github.pr.updated`
//! and `github.run.updated`. A pull request read in full is kept while the
//! list's copy of it stays the same.
//!
//! The node also asks again on its own (`keep_fresh`): every two minutes
//! while an app window is visible, every ten otherwise, so an open inbox
//! updates without a read. When GitHub says to slow down (a rate limit), it
//! waits until GitHub's reset, and the inbox says so. Each ask's outcome
//! goes out live as `github.inbox.updated`.

use crate::git::{gh, gh_json};
use crate::routes::github::{home, origin_slug, slug, text};
use crate::state::AppState;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    Json,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// A thread opened this pull request (`/git/pr`): `{repository, number,
/// url, branch}`.
pub(crate) const PR_OPENED: &str = "git.pr.opened";
const SCOPE_FILE: &str = "github-scope.json";
/// A list read this long after GitHub's last answer asks again behind it.
const FRESH: Duration = Duration::from_secs(60);
/// The node asks again on its own this often while an app window is
/// visible, and this often otherwise; it looks every half minute.
const VISIBLE_EVERY: Duration = Duration::from_secs(2 * 60);
const HIDDEN_EVERY: Duration = Duration::from_secs(10 * 60);
const LOOK_EVERY: Duration = Duration::from_secs(30);
/// A rate limit that names no reset is waited out a minute, doubling while
/// it repeats, up to fifteen.
const FIRST_WAIT: Duration = Duration::from_secs(60);
const LONGEST_WAIT: Duration = Duration::from_secs(15 * 60);
/// The most of one step's log a reply carries: its end, where failures are.
const STEP_LOG: usize = 64 * 1024;
const RUN_FIELDS: &str = "databaseId,workflowName,headBranch,displayTitle,status,conclusion,event,createdAt,updatedAt,url";
const OPERATOR_ONLY: &str = "only the operator acts on GitHub from the inbox: it is published under their name";

const PR: &str = "fragment Pr on PullRequest {
  number title url isDraft createdAt updatedAt additions deletions changedFiles
  headRefName baseRefName headRefOid mergeable mergeStateStatus reviewDecision
  author { login __typename }
  repository { nameWithOwner deleteBranchOnMerge }
  commentCount: comments { totalCount }
  lastCommit: commits(last: 1) { nodes { commit { statusCheckRollup { contexts(first: 100) { nodes {
    __typename
    ... on CheckRun { name status conclusion startedAt detailsUrl checkSuite { workflowRun { workflow { name } } } }
    ... on StatusContext { context state targetUrl createdAt }
  } } } } } }
}";

const SEARCH: &str = "query($q: String!, $endCursor: String) {
  search(query: $q, type: ISSUE, first: 50, after: $endCursor) {
    pageInfo { hasNextPage endCursor }
    nodes { ...Pr }
  }
}";

const ASKED_OF_ME: &str = "query($q: String!, $endCursor: String) {
  search(query: $q, type: ISSUE, first: 100, after: $endCursor) {
    pageInfo { hasNextPage endCursor }
    nodes { ... on PullRequest { number repository { nameWithOwner } } }
  }
}";

/// NUMBER is put in as digits.
const ONE: &str = "query($owner: String!, $name: String!) {
  repository(owner: $owner, name: $name) {
    pullRequest(number: NUMBER) {
      ...Pr
      body
      reviews(last: 100) { nodes { url author { login } state body submittedAt } }
      reviewThreads(first: 100) { nodes {
        id path line startLine diffSide isResolved isOutdated
        comments(first: 100) { nodes { author { login } body createdAt url } }
      } }
      conversation: comments(last: 100) { nodes { author { login } body createdAt url } }
      history: commits(last: 100) { nodes { commit { oid messageHeadline committedDate author { name user { login } } } } }
    }
  }
}";

fn operator_only(why: &str) -> Result<(), (StatusCode, String)> {
    if crate::actor::agent_calling() {
        return Err((StatusCode::FORBIDDEN, why.to_string()));
    }
    Ok(())
}

fn bad(e: String) -> (StatusCode, String) {
    (StatusCode::BAD_REQUEST, e)
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn nodes(connection: &Value) -> impl Iterator<Item = &Value> {
    connection["nodes"].as_array().into_iter().flatten()
}

/// The id at the end of a comment's or review's address
/// (`#issuecomment-…`, `#discussion_r…`, `#pullrequestreview-…`): GitHub's
/// ids have outgrown GraphQL's numbers.
fn id_in(url: &str) -> u64 {
    let anchor = url.rsplit_once('#').map_or("", |(_, anchor)| anchor);
    let digits = anchor.trim_start_matches(|c: char| !c.is_ascii_digit());
    digits.parse().unwrap_or_default()
}

/// An Actions check's run and job, from its address
/// (`…/actions/runs/<run>/job/<job>`).
fn run_and_job(link: &str) -> (Option<u64>, Option<u64>) {
    let Some((_, rest)) = link.split_once("/actions/runs/") else { return (None, None) };
    let mut parts = rest.split('/');
    let run = parts.next().and_then(|run| run.parse().ok());
    let job = match (parts.next(), parts.next()) {
        (Some("job"), Some(job)) => job.split(['?', '#']).next().and_then(|job| job.parse().ok()),
        _ => None,
    };
    (run, job)
}

/// `gh api graphql`, the strings given as raw variables; `pages` follows
/// the query's one connection to its end and answers the pages as a list.
fn graphql(home: &Path, query: &str, vars: &[(&str, &str)], pages: bool) -> Result<Value, String> {
    let mut args = vec!["api".to_string(), "graphql".into()];
    if pages {
        args.extend(["--paginate".into(), "--slurp".into()]);
    }
    args.extend(["-f".into(), format!("query={query}")]);
    for (name, value) in vars {
        args.extend(["-f".into(), format!("{name}={value}")]);
    }
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    gh_json(home, &args)
}

/// Every pull request a search finds.
fn search(home: &Path, query: &str, q: &str) -> Result<Vec<Value>, String> {
    let pages = graphql(home, query, &[("q", q)], true)?;
    Ok(pages
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|page| nodes(&page["data"]["search"]).cloned().collect::<Vec<_>>())
        .filter(|pr| pr["number"].is_u64())
        .collect())
}

/// `gh api` with `input` as the request's JSON body.
fn gh_send(home: &Path, args: &[&str], input: &Value) -> Result<Value, String> {
    if !crate::git::gh_available() {
        return Err("the GitHub CLI (gh) is not installed or not on PATH".to_string());
    }
    let mut child = Command::new("gh")
        .current_dir(home)
        .args(args)
        .args(["--input", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to run gh: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(input.to_string().as_bytes()).map_err(|e| format!("failed to run gh: {e}"))?;
    }
    let out = child.wait_with_output().map_err(|e| format!("failed to run gh: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    serde_json::from_slice(&out.stdout).map_err(|e| format!("gh answered oddly: {e}"))
}

/// `f` over each item, several at once: each waits on GitHub, not the CPU.
fn at_once<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let f = &f;
    items
        .chunks(8)
        .flat_map(|chunk| {
            std::thread::scope(|s| {
                let asked: Vec<_> = chunk.iter().map(|item| s.spawn(move || f(item))).collect();
                asked.into_iter().map(|a| a.join().expect("a GitHub read panicked")).collect::<Vec<_>>()
            })
        })
        .collect()
}

/// For a URL path: everything but a branch name's plain characters escaped.
fn escaped(part: &str) -> String {
    part.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'/' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// The repository and number of a pull request's address, as `gh pr
/// create` prints it last.
pub(crate) fn pr_address(out: &str) -> Option<(String, u64, String)> {
    let url = out.trim().lines().last()?.trim();
    let (repository, number) = url.strip_prefix("https://github.com/")?.split_once("/pull/")?;
    Some((slug(repository).ok()?, number.trim_end_matches('/').parse().ok()?, url.to_string()))
}

// ---- Scope ----

#[derive(schemars::JsonSchema, Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Owner {
    pub(crate) login: String,
    /// user or org.
    pub(crate) kind: String,
    pub(crate) on: bool,
}

#[derive(schemars::JsonSchema, Debug, Clone, Serialize, Deserialize)]
pub(crate) struct RepoOn {
    /// owner/name.
    pub(crate) repository: String,
    pub(crate) on: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Saved {
    #[serde(default)]
    owners: Vec<Owner>,
    #[serde(default)]
    repos: Vec<RepoOn>,
}

#[derive(schemars::JsonSchema, Debug, Clone, Serialize)]
pub(crate) struct GithubScope {
    /// The operator's GitHub login.
    pub(crate) me: String,
    /// The operator, their organizations, then any other owner added; each
    /// on unless turned off.
    pub(crate) owners: Vec<Owner>,
    /// Repositories turned on or off whatever their owner is.
    pub(crate) repos: Vec<RepoOn>,
    /// What GitHub couldn't answer (the operator's organizations, say).
    pub(crate) problems: Vec<String>,
}

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct OwnerOn {
    pub(crate) login: String,
    pub(crate) on: bool,
}

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct ScopeReq {
    /// Owners to turn on or off; a login not listed yet is added.
    #[serde(default)]
    pub(crate) owners: Vec<OwnerOn>,
    /// Repositories to turn on or off whatever their owner is.
    #[serde(default)]
    pub(crate) repos: Vec<RepoOn>,
}

fn saved(state: &AppState) -> Saved {
    std::fs::read(state.base.join(SCOPE_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn save(state: &AppState, saved: &Saved) -> Result<(), String> {
    let text = serde_json::to_vec_pretty(saved).map_err(|e| e.to_string())?;
    let path = state.base.join(SCOPE_FILE);
    let tmp = state.base.join(format!("{SCOPE_FILE}.{}", crate::node::mint_id("tmp")));
    std::fs::write(&tmp, text).and_then(|_| std::fs::rename(&tmp, &path)).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("could not save the inbox's scope: {e}")
    })
}

/// The operator and their organizations as GitHub has them now, all on,
/// with what was saved laid over them.
fn owners(me: &str, orgs: &[String], saved: &Saved) -> Vec<Owner> {
    let mut owners: Vec<Owner> = std::iter::once((me, "user"))
        .chain(orgs.iter().map(|org| (org.as_str(), "org")))
        .map(|(login, kind)| Owner { login: login.to_string(), kind: kind.to_string(), on: true })
        .collect();
    for s in &saved.owners {
        match owners.iter_mut().find(|o| o.login.eq_ignore_ascii_case(&s.login)) {
            Some(o) => o.on = s.on,
            None => owners.push(s.clone()),
        }
    }
    owners
}

fn owner_on(scope: &GithubScope, repository: &str) -> bool {
    let owner = repository.split('/').next().unwrap_or_default();
    scope.owners.iter().any(|o| o.on && o.login.eq_ignore_ascii_case(owner))
}

/// Whether a repository is in the inbox: its own setting, else its owner's.
fn in_scope(scope: &GithubScope, repository: &str) -> bool {
    match scope.repos.iter().find(|r| r.repository.eq_ignore_ascii_case(repository)) {
        Some(r) => r.on,
        None => owner_on(scope, repository),
    }
}

/// One search per owner on, and one per repository on whose owner is not:
/// (what it is for, the search).
fn searches(scope: &GithubScope) -> Vec<(String, String)> {
    let open = "is:pr is:open archived:false";
    let owners = scope.owners.iter().filter(|o| o.on).map(|o| {
        let qualifier = if o.kind == "org" { "org" } else { "user" };
        (o.login.clone(), format!("{open} {qualifier}:{}", o.login))
    });
    let repos = scope
        .repos
        .iter()
        .filter(|r| r.on && !owner_on(scope, &r.repository))
        .map(|r| (r.repository.clone(), format!("{open} repo:{}", r.repository)));
    owners.chain(repos).collect()
}

fn login(login: &str) -> Result<String, String> {
    let login = login.trim();
    let fits = !login.is_empty()
        && login.len() <= 39
        && !login.starts_with('-')
        && login.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    fits.then(|| login.to_string()).ok_or_else(|| format!("{login:?} is not a GitHub login"))
}

/// The operator's account as GitHub last answered: their login and
/// organizations.
#[derive(Clone, Serialize, Deserialize)]
struct Account {
    me: String,
    orgs: Vec<String>,
    /// What GitHub couldn't answer (the operator's organizations, say).
    problems: Vec<String>,
}

/// Asks GitHub who the operator is now, and keeps the answer.
fn ask_account(state: &AppState) -> Result<Account, String> {
    let home = home()?;
    let me = gh(&home, &["api", "user", "--jq", ".login"])?.trim().to_string();
    if me.is_empty() {
        return Err("gh is signed in to no account: run gh auth login".to_string());
    }
    let mut problems = Vec::new();
    let orgs = gh(&home, &["api", "user/orgs", "--paginate", "--jq", ".[].login"]).unwrap_or_else(|e| {
        problems.push(format!("your organizations: {e}"));
        String::new()
    });
    let orgs = orgs.lines().map(str::trim).filter(|o| !o.is_empty()).map(str::to_string).collect();
    let account = Account { me, orgs, problems };
    keep_file(&cache_dir(state).join(ACCOUNT.file), &account);
    ACCOUNT.entry(&mut ACCOUNT.lock(), state).answer = Some(account.clone());
    Ok(account)
}

fn scope_with(account: Account, saved: Saved) -> GithubScope {
    GithubScope { owners: owners(&account.me, &account.orgs, &saved), repos: saved.repos, me: account.me, problems: account.problems }
}

/// The scope as the operator set it, over their account as GitHub last
/// answered (asked again with each list of pull requests).
pub(crate) fn scope(state: &AppState) -> Result<GithubScope, String> {
    let kept = ACCOUNT.entry(&mut ACCOUNT.lock(), state).answer.clone();
    let account = match kept {
        Some(account) => account,
        None => ask_account(state)?,
    };
    Ok(scope_with(account, saved(state)))
}

pub(crate) fn set_scope(state: &AppState, req: &ScopeReq) -> Result<GithubScope, (StatusCode, String)> {
    operator_only("only the operator chooses what the pull request inbox shows")?;
    let current = scope(state).map_err(bad)?;
    let home = home().map_err(bad)?;
    let mut saved = saved(state);
    for o in &req.owners {
        let asked = login(&o.login).map_err(bad)?;
        let (login, kind) = match current.owners.iter().find(|k| k.login.eq_ignore_ascii_case(&asked)) {
            Some(known) => (known.login.clone(), known.kind.clone()),
            None => {
                let user = gh_json(&home, &["api", &format!("users/{asked}")]).map_err(bad)?;
                let kind = if user["type"] == "Organization" { "org" } else { "user" };
                (user["login"].as_str().unwrap_or(&asked).to_string(), kind.to_string())
            }
        };
        saved.owners.retain(|s| !s.login.eq_ignore_ascii_case(&login));
        saved.owners.push(Owner { login, kind, on: o.on });
    }
    for r in &req.repos {
        let repository = slug(&r.repository).map_err(bad)?;
        saved.repos.retain(|s| !s.repository.eq_ignore_ascii_case(&repository));
        saved.repos.push(RepoOn { repository, on: r.on });
    }
    save(state, &saved).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    stale(state);
    scope(state).map_err(bad)
}

// ---- Links to the Bakery ----

/// Which thread and project each pull request and run belongs to.
#[derive(Default)]
struct Links {
    /// (repository, number) → the thread that opened it.
    opened: HashMap<(String, u64), String>,
    /// (repository, branch) → a live worktree on that branch, never a
    /// checkout opened in place.
    on_branch: HashMap<(String, String), String>,
    /// repository → the project it is a root of.
    projects: HashMap<String, String>,
}

impl Links {
    fn thread(&self, repository: &str, number: Option<u64>, branch: &str) -> Option<String> {
        let repository = repository.to_ascii_lowercase();
        number
            .and_then(|n| self.opened.get(&(repository.clone(), n)))
            .or_else(|| self.on_branch.get(&(repository, branch.to_string())))
            .cloned()
    }

    fn project(&self, repository: &str) -> Option<String> {
        self.projects.get(&repository.to_ascii_lowercase()).cloned()
    }
}

fn links(state: &AppState) -> Links {
    let mut links = Links::default();
    // Most recent first, so the latest thread on a branch wins.
    let streams = crate::state::known_streams(state).unwrap_or_default();
    {
        let heart = state.heart.lock().unwrap();
        for s in &streams {
            for e in heart.events_of_family(&s.stream_id, PR_OPENED).unwrap_or_default() {
                let v: Value = serde_json::from_str(&e.payload).unwrap_or_default();
                if let (Some(repository), Some(number)) = (v["repository"].as_str(), v["number"].as_u64()) {
                    links.opened.entry((repository.to_ascii_lowercase(), number)).or_insert_with(|| s.stream_id.clone());
                }
            }
        }
    }
    let mut origins: HashMap<String, Option<String>> = HashMap::new();
    for s in streams.iter().filter(|s| !s.in_place && !s.discarded && s.exists_on_disk && !s.repo_root.is_empty()) {
        let origin = origins.entry(s.repo_root.clone()).or_insert_with(|| origin_slug(Path::new(&s.repo_root)));
        let Some(repository) = origin.clone() else { continue };
        let Ok(branch) = crate::git::run(Path::new(&s.worktree_path), &["branch", "--show-current"]) else { continue };
        links.on_branch.entry((repository, branch.trim().to_string())).or_insert_with(|| s.stream_id.clone());
    }
    for project in crate::catalog::known_projects(state).unwrap_or_default() {
        for root in project.roots.iter().filter(|r| r.is_git) {
            if let Some(repository) = origin_slug(Path::new(&root.path)) {
                links.projects.entry(repository).or_insert_with(|| project.project_id.clone());
            }
        }
    }
    links
}

// ---- Pull requests ----

#[derive(schemars::JsonSchema, Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct CheckCounts {
    pub(crate) pass: u32,
    /// Cancelled ones too.
    pub(crate) fail: u32,
    pub(crate) pending: u32,
    pub(crate) skipped: u32,
}

#[derive(schemars::JsonSchema, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct GithubPr {
    /// owner/name, as GitHub writes it.
    pub(crate) repository: String,
    pub(crate) number: u64,
    pub(crate) title: String,
    pub(crate) url: String,
    pub(crate) author: String,
    pub(crate) author_is_bot: bool,
    /// The branch it would merge.
    pub(crate) branch: String,
    /// The branch it would merge into.
    pub(crate) base: String,
    pub(crate) draft: bool,
    pub(crate) created_at: String,
    pub(crate) updated_at: String,
    /// APPROVED, CHANGES_REQUESTED or REVIEW_REQUIRED.
    pub(crate) review_decision: Option<String>,
    /// A review is asked of the operator, directly or through a team.
    pub(crate) requested_from_me: bool,
    /// The head commit's checks, each one's latest attempt.
    pub(crate) checks: CheckCounts,
    /// MERGEABLE, CONFLICTING or UNKNOWN.
    pub(crate) mergeable: String,
    /// CLEAN, BLOCKED, BEHIND, DIRTY, UNSTABLE, HAS_HOOKS, DRAFT or UNKNOWN.
    pub(crate) merge_state: String,
    pub(crate) additions: u64,
    pub(crate) deletions: u64,
    pub(crate) changed_files: u64,
    /// The repository deletes a pull request's branch once it is merged
    /// (GitHub's "Automatically delete head branches"): where the merge's
    /// delete-branch choice starts.
    pub(crate) delete_branch_on_merge: bool,
    /// Its conversation's comments.
    pub(crate) comments: u64,
    /// The thread that opened it, else a live worktree on its branch in a
    /// clone of its repository; never a checkout opened in place.
    pub(crate) thread_stream_id: Option<String>,
    /// The project its repository is a root of.
    pub(crate) project_id: Option<String>,
}

#[derive(schemars::JsonSchema, Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Prs {
    pub(crate) me: String,
    /// When GitHub gave this answer.
    pub(crate) fetched_at: String,
    /// Most recently updated first.
    pub(crate) prs: Vec<GithubPr>,
    /// Owners or repositories GitHub couldn't answer for; the rest did.
    pub(crate) problems: Vec<String>,
}

#[derive(schemars::JsonSchema, Debug, Clone, Serialize, Deserialize)]
pub(crate) struct GithubCheck {
    pub(crate) name: String,
    /// GitHub's state: SUCCESS, FAILURE, IN_PROGRESS, PENDING, SKIPPED…
    pub(crate) state: String,
    /// pass, fail, pending, skipping or cancel, as gh pr checks has them.
    pub(crate) bucket: String,
    pub(crate) workflow: String,
    pub(crate) link: String,
    /// The Actions run and job behind it; none for a commit status or
    /// another app's check.
    pub(crate) run_id: Option<u64>,
    pub(crate) job_id: Option<u64>,
}

fn bucket(state: &str) -> &'static str {
    match state {
        "SUCCESS" => "pass",
        "SKIPPED" | "NEUTRAL" => "skipping",
        "CANCELLED" => "cancel",
        "FAILURE" | "ERROR" | "TIMED_OUT" | "ACTION_REQUIRED" | "STARTUP_FAILURE" => "fail",
        _ => "pending",
    }
}

/// The head commit's checks, each one's latest attempt only: a re-run adds
/// a check beside the one it replaces.
fn checks_of(pr: &Value) -> Vec<GithubCheck> {
    let rollup = &pr["lastCommit"]["nodes"][0]["commit"]["statusCheckRollup"]["contexts"];
    let mut latest: Vec<(String, GithubCheck)> = Vec::new();
    for c in nodes(rollup) {
        let (started, check) = if c["__typename"] == "StatusContext" {
            let state = text(&c["state"]);
            let check = GithubCheck {
                name: text(&c["context"]),
                bucket: bucket(&state).to_string(),
                state,
                workflow: String::new(),
                link: text(&c["targetUrl"]),
                run_id: None,
                job_id: None,
            };
            (text(&c["createdAt"]), check)
        } else {
            let state = text(if c["status"] == "COMPLETED" { &c["conclusion"] } else { &c["status"] });
            let link = text(&c["detailsUrl"]);
            let (run_id, job_id) = run_and_job(&link);
            let check = GithubCheck {
                name: text(&c["name"]),
                bucket: bucket(&state).to_string(),
                state,
                workflow: text(&c["checkSuite"]["workflowRun"]["workflow"]["name"]),
                link,
                run_id,
                job_id,
            };
            (text(&c["startedAt"]), check)
        };
        match latest.iter_mut().find(|(_, k)| k.workflow == check.workflow && k.name == check.name) {
            Some(slot) if slot.0 < started => *slot = (started, check),
            Some(_) => {}
            None => latest.push((started, check)),
        }
    }
    latest.into_iter().map(|(_, check)| check).collect()
}

fn counts(checks: &[GithubCheck]) -> CheckCounts {
    let mut n = CheckCounts::default();
    for check in checks {
        match check.bucket.as_str() {
            "pass" => n.pass += 1,
            "fail" | "cancel" => n.fail += 1,
            "skipping" => n.skipped += 1,
            _ => n.pending += 1,
        }
    }
    n
}

fn or_unknown(v: &Value) -> String {
    v.as_str().filter(|s| !s.is_empty()).unwrap_or("UNKNOWN").to_string()
}

fn pr_from(p: &Value, asked_of_me: &HashSet<Key>) -> GithubPr {
    let repository = text(&p["repository"]["nameWithOwner"]);
    let number = p["number"].as_u64().unwrap_or_default();
    let author = text(&p["author"]["login"]);
    GithubPr {
        requested_from_me: asked_of_me.contains(&key_of(&repository, number)),
        author_is_bot: p["author"]["__typename"] == "Bot" || author.ends_with("[bot]"),
        title: text(&p["title"]),
        url: text(&p["url"]),
        branch: text(&p["headRefName"]),
        base: text(&p["baseRefName"]),
        draft: p["isDraft"] == true,
        created_at: text(&p["createdAt"]),
        updated_at: text(&p["updatedAt"]),
        review_decision: p["reviewDecision"].as_str().filter(|s| !s.is_empty()).map(str::to_string),
        checks: counts(&checks_of(p)),
        mergeable: or_unknown(&p["mergeable"]),
        merge_state: or_unknown(&p["mergeStateStatus"]),
        additions: p["additions"].as_u64().unwrap_or_default(),
        deletions: p["deletions"].as_u64().unwrap_or_default(),
        changed_files: p["changedFiles"].as_u64().unwrap_or_default(),
        delete_branch_on_merge: p["repository"]["deleteBranchOnMerge"] == true,
        comments: p["commentCount"]["totalCount"].as_u64().unwrap_or_default(),
        thread_stream_id: None,
        project_id: None,
        repository,
        number,
        author,
    }
}

fn link(links: &Links, pr: &mut GithubPr) {
    pr.thread_stream_id = links.thread(&pr.repository, Some(pr.number), &pr.branch);
    pr.project_id = links.project(&pr.repository);
}

/// A pull request across the inbox: `owner/name#number`, in lowercase.
type Key = String;

fn key_of(repository: &str, number: u64) -> Key {
    format!("{}#{number}", repository.to_ascii_lowercase())
}

fn key(pr: &GithubPr) -> Key {
    key_of(&pr.repository, pr.number)
}

/// One answer from GitHub, with each pull request's head commit, to tell a
/// new push from the counts alone.
#[derive(Clone, Serialize, Deserialize)]
struct PrList {
    prs: Prs,
    heads: HashMap<Key, String>,
}

fn fetch_prs(state: &AppState) -> Result<PrList, String> {
    let scope = scope_with(ask_account(state)?, saved(state));
    let home = home()?;
    let mut problems = scope.problems.clone();
    let asked_of_me: HashSet<Key> = match search(&home, ASKED_OF_ME, "is:pr is:open archived:false review-requested:@me") {
        Ok(found) => found.iter().map(|p| key_of(&text(&p["repository"]["nameWithOwner"]), p["number"].as_u64().unwrap_or_default())).collect(),
        Err(e) => {
            problems.push(format!("reviews asked of you: {e}"));
            HashSet::new()
        }
    };
    let searches = searches(&scope);
    let query = format!("{SEARCH}\n{PR}");
    let answers = at_once(&searches, |(_, q)| search(&home, &query, q));
    let links = links(state);
    let mut prs = Vec::new();
    let mut heads = HashMap::new();
    for ((label, _), answer) in searches.iter().zip(answers) {
        match answer {
            Ok(found) => {
                for p in found {
                    let mut pr = pr_from(&p, &asked_of_me);
                    if !in_scope(&scope, &pr.repository) || heads.contains_key(&key(&pr)) {
                        continue;
                    }
                    link(&links, &mut pr);
                    heads.insert(key(&pr), text(&p["headRefOid"]));
                    prs.push(pr);
                }
            }
            Err(e) => problems.push(format!("{label}: {e}")),
        }
    }
    prs.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    if problems.is_empty() {
        drop_views_unlisted(state, &prs);
    }
    Ok(PrList { prs: Prs { me: scope.me, fetched_at: now(), prs, problems }, heads })
}

/// What changed between two answers for one pull request.
fn changes(was: &GithubPr, was_head: Option<&String>, is: &GithubPr, head: Option<&String>) -> Vec<&'static str> {
    let mut changed = Vec::new();
    if was.checks != is.checks {
        changed.push("checks");
    }
    if (&was.review_decision, was.requested_from_me) != (&is.review_decision, is.requested_from_me) {
        changed.push("review");
    }
    if (&was.mergeable, &was.merge_state) != (&is.mergeable, &is.merge_state) {
        changed.push("mergeable");
    }
    if was.comments != is.comments {
        changed.push("comments");
    }
    if was_head != head {
        changed.push("commits");
    }
    if (was.draft, &was.title, &was.base) != (is.draft, &is.title, &is.base) {
        changed.push("state");
    }
    changed
}

/// Each change since the last answer, live. A pull request gone from a
/// whole answer (closed, merged, or out of the scope) goes out as null;
/// from an answer with a problem, it may only be unseen, so it doesn't.
fn pr_updates(before: &PrList, now: &PrList) -> Vec<Value> {
    let was: HashMap<Key, &GithubPr> = before.prs.prs.iter().map(|p| (key(p), p)).collect();
    let mut updates = Vec::new();
    for pr in &now.prs.prs {
        let k = key(pr);
        let changed = match was.get(&k) {
            Some(was) => changes(was, before.heads.get(&k), pr, now.heads.get(&k)),
            None => vec!["state"],
        };
        if !changed.is_empty() {
            updates.push(json!({"kind": "github.pr.updated", "repository": pr.repository, "number": pr.number, "changed": changed, "pr": pr}));
        }
    }
    if now.prs.problems.is_empty() {
        for pr in before.prs.prs.iter().filter(|p| !now.heads.contains_key(&key(p))) {
            updates.push(json!({"kind": "github.pr.updated", "repository": pr.repository, "number": pr.number, "changed": ["state"], "pr": null}));
        }
    }
    updates
}

/// An answer from GitHub that carries its own problems.
trait Answer: Clone + Serialize + DeserializeOwned + Send + 'static {
    fn problems(&mut self) -> &mut Vec<String>;
    /// When GitHub gave it, and what it couldn't answer.
    fn seen(&self) -> (&str, &[String]);
}

impl Answer for PrList {
    fn problems(&mut self) -> &mut Vec<String> {
        &mut self.prs.problems
    }

    fn seen(&self) -> (&str, &[String]) {
        (&self.prs.fetched_at, &self.prs.problems)
    }
}

impl Answer for Runs {
    fn problems(&mut self) -> &mut Vec<String> {
        &mut self.problems
    }

    fn seen(&self) -> (&str, &[String]) {
        (&self.fetched_at, &self.problems)
    }
}

/// One node's last answer from GitHub.
struct Kept<T> {
    answer: Option<T>,
    /// When the ask behind it began; none for an answer read from disk.
    asked: Option<Instant>,
    /// When the operator last changed something on GitHub, or the scope.
    acted: Option<Instant>,
    /// Why the last ask failed, while the answer before stands in.
    failed: Option<String>,
    /// The node is asking GitHub now.
    asking: bool,
    /// When the last ask began, whatever came of it.
    tried: Option<Instant>,
    /// GitHub's rate limit: the node asks again only after this, and how
    /// long it waited last time.
    wait_until: Option<Instant>,
    waited: Option<Duration>,
}

impl<T> Kept<T> {
    /// Asked within the minute, and since the operator last acted.
    fn fresh(&self) -> bool {
        self.asked.is_some_and(|asked| asked.elapsed() < FRESH && self.acted.is_none_or(|acted| asked > acted))
    }

    /// Until when GitHub's rate limit holds the node back, if it does.
    fn waiting(&self) -> Option<Instant> {
        self.wait_until.filter(|until| *until > Instant::now())
    }
}

/// An instant ahead, on the clock.
fn on_clock(at: Instant) -> chrono::DateTime<chrono::Local> {
    chrono::Local::now() + chrono::Duration::from_std(at.saturating_duration_since(Instant::now())).unwrap_or_default()
}

/// What the inbox says while GitHub's rate limit holds the node back.
fn waiting_line(until: Instant) -> String {
    format!("GitHub asked the Bakery to slow down (its rate limit): the inbox asks again at {}", on_clock(until).format("%H:%M"))
}

/// Each node's last answers (tests run several nodes in one process), kept
/// on disk for the node's next start.
struct Last<T> {
    /// Its file in the node's GitHub cache.
    file: &'static str,
    kept: Mutex<BTreeMap<PathBuf, Kept<T>>>,
    /// An ask ended.
    ended: Condvar,
    /// GitHub's account of the operator's rate limits (`/rate_limit`).
    rate: fn() -> Option<Value>,
}

static ACCOUNT: Last<Account> = Last::new("account.json", rate_limit);
static PRS: Last<PrList> = Last::new("prs.json", rate_limit);
static RUNS: Last<Runs> = Last::new("runs.json", rate_limit);

/// GitHub's account of the operator's rate limits; asking it costs none.
fn rate_limit() -> Option<Value> {
    gh_json(&home().ok()?, &["api", "rate_limit"]).ok()
}

impl<T: DeserializeOwned> Last<T> {
    const fn new(file: &'static str, rate: fn() -> Option<Value>) -> Self {
        Last { file, kept: Mutex::new(BTreeMap::new()), ended: Condvar::new(), rate }
    }

    /// The list's name in what goes out live.
    fn list(&self) -> &'static str {
        self.file.trim_end_matches(".json")
    }

    fn lock(&self) -> MutexGuard<'_, BTreeMap<PathBuf, Kept<T>>> {
        self.kept.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The node's own, its answer read from disk the first time.
    fn entry<'a>(&self, kept: &'a mut BTreeMap<PathBuf, Kept<T>>, state: &AppState) -> &'a mut Kept<T> {
        kept.entry(state.base.to_path_buf()).or_insert_with(|| Kept {
            answer: std::fs::read(cache_dir(state).join(self.file)).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()),
            asked: None,
            acted: None,
            failed: None,
            asking: false,
            tried: None,
            wait_until: None,
            waited: None,
        })
    }
}

/// The node's cache of GitHub's answers.
fn cache_dir(state: &AppState) -> PathBuf {
    state.base.join("cache").join("github")
}

/// Written whole, readable by the operator only. It is only a cache: a
/// failed write is let go.
fn keep_file(path: &Path, value: &impl Serialize) {
    use std::os::unix::fs::OpenOptionsExt;
    let tmp = path.with_extension(crate::node::mint_id("tmp"));
    let wrote = path
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|_| serde_json::to_vec(value).map_err(std::io::Error::other))
        .and_then(|text| std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?.write_all(&text))
        .and_then(|_| std::fs::rename(&tmp, path));
    if wrote.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// After the operator changes something on GitHub, or the scope: the lists
/// are asked again at the next read, and each pull request read in full.
fn stale(state: &AppState) {
    let acted = Some(Instant::now());
    PRS.entry(&mut PRS.lock(), state).acted = acted;
    RUNS.entry(&mut RUNS.lock(), state).acted = acted;
    let _ = std::fs::remove_dir_all(views_dir(state));
}

/// The node's last answer, asked for again behind the read once it is no
/// longer fresh: a read waits on GitHub only for the first answer ever, or
/// the operator's refresh.
fn latest<T: Answer>(
    state: &AppState,
    last: &'static Last<T>,
    refresh: bool,
    fetch: fn(&AppState) -> Result<T, String>,
    updates: fn(&T, &T) -> Vec<Value>,
) -> Result<T, String> {
    let mut kept = last.lock();
    let entry = last.entry(&mut kept, state);
    if let (false, Some(answer)) = (refresh, &entry.answer) {
        let mut answer = answer.clone();
        if let Some(why) = &entry.failed {
            answer.problems().insert(0, format!("GitHub didn't answer just now, so this is its earlier answer: {why}"));
        }
        if let Some(until) = entry.waiting() {
            answer.problems().insert(0, waiting_line(until));
        }
        if !entry.fresh() && !entry.asking && entry.waiting().is_none() {
            entry.asking = true;
            let state = state.clone();
            std::thread::spawn(move || ask(&state, last, fetch, updates));
        }
        return Ok(answer);
    }
    // An ask under way is waited out, then asked again: the operator's
    // refresh gets an answer from after their action, never before it.
    let mut kept = last.ended.wait_while(kept, |k| k.get(state.base.as_path()).is_some_and(|e| e.asking)).unwrap_or_else(|e| e.into_inner());
    let entry = last.entry(&mut kept, state);
    if let (false, Some(answer)) = (refresh, &entry.answer) {
        return Ok(answer.clone());
    }
    entry.asking = true;
    drop(kept);
    ask(state, last, fetch, updates)
}

/// Asks GitHub for the node (the caller marked it asking) and keeps a new
/// answer, on disk too; what changed since the one before goes out live,
/// and so does the list's state. A rate limit GitHub signals holds the node
/// back until its reset.
fn ask<T: Answer>(state: &AppState, last: &Last<T>, fetch: fn(&AppState) -> Result<T, String>, updates: fn(&T, &T) -> Vec<Value>) -> Result<T, String> {
    let began = Instant::now();
    let answer = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| fetch(state))).unwrap_or_else(|_| Err("asking GitHub failed".to_string()));
    if let Ok(answer) = &answer {
        keep_file(&cache_dir(state).join(last.file), answer);
    }
    let said = match &answer {
        Ok(answer) => answer.seen().1.to_vec(),
        Err(why) => vec![why.clone()],
    };
    let waited = last.entry(&mut last.lock(), state).waited;
    let wait = limit_wait(&said, last.rate, waited);
    let mut kept = last.lock();
    let entry = last.entry(&mut kept, state);
    match &answer {
        Ok(answer) => {
            if let Some(before) = &entry.answer {
                for update in updates(before, answer) {
                    let _ = state.tx.send(update.to_string());
                }
            }
            (entry.answer, entry.asked, entry.failed) = (Some(answer.clone()), Some(began), None);
        }
        Err(why) => entry.failed = Some(why.clone()),
    }
    (entry.tried, entry.wait_until, entry.waited) = (Some(began), wait.map(|wait| Instant::now() + wait), wait);
    entry.asking = false;
    if let Some(held) = &entry.answer {
        let (fetched_at, problems) = held.seen();
        let mut problems = problems.to_vec();
        problems.extend(entry.failed.iter().map(|why| format!("GitHub didn't answer just now, so this is its earlier answer: {why}")));
        problems.splice(0..0, entry.waiting().map(waiting_line));
        let waiting_until = entry.waiting().map(|until| on_clock(until).to_rfc3339());
        let told = json!({"kind": "github.inbox.updated", "list": last.list(), "fetched_at": fetched_at, "problems": problems, "waiting_until": waiting_until});
        let _ = state.tx.send(told.to_string());
    }
    last.ended.notify_all();
    answer
}

/// How long GitHub's rate limit holds the node back, from what an ask's
/// errors and problems say: none when they say nothing of a limit. A 403 or
/// 429, or a word of a rate limit, has GitHub's account looked at: a limit
/// spent waits until its reset. A rate limit with some left (a secondary
/// one) waits its Retry-After, else a minute, doubling while it repeats. A
/// 403 with limits left is no limit (an organization's sign-in wall, say).
fn limit_wait(said: &[String], rate: fn() -> Option<Value>, waited: Option<Duration>) -> Option<Duration> {
    let said: Vec<String> = said.iter().map(|s| s.to_ascii_lowercase()).collect();
    let limited = said.iter().any(|s| s.contains("rate limit") || s.contains("(http 429)"));
    if !limited && !said.iter().any(|s| s.contains("(http 403)")) {
        return None;
    }
    let account = rate();
    let spent = ["core", "graphql", "search"]
        .iter()
        .filter_map(|kind| account.as_ref().map(|a| &a["resources"][kind]))
        .filter(|r| r["remaining"].as_u64() == Some(0))
        .filter_map(|r| r["reset"].as_i64())
        .max();
    if let Some(reset) = spent {
        let left = reset - chrono::Utc::now().timestamp();
        return Some(Duration::from_secs(left.max(0) as u64 + 5));
    }
    if !limited {
        return None;
    }
    let retry_after = said.iter().find_map(|s| {
        let after = &s[s.find("retry-after")? + "retry-after".len()..];
        let digits: String = after.chars().skip_while(|c| !c.is_ascii_digit()).take_while(char::is_ascii_digit).collect();
        digits.parse().ok().map(Duration::from_secs)
    });
    Some(retry_after.unwrap_or_else(|| waited.map_or(FIRST_WAIT, |before| (before * 2).min(LONGEST_WAIT))))
}

/// Asks GitHub again when the node's last ask is `every` old and GitHub's
/// rate limit doesn't hold it back.
fn renew<T: Answer>(state: &AppState, last: &'static Last<T>, every: Duration, fetch: fn(&AppState) -> Result<T, String>, updates: fn(&T, &T) -> Vec<Value>) {
    let mut kept = last.lock();
    let entry = last.entry(&mut kept, state);
    let due = entry.tried.is_none_or(|tried| tried.elapsed() >= every);
    if !due || entry.asking || entry.waiting().is_some() {
        return;
    }
    entry.asking = true;
    drop(kept);
    let _ = ask(state, last, fetch, updates);
}

/// The node's own asking: the inbox's pull requests, then its runs, every
/// two minutes while an app window is visible, every ten otherwise.
pub(crate) fn keep_fresh(state: AppState) {
    std::thread::spawn(move || loop {
        std::thread::sleep(LOOK_EVERY);
        let every = if crate::routes::operator::app_visible(&state) { VISIBLE_EVERY } else { HIDDEN_EVERY };
        renew(&state, &PRS, every, fetch_prs, pr_updates);
        renew(&state, &RUNS, every, fetch_runs, run_updates);
    });
}

pub(crate) fn prs(state: &AppState, refresh: bool) -> Result<Prs, String> {
    latest(state, &PRS, refresh, fetch_prs, pr_updates).map(|list| list.prs)
}

/// The inbox's own copy of one pull request.
fn listed(state: &AppState, repository: &str, number: u64) -> Option<GithubPr> {
    let mut kept = PRS.lock();
    let list = PRS.entry(&mut kept, state).answer.as_ref()?;
    list.prs.prs.iter().find(|p| p.number == number && p.repository.eq_ignore_ascii_case(repository)).cloned()
}

// ---- One pull request ----

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct PrQuery {
    /// owner/name.
    pub(crate) repository: String,
    pub(crate) number: u64,
}

#[derive(schemars::JsonSchema, Debug, Serialize, Deserialize)]
pub(crate) struct PrFile {
    pub(crate) path: String,
    /// Where a renamed file was.
    pub(crate) previous_path: Option<String>,
    /// added, modified, removed or renamed.
    pub(crate) status: String,
    pub(crate) additions: u64,
    pub(crate) deletions: u64,
}

#[derive(schemars::JsonSchema, Debug, Serialize, Deserialize)]
pub(crate) struct Review {
    pub(crate) id: u64,
    pub(crate) author: String,
    /// APPROVED, CHANGES_REQUESTED, COMMENTED, DISMISSED or PENDING.
    pub(crate) state: String,
    pub(crate) body: String,
    pub(crate) submitted_at: String,
}

#[derive(schemars::JsonSchema, Debug, Serialize, Deserialize)]
pub(crate) struct Comment {
    /// What a reply in a review thread names as `in_reply_to`.
    pub(crate) id: u64,
    pub(crate) author: String,
    pub(crate) body: String,
    pub(crate) created_at: String,
    pub(crate) url: String,
}

#[derive(schemars::JsonSchema, Debug, Serialize, Deserialize)]
pub(crate) struct ReviewThread {
    /// What resolving it names as `thread_id`.
    pub(crate) id: String,
    pub(crate) path: String,
    /// None once the lines it was on are gone.
    pub(crate) line: Option<u64>,
    pub(crate) start_line: Option<u64>,
    /// LEFT (the old side) or RIGHT (the new).
    pub(crate) side: String,
    pub(crate) resolved: bool,
    pub(crate) outdated: bool,
    pub(crate) comments: Vec<Comment>,
}

#[derive(schemars::JsonSchema, Debug, Serialize, Deserialize)]
pub(crate) struct PrCommit {
    pub(crate) sha: String,
    pub(crate) subject: String,
    /// Their GitHub login, else the name on the commit.
    pub(crate) author: String,
    pub(crate) committed_at: String,
}

#[derive(schemars::JsonSchema, Debug, Serialize, Deserialize)]
pub(crate) struct PrView {
    pub(crate) pr: GithubPr,
    pub(crate) body: String,
    pub(crate) files: Vec<PrFile>,
    /// Empty and truncated when GitHub won't give a diff this big.
    pub(crate) diff: String,
    pub(crate) diff_truncated: bool,
    /// The diff shows a credential-shaped file: the app keeps it out of
    /// screenshots.
    pub(crate) secret: bool,
    pub(crate) checks: Vec<GithubCheck>,
    /// The last 100, oldest first.
    pub(crate) reviews: Vec<Review>,
    pub(crate) threads: Vec<ReviewThread>,
    /// The conversation's last 100, oldest first.
    pub(crate) comments: Vec<Comment>,
    /// The last 100, oldest first.
    pub(crate) commits: Vec<PrCommit>,
}

fn comment_from(c: &Value) -> Comment {
    Comment {
        id: id_in(c["url"].as_str().unwrap_or_default()),
        author: text(&c["author"]["login"]),
        body: text(&c["body"]),
        created_at: text(&c["createdAt"]),
        url: text(&c["url"]),
    }
}

/// `gh api …/files --paginate --slurp`: a list of pages.
fn files_from(pages: &Value) -> Vec<PrFile> {
    pages
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|page| page.as_array().cloned().unwrap_or_default())
        .map(|f| PrFile {
            path: text(&f["filename"]),
            previous_path: f["previous_filename"].as_str().map(str::to_string),
            status: match f["status"].as_str().unwrap_or_default() {
                status @ ("added" | "removed" | "renamed") => status,
                "copied" => "added",
                _ => "modified",
            }
            .to_string(),
            additions: f["additions"].as_u64().unwrap_or_default(),
            deletions: f["deletions"].as_u64().unwrap_or_default(),
        })
        .collect()
}

fn view_from(p: &Value, files: Vec<PrFile>, mut diff: String, too_big: bool) -> PrView {
    let diff_truncated = too_big || crate::routes::git::cap(&mut diff);
    let secret = crate::routes::git::shown_paths(&diff).any(|path| bakery_exec::is_credential_filename(Path::new(path)));
    PrView {
        pr: pr_from(p, &HashSet::new()),
        body: text(&p["body"]),
        files,
        diff,
        diff_truncated,
        secret,
        checks: checks_of(p),
        reviews: nodes(&p["reviews"])
            .map(|r| Review {
                id: id_in(r["url"].as_str().unwrap_or_default()),
                author: text(&r["author"]["login"]),
                state: text(&r["state"]),
                body: text(&r["body"]),
                submitted_at: text(&r["submittedAt"]),
            })
            .collect(),
        threads: nodes(&p["reviewThreads"])
            .map(|t| ReviewThread {
                id: text(&t["id"]),
                path: text(&t["path"]),
                line: t["line"].as_u64(),
                start_line: t["startLine"].as_u64(),
                side: text(&t["diffSide"]),
                resolved: t["isResolved"] == true,
                outdated: t["isOutdated"] == true,
                comments: nodes(&t["comments"]).map(comment_from).collect(),
            })
            .collect(),
        comments: nodes(&p["conversation"]).map(comment_from).collect(),
        commits: nodes(&p["history"])
            .map(|c| &c["commit"])
            .map(|c| PrCommit {
                sha: text(&c["oid"]),
                subject: text(&c["messageHeadline"]),
                author: c["author"]["user"]["login"].as_str().map(str::to_string).unwrap_or_else(|| text(&c["author"]["name"])),
                committed_at: text(&c["committedDate"]),
            })
            .collect(),
    }
}

fn views_dir(state: &AppState) -> PathBuf {
    cache_dir(state).join("pr")
}

fn view_file(state: &AppState, repository: &str, number: u64) -> PathBuf {
    views_dir(state).join(format!("{}@{number}.json", repository.to_ascii_lowercase().replace('/', "@")))
}

/// The kept view of a pull request while the list's copy of it is the same:
/// a change there (a push, a check, a comment, a review) asks GitHub again.
fn kept_view(state: &AppState, listed: Option<&GithubPr>) -> Option<PrView> {
    let listed = listed?;
    let bytes = std::fs::read(view_file(state, &listed.repository, listed.number)).ok()?;
    serde_json::from_slice::<PrView>(&bytes).ok().filter(|view| &view.pr == listed)
}

/// Keeps a listed pull request's view, unless it shows a credential-shaped
/// file: that one is asked for each time.
fn keep_view(state: &AppState, view: &PrView) {
    let file = view_file(state, &view.pr.repository, view.pr.number);
    match view.secret {
        false => keep_file(&file, view),
        true => {
            let _ = std::fs::remove_file(file);
        }
    }
}

/// Drops the views of pull requests a whole list no longer has (closed,
/// merged, out of the scope).
fn drop_views_unlisted(state: &AppState, prs: &[GithubPr]) {
    let listed: HashSet<PathBuf> = prs.iter().map(|p| view_file(state, &p.repository, p.number)).collect();
    let files = std::fs::read_dir(views_dir(state)).into_iter().flatten().flatten().map(|f| f.path());
    for gone in files.filter(|f| !listed.contains(f)) {
        let _ = std::fs::remove_file(gone);
    }
}

pub(crate) fn pr(state: &AppState, q: &PrQuery) -> Result<PrView, String> {
    let repository = slug(&q.repository)?;
    let listed = listed(state, &repository, q.number);
    if let Some(view) = kept_view(state, listed.as_ref()) {
        return Ok(view);
    }
    let (owner, name) = repository.split_once('/').unwrap_or_default();
    let number = q.number.to_string();
    let home = home()?;
    let query = format!("{}\n{PR}", ONE.replace("NUMBER", &number));
    let answer = graphql(&home, &query, &[("owner", owner), ("name", name)], false)?;
    let p = &answer["data"]["repository"]["pullRequest"];
    if !p.is_object() {
        return Err(format!("{repository} has no pull request {number}"));
    }
    let files = gh_json(&home, &["api", &format!("repos/{repository}/pulls/{number}/files"), "--paginate", "--slurp"])?;
    // GitHub refuses a diff past its own limits.
    let (diff, too_big) = match gh(&home, &["pr", "diff", &number, "-R", &repository]) {
        Ok(diff) => (diff, false),
        Err(_) => (String::new(), true),
    };
    let mut view = view_from(p, files_from(&files), diff, too_big);
    view.pr.requested_from_me = listed.as_ref().is_some_and(|p| p.requested_from_me);
    link(&links(state), &mut view.pr);
    if listed.is_some() {
        keep_view(state, &view);
    }
    Ok(view)
}

// ---- The operator's actions ----

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct LineComment {
    pub(crate) path: String,
    /// The comment's last line; with `start_line`, it spans the lines between.
    pub(crate) line: u64,
    #[serde(default)]
    pub(crate) start_line: Option<u64>,
    /// LEFT (the old side) or RIGHT (the new).
    pub(crate) side: String,
    pub(crate) body: String,
}

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct ReviewReq {
    /// owner/name.
    pub(crate) repository: String,
    pub(crate) number: u64,
    /// COMMENT, APPROVE or REQUEST_CHANGES.
    pub(crate) event: String,
    #[serde(default)]
    pub(crate) body: String,
    #[serde(default)]
    pub(crate) comments: Vec<LineComment>,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct Reviewed {
    pub(crate) review_id: u64,
    pub(crate) url: String,
}

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct CommentReq {
    /// owner/name.
    pub(crate) repository: String,
    pub(crate) number: u64,
    pub(crate) body: String,
    /// A review thread comment's id, to reply in its thread; the
    /// conversation otherwise.
    #[serde(default)]
    pub(crate) in_reply_to: Option<u64>,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct Commented {
    pub(crate) comment_id: u64,
    pub(crate) url: String,
}

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct PrMergeReq {
    /// owner/name.
    pub(crate) repository: String,
    pub(crate) number: u64,
    /// merge or squash.
    pub(crate) method: String,
    /// Delete its branch once merged; false keeps it, even in a repository
    /// that deletes merged branches itself (it is restored at the pull
    /// request's head, as GitHub's "Restore branch" does). A branch in
    /// another repository (a fork) or the default branch is never touched.
    #[serde(default)]
    pub(crate) delete_branch: bool,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct Merged {
    pub(crate) merged: bool,
    /// The commit the merge made.
    pub(crate) sha: String,
    /// Its branch is gone once the merge is done: what is true, whoever
    /// deleted it.
    pub(crate) branch_deleted: bool,
}

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct ReadyReq {
    /// owner/name.
    pub(crate) repository: String,
    pub(crate) number: u64,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct Ready {
    pub(crate) draft: bool,
}

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct ResolveReq {
    /// owner/name.
    pub(crate) repository: String,
    /// A review thread's `id`.
    pub(crate) thread_id: String,
    /// False opens it again.
    pub(crate) resolved: bool,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct Resolved {
    pub(crate) resolved: bool,
}

/// A merged pull request's branch, as the operator chose, whatever its
/// repository does on its own: deleted, or kept (restored at the pull
/// request's head once the repository has deleted it, as GitHub's "Restore
/// branch" does). Answers whether it is gone in the end.
fn settle_branch(
    keep: bool,
    auto_deletes: bool,
    exists: &dyn Fn() -> bool,
    delete: &dyn Fn() -> bool,
    restore: &dyn Fn() -> bool,
    pause: &dyn Fn(),
) -> bool {
    if !keep {
        if exists() {
            delete();
        }
        return !exists();
    }
    if auto_deletes {
        // GitHub deletes it a moment after the merge.
        for _ in 0..30 {
            if !exists() {
                restore();
                break;
            }
            pause();
        }
    }
    !exists()
}

fn sent(state: &AppState, out: Result<Value, String>) -> Result<Value, (StatusCode, String)> {
    stale(state);
    out.map_err(bad)
}

/// The operator's action on a pull request, in the Bakery's history and on
/// the pull request's thread when the inbox knows it.
fn record(state: &AppState, repository: &str, number: Option<u64>, line: Value) {
    let thread = number.and_then(|n| listed(state, repository, n)).and_then(|pr| pr.thread_stream_id);
    crate::remote::done(state, thread.as_deref(), line);
}

pub(crate) fn review(state: &AppState, req: &ReviewReq) -> Result<Reviewed, (StatusCode, String)> {
    operator_only(OPERATOR_ONLY)?;
    let repository = slug(&req.repository).map_err(bad)?;
    if !matches!(req.event.as_str(), "COMMENT" | "APPROVE" | "REQUEST_CHANGES") {
        return Err(bad(format!("{:?}: a review is COMMENT, APPROVE or REQUEST_CHANGES", req.event)));
    }
    let mut comments = Vec::new();
    for c in &req.comments {
        if !matches!(c.side.as_str(), "LEFT" | "RIGHT") {
            return Err(bad(format!("{:?}: a line comment's side is LEFT or RIGHT", c.side)));
        }
        let mut comment = json!({"path": c.path, "line": c.line, "side": c.side, "body": c.body});
        if let Some(start) = c.start_line {
            comment["start_line"] = start.into();
            comment["start_side"] = c.side.clone().into();
        }
        comments.push(comment);
    }
    let path = format!("repos/{repository}/pulls/{}/reviews", req.number);
    let body = json!({"event": req.event, "body": req.body, "comments": comments});
    let out = sent(state, gh_send(&home().map_err(bad)?, &["api", "-X", "POST", &path], &body))?;
    let url = text(&out["html_url"]);
    record(state, &repository, Some(req.number), json!({"what": "review", "event": req.event, "repository": repository, "number": req.number, "url": url}));
    Ok(Reviewed { review_id: out["id"].as_u64().unwrap_or_default(), url })
}

pub(crate) fn comment(state: &AppState, req: &CommentReq) -> Result<Commented, (StatusCode, String)> {
    operator_only(OPERATOR_ONLY)?;
    let repository = slug(&req.repository).map_err(bad)?;
    if req.body.trim().is_empty() {
        return Err(bad("a comment needs a body".to_string()));
    }
    let path = match req.in_reply_to {
        Some(id) => format!("repos/{repository}/pulls/{}/comments/{id}/replies", req.number),
        None => format!("repos/{repository}/issues/{}/comments", req.number),
    };
    let out = sent(state, gh_send(&home().map_err(bad)?, &["api", "-X", "POST", &path], &json!({"body": req.body})))?;
    let url = text(&out["html_url"]);
    record(state, &repository, Some(req.number), json!({"what": "comment", "repository": repository, "number": req.number, "url": url}));
    Ok(Commented { comment_id: out["id"].as_u64().unwrap_or_default(), url })
}

pub(crate) fn merge(state: &AppState, req: &PrMergeReq) -> Result<Merged, (StatusCode, String)> {
    operator_only(OPERATOR_ONLY)?;
    let repository = slug(&req.repository).map_err(bad)?;
    if !matches!(req.method.as_str(), "merge" | "squash") {
        return Err(bad(format!("{:?}: a merge is merge or squash", req.method)));
    }
    let home = home().map_err(bad)?;
    let path = format!("repos/{repository}/pulls/{}", req.number);
    let pr = gh_json(&home, &["api", &path]).map_err(bad)?;
    let merged = gh_send(&home, &["api", "-X", "PUT", &format!("{path}/merge")], &json!({"merge_method": req.method}));
    stale(state);
    // GitHub's reason: not mergeable (405), or the branch moved (409).
    let merged = merged.map_err(|e| match e.contains("(HTTP 405)") || e.contains("(HTTP 409)") {
        true => (StatusCode::CONFLICT, e),
        false => bad(e),
    })?;
    let head = &pr["head"];
    let branch = text(&head["ref"]);
    let ours = head["repo"]["full_name"].as_str().is_some_and(|r| r.eq_ignore_ascii_case(&repository));
    let default = head["repo"]["default_branch"].as_str() == Some(branch.as_str());
    let branch_deleted = ours && !default && !branch.is_empty() && {
        let refs = format!("repos/{repository}/git/refs");
        let one = format!("repos/{repository}/git/ref/heads/{}", escaped(&branch));
        let auto_deletes = gh(&home, &["api", &format!("repos/{repository}"), "--jq", ".delete_branch_on_merge"])
            .is_ok_and(|on| on.trim() == "true");
        let restore = json!({ "ref": format!("refs/heads/{branch}"), "sha": text(&head["sha"]) });
        settle_branch(
            !req.delete_branch,
            auto_deletes,
            &|| gh(&home, &["api", &one]).is_ok(),
            &|| gh(&home, &["api", "-X", "DELETE", &format!("{refs}/heads/{}", escaped(&branch))]).is_ok(),
            &|| gh_send(&home, &["api", "-X", "POST", &refs], &restore).is_ok(),
            &|| std::thread::sleep(Duration::from_secs(1)),
        )
    };
    let line = json!({"what": "merge", "method": req.method, "repository": repository, "number": req.number, "commit": text(&merged["sha"]), "branch_deleted": branch_deleted});
    record(state, &repository, Some(req.number), line);
    Ok(Merged { merged: merged["merged"] == true, sha: text(&merged["sha"]), branch_deleted })
}

pub(crate) fn ready(state: &AppState, req: &ReadyReq) -> Result<Ready, (StatusCode, String)> {
    operator_only(OPERATOR_ONLY)?;
    let repository = slug(&req.repository).map_err(bad)?;
    let out = gh(&home().map_err(bad)?, &["pr", "ready", &req.number.to_string(), "-R", &repository]);
    stale(state);
    out.map_err(bad)?;
    record(state, &repository, Some(req.number), json!({"what": "ready for review", "repository": repository, "number": req.number}));
    Ok(Ready { draft: false })
}

pub(crate) fn resolve(state: &AppState, req: &ResolveReq) -> Result<Resolved, (StatusCode, String)> {
    operator_only(OPERATOR_ONLY)?;
    slug(&req.repository).map_err(bad)?;
    let thread = req.thread_id.trim();
    if thread.is_empty() || !thread.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '=')) {
        return Err(bad(format!("{thread:?} is not a review thread's id")));
    }
    let action = if req.resolved { "resolveReviewThread" } else { "unresolveReviewThread" };
    let mutation = format!("mutation($id: ID!) {{ {action}(input: {{threadId: $id}}) {{ thread {{ isResolved }} }} }}");
    let out = sent(state, graphql(&home().map_err(bad)?, &mutation, &[("id", thread)], false))?;
    let what = if req.resolved { "thread resolved" } else { "thread reopened" };
    record(state, &req.repository, None, json!({"what": what, "repository": req.repository, "thread_id": thread}));
    Ok(Resolved { resolved: out["data"][action]["thread"]["isResolved"] == true })
}

// ---- CI runs ----

#[derive(schemars::JsonSchema, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct GithubRun {
    /// owner/name.
    pub(crate) repository: String,
    pub(crate) run_id: u64,
    pub(crate) branch: String,
    pub(crate) workflow: String,
    pub(crate) title: String,
    /// queued, in_progress or completed.
    pub(crate) status: String,
    /// success, failure, cancelled…; empty until it completes.
    pub(crate) conclusion: String,
    /// What started it: push, pull_request, schedule…
    pub(crate) event: String,
    pub(crate) created_at: String,
    pub(crate) updated_at: String,
    pub(crate) url: String,
    /// A live worktree on its branch in a clone of its repository.
    pub(crate) thread_stream_id: Option<String>,
    pub(crate) project_id: Option<String>,
}

#[derive(schemars::JsonSchema, Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Runs {
    pub(crate) fetched_at: String,
    /// The latest of each workflow on each branch, newest first.
    pub(crate) runs: Vec<GithubRun>,
    /// Repositories GitHub couldn't answer for; the rest did.
    pub(crate) problems: Vec<String>,
}

fn run_from(r: &Value, repository: &str, links: &Links) -> GithubRun {
    let branch = text(&r["headBranch"]);
    GithubRun {
        repository: repository.to_string(),
        run_id: r["databaseId"].as_u64().unwrap_or_default(),
        workflow: text(&r["workflowName"]),
        title: text(&r["displayTitle"]),
        status: text(&r["status"]),
        conclusion: text(&r["conclusion"]),
        event: text(&r["event"]),
        created_at: text(&r["createdAt"]),
        updated_at: text(&r["updatedAt"]),
        url: text(&r["url"]),
        thread_stream_id: links.thread(repository, None, &branch),
        project_id: links.project(repository),
        branch,
    }
}

/// `gh run list` answers newest first, so the first run of each workflow
/// on each branch is its latest.
fn latest_runs(list: &Value, repository: &str, links: &Links) -> Vec<GithubRun> {
    let mut seen = HashSet::new();
    list.as_array()
        .into_iter()
        .flatten()
        .filter(|r| seen.insert((text(&r["headBranch"]), text(&r["workflowName"]))))
        .map(|r| run_from(r, repository, links))
        .collect()
}

fn fetch_runs(state: &AppState) -> Result<Runs, String> {
    let scope = scope(state)?;
    let inbox = prs(state, false)?;
    let links = links(state);
    // The inbox's repositories, the Projects', and those turned on by
    // name; written as GitHub writes them where it has.
    let mut repos: BTreeMap<String, String> = BTreeMap::new();
    let named = inbox.prs.iter().map(|p| p.repository.as_str());
    let named = named.chain(links.projects.keys().map(String::as_str));
    for r in named.chain(scope.repos.iter().filter(|r| r.on).map(|r| r.repository.as_str())) {
        repos.entry(r.to_ascii_lowercase()).or_insert_with(|| r.to_string());
    }
    let repos: Vec<String> = repos.into_values().filter(|r| in_scope(&scope, r)).collect();
    let home = home()?;
    let answers = at_once(&repos, |r| gh_json(&home, &["run", "list", "-R", r, "--limit", "50", "--json", RUN_FIELDS]));
    let mut runs = Vec::new();
    let mut problems = scope.problems;
    for (repository, answer) in repos.iter().zip(answers) {
        match answer {
            Ok(list) => runs.extend(latest_runs(&list, repository, &links)),
            Err(e) => problems.push(format!("{repository}: {e}")),
        }
    }
    runs.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    Ok(Runs { fetched_at: now(), runs, problems })
}

fn run_updates(before: &Runs, now: &Runs) -> Vec<Value> {
    let place = |r: &GithubRun| (r.repository.to_ascii_lowercase(), r.workflow.clone(), r.branch.clone());
    let was: HashMap<_, &GithubRun> = before.runs.iter().map(|r| (place(r), r)).collect();
    now.runs
        .iter()
        .filter(|r| {
            was.get(&place(r)).is_none_or(|w| {
                (w.run_id, &w.status, &w.conclusion, &w.updated_at) != (r.run_id, &r.status, &r.conclusion, &r.updated_at)
            })
        })
        .map(|r| json!({"kind": "github.run.updated", "repository": r.repository, "run": r}))
        .collect()
}

pub(crate) fn runs(state: &AppState) -> Result<Runs, String> {
    latest(state, &RUNS, false, fetch_runs, run_updates)
}

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct RunQuery {
    /// owner/name.
    pub(crate) repository: String,
    pub(crate) run_id: u64,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct JobStep {
    pub(crate) number: u64,
    pub(crate) name: String,
    pub(crate) status: String,
    pub(crate) conclusion: String,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct Job {
    pub(crate) job_id: u64,
    pub(crate) name: String,
    pub(crate) status: String,
    pub(crate) conclusion: String,
    pub(crate) started_at: String,
    pub(crate) completed_at: String,
    pub(crate) steps: Vec<JobStep>,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct RunView {
    pub(crate) run: GithubRun,
    pub(crate) jobs: Vec<Job>,
}

fn jobs_from(jobs: &Value) -> Vec<Job> {
    jobs.as_array()
        .into_iter()
        .flatten()
        .map(|j| Job {
            job_id: j["databaseId"].as_u64().unwrap_or_default(),
            name: text(&j["name"]),
            status: text(&j["status"]),
            conclusion: text(&j["conclusion"]),
            started_at: text(&j["startedAt"]),
            completed_at: text(&j["completedAt"]),
            steps: j["steps"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|s| JobStep {
                    number: s["number"].as_u64().unwrap_or_default(),
                    name: text(&s["name"]),
                    status: text(&s["status"]),
                    conclusion: text(&s["conclusion"]),
                })
                .collect(),
        })
        .collect()
}

pub(crate) fn run(state: &AppState, q: &RunQuery) -> Result<RunView, String> {
    let repository = slug(&q.repository)?;
    let fields = format!("{RUN_FIELDS},jobs");
    let run = gh_json(&home()?, &["run", "view", &q.run_id.to_string(), "-R", &repository, "--json", &fields])?;
    Ok(RunView { jobs: jobs_from(&run["jobs"]), run: run_from(&run, &repository, &links(state)) })
}

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct JobLogQuery {
    /// owner/name.
    pub(crate) repository: String,
    pub(crate) job_id: u64,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct StepLog {
    /// 0 for lines gh couldn't place in a step.
    pub(crate) number: u64,
    pub(crate) name: String,
    pub(crate) conclusion: String,
    pub(crate) log: String,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct RunLog {
    pub(crate) job_id: u64,
    pub(crate) steps: Vec<StepLog>,
    /// A step's log is longer than the 64 KiB it keeps (its end).
    pub(crate) truncated: bool,
}

/// gh prints a job's log as `job<TAB>step<TAB>line`.
fn log_from(job_id: u64, steps: &Value, log: &str) -> RunLog {
    let mut out: Vec<StepLog> = steps
        .as_array()
        .into_iter()
        .flatten()
        .map(|s| StepLog {
            number: s["number"].as_u64().unwrap_or_default(),
            name: text(&s["name"]),
            conclusion: text(&s["conclusion"]),
            log: String::new(),
        })
        .collect();
    for line in log.lines() {
        let mut parts = line.splitn(3, '\t');
        let (step, line) = (parts.nth(1).unwrap_or_default(), parts.next().unwrap_or_default());
        let at = match out.iter().position(|s| s.name == step) {
            Some(at) => at,
            None => {
                out.push(StepLog { number: 0, name: step.to_string(), conclusion: String::new(), log: String::new() });
                out.len() - 1
            }
        };
        out[at].log.push_str(line);
        out[at].log.push('\n');
    }
    let mut truncated = false;
    for step in &mut out {
        if step.log.len() > STEP_LOG {
            let cut = (step.log.len() - STEP_LOG..step.log.len()).find(|&i| step.log.is_char_boundary(i)).unwrap_or(0);
            step.log.replace_range(..cut, "");
            truncated = true;
        }
    }
    RunLog { job_id, steps: out, truncated }
}

pub(crate) fn run_log(q: &JobLogQuery) -> Result<RunLog, String> {
    let repository = slug(&q.repository)?;
    let home = home()?;
    let job = gh_json(&home, &["api", &format!("repos/{repository}/actions/jobs/{}", q.job_id)])?;
    let log = gh(&home, &["run", "view", "-R", &repository, "--job", &q.job_id.to_string(), "--log"])?;
    Ok(log_from(q.job_id, &job["steps"], &log))
}

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct RerunReq {
    /// owner/name.
    pub(crate) repository: String,
    pub(crate) run_id: u64,
    /// Only the jobs that failed, and what depends on them.
    #[serde(default)]
    pub(crate) failed_only: bool,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct Queued {
    pub(crate) queued: bool,
}

pub(crate) fn rerun(state: &AppState, req: &RerunReq) -> Result<Queued, (StatusCode, String)> {
    operator_only(OPERATOR_ONLY)?;
    let repository = slug(&req.repository).map_err(bad)?;
    let run_id = req.run_id.to_string();
    let mut args = vec!["run", "rerun", run_id.as_str(), "-R", repository.as_str()];
    if req.failed_only {
        args.push("--failed");
    }
    let out = gh(&home().map_err(bad)?, &args);
    stale(state);
    out.map_err(bad)?;
    let line = json!({"what": "re-run", "repository": repository, "run_id": req.run_id, "failed_only": req.failed_only});
    record(state, &repository, None, line);
    Ok(Queued { queued: true })
}

// ---- Routes ----

type Reply<T> = Result<Json<T>, (StatusCode, String)>;

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T, (StatusCode, String)> + Send + 'static) -> Reply<T> {
    crate::actor::spawn_blocking(f).await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?.map(Json)
}

pub(crate) async fn scope_route(State(st): State<AppState>) -> Reply<GithubScope> {
    blocking(move || scope(&st).map_err(bad)).await
}

pub(crate) async fn set_scope_route(State(st): State<AppState>, Json(req): Json<ScopeReq>) -> Reply<GithubScope> {
    blocking(move || set_scope(&st, &req)).await
}

pub(crate) async fn prs_route(State(st): State<AppState>) -> Reply<Prs> {
    blocking(move || prs(&st, false).map_err(bad)).await
}

pub(crate) async fn refresh_route(State(st): State<AppState>) -> Reply<Prs> {
    blocking(move || prs(&st, true).map_err(bad)).await
}

pub(crate) async fn pr_route(State(st): State<AppState>, Query(q): Query<PrQuery>) -> Reply<PrView> {
    blocking(move || pr(&st, &q).map_err(bad)).await
}

pub(crate) async fn review_route(State(st): State<AppState>, Json(req): Json<ReviewReq>) -> Reply<Reviewed> {
    blocking(move || review(&st, &req)).await
}

pub(crate) async fn comment_route(State(st): State<AppState>, Json(req): Json<CommentReq>) -> Reply<Commented> {
    blocking(move || comment(&st, &req)).await
}

pub(crate) async fn merge_route(State(st): State<AppState>, Json(req): Json<PrMergeReq>) -> Reply<Merged> {
    blocking(move || merge(&st, &req)).await
}

pub(crate) async fn ready_route(State(st): State<AppState>, Json(req): Json<ReadyReq>) -> Reply<Ready> {
    blocking(move || ready(&st, &req)).await
}

pub(crate) async fn resolve_route(State(st): State<AppState>, Json(req): Json<ResolveReq>) -> Reply<Resolved> {
    blocking(move || resolve(&st, &req)).await
}

pub(crate) async fn runs_route(State(st): State<AppState>) -> Reply<Runs> {
    blocking(move || runs(&st).map_err(bad)).await
}

pub(crate) async fn run_route(State(st): State<AppState>, Query(q): Query<RunQuery>) -> Reply<RunView> {
    blocking(move || run(&st, &q).map_err(bad)).await
}

pub(crate) async fn run_log_route(Query(q): Query<JobLogQuery>) -> Reply<RunLog> {
    blocking(move || run_log(&q).map_err(bad)).await
}

pub(crate) async fn rerun_route(State(st): State<AppState>, Json(req): Json<RerunReq>) -> Reply<Queued> {
    blocking(move || rerun(&st, &req)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::project::{open_stream, OpenReq};
    use crate::testutil::*;
    use bakery_agent::StubModel;
    use std::sync::Arc;

    fn scope_of(owners: Vec<Owner>, repos: Vec<RepoOn>) -> GithubScope {
        GithubScope { me: "me".into(), owners, repos, problems: vec![] }
    }

    fn check(name: &str, conclusion: &str, started: &str, job: u64) -> Value {
        let status = if conclusion.is_empty() { "IN_PROGRESS" } else { "COMPLETED" };
        json!({
            "__typename": "CheckRun", "name": name, "status": status, "conclusion": conclusion, "startedAt": started,
            "detailsUrl": format!("https://github.com/Acme/Widget/actions/runs/20000000001/job/{job}"),
            "checkSuite": {"workflowRun": {"workflow": {"name": "CI"}}}
        })
    }

    /// A pull request as the inbox's search answers it.
    fn node() -> Value {
        json!({
            "number": 7, "title": "Widget", "url": "https://github.com/Acme/Widget/pull/7", "isDraft": false,
            "createdAt": "2026-10-01T00:00:00Z", "updatedAt": "2026-10-09T00:00:00Z",
            "additions": 10, "deletions": 2, "changedFiles": 3,
            "headRefName": "w1/feature", "baseRefName": "main", "headRefOid": "abc",
            "mergeable": "MERGEABLE", "mergeStateStatus": "BLOCKED", "reviewDecision": null,
            "author": {"login": "dependabot", "__typename": "Bot"},
            "repository": {"nameWithOwner": "Acme/Widget", "deleteBranchOnMerge": true},
            "commentCount": {"totalCount": 4},
            "lastCommit": {"nodes": [{"commit": {"statusCheckRollup": {"contexts": {"nodes": [
                check("test", "FAILURE", "2026-10-09T01:00:00Z", 50000000001),
                check("test", "SUCCESS", "2026-10-09T02:00:00Z", 50000000002),
                check("lint", "", "2026-10-09T02:00:00Z", 50000000003),
                check("docs", "SKIPPED", "2026-10-09T02:00:00Z", 50000000004),
                {"__typename": "StatusContext", "context": "deploy/preview", "state": "ERROR",
                 "targetUrl": "https://example.com/x", "createdAt": "2026-10-09T02:00:00Z"}
            ]}}}}]}
        })
    }

    #[test]
    fn the_inbox_is_you_and_your_organizations_with_your_settings_laid_over() {
        let saved = Saved {
            owners: vec![
                Owner { login: "ACME".into(), kind: "org".into(), on: false },
                Owner { login: "friend".into(), kind: "user".into(), on: true },
            ],
            repos: vec![RepoOn { repository: "acme/widget".into(), on: true }, RepoOn { repository: "me/old".into(), on: false }],
        };
        let listed = owners("me", &["acme".into(), "tools".into()], &saved);
        let shown: Vec<_> = listed.iter().map(|o| (o.login.as_str(), o.kind.as_str(), o.on)).collect();
        assert_eq!(shown, [("me", "user", true), ("acme", "org", false), ("tools", "org", true), ("friend", "user", true)]);

        let scope = scope_of(listed, saved.repos);
        assert!(in_scope(&scope, "acme/widget"), "a repository turned on, under an owner turned off");
        assert!(!in_scope(&scope, "acme/other"));
        assert!(!in_scope(&scope, "me/old"), "a repository turned off, under an owner turned on");
        assert!(in_scope(&scope, "Me/New"));
        let asked: Vec<_> = searches(&scope).into_iter().map(|(_, q)| q).collect();
        assert_eq!(
            asked,
            [
                "is:pr is:open archived:false user:me",
                "is:pr is:open archived:false org:tools",
                "is:pr is:open archived:false user:friend",
                "is:pr is:open archived:false repo:acme/widget",
            ]
        );
        for bad in ["", "-x", "a b", "a/b", "a_b", &"x".repeat(40)] {
            assert!(login(bad).is_err(), "{bad:?}");
        }
        assert_eq!(login(" Octo-Cat ").unwrap(), "Octo-Cat");
    }

    #[test]
    fn a_pull_request_reads_as_the_inbox_lists_it() {
        let asked = HashSet::from([key_of("Acme/Widget", 7)]);
        let pr = pr_from(&node(), &asked);
        assert!(pr.requested_from_me && pr.author_is_bot);
        assert_eq!(pr.checks, CheckCounts { pass: 1, fail: 1, pending: 1, skipped: 1 }, "a re-run's pass replaces its failure");
        assert_eq!(
            (pr.review_decision.as_deref(), pr.mergeable.as_str(), pr.merge_state.as_str(), pr.comments),
            (None, "MERGEABLE", "BLOCKED", 4)
        );
        assert_eq!((pr.branch.as_str(), pr.base.as_str(), pr.changed_files), ("w1/feature", "main", 3));
        assert!(pr.delete_branch_on_merge, "the repository's own setting");

        let checks = checks_of(&node());
        let test = checks.iter().find(|c| c.name == "test").unwrap();
        assert_eq!(
            (test.bucket.as_str(), test.run_id, test.job_id, test.workflow.as_str()),
            ("pass", Some(20000000001), Some(50000000002), "CI"),
            "ids past GraphQL's numbers, from the check's address"
        );
        let status = checks.iter().find(|c| c.name == "deploy/preview").unwrap();
        assert_eq!((status.bucket.as_str(), status.run_id, status.job_id), ("fail", None, None));
        assert_eq!(checks.iter().find(|c| c.name == "lint").unwrap().state, "IN_PROGRESS");
    }

    #[test]
    fn each_answer_sends_what_changed_since_the_one_before() {
        let list = |prs: Vec<GithubPr>, heads: &[(u64, &str)], problems: Vec<String>| PrList {
            heads: heads.iter().map(|(n, h)| (key_of("acme/widget", *n), h.to_string())).collect(),
            prs: Prs { me: "me".into(), fetched_at: String::new(), prs, problems },
        };
        let one = pr_from(&node(), &HashSet::new());
        let numbered = |n: u64| GithubPr { number: n, ..one.clone() };
        let before = list(vec![one.clone(), numbered(8)], &[(7, "abc"), (8, "def")], vec![]);
        let mut changed = one.clone();
        changed.checks.pass += 1;
        changed.comments += 1;
        changed.draft = true;
        let now = list(vec![changed, numbered(9)], &[(7, "pushed"), (9, "ghi")], vec![]);

        let updates = pr_updates(&before, &now);
        let of = |n: u64| updates.iter().find(|u| u["number"] == n).cloned().unwrap();
        assert!(updates.iter().all(|u| u["kind"] == "github.pr.updated" && u["repository"] == "Acme/Widget"));
        assert_eq!(of(7)["changed"], json!(["checks", "comments", "commits", "state"]));
        assert_eq!((of(9)["changed"].clone(), of(9)["pr"]["number"].clone()), (json!(["state"]), json!(9)), "a new one");
        assert!(of(8)["pr"].is_null(), "gone: closed, merged or out of the scope");
        assert!(pr_updates(&now, &now).is_empty());
        // An answer with a problem may only not have seen them.
        assert!(pr_updates(&before, &list(vec![], &[], vec!["acme: SSO".into()])).is_empty());
    }

    #[test]
    fn runs_keep_each_workflows_latest_on_each_branch_and_send_their_changes() {
        let run = |id: u64, workflow: &str, status: &str, conclusion: &str| {
            json!({"databaseId": id, "workflowName": workflow, "headBranch": "main", "displayTitle": "t", "status": status,
                   "conclusion": conclusion, "event": "push", "createdAt": "c", "updatedAt": "u", "url": "x"})
        };
        let listed = json!([run(30000000003, "CI", "in_progress", ""), run(30000000002, "CI", "completed", "failure"), run(30000000001, "Docs", "completed", "success")]);
        let runs = latest_runs(&listed, "Acme/Widget", &Links::default());
        let shown: Vec<_> = runs.iter().map(|r| (r.workflow.as_str(), r.run_id)).collect();
        assert_eq!(shown, [("CI", 30000000003), ("Docs", 30000000001)]);

        let before = Runs { fetched_at: String::new(), runs: runs.clone(), problems: vec![] };
        let mut done = runs;
        done[0].status = "completed".into();
        done[0].conclusion = "success".into();
        let updates = run_updates(&before, &Runs { runs: done, ..before.clone() });
        assert_eq!(updates.len(), 1, "{updates:?}");
        assert_eq!((updates[0]["kind"].as_str(), updates[0]["run"]["conclusion"].as_str()), (Some("github.run.updated"), Some("success")));
        assert!(run_updates(&before, &before).is_empty());
    }

    /// A read answers at once with the last answer, from memory or, after a
    /// restart, from disk, and asks GitHub again behind it once it is a
    /// minute old or older than the operator's last action; what changed
    /// goes out live. A failed ask leaves the answer before standing, saying
    /// so. The first answer ever, and the operator's refresh, wait.
    #[test]
    fn a_read_answers_at_once_and_asks_github_again_behind_it() {
        use std::os::unix::fs::PermissionsExt;
        use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
        static LAST: Last<Runs> = Last::new("runs.json", || None);
        static ASKS: AtomicUsize = AtomicUsize::new(0);
        // CI on main, in progress at the first ask, done after; the fifth fails.
        fn fetch(_: &AppState) -> Result<Runs, String> {
            let n = ASKS.fetch_add(1, SeqCst) + 1;
            if n == 5 {
                return Err("offline".into());
            }
            let status = if n == 1 { "in_progress" } else { "completed" };
            let run = json!({"databaseId": 30000000001u64, "workflowName": "CI", "headBranch": "main", "displayTitle": "t", "status": status,
                             "conclusion": "", "event": "push", "createdAt": "c", "updatedAt": format!("u{n}"), "url": "x"});
            Ok(Runs { fetched_at: format!("ask {n}"), runs: latest_runs(&json!([run]), "Acme/Widget", &Links::default()), problems: vec![] })
        }
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let mut events = state.tx.subscribe();
        let read = |refresh| latest(&state, &LAST, refresh, fetch, run_updates).unwrap();
        let acted = || LAST.entry(&mut LAST.lock(), &state).acted = Some(Instant::now());
        let asked = |n: usize| {
            let deadline = Instant::now() + Duration::from_secs(30);
            while ASKS.load(SeqCst) < n || LAST.lock()[state.base.as_path()].asking {
                assert!(Instant::now() < deadline, "never asked again");
                std::thread::sleep(Duration::from_millis(10));
            }
        };

        assert_eq!(read(false).fetched_at, "ask 1", "the first answer ever waits");
        assert_eq!(read(false).fetched_at, "ask 1", "fresh");
        acted();
        assert_eq!(read(false).fetched_at, "ask 1", "the operator acted: this answer at once, the next behind it");
        asked(2);
        let sent = std::iter::from_fn(|| events.try_recv().ok()).map(|e| serde_json::from_str::<Value>(&e).unwrap()).find(|e| e["kind"] == "github.run.updated");
        assert_eq!(sent.unwrap()["run"]["status"], "completed");
        assert_eq!(read(false).fetched_at, "ask 2");

        // The node starts again: its last answer from disk, the next behind it.
        LAST.lock().remove(state.base.as_path());
        assert_eq!(read(false).fetched_at, "ask 2");
        asked(3);
        assert_eq!(read(false).fetched_at, "ask 3");
        let file = std::fs::metadata(cache_dir(&state).join("runs.json")).unwrap();
        assert_eq!(file.permissions().mode() & 0o777, 0o600);

        assert_eq!(read(true).fetched_at, "ask 4", "the operator's refresh waits");
        acted();
        read(false);
        asked(5);
        let failed = read(false);
        assert_eq!(failed.fetched_at, "ask 4");
        assert!(failed.problems[0].contains("offline"), "{:?}", failed.problems);
    }

    /// GitHub's word on its limits holds the node back: a limit spent until
    /// its reset; a secondary one its Retry-After, else a minute, doubling,
    /// up to fifteen; a 403 with limits left (a sign-in wall) not at all.
    #[test]
    fn a_rate_limit_waits_until_github_says() {
        fn spent() -> Option<Value> {
            let reset = chrono::Utc::now().timestamp() + 120;
            Some(json!({"resources": {"core": {"remaining": 10, "reset": reset}, "graphql": {"remaining": 0, "reset": reset}}}))
        }
        fn left() -> Option<Value> {
            Some(json!({"resources": {"core": {"remaining": 4000, "reset": 0}, "graphql": {"remaining": 4000, "reset": 0}}}))
        }
        let said = |s: &str| vec![s.to_string()];
        let wait = limit_wait(&said("gh: API rate limit exceeded for user ID 1. (HTTP 403)"), spent, None).unwrap();
        assert!((120..=126).contains(&wait.as_secs()), "{wait:?}");
        let secondary = said("You have exceeded a secondary rate limit (HTTP 403)");
        assert_eq!(limit_wait(&secondary, left, None), Some(FIRST_WAIT));
        assert_eq!(limit_wait(&secondary, left, Some(FIRST_WAIT)), Some(FIRST_WAIT * 2), "doubling while it repeats");
        assert_eq!(limit_wait(&secondary, left, Some(LONGEST_WAIT)), Some(LONGEST_WAIT));
        assert_eq!(limit_wait(&said("too many requests (HTTP 429) Retry-After: 30"), left, None), Some(Duration::from_secs(30)));
        assert_eq!(limit_wait(&said("Resource protected by organization SAML enforcement (HTTP 403)"), left, None), None);
        assert_eq!(limit_wait(&said("could not resolve host"), spent, None), None);
    }

    /// The node asks again on its own once its last ask is old enough,
    /// never while GitHub's rate limit holds it back; the inbox then says
    /// so, live and in what a read answers. A window says whether it shows.
    #[test]
    fn the_inbox_asks_again_on_its_own_and_waits_out_a_rate_limit() {
        use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
        static LAST: Last<Runs> = Last::new("runs.json", spent);
        static ASKS: AtomicUsize = AtomicUsize::new(0);
        fn spent() -> Option<Value> {
            Some(json!({"resources": {"graphql": {"remaining": 0, "reset": chrono::Utc::now().timestamp() + 600}}}))
        }
        fn fetch(_: &AppState) -> Result<Runs, String> {
            match ASKS.fetch_add(1, SeqCst) {
                0 => Ok(Runs { fetched_at: "first".into(), runs: vec![], problems: vec![] }),
                _ => Err("gh: API rate limit exceeded for user ID 1. (HTTP 403)".into()),
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let mut events = state.tx.subscribe();
        let renew_now = || renew(&state, &LAST, Duration::ZERO, fetch, run_updates);

        renew_now();
        renew_now();
        renew_now();
        assert_eq!(ASKS.load(SeqCst), 2, "not again while the rate limit holds");
        let told: Vec<Value> = std::iter::from_fn(|| events.try_recv().ok())
            .filter_map(|e| serde_json::from_str::<Value>(&e).ok())
            .filter(|e| e["kind"] == "github.inbox.updated")
            .collect();
        assert_eq!(told.len(), 2, "{told:?}");
        assert!(told[0]["waiting_until"].is_null() && told[1]["waiting_until"].is_string() && told[1]["list"] == "runs");
        assert!(told[1]["problems"][0].as_str().unwrap().contains("slow down"), "{:?}", told[1]);
        let read = latest(&state, &LAST, false, fetch, run_updates).unwrap();
        assert_eq!(read.fetched_at, "first");
        assert!(read.problems[0].contains("rate limit"), "{:?}", read.problems);
        assert_eq!(ASKS.load(SeqCst), 2, "a read asks nothing behind it while the node waits");

        LAST.entry(&mut LAST.lock(), &state).wait_until = None;
        renew(&state, &LAST, Duration::from_secs(3600), fetch, run_updates);
        assert_eq!(ASKS.load(SeqCst), 2, "not before its time");

        let base = dir.path();
        assert!(!crate::routes::operator::app_visible(&state));
        crate::routes::operator::seen_for_test(base, true);
        assert!(crate::routes::operator::app_visible(&state));
        crate::routes::operator::seen_for_test(base, false);
        assert!(!crate::routes::operator::app_visible(&state));
    }

    /// A pull request read in full is kept while the list's copy of it stays
    /// the same, and never when it shows a credential-shaped file; a whole
    /// list without it, or the operator's action, drops it.
    #[test]
    fn a_pull_requests_view_is_kept_while_the_list_has_it_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let mut p = node();
        p["body"] = "Fixes it".into();
        let view = view_from(&p, vec![], "diff --git a/x.rs b/x.rs\n".into(), false);
        let listed = view.pr.clone();
        keep_view(&state, &view);
        assert_eq!(kept_view(&state, Some(&listed)).map(|v| v.body).as_deref(), Some("Fixes it"));
        assert!(kept_view(&state, None).is_none(), "not listed: asked each time");
        let changed = GithubPr { updated_at: "2026-10-10T00:00:00Z".into(), ..listed.clone() };
        assert!(kept_view(&state, Some(&changed)).is_none(), "the list's copy changed");

        drop_views_unlisted(&state, std::slice::from_ref(&listed));
        assert!(kept_view(&state, Some(&listed)).is_some(), "still listed");
        drop_views_unlisted(&state, &[]);
        assert!(kept_view(&state, Some(&listed)).is_none(), "gone from the list");

        keep_view(&state, &view);
        stale(&state);
        assert!(kept_view(&state, Some(&listed)).is_none(), "the operator acted");

        keep_view(&state, &view);
        let secret = view_from(&p, vec![], "diff --git a/.env b/.env\n--- a/.env\n+++ b/.env\n@@ -1 +1 @@\n-A=1\n+A=2\n".into(), false);
        keep_view(&state, &secret);
        assert!(secret.secret && !view_file(&state, "Acme/Widget", 7).exists(), "never kept with a credential-shaped file");
    }

    #[test]
    fn one_pull_request_reads_in_full() {
        let files = json!([
            [{"filename": "src/a.rs", "status": "modified", "additions": 1, "deletions": 1},
             {"filename": "b.rs", "previous_filename": "old_b.rs", "status": "renamed", "additions": 0, "deletions": 0}],
            [{"filename": "c.rs", "status": "copied", "additions": 3, "deletions": 0},
             {"filename": "d.sh", "status": "changed", "additions": 0, "deletions": 0}]
        ]);
        let mut p = node();
        p["body"] = "Fixes it".into();
        p["reviews"] = json!({"nodes": [{"url": "https://github.com/Acme/Widget/pull/7#pullrequestreview-3123456789",
            "author": {"login": "al"}, "state": "APPROVED", "body": "ok", "submittedAt": "t"}]});
        p["reviewThreads"] = json!({"nodes": [{"id": "PRRT_kwDOabc", "path": "src/a.rs", "line": 12, "startLine": null,
            "diffSide": "RIGHT", "isResolved": false, "isOutdated": true, "comments": {"nodes": [{"author": {"login": "al"},
            "body": "why?", "createdAt": "t", "url": "https://github.com/Acme/Widget/pull/7#discussion_r2412345678"}]}}]});
        p["conversation"] = json!({"nodes": [{"author": null, "body": "hi", "createdAt": "t",
            "url": "https://github.com/Acme/Widget/pull/7#issuecomment-3312345678"}]});
        p["history"] = json!({"nodes": [{"commit": {"oid": "abc", "messageHeadline": "Add a", "committedDate": "t",
            "author": {"name": "Jo", "user": null}}}]});
        let diff = "diff --git a/.env b/.env\n--- a/.env\n+++ b/.env\n@@ -1 +1 @@\n-A=1\n+A=2\n";

        let view = view_from(&p, files_from(&files), diff.to_string(), false);
        let statuses: Vec<_> = view.files.iter().map(|f| f.status.as_str()).collect();
        assert_eq!(statuses, ["modified", "renamed", "added", "modified"]);
        assert_eq!(view.files[1].previous_path.as_deref(), Some("old_b.rs"));
        assert!(view.secret && !view.diff_truncated, "a credential-shaped file's lines are shown");
        assert_eq!((view.reviews[0].id, view.threads[0].comments[0].id, view.comments[0].id), (3123456789, 2412345678, 3312345678));
        let thread = &view.threads[0];
        assert_eq!((thread.line, thread.start_line, thread.side.as_str(), thread.resolved, thread.outdated), (Some(12), None, "RIGHT", false, true));
        assert_eq!((view.commits[0].author.as_str(), view.comments[0].author.as_str(), view.body.as_str()), ("Jo", "", "Fixes it"));
        assert_eq!(view.checks.len(), 4);
        assert!(!view_from(&p, vec![], "diff --git a/x.rs b/x.rs\n--- a/x.rs\n+++ b/x.rs\n".into(), false).secret);
        let refused = view_from(&p, vec![], String::new(), true);
        assert!(refused.diff_truncated && refused.diff.is_empty(), "GitHub refused a diff this big");
    }

    #[test]
    fn a_jobs_log_splits_by_step_and_keeps_each_steps_end() {
        let steps = json!([{"number": 1, "name": "Set up job", "conclusion": "success"}, {"number": 2, "name": "Run tests", "conclusion": "failure"}]);
        let long = "x".repeat(STEP_LOG);
        let log = format!("build\tSet up job\tready\nbuild\tRun tests\tone\tcolumn\nbuild\tRun tests\t{long}\nbuild\tUNKNOWN STEP\tstray\n");
        let got = log_from(9, &steps, &log);
        let shown: Vec<_> = got.steps.iter().map(|s| (s.number, s.name.as_str())).collect();
        assert_eq!(shown, [(1, "Set up job"), (2, "Run tests"), (0, "UNKNOWN STEP")]);
        assert_eq!((got.steps[0].log.as_str(), got.steps[0].conclusion.as_str(), got.steps[2].log.as_str()), ("ready\n", "success", "stray\n"));
        let tests = &got.steps[1].log;
        assert!(got.truncated && tests.len() == STEP_LOG && tests.ends_with("x\n") && !tests.contains("column"), "{}", tests.len());
    }

    /// The operator's choice about a merged branch holds whatever its
    /// repository does: deleted when asked, else kept, restored once the
    /// repository deleted it on its own; the answer is what is true.
    #[test]
    fn a_merged_branch_ends_as_the_operator_chose() {
        use std::cell::Cell;
        let run = |keep: bool, auto_deletes: bool, delete_works: bool| {
            let (there, calls) = (Cell::new(true), Cell::new(0));
            let gone = settle_branch(
                keep,
                auto_deletes,
                &|| {
                    // The repository deletes it a moment after the merge.
                    calls.set(calls.get() + 1);
                    if auto_deletes && calls.get() == 2 {
                        there.set(false);
                    }
                    there.get()
                },
                &|| {
                    if delete_works {
                        there.set(false);
                    }
                    delete_works
                },
                &|| {
                    there.set(true);
                    true
                },
                &|| {},
            );
            (gone, there.get())
        };
        assert_eq!(run(false, false, true), (true, false), "deleted when asked");
        assert_eq!(run(false, false, false), (false, true), "and says so when it couldn't be");
        assert_eq!(run(true, false, true), (false, true), "kept: nothing to do");
        assert_eq!(run(true, true, true), (false, true), "kept: restored after the repository deleted it");
    }

    #[test]
    fn addresses_give_their_ids() {
        let printed = "Creating pull request for w1/x into main\n\nhttps://github.com/Acme/Widget/pull/12\n";
        assert_eq!(pr_address(printed), Some(("Acme/Widget".into(), 12, "https://github.com/Acme/Widget/pull/12".into())));
        assert_eq!(pr_address("https://gitlab.com/a/b/pull/1"), None);
        assert_eq!(run_and_job("https://github.com/a/b/actions/runs/123/job/456?pr=7"), (Some(123), Some(456)));
        assert_eq!(run_and_job("https://github.com/a/b/actions/runs/123"), (Some(123), None));
        assert_eq!(run_and_job("https://ci.example.com/build/1"), (None, None));
        assert_eq!(id_in("https://github.com/a/b/pull/1#discussion_r2412345678"), 2412345678);
        assert_eq!(id_in("https://github.com/a/b/pull/1"), 0);
        assert_eq!(escaped("w1/fix#2 %"), "w1/fix%232%20%25");
    }

    #[test]
    fn a_pull_request_belongs_to_the_thread_that_opened_it_else_a_worktree_on_its_branch() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let repo = make_repo(&dir.path().join("widget"));
        run_git(&repo, &["remote", "add", "origin", "git@github.com:Acme/Widget.git"]);
        let (project, _) = crate::catalog::create_project(&state, "widget", None).unwrap();
        let worktree = open_stream(&state, &open_req(&repo, "feature")).unwrap().context;
        crate::catalog::link_socket_to_project(&state, &worktree.stream_id, &project.project_id, &worktree.worktree_path, true).unwrap();
        let in_place = OpenReq { worktree_path: Some(repo.to_string_lossy().into_owned()), ..open_req(&repo, "here") };
        let in_place = open_stream(&state, &in_place).unwrap().context;
        assert!(in_place.in_place && !worktree.in_place);
        let opened = json!({"repository": "Acme/Widget", "number": 12, "url": "u", "branch": "gone"});
        crate::state::append(&state, &in_place.stream_id, "human", PR_OPENED, opened.to_string(), vec![]).unwrap();

        let links = links(&state);
        assert_eq!(links.thread("acme/widget", Some(12), "elsewhere"), Some(in_place.stream_id.clone()), "the thread that opened it");
        assert_eq!(links.thread("Acme/Widget", Some(13), &worktree.branch), Some(worktree.stream_id.clone()), "a live worktree on its branch");
        assert_eq!(links.thread("Acme/Widget", Some(13), &in_place.branch), None, "never a checkout opened in place");
        assert_eq!(links.thread("Acme/Other", None, &worktree.branch), None);
        assert_eq!(links.project("ACME/widget"), Some(project.project_id));
    }

    fn forbidden<T>(answer: Result<T, (StatusCode, String)>) -> bool {
        matches!(answer, Err((StatusCode::FORBIDDEN, _)))
    }

    #[test]
    fn only_the_operator_changes_github_or_the_inbox_scope() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let (repository, number) = ("acme/widget".to_string(), 7);
        crate::actor::with("agent:a1".to_string(), || {
            assert!(forbidden(set_scope(&state, &ScopeReq { owners: vec![], repos: vec![] })));
            let review_req = ReviewReq { repository: repository.clone(), number, event: "APPROVE".into(), body: String::new(), comments: vec![] };
            assert!(forbidden(review(&state, &review_req)));
            let comment_req = CommentReq { repository: repository.clone(), number, body: "hi".into(), in_reply_to: None };
            assert!(forbidden(comment(&state, &comment_req)));
            let merge_req = PrMergeReq { repository: repository.clone(), number, method: "squash".into(), delete_branch: true };
            assert!(forbidden(merge(&state, &merge_req)));
            assert!(forbidden(ready(&state, &ReadyReq { repository: repository.clone(), number })));
            let resolve_req = ResolveReq { repository: repository.clone(), thread_id: "PRRT_x".into(), resolved: true };
            assert!(forbidden(resolve(&state, &resolve_req)));
            assert!(forbidden(rerun(&state, &RerunReq { repository: repository.clone(), run_id: 1, failed_only: true })));
        });
        // The operator's own call is checked before anything reaches GitHub.
        let review_req = ReviewReq { repository, number, event: "MERGE".into(), body: String::new(), comments: vec![] };
        assert!(review(&state, &review_req).unwrap_err().1.contains("COMMENT, APPROVE or REQUEST_CHANGES"));

        use crate::api_catalog::Risk;
        for tool in ["github_scope", "github_prs", "github_prs_refresh", "github_pr", "github_runs", "github_run", "github_run_log"] {
            assert_eq!(crate::api_tools::risk_of(tool), Some(Risk::Read), "{tool}");
        }
    }
}
