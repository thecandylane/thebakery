//! The model proxy: one door for API-key model calls (CLIProxyAPI, pinned).
//!
//! The operator installs its release binary from Settings; nothing fetches
//! it implicitly. The node then runs it in a room of its own (Seatbelt: its
//! own folder, and the network only to 127.0.0.1), sets the part of its
//! configuration the node owns through its management API on every start,
//! checks that it listens on 127.0.0.1 alone, and keeps it running. Every
//! upstream call leaves through the node's egress, a CONNECT proxy on
//! 127.0.0.1 allowlisted to the providers' API hosts, so the proxy reaches
//! nowhere else. Keys live only in the proxy's config.yaml, which the node
//! patches but never reads.
//!
//! Each agent's calls carry a client token of its own, and the node is the
//! only reader of the proxy's usage queue, so every call through it lands
//! in the Heart (`model.usage`) against the agent that made it, along with
//! any call a model other than the one asked for served
//! (`model.pin_mismatch`). The totals show in Settings → Agents and, in
//! Prometheus's text format, at `GET /metrics`.

use crate::state::AppState;
use axum::{extract::State, http::StatusCode, Json};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

pub(crate) const VERSION: &str = "8.0.22";
/// The release asset for this Mac and its sha256, as the release publishes it.
#[cfg(target_arch = "aarch64")]
const ASSET: (&str, &str) = (
    "CLIProxyAPI_8.0.22_darwin_aarch64.tar.gz",
    "2ccb94b03ce4feefa2b839f7a0eaae90951d4131572c075099f56c94d15a7f50",
);
#[cfg(not(target_arch = "aarch64"))]
const ASSET: (&str, &str) = (
    "CLIProxyAPI_8.0.22_darwin_amd64.tar.gz",
    "53dfa8d458e224c926314aff437f06524731af9f880844ee79b40c8fece86d65",
);
/// Where the proxy listens: never its default 8317, which may be the
/// operator's own CLIProxyAPI. An isolated node (a scratch test beside the
/// operator's own) names another in BAKERY_MODEL_PROXY_PORT.
pub(crate) fn port() -> u16 {
    env_port("BAKERY_MODEL_PROXY_PORT", 47791)
}

/// The node's egress for the proxy's upstream calls
/// (BAKERY_MODEL_PROXY_EGRESS_PORT on an isolated node).
pub(crate) fn egress_port() -> u16 {
    env_port("BAKERY_MODEL_PROXY_EGRESS_PORT", 47792)
}

fn env_port(name: &str, default: u16) -> u16 {
    std::env::var(name).ok().and_then(|p| p.parse().ok()).unwrap_or(default)
}

/// A scratch test's stand-in for a provider, on this loopback port
/// (BAKERY_MODEL_PROXY_STAND_IN): the proxy may reach it directly. Unset on
/// every real node.
fn stand_in() -> Option<u16> {
    std::env::var("BAKERY_MODEL_PROXY_STAND_IN").ok().and_then(|p| p.parse().ok())
}
/// The proxy's facts in the Heart.
pub(crate) const STREAM: &str = "sys:model-proxy";
/// The API hosts every upstream call may reach.
pub(crate) const PROVIDER_HOSTS: &[&str] = &[
    "api.anthropic.com",
    "api.x.ai",
    "generativelanguage.googleapis.com",
    "api.openai.com",
];

/// The node's own secrets for the proxy, kept in node.json (0600, denied to
/// every agent and container).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProxySecrets {
    /// Its management key; the proxy itself keeps only a hash.
    #[serde(default)]
    pub management_key: Option<String>,
    /// The node's own client token. The proxy answers anyone while its
    /// client list is empty, so the list always holds at least this one.
    #[serde(default)]
    pub client_token: Option<String>,
    /// The key behind every fingerprint (32 bytes, hex): a fingerprint names
    /// an API key without carrying it.
    #[serde(default)]
    pub fingerprint_key: Option<String>,
    /// The API keys the operator gave the proxy, by fingerprint; never the
    /// keys themselves.
    #[serde(default)]
    pub keys: Vec<KeyRecord>,
    /// Each agent's own client token, by agent id: its calls carry it, so
    /// their usage lands on it.
    #[serde(default)]
    pub agent_tokens: BTreeMap<String, String>,
}

/// A provider whose API keys the proxy holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    Claude,
    Gemini,
    Xai,
}

impl Provider {
    const ALL: [Provider; 3] = [Provider::Claude, Provider::Gemini, Provider::Xai];

    /// Its key list in the proxy's management API.
    fn list(self) -> &'static str {
        match self {
            Provider::Claude => "claude-api-key",
            Provider::Gemini => "gemini-api-key",
            Provider::Xai => "xai-api-key",
        }
    }

    /// Its name on the plugs' route (`bakery_agent::proxy`).
    fn route_name(self) -> &'static str {
        match self {
            Provider::Claude => "claude",
            Provider::Gemini => "gemini",
            Provider::Xai => "xai",
        }
    }
}

/// One API key the operator gave the proxy, as the node knows it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KeyRecord {
    pub provider: Provider,
    /// The operator's name for it.
    pub label: String,
    /// When it was added (RFC 3339).
    pub added: String,
    /// blake3 of the key, keyed with `fingerprint_key`.
    pub fingerprint: String,
}

type HttpError = (StatusCode, String);

fn dir(state: &AppState) -> PathBuf {
    state.base.join("proxy")
}

fn bin_dir(state: &AppState) -> PathBuf {
    state
        .base
        .join("bin")
        .join(format!("cliproxyapi-{VERSION}"))
}

fn binary(state: &AppState) -> PathBuf {
    bin_dir(state).join("cli-proxy-api")
}

pub(crate) fn installed(state: &AppState) -> bool {
    binary(state).is_file()
}

fn note(state: &AppState, family: &str, payload: Value) {
    let _ = crate::state::append(state, STREAM, "node", family, payload.to_string(), vec![]);
}

// ─── Install ────────────────────────────────────────────────────────────────

/// Download the pinned release, check its sha256, and unpack its binary.
/// A mismatch installs nothing.
fn install(state: &AppState) -> Result<(), String> {
    let (asset, pinned) = ASSET;
    let url = format!(
        "https://github.com/router-for-me/CLIProxyAPI/releases/download/v{VERSION}/{asset}"
    );
    let response = ureq::get(&url)
        .timeout(Duration::from_secs(300))
        .call()
        .map_err(|e| format!("download {asset}: {e}"))?;
    let mut body = Vec::new();
    response
        .into_reader()
        .take(64 << 20)
        .read_to_end(&mut body)
        .map_err(|e| format!("download {asset}: {e}"))?;
    install_archive(state, asset, &body, pinned)
}

