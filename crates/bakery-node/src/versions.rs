//! Settings → Versions and the Baker's `node_versions` op: what this node
//! actually runs, read from the thing that executes — this process, the
//! installed package manifests, each live session's own handshake — never
//! from a pinned constant or the lockfile. Newer releases come from an
//! on-demand check of the public npm registry. Nothing here installs.

use crate::claude_native::{
    InstalledAdapter, ACCOUNT_ID, ADAPTER_PACKAGE, ADAPTER_VERSION, MIN_NODE_MAJOR, SDK_PACKAGE,
};
use crate::state::AppState;
use axum::{extract::State, http::StatusCode, Json};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime};

static STARTED: OnceLock<SystemTime> = OnceLock::new();

/// Stamps this process's start, once, at boot. "Restart needed" compares the
/// sources against it — not against the binary on disk, which a `cargo test`
/// or `cargo build` refreshes under a node that keeps running the old code.
pub(crate) fn mark_started() {
    let _ = STARTED.set(SystemTime::now());
}

#[derive(schemars::JsonSchema, Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Status {
    Ok,
    UpdateAvailable,
    /// Installed, but not what Bakery requires — the node refuses to run it.
    Mismatch,
    Missing,
}

#[derive(schemars::JsonSchema, Debug, Clone, Serialize)]
pub(crate) struct Component {
    pub(crate) id: &'static str,
    pub(crate) label: &'static str,
    pub(crate) installed: Option<String>,
    /// What Bakery pins or requires, when it constrains this at all.
    pub(crate) required: Option<String>,
    /// From the last update check; `None` until one ran.
    pub(crate) latest: Option<String>,
    pub(crate) status: Status,
    /// Where `installed` was read, or why it couldn't be.
    pub(crate) source: String,
    pub(crate) update: &'static str,
}

#[derive(schemars::JsonSchema, Debug, Clone, Serialize)]
pub(crate) struct Build {
    pub(crate) started_at: Option<String>,
    pub(crate) source_root: Option<String>,
    pub(crate) branch: Option<String>,
    pub(crate) commit: Option<String>,
    pub(crate) uncommitted_files: Option<usize>,
    /// A Rust or Cargo file changed after this node started, so the code that
    /// runs is older than the checkout until the next restart.
    pub(crate) changed_since_start: Option<String>,
}

#[derive(schemars::JsonSchema, Debug, Clone, Serialize)]
pub(crate) struct Session {
    pub(crate) stream_id: String,
    pub(crate) seat: String,
    pub(crate) account_id: String,
    /// The runtime's own name and version from its ACP handshake.
    pub(crate) runtime: Option<String>,
    pub(crate) version: Option<String>,
    pub(crate) model: Option<String>,
    /// Started on an adapter other than the one installed now; it keeps
    /// running that one until the session or the node restarts.
    pub(crate) outdated: bool,
}

#[derive(schemars::JsonSchema, Debug, Clone, Serialize)]
pub(crate) struct Report {
    pub(crate) build: Build,
    pub(crate) components: Vec<Component>,
    pub(crate) sessions: Vec<Session>,
    pub(crate) checked_at: Option<String>,
    pub(crate) check_error: Option<String>,
}

const UPDATE_NODE: &str =
    "Outside Bakery: update Node.js with your version manager (brew, mise, nvm), then restart the node.";
const UPDATE_ADAPTER: &str = "Pinned and patched by Bakery. Upgrade pins the new release, moves the Bakery patch to it, installs, checks the adapter's handshake, then deploys with the tests; any failure puts everything back. If the patch no longer applies, it needs porting first (the Baker can do it). From a terminal: ./scripts/bakery.sh upgrade-adapter <version>.";
const UPDATE_WITH_ADAPTER: &str = "Ships inside the adapter: upgrading the adapter upgrades this.";

/// The newest published releases as of `checked_at`. One node, one cache.
#[derive(Debug, Clone)]
struct Latest {
    checked_at: Option<String>,
    error: Option<String>,
    adapter: Option<String>,
    sdk: Option<String>,
    claude_code: Option<String>,
}

static LATEST: Mutex<Latest> = Mutex::new(Latest {
    checked_at: None,
    error: None,
    adapter: None,
    sdk: None,
    claude_code: None,
});

/// The newest adapter release the last update check found — the only
/// version an Upgrade button may install.
pub(crate) fn latest_adapter() -> Option<String> {
    LATEST.lock().unwrap().adapter.clone()
}

