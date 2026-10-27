//! The seats' gateway: the one host address a container seat reaches, its
//! internal network's own gateway. Once the first seat attaches and that
//! address exists, the node listens there (never on every interface) and
//! serves three things:
//! - `POST /mcp`: the node's MCP tools over HTTP, the same JSON-RPC the stdio
//!   shim speaks, behind the session's own bearer token;
//! - `POST /remote/ask`: a container's own `git push` and `gh`, held for the
//!   operator (`remote_ask`), its thread named by its credential;
//! - `CONNECT host:443`: an HTTPS tunnel, only to the hosts seats need (model
//!   APIs and package registries), the ones the operator adds in
//!   Settings → Machine access, and a project's own list (`host_list`), and
//!   never to this Mac, its network or the tailnet. Nothing else leaves a
//!   seat.

use axum::{
    body::Body,
    extract::Request,
    http::{
        header::{AUTHORIZATION, CONNECTION, HOST, PROXY_AUTHENTICATE, PROXY_AUTHORIZATION, TE, TRAILER, UPGRADE},
        HeaderMap, HeaderValue, Method, StatusCode,
    },
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use hyper::body::Incoming;
use hyper_util::rt::TokioIo;
use serde_json::Value;
use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tower::ServiceExt;

/// Where seats find the node: their network's gateway, this port.
pub(crate) const PORT: u16 = 47790;

/// Hosts every seat may open a tunnel to; the operator adds more
/// (`MachineAccess::allow_hosts`), never fewer. A leading dot allows any
/// subdomain.
pub(crate) const BUILT_IN: &[&str] = &[
    // Model APIs and their sign-in.
    "api.anthropic.com",
    "console.anthropic.com",
    "platform.claude.com",
    "claude.ai",
    "statsig.anthropic.com",
    "api.x.ai",
    "generativelanguage.googleapis.com",
    "api.openai.com",
    // Package registries and source hosts.
    "crates.io",
    "index.crates.io",
    "static.crates.io",
    "registry.npmjs.org",
    "github.com",
    "codeload.github.com",
    ".githubusercontent.com",
    "pypi.org",
    "files.pythonhosted.org",
    "proxy.golang.org",
    "sum.golang.org",
];

/// Whether `authority` (`host:port`) is on `list`, on `port`.
pub(crate) fn allowed<S: AsRef<str>>(authority: &str, list: &[S], port: u16) -> bool {
    let Some((host, p)) = authority.rsplit_once(':') else {
        return false;
    };
    let host = host.to_ascii_lowercase();
    p == port.to_string()
        && list
            .iter()
            .map(AsRef::as_ref)
            .any(|a| match a.strip_prefix('.') {
                Some(domain) => host.ends_with(a) && host.len() > domain.len() + 1,
                None => host == a,
            })
}

/// Whether a tunnel may open to `host:port`, given its CONNECT's headers;
/// `Err` is the refusal: its status and why.
pub(crate) type Allow =
    Arc<dyn Fn(&str, &HeaderMap) -> Result<(), crate::host_list::Refusal> + Send + Sync>;

/// A CONNECT the gateway answers itself rather than tunnelling (a door's
/// sign-in hosts, `swap_tls`): whether it wants an authority, and what
/// serves the connection once the client's tunnel is up.
#[derive(Clone)]
pub(crate) struct Intercept {
    pub(crate) wants: Arc<dyn Fn(&str) -> bool + Send + Sync>,
    pub(crate) serve:
        Arc<dyn Fn(String, hyper::upgrade::Upgraded) -> futures::future::BoxFuture<'static, ()> + Send + Sync>,
}

/// The node's own address, as the stdio shim reaches it.
fn node_url() -> String {
    let addr = std::env::var("BAKERY_NODE_ADDR").unwrap_or_else(|_| "127.0.0.1:7717".to_string());
    format!("http://{addr}")
}

/// Each door's scope (`seat_container::scope_of`), by its address: whose
/// containers its network holds.
fn scopes() -> &'static Mutex<std::collections::HashMap<Ipv4Addr, String>> {
    static SCOPES: OnceLock<Mutex<std::collections::HashMap<Ipv4Addr, String>>> = OnceLock::new();
    SCOPES.get_or_init(Default::default)
}

