//! Native desktop ↔ node connection (desktop-only bootstrap).
//!
//! Bakery is a desktop app: the shell and its node are two processes owned
//! by the same human on the same machine. This module is the whole
//! connection mechanism between them, and it is deliberately narrow:
//!
//! 1. Reuse a node that is already listening — never start a second one,
//!    and **never kill anything**. The only lifecycle action here is
//!    starting a node when none answers.
//! 2. Read the operator credential from `~/.bakery/node.json` (0600, owned
//!    by this user, on `bakery_exec`'s sandbox deny list so no agent room
//!    can read it). The credential travels process→process through this
//!    Rust code and one IPC reply to the trusted window — never a URL,
//!    never a log line, never an agent's environment, never a frontend
//!    build variable.
//! 3. Verify the credential against the running node before reporting
//!    success, so a stale/foreign token surfaces as one honest message
//!    instead of scattered 401s all over the UI.
//!
//! The HTTP client here is ~40 lines of loopback GET rather than a
//! dependency: two fixed requests to 127.0.0.1 is not worth a TLS stack.

use serde::Serialize;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Where the node listens unless `BAKERY_NODE_ADDR` says otherwise — the
/// same default `bakery-node`'s own `main.rs` uses, so both sides agree
/// without either configuring the other.
pub const DEFAULT_ADDR: &str = "127.0.0.1:7717";

/// The one window allowed to ask for the operator credential. Tauri's
/// capability file scopes the command to this label too; this is the
/// runtime half of the same rule, so a webview created later (a preview
/// pane, a devtools window, anything an agent could cause to exist) cannot
/// reach operator access by calling the command directly.
pub const TRUSTED_WINDOW: &str = "main";

#[derive(Debug, Clone, Serialize)]
pub struct Connected {
    pub addr: String,
    /// The operator bearer, for this window's own HTTP calls. Held in
    /// memory by the client; never persisted, never logged.
    pub token: String,
    /// True only when THIS launch started the node (nothing was listening).
    pub started_node: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    /// A window that is not [`TRUSTED_WINDOW`] asked for the credential.
    Untrusted,
    /// Nothing is listening and we could not start one.
    Unreachable,
    /// No `node.json` / no `operator_token` in it.
    NoCredential,
    /// The node is up but rejected our credential.
    Unauthorized,
    /// Something about this machine's setup blocks the connection.
    Misconfigured,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConnectError {
    pub kind: ErrorKind,
    /// One actionable sentence for the connection screen. Never contains a
    /// credential — asserted by test.
    pub message: String,
}

impl ConnectError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self { kind, message: message.into() }
    }
}

pub fn window_is_trusted(label: &str) -> bool {
    label == TRUSTED_WINDOW
}

/// Both sides resolve the address the same way, so neither has to tell the
/// other where to listen.
pub fn resolve_addr() -> String {
    match std::env::var("BAKERY_NODE_ADDR") {
        Ok(a) if !a.trim().is_empty() => a.trim().to_string(),
        _ => DEFAULT_ADDR.to_string(),
    }
}

pub fn bakery_home() -> Option<PathBuf> {
    std::env::var("HOME").ok().filter(|h| !h.trim().is_empty()).map(|h| PathBuf::from(h).join(".bakery"))
}

/// Minimal loopback GET: returns the response's status code. Deliberately
/// ignores the body — both probes here are status-only.
fn http_status(addr: &str, path: &str, bearer: Option<&str>, timeout: Duration) -> std::io::Result<u16> {
    let sock = addr
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "no address"))?;
    let mut stream = TcpStream::connect_timeout(&sock, timeout)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;

    let mut req = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n");
    if let Some(b) = bearer {
        req.push_str(&format!("Authorization: Bearer {b}\r\n"));
    }
    req.push_str("\r\n");
    stream.write_all(req.as_bytes())?;

    // The status line is all we need; stop as soon as we have one.
    let mut head = Vec::new();
    let mut buf = [0u8; 512];
    loop {
        let n = stream.read(&mut buf)?;
        if n == 0 {
            break;
        }
        head.extend_from_slice(&buf[..n]);
        if head.contains(&b'\n') || head.len() > 8192 {
            break;
        }
    }
    let line = String::from_utf8_lossy(&head);
    let status = line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "no status line"))?;
    Ok(status)
}

