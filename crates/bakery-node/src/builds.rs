//! Build folders for every thread's room on this Mac, kept in the Bakery's
//! cache, outside every worktree. Downloaded crates (`CARGO_HOME`) are one
//! folder per project, shared by that project's rooms. Each worktree compiles
//! into its own `target`: cargo fingerprints a path crate by its path, so one
//! thread's tests must not run another's binary out of a shared folder. A
//! room with no worktree keeps the project's `target`. The threads under the
//! Baker, its workspaces and their workers share the Bakery's own cargo home,
//! [`BAKERY`], and each of their worktrees still has its own target. A thread
//! of no project and no workspace keeps what its room gives it. Every room
//! still compiles under one machine-wide budget: a jobserver of two tokens,
//! and `CARGO_BUILD_JOBS` of 2 unless the room asked for fewer. Container seats
//! are not here:
//! their Linux output goes to their own volume, since a Linux build can't
//! use a macOS `target`. The operator's own checkouts and their `target`
//! folders are never touched.

use crate::machine_access::SpawnGrants;
use crate::state::AppState;
use std::path::{Path, PathBuf};

/// The key of the Bakery's own build folder: what the Baker's threads,
/// workspaces and workers share, since the Bakery is no catalog project.
pub(crate) const BAKERY: &str = "bakery";

/// The folder a key's builds live in: `<cache>/builds/<key>`.
pub(crate) fn folder(state: &AppState, key: &str) -> PathBuf {
    let safe: String = key
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' })
        .collect();
    state.base.join("cache").join("builds").join(safe)
}

/// A key's cargo `target` folder.
pub(crate) fn target_dir(state: &AppState, key: &str) -> PathBuf {
    folder(state, key).join("target")
}

/// A key's cargo home: the crates its builds download.
pub(crate) fn cargo_home(state: &AppState, key: &str) -> PathBuf {
    folder(state, key).join("cargo")
}

/// The build folder a thread's rooms share: its project's; else the
/// Bakery's own, for the Baker's threads and for workspaces and their
/// workers; else none. A project's own terminal (`project:<id>`) is its
/// project's.
pub(crate) fn key_of(state: &AppState, stream_id: &str) -> Option<String> {
    if let Some(project) = crate::terminals::project_of_stream(stream_id) {
        return Some(project.to_string());
    }
    let project = crate::state::socket_project(&state.heart.lock().unwrap(), stream_id).0;
    project.or_else(|| {
        let bakerys = crate::baker_ops::is_baker_scope_stream(state, stream_id)
            || crate::workspaces::workspace_of(state, stream_id).is_some();
        bakerys.then(|| BAKERY.to_string())
    })
}

/// Gives a room on this Mac its cargo home and its `target`, and the
/// machine-wide compile budget. The cargo home is the project's. The target
/// is the worktree's, or the project's when the room has no worktree. The
/// room may write both and run what it builds (test binaries, build scripts,
/// procedural macros). A thread with no folder to share keeps its own
/// `target`. Both still draw from the one jobserver, and cargo's job count is
/// 2 unless the room set it lower: an agent types `cargo test` with no
/// `--jobs`, and cargo reads `CARGO_BUILD_JOBS`.
pub(crate) fn extend(state: &AppState, stream_id: &str, grants: &mut SpawnGrants) {
    cap_compile(state, grants);
    let Some(key) = key_of(state, stream_id) else {
        return;
    };
    let cargo = cargo_home(state, &key);
    let _ = std::fs::create_dir_all(&cargo);
    let target = ensure_target(state, &key, checkout_of(state, stream_id).as_deref());
    grants.write.extend([target.clone(), cargo.clone()]);
    grants.exec.push(target.clone());
    // Over the room's own CARGO_HOME (`machine_access::working_env`).
    grants.env.retain(|(name, _)| name != "CARGO_HOME" && name != "CARGO_TARGET_DIR");
    grants.env.push(("CARGO_TARGET_DIR".to_string(), target.display().to_string()));
    grants.env.push(("CARGO_HOME".to_string(), cargo.display().to_string()));
}