/// Whether `stream_id` may use the door at `gateway`: it is of the scope
/// that door's network holds. Apple's networks already keep other
/// projects' containers from reaching it; this holds a credential to its
/// own project's door as well.
fn in_scope(state: &crate::state::AppState, gateway: Ipv4Addr, stream_id: &str) -> bool {
    let scope = crate::seat_container::scope_of(state, stream_id);
    scopes().lock().unwrap().get(&gateway).is_some_and(|held| *held == scope)
}

/// The threads a request to a door names: its credential's (a CONNECT's
/// `Proxy-Authorization`, or `Authorization` for the door's own routes),
/// and the one in a sign-in path (`/s/<thread>/to/…`), which the swap acts
/// as. Each must be of the door's scope: one project's credential never
/// carries another's thread in its path.
fn callers_of(state: &crate::state::AppState, headers: &HeaderMap, path: &str) -> Vec<String> {
    let credential = [hyper::header::PROXY_AUTHORIZATION, hyper::header::AUTHORIZATION]
        .iter()
        .filter_map(|name| headers.get(name)?.to_str().ok())
        .find_map(|value| crate::host_list::credential_of(state, value))
        .map(|(stream_id, _)| stream_id);
    let named = path.strip_prefix("/s/").and_then(|rest| rest.split('/').next()).map(str::to_string);
    credential.into_iter().chain(named).collect()
}

/// Serve seats on `gateway` from now on, for `scope`'s containers only:
/// once per address, retrying while the address doesn't exist yet (Apple
/// container brings it up when the first seat attaches). Its own thread, so
/// no session's runtime owns it.
pub(crate) fn ensure(state: crate::state::AppState, gateway: Ipv4Addr, scope: String) {
    static LISTENING: OnceLock<Mutex<HashSet<Ipv4Addr>>> = OnceLock::new();
    scopes().lock().unwrap().insert(gateway, scope);
    let listening = LISTENING.get_or_init(Default::default);
    if !listening.lock().unwrap().insert(gateway) {
        return;
    }
    std::thread::spawn(move || {
        let Ok(runtime) = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
        else {
            listening.lock().unwrap().remove(&gateway);
            return;
        };
        runtime.block_on(async {
            let addr = SocketAddr::from((gateway, PORT));
            let deadline = Instant::now() + Duration::from_secs(60);
            let listener = loop {
                match tokio::net::TcpListener::bind(addr).await {
                    Ok(listener) => break listener,
                    Err(_) if Instant::now() < deadline => {
                        tokio::time::sleep(Duration::from_millis(200)).await
                    }
                    Err(e) => {
                        eprintln!("seat gateway: cannot listen on {addr}: {e}");
                        listening.lock().unwrap().remove(&gateway);
                        return;
                    }
                }
            };
            // A container's git push and gh, held for the operator as a
            // room's are at its door.
            let asks = Router::new()
                .route("/remote/ask", post(crate::remote_ask::ask_at_gateway))
                .route(
                    "/s/{stream_id}/to/{*rest}",
                    axum::routing::any(crate::swap::gateway_to_host),
                )
                .with_state(state.clone());
            let refused = "this door is another project's".to_string();
            let guard = {
                let (state, refused) = (state.clone(), refused.clone());
                move |req: axum::extract::Request, next: axum::middleware::Next| {
                    let (state, refused) = (state.clone(), refused.clone());
                    async move {
                        let callers = callers_of(&state, req.headers(), req.uri().path());
                        if callers.iter().any(|thread| !in_scope(&state, gateway, thread)) {
                            return (StatusCode::FORBIDDEN, refused).into_response();
                        }
                        next.run(req).await
                    }
                }
            };
            let allow: Allow = Arc::new(move |authority, headers| {
                if callers_of(&state, headers, "").iter().any(|thread| !in_scope(&state, gateway, thread)) {
                    return Err((StatusCode::FORBIDDEN, refused.clone()));
                }
                crate::host_list::verdict(&state, authority, headers)
            });
            let routes = mcp_route(node_url()).merge(asks).layer(axum::middleware::from_fn(guard));
            serve(listener, allow, routes, true, false, None).await;
        });
    });
}

