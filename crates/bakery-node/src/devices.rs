//! Paired devices: the operator's phones (and, later, other nodes) reach this
//! node over Tailscale, each with a token of its own.
//!
//! - Pairing: the operator shows a one-time code (8 characters, 5 minutes,
//!   one use, never on disk); a device trades it at `POST /pair` for its own
//!   token. The node keeps only the token's blake3 hash, in
//!   `~/.bakery/devices.json`, which no agent can read; the device keeps the
//!   token in its own secure store.
//! - The tailnet listener: HTTPS with Tailscale's certificate, on this node's
//!   tailnet address only, serving the same routes behind [`gate`]: a device
//!   token, sent from the tailnet machine that device paired from, on a route
//!   whose override opens it to that kind of device (`remote`). The operator
//!   token is never accepted there, and `POST /pair` is the only route
//!   without a token.
//! - Revoking drops the hash and closes that machine's connections at once.

use crate::api_catalog::{self, Method as RouteMethod};
use crate::state::{append, AppState};
use axum::{
    extract::{MatchedPath, Request, State},
    http::{header::AUTHORIZATION, HeaderValue, Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Extension, Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tower_http::cors::{AllowOrigin, Any, CorsLayer};

const STREAM: &str = "sys:devices";
const CODE_TTL: Duration = Duration::from_secs(5 * 60);
const CODE_TRIES: u32 = 5;
const DEFAULT_PORT: u16 = 7718;

#[derive(schemars::JsonSchema, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum DeviceKind {
    Phone,
    Node,
}

impl DeviceKind {
    fn as_str(self) -> &'static str {
        match self {
            DeviceKind::Phone => "phone",
            DeviceKind::Node => "node",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Paired {
    device_id: String,
    kind: DeviceKind,
    name: String,
    paired_at: String,
    /// The tailnet machine it paired from (Tailscale's stable node id);
    /// every request must come from it.
    tailnet_node: String,
    token_hash: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Registry {
    /// The operator let this node be reached over the tailnet.
    #[serde(default)]
    remote: bool,
    #[serde(default)]
    devices: Vec<Paired>,
}

// ─── The registry ───────────────────────────────────────────────────────────

/// One change to the registry at a time.
fn registry_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(Default::default)
}

fn load(state: &AppState) -> Registry {
    std::fs::read_to_string(state.base.join("devices.json"))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Written whole, 0600, then renamed into place.
fn save(state: &AppState, registry: &Registry) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let text = serde_json::to_string_pretty(registry).map_err(|e| e.to_string())?;
    let tmp = state.base.join(format!("devices.json.{}", crate::node::mint_id("tmp")));
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)
        .and_then(|mut f| f.write_all(text.as_bytes()))
        .and_then(|_| std::fs::rename(&tmp, state.base.join("devices.json")))
        .map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("could not save the paired devices: {e}")
        })
}

fn hash(token: &str) -> blake3::Hash {
    blake3::hash(token.as_bytes())
}

/// The paired device a token belongs to.
fn find(state: &AppState, token: &str) -> Option<Paired> {
    let presented = hash(token);
    // blake3::Hash compares in constant time.
    load(state)
        .devices
        .into_iter()
        .find(|d| blake3::Hash::from_hex(&d.token_hash).is_ok_and(|h| h == presented))
}

fn last_seen() -> &'static Mutex<HashMap<String, String>> {
    static SEEN: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    SEEN.get_or_init(Default::default)
}

// ─── The one-time code ──────────────────────────────────────────────────────

struct Code {
    hash: blake3::Hash,
    expires: Instant,
    misses: u32,
}

fn live_code() -> &'static Mutex<Option<Code>> {
    static CODE: OnceLock<Mutex<Option<Code>>> = OnceLock::new();
    CODE.get_or_init(Default::default)
}

fn random<const N: usize>() -> [u8; N] {
    use std::io::Read;
    let mut buf = [0u8; N];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut buf))
        .expect("/dev/urandom must be readable");
    buf
}