/// Blocking: runs `node --version` and a few git reads.
pub(crate) fn report(state: &AppState) -> Report {
    let latest = LATEST.lock().unwrap().clone();
    let installed = crate::claude_native::installed_adapter(state);
    let installed = installed.as_ref();
    let adapter_version = installed.and_then(|a| a.version.as_deref());
    let sdk_source = || {
        installed
            .and_then(|a| a.sdk.as_ref())
            .map(|dir| dir.join("package.json").display().to_string())
            .unwrap_or_else(|| format!("{SDK_PACKAGE} not found beside the adapter"))
    };
    let sdk_version = installed.and_then(|a| a.sdk_version.clone());
    let claude_code = installed.and_then(|a| a.claude_code_version.clone());
    Report {
        build: build(state),
        components: vec![
            node_js(),
            adapter_component(installed, latest.adapter.as_deref()),
            Component {
                id: "claude-agent-sdk",
                label: "Claude Agent SDK",
                status: status_for(sdk_version.as_deref(), latest.sdk.as_deref()),
                installed: sdk_version,
                required: None,
                latest: latest.sdk.clone(),
                source: sdk_source(),
                update: UPDATE_WITH_ADAPTER,
            },
            Component {
                id: "claude-code",
                label: "Claude Code engine",
                status: status_for(claude_code.as_deref(), latest.claude_code.as_deref()),
                installed: claude_code,
                required: None,
                latest: latest.claude_code.clone(),
                source: sdk_source(),
                update: UPDATE_WITH_ADAPTER,
            },
        ],
        sessions: sessions(state, adapter_version),
        checked_at: latest.checked_at,
        check_error: latest.error,
    }
}

fn node_js() -> Component {
    let (installed, source) = match crate::state::resolve_executable_path("node") {
        Ok(path) => match crate::claude_native::node_version(&path) {
            Ok(version) => (Some(version), path.display().to_string()),
            Err(error) => (None, error),
        },
        Err(()) => (None, "node is not on PATH".to_string()),
    };
    let major = installed.as_deref().and_then(|v| {
        v.trim_start_matches('v')
            .split('.')
            .next()?
            .parse::<u64>()
            .ok()
    });
    let status = match (&installed, major) {
        (None, _) => Status::Missing,
        (Some(_), Some(major)) if major >= MIN_NODE_MAJOR => Status::Ok,
        _ => Status::Mismatch,
    };
    Component {
        id: "nodejs",
        label: "Node.js",
        installed,
        required: Some(format!(">= {MIN_NODE_MAJOR}")),
        latest: None,
        status,
        source,
        update: UPDATE_NODE,
    }
}

fn adapter_component(installed: Option<&InstalledAdapter>, latest: Option<&str>) -> Component {
    let version = installed.and_then(|a| a.version.clone());
    let status = match version.as_deref() {
        Some(v) if v != ADAPTER_VERSION => Status::Mismatch,
        v => status_for(v, latest),
    };
    Component {
        id: "claude-native-adapter",
        label: "Claude native adapter",
        installed: version,
        required: Some(format!("{ADAPTER_VERSION} (pinned)")),
        latest: latest.map(str::to_string),
        status,
        source: installed.map_or_else(
            || format!("{ADAPTER_PACKAGE} is not installed — run pnpm install in bakery-client"),
            |a| a.package.join("package.json").display().to_string(),
        ),
        update: UPDATE_ADAPTER,
    }
}

fn status_for(installed: Option<&str>, latest: Option<&str>) -> Status {
    match (installed, latest) {
        (None, _) => Status::Missing,
        (Some(have), Some(newest)) if older(have, newest) => Status::UpdateAvailable,
        _ => Status::Ok,
    }
}

/// Whether release `a` precedes `b` by dotted numeric parts ("0.76.0" <
/// "0.84.0"); versions that don't parse only count as older when different.
pub(crate) fn older(a: &str, b: &str) -> bool {
    let parts = |v: &str| {
        v.trim_start_matches('v')
            .split(['.', '-', '+'])
            .take(3)
            .map(str::parse::<u64>)
            .collect::<Result<Vec<_>, _>>()
    };
    match (parts(a), parts(b)) {
        (Ok(a), Ok(b)) => a < b,
        _ => a != b,
    }
}

/// The Bakery checkout: the root pinned in node.json, else the tree this
/// binary was built from.
pub(crate) fn source_root(state: &AppState) -> Option<PathBuf> {
    state
        .node
        .lock()
        .unwrap()
        .bakery_source_root
        .clone()
        .map(PathBuf::from)
        .or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .canonicalize()
                .ok()
        })
}