/// Where the model proxy's upstream calls leave this Mac: CONNECT on
/// 127.0.0.1 only, to the hosts `model_proxy::egress_allows` names. Once per
/// process, on its own thread like the seats' gateway.
pub(crate) fn ensure_egress(state: crate::state::AppState) {
    static STARTED: OnceLock<()> = OnceLock::new();
    if STARTED.set(()).is_err() {
        return;
    }
    std::thread::spawn(move || {
        let Ok(runtime) = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
        else {
            return;
        };
        runtime.block_on(async move {
            let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, crate::model_proxy::egress_port()));
            let listener = match tokio::net::TcpListener::bind(addr).await {
                Ok(listener) => listener,
                Err(e) => {
                    eprintln!("model proxy egress: cannot listen on {addr}: {e}");
                    return;
                }
            };
            let allow: Allow = Arc::new(move |authority, _| {
                crate::model_proxy::egress_allows(&state, authority)
                    .then_some(())
                    .ok_or_else(|| {
                        (
                            StatusCode::FORBIDDEN,
                            format!("{authority} is not one of the model hosts"),
                        )
                    })
            });
            serve(listener, allow, Router::new(), false, false, None).await;
        });
    });
}

/// CONNECT tunnels to what `allow` names (with `public_only`, never to a
/// private address), or answered by `intercept` where it wants them; with
/// `plain_http`, plain-HTTP requests through the proxy too, judged the same
/// ([`forward`]); and every other request to `mcp`'s routes.
pub(crate) async fn serve(
    listener: tokio::net::TcpListener,
    allow: Allow,
    mcp: Router,
    public_only: bool,
    plain_http: bool,
    intercept: Option<Intercept>,
) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            continue;
        };
        let (mcp, allow, intercept) = (mcp.clone(), allow.clone(), intercept.clone());
        tokio::spawn(async move {
            let service = hyper::service::service_fn(move |req: Request<Incoming>| {
                let (mcp, allow, intercept) = (mcp.clone(), allow.clone(), intercept.clone());
                async move {
                    if req.method() == Method::CONNECT {
                        Ok(tunnel(req, allow, public_only, intercept).await)
                    } else if plain_http && req.uri().authority().is_some() {
                        Ok(forward(req, allow, public_only).await)
                    } else {
                        mcp.oneshot(req.map(Body::new)).await
                    }
                }
            });
            let _ = hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(stream), service)
                .with_upgrades()
                .await;
        });
    }
}