pub fn probe_health(addr: &str, timeout: Duration) -> bool {
    matches!(http_status(addr, "/health", None, timeout), Ok(200))
}

/// Prove the credential works before reporting a connection — this is what
/// turns "every request 401s mysteriously" into one named failure.
pub fn verify(addr: &str, token: &str, timeout: Duration) -> Result<(), ConnectError> {
    match http_status(addr, "/node", Some(token), timeout) {
        Ok(200) => Ok(()),
        Ok(401) | Ok(403) => Err(ConnectError::new(
            ErrorKind::Unauthorized,
            "The node is running but rejected this machine's stored operator credential. \
             That happens when the node was started with BAKERY_OPERATOR_TOKEN set (its token then \
             differs from ~/.bakery/node.json), or when node.json was replaced while the node kept \
             running. Restart bakery-node without BAKERY_OPERATOR_TOKEN, then retry.",
        )),
        Ok(code) => Err(ConnectError::new(
            ErrorKind::Unreachable,
            format!("The node answered the credential check with an unexpected status {code}. It may be mid-restart — retry."),
        )),
        Err(e) => Err(ConnectError::new(
            ErrorKind::Unreachable,
            format!("Lost the connection while checking the operator credential ({e}). Retry."),
        )),
    }
}

pub fn read_operator_token(base: &Path) -> Result<String, ConnectError> {
    let path = base.join("node.json");
    let text = std::fs::read_to_string(&path).map_err(|e| {
        ConnectError::new(
            ErrorKind::NoCredential,
            format!(
                "Could not read the node's configuration at {} ({e}). bakery-node writes that file the \
                 first time it starts — start the node once, then retry.",
                path.display()
            ),
        )
    })?;
    let parsed: serde_json::Value = serde_json::from_str(&text).map_err(|e| {
        ConnectError::new(
            ErrorKind::Misconfigured,
            format!("The node's configuration at {} is not valid JSON ({e}).", path.display()),
        )
    })?;
    let token = parsed.get("operator_token").and_then(|t| t.as_str()).unwrap_or("").trim().to_string();
    if token.is_empty() {
        return Err(ConnectError::new(
            ErrorKind::NoCredential,
            format!(
                "The node's configuration at {} has no operator credential yet. Start bakery-node once \
                 (it mints one on load), then retry.",
                path.display()
            ),
        ));
    }
    Ok(token)
}

