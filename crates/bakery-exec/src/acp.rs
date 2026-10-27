//! Boxed duplex stdio spawn for ACP (Agent Client Protocol) children.
//!
//! Same deny-closed Seatbelt door as `attach_exec`, but with duplex stdio
//! (stdin + stdout = the RPC stream) and network allowed coarsely. The child
//! binary is content-addressed and staged into a read-only cache so Seatbelt
//! can exec it without an all-of-$HOME or all-of-target/ rule that would let
//! the child rewrite the binary it is running under.

use crate::session::SessionEnvironment;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Server-side-only placeholder for an ACP adapter environment variable that
/// must resolve to this seat's private temporary directory at spawn time.
/// This keeps provider-specific variable names out of the shared room code
/// while preventing adapters from falling back to a host-global `/tmp` path.
pub const PRIVATE_TMPDIR_ENV_VALUE: &str = "{bakery-seat-tmpdir}";
/// In a variable's value, the room's own home: a seat's home in a native
/// room, the room's home in a command room (e.g. a private CARGO_HOME).
pub const ROOM_HOME_ENV_VALUE: &str = "{bakery-room-home}";

/// Server-owned PATH prefix consumed by [`spawn_boxed_acp`]. It is stripped
/// before the child starts and must stay off the plug env allowlist: a plug
/// that could set it could shadow `security` inside the seat.
pub const ACP_PATH_PREFIX_ENV: &str = "BAKERY_ACP_PATH_PREFIX";

/// Server-owned network reach for a room, consumed like
/// [`ACP_PATH_PREFIX_ENV`] and never forwarded: `local` keeps the room to
/// this Mac (loopback and local sockets), so anything farther goes through
/// the Bakery's gateway, a proxy on loopback. Off any agent's or plug's
/// reach: variable grants refuse `BAKERY_*` names.
pub const NET_ENV: &str = "BAKERY_NET";

/// Server-owned: the one port in [`DOORS`] this room may reach, its
/// thread's own door to the Bakery. Consumed like [`NET_ENV`].
pub const DOOR_ENV: &str = "BAKERY_DOOR";

/// Server-owned: the Bakery's own shared ports on this Mac, comma-separated
/// (its API, the model proxy's, the gateways'), closed to the room whatever
/// its network. Consumed like [`NET_ENV`].
pub const SHUT_ENV: &str = "BAKERY_SHUT";

/// Server-owned: `1` for a room that builds with Xcode, which may then make
/// and use temporary folders in the user's own temp folder
/// ([`crate::attach::xcode_temp`]). Consumed like [`NET_ENV`].
pub const XCODE_ENV: &str = "BAKERY_XCODE";

/// Server-owned: secret-named files the operator let this room read, one
/// path a line ([`crate::attach::readable`]): readable, never writable,
/// past the credential fence. Consumed like [`NET_ENV`].
pub const READABLE_ENV: &str = "BAKERY_READABLE";

/// Every thread's door to the Bakery is one port here, on loopback. A room
/// reaches only its own (identity by reach, not by secret): macOS lets any
/// process read a same-user process's arguments and environment, which no
/// Seatbelt rule stops, so a secret alone can't tell rooms apart.
pub const DOORS: std::ops::Range<u16> = 47800..48800;

/// Every project's own service (its Redis, say) and every thread's copy of
/// one listens on one port here, on loopback. A room reaches only the ones
/// [`SERVICES_ENV`] names, as it reaches only its own door.
pub const SERVICES: std::ops::Range<u16> = 46000..46500;

/// Server-owned: the ports in [`SERVICES`] this room may reach,
/// comma-separated: its project's services, and its thread's copies.
/// Consumed like [`NET_ENV`].
pub const SERVICES_ENV: &str = "BAKERY_SERVICES";

/// Server-owned: `open` when the operator lets this sandbox use their key
/// agents (ssh-agent, gpg-agent); closed otherwise
/// ([`crate::whole_machine::key_agent_sockets`]). Consumed like [`NET_ENV`].
pub const KEY_AGENTS_ENV: &str = "BAKERY_KEY_AGENTS";

/// What Claude Code reads as its own settings and instructions in its config
/// folder: settings (hooks, permissions, env, a key helper), its memory file,
/// commands, subagents, skills, output styles and plugins. The runtime only
/// reads them, so its room keeps them read-only ([`claude_settings`]): no
/// command an agent runs can change what the next session loads.
pub const CLAUDE_SETTINGS: &[&str] = &[
    "settings.json",
    "settings.local.json",
    "CLAUDE.md",
    "commands",
    "agents",
    "skills",
    "rules",
    "output-styles",
    "plugins",
];

/// [`CLAUDE_SETTINGS`] in one Claude config folder.
pub fn claude_settings(config_dir: &Path) -> Vec<PathBuf> {
    CLAUDE_SETTINGS.iter().map(|name| config_dir.join(name)).collect()
}

/// Folders agent runtimes keep their own state in. A runtime's state belongs
/// in its private HOME inside Bakery, so no runtime may create or change one
/// of these at a project's root — they stay readable, so a project's own
/// `.cursor/rules` or `.claude/commands` still inform the agent.
pub const AGENT_STATE_DIRS: &[&str] = &[
    ".claude",
    ".codex",
    ".cursor",
    ".gemini",
    ".grok",
    ".agy",
    ".antigravity",
    ".windsurf",
    ".continue",
];

/// What under [`AGENT_STATE_DIRS`] holds project work, not a runtime's
/// state, and so stays writable wherever the workspace is: Claude Code's own
/// worktrees.
pub const AGENT_WORK_DIRS: &[&str] = &[".claude/worktrees"];

// ─── Per-seat HOME ──────────────────────────────────────────────────────────

/// Per-(stream, seat) private HOME for an ACP child. Two seats on the same
/// stream get different directories; neither can clobber the other's session
/// state. `socket_runtime_dir` is stream-wide — do not use it as ACP HOME.
pub fn seat_acp_home(cache_root: &Path, stream_id: &str, seat_name: &str) -> PathBuf {
    let safe = stream_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect::<String>();
    cache_root
        .join("seats")
        .join(safe)
        .join(seat_name)
        .join("home")
}

// ─── Content-addressed binary staging ──────────────────────────────────────

/// Stage `binary` into `<cache_root>/acp-bins/<hex-hash>/exe`, atomically.
/// Two concurrent callers staging the same binary content converge on the
/// same slot (content-addressed, atomic rename). The staged binary is
/// read-only: the child cannot overwrite it while running.
///
/// Returns the path of the staged executable.
pub fn stage_acp_binary(binary: &Path, cache_root: &Path) -> Result<PathBuf, String> {
    let bytes = std::fs::read(binary)
        .map_err(|e| format!("cannot read binary {}: {e}", binary.display()))?;
    let digest = {
        let mut h = blake3::Hasher::new();
        h.update(&bytes);
        h.finalize().to_hex().to_string()
    };

    let bin_dir = cache_root.join("acp-bins").join(&digest);
    let exe_path = bin_dir.join("exe");
    let ready_path = bin_dir.join("ready");

    if ready_path.is_file() {
        return Ok(exe_path);
    }

    static SCRATCH_COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = SCRATCH_COUNTER.fetch_add(1, Ordering::Relaxed);
    let scratch = cache_root
        .join("acp-bins")
        .join(format!(".tmp-{digest}-{}-{n}", std::process::id()));

    std::fs::create_dir_all(&scratch).map_err(|e| e.to_string())?;
    let scratch_exe = scratch.join("exe");
    std::fs::write(&scratch_exe, &bytes).map_err(|e| e.to_string())?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&scratch_exe)
            .map_err(|e| e.to_string())?
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&scratch_exe, perms).map_err(|e| e.to_string())?;
    }

    std::fs::write(scratch.join("ready"), b"").map_err(|e| e.to_string())?;

    if !ready_path.is_file() {
        if std::fs::rename(&scratch, &bin_dir).is_err() && !ready_path.is_file() {
            let _ = std::fs::remove_dir_all(&scratch);
            return Err(format!(
                "failed to publish staged ACP binary for digest {digest}"
            ));
        }
    }
    let _ = std::fs::remove_dir_all(&scratch);
    Ok(exe_path)
}

// ─── ACP-specific Seatbelt profile ─────────────────────────────────────────

fn canon(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}

fn quote(p: &Path) -> String {
    format!(
        "\"{}\"",
        p.to_string_lossy()
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
    )
}

