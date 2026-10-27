//! Human PTY and agent exec both attach through the same door: a deny-closed
//! Seatbelt (`sandbox-exec`) profile wrapped around a scrubbed `env -i`. No
//! shell string, no inherited host environment, no network, and no fallback
//! to a bare host process if Seatbelt itself is unavailable.

use crate::session::SessionEnvironment;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// A hung command must not hang the node — attach is killed and reported as
/// a failure past this. This is the one-shot `/env/exec`/`/env/pty` budget;
/// workspace-RW agent runs (builds, test suites) get their own, larger,
/// caller-clamped budget below.
const ATTACH_TIMEOUT: Duration = Duration::from_secs(30);
/// Default and ceiling for [`attach_exec_workspace_rw`] — an agent `run`
/// exists to build and test, and `cargo test`/`npm test` do not finish in
/// 30 seconds. A tiny timeout here was quietly doing containment's job
/// (verification structurally impossible); the sandbox is the containment,
/// the timeout is only a hang guard.
pub const DEFAULT_WORKSPACE_RUN_TIMEOUT: Duration = Duration::from_secs(300);
pub const MAX_WORKSPACE_RUN_TIMEOUT: Duration = Duration::from_secs(900);
/// Past this many captured bytes, output is truncated with a marker rather
/// than grown without bound.
const MAX_OUTPUT_BYTES: usize = 256 * 1024;

#[derive(Debug)]
pub struct AttachOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

fn home_dir() -> Option<PathBuf> {
    std::env::var("HOME")
        .ok()
        .filter(|h| !h.trim().is_empty())
        .map(PathBuf::from)
}

/// Named credential surfaces denied on every attach, on top of whatever a
/// caller adds via `extra_deny_read`. These are exact files/dirs, not a
/// blanket `~/.bakery` deny: under the deny-closed profile built by
/// `seatbelt_profile`, nothing outside the toolchain/runtime/workspace
/// subpaths is readable in the first place, so a blanket deny would add
/// nothing except the risk of shadowing those legitimate subpaths depending
/// on rule order. Naming the exact files instead makes them safe to list
/// *last* (see `seatbelt_profile`): even a workspace path misconfigured to
/// reach into `$HOME` still can't read these.
pub fn default_deny_paths() -> Vec<PathBuf> {
    let Some(home) = home_dir() else {
        return Vec::new();
    };
    let bakery = home.join(".bakery");
    // Old Hearts a fresh start set aside (`heart-<stamp>.db*`), as secret as
    // the live one. Set aside only as a node starts, before any room.
    let aside = hearts_aside(&bakery);
    let mut paths = vec![
        bakery.join("secrets.json"),
        // The operator's Claude subscription. The node reads it and swaps a
        // stand-in at the door. Host full is allow-by-default outside this
        // list, so the vault and the legacy credential file are named here.
        bakery.join("cache").join("claude-native").join("config").join(".credentials.json"),
        // Nothing writes here anymore. A file left from the short-lived
        // second vault stays unreadable.
        bakery.join("claude-native").join("account"),
        bakery.join("cache").join("claude-container").join("config").join(".credentials.json"),
        bakery.join("heart.db"),
        bakery.join("heart.db-wal"),
        bakery.join("heart.db-shm"),
        bakery.join("node.json"),
        // node.json as it was, kept for a fresh start's rollback.
        bakery.join("node.json.before-fresh-start"),
        // The door's certificate authority's key (bakery-node's swap_tls):
        // whatever signs with it is trusted by rooms that use a sign-in.
        bakery.join("swap"),
        bakery.join("swap").join("subscription-seed"),
        home.join(".ssh"),
        home.join(".claude"),
        home.join(".codex"),
        home.join(".gemini").join("oauth_creds.json"),
        // The host's other secret stores. Nothing here is readable without a
        // grant covering it, but an operator's machine-access grant on a
        // parent (`~/.cargo`, `~/.config`) must never reach these.
        home.join(".gnupg"),
        home.join(".aws"),
        home.join(".azure"),
        home.join(".kube"),
        home.join(".netrc"),
        home.join(".npmrc"),
        home.join(".pypirc"),
        home.join(".git-credentials"),
        home.join(".config").join("git").join("credentials"),
        home.join(".config").join("gh"),
        home.join(".config").join("gcloud"),
        home.join(".docker").join("config.json"),
        // Colima's and Lima's VM keys: ssh into the VM is root over every
        // folder it shares, around every rule here.
        home.join(".colima"),
        home.join(".lima"),
        home.join(".gem").join("credentials"),
        home.join(".cargo").join("credentials"),
        home.join(".cargo").join("credentials.toml"),
        home.join("Library").join("Keychains"),
    ];
    paths.extend(aside);
    paths
}

/// The Hearts a fresh start set aside in `bakery` (`heart-<stamp>.db*`).
fn hearts_aside(bakery: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(bakery)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name().and_then(|name| name.to_str()).is_some_and(|name| name.starts_with("heart-") && name.contains(".db"))
        })
        .collect()
}

pub(crate) fn quote(p: &Path) -> String {
    format!(
        "\"{}\"",
        p.to_string_lossy()
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
    )
}

/// The Apple developer directories a room may read/exec from: the stock
/// CommandLineTools location plus whatever `xcode-select` currently points
/// at (an Xcode.app bundle on machines with full Xcode). Resolved once,
/// host-side, at first profile build — `xcode-select -p` is the only
/// reliable source across macOS versions (the `/var/db/xcode_select_link`
/// symlink does not exist on newer releases).
pub fn apple_developer_dirs() -> &'static [PathBuf] {
    static DIRS: std::sync::OnceLock<Vec<PathBuf>> = std::sync::OnceLock::new();
    DIRS.get_or_init(|| {
        let mut dirs = vec![PathBuf::from("/Library/Developer")];
        if let Ok(out) = std::process::Command::new("/usr/bin/xcode-select")
            .arg("-p")
            .output()
        {
            if out.status.success() {
                let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !p.is_empty() {
                    let mut target = canon(Path::new(&p));
                    // A full-Xcode developer dir lives inside the app bundle
                    // (`/Applications/Xcode.app/Contents/Developer`), and its
                    // tools load frameworks from sibling bundle dirs
                    // (`Contents/SharedFrameworks`, …) — grant the bundle
                    // root, not just the Developer subtree.
                    if let Some(bundle) = target
                        .ancestors()
                        .find(|a| a.extension().and_then(|e| e.to_str()) == Some("app"))
                    {
                        target = bundle.to_path_buf();
                    }
                    if !dirs.contains(&target) {
                        dirs.push(target);
                    }
                }
            }
        }
        dirs
    })
}

/// The Seatbelt kernel check compares against the resolved (symlink-free)
/// path, not the string handed to `sandbox-exec` — on macOS both `/tmp` and
/// `$TMPDIR` are themselves symlinks into `/private/...`, so an uncanonicalized
/// path here would silently fail to match. Falls back to the given path when
/// it does not exist yet.
pub(crate) fn canon(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}

