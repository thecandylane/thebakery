//! The Mac sandbox: the kind of sandbox a project's threads run in on this
//! Mac when its setup says so (Seatbelt), as the operator answered for
//! GreenLeaf. A thread runs the operator's toolchains as they are, read and
//! run only (every room's Apple tools, Homebrew's and mise's), and the
//! project's own copies come first: its own FVM cache, mise data and gem
//! home, and its own package caches. Installing into its copies always asks
//! the operator first: to the agent they're read-only. Its pinned Flutter
//! starts as an APFS clone of the operator's, since Flutter takes a lock
//! inside its SDK on every command and can't run read-only. The control
//! plane lists what the project's copies hold that the operator's don't,
//! and the operator may keep one for good on their own machine.

use crate::machine_access::SpawnGrants;
use crate::state::AppState;
use axum::{extract::State, http::StatusCode, Json};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Homebrew's prefix on Apple silicon, where mise and gem live.
const HOMEBREW: &str = "/opt/homebrew";

/// The project's own copies, kept in the Bakery's cache.
pub(crate) fn own(state: &AppState, project_id: &str) -> PathBuf {
    let safe: String = project_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    state.base.join("cache").join("projects").join(safe)
}

// ponytail: the operator's default homes for FVM and mise; read their
// settings too if they ever move them.
fn fvm_versions(home: &Path) -> PathBuf {
    home.join("fvm").join("versions")
}

fn mise_installs(home: &Path) -> PathBuf {
    home.join(".local/share/mise/installs")
}

/// The active Xcode's developer folder, as `xcode-select -p` names it.
pub(crate) fn developer_dir() -> Option<PathBuf> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| {
        let out = std::process::Command::new("/usr/bin/xcode-select")
            .arg("-p")
            .output()
            .ok()?;
        let dir = PathBuf::from(String::from_utf8(out.stdout).ok()?.trim());
        (out.status.success() && dir.is_dir()).then_some(dir)
    })
    .clone()
}

/// The project a thread belongs to, with its folders, when its setup runs
/// it in the Mac sandbox.
pub(crate) fn mac_project(state: &AppState, stream_id: &str) -> Option<(String, Vec<PathBuf>)> {
    let heart = state.heart.lock().unwrap();
    let project_id = crate::state::socket_project(&heart, stream_id).0?;
    let record = crate::catalog::project_record_locked(&heart, &project_id)?;
    let roots = record
        .roots
        .iter()
        .map(|r| PathBuf::from(&r.path))
        .collect();
    (record.stacks.sandbox == "mac").then_some((project_id, roots))
}

/// The Flutter a project pins (`.fvmrc`), as detection reads it.
fn pinned_flutter(roots: &[PathBuf]) -> Option<String> {
    roots.iter().find_map(|root| {
        crate::control_plane::stack_of(root)
            .into_iter()
            .find(|item| {
                item.name == "Flutter" && item.kind == crate::control_plane::Kind::Toolchain
            })
            .and_then(|item| item.version)
    })
}

/// An APFS clone of `from` at `to`: the whole tree in one step, sharing
/// every block until either side changes. A plain copy where cloning can't.
fn clone_tree(from: &Path, to: &Path) -> Result<(), String> {
    use std::os::unix::ffi::OsStrExt;
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let c = |p: &Path| std::ffi::CString::new(p.as_os_str().as_bytes()).map_err(|e| e.to_string());
    let (src, dst) = (c(from)?, c(to)?);
    // SAFETY: two valid NUL-terminated paths; clonefile reads them only.
    if unsafe { libc::clonefile(src.as_ptr(), dst.as_ptr(), 0) } == 0 {
        return Ok(());
    }
    let out = std::process::Command::new("/bin/cp")
        .arg("-R")
        .arg(from)
        .arg(to)
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// What the operator's caches hold that the project's don't yet, as clones
/// in the project's own (crates in its build folder's cargo home, `cargo`):
/// crates from crates.io, packages from pub.dev, and Swift packages with the
/// binaries they download. So a build needn't fetch what this Mac already
/// has.
fn seed(home: &Path, own: &Path, cargo: &Path) {
    let missing = |theirs: &Path, ours: &Path, keep: &dyn Fn(&Path) -> bool| {
        for entry in std::fs::read_dir(theirs).into_iter().flatten().flatten() {
            let to = ours.join(entry.file_name());
            if keep(&entry.path()) && !to.exists() {
                let _ = clone_tree(&entry.path(), &to);
            }
        }
    };
    let crates_io = |p: &Path| {
        p.file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with("index.crates.io-"))
    };
    for dir in ["index", "cache", "src"] {
        let registry = Path::new(".cargo/registry").join(dir);
        missing(&home.join(&registry), &cargo.join("registry").join(dir), &crates_io);
    }
    for dir in ["hosted/pub.dev", "hosted-hashes/pub.dev"] {
        missing(&home.join(".pub-cache").join(dir), &own.join("pub-cache").join(dir), &|_| true);
    }
    // ponytail: all of the operator's Swift packages, not only this
    // project's: a project's pins miss what its plugins add, and every one
    // is public today. Narrow to the project's own if a private package
    // ever lands in their cache.
    let swiftpm = Path::new("Library/Caches/org.swift.swiftpm");
    let (theirs, ours) = (home.join(swiftpm), own.join("home").join(swiftpm));
    for dir in ["repositories", "artifacts"] {
        missing(&theirs.join(dir), &ours.join(dir), &|_| true);
    }
}

/// Xcode's tools as a Mac sandbox room runs them, first on its PATH: Swift
/// packages' manifests run in the room's sandbox, since macOS allows no
/// sandbox inside one and Xcode would start one for each manifest.
// ponytail: `xcrun xcodebuild …` and `xcodebuild …` only; `xcrun --sdk x
// xcodebuild` still reaches the plain one.
fn shims(own: &Path) {
    let Some(developer) = developer_dir() else {
        return;
    };
    let flag = "-IDEPackageSupportDisableManifestSandbox=YES -skipPackageUpdates";
    let xcodebuild = developer.join("usr/bin/xcodebuild");
    let shims = [
        (
            "xcodebuild",
            format!("exec '{}' {flag} \"$@\"", xcodebuild.display()),
        ),
        (
            "xcrun",
            format!("if [ \"$1\" = xcodebuild ]; then shift; exec /usr/bin/xcrun xcodebuild {flag} \"$@\"; fi\nexec /usr/bin/xcrun \"$@\""),
        ),
    ];
    let bin = own.join("bin");
    let _ = std::fs::create_dir_all(&bin);
    for (name, body) in shims {
        let path = bin.join(name);
        let script = format!(
            "#!/bin/sh\n# The Bakery's: Swift packages' manifests run in this room's sandbox.\n{body}\n"
        );
        if std::fs::read_to_string(&path).ok().as_deref() != Some(script.as_str()) {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::write(&path, &script);
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755));
        }
    }
}