/// The checkout a room compiles in, when it has one.
fn checkout_of(state: &AppState, stream_id: &str) -> Option<PathBuf> {
    let ctx = crate::state::context_for(state, stream_id).ok()?;
    if ctx.worktree_path.is_empty() {
        return None;
    }
    Some(PathBuf::from(ctx.worktree_path))
}

/// Where one worktree's `CARGO_TARGET_DIR` lives. The name is the checkout's
/// path, so removing the worktree finds the same folder.
pub(crate) fn worktree_target(state: &AppState, worktree: &Path) -> PathBuf {
    worktrees_root(state).join(worktree_key(worktree))
}

/// Removes the target a worktree compiled into. The shared cargo home is a
/// different folder and stays.
pub(crate) fn drop_worktree_target(state: &AppState, target: &Path) {
    let root = worktrees_root(state);
    if target.parent() == Some(root.as_path()) && target.is_dir() {
        let _ = std::fs::remove_dir_all(target);
    }
}

/// `<cache>/builds/.worktrees`. A project key cannot name this: [`folder`]
/// only keeps letters, digits and `-`.
fn worktrees_root(state: &AppState) -> PathBuf {
    state.base.join("cache").join("builds").join(".worktrees")
}

fn worktree_key(path: &Path) -> String {
    let canon = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    blake3::hash(canon.to_string_lossy().as_bytes()).to_hex().to_string()
}

/// Creates `target` if it is missing. A new one may start as a copy-on-write
/// clone of a recent target (`cp -c` on this Mac).
fn ensure_target(state: &AppState, key: &str, worktree: Option<&Path>) -> PathBuf {
    let target = match worktree {
        Some(path) => worktree_target(state, path),
        None => target_dir(state, key),
    };
    if target.exists() {
        return target;
    }
    if let Some(src) = recent_target(state, &target) {
        if clone_target(&src, &target) && target.is_dir() {
            // Registry crates stay fresh: their fingerprints list no source
            // paths. A path crate's fingerprint is a workspace-relative path
            // and an mtime, so a sibling target would still run the previous
            // worktree's binary. Drop those fingerprints; the crate rebuilds.
            drop_path_fingerprints(&target);
            return target;
        }
        let _ = std::fs::remove_dir_all(&target);
    }
    let _ = std::fs::create_dir_all(&target);
    target
}

/// The newest existing target, other than `dest`.
fn recent_target(state: &AppState, dest: &Path) -> Option<PathBuf> {
    let builds = state.base.join("cache").join("builds");
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    let note = |best: &mut Option<(std::time::SystemTime, PathBuf)>, path: PathBuf| {
        if path == dest || !path.is_dir() {
            return;
        }
        let Ok(mtime) = path.metadata().and_then(|meta| meta.modified()) else {
            return;
        };
        let newer = match best {
            Some((at, _)) => mtime > *at,
            None => true,
        };
        if newer {
            *best = Some((mtime, path));
        }
    };
    if let Ok(read) = std::fs::read_dir(worktrees_root(state)) {
        for entry in read.flatten() {
            note(&mut best, entry.path());
        }
    }
    if let Ok(read) = std::fs::read_dir(&builds) {
        for entry in read.flatten() {
            if entry.file_name() == ".worktrees" {
                continue;
            }
            note(&mut best, entry.path().join("target"));
        }
    }
    best.map(|(_, path)| path)
}

/// `cp -c` clones the directory on APFS. Anywhere else, or if it fails, the
/// caller starts an empty target.
fn clone_target(src: &Path, dst: &Path) -> bool {
    if let Some(parent) = dst.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return false;
        }
    }
    std::process::Command::new("cp")
        .arg("-cR")
        .arg("--")
        .arg(src)
        .arg(dst)
        .status()
        .is_ok_and(|status| status.success())
}

/// Removes fingerprints that name a Rust source. Cargo leaves registry
/// fingerprints without those paths, and deletes a path crate's when the
/// `dep-` file mentions one (`src/lib.rs`). Missing fingerprint means dirty.
fn drop_path_fingerprints(target: &Path) {
    let Ok(top) = std::fs::read_dir(target) else {
        return;
    };
    for entry in top.flatten() {
        let Ok(units) = std::fs::read_dir(entry.path().join(".fingerprint")) else {
            continue;
        };
        for unit in units.flatten() {
            if fingerprint_names_rust_source(&unit.path()) {
                let _ = std::fs::remove_dir_all(unit.path());
            }
        }
    }
}