/// Escape a path for embedding in a Seatbelt `(regex #"...")` pattern (BSD
/// extended regex, not a plain string — a literal `.` in a temp dir name
/// would otherwise match any character).
fn regex_escape(p: &Path) -> String {
    let s = p.to_string_lossy();
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        if matches!(
            c,
            '.' | '+' | '*' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '^' | '$' | '|' | '\\'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// One glob shape from `credentials::CREDENTIAL_SHAPES` (`*` = any run of
/// characters, everything else literal), turned into a *case-insensitive*
/// BSD regex fragment: Seatbelt's regex dialect has no portable `(?i)`, so
/// each letter becomes an `[Aa]`-style character class instead — matching
/// `is_credential_filename`'s case-insensitive comparison exactly, letter
/// for letter, shape for shape, rather than a hand-copied second list that
/// could quietly drift from it.
pub(crate) fn shape_to_regex(shape: &str) -> String {
    let mut out = String::new();
    for c in shape.chars() {
        if c == '*' {
            out.push_str(".*");
        } else if c.is_ascii_alphabetic() {
            out.push('[');
            out.push(c.to_ascii_uppercase());
            out.push(c.to_ascii_lowercase());
            out.push(']');
        } else if matches!(
            c,
            '.' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '^' | '$' | '|' | '\\'
        ) {
            out.push('\\');
            out.push(c);
        } else {
            out.push(c);
        }
    }
    out
}

/// A package's certificate bundle under `workspace`
/// ([`crate::credentials::is_package_certificate`]), as a Seatbelt regex: a
/// rule laid after the credential deny lets it through.
pub(crate) fn package_certificate_regex(workspace: &Path) -> String {
    let base = regex_escape(workspace);
    let folders = crate::credentials::PACKAGE_FOLDERS.join("|");
    format!("^{base}/(.*/)?({folders})/(.*/)?[^/]*\\.{}$", shape_to_regex("pem"))
}

/// A `.pem` in a room's own temp folder (`seat_tmp`, the seat home's `tmp`)
/// or a folder inside it, as a Seatbelt regex: a rule laid after the
/// credential deny lets it through there, and only there. pip writes a
/// temporary copy of the public CA bundle there (`tmpXXXXcacert.pem`) before
/// any package folder exists, and its build environments put certifi in
/// folders inside it.
pub(crate) fn temp_certificate_regex(seat_tmp: &Path) -> String {
    format!("^{}/(.*/)?[^/]*\\.{}$", regex_escape(seat_tmp), shape_to_regex("pem"))
}

/// `credentials::CREDENTIAL_SHAPES`, expressed as a case-insensitive Seatbelt
/// regex anchored under `workspace` — matches at any depth, any number of
/// path segments in.
pub(crate) fn credential_deny_regex(workspace: &Path) -> String {
    let base = regex_escape(workspace);
    let alternation = crate::credentials::CREDENTIAL_SHAPES
        .iter()
        .map(|shape| shape_to_regex(shape))
        .collect::<Vec<_>>()
        .join("|");
    format!("^{base}/(.*/)?({alternation})$")
}

/// Everything that varies between attach flavors, in one place — a boolean
/// soup of positional flags is exactly how the credential fence got tied to
/// `write_workspace` (an agent with a writable workspace would silently have
/// lost the fence).
struct ProfileSpec<'a> {
    workspace: Option<&'a Path>,
    /// Whether the workspace itself is writable (human PTY, workspace-RW
    /// agent runs). Independent of `credential_fence` on purpose.
    write_workspace: bool,
    /// Agent paths keep the credential-shape deny (read AND, when writable,
    /// write) regardless of workspace writability; human paths do not — an
    /// operator may `cat .env` in their own terminal.
    credential_fence: bool,
    /// Additional read-only grants (e.g. a linked worktree's repo `.git`).
    extra_read: &'a [PathBuf],
    /// Additional writable grants (e.g. the worktree's own gitdir under
    /// `<repo>/.git/worktrees/<name>`, so `git status`/`add` can lock the
    /// per-worktree index).
    extra_write: &'a [PathBuf],
    /// Additional folders it may read and run programs from (an operator's
    /// Machine access run grant).
    extra_exec: &'a [PathBuf],
    deny_read: &'a [PathBuf],
    /// Host full: allow by default minus the deny list.
    whole_machine: Option<&'a crate::WholeMachine>,
    /// Folders where only new files may be made ([`append_only_rules`]).
    append_only: &'a [PathBuf],
    /// This Mac only ([`local_network`]), as [`crate::acp::NET_ENV`] asks.
    local_net: bool,
    /// Xcode's temporary folders ([`xcode_temp`]), as
    /// [`crate::acp::XCODE_ENV`] asks.
    xcode: bool,
    /// The secret-named files the operator let it read ([`readable`]).
    readable: String,
    /// The Bakery's ports closed to it ([`closed_doors`]); empty for a
    /// human's terminal.
    closed: String,
    /// Its key agents' sockets closed, unless its variables open them
    /// ([`crate::whole_machine::key_agent_sockets`]): emitted last.
    key_agents: String,
}

/// The Bakery's ports a room never reaches, emitted last so they win over
/// every network rule above: every thread's door but its own
/// ([`crate::acp::DOOR_ENV`]), the Bakery's shared ports
/// ([`crate::acp::SHUT_ENV`]), and the gateways' port on any address.
pub(crate) fn closed_doors(env: &[(String, String)]) -> String {
    let value = |name: &str| {
        env.iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
            .unwrap_or("")
    };
    let own: Option<u16> = value(crate::acp::DOOR_ENV).parse().ok();
    let mut s = String::new();
    for port in value(crate::acp::SHUT_ENV)
        .split(',')
        .filter_map(|p| p.trim().parse::<u16>().ok())
    {
        s.push_str(&format!("(deny network-outbound (remote ip \"*:{port}\"))\n"));
    }
    for port in crate::acp::DOORS.filter(|p| Some(*p) != own) {
        s.push_str(&format!("(deny network-outbound (remote ip \"localhost:{port}\"))\n"));
    }
    // Every project's services but its own, every thread's copies but its own.
    let services: Vec<u16> = value(crate::acp::SERVICES_ENV)
        .split(',')
        .filter_map(|p| p.trim().parse().ok())
        .collect();
    for port in crate::acp::SERVICES.filter(|p| !services.contains(p)) {
        s.push_str(&format!("(deny network-outbound (remote ip \"localhost:{port}\"))\n"));
    }
    s
}

/// Whether a room's variables ask for [`local_network`].
fn local_net(env: &[(String, String)]) -> bool {
    env.iter()
        .any(|(k, v)| k == crate::acp::NET_ENV && v == "local")
}

/// The secret-named files a room's variables name
/// ([`crate::acp::READABLE_ENV`]): each readable, never writable, emitted
/// last so the credential fence above gives way for exactly these.
pub(crate) fn readable(env: &[(String, String)]) -> String {
    let mut s = String::new();
    let paths = env
        .iter()
        .filter(|(k, _)| k == crate::acp::READABLE_ENV)
        .flat_map(|(_, v)| v.lines())
        .map(str::trim)
        .filter(|p| p.starts_with('/'));
    for path in paths {
        s.push_str(&format!("(allow file-read* (literal {}))\n", quote(&canon(Path::new(path)))));
    }
    s
}

/// Whether a room's variables ask for [`xcode_temp`].
pub(crate) fn xcode(env: &[(String, String)]) -> bool {
    env.iter()
        .any(|(k, v)| k == crate::acp::XCODE_ENV && v == "1")
}

/// The user's own temp folder (`getconf DARWIN_USER_TEMP_DIR`), which every
/// app of theirs shares.
fn user_temp_dir() -> Option<PathBuf> {
    let mut buf = vec![0u8; 1024];
    // SAFETY: `buf` is writable for its length, and confstr writes at most
    // that many bytes, NUL included.
    let n = unsafe { libc::confstr(libc::_CS_DARWIN_USER_TEMP_DIR, buf.as_mut_ptr().cast(), buf.len()) };
    if n == 0 || n > buf.len() {
        return None;
    }
    buf.truncate(n - 1);
    Some(PathBuf::from(String::from_utf8(buf).ok()?))
}

/// For a room that builds with Xcode: Swift's compiler makes its temporary
/// folders (`TemporaryDirectory.<random>`) in the user's own temp folder,
/// whatever TMPDIR says. The room may make new ones there and use them, but
/// neither list that folder nor touch anything else in it.
pub(crate) fn xcode_temp() -> String {
    let Some(temp) = user_temp_dir() else {
        return String::new();
    };
    let temp = canon(&temp);
    let mut s = String::from("(allow file-read-metadata\n");
    for dir in temp.ancestors() {
        s.push_str(&format!("  (literal {})\n", quote(dir)));
    }
    s.push_str(")\n");
    s.push_str(&format!(
        "(allow file-read* file-write* (regex #\"^{}/TemporaryDirectory\\.[A-Za-z0-9]+(/|$)\"))\n",
        regex_escape(&temp)
    ));
    s
}

/// A room kept to this Mac: no network, then loopback and local sockets
/// again (dev servers, local services, the Bakery's gateway), then the
/// container sockets closed once more. Emitted last, so it wins over the
/// room's own network rules.
pub(crate) fn local_network() -> String {
    format!(
        "(deny network*)\n(allow network-outbound (remote ip \"localhost:*\"))\n(allow network-inbound (local ip \"localhost:*\"))\n(allow network-bind (local ip \"localhost:*\"))\n(allow network-outbound (remote unix-socket))\n{}\n",
        crate::whole_machine::CLOSED_SOCKETS
    )
}

/// Git's object store, for a hook that re-adds files (lint-staged): new
/// files and folders may be made, and git's temporary objects written,
/// moved into place and removed, but no existing object or pack is deleted,
/// rewritten, replaced or re-moded, so content a branch already points to
/// can never be swapped. Emitted last, so it wins over every allow above.
fn append_only_rules(dirs: &[PathBuf]) -> String {
    let mut s = String::new();
    for dir in dirs {
        let dir = canon(dir);
        s.push_str(&format!("(allow file-read* file-write-create (subpath {}))\n", quote(&dir)));
        s.push_str(&format!(
            "(deny file-write-data file-write-unlink file-write-mode file-write-flags file-write-xattr file-write-times (subpath {}))\n",
            quote(&dir)
        ));
        let temp = format!("^{}/([0-9a-f]{{2}}/)?tmp_obj_[^/]*$", regex_escape(&dir));
        s.push_str(&format!("(allow file-write* (regex #\"{temp}\"))\n"));
    }
    s
}

/// Deny-closed: nothing is readable, writable, or executable except what is
/// named here.
fn seatbelt_profile(toolchain_root: &Path, runtime_root: &Path, spec: &ProfileSpec) -> String {
    let toolchain_root = canon(toolchain_root);
    let runtime_root = canon(runtime_root);
    let workspace = spec.workspace.map(canon);

    let mut s = String::new();
    // The kernel logs every denial, and a build in a room can make thousands a
    // second (git runs xcrun, which runs xcodebuild, which is refused its
    // preferences again and again). The node reads that log for the one
    // denial it acts on, a credential-shaped file, so the default denial is
    // left unreported; the credential rule below reports as usual.
    s.push_str("(version 1)\n(deny default (with no-report))\n");
    // Apple's baseline mach-lookup/sysctl rules so a process can start at all
    // under `deny default`: logging, preferences, trustd. Never the Keychain:
    // `KEYCHAIN_DENY` closes it explicitly, last.
    s.push_str("(import \"system.sb\")\n");
    // Metadata (stat/lstat/realpath): git, compilers, and every path
    // canonicalizer walk the ANCESTOR chains of the paths a room touches
    // (`/private`, `$TMPDIR` parents, the workspace's own parents) and fail
    // bizarrely without it. Scoped to exactly those chains — a global
    // metadata allow would hand a sandboxed child a host-wide
    // reconnaissance channel (existence/size/mtime of other homes, ~/.ssh,
    // heart.db) even with content reads denied. Emitted early; the
    // credential/`$HOME` denies come LAST, so `file-read*` denies (which
    // cover metadata too) still win for those paths.
    {
        let mut chain_roots: Vec<PathBuf> = vec![toolchain_root.clone(), runtime_root.clone()];
        if let Some(w) = &workspace {
            chain_roots.push(w.clone());
        }
        for p in spec
            .extra_read
            .iter()
            .chain(spec.extra_write.iter())
            .chain(spec.extra_exec.iter())
        {
            chain_roots.push(canon(p));
        }
        for d in apple_developer_dirs() {
            chain_roots.push(d.clone());
        }
        let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
        s.push_str("(allow file-read-metadata\n");
        for root in &chain_roots {
            for anc in root.ancestors() {
                if seen.insert(anc.to_path_buf()) {
                    s.push_str(&format!("  (literal {})\n", quote(anc)));
                }
            }
        }
        s.push_str(")\n");
    }
    // Provision may reach the network; an attached command never does.
    s.push_str("(deny network*)\n");
    // `/usr/bin/script` (the PTY path) forks a child and allocates a
    // pseudo-tty — neither is a file operation, so the file-read*/file-write*
    // rules below don't cover them no matter how broad. Harmless for the
    // plain-exec path, which never touches a tty.
    // A room may stop itself and a process in its own sandbox, so cargo
    // can reap rustc. Nothing outside that sandbox. A bare allow would
    // reach the rest of the machine.
    s.push_str("(allow process-fork)\n");
    s.push_str("(allow signal (target self))\n");
    s.push_str("(allow signal (target same-sandbox))\n");
    s.push_str("(allow pseudo-tty)\n");
    s.push_str("(allow file-read* file-write* file-ioctl (literal \"/dev/ptmx\"))\n");
    s.push_str("(allow file-read* file-write* file-ioctl (regex #\"^/dev/ttys[0-9]+$\"))\n");

    // Apple developer tooling: `/usr/bin/git`, `/usr/bin/cc`, etc. are
    // xcrun shims that re-exec binaries under the ACTIVE developer
    // directory (`xcode-select`'s `/var/db/xcode_select_link` — either
    // `/Library/Developer/CommandLineTools` or an Xcode.app bundle).
    // Without these grants, git and every compiler fail inside the room,
    // which quietly made verification impossible. Read/exec only, no write.
    let dev_dirs = apple_developer_dirs();
    s.push_str("(allow process-exec\n");
    for p in ["/bin", "/usr/bin", "/usr/lib", "/System"] {
        s.push_str(&format!("  (subpath {})\n", quote(Path::new(p))));
    }
    for p in dev_dirs {
        s.push_str(&format!("  (subpath {})\n", quote(p)));
    }
    for p in spec.extra_exec {
        s.push_str(&format!("  (subpath {})\n", quote(&canon(p))));
    }
    s.push_str(&format!("  (subpath {})\n", quote(&toolchain_root)));
    // An agent runs what it builds and installs: its workspace (tests,
    // scripts, node_modules tools, a venv) and its room's own home and temp
    // folder (go run, npx). Never another project's folder or the Bakery's.
    for p in workspace.iter().chain([&runtime_root]) {
        s.push_str(&format!("  (subpath {})\n", quote(p)));
    }
    s.push_str(")\n");
    // And loads the libraries those bring (native add-ons, dylibs).
    s.push_str("(allow file-map-executable\n");
    for p in workspace.iter().chain([&runtime_root, &toolchain_root]).chain(dev_dirs).chain(spec.extra_exec) {
        s.push_str(&format!("  (subpath {})\n", quote(&canon(p))));
    }
    s.push_str(")\n");

    s.push_str("(allow file-read*\n");
    // /etc is a link to /private/etc, and Seatbelt matches the real path:
    // without it LibreSSL can't read its settings ("Auto configuration
    // failed"), so cargo can't fetch or build.
    for p in ["/bin", "/usr/bin", "/usr/lib", "/System", "/dev", "/etc"] {
        s.push_str(&format!("  (subpath {})\n", quote(&canon(Path::new(p)))));
    }
    for p in dev_dirs {
        s.push_str(&format!("  (subpath {})\n", quote(p)));
    }
    s.push_str(&format!(
        "  (literal {})\n",
        quote(Path::new("/var/db/xcode_select_link"))
    ));
    // Xcode's license-acceptance marker: xcodebuild (which /usr/bin/git's
    // shim consults on full-Xcode machines) refuses to run at all when it
    // cannot read this. One plist, read-only.
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

    // Toolchain/runtime/workspace last among the *allows*, so they win over
    // anything above regardless of where they happen to sit on disk.
    s.push_str("(allow file-read*\n");
    s.push_str(&format!("  (subpath {})\n", quote(&toolchain_root)));
    s.push_str(&format!("  (subpath {})\n", quote(&runtime_root)));
    if let Some(w) = &workspace {
        s.push_str(&format!("  (subpath {})\n", quote(w)));
    }
    for p in spec.extra_read {
        s.push_str(&format!("  (subpath {})\n", quote(&canon(p))));
    }
    for p in spec.extra_write.iter().chain(spec.extra_exec.iter()) {
        s.push_str(&format!("  (subpath {})\n", quote(&canon(p))));
    }
    s.push_str(")\n");

    s.push_str("(allow file-write*\n");
    s.push_str(&format!("  (subpath {})\n", quote(&runtime_root)));
    if spec.write_workspace {
        if let Some(w) = &workspace {
            s.push_str(&format!("  (subpath {})\n", quote(w)));
        }
    }
    for p in spec.extra_write {
        s.push_str(&format!("  (subpath {})\n", quote(&canon(p))));
    }
    s.push_str(")\n");

    // Named credential surfaces: the absolute last word, so even a workspace
    // path that reaches into `$HOME` cannot read (or replace) these.
    if !spec.deny_read.is_empty() {
        s.push_str("(deny file-read* file-write*\n");
        for p in spec.deny_read {
            s.push_str(&format!("  (subpath {})\n", quote(&canon(p))));
        }
        s.push_str(")\n");
    }

    // Agent paths only (never the human PTY: an operator may `cat .env` in
    // their own terminal). The argv-level check in `attach_exec` is a belt,
    // not the lock — it only sees literal filenames, so `sh -c "cat .env"`
    // or a one-liner interpreter script that opens the file by name never
    // surfaces as an argv token. This is the actual lock: Seatbelt itself
    // refuses the access regardless of how the command names the path. Last
    // rules in the profile, same reasoning as the `$HOME` denies above.
    // With a writable workspace the fence also denies *writes* to
    // credential-shaped names, so an agent run cannot clobber an existing
    // `.env` either.
    if spec.credential_fence {
        if let Some(w) = &workspace {
            // An agent's runtime settings (`.claude/settings.local.json`
            // grants a runtime its own permissions) never change from an
            // agent's commands, as its room already keeps them from it.
            s.push_str("(deny file-write*\n");
            for dir in crate::acp::AGENT_STATE_DIRS {
                s.push_str(&format!("  (subpath {})\n", quote(&w.join(dir))));
            }
            s.push_str(")\n");
            // Its work there stays writable (`AGENT_WORK_DIRS`).
            if spec.write_workspace {
                s.push_str("(allow file-write*\n");
                for dir in crate::acp::AGENT_WORK_DIRS {
                    s.push_str(&format!("  (subpath {})\n", quote(&w.join(dir))));
                }
                s.push_str(")\n");
            }
            // Nor git's settings and hooks, which git outside this room
            // uses: closed as in the ACP room.
            s.push_str(&crate::whole_machine::workspace_git_deny(w));
            s.push_str(&crate::whole_machine::git_settings_deny(Some(w)));
            s.push_str("(deny file-read*\n");
            s.push_str(&format!("  (regex #\"{}\"))\n", credential_deny_regex(w)));
            if spec.write_workspace {
                s.push_str("(deny file-write*\n");
                s.push_str(&format!("  (regex #\"{}\"))\n", credential_deny_regex(w)));
            }
            // A package's certificate bundle goes through: laid after.
            let access = if spec.write_workspace { "file-read* file-write*" } else { "file-read*" };
            s.push_str(&format!("(allow {access}\n  (regex #\"{}\"))\n", package_certificate_regex(w)));
        }
    }
    s.push_str(crate::whole_machine::KEYCHAIN_DENY);
    s
}

/// `<cache_root>/envs/<digest>` → `<cache_root>`. `SessionEnvironment` does
/// not carry `cache_root` directly, but `session::provision` always builds
/// `toolchain_root` this way, so walking up two levels recovers it without
/// widening every attach signature just to thread one more path through.
pub(crate) fn cache_root_of(env: &SessionEnvironment) -> PathBuf {
    env.toolchain_root
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| env.toolchain_root.clone())
}

static PROFILE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Every attach — one-shot exec, one-shot pty, and the interactive room
/// shell alike — gets its own Seatbelt profile file, written to a directory
/// `seatbelt_profile` never grants read *or* write access to. Two attaches
/// used to share one fixed path (`runtime_root/.seatbelt.sb`), and
/// `runtime_root` is in every attach's file-write* allow list: a concurrent
/// human PTY attach (write_workspace=true) could overwrite the agent exec
/// path's just-written read-only profile before `sandbox-exec -f` ever got
/// to read it, handing the agent pipe the human's writable-workspace rules.
/// A unique, sandboxed-unreachable path per attach makes that clobber
/// impossible rather than merely unlikely.
fn write_attach_profile(env: &SessionEnvironment, spec: &ProfileSpec) -> Result<PathBuf, String> {
    let dir = cache_root_of(env).join("profiles");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let n = PROFILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = dir.join(format!("{}-{}-{n}.sb", std::process::id(), env.digest));

    let profile = match spec.whole_machine {
        Some(m) => {
            let mut read = vec![env.toolchain_root.clone()];
            read.extend_from_slice(spec.extra_read);
            read.extend_from_slice(spec.extra_exec);
            let mut write = vec![env.runtime_root.clone()];
            write.extend_from_slice(spec.extra_write);
            crate::whole_machine::profile(
                m,
                &read,
                &write,
                spec.workspace,
                spec.write_workspace,
                spec.deny_read,
            )
        }
        None => seatbelt_profile(&env.toolchain_root, &env.runtime_root, spec),
    };
    let mut profile = profile + &append_only_rules(spec.append_only);
    if spec.local_net {
        profile.push_str(&local_network());
    }
    if spec.xcode {
        profile.push_str(&xcode_temp());
    }
    profile.push_str(&spec.readable);
    if spec.whole_machine.is_none() {
        profile.push_str(&spec.closed);
    }
    profile.push_str(&spec.key_agents);
    std::fs::write(&path, &profile).map_err(|e| e.to_string())?;
    Ok(path)
}

