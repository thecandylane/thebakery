use crate::spec::Spec;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

/// A room: one stream's long-lived attach point. Two parts, deliberately not
/// one directory:
///
/// - `toolchain_root` — `<cache_root>/envs/<digest>`, published once per spec
///   digest and shared by every stream that pins it. Read-only at attach time
///   (see `attach`): a command must never be able to rewrite the compiler a
///   later socket with the same digest will run.
/// - `runtime_root` — `<cache_root>/sockets/<stream_id>`, this stream's own
///   writable HOME/tmp. Two streams that happen to pin the same toolchain
///   never share a HOME.
#[derive(Debug, Clone)]
pub struct SessionEnvironment {
    pub spec: Spec,
    pub digest: String,
    pub toolchain_root: PathBuf,
    pub runtime_root: PathBuf,
    pub cache_hit: bool,
}

impl SessionEnvironment {
    pub fn toolchain_rust_bin(&self) -> PathBuf {
        self.toolchain_root.join("toolchain/rust/bin")
    }

    pub fn home(&self) -> PathBuf {
        self.runtime_root.join("home")
    }

    pub fn tmp(&self) -> PathBuf {
        self.runtime_root.join("tmp")
    }

    /// This backend is a Seatbelt prefix, not a VM or container: file access
    /// and outbound network are enforced; Keychain/XPC service isolation is
    /// not. Honest about the tier so a caller can fail closed if it needs more.
    pub const ENFORCEMENT_TIER: &'static str = "seatbelt-prefix";
}

/// Whether `<cache_root>/envs/<digest>` is already provisioned, without side effects.
pub struct Status {
    pub digest: String,
    pub root: PathBuf,
    pub ready: bool,
}

fn ready_marker(root: &Path) -> PathBuf {
    root.join("ready")
}

fn envs_dir(cache_root: &Path) -> PathBuf {
    cache_root.join("envs")
}

