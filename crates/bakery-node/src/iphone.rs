//! "Build to my iPhone", the operator's side of it. A thread builds its iOS
//! app unsigned in its own sandbox (`flutter build ios --no-codesign`); the
//! operator's button signs a copy with their own certificate, for their own
//! provisioning profile, and installs it on their iPhone. Only Apple's tools
//! touch their key, and nothing the thread's agents wrote runs here: no
//! Podfile, build script or build hook. The copy is made in the thread's own
//! room, so it holds nothing that room couldn't read, and holds no link.

use crate::state::AppState;
use axum::{extract::State, http::StatusCode, Json};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

type Fail = (StatusCode, String);

fn conflict(message: impl Into<String>) -> Fail {
    (StatusCode::CONFLICT, message.into())
}

#[derive(Deserialize, schemars::JsonSchema)]
pub(crate) struct IphoneInstallReq {
    pub(crate) stream_id: String,
    /// The iPhone, by its name or identifier; when left out, the one iPhone
    /// paired with this Mac.
    #[serde(default)]
    pub(crate) device: Option<String>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct IphoneInstalled {
    /// The iPhone it's on now.
    pub(crate) device: String,
    pub(crate) bundle_id: String,
    /// When the thread built it (RFC 3339).
    pub(crate) built: String,
    /// The provisioning profile it's signed for.
    pub(crate) profile: String,
}

/// Runs an Apple tool to the end, or gives up after `timeout`.
fn run(cmd: Command, timeout: Duration) -> Result<String, String> {
    let program = cmd.get_program().to_string_lossy().into_owned();
    let (stdout, stderr, code) = bakery_exec::run_with_timeout(cmd, timeout)?;
    if code == 0 {
        return Ok(stdout);
    }
    let said = if stderr.trim().is_empty() { stdout } else { stderr };
    let lines: Vec<&str> = said.trim().lines().collect();
    let tail = lines[lines.len().saturating_sub(6)..].join("\n");
    Err(format!("{program} failed: {tail}"))
}

/// One value from a property list, as `plutil` prints it in `format`
/// (`raw`, `json`, `xml1`).
fn plist(file: &Path, key: &str, format: &str) -> Option<String> {
    let out = Command::new("/usr/bin/plutil")
        .args(["-extract", key, format, "-o", "-"])
        .arg(file)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn plutil(args: &[&str], file: &Path) -> Result<(), String> {
    let mut cmd = Command::new("/usr/bin/plutil");
    cmd.args(args).arg(file);
    run(cmd, Duration::from_secs(30)).map(|_| ())
}

/// The thread's newest unsigned iPhone build: Flutter's
/// `build/ios/iphoneos/<name>.app`, in the workspace's own folder or an
/// app's folder one down (GreenLeaf's `frontend`).
fn built_app(workspace: &Path) -> Option<PathBuf> {
    let folders = std::iter::once(workspace.to_path_buf()).chain(
        std::fs::read_dir(workspace)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .map(|e| e.path()),
    );
    let mut apps = Vec::new();
    for folder in folders {
        let built = std::fs::read_dir(folder.join("build/ios/iphoneos"));
        for entry in built.into_iter().flatten().flatten() {
            let path = entry.path();
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.is_dir() && path.extension().is_some_and(|e| e == "app") {
                apps.push((meta.modified().ok(), path));
            }
        }
    }
    apps.into_iter().max_by_key(|(at, _)| *at).map(|(_, p)| p)
}

/// A copy of the thread's `app` in `stage`, made in the thread's own room:
/// whatever its folders point at, the copy holds only what the room can
/// read. Refused if it holds a link, or anything but folders and files.
fn stage(state: &AppState, stream_id: &str, workspace: &Path, app: &Path, stage: &Path) -> Result<PathBuf, Fail> {
    let internal = |e: String| (StatusCode::INTERNAL_SERVER_ERROR, e);
    let cache_root = crate::state::cache_root(state);
    let env = bakery_exec::ensure_thin_room(workspace, &cache_root, stream_id).map_err(internal)?;
    let copy = stage.join(app.file_name().unwrap_or_default());
    let argv = vec![
        "/bin/cp".to_string(),
        "-RP".to_string(),
        app.display().to_string(),
        copy.display().to_string(),
    ];
    let out = bakery_exec::attach_exec_granted(
        &env,
        &argv,
        Some(workspace),
        bakery_exec::ExtraGrants {
            write: &[stage.to_path_buf()],
            ..Default::default()
        },
        Duration::from_secs(600),
    )
    .map_err(internal)?;
    if out.exit_code != 0 {
        return Err(conflict(format!(
            "Couldn't copy the build out of the thread's room: {}",
            out.stderr.trim()
        )));
    }
    fn plain(dir: &Path, root: &Path) -> Result<(), String> {
        for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())?.flatten() {
            let kind = entry.file_type().map_err(|e| e.to_string())?;
            let path = entry.path();
            let shown = path.strip_prefix(root).unwrap_or(&path).display().to_string();
            if kind.is_symlink() {
                return Err(format!("The build holds a link, {shown}: refused."));
            } else if kind.is_dir() {
                plain(&path, root)?;
            } else if !kind.is_file() {
                return Err(format!("The build holds something not a file, {shown}: refused."));
            }
        }
        Ok(())
    }
    plain(&copy, stage).map_err(conflict)?;
    let platforms = plist(&copy.join("Info.plist"), "CFBundleSupportedPlatforms", "json");
    if !platforms.unwrap_or_default().contains("\"iPhoneOS\"") {
        return Err(conflict("The thread's build isn't an iPhone app."));
    }
    Ok(copy)
}

/// An iPhone paired with this Mac.
#[derive(Debug, Clone, PartialEq)]
struct Device {
    identifier: String,
    name: String,
    udid: String,
}

/// The iPhones paired with this Mac, from `devicectl list devices`' JSON.
fn iphones(listing: &serde_json::Value) -> Vec<Device> {
    let devices = listing.pointer("/result/devices").and_then(|d| d.as_array());
    devices
        .into_iter()
        .flatten()
        .filter_map(|d| {
            let s = |p: &str| d.pointer(p).and_then(|v| v.as_str()).map(str::to_string);
            let paired = s("/connectionProperties/pairingState")? == "paired";
            (paired && s("/hardwareProperties/platform")? == "iOS").then_some(Device {
                identifier: s("/identifier")?,
                name: s("/deviceProperties/name")?,
                udid: s("/hardwareProperties/udid")?,
            })
        })
        .collect()
}

/// The iPhone asked for, or the only one.
fn choose(devices: &[Device], wanted: Option<&str>) -> Result<Device, String> {
    let names = devices.iter().map(|d| d.name.as_str()).collect::<Vec<_>>().join(", ");
    match (wanted, devices) {
        (Some(w), _) => devices
            .iter()
            .find(|d| d.name == w || d.identifier == w || d.udid == w)
            .cloned()
            .ok_or_else(|| format!("No iPhone \"{w}\" is paired with this Mac (paired: {names}).")),
        (None, [only]) => Ok(only.clone()),
        (None, []) => Err("No iPhone is paired with this Mac: connect it once by cable, unlock it, trust this Mac, and turn on Developer Mode.".to_string()),
        (None, _) => Err(format!("Several iPhones are paired with this Mac; say which: {names}.")),
    }
}

/// A provisioning profile on this Mac, as Xcode keeps them.
#[derive(Debug, Clone)]
struct Profile {
    /// The `.mobileprovision` file, embedded as it is.
    file: PathBuf,
    /// Its contents, decoded.
    plist: PathBuf,
    name: String,
    team: String,
    /// `<team>.<bundle ID>`, or a pattern ending in `*`.
    app_id: String,
    devices: Vec<String>,
    all_devices: bool,
    expires: DateTime<Utc>,
    /// Its certificates' SHA-1s, as `security find-identity` prints them.
    certificates: Vec<String>,
}

/// A profile from its decoded contents.
fn profile(file: &Path, plist: &Path) -> Option<Profile> {
    use base64::Engine;
    use sha1::Digest;
    let get = |key: &str| self::plist(plist, key, "raw");
    let mut certificates = Vec::new();
    while let Some(der) = get(&format!("DeveloperCertificates.{}", certificates.len())) {
        let der = base64::engine::general_purpose::STANDARD.decode(der).ok()?;
        let hash = sha1::Sha1::digest(der);
        certificates.push(hash.iter().map(|b| format!("{b:02X}")).collect());
    }
    let devices = self::plist(plist, "ProvisionedDevices", "json")
        .and_then(|j| serde_json::from_str(&j).ok())
        .unwrap_or_default();
    Some(Profile {
        file: file.to_path_buf(),
        plist: plist.to_path_buf(),
        name: get("Name")?,
        team: get("TeamIdentifier.0")?,
        app_id: get("Entitlements.application-identifier")?,
        devices,
        all_devices: get("ProvisionsAllDevices").as_deref() == Some("true"),
        expires: get("ExpirationDate")?.parse().ok()?,
        certificates,
    })
}

/// Every provisioning profile on this Mac, decoded into `work`.
fn profiles(home: &Path, work: &Path) -> Vec<Profile> {
    let folders = [
        "Library/Developer/Xcode/UserData/Provisioning Profiles",
        "Library/MobileDevice/Provisioning Profiles",
    ];
    let files = folders
        .iter()
        .flat_map(|f| std::fs::read_dir(home.join(f)).into_iter().flatten().flatten())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "mobileprovision"));
    files
        .enumerate()
        .filter_map(|(n, file)| {
            let decoded = work.join(format!("profile-{n}.plist"));
            let mut cms = Command::new("/usr/bin/security");
            cms.args(["cms", "-D", "-i"]).arg(&file).arg("-o").arg(&decoded);
            run(cms, Duration::from_secs(30)).ok()?;
            profile(&file, &decoded)
        })
        .collect()
}