/// Crockford's base32: no I, L, O or U to misread.
const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// As typed: case, dashes and the look-alike letters don't matter.
fn normalize(code: &str) -> String {
    code.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| match c.to_ascii_uppercase() {
            'O' => '0',
            'I' | 'L' => '1',
            c => c,
        })
        .collect()
}

/// A new code, replacing any other, shown as `ABCD-EFGH`.
fn mint_code() -> (String, Instant) {
    let mut bytes = [0u8; 8];
    bytes[3..].copy_from_slice(&random::<5>());
    let bits = u64::from_be_bytes(bytes);
    let code: String = (0..8).rev().map(|i| ALPHABET[((bits >> (i * 5)) & 31) as usize] as char).collect();
    let expires = Instant::now() + CODE_TTL;
    *live_code().lock().unwrap() = Some(Code { hash: hash(&code), expires, misses: 0 });
    (format!("{}-{}", &code[..4], &code[4..]), expires)
}

/// Uses up the live code when `given` is it; a wrong one counts against it.
fn take_code(given: &str) -> Result<(), String> {
    let mut slot = live_code().lock().unwrap();
    let Some(code) = slot.as_mut().filter(|c| c.expires > Instant::now()) else {
        *slot = None;
        return Err("no pairing code is waiting: show one in Bakery (Settings, Machines)".to_string());
    };
    if hash(&normalize(given)) == code.hash {
        *slot = None;
        return Ok(());
    }
    code.misses += 1;
    if code.misses >= CODE_TRIES {
        *slot = None;
        return Err("too many wrong codes: show a new one in Bakery".to_string());
    }
    Err("wrong pairing code".to_string())
}

// ─── Tailscale ──────────────────────────────────────────────────────────────

/// The Tailscale CLI: on PATH, or inside the Mac app.
pub(crate) fn tailscale() -> Option<PathBuf> {
    crate::state::resolve_executable_path("tailscale")
        .ok()
        .or_else(|| Some(PathBuf::from("/Applications/Tailscale.app/Contents/MacOS/Tailscale")).filter(|p| p.is_file()))
}

#[derive(Debug, PartialEq)]
struct Tailnet {
    dns_name: String,
    ipv4: Ipv4Addr,
}