fn fingerprint_names_rust_source(dir: &Path) -> bool {
    let Ok(read) = std::fs::read_dir(dir) else {
        return false;
    };
    for entry in read.flatten() {
        if !entry.file_name().to_string_lossy().starts_with("dep-") {
            continue;
        }
        let Ok(bytes) = std::fs::read(entry.path()) else {
            continue;
        };
        if bytes.windows(3).any(|window| window == b".rs") {
            return true;
        }
    }
    false
}

/// The one compile jobserver for this process: two tokens, created the
/// first time the node (or a test) builds an [`AppState`].
pub(crate) fn compile_jobserver() -> jobserver::Client {
    static CLIENT: std::sync::OnceLock<jobserver::Client> = std::sync::OnceLock::new();
    CLIENT
        .get_or_init(|| {
            let client = jobserver::Client::new(2).expect("compile jobserver");
            // The crate's pipes are cloexec. A room only keeps them across
            // exec when they stay open, and the MAKEFLAGS line names them.
            keep_jobserver_open(&makeflags(&client));
            client
        })
        .clone()
}

/// `MAKEFLAGS` as the jobserver crate would hand it to a child.
fn makeflags(client: &jobserver::Client) -> String {
    let mut cmd = std::process::Command::new("true");
    client.configure_make(&mut cmd);
    cmd.get_envs()
        .find(|(key, _)| *key == "MAKEFLAGS")
        .and_then(|(_, value)| value)
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Leaves the jobserver's pipe ends open across exec. A `fifo:` auth needs
/// no descriptor: the child opens the path itself.
fn keep_jobserver_open(line: &str) {
    let Some(auth) = line.rsplit("--jobserver-auth=").next() else {
        return;
    };
    let auth = auth.split_whitespace().next().unwrap_or("");
    if auth.starts_with("fifo:") {
        return;
    }
    let mut parts = auth.split(',');
    let (Some(read), Some(write)) = (parts.next(), parts.next()) else {
        return;
    };
    for fd in [read, write].into_iter().filter_map(|fd| fd.parse::<libc::c_int>().ok()) {
        // Safety: these are the pipe ends the jobserver crate just created
        // and named in its own MAKEFLAGS line. Clearing cloexec lets a room
        // inherit them. A bad number makes fcntl fail and we leave it.
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFD);
            if flags >= 0 {
                libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
            }
        }
    }
}

/// Puts the process jobserver's `MAKEFLAGS` line into the room, in place of
/// any jobserver the room supplied, and holds `CARGO_BUILD_JOBS` at 2.
/// Cargo reads `CARGO_MAKEFLAGS` before `MAKEFLAGS`, so the same line is
/// written to each name the jobserver crate sets. A room that set 1 keeps 1.
fn cap_compile(state: &AppState, grants: &mut SpawnGrants) {
    let line = makeflags(&state.compile_jobs);
    grants.env.retain(|(name, _)| name != "MAKEFLAGS" && name != "MFLAGS" && name != "CARGO_MAKEFLAGS");
    grants.env.push(("MAKEFLAGS".to_string(), line.clone()));
    grants.env.push(("MFLAGS".to_string(), line.clone()));
    grants.env.push(("CARGO_MAKEFLAGS".to_string(), line));
    let hold = match grants.env.iter().rev().find(|(name, _)| name == "CARGO_BUILD_JOBS") {
        None => true,
        Some((_, value)) => match value.trim().parse::<u32>() {
            Ok(n) if n > 2 => true,
            Ok(_) => false,
            Err(_) => true,
        },
    };
    if hold {
        grants.env.retain(|(name, _)| name != "CARGO_BUILD_JOBS");
        grants.env.push(("CARGO_BUILD_JOBS".to_string(), "2".to_string()));
    }
}