/// A stream id like `wt:name` becomes a plain directory name — the exact
/// character set doesn't matter, only that it is deterministic and never
/// contains a path separator.
fn socket_dir_name(stream_id: &str) -> String {
    stream_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// `<cache_root>/sockets/<sanitized stream_id>` — one stream's own writable
/// runtime dir. Public so a caller that needs to *destroy* a stream's room
/// (lifecycle discard) can find the same path `provision` writes into
/// without duplicating — or drifting from — the sanitization above.
pub fn socket_runtime_dir(cache_root: &Path, stream_id: &str) -> PathBuf {
    cache_root.join("sockets").join(socket_dir_name(stream_id))
}

pub fn status(workspace: &Path, cache_root: &Path) -> Result<Status, String> {
    let spec = Spec::load(workspace)?;
    let digest = spec.digest();
    let root = envs_dir(cache_root).join(&digest);
    let ready = ready_marker(&root).is_file();
    Ok(Status {
        digest,
        root,
        ready,
    })
}

/// Provision the toolchain cache for `workspace`'s pinned spec (cache hit when
/// `<cache_root>/envs/<digest>/ready` already exists — no toolchain copy or
/// install runs again), then ensure `stream_id`'s own writable runtime dir
/// exists on top of it. Fails closed if the host's toolchain does not match
/// the pinned spec version, and never leaves a `ready` marker on a half-built
/// cache: the clone lands in a temp dir next to the digest and is renamed
/// into place only once it fully succeeds.
pub fn provision(
    workspace: &Path,
    cache_root: &Path,
    stream_id: &str,
) -> Result<SessionEnvironment, String> {
    let spec = Spec::load(workspace)?;
    provision_spec(spec, cache_root, stream_id)
}

/// Attach a `run` to the room this socket already has.
///
/// `mise.toml` is one optional overlay when the project's toolchain adapter
/// is `mise` **and** a pin file is already there. Never required, never
/// written. `proto` / `moon` / anything else boots the thin room — the
/// project stack is configuration, not admission.
pub fn room_for_run(
    workspace: &Path,
    cache_root: &Path,
    stream_id: &str,
    toolchain: &str,
) -> Result<SessionEnvironment, String> {
    if toolchain == "mise" && workspace.join("mise.toml").is_file() {
        match provision_project_tools(workspace, cache_root, stream_id) {
            ProjectToolsStatus::Ready(env) => return Ok(env),
            ProjectToolsStatus::NotConfigured => {}
            ProjectToolsStatus::Unsupported(e) => return Err(e),
        }
    }
    ensure_thin_room(workspace, cache_root, stream_id)
}

/// The thin room: always available, regardless of `mise.toml` — no toolchain
/// pin, no host sysroot clone, no gate on the workspace's contents at all.
/// `workspace` is accepted (not read) only so callers have one uniform
/// `(workspace, cache_root, stream_id)` shape to swap between this and
/// [`provision`]/[`provision_project_tools`]. The empty-tools [`Spec`]'s own
/// digest IS the thin id `07b-THIN-ROOM.md` asks for — distinct from any
/// real toolchain digest, with no extra plumbing needed to keep it that way.
pub fn ensure_thin_room(
    _workspace: &Path,
    cache_root: &Path,
    stream_id: &str,
) -> Result<SessionEnvironment, String> {
    provision_spec(
        Spec {
            tools: std::collections::BTreeMap::new(),
        },
        cache_root,
        stream_id,
    )
}

/// Project toolchain pins (`mise.toml`) as one *optional* capability, never
/// admission to the room: no `mise.toml` is "not configured", not an error —
/// only an actual unsupported/mismatched pin is `Unsupported`. A caller that
/// wants pinned `rustc` checks this; chat/ACP/terminal never do.
pub enum ProjectToolsStatus {
    NotConfigured,
    Ready(SessionEnvironment),
    Unsupported(String),
}

pub fn provision_project_tools(
    workspace: &Path,
    cache_root: &Path,
    stream_id: &str,
) -> ProjectToolsStatus {
    if !workspace.join("mise.toml").is_file() {
        return ProjectToolsStatus::NotConfigured;
    }
    match provision(workspace, cache_root, stream_id) {
        Ok(env) => ProjectToolsStatus::Ready(env),
        Err(e) => ProjectToolsStatus::Unsupported(e),
    }
}

/// Attach to an already-materialized project toolchain, structurally
/// incapable of provisioning one: the spec is loaded exactly once and its
/// digest's `ready` marker is checked against that same load — no separate
/// `status()`-then-`provision()` pair whose two manifest reads (and two
/// digest computations) could observe a `mise.toml` edit or a cache
/// eviction in between and quietly provision a *different* (or newly cold)
/// digest than the one just checked. This function calls neither
/// [`provision`] nor `publish_toolchain`; there is no path in it that can
/// ever start a toolchain clone. Only the socket's own writable runtime dir
/// is created (or reused) here — never the toolchain cache itself.
pub fn open_cached_project_tools(
    workspace: &Path,
    cache_root: &Path,
    stream_id: &str,
) -> Option<SessionEnvironment> {
    let spec = Spec::load(workspace).ok()?;
    if !spec.unsupported_tools().is_empty() {
        return None;
    }
    let digest = spec.digest();
    let toolchain_root = envs_dir(cache_root).join(&digest);
    if !ready_marker(&toolchain_root).is_file() {
        return None;
    }
    let runtime_root = socket_runtime_dir(cache_root, stream_id);
    std::fs::create_dir_all(runtime_root.join("home")).ok()?;
    std::fs::create_dir_all(runtime_root.join("tmp")).ok()?;
    Some(SessionEnvironment {
        spec,
        digest,
        toolchain_root,
        runtime_root,
        cache_hit: true,
    })
}

fn provision_spec(
    spec: Spec,
    cache_root: &Path,
    stream_id: &str,
) -> Result<SessionEnvironment, String> {
    let digest = spec.digest();
    let envs = envs_dir(cache_root);
    std::fs::create_dir_all(&envs).map_err(|e| e.to_string())?;
    let toolchain_root = envs.join(&digest);
    let ready = ready_marker(&toolchain_root);
    // Reassigned below, under the lock, right before the build/no-build
    // decision — a stale value sampled before waiting on the lock would
    // report "provisioned" for a call that actually just inherited another
    // racer's finished build.
    let mut cache_hit = ready.is_file();

    if !cache_hit {
        // Two streams provisioning the same digest at once must not race each
        // other into the same scratch dir or duplicate the clone — serialize
        // per digest. A different digest gets its own lock and proceeds in
        // parallel.
        let lock = digest_lock(&digest);
        let _guard = lock.lock().unwrap();
        // The lock's previous holder may have just finished this exact
        // digest while we were waiting for it — that makes this call a
        // genuine cache hit, even though the outer check above missed it.
        cache_hit = ready.is_file();
        if !cache_hit {
            publish_toolchain(&spec, &envs, &digest, &toolchain_root, &ready)?;
        }
    }

    let runtime_root = socket_runtime_dir(cache_root, stream_id);
    std::fs::create_dir_all(runtime_root.join("home")).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(runtime_root.join("tmp")).map_err(|e| e.to_string())?;

    Ok(SessionEnvironment {
        spec,
        digest,
        toolchain_root,
        runtime_root,
        cache_hit,
    })
}

/// Cheap room fixture for sandbox/process tests. Tests exercising real
/// provisioning keep calling [`provision`]; tests that only need the room
/// directory shape and a command on its PATH should not clone the host's
/// entire Rust sysroot for every case.
#[cfg(test)]
pub(crate) fn synthetic_test_environment(
    workspace: &Path,
    cache_root: &Path,
    stream_id: &str,
) -> SessionEnvironment {
    let spec = if workspace.join("mise.toml").is_file() {
        Spec::load(workspace).unwrap()
    } else {
        Spec::from_mise_toml("[tools]\nrust = \"1.98.0\"\n").unwrap()
    };
    let digest = spec.digest();
    let toolchain_root = envs_dir(cache_root).join(&digest);
    let rust_bin = toolchain_root.join("toolchain/rust/bin");
    std::fs::create_dir_all(&rust_bin).unwrap();

    let rustc = rust_bin.join("rustc");
    let rust_version = spec.rust_version().expect("test room spec must pin Rust");
    std::fs::write(
        &rustc,
        format!("#!/bin/sh\necho 'rustc {rust_version} (test fixture)'\n"),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(&rustc).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&rustc, permissions).unwrap();
    }
    std::fs::write(ready_marker(&toolchain_root), b"").unwrap();

    let runtime_root = socket_runtime_dir(cache_root, stream_id);
    std::fs::create_dir_all(runtime_root.join("home")).unwrap();
    std::fs::create_dir_all(runtime_root.join("tmp")).unwrap();

    SessionEnvironment {
        spec,
        digest,
        toolchain_root,
        runtime_root,
        cache_hit: true,
    }
}

