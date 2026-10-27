//! An agent's own `git push` and `gh` inside its room, caught and held for
//! the operator. The room's git sends every push to a Bakery helper
//! (`git-remote-bakery`), and its `gh` is a Bakery stand-in; both ask the
//! node at their thread's own door (`doors`), which only that thread's rooms
//! can reach, so the door itself says who is asking: no token. The node
//! answers each with the same card, or grant, the agent's own tool call
//! gets (`git_push`, `git_pr`, `github_issue_create`,
//! `github_issue_comment`). Any program in the room may ask, and the
//! operator still sees a card for each request unless they granted that
//! exact branch.

use crate::api_tools::{Caller, WAITING};
use crate::node::Seat;
use crate::state::AppState;
use axum::{extract::State, http::StatusCode, Json};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The name git runs the push helper by (`bakery::<url>`).
pub(crate) const HELPER: &str = "git-remote-bakery";

/// git's settings in a room: a push to any remote goes to the helper.
pub(crate) fn git_config() -> Vec<(String, String)> {
    ["https://", "http://", "ssh://", "git@", "file://"]
        .into_iter()
        .map(|prefix| (format!("url.bakery::{prefix}.pushInsteadOf"), prefix.to_string()))
        .collect()
}

/// The helper and the stand-in: links named `git-remote-bakery` and `gh` to
/// this node's staged binary, in a folder that goes first on the room's
/// PATH. Returns that folder and the staged binary's.
pub(crate) fn shims(binary: &Path, cache_root: &Path) -> Result<(PathBuf, PathBuf), String> {
    // Staged once per binary and cache while the node runs: staging reads
    // and hashes the whole binary, and every runtime start (and every agent
    // the agents list shows) asks for it.
    type Staged = Vec<((PathBuf, PathBuf), (PathBuf, PathBuf))>;
    static STAGED: std::sync::Mutex<Staged> = std::sync::Mutex::new(Vec::new());
    let key = (binary.to_path_buf(), cache_root.to_path_buf());
    let known = STAGED.lock().unwrap().iter().find(|(k, _)| *k == key).map(|(_, found)| found.clone());
    if let Some((bin, staged_dir)) = known.filter(|(bin, dir)| bin.is_dir() && dir.join("exe").is_file()) {
        return Ok((bin, staged_dir));
    }
    let staged = bakery_exec::stage_acp_binary(binary, cache_root)?;
    let staged_dir = staged.parent().ok_or("the staged binary has no folder")?.to_path_buf();
    let bin = staged_dir.join("bin");
    std::fs::create_dir_all(&bin).map_err(|e| format!("{}: {e}", bin.display()))?;
    for name in [HELPER, "gh"] {
        let link = bin.join(name);
        if link.symlink_metadata().is_err() {
            std::os::unix::fs::symlink("../exe", &link).map_err(|e| format!("{}: {e}", link.display()))?;
        }
    }
    let mut known = STAGED.lock().unwrap();
    known.retain(|(k, _)| *k != key);
    known.push((key, (bin.clone(), staged_dir.clone())));
    Ok((bin, staged_dir))
}

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct PushRef {
    /// What git sends: a branch, HEAD or a commit, in the room's repository.
    pub(crate) src: String,
    /// Where it goes on the remote: refs/heads/<branch>.
    pub(crate) dst: String,
    #[serde(default)]
    pub(crate) force: bool,
}

#[derive(schemars::JsonSchema, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum Ask {
    /// `git push`, through the helper.
    Push {
        /// The room repository's top folder: it must be the conversation's own.
        repo: String,
        refs: Vec<PushRef>,
    },
    /// `gh`, through the stand-in; a body file already read into --body.
    Gh {
        repo: String,
        args: Vec<String>,
    },
}

#[derive(schemars::JsonSchema, Debug, Serialize, Default)]
pub(crate) struct RefAnswer {
    pub(crate) dst: String,
    pub(crate) ok: bool,
    pub(crate) message: String,
}

#[derive(schemars::JsonSchema, Debug, Serialize, Default)]
pub(crate) struct Answer {
    pub(crate) ok: bool,
    pub(crate) message: String,
    /// What a `gh` request made, where GitHub says.
    pub(crate) url: Option<String>,
    /// Each pushed ref's answer, for git.
    pub(crate) refs: Vec<RefAnswer>,
}