/// The profile that signs `bundle_id` for the iPhone `udid`: unexpired,
/// listing that iPhone, its own app ID before a pattern, then the latest.
fn pick<'a>(profiles: &'a [Profile], bundle_id: &str, udid: &str, now: DateTime<Utc>) -> Option<&'a Profile> {
    profiles
        .iter()
        .filter(|p| p.expires > now && (p.all_devices || p.devices.iter().any(|d| d == udid)))
        .filter_map(|p| {
            let pattern = p.app_id.strip_prefix(&format!("{}.", p.team))?;
            let exact = pattern == bundle_id;
            let covers = pattern
                .strip_suffix('*')
                .is_some_and(|prefix| bundle_id.starts_with(prefix));
            (exact || covers).then_some((exact, p.expires, p))
        })
        .max_by_key(|(exact, expires, _)| (*exact, *expires))
        .map(|(_, _, p)| p)
}

/// `security find-identity -v -p codesigning`'s signing identities, as
/// their certificates' SHA-1s.
fn identities(listing: &str) -> Vec<String> {
    listing
        .lines()
        .filter_map(|line| {
            let (_, rest) = line.trim().split_once(") ")?;
            let hash = rest.split(' ').next()?;
            (hash.len() == 40 && hash.chars().all(|c| c.is_ascii_hexdigit()))
                .then(|| hash.to_ascii_uppercase())
        })
        .collect()
}