fn env_and_path(env: &SessionEnvironment) -> (PathBuf, String) {
    let home = env.home();
    // Thin rooms pin nothing, so `toolchain_rust_bin()` never existed —
    // only prepend it when there is an actual toolchain to expose.
    let path = if env.toolchain_rust_bin().is_dir() {
        format!("{}:/bin:/usr/bin", env.toolchain_rust_bin().display())
    } else {
        "/bin:/usr/bin".to_string()
    };
    (home, path)
}

/// The room's own writable tmp, exported as `TMPDIR` through the scrubbed
/// `env -i`: xcrun (every `/usr/bin/git`/`cc` shim), compilers, and test
/// runners all need a writable temp dir, and without an explicit `TMPDIR`
/// they fall back to the *host* per-user temp dir the sandbox rightly
/// denies. Created on demand — provision makes it, but a long-lived room
/// may have had it cleaned.
fn room_tmpdir(env: &SessionEnvironment) -> PathBuf {
    let tmp = env.tmp();
    let _ = std::fs::create_dir_all(&tmp);
    tmp
}

/// Read `r` to EOF, but only ever *keep* the first `cap` bytes — past that,
/// still drain (so a chatty child never blocks on a full pipe and hangs
/// around past when we stopped caring), just discard what's read. Appends a
/// truncation marker once, the moment the cap is first crossed.
fn read_capped<R: Read>(mut r: R, cap: usize) -> Vec<u8> {
    let mut buf = Vec::with_capacity(cap.min(64 * 1024));
    let mut chunk = [0u8; 8192];
    let mut truncated = false;
    loop {
        let n = match r.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => break,
        };
        if truncated {
            continue; // draining only, past the cap
        }
        let take = (cap - buf.len()).min(n);
        buf.extend_from_slice(&chunk[..take]);
        if take < n {
            truncated = true;
            buf.extend_from_slice(format!("\n… truncated at {cap} bytes").as_bytes());
        }
    }
    buf
}

/// How long the reader threads get to drain a just-killed pipe before their
/// output is given up on. Bounded on purpose: a grandchild that somehow
/// survives the process-group kill and keeps its end of the pipe open must
/// not be able to wedge attach past this, the way an unbounded `.join()`
/// could.
const REAP_GRACE: Duration = Duration::from_secs(2);

/// Read `r` on a background thread, capped as it streams in (never buffered
/// in full before the cap is applied), reporting through a channel rather
/// than a `JoinHandle` — a channel can be waited on with a deadline; a
/// `JoinHandle::join()` cannot.
fn spawn_capped_reader<R: Read + Send + 'static>(
    mut r: R,
    cap: usize,
) -> std::sync::mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(read_capped(&mut r, cap));
    });
    rx
}

/// Whatever the reader sent before `deadline`, or empty if it hasn't finished
/// yet — the thread itself is left to exit on its own once the pipe closes;
/// it is not joined, so a slow drain cannot block the caller.
fn recv_before(rx: &std::sync::mpsc::Receiver<Vec<u8>>, deadline: Instant) -> Vec<u8> {
    let now = Instant::now();
    let remaining = if deadline > now {
        deadline - now
    } else {
        Duration::ZERO
    };
    rx.recv_timeout(remaining).unwrap_or_default()
}

/// Run `cmd` to completion, or give up once `ATTACH_TIMEOUT` elapses. `cmd`
/// is placed in its own process group (`setpgid`) before spawn specifically
/// so it can be reaped in full: the PTY path forks `script`, which forks the
/// attached command, and killing only the direct child would leave those
/// grandchildren running — one of which can still hold the stdout/stderr
/// pipe open and block a plain `.join()` forever even after the leader has
/// already exited. So the group is killed unconditionally once the leader
/// is done (exited or timed out), and the reader threads are only ever
/// waited on for a bounded grace period after that, never indefinitely.
pub fn run_with_timeout(
    mut cmd: std::process::Command,
    timeout: Duration,
) -> Result<(String, String, i32), String> {
    use std::os::unix::process::CommandExt;

    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd.process_group(0); // new pgid == this child's pid
    let mut child = cmd.spawn().map_err(|e| format!("spawn: {e}"))?;
    let pid = child.id();

    let stdout_pipe = child.stdout.take().expect("stdout piped");
    let stderr_pipe = child.stderr.take().expect("stderr piped");
    let stdout_rx = spawn_capped_reader(stdout_pipe, MAX_OUTPUT_BYTES);
    let stderr_rx = spawn_capped_reader(stderr_pipe, MAX_OUTPUT_BYTES);

    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            break Some(status);
        }
        if Instant::now() >= deadline {
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let timed_out = status.is_none();

    // Unconditional: even a leader that exited cleanly may have left a
    // grandchild behind holding the pipes open.
    kill_process_group(pid);
    if timed_out {
        let _ = child.wait();
    }

    let join_deadline = Instant::now() + REAP_GRACE;
    let stdout = recv_before(&stdout_rx, join_deadline);
    let stderr = recv_before(&stderr_rx, join_deadline);

    if timed_out {
        return Err(format!("attach timed out after {}s", timeout.as_secs()));
    }
    Ok((
        String::from_utf8_lossy(&stdout).into_owned(),
        String::from_utf8_lossy(&stderr).into_owned(),
        status.expect("set when not timed out").code().unwrap_or(-1),
    ))
}

/// `kill(2)` a negative pid signals the whole process group, not just the
/// leader — the only way to be sure a forked `script`'s children die with
/// it. A nonzero return (anything other than the group already being gone)
/// falls back to killing just the leader pid, rather than assuming the group
/// form always works.
///
/// Public: the interactive PTY dock (`bakery-node`) spawns its own long-lived
/// shell outside `run_with_timeout` and needs the exact same group-kill on
/// disconnect, not just a direct-pid `kill` that leaves backgrounded
/// descendants running.
pub fn kill_process_group(pid: u32) {
    let pgid = pid as libc::pid_t;
    let rc = unsafe { libc::kill(-pgid, libc::SIGKILL) };
    if rc != 0 {
        unsafe {
            libc::kill(pgid, libc::SIGKILL);
        }
    }
}

/// Per-attach knobs shared by all public entry points.
struct AttachOpts<'a> {
    cwd: Option<&'a Path>,
    write_workspace: bool,
    credential_fence: bool,
    pty: bool,
    timeout: Duration,
    extra_deny_read: &'a [PathBuf],
    extra_read: &'a [PathBuf],
    extra_write: &'a [PathBuf],
    extra_exec: &'a [PathBuf],
    whole_machine: Option<&'a crate::WholeMachine>,
    path: &'a [PathBuf],
    env: &'a [(String, String)],
    append_only: &'a [PathBuf],
}

/// The variables an agent's command gets beside HOME, PATH and TMPDIR, as
/// `KEY=VALUE` for `env -i`: never one of those three, and never the PATH
/// prefix a native runtime's spawn consumes. [`crate::acp::ROOM_HOME_ENV_VALUE`]
/// in a value is the room's own home.
fn agent_env<'a>(env: &'a [(String, String)], home: &'a Path) -> impl Iterator<Item = String> + 'a {
    env.iter()
        .filter(|(k, _)| {
            !matches!(k.as_str(), "HOME" | "PATH" | "TMPDIR")
                && k != crate::acp::ACP_PATH_PREFIX_ENV
                && ![
                    crate::acp::NET_ENV,
                    crate::acp::DOOR_ENV,
                    crate::acp::SHUT_ENV,
                    crate::acp::XCODE_ENV,
                    crate::acp::READABLE_ENV,
                    crate::acp::SERVICES_ENV,
                    crate::acp::KEY_AGENTS_ENV,
                ]
                .contains(&k.as_str())
        })
        .map(move |(k, v)| format!("{k}={}", v.replace(crate::acp::ROOM_HOME_ENV_VALUE, &home.to_string_lossy())))
}

/// An agent's HOME and PATH: the folders it was given to run tools from by
/// name first, then the room's own, then, on Host full, the operator's.
fn agent_path(env: &SessionEnvironment, first: &[PathBuf], whole: Option<&crate::WholeMachine>) -> (PathBuf, String) {
    let (home, room) = env_and_path(env);
    let mut parts: Vec<String> = first.iter().map(|p| p.display().to_string()).collect();
    parts.push(room);
    parts.extend(whole.filter(|m| !m.path.is_empty()).map(|m| m.path.clone()));
    (home, parts.join(":"))
}

fn run_attached(
    env: &SessionEnvironment,
    argv: &[String],
    opts: &AttachOpts,
) -> Result<AttachOutput, String> {
    if argv.is_empty() {
        return Err("argv must not be empty".to_string());
    }
    if !Path::new("/usr/bin/sandbox-exec").is_file() {
        return Err(
            "seatbelt (sandbox-exec) unavailable — refusing to attach without it".to_string(),
        );
    }

    let mut deny_read = default_deny_paths();
    deny_read.extend_from_slice(opts.extra_deny_read);

    let profile_path = write_attach_profile(
        env,
        &ProfileSpec {
            workspace: opts.cwd,
            write_workspace: opts.write_workspace,
            credential_fence: opts.credential_fence,
            extra_read: opts.extra_read,
            extra_write: opts.extra_write,
            extra_exec: opts.extra_exec,
            deny_read: &deny_read,
            whole_machine: opts.whole_machine,
            append_only: opts.append_only,
            local_net: local_net(opts.env),
            xcode: xcode(opts.env),
            readable: readable(opts.env),
            closed: if opts.credential_fence { closed_doors(opts.env) } else { String::new() },
            key_agents: crate::whole_machine::key_agent_sockets(opts.env),
        },
    )?;

    let (home, path) = agent_path(env, opts.path, opts.whole_machine);

    let mut cmd = std::process::Command::new("/usr/bin/sandbox-exec");
    cmd.arg("-f").arg(&profile_path);
    cmd.arg("/usr/bin/env")
        .arg("-i")
        .arg(format!("HOME={}", home.display()))
        .arg(format!("PATH={path}"))
        .arg(format!("TMPDIR={}", room_tmpdir(env).display()));
    cmd.args(agent_env(opts.env, &home));
    if opts.pty {
        cmd.arg("/usr/bin/script").arg("-q").arg("/dev/null");
    }
    for a in argv {
        cmd.arg(a);
    }
    if let Some(dir) = opts.cwd {
        cmd.current_dir(dir);
    }

    let result = run_with_timeout(cmd, opts.timeout);
    let _ = std::fs::remove_file(&profile_path); // one-shot: nothing else will ever read it again
    let (mut stdout, stderr, exit_code) = result?;
    if opts.pty {
        stdout = strip_pty_noise(&stdout);
    }
    Ok(AttachOutput {
        stdout,
        stderr,
        exit_code,
    })
}

/// A PTY echoes control input, so `script`'s transcript carries an echoed
/// `^D` (EOF) immediately erased by backspaces, plus `\r` line endings that
/// plain exec never produces. Replay the backspaces rather than just
/// filtering bytes, so the two attach paths compare equal.
fn strip_pty_noise(s: &str) -> String {
    let mut out: Vec<char> = Vec::new();
    for c in s.chars() {
        match c {
            '\u{8}' => {
                out.pop();
            }
            '\r' | '\u{4}' => {}
            _ => out.push(c),
        }
    }
    out.into_iter().collect()
}

/// Agent exec: no shell string, argv only. `cwd`, when given, only affects
/// where the command runs and what it may read — the agent's write surface
/// stays the room's own runtime dir, never the workspace (that goes through
/// the Manual propose/accept gate, not a raw process).
///
/// Unlike `attach_pty`, this path refuses outright if any argv token names a
/// credential-shaped file (`secrets.json`, `.env`, `*.pem`, …; same check the
/// files jail uses) — an operator at a real keyboard may `cat .env` in their
/// own terminal, an agent may not. Checked before Seatbelt ever runs, so a
/// denied call never touches the filesystem at all.
pub fn attach_exec(
    env: &SessionEnvironment,
    argv: &[String],
    extra_deny_read: &[PathBuf],
    cwd: Option<&Path>,
) -> Result<AttachOutput, String> {
    if argv
        .iter()
        .any(|a| crate::credentials::is_hidden_credential(Path::new(a)))
    {
        return Err(crate::credentials::CREDENTIAL_SURFACE_DENIED.to_string());
    }
    run_attached(
        env,
        argv,
        &AttachOpts {
            cwd,
            write_workspace: false,
            credential_fence: true,
            pty: false,
            timeout: ATTACH_TIMEOUT,
            extra_deny_read,
            extra_read: &[],
            extra_write: &[],
            extra_exec: &[],
            whole_machine: None,
            path: &[],
            env: &[],
            append_only: &[],
        },
    )
}

/// What an operator's Machine access adds to one agent command: folders it
/// may also read, write, or run programs from. The default deny list still
/// wins over every grant — it is emitted after every allow.
#[derive(Clone, Copy, Default)]
pub struct ExtraGrants<'a> {
    pub read: &'a [PathBuf],
    pub write: &'a [PathBuf],
    pub exec: &'a [PathBuf],
    /// The operator's own deny list, on top of the default one.
    pub deny: &'a [PathBuf],
    /// Host full: the whole machine minus the deny list.
    pub whole_machine: Option<&'a crate::WholeMachine>,
    /// Folders first on PATH, in order, so their tools run by name.
    pub path: &'a [PathBuf],
    /// Variables for the command beside HOME, PATH and TMPDIR, which no
    /// variable here replaces.
    pub env: &'a [(String, String)],
}

/// [`attach_exec`] plus the operator's Machine access grants: the workspace
/// stays read-only, and the granted folders become reachable as granted.
pub fn attach_exec_granted(
    env: &SessionEnvironment,
    argv: &[String],
    cwd: Option<&Path>,
    grants: ExtraGrants,
    timeout: Duration,
) -> Result<AttachOutput, String> {
    if argv
        .iter()
        .any(|a| crate::credentials::is_hidden_credential(Path::new(a)))
    {
        return Err(crate::credentials::CREDENTIAL_SURFACE_DENIED.to_string());
    }
    run_attached(
        env,
        argv,
        &AttachOpts {
            cwd,
            write_workspace: false,
            credential_fence: true,
            pty: false,
            timeout: timeout.min(MAX_WORKSPACE_RUN_TIMEOUT),
            extra_deny_read: grants.deny,
            extra_read: grants.read,
            extra_write: grants.write,
            extra_exec: grants.exec,
            whole_machine: grants.whole_machine,
            path: grants.path,
            env: grants.env,
            append_only: &[],
        },
    )
}