/// Deny-closed ACP profile. Differs from `attach_exec`'s profile in two
/// ways that are honest about the ACP child's actual needs:
///
/// 1. `allow network*` — an ACP-speaking coding agent may need to reach its
///    own model API, auth endpoint, or telemetry sink. Seatbelt is not a
///    per-host firewall; this rule allows all outbound network, not just
///    named hosts. Stated explicitly here rather than hidden.
///
/// 2. `process-exec` extended to include the staged binary and its directory
///    (plus, when the ACP child needs to spawn its own MCP server(s) —
///    07d-B Phase 2 Stage C — every directory in `extra_exec_dirs`, each
///    expected to be its own content-addressed staged binary directory, the
///    same shape as `staged_binary`'s own). Every such directory is
///    read-only for the child (no write-allow rule covers it), so the child
///    can never rewrite a binary it is running under.
///
/// HOME is `seat_home` (per seat, per stream), NOT the stream-wide
/// `socket_runtime_dir`. Trusted sockets (non-Burn streams) get read+write
/// on the workspace; Burn streams are read-only (same as the old behaviour).
///
/// `extra_read_dirs` are read-only grants (an operator's machine-access
/// grant, e.g. `~/.gitconfig`). Every grant — read, exec, or write — still
/// loses to `deny_read` and the credential regex, emitted last.
#[allow(clippy::too_many_arguments)] // positional grants; see write_acp_profile
fn acp_seatbelt_profile(
    toolchain_root: &Path,
    staged_binary: &Path,
    seat_home: &Path,
    workspace: &Path,
    deny_read: &[PathBuf],
    trusted: bool,
    extra_exec_dirs: &[PathBuf],
    extra_rw_dirs: &[PathBuf],
    extra_read_dirs: &[PathBuf],
) -> String {
    let toolchain_root = canon(toolchain_root);
    let staged_bin_dir = canon(staged_binary.parent().unwrap_or(staged_binary));
    let seat_home = canon(seat_home);
    let workspace = canon(workspace);
    let extra_exec_dirs: Vec<PathBuf> = extra_exec_dirs.iter().map(|p| canon(p)).collect();
    // Bakery-OWNED credential directories, read+write. A subscription adapter
    // authenticates by reading its own credential file and rewriting it when
    // the token refreshes, so a read-only grant would break on first refresh.
    // The caller must keep these under `~/.bakery` — never the operator's own
    // `~/.claude`/`~/.codex`, which stay denied below so a Bakery session can
    // never rotate the refresh token out from under the host CLI.
    let extra_rw_dirs: Vec<PathBuf> = extra_rw_dirs.iter().map(|p| canon(p)).collect();
    let extra_read_dirs: Vec<PathBuf> = extra_read_dirs.iter().map(|p| canon(p)).collect();

    let mut s = String::new();
    // The default denial goes unreported (a build in a seat can make thousands
    // a second, and the node reads the kernel's log for credential denials
    // alone); the credential rules below report as usual.
    s.push_str("(version 1)\n(deny default (with no-report))\n");
    s.push_str("(import \"system.sb\")\n");
    // Metadata-only traversal for every granted tree. Interpreters such as
    // Node call realpath/lstat on each ancestor before opening an entrypoint;
    // `(subpath <leaf>)` does not authorize those ancestor metadata reads.
    // Keep this scoped to the exact chains instead of granting host-wide
    // metadata reconnaissance. Credential denies are emitted last and still
    // override this allowance for protected paths.
    {
        let mut chain_roots = vec![
            toolchain_root.clone(),
            staged_bin_dir.clone(),
            seat_home.clone(),
            workspace.clone(),
        ];
        chain_roots.extend(extra_exec_dirs.iter().cloned());
        chain_roots.extend(crate::attach::apple_developer_dirs().iter().cloned());
        chain_roots.extend(extra_rw_dirs.iter().cloned());
        chain_roots.extend(extra_read_dirs.iter().cloned());
        let mut seen = std::collections::HashSet::<PathBuf>::new();
        s.push_str("(allow file-read-metadata\n");
        for root in &chain_roots {
            for ancestor in root.ancestors() {
                if seen.insert(ancestor.to_path_buf()) {
                    s.push_str(&format!("  (literal {})\n", quote(ancestor)));
                }
            }
        }
        s.push_str(")\n");
    }
    // Network: allowed coarsely. ACP agent may need its own endpoints.
    // This is an honest statement — Seatbelt is not a per-host firewall.
    s.push_str("(allow network*)\n");
    // Except the closed sockets: Docker's (its own switch, off by default)
    // and Claude Code's sessions'.
    s.push_str(crate::whole_machine::CLOSED_SOCKETS);
    s.push('\n');
    // A seat may stop itself and a process in its own sandbox, and no
    // process outside it. A bare allow would reach the rest of the machine.
    s.push_str("(allow process-fork)\n");
    s.push_str("(allow signal (target self))\n");
    s.push_str("(allow signal (target same-sandbox))\n");
    s.push_str("(allow pseudo-tty)\n");
    s.push_str("(allow file-read* file-write* file-ioctl (literal \"/dev/ptmx\"))\n");
    s.push_str("(allow file-read* file-write* file-ioctl (regex #\"^/dev/ttys[0-9]+$\"))\n");

    // The standard macOS PATH's system folders. `/usr/sbin` and `/sbin` hold
    // Apple's own tools (Flutter needs `sysctl`); none changes the system
    // without root.
    s.push_str("(allow process-exec\n");
    for p in [
        "/bin",
        "/usr/bin",
        "/usr/sbin",
        "/sbin",
        "/usr/lib",
        "/System",
        "/System/Volumes/Preboot/Cryptexes",
    ] {
        s.push_str(&format!("  (subpath {})\n", quote(Path::new(p))));
    }
    s.push_str(&format!("  (subpath {})\n", quote(&toolchain_root)));
    // The staged binary's directory — the only place Seatbelt will allow
    // exec of this specific content-addressed binary.
    s.push_str(&format!("  (subpath {})\n", quote(&staged_bin_dir)));
    for dir in &extra_exec_dirs {
        s.push_str(&format!("  (subpath {})\n", quote(dir)));
    }
    // The real git (and cc, clang) behind /usr/bin's xcrun shims, as in a
    // command room: read and run only.
    for dir in crate::attach::apple_developer_dirs() {
        s.push_str(&format!("  (subpath {})\n", quote(dir)));
    }
    // What the agent builds and installs, as in a command room: its
    // workspace, and its seat's own home and temp folder.
    for dir in [&workspace, &seat_home] {
        s.push_str(&format!("  (subpath {})\n", quote(dir)));
    }
    s.push_str(")\n");
    s.push_str("(allow file-map-executable\n");
    for dir in [&workspace, &seat_home, &toolchain_root, &staged_bin_dir]
        .into_iter()
        .chain(&extra_exec_dirs)
        .chain(crate::attach::apple_developer_dirs())
    {
        s.push_str(&format!("  (subpath {})\n", quote(dir)));
    }
    s.push_str(")\n");

    s.push_str("(allow file-read*\n");
    // /System/Volumes/Preboot/Cryptexes covers the dyld shared cache on macOS 13+
    // (arm64e) — the cache lives at
    // /System/Volumes/Preboot/Cryptexes/OS/System/Library/dyld/ which is NOT under
    // /System itself.
    for p in [
        "/bin",
        "/usr/bin",
        "/usr/sbin",
        "/sbin",
        "/usr/lib",
        "/System",
        "/System/Volumes/Preboot/Cryptexes",
        "/dev",
    ] {
        s.push_str(&format!("  (subpath {})\n", quote(Path::new(p))));
    }
    // /etc is a symlink → /private/etc. Canonicalize it so Seatbelt matches
    // the real path, rather than opening all of /private (which includes /tmp,
    // /var/folders, etc. — outside the room).
    s.push_str(&format!(
        "  (subpath {})\n",
        quote(&canon(Path::new("/etc")))
    ));
    // `/usr/bin/git`, `/usr/bin/cc` are xcrun shims: with an exec grant on the
    // developer directory they also need to read which one is active and, on
    // full-Xcode machines, the license marker. One link, one plist.
    s.push_str(&format!(
        "  (literal {})\n",
        quote(Path::new("/var/db/xcode_select_link"))
    ));
    s.push_str(&format!(
        "  (literal {})\n",
        quote(Path::new("/Library/Preferences/com.apple.dt.Xcode.plist"))
    ));
    // `/bin/sh` reads which shell it is from this one symlink; without it
    // every command prints "Error opening /private/var/select/sh".
    s.push_str(&format!(
        "  (literal {})\n",
        quote(Path::new("/private/var/select/sh"))
    ));
    s.push_str(")\n");

    s.push_str("(allow file-write*\n");
    s.push_str(&format!("  (subpath {}))\n", quote(Path::new("/dev"))));

    s.push_str("(allow file-read*\n");
    s.push_str(&format!("  (subpath {})\n", quote(&toolchain_root)));
    s.push_str(&format!("  (subpath {})\n", quote(&staged_bin_dir)));
    for dir in crate::attach::apple_developer_dirs() {
        s.push_str(&format!("  (subpath {})\n", quote(dir)));
    }
    for dir in &extra_exec_dirs {
        s.push_str(&format!("  (subpath {})\n", quote(dir)));
    }
    s.push_str(&format!("  (subpath {})\n", quote(&seat_home)));
    s.push_str(&format!("  (subpath {})\n", quote(&workspace)));
    for dir in &extra_rw_dirs {
        s.push_str(&format!("  (subpath {})\n", quote(dir)));
    }
    for dir in &extra_read_dirs {
        s.push_str(&format!("  (subpath {})\n", quote(dir)));
    }
    s.push_str(")\n");

    // Write: seat HOME always; workspace only for trusted sockets (Burn = read-only).
    // Bakery-owned credential dirs are writable regardless of trust — a Burn
    // stream still has to be able to refresh its own token.
    s.push_str("(allow file-write*\n");
    s.push_str(&format!("  (subpath {})\n", quote(&seat_home)));
    for dir in &extra_rw_dirs {
        s.push_str(&format!("  (subpath {})\n", quote(dir)));
    }
    if trusted {
        s.push_str(&format!("  (subpath {})\n", quote(&workspace)));
    }
    s.push_str(")\n");

    // Agent state never lands in the project (see `AGENT_STATE_DIRS`); its
    // work there does (`AGENT_WORK_DIRS`), where the workspace is writable.
    s.push_str("(deny file-write*\n");
    for dir in AGENT_STATE_DIRS {
        s.push_str(&format!("  (subpath {})\n", quote(&workspace.join(dir))));
    }
    s.push_str(")\n");
    if trusted {
        s.push_str("(allow file-write*\n");
        for dir in AGENT_WORK_DIRS {
            s.push_str(&format!("  (subpath {})\n", quote(&workspace.join(dir))));
        }
        s.push_str(")\n");
    }
    s.push_str(&crate::whole_machine::workspace_git_deny(&workspace));
    s.push_str(&crate::whole_machine::git_settings_deny(Some(&workspace)));

    // Named credential surfaces: the absolute last word, read and write.
    if !deny_read.is_empty() {
        s.push_str("(deny file-read* file-write*\n");
        for p in deny_read {
            s.push_str(&format!("  (subpath {})\n", quote(&canon(p))));
        }
        s.push_str(")\n");
    }

    // Credential-shaped filenames, after every directory grant so the grant
    // cannot expose them. A directory that holds a sign-in may still be
    // granted for the files that are not credentials. The seat's own
    // stand-in, under a `seat-config` directory, is readable again below.
    // A copied `auth.json` in the seat home is not.
    let mut credential_roots = vec![seat_home.clone(), workspace.clone()];
    credential_roots.extend(extra_rw_dirs.iter().cloned());
    for root in &credential_roots {
        let deny_regex = crate::attach::credential_deny_regex(root);
        s.push_str("(deny file-read*\n");
        s.push_str(&format!("  (regex #\"{deny_regex}\"))\n"));
        s.push_str("(deny file-write*\n");
        s.push_str(&format!("  (regex #\"{deny_regex}\"))\n"));
    }
    let shapes = crate::credentials::CREDENTIAL_SHAPES
        .iter()
        .map(|shape| crate::attach::shape_to_regex(shape))
        .collect::<Vec<_>>()
        .join("|");
    s.push_str("(allow file-read*\n");
    s.push_str(&format!("  (regex #\"^.*/seat-config/(.*/)?({shapes})$\"))\n"));
    let package = crate::attach::package_certificate_regex(&workspace);
    s.push_str(&format!("(allow file-read* file-write*\n  (regex #\"{package}\"))\n"));
    // A `.pem` in this room's own temp folder, and nowhere else.
    let temp = crate::attach::temp_certificate_regex(&seat_home.join("tmp"));
    s.push_str(&format!("(allow file-read* file-write*\n  (regex #\"{temp}\"))\n"));
    s.push_str(crate::whole_machine::KEYCHAIN_DENY);

    s
}

// ─── The handle ─────────────────────────────────────────────────────────────

// Inner struct holds the fields that need moving — by not implementing Drop
// itself, it can be pattern-destructured freely in `into_io_and_killer`.
struct BoxedAcpHandleInner {
    stdin: ChildStdin,
    stdout: ChildStdout,
    stderr: ChildStderr,
    child: Child,
    cleanup: Cleanup,
}

/// What to undo once the child is gone.
pub(crate) enum Cleanup {
    /// Seatbelt: the profile `sandbox-exec` read at start.
    Profile(PathBuf),
    /// A container, killed by name: its runtime can outlive a killed CLI.
    Container {
        cli: PathBuf,
        host_env: Vec<(String, String)>,
        name: String,
    },
}

/// A live boxed ACP child: the process handle plus its three connected pipe
/// ends. The caller speaks ACP on stdin/stdout; stderr is captured separately
/// and bounded by the caller (the ACP crate's framing layer owns stdout).
pub struct BoxedAcpHandle {
    pub pid: u32,
    inner: Option<BoxedAcpHandleInner>,
}

const ACP_REAP_GRACE: Duration = Duration::from_secs(3);