/// Whether `allow` lets the request with `headers` reach `authority`; the
/// refusal to send back if not. Deciding may read the node's records, so it
/// runs off the async workers.
async fn judged(allow: Allow, authority: &str, headers: &HeaderMap) -> Option<Response> {
    let (asked, headers) = (authority.to_string(), headers.clone());
    match tokio::task::spawn_blocking(move || allow(&asked, &headers)).await {
        Ok(Ok(())) => None,
        // A client that sends its credential only when asked gets asked.
        Ok(Err((StatusCode::PROXY_AUTHENTICATION_REQUIRED, why))) => {
            let ask = [(PROXY_AUTHENTICATE, "Basic realm=\"Bakery\"")];
            Some((StatusCode::PROXY_AUTHENTICATION_REQUIRED, ask, why).into_response())
        }
        Ok(Err((status, why))) => Some((status, why).into_response()),
        Err(e) => Some((StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()),
    }
}

/// `CONNECT host:port`: a byte tunnel to an allowed host, or the
/// connection answered by `intercept` when it wants the host; refused
/// otherwise.
async fn tunnel(req: Request<Incoming>, allow: Allow, public_only: bool, intercept: Option<Intercept>) -> Response {
    let Some(authority) = req.uri().authority().map(ToString::to_string) else {
        return (StatusCode::BAD_REQUEST, "CONNECT needs host:port").into_response();
    };
    if let Some(refused) = judged(allow, &authority, req.headers()).await {
        return refused;
    }
    if let Some(intercept) = intercept {
        let (wants, asked) = (intercept.wants.clone(), authority.clone());
        if tokio::task::spawn_blocking(move || wants(&asked)).await.unwrap_or(false) {
            tokio::spawn(async move {
                if let Ok(upgraded) = hyper::upgrade::on(req).await {
                    (intercept.serve)(authority, upgraded).await;
                }
            });
            return Response::new(Body::empty());
        }
    }
    let addrs = match addresses(&authority, public_only).await {
        Ok(addrs) => addrs,
        Err(why) => return (StatusCode::BAD_GATEWAY, why).into_response(),
    };
    tokio::spawn(async move {
        let Ok(upgraded) = hyper::upgrade::on(req).await else {
            return;
        };
        if let Some(mut upstream) = connect(&addrs).await {
            let _ = tokio::io::copy_bidirectional(&mut TokioIo::new(upgraded), &mut upstream).await;
        }
    });
    Response::new(Body::empty())
}

/// A plain-HTTP request through the proxy (`GET http://host/path`, as a
/// browser sends one): judged as a CONNECT to its host and port would be,
/// then sent on as an ordinary request and its answer brought back.
async fn forward(mut req: Request<Incoming>, allow: Allow, public_only: bool) -> Response {
    let uri = req.uri().clone();
    let (Some("http"), Some(host)) = (uri.scheme_str(), uri.host()) else {
        return (StatusCode::BAD_REQUEST, "only an http:// request goes through as it is; https:// takes a CONNECT").into_response();
    };
    let authority = format!("{host}:{}", uri.port_u16().unwrap_or(80));
    if let Some(refused) = judged(allow, &authority, req.headers()).await {
        return refused;
    }
    let addrs = match addresses(&authority, public_only).await {
        Ok(addrs) => addrs,
        Err(why) => return (StatusCode::BAD_GATEWAY, why).into_response(),
    };
    let Some(upstream) = connect(&addrs).await else {
        return (StatusCode::BAD_GATEWAY, format!("{authority} didn't answer")).into_response();
    };
    let (mut sender, conn) = match hyper::client::conn::http1::handshake(TokioIo::new(upstream)).await {
        Ok(pair) => pair,
        Err(e) => return (StatusCode::BAD_GATEWAY, e.to_string()).into_response(),
    };
    tokio::spawn(conn);
    // On to the host as its own request: its path, its Host, and nothing
    // meant for the proxy or this one connection.
    *req.uri_mut() = uri
        .path_and_query()
        .map_or("/", |p| p.as_str())
        .parse()
        .unwrap_or_default();
    let headers = req.headers_mut();
    for hop in [PROXY_AUTHORIZATION, CONNECTION, TE, TRAILER, UPGRADE] {
        headers.remove(hop);
    }
    for hop in ["proxy-connection", "keep-alive"] {
        headers.remove(hop);
    }
    if let Some(host) = uri.authority().and_then(|a| HeaderValue::from_str(a.as_str()).ok()) {
        headers.insert(HOST, host);
    }
    match sender.send_request(req).await {
        Ok(answer) => answer.map(Body::new).into_response(),
        Err(e) => (StatusCode::BAD_GATEWAY, e.to_string()).into_response(),
    }
}

/// Where `authority` leads, IPv4 first: trying a host's IPv6 address first
/// stalls every tunnel on a Mac whose IPv6 goes nowhere. With
/// `public_only`, public addresses alone, so no name a seat may reach leads
/// it to this Mac, its network or the tailnet.
async fn addresses(authority: &str, public_only: bool) -> Result<Vec<SocketAddr>, String> {
    let mut addrs: Vec<SocketAddr> = tokio::net::lookup_host(authority)
        .await
        .map_err(|_| format!("{authority} could not be resolved"))?
        .collect();
    addrs.sort_by_key(SocketAddr::is_ipv6);
    if public_only {
        addrs.retain(|a| public(a.ip()));
    }
    if addrs.is_empty() {
        return Err(format!("{authority} has no address a seat may reach"));
    }
    Ok(addrs)
}

/// Whether an address lies outside this Mac, its local networks and the
/// tailnet (100.64.0.0/10 and its IPv6 range, a unique local one).
pub(crate) fn public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            !(a == 0
                || v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_multicast()
                || (a == 100 && (64..128).contains(&b)))
        }
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => public(IpAddr::V4(v4)),
            None => {
                let first = v6.segments()[0];
                !(v6.is_loopback()
                    || v6.is_unspecified()
                    || v6.is_multicast()
                    || (first & 0xfe00) == 0xfc00
                    || (first & 0xffc0) == 0xfe80)
            }
        },
    }
}