/// One agent run's shape for [`attach_exec_workspace_rw`].
pub struct WorkspaceRun<'a> {
    pub argv: &'a [String],
    pub extra_deny_read: &'a [PathBuf],
    /// Clamped to [`MAX_WORKSPACE_RUN_TIMEOUT`]; pass
    /// [`DEFAULT_WORKSPACE_RUN_TIMEOUT`] when the caller has no opinion.
    pub timeout: Duration,
    /// Additional read-only grants (e.g. a linked worktree's repo `.git`).
    pub extra_read: &'a [PathBuf],
    /// Additional writable grants (e.g. the linked worktree's own gitdir).
    pub extra_write: &'a [PathBuf],
    /// Additional folders it may read and run programs from.
    pub extra_exec: &'a [PathBuf],
    /// Host full: the whole machine minus the deny list.
    pub whole_machine: Option<&'a crate::WholeMachine>,
    /// Folders first on PATH, in order, so their tools run by name.
    pub path: &'a [PathBuf],
    /// Variables for the command beside HOME, PATH and TMPDIR.
    pub env: &'a [(String, String)],
    /// Folders where only new files may be made: git's object store, for a
    /// hook that re-adds files.
    pub append_only: &'a [PathBuf],
}

/// Agent build/test execution: same deny-closed Seatbelt door as
/// [`attach_exec`], but the workspace itself is writable — `cargo test`,
/// `npm test`, formatters, and codegen must be able to write `target/`,
/// caches, and their own outputs *inside the root* or verification is
/// impossible. Containment is the sandbox boundary (nothing outside the
/// named roots, network denied, credential shapes denied for read AND
/// write), never a read-only workspace pretending to be a security model.
/// The per-task Review baseline diff is what makes these writes reviewable.
pub fn attach_exec_workspace_rw(
    env: &SessionEnvironment,
    workspace: &Path,
    run: &WorkspaceRun,
) -> Result<AttachOutput, String> {
    if run
        .argv
        .iter()
        .any(|a| crate::credentials::is_hidden_credential(Path::new(a)))
    {
        return Err(crate::credentials::CREDENTIAL_SURFACE_DENIED.to_string());
    }
    let timeout = run.timeout.min(MAX_WORKSPACE_RUN_TIMEOUT);
    run_attached(
        env,
        run.argv,
        &AttachOpts {
            cwd: Some(workspace),
            write_workspace: true,
            credential_fence: true,
            pty: false,
            timeout,
            extra_deny_read: run.extra_deny_read,
            extra_read: run.extra_read,
            extra_write: run.extra_write,
            extra_exec: run.extra_exec,
            whole_machine: run.whole_machine,
            path: run.path,
            env: run.env,
            append_only: run.append_only,
        },
    )
}

/// Human-terminal attach: same env, same Seatbelt profile, via a real PTY.
/// `cwd`, when given, is also allowed to be read *and written* — this is the
/// human path, and a terminal sitting in the workspace has to be able to
/// edit it.
pub fn attach_pty(
    env: &SessionEnvironment,
    argv: &[String],
    extra_deny_read: &[PathBuf],
    cwd: Option<&Path>,
) -> Result<AttachOutput, String> {
    run_attached(
        env,
        argv,
        &AttachOpts {
            cwd,
            write_workspace: cwd.is_some(),
            credential_fence: false,
            pty: true,
            timeout: ATTACH_TIMEOUT,
            extra_deny_read,
            extra_read: &[],
            extra_write: &[],
            extra_exec: &[],
            whole_machine: None,
            path: &[],
            env: &[],
            append_only: &[],
        },
    )
}

/// What to spawn under a caller-owned pty for the studio's interactive
/// terminal dock. `profile_path` is this attach's own unique Seatbelt
/// profile — the caller is responsible for removing it once the shell exits
/// (this crate has no async/pty dependency to hook that lifecycle itself).
pub struct RoomShell {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub profile_path: PathBuf,
}

/// A long-lived interactive shell in the room, for the studio's PTY dock:
/// same Seatbelt profile and scrubbed `env -i` door as `attach_pty`, but the
/// caller owns the pty (open, read, write, resize) instead of `run_attached`
/// running one argv to completion and capturing its output. This crate has
/// no pty/async dependency, so it hands back only the program + args to
/// spawn under a real pty — allocating that pty is the caller's job.
/// `tools` adds what the shell may also read and run, with its folders
/// first on PATH and its variables set: the toolchains a thread's agents
/// run by name, so the operator's own shell finds them too.
pub fn interactive_room_shell(
    env: &SessionEnvironment,
    workspace: &Path,
    tools: &ExtraGrants,
) -> Result<RoomShell, String> {
    if !Path::new("/usr/bin/sandbox-exec").is_file() {
        return Err(
            "seatbelt (sandbox-exec) unavailable — refusing to attach without it".to_string(),
        );
    }

    let mut deny_read = default_deny_paths();
    deny_read.extend_from_slice(tools.deny);

    let profile_path = write_attach_profile(
        env,
        &ProfileSpec {
            workspace: Some(workspace),
            write_workspace: true,
            credential_fence: false,
            extra_read: tools.read,
            extra_write: tools.write,
            extra_exec: tools.exec,
            deny_read: &deny_read,
            whole_machine: tools.whole_machine,
            append_only: &[],
            local_net: false,
            xcode: false,
            readable: String::new(),
            closed: String::new(),
            key_agents: crate::whole_machine::key_agent_sockets(tools.env),
        },
    )?;

    let (home, path) = agent_path(env, tools.path, tools.whole_machine);
    // The scrubbed `env -i` strips `$SHELL` along with everything else, so
    // read the operator's actual shell before it's gone — falling back to
    // `/bin/zsh` (the stock macOS default) only if unset.
    let shell = std::env::var("SHELL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "/bin/zsh".to_string());

    let program = PathBuf::from("/usr/bin/sandbox-exec");
    let mut args = vec![
        "-f".to_string(),
        profile_path.to_string_lossy().into_owned(),
        "/usr/bin/env".to_string(),
        "-i".to_string(),
        format!("HOME={}", home.display()),
        format!("PATH={path}"),
        format!("TMPDIR={}", room_tmpdir(env).display()),
        "TERM=xterm-256color".to_string(),
    ];
    args.extend(agent_env(tools.env, &home));
    args.extend([shell, "-i".to_string()]);
    Ok(RoomShell {
        program,
        args,
        profile_path,
    })
}

/// One long-running agent command for [`agent_room_command`].
pub struct JobRun<'a> {
    pub argv: &'a [String],
    /// `true` as [`attach_exec_workspace_rw`] runs, `false` as
    /// [`attach_exec_granted`] does: the workspace itself read-only.
    pub write_workspace: bool,
    pub grants: ExtraGrants<'a>,
}

/// An agent command that outlives one call (a build, a test run, a dev
/// server): the same Seatbelt profile, credential fence and scrubbed
/// `env -i` door as [`attach_exec_granted`] / [`attach_exec_workspace_rw`],
/// handed back as a program and args to spawn, the way
/// [`interactive_room_shell`] is. No timeout: the caller owns the process
/// (its pty, its output, its kill) and removes `profile_path` once it
/// exits.
pub fn agent_room_command(
    env: &SessionEnvironment,
    workspace: &Path,
    run: &JobRun,
) -> Result<RoomShell, String> {
    if run.argv.is_empty() {
        return Err("argv must not be empty".to_string());
    }
    if run
        .argv
        .iter()
        .any(|a| crate::credentials::is_hidden_credential(Path::new(a)))
    {
        return Err(crate::credentials::CREDENTIAL_SURFACE_DENIED.to_string());
    }
    if !Path::new("/usr/bin/sandbox-exec").is_file() {
        return Err(
            "seatbelt (sandbox-exec) unavailable — refusing to attach without it".to_string(),
        );
    }
    let mut deny_read = default_deny_paths();
    deny_read.extend_from_slice(run.grants.deny);
    let profile_path = write_attach_profile(
        env,
        &ProfileSpec {
            workspace: Some(workspace),
            write_workspace: run.write_workspace,
            credential_fence: true,
            extra_read: run.grants.read,
            extra_write: run.grants.write,
            extra_exec: run.grants.exec,
            deny_read: &deny_read,
            whole_machine: run.grants.whole_machine,
            append_only: &[],
            local_net: local_net(run.grants.env),
            xcode: xcode(run.grants.env),
            readable: readable(run.grants.env),
            closed: closed_doors(run.grants.env),
            key_agents: crate::whole_machine::key_agent_sockets(run.grants.env),
        },
    )?;
    let (home, path) = agent_path(env, run.grants.path, run.grants.whole_machine);
    let mut args = vec![
        "-f".to_string(),
        profile_path.to_string_lossy().into_owned(),
        "/usr/bin/env".to_string(),
        "-i".to_string(),
        format!("HOME={}", home.display()),
        format!("PATH={path}"),
        format!("TMPDIR={}", room_tmpdir(env).display()),
        "TERM=xterm-256color".to_string(),
    ];
    args.extend(agent_env(run.grants.env, &home));
    args.extend(run.argv.iter().cloned());
    Ok(RoomShell {
        program: PathBuf::from("/usr/bin/sandbox-exec"),
        args,
        profile_path,
    })
}

/// A listening socket no room may reach, counting who connects: where
/// Claude Code keeps its sessions' own (`/tmp/cc-socks*`), or where a key
/// agent keeps its (`key_agents`).
#[cfg(all(test, target_os = "macos"))]
pub(crate) struct SocketCanary {
    root: PathBuf,
    pub(crate) path: PathBuf,
    reached: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

#[cfg(all(test, target_os = "macos"))]
impl SocketCanary {
    /// A canary at `/private/tmp/<top>-<pid>-<n>/<sub>/<file>`.
    pub(crate) fn at(top: &str, sub: &str, file: &str) -> Self {
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let root = PathBuf::from(format!("/private/tmp/{top}-{}-{n}", std::process::id()));
        let dir = root.join(sub);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(file);
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let reached = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = reached.clone();
        std::thread::spawn(move || {
            for _ in listener.incoming().flatten() {
                counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
        });
        // It answers from outside a room: a room's refusal is the profile's.
        std::os::unix::net::UnixStream::connect(&path).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while reached.load(std::sync::atomic::Ordering::SeqCst) == 0 {
            assert!(Instant::now() < deadline, "the canary never answered");
            std::thread::sleep(Duration::from_millis(10));
        }
        reached.store(0, std::sync::atomic::Ordering::SeqCst);
        SocketCanary { root, path, reached }
    }

    /// Where Claude Code keeps its sessions' sockets.
    pub(crate) fn sessions() -> Self {
        Self::at("cc-socks-canary", "", "1.sock")
    }

    /// Where key agents keep theirs: launchd's ssh-agent, a started
    /// ssh-agent, gpg-agent, 1Password and Secretive.
    pub(crate) fn key_agents() -> Vec<Self> {
        vec![
            Self::at("com.apple.launchd.bakery-canary", "", "Listeners"),
            Self::at("ssh-bakery-canary", "", "agent.4242"),
            Self::at("bakery-canary", ".gnupg", "S.gpg-agent"),
            Self::at("bakery-canary", "2BUA8C4S2C.com.1password/t", "agent.sock"),
            Self::at("bakery-canary", "com.maxgoedjen.Secretive.SecretAgent/Data", "socket.ssh"),
        ]
    }

    /// The paths a program may try it by: as /private/tmp, as /tmp, and
    /// through a link in `workspace`.
    pub(crate) fn paths(&self, workspace: &Path) -> Vec<String> {
        let link = workspace.join(format!("canary-{}.sock", self.root.file_name().unwrap().to_string_lossy()));
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&self.path, &link).unwrap();
        let short = self.path.strip_prefix("/private").unwrap().to_path_buf();
        [self.path.clone(), Path::new("/").join(short), link]
            .iter()
            .map(|p| p.display().to_string())
            .collect()
    }