fn reap(pid: u32, mut child: Child, cleanup: Cleanup) {
    crate::attach::kill_process_group(pid);
    let deadline = std::time::Instant::now() + ACP_REAP_GRACE;
    while std::time::Instant::now() < deadline {
        if let Ok(Some(_)) = child.try_wait() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    match cleanup {
        Cleanup::Profile(path) => {
            let _ = std::fs::remove_file(path);
        }
        Cleanup::Container {
            cli,
            host_env,
            name,
        } => {
            let _ = std::process::Command::new(cli)
                .envs(host_env)
                .args(["kill", &name])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}

impl BoxedAcpHandle {
    /// A handle on a child spawned with all three pipes.
    pub(crate) fn new(mut child: Child, cleanup: Cleanup) -> Self {
        let pid = child.id();
        let stdin = child.stdin.take().expect("stdin piped");
        let stdout = child.stdout.take().expect("stdout piped");
        let stderr = child.stderr.take().expect("stderr piped");
        Self {
            pid,
            inner: Some(BoxedAcpHandleInner {
                stdin,
                stdout,
                stderr,
                child,
                cleanup,
            }),
        }
    }

    pub fn kill_and_reap(&mut self) {
        if let Some(inner) = self.inner.take() {
            reap(self.pid, inner.child, inner.cleanup);
        }
    }

    /// Consume the handle, yielding its I/O ends separately from a
    /// kill-and-reap closure.  Callers that move `stdin`/`stdout` into async
    /// wrappers (which partially-move the struct) use this to still perform a
    /// clean kill+reap by calling the returned closure later.
    pub fn into_io_and_killer(
        mut self,
    ) -> (u32, ChildStdin, ChildStdout, ChildStderr, impl FnOnce()) {
        // Option::take empties self.inner so Drop is a no-op when self drops
        // at the end of this call.  BoxedAcpHandleInner has no Drop impl so
        // its fields can be freely pattern-destructured.
        let BoxedAcpHandleInner {
            stdin,
            stdout,
            stderr,
            child,
            cleanup,
        } = self.inner.take().expect("BoxedAcpHandle already consumed");
        let pid = self.pid;
        (pid, stdin, stdout, stderr, move || {
            reap(pid, child, cleanup)
        })
    }
}

impl Drop for BoxedAcpHandle {
    fn drop(&mut self) {
        self.kill_and_reap();
    }
}

// ─── The spawn ──────────────────────────────────────────────────────────────

static ACP_PROFILE_COUNTER: AtomicU64 = AtomicU64::new(0);

#[allow(clippy::too_many_arguments)] // positional grants; bundling into a ProfileSpec-style struct is queued cleanup
fn write_acp_profile(
    cache_root: &Path,
    env_digest: &str,
    staged_binary: &Path,
    seat_home: &Path,
    workspace: &Path,
    deny_read: &[PathBuf],
    toolchain_root: &Path,
    trusted: bool,
    extra_exec_dirs: &[PathBuf],
    extra_rw_dirs: &[PathBuf],
    extra_read_dirs: &[PathBuf],
    whole_machine: Option<&crate::WholeMachine>,
    read_only: &[PathBuf],
    extra_env: &[(String, String)],
) -> Result<PathBuf, String> {
    let local_net = extra_env
        .iter()
        .any(|(k, v)| k == NET_ENV && v == "local");
    // Profiles directory: outside every allow rule, so the child can never
    // read this file — same design as attach.rs's write_attach_profile.
    let dir = cache_root.join("profiles");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let n = ACP_PROFILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = dir.join(format!("acp-{}-{}-{n}.sb", std::process::id(), env_digest));

    let profile = match whole_machine {
        Some(m) => {
            let staged_dir = staged_binary.parent().unwrap_or(staged_binary);
            let mut read = vec![toolchain_root.to_path_buf(), staged_dir.to_path_buf()];
            read.extend_from_slice(extra_exec_dirs);
            read.extend_from_slice(extra_read_dirs);
            let mut write = vec![seat_home.to_path_buf()];
            write.extend_from_slice(extra_rw_dirs);
            crate::whole_machine::profile(m, &read, &write, Some(workspace), trusted, deny_read)
        }
        None => acp_seatbelt_profile(
            toolchain_root,
            staged_binary,
            seat_home,
            workspace,
            deny_read,
            trusted,
            extra_exec_dirs,
            extra_rw_dirs,
            extra_read_dirs,
        ),
    };
    // Read-only inside what is otherwise writable: last, so nothing above
    // reopens it.
    let mut profile = profile;
    for p in read_only {
        profile.push_str(&format!("(deny file-write* (subpath {}))\n", quote(&canon(p))));
    }
    if local_net {
        profile.push_str(&crate::attach::local_network());
    }
    if crate::attach::xcode(extra_env) {
        profile.push_str(&crate::attach::xcode_temp());
    }
    profile.push_str(&crate::attach::readable(extra_env));
    if whole_machine.is_none() {
        profile.push_str(&crate::attach::closed_doors(extra_env));
    }
    profile.push_str(&crate::whole_machine::key_agent_sockets(extra_env));
    std::fs::write(&path, &profile).map_err(|e| e.to_string())?;
    Ok(path)
}

/// Spawn an ACP child in the box:
/// - Deny-closed Seatbelt profile (network coarsely allowed)
/// - Scrubbed `env -i`: HOME/TMPDIR inside seat_home, PATH=toolchain_bin + /bin + /usr/bin
/// - `extra_env` injected on the child only (not this process)
/// - `current_dir` = workspace (readable and writable — trusted socket)
/// - Own process group; kill via [`BoxedAcpHandle::kill_and_reap`]
///
/// Fails closed if `/usr/bin/sandbox-exec` is missing — no host-process
/// fallback.
///
/// `extra_process_exec_dirs` (07d-B Phase 2 Stage C): additional
/// content-addressed staged-binary directories the child may `exec` — used
/// when the ACP child itself needs to spawn a loopback MCP server named in
/// `session/new`'s `mcpServers`. Each entry is expected to be one
/// `stage_acp_binary`-style read-only directory, the same shape
/// `staged_binary`'s own parent is.
///
/// `extra_rw_dirs`: directories the child may read AND write — Bakery-owned
/// credential directories (so a subscription adapter can refresh its own
/// OAuth token in place; the operator's own `~/.claude`/`~/.codex` stay in
/// `default_deny_paths` so a Bakery session can never rotate the host CLI's
/// refresh token) and an operator's read+write machine-access grants.
///
/// `extra_read_dirs`: read-only grants (see `acp_seatbelt_profile`).
///
/// `whole_machine`: `Some` for Host full (allow by default minus the deny
/// list, see [`crate::WholeMachine`]); `None` keeps the deny-closed room.
#[allow(clippy::too_many_arguments)] // same as write_acp_profile above
pub fn spawn_boxed_acp(
    env: &SessionEnvironment,
    cache_root: &Path,
    staged_binary: &Path,
    seat_home: &Path,
    workspace: &Path,
    extra_deny_read: &[PathBuf],
    extra_env: &[(String, String)],
    argv_suffix: &[String],
    trusted: bool,
    extra_process_exec_dirs: &[PathBuf],
    extra_rw_dirs: &[PathBuf],
    extra_read_dirs: &[PathBuf],
    whole_machine: Option<&crate::WholeMachine>,
    read_only: &[PathBuf],
) -> Result<BoxedAcpHandle, String> {
    if !Path::new("/usr/bin/sandbox-exec").is_file() {
        return Err(
            "seatbelt (sandbox-exec) unavailable — refusing to spawn boxed ACP child without it"
                .to_string(),
        );
    }

    std::fs::create_dir_all(seat_home).map_err(|e| e.to_string())?;
    let seat_tmp = seat_home.join("tmp");
    std::fs::create_dir_all(&seat_tmp).map_err(|e| e.to_string())?;
    // Must exist before the profile canonicalizes it, or the grant silently
    // resolves to the wrong path and the adapter is jailed out of its own
    // credential again.
    for dir in extra_rw_dirs {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("credential dir {}: {e}", dir.display()))?;
    }

    let mut deny_read = crate::attach::default_deny_paths();
    deny_read.extend_from_slice(extra_deny_read);

    let profile_path = write_acp_profile(
        cache_root,
        &env.digest,
        staged_binary,
        seat_home,
        workspace,
        &deny_read,
        &env.toolchain_root,
        trusted,
        extra_process_exec_dirs,
        extra_rw_dirs,
        extra_read_dirs,
        whole_machine,
        read_only,
        extra_env,
    )?;

    // Thin rooms pin nothing, so `toolchain_rust_bin()` never existed —
    // only prepend it when there is an actual toolchain to expose.
    // `ACP_PATH_PREFIX_ENV` is consumed here and never forwarded: Claude
    // native uses it to put a `security` stub ahead of `/usr/bin`.
    let mut path_prefixes = Vec::new();
    let mut forwarded_env = Vec::new();
    for (k, v) in extra_env {
        if k == ACP_PATH_PREFIX_ENV {
            if !v.is_empty() {
                path_prefixes.push(v.as_str());
            }
        } else if ![NET_ENV, DOOR_ENV, SHUT_ENV, XCODE_ENV, READABLE_ENV, SERVICES_ENV, KEY_AGENTS_ENV].contains(&k.as_str()) {
            forwarded_env.push((k.as_str(), v.as_str()));
        }
    }
    let mut path_val = if env.toolchain_rust_bin().is_dir() {
        format!(
            "{}:/bin:/usr/bin:/usr/sbin:/sbin",
            env.toolchain_rust_bin().display()
        )
    } else {
        "/bin:/usr/bin:/usr/sbin:/sbin".to_string()
    };
    if !path_prefixes.is_empty() {
        path_val = format!("{}:{path_val}", path_prefixes.join(":"));
    }
    if let Some(m) = whole_machine.filter(|m| !m.path.is_empty()) {
        path_val = format!("{path_val}:{}", m.path);
    }

    let mut cmd = std::process::Command::new("/usr/bin/sandbox-exec");
    cmd.arg("-f").arg(&profile_path);
    cmd.arg("/usr/bin/env")
        .arg("-i")
        .arg(format!("HOME={}", seat_home.display()))
        .arg(format!("TMPDIR={}", seat_tmp.display()))
        .arg(format!("PATH={path_val}"))
        // Without a locale, CocoaPods, Ruby and others warn or misread UTF-8.
        .arg("LANG=en_US.UTF-8");
    for (k, v) in forwarded_env {
        let value = if v == PRIVATE_TMPDIR_ENV_VALUE {
            seat_tmp.to_string_lossy().into_owned()
        } else {
            v.replace(ROOM_HOME_ENV_VALUE, &seat_home.to_string_lossy())
        };
        cmd.arg(format!("{k}={value}"));
    }
    cmd.arg(staged_binary);
    for a in argv_suffix {
        cmd.arg(a);
    }
    cmd.current_dir(workspace);

    use std::os::unix::process::CommandExt;
    cmd.process_group(0);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let child = cmd
        .spawn()
        .map_err(|e| format!("spawn boxed ACP child: {e}"))?;

    // Profile is deleted in kill_and_reap / into_io_and_killer — not here.
    // sandbox-exec reads the file after fork() but before exec'ing the target;
    // deleting immediately races with that read and causes exit 65.
    Ok(BoxedAcpHandle::new(child, Cleanup::Profile(profile_path)))
}

// Seatbelt is macOS only: on Linux (a container seat) these have nothing to test.
#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use crate::session::synthetic_test_environment;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::fs::PermissionsExt;

    /// `scripts/test.sh` builds it before the tests and names its copy, so no
    /// test spends its time limit building it; without that, built here.
    fn fake_acp_agent_binary() -> PathBuf {
        static BINARY: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
        BINARY
            .get_or_init(|| {
                if let Some(path) = std::env::var_os("BAKERY_FAKE_ACP_AGENT_BIN_FOR_TEST") {
                    let path = PathBuf::from(path);
                    assert!(path.is_file(), "BAKERY_FAKE_ACP_AGENT_BIN_FOR_TEST names {}, which isn't there", path.display());
                    return path;
                }
                let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
                let status = std::process::Command::new(&cargo)
                    .args([
                        "build",
                        "-p",
                        "bakery-node",
                        "--bin",
                        "bakery-fake-acp-agent",
                    ])
                    .status()
                    .expect("run cargo build for fake ACP agent");
                assert!(status.success(), "building bakery-fake-acp-agent failed");
                let mut path = std::env::current_exe().expect("current test binary path");
                path.pop();
                if path.ends_with("deps") {
                    path.pop();
                }
                path.push("bakery-fake-acp-agent");
                assert!(
                    path.is_file(),
                    "fake ACP agent binary not found at {path:?}"
                );
                path
            })
            .clone()
    }

    fn provisioned(
        cache_root: &Path,
        workspace_root: &Path,
        stream_id: &str,
    ) -> SessionEnvironment {
        synthetic_test_environment(workspace_root, cache_root, stream_id)
    }

    fn prompt_notification_text(handle: &mut BoxedAcpHandle) -> String {
        let io = handle.inner.as_mut().unwrap();
        let stdin = &mut io.stdin;
        let mut reader = BufReader::new(&mut io.stdout);
        let mut line = String::new();

        writeln!(stdin, "{}", r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1,"clientInfo":{"name":"t","version":"0"}}}"#).unwrap();
        reader.read_line(&mut line).unwrap();
        line.clear();
        writeln!(stdin, "{}", r#"{"jsonrpc":"2.0","id":2,"method":"authenticate","params":{"authMethodId":"cached_token"}}"#).unwrap();
        reader.read_line(&mut line).unwrap();
        line.clear();
        writeln!(stdin, "{}", r#"{"jsonrpc":"2.0","id":3,"method":"session/new","params":{"cwd":"/","mcpServers":[]}}"#).unwrap();
        reader.read_line(&mut line).unwrap();
        line.clear();
        writeln!(stdin, "{}", r#"{"jsonrpc":"2.0","id":4,"method":"session/prompt","params":{"sessionId":"fake-session","messages":[{"role":"user","content":[{"type":"text","text":"hi"}]}]}}"#).unwrap();
        reader.read_line(&mut line).unwrap();
        let notification: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        line.clear();
        reader.read_line(&mut line).unwrap();
        notification["params"]["update"]["content"]["text"]
            .as_str()
            .unwrap_or("")
            .to_string()
    }

    #[test]
    fn two_seats_on_one_stream_get_different_homes() {
        let cache = tempfile::tempdir().unwrap();
        let a = seat_acp_home(cache.path(), "wt:x", "orchestrator");
        let b = seat_acp_home(cache.path(), "wt:x", "worker");
        assert_ne!(a, b);
    }

    #[test]
    fn staging_the_same_binary_twice_is_idempotent() {
        let cache = tempfile::tempdir().unwrap();
        let binary = fake_acp_agent_binary();
        let p1 = stage_acp_binary(&binary, cache.path()).unwrap();
        let p2 = stage_acp_binary(&binary, cache.path()).unwrap();
        assert_eq!(p1, p2);
        assert!(p1.is_file());
    }

    #[test]
    fn staged_binary_is_in_cache_not_in_seat_home() {
        let cache = tempfile::tempdir().unwrap();
        let binary = fake_acp_agent_binary();
        let staged = stage_acp_binary(&binary, cache.path()).unwrap();
        assert!(
            staged.starts_with(cache.path().join("acp-bins")),
            "must be in cache: {staged:?}"
        );
        let seat_home = seat_acp_home(cache.path(), "wt:y", "orchestrator");
        assert!(
            !staged.starts_with(&seat_home),
            "must not be in seat HOME: {staged:?}"
        );
    }

    /// A Bakery-owned credential dir is readable AND writable (an OAuth
    /// adapter rewrites its token on refresh), while the operator's own
    /// `~/.claude` stays denied — the deny block is emitted last on purpose.
    #[test]
    fn credential_dirs_are_writable_but_never_the_operators_own_store() {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/Users/operator".into());
        let operator_store = PathBuf::from(&home).join(".claude");
        let bakery_owned = PathBuf::from(&home).join(".bakery/cache/claude-native/config");

        let profile = acp_seatbelt_profile(
            Path::new("/opt/bakery/toolchain"),
            Path::new("/private/tmp/bakery-cache/acp-bins/hash/exe"),
            Path::new("/private/tmp/bakery-cache/seats/stream/seat/home"),
            Path::new("/Users/operator/project"),
            &[operator_store.clone()],
            false, // even a Burn stream must be able to refresh its own token
            &[],
            &[bakery_owned.clone()],
            &[],
        );

        let grant = format!("(subpath {})", quote(&canon(&bakery_owned)));
        assert_eq!(
            profile.matches(&grant).count(),
            2,
            "the credential dir needs both a read and a write grant:\n{profile}"
        );
        let deny_block = profile.split("(deny file-read*").nth(1).unwrap_or("");
        assert!(
            deny_block.contains(&quote(&canon(&operator_store))),
            "the operator's own ~/.claude must still be denied:\n{profile}"
        );
        assert!(profile.contains("(deny default (with no-report))"), "the default denial is not logged");
        assert_eq!(profile.matches("no-report").count(), 1, "the credential rules still report:\n{profile}");
        assert!(profile.lines().any(|line| line == "(allow signal (target self))"));
        assert!(profile.lines().any(|line| line == "(allow signal (target same-sandbox))"));
        assert!(!profile.lines().any(|line| line == "(allow signal)"));
    }

    #[test]
    fn path_prefix_shadows_security_inside_the_seatbelt() {
        let cache = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let env = provisioned(cache.path(), workspace.path(), "wt:shim");
        let binary = fake_acp_agent_binary();
        let staged = stage_acp_binary(&binary, cache.path()).unwrap();
        let seat_home = seat_acp_home(cache.path(), "wt:shim", "orchestrator");
        let shim_dir = cache.path().join("shim");
        std::fs::create_dir_all(&shim_dir).unwrap();
        let shim = shim_dir.join("security");
        std::fs::write(&shim, "#!/bin/sh\nexit 44\n").unwrap();
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();

        let mut handle = spawn_boxed_acp(
            &env,
            cache.path(),
            &staged,
            &seat_home,
            workspace.path(),
            &[],
            &[
                (ACP_PATH_PREFIX_ENV.to_string(), shim_dir.display().to_string()),
                (
                    "BAKERY_FAKE_ACP_EXEC".to_string(),
                    "security >/dev/null 2>&1; printf 'code=%s prefix=%s' \"$?\" \"${BAKERY_ACP_PATH_PREFIX-absent}\"".to_string(),
                ),
            ],
            &[],
            true,
            &[shim_dir],
            &[],
            &[],
            None,
            &[],
        )
        .unwrap();
        let text = prompt_notification_text(&mut handle);
        handle.kill_and_reap();
        assert!(
            text.contains("code=44") && text.contains("prefix=absent"),
            "security inside the seat must be the stub, and the prefix env must not be forwarded: {text}"
        );
    }

    /// A read-only grant is readable and nothing more, and a host secret
    /// store under any grant — even read+write on its parent — stays denied:
    /// the denies are emitted after every allow.
    #[test]
    fn grants_never_reach_host_secrets_and_read_grants_stay_read_only() {
        let home =
            PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/Users/operator".into()));
        let gitconfig = home.join(".gitconfig");
        let profile = acp_seatbelt_profile(
            Path::new("/opt/bakery/toolchain"),
            Path::new("/private/tmp/bakery-cache/acp-bins/hash/exe"),
            Path::new("/private/tmp/bakery-cache/seats/stream/seat/home"),
            Path::new("/Users/operator/project"),
            &crate::attach::default_deny_paths(),
            true,
            &[],
            &[home.join(".cargo")],
            std::slice::from_ref(&gitconfig),
        );
        let read_grant = format!("(subpath {})", quote(&canon(&gitconfig)));
        assert_eq!(
            profile.matches(&read_grant).count(),
            1,
            "a read grant is one file-read* allow, never exec or write:\n{profile}"
        );
        let deny_at = profile.find("(deny file-read*").unwrap();
        let secret = quote(&canon(&home.join(".cargo").join("credentials.toml")));
        assert!(
            profile[deny_at..].contains(&format!("(subpath {secret})")),
            "a secret under a read+write grant must still be denied:\n{profile}"
        );
        assert!(
            deny_at > profile.rfind("(allow file-write*").unwrap(),
            "denies must follow every allow:\n{profile}"
        );
    }

    #[test]
    fn profile_grants_metadata_traversal_for_extra_exec_dirs() {
        let profile = acp_seatbelt_profile(
            Path::new("/opt/bakery/toolchain"),
            Path::new("/private/tmp/bakery-cache/acp-bins/hash/exe"),
            Path::new("/private/tmp/bakery-cache/seats/stream/seat/home"),
            Path::new("/Users/operator/project"),
            &[],
            true,
            &[PathBuf::from("/Users/operator/app/node_modules")],
            &[],
            &[],
        );

        assert!(profile.contains("(allow file-read-metadata"));
        assert!(profile.contains("(literal \"/Users\")"));
        assert!(profile.contains("(literal \"/Users/operator\")"));
        assert!(profile.contains("(subpath \"/Users/operator/app/node_modules\")"));
        assert!(
            !profile.contains("(subpath \"/Users\")"),
            "ancestor access must remain metadata-only"
        );
    }

    #[test]
    fn boxed_acp_child_completes_a_round_trip_handshake() {
        let cache = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let env = provisioned(cache.path(), workspace.path(), "wt:acp-rt");
        let binary = fake_acp_agent_binary();
        let staged = stage_acp_binary(&binary, cache.path()).unwrap();
        let seat_home = seat_acp_home(cache.path(), "wt:acp-rt", "orchestrator");

        let mut handle = spawn_boxed_acp(
            &env,
            cache.path(),
            &staged,
            &seat_home,
            workspace.path(),
            &[],
            &[],
            &[],
            true,
            &[],
            &[],
            &[],
            None,
            &[],
        )
        .unwrap();

        let mut line = String::new();
        {
            let io = handle.inner.as_mut().unwrap();
            let stdin = &mut io.stdin;
            let mut reader = BufReader::new(&mut io.stdout);

            writeln!(stdin, "{}", r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1,"clientInfo":{"name":"test","version":"0"}}}"#).unwrap();
            reader.read_line(&mut line).unwrap();
            let init: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
            assert_eq!(init["result"]["protocolVersion"], 1);
            line.clear();

            writeln!(stdin, "{}", r#"{"jsonrpc":"2.0","id":2,"method":"authenticate","params":{"authMethodId":"cached_token"}}"#).unwrap();
            reader.read_line(&mut line).unwrap();
            line.clear();

            writeln!(stdin, "{}", r#"{"jsonrpc":"2.0","id":3,"method":"session/new","params":{"cwd":"/","mcpServers":[]}}"#).unwrap();
            reader.read_line(&mut line).unwrap();
            let snew: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
            assert!(snew["result"]["sessionId"].is_string());
            line.clear();

            writeln!(stdin, "{}", r#"{"jsonrpc":"2.0","id":4,"method":"session/prompt","params":{"sessionId":"fake-session","messages":[{"role":"user","content":[{"type":"text","text":"hi"}]}]}}"#).unwrap();

            // Fake agent sends a notification (session/update) then the response.
            reader.read_line(&mut line).unwrap(); // notification
            line.clear();
            reader.read_line(&mut line).unwrap(); // response
            let resp: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
            assert_eq!(resp["result"]["stopReason"], "end_turn");
        } // borrows on handle.stdin and handle.stdout drop here

        handle.kill_and_reap();
    }

    /// The fake ACP agent reports its CWD via a session/update notification.
    /// The cwd must be the worktree, not bakery-node's own cwd.
    #[test]
    fn boxed_acp_child_cwd_is_the_worktree() {
        let cache = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let env = provisioned(cache.path(), workspace.path(), "wt:acp-cwd");
        let binary = fake_acp_agent_binary();
        let staged = stage_acp_binary(&binary, cache.path()).unwrap();
        let seat_home = seat_acp_home(cache.path(), "wt:acp-cwd", "orchestrator");

        let mut handle = spawn_boxed_acp(
            &env,
            cache.path(),
            &staged,
            &seat_home,
            workspace.path(),
            &[],
            &[("BAKERY_FAKE_ACP_PRINT_CWD".to_string(), "1".to_string())],
            &[],
            true,
            &[],
            &[],
            &[],
            None,
            &[],
        )
        .unwrap();

        let cwd_text = {
            let io = handle.inner.as_mut().unwrap();
            let stdin = &mut io.stdin;
            let mut reader = BufReader::new(&mut io.stdout);
            let mut line = String::new();

            writeln!(stdin, "{}", r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1,"clientInfo":{"name":"t","version":"0"}}}"#).unwrap();
            reader.read_line(&mut line).unwrap();
            line.clear();

            writeln!(stdin, "{}", r#"{"jsonrpc":"2.0","id":2,"method":"authenticate","params":{"authMethodId":"cached_token"}}"#).unwrap();
            reader.read_line(&mut line).unwrap();
            line.clear();

            writeln!(stdin, "{}", r#"{"jsonrpc":"2.0","id":3,"method":"session/new","params":{"cwd":"/","mcpServers":[]}}"#).unwrap();
            reader.read_line(&mut line).unwrap();
            line.clear();

            writeln!(stdin, "{}", r#"{"jsonrpc":"2.0","id":4,"method":"session/prompt","params":{"sessionId":"fake-session","messages":[{"role":"user","content":[{"type":"text","text":"hi"}]}]}}"#).unwrap();
            reader.read_line(&mut line).unwrap();
            let notif: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
            line.clear();
            reader.read_line(&mut line).unwrap(); // response; discard

            notif["params"]["update"]["content"]["text"]
                .as_str()
                .unwrap_or("")
                .to_string()
        };

        handle.kill_and_reap();

        let reported = PathBuf::from(cwd_text.trim())
            .canonicalize()
            .unwrap_or_else(|_| PathBuf::from(cwd_text.trim()));
        let expected = workspace.path().canonicalize().unwrap();
        assert_eq!(reported, expected);
    }

    #[test]
    fn host_env_does_not_reach_boxed_acp_child() {
        std::env::set_var("HOST_SECRET_ACP_TEST", "must-not-leak");
        let cache = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let env = provisioned(cache.path(), workspace.path(), "wt:acp-env");
        let binary = fake_acp_agent_binary();
        let staged = stage_acp_binary(&binary, cache.path()).unwrap();
        let seat_home = seat_acp_home(cache.path(), "wt:acp-env", "orchestrator");

        let mut handle = spawn_boxed_acp(
            &env,
            cache.path(),
            &staged,
            &seat_home,
            workspace.path(),
            &[],
            &[(
                "BAKERY_FAKE_ACP_CHECK_ENV_ABSENT".to_string(),
                "HOST_SECRET_ACP_TEST".to_string(),
            )],
            &[],
            true,
            &[],
            &[],
            &[],
            None,
            &[],
        )
        .unwrap();

        let notification_text = {
            let io = handle.inner.as_mut().unwrap();
            let stdin = &mut io.stdin;
            let mut reader = BufReader::new(&mut io.stdout);
            let mut line = String::new();

            writeln!(stdin, "{}", r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1,"clientInfo":{"name":"t","version":"0"}}}"#).unwrap();
            reader.read_line(&mut line).unwrap();
            line.clear();

            writeln!(stdin, "{}", r#"{"jsonrpc":"2.0","id":2,"method":"authenticate","params":{"authMethodId":"cached_token"}}"#).unwrap();
            reader.read_line(&mut line).unwrap();
            line.clear();

            writeln!(stdin, "{}", r#"{"jsonrpc":"2.0","id":3,"method":"session/new","params":{"cwd":"/","mcpServers":[]}}"#).unwrap();
            reader.read_line(&mut line).unwrap();
            line.clear();

            writeln!(stdin, "{}", r#"{"jsonrpc":"2.0","id":4,"method":"session/prompt","params":{"sessionId":"fake-session","messages":[{"role":"user","content":[{"type":"text","text":"hi"}]}]}}"#).unwrap();

            // BAKERY_FAKE_ACP_CHECK_ENV_ABSENT: notification arrives before response.
            reader.read_line(&mut line).unwrap();
            let notif: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
            line.clear();
            reader.read_line(&mut line).unwrap(); // response; discard

            notif["params"]["update"]["content"]["text"]
                .as_str()
                .unwrap_or("")
                .to_string()
        };

        std::env::remove_var("HOST_SECRET_ACP_TEST");
        assert!(
            notification_text.contains("is absent"),
            "HOST_SECRET_ACP_TEST must be absent: {notification_text:?}"
        );
        handle.kill_and_reap();
    }

    /// Drop kills the child: record the pid before dropping the handle; after
    /// drop, the process group must be gone (ESRCH from kill -0).
    #[test]
    fn boxed_acp_drop_kills_the_child() {
        let cache = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let env = provisioned(cache.path(), workspace.path(), "wt:acp-drop");
        let binary = fake_acp_agent_binary();
        let staged = stage_acp_binary(&binary, cache.path()).unwrap();
        let seat_home = seat_acp_home(cache.path(), "wt:acp-drop", "orchestrator");

        let handle = spawn_boxed_acp(
            &env,
            cache.path(),
            &staged,
            &seat_home,
            workspace.path(),
            &[],
            &[],
            &[],
            true,
            &[],
            &[],
            &[],
            None,
            &[],
        )
        .unwrap();
        let pid = handle.pid as libc::pid_t;
        // Drop without calling kill_and_reap — the Drop impl must kill it.
        drop(handle);

        // Give the grace period enough time to fully reap the process.
        std::thread::sleep(std::time::Duration::from_millis(200));
        let rc = unsafe { libc::kill(pid, 0) };
        let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        assert!(
            rc != 0 && errno == libc::ESRCH,
            "child pid {pid} must be gone after drop (rc={rc} errno={errno})"
        );
    }

    /// A native runtime's box reaches the network, but never Claude Code's own
    /// session sockets, by every path to one, its local network on or not: a
    /// project agent can never put a message into the operator's sessions.
    #[test]
    fn a_boxed_runtime_never_reaches_claude_codes_session_sockets() {
        let canary = crate::attach::SocketCanary::sessions();
        let cache = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let env = provisioned(cache.path(), workspace.path(), "wt:acp-cc-socks");
        let staged = stage_acp_binary(&fake_acp_agent_binary(), cache.path()).unwrap();
        let seat_home = seat_acp_home(cache.path(), "wt:acp-cc-socks", "orchestrator");
        std::fs::create_dir_all(&seat_home).unwrap();
        let local = [(NET_ENV.to_string(), "local".to_string())];
        for net in [&local[..], &[]] {
            let deny = crate::attach::default_deny_paths();
            let profile = write_acp_profile(
                cache.path(), &env.digest, &staged, &seat_home, workspace.path(), &deny, &env.toolchain_root,
                true, &[], &[], &[], None, &[], net,
            )
            .unwrap();
            for path in canary.paths(workspace.path()) {
                let out = std::process::Command::new("/usr/bin/sandbox-exec")
                    .args(["-f", &profile.display().to_string(), "/usr/bin/nc", "-U", "-w", "1", &path])
                    .current_dir(workspace.path())
                    .output()
                    .unwrap();
                assert!(!out.status.success(), "{path} (net {net:?}): {}", String::from_utf8_lossy(&out.stderr));
            }
        }
        assert_eq!(canary.reached(), 0, "a native runtime reached the operator's sessions");
    }

    /// A native runtime's box never reaches the operator's key agents, its
    /// local network on or not, as the whole machine doesn't, by every path
    /// to one.
    #[test]
    fn a_boxed_runtime_never_reaches_the_operators_key_agents() {
        let mut canaries = crate::attach::SocketCanary::key_agents();
        canaries.push(crate::attach::SocketCanary::at("bakery-canary", "", "own-agent"));
        let cache = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let env = provisioned(cache.path(), workspace.path(), "wt:acp-key-agents");
        let staged = stage_acp_binary(&fake_acp_agent_binary(), cache.path()).unwrap();
        let seat_home = seat_acp_home(cache.path(), "wt:acp-key-agents", "orchestrator");
        std::fs::create_dir_all(&seat_home).unwrap();
        let machine = crate::WholeMachine { home: home.path().to_path_buf(), bakery_root: cache.path().to_path_buf(), path: String::new() };
        let var = |k: &str, v: &str| (k.to_string(), v.to_string());
        let own = var("SSH_AUTH_SOCK", &canaries.last().unwrap().path.display().to_string());
        let local = [var(NET_ENV, "local"), own.clone()];
        let plain = [own];
        let deny = crate::attach::default_deny_paths();
        for (net, whole) in [(&local[..], None), (&plain[..], None), (&plain[..], Some(&machine))] {
            let profile = write_acp_profile(
                cache.path(), &env.digest, &staged, &seat_home, workspace.path(), &deny, &env.toolchain_root,
                true, &[], &[], &[], whole, &[], net,
            )
            .unwrap();
            for canary in &canaries {
                for path in canary.paths(workspace.path()) {
                    let out = std::process::Command::new("/usr/bin/sandbox-exec")
                        .args(["-f", &profile.display().to_string(), "/usr/bin/nc", "-U", "-w", "1", &path])
                        .current_dir(workspace.path())
                        .output()
                        .unwrap();
                    assert!(!out.status.success(), "{path} (whole {}): {}", whole.is_some(), String::from_utf8_lossy(&out.stderr));
                }
            }
        }
        for canary in &canaries {
            assert_eq!(canary.reached(), 0, "{} was reached", canary.path.display());
        }
    }

    /// The boxed child cannot read files under /private/tmp — their canonical
    /// path is covered by /private which is no longer in the allow list.
    #[test]
    fn boxed_acp_child_cannot_read_outside_room() {
        let marker = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(marker.path(), b"secret-marker").unwrap();
        let marker_path = marker.path().to_string_lossy().into_owned();

        let cache = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let env = provisioned(cache.path(), workspace.path(), "wt:acp-deny");
        let binary = fake_acp_agent_binary();
        let staged = stage_acp_binary(&binary, cache.path()).unwrap();
        let seat_home = seat_acp_home(cache.path(), "wt:acp-deny", "orchestrator");

        let mut handle = spawn_boxed_acp(
            &env,
            cache.path(),
            &staged,
            &seat_home,
            workspace.path(),
            &[],
            &[("BAKERY_FAKE_ACP_READ_FILE".to_string(), marker_path)],
            &[],
            true,
            &[],
            &[],
            &[],
            None,
            &[],
        )
        .unwrap();

        let notification_text = {
            let io = handle.inner.as_mut().unwrap();
            let stdin = &mut io.stdin;
            let mut reader = BufReader::new(&mut io.stdout);
            let mut line = String::new();

            writeln!(stdin, "{}", r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1,"clientInfo":{"name":"t","version":"0"}}}"#).unwrap();
            reader.read_line(&mut line).unwrap();
            line.clear();

            writeln!(stdin, "{}", r#"{"jsonrpc":"2.0","id":2,"method":"authenticate","params":{"authMethodId":"cached_token"}}"#).unwrap();
            reader.read_line(&mut line).unwrap();
            line.clear();

            writeln!(stdin, "{}", r#"{"jsonrpc":"2.0","id":3,"method":"session/new","params":{"cwd":"/","mcpServers":[]}}"#).unwrap();
            reader.read_line(&mut line).unwrap();
            line.clear();

            writeln!(stdin, "{}", r#"{"jsonrpc":"2.0","id":4,"method":"session/prompt","params":{"sessionId":"fake-session","messages":[{"role":"user","content":[{"type":"text","text":"hi"}]}]}}"#).unwrap();
            reader.read_line(&mut line).unwrap();
            let notif: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
            line.clear();
            reader.read_line(&mut line).unwrap(); // response; discard

            notif["params"]["update"]["content"]["text"]
                .as_str()
                .unwrap_or("")
                .to_string()
        };

        handle.kill_and_reap();
        assert!(
            notification_text.contains("read denied"),
            "boxed child must not be able to read outside its room: {notification_text:?}"
        );
    }

    #[test]
    fn boxed_acp_child_cannot_read_an_explicitly_denied_workspace_path() {
        let cache = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let denied = workspace.path().join("planted-private-note.txt");
        std::fs::write(&denied, b"planted test marker").unwrap();
        let env = provisioned(cache.path(), workspace.path(), "wt:acp-extra-deny");
        let binary = fake_acp_agent_binary();
        let staged = stage_acp_binary(&binary, cache.path()).unwrap();
        let seat_home = seat_acp_home(cache.path(), "wt:acp-extra-deny", "orchestrator");
        let mut handle = spawn_boxed_acp(
            &env,
            cache.path(),
            &staged,
            &seat_home,
            workspace.path(),
            std::slice::from_ref(&denied),
            &[(
                "BAKERY_FAKE_ACP_READ_FILE".to_string(),
                denied.to_string_lossy().into_owned(),
            )],
            &[],
            true,
            &[],
            &[],
            &[],
            None,
            &[],
        )
        .unwrap();

        let text = prompt_notification_text(&mut handle);
        handle.kill_and_reap();
        assert!(
            text.contains("read denied"),
            "explicit deny must override readable workspace: {text:?}"
        );
    }

    /// A runtime can't leave its own state in the project: writing into a
    /// `.claude/` at the workspace root is refused by the kernel, while an
    /// ordinary workspace file still writes.
    #[test]
    fn a_runtime_cannot_write_agent_state_into_the_project() {
        let cache = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        std::fs::create_dir(workspace.path().join(".claude")).unwrap();
        let env = provisioned(cache.path(), workspace.path(), "wt:acp-agent-dirs");
        let staged = stage_acp_binary(&fake_acp_agent_binary(), cache.path()).unwrap();
        let seat_home = seat_acp_home(cache.path(), "wt:acp-agent-dirs", "orchestrator");
        let write = |path: PathBuf| {
            let mut handle = spawn_boxed_acp(
                &env,
                cache.path(),
                &staged,
                &seat_home,
                workspace.path(),
                &[],
                &[(
                    "BAKERY_FAKE_ACP_WRITE_FILE".to_string(),
                    path.to_string_lossy().into_owned(),
                )],
                &[],
                true,
                &[],
                &[],
                &[],
                None,
                &[],
            )
            .unwrap();
            let text = prompt_notification_text(&mut handle);
            handle.kill_and_reap();
            text
        };
        let blocked = write(workspace.path().join(".claude/settings.local.json"));
        assert!(blocked.contains("Operation not permitted"), "{blocked}");
        let ordinary = write(workspace.path().join("notes.txt"));
        assert_eq!(ordinary, "write succeeded");
        // Claude Code's own worktrees there hold project work.
        std::fs::create_dir_all(workspace.path().join(".claude/worktrees/fix")).unwrap();
        let worktree = write(workspace.path().join(".claude/worktrees/fix/notes.txt"));
        assert_eq!(worktree, "write succeeded");
    }

    /// The credential_deny_regex in the seatbelt profile must block reads of
    /// `.env` files even inside the workspace that the child is allowed to
    /// read for source code (workspaces are readable but .env must be denied).
    #[test]
    fn boxed_acp_child_cannot_read_workspace_dotenv() {
        let cache = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let env_file = workspace.path().join(".env");
        std::fs::write(&env_file, b"SECRET=do-not-leak").unwrap();

        let env = provisioned(cache.path(), workspace.path(), "wt:acp-dotenv");
        let binary = fake_acp_agent_binary();
        let staged = stage_acp_binary(&binary, cache.path()).unwrap();
        let seat_home = seat_acp_home(cache.path(), "wt:acp-dotenv", "orchestrator");

        let mut handle = spawn_boxed_acp(
            &env,
            cache.path(),
            &staged,
            &seat_home,
            workspace.path(),
            &[],
            &[(
                "BAKERY_FAKE_ACP_READ_FILE".to_string(),
                env_file.to_string_lossy().into_owned(),
            )],
            &[],
            true,
            &[],
            &[],
            &[],
            None,
            &[],
        )
        .unwrap();

        let notification_text = {
            let io = handle.inner.as_mut().unwrap();
            let stdin = &mut io.stdin;
            let mut reader = BufReader::new(&mut io.stdout);
            let mut line = String::new();

            writeln!(stdin, "{}", r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1,"clientInfo":{"name":"t","version":"0"}}}"#).unwrap();
            reader.read_line(&mut line).unwrap();
            line.clear();
            writeln!(stdin, "{}", r#"{"jsonrpc":"2.0","id":2,"method":"authenticate","params":{"authMethodId":"cached_token"}}"#).unwrap();
            reader.read_line(&mut line).unwrap();
            line.clear();
            writeln!(stdin, "{}", r#"{"jsonrpc":"2.0","id":3,"method":"session/new","params":{"cwd":"/","mcpServers":[]}}"#).unwrap();
            reader.read_line(&mut line).unwrap();
            line.clear();
            writeln!(stdin, "{}", r#"{"jsonrpc":"2.0","id":4,"method":"session/prompt","params":{"sessionId":"fake-session","messages":[{"role":"user","content":[{"type":"text","text":"hi"}]}]}}"#).unwrap();
            reader.read_line(&mut line).unwrap();
            let notif: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
            line.clear();
            reader.read_line(&mut line).unwrap(); // response; discard
            notif["params"]["update"]["content"]["text"]
                .as_str()
                .unwrap_or("")
                .to_string()
        };

        handle.kill_and_reap();
        assert!(
            notification_text.contains("read denied"),
            "workspace .env must be denied by the credential regex: {notification_text:?}"
        );
    }

    #[test]
    fn staged_binary_is_byte_for_byte_unchanged_after_spawn_and_kill() {
        let cache = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let env = provisioned(cache.path(), workspace.path(), "wt:acp-ro");
        let binary = fake_acp_agent_binary();
        let staged = stage_acp_binary(&binary, cache.path()).unwrap();
        let before = std::fs::read(&staged).unwrap();
        let seat_home = seat_acp_home(cache.path(), "wt:acp-ro", "orchestrator");

        let mut handle = spawn_boxed_acp(
            &env,
            cache.path(),
            &staged,
            &seat_home,
            workspace.path(),
            &[],
            &[],
            &[],
            true,
            &[],
            &[],
            &[],
            None,
            &[],
        )
        .unwrap();
        handle.kill_and_reap();

        let after = std::fs::read(&staged).unwrap();
        assert_eq!(before, after);

        // Structural proof: staged binary is not under any path the profile allows writing.
        assert!(
            !staged.starts_with(&seat_home),
            "staged binary must not be in seat HOME: {staged:?}"
        );
    }

    #[test]
    fn staged_binary_directory_resists_an_active_child_write_attempt() {
        let cache = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let env = provisioned(cache.path(), workspace.path(), "wt:acp-ro-active");
        let binary = fake_acp_agent_binary();
        let staged = stage_acp_binary(&binary, cache.path()).unwrap();
        let sidecar = staged.parent().unwrap().join("write-proof");
        std::fs::write(&sidecar, b"unchanged").unwrap();
        let seat_home = seat_acp_home(cache.path(), "wt:acp-ro-active", "orchestrator");
        let mut handle = spawn_boxed_acp(
            &env,
            cache.path(),
            &staged,
            &seat_home,
            workspace.path(),
            &[],
            &[(
                "BAKERY_FAKE_ACP_WRITE_FILE".to_string(),
                sidecar.to_string_lossy().into_owned(),
            )],
            &[],
            true,
            &[],
            &[],
            &[],
            None,
            &[],
        )
        .unwrap();

        let text = prompt_notification_text(&mut handle);
        handle.kill_and_reap();
        assert!(
            text.contains("write denied"),
            "child must not write beside its staged executable: {text:?}"
        );
        assert_eq!(std::fs::read(&sidecar).unwrap(), b"unchanged");
    }

    // ─── S0 canary suite (BAKER-NATIVE-CHAT-CODE-02, Gate A) ────────────────
    //
    // Red-before / green-after evidence for the native credential boundary.
    // The fixture mirrors production exactly: the credential root is
    // `<cache_root>/claude-native/config` and is handed to the spawn through
    // `extra_rw_dirs` — the parameter `claude_native::acp_spawn_config` fills
    // via `credential_dirs` today. Every marker is fixture text planted in a
    // tempdir. Nothing here touches an operator home, a Keychain, or a real
    // credential.
    //
    // Credential-shaped names are denied after the directory grant, so a
    // root handed in through `extra_rw_dirs` is still unreadable. The
    // assertions were not weakened to get here.
    //
    // Non-ignored `s0_` tests are properties that already hold, plus harness
    // controls: a denial only means something if the same hook demonstrably
    // CAN read an ordinary project file.
    //
    // Scope limit, stated so no one over-reads a green result later: the fake
    // agent performs file access itself. It proves what the KERNEL profile
    // allows a native process tree. It cannot prove what the real Claude
    // engine's in-process tool layer does; that needs the exact released
    // engine (see the S0 evidence doc).

    struct CanaryRoom {
        cache: tempfile::TempDir,
        workspace: tempfile::TempDir,
        credential_root: PathBuf,
        credential_file: PathBuf,
        hook_config: PathBuf,
        marker: String,
    }

    const BENIGN_HOOK_CONFIG: &[u8] = b"{\"fixture\":\"benign\"}";

    impl CanaryRoom {
        fn new(tag: &str) -> Self {
            let cache = tempfile::tempdir().unwrap();
            let workspace = tempfile::tempdir().unwrap();
            let credential_root = cache.path().join("claude-native").join("config");
            std::fs::create_dir_all(&credential_root).unwrap();
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let marker = format!("S0-CANARY-{tag}-{}-{nonce}", std::process::id());
            let credential_file = credential_root.join(".credentials.json");
            std::fs::write(&credential_file, format!("{{\"fixture\":\"{marker}\"}}")).unwrap();
            // Loaded by the engine at the NEXT session start, so a write here
            // persists attacker-chosen hooks, MCP servers, or allow rules.
            let hook_config = credential_root.join("settings.json");
            std::fs::write(&hook_config, BENIGN_HOOK_CONFIG).unwrap();
            Self {
                cache,
                workspace,
                credential_root,
                credential_file,
                hook_config,
                marker,
            }
        }

        fn seat_home(&self, stream: &str, seat: &str) -> PathBuf {
            seat_acp_home(self.cache.path(), stream, seat)
        }

        /// Spawn exactly as production spawns claude-native today: the
        /// credential root granted read AND write to the whole native tree.
        fn spawn(&self, stream: &str, seat: &str, hook: &str, value: String) -> BoxedAcpHandle {
            self.spawn_with_grant(stream, seat, hook, value, true)
        }

        fn spawn_with_grant(
            &self,
            stream: &str,
            seat: &str,
            hook: &str,
            value: String,
            granted: bool,
        ) -> BoxedAcpHandle {
            let env = provisioned(self.cache.path(), self.workspace.path(), stream);
            let staged = stage_acp_binary(&fake_acp_agent_binary(), self.cache.path()).unwrap();
            let seat_home = self.seat_home(stream, seat);
            let grant = if granted {
                vec![self.credential_root.clone()]
            } else {
                Vec::new()
            };
            // As production keeps them: Claude's settings read-only.
            let read_only = if granted { claude_settings(&self.credential_root) } else { Vec::new() };
            spawn_boxed_acp(
                &env,
                self.cache.path(),
                &staged,
                &seat_home,
                self.workspace.path(),
                &[],
                &[(hook.to_string(), value)],
                &[],
                true,
                &[],
                &grant,
                &[],
                None,
                &read_only,
            )
            .unwrap()
        }
    }

    fn canary_run(mut handle: BoxedAcpHandle) -> String {
        let text = prompt_notification_text(&mut handle);
        handle.kill_and_reap();
        text
    }

    fn path_arg(path: &Path) -> String {
        path.to_string_lossy().into_owned()
    }

    // ── RED today: model-directed access to the credential root ─────────────

    #[test]
    fn s0_canary_inprocess_read_of_credential_root_is_denied() {
        let room = CanaryRoom::new("inproc-read");
        let text = canary_run(room.spawn(
            "wt:s0-read",
            "orchestrator",
            "BAKERY_FAKE_ACP_READ_FILE",
            path_arg(&room.credential_file),
        ));
        assert!(
            !text.contains(&room.marker),
            "in-process read reached the credential canary: {text:?}"
        );
        assert!(text.contains("read denied"), "{text:?}");
    }

    #[test]
    fn s0_canary_inprocess_write_of_credential_root_is_denied() {
        let room = CanaryRoom::new("inproc-write");
        let text = canary_run(room.spawn(
            "wt:s0-write",
            "orchestrator",
            "BAKERY_FAKE_ACP_WRITE_FILE",
            path_arg(&room.credential_file),
        ));
        let after = std::fs::read_to_string(&room.credential_file).unwrap();
        assert!(
            after.contains(&room.marker),
            "in-process write replaced the credential: {text:?}"
        );
        assert!(text.contains("write denied"), "{text:?}");
    }

    #[test]
    fn s0_canary_config_hook_tamper_is_denied() {
        let room = CanaryRoom::new("hook-tamper");
        let text = canary_run(room.spawn(
            "wt:s0-hook",
            "orchestrator",
            "BAKERY_FAKE_ACP_WRITE_FILE",
            path_arg(&room.hook_config),
        ));
        assert_eq!(
            std::fs::read(&room.hook_config).unwrap(),
            BENIGN_HOOK_CONFIG,
            "hook/config file loaded by the next session was rewritten: {text:?}"
        );
    }

    /// Parity: every other native runtime (Codex, Gemini, Grok Build…) gets
    /// its sign-in by a copy of its config folder into the seat's own home,
    /// which the whole runtime and its commands may read and write.
    #[test]
    fn s0_canary_shell_subprocess_cannot_read_a_copied_runtime_sign_in() {
        let room = CanaryRoom::new("copied-sign-in");
        let copied = room.seat_home("wt:s0-copied", "orchestrator").join(".codex").join("auth.json");
        std::fs::create_dir_all(copied.parent().unwrap()).unwrap();
        std::fs::write(&copied, format!("{{\"fixture\":\"{}\"}}", room.marker)).unwrap();
        let text = canary_run(room.spawn_with_grant(
            "wt:s0-copied",
            "orchestrator",
            "BAKERY_FAKE_ACP_EXEC",
            "cat \"$HOME/.codex/auth.json\"".to_string(),
            false,
        ));
        assert!(!text.contains(&room.marker), "a shell child read the copied sign-in: {text:?}");
    }

    #[test]
    fn s0_canary_shell_subprocess_cannot_read_credential_root() {
        let room = CanaryRoom::new("shell-read");
        let text = canary_run(room.spawn(
            "wt:s0-shell",
            "orchestrator",
            "BAKERY_FAKE_ACP_EXEC",
            format!("/bin/cat '{}'", room.credential_file.display()),
        ));
        assert!(
            text.starts_with("exec "),
            "shell hook did not run: {text:?}"
        );
        assert!(
            !text.contains(&room.marker),
            "shell subprocess read the credential canary: {text:?}"
        );
    }

    #[test]
    fn s0_canary_nested_subprocess_cannot_read_credential_root() {
        let room = CanaryRoom::new("nested-read");
        let text = canary_run(room.spawn(
            "wt:s0-nested",
            "orchestrator",
            "BAKERY_FAKE_ACP_EXEC",
            format!(
                "/bin/sh -c \"/bin/sh -c '/bin/cat {}'\"",
                room.credential_file.display()
            ),
        ));
        assert!(
            text.starts_with("exec "),
            "shell hook did not run: {text:?}"
        );
        assert!(
            !text.contains(&room.marker),
            "nested subprocess read the credential canary: {text:?}"
        );
    }

    #[test]
    fn s0_canary_shell_subprocess_cannot_tamper_hook_config() {
        let room = CanaryRoom::new("shell-tamper");
        let text = canary_run(room.spawn(
            "wt:s0-shell-tamper",
            "orchestrator",
            "BAKERY_FAKE_ACP_EXEC",
            format!("printf tampered > '{}'", room.hook_config.display()),
        ));
        assert_eq!(
            std::fs::read(&room.hook_config).unwrap(),
            BENIGN_HOOK_CONFIG,
            "shell subprocess rewrote next-session hook/config: {text:?}"
        );
    }

    #[test]
    fn s0_canary_workspace_symlink_cannot_reach_credential_root() {
        let room = CanaryRoom::new("symlink");
        let link = room.workspace.path().join("innocent-looking.json");
        std::os::unix::fs::symlink(&room.credential_file, &link).unwrap();
        let text = canary_run(room.spawn(
            "wt:s0-link",
            "orchestrator",
            "BAKERY_FAKE_ACP_READ_FILE",
            path_arg(&link),
        ));
        assert!(
            !text.contains(&room.marker),
            "workspace symlink reached the credential canary: {text:?}"
        );
    }

    #[test]
    fn s0_canary_relative_traversal_from_seat_home_cannot_reach_credential_root() {
        let room = CanaryRoom::new("traversal");
        // seat_home = <cache>/seats/<stream>/<seat>/home — four levels below <cache>.
        let traversal = room
            .seat_home("wt:s0-trav", "orchestrator")
            .join("../../../../claude-native/config/.credentials.json");
        let text = canary_run(room.spawn(
            "wt:s0-trav",
            "orchestrator",
            "BAKERY_FAKE_ACP_READ_FILE",
            path_arg(&traversal),
        ));
        assert!(
            !text.contains(&room.marker),
            "relative traversal reached the credential canary: {text:?}"
        );
    }

    #[test]
    fn s0_canary_one_session_cannot_tamper_another_sessions_credential_root() {
        let room = CanaryRoom::new("cross-session");
        let _ = canary_run(room.spawn(
            "wt:s0-session-a",
            "orchestrator",
            "BAKERY_FAKE_ACP_WRITE_FILE",
            path_arg(&room.hook_config),
        ));
        let seen_by_b = canary_run(room.spawn(
            "wt:s0-session-b",
            "worker",
            "BAKERY_FAKE_ACP_READ_FILE",
            path_arg(&room.hook_config),
        ));
        assert!(
            !seen_by_b.contains("rewrite-attempt"),
            "session B observed session A's write into shared credential/config state: {seen_by_b:?}"
        );
    }

    // ── GREEN today: properties that already hold, plus harness controls ────

    /// Harness control: the in-process hook CAN read an ordinary project file,
    /// so every denial above is a real denial, not a broken hook. This is also
    /// the "ordinary coding tools keep working" floor the boundary must keep.
    #[test]
    fn s0_control_inprocess_hook_reads_a_planted_workspace_file() {
        let room = CanaryRoom::new("harness-read");
        let planted = room.workspace.path().join("notes.txt");
        std::fs::write(&planted, &room.marker).unwrap();
        let text = canary_run(room.spawn(
            "wt:s0-harness",
            "orchestrator",
            "BAKERY_FAKE_ACP_READ_FILE",
            path_arg(&planted),
        ));
        assert!(
            text.contains(&room.marker),
            "hook could not read an ordinary project file: {text:?}"
        );
    }

    /// Harness control for the shell hook, and the same working-tools floor.
    #[test]
    fn s0_control_shell_hook_reads_a_planted_workspace_file() {
        let room = CanaryRoom::new("harness-shell");
        let planted = room.workspace.path().join("notes.txt");
        std::fs::write(&planted, &room.marker).unwrap();
        let text = canary_run(room.spawn(
            "wt:s0-harness-shell",
            "orchestrator",
            "BAKERY_FAKE_ACP_EXEC",
            format!("/bin/cat '{}'", planted.display()),
        ));
        assert!(
            text.starts_with("exec status=0"),
            "shell hook failed: {text:?}"
        );
        assert!(
            text.contains(&room.marker),
            "shell child could not read an ordinary project file: {text:?}"
        );
    }

    /// The grant is the ONLY thing exposing the root: without it the
    /// deny-closed profile already refuses. So a boundary that stops handing
    /// the root to the sandboxed tree must deliver authentication some other
    /// way — deleting the grant alone would just break sign-in.
    #[test]
    fn s0_control_without_the_grant_the_credential_root_is_unreadable() {
        let room = CanaryRoom::new("no-grant");
        let text = canary_run(room.spawn_with_grant(
            "wt:s0-nogrant",
            "orchestrator",
            "BAKERY_FAKE_ACP_READ_FILE",
            path_arg(&room.credential_file),
            false,
        ));
        assert!(!text.contains(&room.marker), "{text:?}");
        assert!(text.contains("read denied"), "{text:?}");
    }

    /// `env -i`: a secret in bakery-node's own environment never reaches a
    /// model-directed shell. `printenv` is used rather than dumping the whole
    /// environment so output clipping can never hide a leak.
    #[test]
    fn s0_host_env_secret_is_not_inherited_by_a_shell_subprocess() {
        let room = CanaryRoom::new("env");
        let var = format!("S0_HOST_CANARY_{}", std::process::id());
        std::env::set_var(&var, &room.marker);
        let text = canary_run(room.spawn(
            "wt:s0-env",
            "orchestrator",
            "BAKERY_FAKE_ACP_EXEC",
            format!("/usr/bin/printenv {var}; echo rc=$?"),
        ));
        std::env::remove_var(&var);
        assert!(
            text.starts_with("exec "),
            "shell hook did not run: {text:?}"
        );
        assert!(
            !text.contains(&room.marker),
            "host environment secret reached a shell child: {text:?}"
        );
        assert!(
            text.contains("rc=1"),
            "printenv should report the variable absent: {text:?}"
        );
    }

    /// Another seat's private HOME stays unreadable, in-process and from a
    /// shell child. This is the per-seat isolation the credential root lacks.
    #[test]
    fn s0_another_seats_home_is_unreadable_in_process_and_from_a_shell() {
        let room = CanaryRoom::new("other-seat");
        let other = room.seat_home("wt:s0-seats", "worker");
        std::fs::create_dir_all(&other).unwrap();
        let planted = other.join("session-state.json");
        std::fs::write(&planted, &room.marker).unwrap();

        let in_process = canary_run(room.spawn(
            "wt:s0-seats",
            "orchestrator",
            "BAKERY_FAKE_ACP_READ_FILE",
            path_arg(&planted),
        ));
        assert!(!in_process.contains(&room.marker), "{in_process:?}");

        let from_shell = canary_run(room.spawn(
            "wt:s0-seats",
            "orchestrator",
            "BAKERY_FAKE_ACP_EXEC",
            format!("/bin/cat '{}'", planted.display()),
        ));
        assert!(from_shell.starts_with("exec "), "{from_shell:?}");
        assert!(!from_shell.contains(&room.marker), "{from_shell:?}");
    }

    // ── Host full: the whole machine minus the deny list ────────────────────

    /// The cache stands in for the Bakery root, so the seat's own paths are
    /// carve-outs inside a denied tree, exactly as under `~/.bakery`. `home`
    /// stands in for the operator's, so no test ever touches the real one.
    struct HostFull {
        cache: tempfile::TempDir,
        workspace: tempfile::TempDir,
        home: tempfile::TempDir,
        elsewhere: tempfile::TempDir,
    }

    impl HostFull {
        fn new() -> Self {
            Self {
                cache: tempfile::tempdir().unwrap(),
                workspace: tempfile::tempdir().unwrap(),
                home: tempfile::tempdir().unwrap(),
                elsewhere: tempfile::tempdir().unwrap(),
            }
        }

        /// One shell command in the child; `None` runs it in today's room.
        fn run(&self, whole: bool, deny: &[PathBuf], script: String) -> String {
            self.run_with(whole, deny, &[], script)
        }

        /// [`Self::run`], with folders granted read and write.
        fn run_with(
            &self,
            whole: bool,
            deny: &[PathBuf],
            rw: &[PathBuf],
            script: String,
        ) -> String {
            self.run_env(whole, deny, rw, &[], ("BAKERY_FAKE_ACP_EXEC", script))
        }

        /// [`Self::run`] in today's room, with folders granted to read and
        /// run from, as a node room gets its tools' folders.
        fn run_tools(&self, tools: &[PathBuf], script: String) -> String {
            self.run_env(false, &[], &[], tools, ("BAKERY_FAKE_ACP_EXEC", script))
        }

        /// The fake agent's first reply, with one of its switches set.
        fn run_env(
            &self,
            whole: bool,
            deny: &[PathBuf],
            rw: &[PathBuf],
            tools: &[PathBuf],
            (switch, value): (&str, String),
        ) -> String {
            let env = provisioned(self.cache.path(), self.workspace.path(), "wt:full");
            let staged = stage_acp_binary(&fake_acp_agent_binary(), self.cache.path()).unwrap();
            let seat_home = seat_acp_home(self.cache.path(), "wt:full", "orchestrator");
            let machine = crate::WholeMachine {
                home: self.home.path().to_path_buf(),
                bakery_root: self.cache.path().to_path_buf(),
                path: "/usr/bin:/bin".to_string(),
            };
            canary_run(
                spawn_boxed_acp(
                    &env,
                    self.cache.path(),
                    &staged,
                    &seat_home,
                    self.workspace.path(),
                    deny,
                    &[(switch.to_string(), value)],
                    &[],
                    true,
                    tools,
                    rw,
                    tools,
                    whole.then_some(&machine),
                    &[],
                )
                .unwrap(),
            )
        }
    }

    fn probe(name: &str, command: String) -> String {
        format!("{command} >/dev/null 2>&1 && printf '{name}=yes ' || printf '{name}=no '; ")
    }

    fn plant(path: &Path, text: &str) -> PathBuf {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
        path.to_path_buf()
    }

    /// No agent room reaches the Keychain: inside each one, securityd can't
    /// even be looked up, while trustd (certificate checks) still answers.
    /// The probe is a bare mach lookup and reads nothing, so it never touches
    /// a Keychain item.
    #[test]
    fn no_agent_room_reaches_the_keychain() {
        const NAMES: &str = "com.apple.SecurityServer,com.apple.securityd.xpc,com.apple.trustd";
        let closed = |text: &str| {
            text.contains("com.apple.SecurityServer=denied")
                && text.contains("com.apple.securityd.xpc=denied")
                && text.contains("com.apple.trustd=reachable")
        };
        let outside = std::process::Command::new(fake_acp_agent_binary())
            .args(["--mach-probe", "com.apple.SecurityServer"])
            .output()
            .unwrap();
        let outside = String::from_utf8_lossy(&outside.stdout).into_owned();
        assert_eq!(outside.trim(), "com.apple.SecurityServer=reachable", "the probe sees securityd outside a room");

        let t = HostFull::new();
        for whole in [false, true] {
            let text = t.run_env(whole, &[], &[], &[], ("BAKERY_FAKE_ACP_MACH_PROBE", NAMES.to_string()));
            assert!(closed(&text), "a native runtime's room (Host full: {whole}): {text}");
        }

        // A command room (run, jobs), today's and Host full.
        let env = provisioned(t.cache.path(), t.workspace.path(), "wt:keychain");
        let staged = stage_acp_binary(&fake_acp_agent_binary(), t.cache.path()).unwrap();
        let exec = [staged.parent().unwrap().to_path_buf()];
        let argv = [staged.display().to_string(), "--mach-probe".to_string(), NAMES.to_string()];
        let machine = crate::WholeMachine {
            home: t.home.path().to_path_buf(),
            bakery_root: t.cache.path().to_path_buf(),
            path: "/usr/bin:/bin".to_string(),
        };
        for whole in [None, Some(&machine)] {
            let grants = crate::attach::ExtraGrants { exec: &exec, whole_machine: whole, ..Default::default() };
            let out = crate::attach::attach_exec_granted(&env, &argv, None, grants, std::time::Duration::from_secs(30)).unwrap();
            assert!(closed(&out.stdout), "a command room (Host full: {}): {} {}", whole.is_some(), out.stdout, out.stderr);
        }
    }

    /// A native agent reads and writes a package's certificate bundle; a key
    /// at the project's root and a secret inside a package folder stay
    /// blocked.
    #[test]
    fn a_native_room_reads_and_writes_package_certificates_only() {
        let t = HostFull::new();
        plant(&t.workspace.path().join("id.pem"), "key");
        let ws = t.workspace.path().display().to_string();
        let pip = format!("{ws}/.venv/lib/python3.12/site-packages/pip/_vendor/certifi");
        let grpc = format!("{ws}/.venv/lib/python3.12/site-packages/grpc/_cython/_credentials");
        let script = [
            format!("mkdir -p '{pip}' '{grpc}'; "),
            probe("pip", format!("printf cert > '{pip}/cacert.pem' && cat '{pip}/cacert.pem'")),
            probe("grpc", format!("printf cert > '{grpc}/roots.pem' && cat '{grpc}/roots.pem'")),
            probe("key", format!("cat '{ws}/id.pem'")),
            probe("env", format!("printf A=1 > '{pip}/.env'")),
        ]
        .concat();
        let text = t.run(false, &[], script);
        assert!(text.contains("pip=yes grpc=yes key=no env=no"), "{text}");
    }

    /// A room reads and writes a `.pem` in its own temp folder and a folder
    /// inside it (pip's copy of the CA bundle); one elsewhere in the seat
    /// home, in a `tmp` folder of the workspace, and a key in the workspace
    /// stay blocked.
    #[test]
    fn a_native_room_keeps_certificates_in_its_own_temp_folder_only() {
        let t = HostFull::new();
        plant(&t.workspace.path().join("id.pem"), "key");
        let ws = t.workspace.path().display().to_string();
        let script = [
            "mkdir -p \"$TMPDIR/build/certifi\" \"$HOME/certs\"; ".to_string(),
            format!("mkdir -p '{ws}/tmp'; "),
            probe("tmp", "printf cert > \"$TMPDIR/tmpab12cacert.pem\" && cat \"$TMPDIR/tmpab12cacert.pem\"".to_string()),
            probe("sub", "printf cert > \"$TMPDIR/build/certifi/cacert.pem\" && cat \"$TMPDIR/build/certifi/cacert.pem\"".to_string()),
            probe("home", "printf cert > \"$HOME/certs/x.pem\"".to_string()),
            probe("wstmp", format!("printf cert > '{ws}/tmp/x.pem'")),
            probe("key", format!("cat '{ws}/id.pem'")),
        ]
        .concat();
        let text = t.run(false, &[], script);
        assert!(text.contains("tmp=yes sub=yes home=no wstmp=no key=no"), "{text}");
    }

    /// A real python3, found outside the room as a node room finds its
    /// tools: the operator's own on PATH (mise's installs, Homebrew) first,
    /// then Xcode's real tools folder. Never `/usr/bin/python3`, a stand-in
    /// that asks xcrun for a cache in the host's temp folder, which a room
    /// can't write; nor a version manager's shim, which runs the manager.
    fn real_python3() -> Option<PathBuf> {
        let on_path: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
        let developer = std::process::Command::new("xcode-select")
            .arg("-p")
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()));
        on_path
            .into_iter()
            .filter(|dir| dir != Path::new("/usr/bin") && !dir.ends_with("shims"))
            .chain(developer.map(|dir| dir.join("usr/bin")))
            .map(|dir| dir.join("python3"))
            .find(|python| python.is_file())
    }

    /// A new venv gets its pip in a native room, too.
    #[test]
    fn a_new_venv_gets_its_pip_in_a_native_room() {
        let Some(python) = real_python3() else {
            eprintln!("skipped: no python3 on this Mac but /usr/bin's stand-in (install one with mise or Homebrew, or Xcode)");
            return;
        };
        // The room gets the folder python3 is installed in (its `bin` and
        // its standard library), to read and run from.
        // By its real path: the room may not read the links on the way.
        let python = python.canonicalize().unwrap();
        let install = python.parent().and_then(Path::parent).unwrap().to_path_buf();
        let t = HostFull::new();
        let ws = t.workspace.path().display().to_string();
        let python = python.display();
        let text = t.run_tools(&[install], format!("cd '{ws}' && '{python}' -m venv .venv && .venv/bin/python -m pip --version"));
        assert!(text.contains("exec status=0") && text.contains("stdout=pip "), "{python}: {text}");
    }

    /// A native agent's tools run what it builds and installs, in its
    /// workspace and its seat's home, never a program from a folder it may
    /// only read; a variable naming the room's home gets the seat's.
    #[test]
    fn a_native_agent_runs_what_it_builds_in_its_own_room() {
        let (cache, workspace, elsewhere) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let outside = plant(&elsewhere.path().join("tool.sh"), "#!/bin/sh\nexit 0\n");
        std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o755)).unwrap();
        let script = [
            "printf '#!/bin/sh\\nexit 0\\n' > s.sh; chmod +x s.sh; printf 'int main(void){return 0;}\\n' > t.c; ".to_string(),
            probe("script", "./s.sh".to_string()),
            probe("built", "cc -o t t.c && ./t".to_string()),
            probe("home", r#"cp t "$HOME/t" && "$HOME/t""#.to_string()),
            probe("outside", format!("'{}'", outside.display())),
            probe("own", r#"test "$CARGO_HOME" = "$HOME/.cargo""#.to_string()),
        ]
        .concat();
        let env = provisioned(cache.path(), workspace.path(), "wt:builds");
        let staged = stage_acp_binary(&fake_acp_agent_binary(), cache.path()).unwrap();
        let seat_home = seat_acp_home(cache.path(), "wt:builds", "orchestrator");
        let vars = [
            ("BAKERY_FAKE_ACP_EXEC".to_string(), format!("cd '{}' && {script}", workspace.path().display())),
            ("CARGO_HOME".to_string(), format!("{ROOM_HOME_ENV_VALUE}/.cargo")),
        ];
        let read = [elsewhere.path().to_path_buf()];
        let handle =
            spawn_boxed_acp(&env, cache.path(), &staged, &seat_home, workspace.path(), &[], &vars, &[], true, &[], &[], &read, None, &[])
                .unwrap();
        let text = canary_run(handle);
        assert!(text.contains("script=yes built=yes home=yes outside=no own=yes"), "{text}");
    }

    /// A native runtime's room runs the real git behind /usr/bin's shim, as a
    /// command room does: no agent spends its turn hunting for git.
    #[test]
    fn a_native_room_runs_the_real_git() {
        let t = HostFull::new();
        let text = t.run(false, &[], "git --version".to_string());
        assert!(text.contains("git version"), "{text}");
    }

    #[test]
    fn host_full_reaches_the_machine_but_not_the_bakery_root_or_the_deny_list() {
        let t = HostFull::new();
        let outside = plant(&t.elsewhere.path().join("notes.txt"), "reachable");
        let private = t.elsewhere.path().join("private");
        let secret = plant(&private.join("notes.txt"), "secret");
        let state = plant(&t.cache.path().join("state/notes.txt"), "bakery's");
        let other_seat = plant(
            &seat_acp_home(t.cache.path(), "wt:full", "worker").join("notes.txt"),
            "worker's",
        );
        let script = [
            probe("outside", format!("cat '{}'", outside.display())),
            probe("bakery", format!("cat '{}'", state.display())),
            probe("seat", format!("cat '{}'", other_seat.display())),
            probe("deny", format!("cat '{}'", secret.display())),
            probe(
                "denyw",
                format!("touch '{}'", private.join("new").display()),
            ),
            probe("home", "touch \"$HOME/made\"".to_string()),
            probe(
                "ws",
                format!("touch '{}'", t.workspace.path().join("built").display()),
            ),
        ]
        .concat();
        let text = t.run(true, std::slice::from_ref(&private), script);
        assert!(
            text.contains("outside=yes bakery=no seat=no deny=no denyw=no home=yes ws=yes"),
            "{text}"
        );
    }

    #[test]
    fn host_full_cannot_change_what_runs_later() {
        let t = HostFull::new();
        let h = t.home.path();
        let zshrc = plant(&h.join(".zshrc"), "");
        std::fs::create_dir_all(h.join("Library/LaunchAgents")).unwrap();
        let repo = t.elsewhere.path().join("repo");
        std::fs::create_dir_all(repo.join(".git/hooks")).unwrap();
        plant(&repo.join(".git/config"), "");
        let script = [
            probe("zshrc", format!("echo x >> '{}'", zshrc.display())),
            probe(
                "agent",
                format!(
                    "touch '{}'",
                    h.join("Library/LaunchAgents/x.plist").display()
                ),
            ),
            probe(
                "hook",
                format!("touch '{}'", repo.join(".git/hooks/pre-commit").display()),
            ),
            probe(
                "gitcfg",
                format!("echo x >> '{}'", repo.join(".git/config").display()),
            ),
            probe("repo", format!("touch '{}'", repo.join("file").display())),
            probe("homefile", format!("touch '{}'", h.join("notes").display())),
        ]
        .concat();
        let text = t.run(true, &[], script);
        assert!(
            text.contains("zshrc=no agent=no hook=no gitcfg=no repo=yes homefile=yes"),
            "{text}"
        );
        assert_eq!(std::fs::read_to_string(&zshrc).unwrap(), "");
    }

    /// The operator's own git works in a linked worktree later, so no level
    /// lets the agent repoint the worktree's `.git` file or its git folder's
    /// `commondir`, or move the worktree away and back. Git still works
    /// there in Host full, where the worktree's git folders are reachable
    /// (a room reaches none outside its workspace).
    #[test]
    fn no_level_repoints_or_moves_a_linked_worktree() {
        let git = |dir: &Path, args: &[&str]| {
            let out = std::process::Command::new("git")
                .current_dir(dir)
                .args(args)
                .output()
                .unwrap();
            let err = String::from_utf8_lossy(&out.stderr);
            assert!(out.status.success(), "git {args:?}: {err}");
        };
        let who = ["-c", "user.name=t", "-c", "user.email=t@t"];
        for whole in [false, true] {
            let t = HostFull::new();
            let repo = t.elsewhere.path().join("repo");
            std::fs::create_dir_all(&repo).unwrap();
            git(&repo, &["init", "-q", "-b", "main"]);
            plant(&repo.join("a.txt"), "a");
            git(&repo, &["add", "a.txt"]);
            git(&repo, &[&who[..], &["commit", "-q", "-m", "a"]].concat());
            let ws = t.workspace.path();
            git(
                &repo,
                &["worktree", "add", "-q", "-b", "w", ws.to_str().unwrap()],
            );
            let pointer = ws.join(".git");
            let before = std::fs::read_to_string(&pointer).unwrap();
            let own = crate::attach::canon(Path::new(
                before.trim().strip_prefix("gitdir:").unwrap().trim(),
            ));
            let moved = t.elsewhere.path().join("moved");
            let in_ws = |args: &str| {
                format!(
                    "git -C '{}' {who} {args}",
                    ws.display(),
                    who = who.join(" ")
                )
            };
            let script = [
                probe(
                    "pointer",
                    format!("echo 'gitdir: /tmp/evil' > '{}'", pointer.display()),
                ),
                probe("unlink", format!("rm -f '{}'", pointer.display())),
                probe(
                    "move",
                    format!("mv '{}' '{}'", ws.display(), moved.display()),
                ),
                probe(
                    "commondir",
                    format!("echo /tmp/evil > '{}'", own.join("commondir").display()),
                ),
                probe("work", format!("touch '{}'", ws.join("b.txt").display())),
                probe("status", in_ws("status --porcelain")),
                probe(
                    "commit",
                    format!("{} && {}", in_ws("add b.txt"), in_ws("commit -q -m b")),
                ),
            ]
            .concat();
            let text = t.run(whole, &[], script);
            let held = "pointer=no unlink=no move=no commondir=no work=yes";
            assert!(text.contains(held), "Host full {whole}: {text}");
            if whole {
                assert!(text.contains("status=yes commit=yes"), "{text}");
            }
            assert_eq!(std::fs::read_to_string(&pointer).unwrap(), before);
            assert!(!moved.exists());
        }
    }

    /// An agent starts repositories inside its own workspace (`git init`,
    /// `cargo new`) at every level, while git's settings and hooks stay
    /// closed everywhere else: in the workspace's own repository, there when
    /// the runtime started, and in the operator's, granted or not.
    #[test]
    fn repositories_start_in_the_workspace_and_the_operators_stay_closed() {
        let git = |dir: &Path, args: &[&str]| {
            let out = std::process::Command::new("git")
                .current_dir(dir)
                .args(args)
                .output()
                .unwrap();
            let err = String::from_utf8_lossy(&out.stderr);
            assert!(out.status.success(), "git {args:?}: {err}");
        };
        let cargo = std::env::var("CARGO").unwrap();
        for whole in [false, true] {
            let t = HostFull::new();
            let ws = t.workspace.path();
            git(ws, &["init", "-q"]);
            let theirs = t.elsewhere.path().join("theirs");
            std::fs::create_dir_all(&theirs).unwrap();
            git(&theirs, &["init", "-q"]);
            let at = |p: &Path, rel: &str| p.join(rel).display().to_string();
            let mut script = vec![
                probe("own_config", format!("echo x >> '{}'", at(ws, ".git/config"))),
                probe("own_hook", format!("touch '{}'", at(ws, ".git/hooks/pre-commit"))),
                probe(
                    "their_config",
                    format!("echo x >> '{}'", at(&theirs, ".git/config")),
                ),
                probe(
                    "their_hook",
                    format!("touch '{}'", at(&theirs, ".git/hooks/pre-commit")),
                ),
                probe("their_file", format!("touch '{}'", at(&theirs, "notes"))),
                // A new repository's settings and hooks, as git init writes them.
                probe(
                    "new_repo",
                    format!(
                        "mkdir -p '{0}/.git/hooks' && echo x > '{0}/.git/config' && touch '{0}/.git/hooks/pre-commit'",
                        at(ws, "scratch")
                    ),
                ),
            ];
            if whole {
                script.push(probe(
                    "git_init",
                    format!("git init -q '{}'", at(ws, "fresh")),
                ));
                script.push(probe(
                    "cargo_new",
                    format!("'{cargo}' new -q --vcs git '{}'", at(ws, "newcrate")),
                ));
            }
            let text = t.run_with(whole, &[], std::slice::from_ref(&theirs), script.concat());
            let held = "own_config=no own_hook=no their_config=no their_hook=no their_file=yes new_repo=yes";
            assert!(text.contains(held), "Host full {whole}: {text}");
            if whole {
                assert!(text.contains("git_init=yes cargo_new=yes"), "{text}");
                assert!(ws.join("fresh/.git/config").is_file());
                assert!(ws.join("newcrate/.git/config").is_file());
            }
        }
    }

    /// Whoever reaches a Docker socket can mount around every rule, so no
    /// level reaches one; any other socket stays reachable in Host full.
    #[test]
    fn no_level_reaches_a_docker_socket() {
        use std::os::unix::net::UnixListener;
        let t = HostFull::new();
        let run = t.elsewhere.path().join("run");
        std::fs::create_dir_all(&run).unwrap();
        let docker = UnixListener::bind(run.join("docker.sock")).unwrap();
        let other = UnixListener::bind(run.join("other.sock")).unwrap();
        docker.set_nonblocking(true).unwrap();
        other.set_nonblocking(true).unwrap();
        let connect = |name: &str| {
            format!(
                "/usr/bin/nc -U -w 1 '{}' </dev/null; ",
                run.join(name).display()
            )
        };
        for whole in [true, false] {
            let _ = t.run(whole, &[], connect("docker.sock") + &connect("other.sock"));
            assert!(
                docker.accept().is_err(),
                "whole={whole}: reached docker.sock"
            );
            assert!(
                other.accept().is_ok(),
                "whole={whole}: control socket unreachable"
            );
        }
    }
}