/// This node on the tailnet, from `tailscale status --json`, or exactly what
/// the operator has to turn on first.
fn tailnet_from_status(status: &Value) -> Result<Tailnet, String> {
    if status["BackendState"] != "Running" {
        return Err("Tailscale isn't connected on this Mac: open Tailscale and sign in.".to_string());
    }
    if status["CurrentTailnet"]["MagicDNSEnabled"] != true {
        return Err("Turn on MagicDNS in the Tailscale admin console (DNS page), so phones and machines can reach this Mac by name.".to_string());
    }
    if status["CertDomains"].as_array().is_none_or(|d| d.is_empty()) {
        return Err("Turn on HTTPS Certificates in the Tailscale admin console (DNS page), so this Mac can serve phones and machines over TLS.".to_string());
    }
    let dns_name = status["Self"]["DNSName"].as_str().unwrap_or_default().trim_end_matches('.').to_string();
    let ipv4 = status["Self"]["TailscaleIPs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .find_map(|ip| ip.parse::<Ipv4Addr>().ok());
    match (dns_name.is_empty(), ipv4) {
        (false, Some(ipv4)) => Ok(Tailnet { dns_name, ipv4 }),
        _ => Err("Tailscale hasn't given this Mac a name and an IPv4 address yet: check that it's signed in.".to_string()),
    }
}

/// The tailnet, and a certificate for this node's name in `~/.bakery/tls`.
fn prepare(state: &AppState) -> Result<(PathBuf, Tailnet, PathBuf, PathBuf), String> {
    let cli = tailscale().ok_or("Install Tailscale (its app, or the tailscale command line) to reach this Mac from phones and machines.")?;
    let out = std::process::Command::new(&cli)
        .args(["status", "--json"])
        .output()
        .map_err(|e| format!("could not run tailscale: {e}"))?;
    let status: Value = serde_json::from_slice(&out.stdout).map_err(|_| {
        format!("tailscale status failed: {}", String::from_utf8_lossy(&out.stderr).trim())
    })?;
    let tailnet = tailnet_from_status(&status)?;
    let dir = state.base.join("tls");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let (cert, key) = (dir.join("tailnet.crt"), dir.join("tailnet.key"));
    let out = std::process::Command::new(&cli)
        .arg("cert")
        .arg("--cert-file")
        .arg(&cert)
        .arg("--key-file")
        .arg(&key)
        .arg(&tailnet.dns_name)
        .output()
        .map_err(|e| format!("could not run tailscale cert: {e}"))?;
    if !out.status.success() {
        return Err(format!("tailscale cert failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok((cli, tailnet, cert, key))
}

/// The tailnet machine at `ip`: Tailscale's stable node id.
async fn whois(cli: PathBuf, ip: IpAddr) -> Option<String> {
    let out = tokio::process::Command::new(&cli).args(["whois", "--json", &ip.to_string()]).output().await.ok()?;
    let who: Value = serde_json::from_slice(&out.stdout).ok()?;
    who["Node"]["StableID"].as_str().filter(|id| !id.is_empty()).map(str::to_string)
}

// ─── The tailnet listener ───────────────────────────────────────────────────

#[derive(schemars::JsonSchema, Debug, Clone, Default, Serialize)]
pub(crate) struct RemoteStatus {
    /// The operator let this node be reached over the tailnet.
    pub(crate) enabled: bool,
    /// Where phones and machines reach it, while it listens.
    pub(crate) url: Option<String>,
    /// Why it isn't listening, when it should be: what to turn on.
    pub(crate) problem: Option<String>,
}

#[derive(Default)]
struct Listener {
    status: RemoteStatus,
    stop: Option<tokio::sync::watch::Sender<bool>>,
}

fn listener() -> &'static Mutex<Listener> {
    static LISTENER: OnceLock<Mutex<Listener>> = OnceLock::new();
    LISTENER.get_or_init(Default::default)
}

/// Each tailnet machine's open connections, so a revoke can close them.
fn connections() -> &'static Mutex<HashMap<String, Vec<tokio::task::AbortHandle>>> {
    static OPEN: OnceLock<Mutex<HashMap<String, Vec<tokio::task::AbortHandle>>>> = OnceLock::new();
    OPEN.get_or_init(Default::default)
}

/// Which tailnet machine a connection came from (None when Tailscale
/// couldn't say, which no device's check passes).
#[derive(Clone)]
pub(crate) struct Peer {
    pub(crate) tailnet_node: Option<String>,
}

/// The routes, behind [`gate`] instead of the operator's auth.
pub(crate) fn remote_router(state: AppState) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::list(
            crate::state::allowed_origins().iter().filter_map(|o| o.parse::<HeaderValue>().ok()),
        ))
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
        .allow_headers(Any);
    crate::api_routes()
        .route_layer(axum::middleware::from_fn_with_state(state.clone(), gate))
        .layer(cors)
        .with_state(state)
}

fn tls(cert: &Path, key: &Path) -> Result<tokio_rustls::TlsAcceptor, String> {
    use rustls::pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer};
    let certs = CertificateDer::pem_file_iter(cert)
        .and_then(|certs| certs.collect::<Result<Vec<_>, _>>())
        .map_err(|e| format!("the tailnet certificate: {e}"))?;
    let key = PrivateKeyDer::from_pem_file(key).map_err(|e| format!("the tailnet key: {e}"))?;
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| format!("the tailnet certificate: {e}"))?;
    Ok(tokio_rustls::TlsAcceptor::from(Arc::new(config)))
}

