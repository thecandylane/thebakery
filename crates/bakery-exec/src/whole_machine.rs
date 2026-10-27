//! Host full, the top of the scope dial: an agent the operator trusts with the
//! whole Mac, the way a Claude Code session running as them would be, minus
//! the deny list. The profile allows by default, then denies in this order:
//! Bakery's own state (giving this runtime its own paths back), files and
//! services that run code later outside any sandbox, container runtimes (they
//! mount around every rule), the Keychain, and last the deny list itself.
//!
//! What it cannot close: a job handed to launchd through its own API, and
//! programs the operator runs later (brew, cargo, editors), which a Host full
//! agent can change like any other file.

use crate::attach::{canon, credential_deny_regex, quote, shape_to_regex};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// The machine-wide facts a Host full profile needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WholeMachine {
    /// The operator's home: its shell startup files and LaunchAgents are
    /// named from it.
    pub home: PathBuf,
    /// Bakery's own state folder, denied whole except this runtime's own
    /// paths (its home, workspace, staged binaries).
    pub bakery_root: PathBuf,
    /// Appended to the room's PATH, so host tools resolve as they do for the
    /// operator.
    pub path: String,
}

impl WholeMachine {
    /// Whether `path` is something the operator's own sessions run later:
    /// never writable by an agent, at any level. `path` must be canonical.
    pub fn runs_later(&self, path: &Path) -> bool {
        let home = canon(&self.home);
        let text = path.to_string_lossy();
        STARTUP_FILES.iter().any(|f| path == home.join(f))
            || STARTUP_DIRS.iter().any(|d| path.starts_with(home.join(d)))
            || SYSTEM_STARTUP_DIRS.iter().any(|d| path.starts_with(d))
            || text.ends_with("/.git/config")
            || text.ends_with("/.git/hooks")
            || text.contains("/.git/hooks/")
            || (text.contains("/.git/worktrees/")
                && GIT_POINTERS
                    .iter()
                    .any(|f| text.ends_with(&format!("/{f}"))))
            || (text.ends_with("/.git") && path.is_file())
    }
}

/// The files in a linked worktree's own git folder that name another place
/// git then reads settings or hooks from.
const GIT_POINTERS: &[&str] = &["commondir", "gitdir", "config.worktree"];

/// Git's settings and hooks, and what a linked worktree's git folder points
/// at: all run when the operator next uses git in that repository.
const GIT_SETTINGS: &str =
    r#"/\.git/(config|hooks(/.*)?|worktrees/[^/]+/(commondir|gitdir|config\.worktree))$"#;

/// Closes git's settings and hooks everywhere but inside the agent's own
/// workspace, where it may create repositories (`git init`, `cargo new`).
/// The workspace's own git folder, there when the runtime started, stays
/// closed: the operator's git uses it.
pub(crate) fn git_settings_deny(workspace: Option<&Path>) -> String {
    let Some(w) = workspace.map(canon) else {
        return format!("(deny file-write* (regex #\"{GIT_SETTINGS}\"))\n");
    };
    let mut s = format!(
        "(deny file-write* (require-all (regex #\"{GIT_SETTINGS}\") (require-not (subpath {}))))\n",
        quote(&w)
    );
    let git = w.join(".git");
    if git.is_dir() {
        s.push_str(&format!(
            "(deny file-write*\n  (literal {})\n  (subpath {})\n  (subpath {}))\n",
            quote(&git.join("config")),
            quote(&git.join("hooks")),
            quote(&git.join("worktrees")),
        ));
    }
    s
}

/// A workspace the operator's own git works in later stays where it is, and
/// a linked worktree's `.git` file keeps naming its own git folder: moved
/// away and back changed, or repointed at a folder of the agent's own, the
/// operator's git would run that folder's hooks and settings.
/// The Keychain, the same secrets as ~/Library/Keychains, through securityd:
/// closed to every agent room, whatever else the room allows. Certificate
/// checks go through trustd, which stays open.
pub(crate) const KEYCHAIN_DENY: &str = concat!(
    "(deny mach-lookup\n",
    "  (global-name \"com.apple.SecurityServer\")\n",
    "  (global-name-prefix \"com.apple.securityd\"))\n",
);

pub(crate) fn workspace_git_deny(workspace: &Path) -> String {
    let workspace = canon(workspace);
    let mut s = format!("(deny file-write*\n  (literal {})\n", quote(&workspace));
    let pointer = workspace.join(".git");
    if pointer.is_file() {
        s.push_str(&format!("  (literal {})\n", quote(&pointer)));
    }
    s.push_str(")\n");
    s
}