fn build(state: &AppState) -> Build {
    let root = source_root(state);
    let started = STARTED.get().copied();
    let git = |args: &[&str]| {
        let out = crate::git::run(root.as_deref()?, args).ok()?;
        let out = out.trim();
        (!out.is_empty()).then(|| out.to_string())
    };
    let changed = root
        .as_deref()
        .zip(started)
        .and_then(|(root, started)| changed_since(root, started))
        .map(|path| {
            let shown = root
                .as_deref()
                .and_then(|root| path.strip_prefix(root).ok());
            shown.unwrap_or(&path).display().to_string()
        });
    Build {
        started_at: started.map(|t| chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339()),
        branch: git(&["rev-parse", "--abbrev-ref", "HEAD"]),
        commit: git(&["rev-parse", "--short", "HEAD"]),
        uncommitted_files: root
            .as_deref()
            .and_then(|root| crate::git::porcelain_status(root).ok())
            .map(|status| status.len()),
        changed_since_start: changed,
        source_root: root.map(|root| root.display().to_string()),
    }
}

/// The first Rust source or Cargo file under `root` modified after `since`.
fn changed_since(root: &Path, since: SystemTime) -> Option<PathBuf> {
    let lock = root.join("Cargo.lock");
    newer_source(&root.join("crates"), since)
        .or_else(|| modified_after(&lock, since).then_some(lock))
}

fn newer_source(dir: &Path, since: SystemTime) -> Option<PathBuf> {
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            if path
                .file_name()
                .is_some_and(|n| n == "target" || n == "node_modules")
            {
                continue;
            }
            if let Some(found) = newer_source(&path, since) {
                return Some(found);
            }
        } else if (path.extension().is_some_and(|e| e == "rs")
            || path.file_name().is_some_and(|n| n == "Cargo.toml"))
            && modified_after(&path, since)
        {
            return Some(path);
        }
    }
    None
}

pub(crate) fn modified_after(path: &Path, since: SystemTime) -> bool {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .is_ok_and(|modified| modified > since)
}

fn sessions(state: &AppState, adapter_version: Option<&str>) -> Vec<Session> {
    // Clone the live handles out first: a snapshot takes the session's own
    // lock, which must never be taken under the registry's.
    let live: Vec<_> = state
        .acp_sessions
        .lock()
        .unwrap()
        .iter()
        .filter_map(|((stream_id, seat), slot)| match slot {
            crate::pipe::AcpSlot::Live(live) => Some((stream_id.clone(), *seat, live.clone())),
            _ => None,
        })
        .collect();
    let mut out: Vec<Session> = live
        .into_iter()
        .map(|(stream_id, seat, live)| {
            let snapshot = live.snapshot();
            let runtime = snapshot.runtime.as_ref();
            let outdated = snapshot.account_id == ACCOUNT_ID
                && runtime.is_some_and(|r| {
                    r.name.contains("claude-agent-acp")
                        && adapter_version.is_some_and(|installed| r.version != installed)
                });
            Session {
                stream_id,
                seat: seat.as_str().to_string(),
                account_id: snapshot.account_id.clone(),
                runtime: runtime.map(|r| r.title.clone().unwrap_or_else(|| r.name.clone())),
                version: runtime.map(|r| r.version.clone()),
                model: snapshot.effective_model.clone(),
                outdated,
            }
        })
        .collect();
    out.sort_by(|a, b| (&a.stream_id, &a.seat).cmp(&(&b.stream_id, &b.seat)));
    out
}

const REGISTRY: &str = "https://registry.npmjs.org";

/// One GET. A connection that fails — a network blip — gets one more try
/// after `pause`; an answer, even a refusal, is final.
fn get_with_retry(url: &str, pause: Duration) -> Result<ureq::Response, ureq::Error> {
    // Two minutes in tests: a fresh test binary's loopback connection can
    // wait on LuLu longer than ten seconds under a full suite's load.
    let limit = Duration::from_secs(if cfg!(test) { 120 } else { 10 });
    let get = || ureq::get(url).timeout(limit).call();
    match get() {
        Err(ureq::Error::Transport(_)) => {
            std::thread::sleep(pause);
            get()
        }
        answered => answered,
    }
}

/// One published manifest; `tag` is a dist-tag ("latest") or a version.
fn npm_manifest(package: &str, tag: &str) -> Result<serde_json::Value, String> {
    let url = format!("{REGISTRY}/{}/{tag}", package.replacen('/', "%2f", 1));
    match get_with_retry(&url, Duration::from_secs(3)) {
        Ok(response) => response
            .into_json()
            .map_err(|e| format!("npm registry ({package}): {e}")),
        Err(ureq::Error::Transport(_)) => Err(
            "couldn't reach the npm registry — you may be offline. Try again in a moment."
                .to_string(),
        ),
        Err(e) => Err(format!("npm registry ({package}): {e}")),
    }
}