/// Serves one TLS connection: who it is on the tailnet, then the routes.
async fn connection(tcp: tokio::net::TcpStream, peer: SocketAddr, acceptor: tokio_rustls::TlsAcceptor, app: Router, cli: PathBuf) {
    let Ok(stream) = acceptor.accept(tcp).await else { return };
    let tailnet_node = whois(cli, peer.ip()).await;
    let app = app.layer(Extension(Peer { tailnet_node: tailnet_node.clone() }));
    let serving = tokio::spawn(async move {
        let io = hyper_util::rt::TokioIo::new(stream);
        let service = hyper_util::service::TowerToHyperService::new(app);
        let _ = hyper::server::conn::http1::Builder::new().serve_connection(io, service).with_upgrades().await;
    });
    if let Some(node) = tailnet_node {
        let mut open = connections().lock().unwrap();
        let handles = open.entry(node).or_default();
        handles.retain(|h| !h.is_finished());
        handles.push(serving.abort_handle());
    }
    let _ = serving.await;
}

/// Listens on the tailnet when the operator turned it on, or says exactly
/// what is missing.
pub(crate) async fn start(state: AppState) -> RemoteStatus {
    stop();
    let prepared = {
        let state = state.clone();
        crate::actor::spawn_blocking(move || prepare(&state)).await.map_err(|e| e.to_string()).and_then(|r| r)
    };
    let status = match prepared.and_then(|(cli, tailnet, cert, key)| Ok((cli, tailnet, tls(&cert, &key)?))) {
        Err(problem) => RemoteStatus { enabled: true, url: None, problem: Some(problem) },
        Ok((cli, tailnet, acceptor)) => {
            let port = std::env::var("BAKERY_REMOTE_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(DEFAULT_PORT);
            match tokio::net::TcpListener::bind(SocketAddr::from((tailnet.ipv4, port))).await {
                Err(e) => RemoteStatus {
                    enabled: true,
                    url: None,
                    problem: Some(format!("could not listen on {}:{port}: {e}", tailnet.ipv4)),
                },
                Ok(tcp) => {
                    let (stop_tx, mut stop_rx) = tokio::sync::watch::channel(false);
                    let app = remote_router(state);
                    tokio::spawn(async move {
                        loop {
                            tokio::select! {
                                accepted = tcp.accept() => if let Ok((stream, peer)) = accepted {
                                    tokio::spawn(connection(stream, peer, acceptor.clone(), app.clone(), cli.clone()));
                                },
                                _ = stop_rx.changed() => break,
                            }
                        }
                    });
                    listener().lock().unwrap().stop = Some(stop_tx);
                    RemoteStatus { enabled: true, url: Some(format!("https://{}:{port}", tailnet.dns_name)), problem: None }
                }
            }
        }
    };
    listener().lock().unwrap().status = status.clone();
    status
}

/// Stops listening and closes every tailnet connection.
fn stop() {
    let mut l = listener().lock().unwrap();
    if let Some(stop) = l.stop.take() {
        let _ = stop.send(true);
    }
    l.status = RemoteStatus::default();
    for handles in connections().lock().unwrap().drain().map(|(_, h)| h) {
        handles.iter().for_each(|h| h.abort());
    }
}

/// At startup: listens when the operator turned it on before.
pub(crate) async fn start_on_boot(state: AppState) {
    if load(&state).remote {
        start(state).await;
    }
}

// ─── The gate ───────────────────────────────────────────────────────────────

fn bearer(req: &Request) -> Option<String> {
    req.headers()
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|v| v.trim().to_string())
}