/// Each address in turn, with a time limit.
async fn connect(addrs: &[SocketAddr]) -> Option<tokio::net::TcpStream> {
    for &addr in addrs {
        let attempt = tokio::net::TcpStream::connect(addr);
        if let Ok(Ok(stream)) = tokio::time::timeout(Duration::from_secs(5), attempt).await {
            return Some(stream);
        }
    }
    None
}

/// The seats' `POST /mcp`, answered against the node at `node`.
fn mcp_route(node: String) -> Router {
    Router::new().route(
        "/mcp",
        post(move |headers: HeaderMap, body: String| mcp(node.clone(), headers, body)),
    )
}

/// `POST /mcp`: one JSON-RPC message, answered by the stdio shim's own
/// handler against the node, with the session's token.
async fn mcp(node: String, headers: HeaderMap, body: String) -> Response {
    let token = headers
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::to_string);
    let Some(token) = token else {
        return (StatusCode::UNAUTHORIZED, "a session token is required").into_response();
    };
    let Ok(request) = serde_json::from_str::<Value>(&body) else {
        return (StatusCode::BAD_REQUEST, "not JSON").into_response();
    };
    let reply = tokio::task::spawn_blocking(move || {
        // The node itself, long past the Mac's check of a new process.
        let first = std::sync::Once::new();
        first.call_once(|| {});
        let client = crate::mcp_shim::HttpNodeClient {
            base_url: node,
            token,
            first,
        };
        crate::mcp_shim::handle_request(&client, &request)
    })
    .await;
    match reply {
        Ok(Some(answer)) => Json(answer).into_response(),
        // A notification gets no answer.
        Ok(None) => StatusCode::ACCEPTED.into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::header::PROXY_AUTHORIZATION;
    use std::io::{Read, Write};

    /// A project's door serves its own project's threads only: one named by
    /// its credential or by a sign-in path from another project is refused
    /// there, whatever reached the door.
    #[test]
    fn a_door_serves_only_its_own_projects_threads() {
        let dir = tempfile::tempdir().unwrap();
        let state = crate::testutil::state_with(dir.path(), std::sync::Arc::new(bakery_agent::StubModel));
        let mut threads = Vec::new();
        for name in ["farm", "shop"] {
            let folder = dir.path().join(name);
            std::fs::create_dir_all(&folder).unwrap();
            let root = crate::catalog::DirectoryRootSpec::Attach(folder.to_str().unwrap());
            threads.push(crate::catalog::create_project(&state, name, Some(root)).unwrap().1.stream_id);
        }
        let gateway = Ipv4Addr::new(192, 0, 2, 1);
        scopes().lock().unwrap().insert(gateway, crate::seat_container::scope_of(&state, &threads[0]));
        assert!(in_scope(&state, gateway, &threads[0]));
        assert!(!in_scope(&state, gateway, &threads[1]), "another project's thread");
        assert!(!in_scope(&state, Ipv4Addr::new(192, 0, 2, 2), &threads[0]), "a door it was never given");

        let (name, signed) = crate::host_list::credential(&state, &threads[1], crate::host_list::Kind::Container);
        let mut headers = HeaderMap::new();
        let basic = bakery_agent::tool_wire::encode_base64(format!("{name}:{signed}").as_bytes());
        headers.insert(PROXY_AUTHORIZATION, HeaderValue::from_str(&format!("Basic {basic}")).unwrap());
        assert_eq!(callers_of(&state, &headers, ""), [threads[1].clone()]);
        let path = format!("/s/{}/to/api.anthropic.com/v1/messages", threads[1]);
        assert_eq!(callers_of(&state, &HeaderMap::new(), &path), [threads[1].clone()]);
        assert!(callers_of(&state, &HeaderMap::new(), "/mcp").is_empty(), "its own route checks it");
        // Its own credential, another project's thread in the path: both
        // count, so the door at its own project refuses it.
        let (name, signed) = crate::host_list::credential(&state, &threads[0], crate::host_list::Kind::Container);
        let basic = bakery_agent::tool_wire::encode_base64(format!("{name}:{signed}").as_bytes());
        let mut own = HeaderMap::new();
        own.insert(PROXY_AUTHORIZATION, HeaderValue::from_str(&format!("Basic {basic}")).unwrap());
        let callers = callers_of(&state, &own, &path);
        assert_eq!(callers, [threads[0].clone(), threads[1].clone()]);
        assert!(callers.iter().any(|t| !in_scope(&state, gateway, t)), "refused");
    }

    /// The operator's additions count at once, beside the built-in list.
    #[test]
    fn the_operators_hosts_count_beside_the_built_in_ones() {
        let dir = tempfile::tempdir().unwrap();
        let state =
            crate::testutil::state_with(dir.path(), std::sync::Arc::new(bakery_agent::StubModel));
        // A thread of no project: the defaults and the operator's additions.
        let (name, signed) =
            crate::host_list::credential(&state, "wt:plain", crate::host_list::Kind::Container);
        let basic = bakery_agent::tool_wire::encode_base64(format!("{name}:{signed}").as_bytes());
        let mut headers = HeaderMap::new();
        headers.insert(
            PROXY_AUTHORIZATION,
            format!("Basic {basic}").parse().unwrap(),
        );
        let seat_may_reach = |state: &crate::state::AppState, authority: &str| {
            crate::host_list::verdict(state, authority, &headers).is_ok()
        };
        let anonymous = crate::host_list::verdict(&state, "index.crates.io:443", &HeaderMap::new());
        assert_eq!(
            anonymous.unwrap_err().0,
            StatusCode::PROXY_AUTHENTICATION_REQUIRED
        );
        assert!(seat_may_reach(&state, "index.crates.io:443"));
        assert!(!seat_may_reach(&state, "registry.example.com:443"));
        state
            .node
            .lock()
            .unwrap()
            .machine_access
            .allow_hosts
            .push(".example.com".to_string());
        assert!(seat_may_reach(&state, "registry.example.com:443"));
        assert!(!seat_may_reach(&state, "registry.example.com:80"));
        assert!(!seat_may_reach(&state, "example.org:443"));
    }

    #[test]
    fn only_listed_hosts_on_the_tls_port() {
        let list = ["crates.io", ".githubusercontent.com"];
        assert!(allowed("crates.io:443", &list, 443));
        assert!(allowed("Crates.IO:443", &list, 443));
        assert!(allowed("objects.githubusercontent.com:443", &list, 443));
        for refused in [
            "crates.io:80",
            "evil.crates.io:443",
            "githubusercontent.com:443",
            "evilgithubusercontent.com:443",
            "example.com:443",
            "[::1]:443",
            "crates.io",
        ] {
            assert!(!allowed(refused, &list, 443), "{refused}");
        }
    }

    #[test]
    fn a_seat_never_reaches_this_mac_its_network_or_the_tailnet() {
        for private in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.10",
            "169.254.1.1",
            "100.100.1.1",
            "0.0.0.0",
            "::1",
            "fd7a:115c:a1e0::1",
            "fe80::1",
            "::ffff:192.168.1.10",
        ] {
            assert!(!public(private.parse().unwrap()), "{private}");
        }
        for open in ["140.82.112.3", "100.128.0.1", "2606:4700::1111"] {
            assert!(public(open.parse().unwrap()), "{open}");
        }
    }

    /// A tunnel to an allowed host carries bytes both ways; any other host
    /// is refused before anything connects, and so is a private address
    /// when the gateway serves seats; `/mcp` wants a token. (The client
    /// blocks a thread, so the server needs a worker of its own.)
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_gateway_tunnels_only_to_allowed_hosts_and_guards_mcp() {
        let echo = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let echo_port = echo.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut conn, _)) = echo.accept() {
                let mut buf = [0u8; 4];
                if conn.read_exact(&mut buf).is_ok() {
                    let _ = conn.write_all(&buf);
                }
            }
        });
        let allow: Allow = Arc::new(move |authority, headers: &HeaderMap| {
            if !headers.contains_key(PROXY_AUTHORIZATION) {
                let why = "a thread's credential is needed".to_string();
                return Err((StatusCode::PROXY_AUTHENTICATION_REQUIRED, why));
            }
            allowed(authority, &["127.0.0.1"], echo_port)
                .then_some(())
                .ok_or_else(|| {
                    (
                        StatusCode::FORBIDDEN,
                        format!("{authority} is not one of the hosts seats may reach"),
                    )
                })
        });
        let gateway = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let at = gateway.local_addr().unwrap();
        let seats = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let seats_at = seats.local_addr().unwrap();
        let mcp = mcp_route("http://127.0.0.1:9".to_string());
        tokio::spawn(serve(gateway, allow.clone(), mcp.clone(), false, false, None));
        tokio::spawn(serve(seats, allow, mcp, true, false, None));
        crate::testutil::first_connection(at);
        crate::testutil::first_connection(seats_at);

        // A fresh binary's first loopback call waits on LuLu, longer under a
        // full suite's load: a long limit.
        let talk_to = move |at: SocketAddr, request: String, then: &'static [u8]| {
            std::thread::spawn(move || {
                let mut conn = std::net::TcpStream::connect(at).unwrap();
                conn.set_read_timeout(Some(Duration::from_secs(120)))
                    .unwrap();
                conn.write_all(request.as_bytes()).unwrap();
                let mut head = vec![0u8; 512];
                let n = conn.read(&mut head).unwrap();
                let mut out = String::from_utf8_lossy(&head[..n]).into_owned();
                if !then.is_empty() && out.starts_with("HTTP/1.1 200") {
                    conn.write_all(then).unwrap();
                    let mut echoed = [0u8; 4];
                    conn.read_exact(&mut echoed).unwrap();
                    out.push_str(&String::from_utf8_lossy(&echoed));
                }
                out
            })
            .join()
            .unwrap()
        };
        let talk = move |request: String, then: &'static [u8]| talk_to(at, request, then);

        let open = talk(
            format!(
                "CONNECT 127.0.0.1:{echo_port} HTTP/1.1\r\nHost: 127.0.0.1:{echo_port}\r\nProxy-Authorization: Basic eDp5\r\n\r\n"
            ),
            b"ping",
        );
        assert!(
            open.starts_with("HTTP/1.1 200") && open.ends_with("ping"),
            "{open}"
        );
        let refused = talk(
            "CONNECT example.com:443 HTTP/1.1\r\nHost: example.com:443\r\nProxy-Authorization: Basic eDp5\r\n\r\n"
                .to_string(),
            b"",
        );
        assert!(refused.starts_with("HTTP/1.1 403"), "{refused}");
        let anonymous = talk(
            format!(
                "CONNECT 127.0.0.1:{echo_port} HTTP/1.1\r\nHost: 127.0.0.1:{echo_port}\r\n\r\n"
            ),
            b"",
        );
        assert!(
            anonymous.starts_with("HTTP/1.1 407")
                && anonymous
                    .to_ascii_lowercase()
                    .contains("proxy-authenticate: basic realm=\"bakery\""),
            "a client that waits to be asked is asked: {anonymous}"
        );
        let private = talk_to(
            seats_at,
            format!(
                "CONNECT 127.0.0.1:{echo_port} HTTP/1.1\r\nHost: 127.0.0.1:{echo_port}\r\nProxy-Authorization: Basic eDp5\r\n\r\n"
            ),
            b"ping",
        );
        assert!(private.starts_with("HTTP/1.1 502"), "{private}");

        let body = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
        let post = |auth: &str| {
            format!(
                "POST /mcp HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\n{auth}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
        };
        let anonymous = talk(post(""), b"");
        assert!(anonymous.starts_with("HTTP/1.1 401"), "{anonymous}");
        let answered = talk(post("Authorization: Bearer t\r\n"), b"");
        assert!(
            answered.starts_with("HTTP/1.1 200") && answered.contains("bakery-mcp-shim"),
            "{answered}"
        );
    }

    /// With plain HTTP on (a door), a request through the proxy goes on as
    /// the host's own request, judged like a tunnel: its path and Host, none
    /// of the proxy's headers, and the host's answer brought back.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_door_forwards_plain_http_to_allowed_hosts_only() {
        let site = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let site_port = site.local_addr().unwrap().port();
        let (seen_tx, seen) = std::sync::mpsc::channel::<String>();
        std::thread::spawn(move || {
            for mut conn in site.incoming().flatten() {
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") && conn.read(&mut byte).unwrap_or(0) == 1 {
                    head.push(byte[0]);
                }
                let _ = seen_tx.send(String::from_utf8_lossy(&head).into_owned());
                let _ = conn.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello");
            }
        });
        let allow: Allow = Arc::new(move |authority, _: &HeaderMap| {
            allowed(authority, &["127.0.0.1"], site_port)
                .then_some(())
                .ok_or_else(|| (StatusCode::FORBIDDEN, format!("{authority} is not on the list")))
        });
        let door = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let at = door.local_addr().unwrap();
        tokio::spawn(serve(door, allow, Router::new(), false, true, None));
        crate::testutil::first_connection(at);

        let ask = move |request: String| {
            std::thread::spawn(move || {
                // A fresh binary's first loopback call can wait on LuLu.
                let mut conn = std::net::TcpStream::connect(at).unwrap();
                conn.set_read_timeout(Some(Duration::from_secs(120))).unwrap();
                conn.write_all(request.as_bytes()).unwrap();
                let mut answer = String::new();
                let _ = conn.read_to_string(&mut answer);
                answer
            })
            .join()
            .unwrap()
        };
        let answer = ask(format!(
            "GET http://127.0.0.1:{site_port}/page?x=1 HTTP/1.1\r\nHost: 127.0.0.1:{site_port}\r\nProxy-Authorization: Basic eDp5\r\nProxy-Connection: keep-alive\r\nConnection: close\r\n\r\n"
        ));
        assert!(answer.starts_with("HTTP/1.1 200") && answer.ends_with("hello"), "{answer}");
        let head = seen.recv_timeout(Duration::from_secs(120)).unwrap();
        assert!(head.starts_with("GET /page?x=1 HTTP/1.1\r\n"), "{head}");
        let head = head.to_ascii_lowercase();
        assert!(head.contains(&format!("host: 127.0.0.1:{site_port}")), "{head}");
        assert!(!head.contains("proxy-authorization") && !head.contains("proxy-connection"), "{head}");

        let refused = ask("GET http://example.com/ HTTP/1.1\r\nHost: example.com\r\nConnection: close\r\n\r\n".to_string());
        assert!(refused.starts_with("HTTP/1.1 403"), "{refused}");
        assert!(seen.try_recv().is_err(), "nothing reached past the door");
    }

    /// A real seat on the internal network: the gateway opens a tunnel to an
    /// allowed host and refuses any other, the seat has no route of its own,
    /// and the node's `/mcp` answers there. It checks the gateway's own
    /// answers (`http_connect`), not the far end: on this Mac, LuLu holds a
    /// fresh test binary's outbound connections until the operator allows it.
    ///   cargo test -p bakery-node seat_gateway::tests::live -- --ignored
    #[test]
    #[ignore = "needs Apple container and the bakery/workspace:dev image"]
    fn live_a_seat_reaches_allowed_hosts_and_mcp_only_through_the_gateway() {
        let cli = std::path::Path::new("/opt/homebrew/bin/container");
        let labels = ["com.bakery.w2=s4".to_string()];
        let gateway = bakery_exec::ensure_seat_network(cli, "w2-seats", &labels).unwrap();
        let dir = tempfile::tempdir().unwrap();
        ensure(
            crate::testutil::state_with(dir.path(), std::sync::Arc::new(bakery_agent::StubModel)),
            gateway,
            "thread:wt:live".to_string(),
        );
        let proxy = format!("http://{gateway}:{PORT}");
        let probe = format!(
            r#"connect() {{ curl -s -o /dev/null -w '%{{http_connect}}' --max-time 3 -x {proxy} "$@"; }}
for i in $(seq 1 50); do curl -s -o /dev/null --max-time 1 {proxy}/mcp && break; sleep 0.2; done
printf 'allowed=%s ' "$(connect https://index.crates.io/config.json)"
printf 'other=%s ' "$(connect https://example.com/)"
printf 'direct=%s ' "$(curl -s -o /dev/null -w '%{{http_code}}' --max-time 3 https://index.crates.io/config.json)"
printf 'mcp=%s' "$(curl -s --max-time 8 -X POST {proxy}/mcp -H 'Authorization: Bearer t' -d '{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{}}}}' | grep -o bakery-mcp-shim)""#
        );
        let out = std::process::Command::new(cli)
            .args([
                "run",
                "--rm",
                "--network",
                "w2-seats",
                "--label",
                &labels[0],
            ])
            .args(["bakery/workspace:dev", "sh", "-c", &probe])
            .output()
            .unwrap();
        let said = String::from_utf8_lossy(&out.stdout);
        assert_eq!(
            said.trim(),
            "allowed=200 other=403 direct=000 mcp=bakery-mcp-shim",
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