/// A project of one repository (`base/name`), and a second thread in it on
/// a worktree of its own: the project's id and its two threads.
#[cfg(test)]
pub(crate) fn project_with_two_threads(state: &AppState, base: &std::path::Path, name: &str) -> (String, String, String) {
    use crate::testutil::{make_repo, open_req};
    let repo = make_repo(&base.join(name));
    let root = crate::catalog::DirectoryRootSpec::Attach(repo.to_str().unwrap());
    let (project, first) = crate::catalog::create_project(state, name, Some(root)).unwrap();
    let second = crate::routes::project::open_stream(state, &open_req(&repo, &format!("{name}-side"))).unwrap().context;
    crate::catalog::link_socket_to_project(state, &second.stream_id, &project.project_id, &second.worktree_path, true).unwrap();
    (project.project_id, first.stream_id, second.stream_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine_access::Reach;
    use crate::testutil::*;
    use std::sync::Arc;

    fn var(grants: &SpawnGrants, name: &str) -> Option<String> {
        grants.env.iter().rev().find(|(k, _)| k == name).map(|(_, v)| v.clone())
    }

    /// A folder in the Bakery's cache, outside every worktree, named for its
    /// key and for nothing else.
    #[test]
    fn a_projects_build_folder_is_in_the_bakerys_cache() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(bakery_agent::StubModel));
        let cache = state.base.join("cache/builds");
        assert_eq!(folder(&state, "project_ab12"), cache.join("project_ab12"));
        assert_eq!(target_dir(&state, "project_ab12"), cache.join("project_ab12/target"));
        assert_eq!(cargo_home(&state, "project_ab12"), cache.join("project_ab12/cargo"));
        assert_eq!(folder(&state, "../../etc"), cache.join("______etc"), "a key never leaves the folder");
        assert!(!folder(&state, BAKERY).starts_with(&*state.worktree_base));
    }

    /// Threads of one project share a key, two projects don't, the Baker's
    /// threads, workspaces and workers share the Bakery's own, and a plain
    /// chat shares nothing.
    #[test]
    fn a_threads_key_is_its_projects_else_the_bakerys_own() {
        let dir = tempfile::tempdir().unwrap();
        let repo = bakery_repo_for_test(dir.path());
        run_git(&repo, &["add", "."]);
        run_git(&repo, &["commit", "-qm", "the bakery"]);
        let state = state_with_bakery_root(dir.path(), &repo);
        let home = crate::routes::baker::ensure_permanent_baker(&state, None, None).unwrap().stream_id;
        let (farm, first, second) = project_with_two_threads(&state, dir.path(), "farm");
        let (shop, other, _) = project_with_two_threads(&state, dir.path(), "shop");

        let key = |s: &str| key_of(&state, s);
        assert_eq!(key(&first), Some(farm.clone()));
        assert_eq!(key(&second), key(&first), "two threads of one project");
        assert_eq!(key(&other), Some(shop));
        assert_ne!(key(&other), key(&first), "two projects");
        assert_eq!(key(&format!("project:{farm}")), key(&first), "its terminal");

        assert_eq!(key(&home).as_deref(), Some(BAKERY));
        let req = crate::baker_threads::CreateThreadReq { title: Some("a question".into()), ..Default::default() };
        let thread = crate::baker_threads::create_thread(&state, &req, "human", None, None).unwrap().thread.stream_id;
        assert_eq!(key(&thread).as_deref(), Some(BAKERY), "a Baker thread");
        crate::routes::seats::seat_assign_checked(&state, &home, crate::node::Seat::Baker, "stub").unwrap();
        let orc = crate::workspaces::create_workspace(&state, "Bakery", None, "human").unwrap().stream_id;
        assert_eq!(key(&orc).as_deref(), Some(BAKERY), "a workspace");
        let req = crate::baker_threads::CreateThreadReq { title: Some("worker".into()), ..Default::default() };
        let worker = crate::baker_threads::create_peer(&state, &req, &orc, "human").unwrap().thread.stream_id;
        assert_eq!(key(&worker).as_deref(), Some(BAKERY), "a workspace's worker");

        let chat = crate::catalog::create_chat(&state, "elsewhere").unwrap();
        assert_eq!(key(&chat.stream_id), None, "a plain chat keeps its own room's");
    }

    /// Two worktrees of one project share its cargo home and not its target.
    /// Another project's room gets neither. A container seat gets none, a
    /// chat of no project none.
    #[test]
    fn rooms_of_one_project_share_a_cargo_home() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(bakery_agent::StubModel));
        let (farm, first, second) = project_with_two_threads(&state, dir.path(), "farm");
        let (_shop, other, _) = project_with_two_threads(&state, dir.path(), "shop");
        let cargo = cargo_home(&state, &farm);
        let cargo_s = cargo.display().to_string();
        let mut targets = Vec::new();

        for stream in [&first, &second] {
            for starting in [true, false] {
                let granted = if starting {
                    crate::machine_access::for_stream(&state, stream)
                } else {
                    crate::machine_access::shown_for(&state, stream)
                };
                let target = PathBuf::from(var(&granted, "CARGO_TARGET_DIR").expect("a target"));
                assert_eq!(var(&granted, "CARGO_HOME").as_deref(), Some(cargo_s.as_str()));
                let count = |name: &str| granted.env.iter().filter(|(k, _)| k == name).count();
                assert_eq!((count("CARGO_HOME"), count("CARGO_TARGET_DIR")), (1, 1), "{:?}", granted.env);
                assert!(granted.write.contains(&target) && granted.write.contains(&cargo), "{:?}", granted.write);
                assert!(granted.exec.contains(&target), "what it builds runs");
                assert!(target.is_dir() && cargo.is_dir());
                if starting {
                    targets.push(target);
                }
            }
        }
        assert_ne!(targets[0], targets[1], "two worktrees");
        let elsewhere = crate::machine_access::for_stream(&state, &other);
        let theirs = PathBuf::from(var(&elsewhere, "CARGO_TARGET_DIR").expect("a target"));
        assert_ne!(theirs, targets[0]);
        assert!(!elsewhere.write.contains(&targets[0]), "another project's folder is not its to write");

        state.node.lock().unwrap().machine_access.reach.insert(first.clone(), Reach::Container);
        let seat = crate::machine_access::for_stream(&state, &first);
        assert_eq!(var(&seat, "CARGO_TARGET_DIR"), None);
        assert!(!seat.write.contains(&targets[0]), "a container seat keeps its own volume");
        let chat = crate::catalog::create_chat(&state, "elsewhere").unwrap();
        let alone = crate::machine_access::for_stream(&state, &chat.stream_id);
        assert_eq!(var(&alone, "CARGO_TARGET_DIR"), None);
    }

    /// Two rooms draw from the one jobserver created for the process, and
    /// cargo is held at two jobs. A room that set 1 keeps 1. A room with no
    /// shared folder still gets the budget, and not a build folder.
    #[test]
    fn two_rooms_share_one_jobserver_and_two_build_jobs() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(bakery_agent::StubModel));
        let (_farm, first, second) = project_with_two_threads(&state, dir.path(), "farm");
        let left = crate::machine_access::for_stream(&state, &first);
        let right = crate::machine_access::for_stream(&state, &second);
        let flags = var(&left, "MAKEFLAGS").expect("a jobserver line");
        assert!(flags.contains("--jobserver-auth="), "{flags}");
        assert_eq!(var(&right, "MAKEFLAGS").as_deref(), Some(flags.as_str()));
        assert_eq!(var(&left, "CARGO_MAKEFLAGS").as_deref(), Some(flags.as_str()));
        assert_eq!(var(&left, "CARGO_BUILD_JOBS").as_deref(), Some("2"));
        assert_eq!(var(&right, "CARGO_BUILD_JOBS").as_deref(), Some("2"));

        let mut one = SpawnGrants::default();
        one.env.push(("CARGO_BUILD_JOBS".to_string(), "1".to_string()));
        one.env.push(("MAKEFLAGS".to_string(), "--jobserver-auth=9,10".to_string()));
        extend(&state, &first, &mut one);
        assert_eq!(var(&one, "CARGO_BUILD_JOBS").as_deref(), Some("1"));
        assert_eq!(var(&one, "MAKEFLAGS").as_deref(), Some(flags.as_str()), "the room's jobserver is replaced");

        let mut eight = SpawnGrants::default();
        eight.env.push(("CARGO_BUILD_JOBS".to_string(), "8".to_string()));
        extend(&state, &first, &mut eight);
        assert_eq!(var(&eight, "CARGO_BUILD_JOBS").as_deref(), Some("2"));

        let chat = crate::catalog::create_chat(&state, "elsewhere").unwrap();
        let mut alone = SpawnGrants::default();
        extend(&state, &chat.stream_id, &mut alone);
        assert_eq!(var(&alone, "CARGO_TARGET_DIR"), None, "no shared folder");
        assert_eq!(var(&alone, "CARGO_BUILD_JOBS").as_deref(), Some("2"));
        assert_eq!(var(&alone, "MAKEFLAGS").as_deref(), Some(flags.as_str()));

        let dir2 = tempfile::tempdir().unwrap();
        let other = state_with(dir2.path(), Arc::new(bakery_agent::StubModel));
        let mut again = SpawnGrants::default();
        extend(&other, &chat.stream_id, &mut again);
        assert_eq!(var(&again, "MAKEFLAGS").as_deref(), Some(flags.as_str()), "one client for the process");
    }

    /// Two worktrees of one repo compile into different targets and download
    /// into one cargo home. Removing one worktree removes only its target. A
    /// room with no worktree keeps the project's target.
    #[test]
    fn two_worktrees_get_different_targets_and_removing_one_removes_only_its() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(bakery_agent::StubModel));
        let (farm, first, second) = project_with_two_threads(&state, dir.path(), "farm");

        let mut left = SpawnGrants::default();
        extend(&state, &first, &mut left);
        let left_target = PathBuf::from(var(&left, "CARGO_TARGET_DIR").expect("a target"));
        std::fs::write(left_target.join("warm"), b"registry").unwrap();
        let path_crate = left_target.join("debug/.fingerprint/sample-abc");
        std::fs::create_dir_all(&path_crate).unwrap();
        std::fs::write(path_crate.join("dep-lib-sample"), b"src/lib.rs").unwrap();
        let registry_crate = left_target.join("debug/.fingerprint/cfg-if-abc");
        std::fs::create_dir_all(&registry_crate).unwrap();
        std::fs::write(registry_crate.join("dep-lib-cfg_if"), b"registry").unwrap();

        let mut right = SpawnGrants::default();
        extend(&state, &second, &mut right);
        let right_target = PathBuf::from(var(&right, "CARGO_TARGET_DIR").expect("a target"));
        let cargo = PathBuf::from(var(&left, "CARGO_HOME").expect("a cargo home"));
        assert_ne!(left_target, right_target);
        assert_eq!(var(&right, "CARGO_HOME").as_deref(), Some(cargo.display().to_string().as_str()));
        assert_eq!(std::fs::read(right_target.join("warm")).unwrap(), b"registry", "a new target is a clone of a recent one");
        assert!(!right_target.join("debug/.fingerprint/sample-abc").exists(), "the worktree's own crate rebuilds");
        assert!(right_target.join("debug/.fingerprint/cfg-if-abc").is_dir(), "a registry crate stays");
        assert_ne!(left_target, target_dir(&state, &farm));
        assert_eq!(cargo, cargo_home(&state, &farm));

        let mut plain = SpawnGrants::default();
        extend(&state, &format!("project:{farm}"), &mut plain);
        let shared = PathBuf::from(var(&plain, "CARGO_TARGET_DIR").expect("the project's target"));
        assert_eq!(shared, target_dir(&state, &farm), "no worktree keeps today's target");
        assert_eq!(var(&plain, "CARGO_HOME").as_deref(), Some(cargo.display().to_string().as_str()));

        std::fs::write(cargo.join("keep"), b"crates").unwrap();
        std::fs::write(left_target.join("keep"), b"left").unwrap();
        std::fs::write(shared.join("keep"), b"shared").unwrap();

        let ctx = crate::state::context_for(&state, &second).unwrap();
        crate::routes::project::remove_worktree(&state, &ctx.repo_root, std::path::Path::new(&ctx.worktree_path)).unwrap();

        assert!(!right_target.exists(), "the removed worktree's target goes");
        assert!(!std::path::Path::new(&ctx.worktree_path).exists());
        assert_eq!(std::fs::read(left_target.join("keep")).unwrap(), b"left");
        assert_eq!(std::fs::read(shared.join("keep")).unwrap(), b"shared");
        assert_eq!(std::fs::read(cargo.join("keep")).unwrap(), b"crates");
    }
}