/// One lock object per digest, created on first use. Different digests never
/// block each other; the same digest from two streams (or, in a test, two
/// threads) serializes here instead of racing into the same scratch dir.
static DIGEST_LOCKS: LazyLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn digest_lock(digest: &str) -> Arc<Mutex<()>> {
    DIGEST_LOCKS
        .lock()
        .unwrap()
        .entry(digest.to_string())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

/// Process-wide, so two scratch dirs for the *same* digest never collide even
/// though the digest lock above already serializes them — belt-and-suspenders
/// against a future caller that provisions without going through `provision`.
static SCRATCH_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Build the toolchain in a scratch dir, then atomically rename it onto
/// `toolchain_root`. If a concurrent provision already won by the time this
/// one finishes, that is a cache hit for us too — not an error — so the
/// loser's scratch dir is just discarded. Never writes `ready` for a spec
/// this backend cannot actually provision.
fn publish_toolchain(
    spec: &Spec,
    envs: &Path,
    digest: &str,
    toolchain_root: &Path,
    ready: &Path,
) -> Result<(), String> {
    let unique = SCRATCH_COUNTER.fetch_add(1, Ordering::Relaxed);
    let scratch = envs.join(format!(".tmp-{digest}-{}-{unique}", std::process::id()));
    if scratch.exists() {
        std::fs::remove_dir_all(&scratch).map_err(|e| e.to_string())?;
    }
    std::fs::create_dir_all(&scratch).map_err(|e| e.to_string())?;

    let build = || -> Result<(), String> {
        let unsupported = spec.unsupported_tools();
        if !unsupported.is_empty() {
            return Err(format!(
                "mise.toml pins {} — only \"rust\" is provisioned; refusing to write a ready marker for an incomplete room",
                unsupported.join(", ")
            ));
        }
        if let Some(want) = spec.rust_version() {
            crate::toolchain::provision_rust(want, &scratch)?;
        }
        std::fs::write(ready_marker(&scratch), b"").map_err(|e| e.to_string())
    };

    if let Err(e) = build() {
        let _ = std::fs::remove_dir_all(&scratch);
        return Err(e);
    }

    if !ready.is_file() {
        if std::fs::rename(&scratch, toolchain_root).is_err() && !ready.is_file() {
            let _ = std::fs::remove_dir_all(&scratch);
            return Err(format!(
                "publishing toolchain cache for digest {digest} failed and no concurrent provision won either"
            ));
        }
    }
    let _ = std::fs::remove_dir_all(&scratch); // no-op if the rename already moved it
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn room_for_run_does_not_require_mise_toml() {
        let workspace = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let env = room_for_run(workspace.path(), cache.path(), "wt:run-thin", "none").unwrap();
        assert!(env.runtime_root.exists());
        let env_mise_none =
            room_for_run(workspace.path(), cache.path(), "wt:run-mise-empty", "mise").unwrap();
        assert!(env_mise_none.runtime_root.exists());
        let env_proto =
            room_for_run(workspace.path(), cache.path(), "wt:run-proto", "proto").unwrap();
        assert!(env_proto.runtime_root.exists());
    }

    #[test]
    fn ensure_thin_room_works_without_a_mise_toml() {
        let workspace = tempfile::tempdir().unwrap(); // no mise.toml
        let cache = tempfile::tempdir().unwrap();

        let env = ensure_thin_room(workspace.path(), cache.path(), "wt:thin").unwrap();
        assert!(env.home().is_dir());
        assert!(env.tmp().is_dir());
        // No toolchain clone: the thin room's spec pins nothing, so
        // `toolchain/rust/bin` under its toolchain_root was never created.
        assert!(
            !env.toolchain_rust_bin().is_dir(),
            "thin room must not clone a rust toolchain"
        );
    }

    #[test]
    fn ensure_thin_room_ignores_an_unsupported_pin_in_mise_toml() {
        let workspace = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        std::fs::write(
            workspace.path().join("mise.toml"),
            "[tools]\nnode = \"22\"\n",
        )
        .unwrap();

        // Thin boot must succeed even though this mise.toml pins a tool this
        // backend cannot provision — mise.toml is an adapter, never a gate.
        let env = ensure_thin_room(workspace.path(), cache.path(), "wt:thin2").unwrap();
        assert!(env.home().is_dir());

        // The optional, independent route may still fail on its own.
        match provision_project_tools(workspace.path(), cache.path(), "wt:thin2") {
            ProjectToolsStatus::Unsupported(e) => assert!(e.contains("node")),
            _ => panic!("an unsupported pin must fail provision_project_tools, not thin boot"),
        }
    }

    #[test]
    fn provision_project_tools_reports_not_configured_without_a_mise_toml() {
        let workspace = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();

        match provision_project_tools(workspace.path(), cache.path(), "wt:noconf") {
            ProjectToolsStatus::NotConfigured => {}
            _ => panic!("a workspace with no mise.toml must report NotConfigured, not an error"),
        }
    }

    #[test]
    fn provision_project_tools_is_ready_when_mise_toml_pins_a_matching_rust() {
        let workspace = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let out = std::process::Command::new("rustc")
            .arg("--version")
            .output()
            .unwrap();
        let version = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let version = version.split_whitespace().nth(1).unwrap();
        std::fs::write(
            workspace.path().join("mise.toml"),
            format!("[tools]\nrust = \"{version}\"\n"),
        )
        .unwrap();

        match provision_project_tools(workspace.path(), cache.path(), "wt:ready") {
            ProjectToolsStatus::Ready(env) => {
                assert!(env.toolchain_rust_bin().join("rustc").is_file())
            }
            _ => panic!("a matching rust pin must be Ready"),
        }
    }

    #[test]
    fn open_cached_project_tools_returns_none_when_nothing_is_cached_yet() {
        let workspace = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let out = std::process::Command::new("rustc")
            .arg("--version")
            .output()
            .unwrap();
        let version = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let version = version.split_whitespace().nth(1).unwrap();
        std::fs::write(
            workspace.path().join("mise.toml"),
            format!("[tools]\nrust = \"{version}\"\n"),
        )
        .unwrap();

        assert!(
            open_cached_project_tools(workspace.path(), cache.path(), "wt:cold").is_none(),
            "a cold (never-provisioned) pin must not be opened as cached"
        );
    }

    #[test]
    fn open_cached_project_tools_returns_none_without_a_mise_toml() {
        let workspace = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        assert!(open_cached_project_tools(workspace.path(), cache.path(), "wt:none").is_none());
    }

    #[test]
    fn open_cached_project_tools_returns_none_for_an_unsupported_pin_even_if_the_digest_happens_to_be_ready(
    ) {
        let workspace = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        std::fs::write(
            workspace.path().join("mise.toml"),
            "[tools]\nnode = \"22\"\n",
        )
        .unwrap();
        assert!(
            open_cached_project_tools(workspace.path(), cache.path(), "wt:unsupported").is_none()
        );
    }

    #[test]
    fn open_cached_project_tools_opens_a_genuinely_warm_toolchain_without_provisioning_it() {
        let workspace = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let out = std::process::Command::new("rustc")
            .arg("--version")
            .output()
            .unwrap();
        let version = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let version = version.split_whitespace().nth(1).unwrap();
        std::fs::write(
            workspace.path().join("mise.toml"),
            format!("[tools]\nrust = \"{version}\"\n"),
        )
        .unwrap();

        // Warm the cache the normal way first.
        let provisioned = provision(workspace.path(), cache.path(), "wt:warm").unwrap();
        assert!(provisioned.toolchain_rust_bin().join("rustc").is_file());

        let opened = open_cached_project_tools(workspace.path(), cache.path(), "wt:warm2")
            .expect("a genuinely warm digest must open");
        assert_eq!(opened.digest, provisioned.digest);
        assert!(opened.toolchain_rust_bin().join("rustc").is_file());
        assert_ne!(
            opened.runtime_root, provisioned.runtime_root,
            "each stream still gets its own runtime dir"
        );
    }

    fn host_rustc_short_version() -> String {
        let out = std::process::Command::new("rustc")
            .arg("--version")
            .output()
            .unwrap();
        let line = String::from_utf8_lossy(&out.stdout).trim().to_string();
        line.split_whitespace().nth(1).unwrap().to_string()
    }

    #[test]
    fn second_provision_is_a_cache_hit() {
        let workspace = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        std::fs::write(
            workspace.path().join("mise.toml"),
            format!("[tools]\nrust = \"{}\"\n", host_rustc_short_version()),
        )
        .unwrap();

        let first = provision(workspace.path(), cache.path(), "wt:a").unwrap();
        assert!(!first.cache_hit, "first provision must actually provision");
        assert!(first.toolchain_rust_bin().join("rustc").is_file());

        let second = provision(workspace.path(), cache.path(), "wt:a").unwrap();
        assert!(
            second.cache_hit,
            "second provision with the same digest must hit cache"
        );
        assert_eq!(second.digest, first.digest);
        assert_eq!(second.toolchain_root, first.toolchain_root);
    }

    #[test]
    fn two_streams_with_the_same_spec_get_different_runtime_dirs() {
        let workspace = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        std::fs::write(
            workspace.path().join("mise.toml"),
            format!("[tools]\nrust = \"{}\"\n", host_rustc_short_version()),
        )
        .unwrap();

        let a = synthetic_test_environment(workspace.path(), cache.path(), "wt:alpha");
        let b = synthetic_test_environment(workspace.path(), cache.path(), "wt:beta");

        assert_eq!(
            a.toolchain_root, b.toolchain_root,
            "same digest — same toolchain cache"
        );
        assert_ne!(
            a.runtime_root, b.runtime_root,
            "different streams must not share a HOME"
        );
        assert!(
            b.cache_hit,
            "the toolchain itself was already cached by stream a"
        );
    }

    #[test]
    fn a_mismatched_pinned_version_fails_closed() {
        let workspace = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        std::fs::write(
            workspace.path().join("mise.toml"),
            "[tools]\nrust = \"0.0.0-not-installed\"\n",
        )
        .unwrap();

        let err = provision(workspace.path(), cache.path(), "wt:a").unwrap_err();
        assert!(
            err.contains("refusing to provision a mismatched compiler"),
            "got: {err}"
        );
    }

    #[test]
    fn a_failed_provision_never_leaves_a_ready_marker() {
        let workspace = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        std::fs::write(
            workspace.path().join("mise.toml"),
            "[tools]\nrust = \"0.0.0-not-installed\"\n",
        )
        .unwrap();

        assert!(provision(workspace.path(), cache.path(), "wt:a").is_err());

        let st = status(workspace.path(), cache.path()).unwrap();
        assert!(
            !st.ready,
            "a half-built (here: never-built) toolchain must not read as ready"
        );
        // No scratch directories left behind either.
        let leftovers: Vec<_> = std::fs::read_dir(envs_dir(cache.path()))
            .map(|it| it.flatten().collect())
            .unwrap_or_default();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn an_unsupported_pinned_tool_fails_closed_without_a_ready_marker() {
        let workspace = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        std::fs::write(
            workspace.path().join("mise.toml"),
            format!(
                "[tools]\nrust = \"{}\"\nnode = \"22\"\n",
                host_rustc_short_version()
            ),
        )
        .unwrap();

        let err = provision(workspace.path(), cache.path(), "wt:a").unwrap_err();
        assert!(err.contains("node"), "got: {err}");

        let st = status(workspace.path(), cache.path()).unwrap();
        assert!(!st.ready, "an unsupported tool must never read as ready");
    }

    #[test]
    fn two_concurrent_provisions_of_the_same_digest_do_not_collide() {
        let workspace = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        std::fs::write(
            workspace.path().join("mise.toml"),
            format!("[tools]\nrust = \"{}\"\n", host_rustc_short_version()),
        )
        .unwrap();

        let workspace_path = workspace.path().to_path_buf();
        let cache_path = cache.path().to_path_buf();
        let cache_path2 = cache_path.clone();
        let workspace_path2 = workspace_path.clone();
        let a = std::thread::spawn(move || provision(&workspace_path, &cache_path, "wt:a"));
        let b = std::thread::spawn(move || provision(&workspace_path2, &cache_path2, "wt:b"));

        let a = a.join().unwrap().unwrap();
        let b = b.join().unwrap().unwrap();
        assert_eq!(a.toolchain_root, b.toolchain_root);
        assert!(a.toolchain_rust_bin().join("rustc").is_file());
        // No leftover scratch dirs from either racer.
        let leftovers: Vec<_> = std::fs::read_dir(envs_dir(cache.path()))
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with(".tmp-"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    /// A call that samples `cache_hit` before waiting on the digest lock,
    /// then never rechecks, would report "I provisioned this" for a call
    /// that actually did no work at all — it only ever waited for a racer's
    /// build to land. Forces that exact ordering: hold the digest lock in
    /// this thread, spawn `provision`, let it observe "not ready" and block
    /// on the lock, then make it ready and release.
    #[test]
    fn a_call_that_only_waited_out_a_racer_reports_a_true_cache_hit() {
        let workspace = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        std::fs::write(
            workspace.path().join("mise.toml"),
            format!("[tools]\nrust = \"{}\"\n", host_rustc_short_version()),
        )
        .unwrap();
        let spec = Spec::load(workspace.path()).unwrap();
        let digest = spec.digest();

        let lock = digest_lock(&digest);
        let guard = lock.lock().unwrap();

        let workspace_path = workspace.path().to_path_buf();
        let cache_path = cache.path().to_path_buf();
        let waiter =
            std::thread::spawn(move || provision(&workspace_path, &cache_path, "wt:waiter"));

        // Give the waiter time to see "not ready" and block on the lock we hold.
        std::thread::sleep(std::time::Duration::from_millis(150));

        // Simulate the "other holder" finishing while the waiter is blocked.
        let toolchain_root = envs_dir(cache.path()).join(&digest);
        std::fs::create_dir_all(&toolchain_root).unwrap();
        std::fs::write(ready_marker(&toolchain_root), b"").unwrap();
        drop(guard);

        let result = waiter.join().unwrap().unwrap();
        assert!(
            result.cache_hit,
            "a call that only ever waited for a racer's build must report a true cache hit"
        );
    }

    #[test]
    fn status_reports_ready_without_provisioning() {
        let workspace = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        std::fs::write(
            workspace.path().join("mise.toml"),
            format!("[tools]\nrust = \"{}\"\n", host_rustc_short_version()),
        )
        .unwrap();

        let before = status(workspace.path(), cache.path()).unwrap();
        assert!(!before.ready);
        assert!(!before.root.exists(), "status must not provision");

        synthetic_test_environment(workspace.path(), cache.path(), "wt:a");
        let after = status(workspace.path(), cache.path()).unwrap();
        assert!(after.ready);
        assert_eq!(after.digest, before.digest);
    }
}