/// Every tailnet request: `POST /pair` as it is; the event WebSocket behind
/// its own one-time ticket (which only a device token mints here); anything
/// else needs a paired device's token, from the tailnet machine it paired
/// from, on a route its kind may call. The operator token is no device's.
async fn gate(State(st): State<AppState>, req: Request, next: Next) -> Response {
    let path = req.extensions().get::<MatchedPath>().map(|p| p.as_str().to_string()).unwrap_or_default();
    let method = match *req.method() {
        Method::GET => RouteMethod::Get,
        Method::POST => RouteMethod::Post,
        _ => return StatusCode::METHOD_NOT_ALLOWED.into_response(),
    };
    if (method == RouteMethod::Post && path == "/pair") || (method == RouteMethod::Get && path == "/events") {
        return next.run(req).await;
    }
    let Some(device) = bearer(&req).and_then(|token| find(&st, &token)) else {
        return (StatusCode::UNAUTHORIZED, "this needs a paired device's token").into_response();
    };
    let from = req.extensions().get::<Peer>().and_then(|p| p.tailnet_node.as_deref());
    if from != Some(device.tailnet_node.as_str()) {
        return (StatusCode::FORBIDDEN, "not from the tailnet machine this device paired from").into_response();
    }
    if !api_catalog::remote_allows(method, &path, device.kind.as_str()) {
        let what = format!("{} {path} isn't open to a {}", method.as_str(), device.kind.as_str());
        return (StatusCode::FORBIDDEN, what).into_response();
    }
    last_seen().lock().unwrap().insert(device.device_id.clone(), chrono::Utc::now().to_rfc3339());
    crate::actor::scope(format!("device:{}", device.device_id), next.run(req)).await
}

// ─── Routes ─────────────────────────────────────────────────────────────────

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct PairReq {
    /// The code Bakery shows (Settings, Machines).
    pub(crate) code: String,
    /// What to call this device, e.g. "Sam's iPhone".
    pub(crate) name: String,
    pub(crate) kind: DeviceKind,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct PairResp {
    pub(crate) device_id: String,
    /// The device's own token: keep it in the device's secure store. The
    /// node never shows it again.
    pub(crate) token: String,
}

fn pair(state: &AppState, from: Option<String>, req: &PairReq) -> Result<PairResp, (StatusCode, String)> {
    let name: String = req.name.trim().chars().take(64).collect();
    if name.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "name this device".to_string()));
    }
    // Checked before the code, so a try from off the tailnet can't spend one.
    let tailnet_node = from.ok_or((StatusCode::FORBIDDEN, "pair over Tailscale: this connection isn't from a tailnet machine".to_string()))?;
    take_code(&req.code).map_err(|e| (StatusCode::FORBIDDEN, e))?;
    let token = random::<32>().iter().map(|b| format!("{b:02x}")).collect::<String>();
    let device = Paired {
        device_id: crate::node::mint_id("device"),
        kind: req.kind,
        name,
        paired_at: chrono::Utc::now().to_rfc3339(),
        tailnet_node,
        token_hash: hash(&token).to_hex().to_string(),
    };
    {
        let _one = registry_lock().lock().unwrap();
        let mut registry = load(state);
        registry.devices.push(device.clone());
        save(state, &registry).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    }
    let fact = serde_json::json!({"device_id": device.device_id, "kind": device.kind, "name": device.name});
    let _ = append(state, STREAM, "human", "device.paired", fact.to_string(), vec![]);
    Ok(PairResp { device_id: device.device_id, token })
}

