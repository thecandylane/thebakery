//! Container seats: an agent's runtime in the Bakery workspace image
//! (`containers/workspace`), a Linux container with only its worktree
//! mounted, no Docker socket, and no network unless one is named. Apple
//! container on the Mac (one light VM per seat) and Docker elsewhere (Colima,
//! a Linux server) take the same `run` flags used here, so one plan serves both.
//!
//! The deny list holds here too: a denied path is never mounted, and a denied
//! folder inside a mounted one is covered by an empty tmpfs, a denied file
//! (or a credential-named one) by an empty read-only file. Where a seat may
//! write, what the operator's git runs or follows later stays read-only.
//! Docker lays single-file covers itself; Apple container binds folders
//! only, so there the seat's init lays them ([`SEAT_INIT`]).

use crate::acp::{BoxedAcpHandle, Cleanup};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The image's unprivileged user and its home (a per-seat volume).
const AGENT_UID: &str = "1000:1000";
const AGENT_HOME: &str = "/home/agent";

/// Which container CLI runs the seat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// Apple's `container`: one light VM per container. The Mac's default.
    Apple,
    /// Docker: Colima on the Mac, Docker or Podman on a Linux server.
    Docker,
}

/// A host folder the seat sees at its own path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Share {
    pub path: PathBuf,
    pub write: bool,
    /// Where the seat sees it; `None` is its own path.
    pub target: Option<PathBuf>,
    /// Hide the credential-named files inside it, as the room hides them by
    /// name everywhere: an operator's folder granted to the seat.
    pub hide_credentials: bool,
}

impl Share {
    /// A host folder at its own path inside the seat.
    pub fn same_path(path: PathBuf, write: bool) -> Self {
        Self {
            path,
            write,
            target: None,
            hide_credentials: false,
        }
    }

    /// A folder the operator granted, at its own path, its credential files
    /// hidden.
    pub fn granted(path: PathBuf, write: bool) -> Self {
        Self {
            hide_credentials: true,
            ..Self::same_path(path, write)
        }
    }
}

/// One seat's container.
pub struct ContainerSeat<'a> {
    pub backend: Backend,
    /// The backend CLI, absolute.
    pub cli: &'a Path,
    /// For the CLI process only, never the container (Docker's `DOCKER_HOST`).
    pub host_env: &'a [(String, String)],
    pub image: &'a str,
    /// Unique per seat; also names the seat's home volume.
    pub name: &'a str,
    /// `key=value`, on the container and its home volume.
    pub labels: &'a [String],
    pub workspace: &'a Path,
    pub write_workspace: bool,
    /// More folders, each at its own path (git's shared folder, grants).
    pub shares: &'a [Share],
    /// The deny list.
    pub deny: &'a [PathBuf],
    /// `None`: loopback only.
    pub network: Option<&'a str>,
    /// Passed by name (`--env NAME`), so values never reach the CLI's argv.
    pub env: &'a [(String, String)],
    pub argv: &'a [String],
    /// Its share of the machine; `None` is the backend's own default
    /// (Apple container's is 4 CPUs and 1 GB, too little for cargo).
    pub cpus: Option<u32>,
    pub memory_gib: Option<u32>,
    /// An agent's sandbox, not the operator's editor: its runtimes' own
    /// settings folders at the workspace's top (`AGENT_STATE_DIRS`) are
    /// mounted read-only, made empty first when missing, so nothing in it
    /// can grant a runtime its own permissions.
    pub protect_agent_settings: bool,
}

fn canon(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}

/// A path as a mount field: both CLIs split mounts on `:` and `,`.
fn field(p: &Path) -> Result<String, String> {
    let text = p.to_string_lossy();
    if text.contains([':', ',']) {
        return Err(format!("{text}: a path with ':' or ',' can't be mounted"));
    }
    Ok(text.into_owned())
}

fn home_volume(name: &str) -> String {
    format!("{name}-home")
}

/// Credential-named files in `dir`, outside dependency and build folders.
/// ponytail: skips .git, node_modules and target; one walk per seat start.
fn credential_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            let name = entry.file_name();
            if !matches!(name.to_str(), Some(".git" | "node_modules" | "target")) {
                credential_files(&path, out);
            }
        } else if crate::credentials::is_hidden_credential(&path) {
            out.push(path);
        }
    }
}

/// A seat's init on Apple container, before the seat's command: it covers
/// each file it must hide with an empty read-only one and makes each file it
/// must keep read-only, then runs the command as the image's agent user
/// (uid 1000) with no capabilities and no way to gain one (`no_new_privs`),
/// so no cover can be undone. A cover that can't be laid stops the seat.
pub const SEAT_INIT: &str = r#"set -e
cover=hide
while [ "$1" != -- ]; do
  case "$1" in
    hide|keep) cover=$1 ;;
    *) if [ "$cover" = hide ]; then mount --bind -o ro /dev/null "$1"; else mount --bind -o ro "$1" "$1"; fi ;;
  esac
  shift
done
shift
exec setpriv --reuid=1000 --regid=1000 --clear-groups --inh-caps=-all --bounding-set=-all --no-new-privs -- "$@""#;

/// What a seat's start mounts, and the single files its init covers.
struct Mounts {
    args: Vec<String>,
    /// Paths inside the seat its init covers with an empty read-only file.
    hide: Vec<PathBuf>,
    /// Paths inside the seat its init makes read-only.
    keep: Vec<PathBuf>,
}

/// What the operator's git runs or follows later, in a folder a seat may
/// write: a linked worktree's `.git` file, a repository's own settings,
/// hooks and worktrees, and a linked worktree's git folder's pointers.
fn git_settings(dir: &Path) -> Vec<PathBuf> {
    [
        ".git",
        ".git/config",
        ".git/hooks",
        ".git/worktrees",
        "commondir",
        "gitdir",
        "config.worktree",
    ]
    .iter()
    .map(|rel| dir.join(rel))
    .filter(|p| p.exists() && !(p.ends_with(".git") && p.is_dir()))
    .collect()
}