const HELD: &str = "waiting for the operator to approve this in the Bakery; run it again once they have";

fn seat_of(state: &AppState, stream_id: &str) -> Seat {
    if crate::baker_ops::is_baker_scope_stream(state, stream_id) {
        Seat::Baker
    } else {
        Seat::Orchestrator
    }
}

/// The room's repository is the conversation's own checkout, or nothing is
/// done from it.
fn own_checkout(state: &AppState, stream_id: &str, repo: &str) -> Result<PathBuf, String> {
    let dir = crate::routes::git::checkout(state, stream_id)?;
    let same = |a: &Path, b: &Path| a.canonicalize().ok().zip(b.canonicalize().ok()).is_some_and(|(a, b)| a == b);
    if same(&dir, Path::new(repo)) {
        Ok(dir)
    } else {
        Err("this goes out from the conversation's own checkout only".to_string())
    }
}

/// A Bakery tool, called as the conversation's agent: the card and grants
/// apply as to its own call.
pub(crate) fn as_agent(state: &AppState, stream_id: &str, tool: &str, args: &Value) -> Result<String, String> {
    let seat = seat_of(state, stream_id);
    let mut caller = Caller::mcp(state, stream_id, seat);
    // Tests don't wait a minute for an operator who never comes.
    if cfg!(test) {
        caller.wait = Some(std::time::Duration::from_millis(300));
    }
    crate::actor::with(crate::actor::agent(stream_id, seat), || crate::api_tools::call(state, &caller, tool, args))
        .map_err(|e| if e == WAITING { HELD.to_string() } else { e })
}

fn push(state: &AppState, stream_id: &str, repo: &str, refs: &[PushRef]) -> Answer {
    let dir = match own_checkout(state, stream_id, repo) {
        Ok(dir) => dir,
        Err(e) => return Answer { message: e, ..Answer::default() },
    };
    let git = |args: &[&str]| crate::git::run(&dir, args).map(|out| out.trim().to_string());
    let one = |r: &PushRef| -> Result<String, String> {
        let branch = r.dst.strip_prefix("refs/heads/").ok_or_else(|| format!("only branches go out from a room, not {}", r.dst))?;
        if r.src.is_empty() {
            return Err("deleting a branch is the operator's".to_string());
        }
        if r.force {
            return Err("a force push is the operator's".to_string());
        }
        let sent = git(&["rev-parse", "--verify", &format!("{}^{{commit}}", r.src)]).map_err(|_| format!("{} is no commit here", r.src))?;
        let tip = git(&["rev-parse", "--verify", &format!("refs/heads/{branch}^{{commit}}")]).ok();
        if tip.as_deref() != Some(sent.as_str()) {
            return Err(format!("a room pushes a branch as it is: {} isn't {branch}'s newest commit here", r.src));
        }
        as_agent(state, stream_id, "git_push", &json!({ "stream_id": stream_id, "branch": branch }))
    };
    let refs: Vec<RefAnswer> = refs
        .iter()
        .map(|r| match one(r) {
            Ok(out) => RefAnswer { dst: r.dst.clone(), ok: true, message: out },
            Err(e) => RefAnswer { dst: r.dst.clone(), ok: false, message: e },
        })
        .collect();
    Answer { ok: refs.iter().all(|r| r.ok), refs, ..Answer::default() }
}

/// A `gh` call's flags and words.
struct Gh<'a> {
    words: Vec<&'a str>,
    flags: HashMap<&'a str, Vec<&'a str>>,
}

fn parse(args: &[String]) -> Result<Gh<'_>, String> {
    const VALUED: &[(&str, &str)] = &[
        ("-t", "title"), ("--title", "title"), ("-b", "body"), ("--body", "body"), ("-B", "base"), ("--base", "base"),
        ("-l", "label"), ("--label", "label"), ("-R", "repo"), ("--repo", "repo"), ("-H", "head"), ("--head", "head"),
    ];
    let mut gh = Gh { words: Vec::new(), flags: HashMap::new() };
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let (name, inline) = match arg.split_once('=') {
            Some((name, value)) if name.starts_with("--") => (name, Some(value)),
            _ => (arg.as_str(), None),
        };
        if let Some((_, key)) = VALUED.iter().find(|(flag, _)| *flag == name) {
            let value = match inline {
                Some(value) => value,
                None => it.next().map(String::as_str).ok_or_else(|| format!("{name} needs a value"))?,
            };
            gh.flags.entry(key).or_default().push(value);
        } else if arg.starts_with('-') {
            return Err(format!("{arg}: the Bakery's gh takes --title, --body, --base, --label and --repo only"));
        } else {
            gh.words.push(arg);
        }
    }
    Ok(gh)
}

