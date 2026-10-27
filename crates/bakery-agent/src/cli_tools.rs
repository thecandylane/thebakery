//! Codex / Cursor CLI account detection (07d-A3D1 Phase A).
//!
//! Neither tool has an OAuth client Bakery implements itself — each already
//! ships its own interactive `login` command and a non-interactive status
//! check (`codex login status`, `cursor-agent status`) that a human runs, or
//! now Bakery runs on their behalf. This module only ever reads an exit
//! code, never token/credential file contents, so unlike
//! [`crate::claude_code_slot`] it needs no private/operator-local seam — it
//! lives entirely in the public tree.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Which CLI's interactive login to run — a closed set, never a free-form
/// argv, so there is no command-injection surface no matter what a caller
/// sends over the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LoginCommand {
    #[serde(rename = "codex-login")]
    Codex,
    #[serde(rename = "cursor-login")]
    Cursor,
    /// Pinned Claude ACP adapter. The concrete Node executable and package
    /// entry are resolved by bakery-node, which owns adapter installation;
    /// this enum carries identity only.
    #[serde(rename = "claude-native-login")]
    ClaudeNative,
}

impl LoginCommand {
    /// The one true name a system login terminal for this command is ever
    /// given — derived here, server-side, from the terminal's actual
    /// identity, never taken from a request body. A terminal's `name` field
    /// is otherwise free-form/renameable and must never be trusted to prove
    /// *which* login command a session is really running (07d-A3D1 review
    /// finding 1: a Cursor session named "codex-login" must not be reused as
    /// Codex — identity is `LoginCommand`, name is cosmetic only).
    pub fn canonical_name(&self) -> &'static str {
        match self {
            LoginCommand::Codex => "codex-login",
            LoginCommand::Cursor => "cursor-login",
            LoginCommand::ClaudeNative => "claude-native-login",
        }
    }
}

/// Program + args for one interactive login — resolved entirely server-side
/// from the closed [`LoginCommand`] enum, never taken from a request body.
pub fn login_argv(cmd: LoginCommand) -> Result<(PathBuf, Vec<String>), &'static str> {
    match cmd {
        LoginCommand::Codex => Ok((codex_bin(), vec!["login".into()])),
        LoginCommand::Cursor => Ok((cursor_bin(), vec!["login".into()])),
        LoginCommand::ClaudeNative => Err("Claude native login argv is resolved by bakery-node's pinned adapter registry"),
    }
}

/// `installed`: the binary resolved and spawned at all. `linked`: its own
/// status subcommand exited 0. `installed == false` implies `linked == false`
/// — an absent binary is never "linked".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliAccountStatus {
    pub installed: bool,
    pub linked: bool,
}

const NOT_LINKED: CliAccountStatus = CliAccountStatus { installed: true, linked: false };
const NOT_INSTALLED: CliAccountStatus = CliAccountStatus { installed: false, linked: false };

const CACHE_TTL: Duration = Duration::from_secs(10);
// Bounds worst case `/auth/cli-status` latency for a hung binary. Shortened
// under test so the hang-times-out case stays fast; real tuning is a
// follow-up once this runs against real Codex/Cursor installs (07d-A3D1
// plan, "Risks / follow-ups").
#[cfg(not(test))]
const STATUS_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(test)]
const STATUS_TIMEOUT: Duration = Duration::from_millis(300);

static CODEX_CACHE: Mutex<Option<(Instant, CliAccountStatus)>> = Mutex::new(None);
static CURSOR_CACHE: Mutex<Option<(Instant, CliAccountStatus)>> = Mutex::new(None);

/// `codex login status` — exit 0 means signed in. This contract is
/// source-confirmed, not assumed: `openai/codex`'s
/// `codex-rs/cli/src/login.rs::run_login_status` calls
/// `std::process::exit(0)` on every authenticated branch (API key, ChatGPT,
/// access token, personal access token, Bedrock) and `std::process::exit(1)`
/// on `Ok(None)` ("Not logged in") and on error — never anything else.
/// `force` bypasses the 10s cache (used once, right after a login
/// terminal's exit code arrives, so the row updates immediately instead of
/// waiting out the TTL).
pub fn codex_status(force: bool) -> CliAccountStatus {
    cached(&CODEX_CACHE, force, || probe(&codex_bin(), &["login", "status"]))
}

