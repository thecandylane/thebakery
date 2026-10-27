//! Settings → Versions → Bakery (D1): Deploy, Restart and Roll back as
//! buttons. The app in use runs from a deployed build in this node's own
//! home, `~/.bakery/deploy/current`, so agent edits change the source only
//! and the operator declares when the app changes. Agents can never be
//! granted that home, so they cannot swap what runs either.
//!
//! `POST /control/bakery` starts one allowlisted `scripts/bakery.sh` action —
//! never an argument a caller chose — detached in its own process group,
//! because every one of them stops this very node half-way through. The
//! deployed copy of the script runs when there is one: the working tree's
//! copy is agent-editable, and a button must not run code the operator never
//! deployed. The node that comes back learns how the action went from two
//! files in its home: `control.json` (what started, when, which pid),
//! written here, and `control.exit`, the exit code the script's EXIT trap
//! writes.
//!
//! `upgrade-adapter` (V1) takes the one argument no caller supplies: the
//! newest Claude adapter release this node's own update check found, and
//! only when it is newer than the pinned one.
//!
//! `deploy-ui` is the top bar's Hot reload: `scripts/bakery.sh deploy --ui`
//! builds only the UI and swaps it in. The open app reloads on it; this node
//! is not restarted and no turn is touched. Its `--ui` belongs to the action
//! ([`script_args`]); a caller can never add it to `deploy`, or anything else.

use crate::state::AppState;
use axum::{extract::State, http::StatusCode, Json};
use serde::{Deserialize, Serialize};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::SystemTime;

/// The only actions a button can start.
const ACTIONS: [&str; 5] = [
    "restart",
    "deploy",
    "deploy-ui",
    "rollback",
    "upgrade-adapter",
];
/// Under the node's home — where scripts/bakery.sh puts it too.
const DEPLOY_DIR: &str = "deploy";
const LOG_DIR: &str = ".bakery-dev-logs";
/// What a deploy builds, as git pathspecs — the same list as
/// `DEPLOY_SOURCES` in scripts/bakery.sh.
const DEPLOY_SOURCES: [&str; 6] = [
    "crates",
    "Cargo.toml",
    "Cargo.lock",
    "bakery-client",
    "scripts/bakery.sh",
    ":(exclude)bakery-client/e2e",
];
const LOG_TAIL_LINES: usize = 40;
const UNDEPLOYED_LISTED: usize = 20;

/// `build.json`, as the script writes it when a deploy begins.
#[derive(schemars::JsonSchema, Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct DeployedBuild {
    pub(crate) commit: Option<String>,
    pub(crate) built_at: Option<String>,
    /// Uncommitted files the build was made with.
    pub(crate) dirty_files: Option<usize>,
}

/// One button press, as recorded when it started.
#[derive(Debug, Serialize, Deserialize)]
struct Started {
    action: String,
    started_at: String,
    pid: u32,
}

#[derive(schemars::JsonSchema, Debug, Clone, Serialize)]
pub(crate) struct LastAction {
    pub(crate) action: String,
    pub(crate) started_at: String,
    pub(crate) running: bool,
    /// The script's own verdict: `None` while it runs, or when it ended
    /// without giving one (killed outright).
    pub(crate) ok: Option<bool>,
    /// Why, when it failed: `upgrade-adapter` exits 3 when the Bakery patch
    /// does not apply to the new release and needs porting.
    pub(crate) exit_code: Option<i32>,
    pub(crate) log_tail: Vec<String>,
}

#[derive(schemars::JsonSchema, Debug, Clone, Serialize)]
pub(crate) struct ControlStatus {
    pub(crate) deployed: Option<DeployedBuild>,
    /// What Roll back would switch to.
    pub(crate) previous: Option<DeployedBuild>,
    /// This node runs the deployed build, not one built from source.
    pub(crate) running_deployed: bool,
    /// Source files changed after the deployed build began, the first
    /// [`UNDEPLOYED_LISTED`] of them; empty when nothing is deployed.
    pub(crate) undeployed: Vec<String>,
    pub(crate) undeployed_count: usize,
    pub(crate) last: Option<LastAction>,
    /// A restart (a deploy's, say) waiting for running turns, with
    /// `POST /control/restart/now` to go ahead at once.
    pub(crate) restart: Option<crate::service::RestartWait>,
}

