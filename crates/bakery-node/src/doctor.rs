//! The toolchains doctor: for a thread's own folder, each toolchain it pins
//! (mise's tools, Rust's channel, FVM's Flutter, Xcode) beside what the
//! thread's own room runs. The tools are asked inside that room, as its
//! agent's own commands run, since a project's files never run outside one;
//! FVM's Flutter is looked up in its caches, run by nothing.

use crate::state::AppState;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::Duration;

type Fail = (StatusCode, String);

#[derive(Debug, Serialize, schemars::JsonSchema, PartialEq)]
pub(crate) struct Toolchain {
    /// `mise`, `rustup`, `FVM` or `Xcode`.
    pub(crate) manager: String,
    /// `node`, `python`, `Rust`, `Flutter`, `Xcode`…
    pub(crate) tool: String,
    /// The version the folder pins; none when it pins none.
    pub(crate) wanted: Option<String>,
    /// The file that pins it, relative to the folder.
    pub(crate) source: Option<String>,
    /// What the thread's room runs; none when it runs none.
    pub(crate) found: Option<String>,
    pub(crate) ready: bool,
    /// Why not, as the tool said it.
    pub(crate) why: Option<String>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct Toolchains {
    /// The thread's own folder.
    pub(crate) folder: String,
    pub(crate) toolchains: Vec<Toolchain>,
}

/// A command's first line of complaint.
fn first_line(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_string)
}

/// The folder's own tools, as mise reports them (`mise ls --current
/// --json`): those its own files pin, never the operator's global ones.
fn mise_rows(report: &Value, folder: &Path) -> Vec<Toolchain> {
    let mut rows = Vec::new();
    for (tool, versions) in report.as_object().into_iter().flatten() {
        let versions = versions.as_array().map(Vec::as_slice).unwrap_or_default();
        let own = versions.iter().find_map(|v| {
            let path = Path::new(v["source"]["path"].as_str()?);
            path.strip_prefix(folder)
                .ok()
                .map(|rel| (v, rel.display().to_string()))
        });
        let Some((pinned, source)) = own else {
            continue;
        };
        let installed = versions.iter().find(|v| v["installed"] == true);
        rows.push(Toolchain {
            manager: "mise".to_string(),
            tool: tool.clone(),
            wanted: pinned["requested_version"].as_str().map(str::to_string),
            source: Some(source),
            found: installed
                .and_then(|v| v["version"].as_str())
                .map(str::to_string),
            ready: installed.is_some(),
            why: installed.is_none().then(|| "not installed".to_string()),
        });
    }
    rows
}

/// The toolchain rustup runs in the folder: `1.85.0-aarch64-apple-darwin
/// (overridden by …)` gives `1.85.0-aarch64-apple-darwin`.
fn rustup_toolchain(said: &str) -> Option<String> {
    said.split_whitespace().next().map(str::to_string)
}

/// `Xcode 26.6` gives `26.6`.
fn xcode_version(said: &str) -> Option<String> {
    said.lines()
        .next()?
        .strip_prefix("Xcode ")
        .map(|v| v.trim().to_string())
}

/// The thread's own room, as its agent's commands run in it.
struct Room {
    env: bakery_exec::SessionEnvironment,
    granted: crate::machine_access::SpawnGrants,
    folder: PathBuf,
}

impl Room {
    fn of(state: &AppState, stream_id: &str, folder: &Path) -> Result<Room, String> {
        Ok(Room {
            env: bakery_exec::ensure_thin_room(
                folder,
                &crate::state::cache_root(state),
                stream_id,
            )?,
            granted: crate::machine_access::for_stream(state, stream_id),
            folder: folder.to_path_buf(),
        })
    }

    /// `argv` in the room, its folder read only.
    fn run(
        &self,
        argv: &[&str],
        vars: &[(&str, &str)],
    ) -> Result<bakery_exec::AttachOutput, String> {
        let mut env = self.granted.env.clone();
        env.extend(vars.iter().map(|(k, v)| (k.to_string(), v.to_string())));
        let argv: Vec<String> = argv.iter().map(|a| a.to_string()).collect();
        let g = &self.granted;
        bakery_exec::attach_exec_granted(
            &self.env,
            &argv,
            Some(&self.folder),
            bakery_exec::ExtraGrants {
                read: &g.read,
                write: &g.write,
                exec: &g.exec,
                deny: &g.deny,
                whole_machine: g.whole_machine.as_ref(),
                path: &g.path,
                env: &env,
            },
            Duration::from_secs(60),
        )
    }
}

