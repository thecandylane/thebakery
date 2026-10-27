//! A thread's door: its own port on loopback ([`bakery_exec::DOORS`]), where
//! its sandbox on this Mac reaches the Bakery: its MCP tools, and for the
//! Mac sandbox the gateway. Every room's profile closes every other door and
//! the Bakery's shared ports, so a room is known by the one door it can
//! reach, not by a secret it holds. macOS lets any process read a same-user
//! process's arguments and environment, and no Seatbelt rule stops it, so a
//! token a room reads from a sibling's process must open nothing: at a door,
//! a token counts only for that door's own thread.

use crate::state::AppState;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

/// Each thread's door while the node runs.
fn doors() -> &'static Mutex<HashMap<String, u16>> {
    static DOORS: OnceLock<Mutex<HashMap<String, u16>>> = OnceLock::new();
    DOORS.get_or_init(Default::default)
}

/// Where each thread's door was, kept across restarts: a thread keeps its
/// port, so the app's Preview, which holds it, needn't ask again.
fn kept_file(state: &AppState) -> PathBuf {
    state.base.join("doors.json")
}

fn kept(state: &AppState) -> HashMap<String, u16> {
    std::fs::read_to_string(kept_file(state))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn keep(state: &AppState, kept: &HashMap<String, u16>) {
    let (file, json) = (kept_file(state), serde_json::to_string(kept).unwrap_or_default());
    let fresh = file.with_extension("json.new");
    if let Err(e) = std::fs::write(&fresh, json).and_then(|_| std::fs::rename(&fresh, &file)) {
        eprintln!("doors: keeping {}: {e}", file.display());
    }
}

/// The ports to try for `stream_id`'s door, in order: its own from before;
/// then from where its name points in the block, each port no other thread
/// kept; last the ones others kept, so a full block still opens.
fn order(stream_id: &str, kept: &HashMap<String, u16>) -> Vec<u16> {
    let block: Vec<u16> = bakery_exec::DOORS.collect();
    let hash = blake3::hash(stream_id.as_bytes());
    let at = u64::from_le_bytes(hash.as_bytes()[..8].try_into().unwrap()) as usize % block.len();
    let own = kept.get(stream_id).copied();
    let others: HashSet<u16> = kept
        .iter()
        .filter(|(thread, _)| *thread != stream_id)
        .map(|(_, port)| *port)
        .collect();
    let ring = block[at..]
        .iter()
        .chain(&block[..at])
        .copied()
        .filter(|p| Some(*p) != own);
    let (free, theirs): (Vec<u16>, Vec<u16>) = ring.partition(|p| !others.contains(p));
    own.into_iter().chain(free).chain(theirs).collect()
}

/// The thread's door, opened the first time its room needs it, on the same
/// port as before a restart when that port is free.
pub(crate) fn door(state: &AppState, stream_id: &str) -> Result<u16, String> {
    let mut doors = doors().lock().unwrap();
    if let Some(port) = doors.get(stream_id) {
        return Ok(*port);
    }
    let taken: HashSet<u16> = doors.values().copied().collect();
    let mut kept = kept(state);
    for port in order(stream_id, &kept).into_iter().filter(|p| !taken.contains(p)) {
        // A port something else holds (another node's door, say) is passed by.
        let Ok(listener) = std::net::TcpListener::bind(("127.0.0.1", port)) else {
            continue;
        };
        open(state.clone(), stream_id.to_string(), listener)?;
        doors.insert(stream_id.to_string(), port);
        if kept.get(stream_id) != Some(&port) {
            // A thread that kept this port finds another next time.
            kept.retain(|_, p| *p != port);
            kept.insert(stream_id.to_string(), port);
            keep(state, &kept);
        }
        return Ok(port);
    }
    Err("every door to the Bakery on this Mac is in use".to_string())
}

/// The Bakery's own ports on this Mac a room never reaches: its API, the
/// model proxy's two, and the gateways'.
fn shared_ports() -> String {
    let api = std::env::var("BAKERY_NODE_ADDR")
        .ok()
        .and_then(|addr| addr.rsplit_once(':')?.1.parse::<u16>().ok())
        .unwrap_or(7717);
    [
        api,
        crate::model_proxy::port(),
        crate::model_proxy::egress_port(),
        crate::seat_gateway::PORT,
    ]
    .map(|p| p.to_string())
    .join(",")
}

/// Where a room's own programs find their thread's door (a helper that
/// reaches the Bakery, say): no secret, since only that room reaches it.
pub(crate) const URL_ENV: &str = "BAKERY_DOOR_URL";

/// How long a fresh process waits for its first connection to the Bakery.
pub(crate) const FIRST_CONNECTION_WAIT: std::time::Duration = std::time::Duration::from_secs(300);

/// Waits out a fresh process's first connection to the Bakery at `url`
/// (`http://[who@]host:port/...`), up to `within`. The Mac's network filter
/// (LuLu) checks a new process on its first connection: seconds on a quiet
/// Mac, past half a minute under load, while the next connection takes a
/// millisecond. An attempt given up early (ureq gives up connecting at 30 s)
/// starts that check over, so this is one connect, waited out. A room's
/// helpers call it before their first request; nothing to wait for when
/// `url` names no address.
pub(crate) fn first_connection(url: &str, within: std::time::Duration) -> Result<(), String> {
    use std::net::ToSocketAddrs;
    let authority = url.split_once("://").map_or(url, |(_, rest)| rest).split('/').next().unwrap_or_default();
    let at = authority.rsplit('@').next().unwrap_or_default();
    let Some(addr) = at.to_socket_addrs().ok().and_then(|mut found| found.next()) else {
        return Ok(());
    };
    std::net::TcpStream::connect_timeout(&addr, within)
        .map(drop)
        .map_err(|e| format!("the Bakery didn't answer at {at}: {e}"))
}

/// What a thread's room on this Mac is told of its doors: its own (for its
/// profile, and as [`URL_ENV`] for its programs), and the Bakery's ports
/// closed to it.
pub(crate) fn room_env(state: &AppState, stream_id: &str) -> Vec<(String, String)> {
    let mut env = vec![(bakery_exec::SHUT_ENV.to_string(), shared_ports())];
    match door(state, stream_id) {
        Ok(port) => {
            env.push((bakery_exec::DOOR_ENV.to_string(), port.to_string()));
            env.push((URL_ENV.to_string(), format!("http://127.0.0.1:{port}")));
        }
        Err(e) => eprintln!("doors: {stream_id}: {e}"),
    }
    env
}

/// A door's own thread, which every request to it comes from: a route
/// mounted on the door (below) takes it as its state.
#[derive(Clone)]
pub(crate) struct Door {
    pub(crate) state: AppState,
    pub(crate) stream_id: String,
}

/// Whether the MCP token `headers` carry is this door's own thread's.
fn ours(door: &Door, headers: &HeaderMap) -> Result<(), (StatusCode, String)> {
    let token = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default();
    let thread = door
        .state
        .mcp_tokens
        .lock()
        .unwrap()
        .get(token)
        .map(|binding| binding.stream_id.clone());
    match thread {
        Some(thread) if thread == door.stream_id => Ok(()),
        Some(_) => Err((
            StatusCode::FORBIDDEN,
            "this door is another thread's".to_string(),
        )),
        None => Err((
            StatusCode::UNAUTHORIZED,
            "unknown or expired MCP token".to_string(),
        )),
    }
}

async fn tools(
    State(door): State<Door>,
    headers: HeaderMap,
) -> Result<Json<Vec<crate::mcp::McpToolWire>>, (StatusCode, String)> {
    ours(&door, &headers)?;
    crate::mcp::mcp_tools(State(door.state), headers).await
}

async fn call(
    State(door): State<Door>,
    headers: HeaderMap,
    Json(req): Json<crate::mcp::McpCallReq>,
) -> Result<Json<crate::mcp::McpCallResp>, (StatusCode, String)> {
    ours(&door, &headers)?;
    crate::mcp::mcp_call(State(door.state), headers, Json(req)).await
}

/// Serves a thread's door from now on, on its own thread like the
/// gateways: its MCP tools, and for its sandbox and its Preview a proxy,
/// CONNECT and plain http alike, judged as its thread's.
fn open(state: AppState, stream_id: String, listener: std::net::TcpListener) -> Result<(), String> {
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    std::thread::spawn(move || {
        let Ok(runtime) = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
        else {
            return;
        };
        runtime.block_on(async move {
            let Ok(listener) = tokio::net::TcpListener::from_std(listener) else {
                return;
            };
            // Every route here serves the door's own thread only.
            let routes = Router::new()
                .route("/mcp/tools", get(tools))
                .route("/mcp/call", post(call))
                // Its own git push and gh, held for the operator.
                .route("/remote/ask", post(crate::remote_ask::ask_at_door))
                // Its project's sign-ins, their stand-ins made real (`swap`).
                .route("/to/{*rest}", axum::routing::any(crate::swap::to_host))
                // Google's metadata server, for a project's Google sign-in.
                .route("/", get(crate::swap::metadata))
                .route("/computeMetadata/{*rest}", get(crate::swap::metadata))
                .with_state(Door {
                    state: state.clone(),
                    stream_id: stream_id.clone(),
                });
            // A sign-in host its project turned on is the door's to answer.
            let intercept = {
                let (state, stream_id) = (state.clone(), stream_id.clone());
                let (wanted, sid) = (state.clone(), stream_id.clone());
                crate::seat_gateway::Intercept {
                    wants: Arc::new(move |authority| crate::swap::intercepts(&wanted, &sid, authority)),
                    serve: Arc::new(move |authority, upgraded| {
                        let host = authority.rsplit_once(':').map_or(authority.clone(), |(h, _)| h.to_string());
                        Box::pin(crate::swap_tls::serve(state.clone(), stream_id.clone(), host, upgraded))
                    }),
                }
            };
            let allow: crate::seat_gateway::Allow = Arc::new(move |authority, headers| {
                if crate::swap::intercepts(&state, &stream_id, authority) {
                    return Ok(());
                }
                crate::host_list::verdict_for(
                    &state,
                    authority,
                    &stream_id,
                    crate::host_list::Kind::Mac,
                    headers,
                )
            });
            crate::seat_gateway::serve(listener, allow, routes, true, true, Some(intercept)).await;
        });
    });
    Ok(())
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct DoorQuery {
    pub(crate) stream_id: String,
}

#[derive(serde::Serialize, schemars::JsonSchema)]
pub(crate) struct DoorUrl {
    /// `http://127.0.0.1:<port>`: a proxy for the thread's Preview, held to
    /// its project's list.
    pub(crate) url: String,
}

/// `GET /thread/door`: a thread's door, for the app's Preview of it.
pub(crate) async fn door_route(
    State(st): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<DoorQuery>,
) -> Result<Json<DoorUrl>, (StatusCode, String)> {
    crate::actor::spawn_blocking(move || {
        crate::state::context_for(&st, &q.stream_id).map_err(|e| (StatusCode::NOT_FOUND, e))?;
        let port = door(&st, &q.stream_id).map_err(|e| (StatusCode::SERVICE_UNAVAILABLE, e))?;
        Ok(Json(DoorUrl {
            url: format!("http://127.0.0.1:{port}"),
        }))
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh process's first connection to the Bakery waits on the door
    /// its URL names, whatever else the URL carries (a sign-in, a path); a
    /// closed door says so, and a URL with no address has nothing to wait for.
    #[test]
    fn a_first_connection_waits_on_the_door_its_url_names() {
        let open = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = open.local_addr().unwrap().port();
        let wait = std::time::Duration::from_secs(300);
        assert_eq!(first_connection(&format!("http://wt-a:sig%2Fx@127.0.0.1:{port}/remote/ask"), wait), Ok(()));
        drop(open);
        // Port 1: privileged, so nothing of ours listens there, and no other
        // test can be handed it as a free port.
        let closed = first_connection("http://127.0.0.1:1", wait).unwrap_err();
        assert!(closed.contains("127.0.0.1:1"), "{closed}");
        assert_eq!(first_connection("not a url", wait), Ok(()));
    }

    /// A thread's door answers its own thread's MCP token, and never
    /// another's, though that token is in reach of any process on this Mac.
    #[test]
    fn a_door_answers_only_its_own_threads_token() {
        let dir = tempfile::tempdir().unwrap();
        let state =
            crate::testutil::state_with(dir.path(), std::sync::Arc::new(bakery_agent::StubModel));
        let scope = crate::mcp::McpScope::Agent;
        let seat = crate::node::Seat::Orchestrator;
        let mine = crate::mcp::mint_mcp_token(&state, "wt:door-a", seat, scope);
        let theirs = crate::mcp::mint_mcp_token(&state, "wt:door-b", seat, scope);
        let port = door(&state, "wt:door-a").unwrap();
        crate::testutil::first_connection(([127, 0, 0, 1], port).into());
        assert!(bakery_exec::DOORS.contains(&port));
        assert_eq!(
            door(&state, "wt:door-a").unwrap(),
            port,
            "one door per thread"
        );
        assert_ne!(door(&state, "wt:door-b").unwrap(), port);

        // A fresh binary's first loopback call can wait on LuLu, longer
        // under load: each call tries again until a two-minute deadline.
        let tools = |token: &str| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
            loop {
                let answer = ureq::get(&format!("http://127.0.0.1:{port}/mcp/tools"))
                    .timeout(std::time::Duration::from_secs(120))
                    .set("Authorization", &format!("Bearer {token}"))
                    .call();
                match answer {
                    Ok(r) => return r.status(),
                    Err(ureq::Error::Status(code, _)) => return code,
                    Err(ureq::Error::Transport(t)) => {
                        assert!(
                            std::time::Instant::now() < deadline,
                            "the door never answered: {t}"
                        );
                        std::thread::sleep(std::time::Duration::from_millis(200));
                    }
                }
            }
        };
        assert_eq!(tools(&mine), 200, "its own thread's token");
        assert_eq!(tools(&theirs), 403, "a sibling's token opens nothing here");
        assert_eq!(tools("made-up"), 401);

        let env = room_env(&state, "wt:door-a");
        let value = |k: &str| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
        assert_eq!(value(bakery_exec::DOOR_ENV), Some(port.to_string()));
        assert_eq!(value(URL_ENV), Some(format!("http://127.0.0.1:{port}")));
        let shut = value(bakery_exec::SHUT_ENV).unwrap();
        assert!(shut
            .split(',')
            .any(|p| p == crate::model_proxy::port().to_string()));
        assert!(shut
            .split(',')
            .any(|p| p == crate::seat_gateway::PORT.to_string()));
    }
    /// A thread's door keeps its port across restarts: its own from before,
    /// else where its name points; a port another thread kept comes last.
    #[test]
    fn a_threads_door_keeps_its_port_across_restarts() {
        let kept: HashMap<String, u16> = [("wt:a".to_string(), 47900)].into();
        let a = order("wt:a", &kept);
        assert_eq!(a[0], 47900, "its own from before");
        let b = order("wt:b", &kept);
        assert_eq!(b, order("wt:b", &HashMap::new()).into_iter().filter(|p| *p != 47900).chain([47900]).collect::<Vec<_>>());
        assert_eq!(*b.last().unwrap(), 47900, "another's kept port last");
        assert_eq!(b.len(), bakery_exec::DOORS.len(), "every port, once");

        let dir = tempfile::tempdir().unwrap();
        let state =
            crate::testutil::state_with(dir.path(), std::sync::Arc::new(bakery_agent::StubModel));
        let port = door(&state, "wt:keep-1").unwrap();
        assert_eq!(super::kept(&state).get("wt:keep-1"), Some(&port), "kept");

        // A restart: a node whose file says where a thread's door was opens
        // it there again. (This test's node still holds its own doors.)
        let after = tempfile::tempdir().unwrap();
        let restarted =
            crate::testutil::state_with(after.path(), std::sync::Arc::new(bakery_agent::StubModel));
        let free = bakery_exec::DOORS
            .rev()
            .find(|p| std::net::TcpListener::bind(("127.0.0.1", *p)).is_ok())
            .unwrap();
        keep(&restarted, &[("wt:keep-2".to_string(), free)].into());
        assert_eq!(door(&restarted, "wt:keep-2").unwrap(), free);
    }

    /// One raw request to a door, as a browser sends it through its proxy:
    /// the status it answers.
    fn exchange(port: u16, request: &str) -> u16 {
        use std::io::{Read, Write};
        // A fresh binary's first loopback call can wait on LuLu.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
        let mut stream = loop {
            match std::net::TcpStream::connect(("127.0.0.1", port)) {
                Ok(stream) => break stream,
                Err(e) => {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "the door never answered: {e}"
                    );
                    std::thread::sleep(std::time::Duration::from_millis(200));
                }
            }
        };
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(120)))
            .unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") && stream.read(&mut byte).unwrap() == 1 {
            head.push(byte[0]);
        }
        let head = String::from_utf8_lossy(&head).to_string();
        head.split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(|| panic!("{head}"))
    }

    /// The app's Preview goes out through its thread's door (GET
    /// /thread/door): a host off the project's list waits on the operator,
    /// a card in that thread, as for the thread's own sandbox. A page there
    /// never asks in the thread's name.
    #[test]
    fn a_previews_off_list_host_asks_in_its_thread() {
        let (dir, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let state =
            crate::testutil::state_with(dir.path(), std::sync::Arc::new(bakery_agent::StubModel));
        let root = crate::catalog::DirectoryRootSpec::Attach(work.path().to_str().unwrap());
        let (_, thread) = crate::catalog::create_project(&state, "shop", Some(root)).unwrap();
        let sid = thread.stream_id.clone();
        let query = axum::extract::Query(DoorQuery {
            stream_id: sid.clone(),
        });
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let url = runtime
            .block_on(door_route(State(state.clone()), query))
            .unwrap()
            .0
            .url;
        let port: u16 = url
            .strip_prefix("http://127.0.0.1:")
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(port, door(&state, &sid).unwrap(), "the thread's own door");

        let browser = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/146.0.0.0 Electron/44.7.0 Safari/537.36";
        let connect = format!(
            "CONNECT offlist.example:443 HTTP/1.1\r\nHost: offlist.example:443\r\nProxy-Connection: keep-alive\r\nUser-Agent: {browser}\r\n\r\n"
        );
        assert_eq!(exchange(port, &connect), 403, "held");
        let asks = state
            .heart
            .lock()
            .unwrap()
            .events_of_family(&sid, crate::host_list::ASK)
            .unwrap();
        assert_eq!(asks.len(), 1, "one card in the thread");
        let card: serde_json::Value = serde_json::from_str(&asks[0].payload).unwrap();
        assert_eq!(
            (card["host"].as_str(), card["program"].as_str()),
            (Some("offlist.example"), Some("Mozilla/5.0"))
        );

        let ask = r#"{"kind":"push","repo":"/tmp/x","refs":[]}"#;
        let proxied = format!(
            "POST http://offlist.example/remote/ask HTTP/1.1\r\nHost: offlist.example\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{ask}",
            ask.len()
        );
        assert_eq!(
            exchange(port, &proxied),
            403,
            "a page's request through the door is not the thread asking"
        );
        // Past the proxy, on this Mac's own address: a page's own request
        // names its origin, and is refused; git's and gh's never do.
        let direct = |origin: &str| {
            format!(
                "POST /remote/ask HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{origin}Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{ask}",
                ask.len()
            )
        };
        assert_eq!(
            exchange(port, &direct("Origin: http://localhost:5173\r\n")),
            403,
            "a page's own request to the door"
        );
        assert_eq!(exchange(port, &direct("")), 200, "the room's own ask");
    }
}