fn parse_action(action: &str) -> Option<&'static str> {
    ACTIONS.into_iter().find(|known| *known == action)
}

/// What an action passes to `scripts/bakery.sh`: its own name, except for
/// the UI-only deploy, which is `deploy --ui`.
fn script_args(action: &'static str) -> Vec<&'static str> {
    match action {
        "deploy-ui" => vec!["deploy", "--ui"],
        other => vec![other],
    }
}

fn read_build(slot: &Path) -> Option<DeployedBuild> {
    serde_json::from_slice(&std::fs::read(slot.join("build.json")).ok()?).ok()
}

/// Every deploy source git knows (tracked, or new and not ignored) modified
/// after `since` — build.json's own time, so an edit made while the build
/// ran counts as not deployed. A deletion has no time to compare, so it
/// only counts through the edits that usually come with it.
fn undeployed(root: &Path, since: SystemTime) -> Vec<String> {
    let mut args = vec![
        "ls-files",
        "-z",
        "--cached",
        "--others",
        "--exclude-standard",
        "--",
    ];
    args.extend(DEPLOY_SOURCES);
    let Ok(listed) = crate::git::run(root, &args) else {
        return Vec::new();
    };
    let mut changed: Vec<String> = listed
        .split('\0')
        .filter(|path| {
            !path.is_empty() && crate::versions::modified_after(&root.join(path), since)
        })
        .map(str::to_string)
        .collect();
    // An unmerged path is listed once per conflict stage.
    changed.sort();
    changed.dedup();
    changed
}

/// Signal 0 delivers nothing; it only asks whether the process exists.
fn alive(pid: u32) -> bool {
    libc::pid_t::try_from(pid).is_ok_and(|pid| pid > 0 && unsafe { libc::kill(pid, 0) } == 0)
}

fn log_path(root: &Path, action: &str) -> PathBuf {
    root.join(LOG_DIR).join(format!("control-{action}.log"))
}

fn log_tail(path: &Path) -> Vec<String> {
    let text = std::fs::read(path)
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(LOG_TAIL_LINES)..]
        .iter()
        .map(|line| line.to_string())
        .collect()
}

fn last_action(root: &Path, base: &Path) -> Option<LastAction> {
    let started: Started =
        serde_json::from_slice(&std::fs::read(base.join("control.json")).ok()?).ok()?;
    // The action names its log file, so only a known one is trusted.
    let action = parse_action(&started.action)?;
    let exit = std::fs::read_to_string(base.join("control.exit"))
        .ok()
        .and_then(|code| code.trim().parse::<i32>().ok());
    Some(LastAction {
        action: action.to_string(),
        started_at: started.started_at,
        running: exit.is_none() && alive(started.pid),
        ok: exit.map(|code| code == 0),
        exit_code: exit,
        log_tail: log_tail(&log_path(root, action)),
    })
}

/// What `upgrade-adapter` may install: the newest release the node's own
/// update check found (`latest`), never a caller's, and only a newer one.
fn upgrade_target(latest: Option<&str>) -> Result<String, (StatusCode, String)> {
    use crate::claude_native::{ADAPTER_PACKAGE, ADAPTER_VERSION};
    let latest = latest.ok_or_else(|| {
        (
            StatusCode::CONFLICT,
            "no update check has run since the node started — Check for updates first".to_string(),
        )
    })?;
    if !crate::adapter_pin::exact_version(latest) {
        return Err((
            StatusCode::BAD_GATEWAY,
            format!("the npm registry reported {latest:?}, which is not a release version"),
        ));
    }
    if !crate::versions::older(ADAPTER_VERSION, latest) {
        return Err((
            StatusCode::CONFLICT,
            format!("{ADAPTER_PACKAGE}@{ADAPTER_VERSION} is not older than the newest release found ({latest})"),
        ));
    }
    Ok(latest.to_string())
}