/// Files the operator's own shells and git run, relative to their home.
const STARTUP_FILES: &[&str] = &[
    ".zshrc",
    ".zshenv",
    ".zprofile",
    ".zlogin",
    ".zlogout",
    ".bashrc",
    ".bash_profile",
    ".bash_login",
    ".bash_logout",
    ".profile",
    ".gitconfig",
];

/// Folders whose contents run later: login agents, shell plugins, git's own
/// settings.
const STARTUP_DIRS: &[&str] = &[
    "Library/LaunchAgents",
    ".config/fish",
    ".oh-my-zsh",
    ".config/git",
];

const SYSTEM_STARTUP_DIRS: &[&str] = &[
    "/Library/LaunchAgents",
    "/Library/LaunchDaemons",
    // cron and at tables (`crontab` is setuid, but keeps this profile).
    "/private/var/at",
];

/// Certificate containers: CA bundles share these names, so they stay a
/// workspace-only fence. Every other credential name is denied everywhere.
const CERT_SHAPES: &[&str] = &["*.pem", "*.p12", "*.pfx"];

/// Sockets no sandbox reaches, its network open or not: Docker's, Podman's
/// and Colima's (whoever reaches one can mount any folder its VM shares), and
/// Claude Code's sessions' own (`/tmp/cc-socks/<pid>.sock`: the operator's
/// sessions take a message from whoever connects).
pub(crate) const CLOSED_SOCKETS: &str = concat!(
    r#"(deny network-outbound (remote unix-socket (path-regex #"/(docker|podman)\.sock$")))"#,
    "\n",
    r#"(deny network-outbound (remote unix-socket (path-regex #"/tmp/cc-socks")))"#,
);

/// Key agents' sockets: ssh-agent's (launchd's, a started one's,
/// 1Password's, Secretive's) and gpg-agent's. Whoever reaches one signs as
/// the operator, with their keys.
const KEY_AGENT_SOCKETS: &[&str] = &[
    r"/tmp/com\.apple\.launchd\.[^/]+/Listeners$",
    r"/tmp/ssh-[^/]+/agent\.[0-9]+$",
    r"/2BUA8C4S2C\.com\.1password/t/agent\.sock$",
    r"/com\.maxgoedjen\.Secretive\.SecretAgent/Data/socket\.ssh$",
    r"/\.gnupg/S\.gpg-agent[^/]*$",
];