    /// How many connections reached it since it was made.
    pub(crate) fn reached(&self) -> usize {
        // Late arrivals land too.
        std::thread::sleep(Duration::from_millis(200));
        self.reached.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[cfg(all(test, target_os = "macos"))]
impl Drop for SocketCanary {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

// Seatbelt is macOS only: on Linux (a container seat) these have nothing to test.
#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use crate::session::synthetic_test_environment;

    fn provisioned(
        cache_root: &Path,
        workspace_root: &Path,
        stream_id: &str,
    ) -> SessionEnvironment {
        synthetic_test_environment(workspace_root, cache_root, stream_id)
    }

    fn rustc_version_line(out: &str) -> &str {
        out.lines()
            .find(|l| l.starts_with("rustc "))
            .unwrap_or(out)
            .trim()
    }

    /// A Heart a fresh start set aside is as denied as the live one.
    #[test]
    fn a_heart_set_aside_is_denied_like_the_live_one() {
        let bakery = tempfile::tempdir().unwrap();
        for name in ["heart.db", "heart-20261013T120000.db", "heart-20261013T120000.db-wal", "heart-failed-20261013T1.db", "hearth.txt", "node.json"] {
            std::fs::write(bakery.path().join(name), "").unwrap();
        }
        let mut aside: Vec<String> = hearts_aside(bakery.path()).iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
        aside.sort();
        assert_eq!(aside, ["heart-20261013T120000.db", "heart-20261013T120000.db-wal", "heart-failed-20261013T1.db"]);
    }

    /// The door's certificate authority key is on every room's deny list:
    /// a room given the Bakery's whole folder still can't read it (canary).
    #[test]
    fn no_room_reads_the_doors_certificate_authority_key() {
        let home = home_dir().unwrap();
        assert!(default_deny_paths().contains(&home.join(".bakery/swap")));
        let cache = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let bakery = tempfile::tempdir().unwrap();
        let bakery = canon(bakery.path());
        std::fs::create_dir_all(bakery.join("swap")).unwrap();
        std::fs::write(bakery.join("swap/ca.key"), "canary-key\n").unwrap();
        std::fs::write(bakery.join("open.txt"), "open\n").unwrap();
        let env = provisioned(cache.path(), workspace.path(), "wt:ca-key");
        let argv = ["/bin/sh".to_string(), "-c".to_string(), format!("cat {0}/open.txt; cat {0}/swap/ca.key 2>/dev/null || echo denied", bakery.display())];
        let out = attach_exec_granted(
            &env,
            &argv,
            None,
            ExtraGrants {
                read: &[bakery.clone()],
                deny: &[bakery.join("swap")],
                ..Default::default()
            },
            Duration::from_secs(90),
        )
        .unwrap();
        assert_eq!(out.stdout, "open\ndenied\n");
    }

    /// A room reaches its own project's services and its thread's copies,
    /// and no other project's, though all listen on this Mac's loopback.
    #[test]
    fn a_room_reaches_only_its_own_services() {
        use std::io::{Read, Write};
        let cache = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let env = provisioned(cache.path(), workspace.path(), "wt:services");
        let mut ports = crate::acp::SERVICES
            .rev()
            .filter_map(|p| std::net::TcpListener::bind(("127.0.0.1", p)).ok());
        let (own, theirs) = (ports.next().unwrap(), ports.next().unwrap());
        let port = |l: &std::net::TcpListener| l.local_addr().unwrap().port();
        let (own_port, their_port) = (port(&own), port(&theirs));
        for listener in [own, theirs] {
            std::thread::spawn(move || {
                for mut conn in listener.incoming().flatten() {
                    let _ = conn.read(&mut [0u8; 1024]);
                    let _ = conn.write_all(b"HTTP/1.0 200 OK\r\nContent-Length: 2\r\n\r\nok");
                }
            });
        }
        let room = [
            (crate::acp::NET_ENV.to_string(), "local".to_string()),
            (crate::acp::SERVICES_ENV.to_string(), own_port.to_string()),
        ];
        let curl = |port: u16| {
            let url = format!("http://127.0.0.1:{port}/");
            let argv = ["/usr/bin/curl", "-s", "-o", "/dev/null", "-w", "%{http_code}", "--max-time", "60", &url].map(String::from);
            attach_exec_granted(&env, &argv, None, ExtraGrants { env: &room, ..Default::default() }, Duration::from_secs(90))
                .unwrap()
                .stdout
        };
        assert_eq!(curl(own_port), "200", "its own");
        assert_eq!(curl(their_port), "000", "never another's");
    }

    /// A secret-named file the operator allowed reads in the room, and only
    /// that one: it never writes, and every other stays fenced.
    #[test]
    fn a_room_reads_only_the_secret_named_file_it_was_allowed() {
        let cache = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let root = canon(workspace.path());
        std::fs::write(root.join("pub.pem"), "public\n").unwrap();
        std::fs::write(root.join("other.pem"), "other\n").unwrap();
        let env = provisioned(cache.path(), &root, "wt:readable");
        // Names that don't end in .pem, so the argv check lets them through
        // to the fence itself.
        let script = "cat pub.p*m; cat other.p*m 2>/dev/null || echo fenced; echo x >> pub.p*m 2>/dev/null || echo unwritten";
        let run = |room: &[(String, String)]| {
            let argv = ["/bin/sh".to_string(), "-c".to_string(), script.to_string()];
            attach_exec_granted(&env, &argv, Some(&root), ExtraGrants { env: room, ..Default::default() }, Duration::from_secs(90))
                .unwrap()
                .stdout
        };
        let allowed = [(crate::acp::READABLE_ENV.to_string(), root.join("pub.pem").display().to_string())];
        assert_eq!(run(&allowed), "public\nfenced\nunwritten\n");
        assert!(run(&[]).starts_with("fenced"), "fenced without it");
    }

    /// A room that builds with Xcode makes and uses its compiler's temporary
    /// folders in the user's own temp folder, and touches nothing else
    /// there; a room that doesn't, none at all.
    #[test]
    fn an_xcode_room_makes_only_its_own_temporary_folders_in_the_users() {
        let cache = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let env = provisioned(cache.path(), workspace.path(), "wt:xcode");
        let temp = canon(&user_temp_dir().unwrap());
        let other = temp.join(format!("bakery-xcode-probe-{}", std::process::id()));
        let script = format!(
            "d=$(mktemp -d '{}/TemporaryDirectory.XXXXXX') && echo ok > \"$d/f\" && cat \"$d/f\" && rm -r \"$d\"; touch '{}' 2>/dev/null && echo wrote",
            temp.display(),
            other.display()
        );
        let run = |room: &[(String, String)]| {
            let argv = ["/bin/sh".to_string(), "-c".to_string(), script.clone()];
            let out = attach_exec_granted(
                &env,
                &argv,
                None,
                ExtraGrants {
                    env: room,
                    ..Default::default()
                },
                Duration::from_secs(90),
            )
            .unwrap();
            out.stdout.trim().to_string()
        };
        let xcode = [(crate::acp::XCODE_ENV.to_string(), "1".to_string())];
        let made = run(&xcode);
        let _ = std::fs::remove_file(&other);
        assert_eq!(made, "ok", "its own temporary folder, and nothing else");
        assert_eq!(run(&[]), "", "none without");
    }

    /// A room kept to this Mac reaches a server on loopback and nothing
    /// farther; a command room otherwise reaches nothing at all.
    #[test]
    fn a_local_room_reaches_loopback_only() {
        use std::io::{Read, Write};
        let cache = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let env = provisioned(cache.path(), workspace.path(), "wt:local-net");
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for conn in listener.incoming().flatten() {
                let mut conn = conn;
                let _ = conn.read(&mut [0u8; 1024]);
                let _ = conn.write_all(b"HTTP/1.0 200 OK\r\nContent-Length: 2\r\n\r\nok");
            }
        });
        let curl = |url: &str, net: &[(String, String)]| {
            let argv = [
                "/usr/bin/curl",
                "-s",
                "-o",
                "/dev/null",
                "-w",
                "%{http_code}",
                "--max-time",
                "60",
                url,
            ]
            .map(String::from);
            let out = attach_exec_granted(
                &env,
                &argv,
                None,
                ExtraGrants {
                    env: net,
                    ..Default::default()
                },
                Duration::from_secs(90),
            )
            .unwrap();
            out.stdout
        };
        let local = [(crate::acp::NET_ENV.to_string(), "local".to_string())];
        let loopback = format!("http://127.0.0.1:{port}/");
        assert_eq!(curl(&loopback, &local), "200", "loopback is this Mac");
        assert_eq!(curl("http://192.0.2.1/", &local), "000", "nothing farther");
        assert_eq!(
            curl(&loopback, &[]),
            "000",
            "a command room reaches nothing"
        );
    }

    /// Claude Code's own session sockets are out of every room's reach, its
    /// local sockets open or not, by every path to one: a project agent can
    /// never put a message into the operator's sessions.
    #[test]
    fn a_room_never_reaches_claude_codes_session_sockets() {
        let canary = super::SocketCanary::sessions();
        let cache = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let env = provisioned(cache.path(), workspace.path(), "wt:cc-socks");
        let local = [(crate::acp::NET_ENV.to_string(), "local".to_string())];
        for net in [&local[..], &[]] {
            for path in canary.paths(workspace.path()) {
                let argv = ["/usr/bin/nc", "-U", "-w", "1", &path].map(String::from);
                let grants = ExtraGrants { env: net, ..Default::default() };
                let out = attach_exec_granted(&env, &argv, Some(workspace.path()), grants, Duration::from_secs(30)).unwrap();
                assert_ne!(out.exit_code, 0, "{path} (net {net:?}): {}", out.stderr);
            }
        }
        assert_eq!(canary.reached(), 0, "a room reached the operator's sessions");
    }

    /// The operator's key agents (launchd's ssh-agent, a started one,
    /// gpg-agent, 1Password's, Secretive's, and wherever SSH_AUTH_SOCK
    /// points) are out of reach of every room, its local sockets open or
    /// not, and of the whole machine, by every path to one; opened for one
    /// sandbox, they answer it.
    #[test]
    fn a_room_never_reaches_the_operators_key_agents() {
        let mut canaries = super::SocketCanary::key_agents();
        canaries.push(super::SocketCanary::at("bakery-canary", "", "own-agent"));
        let cache = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let env = provisioned(cache.path(), workspace.path(), "wt:key-agents");
        let machine = crate::WholeMachine { home: home.path().to_path_buf(), bakery_root: cache.path().to_path_buf(), path: String::new() };
        let var = |k: &str, v: &str| (k.to_string(), v.to_string());
        let own = var("SSH_AUTH_SOCK", &canaries.last().unwrap().path.display().to_string());
        let local = [var(crate::acp::NET_ENV, "local"), own.clone()];
        let plain = [own];
        let nc = |path: &str, grants: ExtraGrants| {
            let argv = ["/usr/bin/nc", "-U", "-w", "1", path].map(String::from);
            attach_exec_granted(&env, &argv, Some(workspace.path()), grants, Duration::from_secs(30)).unwrap()
        };
        for canary in &canaries {
            for path in canary.paths(workspace.path()) {
                let sandboxes = [
                    ("a room kept to this Mac", ExtraGrants { env: &local, ..Default::default() }),
                    ("a room", ExtraGrants { env: &plain, ..Default::default() }),
                    ("the whole machine", ExtraGrants { env: &plain, whole_machine: Some(&machine), ..Default::default() }),
                ];
                for (what, grants) in sandboxes {
                    let out = nc(&path, grants);
                    assert_ne!(out.exit_code, 0, "{what} reached {path}: {}", out.stderr);
                }
            }
            assert_eq!(canary.reached(), 0, "{} was reached", canary.path.display());
        }
        let open = [var(crate::acp::NET_ENV, "local"), var(crate::acp::KEY_AGENTS_ENV, "open")];
        let first = &canaries[0];
        let out = nc(&first.path.display().to_string(), ExtraGrants { env: &open, ..Default::default() });
        assert_eq!((out.exit_code, first.reached()), (0, 1), "opened, it answers: {}", out.stderr);
    }

    /// A room reaches its own thread's door and no other: a sibling's door
    /// and the Bakery's shared ports stay closed, so a secret it reads from
    /// a sibling process opens nothing.
    #[test]
    fn a_room_reaches_its_own_door_and_no_sibling_s() {
        use std::io::{Read, Write};
        let cache = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let env = provisioned(cache.path(), workspace.path(), "wt:doors");
        let serve = |listener: std::net::TcpListener| {
            std::thread::spawn(move || {
                for mut conn in listener.incoming().flatten() {
                    let _ = conn.read(&mut [0u8; 1024]);
                    let _ = conn.write_all(b"HTTP/1.0 200 OK\r\nContent-Length: 2\r\n\r\nok");
                }
            });
        };
        // Two doors from the top of the block, clear of a running node's.
        let mut doors = crate::acp::DOORS
            .rev()
            .filter_map(|p| std::net::TcpListener::bind(("127.0.0.1", p)).ok());
        let (own, sibling) = (doors.next().unwrap(), doors.next().unwrap());
        let shared = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = |l: &std::net::TcpListener| l.local_addr().unwrap().port();
        let (own_port, sibling_port, shared_port) = (port(&own), port(&sibling), port(&shared));
        serve(own);
        serve(sibling);
        serve(shared);
        let room = [
            (crate::acp::NET_ENV.to_string(), "local".to_string()),
            (crate::acp::DOOR_ENV.to_string(), own_port.to_string()),
            (crate::acp::SHUT_ENV.to_string(), shared_port.to_string()),
        ];
        let curl = |port: u16| {
            let argv = [
                "/usr/bin/curl",
                "-s",
                "-o",
                "/dev/null",
                "-w",
                "%{http_code}",
                "--max-time",
                "60",
                &format!("http://127.0.0.1:{port}/"),
            ]
            .map(String::from);
            attach_exec_granted(
                &env,
                &argv,
                None,
                ExtraGrants {
                    env: &room,
                    ..Default::default()
                },
                Duration::from_secs(90),
            )
            .unwrap()
            .stdout
        };
        assert_eq!(curl(own_port), "200", "its own door");
        assert_eq!(curl(sibling_port), "000", "never a sibling's");
        assert_eq!(curl(shared_port), "000", "never the Bakery's shared ports");
    }

    /// Machine access on an agent command: a granted folder is reachable as
    /// granted — read, write, or run its programs — and no other folder is.
    #[test]
    fn granted_folders_are_reachable_as_granted_and_nothing_else_is() {
        use std::os::unix::fs::PermissionsExt;
        let cache = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let env = provisioned(cache.path(), workspace.path(), "wt:grants");
        let (project, other) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        std::fs::write(project.path().join("README"), "cloned\n").unwrap();
        std::fs::write(other.path().join("README"), "private\n").unwrap();
        let tool = project.path().join("tool.sh");
        std::fs::write(&tool, "#!/bin/sh\necho built\n").unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        let granted = [project.path().to_path_buf()];
        let attach = |argv: Vec<String>, grants: ExtraGrants| {
            attach_exec_granted(&env, &argv, Some(workspace.path()), grants, Duration::from_secs(30))
                .unwrap()
        };
        let cat = |path: &Path, grants| attach(vec!["/bin/cat".into(), path.display().to_string()], grants);
        let read = ExtraGrants { read: &granted, ..Default::default() };

        assert_ne!(cat(&project.path().join("README"), ExtraGrants::default()).exit_code, 0);
        let out = cat(&project.path().join("README"), read);
        assert_eq!(out.stdout, "cloned\n", "{}", out.stderr);
        assert_ne!(cat(&other.path().join("README"), read).exit_code, 0, "only the granted folder");

        let run = |grants| attach(vec![tool.display().to_string()], grants);
        assert_ne!(run(read).exit_code, 0, "read alone runs nothing");
        let out = run(ExtraGrants { exec: &granted, ..Default::default() });
        assert_eq!(out.stdout, "built\n", "{}", out.stderr);

        let made = project.path().join("out");
        let touch = |grants| attach(vec!["/usr/bin/touch".into(), made.display().to_string()], grants);
        assert_ne!(touch(ExtraGrants { exec: &granted, ..Default::default() }).exit_code, 0);
        assert!(!made.exists(), "a run grant writes nothing");
        assert_eq!(touch(ExtraGrants { write: &granted, ..Default::default() }).exit_code, 0);
        assert!(made.exists());
    }