fn gh(state: &AppState, stream_id: &str, repo: &str, args: &[String]) -> Answer {
    let fail = |message: String| Answer { message, ..Answer::default() };
    let dir = match own_checkout(state, stream_id, repo) {
        Ok(dir) => dir,
        Err(e) => return fail(e),
    };
    let call = match parse(args) {
        Ok(call) => call,
        Err(e) => return fail(e),
    };
    let flag = |key: &str| call.flags.get(key).and_then(|v| v.last().copied());
    // An issue's repository: the one named, else the checkout's GitHub origin.
    let repository = || {
        let origin = || crate::git::run(&dir, &["remote", "get-url", "origin"]).ok().and_then(|url| crate::routes::github::github_slug(&url));
        flag("repo").map(str::to_string).or_else(origin).ok_or_else(|| "no GitHub repository here: give --repo owner/name".to_string())
    };
    let number = || call.words.get(2).and_then(|n| n.trim_start_matches('#').parse::<u64>().ok());
    let made = match call.words.get(..2) {
        Some(["pr", "create"]) => {
            if let Some(head) = flag("head") {
                let current = crate::git::run(&dir, &["branch", "--show-current"]).unwrap_or_default();
                if head != current.trim() {
                    return fail("a room opens a pull request for its own branch only".to_string());
                }
            }
            let Some(title) = flag("title") else { return fail("gh pr create here needs --title".to_string()) };
            let args = json!({ "stream_id": stream_id, "title": title, "body": flag("body"), "base": flag("base") });
            as_agent(state, stream_id, "git_pr", &args)
        }
        Some(["issue", "create"]) => {
            let repository = match repository() {
                Ok(repository) => repository,
                Err(e) => return fail(e),
            };
            let Some(title) = flag("title") else { return fail("gh issue create here needs --title".to_string()) };
            let labels = call.flags.get("label").cloned().unwrap_or_default();
            let args = json!({ "repository": repository, "title": title, "body": flag("body").unwrap_or_default(), "labels": labels });
            as_agent(state, stream_id, "github_issue_create", &args)
        }
        Some(["issue" | "pr", "comment"]) => {
            let repository = match repository() {
                Ok(repository) => repository,
                Err(e) => return fail(e),
            };
            let (Some(number), Some(body)) = (number(), flag("body")) else {
                return fail("gh comment here needs the number and --body".to_string());
            };
            let args = json!({ "repository": repository, "number": number, "body": body });
            as_agent(state, stream_id, "github_issue_comment", &args)
        }
        _ => {
            return fail(
                "in a Bakery room, gh only asks to open a pull request or an issue, or to comment; the operator merges, \
                 reviews and changes the rest, and the Bakery's GitHub tools read"
                    .to_string(),
            )
        }
    };
    match made {
        Ok(body) => {
            let said: Value = serde_json::from_str(&body).unwrap_or_default();
            let url = said["url"].as_str().or(said["output"].as_str()).map(|u| u.trim().lines().last().unwrap_or_default().to_string());
            Answer { ok: true, message: body, url, ..Answer::default() }
        }
        Err(e) => fail(e),
    }
}

/// `POST /remote/ask` at a thread's door: one request from its room.
pub(crate) async fn ask_at_door(
    State(door): State<crate::doors::Door>,
    headers: axum::http::HeaderMap,
    uri: axum::http::Uri,
    Json(ask): Json<Ask>,
) -> Result<Json<Answer>, (StatusCode, String)> {
    // Asked of the door, never through it: a request the door proxies (a
    // page in the thread's Preview, say) names a host, and isn't its room.
    if uri.authority().is_some() {
        return Err((StatusCode::FORBIDDEN, "ask the door itself, not through it".to_string()));
    }
    not_a_page(&headers)?;
    let crate::doors::Door { state, stream_id } = door;
    answered(state, stream_id, ask).await
}