pub(crate) async fn pair_route(
    State(st): State<AppState>,
    peer: Option<Extension<Peer>>,
    Json(req): Json<PairReq>,
) -> Result<Json<PairResp>, (StatusCode, String)> {
    let from = peer.and_then(|Extension(p)| p.tailnet_node);
    crate::actor::spawn_blocking(move || pair(&st, from, &req))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map(Json)
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct DeviceView {
    pub(crate) device_id: String,
    pub(crate) kind: DeviceKind,
    pub(crate) name: String,
    pub(crate) paired_at: String,
    /// Its last call since this node started.
    pub(crate) last_seen: Option<String>,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct Devices {
    pub(crate) remote: RemoteStatus,
    pub(crate) devices: Vec<DeviceView>,
}

fn devices(state: &AppState) -> Devices {
    let registry = load(state);
    let seen = last_seen().lock().unwrap();
    let mut remote = listener().lock().unwrap().status.clone();
    remote.enabled = registry.remote;
    Devices {
        remote,
        devices: registry
            .devices
            .into_iter()
            .map(|d| DeviceView {
                last_seen: seen.get(&d.device_id).cloned(),
                device_id: d.device_id,
                kind: d.kind,
                name: d.name,
                paired_at: d.paired_at,
            })
            .collect(),
    }
}

pub(crate) async fn devices_route(State(st): State<AppState>) -> Json<Devices> {
    Json(devices(&st))
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct PairCode {
    /// Type it into the device, or scan it from the QR code.
    pub(crate) code: String,
    pub(crate) expires_in_secs: u64,
    /// Where the device reaches this node.
    pub(crate) url: String,
}

pub(crate) async fn pair_code_route() -> Result<Json<PairCode>, (StatusCode, String)> {
    let status = listener().lock().unwrap().status.clone();
    let url = status.url.ok_or_else(|| {
        let why = status.problem.unwrap_or_else(|| "turn on \"Reachable from your other devices\" first".to_string());
        (StatusCode::CONFLICT, format!("nothing can pair yet: {why}"))
    })?;
    let (code, expires) = mint_code();
    Ok(Json(PairCode { code, expires_in_secs: expires.saturating_duration_since(Instant::now()).as_secs(), url }))
}

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct RevokeReq {
    pub(crate) device_id: String,
}

fn revoke(state: &AppState, device_id: &str) -> Result<Devices, String> {
    let gone = {
        let _one = registry_lock().lock().unwrap();
        let mut registry = load(state);
        let at = registry
            .devices
            .iter()
            .position(|d| d.device_id == device_id)
            .ok_or(format!("no paired device {device_id}"))?;
        let gone = registry.devices.remove(at);
        save(state, &registry)?;
        gone
    };
    // Its token stops working at once; so do the connections it has open.
    if let Some(handles) = connections().lock().unwrap().remove(&gone.tailnet_node) {
        handles.iter().for_each(|h| h.abort());
    }
    let fact = serde_json::json!({"device_id": device_id}).to_string();
    let _ = append(state, STREAM, "human", "device.revoked", fact, vec![]);
    Ok(devices(state))
}

pub(crate) async fn revoke_route(State(st): State<AppState>, Json(req): Json<RevokeReq>) -> Result<Json<Devices>, (StatusCode, String)> {
    crate::actor::spawn_blocking(move || revoke(&st, &req.device_id))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))
}

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct RemoteReq {
    /// Reachable from the operator's other devices over Tailscale.
    pub(crate) enabled: bool,
}

pub(crate) async fn remote_route(State(st): State<AppState>, Json(req): Json<RemoteReq>) -> Result<Json<RemoteStatus>, (StatusCode, String)> {
    {
        let _one = registry_lock().lock().unwrap();
        let mut registry = load(&st);
        registry.remote = req.enabled;
        save(&st, &registry).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    }
    if req.enabled {
        return Ok(Json(start(st).await));
    }
    stop();
    Ok(Json(RemoteStatus::default()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;
    use bakery_agent::StubModel;
    use tower::ServiceExt;

    fn pair_with(state: &AppState, code: &str, from: &str) -> Result<PairResp, (StatusCode, String)> {
        let req = PairReq { code: code.to_string(), name: "Sam's iPhone".into(), kind: DeviceKind::Phone };
        pair(state, Some(from.to_string()), &req)
    }

    /// A node has one pairing code at a time (a process-wide slot), so the
    /// tests that mint one take turns.
    static ONE_CODE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    #[test]
    fn a_code_pairs_once_and_the_node_keeps_only_the_tokens_hash() {
        let _turn = ONE_CODE.blocking_lock();
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let (code, _) = mint_code();
        let off_tailnet = PairReq { code: code.clone(), name: "x".into(), kind: DeviceKind::Phone };
        assert!(pair(&state, None, &off_tailnet).is_err(), "only over the tailnet");
        let paired = pair_with(&state, &code.to_lowercase(), "nPHONE").unwrap();
        let stored = std::fs::read_to_string(dir.path().join("devices.json")).unwrap();
        assert!(!stored.contains(&paired.token), "the token itself is never stored");
        assert!(stored.contains(&hash(&paired.token).to_hex().to_string()));
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir.path().join("devices.json")).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert!(pair_with(&state, &code, "nPHONE").is_err(), "a code pairs once");

        let (code, _) = mint_code();
        for _ in 0..CODE_TRIES {
            assert!(pair_with(&state, "WRONG-CODE", "nPHONE").is_err());
        }
        let err = pair_with(&state, &code, "nPHONE").unwrap_err().1;
        assert!(err.contains("no pairing code"), "five wrong tries spend the code: {err}");
        assert_eq!(load(&state).devices.len(), 1);
    }

    #[test]
    fn tailscale_says_exactly_what_to_turn_on() {
        let ok = serde_json::json!({
            "BackendState": "Running", "CurrentTailnet": {"MagicDNSEnabled": true}, "CertDomains": ["mac.tail1.ts.net"],
            "Self": {"DNSName": "mac.tail1.ts.net.", "TailscaleIPs": ["100.64.0.1", "fd7a::1"]},
        });
        assert_eq!(
            tailnet_from_status(&ok),
            Ok(Tailnet { dns_name: "mac.tail1.ts.net".into(), ipv4: Ipv4Addr::new(100, 64, 0, 1) })
        );
        let with = |key: &str, value: Value| {
            let mut s = ok.clone();
            s[key] = value;
            tailnet_from_status(&s).unwrap_err()
        };
        assert!(with("BackendState", "Stopped".into()).contains("sign in"));
        assert!(with("CurrentTailnet", serde_json::json!({"MagicDNSEnabled": false})).contains("Turn on MagicDNS"));
        assert!(with("CertDomains", serde_json::json!([])).contains("Turn on HTTPS Certificates"));
    }

    /// A phone's token answers only where the override opens the route.
    /// Every route is decided by `remote_allows`, with no request. Real
    /// requests cover one open route, one closed route, a missing token,
    /// the operator token, another tailnet machine, and a revoked token.
    #[tokio::test(flavor = "multi_thread")]
    async fn every_route_answers_a_device_only_where_its_override_opens_it() {
        let overrides: serde_json::Value =
            serde_json::from_str(include_str!("api_overrides.json")).expect("api_overrides.json");
        let mut seen = 0usize;
        let mut phone_may = None;
        let mut phone_may_not = None;
        for (route, _, _) in api_catalog::listing() {
            seen += 1;
            let key = format!("{} {}", route.method.as_str(), route.path);
            let named = overrides["routes"][&key]["remote"]
                .as_array()
                .is_some_and(|kinds| kinds.iter().any(|k| k.as_str() == Some("phone")));
            let open = api_catalog::remote_allows(route.method, route.path, "phone");
            assert_eq!(open, named, "{key} is open to a phone only where its override says so");
            let tokenless = (route.method == RouteMethod::Post && route.path == "/pair")
                || (route.method == RouteMethod::Get && route.path == "/events");
            if open && route.method == RouteMethod::Get && route.path == "/health" {
                phone_may = Some((route.method, route.path));
            }
            if !open && !tokenless && phone_may_not.is_none() {
                phone_may_not = Some((route.method, route.path));
            }
        }
        assert!(seen > 50, "the catalog walk covers the routes, not a sample: {seen}");
        let (open_method, open_path) = phone_may.expect("GET /health is open to phones");
        let (closed_method, closed_path) = phone_may_not.expect("a route stays closed to phones");

        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let phone = {
            let _turn = ONE_CODE.lock().await;
            let (code, _) = mint_code();
            let state = state.clone();
            tokio::task::spawn_blocking(move || pair_with(&state, &code, "nPHONE").unwrap()).await.unwrap()
        };
        let app = remote_router(state.clone());
        let call = |method: RouteMethod, path: &str, token: Option<&str>, from: &str| {
            let uri = path
                .split('/')
                .map(|seg| if seg.starts_with('{') && seg.ends_with('}') { "x" } else { seg })
                .collect::<Vec<_>>()
                .join("/");
            let mut req = axum::http::Request::builder().method(method.as_str()).uri(uri);
            if let Some(token) = token {
                req = req.header(AUTHORIZATION, format!("Bearer {token}"));
            }
            let mut req = req.body(axum::body::Body::empty()).unwrap();
            req.extensions_mut().insert(Peer { tailnet_node: Some(from.to_string()) });
            app.clone().oneshot(req)
        };
        let operator = state.node.lock().unwrap().operator_token.clone().unwrap_or_else(|| "test-operator-token".into());
        let status = call(open_method, open_path, Some(&phone.token), "nPHONE").await.unwrap().status();
        assert_eq!(status, StatusCode::OK, "a phone may call {open_path}");
        let status = call(closed_method, closed_path, Some(&phone.token), "nPHONE").await.unwrap().status();
        assert_eq!(status, StatusCode::FORBIDDEN, "a phone may not call {closed_path}");
        let status = call(open_method, open_path, None, "nPHONE").await.unwrap().status();
        assert_eq!(status, StatusCode::UNAUTHORIZED, "a missing token is refused");
        let status = call(open_method, open_path, Some(&operator), "nPHONE").await.unwrap().status();
        assert_eq!(status, StatusCode::UNAUTHORIZED, "the operator token is refused");
        let status = call(open_method, open_path, Some(&phone.token), "nOTHER").await.unwrap().status();
        assert_eq!(status, StatusCode::FORBIDDEN, "another tailnet machine is refused");
        let (state2, id) = (state.clone(), phone.device_id.clone());
        tokio::task::spawn_blocking(move || revoke(&state2, &id).unwrap()).await.unwrap();
        let status = call(open_method, open_path, Some(&phone.token), "nPHONE").await.unwrap().status();
        assert_eq!(status, StatusCode::UNAUTHORIZED, "a revoked token is refused");
    }

    #[test]
    fn the_operators_most_powerful_routes_are_never_open_to_a_phone() {
        let never = [
            "/auth", "/machine-access", "/control", "/files/write", "/jobs", "/containers", "/devices", "/mcp",
            "/env", "/providers", "/agents/storage", "/code", "/preview/", "/desktop", "/stream/discard", "/seat", "/policy",
            "/agents/tool-policy", "/client/log", "/versions/check", "/accounts",
        ];
        for (route, _, _) in api_catalog::listing() {
            let sensitive = never.iter().any(|p| route.path.starts_with(p));
            let changes_things = route.method == RouteMethod::Post
                && ["/git", "/ui", "/project", "/task", "/baker/push", "/baker/pr", "/baker/merge", "/worktree"]
                    .iter()
                    .any(|p| route.path.starts_with(p));
            if sensitive || changes_things {
                let what = format!("{} {}", route.method.as_str(), route.path);
                assert!(!api_catalog::remote_allows(route.method, route.path, "phone"), "{what} is never a phone's");
            }
            // The operator's terminals go with them: this Mac's, a project's
            // and a thread's, from the phone too.
            if route.path.starts_with("/terminal") {
                let what = format!("{} {}", route.method.as_str(), route.path);
                assert!(api_catalog::remote_allows(route.method, route.path, "phone"), "{what} is the phone's too");
            }
        }
    }
}
