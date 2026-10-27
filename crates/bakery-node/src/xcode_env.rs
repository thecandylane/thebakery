//! Xcode's real tools, worked out once for every room.
//!
//! `/usr/bin/cc`, `ar`, `clang` and `git` are xcrun shims that look their
//! tool up on every call, and a room can't keep xcrun's cache, so a cargo
//! build there asks xcrun and xcodebuild again and again; the cc crate also
//! asks xcrun for the SDK path unless SDKROOT is set. Here the node finds
//! the folders once, and a room gets DEVELOPER_DIR, SDKROOT and the real
//! tools' folders ahead of /usr/bin. A Mac sandbox room's own tools
//! (`mac_sandbox`) still come first. An exported SDKROOT can pin an iOS
//! build to the macOS SDK, so a folder that builds Apple apps gets the
//! rest without it.

use crate::machine_access::{Access, EnvVar, Grant, MachineAccess, Reach, Scope};
use crate::state::AppState;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// One developer folder's tools, as a room finds them.
#[derive(Debug, PartialEq, Eq)]
struct Xcode {
    developer: PathBuf,
    sdk: Option<PathBuf>,
    /// Real tools' folders, the toolchain's (cc, clang, ar, ld) first.
    bins: Vec<PathBuf>,
}

/// What `developer` holds: full Xcode, Command Line Tools alone, or nothing
/// a room could use.
fn find(developer: &Path) -> Option<Xcode> {
    let bins: Vec<PathBuf> = [
        developer.join("Toolchains/XcodeDefault.xctoolchain/usr/bin"),
        developer.join("usr/bin"),
    ]
    .into_iter()
    .filter(|dir| dir.is_dir())
    .collect();
    if bins.is_empty() {
        return None;
    }
    let sdk = [
        developer.join("Platforms/MacOSX.platform/Developer/SDKs/MacOSX.sdk"),
        developer.join("SDKs/MacOSX.sdk"),
    ]
    .into_iter()
    .find(|dir| dir.is_dir());
    Some(Xcode { developer: developer.to_path_buf(), sdk, bins })
}

/// The active developer folder's tools, looked up once per node run.
fn active() -> Option<&'static Xcode> {
    static XCODE: OnceLock<Option<Xcode>> = OnceLock::new();
    XCODE
        .get_or_init(|| crate::mac_sandbox::developer_dir().and_then(|dir| find(&dir)))
        .as_ref()
}

/// Whether `folder` builds Apple apps: an Xcode project, a Swift package, a
/// Podfile, or a Flutter app. Its top level only.
// ponytail: an app deeper in a monorepo still gets SDKROOT; look one level
// down if one turns up.
fn builds_apple_apps(folder: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        matches!(&*name, "ios" | "macos" | "Package.swift" | "Podfile" | "pubspec.yaml")
            || name.ends_with(".xcodeproj")
            || name.ends_with(".xcworkspace")
    })
}

/// Gives a room its Xcode: variables, and the real tools' folders to run
/// and to find by name. Nothing outside a room on this Mac.
pub(crate) fn extend(state: &AppState, stream_id: &str, reach: Reach, policy: &mut MachineAccess) {
    if reach != Reach::HostRoom {
        return;
    }
    if let Some(xcode) = active() {
        let folder = crate::state::context_for(state, stream_id).ok().map(|c| PathBuf::from(c.worktree_path));
        add(xcode, !folder.is_some_and(|f| builds_apple_apps(&f)), policy);
    }
}