/// `POST /remote/ask` at the seats' gateway: one request from a container,
/// its thread named by the credential its programs send (`BAKERY_DOOR_URL`
/// carries it, as Basic auth), which only that thread's containers hold.
pub(crate) async fn ask_at_gateway(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    uri: axum::http::Uri,
    Json(ask): Json<Ask>,
) -> Result<Json<Answer>, (StatusCode, String)> {
    if uri.authority().is_some() {
        return Err((StatusCode::FORBIDDEN, "ask the gateway itself, not through it".to_string()));
    }
    not_a_page(&headers)?;
    let basic = [axum::http::header::AUTHORIZATION, axum::http::header::PROXY_AUTHORIZATION]
        .iter()
        .find_map(|name| headers.get(name)?.to_str().ok());
    let Some((stream_id, _)) = basic.and_then(|basic| crate::host_list::credential_of(&state, basic)) else {
        return Err((StatusCode::UNAUTHORIZED, "a thread's own credential is needed".to_string()));
    };
    answered(state, stream_id, ask).await
}

/// A browser's request (it always names its page's origin) is never the
/// thread asking: git and gh send none. A page in the thread's Preview
/// reaches the door on this Mac's own address, past the proxy.
fn not_a_page(headers: &axum::http::HeaderMap) -> Result<(), (StatusCode, String)> {
    if headers.contains_key(axum::http::header::ORIGIN) {
        return Err((StatusCode::FORBIDDEN, "a web page never asks in the thread's name".to_string()));
    }
    Ok(())
}