    /// Git's object store in an agent's room, for a hook that re-adds files:
    /// git can make a new object there, but nothing existing can be deleted,
    /// rewritten, re-moded or renamed over, so a blob a branch already points
    /// to can never be swapped.
    #[test]
    fn an_agents_room_adds_objects_but_never_changes_one() {
        use std::os::unix::fs::PermissionsExt;
        let cache = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git").current_dir(repo.path()).args(args).output().unwrap();
            assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
            String::from_utf8(out.stdout).unwrap()
        };
        git(&["init", "-q"]);
        std::fs::write(repo.path().join("a.txt"), "main points here\n").unwrap();
        let blob = git(&["hash-object", "-w", "a.txt"]).trim().to_string();
        let git_dir = repo.path().join(".git").canonicalize().unwrap();
        let objects = git_dir.join("objects");
        let existing = objects.join(&blob[..2]).join(&blob[2..]);
        // Writable by mode, so only the room can be what stops a change.
        std::fs::set_permissions(&existing, std::fs::Permissions::from_mode(0o644)).unwrap();
        let pack = objects.join("pack").join("pack-test.pack");
        std::fs::write(&pack, "pack").unwrap();
        let before = std::fs::read(&existing).unwrap();

        let probe = |name: &str, command: String| format!("{command} >/dev/null 2>&1 && printf '{name}=yes ' || printf '{name}=no '; ");
        let ex = existing.display();
        let tmp = objects.join("tmp_obj_canary");
        let script = [
            "printf 'brand new\\n' > new.txt; ".to_string(),
            probe("new", "git hash-object -w new.txt".to_string()),
            probe("rm", format!("rm -f '{ex}'")),
            probe("write", format!("printf x > '{ex}'")),
            probe("chmod", format!("chmod 600 '{ex}'")),
            probe("over", format!("printf y > '{}' && mv -f '{}' '{ex}'", tmp.display(), tmp.display())),
            probe("pack", format!("rm -f '{}'", pack.display())),
        ]
        .concat();
        let env = provisioned(cache.path(), ws.path(), "wt:objects");
        let argv = ["/bin/sh".to_string(), "-c".to_string(), script];
        let vars = [("GIT_DIR".to_string(), git_dir.display().to_string())];
        let out = attach_exec_workspace_rw(
            &env,
            ws.path(),
            &WorkspaceRun {
                argv: &argv,
                extra_deny_read: &[],
                timeout: Duration::from_secs(30),
                extra_read: &[git_dir.clone()],
                extra_write: &[],
                extra_exec: &[],
                whole_machine: None,
                path: &[],
                env: &vars,
                append_only: &[objects.clone()],
            },
        )
        .unwrap();
        assert!(
            out.stdout.contains("new=yes rm=no write=no chmod=no over=no pack=no"),
            "{} {}",
            out.stdout,
            out.stderr
        );
        assert_eq!(std::fs::read(&existing).unwrap(), before, "the existing object is untouched");
        assert!(tmp.is_file(), "the temporary object was made: only renaming it over was refused");
        assert!(pack.is_file());
    }

    /// An agent runs what it builds and installs, in its workspace and its
    /// room's home, never a program from a folder it may only read; it reads
    /// LibreSSL's settings under /etc (cargo needs them), and a variable
    /// naming the room's home gets the real one.
    #[test]
    fn an_agent_runs_what_it_builds_in_its_own_room() {
        use std::os::unix::fs::PermissionsExt;
        let (cache, ws, elsewhere) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let outside = elsewhere.path().join("tool.sh");
        std::fs::write(&outside, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o755)).unwrap();
        let probe = |name: &str, command: String| format!("{command} >/dev/null 2>&1 && printf '{name}=yes ' || printf '{name}=no '; ");
        let script = [
            "printf '#!/bin/sh\\nexit 0\\n' > s.sh; chmod +x s.sh; printf 'int main(void){return 0;}\\n' > t.c; ".to_string(),
            probe("script", "./s.sh".to_string()),
            probe("built", "cc -o t t.c && ./t".to_string()),
            probe("home", r#"cp t "$HOME/t" && "$HOME/t""#.to_string()),
            probe("outside", format!("'{}'", outside.display())),
            probe("etc", "cat /etc/ssl/openssl.cnf".to_string()),
            probe("own", r#"test "$CARGO_HOME" = "$HOME/.cargo""#.to_string()),
        ]
        .concat();
        let env = provisioned(cache.path(), ws.path(), "wt:builds");
        let argv = ["/bin/sh".to_string(), "-c".to_string(), script];
        let vars = [("CARGO_HOME".to_string(), format!("{}/.cargo", crate::acp::ROOM_HOME_ENV_VALUE))];
        let out = attach_exec_workspace_rw(
            &env,
            ws.path(),
            &WorkspaceRun {
                argv: &argv,
                extra_deny_read: &[],
                timeout: Duration::from_secs(60),
                extra_read: &[elsewhere.path().to_path_buf()],
                extra_write: &[],
                extra_exec: &[],
                whole_machine: None,
                path: &[],
                env: &vars,
                append_only: &[],
            },
        )
        .unwrap();
        let want = "script=yes built=yes home=yes outside=no etc=yes own=yes";
        assert!(out.stdout.contains(want), "{} {}", out.stdout, out.stderr);
    }

    /// A package's certificate bundle is read and written in a command
    /// room; a key at the project's root and a secret inside a package
    /// folder stay blocked.
    #[test]
    fn a_command_room_reads_and_writes_package_certificates_only() {
        let (cache, ws) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let probe = |name: &str, command: &str| format!("{command} >/dev/null 2>&1 && printf '{name}=yes ' || printf '{name}=no '; ");
        let pip = ".venv/lib/python3.12/site-packages/pip/_vendor/certifi";
        let grpc = ".venv/lib/python3.12/site-packages/grpc/_cython/_credentials";
        std::fs::write(ws.path().join("id.pem"), "key\n").unwrap();
        let script = [
            format!("mkdir -p {pip} {grpc} node_modules/sdk; "),
            probe("pip", &format!("printf cert > {pip}/cacert.pem && cat {pip}/cacert.pem")),
            probe("grpc", &format!("printf cert > {grpc}/roots.pem && cat {grpc}/roots.pem")),
            probe("node", "printf cert > node_modules/sdk/ca.pem && cat node_modules/sdk/ca.pem"),
            probe("key", "cat id.pem"),
            probe("env", &format!("printf A=1 > {pip}/.env")),
        ]
        .concat();
        let env = provisioned(cache.path(), ws.path(), "wt:certs");
        let argv = ["/bin/sh".to_string(), "-c".to_string(), script];
        let run = WorkspaceRun {
            argv: &argv,
            extra_deny_read: &[],
            timeout: Duration::from_secs(60),
            extra_read: &[],
            extra_write: &[],
            extra_exec: &[],
            whole_machine: None,
            path: &[],
            env: &[],
            append_only: &[],
        };
        let out = attach_exec_workspace_rw(&env, ws.path(), &run).unwrap();
        assert!(out.stdout.contains("pip=yes grpc=yes node=yes key=no env=no"), "{} {}", out.stdout, out.stderr);
    }

    /// A new venv gets its pip in a command room: ensurepip writes certifi's
    /// bundle, and pip reads it.
    #[test]
    fn a_new_venv_gets_its_pip_in_a_command_room() {
        let (cache, ws) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let env = provisioned(cache.path(), ws.path(), "wt:venv");
        let argv = ["/bin/sh".to_string(), "-c".to_string(), "/usr/bin/python3 -m venv .venv && .venv/bin/python -m pip --version".to_string()];
        let run = WorkspaceRun {
            argv: &argv,
            extra_deny_read: &[],
            timeout: Duration::from_secs(300),
            extra_read: &[],
            extra_write: &[],
            extra_exec: &[],
            whole_machine: None,
            path: &[],
            env: &[],
            append_only: &[],
        };
        let out = attach_exec_workspace_rw(&env, ws.path(), &run).unwrap();
        assert!(out.exit_code == 0 && out.stdout.starts_with("pip "), "{} {}", out.stdout, out.stderr);
    }

    #[test]
    fn agent_exec_refuses_to_cat_a_planted_dotenv_or_auth_file_in_the_workspace() {
        let cache = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let env = provisioned(cache.path(), workspace.path(), "wt:a");
        std::fs::write(workspace.path().join(".env"), "API_KEY=do-not-leak\n").unwrap();
        std::fs::write(
            workspace.path().join("auth.json"),
            r#"{"token":"do-not-leak"}"#,
        )
        .unwrap();

        for name in [".env", "auth.json"] {
            let err = attach_exec(
                &env,
                &["/bin/cat".to_string(), name.to_string()],
                &[],
                Some(workspace.path()),
            )
            .unwrap_err();
            assert_eq!(err, "denied: credential surface", "{name} got: {err}");
        }

        // Same session, unrelated command: rustc must still run.
        let rustc = attach_exec(
            &env,
            &["rustc".to_string(), "--version".to_string()],
            &[],
            Some(workspace.path()),
        )
        .unwrap();
        assert_eq!(rustc.exit_code, 0, "stderr: {}", rustc.stderr);
    }

    /// The bug this fixes: `is_credential_filename` only ever sees whole argv
    /// tokens, so `/bin/sh -c "cat .env"` never puts a bare `.env` token in
    /// argv at all — the credential-shaped name is buried inside a shell
    /// command string. The argv check alone would let this through; the
    /// Seatbelt regex on the agent profile is the actual lock.
    #[test]
    fn agent_exec_cannot_read_a_dotenv_via_a_shell_one_liner() {
        let cache = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let env = provisioned(cache.path(), workspace.path(), "wt:a");
        std::fs::write(
            workspace.path().join(".env"),
            "API_KEY=do-not-leak-via-shell\n",
        )
        .unwrap();
        std::fs::write(workspace.path().join("readme.txt"), "not a secret\n").unwrap();

        let out = attach_exec(
            &env,
            &[
                "/bin/sh".to_string(),
                "-c".to_string(),
                "cat .env".to_string(),
            ],
            &[],
            Some(workspace.path()),
        )
        .unwrap();
        assert_ne!(
            out.exit_code, 0,
            "the shell one-liner must not be able to read .env"
        );
        assert!(
            !out.stdout.contains("do-not-leak-via-shell"),
            "stdout: {:?}",
            out.stdout
        );

        // An ordinary file in the same workspace is unaffected.
        let ok = attach_exec(
            &env,
            &[
                "/bin/sh".to_string(),
                "-c".to_string(),
                "cat readme.txt".to_string(),
            ],
            &[],
            Some(workspace.path()),
        )
        .unwrap();
        assert_eq!(ok.exit_code, 0, "stderr: {}", ok.stderr);
        assert!(ok.stdout.contains("not a secret"));

        // Same profile shape, unrelated command: rustc must still run.
        let rustc = attach_exec(
            &env,
            &["rustc".to_string(), "--version".to_string()],
            &[],
            Some(workspace.path()),
        )
        .unwrap();
        assert_eq!(rustc.exit_code, 0, "stderr: {}", rustc.stderr);
    }

    /// `is_credential_filename` is case-insensitive; the Seatbelt regex must
    /// be too, or a case-sensitive volume lets `.ENV`/`Auth.JSON` through a
    /// shell one-liner even though the same names in lowercase are denied.
    #[test]
    fn agent_exec_cannot_read_mixed_case_credential_names_via_a_shell_one_liner() {
        let cache = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let env = provisioned(cache.path(), workspace.path(), "wt:a");
        std::fs::write(
            workspace.path().join(".ENV"),
            "API_KEY=do-not-leak-mixed-case\n",
        )
        .unwrap();
        std::fs::write(
            workspace.path().join("Auth.JSON"),
            r#"{"token":"do-not-leak-mixed-case"}"#,
        )
        .unwrap();

        for name in [".ENV", "Auth.JSON"] {
            let out = attach_exec(
                &env,
                &[
                    "/bin/sh".to_string(),
                    "-c".to_string(),
                    format!("cat {name}"),
                ],
                &[],
                Some(workspace.path()),
            )
            .unwrap();
            assert_ne!(out.exit_code, 0, "{name} must be denied regardless of case");
            assert!(
                !out.stdout.contains("do-not-leak-mixed-case"),
                "{name} stdout: {:?}",
                out.stdout
            );
        }

        let rustc = attach_exec(
            &env,
            &["rustc".to_string(), "--version".to_string()],
            &[],
            Some(workspace.path()),
        )
        .unwrap();
        assert_eq!(rustc.exit_code, 0, "stderr: {}", rustc.stderr);
    }

    #[test]
    fn human_pty_may_still_read_a_dotenv_in_the_workspace() {
        let cache = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let env = provisioned(cache.path(), workspace.path(), "wt:a");
        std::fs::write(
            workspace.path().join(".env"),
            "API_KEY=fine-in-your-own-terminal\n",
        )
        .unwrap();

        let out = attach_pty(
            &env,
            &["/bin/cat".to_string(), ".env".to_string()],
            &[],
            Some(workspace.path()),
        )
        .unwrap();
        assert_eq!(out.exit_code, 0, "stderr: {}", out.stderr);
        assert!(out.stdout.contains("fine-in-your-own-terminal"));
    }

    #[test]
    fn exec_and_pty_report_the_same_rustc_version() {
        let cache = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let env = provisioned(cache.path(), workspace.path(), "wt:a");

        let exec_out = attach_exec(
            &env,
            &["rustc".to_string(), "--version".to_string()],
            &[],
            None,
        )
        .unwrap();
        let pty_out = attach_pty(
            &env,
            &["rustc".to_string(), "--version".to_string()],
            &[],
            None,
        )
        .unwrap();

        assert_eq!(exec_out.exit_code, 0, "exec stderr: {}", exec_out.stderr);
        assert_eq!(pty_out.exit_code, 0, "pty stderr: {}", pty_out.stderr);
        assert_eq!(
            rustc_version_line(&exec_out.stdout),
            rustc_version_line(&pty_out.stdout)
        );
        assert!(rustc_version_line(&exec_out.stdout).starts_with("rustc 1.98.0"));
    }

    #[test]
    fn host_secret_env_does_not_reach_the_room() {
        std::env::set_var("HOST_SECRET", "leaked");
        let cache = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let env = provisioned(cache.path(), workspace.path(), "wt:a");

        let out = attach_exec(
            &env,
            &["/usr/bin/printenv".to_string(), "HOST_SECRET".to_string()],
            &[],
            None,
        )
        .unwrap();

        std::env::remove_var("HOST_SECRET");
        assert_ne!(out.exit_code, 0, "printenv must find nothing");
        assert!(out.stdout.trim().is_empty());
    }

    #[test]
    fn attach_has_no_http_proxy_leak() {
        std::env::set_var("http_proxy", "http://leaked-proxy.invalid:8080");
        let cache = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let env = provisioned(cache.path(), workspace.path(), "wt:a");

        let out = attach_exec(
            &env,
            &["/usr/bin/printenv".to_string(), "http_proxy".to_string()],
            &[],
            None,
        )
        .unwrap();

        std::env::remove_var("http_proxy");
        assert_ne!(out.exit_code, 0, "printenv must find nothing");
        assert!(out.stdout.trim().is_empty());
    }

    /// A stronger network-deny check than DNS failure alone: bind a real
    /// listener on localhost and confirm attach cannot reach *that*, not
    /// just that some remote host is unresolvable.
    #[test]
    fn attach_cannot_reach_a_listener_on_localhost() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        // Accept-and-drop so a connection attempt has something to hang up on
        // instead of racing this thread's exit.
        let accept_thread = std::thread::spawn(move || {
            let _ = listener.accept();
        });

        let cache = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let env = provisioned(cache.path(), workspace.path(), "wt:a");

        let out = attach_exec(
            &env,
            &[
                "/usr/bin/nc".to_string(),
                "-G".to_string(),
                "2".to_string(),
                "-z".to_string(),
                "127.0.0.1".to_string(),
                port.to_string(),
            ],
            &[],
            None,
        )
        .unwrap();
        assert_ne!(
            out.exit_code, 0,
            "must not reach a listener on localhost either: {}",
            out.stderr
        );

        // Sanity: an unsandboxed nc against the same listener succeeds, so the
        // failure above is the sandbox denying it, not the listener being down.
        let plain = std::process::Command::new("/usr/bin/nc")
            .args(["-G", "2", "-z", "127.0.0.1", &port.to_string()])
            .status()
            .unwrap();
        assert!(
            plain.success(),
            "sanity check: the listener itself must be reachable outside the sandbox"
        );
        let _ = accept_thread.join();
    }