/// The mounts: each share at its own path, then what hides the deny list
/// (and credential files) inside it and, where the seat may write, what
/// keeps git's settings read-only.
fn mounts(seat: &ContainerSeat) -> Result<Mounts, String> {
    let deny: Vec<PathBuf> = seat.deny.iter().map(|p| canon(p)).collect();
    let workspace = Share::granted(seat.workspace.to_path_buf(), seat.write_workspace);
    let mut out = Mounts {
        args: Vec::new(),
        hide: Vec::new(),
        keep: Vec::new(),
    };
    // Each folder mounted so far, and where the seat sees it.
    let mut mounted: Vec<(PathBuf, PathBuf)> = Vec::new();
    for share in std::iter::once(&workspace).chain(seat.shares) {
        let path = canon(&share.path);
        if let Some(denied) = deny.iter().find(|d| path.starts_with(d)) {
            return Err(format!(
                "{} is inside {}, which is always denied",
                path.display(),
                denied.display()
            ));
        }
        let target = share.target.clone().unwrap_or_else(|| path.clone());
        if seat.backend == Backend::Apple && !path.is_dir() {
            // Apple container binds folders only: a read-only file a folder
            // already mounted carries is made read-only there by the init.
            let carried = mounted
                .iter()
                .any(|(from, to)| path.strip_prefix(from).is_ok_and(|rel| to.join(rel) == target));
            if share.write || !carried {
                return Err(format!("{}: Apple container mounts folders only", path.display()));
            }
            out.keep.push(target);
            continue;
        }
        mounted.push((path.clone(), target.clone()));
        let readonly = if share.write { "" } else { ",readonly" };
        out.args.push("--mount".to_string());
        out.args.push(format!(
            "type=bind,source={},target={}{readonly}",
            field(&path)?,
            field(&target)?
        ));
        // A path inside this share, as the seat sees it.
        let inside = |d: &Path| target.join(d.strip_prefix(&path).unwrap_or(d));

        let mut hidden: Vec<PathBuf> = deny
            .iter()
            .filter(|d| d.starts_with(&path) && **d != path && d.exists())
            .cloned()
            .collect();
        if share.hide_credentials {
            credential_files(&path, &mut hidden);
        }
        for denied in hidden {
            if denied.is_dir() {
                out.args.push("--tmpfs".to_string());
                out.args.push(field(&inside(&denied))?);
            } else if seat.backend == Backend::Docker {
                out.args.push("--mount".to_string());
                out.args.push(format!(
                    "type=bind,source=/dev/null,target={},readonly",
                    field(&inside(&denied))?
                ));
            } else {
                out.hide.push(inside(&denied));
            }
        }
        if share.write {
            let mut kept = git_settings(&path);
            if seat.protect_agent_settings && path == canon(seat.workspace) {
                for dir in crate::acp::AGENT_STATE_DIRS {
                    let settings = path.join(dir);
                    if !settings.exists() {
                        // A mount point: an empty folder git never lists.
                        std::fs::create_dir_all(&settings)
                            .map_err(|e| format!("{}: {e}", settings.display()))?;
                    }
                    kept.push(settings);
                }
            }
            for kept in kept {
                if kept.is_dir() || seat.backend == Backend::Docker {
                    out.args.push("--mount".to_string());
                    out.args.push(format!(
                        "type=bind,source={},target={},readonly",
                        field(&kept)?,
                        field(&inside(&kept))?
                    ));
                } else {
                    out.keep.push(inside(&kept));
                }
            }
            // Its work there stays writable (`AGENT_WORK_DIRS`), mounted over
            // the read-only settings folder that holds it.
            if seat.protect_agent_settings && path == canon(seat.workspace) {
                for dir in crate::acp::AGENT_WORK_DIRS {
                    let work = path.join(dir);
                    std::fs::create_dir_all(&work).map_err(|e| format!("{}: {e}", work.display()))?;
                    out.args.push("--mount".to_string());
                    out.args.push(format!("type=bind,source={},target={}", field(&work)?, field(&inside(&work))?));
                }
            }
        }
    }
    Ok(out)
}

/// [`run_args`] for a shell a person types in, under a terminal of its own:
/// the CLI gives it a tty, and the container goes when the shell exits.
pub fn interactive_run_args(seat: &ContainerSeat) -> Result<Vec<String>, String> {
    let mut args = run_args(seat)?;
    args.insert(1, "--tty".to_string());
    Ok(args)
}

/// Everything after the CLI name for `run`.
pub(crate) fn run_args(seat: &ContainerSeat) -> Result<Vec<String>, String> {
    let mounts = mounts(seat)?;
    let init = !mounts.hide.is_empty() || !mounts.keep.is_empty();
    let mut args: Vec<String> = [
        "run",
        "--interactive",
        "--rm",
        "--name",
        seat.name,
        "--cap-drop",
        "ALL",
        "--read-only",
        "--tmpfs",
        "/tmp",
        "--network",
        seat.network.unwrap_or("none"),
        "--volume",
    ]
    .map(String::from)
    .to_vec();
    args.push(format!("{}:{AGENT_HOME}", home_volume(seat.name)));
    if let Some(cpus) = seat.cpus {
        args.extend(["--cpus".to_string(), cpus.to_string()]);
    }
    if let Some(gib) = seat.memory_gib {
        args.extend(["--memory".to_string(), format!("{gib}G")]);
    }
    if init {
        // Root just long enough for SEAT_INIT to lay its covers.
        args.extend(["--user", "0"].map(String::from));
        for cap in ["CAP_SYS_ADMIN", "CAP_SETUID", "CAP_SETGID", "CAP_SETPCAP"] {
            args.extend(["--cap-add".to_string(), cap.to_string()]);
        }
        args.extend(["--env".to_string(), format!("HOME={AGENT_HOME}")]);
    }
    for label in seat.labels {
        args.push("--label".to_string());
        args.push(label.clone());
    }
    args.extend(mounts.args);
    for (name, _) in seat.env {
        args.push("--env".to_string());
        args.push(name.clone());
    }
    args.push("--workdir".to_string());
    args.push(field(&canon(seat.workspace))?);
    args.push(seat.image.to_string());
    if init {
        args.extend(["sh", "-c", SEAT_INIT, "bakery-seat-init", "hide"].map(String::from));
        for p in &mounts.hide {
            args.push(field(p)?);
        }
        args.push("keep".to_string());
        for p in &mounts.keep {
            args.push(field(p)?);
        }
        args.push("--".to_string());
    }
    args.extend(seat.argv.iter().cloned());
    Ok(args)
}