/// Asks the public npm registry for the newest adapter and the SDK (and
/// Claude Code engine) it ships with. Blocking, and only ever on request —
/// never polled.
pub(crate) fn check_latest() {
    let now = chrono::Utc::now().to_rfc3339();
    let text = |value: &serde_json::Value, key: &str| {
        value
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    };
    let fetched = (|| -> Result<Latest, String> {
        let adapter = npm_manifest(ADAPTER_PACKAGE, "latest")?;
        // The adapter names the SDK release it ships (a range names its floor).
        let sdk = match adapter
            .get("dependencies")
            .and_then(|deps| deps.get(SDK_PACKAGE))
            .and_then(serde_json::Value::as_str)
        {
            Some(spec) => Some(npm_manifest(
                SDK_PACKAGE,
                spec.trim_start_matches(['^', '~', '=']),
            )?),
            None => None,
        };
        Ok(Latest {
            checked_at: Some(now.clone()),
            error: None,
            adapter: text(&adapter, "version"),
            sdk: sdk.as_ref().and_then(|sdk| text(sdk, "version")),
            claude_code: sdk.as_ref().and_then(|sdk| text(sdk, "claudeCodeVersion")),
        })
    })();
    let mut latest = LATEST.lock().unwrap();
    match fetched {
        Ok(fresh) => *latest = fresh,
        Err(error) => {
            latest.checked_at = Some(now);
            latest.error = Some(error);
        }
    }
}

pub(crate) async fn versions(
    State(st): State<AppState>,
) -> Result<Json<Report>, (StatusCode, String)> {
    crate::actor::spawn_blocking(move || report(&st))
        .await
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

pub(crate) async fn versions_check(
    State(st): State<AppState>,
) -> Result<Json<Report>, (StatusCode, String)> {
    crate::actor::spawn_blocking(move || {
        check_latest();
        report(&st)
    })
    .await
    .map(Json)
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dropped_connection_is_tried_once_more() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/pkg", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            // The first connection closes before any answer, like a blip.
            drop(listener.accept().unwrap());
            let (mut stream, _) = listener.accept().unwrap();
            let _ = stream.read(&mut [0u8; 1024]);
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}")
                .unwrap();
        });
        let response = get_with_retry(&url, Duration::from_millis(10)).unwrap();
        assert_eq!(response.status(), 200);
        server.join().unwrap();
    }

    #[test]
    fn release_order_is_numeric_not_textual() {
        assert!(older("0.76.0", "0.84.0"));
        assert!(older("0.9.0", "0.10.0"));
        assert!(older("2.1.257", "2.1.284"));
        assert!(!older("0.84.0", "0.84.0"));
        assert!(!older("0.84.1", "0.84.0"));
        assert!(older("v22.1.0", "v22.12.0"));
        assert!(older("weird", "other") && !older("same", "same"));
    }

    fn installed(version: &str) -> InstalledAdapter {
        InstalledAdapter {
            package: PathBuf::from("/x/claude-agent-acp"),
            version: Some(version.to_string()),
            sdk: None,
            sdk_version: None,
            claude_code_version: None,
        }
    }

    /// The pin is the gate: a different installed version is a mismatch the
    /// node refuses to run, even when it is newer; an update is only offered
    /// for the pinned version once a check has seen a newer release.
    #[test]
    fn the_adapter_is_judged_against_its_pin_then_the_last_check() {
        let status =
            |adapter: Option<&InstalledAdapter>, latest| adapter_component(adapter, latest).status;
        let pinned = installed(ADAPTER_VERSION);
        assert_eq!(status(None, None), Status::Missing);
        assert_eq!(status(Some(&installed("999.0.0")), None), Status::Mismatch);
        assert_eq!(status(Some(&pinned), None), Status::Ok);
        assert_eq!(status(Some(&pinned), Some(ADAPTER_VERSION)), Status::Ok);
        assert_eq!(
            status(Some(&pinned), Some("999.0.0")),
            Status::UpdateAvailable
        );
    }

    #[test]
    fn only_rust_and_cargo_changes_after_start_need_a_restart() {
        let root = tempfile::tempdir().unwrap();
        let src = root.path().join("crates/x/src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(root.path().join("crates/x/target")).unwrap();
        std::fs::write(root.path().join("crates/x/target/built.rs"), "").unwrap();
        std::fs::write(src.join("notes.md"), "").unwrap();
        assert_eq!(
            changed_since(root.path(), SystemTime::UNIX_EPOCH),
            None,
            "docs and build output never count"
        );

        std::fs::write(src.join("lib.rs"), "").unwrap();
        assert_eq!(
            changed_since(root.path(), SystemTime::UNIX_EPOCH),
            Some(src.join("lib.rs"))
        );
        let later = SystemTime::now() + Duration::from_secs(60);
        assert_eq!(
            changed_since(root.path(), later),
            None,
            "nothing changed after the start"
        );
    }
}