/// `cursor-agent status` — exit 0 is *assumed* to mean signed in, but this
/// is **not** a confirmed contract (07d-A3D1 review finding 2): Cursor's own
/// docs (cursor.com/docs/cli/reference/authentication) describe `status`'s
/// human-readable output only and document no exit-code semantics at all,
/// and this CLI has a forum-confirmed history of a *different* subcommand
/// (`cursor-agent update`) exiting nonzero on success in past builds (fixed
/// in newer ones) — so exit-code discipline here is not something to take
/// on faith. There is no real `cursor-agent` install in this environment to
/// verify against directly. Treat `linked` for Cursor as best-effort until
/// confirmed by hand against a real authenticated/unauthenticated install;
/// `installed` (whether the binary resolves at all) does not depend on this
/// assumption and stays trustworthy regardless.
pub fn cursor_status(force: bool) -> CliAccountStatus {
    cached(&CURSOR_CACHE, force, || probe(&cursor_bin(), &["status"]))
}

fn cached(
    cache: &Mutex<Option<(Instant, CliAccountStatus)>>,
    force: bool,
    probe_fn: impl FnOnce() -> CliAccountStatus,
) -> CliAccountStatus {
    let mut guard = cache.lock().unwrap_or_else(|e| e.into_inner());
    if !force {
        if let Some((at, status)) = *guard {
            if at.elapsed() < CACHE_TTL {
                return status;
            }
        }
    }
    let status = probe_fn();
    *guard = Some((Instant::now(), status));
    status
}

/// Spawn `bin args...` with no stdin (never blocks on input — this is the
/// non-interactive status check, not the login itself) and poll for exit
/// within `STATUS_TIMEOUT`. A spawn failure means not installed; a timeout
/// means it spawned (so it exists) but is hung or slow — never guess a
/// binary that hasn't answered is signed in.
fn probe(bin: &std::path::Path, args: &[&str]) -> CliAccountStatus {
    let mut child = match Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return NOT_INSTALLED,
    };

    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return CliAccountStatus { installed: true, linked: status.success() };
            }
            Ok(None) => {
                if started.elapsed() >= STATUS_TIMEOUT {
                    let _ = child.kill();
                    let _ = child.wait();
                    return NOT_LINKED;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => return NOT_LINKED,
        }
    }
}

fn codex_bin() -> PathBuf {
    #[cfg(any(test, feature = "test-util"))]
    if let Ok(p) = std::env::var("BAKERY_CODEX_BIN_FOR_TEST") {
        return PathBuf::from(p);
    }
    PathBuf::from("codex")
}

fn cursor_bin() -> PathBuf {
    #[cfg(any(test, feature = "test-util"))]
    if let Ok(p) = std::env::var("BAKERY_CURSOR_BIN_FOR_TEST") {
        return PathBuf::from(p);
    }
    PathBuf::from("cursor-agent")
}