/// `root` is the checkout the builds come from; `base` is this node's home.
pub(crate) fn status(root: &Path, base: &Path) -> ControlStatus {
    let deploy = base.join(DEPLOY_DIR);
    let undeployed = std::fs::metadata(deploy.join("current/build.json"))
        .and_then(|meta| meta.modified())
        .map(|since| undeployed(root, since))
        .unwrap_or_default();
    let running_deployed = std::env::current_exe()
        .and_then(|exe| exe.canonicalize())
        .ok()
        .zip(deploy.canonicalize().ok())
        .is_some_and(|(exe, deploy)| exe.starts_with(deploy));
    ControlStatus {
        deployed: read_build(&deploy.join("current")),
        previous: read_build(&deploy.join("previous")),
        running_deployed,
        undeployed_count: undeployed.len(),
        undeployed: undeployed.into_iter().take(UNDEPLOYED_LISTED).collect(),
        last: last_action(root, base),
        restart: None,
    }
}

/// The deployed copy when there is one (see the module doc).
fn script(root: &Path, base: &Path) -> PathBuf {
    let deployed = base.join(DEPLOY_DIR).join("current/bakery.sh");
    if deployed.is_file() {
        deployed
    } else {
        root.join("scripts/bakery.sh")
    }
}

/// Held from the running check until the new action is on record, so two
/// presses can never both start.
static STARTING: Mutex<()> = Mutex::new(());

/// `version` is the one argument an action takes (`upgrade-adapter`'s), and
/// only ever from [`upgrade_target`].
fn start(
    root: &Path,
    base: &Path,
    action: &'static str,
    version: Option<&str>,
) -> Result<(), (StatusCode, String)> {
    let _one_at_a_time = STARTING.lock().unwrap();
    if let Some(last) = last_action(root, base).filter(|last| last.running) {
        return Err((
            StatusCode::CONFLICT,
            format!("{} is still running — wait for it to finish", last.action),
        ));
    }
    let failed = |what: &str, e: std::io::Error| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("could not {what}: {e}"),
        )
    };
    std::fs::create_dir_all(root.join(LOG_DIR)).map_err(|e| failed("create the log folder", e))?;
    let log = std::fs::File::create(log_path(root, action)).map_err(|e| failed("open the log", e))?;
    let err_log = log.try_clone().map_err(|e| failed("open the log", e))?;
    let exit_file = base.join("control.exit");
    let _ = std::fs::remove_file(&exit_file);
    let mut child = Command::new(script(root, base))
        .args(script_args(action))
        .args(version)
        .current_dir(root)
        // The deployed copy lives outside the checkout, so it is told which.
        .env("BAKERY_ROOT", root)
        .env("BAKERY_CONTROL_EXIT", &exit_file)
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(err_log)
        // Its own process group: the script stops this node and the app,
        // and must not go down with either.
        .process_group(0)
        .spawn()
        .map_err(|e| failed(&format!("start scripts/bakery.sh {action}"), e))?;
    let pid = child.id();
    // Reaped here if this node outlives it (a failed build), so a finished
    // script never lingers as a zombie that `alive` would count as running.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    let started = Started {
        action: action.to_string(),
        started_at: chrono::Utc::now().to_rfc3339(),
        pid,
    };
    std::fs::write(
        base.join("control.json"),
        serde_json::to_vec(&started).unwrap_or_default(),
    )
    .map_err(|e| failed("record the action", e))
}

fn source_root(st: &AppState) -> Result<PathBuf, (StatusCode, String)> {
    crate::versions::source_root(st).ok_or_else(|| {
        (
            StatusCode::CONFLICT,
            "this node does not know its Bakery checkout — set bakery_source_root in node.json"
                .to_string(),
        )
    })
}

