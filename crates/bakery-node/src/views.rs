//! What the desktop's views show for a conversation: its worktree in the
//! operator's editor (openvscode-server, in a container of its own) and the
//! dev servers it runs. Every URL is on 127.0.0.1: the node listens on a
//! fresh loopback port and forwards each connection to the container's
//! address on the seats' host-only network, so nothing listens on the Mac's
//! other interfaces, and a dev server started later needs no restart. The
//! editor's URL carries a connection token made at each start; the token
//! lives only in a file in the node's own folder, mounted read-only into the
//! editor's container, never on a command line or in an environment.

use crate::state::AppState;
use axum::{extract::State, http::StatusCode, Json};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

type HttpError = (StatusCode, String);

/// Where openvscode-server listens inside its container.
const EDITOR_PORT: u16 = 3000;

/// A view the node serves: its URL and where its loopback port forwards.
struct Open {
    url: String,
    /// `None` for a dev server on this Mac itself, which needs no forward.
    to: Option<SocketAddr>,
    task: Option<tokio::task::AbortHandle>,
}

/// Open views, by conversation and what they show (`code`, `preview:<port>`).
fn open_views() -> &'static Mutex<HashMap<(String, String), Open>> {
    static VIEWS: OnceLock<Mutex<HashMap<(String, String), Open>>> = OnceLock::new();
    VIEWS.get_or_init(Default::default)
}

/// Whether a conversation's view (`code`, `preview:<port>`) is open.
pub(crate) fn is_open(stream_id: &str, view: &str) -> bool {
    open_views()
        .lock()
        .unwrap()
        .contains_key(&(stream_id.to_string(), view.to_string()))
}

/// One open at a time per view: a second request waits for the first and
/// gets its URL. Two editor starts at once each wrote a fresh token to the
/// editor's one token file, so the second could overwrite the first's before
/// the starting editor read it, and the first answered with a URL its own
/// editor refused ("Forbidden.").
fn opening(key: &(String, String)) -> Arc<tokio::sync::Mutex<()>> {
    static OPENING: OnceLock<Mutex<HashMap<(String, String), Arc<tokio::sync::Mutex<()>>>>> =
        OnceLock::new();
    OPENING
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .entry(key.clone())
        .or_default()
        .clone()
}

fn internal(e: impl std::fmt::Display) -> HttpError {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

pub(crate) fn editor_name(stream_id: &str) -> String {
    format!(
        "bakery-code-{}",
        &blake3::hash(stream_id.as_bytes()).to_hex()[..12]
    )
}

/// A query value, percent-encoded but for unreserved characters and `/`.
fn query_value(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Listens on a fresh 127.0.0.1 port and copies each connection to `to`.
async fn forward(to: SocketAddr) -> Result<(u16, tokio::task::AbortHandle), HttpError> {
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .map_err(internal)?;
    let port = listener.local_addr().map_err(internal)?.port();
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut inbound, _)) = listener.accept().await else {
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            };
            tokio::spawn(async move {
                if let Ok(mut outbound) = tokio::net::TcpStream::connect(to).await {
                    let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
                }
            });
        }
    });
    Ok((port, task.abort_handle()))
}

async fn reachable(to: SocketAddr) -> bool {
    let attempt = tokio::net::TcpStream::connect(to);
    matches!(
        tokio::time::timeout(Duration::from_secs(1), attempt).await,
        Ok(Ok(_))
    )
}