/// Runs the CLI: its stdout, or its stderr as the error.
fn cli_output(cli: &Path, host_env: &[(String, String)], args: &[&str]) -> Result<String, String> {
    let out = Command::new(cli)
        .envs(host_env.iter().cloned())
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("{}: {e}", cli.display()))?;
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
    }
    Err(format!(
        "{} {}: {}",
        cli.display(),
        args.first().unwrap_or(&""),
        String::from_utf8_lossy(&out.stderr).trim()
    ))
}

fn quietly(seat: &ContainerSeat, args: &[&str]) -> Result<(), String> {
    cli_output(seat.cli, seat.host_env, args).map(drop)
}

/// An Apple container network's gateway: the host's own address on it.
fn apple_gateway(cli: &Path, name: &str) -> Result<Option<std::net::Ipv4Addr>, String> {
    let list = cli_output(cli, &[], &["network", "list", "--format", "json"])?;
    let networks: serde_json::Value =
        serde_json::from_str(&list).map_err(|e| format!("network list: {e}"))?;
    let gateway = networks
        .as_array()
        .into_iter()
        .flatten()
        .find(|n| n["id"] == name)
        .and_then(|n| n["status"]["ipv4Gateway"].as_str())
        .map(|g| g.parse().map_err(|e| format!("{name}'s gateway {g}: {e}")))
        .transpose()?;
    Ok(gateway)
}

/// The seats' network on Apple container: host-only, so no internet and no
/// DNS, made once. Returns its gateway, the one host address a seat on it
/// reaches; the node serves the seats there.
pub fn ensure_seat_network(
    cli: &Path,
    name: &str,
    labels: &[String],
) -> Result<std::net::Ipv4Addr, String> {
    if let Some(gateway) = apple_gateway(cli, name)? {
        return Ok(gateway);
    }
    let mut create = vec!["network", "create", "--internal"];
    for label in labels {
        create.extend(["--label", label.as_str()]);
    }
    create.push(name);
    // Another seat of the same project may have made it just now.
    if let Err(e) = cli_output(cli, &[], &create) {
        return apple_gateway(cli, name)?.ok_or(e);
    }
    apple_gateway(cli, name)?.ok_or_else(|| format!("network {name} has no gateway"))
}

/// The seat's home volume, made once and handed to the image's agent user
/// (a fresh volume belongs to root).
fn ensure_home(seat: &ContainerSeat) -> Result<(), String> {
    let volume = home_volume(seat.name);
    if quietly(seat, &["volume", "inspect", &volume]).is_ok() {
        return Ok(());
    }
    let mut create = vec!["volume", "create"];
    for label in seat.labels {
        create.extend(["--label", label.as_str()]);
    }
    create.push(&volume);
    quietly(seat, &create)?;
    let mount = format!("{volume}:{AGENT_HOME}");
    let mut chown = vec!["run", "--rm", "--user", "0", "--network", "none"];
    for label in seat.labels {
        chown.extend(["--label", label.as_str()]);
    }
    chown.extend([
        "--volume", &mount, seat.image, "chown", AGENT_UID, AGENT_HOME,
    ]);
    quietly(seat, &chown)
}

/// Whether any `labels`/`Labels` map in a backend's inspect JSON (Apple
/// container's or Docker's) carries `key=want`.
pub fn has_label(v: &serde_json::Value, key: &str, want: &str) -> bool {
    match v {
        serde_json::Value::Object(m) => {
            ["labels", "Labels"].iter().any(|name| {
                m.get(*name)
                    .and_then(|l| l.get(key))
                    .and_then(|x| x.as_str())
                    == Some(want)
            }) || m.values().any(|x| has_label(x, key, want))
        }
        serde_json::Value::Array(a) => a.iter().any(|x| has_label(x, key, want)),
        _ => false,
    }
}

/// A seat's container left behind when its node stopped without reaping it
/// (a crash): the CLI's client died, the container kept running, and the
/// next start would fail on its name. It is removed only when it carries
/// every one of this seat's labels.
fn clear_leftover(seat: &ContainerSeat) -> Result<(), String> {
    let inspect: &[&str] = match seat.backend {
        Backend::Apple => &["inspect", seat.name],
        Backend::Docker => &["container", "inspect", seat.name],
    };
    let Ok(text) = cli_output(seat.cli, seat.host_env, inspect) else {
        return Ok(());
    };
    let found: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
    let ours = seat.labels.iter().all(|label| {
        let (key, value) = label.split_once('=').unwrap_or((label, ""));
        has_label(&found, key, value)
    });
    if !ours {
        return Err(format!(
            "a container named {} exists and isn't this seat's; remove or rename it",
            seat.name
        ));
    }
    let remove = match seat.backend {
        Backend::Apple => ["delete", "--force", seat.name],
        Backend::Docker => ["rm", "--force", seat.name],
    };
    quietly(seat, &remove)
}

/// Start the seat's runtime in its container, stdio live like any other
/// boxed ACP child. Killing the handle kills the container by name.
pub fn spawn_container_acp(seat: &ContainerSeat) -> Result<BoxedAcpHandle, String> {
    let args = run_args(seat)?;
    clear_leftover(seat)?;
    ensure_home(seat)?;
    let mut cmd = Command::new(seat.cli);
    cmd.envs(seat.host_env.iter().cloned())
        .envs(seat.env.iter().cloned())
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    use std::os::unix::process::CommandExt;
    cmd.process_group(0);
    let child = cmd
        .spawn()
        .map_err(|e| format!("start {}: {e}", seat.cli.display()))?;
    Ok(BoxedAcpHandle::new(
        child,
        Cleanup::Container {
            cli: seat.cli.to_path_buf(),
            host_env: seat.host_env.to_vec(),
            name: seat.name.to_string(),
        },
    ))
}