pub(crate) async fn control_status(
    State(st): State<AppState>,
) -> Result<Json<ControlStatus>, (StatusCode, String)> {
    let root = source_root(&st)?;
    crate::actor::spawn_blocking(move || ControlStatus {
        restart: crate::service::restart_wait(&st),
        ..status(&root, &st.base)
    })
    .await
    .map(Json)
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct ControlReq {
    /// `restart`, `deploy`, `deploy-ui` (only the UI is built and swapped in:
    /// the open app reloads, nothing restarts), `rollback` or `upgrade-adapter`.
    action: String,
}

pub(crate) async fn control_run(
    State(st): State<AppState>,
    Json(req): Json<ControlReq>,
) -> Result<Json<ControlStatus>, (StatusCode, String)> {
    let action = parse_action(&req.action).ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            format!(
                "unknown action {:?} — one of {}",
                req.action,
                ACTIONS.join(", ")
            ),
        )
    })?;
    let version = if action == "upgrade-adapter" {
        Some(upgrade_target(crate::versions::latest_adapter().as_deref())?)
    } else {
        None
    };
    let root = source_root(&st)?;
    crate::actor::spawn_blocking(move || {
        start(&root, &st.base, action, version.as_deref())?;
        Ok(Json(status(&root, &st.base)))
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
}

/// Something the app reports about itself (`POST /client/log`): an uncaught
/// error, a React error, a navigation that never finished.
#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct ClientLog {
    kind: String,
    message: String,
    #[serde(default)]
    detail: String,
    #[serde(default)]
    path: String,
}

fn client_log_line(entry: &ClientLog) -> String {
    let clip = |text: &str, max: usize| text.chars().take(max).collect::<String>();
    let mut line = format!(
        "client {}: {} [{}]",
        clip(&entry.kind, 40),
        clip(&entry.message, 2000).replace('\n', " "),
        clip(&entry.path, 300),
    );
    if !entry.detail.trim().is_empty() {
        line.push_str("\n  ");
        line.push_str(&clip(&entry.detail, 4000).replace('\n', "\n  "));
    }
    line
}