fn install_archive(state: &AppState, asset: &str, body: &[u8], pinned: &str) -> Result<(), String> {
    use sha2::Digest;
    let got = format!("{:x}", sha2::Sha256::digest(body));
    if got != pinned {
        return Err(format!(
            "{asset}: its sha256 is {got}, not the pinned {pinned}; nothing was installed"
        ));
    }
    let target = bin_dir(state);
    let staging = target.with_extension("partial");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| format!("{}: {e}", staging.display()))?;
    let archive = staging.join(asset);
    std::fs::write(&archive, body).map_err(|e| e.to_string())?;
    let out = Command::new("/usr/bin/tar")
        .arg("-xzf")
        .arg(&archive)
        .arg("-C")
        .arg(&staging)
        .arg("cli-proxy-api")
        .output()
        .map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&archive);
    if !out.status.success() {
        return Err(format!(
            "unpack {asset}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let _ = std::fs::remove_dir_all(&target);
    std::fs::rename(&staging, &target).map_err(|e| e.to_string())?;
    Ok(())
}

// ─── Its room and configuration ─────────────────────────────────────────────

/// The proxy's Seatbelt room: its binary, its own folder, and the network
/// only to 127.0.0.1 (its own port, and the node's egress). Certificate
/// checks go through trustd; the Keychain stays out of reach.
fn room(state: &AppState) -> String {
    // Seatbelt matches real paths (/private/var, not /var).
    let quote = |p: PathBuf| {
        let real = p.canonicalize().unwrap_or(p);
        serde_json::to_string(&real.display().to_string()).unwrap_or_default()
    };
    format!(
        r#"(version 1)
(deny default)
(import "system.sb")
(allow process-exec (literal {binary}))
(allow process-fork)
(allow file-read* (subpath {bin_dir}) (subpath {dir}))
(allow file-write* (subpath {dir}))
(allow system-socket)
(allow network-bind (local ip "localhost:{port}"))
(allow network-inbound (local ip "localhost:{port}"))
(allow network-outbound (remote ip "localhost:{egress}"))
(allow mach-lookup (global-name "com.apple.trustd") (global-name "com.apple.trustd.agent"))
"#,
        binary = quote(binary(state)),
        bin_dir = quote(bin_dir(state)),
        dir = quote(dir(state)),
        port = port(),
        egress = egress_port(),
    ) + &stand_in().map(|p| format!("(allow network-outbound (remote ip \"localhost:{p}\"))\n")).unwrap_or_default()
}

/// The configuration the node owns, as the management API takes it, set on
/// every start: loopback only, no plugins, no discovery, no control panel,
/// usage kept for the node to read, every upstream call through the node's
/// egress, and OAuth sign-ins only from an empty folder of its own, never
/// the operator's ~/.cli-proxy-api.
fn owned(state: &AppState, client_tokens: &[String]) -> Value {
    let d = dir(state);
    json!({
        "access": { "api-keys": client_tokens },
        "server": { "host": "127.0.0.1", "port": port(), "discovery": { "enabled": false } },
        "management": {
            "allow-remote": false,
            "disable-control-panel": true,
            "disable-auto-update-panel": true,
        },
        "requests": { "proxy-url": format!("http://127.0.0.1:{}", egress_port()) },
        "routing": { "strategy": "round-robin" },
        "oauth": { "auth-dir": d.join("oauth") },
        "observability": {
            "logs": { "request-log": false },
            "usage": {
                "usage-statistics-enabled": true,
                "redis-usage-queue-retention-seconds": 3600,
            },
        },
        "plugins": { "enabled": false, "dir": d.join("plugins") },
    })
}

/// The proxy's first config.yaml (YAML takes JSON): what the node owns and
/// the management key, which the proxy hashes on start. Written once; from
/// then on only the proxy writes the file.
fn first_config(state: &AppState, secrets: &Secrets) -> Value {
    let mut config = owned(state, std::slice::from_ref(&secrets.client));
    config["config-version"] = json!(8);
    config["management"]["secret-key"] = json!(secrets.management);
    config
}

struct Secrets {
    management: String,
    client: String,
}

/// The management key and the node's client token, made the first time.
fn secrets(state: &AppState) -> Result<Secrets, String> {
    let mut node = state.node.lock().unwrap();
    let mut made = false;
    let mut mint = |slot: &mut Option<String>| {
        slot.get_or_insert_with(|| {
            made = true;
            crate::node::mint_operator_token()
        })
        .clone()
    };
    let management = mint(&mut node.model_proxy.management_key);
    let client = mint(&mut node.model_proxy.client_token);
    if made {
        node.save(&state.base)?;
    }
    Ok(Secrets { management, client })
}

fn prepare(state: &AppState) -> Result<Secrets, String> {
    let d = dir(state);
    for sub in ["oauth", "plugins", "tmp", "logs"] {
        std::fs::create_dir_all(d.join(sub)).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    let had_key = state
        .node
        .lock()
        .unwrap()
        .model_proxy
        .management_key
        .is_some();
    let secrets = secrets(state)?;
    let config = d.join("config.yaml");
    if !config.exists() {
        let text = serde_json::to_string_pretty(&first_config(state, &secrets)).unwrap_or_default();
        std::fs::write(&config, text).map_err(|e| format!("{}: {e}", config.display()))?;
    } else if !had_key {
        return Err(format!(
            "the node lost the proxy's management key; move {} aside to start it afresh (its keys go with it)",
            config.display()
        ));
    }
    std::fs::write(d.join("room.sb"), room(state)).map_err(|e| e.to_string())?;
    Ok(secrets)
}

// ─── Running it ─────────────────────────────────────────────────────────────

/// What the node knows about the proxy now.
#[derive(Debug, Clone, Default, Serialize, schemars::JsonSchema)]
pub(crate) struct ProxyStatus {
    installed: bool,
    version: String,
    running: bool,
    healthy: bool,
    port: u16,
    /// Why it isn't running, or what last stopped it.
    error: Option<String>,
    /// The hosts its upstream calls may reach.
    egress_hosts: Vec<String>,
}

fn status_cell() -> &'static Mutex<ProxyStatus> {
    static STATUS: OnceLock<Mutex<ProxyStatus>> = OnceLock::new();
    STATUS.get_or_init(Default::default)
}

fn set_status(running: bool, healthy: bool, error: Option<String>) {
    let mut status = status_cell().lock().unwrap();
    status.running = running;
    status.healthy = healthy;
    status.error = error;
}

/// How long a call to the proxy on this Mac may take: `limit`, or two
/// minutes in tests, whose process can stall longer than a minute under a
/// full suite's load beside other suites (and a call given up early fails
/// the test for nothing).
fn local(limit: Duration) -> Duration {
    if cfg!(test) {
        Duration::from_secs(120)
    } else {
        limit
    }
}

fn get(url: &str, key: Option<&str>) -> Result<Value, String> {
    get_within(url, key, local(Duration::from_secs(3)))
}

fn get_within(url: &str, key: Option<&str>, timeout: Duration) -> Result<Value, String> {
    let mut request = ureq::get(url).timeout(timeout);
    if let Some(key) = key {
        request = request.set("Authorization", &format!("Bearer {key}"));
    }
    let response = request.call().map_err(scrub)?;
    response.into_json().map_err(|e| e.to_string())
}

fn send(method: &str, url: &str, key: &str, body: Value) -> Result<(), String> {
    ureq::request(method, url)
        .timeout(local(Duration::from_secs(5)))
        .set("Authorization", &format!("Bearer {key}"))
        .send_json(body)
        .map(drop)
        .map_err(scrub)
}

/// A management call's error, without its body: the proxy's error text may
/// quote configuration, keys included.
fn scrub(e: ureq::Error) -> String {
    match e {
        ureq::Error::Status(code, _) => format!("the proxy answered {code}"),
        ureq::Error::Transport(t) => format!("the proxy is unreachable: {}", t.kind()),
    }
}

fn base_url() -> String {
    // Tests may point the node at a stand-in for the management API.
    #[cfg(test)]
    if let Ok(base) = std::env::var("BAKERY_TEST_PROXY_BASE") {
        return base;
    }
    format!("http://127.0.0.1:{}", port())
}

/// Every listening socket `pid` has, as lsof prints them ("127.0.0.1:47791").
fn listening(pid: u32) -> Vec<String> {
    let out = Command::new("/usr/sbin/lsof")
        .args([
            "-nP",
            "-a",
            "-p",
            &pid.to_string(),
            "-iTCP",
            "-sTCP:LISTEN",
            "-Fn",
        ])
        .stdin(Stdio::null())
        .output();
    out.map(|o| listen_addresses(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or_default()
}

/// lsof's `-Fn` lines ("n127.0.0.1:47791", "n*:47791").
fn listen_addresses(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|l| l.strip_prefix('n'))
        .map(str::to_string)
        .collect()
}

/// The proxy listens on 127.0.0.1:port() and nowhere else.
fn listens_on_loopback_only(addresses: &[String]) -> bool {
    let only = format!("127.0.0.1:{}", port());
    !addresses.is_empty() && addresses.iter().all(|a| *a == only)
}

/// Sets what the node owns, then checks what it can't set: no plugin is
/// loaded, and no quota switch may change the model.
fn configure(state: &AppState, secrets: &Secrets) -> Result<(), String> {
    let (base, key) = (base_url(), secrets.management.as_str());
    {
        // An agent's token made meanwhile is either in this list or sent after it.
        let _one = keys_lock().lock().unwrap();
        prune_agent_tokens(state);
        send(
            "PATCH",
            &format!("{base}/v8/management/config"),
            key,
            owned(state, &client_tokens(state)),
        )?;
    }
    let switch = format!("{base}/v0/management/quota-exceeded/switch-preview-model");
    if get(&switch, Some(key))?["switch-preview-model"] == json!(true) {
        send("PUT", &switch, key, json!({ "value": false }))?;
    }
    let plugins = get(&format!("{base}/v8/management/plugins"), Some(key))?;
    let none = plugins["plugins_enabled"] == json!(false)
        && plugins["plugins"].as_array().is_some_and(|p| p.is_empty())
        && std::fs::read_dir(dir(state).join("plugins")).is_ok_and(|mut d| d.next().is_none());
    if !none {
        return Err(
            "a plugin is loaded or waiting in its plugins folder; the proxy runs with none"
                .to_string(),
        );
    }
    Ok(())
}

/// Stops a proxy a crashed node left running, found by its pid file.
fn stop_leftover(state: &AppState) {
    let pid_file = dir(state).join("proxy.pid");
    let Some(pid) = std::fs::read_to_string(&pid_file)
        .ok()
        .and_then(|t| t.trim().parse::<i32>().ok())
    else {
        return;
    };
    let command = Command::new("/bin/ps")
        .args(["-p", &pid.to_string(), "-o", "comm="])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    if command.ends_with("cli-proxy-api") {
        // SAFETY: a plain kill(2) of the proxy this node started, checked by name.
        unsafe { libc::kill(pid, libc::SIGTERM) };
        std::thread::sleep(Duration::from_millis(500));
    }
    let _ = std::fs::remove_file(pid_file);
}

/// Caps the proxy's log, which gains a line per call and per usage read:
/// past 16 MB it moves to proxy.log.1, the one older generation kept.
fn trim_log(state: &AppState) {
    let log = dir(state).join("logs").join("proxy.log");
    if std::fs::metadata(&log).is_ok_and(|m| m.len() > 16 << 20) {
        let _ = std::fs::copy(&log, log.with_extension("log.1"));
        // The proxy appends, so its next line starts the file afresh.
        let _ = std::fs::OpenOptions::new()
            .write(true)
            .open(&log)
            .and_then(|f| f.set_len(0));
    }
}

fn spawn(state: &AppState) -> Result<Child, String> {
    let d = dir(state);
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(d.join("logs").join("proxy.log"))
        .map_err(|e| e.to_string())?;
    let err = log.try_clone().map_err(|e| e.to_string())?;
    let child = Command::new("/usr/bin/sandbox-exec")
        .arg("-f")
        .arg(d.join("room.sb"))
        .arg(binary(state))
        .arg("--config")
        .arg(d.join("config.yaml"))
        .arg("--local-model")
        // It reads its working folder at start: its own.
        .current_dir(&d)
        .env_clear()
        .env("HOME", &d)
        .env("TMPDIR", d.join("tmp"))
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(err)
        .spawn()
        .map_err(|e| format!("start the proxy: {e}"))?;
    let _ = std::fs::write(d.join("proxy.pid"), child.id().to_string());
    Ok(child)
}

/// The proxy, from start to its end: started, checked, configured, then
/// watched. Returns why it ended, and whether it stays down until the
/// operator fixes something.
fn run_once(state: &AppState) -> (String, bool) {
    let secrets = match prepare(state) {
        Ok(secrets) => secrets,
        Err(why) => return (why, true),
    };
    stop_leftover(state);
    let mut child = match spawn(state) {
        Ok(child) => child,
        Err(why) => return (why, false),
    };
    let stop = |child: &mut Child| {
        let _ = child.kill();
        let _ = child.wait();
    };
    let health = format!("{}/healthz", base_url());
    let deadline = Instant::now() + Duration::from_secs(30);
    // Each probe may wait long: a network filter (LuLu) checks a fresh node
    // binary on its first connection, longer under load, and a probe given up
    // early never sees that check finish. A refused probe returns at once.
    while get_within(&health, None, Duration::from_secs(20)).is_err() {
        if Instant::now() > deadline || child.try_wait().ok().flatten().is_some() {
            stop(&mut child);
            return ("it didn't answer /healthz within 30 s".to_string(), false);
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    let addresses = listening(child.id());
    if !listens_on_loopback_only(&addresses) {
        stop(&mut child);
        note(
            state,
            "model_proxy.refused",
            json!({ "listening": addresses }),
        );
        return (
            format!(
                "it listened on {} instead of 127.0.0.1:{} only; stopped until that's fixed",
                addresses.join(", "),
                port()
            ),
            true,
        );
    }
    if let Err(why) = configure(state, &secrets) {
        stop(&mut child);
        note(state, "model_proxy.refused", json!({ "why": why }));
        return (why, true);
    }
    // A new run: it took every kept token, and it has no cooldowns yet.
    MINTING_HALTED.store(false, Ordering::SeqCst);
    *quota().lock().unwrap() = Quota::default();
    set_status(true, true, None);
    if let Err(why) = reconcile(state) {
        note(state, "model_proxy.keys_unchecked", json!({ "why": why }));
    }
    follow_cooldowns(state, &secrets.management);
    note(
        state,
        "model_proxy.up",
        json!({ "version": VERSION, "port": port() }),
    );
    // Usage every 2 s, health every 10 s.
    let (mut misses, mut tick) = (0, 0u64);
    loop {
        std::thread::sleep(Duration::from_secs(2));
        if let Ok(Some(exit)) = child.try_wait() {
            return (format!("it exited ({exit})"), false);
        }
        read_usage(state, &secrets.management);
        tick += 1;
        if tick % 5 != 0 {
            continue;
        }
        trim_log(state);
        if get(&health, None).is_ok() {
            misses = 0;
        } else {
            misses += 1;
            if misses >= 3 {
                stop(&mut child);
                return ("it stopped answering /healthz".to_string(), false);
            }
        }
    }
}

/// Keeps the proxy running, restarting it with a growing pause (1 s to a
/// minute), until something the operator must fix stops it. Each restart
/// checks its listening sockets again.
pub(crate) fn start(state: AppState) {
    // One supervisor at a time; it clears this when the proxy stays down,
    // so the operator can start it again once they've fixed the cause.
    static RUNNING: AtomicBool = AtomicBool::new(false);
    if !installed(&state) || RUNNING.swap(true, Ordering::SeqCst) {
        return;
    }
    crate::seat_gateway::ensure_egress(state.clone());
    std::thread::spawn(move || {
        let mut pause = 1;
        loop {
            let began = Instant::now();
            let (why, fatal) = run_once(&state);
            RUN.fetch_add(1, Ordering::SeqCst);
            set_status(false, false, Some(why.clone()));
            // A proxy that ran a while and dropped comes straight back: its
            // plugs keep pointing at it, and their turns wait out the gap
            // (`resume`). One that won't come up, or must stay down, hands
            // the plugs back to their providers directly.
            if fatal || began.elapsed() < Duration::from_secs(60) {
                bakery_agent::proxy::set(None);
            }
            note(
                &state,
                "model_proxy.down",
                json!({ "why": why, "restarts": !fatal }),
            );
            if fatal {
                RUNNING.store(false, Ordering::SeqCst);
                return;
            }
            if began.elapsed() > Duration::from_secs(300) {
                pause = 1;
            }
            std::thread::sleep(Duration::from_secs(pause));
            pause = (pause * 2).min(60);
        }
    });
}

// ─── Keys ───────────────────────────────────────────────────────────────────
//
// The management API can only replace a provider's whole key list, so an
// add reads the list, appends, and writes it back; a removal finds its key
// by fingerprint and deletes that index. Keys pass through the node's
// memory for that one call and nowhere else: never stored, logged, returned
// or put in a URL.

/// One add or removal at a time, so a read-append-write never loses a key.
fn keys_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(Default::default)
}

fn fingerprint(state: &AppState, key: &str) -> Result<String, String> {
    let mut node = state.node.lock().unwrap();
    let hex = match &node.model_proxy.fingerprint_key {
        Some(hex) => hex.clone(),
        None => {
            let hex = crate::node::mint_operator_token();
            node.model_proxy.fingerprint_key = Some(hex.clone());
            node.save(&state.base)?;
            hex
        }
    };
    let mut secret = [0u8; 32];
    for (i, byte) in secret.iter_mut().enumerate() {
        *byte = u8::from_str_radix(hex.get(2 * i..2 * i + 2).unwrap_or("00"), 16).unwrap_or(0);
    }
    Ok(blake3::keyed_hash(&secret, key.as_bytes())
        .to_hex()
        .to_string())
}

/// A provider's entries, as the management API holds them.
fn entries(provider: Provider, key: &str) -> Result<Vec<Value>, String> {
    let list = get(
        &format!("{}/v0/management/{}", base_url(), provider.list()),
        Some(key),
    )?;
    Ok(list[provider.list()]
        .as_array()
        .cloned()
        .unwrap_or_default())
}

fn entry_fingerprint(state: &AppState, entry: &Value) -> Option<String> {
    entry["api-key"]
        .as_str()
        .and_then(|k| fingerprint(state, k.trim()).ok())
}

fn records(state: &AppState) -> Vec<KeyRecord> {
    state.node.lock().unwrap().model_proxy.keys.clone()
}

fn save_records(state: &AppState, keys: Vec<KeyRecord>) -> Result<(), String> {
    let mut node = state.node.lock().unwrap();
    node.model_proxy.keys = keys;
    node.save(&state.base)
}

/// The proxy's index of a key, as it computes it: the first 8 bytes of
/// sha256 over its list name, base URL (none) and the key. A usage record or
/// a cooldown names the key that served it this way.
fn auth_index(provider: Provider, key: &str) -> String {
    use sha2::Digest;
    let digest = sha2::Sha256::digest(format!("{}:+{}", provider.list(), key.trim()));
    digest[..8].iter().map(|b| format!("{b:02x}")).collect()
}

/// Each key the proxy holds, by its index: its provider and the operator's
/// label. In memory only, never in node.json or the Heart, since the index
/// is an unsalted hash of the key; filled when the node holds the key anyway
/// (an add, and the read when the proxy comes up).
fn key_names() -> &'static Mutex<HashMap<String, (Provider, String)>> {
    static NAMES: OnceLock<Mutex<HashMap<String, (Provider, String)>>> = OnceLock::new();
    NAMES.get_or_init(Default::default)
}

/// Point the plugs at the proxy for every provider it holds a key for.
fn update_route(state: &AppState) {
    let healthy = status_cell().lock().unwrap().healthy;
    let token = state.node.lock().unwrap().model_proxy.client_token.clone();
    let providers: Vec<String> = Provider::ALL
        .into_iter()
        .filter(|p| records(state).iter().any(|r| r.provider == *p))
        .map(|p| p.route_name().to_string())
        .collect();
    bakery_agent::proxy::set(match (healthy, token) {
        (true, Some(token)) if !providers.is_empty() => Some(bakery_agent::proxy::Route {
            base: base_url(),
            token,
            providers,
        }),
        _ => None,
    });
}

/// Drops the records of keys the proxy no longer holds (a hand edit, a
/// fresh config), then routes the plugs. Run when the proxy comes up.
fn reconcile(state: &AppState) -> Result<(), String> {
    let _one = keys_lock().lock().unwrap();
    let key = secrets(state)?.management;
    let mut kept = Vec::new();
    let mut names = HashMap::new();
    for provider in Provider::ALL {
        for entry in entries(provider, &key)? {
            let Some(print) = entry_fingerprint(state, &entry) else {
                continue;
            };
            if let Some(record) = records(state)
                .into_iter()
                .find(|r| r.provider == provider && r.fingerprint == print)
            {
                let held = entry["api-key"].as_str().unwrap_or_default();
                names.insert(auth_index(provider, held), (provider, record.label.clone()));
                kept.push(record);
            }
        }
    }
    *key_names().lock().unwrap() = names;
    save_records(state, kept)?;
    update_route(state);
    Ok(())
}

fn running(state: &AppState) -> Result<String, String> {
    if !status_cell().lock().unwrap().healthy {
        return Err("the model proxy isn't running: start it first".to_string());
    }
    Ok(secrets(state)?.management)
}

/// Gives the proxy one more key for `provider`.
fn add_key(state: &AppState, provider: Provider, key: &str, label: &str) -> Result<(), String> {
    let key = key.trim();
    if key.is_empty()
        || key.len() > 1024
        || key.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return Err("that isn't an API key".to_string());
    }
    let management = running(state)?;
    let _one = keys_lock().lock().unwrap();
    let print = fingerprint(state, key)?;
    let mut list = entries(provider, &management)?;
    if list
        .iter()
        .any(|e| entry_fingerprint(state, e).as_deref() == Some(print.as_str()))
    {
        return Err("the proxy already holds that key".to_string());
    }
    list.push(json!({ "api-key": key }));
    send(
        "PUT",
        &format!("{}/v0/management/{}", base_url(), provider.list()),
        &management,
        Value::Array(list),
    )?;
    let mut keys = records(state);
    let label = match label.trim() {
        "" => format!(
            "{} key {}",
            provider.route_name(),
            keys.iter().filter(|r| r.provider == provider).count() + 1
        ),
        label => label.chars().take(60).collect(),
    };
    keys.push(KeyRecord {
        provider,
        label: label.clone(),
        added: chrono::Utc::now().to_rfc3339(),
        fingerprint: print,
    });
    key_names()
        .lock()
        .unwrap()
        .insert(auth_index(provider, key), (provider, label.clone()));
    save_records(state, keys)?;
    update_route(state);
    note(
        state,
        "model_proxy.key_added",
        json!({ "provider": provider }),
    );
    Ok(())
}

/// Takes one key back from the proxy, found by its fingerprint.
fn remove_key(state: &AppState, provider: Provider, id: &str) -> Result<(), String> {
    let management = running(state)?;
    let _one = keys_lock().lock().unwrap();
    let held = entries(provider, &management)?;
    let at = held
        .iter()
        .position(|e| entry_fingerprint(state, e).as_deref() == Some(id));
    let mut keys = records(state);
    keys.retain(|r| !(r.provider == provider && r.fingerprint == id));
    if let Some(at) = at {
        let key = held[at]["api-key"].as_str().unwrap_or_default();
        key_names()
            .lock()
            .unwrap()
            .remove(&auth_index(provider, key));
        let url = format!(
            "{}/v0/management/{}?index={at}",
            base_url(),
            provider.list()
        );
        ureq::delete(&url)
            .timeout(local(Duration::from_secs(5)))
            .set("Authorization", &format!("Bearer {management}"))
            .call()
            .map(drop)
            .map_err(scrub)?;
    }
    save_records(state, keys)?;
    update_route(state);
    match at {
        Some(_) => {
            note(
                state,
                "model_proxy.key_removed",
                json!({ "provider": provider }),
            );
            Ok(())
        }
        None => Err("that key is no longer in the proxy; the list is refreshed".to_string()),
    }
}

#[derive(Serialize, schemars::JsonSchema)]
pub(crate) struct KeyView {
    /// Its fingerprint: names the key without carrying it.
    id: String,
    label: String,
    added: String,
}

#[derive(Serialize, schemars::JsonSchema)]
pub(crate) struct ProviderKeys {
    provider: Provider,
    keys: Vec<KeyView>,
}

/// Which providers the proxy holds keys for, and how many; never a key.
#[derive(Serialize, schemars::JsonSchema)]
pub(crate) struct ModelKeys {
    providers: Vec<ProviderKeys>,
}

fn model_keys(state: &AppState) -> ModelKeys {
    let keys = records(state);
    ModelKeys {
        providers: Provider::ALL
            .into_iter()
            .map(|provider| ProviderKeys {
                provider,
                keys: keys
                    .iter()
                    .filter(|r| r.provider == provider)
                    .map(|r| KeyView {
                        id: r.fingerprint.clone(),
                        label: r.label.clone(),
                        added: r.added.clone(),
                    })
                    .collect(),
            })
            .collect(),
    }
}

/// An API key the operator gives the proxy. (No Debug: it holds a key.)
#[derive(Deserialize, schemars::JsonSchema)]
pub(crate) struct AddKey {
    provider: Provider,
    /// The API key itself; it goes to the proxy and nowhere else.
    key: String,
    /// The operator's name for it.
    #[serde(default)]
    label: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub(crate) struct RemoveKey {
    provider: Provider,
    /// The key's fingerprint, from GET /model-proxy/keys.
    id: String,
}

/// `GET /model-proxy/keys`: which providers the proxy holds keys for.
pub(crate) async fn list_keys(State(st): State<AppState>) -> Json<ModelKeys> {
    Json(model_keys(&st))
}

/// `POST /model-proxy/keys`: the operator gives the proxy an API key.
/// Hidden from agents; no answer ever carries a key.
pub(crate) async fn add_key_route(
    State(st): State<AppState>,
    Json(r): Json<AddKey>,
) -> Result<Json<ModelKeys>, HttpError> {
    let state = st.clone();
    crate::actor::spawn_blocking(move || add_key(&state, r.provider, &r.key, &r.label))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map_err(|e| (StatusCode::CONFLICT, e))?;
    Ok(Json(model_keys(&st)))
}

/// `POST /model-proxy/keys/remove`: the operator takes a key back.
/// Destructive; hidden from agents.
pub(crate) async fn remove_key_route(
    State(st): State<AppState>,
    Json(r): Json<RemoveKey>,
) -> Result<Json<ModelKeys>, HttpError> {
    let state = st.clone();
    crate::actor::spawn_blocking(move || remove_key(&state, r.provider, &r.id))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map_err(|e| (StatusCode::CONFLICT, e))?;
    Ok(Json(model_keys(&st)))
}

// ─── Agents' tokens and usage ───────────────────────────────────────────────

/// Every client token the proxy answers: the node's own, then each agent's.
fn client_tokens(state: &AppState) -> Vec<String> {
    let node = state.node.lock().unwrap();
    let secrets = &node.model_proxy;
    secrets
        .client_token
        .iter()
        .chain(secrets.agent_tokens.values())
        .cloned()
        .collect()
}

/// Whose each client token is: an agent's id, or `node`.
fn token_owners(state: &AppState) -> HashMap<String, String> {
    let node = state.node.lock().unwrap();
    let secrets = &node.model_proxy;
    secrets
        .agent_tokens
        .iter()
        .map(|(agent, token)| (token.clone(), agent.clone()))
        .chain(
            secrets
                .client_token
                .clone()
                .map(|t| (t, "node".to_string())),
        )
        .collect()
}

/// Set when the proxy wouldn't take a new agent's token, until it starts
/// again (which also drops a token it took but the node never kept), so a
/// failing proxy isn't sent a new token by every turn.
static MINTING_HALTED: AtomicBool = AtomicBool::new(false);

/// Whether a conversation is gone: never recorded, deleted, or archived.
fn conversation_gone(heart: &bakery_heart::Heart, stream_id: &str) -> bool {
    let archived = |family: &str| {
        heart
            .last_of_family(stream_id, family)
            .ok()
            .flatten()
            .and_then(|e| serde_json::from_str::<Value>(&e.payload).ok())
            .is_some_and(|p| p["archived"] == json!(true))
    };
    heart
        .read_stream_tail(stream_id, 1)
        .is_ok_and(|tail| tail.is_empty())
        || crate::state::discarded_of(heart, stream_id)
        || archived(crate::agents::SOCKET_ARCHIVED_FAMILY)
        || archived(crate::baker_threads::THREAD_ARCHIVED_FAMILY)
}

/// Drops the tokens of agents whose conversation is gone, before a start
/// gives the proxy the list.
fn prune_agent_tokens(state: &AppState) {
    let agents: Vec<String> = state
        .node
        .lock()
        .unwrap()
        .model_proxy
        .agent_tokens
        .keys()
        .cloned()
        .collect();
    let gone: Vec<String> = {
        let heart = state.heart.lock().unwrap();
        agents
            .into_iter()
            .filter(|agent| {
                let stream_id = agent.rsplit_once(':').map_or(agent.as_str(), |(s, _)| s);
                conversation_gone(&heart, stream_id)
            })
            .collect()
    };
    if gone.is_empty() {
        return;
    }
    let mut node = state.node.lock().unwrap();
    for agent in &gone {
        node.model_proxy.agent_tokens.remove(agent);
    }
    let _ = node.save(&state.base);
}

/// The token `agent`'s calls through the proxy carry, made and given to the
/// proxy the first time. `None` while the proxy serves no provider, or when
/// it won't take a new token: the calls then carry the node's own.
pub(crate) fn agent_token(state: &AppState, agent: &str) -> Option<String> {
    if !status_cell().lock().unwrap().healthy || records(state).is_empty() {
        return None;
    }
    let kept = || {
        state
            .node
            .lock()
            .unwrap()
            .model_proxy
            .agent_tokens
            .get(agent)
            .cloned()
    };
    if let Some(token) = kept() {
        return Some(token);
    }
    let _one = keys_lock().lock().unwrap();
    if let Some(token) = kept() {
        return Some(token);
    }
    if MINTING_HALTED.load(Ordering::SeqCst) {
        return None;
    }
    let management = secrets(state).ok()?.management;
    let token = crate::node::mint_operator_token();
    let mut tokens = client_tokens(state);
    tokens.push(token.clone());
    // The proxy takes it before the node keeps it, so a kept token answers.
    let sent = send(
        "PATCH",
        &format!("{}/v8/management/config", base_url()),
        &management,
        json!({ "access": { "api-keys": tokens } }),
    );
    // It answers the PATCH before its config watcher applies it.
    let models = format!("{}/v1/models", base_url());
    let deadline = Instant::now() + local(Duration::from_secs(5));
    let mut taken = sent.is_ok();
    while taken && get(&models, Some(&token)).is_err() {
        taken = Instant::now() < deadline;
        std::thread::sleep(Duration::from_millis(100));
    }
    if !taken {
        MINTING_HALTED.store(true, Ordering::SeqCst);
        return None;
    }
    let mut node = state.node.lock().unwrap();
    node.model_proxy
        .agent_tokens
        .insert(agent.to_string(), token.clone());
    node.save(&state.base).ok()?;
    Some(token)
}

/// One call through the proxy, as the Heart keeps it (`model.usage`): whose
/// it was, which model answered, what it came to. Never the token it carried.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct Call {
    /// The agent whose token it carried; `node` for the node's own calls.
    agent: String,
    provider: String,
    /// The model the proxy called.
    model: String,
    /// The model asked for, when the proxy called another.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    asked: String,
    /// The model the provider says answered, when that was another.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    served: String,
    /// `ok`, or the HTTP status it failed with.
    status: String,
    /// The operator's label for the key that served it, when the node knows it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    key: String,
    tokens_in: u64,
    tokens_out: u64,
    #[serde(default)]
    reasoning: u64,
    #[serde(default)]
    cached: u64,
    latency_ms: u64,
    at: String,
}

impl Call {
    /// A model other than the one asked for answered it.
    fn off_pin(&self) -> bool {
        !self.asked.is_empty() || !self.served.is_empty()
    }
}

/// Whether `a` and `b` name one model, as the proxy itself judges it: case,
/// a provider prefix, a thinking suffix, `-latest` and a dated snapshot
/// aside. An empty name claims nothing.
fn same_model(a: &str, b: &str) -> bool {
    fn bare(name: &str) -> String {
        let name = name.trim().to_ascii_lowercase();
        let name = match name.rfind('(') {
            Some(i) if name.ends_with(')') => name[..i].trim().to_string(),
            _ => name,
        };
        let name = match name.rfind('/') {
            Some(i) if i + 1 < name.len() => name[i + 1..].to_string(),
            _ => name,
        };
        name.strip_suffix("-latest")
            .map(str::to_string)
            .unwrap_or(name)
    }
    fn snapshot_of(base: &str, other: &str) -> bool {
        let Some(version) = other.strip_prefix(base).and_then(|v| v.strip_prefix('-')) else {
            return false;
        };
        let dashes: &[usize] = match version.len() {
            3 | 8 => &[],
            10 => &[4, 7],
            _ => return false,
        };
        version.bytes().enumerate().all(|(i, c)| {
            if dashes.contains(&i) {
                c == b'-'
            } else {
                c.is_ascii_digit()
            }
        })
    }
    let (a, b) = (bare(a), bare(b));
    a.is_empty() || b.is_empty() || a == b || snapshot_of(&a, &b) || snapshot_of(&b, &a)
}

/// A record from the proxy's usage queue, as a call of an agent in `owners`,
/// served by a key in `names`.
fn call_of(
    record: &Value,
    owners: &HashMap<String, String>,
    names: &HashMap<String, (Provider, String)>,
) -> Call {
    let text = |key: &str| record[key].as_str().unwrap_or_default().trim().to_string();
    let tokens = |key: &str| record["tokens"][key].as_u64().unwrap_or(0);
    let model = text("model");
    let other = |name: String| {
        if same_model(&name, &model) {
            String::new()
        } else {
            name
        }
    };
    Call {
        agent: owners
            .get(&text("api_key"))
            .cloned()
            .unwrap_or_else(|| "unknown".to_string()),
        provider: text("provider"),
        asked: other(text("alias")),
        served: other(text("response_model")),
        status: match (
            record["failed"].as_bool(),
            record["fail"]["status_code"].as_u64(),
        ) {
            (Some(true), Some(code)) => code.to_string(),
            (Some(true), None) => "error".to_string(),
            _ => "ok".to_string(),
        },
        key: names
            .get(&text("auth_index"))
            .map(|(_, label)| label.clone())
            .unwrap_or_default(),
        tokens_in: tokens("input_tokens"),
        tokens_out: tokens("output_tokens"),
        reasoning: tokens("reasoning_tokens"),
        cached: tokens("cached_tokens"),
        latency_ms: record["latency_ms"].as_u64().unwrap_or(0),
        at: text("timestamp"),
        model,
    }
}

/// One agent's calls to one model, added up.
#[derive(Debug, Default)]
struct Totals {
    /// Calls by status: `ok`, `429`, …
    calls: BTreeMap<String, u64>,
    tokens_in: u64,
    tokens_out: u64,
    reasoning: u64,
    cached: u64,
    latency_ms: u64,
    off_pin: u64,
    last_at: String,
    /// Its last call's status.
    last_status: String,
}

/// Every call through the proxy, by (agent, provider, model).
type Ledger = BTreeMap<(String, String, String), Totals>;

fn add(ledger: &mut Ledger, call: &Call) {
    let totals = ledger
        .entry((
            call.agent.clone(),
            call.provider.clone(),
            call.model.clone(),
        ))
        .or_default();
    *totals.calls.entry(call.status.clone()).or_default() += 1;
    totals.tokens_in += call.tokens_in;
    totals.tokens_out += call.tokens_out;
    totals.reasoning += call.reasoning;
    totals.cached += call.cached;
    totals.latency_ms += call.latency_ms;
    totals.off_pin += u64::from(call.off_pin());
    if call.at >= totals.last_at {
        totals.last_at = call.at.clone();
        totals.last_status = call.status.clone();
    }
}

/// Every call through the proxy, added up from the Heart once, then as each
/// call comes in.
// ponytail: folds every model.usage event once per start; keep a snapshot
// in the node's folder when that gets slow.
fn ledger(state: &AppState) -> std::sync::MutexGuard<'static, Ledger> {
    static LEDGER: OnceLock<Mutex<Ledger>> = OnceLock::new();
    LEDGER
        .get_or_init(|| {
            let events = state
                .heart
                .lock()
                .unwrap()
                .events_of_family(STREAM, "model.usage")
                .unwrap_or_default();
            let mut ledger = Ledger::new();
            for call in events
                .iter()
                .filter_map(|e| serde_json::from_str::<Call>(&e.payload).ok())
            {
                add(&mut ledger, &call);
            }
            Mutex::new(ledger)
        })
        .lock()
        .unwrap()
}

/// Adds a call up (before the Heart has it, so a first read of the Heart
/// can't count it twice), then records it, and a pin mismatch on its own.
fn record_call(state: &AppState, call: Call) {
    add(&mut ledger(state), &call);
    if call.off_pin() {
        let or_called = |name: &str| {
            if name.is_empty() {
                call.model.clone()
            } else {
                name.to_string()
            }
        };
        note(
            state,
            "model.pin_mismatch",
            json!({
                "agent": call.agent,
                "provider": call.provider,
                "asked": or_called(&call.asked),
                "called": call.model,
                "served": or_called(&call.served),
            }),
        );
    }
    note(
        state,
        "model.usage",
        serde_json::to_value(&call).unwrap_or_default(),
    );
}

/// Takes every record in the proxy's usage queue (the node is its only
/// reader) and records each call against the agent whose token it carried.
fn read_usage(state: &AppState, management: &str) {
    const BATCH: usize = 200;
    let url = format!(
        "{}/v8/management/observability/usage/queue?count={BATCH}",
        base_url()
    );
    loop {
        let Ok(Value::Array(records)) = get(&url, Some(management)) else {
            return;
        };
        let owners = token_owners(state);
        let names = key_names().lock().unwrap().clone();
        for record in &records {
            let call = call_of(record, &owners, &names);
            if call.status == "ok" {
                let index = record["auth_index"].as_str().unwrap_or_default();
                recovered(&call.provider, &call.model, index);
            }
            record_call(state, call);
        }
        if records.len() < BATCH {
            return;
        }
    }
}

/// One agent's calls through the model proxy, all time.
#[derive(Debug, Default, Serialize, schemars::JsonSchema)]
pub(crate) struct AgentUsage {
    calls: u64,
    /// Calls that failed: refused, rate-limited, or the provider erred.
    failed: u64,
    tokens_in: u64,
    tokens_out: u64,
    /// Calls a model other than the one asked for answered.
    pin_mismatches: u64,
    /// When its last call went through (RFC 3339).
    last_at: String,
}

/// What `agent`'s calls through the proxy came to; `None` before its first.
pub(crate) fn agent_usage(state: &AppState, agent: &str) -> Option<AgentUsage> {
    let ledger = ledger(state);
    let mut usage: Option<AgentUsage> = None;
    for (_, totals) in ledger.iter().filter(|((a, _, _), _)| a == agent) {
        let u = usage.get_or_insert_with(Default::default);
        let calls: u64 = totals.calls.values().sum();
        u.calls += calls;
        u.failed += calls - totals.calls.get("ok").copied().unwrap_or(0);
        u.tokens_in += totals.tokens_in;
        u.tokens_out += totals.tokens_out;
        u.pin_mismatches += totals.off_pin;
        if totals.last_at > u.last_at {
            u.last_at = totals.last_at.clone();
        }
    }
    usage
}

/// The proxy's numbers in Prometheus's text format.
fn metrics_text(up: bool, ledger: &Ledger) -> String {
    use std::fmt::Write;
    let esc = |v: &str| {
        v.replace('\\', r"\\")
            .replace('"', r#"\""#)
            .replace('\n', r"\n")
    };
    let (mut calls, mut tokens, mut latency, mut off_pin) =
        (String::new(), String::new(), String::new(), String::new());
    for ((agent, provider, model), t) in ledger {
        let at = format!(
            r#"agent="{}",provider="{}",model="{}""#,
            esc(agent),
            esc(provider),
            esc(model)
        );
        for (status, n) in &t.calls {
            let _ = writeln!(
                calls,
                r#"bakery_model_requests_total{{{at},status="{}"}} {n}"#,
                esc(status)
            );
        }
        for (kind, n) in [
            ("input", t.tokens_in),
            ("output", t.tokens_out),
            ("reasoning", t.reasoning),
            ("cached", t.cached),
        ] {
            let _ = writeln!(
                tokens,
                r#"bakery_model_tokens_total{{{at},kind="{kind}"}} {n}"#
            );
        }
        let count: u64 = t.calls.values().sum();
        let seconds = t.latency_ms as f64 / 1000.0;
        let _ = writeln!(
            latency,
            "bakery_model_latency_seconds_sum{{{at}}} {seconds}"
        );
        let _ = writeln!(
            latency,
            "bakery_model_latency_seconds_count{{{at}}} {count}"
        );
        let _ = writeln!(
            off_pin,
            "bakery_model_pin_mismatch_total{{{at}}} {}",
            t.off_pin
        );
    }
    let mut out = String::new();
    for (name, kind, help, body) in [
        (
            "bakery_model_proxy_up",
            "gauge",
            "Whether the model proxy is running and answering.",
            format!("bakery_model_proxy_up {}\n", u8::from(up)),
        ),
        (
            "bakery_model_requests_total",
            "counter",
            "Model calls through the proxy, by agent, provider, model and status.",
            calls,
        ),
        (
            "bakery_model_tokens_total",
            "counter",
            "Tokens of model calls through the proxy, by kind.",
            tokens,
        ),
        (
            "bakery_model_latency_seconds",
            "summary",
            "How long model calls through the proxy took.",
            latency,
        ),
        (
            "bakery_model_pin_mismatch_total",
            "counter",
            "Calls a model other than the one asked for answered.",
            off_pin,
        ),
    ] {
        let _ = write!(out, "# HELP {name} {help}\n# TYPE {name} {kind}\n{body}");
    }
    out
}

/// `GET /metrics`: the model proxy's numbers, in Prometheus's text format.
/// Hidden from agents; a scraper sends the operator's token.
pub(crate) async fn metrics(
    State(st): State<AppState>,
) -> Result<([(axum::http::HeaderName, &'static str); 1], String), HttpError> {
    let up = status_cell().lock().unwrap().healthy;
    let text = crate::actor::spawn_blocking(move || metrics_text(up, &ledger(&st)))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok((
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4",
        )],
        text,
    ))
}

// ─── Quota ──────────────────────────────────────────────────────────────────
//
// The proxy rotates keys within a model by itself: a key that hits its
// quota cools down for that model, and the next call goes to another key.
// It pushes each cooldown to subscribers of its Redis-protocol `errors`
// channel (on its own port, behind the management key) and never queues
// them, so the node keeps one subscription while the proxy runs. When every
// key for a model is cooling, the model is out of quota, and switching
// models is the Baker's decision: the node tells it, and a switch it makes
// `because` of that hands each stopped conversation to the new model.

/// Bumped when a proxy run ends, so that run's cooldown follower stops.
static RUN: AtomicU64 = AtomicU64::new(0);

/// What the node knows of the proxy's cooldowns this run.
#[derive(Default)]
struct Quota {
    /// (provider, model) → each cooling key's index → until when.
    cooling: HashMap<(String, String), HashMap<String, chrono::DateTime<chrono::Utc>>>,
    /// Models noted out of quota, until a key serves them again.
    out: HashSet<(String, String)>,
}

fn quota() -> &'static Mutex<Quota> {
    static QUOTA: OnceLock<Mutex<Quota>> = OnceLock::new();
    QUOTA.get_or_init(Default::default)
}

/// One Redis-protocol command.
fn resp_command(args: &[&str]) -> Vec<u8> {
    let mut out = format!("*{}\r\n", args.len()).into_bytes();
    for arg in args {
        out.extend_from_slice(format!("${}\r\n{arg}\r\n", arg.len()).as_bytes());
    }
    out
}

/// One Redis-protocol reply, flattened: an array's items, or its one value.
fn read_resp(reader: &mut impl std::io::BufRead) -> Result<Vec<String>, String> {
    fn line(reader: &mut impl std::io::BufRead) -> Result<String, String> {
        let mut text = String::new();
        match reader.read_line(&mut text) {
            Ok(0) => Err("the proxy closed the connection".to_string()),
            Ok(_) => Ok(text.trim_end_matches(['\r', '\n']).to_string()),
            Err(e) => Err(e.to_string()),
        }
    }
    fn value(reader: &mut impl std::io::BufRead, head: &str) -> Result<String, String> {
        let (kind, rest) = head.split_at(head.len().min(1));
        match kind {
            "+" | ":" => Ok(rest.to_string()),
            "-" => Err(rest.to_string()),
            "$" => {
                let len: i64 = rest.parse().map_err(|_| format!("a bad length: {rest}"))?;
                if len < 0 {
                    return Ok(String::new());
                }
                if len > 1 << 20 {
                    return Err("a reply over 1 MB".to_string());
                }
                let mut bytes = vec![0; len as usize + 2];
                reader.read_exact(&mut bytes).map_err(|e| e.to_string())?;
                bytes.truncate(len as usize);
                Ok(String::from_utf8_lossy(&bytes).into_owned())
            }
            _ => Err(format!("an unexpected reply: {head}")),
        }
    }
    let head = line(reader)?;
    match head.strip_prefix('*') {
        Some(count) => {
            let count: usize = count.parse().map_err(|_| format!("a bad count: {count}"))?;
            (0..count)
                .map(|_| line(reader).and_then(|h| value(reader, &h)))
                .collect()
        }
        None => value(reader, &head).map(|v| vec![v]),
    }
}

/// Follows the proxy's pushed cooldowns until its connection ends.
fn follow_errors(state: &AppState, address: &str, management: &str) -> Result<(), String> {
    use std::io::Write;
    let stream = std::net::TcpStream::connect(address).map_err(|e| e.to_string())?;
    // The first exchange may wait on a network filter; pushes then come
    // when they come.
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .map_err(|e| e.to_string())?;
    let mut writer = stream.try_clone().map_err(|e| e.to_string())?;
    let mut reader = std::io::BufReader::new(stream);
    for command in [&["AUTH", management][..], &["SUBSCRIBE", "errors"]] {
        writer
            .write_all(&resp_command(command))
            .map_err(|e| e.to_string())?;
        read_resp(&mut reader)?;
    }
    reader
        .get_ref()
        .set_read_timeout(None)
        .map_err(|e| e.to_string())?;
    loop {
        let message = read_resp(&mut reader)?;
        if message.first().is_some_and(|m| m == "message") {
            if let Some(Ok(event)) = message.get(2).map(|p| serde_json::from_str::<Value>(p)) {
                cooled(state, &event, management);
            }
        }
    }
}

/// Follows this run's cooldowns on a thread of their own, reconnecting
/// until the run ends.
fn follow_cooldowns(state: &AppState, management: &str) {
    let run = RUN.load(Ordering::SeqCst);
    let (state, management) = (state.clone(), management.to_string());
    let address = base_url().trim_start_matches("http://").to_string();
    std::thread::spawn(move || {
        while RUN.load(Ordering::SeqCst) == run {
            let _ = follow_errors(&state, &address, &management);
            std::thread::sleep(Duration::from_secs(1));
        }
    });
}

/// One key cooling for one model, as the proxy pushed it: noted, and once
/// every key the node gave the proxy for that provider is cooling, the
/// model is out of quota.
fn cooled(state: &AppState, event: &Value, management: &str) {
    let model_state = &event["auth_status"]["model"];
    let limits = &model_state["quota"];
    if event["status_code"] != json!(429) && limits["exceeded"] != json!(true) {
        return;
    }
    let text = |v: &Value| v.as_str().unwrap_or_default().trim().to_string();
    let (provider, model, index) = (
        text(&event["provider"]),
        text(&event["model"]),
        text(&event["auth_index"]),
    );
    let until = [
        &limits["next_recover_at"],
        &model_state["next_retry_after"],
        &event["auth_status"]["next_retry_after"],
    ]
    .into_iter()
    .find_map(|v| chrono::DateTime::parse_from_rfc3339(v.as_str()?).ok())
    .map(|t| t.with_timezone(&chrono::Utc))
    .unwrap_or_else(|| chrono::Utc::now() + chrono::Duration::seconds(60));
    let (label, keys) = {
        let names = key_names().lock().unwrap();
        let keys: Vec<String> = names
            .iter()
            .filter(|(_, (p, _))| p.route_name() == provider)
            .map(|(i, _)| i.clone())
            .collect();
        (names.get(&index).map(|(_, label)| label.clone()), keys)
    };
    note(
        state,
        "model.key_cooling",
        json!({
            "provider": provider,
            "model": model,
            "key": label.unwrap_or_else(|| "a key the node didn't add".to_string()),
            "until": until.to_rfc3339(),
        }),
    );
    let out_until = {
        let mut guard = quota().lock().unwrap();
        let quota = &mut *guard;
        let now = chrono::Utc::now();
        let target = (provider.clone(), model.clone());
        let cooling = quota.cooling.entry(target.clone()).or_default();
        cooling.insert(index, until);
        cooling.retain(|_, until| *until > now);
        let all = !keys.is_empty() && keys.iter().all(|k| cooling.contains_key(k));
        let soonest = cooling.values().min().copied();
        if !all {
            quota.out.remove(&target);
            None
        } else if quota.out.insert(target) {
            soonest
        } else {
            None
        }
    };
    if let Some(until) = out_until {
        quota_exhausted(state, &provider, &model, until, management);
    }
}

/// A key served `model` again, so it isn't cooling, and the model isn't out.
fn recovered(provider: &str, model: &str, index: &str) {
    let target = (provider.to_string(), model.to_string());
    let mut guard = quota().lock().unwrap();
    let quota = &mut *guard;
    if let Some(cooling) = quota.cooling.get_mut(&target) {
        cooling.remove(index);
    }
    quota.out.remove(&target);
}

/// Every key for `model` is cooling: noted with the agents whose calls it
/// stopped, and the Baker told, since switching models is its decision.
fn quota_exhausted(
    state: &AppState,
    provider: &str,
    model: &str,
    until: chrono::DateTime<chrono::Utc>,
    management: &str,
) {
    // The calls the limit stopped are in the usage queue: count them first.
    read_usage(state, management);
    let agents: Vec<String> = ledger(state)
        .iter()
        .filter(|((agent, p, m), totals)| {
            p == provider && m == model && totals.last_status == "429" && agent.contains(':')
        })
        .map(|((agent, _, _), _)| agent.clone())
        .collect();
    let until = until.to_rfc3339();
    let Ok(event) = crate::state::append(
        state,
        STREAM,
        "node",
        "model.quota_exhausted",
        json!({ "provider": provider, "model": model, "until": until, "agents": agents })
            .to_string(),
        vec![],
    ) else {
        return;
    };
    let Some(baker) = crate::baker_threads::baker_pointer(state) else {
        return;
    };
    if agents.is_empty() {
        return;
    }
    let stopped: Vec<String> = agents
        .iter()
        .map(|agent| match agent.rsplit_once(':') {
            Some((stream_id, seat)) => format!("stream_id {stream_id}, seat {seat}"),
            None => agent.clone(),
        })
        .collect();
    let text = format!(
        "The model proxy is out of quota for {model} ({provider}): every key is cooling until {until}. \
         These agents' calls stopped:\n- {}\n\
         Switching models is your decision. Either switch each of them with seat_assign to a model \
         of another provider, giving a reason and because \"{hash}\" (that hands each stopped \
         conversation over to its new model), or leave them to wait until then.",
        stopped.join("\n- "),
        hash = event.event_hash,
    );
    let _ = crate::baker_threads::message_thread(state, &baker, &text, "bakery");
}

/// After a switch made `because` of a `model.quota_exhausted`, hands the
/// conversation to its new model: when the seat's agent was one that quota
/// stopped, and its last turn failed on it, an ordinary queued message (by
/// `bakery`) says what happened and to carry on, so every gate still
/// applies to what it does next. Any other switch queues nothing.
pub(crate) fn hand_over(
    state: &AppState,
    stream_id: &str,
    seat: crate::node::Seat,
    because: &str,
    switch: (Option<&str>, &str, &str),
) {
    let (from, to, reason) = switch;
    let agent = crate::node::agent_id(stream_id, seat);
    let (out, tail) = {
        let heart = state.heart.lock().unwrap();
        let out = heart
            .events_of_family(STREAM, "model.quota_exhausted")
            .unwrap_or_default()
            .into_iter()
            .find(|e| e.event_hash == because);
        (
            out,
            heart.read_stream_tail(stream_id, 200).unwrap_or_default(),
        )
    };
    let Some(out) = out.and_then(|e| serde_json::from_str::<Value>(&e.payload).ok()) else {
        return;
    };
    if !out["agents"]
        .as_array()
        .is_some_and(|agents| agents.iter().any(|a| a == agent.as_str()))
    {
        return;
    }
    let ends = [
        "agent.turn.done",
        "agent.turn.failed",
        "agent.turn.exhausted",
        "agent.turn.cancelled",
    ];
    let last = tail
        .iter()
        .rev()
        .filter(|e| ends.contains(&e.family.as_str()))
        .filter_map(|e| Some((e, serde_json::from_str::<Value>(&e.payload).ok()?)))
        .find(|(_, p)| p["agent_id"] == agent.as_str());
    let Some((_, failed)) = last.filter(|(e, p)| {
        e.family == "agent.turn.failed"
            && p["reason"]
                .as_str()
                .is_some_and(|r| r.contains("model_cooldown") || r.contains("429"))
    }) else {
        return;
    };
    let text = format!(
        "The Baker switched this conversation to {to}{from}: {reason}\n\
         {model} stopped because every {provider} key is out of quota for it until {until}. \
         Carry on from where it stopped. The instruction it was working on:\n{instruction}",
        from = from.map(|f| format!(" from {f}")).unwrap_or_default(),
        model = out["model"].as_str().unwrap_or("its model"),
        provider = out["provider"].as_str().unwrap_or("provider"),
        until = out["until"].as_str().unwrap_or("later"),
        instruction = failed["instruction"].as_str().unwrap_or_default(),
    );
    let _ = if crate::baker_ops::is_baker_scope_stream(state, stream_id) {
        crate::baker_threads::message_thread(state, stream_id, &text, "bakery").map(drop)
    } else {
        crate::agent_control::enqueue_as(
            state,
            crate::routes::agent::AgentTurnReq {
                stream_id: stream_id.to_string(),
                file_path: None,
                instruction: text,
                seat: Some(seat),
                model_id: None,
                effort: None,
                actor: None,
            },
            vec![],
            false,
            "bakery",
        )
        .map(drop)
    };
}

// ─── Egress ─────────────────────────────────────────────────────────────────

/// Whether the proxy's upstream call may reach `authority`: a provider's
/// API host on 443. A refused host and port are noted in the Heart once
/// (never a path or a header), so what else the proxy tries to reach is on
/// record.
pub(crate) fn egress_allows(state: &AppState, authority: &str) -> bool {
    if crate::seat_gateway::allowed(authority, PROVIDER_HOSTS, 443) {
        return true;
    }
    static NOTED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let authority = authority.to_ascii_lowercase();
    if NOTED
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .insert(authority.clone())
    {
        let (host, port) = authority.rsplit_once(':').unwrap_or((&authority, ""));
        note(
            state,
            "model_proxy.egress_refused",
            json!({ "host": host, "port": port }),
        );
    }
    false
}

// ─── Routes ─────────────────────────────────────────────────────────────────

fn status(state: &AppState) -> ProxyStatus {
    ProxyStatus {
        installed: installed(state),
        version: VERSION.to_string(),
        port: port(),
        egress_hosts: PROVIDER_HOSTS.iter().map(|h| h.to_string()).collect(),
        ..status_cell().lock().unwrap().clone()
    }
}

/// `GET /model-proxy`: whether the proxy is installed and running.
pub(crate) async fn proxy_status(State(st): State<AppState>) -> Json<ProxyStatus> {
    Json(status(&st))
}

/// `POST /model-proxy/start`: the operator starts the installed proxy again,
/// after fixing what stopped it. Hidden from agents.
pub(crate) async fn start_proxy(
    State(st): State<AppState>,
) -> Result<Json<ProxyStatus>, HttpError> {
    if !installed(&st) {
        return Err((
            StatusCode::CONFLICT,
            "the model proxy isn't installed: install it first".to_string(),
        ));
    }
    start(st.clone());
    Ok(Json(status(&st)))
}

/// `POST /model-proxy/install`: the operator installs the pinned release
/// (its sha256 checked), and the node starts it. Hidden from agents.
pub(crate) async fn install_proxy(
    State(st): State<AppState>,
) -> Result<Json<ProxyStatus>, HttpError> {
    let state = st.clone();
    crate::actor::spawn_blocking(move || install(&state))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map_err(|e| (StatusCode::CONFLICT, e))?;
    start(st.clone());
    Ok(Json(status(&st)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> (tempfile::TempDir, AppState) {
        let dir = tempfile::tempdir().unwrap();
        let state =
            crate::testutil::state_with(dir.path(), std::sync::Arc::new(bakery_agent::StubModel));
        (dir, state)
    }

    #[test]
    fn the_node_owns_a_loopback_proxy_with_no_plugins_and_its_own_sign_in_folder() {
        let (dir, state) = state();
        let owned = owned(&state, &["c".to_string()]);
        assert_eq!(owned["access"]["api-keys"], json!(["c"]), "never empty");
        assert_eq!(owned["server"]["host"], "127.0.0.1");
        assert_eq!(owned["server"]["port"], port());
        assert_eq!(owned["plugins"]["enabled"], false);
        assert_eq!(owned["management"]["allow-remote"], false);
        assert_eq!(
            owned["requests"]["proxy-url"],
            format!("http://127.0.0.1:{}", egress_port())
        );
        let auth_dir = owned["oauth"]["auth-dir"].as_str().unwrap();
        assert!(
            auth_dir.starts_with(&dir.path().display().to_string()),
            "{auth_dir}"
        );
        assert!(!auth_dir.contains(".cli-proxy-api"));
        let first = first_config(
            &state,
            &Secrets {
                management: "k".into(),
                client: "c".into(),
            },
        );
        assert_eq!(first["management"]["secret-key"], "k");
        assert_eq!(first["access"]["api-keys"], json!(["c"]));

        let room = room(&state);
        for rule in [
            "(deny default)",
            &format!(r#"(allow network-outbound (remote ip "localhost:{}"))"#, egress_port()),
            &format!(r#"(allow network-bind (local ip "localhost:{}"))"#, port()),
        ] {
            assert!(room.contains(rule), "missing {rule}: {room}");
        }
        assert!(!room.contains("SecurityServer"), "the Keychain stays out");
    }

    #[test]
    fn only_a_loopback_listener_passes() {
        let ok = listen_addresses(&format!("p123\nf6\nn127.0.0.1:{}\n", port()));
        assert!(listens_on_loopback_only(&ok));
        let wide = listen_addresses(&format!("p123\nf6\nn*:{}\n", port()));
        assert!(!listens_on_loopback_only(&wide));
        assert!(!listens_on_loopback_only(&[]));
        let both = listen_addresses(&format!("n127.0.0.1:{0}\nn[::1]:{0}\n", port()));
        assert!(!listens_on_loopback_only(&both));
    }

    #[test]
    fn a_long_proxy_log_moves_aside() {
        let (_dir, state) = state();
        let logs = dir(&state).join("logs");
        std::fs::create_dir_all(&logs).unwrap();
        std::fs::write(logs.join("proxy.log"), vec![b'x'; (16 << 20) + 1]).unwrap();
        trim_log(&state);
        assert_eq!(std::fs::metadata(logs.join("proxy.log")).unwrap().len(), 0);
        assert_eq!(
            std::fs::metadata(logs.join("proxy.log.1")).unwrap().len(),
            (16 << 20) + 1
        );
        std::fs::write(logs.join("proxy.log"), "short").unwrap();
        trim_log(&state);
        assert_eq!(
            std::fs::read_to_string(logs.join("proxy.log")).unwrap(),
            "short"
        );
    }

    #[test]
    fn a_download_that_isnt_the_pinned_release_installs_nothing() {
        let (_dir, state) = state();
        let err =
            install_archive(&state, "x.tar.gz", b"not the release", &"0".repeat(64)).unwrap_err();
        assert!(err.contains("not the pinned"), "{err}");
        assert!(!installed(&state));
    }

    #[test]
    fn the_egress_reaches_provider_hosts_only_and_notes_the_rest_once() {
        let (_dir, state) = state();
        assert!(egress_allows(&state, "api.anthropic.com:443"));
        assert!(!egress_allows(&state, "api.anthropic.com:80"));
        assert!(!egress_allows(&state, "w2-test.example.org:443"));
        assert!(!egress_allows(&state, "w2-test.example.org:443"));
        let events = state.heart.lock().unwrap().read_stream(STREAM).unwrap();
        let noted = |host: &str| {
            events
                .iter()
                .filter(|e| e.family == "model_proxy.egress_refused" && e.payload.contains(host))
                .count()
        };
        assert_eq!(noted("w2-test.example.org"), 1, "noted once");
        assert_eq!(noted("\"port\":\"80\""), 1, "with its port");
    }

    #[test]
    fn a_dated_or_prefixed_name_is_the_same_model_and_another_model_is_not() {
        for (a, b) in [
            ("claude-sonnet-4-5", "claude-sonnet-4-5-20250929"),
            ("claude-sonnet-4-5", "anthropic/Claude-Sonnet-4-5"),
            ("gemini-2.5-pro", "gemini-2.5-pro-latest"),
            ("gemini-2.0-flash", "gemini-2.0-flash-001"),
            ("gpt-4o", "gpt-4o-2024-08-06"),
            ("claude-opus-4-1(high)", "claude-opus-4-1"),
            ("", "grok-4"),
        ] {
            assert!(same_model(a, b) && same_model(b, a), "{a} / {b}");
        }
        for (a, b) in [
            ("claude-sonnet-4-5", "claude-haiku-4-5"),
            ("gemini-2.5-pro", "gemini-2.5-flash"),
            ("claude-opus-4", "claude-opus-4-1"),
        ] {
            assert!(!same_model(a, b) && !same_model(b, a), "{a} / {b}");
        }
    }

    fn usage_record(
        token: &str,
        model: &str,
        asked: &str,
        served: &str,
        failed: Option<u64>,
    ) -> Value {
        json!({
            "api_key": token,
            "provider": "claude",
            "model": model,
            "alias": asked,
            "response_model": served,
            "failed": failed.is_some(),
            "fail": { "status_code": failed.unwrap_or(200), "body": "upstream said sk-w2-not-kept" },
            "tokens": { "input_tokens": 10, "output_tokens": 5, "reasoning_tokens": 1, "cached_tokens": 2 },
            "latency_ms": 1500,
            "timestamp": "2026-10-08T10:00:00Z",
            "auth_index": "i-a",
        })
    }

    /// Each record is a call of the agent whose token it carried; the copy
    /// the Heart keeps holds neither the token nor the provider's error, and
    /// the calls add up in the metrics, label text escaped.
    #[test]
    fn each_call_lands_on_its_agent_and_adds_up() {
        let owners = HashMap::from([
            ("tok-a".to_string(), "s1:main".to_string()),
            ("tok-n".to_string(), "node".to_string()),
        ]);
        let names = HashMap::from([(
            "i-a".to_string(),
            (Provider::Claude, "work key".to_string()),
        )]);
        let sonnet = "claude-sonnet-4-5";
        let ok = call_of(
            &usage_record("tok-a", sonnet, sonnet, "claude-sonnet-4-5-20250929", None),
            &owners,
            &names,
        );
        assert_eq!((ok.agent.as_str(), ok.status.as_str()), ("s1:main", "ok"));
        assert_eq!(ok.key, "work key", "the key that served it, by its label");
        assert!(!ok.off_pin(), "a dated snapshot is the model asked for");
        let limited = call_of(
            &usage_record("tok-a", sonnet, sonnet, "", Some(429)),
            &owners,
            &names,
        );
        assert_eq!(limited.status, "429");
        let swapped = call_of(
            &usage_record(
                "tok-n",
                "claude-haiku-4-5",
                sonnet,
                "claude-haiku-4-5",
                None,
            ),
            &owners,
            &names,
        );
        assert_eq!(
            (swapped.agent.as_str(), swapped.asked.as_str()),
            ("node", sonnet)
        );
        assert!(swapped.off_pin());
        assert_eq!(
            call_of(
                &usage_record("tok-x", sonnet, sonnet, "", None),
                &owners,
                &names
            )
            .agent,
            "unknown"
        );

        let kept = serde_json::to_string(&ok).unwrap();
        assert!(
            !kept.contains("tok-a") && !kept.contains("sk-w2-not-kept"),
            "{kept}"
        );
        assert_eq!(serde_json::from_str::<Call>(&kept).unwrap(), ok);

        let mut ledger = Ledger::new();
        for call in [&ok, &limited, &swapped] {
            add(&mut ledger, call);
        }
        let text = metrics_text(true, &ledger);
        let at = r#"agent="s1:main",provider="claude",model="claude-sonnet-4-5""#;
        for line in [
            "bakery_model_proxy_up 1".to_string(),
            format!(r#"bakery_model_requests_total{{{at},status="ok"}} 1"#),
            format!(r#"bakery_model_requests_total{{{at},status="429"}} 1"#),
            format!(r#"bakery_model_tokens_total{{{at},kind="input"}} 20"#),
            format!("bakery_model_latency_seconds_sum{{{at}}} 3"),
            format!("bakery_model_latency_seconds_count{{{at}}} 2"),
            r#"bakery_model_pin_mismatch_total{agent="node",provider="claude",model="claude-haiku-4-5"} 1"#.to_string(),
        ] {
            assert!(text.lines().any(|l| l == line), "missing {line}:\n{text}");
        }
        let mut odd = Ledger::new();
        add(
            &mut odd,
            &Call {
                agent: "a\"b\\c\nd".into(),
                status: "ok".into(),
                ..Default::default()
            },
        );
        let text = metrics_text(false, &odd);
        assert!(text.contains(r#"agent="a\"b\\c\nd""#), "{text}");
        assert!(text.contains("bakery_model_proxy_up 0"));
    }

    /// Runs one of these tests again as a child process, one child at a
    /// time: each is a whole test binary with a stand-in proxy of its own,
    /// and several at once under a full suite's load starved one another
    /// past their time limits.
    fn as_child(test: &str) -> (bool, String) {
        static ONE_CHILD: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _turn = ONE_CHILD.lock().unwrap_or_else(|p| p.into_inner());
        let out = Command::new(std::env::current_exe().unwrap())
            .args([test, "--exact", "--nocapture", "--test-threads=1"])
            .env("BAKERY_TEST_SENTINEL_CHILD", "1")
            .output()
            .unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out.status.success(), text)
    }

    /// A child's first connection to its stand-in, waited out once
    /// (`testutil::first_connection`) before its flows run.
    fn first_connection(base: &str) {
        crate::testutil::first_connection(base.trim_start_matches("http://").parse().unwrap());
    }

    /// An agent's token is made once and given to the proxy before the node
    /// keeps it; a proxy that won't take it leaves the agent on the node's
    /// token. Every queued call lands on its agent, however many are
    /// waiting, and no token reaches the Heart. Runs as a child process, so
    /// the stand-in proxy it points the node at touches no other test.
    #[test]
    fn each_agents_calls_land_on_it_and_no_token_is_kept() {
        if std::env::var_os("BAKERY_TEST_SENTINEL_CHILD").is_some() {
            return usage_flows();
        }
        let (passed, text) =
            as_child("model_proxy::tests::each_agents_calls_land_on_it_and_no_token_is_kept");
        assert!(
            passed && text.contains("1 passed"),
            "the child failed:\n{text}"
        );
    }

    fn usage_flows() {
        use axum::response::IntoResponse;
        use std::sync::Arc;
        let patched: Arc<Mutex<Vec<Value>>> = Default::default();
        let queue: Arc<Mutex<Vec<Value>>> = Default::default();
        let refuse = Arc::new(AtomicBool::new(false));
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let listener = runtime
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (patched_by, queue_by, refuse_by) = (patched.clone(), queue.clone(), refuse.clone());
        let app = axum::Router::new().fallback(move |req: axum::extract::Request| {
            let (patched, queue, refuse) =
                (patched_by.clone(), queue_by.clone(), refuse_by.clone());
            async move {
                let (parts, body) = req.into_parts();
                let body = axum::body::to_bytes(body, 1 << 20)
                    .await
                    .unwrap_or_default();
                match (parts.method.as_str(), parts.uri.path()) {
                    ("PATCH", "/v8/management/config") if refuse.load(Ordering::SeqCst) => {
                        StatusCode::INTERNAL_SERVER_ERROR.into_response()
                    }
                    ("PATCH", "/v8/management/config") => {
                        patched
                            .lock()
                            .unwrap()
                            .push(serde_json::from_slice(&body).unwrap_or_default());
                        Json(json!({ "status": "ok" })).into_response()
                    }
                    ("GET", "/v1/models") => {
                        let bearer = parts
                            .headers
                            .get("authorization")
                            .and_then(|v| v.to_str().ok())
                            .and_then(|v| v.strip_prefix("Bearer "))
                            .map(|t| json!(t));
                        let answers = patched.lock().unwrap().last().is_some_and(|p| {
                            p["access"]["api-keys"].as_array().is_some_and(|keys| {
                                bearer.as_ref().is_some_and(|b| keys.contains(b))
                            })
                        });
                        if answers {
                            Json(json!({ "data": [] })).into_response()
                        } else {
                            StatusCode::UNAUTHORIZED.into_response()
                        }
                    }
                    ("GET", "/v8/management/observability/usage/queue") => {
                        let count = parts
                            .uri
                            .query()
                            .and_then(|q| q.strip_prefix("count="))
                            .and_then(|n| n.parse::<usize>().ok())
                            .unwrap_or(1);
                        let mut queue = queue.lock().unwrap();
                        let count = count.min(queue.len());
                        let taken: Vec<Value> = queue.drain(..count).collect();
                        Json(Value::Array(taken)).into_response()
                    }
                    _ => StatusCode::NOT_FOUND.into_response(),
                }
            }
        });
        runtime.spawn(async move { axum::serve(listener, app).await.unwrap() });
        std::env::set_var("BAKERY_TEST_PROXY_BASE", &base);
        let began = Instant::now();
        first_connection(&base);

        let ready_after = began.elapsed();
        let (_dir, state) = state();
        let secrets = prepare(&state).unwrap();
        let agent = "w2-usage:main";
        assert_eq!(agent_token(&state, agent), None, "the proxy isn't running");
        set_status(true, true, None);
        assert_eq!(
            agent_token(&state, agent),
            None,
            "it serves no provider yet"
        );
        state.node.lock().unwrap().model_proxy.keys.push(KeyRecord {
            provider: Provider::Claude,
            label: "k".into(),
            added: String::new(),
            fingerprint: "f".into(),
        });
        let token = agent_token(&state, agent).unwrap_or_else(|| {
            // Why there's none, so a red run says: the stand-in's own answer
            // to the same calls, and what it saw.
            let patch = send(
                "PATCH",
                &format!("{}/v8/management/config", base_url()),
                &secrets.management,
                json!({ "access": { "api-keys": client_tokens(&state) } }),
            );
            panic!(
                "a token of its own: none; the stand-in first answered after {ready_after:?}; minting halted {}; a PATCH now: {patch:?}; the stand-in saw {:?}",
                MINTING_HALTED.load(Ordering::SeqCst),
                patched.lock().unwrap()
            )
        });
        assert_eq!(
            agent_token(&state, agent).as_deref(),
            Some(token.as_str()),
            "made once"
        );
        assert_eq!(
            *patched.lock().unwrap(),
            vec![json!({ "access": { "api-keys": [secrets.client.clone(), token.clone()] } })],
            "the proxy takes every token, the node's first"
        );
        refuse.store(true, Ordering::SeqCst);
        assert_eq!(
            agent_token(&state, "w2-usage:other"),
            None,
            "refused: the node's token"
        );
        refuse.store(false, Ordering::SeqCst);
        assert!(!client_tokens(&state).iter().any(|t| t.is_empty()));
        assert_eq!(client_tokens(&state).len(), 2, "a refused token isn't kept");
        assert_eq!(
            agent_token(&state, "w2-usage:third"),
            None,
            "no new token until the proxy starts again"
        );
        assert_eq!(
            agent_token(&state, agent).as_deref(),
            Some(token.as_str()),
            "kept ones still answer"
        );
        MINTING_HALTED.store(false, Ordering::SeqCst);
        assert!(
            agent_token(&state, "w2-usage:third").is_some(),
            "a new run mints again"
        );
        state
            .node
            .lock()
            .unwrap()
            .model_proxy
            .agent_tokens
            .remove("w2-usage:third");

        let sonnet = "claude-sonnet-4-5";
        {
            let mut queue = queue.lock().unwrap();
            for _ in 0..201 {
                queue.push(usage_record(&token, sonnet, sonnet, "", None));
            }
            queue.push(usage_record(&token, sonnet, sonnet, "", Some(429)));
            queue.push(usage_record(&token, "claude-haiku-4-5", sonnet, "", None));
            queue.push(usage_record(&secrets.client, sonnet, sonnet, "", None));
        }
        read_usage(&state, &secrets.management);
        assert!(
            queue.lock().unwrap().is_empty(),
            "every waiting call is read"
        );
        let usage = agent_usage(&state, agent).expect("its calls");
        assert_eq!(
            (usage.calls, usage.failed, usage.pin_mismatches),
            (203, 1, 1)
        );
        assert_eq!((usage.tokens_in, usage.tokens_out), (2030, 1015));
        assert_eq!(agent_usage(&state, "node").map(|u| u.calls), Some(1));
        assert!(agent_usage(&state, "w2-usage:other").is_none());

        let events = state.heart.lock().unwrap().read_stream(STREAM).unwrap();
        let of = |family: &str| events.iter().filter(|e| e.family == family).count();
        assert_eq!(of("model.usage"), 204);
        assert_eq!(of("model.pin_mismatch"), 1);
        for event in &events {
            for secret in [&token, &secrets.client, &secrets.management] {
                assert!(
                    !event.payload.contains(secret.as_str()),
                    "{} carries a token",
                    event.family
                );
            }
        }
    }

    #[test]
    fn a_key_is_named_as_the_proxy_names_it() {
        // sha256("<list>:+<key>")[..8], as the proxy computes it.
        assert_eq!(
            auth_index(Provider::Claude, "sk-ant-w2-example"),
            "e65a4db452ae51dd"
        );
        assert_eq!(
            auth_index(Provider::Xai, " xai-w2-example "),
            "30c7e99eddae4724"
        );
    }

    #[test]
    fn the_redis_protocol_reads_as_the_proxy_writes_it() {
        let mut input = std::io::Cursor::new(
            b"+OK\r\n*3\r\n$9\r\nsubscribe\r\n$6\r\nerrors\r\n:1\r\n\
              *3\r\n$7\r\nmessage\r\n$6\r\nerrors\r\n$7\r\n{\"a\":1}\r\n-ERR nope\r\n"
                .to_vec(),
        );
        assert_eq!(read_resp(&mut input), Ok(vec!["OK".to_string()]));
        assert_eq!(
            read_resp(&mut input),
            Ok(vec!["subscribe".into(), "errors".into(), "1".into()])
        );
        assert_eq!(read_resp(&mut input).unwrap()[2], "{\"a\":1}");
        assert_eq!(read_resp(&mut input), Err("ERR nope".to_string()));
        assert!(read_resp(&mut input).is_err(), "the end of the connection");
        assert_eq!(
            resp_command(&["AUTH", "k"]),
            b"*2\r\n$4\r\nAUTH\r\n$1\r\nk\r\n"
        );
    }

    /// At each start the proxy is given only the tokens of conversations
    /// that still exist and aren't archived.
    #[test]
    fn a_gone_or_archived_conversations_token_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let repo = crate::testutil::make_repo(&dir.path().join("repo"));
        let state =
            crate::testutil::state_with(dir.path(), std::sync::Arc::new(bakery_agent::StubModel));
        let open = |name: &str| {
            crate::routes::project::open_stream(&state, &crate::testutil::open_req(&repo, name))
                .unwrap()
                .context
                .stream_id
        };
        let (live, archived) = (open("live"), open("archived"));
        crate::state::append(
            &state,
            &archived,
            "human",
            crate::agents::SOCKET_ARCHIVED_FAMILY,
            json!({ "archived": true }).to_string(),
            vec![],
        )
        .unwrap();
        let tokens = [
            (format!("{live}:orchestrator"), "t1"),
            (format!("{archived}:orchestrator"), "t2"),
            ("w2-nowhere:orchestrator".to_string(), "t3"),
        ];
        state.node.lock().unwrap().model_proxy.agent_tokens = tokens
            .iter()
            .map(|(a, t)| (a.clone(), t.to_string()))
            .collect();
        prune_agent_tokens(&state);
        let kept: Vec<String> = state
            .node
            .lock()
            .unwrap()
            .model_proxy
            .agent_tokens
            .keys()
            .cloned()
            .collect();
        assert_eq!(kept, vec![format!("{live}:orchestrator")]);
    }

    /// The proxy's pushed cooldowns: each key's is noted by its label; once
    /// every key for the model is cooling, the model is out, with the agents
    /// it stopped, and the Baker is told. The Baker's switch `because` of
    /// that hands the stopped conversation over; a switch without it, or
    /// for another reason, hands nothing over. Runs as a child process: the
    /// usage it reads comes from a stand-in proxy.
    #[test]
    fn quota_runs_out_the_baker_hears_and_its_switch_hands_over() {
        if std::env::var_os("BAKERY_TEST_SENTINEL_CHILD").is_some() {
            return quota_flows();
        }
        let (passed, text) = as_child(
            "model_proxy::tests::quota_runs_out_the_baker_hears_and_its_switch_hands_over",
        );
        assert!(
            passed && text.contains("1 passed"),
            "the child failed:\n{text}"
        );
    }

    fn quota_flows() {
        use axum::response::IntoResponse;
        use std::sync::Arc;
        // The usage queue, on a stand-in for the proxy's HTTP side.
        let queue: Arc<Mutex<Vec<Value>>> = Default::default();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let listener = runtime
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let queue_by = queue.clone();
        let app = axum::Router::new().fallback(move |req: axum::extract::Request| {
            let queue = queue_by.clone();
            async move {
                if req.uri().path() != "/v8/management/observability/usage/queue" {
                    return StatusCode::NOT_FOUND.into_response();
                }
                Json(Value::Array(std::mem::take(&mut *queue.lock().unwrap()))).into_response()
            }
        });
        runtime.spawn(async move { axum::serve(listener, app).await.unwrap() });
        std::env::set_var("BAKERY_TEST_PROXY_BASE", &base);
        first_connection(&base);

        let dir = tempfile::tempdir().unwrap();
        let repo = crate::testutil::make_repo(&dir.path().join("repo"));
        let state = crate::testutil::state_with(dir.path(), Arc::new(bakery_agent::StubModel));
        let secrets = prepare(&state).unwrap();
        let open = |name: &str| {
            crate::routes::project::open_stream(&state, &crate::testutil::open_req(&repo, name))
                .unwrap()
                .context
                .stream_id
        };
        let (worker, baker) = (open("worker"), open("baker"));
        state.node.lock().unwrap().baker_stream_id = Some(baker.clone());
        let seat = crate::node::Seat::Orchestrator;
        let agent = crate::node::agent_id(&worker, seat);
        state
            .node
            .lock()
            .unwrap()
            .model_proxy
            .agent_tokens
            .insert(agent.clone(), "tok-q".into());
        *key_names().lock().unwrap() = HashMap::from([
            ("i1".to_string(), (Provider::Xai, "first".to_string())),
            ("i2".to_string(), (Provider::Xai, "second".to_string())),
        ]);
        crate::state::assign_seat_model(&state, &worker, seat, "grok-4").unwrap();
        crate::state::append(
            &state,
            &worker,
            "node",
            "agent.turn.failed",
            json!({
                "agent_id": agent,
                "reason": "xAI API 429: {\"error\":{\"code\":\"model_cooldown\"}}",
                "instruction": "fix the build",
            })
            .to_string(),
            vec![],
        )
        .unwrap();
        queue.lock().unwrap().push(json!({
            "api_key": "tok-q",
            "provider": "xai",
            "model": "grok-4",
            "failed": true,
            "fail": { "status_code": 429 },
            "auth_index": "i2",
            "timestamp": "2026-10-08T10:00:00Z",
        }));

        // The proxy's errors channel: two keys run out; a 500 isn't quota.
        let push = |index: &str, status: u64, until: &str| {
            json!({
                "provider": "xai",
                "model": "grok-4",
                "auth_index": index,
                "status_code": status,
                "auth_status": { "model": {
                    "name": "grok-4",
                    "quota": { "exceeded": status == 429, "next_recover_at": until },
                } },
            })
            .to_string()
        };
        let pushes = [
            push("i1", 500, ""),
            push("i1", 429, "2030-01-01T00:10:00Z"),
            push("i2", 429, "2030-01-01T00:05:00Z"),
        ];
        let resp = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = resp.local_addr().unwrap().to_string();
        let server = std::thread::spawn(move || {
            use std::io::Write;
            let (conn, _) = resp.accept().unwrap();
            let mut reader = std::io::BufReader::new(conn.try_clone().unwrap());
            let mut writer = conn;
            let mut commands = Vec::new();
            for reply in ["+OK\r\n", "*3\r\n$9\r\nsubscribe\r\n$6\r\nerrors\r\n:1\r\n"] {
                commands.push(read_resp(&mut reader).unwrap());
                writer.write_all(reply.as_bytes()).unwrap();
            }
            for body in pushes {
                let message = format!(
                    "*3\r\n$7\r\nmessage\r\n$6\r\nerrors\r\n${}\r\n{body}\r\n",
                    body.len()
                );
                writer.write_all(message.as_bytes()).unwrap();
            }
            commands
        });
        let ended = follow_errors(&state, &address, &secrets.management);
        let commands = server.join().unwrap();
        assert!(ended.is_err(), "it follows until the connection ends");
        assert!(
            commands[0] == ["AUTH", secrets.management.as_str()],
            "it signs in with the management key"
        );
        assert_eq!(commands[1], ["SUBSCRIBE", "errors"]);

        let events = state.heart.lock().unwrap().read_stream(STREAM).unwrap();
        let of = |family: &str| -> Vec<Value> {
            events
                .iter()
                .filter(|e| e.family == family)
                .map(|e| serde_json::from_str(&e.payload).unwrap())
                .collect()
        };
        let cooling = of("model.key_cooling");
        assert_eq!(
            cooling
                .iter()
                .map(|c| c["key"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["first", "second"],
            "each quota cooldown, by the key's label; the 500 isn't one"
        );
        let out = of("model.quota_exhausted");
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(out[0]["agents"], json!([agent]));
        assert_eq!(
            out[0]["until"], "2030-01-01T00:05:00+00:00",
            "the soonest key back"
        );
        let hash = events
            .iter()
            .find(|e| e.family == "model.quota_exhausted")
            .unwrap()
            .event_hash
            .clone();
        let queued = |stream: &str| -> Vec<Value> {
            state
                .heart
                .lock()
                .unwrap()
                .read_stream(stream)
                .unwrap()
                .into_iter()
                .filter(|e| e.family == "agent.follow_up.queued")
                .map(|e| serde_json::from_str(&e.payload).unwrap())
                .collect()
        };
        let told = queued(&baker);
        assert_eq!(told.len(), 1, "the Baker is told once");
        let told = told[0]["instruction"].as_str().unwrap();
        assert!(
            told.contains(&hash) && told.contains("grok-4") && told.contains(&worker),
            "{told}"
        );

        let switch = |because: Option<String>| {
            runtime
                .block_on(crate::routes::seats::assign_seat(
                    State(state.clone()),
                    Json(crate::routes::seats::SeatAssignReq {
                        stream_id: worker.clone(),
                        seat,
                        model_id: "stub".into(),
                        provider: None,
                        reason: Some("grok-4 is out of quota".into()),
                        because,
                    }),
                ))
                .map(|Json(event)| event)
                .unwrap()
        };
        let made = switch(Some(hash.clone()));
        let recorded: crate::node::SeatAssignment = serde_json::from_str(&made.payload).unwrap();
        assert_eq!(recorded.reason.as_deref(), Some("grok-4 is out of quota"));
        assert_eq!(recorded.from.as_deref(), Some("grok-4"));
        assert_eq!(
            made.causality,
            vec![hash.clone()],
            "the quota signal caused it"
        );
        let handed = queued(&worker);
        assert_eq!(handed.len(), 1, "{handed:?}");
        let text = handed[0]["instruction"].as_str().unwrap();
        assert!(
            text.contains("Carry on") && text.contains("fix the build") && text.contains("grok-4"),
            "{text}"
        );
        assert_eq!(handed[0]["actor"], "bakery");
        switch(None);
        switch(Some(made.event_hash.clone()));
        assert_eq!(
            queued(&worker).len(),
            1,
            "no other switch hands anything over"
        );
    }

    const SENTINEL: &str = "sk-w2-SENTINEL-c0ffee-never-shown";

    /// A planted key never shows up in an answer, the Heart, the node's own
    /// output, or any file the node writes; the node never calls the
    /// proxy's OAuth or sign-in API; and the proxy's OAuth folder stays
    /// empty. The node's output is this test's own, run again as a child
    /// process so it can be read whole.
    #[test]
    fn a_key_never_shows_up_anywhere() {
        if std::env::var_os("BAKERY_TEST_SENTINEL_CHILD").is_some() {
            return sentinel_flows();
        }
        let (passed, text) = as_child("model_proxy::tests::a_key_never_shows_up_anywhere");
        let shown = text.replace(SENTINEL, "<THE KEY>");
        assert!(passed, "the child failed:\n{shown}");
        assert!(
            shown.contains("1 passed"),
            "the child ran nothing:\n{shown}"
        );
        assert!(
            !text.contains(SENTINEL),
            "the node printed the key:\n{shown}"
        );
    }

    fn sentinel_flows() {
        use axum::response::IntoResponse;
        use std::sync::Arc;
        // A stand-in for the proxy's management API, holding Claude's keys.
        let seen: Arc<Mutex<Vec<String>>> = Default::default();
        let held: Arc<Mutex<Vec<Value>>> = Default::default();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let listener = runtime
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (seen_by, held_by) = (seen.clone(), held.clone());
        let app = axum::Router::new().fallback(move |req: axum::extract::Request| {
            let (seen, held) = (seen_by.clone(), held_by.clone());
            async move {
                let (parts, body) = req.into_parts();
                let path = parts.uri.path().to_string();
                seen.lock().unwrap().push(path.clone());
                let body = axum::body::to_bytes(body, 1 << 20)
                    .await
                    .unwrap_or_default();
                let list = "/v0/management/claude-api-key";
                match (parts.method.as_str(), path.as_str()) {
                    ("GET", p) if p == list => {
                        Json(json!({ "claude-api-key": held.lock().unwrap().clone() }))
                            .into_response()
                    }
                    // A failing proxy whose error quotes what it was sent.
                    ("PUT", p) if p == list && String::from_utf8_lossy(&body).contains("FAIL") => (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        String::from_utf8_lossy(&body).into_owned(),
                    )
                        .into_response(),
                    ("PUT", p) if p == list => {
                        *held.lock().unwrap() = serde_json::from_slice(&body).unwrap_or_default();
                        Json(json!({ "status": "ok" })).into_response()
                    }
                    ("DELETE", p) if p == list => {
                        let at = parts
                            .uri
                            .query()
                            .and_then(|q| q.strip_prefix("index="))
                            .and_then(|i| i.parse::<usize>().ok())
                            .unwrap_or(usize::MAX);
                        let mut held = held.lock().unwrap();
                        if at < held.len() {
                            held.remove(at);
                        }
                        Json(json!({ "status": "ok" })).into_response()
                    }
                    ("GET", p) if p.ends_with("-api-key") => {
                        let name = p.trim_start_matches("/v0/management/").to_string();
                        Json(json!({ name: [] })).into_response()
                    }
                    _ => StatusCode::NOT_FOUND.into_response(),
                }
            }
        });
        runtime.spawn(async move { axum::serve(listener, app).await.unwrap() });
        std::env::set_var("BAKERY_TEST_PROXY_BASE", &base);
        first_connection(&base);

        let (dir, state) = state();
        prepare(&state).unwrap();
        set_status(true, true, None);
        let answer = |result: Result<Json<ModelKeys>, HttpError>| match result {
            Ok(Json(keys)) => serde_json::to_string(&keys).unwrap(),
            Err((code, why)) => format!("{code} {why}"),
        };
        let add = |key: String| {
            answer(runtime.block_on(add_key_route(
                State(state.clone()),
                Json(AddKey {
                    provider: Provider::Claude,
                    key,
                    label: "planted".into(),
                }),
            )))
        };
        let remove = |id: String| {
            answer(runtime.block_on(remove_key_route(
                State(state.clone()),
                Json(RemoveKey {
                    provider: Provider::Claude,
                    id,
                }),
            )))
        };
        let mut answers = vec![add(SENTINEL.to_string())];
        // When it didn't land, the add's own answer says why (and never
        // carries the key).
        assert!(
            held.lock()
                .unwrap()
                .first()
                .is_some_and(|e| e["api-key"] == SENTINEL),
            "the key reached the proxy: {}",
            answers[0]
        );
        assert_eq!(
            bakery_agent::proxy::route("claude").map(|(b, _)| b),
            Some(base.clone()),
            "Claude's calls go through the proxy now"
        );
        answers.push(add(SENTINEL.to_string()));
        answers.push(add(format!("{SENTINEL}-FAIL")));
        answers.push(add(format!("{SENTINEL} with a space")));
        let listed = model_keys(&state);
        answers.push(serde_json::to_string(&listed).unwrap());
        let id = listed.providers[0].keys[0].id.clone();
        answers.push(remove(id.clone()));
        answers.push(remove(id));
        assert!(held.lock().unwrap().is_empty(), "removed from the proxy");
        assert_eq!(bakery_agent::proxy::route("claude"), None, "direct again");

        for (n, text) in answers.iter().enumerate() {
            assert!(!text.contains(SENTINEL), "answer {n} carries the key");
        }
        assert!(answers[1].contains("already"), "a second add is refused");
        assert!(answers[2].contains("answered 500"), "its error is scrubbed");
        assert!(answers[6].contains("no longer"), "a gone key says so");
        let events = state.heart.lock().unwrap().read_stream(STREAM).unwrap();
        for event in &events {
            assert!(
                !event.payload.contains(SENTINEL),
                "{} carries the key",
                event.family
            );
        }
        fn walk(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
            for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, out);
                } else {
                    out.push(path);
                }
            }
        }
        let mut files = Vec::new();
        walk(dir.path(), &mut files);
        for file in files {
            let text =
                String::from_utf8_lossy(&std::fs::read(&file).unwrap_or_default()).into_owned();
            assert!(
                !text.contains(SENTINEL),
                "{} carries the key",
                file.display()
            );
        }
        let calls = seen.lock().unwrap().clone();
        assert!(
            calls
                .iter()
                .all(|p| !p.contains("oauth") && !p.contains("login") && !p.contains("auth-url")),
            "{calls:?}"
        );
        let oauth = std::fs::read_dir(dir.path().join("proxy/oauth")).unwrap();
        assert_eq!(oauth.count(), 0, "the subscription stays out");
    }

    /// The real binary in its room, with no network beyond 127.0.0.1: it
    /// starts, listens on loopback only, takes the node's configuration and
    /// loads no plugin.
    ///   BAKERY_PROXY_BINARY=<cli-proxy-api of the pinned release> cargo test -p bakery-node model_proxy -- --ignored
    #[test]
    #[ignore = "needs the pinned CLIProxyAPI binary"]
    fn live_the_proxy_runs_in_its_room_on_loopback() {
        let (_dir, state) = state();
        let source = std::env::var("BAKERY_PROXY_BINARY").unwrap();
        std::fs::create_dir_all(bin_dir(&state)).unwrap();
        std::fs::copy(source, binary(&state)).unwrap();
        let secrets = prepare(&state).unwrap();
        stop_leftover(&state);
        let mut child = spawn(&state).unwrap();
        let health = format!("{}/healthz", base_url());
        let deadline = Instant::now() + Duration::from_secs(20);
        while get(&health, None).is_err() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(250));
        }
        let addresses = listening(child.id());
        let configured = configure(&state, &secrets);
        let models = format!("{}/v1/models", base_url());
        let refused = get(&models, None).is_err();
        let answered = get(&models, Some(&secrets.client)).is_ok();
        let _ = child.kill();
        let _ = child.wait();
        let log = std::fs::read_to_string(dir(&state).join("logs/proxy.log")).unwrap_or_default();
        assert!(listens_on_loopback_only(&addresses), "{addresses:?}\n{log}");
        assert_eq!(configured, Ok(()));
        assert!(refused, "no client token, no answer");
        assert!(answered, "the node's own token answers");
    }

    /// The real proxy takes an agent's token at once (the node's own still
    /// answers), and a call that token makes lands on the agent, read from
    /// the proxy's own usage queue. The call reaches nothing: its key is
    /// made up, and nothing listens at the egress.
    ///   BAKERY_PROXY_BINARY=<cli-proxy-api of the pinned release> cargo test -p bakery-node model_proxy -- --ignored --test-threads=1
    #[test]
    #[ignore = "needs the pinned CLIProxyAPI binary"]
    fn live_an_agents_token_answers_and_its_call_lands_on_it() {
        let (_dir, state) = state();
        let source = std::env::var("BAKERY_PROXY_BINARY").unwrap();
        std::fs::create_dir_all(bin_dir(&state)).unwrap();
        std::fs::copy(source, binary(&state)).unwrap();
        let secrets = prepare(&state).unwrap();
        stop_leftover(&state);
        let mut child = spawn(&state).unwrap();
        let health = format!("{}/healthz", base_url());
        let deadline = Instant::now() + Duration::from_secs(20);
        while get(&health, None).is_err() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(250));
        }
        let configured = configure(&state, &secrets);
        set_status(true, true, None);
        let added = add_key(
            &state,
            Provider::Claude,
            "sk-ant-w2-made-up-not-a-key",
            "live",
        );
        let agent = "w2-live:main";
        let token = agent_token(&state, agent);
        let models = format!("{}/v1/models", base_url());
        let listed = token.as_deref().map(|t| get(&models, Some(t)));
        let answered = listed.as_ref().map(Result::is_ok);
        // A model the proxy itself lists: it knows older ones only by dated ids.
        let model = listed
            .and_then(Result::ok)
            .and_then(|l| l["data"][0]["id"].as_str().map(str::to_string))
            .unwrap_or_default();
        let node_answers = get(&models, Some(&secrets.client)).is_ok();
        // The errors channel, subscribed as the node does, before the call.
        let pushed = {
            use std::io::Write;
            let address = base_url().trim_start_matches("http://").to_string();
            let stream = std::net::TcpStream::connect(address).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(30)))
                .unwrap();
            let mut writer = stream.try_clone().unwrap();
            let mut reader = std::io::BufReader::new(stream);
            let mut replies = Vec::new();
            for command in [
                &["AUTH", secrets.management.as_str()][..],
                &["SUBSCRIBE", "errors"],
            ] {
                writer.write_all(&resp_command(command)).unwrap();
                replies.push(read_resp(&mut reader));
            }
            (replies, reader)
        };
        let called = token.as_deref().map(|t| {
            let call = ureq::post(&format!("{}/v1/messages", base_url()))
                .timeout(Duration::from_secs(60))
                .set("x-api-key", t)
                .set("anthropic-version", "2023-06-01")
                .send_json(json!({
                    "model": model,
                    "max_tokens": 8,
                    "messages": [{ "role": "user", "content": "hi" }],
                }));
            match call {
                Ok(r) => (r.status(), String::new()),
                Err(ureq::Error::Status(code, r)) => (code, r.into_string().unwrap_or_default()),
                Err(e) => (0, e.to_string()),
            }
        });
        let deadline = Instant::now() + Duration::from_secs(10);
        while agent_usage(&state, agent).is_none() && Instant::now() < deadline {
            read_usage(&state, &secrets.management);
            std::thread::sleep(Duration::from_millis(250));
        }
        let usage = agent_usage(&state, agent);
        let (replies, mut reader) = pushed;
        let error = read_resp(&mut reader);
        let calls = state.heart.lock().unwrap().read_stream(STREAM).unwrap();
        let _ = child.kill();
        let _ = child.wait();
        set_status(false, false, None);
        bakery_agent::proxy::set(None);
        let log = std::fs::read_to_string(dir(&state).join("logs/proxy.log")).unwrap_or_default();
        assert_eq!(configured, Ok(()));
        assert_eq!(added, Ok(()));
        assert_eq!(answered, Some(true), "its token answers at once\n{log}");
        assert!(node_answers, "the node's own still answers");
        assert!(
            called
                .as_ref()
                .is_some_and(|(code, _)| *code != 401 && *code != 403),
            "the proxy took the agent's call: {called:?}\n{log}"
        );
        let usage =
            usage.unwrap_or_else(|| panic!("the call landed on no agent ({called:?})\n{log}"));
        assert_eq!((usage.calls, usage.failed), (1, 1), "{usage:?}");
        let call = calls.iter().find(|e| e.family == "model.usage").unwrap();
        assert!(
            call.payload.contains(r#""key":"live""#),
            "the key by its label: {}",
            call.payload
        );
        assert!(
            replies.iter().all(Result::is_ok),
            "the management key opens the errors channel\n{log}"
        );
        let error = error.unwrap_or_else(|e| panic!("nothing pushed: {e}\n{log}"));
        let event: Value = serde_json::from_str(&error[2]).unwrap();
        assert_eq!(event["provider"], "claude", "{event}");
        assert_eq!(event["model"], model, "{event}");
        assert_eq!(
            event["auth_index"],
            auth_index(Provider::Claude, "sk-ant-w2-made-up-not-a-key"),
            "the node names the key as the proxy does"
        );
    }

    /// The real management API takes a key, holds it, and gives it back by
    /// fingerprint; its log never shows the key, and once removed, neither
    /// does its config.
    ///   BAKERY_PROXY_BINARY=<cli-proxy-api of the pinned release> cargo test -p bakery-node model_proxy -- --ignored
    #[test]
    #[ignore = "needs the pinned CLIProxyAPI binary"]
    fn live_a_key_goes_in_and_out_by_fingerprint() {
        let (_dir, state) = state();
        let source = std::env::var("BAKERY_PROXY_BINARY").unwrap();
        std::fs::create_dir_all(bin_dir(&state)).unwrap();
        std::fs::copy(source, binary(&state)).unwrap();
        let secrets = prepare(&state).unwrap();
        stop_leftover(&state);
        let mut child = spawn(&state).unwrap();
        let health = format!("{}/healthz", base_url());
        let deadline = Instant::now() + Duration::from_secs(20);
        while get(&health, None).is_err() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(250));
        }
        let configured = configure(&state, &secrets);
        set_status(true, true, None);
        let added = add_key(&state, Provider::Claude, SENTINEL, "live");
        let held = entries(Provider::Claude, &secrets.management)
            .map(|e| e.len())
            .unwrap_or_default();
        let again = add_key(&state, Provider::Claude, SENTINEL, "live");
        let id = records(&state).first().map(|r| r.fingerprint.clone());
        let removed = id.map(|id| remove_key(&state, Provider::Claude, &id));
        let after = entries(Provider::Claude, &secrets.management)
            .map(|e| e.len())
            .unwrap_or(usize::MAX);
        std::thread::sleep(Duration::from_millis(500));
        let _ = child.kill();
        let _ = child.wait();
        set_status(false, false, None);
        let log = std::fs::read_to_string(dir(&state).join("logs/proxy.log")).unwrap_or_default();
        let config = std::fs::read_to_string(dir(&state).join("config.yaml")).unwrap_or_default();
        assert_eq!(configured, Ok(()));
        assert_eq!(added, Ok(()));
        assert_eq!(held, 1, "the proxy holds it");
        assert!(again.is_err(), "a second add is refused");
        assert_eq!(removed, Some(Ok(())));
        assert_eq!(after, 0, "and gives it back");
        assert!(!log.contains(SENTINEL), "its log never shows the key");
        assert!(!config.contains(SENTINEL), "removed from its config");
        let events = state.heart.lock().unwrap().read_stream(STREAM).unwrap();
        assert!(events.iter().all(|e| !e.payload.contains(SENTINEL)));
    }
}