/// The entitlements `bundle_id` is signed with, written to `to`: its
/// profile's, with a pattern made its own app ID, as Xcode signs it.
fn entitlements(profile: &Profile, bundle_id: &str, to: &Path) -> Result<(), String> {
    let mut extract = Command::new("/usr/bin/plutil");
    extract
        .args(["-extract", "Entitlements", "xml1", "-o"])
        .arg(to)
        .arg(&profile.plist);
    run(extract, Duration::from_secs(30))?;
    let own = format!("{}.{bundle_id}", profile.team);
    plutil(&["-replace", "application-identifier", "-string", &own], to)?;
    if let Some(groups) = plist(to, "keychain-access-groups", "json") {
        let groups: Vec<String> = serde_json::from_str(&groups).unwrap_or_default();
        let groups: Vec<String> = groups
            .into_iter()
            .map(|g| if g.ends_with('*') { own.clone() } else { g })
            .collect();
        let groups = serde_json::to_string(&groups).map_err(|e| e.to_string())?;
        plutil(&["-replace", "keychain-access-groups", "-json", &groups], to)?;
    }
    Ok(())
}

fn codesign(identity: &str, path: &Path, entitlements: Option<&Path>) -> Result<(), String> {
    let mut cmd = Command::new("/usr/bin/codesign");
    cmd.args(["--force", "--sign", identity, "--timestamp=none"]);
    if let Some(entitlements) = entitlements {
        cmd.arg("--entitlements")
            .arg(entitlements)
            .arg("--generate-entitlement-der");
    }
    cmd.arg(path);
    run(cmd, Duration::from_secs(300)).map(|_| ())
}