/// Readies the project's own copies: its folders, its pinned Flutter as a
/// clone of the operator's, and mise's shims for what its threads may run.
/// Returns the Flutter SDK they run, if the project has one.
fn prepare(state: &AppState, project_id: &str, roots: &[PathBuf]) -> Option<PathBuf> {
    // ponytail: once per project per five minutes; a pin changed in between
    // counts from the next one.
    static READY: OnceLock<Mutex<HashMap<String, (Instant, Option<PathBuf>)>>> = OnceLock::new();
    let ready = READY.get_or_init(Default::default);
    if let Some((at, sdk)) = ready.lock().unwrap().get(project_id) {
        if at.elapsed() < Duration::from_secs(300) {
            return sdk.clone();
        }
    }
    let own = own(state, project_id);
    for dir in [
        "fvm/versions",
        "home",
        "mise",
        "mise-state",
        "gems",
        "pub-cache",
        "cocoapods",
        "npm",
    ] {
        let _ = std::fs::create_dir_all(own.join(dir));
    }
    let home = crate::machine_access::home_of(state);
    seed(&home, &own, &crate::builds::cargo_home(state, project_id));
    shims(&own);
    let sdk = pinned_flutter(roots).map(|version| {
        let sdk = own.join("fvm/versions").join(&version);
        let theirs = fvm_versions(&home).join(&version);
        if !sdk.exists() && theirs.join("bin/flutter").is_file() {
            if let Err(e) = clone_tree(&theirs, &sdk) {
                eprintln!("mac sandbox: cloning Flutter {version} for {project_id}: {e}");
            }
        }
        sdk
    });
    // The project's own mise throughout: never the operator's settings
    // (their global config may set variables) or state.
    let mise = Path::new(HOMEBREW).join("bin/mise");
    if mise.is_file() && !own.join("mise/shims").is_dir() {
        let _ = std::process::Command::new(&mise)
            .arg("reshim")
            .env("MISE_OFFLINE", "1")
            .env("MISE_DATA_DIR", own.join("mise"))
            .env("MISE_SHARED_INSTALL_DIRS", mise_installs(&home))
            .env("MISE_CONFIG_DIR", own.join("mise-state/config"))
            .env("MISE_STATE_DIR", own.join("mise-state"))
            .env("MISE_CACHE_DIR", own.join("mise-state/cache"))
            .output();
    }
    let sdk = sdk.filter(|sdk| sdk.join("bin/flutter").is_file());
    ready
        .lock()
        .unwrap()
        .insert(project_id.to_string(), (Instant::now(), sdk.clone()));
    sdk
}

/// A Mac sandbox thread's toolchains, on top of every room's: the
/// project's own copies, first on PATH, and the variables that point every
/// tool at them. Nothing for a thread whose project runs elsewhere. Only a
/// runtime `starting` readies the copies and opens its thread's door; one
/// shown has what the last readying left.
pub(crate) fn extend(state: &AppState, stream_id: &str, grants: &mut SpawnGrants, starting: bool) {
    let Some((project_id, roots)) = mac_project(state, stream_id) else {
        return;
    };
    let home = crate::machine_access::home_of(state);
    let own = own(state, &project_id);
    let sdk = match starting {
        true => prepare(state, &project_id, &roots),
        false => pinned_flutter(&roots)
            .map(|version| own.join("fvm/versions").join(version))
            .filter(|sdk| sdk.join("bin/flutter").is_file()),
    };
    apply(grants, &home, &own, sdk.as_deref(), &roots);
    if !starting {
        return;
    }

    // Its network: this Mac, and past it only its thread's door, where the
    // gateway holds it to its project's list.
    let proxy = match crate::doors::door(state, stream_id) {
        Ok(port) => format!("http://127.0.0.1:{port}"),
        Err(e) => {
            eprintln!("mac sandbox: {stream_id}: {e}");
            return;
        }
    };
    for name in ["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy"] {
        grants.env.push((name.to_string(), proxy.clone()));
    }
    for name in ["NO_PROXY", "no_proxy"] {
        grants
            .env
            .push((name.to_string(), "localhost,127.0.0.1,::1".to_string()));
    }
    grants
        .env
        .push((bakery_exec::NET_ENV.to_string(), "local".to_string()));
}

/// The Mac sandbox's grants and variables for a project's own copies at
/// `own`, the operator's toolchains under `home`, and its Flutter `sdk`.
fn apply(grants: &mut SpawnGrants, home: &Path, own: &Path, sdk: Option<&Path>, roots: &[PathBuf]) {
    // Its caches and its tools' state are its own to write; its toolchain
    // copies only to run, though its Flutter writes inside its own SDK.
    grants.write.extend(
        ["pub-cache", "cocoapods", "npm", "mise-state", "home"].map(|dir| own.join(dir)),
    );
    // The operator's Rust (rustup's toolchains, iOS targets among them),
    // read and run only; downloaded crates land in the project's build
    // folder (`builds`), shared by all its threads. Never the rest of
    // ~/.cargo, where a credentials file may sit.
    let (rustup, rust_bin) = (home.join(".rustup"), home.join(".cargo/bin"));
    let rust = rustup.is_dir() && rust_bin.is_dir();
    if rust {
        grants.exec.extend([rustup.clone(), rust_bin.clone()]);
    }
    grants.write.extend(sdk.map(Path::to_path_buf));
    // The Flutter tool's own packages, which its SDK (cloned from the
    // operator's) lists in the operator's pub cache: unless it finds them,
    // every command fetches them anew. Read only, and only pub.dev's,
    // never the cache's credentials.
    if sdk.is_some() {
        grants.read.push(home.join(".pub-cache/hosted/pub.dev"));
    }
    grants
        .exec
        .extend(["fvm", "mise", "gems", "bin"].map(|dir| own.join(dir)));

    // Its own tools ahead of the operator's: Xcode's as a room runs them
    // ([`shims`]); its Flutter's dart, not Homebrew's; its mise, which sees
    // the operator's installs too; then
    // Xcode's own git and tools, not /usr/bin's xcrun shims (which can't
    // keep their cache in a room, so each call would look its tool up anew).
    let xcode = developer_dir().map(|developer| developer.join("usr/bin"));
    let first: Vec<PathBuf> = [own.join("bin")]
        .into_iter()
        .chain(sdk.map(|sdk| sdk.join("bin")))
        .chain([own.join("mise/shims"), own.join("gems/bin")])
        .chain(rust.then(|| rust_bin.clone()))
        .chain(xcode)
        .collect();
    grants.path.splice(0..0, first.iter().cloned());
    // Flutter asks sysctl what Mac it's on.
    grants.exec.push(PathBuf::from("/usr/sbin"));
    grants.path.push(PathBuf::from("/usr/sbin"));
    let ahead = first.iter().map(|dir| {
        (
            bakery_exec::ACP_PATH_PREFIX_ENV.to_string(),
            dir.display().to_string(),
        )
    });
    grants.env.splice(0..0, ahead);
    let trusted = roots
        .iter()
        .map(|r| r.display().to_string())
        .collect::<Vec<_>>()
        .join(":");
    let dir = |name: &str| own.join(name).display().to_string();
    // After every room's own mise setup, so these are the ones that count.
    // Nothing installs on its own: the operator answers every install.
    let vars = [
        ("FVM_CACHE_PATH", dir("fvm")),
        ("MISE_DATA_DIR", dir("mise")),
        ("MISE_STATE_DIR", dir("mise-state")),
        (
            "MISE_SHARED_INSTALL_DIRS",
            mise_installs(home).display().to_string(),
        ),
        ("MISE_TRUSTED_CONFIG_PATHS", trusted),
        ("MISE_AUTO_INSTALL", "0".to_string()),
        ("MISE_EXEC_AUTO_INSTALL", "0".to_string()),
        ("MISE_NOT_FOUND_AUTO_INSTALL", "0".to_string()),
        ("GEM_HOME", dir("gems")),
        ("PUB_CACHE", dir("pub-cache")),
        ("CP_HOME_DIR", dir("cocoapods")),
        ("npm_config_cache", dir("npm")),
        // CocoaPods refuses to install in any other encoding.
        ("LANG", "en_US.UTF-8".to_string()),
        // Apple's tools find their home here, not from HOME: Xcode's build
        // files (DerivedData) and caches are the project's own, never the
        // operator's ~/Library.
        ("CFFIXED_USER_HOME", dir("home")),
        // Swift's compiler keeps its temporary files in the user's own temp
        // folder, whatever TMPDIR says.
        (bakery_exec::XCODE_ENV, "1".to_string()),
        // Compiled modules outside an Xcode build (Swift packages'
        // manifests, a plain swiftc or clang) go in the project's own cache,
        // never the operator's, which their own builds load.
        ("CLANG_MODULE_CACHE_PATH", dir("home/Library/Caches/clang/ModuleCache")),
        ("SWIFTPM_MODULECACHE_OVERRIDE", dir("home/Library/Caches/clang/ModuleCache")),
    ];
    grants
        .env
        .extend(vars.into_iter().map(|(k, v)| (k.to_string(), v)));
    if rust {
        grants
            .env
            .push(("RUSTUP_HOME".to_string(), rustup.display().to_string()));
    }
}