    #[test]
    fn attached_path_has_no_host_homebrew_or_host_cargo() {
        let cache = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let env = provisioned(cache.path(), workspace.path(), "wt:a");

        let out = attach_exec(
            &env,
            &["/usr/bin/printenv".to_string(), "PATH".to_string()],
            &[],
            None,
        )
        .unwrap();

        assert_eq!(out.exit_code, 0, "stderr: {}", out.stderr);
        assert!(
            !out.stdout.contains("/opt/homebrew"),
            "PATH: {}",
            out.stdout
        );
        assert!(!out.stdout.contains("/.cargo"), "PATH: {}", out.stdout);
    }

    #[test]
    fn attach_denies_outbound_network() {
        let cache = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let env = provisioned(cache.path(), workspace.path(), "wt:a");

        // DNS resolution alone must fail — no need for the host to even be reachable.
        let out = attach_exec(
            &env,
            &[
                "/usr/bin/nc".to_string(),
                "-G".to_string(),
                "2".to_string(),
                "-z".to_string(),
                "example.com".to_string(),
                "80".to_string(),
            ],
            &[],
            None,
        )
        .unwrap();
        assert_ne!(
            out.exit_code,
            0,
            "outbound network must be denied: {out_stdout} {out_stderr}",
            out_stdout = out.stdout,
            out_stderr = out.stderr
        );
    }

    #[test]
    fn attach_cannot_rewrite_its_own_toolchain() {
        let cache = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let env = provisioned(cache.path(), workspace.path(), "wt:a");
        let rustc_path = env.toolchain_rust_bin().join("rustc");
        let before = std::fs::read(&rustc_path).unwrap();

        let out = attach_exec(
            &env,
            &[
                "/usr/bin/tee".to_string(),
                rustc_path.to_string_lossy().into_owned(),
            ],
            &[],
            None,
        )
        .unwrap();
        assert_ne!(
            out.exit_code, 0,
            "writing into the toolchain cache must be denied"
        );

        let after = std::fs::read(&rustc_path).unwrap();
        assert_eq!(
            before, after,
            "the cached rustc binary must be byte-for-byte unchanged"
        );
    }

    /// This path is now denied twice over — Seatbelt (`extra_deny_read`) *and*
    /// the argv-level credential-filename check both refuse a `secrets.json`
    /// read, independently. Either denial is an acceptable outcome here; what
    /// must never happen is the content leaking.
    #[test]
    fn planted_secrets_file_outside_the_room_is_unreadable() {
        let cache = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let env = provisioned(cache.path(), workspace.path(), "wt:a");

        let secrets_dir = tempfile::tempdir().unwrap();
        let secrets_path = secrets_dir.path().join("secrets.json");
        std::fs::write(&secrets_path, r#"{"api_key":"do-not-leak"}"#).unwrap();

        match attach_exec(
            &env,
            &[
                "/bin/cat".to_string(),
                secrets_path.to_string_lossy().into_owned(),
            ],
            std::slice::from_ref(&secrets_path),
            None,
        ) {
            Ok(out) => {
                assert_ne!(out.exit_code, 0, "cat of a denied path must fail");
                assert!(!out.stdout.contains("do-not-leak"));
            }
            Err(e) => assert_eq!(e, "denied: credential surface", "got: {e}"),
        }

        // Same profile shape, unrelated command: rustc must still run.
        let rustc = attach_exec(
            &env,
            &["rustc".to_string(), "--version".to_string()],
            std::slice::from_ref(&secrets_path),
            None,
        )
        .unwrap();
        assert_eq!(rustc.exit_code, 0, "stderr: {}", rustc.stderr);
    }

    /// The default deny set (not `extra_deny_read`) must cover every named
    /// credential surface, and must not collaterally break the room's own
    /// toolchain read. Provision first (needs the real `HOME`/rustup), then
    /// swap `HOME` only for the attach calls under test.
    ///
    /// Most of these filenames *also* trip the separate argv-level
    /// credential-filename check (`attach_exec` refuses before Seatbelt ever
    /// runs) — that overlap is fine, both layers agreeing the content must
    /// not leak is the point; only `id_rsa` (not a recognized credential
    /// *filename* shape) exercises the Seatbelt path alone.
    #[test]
    fn default_deny_paths_cover_bakery_ssh_claude_codex_and_gemini_oauth() {
        let cache = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let env = provisioned(cache.path(), workspace.path(), "wt:a");

        let fake_home = tempfile::tempdir().unwrap();
        let planted = [
            fake_home.path().join(".bakery").join("secrets.json"),
            fake_home.path().join(".ssh").join("id_rsa"),
            fake_home.path().join(".claude").join("credentials.json"),
            fake_home.path().join(".codex").join("auth.json"),
            fake_home.path().join(".gemini").join("oauth_creds.json"),
        ];
        for p in &planted {
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, "TOP-SECRET").unwrap();
        }

        let real_home = std::env::var("HOME").unwrap();
        std::env::set_var("HOME", fake_home.path());

        let mut results = Vec::new();
        for p in &planted {
            match attach_exec(
                &env,
                &["/bin/cat".to_string(), p.to_string_lossy().into_owned()],
                &[],
                None,
            ) {
                Ok(out) => {
                    assert!(
                        !out.stdout.contains("TOP-SECRET"),
                        "{p:?} leaked: {}",
                        out.stdout
                    );
                    results.push((p.clone(), out.exit_code != 0));
                }
                Err(e) => {
                    assert_eq!(e, "denied: credential surface", "{p:?} got: {e}");
                    results.push((p.clone(), true));
                }
            }
        }
        let rustc = attach_exec(
            &env,
            &["rustc".to_string(), "--version".to_string()],
            &[],
            None,
        )
        .unwrap();

        std::env::set_var("HOME", real_home);

        for (p, denied) in &results {
            assert!(
                *denied,
                "{p:?} must be denied by default, no extra_deny_read needed"
            );
        }
        assert_eq!(
            rustc.exit_code, 0,
            "the room's own toolchain must still be readable: {}",
            rustc.stderr
        );
    }

    #[test]
    fn attach_runs_with_the_given_cwd_not_the_daemons() {
        let cache = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("marker.txt"), "here\n").unwrap();
        let env = provisioned(cache.path(), workspace.path(), "wt:a");