/// Where a node binary might be, in the order a desktop launch should try:
/// an explicit override, then next to the app itself (the bundled layout),
/// then a development checkout's build output.
pub fn find_node_binary() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("BAKERY_NODE_BIN") {
        let p = PathBuf::from(p.trim());
        if p.is_file() {
            return Some(p);
        }
    }
    let name = if cfg!(windows) { "bakery-node.exe" } else { "bakery-node" };
    let mut starts: Vec<PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            // Bundled: the node ships beside the app binary.
            let sibling = dir.join(name);
            if sibling.is_file() {
                return Some(sibling);
            }
            starts.push(dir.to_path_buf());
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        starts.push(cwd);
    }
    // Development: walk up to a checkout root and use its build output.
    for start in starts {
        for ancestor in start.ancestors() {
            for profile in ["debug", "release"] {
                let candidate = ancestor.join("target").join(profile).join(name);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

/// Start a node. Never called while one is answering, so this can only ever
/// ADD a process — it has no path that stops one.
fn spawn_node(bin: &Path, base: &Path) -> Result<(), ConnectError> {
    let _ = std::fs::create_dir_all(base);
    let log_path = base.join("node.log");
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let log = opts.open(&log_path).map_err(|e| {
        ConnectError::new(
            ErrorKind::Misconfigured,
            format!("Could not open {} for the node's output ({e}).", log_path.display()),
        )
    })?;
    let err_log = log
        .try_clone()
        .map_err(|e| ConnectError::new(ErrorKind::Misconfigured, format!("Could not prepare the node's log ({e}).")))?;

    std::process::Command::new(bin)
        .stdin(std::process::Stdio::null())
        .stdout(log)
        .stderr(err_log)
        .spawn()
        .map_err(|e| {
            ConnectError::new(
                ErrorKind::Misconfigured,
                format!("Could not start the node at {} ({e}).", bin.display()),
            )
        })?;
    Ok(())
}

fn wait_for_health(addr: &str, wait: Duration, timeout: Duration) -> bool {
    let deadline = Instant::now() + wait;
    loop {
        if probe_health(addr, timeout) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Last few lines of the node's log — the actionable half of an
/// "it did not come up" message. Bounded, and the node never prints a
/// credential (asserted by `bakery-node`'s own tests).
fn log_tail(base: &Path, lines: usize) -> Option<String> {
    let text = std::fs::read_to_string(base.join("node.log")).ok()?;
    let tail: Vec<&str> = text.lines().rev().take(lines).collect();
    let tail: Vec<&str> = tail.into_iter().rev().collect();
    let joined = tail.join("\n");
    (!joined.trim().is_empty()).then_some(joined)
}

pub struct Options {
    pub base_dir: PathBuf,
    pub addr: String,
    /// False in tests that must prove no process is ever started.
    pub allow_spawn: bool,
    pub node_bin: Option<PathBuf>,
    pub probe_timeout: Duration,
    pub spawn_wait: Duration,
}

impl Options {
    pub fn desktop() -> Result<Self, ConnectError> {
        let base_dir = bakery_home().ok_or_else(|| {
            ConnectError::new(ErrorKind::Misconfigured, "No HOME directory is set, so Bakery cannot find ~/.bakery.")
        })?;
        Ok(Self {
            base_dir,
            addr: resolve_addr(),
            allow_spawn: true,
            node_bin: None,
            probe_timeout: Duration::from_secs(2),
            spawn_wait: Duration::from_secs(20),
        })
    }
}

/// The whole bootstrap: reuse → (start if absent) → read credential →
/// verify → connected.
pub fn connect(opts: &Options) -> Result<Connected, ConnectError> {
    let mut started_node = false;

    if !probe_health(&opts.addr, opts.probe_timeout) {
        if opts.allow_spawn {
            match opts.node_bin.clone().or_else(find_node_binary) {
                Some(bin) => {
                    spawn_node(&bin, &opts.base_dir)?;
                    started_node = wait_for_health(&opts.addr, opts.spawn_wait, opts.probe_timeout);
                }
                None => {
                    // Someone may have started one in the meantime — check
                    // before claiming there is no node to be had.
                    if !probe_health(&opts.addr, opts.probe_timeout) {
                        return Err(ConnectError::new(
                            ErrorKind::Unreachable,
                            format!(
                                "No node is listening on {} and no bakery-node binary was found to start. \
                                 Build it (cargo build -p bakery-node) or set BAKERY_NODE_BIN to its path, then retry.",
                                opts.addr
                            ),
                        ));
                    }
                }
            }
        }
        if !probe_health(&opts.addr, opts.probe_timeout) {
            let mut message = format!(
                "The Bakery node is not answering on {}. If something else is using that port, free it or set \
                 BAKERY_NODE_ADDR for both the node and this app.",
                opts.addr
            );
            if let Some(tail) = log_tail(&opts.base_dir, 8) {
                message.push_str(&format!("\n\nThe node's last output was:\n{tail}"));
            }
            return Err(ConnectError::new(ErrorKind::Unreachable, message));
        }
    }

    let token = read_operator_token(&opts.base_dir)?;
    verify(&opts.addr, &token, opts.probe_timeout)?;
    Ok(Connected { addr: opts.addr.clone(), token, started_node })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufRead;
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;

    /// A stand-in node: answers `/health` and `/node` exactly the way the
    /// real one does for this bootstrap's two probes, so these tests
    /// exercise the real connect path end to end over real TCP.
    struct StubNode {
        addr: String,
        stop: Arc<AtomicBool>,
        hits: Arc<AtomicUsize>,
    }

    impl StubNode {
        fn start(expected_token: Option<&str>) -> Self {
            Self::start_on(None, expected_token)
        }

        /// `port: Some(p)` re-binds a specific port — how the restart test
        /// brings "the same node" back.
        fn start_on(port: Option<u16>, expected_token: Option<&str>) -> Self {
            let listener = TcpListener::bind(("127.0.0.1", port.unwrap_or(0))).expect("bind stub node");
            let addr = listener.local_addr().expect("stub addr").to_string();
            listener.set_nonblocking(true).unwrap();
            let stop = Arc::new(AtomicBool::new(false));
            let hits = Arc::new(AtomicUsize::new(0));
            let expected = expected_token.map(str::to_string);
            let stop_thread = stop.clone();
            let hits_thread = hits.clone();
            std::thread::spawn(move || {
                while !stop_thread.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((mut sock, _)) => {
                            hits_thread.fetch_add(1, Ordering::Relaxed);
                            sock.set_nonblocking(false).ok();
                            let mut reader = std::io::BufReader::new(sock.try_clone().unwrap());
                            let mut request_line = String::new();
                            let _ = reader.read_line(&mut request_line);
                            let mut authorization = String::new();
                            loop {
                                let mut header = String::new();
                                if reader.read_line(&mut header).unwrap_or(0) == 0 {
                                    break;
                                }
                                if header.trim().is_empty() {
                                    break;
                                }
                                if let Some(rest) = header.to_ascii_lowercase().strip_prefix("authorization:") {
                                    authorization = rest.trim().to_string();
                                }
                            }
                            let path = request_line.split_whitespace().nth(1).unwrap_or("").to_string();
                            let status = if path == "/health" {
                                "200 OK"
                            } else if path == "/node" {
                                match &expected {
                                    Some(t) if authorization == format!("bearer {t}") => "200 OK",
                                    Some(_) => "401 Unauthorized",
                                    None => "200 OK",
                                }
                            } else {
                                "404 Not Found"
                            };
                            let _ = sock.write_all(
                                format!("HTTP/1.1 {status}\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}")
                                    .as_bytes(),
                            );
                        }
                        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(10));
                        }
                        Err(_) => break,
                    }
                }
            });
            StubNode { addr, stop, hits }
        }

        fn port(&self) -> u16 {
            self.addr.rsplit(':').next().unwrap().parse().unwrap()
        }

        fn stop(self) {
            self.stop.store(true, Ordering::Relaxed);
            // Give the accept loop a tick to release the port.
            std::thread::sleep(Duration::from_millis(80));
        }
    }

    fn base_with_token(dir: &Path, token: &str) -> PathBuf {
        let base = dir.join(".bakery");
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(
            base.join("node.json"),
            serde_json::json!({ "node_id": "test", "operator_token": token }).to_string(),
        )
        .unwrap();
        base
    }

    fn opts(base: PathBuf, addr: String) -> Options {
        Options {
            base_dir: base,
            addr,
            allow_spawn: false,
            node_bin: None,
            probe_timeout: Duration::from_secs(2),
            spawn_wait: Duration::from_secs(3),
        }
    }

    /// Fresh desktop launch: the app finds the node, reads the credential
    /// from disk (never a URL or a build variable), proves it works, and
    /// reports connected.
    #[test]
    fn fresh_launch_connects_using_the_stored_credential() {
        let dir = tempdir();
        let base = base_with_token(dir.path(), "tok-fresh");
        let node = StubNode::start(Some("tok-fresh"));

        let connected = connect(&opts(base, node.addr.clone())).expect("fresh launch must connect");
        assert_eq!(connected.token, "tok-fresh");
        assert_eq!(connected.addr, node.addr);
        assert!(!connected.started_node, "a node was already listening — nothing should have been started");
        node.stop();
    }

    /// Subsequent launch: a node is already listening, so the app reuses it
    /// and never starts a second one. Proven by pointing `node_bin` at a
    /// script that would leave a sentinel behind if it were ever run.
    #[cfg(unix)]
    #[test]
    fn a_subsequent_launch_reuses_the_running_node_and_starts_nothing() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempdir();
        let base = base_with_token(dir.path(), "tok-reuse");
        let node = StubNode::start(Some("tok-reuse"));

        let sentinel = dir.path().join("spawned");
        let script = dir.path().join("would-spawn.sh");
        std::fs::write(&script, format!("#!/bin/sh\ntouch {}\n", sentinel.display())).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let mut o = opts(base, node.addr.clone());
        o.allow_spawn = true;
        o.node_bin = Some(script);

        for launch in 1..=2 {
            let connected = connect(&o).unwrap_or_else(|e| panic!("launch {launch} must connect: {e:?}"));
            assert!(!connected.started_node);
        }
        assert!(!sentinel.exists(), "a reachable node must never be duplicated by a spawn");
        assert!(node.hits.load(Ordering::Relaxed) >= 4, "each launch probes health and verifies the credential");
        node.stop();
    }

    /// Node restart: while it is down the app says so honestly (and does
    /// nothing destructive); once it is back on the same address the very
    /// next attempt — what the Retry button does — reconnects.
    #[test]
    fn a_node_restart_is_reported_then_reconnects_on_retry() {
        let dir = tempdir();
        let base = base_with_token(dir.path(), "tok-restart");
        let node = StubNode::start(Some("tok-restart"));
        let addr = node.addr.clone();
        let port = node.port();

        assert!(connect(&opts(base.clone(), addr.clone())).is_ok());

        node.stop();
        let err = connect(&opts(base.clone(), addr.clone())).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Unreachable, "{err:?}");
        assert!(err.message.contains(&addr), "the message names where it looked: {}", err.message);

        let restarted = StubNode::start_on(Some(port), Some("tok-restart"));
        let connected = connect(&opts(base, addr)).expect("retry after the node comes back must connect");
        assert_eq!(connected.token, "tok-restart");
        restarted.stop();
    }

    /// Invalidated credential: the node is up but rejects what is on disk
    /// (classic cause: the node was started with BAKERY_OPERATOR_TOKEN).
    /// One named failure, with the actual fix — and never the credential
    /// itself in the message.
    #[test]
    fn an_invalidated_credential_is_named_with_its_fix_and_never_echoed() {
        let dir = tempdir();
        let base = base_with_token(dir.path(), "stale-token-value");
        let node = StubNode::start(Some("the-nodes-real-token"));

        let err = connect(&opts(base, node.addr.clone())).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Unauthorized, "{err:?}");
        assert!(err.message.contains("BAKERY_OPERATOR_TOKEN"), "{}", err.message);
        assert!(!err.message.contains("stale-token-value"), "a credential must never appear in a message");
        assert!(!err.message.contains("the-nodes-real-token"));
        node.stop();
    }

    /// A machine that has never run the node has no credential yet — say
    /// exactly that, not "unauthorized".
    #[test]
    fn a_machine_with_no_node_json_gets_a_setup_message_not_an_auth_error() {
        let dir = tempdir();
        let base = dir.path().join(".bakery");
        std::fs::create_dir_all(&base).unwrap();
        let node = StubNode::start(None);

        let err = connect(&opts(base, node.addr.clone())).unwrap_err();
        assert_eq!(err.kind, ErrorKind::NoCredential, "{err:?}");
        assert!(err.message.contains("bakery-node"), "{}", err.message);
        node.stop();
    }

    /// Nothing listening and nothing to start: an actionable message, and
    /// no attempt to take a port from whatever else might be there.
    #[test]
    fn an_unreachable_node_with_no_binary_explains_what_to_do() {
        let dir = tempdir();
        let base = base_with_token(dir.path(), "tok");
        // Bind a port and immediately release it so we have one that is
        // almost certainly free — nothing answers there.
        let free = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = free.local_addr().unwrap().to_string();
        drop(free);

        let mut o = opts(base, addr);
        o.allow_spawn = true;
        o.node_bin = Some(PathBuf::from("/nonexistent/bakery-node"));
        let err = connect(&o).unwrap_err();
        assert!(matches!(err.kind, ErrorKind::Unreachable | ErrorKind::Misconfigured), "{err:?}");
    }

    /// Only Bakery's own window may ask for the operator credential.
    #[test]
    fn only_the_trusted_window_may_request_the_credential() {
        assert!(window_is_trusted(TRUSTED_WINDOW));
        for other in ["preview", "devtools", "agent-view", "main2", ""] {
            assert!(!window_is_trusted(other), "{other} must not be trusted");
        }
    }

    #[test]
    fn the_address_resolves_the_same_way_the_node_resolves_it() {
        // No env set in this process: both sides land on the shared default.
        if std::env::var("BAKERY_NODE_ADDR").is_err() {
            assert_eq!(resolve_addr(), DEFAULT_ADDR);
        }
    }

    // Small tempdir helper — the app has no dev-dependency on tempfile and
    // this is all it needs.
    struct TempDir(PathBuf);
    impl TempDir {
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn tempdir() -> TempDir {
        // Counter, not just a timestamp: these tests run in parallel
        // threads of one process, and two of them landing in the same clock
        // tick would share a directory — each overwriting the other's
        // node.json, which looks exactly like a credential mismatch.
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!("bakery-connect-{}-{nanos}-{n}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }
}