/// Signs `bundle` from the inside out, as Xcode does: what it embeds, its
/// app extensions (each for its own profile), then itself, its profile
/// embedded. `identity_of` names the signing identity for a profile.
/// Returns the profile's name.
fn sign(
    bundle: &Path,
    profiles: &[Profile],
    identity_of: &dyn Fn(&Profile) -> Option<String>,
    udid: &str,
    work: &Path,
    step: &dyn Fn(String),
) -> Result<String, String> {
    let named = |path: &Path| path.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let inside = |dir: &str| {
        let mut paths: Vec<PathBuf> = std::fs::read_dir(bundle.join(dir))
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .collect();
        paths.sort();
        paths
    };
    let bundle_id = plist(&bundle.join("Info.plist"), "CFBundleIdentifier", "raw")
        .ok_or_else(|| format!("{} has no bundle identifier.", bundle.display()))?;
    let profile = pick(profiles, &bundle_id, udid, Utc::now()).ok_or_else(|| {
        format!("No provisioning profile on this Mac covers {bundle_id} on this iPhone. Building it to this iPhone once from Xcode makes one.")
    })?;
    let identity = identity_of(profile).ok_or_else(|| {
        format!(
            "No signing certificate on this Mac matches the profile \"{}\".",
            profile.name
        )
    })?;
    for path in inside("Frameworks") {
        if path.extension().is_some_and(|e| e == "framework" || e == "dylib") {
            step(format!("Signing {}", named(&path)));
            codesign(&identity, &path, None)?;
        }
    }
    for path in inside("PlugIns") {
        if path.extension().is_some_and(|e| e == "appex") {
            sign(&path, profiles, identity_of, udid, work, step)?;
        }
    }
    std::fs::copy(&profile.file, bundle.join("embedded.mobileprovision"))
        .map_err(|e| e.to_string())?;
    let granted = work.join(format!("{bundle_id}.entitlements"));
    entitlements(profile, &bundle_id, &granted)?;
    step(format!("Signing {} for \"{}\"", named(bundle), profile.name));
    codesign(&identity, bundle, Some(&granted))?;
    Ok(profile.name.clone())
}

/// A fresh folder of the node's own, removed when dropped.
struct Work(PathBuf);

impl Work {
    fn new() -> Result<Self, String> {
        use std::os::unix::fs::DirBuilderExt;
        let nonce: u64 = rand_nonce();
        let dir = std::env::temp_dir().join(format!("bakery-iphone-{}-{nonce:016x}", std::process::id()));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&dir)
            .map_err(|e| e.to_string())?;
        Ok(Work(dir))
    }
}