        let out =
            attach_exec(&env, &["/bin/pwd".to_string()], &[], Some(workspace.path())).unwrap();
        assert_eq!(out.exit_code, 0, "stderr: {}", out.stderr);
        assert_eq!(
            Path::new(out.stdout.trim()).canonicalize().unwrap(),
            workspace.path().canonicalize().unwrap()
        );
    }

    /// A room may stop a process it started. A process the room did not
    /// start is outside the sandbox, and the signal is refused.
    #[test]
    fn a_room_can_signal_a_process_it_started_and_not_one_outside() {
        let outside = std::process::Command::new("/bin/sleep")
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let outside_pid = outside.id();
        struct Reap(std::process::Child);
        impl Drop for Reap {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let mut outside = Reap(outside);

        let cache = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let env = provisioned(cache.path(), workspace.path(), "wt:signal");
        let script = format!(
            "/bin/sleep 30 >/dev/null 2>&1 & child=$!; \
             if /bin/kill \"$child\"; then echo OWN_OK; wait \"$child\"; else echo OWN_FAIL; fi; \
             if /bin/kill {outside_pid}; then echo OUT_OK; else echo OUT_FAIL; fi"
        );
        let out = attach_exec(
            &env,
            &["/bin/sh".into(), "-c".into(), script],
            &[],
            Some(workspace.path()),
        )
        .unwrap();
        assert_eq!(out.exit_code, 0, "stderr: {}", out.stderr);
        assert!(
            out.stdout.contains("OWN_OK"),
            "a room can signal a process it started: {} {}",
            out.stdout,
            out.stderr
        );
        assert!(
            out.stdout.contains("OUT_FAIL") && !out.stdout.contains("OUT_OK"),
            "a room cannot signal a process outside its sandbox: {} {}",
            out.stdout,
            out.stderr
        );
        assert!(
            outside.0.try_wait().unwrap().is_none(),
            "the process outside the sandbox is still running"
        );
    }

    /// A leaked grandchild (here, a backgrounded `sleep` the shell leader
    /// does not wait for) still holds the write end of stdout open after the
    /// leader itself exits. Before the process-group kill this landed, that
    /// wedged the reader thread's `read_to_end` — and a plain `.join()` on
    /// top of it — until the grandchild finished on its own. It must instead
    /// come back within about `REAP_GRACE`, not the grandchild's lifetime,
    /// and the grandchild itself must actually be dead, not just detached.
    #[test]
    fn a_backgrounded_grandchild_holding_stdout_does_not_wedge_attach() {
        let cache = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let env = provisioned(cache.path(), workspace.path(), "wt:a");

        let started = Instant::now();
        let out = attach_exec(
            &env,
            // Backgrounded directly in this shell (not a subshell) so `$!`
            // is the grandchild's own pid, printed before the leader exits.
            &[
                "/bin/sh".to_string(),
                "-c".to_string(),
                "sleep 10 & echo $!; echo done".to_string(),
            ],
            &[],
            None,
        )
        .unwrap();
        let elapsed = started.elapsed();

        assert_eq!(out.exit_code, 0, "stderr: {}", out.stderr);
        assert!(out.stdout.contains("done"), "stdout: {:?}", out.stdout);
        assert!(
            elapsed < Duration::from_secs(4),
            "must come back within roughly REAP_GRACE ({REAP_GRACE:?}), not the grandchild's 10s lifetime: took {elapsed:?}"
        );

        let grandchild_pid: libc::pid_t = out
            .stdout
            .lines()
            .next()
            .and_then(|l| l.trim().parse().ok())
            .expect("first line must be the backgrounded sleep's pid");
        let mut alive = true;
        for _ in 0..10 {
            alive = unsafe { libc::kill(grandchild_pid, 0) } == 0;
            if !alive {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(
            !alive,
            "the backgrounded sleep must be dead too, not just the shell leader"
        );
    }

    #[test]
    fn read_capped_stops_growing_the_buffer_at_the_cap_but_still_drains_the_reader() {
        let payload = vec![b'x'; 10_000];
        let capped = read_capped(&payload[..], 100);
        assert!(capped.starts_with(&payload[..100]));
        assert!(String::from_utf8_lossy(&capped).contains("truncated at 100 bytes"));
        // The marker is the only thing appended past the cap — the buffer
        // never grew to hold the other 9,900 bytes.
        assert!(
            capped.len() < 200,
            "capped output must not scale with input: {} bytes",
            capped.len()
        );
    }

    #[test]
    fn read_capped_does_not_truncate_when_input_is_at_or_under_the_cap() {
        let payload = vec![b'y'; 100];
        let capped = read_capped(&payload[..], 100);
        assert_eq!(capped, payload, "no marker when nothing was actually cut");
    }

    #[test]
    fn agent_room_command_runs_the_argv_behind_the_agent_profile() {
        let cache_root = tempfile::tempdir().unwrap();
        let workspace_root = tempfile::tempdir().unwrap();
        let env = provisioned(cache_root.path(), workspace_root.path(), "wt:job");
        let argv = vec!["/bin/sh".to_string(), "-c".to_string(), "cargo test".to_string()];
        let job = |write_workspace| {
            agent_room_command(
                &env,
                workspace_root.path(),
                &JobRun { argv: &argv, write_workspace, grants: ExtraGrants::default() },
            )
            .unwrap()
        };
        let cmd = job(false);
        assert_eq!(cmd.program, Path::new("/usr/bin/sandbox-exec"));
        assert_eq!(PathBuf::from(&cmd.args[1]), cmd.profile_path);
        assert!(cmd.args.iter().any(|a| a == "-i") && cmd.args.iter().any(|a| a.starts_with("HOME=")));
        assert_eq!(cmd.args[cmd.args.len() - 3..], argv[..], "the argv runs last, as given");
        let read_only = std::fs::read_to_string(&cmd.profile_path).unwrap();
        let writable = std::fs::read_to_string(&job(true).profile_path).unwrap();
        assert!(read_only.contains("(deny default (with no-report))"));
        assert_eq!(read_only.matches("no-report").count(), 1, "only the default denial goes unreported: the credential rule reports");
        assert_ne!(read_only, writable, "write_workspace changes what it may write");

        let secret = vec!["cat".to_string(), ".env".to_string()];
        let refused = agent_room_command(
            &env,
            workspace_root.path(),
            &JobRun { argv: &secret, write_workspace: false, grants: ExtraGrants::default() },
        );
        assert!(refused.is_err(), "a credential-shaped argument never starts");
    }

    #[test]
    fn interactive_room_shell_builds_a_sandboxed_login_command_and_writes_its_profile() {
        let cache_root = tempfile::tempdir().unwrap();
        let workspace_root = tempfile::tempdir().unwrap();
        let env = provisioned(cache_root.path(), workspace_root.path(), "wt:pty");

        std::env::set_var("SHELL", "/bin/zsh");
        let shell = interactive_room_shell(&env, workspace_root.path(), &ExtraGrants::default()).unwrap();

        assert_eq!(shell.program, Path::new("/usr/bin/sandbox-exec"));
        assert_eq!(shell.args[0], "-f");
        assert_eq!(PathBuf::from(&shell.args[1]), shell.profile_path);
        assert!(
            shell.profile_path.is_file(),
            "profile must be written to disk before spawn"
        );
        assert!(std::fs::read_to_string(&shell.profile_path)
            .unwrap()
            .contains("(deny default (with no-report))"));
        assert!(
            shell.args.contains(&"/bin/zsh".to_string()),
            "{:?}",
            shell.args
        );
        assert!(
            shell.args.iter().any(|a| a.starts_with("HOME=")),
            "{:?}",
            shell.args
        );
        assert!(
            shell.args.iter().any(|a| a.starts_with("PATH=")),
            "{:?}",
            shell.args
        );
        assert_eq!(
            shell.args.last().unwrap(),
            "-i",
            "shell must be started interactively"
        );
    }

    /// The tools a room's shell is given run by name: their folder first on
    /// PATH and runnable under the room's profile, their variables set.
    #[test]
    fn interactive_room_shell_runs_the_tools_it_is_given_by_name() {
        use std::os::unix::fs::PermissionsExt;
        let cache_root = tempfile::tempdir().unwrap();
        let workspace_root = tempfile::tempdir().unwrap();
        let tools = tempfile::tempdir().unwrap();
        let bin = tools.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let tool = bin.join("bakery-test-tool");
        std::fs::write(&tool, "#!/bin/sh\necho \"ran: $TOOL_SAID\"\n").unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        let env = provisioned(cache_root.path(), workspace_root.path(), "wt:tools");
        let dirs = [bin];
        let vars = [("TOOL_SAID".to_string(), "hello".to_string())];
        let tools_given = ExtraGrants { exec: &dirs, path: &dirs, env: &vars, ..ExtraGrants::default() };

        let shell = interactive_room_shell(&env, workspace_root.path(), &tools_given).unwrap();
        let mut args = shell.args[..shell.args.len() - 2].to_vec();
        args.extend(["/bin/sh", "-c", "bakery-test-tool"].map(String::from));
        let out = std::process::Command::new(&shell.program)
            .args(&args)
            .current_dir(workspace_root.path())
            .output()
            .unwrap();
        let said = String::from_utf8_lossy(&out.stdout);
        assert_eq!(said.trim(), "ran: hello", "{}", String::from_utf8_lossy(&out.stderr));
    }

    #[test]
    fn interactive_room_shell_falls_back_to_zsh_when_shell_is_unset() {
        let cache_root = tempfile::tempdir().unwrap();
        let workspace_root = tempfile::tempdir().unwrap();
        let env = provisioned(cache_root.path(), workspace_root.path(), "wt:pty2");

        std::env::remove_var("SHELL");
        let shell = interactive_room_shell(&env, workspace_root.path(), &ExtraGrants::default()).unwrap();
        assert!(
            shell.args.contains(&"/bin/zsh".to_string()),
            "{:?}",
            shell.args
        );
    }

    /// The bug 03b fixes: two attaches on the same room used to write the
    /// exact same fixed path (`runtime_root/.seatbelt.sb`) — a concurrent
    /// human PTY attach (write_workspace=true) could overwrite the agent
    /// exec path's already-written read-only profile before `sandbox-exec
    /// -f` ever read it, handing the agent pipe the human's writable rules.
    /// Every attach must get its own never-shared, never-overwritten file.
    #[test]
    fn concurrent_attaches_on_the_same_room_never_share_or_clobber_a_profile_path() {
        let cache_root = tempfile::tempdir().unwrap();
        let workspace_root = tempfile::tempdir().unwrap();
        let env = provisioned(cache_root.path(), workspace_root.path(), "wt:shared");

        // The agent exec shape: read-only workspace.
        let agent_profile = write_attach_profile(
            &env,
            &ProfileSpec {
                workspace: Some(workspace_root.path()),
                write_workspace: false,
                credential_fence: true,
                extra_read: &[],
                extra_write: &[],
                extra_exec: &[],
                deny_read: &[],
                whole_machine: None,
                append_only: &[],
                local_net: false,
                xcode: false,
                readable: String::new(),
                closed: String::new(),
                key_agents: String::new(),
            },
        )
        .unwrap();
        // The human pty shape, for the *same* room, right after: this is
        // exactly the interleaving that used to clobber the file above.
        let human_profile = write_attach_profile(
            &env,
            &ProfileSpec {
                workspace: Some(workspace_root.path()),
                write_workspace: true,
                credential_fence: false,
                extra_read: &[],
                extra_write: &[],
                extra_exec: &[],
                deny_read: &[],
                whole_machine: None,
                append_only: &[],
                local_net: false,
                xcode: false,
                readable: String::new(),
                closed: String::new(),
                key_agents: String::new(),
            },
        )
        .unwrap();

        assert_ne!(
            agent_profile, human_profile,
            "each attach must get its own profile path"
        );

        // Each file on disk still reflects only its own shape — proof the
        // second write never touched the first's file. Both profiles name the
        // workspace to read it and to run what's built there;
        // `write_workspace` adds exactly one more occurrence, in the
        // write-allow block — so counting occurrences (not just presence)
        // distinguishes them.
        let agent_text = std::fs::read_to_string(&agent_profile).unwrap();
        let human_text = std::fs::read_to_string(&human_profile).unwrap();
        let workspace_subpath_line =
            format!("(subpath {})\n", quote(&canon(workspace_root.path())));
        let agent_lines = agent_text.matches(&workspace_subpath_line).count();
        assert!(agent_lines >= 1, "agent profile must read the workspace:\n{agent_text}");
        assert_eq!(
            human_text.matches(&workspace_subpath_line).count(),
            agent_lines + 1,
            "only the human profile may write the workspace:\n{agent_text}\n{human_text}"
        );

        // Neither path is reachable by a process actually sandboxed under
        // either profile: `cache_root/profiles` is outside every allow rule
        // (toolchain_root, runtime_root, workspace), so `deny default` wins.
        for profile in [&agent_profile, &human_profile] {
            assert!(
                !agent_text.contains(&quote(profile.parent().unwrap())),
                "the profiles directory itself must never be granted access"
            );
        }
    }

    fn rw_run(
        env: &SessionEnvironment,
        workspace: &Path,
        argv: &[String],
    ) -> Result<AttachOutput, String> {
        attach_exec_workspace_rw(
            env,
            workspace,
            &WorkspaceRun {
                argv,
                extra_deny_read: &[],
                timeout: Duration::from_secs(30),
                extra_read: &[],
                extra_write: &[],
                extra_exec: &[],
                whole_machine: None,
                path: &[],
                env: &[],
                append_only: &[],
            },
        )
    }

    /// The point of the RW door: builds and tests can write inside the root.
    /// The credential fence survives the writable workspace — Seatbelt still
    /// refuses to READ a credential-shaped file, and now also refuses to
    /// WRITE one (a writable workspace must not let a run clobber `.env`).
    #[test]
    fn workspace_rw_run_writes_inside_the_root_but_credentials_stay_fenced_both_ways() {
        let cache = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let env = provisioned(cache.path(), workspace.path(), "wt:rw");
        std::fs::write(workspace.path().join(".env"), "API_KEY=do-not-leak\n").unwrap();

        // Write into the workspace: allowed now.
        let out = rw_run(
            &env,
            workspace.path(),
            &[
                "/bin/sh".into(),
                "-c".into(),
                "mkdir -p target && echo built > target/out.txt".into(),
            ],
        )
        .unwrap();
        assert_eq!(out.exit_code, 0, "workspace write must succeed: {out:?}");
        assert_eq!(
            std::fs::read_to_string(workspace.path().join("target/out.txt"))
                .unwrap()
                .trim(),
            "built"
        );

        // Reading a credential-shaped file: still denied by Seatbelt.
        let out = rw_run(
            &env,
            workspace.path(),
            &["/bin/sh".into(), "-c".into(), "cat .env".into()],
        )
        .unwrap();
        assert_ne!(out.exit_code, 0, "credential read must fail: {out:?}");
        assert!(
            !out.stdout.contains("do-not-leak"),
            "credential bytes must never surface: {out:?}"
        );

        // Writing a credential-shaped file: denied too.
        let out = rw_run(
            &env,
            workspace.path(),
            &["/bin/sh".into(), "-c".into(), "echo clobber > .env".into()],
        )
        .unwrap();
        assert_ne!(out.exit_code, 0, "credential write must fail: {out:?}");
        assert_eq!(
            std::fs::read_to_string(workspace.path().join(".env")).unwrap(),
            "API_KEY=do-not-leak\n",
            "the existing .env must be untouched"
        );
    }

    /// An agent's commands can't change its runtimes' own settings in the
    /// project: neither an existing `.claude/` nor one it would create.
    #[test]
    fn workspace_rw_run_cannot_write_the_agents_settings_folders() {
        let cache = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let env = provisioned(cache.path(), workspace.path(), "wt:agent-dirs");
        std::fs::create_dir_all(workspace.path().join(".claude")).unwrap();
        std::fs::write(workspace.path().join(".claude/settings.json"), "{}\n").unwrap();
        for script in [
            "echo '{\"permissions\":{\"allow\":[\"Bash(*)\"]}}' > .claude/settings.local.json",
            "echo x >> .claude/settings.json",
            "mkdir -p .gemini && echo '{}' > .gemini/settings.json",
        ] {
            let out = rw_run(
                &env,
                workspace.path(),
                &["/bin/sh".into(), "-c".into(), script.into()],
            )
            .unwrap();
            assert_ne!(out.exit_code, 0, "{script} must fail: {out:?}");
        }
        assert!(!workspace
            .path()
            .join(".claude/settings.local.json")
            .exists());
        assert_eq!(
            std::fs::read_to_string(workspace.path().join(".claude/settings.json")).unwrap(),
            "{}\n"
        );
        assert!(!workspace.path().join(".gemini").exists());
    }

    /// Nor the repository's own git settings or hooks; its other files stay
    /// the agent's to change.
    #[test]
    fn workspace_rw_run_cannot_write_the_repositorys_git_settings() {
        let cache = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(workspace.path().join(".git/hooks")).unwrap();
        std::fs::write(workspace.path().join(".git/config"), "[core]\n").unwrap();
        let env = provisioned(cache.path(), workspace.path(), "wt:git-settings");
        for script in ["echo x >> .git/config", "echo x > .git/hooks/pre-commit"] {
            let out = rw_run(
                &env,
                workspace.path(),
                &["/bin/sh".into(), "-c".into(), script.into()],
            )
            .unwrap();
            assert_ne!(out.exit_code, 0, "{script} must fail: {out:?}");
        }
        assert_eq!(
            std::fs::read_to_string(workspace.path().join(".git/config")).unwrap(),
            "[core]\n"
        );
        assert!(!workspace.path().join(".git/hooks/pre-commit").exists());
        let out = rw_run(
            &env,
            workspace.path(),
            &["/bin/sh".into(), "-c".into(), "echo ok > notes.txt".into()],
        )
        .unwrap();
        assert_eq!(out.exit_code, 0, "{out:?}");
    }

    /// The timeout is a caller budget, not a hidden constant: a 1s budget
    /// kills a 5s sleep and says which budget it enforced.
    #[test]
    fn workspace_rw_run_enforces_the_callers_own_timeout_budget() {
        let cache = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let env = provisioned(cache.path(), workspace.path(), "wt:slow");
        let argv = ["/bin/sleep".to_string(), "5".to_string()];
        let err = attach_exec_workspace_rw(
            &env,
            workspace.path(),
            &WorkspaceRun {
                argv: &argv,
                extra_deny_read: &[],
                timeout: Duration::from_secs(1),
                extra_read: &[],
                extra_write: &[],
                extra_exec: &[],
                whole_machine: None,
                path: &[],
                env: &[],
                append_only: &[],
            },
        )
        .unwrap_err();
        assert!(err.contains("timed out after 1s"), "{err}");
    }

    /// `git status` must work inside a linked worktree's room once the
    /// repo's shared `.git` (read) and this worktree's own gitdir (write)
    /// are granted — plus the `/Library/Developer` CLT grant, since
    /// `/usr/bin/git` is an xcrun shim that re-execs from there.
    #[test]
    fn git_status_works_inside_a_linked_worktree_room_with_git_grants() {
        let host_git = |dir: &Path, args: &[&str]| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(args)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        let base = tempfile::tempdir().unwrap();
        let repo = base.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        host_git(&repo, &["init", "-q"]);
        host_git(&repo, &["config", "user.email", "t@example.invalid"]);
        host_git(&repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("a.txt"), "a\n").unwrap();
        host_git(&repo, &["add", "."]);
        host_git(&repo, &["commit", "-qm", "init"]);
        let wt = base.path().join("wt");
        host_git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                wt.to_str().unwrap(),
                "-b",
                "task/t",
            ],
        );

        let cache = tempfile::tempdir().unwrap();
        let env = provisioned(cache.path(), &wt, "wt:gitty");
        std::fs::write(wt.join("b.txt"), "b\n").unwrap();

        let shared = repo.join(".git").canonicalize().unwrap();
        let gitdir_pointer = std::fs::read_to_string(wt.join(".git")).unwrap();
        let own = PathBuf::from(
            gitdir_pointer
                .trim()
                .strip_prefix("gitdir:")
                .unwrap()
                .trim(),
        )
        .canonicalize()
        .unwrap();

        let argv = [
            "/usr/bin/git".to_string(),
            "status".to_string(),
            "--porcelain".to_string(),
        ];
        let out = attach_exec_workspace_rw(
            &env,
            &wt,
            &WorkspaceRun {
                argv: &argv,
                extra_deny_read: &[],
                timeout: Duration::from_secs(30),
                extra_read: std::slice::from_ref(&shared),
                extra_write: std::slice::from_ref(&own),
                extra_exec: &[],
                whole_machine: None,
                path: &[],
                env: &[],
                append_only: &[],
            },
        )
        .unwrap();
        assert_eq!(
            out.exit_code, 0,
            "git status must succeed in the room: {out:?}"
        );
        assert!(
            out.stdout.contains("b.txt"),
            "the untracked file must be visible: {out:?}"
        );
    }
}