/// After what the policy already has on PATH: the operator's mise and
/// Homebrew tools still come first, and these replace /usr/bin's shims.
fn add(xcode: &Xcode, sdkroot: bool, policy: &mut MachineAccess) {
    let var = |name: &str, value: &Path| EnvVar {
        name: name.to_string(),
        value: value.display().to_string(),
        scope: Scope::AllAgents,
        preset: Some("xcode".to_string()),
    };
    policy.env.push(var("DEVELOPER_DIR", &xcode.developer));
    policy.env.extend(xcode.sdk.iter().filter(|_| sdkroot).map(|sdk| var("SDKROOT", sdk)));
    policy.grants.extend(xcode.bins.iter().map(|bin| Grant {
        path: bin.display().to_string(),
        access: Access::Exec,
        scope: Scope::Baker,
        on_path: true,
        preset: Some("xcode".to_string()),
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dirs(root: &Path, rels: &[&str]) {
        for rel in rels {
            std::fs::create_dir_all(root.join(rel)).unwrap();
        }
    }

    /// Full Xcode: the toolchain's folder (cc, clang, ar, ld) ahead of the
    /// developer folder's own (git, make), and the macOS SDK from its
    /// platform folder.
    #[test]
    fn full_xcode_has_its_toolchain_first_and_its_sdk() {
        let dev = tempfile::tempdir().unwrap();
        dirs(dev.path(), &["usr/bin", "Toolchains/XcodeDefault.xctoolchain/usr/bin", "Platforms/MacOSX.platform/Developer/SDKs/MacOSX.sdk"]);
        let xcode = find(dev.path()).unwrap();
        assert_eq!(xcode.bins, [dev.path().join("Toolchains/XcodeDefault.xctoolchain/usr/bin"), dev.path().join("usr/bin")]);
        assert_eq!(xcode.sdk, Some(dev.path().join("Platforms/MacOSX.platform/Developer/SDKs/MacOSX.sdk")));
        assert_eq!(xcode.developer, dev.path());
    }

    /// Command Line Tools alone: one tools folder, the SDK beside it.
    #[test]
    fn command_line_tools_alone_have_one_folder_and_their_sdk() {
        let dev = tempfile::tempdir().unwrap();
        dirs(dev.path(), &["usr/bin", "SDKs/MacOSX.sdk"]);
        let xcode = find(dev.path()).unwrap();
        assert_eq!(xcode.bins, [dev.path().join("usr/bin")]);
        assert_eq!(xcode.sdk, Some(dev.path().join("SDKs/MacOSX.sdk")));
    }

    /// No tools folder, nothing to give; no SDK, no SDKROOT, but the tools
    /// still go first.
    #[test]
    fn a_missing_part_degrades_to_what_exists() {
        let dev = tempfile::tempdir().unwrap();
        assert_eq!(find(dev.path()), None);
        assert_eq!(find(&dev.path().join("absent")), None);
        dirs(dev.path(), &["usr/bin"]);
        let xcode = find(dev.path()).unwrap();
        assert_eq!(xcode.sdk, None);
        let mut policy = MachineAccess::default();
        add(&xcode, true, &mut policy);
        let names: Vec<&str> = policy.env.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(names, ["DEVELOPER_DIR"]);
        assert_eq!(policy.grants.len(), 1);
    }

    /// A room gets the variables and each tools folder to run, on its PATH
    /// in order, after what it already had.
    #[test]
    fn a_room_gets_the_variables_and_the_tools_to_run_by_name() {
        let dev = tempfile::tempdir().unwrap();
        dirs(dev.path(), &["usr/bin", "Toolchains/XcodeDefault.xctoolchain/usr/bin", "Platforms/MacOSX.platform/Developer/SDKs/MacOSX.sdk"]);
        let xcode = find(dev.path()).unwrap();
        let before = Grant { path: "/opt/homebrew/bin".to_string(), access: Access::Exec, scope: Scope::Baker, on_path: true, preset: None };
        let mut policy = MachineAccess { grants: vec![before.clone()], ..MachineAccess::default() };
        add(&xcode, true, &mut policy);
        assert_eq!(policy.grants[0], before, "what it had stays first");
        assert!(policy.grants[1..].iter().all(|g| g.access == Access::Exec && g.on_path));
        let on_path: Vec<&str> = policy.grants[1..].iter().map(|g| g.path.as_str()).collect();
        assert_eq!(on_path, [xcode.bins[0].to_str().unwrap(), xcode.bins[1].to_str().unwrap()]);
        let var = |name: &str| policy.env.iter().find(|v| v.name == name).map(|v| v.value.clone());
        assert_eq!(var("DEVELOPER_DIR"), Some(dev.path().display().to_string()));
        assert_eq!(var("SDKROOT"), xcode.sdk.map(|p| p.display().to_string()));
    }

    /// A folder that builds Apple apps gets the tools and DEVELOPER_DIR but
    /// no SDKROOT, which could pin its iOS builds to the macOS SDK.
    #[test]
    fn an_apple_app_folder_gets_no_sdkroot() {
        let dev = tempfile::tempdir().unwrap();
        dirs(dev.path(), &["usr/bin", "SDKs/MacOSX.sdk"]);
        let xcode = find(dev.path()).unwrap();
        let mut policy = MachineAccess::default();
        add(&xcode, false, &mut policy);
        let names: Vec<&str> = policy.env.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(names, ["DEVELOPER_DIR"]);
        assert_eq!(policy.grants.len(), 1, "its tools still come first");

        let app = tempfile::tempdir().unwrap();
        assert!(!builds_apple_apps(app.path()), "empty");
        std::fs::write(app.path().join("Cargo.toml"), "").unwrap();
        assert!(!builds_apple_apps(app.path()), "a Rust crate");
        for marker in ["ios", "macos", "Package.swift", "Podfile", "pubspec.yaml", "Farm.xcodeproj", "Farm.xcworkspace"] {
            let app = tempfile::tempdir().unwrap();
            std::fs::write(app.path().join(marker), "").unwrap();
            assert!(builds_apple_apps(app.path()), "{marker}");
        }
        assert!(!builds_apple_apps(&app.path().join("absent")));
    }

    /// Only a room on this Mac: Host full is the operator's own machine, and
    /// a container is Linux. A default project (a Mac sandbox one) gets it.
    #[test]
    fn only_a_room_on_this_mac_gets_it() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join(".bakery");
        std::fs::create_dir_all(&base).unwrap();
        let state = crate::testutil::state_with(&base, std::sync::Arc::new(bakery_agent::StubModel));
        let app = dir.path().join("farm");
        std::fs::create_dir_all(&app).unwrap();
        let root = crate::catalog::DirectoryRootSpec::Attach(app.to_str().unwrap());
        let (_, thread) = crate::catalog::create_project(&state, "farm", Some(root)).unwrap();
        let given = |reach: Reach| {
            let mut policy = MachineAccess::default();
            extend(&state, &thread.stream_id, reach, &mut policy);
            policy
        };
        let room = given(Reach::HostRoom);
        assert_eq!(!room.env.is_empty(), active().is_some(), "a room gets it when this Mac has Xcode");
        assert_eq!(room.env.iter().any(|v| v.name == "SDKROOT"), active().is_some_and(|x| x.sdk.is_some()));
        for reach in [Reach::HostFull, Reach::Container, Reach::Locked] {
            assert!(given(reach).env.is_empty(), "{reach:?}");
        }
    }
}