/// Waits for something to listen at `to`, up to a minute: a first start
/// unpacks the editor's server.
async fn listening(to: SocketAddr) -> bool {
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline {
        if reachable(to).await {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    false
}

/// The URL of a view still being served, or `None` (and the view dropped)
/// when what it forwards to has gone away.
async fn still_open(key: &(String, String)) -> Option<String> {
    let (url, to) = {
        let views = open_views().lock().unwrap();
        let open = views.get(key)?;
        (open.url.clone(), open.to)
    };
    if let Some(to) = to {
        if !reachable(to).await {
            close(key);
            return None;
        }
    }
    Some(url)
}

fn close(key: &(String, String)) -> bool {
    let open = open_views().lock().unwrap().remove(key);
    if let Some(task) = open.as_ref().and_then(|o| o.task.as_ref()) {
        task.abort();
    }
    open.is_some()
}

#[derive(Deserialize, schemars::JsonSchema)]
pub(crate) struct CodeReq {
    stream_id: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub(crate) struct PreviewReq {
    stream_id: String,
    /// The port the dev server listens on, in the conversation's container
    /// or on this Mac.
    port: u16,
}

#[derive(Serialize, schemars::JsonSchema)]
pub(crate) struct ViewUrl {
    /// A 127.0.0.1 URL, for this Mac's own views.
    url: String,
}

#[derive(Serialize, schemars::JsonSchema)]
pub(crate) struct Closed {
    /// Whether the conversation's editor was open.
    closed: bool,
}

/// `POST /code/open`: the conversation's worktree in the operator's editor.
/// Its URL carries the editor's connection token, so only whoever holds
/// the URL can use the editor and its terminal. Hidden from agents.
pub(crate) async fn code_open(
    State(st): State<AppState>,
    Json(r): Json<CodeReq>,
) -> Result<Json<ViewUrl>, HttpError> {
    let key = (r.stream_id.clone(), "code".to_string());
    let opening = opening(&key);
    let _one = opening.lock().await;
    if let Some(url) = still_open(&key).await {
        return Ok(Json(ViewUrl { url }));
    }
    let stream_id = r.stream_id.clone();
    let (address, token, folder) = crate::actor::spawn_blocking(move || {
        let ctx = crate::state::context_for(&st, &stream_id)?;
        if ctx.worktree_path.is_empty() {
            return Err(format!("{stream_id} has no worktree to open"));
        }
        let name = editor_name(&stream_id);
        let token_dir = crate::state::cache_root(&st).join("code").join(&name);
        std::fs::create_dir_all(&token_dir).map_err(|e| format!("{}: {e}", token_dir.display()))?;
        let token = crate::node::mint_operator_token();
        std::fs::write(token_dir.join("token"), &token).map_err(|e| e.to_string())?;
        let workspace = std::path::PathBuf::from(&ctx.worktree_path);
        let address = crate::seat_container::start_editor(
            &st,
            &stream_id,
            &workspace,
            &name,
            &token_dir,
            EDITOR_PORT,
        )?;
        Ok((address, token, ctx.worktree_path))
    })
    .await
    .map_err(internal)?
    .map_err(|e| (StatusCode::CONFLICT, e))?;
    let to = SocketAddr::from((address, EDITOR_PORT));
    if !listening(to).await {
        return Err((
            StatusCode::GATEWAY_TIMEOUT,
            "the editor didn't start within a minute".to_string(),
        ));
    }
    let (port, task) = forward(to).await?;
    let url = format!(
        "http://127.0.0.1:{port}/?tkn={token}&folder={}",
        query_value(&folder)
    );
    open_views().lock().unwrap().insert(
        key,
        Open {
            url: url.clone(),
            to: Some(to),
            task: Some(task),
        },
    );
    Ok(Json(ViewUrl { url }))
}

#[derive(Deserialize, schemars::JsonSchema)]
pub(crate) struct GotoReq {
    stream_id: String,
    /// The edit row's change (`agent.file.changed`).
    event_hash: String,
}

#[derive(Serialize, schemars::JsonSchema, Debug)]
pub(crate) struct Goto {
    /// The conversation's editor: load it into its Code view, unless `opened`.
    url: String,
    /// The editor already showing in its Code view opened the file there:
    /// bring that view forward.
    opened: bool,
    /// The file, as the editor names it.
    file: String,
    /// The first line the edit changed, where the cursor goes.
    line: usize,
    /// The file no longer reads as the edit left it: changed since, or undone.
    changed: bool,
}

/// `POST /code/goto`: an edit row's file in its conversation's editor, at the
/// first line the edit changed, whatever happened to the file since. Hidden
/// from agents.
pub(crate) async fn code_goto(
    State(st): State<AppState>,
    Json(r): Json<GotoReq>,
) -> Result<Json<Goto>, HttpError> {
    let (state, stream_id, hash) = (st.clone(), r.stream_id.clone(), r.event_hash);
    let (file, line, changed) = crate::actor::spawn_blocking(move || edit_location(&state, &stream_id, &hash))
        .await
        .map_err(internal)?
        .map_err(|e| (StatusCode::NOT_FOUND, e))?;
    let running = still_open(&(r.stream_id.clone(), "code".to_string())).await.is_some();
    let Json(ViewUrl { url }) = code_open(State(st), Json(CodeReq { stream_id: r.stream_id.clone() })).await?;
    let (name, at) = (editor_name(&r.stream_id), file.clone());
    let opened = running
        && crate::actor::spawn_blocking(move || crate::seat_container::editor_goto(&name, &at, line))
            .await
            .is_ok_and(|done| done.is_ok());
    let url = if opened { url } else { at_line(&url, &file, line) };
    Ok(Json(Goto { url, opened, file, line, changed }))
}

/// The edit's file as the editor names it (the worktree is mounted at its own
/// path), the first line the edit changed, and whether the file now differs
/// from what the edit left.
fn edit_location(st: &AppState, stream_id: &str, event_hash: &str) -> Result<(String, usize, bool), String> {
    let (p, before, after) = crate::snapshots::edit_sides(st, stream_id, event_hash)?;
    let rel = p["file_path"].as_str().unwrap_or_default();
    let on_disk = crate::state::jail_for(st, stream_id)?.resolve(rel)?;
    let now = std::fs::read_to_string(&on_disk).ok();
    let removed = match p["landed"].is_object() {
        true => p["landed"]["after"].is_null(),
        false => p["new_content"].is_null(),
    };
    // A proposal not decided yet isn't in the file: nothing changed since.
    let undecided = !p["landed"].is_object() && {
        let heart = st.heart.lock().unwrap();
        !["agent.edit.accepted", "agent.edit.rejected"].into_iter().any(|family| {
            let decisions = heart.events_of_family(stream_id, family).unwrap_or_default();
            decisions.iter().any(|e| e.causality.iter().any(|h| h == event_hash))
        })
    };
    let changed = !undecided
        && match &after {
            Some(text) => now.as_deref() != Some(text.as_str()),
            // It removed the file; a file that isn't text can't be compared.
            None => removed && on_disk.exists(),
        };
    let ctx = crate::state::context_for(st, stream_id)?;
    let file = std::path::Path::new(&ctx.worktree_path).join(rel).to_string_lossy().into_owned();
    let line = first_changed_line(before.as_deref().unwrap_or(""), after.as_deref().unwrap_or(""));
    Ok((file, line, changed))
}

/// The first line, from 1, where `after` differs from `before`; past the end
/// of `after`, its last line.
fn first_changed_line(before: &str, after: &str) -> usize {
    let same = before.lines().zip(after.lines()).take_while(|(a, b)| a == b).count();
    (same + 1).min(after.lines().count().max(1))
}

/// The editor's URL, opening `file` at `line` when its page loads
/// (openvscode reads the `payload` query: `openFile` with `gotoLineMode`).
fn at_line(url: &str, file: &str, line: usize) -> String {
    let authority = url.trim_start_matches("http://").split('/').next().unwrap_or_default();
    let open = format!("vscode-remote://{authority}{}:{line}", query_value(file));
    let payload = serde_json::json!([["openFile", open], ["gotoLineMode", "true"]]);
    format!("{url}&payload={}", query_value(&payload.to_string()))
}

/// `POST /code/close`: stop the conversation's editor.
pub(crate) async fn code_close(Json(r): Json<CodeReq>) -> Json<Closed> {
    let closed = close(&(r.stream_id.clone(), "code".to_string()));
    let name = editor_name(&r.stream_id);
    let _ = crate::actor::spawn_blocking(move || crate::seat_container::stop(&name)).await;
    Json(Closed { closed })
}

/// `POST /preview/open`: a 127.0.0.1 URL for the dev server a conversation
/// runs on `port`, in its container (the editor's or a seat's) or, for an
/// agent in its room or on the whole Mac, on this Mac itself.
pub(crate) async fn preview_open(Json(r): Json<PreviewReq>) -> Result<Json<ViewUrl>, HttpError> {
    if r.port == 0 {
        return Err((StatusCode::BAD_REQUEST, "port starts at 1".to_string()));
    }
    let key = (r.stream_id.clone(), format!("preview:{}", r.port));
    let opening = opening(&key);
    let _one = opening.lock().await;
    if let Some(url) = still_open(&key).await {
        return Ok(Json(ViewUrl { url }));
    }
    let stream_id = r.stream_id.clone();
    let address =
        crate::actor::spawn_blocking(move || crate::seat_container::address_of(&stream_id))
            .await
            .map_err(internal)?;
    let open = match address {
        Some(address) => {
            let to = SocketAddr::from((address, r.port));
            let (port, task) = forward(to).await?;
            Open {
                url: format!("http://127.0.0.1:{port}/"),
                to: Some(to),
                task: Some(task),
            }
        }
        None => Open {
            url: format!("http://127.0.0.1:{}/", r.port),
            to: None,
            task: None,
        },
    };
    let url = open.url.clone();
    open_views().lock().unwrap().insert(key, open);
    Ok(Json(ViewUrl { url }))
}

/// Closes every view of a conversation and stops the editor this node
/// started for it: it was archived. (An editor left from before a restart
/// gives way when the conversation's editor next opens.)
pub(crate) fn stop_all(stream_id: &str) {
    let keys: Vec<(String, String)> = open_views()
        .lock()
        .unwrap()
        .keys()
        .filter(|(id, _)| id == stream_id)
        .cloned()
        .collect();
    for key in &keys {
        close(key);
    }
    if keys.iter().any(|(_, view)| view == "code") {
        crate::seat_container::stop(&editor_name(stream_id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    /// An edit row opens at its first changed line, and says when the file
    /// no longer reads as the edit left it.
    #[test]
    fn an_edit_opens_at_its_first_changed_line_and_says_when_it_changed_since() {
        use crate::testutil::*;
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), std::sync::Arc::new(bakery_agent::StubModel));
        let ctx = crate::routes::project::open_stream(&state, &open_req(&repo, "goto")).unwrap().context;
        let model: std::sync::Arc<dyn bakery_agent::Model> = std::sync::Arc::new(bakery_agent::StubModel);
        let (before, after) = ("one\ntwo\nthree\n", "one\nTWO\nthree\nfour\n");
        let edit = crate::routes::agent::propose_edit_event(
            &state, &ctx.stream_id, crate::node::Seat::Orchestrator, &model, "f.txt", after, "why", before.into(), "t1", "c1",
        )
        .unwrap();
        let file = std::path::Path::new(&ctx.worktree_path).join("f.txt");
        std::fs::write(&file, before).unwrap();
        let at = edit_location(&state, &ctx.stream_id, &edit.event_hash).unwrap();
        assert_eq!(at, (file.to_string_lossy().into_owned(), 2, false), "undecided: not in the file yet");
        let decision = crate::routes::agent::AgentDecisionReq { stream_id: ctx.stream_id.clone(), event_hash: edit.event_hash.clone() };
        crate::routes::agent::decide_as(&state, &decision, true, None, "human").unwrap();
        assert!(!edit_location(&state, &ctx.stream_id, &edit.event_hash).unwrap().2, "as the edit left it");
        std::fs::write(&file, "since\n").unwrap();
        assert!(edit_location(&state, &ctx.stream_id, &edit.event_hash).unwrap().2, "changed since");

        assert_eq!(first_changed_line("a\nb\n", "a\nb\n"), 2, "unchanged: its last line");
        assert_eq!(first_changed_line("a\nb\nc\n", "a\n"), 1, "cut at the end: its last line");
        assert_eq!(first_changed_line("a\n", ""), 1, "removed: line 1");
        let url = at_line("http://127.0.0.1:5000/?tkn=t&folder=/w", "/w/a b.rs", 7);
        assert!(url.starts_with("http://127.0.0.1:5000/?tkn=t&folder=/w&payload=%5B%5B%22openFile%22"), "{url}");
        assert!(url.contains("vscode-remote%3A//127.0.0.1%3A5000/w/a%2520b.rs%3A7"), "{url}");
        assert!(url.contains("gotoLineMode"), "{url}");
    }

    #[tokio::test]
    async fn one_open_at_a_time_per_view() {
        let key = ("w2-views".to_string(), "code".to_string());
        let first = opening(&key).lock_owned().await;
        assert!(
            opening(&key).try_lock().is_err(),
            "a second open of it waits"
        );
        let other = ("w2-views".to_string(), "preview:5173".to_string());
        assert!(opening(&other).try_lock().is_ok(), "another view doesn't");
        drop(first);
        assert!(
            opening(&key).try_lock().is_ok(),
            "and goes once the first is done"
        );
    }

    #[test]
    fn a_folder_rides_in_the_url_encoded() {
        assert_eq!(
            query_value("/Users/op/My Project/ü"),
            "/Users/op/My%20Project/%C3%BC"
        );
        assert_eq!(query_value("a&b=c#d"), "a%26b%3Dc%23d");
    }

    /// The forward listens on 127.0.0.1 only and carries bytes both ways;
    /// closing it stops new connections.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_view_forwards_loopback_to_its_target_until_closed() {
        let echo = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let target = echo.local_addr().unwrap();
        std::thread::spawn(move || {
            if let Some(Ok(mut conn)) = echo.incoming().next() {
                let mut buf = [0u8; 4];
                if conn.read_exact(&mut buf).is_ok() {
                    let _ = conn.write_all(&buf);
                }
            }
        });
        let (port, task) = forward(target).await.unwrap();
        let echoed = std::thread::spawn(move || {
            let mut conn = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
            conn.write_all(b"ping").unwrap();
            let mut back = [0u8; 4];
            conn.read_exact(&mut back).unwrap();
            back
        })
        .join()
        .unwrap();
        assert_eq!(&echoed, b"ping");
        let key = ("s".to_string(), "preview:1".to_string());
        open_views().lock().unwrap().insert(
            key.clone(),
            Open {
                url: String::new(),
                to: Some(target),
                task: Some(task),
            },
        );
        assert!(close(&key));
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            std::net::TcpStream::connect(("127.0.0.1", port)).is_err(),
            "closed views stop listening"
        );
    }
}