/// Where FVM keeps Flutter versions for the thread: its project's own
/// copies first (the Mac sandbox), then the operator's.
fn flutter_caches(state: &AppState, stream_id: &str) -> Vec<PathBuf> {
    let project = {
        let heart = state.heart.lock().unwrap();
        crate::state::socket_project(&heart, stream_id).0
    };
    let home = crate::machine_access::home_of(state);
    project
        .map(|id| crate::mac_sandbox::own(state, &id).join("fvm/versions"))
        .into_iter()
        .chain([home.join("fvm/versions")])
        .collect()
}

/// Whether the folder builds for Apple's platforms, so Xcode matters.
fn builds_for_apple(folder: &Path, stack: &[crate::control_plane::StackItem]) -> bool {
    stack
        .iter()
        .any(|i| matches!(i.name.as_str(), "Swift" | "CocoaPods" | "Flutter"))
        || ["ios", "macos", ".xcode-version"]
            .iter()
            .any(|n| folder.join(n).exists())
}

fn doctor(state: &AppState, stream_id: &str) -> Result<Toolchains, Fail> {
    use crate::control_plane::Kind;
    use crate::machine_access::Reach;
    let internal = |e: String| (StatusCode::INTERNAL_SERVER_ERROR, e);
    if matches!(
        crate::machine_access::reach_for(state, stream_id),
        Reach::Container | Reach::Locked
    ) {
        return Err((
            StatusCode::CONFLICT,
            "this thread runs in a container: its image holds its toolchains".to_string(),
        ));
    }
    let context =
        crate::state::context_for(state, stream_id).map_err(|e| (StatusCode::NOT_FOUND, e))?;
    let folder = PathBuf::from(&context.worktree_path);
    let stack = crate::control_plane::stack_of(&folder);
    let pinned = |name: &str| {
        stack
            .iter()
            .find(|i| i.kind == Kind::Toolchain && i.name == name)
    };
    let room = Room::of(state, stream_id, &folder).map_err(internal)?;
    let mut rows = Vec::new();

    let mise_files = ["mise.toml", ".mise.toml", ".tool-versions"];
    if stack
        .iter()
        .any(|i| i.kind == Kind::Toolchain && mise_files.iter().any(|f| i.source.ends_with(f)))
    {
        let out = room
            .run(&["mise", "ls", "--current", "--json"], &[])
            .map_err(internal)?;
        if out.exit_code == 0 {
            let report = serde_json::from_str::<Value>(&out.stdout).unwrap_or_default();
            rows.extend(mise_rows(&report, &folder));
        } else {
            rows.push(Toolchain {
                manager: "mise".to_string(),
                tool: "mise".to_string(),
                wanted: None,
                source: None,
                found: None,
                ready: false,
                why: first_line(&out.stderr).or_else(|| first_line(&out.stdout)),
            });
        }
    }

    if let Some(rust) = pinned("Rust") {
        let out = room
            .run(
                &["rustup", "show", "active-toolchain"],
                &[("RUSTUP_AUTO_INSTALL", "0")],
            )
            .map_err(internal)?;
        let ready = out.exit_code == 0;
        rows.push(Toolchain {
            manager: "rustup".to_string(),
            tool: "Rust".to_string(),
            wanted: rust.version.clone(),
            source: Some(rust.source.clone()),
            found: ready.then(|| rustup_toolchain(&out.stdout)).flatten(),
            ready,
            why: (!ready).then(|| first_line(&out.stderr)).flatten(),
        });
    }

    if let Some(flutter) = pinned("Flutter") {
        let caches = flutter_caches(state, stream_id);
        let found = flutter.version.as_ref().filter(|v| {
            caches
                .iter()
                .any(|cache| cache.join(v.as_str()).join("bin/flutter").is_file())
        });
        rows.push(Toolchain {
            manager: "FVM".to_string(),
            tool: "Flutter".to_string(),
            wanted: flutter.version.clone(),
            source: Some(flutter.source.clone()),
            found: found.cloned(),
            ready: found.is_some(),
            why: found.is_none().then(|| "not in FVM's cache".to_string()),
        });
    }

    if builds_for_apple(&folder, &stack) {
        let wanted = std::fs::read_to_string(folder.join(".xcode-version"))
            .ok()
            .and_then(|t| first_line(&t));
        let out = room
            .run(&["xcodebuild", "-version"], &[])
            .map_err(internal)?;
        let found = (out.exit_code == 0)
            .then(|| xcode_version(&out.stdout))
            .flatten();
        let ready = match (&wanted, &found) {
            (Some(w), Some(f)) => f.starts_with(w.as_str()),
            (None, found) => found.is_some(),
            _ => false,
        };
        let why = match &found {
            None => {
                first_line(&out.stderr).or_else(|| Some("xcodebuild didn't answer".to_string()))
            }
            Some(_) if !ready => Some("another version is selected".to_string()),
            Some(_) => None,
        };
        rows.push(Toolchain {
            manager: "Xcode".to_string(),
            tool: "Xcode".to_string(),
            source: wanted.as_ref().map(|_| ".xcode-version".to_string()),
            wanted,
            found,
            ready,
            why,
        });
    }

    Ok(Toolchains {
        folder: folder.display().to_string(),
        toolchains: rows,
    })
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct ToolchainsQuery {
    pub(crate) stream_id: String,
}

/// `GET /thread/toolchains`: the toolchains a thread's folder pins, and
/// what its room runs.
pub(crate) async fn toolchains_route(
    State(st): State<AppState>,
    Query(q): Query<ToolchainsQuery>,
) -> Result<Json<Toolchains>, Fail> {
    if crate::actor::agent_calling() {
        return Err((
            StatusCode::FORBIDDEN,
            "the toolchains doctor is the operator's".to_string(),
        ));
    }
    crate::actor::spawn_blocking(move || doctor(&st, &q.stream_id))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map(Json)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_tool_is_read_in_its_own_words() {
        let folder = Path::new("/w/farm");
        let report: Value = serde_json::from_str(
            r#"{
              "node": [{"version":"22.11.0","requested_version":"22","installed":true,"active":true,
                        "source":{"type":"mise.toml","path":"/w/farm/mise.toml"}}],
              "python": [{"version":"3.13.1","requested_version":"3.13","installed":false,"active":true,
                          "source":{"type":".tool-versions","path":"/w/farm/backend/.tool-versions"}}],
              "go": [{"version":"1.27.0","requested_version":"latest","installed":true,"active":true,
                      "source":{"type":"mise.toml","path":"/Users/op/.config/mise/config.toml"}}]
            }"#,
        )
        .unwrap();
        let rows = mise_rows(&report, folder);
        let seen: Vec<(&str, Option<&str>, Option<&str>, bool)> = rows
            .iter()
            .map(|r| {
                (
                    r.tool.as_str(),
                    r.wanted.as_deref(),
                    r.found.as_deref(),
                    r.ready,
                )
            })
            .collect();
        assert_eq!(
            seen,
            [
                ("node", Some("22"), Some("22.11.0"), true),
                ("python", Some("3.13"), None, false)
            ],
            "its own pins only, never the operator's global tools"
        );
        assert_eq!(rows[1].source.as_deref(), Some("backend/.tool-versions"));
        assert_eq!(
            rustup_toolchain(
                "1.85.0-aarch64-apple-darwin (overridden by '/w/farm/rust-toolchain.toml')"
            )
            .as_deref(),
            Some("1.85.0-aarch64-apple-darwin")
        );
        assert_eq!(
            xcode_version("Xcode 26.6\nBuild version 17F113").as_deref(),
            Some("26.6")
        );
        assert_eq!(
            xcode_version("xcode-select: error: tool 'xcodebuild' requires Xcode"),
            None
        );
    }

    /// A thread's folder pinning versions nobody has: each is asked in its
    /// room (or its caches) and named as not ready, with the reason.
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn a_threads_pins_are_checked_in_its_own_room() {
        let dir = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        std::fs::write(
            work.path().join("rust-toolchain.toml"),
            "[toolchain]\nchannel = \"0.0.1\"\n",
        )
        .unwrap();
        std::fs::write(work.path().join(".fvmrc"), r#"{"flutter":"0.0.1"}"#).unwrap();
        let state =
            crate::testutil::state_with(dir.path(), std::sync::Arc::new(bakery_agent::StubModel));
        let root = crate::catalog::DirectoryRootSpec::Attach(work.path().to_str().unwrap());
        let (_, thread) = crate::catalog::create_project(&state, "farm", Some(root)).unwrap();

        let found = doctor(&state, &thread.stream_id).unwrap();
        let row = |manager: &str| {
            found
                .toolchains
                .iter()
                .find(|t| t.manager == manager)
                .unwrap_or_else(|| panic!("{manager}: {found:?}"))
        };
        let rust = row("rustup");
        assert_eq!(
            (rust.wanted.as_deref(), rust.source.as_deref()),
            (Some("0.0.1"), Some("rust-toolchain.toml"))
        );
        assert!(!rust.ready && rust.why.is_some(), "{rust:?}");
        let flutter = row("FVM");
        assert_eq!(flutter.wanted.as_deref(), Some("0.0.1"));
        assert_eq!(
            (flutter.ready, flutter.why.as_deref()),
            (false, Some("not in FVM's cache"))
        );
        assert!(
            row("Xcode").why.is_some() || row("Xcode").ready,
            "Flutter builds for Apple's platforms"
        );
    }
}