/// Test-only env/cache isolation, shared by every caller across every crate
/// that links this one — `cargo test` runs test functions on multiple
/// threads within one process, so this crate's own tests below AND
/// bakery-node's route tests (which also point
/// `BAKERY_CODEX_BIN_FOR_TEST`/`BAKERY_CURSOR_BIN_FOR_TEST` at fixtures) must
/// serialize on the *same* lock — a lock private to one crate's test module
/// does not protect the other's tests from racing the same process-wide env
/// vars (07d-A3D1 review finding 6).
#[cfg(any(test, feature = "test-util"))]
pub mod test_support {
    use std::path::Path;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Runs `f` with either/both test-bin overrides set (only the `Some`
    /// ones are touched) and the corresponding status cache(s) cleared
    /// first, restoring the previous env value(s) afterward. Holds one
    /// process-wide lock for the whole call — callers must not nest two
    /// calls on the same thread (std `Mutex` is not reentrant).
    pub fn with_test_bins<T>(codex: Option<&Path>, cursor: Option<&Path>, f: impl FnOnce() -> T) -> T {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev_codex = codex.map(|p| {
            let prev = std::env::var("BAKERY_CODEX_BIN_FOR_TEST").ok();
            std::env::set_var("BAKERY_CODEX_BIN_FOR_TEST", p);
            prev
        });
        let prev_cursor = cursor.map(|p| {
            let prev = std::env::var("BAKERY_CURSOR_BIN_FOR_TEST").ok();
            std::env::set_var("BAKERY_CURSOR_BIN_FOR_TEST", p);
            prev
        });
        if codex.is_some() {
            *super::CODEX_CACHE.lock().unwrap_or_else(|e| e.into_inner()) = None;
        }
        if cursor.is_some() {
            *super::CURSOR_CACHE.lock().unwrap_or_else(|e| e.into_inner()) = None;
        }

        let out = f();

        if codex.is_some() {
            match prev_codex.flatten() {
                Some(v) => std::env::set_var("BAKERY_CODEX_BIN_FOR_TEST", v),
                None => std::env::remove_var("BAKERY_CODEX_BIN_FOR_TEST"),
            }
        }
        if cursor.is_some() {
            match prev_cursor.flatten() {
                Some(v) => std::env::set_var("BAKERY_CURSOR_BIN_FOR_TEST", v),
                None => std::env::remove_var("BAKERY_CURSOR_BIN_FOR_TEST"),
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    // This machine has a real `codex` on PATH (docs/CLI_CAPABILITY_MAP.md) —
    // every test must set an explicit override, including to a
    // deliberately-missing path, never rely on ambient PATH state either way.

    fn fixture_script(body: &str) -> tempfile::TempPath {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(f, "#!/bin/sh\n{body}\n").unwrap();
        let path = f.into_temp_path();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn with_codex_bin<T>(path: &std::path::Path, f: impl FnOnce() -> T) -> T {
        test_support::with_test_bins(Some(path), None, f)
    }

    #[test]
    fn not_installed_when_the_binary_does_not_resolve() {
        with_codex_bin(std::path::Path::new("/nonexistent/bin/codex-does-not-exist"), || {
            let s = codex_status(true);
            assert_eq!(s, CliAccountStatus { installed: false, linked: false });
        });
    }

    #[test]
    fn linked_when_status_exits_zero() {
        let script = fixture_script("exit 0");
        with_codex_bin(&script, || {
            let s = codex_status(true);
            assert_eq!(s, CliAccountStatus { installed: true, linked: true });
        });
    }

    #[test]
    fn installed_but_not_linked_when_status_exits_nonzero() {
        let script = fixture_script("exit 1");
        with_codex_bin(&script, || {
            let s = codex_status(true);
            assert_eq!(s, CliAccountStatus { installed: true, linked: false });
        });
    }

    #[test]
    fn a_hung_status_check_times_out_as_installed_not_linked() {
        let script = fixture_script("sleep 5");
        with_codex_bin(&script, || {
            let started = Instant::now();
            let s = codex_status(true);
            assert_eq!(s, NOT_LINKED);
            assert!(
                started.elapsed() < Duration::from_secs(2),
                "must not wait out the real 5s prod timeout in tests: {:?}",
                started.elapsed()
            );
        });
    }

    #[test]
    fn a_cache_hit_is_not_reprobed_until_forced() {
        let flip = fixture_script("exit 0");
        with_codex_bin(&flip, || {
            assert_eq!(codex_status(false), CliAccountStatus { installed: true, linked: true });
            // Same path, contents unreachable to change mid-test — instead
            // prove the cache path itself: a second non-forced call must not
            // reprobe (if it did, it would still see exit 0 anyway, so
            // assert directly on the cache's `fetched_at` staying put).
            let first_fetch = CODEX_CACHE.lock().unwrap().unwrap().0;
            let _ = codex_status(false);
            let second_fetch = CODEX_CACHE.lock().unwrap().unwrap().0;
            assert_eq!(first_fetch, second_fetch, "non-forced call reused the cached probe");
            let _ = codex_status(true);
            let third_fetch = CODEX_CACHE.lock().unwrap().unwrap().0;
            assert!(third_fetch > second_fetch, "force=true must reprobe");
        });
    }

    #[test]
    fn login_argv_resolves_the_test_override_and_has_no_extra_args() {
        with_codex_bin(std::path::Path::new("/tmp/fake-codex"), || {
            let (bin, args) = login_argv(LoginCommand::Codex).unwrap();
            assert_eq!(bin, std::path::PathBuf::from("/tmp/fake-codex"));
            assert_eq!(args, vec!["login".to_string()]);
        });
    }

    #[test]
    fn login_command_wire_values_are_kebab_case() {
        assert_eq!(serde_json::to_string(&LoginCommand::Codex).unwrap(), "\"codex-login\"");
        assert_eq!(serde_json::to_string(&LoginCommand::Cursor).unwrap(), "\"cursor-login\"");
        assert_eq!(serde_json::to_string(&LoginCommand::ClaudeNative).unwrap(), "\"claude-native-login\"");
    }

    #[test]
    fn canonical_name_matches_the_wire_value() {
        // Not load-bearing by accident — `terminal_create_system` derives a
        // login terminal's *displayed* name from this, independently of its
        // *identity* (the `LoginCommand` itself). They happen to read the
        // same today; that's convenience, not something callers should rely
        // on structurally.
        assert_eq!(LoginCommand::Codex.canonical_name(), "codex-login");
        assert_eq!(LoginCommand::Cursor.canonical_name(), "cursor-login");
        assert_eq!(LoginCommand::ClaudeNative.canonical_name(), "claude-native-login");
    }
}