/// What a Mac sandbox thread's agent is told of its toolchains, once a
/// session: what it runs, and that installing asks the operator.
pub(crate) fn session_line(state: &AppState, stream_id: &str) -> Option<String> {
    let (project_id, roots) = mac_project(state, stream_id)?;
    let flutter = prepare(state, &project_id, &roots)
        .and_then(|sdk| {
            sdk.file_name().map(|v| {
                format!(
                    " its own Flutter {} is first on PATH; for the operator's iPhone, build with `flutter build ios --no-codesign` and their Build to my iPhone button signs and installs it;",
                    v.to_string_lossy()
                )
            })
        })
        .unwrap_or_default();
    Some(format!(
        "Mac sandbox: the operator's Homebrew, Xcode, mise tools and Ruby gems are yours to run, never to change;{flutter} pub, CocoaPods and npm caches are this project's own; installing a toolchain or version the operator lacks needs them: ask with toolchain_request (tool flutter, mise or gem; name; version; reason), never work around it"
    ))
}

/// One thing the project's own copies hold that the operator's don't.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub(crate) struct Addition {
    /// `flutter`, `mise` or `gem`.
    pub(crate) tool: String,
    /// What it is: `flutter`, a mise tool (`node`), or a gem's name.
    pub(crate) name: String,
    pub(crate) version: String,
}