impl Drop for Work {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn rand_nonce() -> u64 {
    let mut bytes = [0u8; 8];
    // SAFETY: `bytes` is writable for its length.
    unsafe { libc::arc4random_buf(bytes.as_mut_ptr().cast(), bytes.len()) };
    u64::from_ne_bytes(bytes)
}

/// One step of a press, live to the app while it runs: a
/// `{"kind":"iphone-progress"}` envelope on its event socket, never history.
fn progress(state: &AppState, stream_id: &str, step: &str) {
    let envelope = serde_json::json!({
        "kind": "iphone-progress",
        "stream_id": stream_id,
        "step": step,
        "at": Utc::now().to_rfc3339(),
    });
    let _ = state.tx.send(envelope.to_string());
}

/// The paired iPhones, as `devicectl` lists them.
fn paired(work: &Path) -> Result<Vec<Device>, Fail> {
    let listing = work.join("devices.json");
    let mut list = Command::new("/usr/bin/xcrun");
    list.args(["devicectl", "list", "devices", "--quiet", "--json-output"])
        .arg(&listing);
    run(list, Duration::from_secs(60)).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let listing: serde_json::Value = std::fs::read_to_string(&listing)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    Ok(iphones(&listing))
}

/// The button: the thread's newest unsigned build, signed and installed on
/// the operator's iPhone, each step told live ([`progress`]).
fn install(state: &AppState, req: &IphoneInstallReq) -> Result<IphoneInstalled, Fail> {
    let step = |what: String| progress(state, &req.stream_id, &what);
    let internal = |e: String| (StatusCode::INTERNAL_SERVER_ERROR, e);
    let context = crate::state::context_for(state, &req.stream_id).map_err(|e| (StatusCode::NOT_FOUND, e))?;
    let workspace = PathBuf::from(&context.worktree_path)
        .canonicalize()
        .map_err(|e| internal(e.to_string()))?;
    let app = built_app(&workspace).ok_or_else(|| {
        conflict("This thread has no iPhone build yet: its agent makes one with `flutter build ios --no-codesign`.")
    })?;
    let built = std::fs::symlink_metadata(&app)
        .and_then(|m| m.modified())
        .map(|t| DateTime::<Utc>::from(t).to_rfc3339())
        .unwrap_or_default();
    let work = Work::new().map_err(internal)?;
    let staged_in = work.0.join("app");
    std::fs::create_dir(&staged_in).map_err(|e| internal(e.to_string()))?;
    step("Copying the build out of the thread's room".to_string());
    let copy = stage(state, &req.stream_id, &workspace, &app, &staged_in)?;

    step("Finding your iPhone".to_string());
    let device = choose(&paired(&work.0)?, req.device.as_deref()).map_err(conflict)?;

    step("Reading your provisioning profiles".to_string());
    let home = crate::machine_access::home_of(state);
    let profiles = profiles(&home, &work.0);
    let mut find = Command::new("/usr/bin/security");
    find.args(["find-identity", "-v", "-p", "codesigning"]);
    let identities = identities(&run(find, Duration::from_secs(60)).map_err(internal)?);
    let identity_of = |p: &Profile| {
        p.certificates
            .iter()
            .find(|c| identities.contains(c))
            .cloned()
    };
    let profile = sign(&copy, &profiles, &identity_of, &device.udid, &work.0, &step).map_err(conflict)?;
    let bundle_id = plist(&copy.join("Info.plist"), "CFBundleIdentifier", "raw").unwrap_or_default();

    step(format!("Installing on {}", device.name));
    let mut put = Command::new("/usr/bin/xcrun");
    put.args(["devicectl", "device", "install", "app", "--device", &device.udid])
        .arg(&copy);
    run(put, Duration::from_secs(900)).map_err(|e| (StatusCode::BAD_GATEWAY, e))?;
    step(format!("IphoneInstalled on {}", device.name));
    Ok(IphoneInstalled {
        device: device.name,
        bundle_id,
        built,
        profile,
    })
}

#[derive(Deserialize, schemars::JsonSchema)]
pub(crate) struct IphoneBuildQuery {
    pub(crate) stream_id: String,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct IphoneBuilt {
    /// The app's folder name (`Runner.app`).
    pub(crate) app: String,
    /// When the thread built it (RFC 3339).
    pub(crate) built: String,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct IphoneBuild {
    /// The newest unsigned iPhone build the button would install, if any.
    pub(crate) build: Option<IphoneBuilt>,
}

/// `GET /thread/iphone`: whether the thread has a build for the button, in
/// one call, looked for where the button looks.
pub(crate) async fn build_route(
    State(st): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<IphoneBuildQuery>,
) -> Result<Json<IphoneBuild>, Fail> {
    crate::actor::spawn_blocking(move || {
        let context = crate::state::context_for(&st, &q.stream_id).map_err(|e| (StatusCode::NOT_FOUND, e))?;
        let build = PathBuf::from(&context.worktree_path)
            .canonicalize()
            .ok()
            .and_then(|workspace| built_app(&workspace))
            .map(|app| IphoneBuilt {
                app: app.file_name().unwrap_or_default().to_string_lossy().into_owned(),
                built: std::fs::symlink_metadata(&app)
                    .and_then(|m| m.modified())
                    .map(|t| DateTime::<Utc>::from(t).to_rfc3339())
                    .unwrap_or_default(),
            });
        Ok(Json(IphoneBuild { build }))
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
}

/// The thread's Flutter app with an iOS folder: its workspace's own, or one
/// folder down (GreenLeaf's `frontend`).
fn flutter_app(workspace: &Path) -> Option<PathBuf> {
    let mut below: Vec<PathBuf> = std::fs::read_dir(workspace)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .collect();
    below.sort();
    std::iter::once(workspace.to_path_buf())
        .chain(below)
        .find(|dir| dir.join("pubspec.yaml").is_file() && dir.join("ios").is_dir())
}

/// What "Run on my iPhone (as me)" types for the operator: from the app's
/// folder, its pinned Flutter (FVM's when it pins one) run on `udid`.
fn run_command(workspace: &Path, app: &Path, udid: &str) -> String {
    let fvm = [app, workspace].iter().any(|dir| dir.join(".fvmrc").is_file());
    let flutter = if fvm { "fvm flutter" } else { "flutter" };
    let run = format!("{flutter} run -d {udid}");
    match app.strip_prefix(workspace).ok().filter(|rel| !rel.as_os_str().is_empty()) {
        Some(rel) => format!("cd '{}' && {run}", rel.display().to_string().replace('\'', "'\\''")),
        None => run,
    }
}

#[derive(Deserialize, schemars::JsonSchema)]
pub(crate) struct IphoneRunReq {
    pub(crate) stream_id: String,
    /// The iPhone, by its name or identifier; when left out, the one iPhone
    /// paired with this Mac.
    #[serde(default)]
    pub(crate) device: Option<String>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct IphoneRunPrepared {
    /// The operator's own terminal (not a room) where it waits for Enter.
    pub(crate) terminal_id: String,
    pub(crate) command: String,
    pub(crate) device: String,
}

/// `POST /thread/iphone/run`: "Run on my iPhone (as me)". Types `flutter
/// run` for the thread's app into a new terminal of the operator's own, in
/// its worktree, and runs nothing: the operator presses Enter, and it runs
/// as them, the project's build code included, with live logs and hot
/// reload.
pub(crate) async fn run_route(
    State(st): State<AppState>,
    Json(req): Json<IphoneRunReq>,
) -> Result<Json<IphoneRunPrepared>, Fail> {
    let (stream_id, wanted) = (req.stream_id.clone(), req.device.clone());
    let state = st.clone();
    let (command, device) = crate::actor::spawn_blocking(move || {
        let context = crate::state::context_for(&state, &stream_id).map_err(|e| (StatusCode::NOT_FOUND, e))?;
        let workspace = PathBuf::from(&context.worktree_path)
            .canonicalize()
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        let app = flutter_app(&workspace).ok_or_else(|| conflict("This thread has no Flutter app with an iOS folder."))?;
        let work = Work::new().map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
        let device = choose(&paired(&work.0)?, wanted.as_deref()).map_err(conflict)?;
        Ok::<_, Fail>((run_command(&workspace, &app, &device.udid), device.name))
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))??;
    let name = Some("Run on my iPhone".to_string());
    let info = crate::routes::terminal::spawn_shell(&st, req.stream_id.clone(), "host".to_string(), name).await?;
    let handle = st
        .terminals
        .lock()
        .unwrap()
        .get(&info.terminal_id)
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "its terminal closed at once".to_string()))?;
    crate::routes::terminal::wait_for_prompt(&handle).await;
    handle
        .type_for_operator(&command)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let prepared = serde_json::json!({
        "commands": [{ "terminal_id": info.terminal_id, "name": info.name, "command": command }],
    });
    crate::state::append(&st, &req.stream_id, "human", crate::routes::terminal::PREPARED_FAMILY, prepared.to_string(), vec![])
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(Json(IphoneRunPrepared {
        terminal_id: info.terminal_id,
        command,
        device,
    }))
}

/// `POST /thread/iphone/install`: the operator's "Build to my iPhone".
pub(crate) async fn install_route(
    State(st): State<AppState>,
    Json(req): Json<IphoneInstallReq>,
) -> Result<Json<IphoneInstalled>, Fail> {
    crate::actor::spawn_blocking(move || install(&st, &req).map(Json))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A profile's decoded contents, as `security cms -D` writes them.
    fn profile_plist(dir: &Path, name: &str, app_id: &str, devices: &[&str], expires: &str) -> Profile {
        let devices: String = devices.iter().map(|d| format!("<string>{d}</string>")).collect();
        let xml = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Name</key><string>{name}</string>
<key>TeamIdentifier</key><array><string>TEAM123456</string></array>
<key>ExpirationDate</key><date>{expires}</date>
<key>DeveloperCertificates</key><array><data>Y2VydGlmaWNhdGU=</data></array>
<key>ProvisionedDevices</key><array>{devices}</array>
<key>Entitlements</key><dict>
<key>application-identifier</key><string>TEAM123456.{app_id}</string>
<key>keychain-access-groups</key><array><string>TEAM123456.*</string><string>com.apple.token</string></array>
<key>get-task-allow</key><true/>
</dict>
</dict></plist>"#
        );
        let file = dir.join(format!("{name}.mobileprovision"));
        let decoded = dir.join(format!("{name}.plist"));
        std::fs::write(&file, "the signed profile").unwrap();
        std::fs::write(&decoded, xml).unwrap();
        profile(&file, &decoded).unwrap()
    }

    /// A profile reads as Xcode keeps it, and the one that signs an app is
    /// unexpired, lists the iPhone, and is the app's own before a pattern.
    #[test]
    fn the_profile_for_an_app_is_its_own_on_that_iphone() {
        let dir = tempfile::tempdir().unwrap();
        let farm = profile_plist(dir.path(), "farm", "com.greenleaf.app", &["UDID-1"], "2099-01-01T00:00:00Z");
        assert_eq!(farm.team, "TEAM123456");
        // SHA-1 of "certificate", as find-identity prints a certificate's.
        assert_eq!(farm.certificates, ["735AD571C189D7BA84464BF4A9F1D2280175B128"]);
        let any = profile_plist(dir.path(), "any", "*", &["UDID-1", "UDID-2"], "2099-06-01T00:00:00Z");
        let old = profile_plist(dir.path(), "old", "com.greenleaf.app", &["UDID-1"], "2020-01-01T00:00:00Z");
        let all = [old, any, farm];
        let now = Utc::now();
        let name = |p: Option<&Profile>| p.map(|p| p.name.clone());
        assert_eq!(name(pick(&all, "com.greenleaf.app", "UDID-1", now)), Some("farm".into()), "its own before a pattern");
        assert_eq!(name(pick(&all, "com.greenleaf.app", "UDID-2", now)), Some("any".into()), "the only one with that iPhone");
        assert_eq!(name(pick(&all, "com.other.app", "UDID-3", now)), None, "no iPhone listed");
        assert_eq!(name(pick(&all[..1], "com.greenleaf.app", "UDID-1", now)), None, "expired");
    }

    #[test]
    fn signing_identities_and_iphones_read_as_apples_tools_print_them() {
        let listing = "  1) 0123456789abcdef0123456789ABCDEF01234567 \"Apple Development: Someone (TEAM123456)\"\n  2) 89ABCDEF0123456789ABCDEF0123456789ABCDEF \"Apple Distribution: Someone (TEAM123456)\"\n     2 valid identities found\n";
        assert_eq!(
            identities(listing),
            ["0123456789ABCDEF0123456789ABCDEF01234567", "89ABCDEF0123456789ABCDEF0123456789ABCDEF"]
        );
        let devices: serde_json::Value = serde_json::from_str(
            r#"{"result":{"devices":[
              {"identifier":"A-1","deviceProperties":{"name":"Phone"},"hardwareProperties":{"platform":"iOS","udid":"UDID-1"},"connectionProperties":{"pairingState":"paired"}},
              {"identifier":"W-1","deviceProperties":{"name":"Watch"},"hardwareProperties":{"platform":"watchOS","udid":"UDID-W"},"connectionProperties":{"pairingState":"paired"}},
              {"identifier":"B-1","deviceProperties":{"name":"Not trusted"},"hardwareProperties":{"platform":"iOS","udid":"UDID-2"},"connectionProperties":{"pairingState":"unpaired"}}
            ]}}"#,
        )
        .unwrap();
        let phones = iphones(&devices);
        assert_eq!(phones.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), ["Phone"]);
        assert_eq!(choose(&phones, None).unwrap().udid, "UDID-1");
        assert_eq!(choose(&phones, Some("A-1")).unwrap().name, "Phone");
        assert!(choose(&phones, Some("Other")).unwrap_err().contains("paired: Phone"));
        assert!(choose(&[], None).unwrap_err().contains("Developer Mode"));
        let two = [phones[0].clone(), Device { name: "Second".into(), ..phones[0].clone() }];
        assert!(choose(&two, None).unwrap_err().contains("say which"));
    }

    /// A bundle with Info.plist, its executable a copy of `/usr/bin/true`.
    fn bundle(dir: &Path, id: &str, executable: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::copy("/usr/bin/true", dir.join(executable)).unwrap();
        let info = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict><key>CFBundleIdentifier</key><string>{id}</string><key>CFBundleExecutable</key><string>{executable}</string><key>CFBundleSupportedPlatforms</key><array><string>iPhoneOS</string></array></dict></plist>"#
        );
        std::fs::write(dir.join("Info.plist"), info).unwrap();
    }

    /// An app is signed inside out with its profile embedded and its own app
    /// ID in its entitlements. Signed ad hoc here: the operator's identity
    /// and keychain are never touched by a test.
    #[test]
    fn an_app_is_signed_inside_out_for_its_profile() {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("Runner.app");
        bundle(&app, "com.greenleaf.app", "Runner");
        bundle(&app.join("Frameworks/Flutter.framework"), "io.flutter.flutter", "Flutter");
        bundle(&app.join("PlugIns/Notify.appex"), "com.greenleaf.app.notify", "Notify");
        let any = profile_plist(dir.path(), "any", "com.greenleaf.*", &["UDID-1"], "2099-01-01T00:00:00Z");
        let steps = std::cell::RefCell::new(Vec::new());
        let step = |what: String| steps.borrow_mut().push(what);
        let profile = sign(&app, &[any.clone()], &|_| Some("-".to_string()), "UDID-1", dir.path(), &step).unwrap();
        assert_eq!(profile, "any");
        assert_eq!(
            steps.into_inner(),
            [
                "Signing Flutter.framework",
                "Signing Notify.appex for \"any\"",
                "Signing Runner.app for \"any\"",
            ],
            "inside out, each piece told"
        );
        for signed in [app.join("Frameworks/Flutter.framework"), app.join("PlugIns/Notify.appex"), app.clone()] {
            let verify = Command::new("/usr/bin/codesign").arg("--verify").arg(&signed).output().unwrap();
            assert!(verify.status.success(), "{}: {}", signed.display(), String::from_utf8_lossy(&verify.stderr));
        }
        assert!(app.join("embedded.mobileprovision").is_file());
        assert!(app.join("PlugIns/Notify.appex/embedded.mobileprovision").is_file());
        let shown = Command::new("/usr/bin/codesign")
            .args(["-d", "--entitlements", ":-"])
            .arg(&app)
            .output()
            .unwrap();
        let shown = String::from_utf8_lossy(&shown.stdout);
        assert!(shown.contains("TEAM123456.com.greenleaf.app"), "{shown}");
        assert!(!shown.contains("TEAM123456.*"), "a pattern made its own: {shown}");
        assert!(shown.contains("com.apple.token"), "{shown}");
        let none = sign(&app, &[], &|_| Some("-".to_string()), "UDID-1", dir.path(), &|_| ()).unwrap_err();
        assert!(none.contains("No provisioning profile"), "{none}");
        let no_key = sign(&app, &[any], &|_| None, "UDID-1", dir.path(), &|_| ()).unwrap_err();
        assert!(no_key.contains("No signing certificate"), "{no_key}");
    }

    /// "Run on my iPhone" runs the app's own Flutter from its folder.
    #[test]
    fn running_on_the_iphone_uses_the_apps_folder_and_its_pinned_flutter() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().canonicalize().unwrap();
        assert_eq!(flutter_app(&workspace), None);
        let app = workspace.join("frontend");
        std::fs::create_dir_all(app.join("ios")).unwrap();
        std::fs::write(app.join("pubspec.yaml"), "name: farm\n").unwrap();
        assert_eq!(flutter_app(&workspace), Some(app.clone()));
        assert_eq!(run_command(&workspace, &app, "UDID-1"), "cd 'frontend' && flutter run -d UDID-1");
        std::fs::write(app.join(".fvmrc"), r#"{"flutter": "3.44.6"}"#).unwrap();
        assert_eq!(run_command(&workspace, &app, "UDID-1"), "cd 'frontend' && fvm flutter run -d UDID-1");
        assert_eq!(run_command(&workspace, &workspace, "UDID-1"), "flutter run -d UDID-1");
    }

    /// The build comes out of the thread's room as plain folders and files,
    /// the newest of the workspace's; one holding a link is refused.
    #[test]
    fn a_build_is_copied_out_of_the_room_without_links() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join(".bakery");
        std::fs::create_dir_all(&base).unwrap();
        let state = crate::testutil::state_with(&base, std::sync::Arc::new(bakery_agent::StubModel));
        std::fs::create_dir_all(dir.path().join("farm")).unwrap();
        let workspace = dir.path().join("farm").canonicalize().unwrap();
        assert_eq!(built_app(&workspace), None);
        let app = workspace.join("frontend/build/ios/iphoneos/Runner.app");
        bundle(&app, "com.greenleaf.app", "Runner");
        assert_eq!(built_app(&workspace), Some(app.clone()));

        let work = Work::new().unwrap();
        let copy = stage(&state, "wt:iphone", &workspace, &app, &work.0).unwrap();
        assert!(copy.join("Runner").is_file());

        std::os::unix::fs::symlink(dir.path().join(".bakery"), app.join("elsewhere")).unwrap();
        let again = Work::new().unwrap();
        let refused = stage(&state, "wt:iphone", &workspace, &app, &again.0).unwrap_err();
        assert!(refused.1.contains("holds a link, Runner.app/elsewhere"), "{}", refused.1);
        let gone = work.0.clone();
        drop(work);
        assert!(!gone.exists(), "the node's copy goes when done");
    }
}