/// Start a long-running server in a seat's container, detached, with the
/// seat's mounts, covers and limits: the operator's editor in a worktree's
/// container. It outlives this call; [`stop_container`] ends it.
pub fn start_container(seat: &ContainerSeat) -> Result<(), String> {
    let mut args = run_args(seat)?;
    if let Some(flag) = args.iter_mut().find(|a| *a == "--interactive") {
        *flag = "--detach".to_string();
    }
    clear_leftover(seat)?;
    ensure_home(seat)?;
    let out = Command::new(seat.cli)
        .envs(seat.host_env.iter().cloned())
        .envs(seat.env.iter().cloned())
        .args(&args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("start {}: {e}", seat.cli.display()))?;
    if out.status.success() {
        return Ok(());
    }
    Err(format!(
        "{} run: {}",
        seat.cli.display(),
        String::from_utf8_lossy(&out.stderr).trim()
    ))
}

/// Stop and remove a container this node started, by name.
pub fn stop_container(
    backend: Backend,
    cli: &Path,
    host_env: &[(String, String)],
    name: &str,
) -> Result<(), String> {
    let remove: &[&str] = match backend {
        Backend::Apple => &["delete", "--force", name],
        Backend::Docker => &["rm", "--force", name],
    };
    cli_output(cli, host_env, remove).map(drop)
}

/// The first IPv4 address in a backend's inspect or list JSON: Apple
/// container's `ipv4Address` ("192.168.128.3/24"), Docker's `IPAddress`.
pub fn first_ipv4(v: &serde_json::Value) -> Option<std::net::Ipv4Addr> {
    match v {
        serde_json::Value::Object(m) => m.iter().find_map(|(key, x)| match (key.as_str(), x) {
            ("ipv4Address" | "IPAddress", serde_json::Value::String(s)) => {
                s.split('/').next()?.parse().ok()
            }
            _ => first_ipv4(x),
        }),
        serde_json::Value::Array(a) => a.iter().find_map(first_ipv4),
        _ => None,
    }
}