fn dirs_in(dir: &Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

/// What a project's own copies hold beyond the operator's: Flutter
/// versions they lack, mise installs, gems.
pub(crate) fn additions(state: &AppState, project_id: &str) -> Vec<Addition> {
    let (own, home) = (
        own(state, project_id),
        crate::machine_access::home_of(state),
    );
    let mut out = Vec::new();
    let add = |out: &mut Vec<Addition>, tool: &str, name: &str, version: String| {
        out.push(Addition {
            tool: tool.to_string(),
            name: name.to_string(),
            version,
        })
    };
    for version in dirs_in(&own.join("fvm/versions")) {
        if !fvm_versions(&home).join(&version).exists() {
            add(&mut out, "flutter", "flutter", version);
        }
    }
    for tool in dirs_in(&own.join("mise/installs")) {
        for version in dirs_in(&own.join("mise/installs").join(&tool)) {
            add(&mut out, "mise", &tool, version);
        }
    }
    let specs = std::fs::read_dir(own.join("gems/specifications"))
        .map(|e| {
            e.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut gems: Vec<(String, String)> = specs
        .iter()
        .filter_map(|spec| spec.strip_suffix(".gemspec")?.rsplit_once('-'))
        .map(|(name, version)| (name.to_string(), version.to_string()))
        .collect();
    gems.sort();
    for (name, version) in gems {
        add(&mut out, "gem", &name, version);
    }
    out
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct KeepReq {
    pub(crate) project_id: String,
    #[serde(flatten)]
    pub(crate) addition: Addition,
}

/// Keeps one of a project's additions for good: the operator's own FVM,
/// mise or gems gets it, so every project has it. Run by the node, as the
/// operator's action, never an agent's.
pub(crate) fn keep(state: &AppState, req: &KeepReq) -> Result<Vec<Addition>, (StatusCode, String)> {
    let bad = |e: String| (StatusCode::BAD_REQUEST, e);
    if crate::actor::agent_calling() {
        return Err((
            StatusCode::FORBIDDEN,
            "only the operator keeps a toolchain for good".to_string(),
        ));
    }
    let Addition {
        tool,
        name,
        version,
    } = &req.addition;
    if !additions(state, &req.project_id).contains(&req.addition) {
        return Err((
            StatusCode::NOT_FOUND,
            format!("{name} {version} isn't one of this project's additions"),
        ));
    }
    let (own, home) = (
        own(state, &req.project_id),
        crate::machine_access::home_of(state),
    );
    match tool.as_str() {
        "flutter" => clone_tree(
            &own.join("fvm/versions").join(version),
            &fvm_versions(&home).join(version),
        )
        .map_err(bad)?,
        "mise" => {
            let theirs = mise_installs(&home).join(name).join(version);
            if theirs.exists() {
                return Err(bad(format!("{name} {version} is already yours")));
            }
            clone_tree(&own.join("mise/installs").join(name).join(version), &theirs)
                .map_err(bad)?;
            let _ = std::process::Command::new(Path::new(HOMEBREW).join("bin/mise"))
                .arg("reshim")
                .env("MISE_OFFLINE", "1")
                .output();
        }
        "gem" => {
            let gem = own.join("gems/cache").join(format!("{name}-{version}.gem"));
            let out = std::process::Command::new(Path::new(HOMEBREW).join("bin/gem"))
                .args(["install", "--local", "--no-document"])
                .arg(&gem)
                .output()
                .map_err(|e| bad(e.to_string()))?;
            if !out.status.success() {
                return Err(bad(String::from_utf8_lossy(&out.stderr).trim().to_string()));
            }
        }
        other => return Err(bad(format!("unknown toolchain {other:?}"))),
    }
    Ok(additions(state, &req.project_id))
}

/// `POST /project/toolchain/keep`: keep a project's addition for good.
pub(crate) async fn keep_route(
    State(st): State<AppState>,
    Json(req): Json<KeepReq>,
) -> Result<Json<Vec<Addition>>, (StatusCode, String)> {
    crate::actor::spawn_blocking(move || keep(&st, &req))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map(Json)
}

// ─── Installs: asked by an agent, answered by the operator, run by the node ──

/// An agent asked for a toolchain or version in its project's own copies:
/// a card in its thread, and a line in the project's list.
pub(crate) const INSTALL_ASK: &str = "toolchain.install.ask";
/// The operator's answer: on the project's stream, and in the asking
/// thread citing its ask.
pub(crate) const INSTALL_DECIDED: &str = "toolchain.install.decided";
/// What the install did, and the last of what it printed.
pub(crate) const INSTALL_DONE: &str = "toolchain.install.done";

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub(crate) struct InstallReq {
    pub(crate) stream_id: String,
    /// `flutter` (a Flutter version, through FVM), `mise` (a mise tool's
    /// version) or `gem`.
    pub(crate) tool: String,
    /// The mise tool or the gem; `flutter` for Flutter.
    pub(crate) name: String,
    pub(crate) version: String,
    /// Why the project needs it, for the operator.
    #[serde(default)]
    pub(crate) reason: String,
}

/// An install's record, on the project's stream: the ask, then its answer
/// and its result, each naming the thread's card (`ask`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Record {
    project_id: String,
    tool: String,
    name: String,
    version: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    reason: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    stream_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    ask: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    allow: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ok: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

/// One install, as History and the control plane show it.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub(crate) struct Install {
    pub(crate) tool: String,
    pub(crate) name: String,
    pub(crate) version: String,
    pub(crate) reason: String,
    /// The thread that asked.
    pub(crate) stream_id: String,
    /// The ask's event in that thread: its card.
    pub(crate) event_hash: String,
    pub(crate) asked_at: String,
    /// The operator's answer, once given.
    pub(crate) allow: Option<bool>,
    pub(crate) answered_at: Option<String>,
    /// Whether it installed, once done, with the last of its output.
    pub(crate) ok: Option<bool>,
    pub(crate) detail: Option<String>,
    pub(crate) done_at: Option<String>,
}

/// Asks and answers one at a time, so a project never asks twice for one
/// install.
fn one_install_at_a_time() -> std::sync::MutexGuard<'static, ()> {
    static ONE: Mutex<()> = Mutex::new(());
    ONE.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A name or version as installers take it: plain, never an option.
fn plain(word: &str, what: &str) -> Result<String, String> {
    let word = word.trim();
    let ok = !word.is_empty()
        && word.len() <= 100
        && !word.starts_with('-')
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._+-:/@".contains(c));
    ok.then(|| word.to_string())
        .ok_or_else(|| format!("{what} {word:?} isn't one an installer takes"))
}

/// Every install a project's agents asked for, oldest first.
pub(crate) fn installs(state: &AppState, project_id: &str) -> Vec<Install> {
    let heart = state.heart.lock().unwrap();
    let of = |family| {
        heart
            .events_of_family(project_id, family)
            .unwrap_or_default()
    };
    let (asks, decided, done) = (of(INSTALL_ASK), of(INSTALL_DECIDED), of(INSTALL_DONE));
    let read = |e: &bakery_core::Event| serde_json::from_str::<Record>(&e.payload).ok();
    asks.iter()
        .filter_map(|e| Some((e, read(e)?)))
        .map(|(e, r)| {
            let answer = decided
                .iter()
                .find_map(|d| read(d).filter(|a| a.ask == r.ask).map(|a| (d, a)));
            let result = done
                .iter()
                .find_map(|d| read(d).filter(|a| a.ask == r.ask).map(|a| (d, a)));
            Install {
                tool: r.tool,
                name: r.name,
                version: r.version,
                reason: r.reason,
                stream_id: r.stream_id,
                event_hash: r.ask,
                asked_at: e.ts.clone(),
                allow: answer.as_ref().and_then(|(_, a)| a.allow),
                answered_at: answer.map(|(d, _)| d.ts.clone()),
                ok: result.as_ref().and_then(|(_, a)| a.ok),
                detail: result.as_ref().and_then(|(_, a)| a.detail.clone()),
                done_at: result.map(|(d, _)| d.ts.clone()),
            }
        })
        .collect()
}

/// What a thread's oldest unanswered install ask names: the thread needs
/// the operator.
pub(crate) fn installing_locked(heart: &bakery_heart::Heart, stream_id: &str) -> Option<String> {
    let asks = heart.events_of_family(stream_id, INSTALL_ASK).ok()?;
    if asks.is_empty() {
        return None;
    }
    let decided = heart.events_of_family(stream_id, INSTALL_DECIDED).ok()?;
    asks.iter()
        .filter(|a| !decided.iter().any(|d| d.causality.contains(&a.event_hash)))
        .find_map(|a| serde_json::from_str::<Record>(&a.payload).ok())
        .map(|r| format!("{} {}", r.name, r.version))
}

/// What's already in reach of the project's agents, needing no install:
/// a Flutter its own copies hold, or the operator's (cloned at once), a
/// mise version among the operator's or its own installs, or a gem it holds.
fn already(state: &AppState, project_id: &str, req: &InstallReq) -> Option<String> {
    let (own, home) = (
        own(state, project_id),
        crate::machine_access::home_of(state),
    );
    match req.tool.as_str() {
        "flutter" => {
            let sdk = own.join("fvm/versions").join(&req.version);
            let theirs = fvm_versions(&home).join(&req.version);
            if sdk.join("bin/flutter").is_file() {
                return Some(format!(
                    "Flutter {} is already in this project's own copies",
                    req.version
                ));
            }
            (theirs.join("bin/flutter").is_file() && clone_tree(&theirs, &sdk).is_ok()).then(|| {
                format!(
                    "Flutter {} is the operator's: it's now cloned into this project's own copies",
                    req.version
                )
            })
        }
        "mise" => [mise_installs(&home), own.join("mise/installs")]
            .iter()
            .any(|dir| dir.join(&req.name).join(&req.version).is_dir())
            .then(|| format!("{} {} is already installed for mise", req.name, req.version)),
        _ => own
            .join("gems/specifications")
            .join(format!("{}-{}.gemspec", req.name, req.version))
            .is_file()
            .then(|| {
                format!(
                    "the gem {} {} is already in this project's own copies",
                    req.name, req.version
                )
            }),
    }
}

/// `toolchain_request`: an agent asks the operator for a toolchain or
/// version in its project's own copies; the operator's answer, and the
/// install's result, come back to its conversation.
pub(crate) fn request(state: &AppState, req: &InstallReq) -> Result<String, String> {
    let Some((project_id, _)) = mac_project(state, &req.stream_id) else {
        return Err("installs are asked for in a Mac sandbox project, and this conversation's project doesn't run in one".to_string());
    };
    if !matches!(req.tool.as_str(), "flutter" | "mise" | "gem") {
        return Err(format!(
            "tool must be flutter, mise or gem, not {:?}",
            req.tool
        ));
    }
    let name = if req.tool == "flutter" {
        "flutter".to_string()
    } else {
        plain(&req.name, "name")?
    };
    let req = InstallReq {
        name,
        version: plain(&req.version, "version")?,
        reason: req.reason.trim().chars().take(500).collect(),
        ..req.clone()
    };
    if let Some(here) = already(state, &project_id, &req) {
        return Ok(format!("{here}: use it."));
    }
    let what = format!("{} {}", req.name, req.version);
    if crate::learning::scratch(state, &req.stream_id) {
        return Err(crate::learning::refuse(state, &req.stream_id, &format!("installing {what}")));
    }
    let _one = one_install_at_a_time();
    let open = installs(state, &project_id).into_iter().any(|i| {
        i.allow.is_none() && i.tool == req.tool && i.name == req.name && i.version == req.version
    });
    if !open {
        let mut record = Record {
            project_id: project_id.clone(),
            tool: req.tool.clone(),
            name: req.name.clone(),
            version: req.version.clone(),
            reason: req.reason.clone(),
            ..Record::default()
        };
        let card = serde_json::to_string(&record).map_err(|e| e.to_string())?;
        let card =
            crate::state::append(state, &req.stream_id, "bakery", INSTALL_ASK, card, vec![])?;
        record.stream_id = req.stream_id.clone();
        record.ask = card.event_hash.clone();
        let line = serde_json::to_string(&record).map_err(|e| e.to_string())?;
        crate::state::append(
            state,
            &project_id,
            "bakery",
            INSTALL_ASK,
            line,
            vec![card.event_hash],
        )?;
    }
    Ok(format!(
        "Asked the operator to install {what} into this project's own copies. Carry on with what you can: Bakery tells you once it's installed, or if they say no."
    ))
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct InstallDecide {
    pub(crate) project_id: String,
    /// The ask's event in its thread, as its card and the list name it.
    pub(crate) event_hash: String,
    pub(crate) allow: bool,
}

/// The operator's answer to an install ask. Yes installs it into the
/// project's own copies, on the node, then tells the thread how it went;
/// no tells the thread so.
pub(crate) fn decide_install(
    state: &AppState,
    req: &InstallDecide,
) -> Result<Vec<Install>, (StatusCode, String)> {
    let internal = |e: String| (StatusCode::INTERNAL_SERVER_ERROR, e);
    if crate::actor::agent_calling() {
        return Err((
            StatusCode::FORBIDDEN,
            "only the operator answers an install".to_string(),
        ));
    }
    let one = one_install_at_a_time();
    let asked = installs(state, &req.project_id)
        .into_iter()
        .find(|i| i.event_hash == req.event_hash)
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                "no such install ask in this project".to_string(),
            )
        })?;
    if asked.allow.is_some() {
        return Err((
            StatusCode::CONFLICT,
            "this install was already answered".to_string(),
        ));
    }
    let record = Record {
        project_id: req.project_id.clone(),
        tool: asked.tool.clone(),
        name: asked.name.clone(),
        version: asked.version.clone(),
        ask: asked.event_hash.clone(),
        allow: Some(req.allow),
        ..Record::default()
    };
    let payload = serde_json::to_string(&record).map_err(|e| internal(e.to_string()))?;
    crate::state::append(
        state,
        &req.project_id,
        "human",
        INSTALL_DECIDED,
        payload.clone(),
        vec![],
    )
    .map_err(internal)?;
    crate::state::append(
        state,
        &asked.stream_id,
        "human",
        INSTALL_DECIDED,
        payload,
        vec![asked.event_hash.clone()],
    )
    .map_err(internal)?;
    drop(one);
    let what = format!("{} {}", asked.name, asked.version);
    if req.allow {
        let (state, project_id) = (state.clone(), req.project_id.clone());
        std::thread::spawn(move || install(&state, &project_id, &asked));
    } else {
        crate::agent_control::tell(
            state,
            &asked.stream_id,
            format!("The operator said no to installing {what}. Carry on without it."),
        )
        .map_err(internal)?;
    }
    Ok(installs(state, &req.project_id))
}

/// The installer a toolchain's install runs: Homebrew's `fvm`, `mise` or
/// `gem`. Tests put `echo` in its place, so nothing is ever fetched.
fn installer(tool: &str) -> std::process::Command {
    if cfg!(test) {
        let mut echo = std::process::Command::new("/bin/echo");
        echo.arg(tool);
        echo
    } else {
        std::process::Command::new(Path::new(HOMEBREW).join("bin").join(tool))
    }
}

/// Runs an install the operator allowed, into the project's own copies,
/// and records how it went, in the project's list and the thread.
// ponytail: no time limit on an installer; add one if one ever hangs.
fn install(state: &AppState, project_id: &str, asked: &Install) {
    let (own, home) = (
        own(state, project_id),
        crate::machine_access::home_of(state),
    );
    let brew = installer;
    let run = |mut cmd: std::process::Command| -> (bool, String) {
        match cmd.output() {
            Ok(out) => {
                let text = format!(
                    "{}{}",
                    String::from_utf8_lossy(&out.stdout),
                    String::from_utf8_lossy(&out.stderr)
                );
                let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
                let tail = lines[lines.len().saturating_sub(20)..].join("\n");
                (out.status.success(), tail.chars().take(2000).collect())
            }
            Err(e) => (false, e.to_string()),
        }
    };
    let mise = |cmd: &mut std::process::Command| {
        cmd.env("MISE_DATA_DIR", own.join("mise"))
            .env("MISE_SHARED_INSTALL_DIRS", mise_installs(&home))
            .env("MISE_CONFIG_DIR", own.join("mise-state/config"))
            .env("MISE_STATE_DIR", own.join("mise-state"))
            .env("MISE_CACHE_DIR", own.join("mise-state/cache"));
    };
    let (ok, detail) = match asked.tool.as_str() {
        "flutter" => {
            let mut cmd = brew("fvm");
            cmd.args(["install", &asked.version])
                .env("FVM_CACHE_PATH", own.join("fvm"));
            run(cmd)
        }
        "mise" => {
            let mut cmd = brew("mise");
            cmd.args(["install", &format!("{}@{}", asked.name, asked.version)]);
            mise(&mut cmd);
            let done = run(cmd);
            let mut reshim = brew("mise");
            reshim.arg("reshim").env("MISE_OFFLINE", "1");
            mise(&mut reshim);
            let _ = reshim.output();
            done
        }
        _ => {
            let mut cmd = brew("gem");
            cmd.args([
                "install",
                &asked.name,
                "-v",
                &asked.version,
                "--no-document",
                "--install-dir",
            ])
            .arg(own.join("gems"))
            .env("GEM_HOME", own.join("gems"));
            run(cmd)
        }
    };
    let record = Record {
        project_id: project_id.to_string(),
        tool: asked.tool.clone(),
        name: asked.name.clone(),
        version: asked.version.clone(),
        ask: asked.event_hash.clone(),
        ok: Some(ok),
        detail: Some(detail.clone()),
        ..Record::default()
    };
    let payload = serde_json::to_string(&record).unwrap_or_default();
    let _ = crate::state::append(
        state,
        project_id,
        "bakery",
        INSTALL_DONE,
        payload.clone(),
        vec![],
    );
    let _ = crate::state::append(
        state,
        &asked.stream_id,
        "bakery",
        INSTALL_DONE,
        payload,
        vec![asked.event_hash.clone()],
    );
    let what = format!("{} {}", asked.name, asked.version);
    let said = if ok {
        format!("The operator allowed installing {what}, and it's installed in this project's own copies. Carry on.")
    } else {
        let last = detail.lines().last().unwrap_or("no output");
        format!("The operator allowed installing {what}, but it failed: {last}. Tell the operator what you need instead.")
    };
    let _ = crate::agent_control::tell(state, &asked.stream_id, said);
}

/// `POST /project/toolchain/request`: an agent asks for an install.
pub(crate) async fn request_route(
    State(st): State<AppState>,
    Json(req): Json<InstallReq>,
) -> Result<Json<String>, (StatusCode, String)> {
    crate::actor::spawn_blocking(move || request(&st, &req))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))
}