/// The key agents closed to a sandbox, every kind of it alike (a room, a
/// native runtime's box, the whole machine): their sockets, and wherever an
/// SSH_AUTH_SOCK points, this process's own or one in the sandbox's
/// variables. Nothing when its variables open them
/// (`BAKERY_KEY_AGENTS=open`: the operator's to give an agent).
pub(crate) fn key_agent_sockets(env: &[(String, String)]) -> String {
    if env.iter().any(|(k, v)| k == crate::acp::KEY_AGENTS_ENV && v == "open") {
        return String::new();
    }
    let mut rules: Vec<String> = KEY_AGENT_SOCKETS
        .iter()
        .map(|re| format!(r#"(deny network-outbound (remote unix-socket (path-regex #"{re}")))"#))
        .collect();
    let given = env.iter().filter(|(k, _)| k == "SSH_AUTH_SOCK").map(|(_, v)| PathBuf::from(v));
    let socks = std::env::var_os("SSH_AUTH_SOCK").map(PathBuf::from).into_iter().chain(given);
    for sock in socks.filter(|p| p.is_absolute()) {
        for path in [canon(&sock), sock] {
            rules.push(format!("(deny network-outbound (remote unix-socket (path-literal {})))", quote(&path)));
        }
    }
    rules.join("\n") + "\n"
}

fn names_everywhere_regex() -> String {
    let alternation = crate::credentials::CREDENTIAL_SHAPES
        .iter()
        .filter(|shape| !CERT_SHAPES.contains(shape))
        .map(|shape| shape_to_regex(shape))
        .collect::<Vec<_>>()
        .join("|");
    format!("^/(.*/)?({alternation})$")
}

fn block(s: &mut String, head: &str, rules: impl IntoIterator<Item = String>) {
    let rules: Vec<String> = rules.into_iter().collect();
    if rules.is_empty() {
        return;
    }
    s.push_str(head);
    s.push('\n');
    for rule in rules {
        s.push_str("  ");
        s.push_str(&rule);
        s.push('\n');
    }
    s.push_str(")\n");
}

/// Allow by default, then deny. `read` and `write` are this runtime's own
/// paths (toolchain, staged binaries, its home, Bakery-owned credentials);
/// only the ones inside the Bakery root need saying, since everything else
/// is allowed already. `deny` is the deny list, emitted last.
pub(crate) fn profile(
    m: &WholeMachine,
    read: &[PathBuf],
    write: &[PathBuf],
    workspace: Option<&Path>,
    write_workspace: bool,
    deny: &[PathBuf],
) -> String {
    let root = canon(&m.bakery_root);
    let home = canon(&m.home);
    let workspace = workspace.map(canon);
    let inside = |paths: &[PathBuf]| -> Vec<PathBuf> {
        paths
            .iter()
            .map(|p| canon(p))
            .filter(|p| p.starts_with(&root))
            .collect()
    };
    let mut read = inside(read);
    let mut write = inside(write);
    if let Some(w) = workspace.as_ref().filter(|w| w.starts_with(&root)) {
        if write_workspace {
            write.push(w.clone());
        } else {
            read.push(w.clone());
        }
    }

    let mut s = String::from("(version 1)\n(allow default)\n");
    s.push_str(&format!(
        "(deny file-read* file-write* (subpath {}))\n",
        quote(&root)
    ));
    // Node and git stat every ancestor of a path they open.
    let mut seen = HashSet::new();
    let ancestors: Vec<String> = read
        .iter()
        .chain(write.iter())
        .flat_map(|p| p.ancestors())
        .filter(|a| a.starts_with(&root) && seen.insert(a.to_path_buf()))
        .map(|a| format!("(literal {})", quote(a)))
        .collect();
    block(&mut s, "(allow file-read-metadata", ancestors);
    block(
        &mut s,
        "(allow file-read*",
        read.iter().map(|p| format!("(subpath {})", quote(p))),
    );
    block(
        &mut s,
        "(allow file-read* file-write*",
        write.iter().map(|p| format!("(subpath {})", quote(p))),
    );
    if let Some(w) = &workspace {
        if !write_workspace {
            s.push_str(&format!("(deny file-write* (subpath {}))\n", quote(w)));
        }
        block(
            &mut s,
            "(deny file-write*",
            crate::acp::AGENT_STATE_DIRS
                .iter()
                .map(|d| format!("(subpath {})", quote(&w.join(d)))),
        );
        // Its work there stays writable (`AGENT_WORK_DIRS`).
        if write_workspace {
            block(
                &mut s,
                "(allow file-write*",
                crate::acp::AGENT_WORK_DIRS
                    .iter()
                    .map(|d| format!("(subpath {})", quote(&w.join(d)))),
            );
        }
        s.push_str(&workspace_git_deny(w));
    }

    // What the operator's own sessions run later, outside any sandbox: the
    // same list `WholeMachine::runs_later` answers for node tools.
    let mut later: Vec<String> = STARTUP_FILES
        .iter()
        .map(|f| format!("(literal {})", quote(&home.join(f))))
        .collect();
    later.extend(
        STARTUP_DIRS
            .iter()
            .map(|d| format!("(subpath {})", quote(&home.join(d)))),
    );
    later.extend(
        SYSTEM_STARTUP_DIRS
            .iter()
            .map(|d| format!("(subpath {})", quote(Path::new(d)))),
    );
    block(&mut s, "(deny file-write*", later);
    s.push_str(&git_settings_deny(workspace.as_deref()));
    s.push_str("(deny appleevent-send)\n");
    // Launching an app (`open -a Terminal x.command`), a login item or an
    // Apple container runs it outside this sandbox.
    s.push_str(concat!(
        "(deny mach-lookup\n",
        "  (global-name \"com.apple.coreservices.launchservicesd\")\n",
        "  (global-name-prefix \"com.apple.lsd.\")\n",
        "  (global-name-prefix \"com.apple.backgroundtaskmanagement\")\n",
        "  (global-name-prefix \"com.apple.container\"))\n",
    ));
    s.push_str(CLOSED_SOCKETS);
    s.push('\n');
    s.push_str(KEYCHAIN_DENY);

    // The deny list: the last word.
    block(
        &mut s,
        "(deny file-read* file-write*",
        deny.iter()
            .map(|p| format!("(subpath {})", quote(&canon(p)))),
    );
    s.push_str(&format!(
        "(deny file-read* file-write* (regex #\"{}\"))\n",
        names_everywhere_regex()
    ));
    if let Some(w) = &workspace {
        let fence = credential_deny_regex(w);
        s.push_str(&format!("(deny file-read* (regex #\"{fence}\"))\n"));
        if write_workspace {
            s.push_str(&format!("(deny file-write* (regex #\"{fence}\"))\n"));
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn machine() -> WholeMachine {
        WholeMachine {
            home: PathBuf::from("/Users/operator"),
            bakery_root: PathBuf::from("/Users/operator/.bakery"),
            path: String::new(),
        }
    }

    fn sample(write_workspace: bool) -> String {
        profile(
            &machine(),
            &[
                PathBuf::from("/Users/operator/.bakery/cache/acp-bins/abc"),
                PathBuf::from("/opt/tool"),
            ],
            &[PathBuf::from(
                "/Users/operator/.bakery/cache/seats/s/baker/home",
            )],
            Some(Path::new("/Users/operator/.bakery/worktrees/w")),
            write_workspace,
            &[
                PathBuf::from("/Users/operator/.ssh"),
                PathBuf::from("/Users/operator/Dev/secret"),
            ],
        )
    }

    #[test]
    fn the_bakery_root_is_denied_and_only_this_runtimes_own_paths_come_back() {
        let p = sample(true);
        let root = p.find(r#"(deny file-read* file-write* (subpath "/Users/operator/.bakery"))"#);
        let own = p.find(r#"(subpath "/Users/operator/.bakery/cache/seats/s/baker/home")"#);
        assert!(root.is_some() && own.is_some() && root < own, "{p}");
        assert!(
            p.contains(r#"(subpath "/Users/operator/.bakery/worktrees/w")"#),
            "{p}"
        );
        assert!(
            !p.contains(r#"(subpath "/opt/tool")"#),
            "paths outside the Bakery root are allowed already:\n{p}"
        );
        assert!(
            p.contains(r#"(literal "/Users/operator/.bakery/cache")"#),
            "ancestors inside the root need metadata:\n{p}"
        );
    }

    #[test]
    fn the_deny_list_is_the_last_word() {
        let p = sample(true);
        let entry = p.find(r#"(subpath "/Users/operator/Dev/secret")"#).unwrap();
        let head = p[..entry].rfind("(deny file-read* file-write*").unwrap();
        assert!(head > p.rfind("(allow ").unwrap(), "{p}");
    }

    #[test]
    fn startup_files_apple_events_containers_and_the_keychain_are_closed() {
        let p = sample(true);
        for needle in [
            r#"(literal "/Users/operator/.zshrc")"#,
            r#"(literal "/Users/operator/.gitconfig")"#,
            r#"(subpath "/Users/operator/Library/LaunchAgents")"#,
            r#"(subpath "/private/var/at")"#,
            r#"/\.git/(config|hooks(/.*)?|worktrees/[^/]+/(commondir|gitdir|config\.worktree))$"#,
            r#"(require-not (subpath "/Users/operator/.bakery/worktrees/w"))"#,
            r#"(literal "/Users/operator/.bakery/worktrees/w")"#,
            "(deny appleevent-send)",
            "com.apple.coreservices.launchservicesd",
            r#"(global-name-prefix "com.apple.container")"#,
            r#"(docker|podman)\.sock$"#,
            r#"(global-name "com.apple.SecurityServer")"#,
        ] {
            assert!(p.contains(needle), "missing {needle}:\n{p}");
        }
    }

    #[test]
    fn a_read_only_workspace_stays_read_only() {
        let p = sample(false);
        assert!(
            p.contains(r#"(deny file-write* (subpath "/Users/operator/.bakery/worktrees/w"))"#),
            "{p}"
        );
        assert!(
            !p.contains("(deny file-write* (regex"),
            "no write fence is needed where nothing is writable:\n{p}"
        );
    }

    #[test]
    fn runs_later_names_the_same_things_the_profile_closes() {
        let m = machine();
        for later in [
            "/Users/operator/.zshrc",
            "/Users/operator/Library/LaunchAgents/x.plist",
            "/private/var/at/tabs/operator",
            "/Users/operator/Dev/app/.git/hooks/pre-commit",
            "/Users/operator/Dev/app/.git/config",
            "/Users/operator/Dev/app/.git/worktrees/w/commondir",
            "/Users/operator/Dev/app/.git/worktrees/w/gitdir",
            "/Users/operator/Dev/app/.git/worktrees/w/config.worktree",
        ] {
            assert!(m.runs_later(Path::new(later)), "{later}");
        }
        let worktree = tempfile::tempdir().unwrap();
        let pointer = worktree.path().join(".git");
        std::fs::write(&pointer, "gitdir: /elsewhere\n").unwrap();
        assert!(m.runs_later(&pointer), "a linked worktree's .git file");
        assert!(
            !m.runs_later(&worktree.path().join("src/.git")),
            "no such file"
        );
        for ordinary in [
            "/Users/operator/.zshrc.bak",
            "/Users/operator/Dev/app/src/config",
        ] {
            assert!(!m.runs_later(Path::new(ordinary)), "{ordinary}");
        }
    }

    #[test]
    fn certificates_are_fenced_only_inside_the_workspace() {
        let everywhere = names_everywhere_regex();
        assert!(everywhere.contains(r"\.[Ee][Nn][Vv]"), "{everywhere}");
        assert!(
            !everywhere.contains("[Pp][Ee][Mm]"),
            "CA bundles are .pem files:\n{everywhere}"
        );
    }
}