/// A deployed app has no console anyone sees, so what it reports lands in
/// this node's log (`.bakery-dev-logs/node.log`), bounded per report.
pub(crate) async fn client_log(Json(entry): Json<ClientLog>) -> Json<serde_json::Value> {
    eprintln!("{}", client_log_line(&entry));
    Json(serde_json::json!({}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{make_repo, state_with};
    use bakery_agent::StubModel;
    use std::sync::Arc;
    use std::time::Duration;

    fn set_mtime(path: &Path, at: SystemTime) {
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(at)
            .unwrap();
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// `service install` writes a per-user LaunchAgent for the deployed
    /// node: started at login, again after a crash but not a clean stop, in
    /// the logged-in session. A dry run in a scratch home, on a spare port:
    /// nothing is loaded, stopped or started.
    #[test]
    fn the_service_starts_the_deployed_node_at_login_and_after_a_crash() {
        let home = tempfile::tempdir().unwrap();
        let deployed = home.path().join(".bakery/deploy/current");
        std::fs::create_dir_all(&deployed).unwrap();
        std::fs::create_dir_all(deployed.join("Bakery.app")).unwrap();
        std::fs::write(deployed.join("bakery-node"), "").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(deployed.join("bakery-node"), std::fs::Permissions::from_mode(0o755)).unwrap();
        let spare = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/bakery.sh");
        let out = std::process::Command::new("/bin/bash")
            .arg(&script)
            .args(["service", "install"])
            .env("HOME", home.path())
            .env("BAKERY_DRY_RUN", "1")
            .env("BAKERY_NODE_ADDR", format!("127.0.0.1:{spare}"))
            .output()
            .unwrap();
        let said = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "{said}{}", String::from_utf8_lossy(&out.stderr));
        let node = deployed.join("bakery-node").display().to_string();
        for wanted in [
            "<key>Label</key><string>dev.bakery.node</string>",
            &format!("<array><string>{node}</string></array>"),
            "<key>RunAtLoad</key><true/>",
            "<key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>",
            "<key>LimitLoadToSessionType</key><string>Aqua</string>",
            &format!("<key>BAKERY_NODE_ADDR</key><string>127.0.0.1:{spare}</string>"),
            "+ launchctl bootstrap gui/",
        ] {
            assert!(said.contains(wanted), "{wanted}\n{said}");
        }
        assert!(
            !home.path().join("Library/LaunchAgents/dev.bakery.node.plist").exists(),
            "a dry run writes nothing"
        );
        assert!(!said.contains("bootout"), "never another HOME's job, even planned: {said}");
    }

    #[test]
    fn only_the_listed_actions_reach_the_script() {
        for action in ACTIONS {
            assert_eq!(parse_action(action), Some(action));
        }
        for refused in [
            "",
            "kill",
            "uninstall",
            "dev",
            "start",
            "Deploy",
            "deploy --checked",
            "restart; rm -rf ~",
            "../deploy",
            "upgrade-adapter 0.85.0",
            "deploy --ui",
            "deploy_ui",
            "Deploy-UI",
            "deploy-ui --checked",
            "deploy-ui; rm -rf ~",
        ] {
            assert_eq!(parse_action(refused), None, "{refused:?}");
        }
    }

    /// The `--ui` is the UI-only deploy's own: `deploy` never gets it, and no
    /// other action gets any argument beyond its name.
    #[test]
    fn only_the_ui_only_deploy_passes_an_argument_beyond_its_name() {
        for action in ACTIONS {
            let wanted: &[&str] = if action == "deploy-ui" {
                &["deploy", "--ui"]
            } else {
                &[action]
            };
            assert_eq!(script_args(action), wanted, "{action}");
        }
    }

    /// The version an upgrade installs is never a caller's: only the newest
    /// release the node's own check found, a real release, and newer.
    #[test]
    fn an_upgrade_takes_only_a_newer_release_the_node_found() {
        use crate::claude_native::ADAPTER_VERSION;
        assert_eq!(upgrade_target(Some("999.0.0")).unwrap(), "999.0.0");
        for (latest, code) in [
            (None, StatusCode::CONFLICT),
            (Some(ADAPTER_VERSION), StatusCode::CONFLICT),
            (Some("0.0.1"), StatusCode::CONFLICT),
            (Some("^999.0.0"), StatusCode::BAD_GATEWAY),
            (Some("999.0.0; rm -rf ~"), StatusCode::BAD_GATEWAY),
        ] {
            assert_eq!(upgrade_target(latest).unwrap_err().0, code, "{latest:?}");
        }
    }

    #[tokio::test]
    async fn the_route_refuses_an_unknown_action_before_running_anything() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let refused = control_run(
            State(state),
            Json(ControlReq {
                action: "uninstall".to_string(),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(refused.0, StatusCode::BAD_REQUEST);
        assert!(!dir.path().join("control.json").exists());
    }

    /// The deployed build comes from build.json; "not deployed" is every
    /// deploy source touched after build.json was written — never docs, e2e
    /// specs, or what the build itself had.
    #[test]
    fn the_status_names_the_deployed_build_and_the_source_changed_since() {
        let dir = tempfile::tempdir().unwrap();
        let root = make_repo(&dir.path().join("repo"));
        let base = dir.path().join("state");
        let empty = status(&root, &base);
        assert!(empty.deployed.is_none() && empty.undeployed.is_empty() && empty.last.is_none());

        let built = SystemTime::now() - Duration::from_secs(3600);
        let current = base.join(DEPLOY_DIR).join("current/build.json");
        write(
            &current,
            r#"{"commit":"abc1234","built_at":"2026-10-02T12:00:00Z","dirty_files":2}"#,
        );
        set_mtime(&current, built);
        write(
            &base.join(DEPLOY_DIR).join("previous/build.json"),
            r#"{"commit":"0ld0000","built_at":"2026-10-01T12:00:00Z","dirty_files":0}"#,
        );
        for (path, at) in [
            ("crates/x/src/lib.rs", built - Duration::from_secs(60)),
            ("crates/x/src/new.rs", built + Duration::from_secs(60)),
            ("bakery-client/src/App.svelte", built + Duration::from_secs(60)),
            ("bakery-client/e2e/app.spec.ts", built + Duration::from_secs(60)),
            ("docs/notes.md", built + Duration::from_secs(60)),
        ] {
            write(&root.join(path), "x");
            set_mtime(&root.join(path), at);
        }

        let report = status(&root, &base);
        assert_eq!(
            report.deployed,
            Some(DeployedBuild {
                commit: Some("abc1234".to_string()),
                built_at: Some("2026-10-02T12:00:00Z".to_string()),
                dirty_files: Some(2),
            })
        );
        assert_eq!(
            report.previous.and_then(|p| p.commit).as_deref(),
            Some("0ld0000")
        );
        assert_eq!(
            report.undeployed,
            ["bakery-client/src/App.svelte", "crates/x/src/new.rs"]
        );
        assert_eq!(report.undeployed_count, 2);
        assert!(!report.running_deployed, "a test binary is not a deployed build");
    }

    /// A restart replaces the node that started the action, so everything
    /// the next one reports comes from the files — and only a known action
    /// may name the log it reads.
    #[test]
    fn the_last_action_is_read_back_from_its_files() {
        let dir = tempfile::tempdir().unwrap();
        let (root, base) = (dir.path().join("repo"), dir.path().join("state"));
        std::fs::create_dir_all(&base).unwrap();
        let record = |action: &str, pid: u32| {
            let started = serde_json::json!({
                "action": action, "started_at": "2026-10-02T12:00:00Z", "pid": pid,
            });
            write(&base.join("control.json"), &started.to_string());
        };
        let lines: Vec<String> = (1..=50).map(|n| format!("line {n}")).collect();
        write(&log_path(&root, "deploy"), &lines.join("\n"));

        record("deploy", std::process::id());
        let running = last_action(&root, &base).unwrap();
        assert!(running.running && running.ok.is_none());
        assert_eq!(running.log_tail.len(), LOG_TAIL_LINES);
        assert_eq!(running.log_tail.last().map(String::as_str), Some("line 50"));

        write(&base.join("control.exit"), "0\n");
        let done = last_action(&root, &base).unwrap();
        assert_eq!((done.running, done.ok), (false, Some(true)));
        write(&base.join("control.exit"), "1\n");
        assert_eq!(last_action(&root, &base).unwrap().ok, Some(false));
        // The code says why: 3 is an upgrade whose patch needs porting.
        write(&base.join("control.exit"), "3\n");
        let porting = last_action(&root, &base).unwrap();
        assert_eq!((porting.ok, porting.exit_code), (Some(false), Some(3)));

        // Ended without a verdict: its process is gone and no exit code.
        std::fs::remove_file(base.join("control.exit")).unwrap();
        let mut gone = Command::new("true").spawn().unwrap();
        gone.wait().unwrap();
        record("deploy", gone.id());
        let ended = last_action(&root, &base).unwrap();
        assert_eq!((ended.running, ended.ok), (false, None));

        record("../../somewhere", std::process::id());
        assert!(last_action(&root, &base).is_none());
    }

    /// The real spawn path, with stand-in scripts: the deployed copy in the
    /// node's home runs rather than the working tree's, told which checkout
    /// to work on, as its own process group leader so it outlives the node it
    /// stops; its exit code is the verdict; an upgrade gets its version as
    /// the one argument; and a second press while one runs is refused.
    #[test]
    fn a_button_runs_the_deployed_script_detached_and_one_at_a_time() {
        let dir = tempfile::tempdir().unwrap();
        let (root, base) = (dir.path().join("repo"), dir.path().join("state"));
        std::fs::create_dir_all(&base).unwrap();
        let script = |path: &Path, body: &str| {
            write(path, &format!("#!/bin/sh\n{body}\n"));
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        script(
            &root.join("scripts/bakery.sh"),
            r#"echo "working tree $1"; echo 0 >"$BAKERY_CONTROL_EXIT""#,
        );
        script(
            &base.join(DEPLOY_DIR).join("current/bakery.sh"),
            r#"echo "deployed $* root=$BAKERY_ROOT pgid=$(ps -o pgid= -p $$ | tr -d ' ') pid=$$"; echo 0 >"$BAKERY_CONTROL_EXIT""#,
        );
        let finished = |action: &str| {
            let deadline = std::time::Instant::now() + Duration::from_secs(20);
            while last_action(&root, &base).is_some_and(|last| last.running) {
                assert!(std::time::Instant::now() < deadline, "the script never finished");
                std::thread::sleep(Duration::from_millis(50));
            }
            let last = last_action(&root, &base).unwrap();
            assert_eq!((last.action.as_str(), last.ok), (action, Some(true)));
            last.log_tail.join("\n")
        };

        start(&root, &base, "rollback", None).unwrap();
        let line = finished("rollback");
        let (pgid, pid) = line
            .strip_prefix(&format!("deployed rollback root={} pgid=", root.display()))
            .and_then(|rest| rest.split_once(" pid="))
            .unwrap_or_else(|| panic!("unexpected log: {line:?}"));
        assert_eq!(pgid, pid, "the script leads its own process group");

        start(&root, &base, "upgrade-adapter", Some("999.0.0")).unwrap();
        let line = finished("upgrade-adapter");
        assert!(
            line.starts_with("deployed upgrade-adapter 999.0.0 root="),
            "{line:?}"
        );

        // A press while one is running is refused, and runs nothing.
        let started = serde_json::json!({
            "action": "deploy", "started_at": "now", "pid": std::process::id(),
        });
        write(&base.join("control.json"), &started.to_string());
        std::fs::remove_file(base.join("control.exit")).unwrap();
        let refused = start(&root, &base, "restart", None).unwrap_err();
        assert_eq!(refused.0, StatusCode::CONFLICT);
        assert!(!log_path(&root, "restart").exists());
    }

    /// Hot reload through the real spawn path, with a stand-in script: the
    /// deployed copy runs as `deploy --ui`, and what it says goes to a log of
    /// its own. It waits on the exit file the script writes, as the node that
    /// comes back does, not on the script's pid.
    #[test]
    fn the_ui_only_deploy_runs_the_script_as_deploy_ui_with_a_log_of_its_own() {
        let dir = tempfile::tempdir().unwrap();
        let (root, base) = (dir.path().join("repo"), dir.path().join("state"));
        std::fs::create_dir_all(&base).unwrap();
        let deployed = base.join(DEPLOY_DIR).join("current/bakery.sh");
        write(
            &deployed,
            "#!/bin/sh\necho \"deployed $* root=$BAKERY_ROOT\"; echo 0 >\"$BAKERY_CONTROL_EXIT\"\n",
        );
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&deployed, std::fs::Permissions::from_mode(0o755)).unwrap();

        start(&root, &base, "deploy-ui", None).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        // The file exists a moment before its code is written.
        while std::fs::read_to_string(base.join("control.exit"))
            .map_or(true, |code| code.trim().is_empty())
        {
            assert!(
                std::time::Instant::now() < deadline,
                "the script never finished"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        let log = std::fs::read_to_string(log_path(&root, "deploy-ui")).unwrap();
        assert_eq!(
            log.trim_end(),
            format!("deployed deploy --ui root={}", root.display())
        );
        assert!(
            !log_path(&root, "deploy").exists(),
            "it is not a full deploy's log"
        );
        let last = last_action(&root, &base).unwrap();
        assert_eq!((last.action.as_str(), last.ok), ("deploy-ui", Some(true)));
    }

    /// The app's own reports reach the node log bounded, one line plus detail.
    #[test]
    fn client_reports_are_one_bounded_line() {
        let entry = ClientLog {
            kind: "error".into(),
            message: format!("TypeError: boom\n{}", "x".repeat(5000)),
            detail: "at a\nat b".into(),
            path: "/baker".into(),
        };
        let line = client_log_line(&entry);
        let first = line.lines().next().unwrap();
        assert!(first.starts_with("client error: TypeError: boom x"), "{first}");
        assert!(first.ends_with("[/baker]") || first.contains("[/baker]"), "{first}");
        assert!(first.chars().count() < 2100);
        assert_eq!(line.lines().nth(1), Some("  at a"));
    }
}