/// A running container's IPv4 address on its network.
pub fn container_ipv4(
    backend: Backend,
    cli: &Path,
    host_env: &[(String, String)],
    name: &str,
) -> Result<std::net::Ipv4Addr, String> {
    let inspect: &[&str] = match backend {
        Backend::Apple => &["inspect", name],
        Backend::Docker => &["container", "inspect", name],
    };
    let found: serde_json::Value = serde_json::from_str(&cli_output(cli, host_env, inspect)?)
        .map_err(|e| format!("inspect {name}: {e}"))?;
    first_ipv4(&found).ok_or_else(|| format!("{name} has no IPv4 address; is it running?"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A seat hides every credential-shaped file but a package's
    /// certificate bundle, which its HTTPS needs.
    #[test]
    fn a_seat_hides_secrets_but_not_package_certificates() {
        let dir = tempfile::tempdir().unwrap();
        let w = dir.path();
        let files = [".env", "certs/id.pem", ".venv/lib/python3.12/site-packages/certifi/cacert.pem", ".venv/lib/python3.12/site-packages/x/.env"];
        for file in files {
            let path = w.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "").unwrap();
        }
        let mut hidden = Vec::new();
        credential_files(w, &mut hidden);
        hidden.sort();
        let want: Vec<PathBuf> = [".env", ".venv/lib/python3.12/site-packages/x/.env", "certs/id.pem"].iter().map(|f| w.join(f)).collect();
        assert_eq!(hidden, want);
    }
    use std::io::{BufRead, BufReader, Read, Write};

    #[test]
    fn a_containers_address_comes_from_either_backends_json() {
        let apple = serde_json::json!([{ "status": { "networks": [
            { "network": "bakery-seats", "ipv4Address": "192.168.128.3/24" }
        ] } }]);
        assert_eq!(first_ipv4(&apple), Some([192, 168, 128, 3].into()));
        let docker = serde_json::json!([{ "NetworkSettings": { "IPAddress": "", "Networks": {
            "bridge": { "IPAddress": "172.17.0.2" }
        } } }]);
        assert_eq!(first_ipv4(&docker), Some([172, 17, 0, 2].into()));
        assert_eq!(first_ipv4(&serde_json::json!([{}])), None);
    }

    struct Room {
        workspace: tempfile::TempDir,
        other: tempfile::TempDir,
    }

    impl Room {
        fn new() -> Self {
            Self::under(&std::env::temp_dir())
        }

        /// Colima shares only ~/Dev with its VM, so Docker's rooms live there.
        fn under(dir: &Path) -> Self {
            Self {
                workspace: tempfile::tempdir_in(dir).unwrap(),
                other: tempfile::tempdir_in(dir).unwrap(),
            }
        }

        fn seat<'a>(
            &'a self,
            backend: Backend,
            shares: &'a [Share],
            deny: &'a [PathBuf],
            env: &'a [(String, String)],
            argv: &'a [String],
        ) -> ContainerSeat<'a> {
            ContainerSeat {
                backend,
                cli: Path::new("/opt/homebrew/bin/container"),
                host_env: &[],
                image: "bakery/workspace:dev",
                name: "w2-s3-seat",
                labels: &[],
                workspace: self.workspace.path(),
                write_workspace: true,
                shares,
                deny,
                network: None,
                env,
                argv,
                cpus: None,
                memory_gib: None,
                protect_agent_settings: true,
            }
        }
    }

    fn real(p: &Path) -> String {
        p.canonicalize().unwrap().display().to_string()
    }

    /// An agent's sandbox sees its runtimes' settings folders read-only,
    /// each made (empty) when missing so it can't be created inside either;
    /// the operator's editor mounts none of them.
    #[test]
    fn a_seat_cannot_write_its_runtimes_settings_folders() {
        let room = Room::new();
        let ws = room.workspace.path().canonicalize().unwrap();
        std::fs::create_dir_all(ws.join(".claude")).unwrap();
        let argv = ["claude-agent-acp".to_string()];
        let line = run_args(&room.seat(Backend::Apple, &[], &[], &[], &argv))
            .unwrap()
            .join(" ");
        for dir in crate::acp::AGENT_STATE_DIRS {
            let settings = ws.join(dir);
            assert!(settings.is_dir(), "{dir} made as a mount point");
            let mount = format!(
                "type=bind,source={0},target={0},readonly",
                settings.display()
            );
            assert!(line.contains(&mount), "{dir} not read-only: {line}");
        }
        // Claude Code's own worktrees there stay writable.
        let work = ws.join(".claude/worktrees");
        assert!(line.contains(&format!("type=bind,source={0},target={0} ", work.display())), "{line}");
        let editor = ContainerSeat {
            protect_agent_settings: false,
            ..room.seat(Backend::Apple, &[], &[], &[], &argv)
        };
        let line = run_args(&editor).unwrap().join(" ");
        assert!(
            !line.contains(".claude"),
            "the editor's are the operator's: {line}"
        );
    }

    #[test]
    fn a_seat_runs_unprivileged_offline_and_sees_its_worktree_at_its_own_path() {
        let room = Room::new();
        let env = [("BAKERY_MCP_TOKEN".to_string(), "secret".to_string())];
        let argv = ["claude-agent-acp".to_string()];
        let args = run_args(&room.seat(Backend::Apple, &[], &[], &env, &argv)).unwrap();
        let line = args.join(" ");
        let ws = real(room.workspace.path());
        let sized = ContainerSeat {
            cpus: Some(5),
            memory_gib: Some(16),
            ..room.seat(Backend::Apple, &[], &[], &env, &argv)
        };
        let sized = run_args(&sized).unwrap().join(" ");
        assert!(sized.contains("--cpus 5 --memory 16G"), "{sized}");
        for needle in [
            "run --interactive --rm --name w2-s3-seat",
            "--cap-drop ALL",
            "--read-only --tmpfs /tmp",
            "--network none",
            "--volume w2-s3-seat-home:/home/agent",
            &format!("--mount type=bind,source={ws},target={ws} "),
            "--env BAKERY_MCP_TOKEN --workdir",
            "bakery/workspace:dev claude-agent-acp",
        ] {
            assert!(line.contains(needle), "missing {needle:?} in {line}");
        }
        assert!(!line.contains("secret"), "values never reach argv: {line}");
    }

    /// Apple container binds folders only: a read-only file a mounted
    /// folder carries is kept read-only by the init, never bound itself;
    /// Docker binds it. A file no mounted folder carries is refused.
    #[test]
    fn a_read_only_file_in_a_mounted_folder_is_kept_not_bound_on_apple() {
        let room = Room::new();
        let settings = room.other.path().join("settings.json");
        std::fs::write(&settings, "{}").unwrap();
        let shares = [
            Share { target: Some(PathBuf::from("/cfg")), ..Share::same_path(room.other.path().to_path_buf(), true) },
            Share { target: Some(PathBuf::from("/cfg/settings.json")), ..Share::same_path(settings.clone(), false) },
        ];
        let apple = run_args(&room.seat(Backend::Apple, &shares, &[], &[], &[])).unwrap().join(" ");
        assert!(!apple.contains("settings.json,readonly"), "{apple}");
        assert!(apple.contains(&format!("type=bind,source={},target=/cfg ", real(room.other.path()))), "{apple}");
        assert!(apple.contains("keep /cfg/settings.json --"), "{apple}");
        let docker = run_args(&room.seat(Backend::Docker, &shares, &[], &[], &[])).unwrap().join(" ");
        assert!(docker.contains(&format!("source={},target=/cfg/settings.json,readonly", real(&settings))), "{docker}");

        let alone = [shares[1].clone()];
        let refused = run_args(&room.seat(Backend::Apple, &alone, &[], &[], &[])).unwrap_err();
        assert!(refused.contains("folders only"), "{refused}");
    }

    #[test]
    fn shares_mount_at_their_own_path_read_only_unless_written() {
        let room = Room::new();
        let shares = [Share::same_path(room.other.path().to_path_buf(), false)];
        let line = run_args(&room.seat(Backend::Apple, &shares, &[], &[], &[]))
            .unwrap()
            .join(" ");
        let other = real(room.other.path());
        assert!(
            line.contains(&format!("type=bind,source={other},target={other},readonly")),
            "{line}"
        );
    }

    #[test]
    fn a_share_can_sit_at_its_own_target_and_its_overlays_follow_it() {
        let room = Room::new();
        let hidden = room.other.path().join("hidden");
        std::fs::create_dir_all(&hidden).unwrap();
        let shares = [Share {
            target: Some(PathBuf::from("/claude-config")),
            ..Share::same_path(room.other.path().to_path_buf(), true)
        }];
        let deny = [hidden];
        let line = run_args(&room.seat(Backend::Apple, &shares, &deny, &[], &[]))
            .unwrap()
            .join(" ");
        let other = real(room.other.path());
        assert!(
            line.contains(&format!("type=bind,source={other},target=/claude-config ")),
            "{line}"
        );
        assert!(line.contains("--tmpfs /claude-config/hidden"), "{line}");
    }

    #[test]
    fn the_deny_list_is_never_mounted_and_hidden_inside_a_mount() {
        let room = Room::new();
        let secrets = room.workspace.path().join("secrets");
        std::fs::create_dir_all(&secrets).unwrap();
        let note = room.workspace.path().join("private.txt");
        std::fs::write(&note, "x").unwrap();
        let deny = [secrets.clone(), note.clone()];

        // A folder is covered by an empty tmpfs; Docker lays an empty file
        // over a file; on Apple container the seat's init does.
        let docker = run_args(&room.seat(Backend::Docker, &[], &deny, &[], &[]))
            .unwrap()
            .join(" ");
        assert!(
            docker.contains(&format!("--tmpfs {}", real(&secrets))),
            "{docker}"
        );
        assert!(
            docker.contains(&format!("source=/dev/null,target={},readonly", real(&note))),
            "{docker}"
        );
        assert!(!docker.contains("bakery-seat-init"), "{docker}");
        let apple = run_args(&room.seat(Backend::Apple, &[], &deny, &[], &[]))
            .unwrap()
            .join(" ");
        assert!(
            apple.contains(&format!("--tmpfs {}", real(&secrets))),
            "{apple}"
        );
        assert!(
            apple.contains(&format!("bakery-seat-init hide {} keep --", real(&note))),
            "{apple}"
        );

        // A share inside a denied folder is never mounted at all.
        let shares = [Share::same_path(secrets.clone(), false)];
        let only_folder = [secrets.clone()];
        let err =
            run_args(&room.seat(Backend::Docker, &shares, &only_folder, &[], &[])).unwrap_err();
        assert!(err.contains("always denied"), "{err}");
    }

    #[test]
    fn credential_files_in_the_workspace_are_hidden_too() {
        let room = Room::new();
        let env_file = room.workspace.path().join("app/.env");
        std::fs::create_dir_all(env_file.parent().unwrap()).unwrap();
        std::fs::write(&env_file, "TOKEN=x").unwrap();
        std::fs::create_dir_all(room.workspace.path().join("node_modules/pkg")).unwrap();
        std::fs::write(room.workspace.path().join("node_modules/pkg/test.pem"), "").unwrap();

        let docker = run_args(&room.seat(Backend::Docker, &[], &[], &[], &[]))
            .unwrap()
            .join(" ");
        assert!(
            docker.contains(&format!("target={},readonly", real(&env_file))),
            "{docker}"
        );
        assert!(
            !docker.contains("test.pem"),
            "dependency folders are skipped: {docker}"
        );
        let apple = run_args(&room.seat(Backend::Apple, &[], &[], &[], &[]))
            .unwrap()
            .join(" ");
        assert!(
            apple.contains(&format!("hide {}", real(&env_file))),
            "{apple}"
        );

        // A granted folder's credential files too; a share the node chose
        // itself (git's own folders, the seat's sign-in) is left as it is.
        let granted = room.other.path().join("granted");
        std::fs::create_dir_all(&granted).unwrap();
        std::fs::write(granted.join(".env.local"), "TOKEN=x").unwrap();
        let shares = [
            Share::granted(granted.clone(), false),
            Share::same_path(room.other.path().join("granted"), false),
        ];
        let docker = run_args(&room.seat(Backend::Docker, &shares[..1], &[], &[], &[]))
            .unwrap()
            .join(" ");
        assert!(
            docker.contains(&format!(
                "target={},readonly",
                real(&granted.join(".env.local"))
            )),
            "{docker}"
        );
        let plain = run_args(&room.seat(Backend::Docker, &shares[1..], &[], &[], &[]))
            .unwrap()
            .join(" ");
        assert!(!plain.contains(".env.local"), "{plain}");
    }

    /// Where a seat may write, what the operator's git runs or follows later
    /// stays read-only: a linked worktree's `.git` file and the pointers in
    /// its own git folder (single files: the init on Apple container), and a
    /// repository's own settings, hooks and worktrees (folders: a mount).
    /// The seat starts as root only when there is a file to cover, and its
    /// command then runs as uid 1000 with no capabilities.
    #[test]
    fn a_seat_keeps_what_the_operators_git_follows_read_only() {
        let room = Room::new();
        let ws = room.workspace.path();
        std::fs::write(ws.join(".git"), "gitdir: /elsewhere\n").unwrap();
        let own = room.other.path().join("own");
        std::fs::create_dir_all(&own).unwrap();
        std::fs::write(own.join("commondir"), "../..\n").unwrap();
        std::fs::write(own.join("gitdir"), "/ws/.git\n").unwrap();
        let repo = room.other.path().join("repo");
        std::fs::create_dir_all(repo.join(".git/hooks")).unwrap();
        std::fs::write(repo.join(".git/config"), "").unwrap();
        let shares = [
            Share::same_path(own.clone(), true),
            Share::same_path(repo.clone(), true),
        ];
        let argv = ["claude-agent-acp".to_string()];

        let apple = run_args(&room.seat(Backend::Apple, &shares, &[], &[], &argv))
            .unwrap()
            .join(" ");
        for needle in [
            "--user 0 --cap-add CAP_SYS_ADMIN --cap-add CAP_SETUID --cap-add CAP_SETGID --cap-add CAP_SETPCAP --env HOME=/home/agent",
            &format!(
                "type=bind,source={0},target={0},readonly",
                real(&repo.join(".git/hooks"))
            ),
            &format!(
                "bakery-seat-init hide keep {} {} {} {} -- claude-agent-acp",
                real(&ws.join(".git")),
                real(&own.join("commondir")),
                real(&own.join("gitdir")),
                real(&repo.join(".git/config"))
            ),
        ] {
            assert!(apple.contains(needle), "missing {needle:?} in {apple}");
        }
        let docker = run_args(&room.seat(Backend::Docker, &shares, &[], &[], &argv))
            .unwrap()
            .join(" ");
        assert!(
            docker.contains(&format!(
                "type=bind,source={0},target={0},readonly",
                real(&ws.join(".git"))
            )),
            "{docker}"
        );
        assert!(!docker.contains("--user 0"), "{docker}");

        // Nothing to cover: the seat starts as the image's own user.
        std::fs::remove_file(ws.join(".git")).unwrap();
        let plain = run_args(&room.seat(Backend::Apple, &[], &[], &[], &argv))
            .unwrap()
            .join(" ");
        assert!(
            !plain.contains("--user 0") && !plain.contains("bakery-seat-init"),
            "{plain}"
        );
    }

    // ── Live: each backend with the image (machine-dependent) ──────────────
    //
    //   container build -t bakery/workspace:dev containers/workspace
    //   docker build --build-arg TARGETARCH=arm64 -t bakery/workspace:dev containers/workspace
    //   cargo test -p bakery-exec container::tests::live -- --ignored

    const LIVE_LABEL: &str = "com.bakery.w2=s3";

    struct Live {
        backend: Backend,
        cli: &'static str,
        host_env: Vec<(String, String)>,
        room: Room,
    }

    fn apple() -> Live {
        Live {
            backend: Backend::Apple,
            cli: "/opt/homebrew/bin/container",
            host_env: Vec::new(),
            room: Room::new(),
        }
    }

    /// Colima's default profile, pinned: never whatever Docker finds first.
    fn docker() -> Live {
        let home = std::env::var("HOME").unwrap();
        Live {
            backend: Backend::Docker,
            cli: "/opt/homebrew/bin/docker",
            host_env: vec![(
                "DOCKER_HOST".to_string(),
                format!("unix://{home}/.colima/default/docker.sock"),
            )],
            room: Room::under(Path::new(env!("CARGO_MANIFEST_DIR"))),
        }
    }

    impl Live {
        fn seat<'a>(
            &'a self,
            name: &'a str,
            labels: &'a [String],
            deny: &'a [PathBuf],
            argv: &'a [String],
        ) -> ContainerSeat<'a> {
            ContainerSeat {
                cli: Path::new(self.cli),
                host_env: &self.host_env,
                name,
                labels,
                ..self.room.seat(self.backend, &[], deny, &[], argv)
            }
        }

        fn drop_home(&self, name: &str) {
            let delete = match self.backend {
                Backend::Apple => "delete",
                Backend::Docker => "rm",
            };
            let _ = Command::new(self.cli)
                .envs(self.host_env.iter().cloned())
                .args(["volume", delete, &home_volume(name)])
                .output();
        }
    }

    fn speaks_acp(live: Live) {
        // A linked worktree's .git file: on Apple container the seat's init
        // runs first, and ACP still speaks through it.
        std::fs::write(
            live.room.workspace.path().join(".git"),
            "gitdir: /elsewhere\n",
        )
        .unwrap();
        let name = format!("w2-s3-acp-{}", std::process::id());
        let labels = [LIVE_LABEL.to_string()];
        let argv = ["claude-agent-acp".to_string()];
        let handle = spawn_container_acp(&live.seat(&name, &labels, &[], &argv)).unwrap();
        let (_, mut stdin, stdout, _stderr, kill) = handle.into_io_and_killer();
        writeln!(
            stdin,
            r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{"protocolVersion":1,"clientCapabilities":{{}}}}}}"#
        )
        .unwrap();
        let mut line = String::new();
        BufReader::new(stdout).read_line(&mut line).unwrap();
        kill();
        live.drop_home(&name);
        let reply: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(
            reply["result"]["agentInfo"]["name"], "@agentclientprotocol/claude-agent-acp",
            "{reply}"
        );
    }

    #[test]
    #[ignore = "needs Apple container and the bakery/workspace:dev image"]
    fn live_apple_seat_speaks_acp_through_its_container() {
        speaks_acp(apple());
    }

    #[test]
    #[ignore = "needs Colima's Docker and the bakery/workspace:dev image"]
    fn live_docker_seat_speaks_acp_through_its_container() {
        speaks_acp(docker());
    }

    /// The operator's editor in a worktree's container: openvscode-server,
    /// detached, on a host-only network, answering only with the connection
    /// token read from its mounted (read-only) token folder, and running as
    /// the image's agent user.
    ///   BAKERY_SEAT_IMAGE=<an image with openvscode-server> cargo test -p bakery-exec live_apple_editor -- --ignored
    #[test]
    #[ignore = "needs Apple container and a seat image with openvscode-server"]
    fn live_apple_editor_answers_only_with_its_token() {
        let live = apple();
        let image =
            std::env::var("BAKERY_SEAT_IMAGE").unwrap_or_else(|_| "bakery/workspace:dev".into());
        let labels = [LIVE_LABEL.to_string()];
        let cli = Path::new(live.cli);
        let gateway = ensure_seat_network(cli, "w2-s7-views", &labels).unwrap();
        let tokens = tempfile::tempdir().unwrap();
        std::fs::write(tokens.path().join("token"), "s7-test-token").unwrap();
        let shares = [Share {
            target: Some(PathBuf::from("/run/bakery-code")),
            ..Share::same_path(tokens.path().to_path_buf(), false)
        }];
        let argv: Vec<String> = [
            "openvscode-server",
            "--host",
            "0.0.0.0",
            "--port",
            "3000",
            "--connection-token-file",
            "/run/bakery-code/token",
        ]
        .map(String::from)
        .to_vec();
        let name = format!("w2-s7-editor-{}", std::process::id());
        // A linked worktree's .git file, so the init runs first.
        std::fs::write(
            live.room.workspace.path().join(".git"),
            "gitdir: /elsewhere\n",
        )
        .unwrap();
        let seat = ContainerSeat {
            image: &image,
            network: Some("w2-s7-views"),
            shares: &shares,
            // The operator's editor: their runtimes' settings are theirs to edit.
            protect_agent_settings: false,
            ..live.seat(&name, &labels, &[], &argv)
        };
        start_container(&seat).unwrap();
        let address = container_ipv4(Backend::Apple, cli, &[], &name).unwrap();
        // The token sets a cookie and redirects to strip itself from the
        // URL; following it with cookies on lands on the workbench.
        let status = |query: &str, follow: bool| {
            let mut curl = Command::new("/usr/bin/curl");
            curl.args([
                "-s",
                "-o",
                "/dev/null",
                "-w",
                "%{http_code}",
                "--max-time",
                "5",
            ]);
            if follow {
                curl.args(["-L", "-b", ""]);
            }
            let out = curl
                .arg(format!("http://{address}:3000/{query}"))
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).into_owned()
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while status("", false) == "000" && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        let with_token = status("?tkn=s7-test-token", true);
        let without = status("", false);
        let wrong = status("?tkn=wrong", true);
        let user = Command::new(live.cli)
            .args(["exec", &name, "sh", "-c", "ps -o user= -C node | sort -u"])
            .output()
            .unwrap();
        let _ = stop_container(Backend::Apple, cli, &[], &name);
        live.drop_home(&name);
        let _ = Command::new(live.cli)
            .args(["network", "delete", "w2-s7-views"])
            .output();
        assert_eq!(with_token, "200", "gateway {gateway}");
        assert_eq!(without, "403");
        assert_eq!(wrong, "403");
        assert_eq!(String::from_utf8_lossy(&user.stdout).trim(), "agent");
    }

    /// A seat its node never reaped (a crash) still runs under the seat's
    /// name: the next start removes it, but only when it carries every one
    /// of the seat's own labels.
    #[test]
    #[ignore = "needs Apple container and the bakery/workspace:dev image"]
    fn live_apple_a_leftover_seat_gives_way_only_to_its_own_seat() {
        let live = apple();
        let name = format!("w2-s3-left-{}", std::process::id());
        let labels = [LIVE_LABEL.to_string(), format!("com.bakery.seat={name}")];
        let leftover = |labels: &[String]| {
            let mut args = vec!["run", "--detach", "--name", name.as_str()];
            for label in labels {
                args.extend(["--label", label.as_str()]);
            }
            args.extend(["bakery/workspace:dev", "sleep", "300"]);
            let out = Command::new(live.cli).args(&args).output().unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        let argv = ["true".to_string()];

        // Another's container by that name stays where it is.
        leftover(&labels[..1]);
        let err = spawn_container_acp(&live.seat(&name, &labels, &[], &argv))
            .err()
            .unwrap();
        assert!(err.contains("isn't this seat's"), "{err}");
        let _ = Command::new(live.cli)
            .args(["delete", "--force", &name])
            .output();

        // The seat's own leftover gives way.
        leftover(&labels);
        let handle = spawn_container_acp(&live.seat(&name, &labels, &[], &argv)).unwrap();
        let (_, _stdin, _stdout, _stderr, kill) = handle.into_io_and_killer();
        kill();
        live.drop_home(&name);
    }

    /// A container seat's own config folder is writable and its settings
    /// in it read-only: Apple starts it, and the seat can't rewrite them.
    #[test]
    #[ignore = "needs Apple container and the bakery/workspace:dev image"]
    fn live_apple_seat_keeps_its_settings_read_only() {
        let live = apple();
        let settings = live.room.other.path().join("settings.json");
        std::fs::write(&settings, "{}").unwrap();
        let shares = [
            Share { target: Some(PathBuf::from("/claude-config")), ..Share::same_path(live.room.other.path().to_path_buf(), true) },
            Share { target: Some(PathBuf::from("/claude-config/settings.json")), ..Share::same_path(settings.clone(), false) },
        ];
        let name = format!("w1-cfg-{}", std::process::id());
        let labels = [LIVE_LABEL.to_string()];
        let script = "cat /claude-config/settings.json >/dev/null && printf 'read=yes ' || printf 'read=no '; \
            echo x > /claude-config/settings.json 2>/dev/null && printf 'rewrite=yes ' || printf 'rewrite=no '; \
            touch /claude-config/history.jsonl && printf 'folder=yes' || printf 'folder=no'";
        let argv = ["sh".to_string(), "-c".to_string(), script.to_string()];
        let seat = ContainerSeat { shares: &shares, ..live.seat(&name, &labels, &[], &argv) };
        let handle = spawn_container_acp(&seat).unwrap();
        let (_, stdin, mut stdout, _stderr, kill) = handle.into_io_and_killer();
        drop(stdin);
        let mut out = String::new();
        stdout.read_to_string(&mut out).unwrap();
        kill();
        live.drop_home(&name);
        assert_eq!(out.trim(), "read=yes rewrite=no folder=yes");
        assert_eq!(std::fs::read_to_string(&settings).unwrap(), "{}");
    }

    #[test]
    #[ignore = "needs Apple container and the bakery/workspace:dev image"]
    fn live_apple_seat_reaches_its_worktree_and_nothing_else() {
        reaches_only_its_worktree(apple());
    }

    #[test]
    #[ignore = "needs Colima's Docker and the bakery/workspace:dev image"]
    fn live_docker_seat_reaches_its_worktree_and_nothing_else() {
        reaches_only_its_worktree(docker());
    }

    fn reaches_only_its_worktree(live: Live) {
        let room = &live.room;
        std::fs::write(room.workspace.path().join("notes.txt"), "mine").unwrap();
        let secrets = room.workspace.path().join("secrets");
        std::fs::create_dir_all(&secrets).unwrap();
        std::fs::write(secrets.join("key"), "secret").unwrap();
        std::fs::write(room.workspace.path().join(".env"), "TOKEN=x").unwrap();
        std::fs::write(room.workspace.path().join(".git"), "gitdir: /elsewhere\n").unwrap();
        let elsewhere = real(room.other.path());
        let name = format!("w2-s3-reach-{}", std::process::id());
        let labels = [LIVE_LABEL.to_string()];
        let deny = [secrets.clone()];
        let probe = |label: &str, command: String| {
            format!("{command} >/dev/null 2>&1 && printf '{label}=yes ' || printf '{label}=no '; ")
        };
        let script = [
            probe("read", "cat notes.txt".to_string()),
            probe("write", "touch built".to_string()),
            probe("home", "touch \"$HOME/made\"".to_string()),
            probe("secret", "cat secrets/key".to_string()),
            probe("elsewhere", format!("ls '{elsewhere}'")),
            probe("hostroot", "ls /Users/Shared".to_string()),
            probe("net", "getent hosts example.com".to_string()),
            probe("docker", "ls /var/run/docker.sock".to_string()),
            probe("root", "touch /usr/local/bin/x".to_string()),
            probe("envfile", "grep -q TOKEN .env".to_string()),
            probe("pointer", "echo x > .git".to_string()),
            probe("uid", "test \"$(id -u)\" = 1000".to_string()),
            probe(
                "nocaps",
                "grep -q 'CapEff:.0000000000000000' /proc/self/status".to_string(),
            ),
        ]
        .concat();
        let argv = ["sh".to_string(), "-c".to_string(), script];
        let handle = spawn_container_acp(&live.seat(&name, &labels, &deny, &argv)).unwrap();
        let (_, stdin, mut stdout, _stderr, kill) = handle.into_io_and_killer();
        drop(stdin);
        let mut out = String::new();
        stdout.read_to_string(&mut out).unwrap();
        kill();
        live.drop_home(&name);
        assert_eq!(
            out.trim(),
            "read=yes write=yes home=yes secret=no elsewhere=no hostroot=no net=no docker=no root=no envfile=no pointer=no uid=yes nocaps=yes"
        );
        assert_eq!(
            std::fs::read_to_string(room.workspace.path().join(".git")).unwrap(),
            "gitdir: /elsewhere\n"
        );
        assert!(
            room.workspace.path().join("built").is_file(),
            "writes land in the worktree"
        );
    }
}