/// `POST /project/toolchain/decide`: the operator's answer to an install.
pub(crate) async fn decide_route(
    State(st): State<AppState>,
    Json(req): Json<InstallDecide>,
) -> Result<Json<Vec<Install>>, (StatusCode, String)> {
    crate::actor::spawn_blocking(move || decide_install(&st, &req))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map(Json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// A Mac sandbox thread runs its project's pinned Flutter from a clone
    /// of the operator's, first on PATH, with its own caches; installs ask,
    /// so its toolchain copies are read-only; a thread whose project runs
    /// elsewhere gets none of it.
    #[test]
    fn a_mac_sandbox_thread_runs_its_projects_own_copies_first() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join(".bakery");
        std::fs::create_dir_all(&base).unwrap();
        let state = crate::testutil::state_with(&base, Arc::new(bakery_agent::StubModel));
        let home = crate::machine_access::home_of(&state);
        let sdk = fvm_versions(&home).join("3.44.6");
        std::fs::create_dir_all(sdk.join("bin")).unwrap();
        std::fs::write(sdk.join("bin/flutter"), "#!/bin/sh\n").unwrap();
        let app = dir.path().join("farm");
        std::fs::create_dir_all(&app).unwrap();
        std::fs::write(app.join(".fvmrc"), r#"{"flutter": "3.44.6"}"#).unwrap();
        let root = crate::catalog::DirectoryRootSpec::Attach(app.to_str().unwrap());
        let (project, thread) = crate::catalog::create_project(&state, "farm", Some(root)).unwrap();
        // What the operator's caches hold, a private registry's crate among
        // them, and their Rust.
        for file in [
            ".cargo/registry/cache/index.crates.io-1949cf8c6b5b557f/serde-1.0.0.crate",
            ".cargo/registry/cache/their-registry-0123/private-0.1.0.crate",
            ".pub-cache/hosted/pub.dev/http-1.0.0/pubspec.yaml",
            "Library/Caches/org.swift.swiftpm/repositories/SDWebImage-a1d552be/config",
            ".rustup/settings.toml",
            ".cargo/bin/cargo",
        ] {
            std::fs::create_dir_all(home.join(file).parent().unwrap()).unwrap();
            std::fs::write(home.join(file), "").unwrap();
        }

        let container = crate::node::ProjectStacks {
            sandbox: "container".to_string(),
            ..Default::default()
        };
        crate::catalog::set_project_stacks(&state, &project.project_id, container).unwrap();
        let mut elsewhere = SpawnGrants::default();
        extend(&state, &thread.stream_id, &mut elsewhere, true);
        assert!(elsewhere.env.is_empty(), "not a Mac sandbox project");

        let stacks = crate::node::ProjectStacks {
            sandbox: "mac".to_string(),
            ..Default::default()
        };
        crate::catalog::set_project_stacks(&state, &project.project_id, stacks).unwrap();
        let mut grants = SpawnGrants::default();
        extend(&state, &thread.stream_id, &mut grants, true);
        let own = own(&state, &project.project_id);
        let clone = own.join("fvm/versions/3.44.6");
        assert!(
            clone.join("bin/flutter").is_file(),
            "cloned from the operator's"
        );

        // The last of a name counts, as for `env`.
        let var = |name: &str| {
            grants
                .env
                .iter()
                .rev()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
        };
        let ahead = grants
            .env
            .iter()
            .find(|(k, _)| k == bakery_exec::ACP_PATH_PREFIX_ENV)
            .map(|(_, v)| v.clone());
        assert_eq!(
            ahead,
            Some(own.join("bin").display().to_string()),
            "Xcode's tools as a room runs them first"
        );
        assert_eq!(grants.path[0], own.join("bin"), "in command rooms too");
        assert_eq!(grants.path[1], clone.join("bin"), "then its Flutter");
        if let Some(developer) = developer_dir() {
            let xcrun = std::fs::read_to_string(own.join("bin/xcrun")).unwrap();
            assert!(xcrun.contains("xcodebuild -IDEPackageSupportDisableManifestSandbox=YES"), "{xcrun}");
            let xcodebuild = std::fs::read_to_string(own.join("bin/xcodebuild")).unwrap();
            assert!(xcodebuild.contains(&developer.join("usr/bin/xcodebuild").display().to_string()));
        }
        let own_dir = |name: &str| Some(own.join(name).display().to_string());
        assert_eq!(var("FVM_CACHE_PATH"), own_dir("fvm"));
        assert_eq!(var("PUB_CACHE"), own_dir("pub-cache"));
        assert_eq!(var("GEM_HOME"), own_dir("gems"));
        assert_eq!(var("MISE_DATA_DIR"), own_dir("mise"));
        assert_eq!(
            var("MISE_SHARED_INSTALL_DIRS"),
            Some(mise_installs(&home).display().to_string())
        );
        let app = app.canonicalize().unwrap();
        assert_eq!(
            var("MISE_TRUSTED_CONFIG_PATHS"),
            Some(app.display().to_string())
        );
        assert_eq!(var("MISE_EXEC_AUTO_INSTALL").as_deref(), Some("0"));
        assert_eq!(
            var(bakery_exec::NET_ENV).as_deref(),
            Some("local"),
            "this Mac only"
        );
        let door = crate::doors::door(&state, &thread.stream_id).unwrap();
        assert_eq!(
            var("HTTPS_PROXY"),
            Some(format!("http://127.0.0.1:{door}")),
            "past it, its thread's own door"
        );
        assert_eq!(
            var("CFFIXED_USER_HOME"),
            own_dir("home"),
            "Xcode's build files its own"
        );
        assert_eq!(var(bakery_exec::XCODE_ENV).as_deref(), Some("1"));
        assert_eq!(
            var("CLANG_MODULE_CACHE_PATH"),
            own_dir("home/Library/Caches/clang/ModuleCache"),
            "never the operator's module cache"
        );
        assert_eq!(var("CARGO_HOME"), None, "its project's build folder gives it (`builds`)");
        assert_eq!(var("RUSTUP_HOME"), Some(home.join(".rustup").display().to_string()));
        assert!(grants.exec.contains(&home.join(".cargo/bin")));
        assert!(
            grants.read.contains(&home.join(".pub-cache/hosted/pub.dev")),
            "the Flutter tool's own packages"
        );

        // The operator's caches, cloned: crates.io's and pub.dev's packages
        // and Swift packages; never another registry's.
        let registry = crate::builds::cargo_home(&state, &project.project_id).join("registry/cache");
        assert!(registry.join("index.crates.io-1949cf8c6b5b557f/serde-1.0.0.crate").is_file());
        assert!(!registry.join("their-registry-0123").exists());
        assert!(own.join("pub-cache/hosted/pub.dev/http-1.0.0/pubspec.yaml").is_file());
        assert!(own
            .join("home/Library/Caches/org.swift.swiftpm/repositories/SDWebImage-a1d552be/config")
            .is_file());

        // Installs ask: its toolchain copies are read-only, but its Flutter
        // writes its own SDK and its caches are its own to fill.
        assert!(grants.write.contains(&clone));
        assert!(grants.write.contains(&own.join("pub-cache")));
        assert!(!grants.write.contains(&own.join("mise")));
        assert!(grants.exec.contains(&own.join("mise")));
        assert!(
            !grants.write.iter().any(|p| p.starts_with(home.join("fvm"))),
            "theirs stays theirs"
        );
        let told = crate::machine_access::session_line(&state, &thread.stream_id).unwrap();
        assert!(
            told.contains("its own Flutter 3.44.6 is first on PATH"),
            "{told}"
        );
        assert!(told.contains("ask with toolchain_request"), "{told}");
    }

    /// Live, on this Mac: the operator's Flutter, cloned into a scratch copy,
    /// and mise's Node, Homebrew's pod and Xcode's clang, run in a command
    /// room with the Mac sandbox's grants. Reads the operator's toolchains;
    /// writes only under TMPDIR. Needs FVM's Flutter 3.44.6 and Homebrew.
    ///   cargo test -p bakery-node mac_sandbox::tests::live -- --ignored --nocapture
    #[test]
    #[ignore = "runs the operator's own toolchains"]
    fn live_the_operators_toolchains_run_in_a_mac_sandbox_room() {
        let home = PathBuf::from(std::env::var("HOME").unwrap());
        let scratch = tempfile::tempdir().unwrap();
        let own = scratch.path().join("own");
        let sdk = own.join("fvm/versions/3.44.6");
        clone_tree(&fvm_versions(&home).join("3.44.6"), &sdk).unwrap();
        for dir in [
            "mise",
            "mise-state",
            "gems",
            "pub-cache",
            "cocoapods",
            "npm",
        ] {
            std::fs::create_dir_all(own.join(dir)).unwrap();
        }
        let reshim = std::process::Command::new(Path::new(HOMEBREW).join("bin/mise"))
            .arg("reshim")
            .env("MISE_OFFLINE", "1")
            .env("MISE_DATA_DIR", own.join("mise"))
            .env("MISE_SHARED_INSTALL_DIRS", mise_installs(&home))
            .env("MISE_CONFIG_DIR", own.join("mise-state/config"))
            .env("MISE_STATE_DIR", own.join("mise-state"))
            .env("MISE_CACHE_DIR", own.join("mise-state/cache"))
            .output()
            .unwrap();
        assert!(
            reshim.status.success(),
            "{}",
            String::from_utf8_lossy(&reshim.stderr)
        );
        let workspace = scratch.path().join("app");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(workspace.join("mise.toml"), "[tools]\nnode = \"24\"\n").unwrap();

        // Every room's own: Homebrew's tools and mise's installs, read and run.
        let mut grants = SpawnGrants::default();
        grants.exec.push(PathBuf::from(HOMEBREW));
        grants.exec.push(home.join(".local/share/mise"));
        grants.path.push(Path::new(HOMEBREW).join("bin"));
        apply(
            &mut grants,
            &home,
            &own,
            Some(&sdk),
            std::slice::from_ref(&workspace),
        );

        let env =
            bakery_exec::ensure_thin_room(&workspace, &scratch.path().join("cache"), "wt:live-mac")
                .unwrap();
        let checks: [(&[&str], &str); 5] = [
            (
                &[
                    "flutter",
                    "--version",
                    "--no-version-check",
                    "--suppress-analytics",
                ],
                "Flutter 3.44.6",
            ),
            (&["dart", "--version"], "Dart SDK"),
            (&["node", "--version"], "v24."),
            (&["sh", "-c", "pod --version 2>&1"], "1."),
            (&["xcrun", "--find", "clang"], "/clang"),
        ];
        for (argv, want) in checks {
            let argv: Vec<String> = argv.iter().map(|a| a.to_string()).collect();
            let out = bakery_exec::attach_exec_workspace_rw(
                &env,
                &workspace,
                &bakery_exec::WorkspaceRun {
                    argv: &argv,
                    extra_deny_read: &grants.deny,
                    timeout: Duration::from_secs(300),
                    extra_read: &grants.read,
                    extra_write: &grants.write,
                    extra_exec: &grants.exec,
                    append_only: &[],
                    whole_machine: None,
                    path: &grants.path,
                    env: &grants.env,
                },
            )
            .unwrap();
            println!("$ {}\n{}{}", argv.join(" "), out.stdout, out.stderr);
            assert!(!out.stdout.contains("UTF-8 encoding"), "{}", out.stdout);
            assert!(
                out.exit_code == 0 && format!("{}{}", out.stdout, out.stderr).contains(want),
                "{argv:?}: exit {}\n{}\n{}",
                out.exit_code,
                out.stdout,
                out.stderr
            );
        }
    }

    /// An agent asks for an install, once however often it asks; the thread
    /// needs the operator until they answer; no tells it so, yes installs
    /// into the project's own copies and tells it how that went; and every
    /// step is in the project's list.
    #[test]
    fn an_install_is_asked_once_answered_by_the_operator_and_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join(".bakery");
        std::fs::create_dir_all(&base).unwrap();
        let state = crate::testutil::state_with(&base, Arc::new(bakery_agent::StubModel));
        let home = crate::machine_access::home_of(&state);
        let app = dir.path().join("farm");
        std::fs::create_dir_all(&app).unwrap();
        let root = crate::catalog::DirectoryRootSpec::Attach(app.to_str().unwrap());
        let (project, thread) = crate::catalog::create_project(&state, "farm", Some(root)).unwrap();
        let ask = |tool: &str, name: &str, version: &str| {
            request(
                &state,
                &InstallReq {
                    stream_id: thread.stream_id.clone(),
                    tool: tool.into(),
                    name: name.into(),
                    version: version.into(),
                    reason: "the functions want Node 22".into(),
                },
            )
        };
        let container = crate::node::ProjectStacks {
            sandbox: "container".to_string(),
            ..Default::default()
        };
        crate::catalog::set_project_stacks(&state, &project.project_id, container).unwrap();
        assert!(ask("mise", "node", "22.23.3")
            .unwrap_err()
            .contains("Mac sandbox"));
        let stacks = crate::node::ProjectStacks {
            sandbox: "mac".to_string(),
            ..Default::default()
        };
        crate::catalog::set_project_stacks(&state, &project.project_id, stacks).unwrap();
        assert!(ask("mise", "--node", "1").is_err(), "never an option");
        assert!(ask("brew", "node", "1").is_err());

        let said = ask("mise", "node", "22.23.3").unwrap();
        assert!(said.contains("Asked the operator"), "{said}");
        ask("mise", "node", "22.23.3").unwrap();
        let p = project.project_id.as_str();
        let list = installs(&state, p);
        assert_eq!(list.len(), 1, "asked once");
        assert_eq!((list[0].name.as_str(), list[0].allow), ("node", None));
        let status = || {
            let heart = state.heart.lock().unwrap();
            crate::agents::conversation_status_locked(&heart, &thread.stream_id, false, 0, None).1
        };
        assert_eq!(status().as_deref(), Some("wants to install node 22.23.3"));

        // The operator's own version needs no install.
        std::fs::create_dir_all(mise_installs(&home).join("node/24.19.0")).unwrap();
        assert!(ask("mise", "node", "24.19.0")
            .unwrap()
            .contains("already installed"));

        let told = |text: &str| {
            let heart = state.heart.lock().unwrap();
            heart
                .events_of_family(&thread.stream_id, crate::agent_control::FOLLOW_UP_QUEUED)
                .unwrap()
                .iter()
                .filter(|e| e.payload.contains(text))
                .count()
        };
        let answer = |event_hash: &str, allow: bool| {
            decide_install(
                &state,
                &InstallDecide {
                    project_id: p.to_string(),
                    event_hash: event_hash.to_string(),
                    allow,
                },
            )
        };
        let no = answer(&list[0].event_hash, false).unwrap();
        assert_eq!(no[0].allow, Some(false));
        assert_eq!(status(), None, "answered");
        assert_eq!(told("said no to installing node 22.23.3"), 1);
        assert_eq!(
            answer(&list[0].event_hash, true).unwrap_err().0,
            StatusCode::CONFLICT
        );

        ask("gem", "cocoapods", "1.17.0").unwrap();
        let gem = installs(&state, p).pop().unwrap();
        answer(&gem.event_hash, true).unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        let done = loop {
            let last = installs(&state, p).pop().unwrap();
            if last.ok.is_some() || Instant::now() > deadline {
                break last;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        assert_eq!(done.ok, Some(true), "{done:?}");
        let detail = done.detail.unwrap();
        assert!(detail.contains("install cocoapods -v 1.17.0"), "{detail}");
        assert!(
            detail.contains(&own(&state, p).join("gems").display().to_string()),
            "into its own copies"
        );
        assert_eq!(told("installed in this project's own copies"), 1);
    }

    /// Live, on this Mac: a Flutter iOS app builds unsigned in a command room
    /// with the Mac sandbox's grants, offline. BAKERY_LIVE_IOS_REPO names a
    /// scratch clone of the app's repository, BAKERY_LIVE_IOS_APP its app
    /// folder inside it, and BAKERY_LIVE_IOS_OWN the project's own copies
    /// (its Flutter clone under fvm/versions/<pin>, caches, cargo home).
    ///   cargo test -p bakery-node mac_sandbox::tests::live_ios -- --ignored --nocapture
    #[test]
    #[ignore = "builds an iOS app with the operator's Xcode"]
    fn live_ios_builds_unsigned_in_a_mac_sandbox_room() {
        let var = |name: &str| PathBuf::from(std::env::var(name).expect(name));
        let (repo, app, own) = (
            var("BAKERY_LIVE_IOS_REPO"),
            var("BAKERY_LIVE_IOS_APP"),
            var("BAKERY_LIVE_IOS_OWN"),
        );
        let home = PathBuf::from(std::env::var("HOME").unwrap());
        let version = pinned_flutter(std::slice::from_ref(&app)).expect("a pinned Flutter");
        let sdk = own.join("fvm/versions").join(version);
        let cargo = own.join("cargo");
        seed(&home, &own, &cargo);
        shims(&own);
        let mut grants = SpawnGrants::default();
        grants.exec.push(PathBuf::from(HOMEBREW));
        grants.exec.push(home.join(".local/share/mise"));
        grants.path.push(Path::new(HOMEBREW).join("bin"));
        apply(&mut grants, &home, &own, Some(&sdk), std::slice::from_ref(&repo));
        grants.write.push(cargo.clone());
        grants.env.push(("CARGO_HOME".to_string(), cargo.display().to_string()));
        grants
            .env
            .push((bakery_exec::NET_ENV.to_string(), "local".to_string()));
        // With BAKERY_LIVE_IOS_ONLINE, past this Mac through a door: a test
        // node's own, for a scratch project whose list allows github.com.
        let node = tempfile::tempdir().unwrap();
        let _online = std::env::var("BAKERY_LIVE_IOS_ONLINE").is_ok().then(|| {
            let base = node.path().join(".bakery");
            std::fs::create_dir_all(&base).unwrap();
            let state = crate::testutil::state_with(&base, Arc::new(bakery_agent::StubModel));
            let root = crate::catalog::DirectoryRootSpec::Attach(app.to_str().unwrap());
            let (project, thread) = crate::catalog::create_project(&state, "farm", Some(root)).unwrap();
            let allow = crate::host_list::Decide {
                project_id: project.project_id.clone(),
                host: "github.com".to_string(),
                allow: Some(true),
            };
            crate::host_list::decide(&state, &allow).unwrap();
            grants.env.extend(crate::doors::room_env(&state, &thread.stream_id));
            let door = crate::doors::door(&state, &thread.stream_id).unwrap();
            for name in ["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy"] {
                grants.env.push((name.to_string(), format!("http://127.0.0.1:{door}")));
            }
            for name in ["NO_PROXY", "no_proxy"] {
                grants.env.push((name.to_string(), "localhost,127.0.0.1,::1".to_string()));
            }
            println!("online through door {door}");
            state
        });
        let scratch = tempfile::tempdir().unwrap();
        let env = bakery_exec::ensure_thin_room(&repo, &scratch.path().join("cache"), "wt:live-ios")
            .unwrap();
        let rel = app.strip_prefix(&repo).unwrap().display().to_string();
        let script = format!(
            "cd '{rel}' && flutter pub get --offline >/dev/null 2>&1; flutter build ios --debug --no-codesign > '{log}' 2>&1; tail -40 '{log}'",
            log = own.join("home/ios-build.log").display()
        );
        // BAKERY_LIVE_IOS_RUN runs that instead, in the same room (one step
        // of the build, say).
        let script = std::env::var("BAKERY_LIVE_IOS_RUN").unwrap_or(script);
        let argv = vec!["/bin/sh".to_string(), "-c".to_string(), script];
        let out = bakery_exec::attach_exec_workspace_rw(
            &env,
            &repo,
            &bakery_exec::WorkspaceRun {
                argv: &argv,
                extra_deny_read: &grants.deny,
                timeout: bakery_exec::MAX_WORKSPACE_RUN_TIMEOUT,
                extra_read: &grants.read,
                extra_write: &grants.write,
                extra_exec: &grants.exec,
                append_only: &[],
                whole_machine: None,
                path: &grants.path,
                env: &grants.env,
            },
        )
        .unwrap();
        println!("exit {}\n{}{}", out.exit_code, out.stdout, out.stderr);
        assert!(out.stdout.contains("Built build/ios"), "no unsigned build");
    }

    /// The control plane lists what the project's copies hold beyond the
    /// operator's, and keeping one makes it the operator's.
    #[test]
    fn additions_are_listed_and_one_kept_becomes_the_operators() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join(".bakery");
        std::fs::create_dir_all(&base).unwrap();
        let state = crate::testutil::state_with(&base, Arc::new(bakery_agent::StubModel));
        let home = crate::machine_access::home_of(&state);
        let app = dir.path().join("farm");
        std::fs::create_dir_all(&app).unwrap();
        let root = crate::catalog::DirectoryRootSpec::Attach(app.to_str().unwrap());
        let (project, _) = crate::catalog::create_project(&state, "farm", Some(root)).unwrap();
        let own = own(&state, &project.project_id);
        std::fs::create_dir_all(fvm_versions(&home).join("3.44.6")).unwrap();
        for d in [
            "fvm/versions/3.44.6",
            "fvm/versions/3.45.0/bin",
            "mise/installs/node/22.23.3",
        ] {
            std::fs::create_dir_all(own.join(d)).unwrap();
        }
        std::fs::create_dir_all(own.join("gems/specifications")).unwrap();
        std::fs::write(
            own.join("gems/specifications/cocoapods-core-1.16.2.gemspec"),
            "",
        )
        .unwrap();
        let p = project.project_id.clone();
        let one = |tool: &str, name: &str, version: &str| Addition {
            tool: tool.into(),
            name: name.into(),
            version: version.into(),
        };
        assert_eq!(
            additions(&state, &p),
            vec![
                one("flutter", "flutter", "3.45.0"),
                one("mise", "node", "22.23.3"),
                one("gem", "cocoapods-core", "1.16.2"),
            ],
            "3.44.6 is the operator's already"
        );
        let kept = keep(
            &state,
            &KeepReq {
                project_id: p.clone(),
                addition: one("flutter", "flutter", "3.45.0"),
            },
        )
        .unwrap();
        assert!(fvm_versions(&home).join("3.45.0/bin").is_dir());
        assert!(
            !kept.contains(&one("flutter", "flutter", "3.45.0")),
            "now the operator's"
        );
        let missing = keep(
            &state,
            &KeepReq {
                project_id: p,
                addition: one("mise", "node", "99.0.0"),
            },
        )
        .unwrap_err();
        assert_eq!(missing.0, StatusCode::NOT_FOUND);
    }
}