async fn answered(state: AppState, stream_id: String, ask: Ask) -> Result<Json<Answer>, (StatusCode, String)> {
    let answer = crate::actor::spawn_blocking(move || match ask {
        Ask::Push { repo, refs } => push(&state, &stream_id, &repo, &refs),
        Ask::Gh { repo, args } => gh(&state, &stream_id, &repo, &args),
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(answer))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipe::ChildToolGateDecisionKind;
    use crate::routes::project::open_stream;
    use crate::testutil::*;
    use bakery_agent::StubModel;
    use std::sync::Arc;

    /// A conversation with a new commit on its own branch, in a clone of a
    /// local origin it reaches by file://, as a room's git does.
    fn conversation(dir: &Path) -> (AppState, crate::state::ActiveContext) {
        let state = state_with(dir, Arc::new(StubModel));
        let local = crate::git::tests::behind_its_origin(dir);
        run_git(&local, &["remote", "set-url", "origin", &format!("file://{}", dir.join("origin.git").display())]);
        let ctx = open_stream(&state, &open_req(&local, "caught")).unwrap().context;
        let wt = Path::new(&ctx.worktree_path);
        std::fs::write(wt.join("one.txt"), "x\n").unwrap();
        run_git(wt, &["add", "."]);
        run_git(wt, &["commit", "-qm", "one"]);
        (state, ctx)
    }

    fn approve(state: &AppState, stream_id: &str) {
        let card = crate::remote::waiting(state).items.into_iter().find(|w| w.stream_id == stream_id).expect("a card waits");
        crate::pipe::decide_child_tool_gate(state, stream_id, &card.card, ChildToolGateDecisionKind::AllowOnce).unwrap();
    }

    #[test]
    fn a_rooms_push_waits_for_the_operator_then_goes_out() {
        let dir = tempfile::tempdir().unwrap();
        let (state, ctx) = conversation(dir.path());
        let (sid, repo) = (ctx.stream_id.as_str(), ctx.worktree_path.as_str());
        let branch = format!("refs/heads/{}", ctx.branch);
        let one = |src: &str, dst: &str, force: bool| vec![PushRef { src: src.into(), dst: dst.into(), force }];

        let first = push(&state, sid, repo, &one(&branch, &branch, false));
        assert!(!first.ok && first.refs[0].message.contains("waiting for the operator"), "{first:?}");
        approve(&state, sid);
        let second = push(&state, sid, repo, &one("HEAD", &branch, false));
        assert!(second.ok, "{second:?}");
        let head = crate::git::run(Path::new(repo), &["rev-parse", "HEAD"]).unwrap();
        let sent = crate::git::run(&dir.path().join("origin.git"), &["rev-parse", &ctx.branch]).unwrap();
        assert_eq!(sent.trim(), head.trim());

        // What a room never does from here.
        for (src, dst, force, why) in [
            (branch.as_str(), branch.as_str(), true, "force push is the operator's"),
            ("", branch.as_str(), false, "deleting a branch"),
            (branch.as_str(), "refs/tags/v1", false, "only branches"),
            ("HEAD~1", branch.as_str(), false, "as it is"),
        ] {
            let answer = push(&state, sid, repo, &one(src, dst, force));
            assert!(!answer.ok && answer.refs[0].message.contains(why), "{answer:?}");
        }
        let elsewhere = push(&state, sid, &dir.path().join("up").display().to_string(), &one(&branch, &branch, false));
        assert!(elsewhere.message.contains("own checkout"), "{elsewhere:?}");
    }

    /// At the seats' gateway, a container's ask is its thread's by the
    /// credential its programs send: the same card a room's push gets. With
    /// no credential, or a forged one, nothing is asked.
    #[test]
    fn a_containers_ask_is_its_threads_by_its_credential() {
        let dir = tempfile::tempdir().unwrap();
        let (state, ctx) = conversation(dir.path());
        let branch = format!("refs/heads/{}", ctx.branch);
        let ask = || Ask::Push {
            repo: ctx.worktree_path.clone(),
            refs: vec![PushRef { src: branch.clone(), dst: branch.clone(), force: false }],
        };
        let (name, signed) = crate::host_list::credential(&state, &ctx.stream_id, crate::host_list::Kind::Container);
        let basic = |pair: String| format!("Basic {}", bakery_agent::tool_wire::encode_base64(pair.as_bytes()));
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let at_gateway = |auth: Option<String>| {
            let mut headers = axum::http::HeaderMap::new();
            if let Some(auth) = auth {
                headers.insert(axum::http::header::AUTHORIZATION, auth.parse().unwrap());
            }
            runtime.block_on(ask_at_gateway(State(state.clone()), headers, "/remote/ask".parse().unwrap(), Json(ask())))
        };
        let held = at_gateway(Some(basic(format!("{name}:{signed}")))).unwrap().0;
        assert!(!held.ok && held.refs[0].message.contains("waiting for the operator"), "{held:?}");
        assert!(
            crate::remote::waiting(&state).items.iter().any(|w| w.stream_id == ctx.stream_id),
            "a card in its thread"
        );
        assert_eq!(at_gateway(None).unwrap_err().0, StatusCode::UNAUTHORIZED);
        let forged = basic(format!("{name}:{}", "0".repeat(64)));
        assert_eq!(at_gateway(Some(forged)).unwrap_err().0, StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn a_rooms_gh_asks_only_what_an_agent_may_ask() {
        let dir = tempfile::tempdir().unwrap();
        let (state, ctx) = conversation(dir.path());
        let (sid, repo) = (ctx.stream_id.as_str(), ctx.worktree_path.as_str());
        let ask = |args: &[&str]| gh(&state, sid, repo, &args.iter().map(|a| a.to_string()).collect::<Vec<_>>());

        let pr = ask(&["pr", "create", "--title", "Fix it", "--body", "why"]);
        assert!(!pr.ok && pr.message.contains("waiting for the operator"), "a card for the pull request: {pr:?}");
        let card = crate::remote::waiting(&state).items.into_iter().find(|w| w.stream_id == sid).unwrap();
        assert_eq!((card.tool.as_str(), card.args["title"].as_str()), ("git_pr", Some("Fix it")));

        let issue = ask(&["issue", "create", "-t", "Broken", "-R", "acme/widget", "-l", "bug"]);
        assert!(issue.message.contains("waiting for the operator"), "{issue:?}");
        let comment = ask(&["issue", "comment", "7", "--body", "seen", "--repo=acme/widget"]);
        assert!(comment.message.contains("waiting for the operator"), "{comment:?}");

        for (args, why) in [
            (&["pr", "merge", "7"][..], "the operator merges"),
            (&["pr", "view", "7"][..], "the operator merges"),
            (&["issue", "create", "-t", "x"][..], "give --repo"),
            (&["pr", "create", "--draft"][..], "--draft"),
            (&["pr", "create", "--title", "x", "--head", "someone-else"][..], "own branch only"),
        ] {
            let answer = ask(args);
            assert!(!answer.ok && answer.message.contains(why), "{args:?}: {answer:?}");
        }
    }
}
