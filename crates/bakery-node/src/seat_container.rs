//! A conversation's runtime in a container: the Container and Locked levels
//! of the scope dial. The seat runs in the Bakery workspace image on Apple
//! container, on the seats' host-only network, and reaches only the node's
//! gateway there: MCP, and a proxy to model APIs and package registries. It
//! sees its worktree (and git's folders for it) and, at the Container level,
//! the folders granted to it, each at its own path.

use crate::machine_access::{Reach, SeatSize, SpawnGrants};
use crate::state::AppState;
use bakery_exec::{has_label, BoxedAcpHandle, ContainerSeat, Share};
use std::path::{Path, PathBuf};

const LABEL: &str = "com.bakery=seat";

/// What a thread's containers are sealed in with: its project, or the
/// thread alone when it has none.
pub(crate) fn scope_of(state: &AppState, stream_id: &str) -> String {
    let project = match crate::terminals::project_of_stream(stream_id) {
        Some(project) => Some(project.to_string()),
        None => crate::state::socket_project(&state.heart.lock().unwrap(), stream_id).0,
    };
    match project {
        Some(project) => format!("project:{project}"),
        None => format!("thread:{stream_id}"),
    }
}

/// A scope's own internal network, its name and labels: Apple's networks
/// are isolated from one another, so one project's containers never reach
/// another's. (The seats' first network, `bakery-seats`, is left as it was.)
/// A project's services run on it too.
pub(crate) fn network_of(scope: &str) -> (String, Vec<String>) {
    let hash = &blake3::hash(scope.as_bytes()).to_hex()[..16];
    let (prefix, label) = match scope.split_once(':') {
        Some(("project", project)) => ("p", format!("com.bakery.project={project}")),
        Some((_, thread)) => ("t", format!("com.bakery.stream={thread}")),
        None => ("t", format!("com.bakery.stream={scope}")),
    };
    (format!("bakery-{prefix}-{hash}"), vec![LABEL.to_string(), label])
}
/// Where a container Claude seat finds its sign-in.
const CLAUDE_CONFIG_AT: &str = "/claude-config";

/// The image seats run in; `BAKERY_SEAT_IMAGE` names another build.
pub(crate) fn image() -> String {
    std::env::var("BAKERY_SEAT_IMAGE").unwrap_or_else(|_| "bakery/workspace:dev".to_string())
}

/// The seat image's recipe, built into the node so a build needs no
/// checkout; a build is current when it carries this recipe's hash.
pub(crate) const DOCKERFILE: &str = include_str!("../../../containers/workspace/Dockerfile");

/// The image's own PATH, from its recipe.
fn image_path() -> &'static str {
    DOCKERFILE
        .split_whitespace()
        .find_map(|word| word.strip_prefix("PATH="))
        .unwrap_or("/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin")
}

/// A seat's own push helper and gh stand-in (`remote_ask`), in Node: the
/// node's own binary, which a room on this Mac runs, is macOS.
const SEAT_BIN: [(&str, &str); 2] = [
    ("git-remote-bakery", include_str!("../../../containers/seat-bin/git-remote-bakery")),
    ("gh", include_str!("../../../containers/seat-bin/gh")),
];
/// Where they sit in a seat, first on its PATH.
const SEAT_BIN_AT: &str = "/opt/bakery/bin";

/// The seat helpers' folder on this Mac, written as this node carries them.
fn seat_bin(state: &AppState) -> Result<PathBuf, String> {
    use std::os::unix::fs::PermissionsExt;
    let dir = crate::state::cache_root(state).join("seat-bin");
    std::fs::create_dir_all(&dir).map_err(|e| format!("seat helpers: {e}"))?;
    for (name, text) in SEAT_BIN {
        let path = dir.join(name);
        if std::fs::read_to_string(&path).ok().as_deref() != Some(text) {
            std::fs::write(&path, text).map_err(|e| format!("seat helpers: {e}"))?;
        }
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).map_err(|e| format!("seat helpers: {e}"))?;
    }
    Ok(dir)
}

/// The Bakery's patch to the Claude connector release bakery-client pins
/// (build.rs), which the seat image applies as pnpm applies it on the host.
const ADAPTER_PATCH: &str = include_str!(concat!(env!("OUT_DIR"), "/claude-agent-acp.patch"));

/// The image's recipe: its Dockerfile and the host's Claude connector (its
/// release and the Bakery's patch), so upgrading the connector rebuilds it.
fn recipe() -> String {
    recipe_of(DOCKERFILE, crate::claude_native::ADAPTER_VERSION, ADAPTER_PATCH)
}

fn recipe_of(dockerfile: &str, adapter: &str, patch: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    for part in [dockerfile, adapter, patch] {
        hasher.update(part.as_bytes());
        hasher.update(&[0]);
    }
    hasher.finalize().to_hex()[..16].to_string()
}

/// Whether the seat image is built from this node's recipe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ImageState {
    Missing,
    /// Built from an older recipe.
    Stale,
    Current,
}

impl ImageState {
    fn as_str(self) -> &'static str {
        match self {
            ImageState::Missing => "missing",
            ImageState::Stale => "stale",
            ImageState::Current => "current",
        }
    }
}

pub(crate) fn image_state() -> ImageState {
    let Ok(cli) = apple_cli() else {
        return ImageState::Missing;
    };
    match std::process::Command::new(cli)
        .args(["image", "inspect", &image()])
        .stdin(std::process::Stdio::null())
        .output()
    {
        Ok(out) if out.status.success() => {
            let inspect = serde_json::from_slice(&out.stdout).unwrap_or_default();
            if has_label(&inspect, "com.bakery.recipe", &recipe()) {
                ImageState::Current
            } else {
                ImageState::Stale
            }
        }
        _ => ImageState::Missing,
    }
}

/// Every container command needs Apple container's own service running.
fn service_up(cli: &Path) -> Result<(), String> {
    let status = std::process::Command::new(cli)
        .args(["system", "status"])
        .stdin(std::process::Stdio::null())
        .output();
    let running = status.is_ok_and(|out| {
        out.status.success()
            && String::from_utf8_lossy(&out.stdout)
                .lines()
                .any(|l| l.starts_with("status") && l.contains("running"))
    });
    if running {
        Ok(())
    } else {
        Err(
            "Setup needed: Apple container isn't running; start it with `container system start`"
                .to_string(),
        )
    }
}

/// Ready when the seat image is built from this node's recipe. Missing or
/// stale, it starts building on its own (the operator's answer), and its
/// progress shows in Settings → Agents.
fn image_ready(state: &AppState) -> Result<(), String> {
    service_up(&apple_cli()?)?;
    match image_state() {
        ImageState::Current => Ok(()),
        now => {
            let building = match start_build(state) {
                Ok(_) => "it's building now; Settings → Agents shows how far it is".to_string(),
                Err(e) => e,
            };
            Err(format!("The seat image is {} ({building}). Try again once it's built.", now.as_str()))
        }
    }
}

/// The seat image build running now, by its pane: one at a time.
static BUILD: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// The seat image build: the one running, or a new one in a pane the
/// operator sees (Settings → Agents), which a cancel closes with Ctrl-C
/// first so Apple's CLI cleans up after itself. Never while bakery.sh holds
/// its lock: a deploy stays fast.
pub(crate) fn start_build(state: &AppState) -> Result<crate::terminals::TerminalInfo, String> {
    let mut running = BUILD.lock().unwrap();
    if let Some(handle) = running.as_deref().and_then(|id| state.terminals.lock().unwrap().get(id)) {
        if handle.is_alive() {
            return Ok(handle.info());
        }
    }
    if deploy_running(state) {
        return Err("a deploy is running; the seat image builds once it's done".to_string());
    }
    let cli = apple_cli()?;
    service_up(&cli)?;
    let dir = crate::state::cache_root(state).join("seat-image");
    std::fs::create_dir_all(&dir)
        .and_then(|_| std::fs::write(dir.join("Dockerfile"), DOCKERFILE))
        .and_then(|_| std::fs::write(dir.join("claude-agent-acp.patch"), ADAPTER_PATCH))
        .map_err(|e| format!("{}: {e}", dir.display()))?;
    let args = vec![
        "build".to_string(),
        "--label".to_string(),
        format!("com.bakery.recipe={}", recipe()),
        "--build-arg".to_string(),
        format!("CLAUDE_ACP_VERSION={}", crate::claude_native::ADAPTER_VERSION),
        "--tag".to_string(),
        image(),
        dir.display().to_string(),
    ];
    let pane = host_terminal(state, "container build", cli, args)?;
    if let Some(handle) = state.terminals.lock().unwrap().get(&pane.terminal_id) {
        handle.interrupt_first();
    }
    *running = Some(pane.terminal_id.clone());
    Ok(pane)
}

/// Whether bakery.sh holds its lock (a deploy, rollback or adapter upgrade):
/// its folder is there and its owner, when it names one, is alive, as
/// bakery.sh itself decides.
fn deploy_running(state: &AppState) -> bool {
    let lock = state.base.join("deploy").join(".lock");
    if !lock.is_dir() {
        return false;
    }
    match std::fs::read_to_string(lock.join("pid")).ok().and_then(|pid| pid.trim().parse::<libc::pid_t>().ok()) {
        // kill(pid, 0) only asks whether the process is there.
        Some(pid) => (unsafe { libc::kill(pid, 0) }) == 0,
        None => true,
    }
}

/// Whether anything runs in the seat image: a conversation set to the
/// Container or Locked reach, or a project whose sandbox is a container.
fn containers_in_use(state: &AppState) -> bool {
    let set = state
        .node
        .lock()
        .unwrap()
        .machine_access
        .reach
        .values()
        .any(|r| matches!(r, Reach::Container | Reach::Locked));
    set || crate::catalog::known_projects(state).is_ok_and(|projects| projects.iter().any(|p| p.stacks.sandbox == "container"))
}

/// Whether to build without being asked: a stale image was built because
/// something used it (a container, or Code); a missing one only once
/// something runs in a container.
fn build_wanted(now: ImageState, in_use: bool) -> bool {
    match now {
        ImageState::Current => false,
        ImageState::Stale => true,
        ImageState::Missing => in_use,
    }
}

/// After the node is up, and once any deploy that started it is done: the
/// seat image is built in the background when it's wanted, so a connector
/// upgrade doesn't wait for the operator.
pub(crate) fn keep_image_current(state: AppState) {
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(60));
        while deploy_running(&state) {
            std::thread::sleep(std::time::Duration::from_secs(15));
        }
        if apple_cli().and_then(|cli| service_up(&cli)).is_err() {
            return;
        }
        if build_wanted(image_state(), containers_in_use(&state)) {
            if let Err(e) = start_build(&state) {
                eprintln!("seat image: {e}");
            }
        }
    });
}

#[derive(serde::Serialize, schemars::JsonSchema)]
pub(crate) struct SeatImage {
    /// The image seats run in.
    image: String,
    state: ImageState,
}

/// `GET /containers/seat-image`: whether the seat image is built from this
/// node's recipe.
pub(crate) async fn seat_image() -> axum::Json<SeatImage> {
    let state = crate::actor::spawn_blocking(image_state)
        .await
        .unwrap_or(ImageState::Missing);
    axum::Json(SeatImage {
        image: image(),
        state,
    })
}

/// `POST /containers/seat-image/build`: build the seat image from this
/// node's recipe in a terminal pane the operator sees, or show the build
/// already running.
pub(crate) async fn build_seat_image(
    axum::extract::State(st): axum::extract::State<AppState>,
) -> Result<axum::Json<crate::terminals::TerminalInfo>, (axum::http::StatusCode, String)> {
    use axum::http::StatusCode;
    crate::actor::spawn_blocking(move || start_build(&st))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map(axum::Json)
        .map_err(|e| (StatusCode::CONFLICT, e))
}

/// A terminal pane on the accounts stream, running `program` as the operator
/// with a minimal environment, never the node's.
fn host_terminal(
    st: &AppState,
    name: &str,
    program: PathBuf,
    args: Vec<String>,
) -> Result<crate::terminals::TerminalInfo, String> {
    let stream_id = crate::terminals::SYSTEM_ACCOUNTS_STREAM_ID.to_string();
    let pending = crate::state::admit_pty(st, &stream_id, "host").map_err(|(_, e)| e)?;
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    let spec = crate::terminals::SpawnSpec {
        stream_id: stream_id.clone(),
        name: Some(name.to_string()),
        mode: "host".to_string(),
        program,
        args,
        cwd: PathBuf::from(&home),
        profile_path: None,
        login_command: None,
        env: Some(vec![
            ("HOME".to_string(), home),
            ("PATH".to_string(), "/usr/bin:/bin".to_string()),
            (
                "USER".to_string(),
                std::env::var("USER").unwrap_or_default(),
            ),
        ]),
    };
    crate::terminals::spawn_terminal(st, spec, pending).map_err(|e| format!("{stream_id}: {e}"))
}

async fn host_pane(
    st: AppState,
    name: &str,
    program: PathBuf,
    args: Vec<String>,
) -> Result<axum::Json<crate::terminals::TerminalInfo>, (axum::http::StatusCode, String)> {
    use axum::http::StatusCode;
    let name = name.to_string();
    crate::actor::spawn_blocking(move || host_terminal(&st, &name, program, args))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map(axum::Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))
}

/// Host folder the "Sign in for containers" terminal writes. Seats do not
/// mount it. A container subscription seat uses the Mac sign-in through the door.
pub(crate) fn claude_config(state: &AppState) -> PathBuf {
    crate::state::cache_root(state)
        .join("claude-container")
        .join("config")
}

/// The terminal command that signs Claude in for containers, once.
pub(crate) fn claude_sign_in_command(state: &AppState) -> Result<Vec<String>, String> {
    let cli = apple_cli()?;
    let config = claude_config(state);
    std::fs::create_dir_all(&config).map_err(|e| format!("{}: {e}", config.display()))?;
    Ok(vec![
        cli.display().to_string(),
        "run".into(),
        "--interactive".into(),
        "--tty".into(),
        "--rm".into(),
        "--label".into(),
        LABEL.into(),
        "--volume".into(),
        format!("{}:{CLAUDE_CONFIG_AT}", config.display()),
        "--env".into(),
        format!("CLAUDE_CONFIG_DIR={CLAUDE_CONFIG_AT}"),
        image(),
        "claude".into(),
        "/login".into(),
    ])
}

/// `POST /machine-access/claude-container/sign-in`: the operator signs
/// Claude in for container seats, once, in a terminal pane they see. Hidden
/// from the Baker. The pane gets a minimal environment, never the node's.
pub(crate) async fn sign_in(
    axum::extract::State(st): axum::extract::State<AppState>,
) -> Result<axum::Json<crate::terminals::TerminalInfo>, (axum::http::StatusCode, String)> {
    use axum::http::StatusCode;
    let ready = st.clone();
    crate::actor::spawn_blocking(move || image_ready(&ready))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map_err(|e| (StatusCode::CONFLICT, e))?;
    let argv = claude_sign_in_command(&st).map_err(|e| (StatusCode::CONFLICT, e))?;
    host_pane(
        st,
        "claude-container-login",
        PathBuf::from(&argv[0]),
        argv[1..].to_vec(),
    )
    .await
}

pub(crate) fn apple_cli() -> Result<PathBuf, String> {
    crate::state::resolve_executable_path("container").map_err(|_| {
        "Setup needed: a Container reach needs Apple container (`container` on the node's PATH)"
            .to_string()
    })
}

/// The command a runtime runs as in the image. Only runtimes the image
/// carries can start in a container.
fn command(model_id: &str) -> Result<Vec<String>, String> {
    if model_id.eq_ignore_ascii_case(crate::claude_native::ACCOUNT_ID) {
        return Ok(vec!["claude-agent-acp".to_string()]);
    }
    Err(format!(
        "Setup needed: {model_id} has no Linux build in the seat image; give this agent a Host reach"
    ))
}

/// This machine's memory in GiB (8 when it can't be read).
fn host_memory_gib() -> u32 {
    let bytes = std::process::Command::new("/usr/sbin/sysctl")
        .args(["-n", "hw.memsize"])
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .and_then(|text| text.trim().parse::<u64>().ok());
    bytes.map_or(8, |b| (b >> 30) as u32)
}

fn host_cores() -> u32 {
    std::thread::available_parallelism().map_or(4, |n| n.get() as u32)
}

/// What a container seat gets of this Mac unless the operator says
/// otherwise: half its cores (two at least), and a quarter of its memory
/// between 4 and 16 GB, room for cargo's biggest links.
pub(crate) fn seat_default() -> SeatSize {
    let cores = host_cores();
    SeatSize {
        cpus: (cores / 2).max(2).min(cores),
        memory_gib: (host_memory_gib() / 4).clamp(4, 16),
    }
}

/// One seat's share: the operator's setting, or the default.
pub(crate) fn seat_size(state: &AppState, stream_id: &str) -> SeatSize {
    let stored = state
        .node
        .lock()
        .unwrap()
        .machine_access
        .seat_size
        .get(stream_id)
        .copied();
    stored.unwrap_or_else(seat_default)
}

/// A size this Mac can give.
pub(crate) fn check_size(size: SeatSize) -> Result<(), String> {
    let (cores, memory) = (host_cores(), host_memory_gib());
    if size.cpus == 0 || size.cpus > cores {
        return Err(format!("CPUs: 1 to {cores} on this Mac"));
    }
    if size.memory_gib == 0 || size.memory_gib > memory {
        return Err(format!("memory: 1 to {memory} GB on this Mac"));
    }
    Ok(())
}

/// What every container on its project's network starts from, seat or
/// editor.
struct Base {
    cli: PathBuf,
    network: String,
    gateway: std::net::Ipv4Addr,
    env: Vec<(String, String)>,
    shares: Vec<Share>,
    deny: Vec<PathBuf>,
}

/// Its project's network and that network's door (as its proxy), its build
/// output in its home volume, git's folders for a linked worktree, and the
/// deny list.
fn base(
    state: &AppState,
    stream_id: &str,
    workspace: &Path,
    operator_deny: &[PathBuf],
) -> Result<Base, String> {
    image_ready(state)?;
    let cli = apple_cli()?;
    let scope = scope_of(state, stream_id);
    let (network, labels) = network_of(&scope);
    let gateway = bakery_exec::ensure_seat_network(&cli, &network, &labels)?;
    // Its project's own services, at their addresses on this network.
    let services = match scope.strip_prefix("project:") {
        Some(project) => crate::services::container_env(state, project),
        None => Vec::new(),
    };
    crate::seat_gateway::ensure(state.clone(), gateway, scope);
    // Its thread's own credential: the gateway holds every process in the
    // container to its project's network list.
    let proxy =
        crate::host_list::proxy_url(state, stream_id, crate::host_list::Kind::Container, gateway);
    let mut env: Vec<(String, String)> = services;
    for name in ["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy"] {
        env.push((name.to_string(), proxy.clone()));
    }
    for name in ["NO_PROXY", "no_proxy"] {
        env.push((name.to_string(), gateway.to_string()));
    }
    // Its programs (git's push helper, gh's stand-in) ask the Bakery at its
    // gateway, as its thread: the credential in the URL says which.
    env.push((crate::doors::URL_ENV.to_string(), proxy.clone()));
    // Linux build output stays in the container's home volume, never in the
    // worktree's macOS target/ (as in .devcontainer).
    env.push((
        "CARGO_TARGET_DIR".to_string(),
        "/home/agent/target".to_string(),
    ));
    let mut shares = Vec::new();
    if let Some((common, own)) = crate::git::linked_worktree_git_dirs(workspace) {
        shares.push(Share::same_path(common, false));
        shares.push(Share::same_path(own, true));
    }
    // The node chooses every mount, so Bakery's own folder is never shared
    // wholesale; the named secret stores and the operator's list still hold.
    let mut deny = bakery_exec::default_deny_paths();
    deny.extend(operator_deny.iter().cloned());
    Ok(Base {
        cli,
        network,
        gateway,
        env,
        shares,
        deny,
    })
}

/// Where the editor's connection token sits inside its container.
const EDITOR_TOKEN_AT: &str = "/run/bakery-code";

/// A conversation's editor: openvscode-server on `port` in a container of
/// its own, the worktree mounted as a seat sees it, its connection token
/// read from `token_dir` (mounted read-only; never an argument or a
/// variable). Returns the container's address on the seats' network.
pub(crate) fn start_editor(
    state: &AppState,
    stream_id: &str,
    workspace: &Path,
    name: &str,
    token_dir: &Path,
    port: u16,
) -> Result<std::net::Ipv4Addr, String> {
    let grants = crate::machine_access::for_stream(state, stream_id);
    let mut base = base(state, stream_id, workspace, &grants.deny)?;
    base.shares.push(Share {
        target: Some(PathBuf::from(EDITOR_TOKEN_AT)),
        ..Share::same_path(token_dir.to_path_buf(), false)
    });
    // Its server-side ("Remote") settings are the ones every editor shares:
    // the Bakery's look, and what the operator sets there, across threads.
    // A folder it kept before is set aside, never deleted.
    let style = crate::editor_style::prepare(state)?;
    base.shares.push(Share {
        target: Some(PathBuf::from(crate::editor_style::MOUNTED_AT)),
        ..Share::same_path(style, true)
    });
    let link = format!(
        r#"d="$HOME/.openvscode-server/data"; mkdir -p "$d" && {{ [ -L "$d/Machine" ] || [ ! -e "$d/Machine" ] || mv "$d/Machine" "$d/Machine.before-bakery"; }} && ln -sfn {}/Machine "$d/Machine" && exec openvscode-server "$@""#,
        crate::editor_style::MOUNTED_AT
    );
    let argv = [
        "sh".to_string(),
        "-c".to_string(),
        link,
        "openvscode-server".to_string(),
        "--host".to_string(),
        "0.0.0.0".to_string(),
        "--port".to_string(),
        port.to_string(),
        "--connection-token-file".to_string(),
        format!("{EDITOR_TOKEN_AT}/token"),
        // The worktree is the operator's own: no trust prompt, and no
        // telemetry. Neither is a setting its shared file can carry.
        "--disable-workspace-trust".to_string(),
        "--telemetry-level".to_string(),
        "off".to_string(),
    ];
    let labels = [
        "com.bakery=code".to_string(),
        format!("com.bakery.stream={stream_id}"),
    ];
    let image = image();
    let size = seat_size(state, stream_id);
    bakery_exec::start_container(&ContainerSeat {
        backend: bakery_exec::Backend::Apple,
        cli: &base.cli,
        host_env: &[],
        image: &image,
        name,
        labels: &labels,
        workspace,
        write_workspace: true,
        shares: &base.shares,
        deny: &base.deny,
        network: Some(&base.network),
        env: &base.env,
        argv: &argv,
        cpus: Some(size.cpus),
        memory_gib: Some(size.memory_gib),
        // The operator's editor: their runtimes' settings are theirs to edit.
        protect_agent_settings: false,
    })?;
    bakery_exec::container_ipv4(bakery_exec::Backend::Apple, &base.cli, &[], name)
}

/// What a terminal runs for the operator's own shell in a container of the
/// seat image: `container run` with a tty, `workspace` mounted as a seat
/// sees it, on the seats' network through the gateway under `stream_id`'s
/// project list. The container goes when the shell exits. The CLI, its
/// arguments, and the variables it hands the container by name.
pub(crate) fn shell(
    state: &AppState,
    stream_id: &str,
    workspace: &Path,
) -> Result<(PathBuf, Vec<String>, Vec<(String, String)>), String> {
    let grants = crate::machine_access::shown_for(state, stream_id);
    let base = base(state, stream_id, workspace, &grants.deny)?;
    let name = format!("bakery-{}", crate::node::mint_id("shell").replace('_', "-"));
    let labels = [
        "com.bakery=shell".to_string(),
        format!("com.bakery.stream={stream_id}"),
    ];
    let image = image();
    let size = seat_size(state, stream_id);
    let argv = ["bash".to_string(), "-l".to_string()];
    let args = bakery_exec::interactive_run_args(&ContainerSeat {
        backend: bakery_exec::Backend::Apple,
        cli: &base.cli,
        host_env: &[],
        image: &image,
        name: &name,
        labels: &labels,
        workspace,
        write_workspace: true,
        shares: &base.shares,
        deny: &base.deny,
        network: Some(&base.network),
        env: &base.env,
        argv: &argv,
        cpus: Some(size.cpus),
        memory_gib: Some(size.memory_gib),
        // The operator's own shell: their runtimes' settings are theirs.
        protect_agent_settings: false,
    })?;
    // The CLI itself with a plain environment; the container's variables
    // go to it by name only.
    let mut env = vec![
        ("HOME".to_string(), std::env::var("HOME").unwrap_or_default()),
        ("PATH".to_string(), "/usr/bin:/bin:/usr/sbin:/sbin".to_string()),
    ];
    env.extend(base.env.iter().cloned());
    Ok((base.cli, args, env))
}

/// Opens `file` at `line` in the window a running editor shows, through
/// openvscode's own command line in its container: the newest window first.
/// Fails when no window is connected, and the caller then loads the file
/// with the editor's page instead.
pub(crate) fn editor_goto(name: &str, file: &str, line: usize) -> Result<(), String> {
    const GOTO: &str = r#"for s in $(ls -t "${XDG_RUNTIME_DIR:-/tmp}"/vscode-ipc-*.sock /tmp/vscode-ipc-*.sock 2>/dev/null); do VSCODE_IPC_HOOK_CLI="$s" timeout 5 /opt/openvscode-server/bin/remote-cli/openvscode-server --goto "$1:$2" && exit 0; done; exit 1"#;
    let out = std::process::Command::new(apple_cli()?)
        .args(["exec", name, "sh", "-c", GOTO, "sh", file, &line.to_string()])
        .output()
        .map_err(|e| e.to_string())?;
    match out.status.success() {
        true => Ok(()),
        false => Err(format!("no editor window took it: {}", String::from_utf8_lossy(&out.stderr).trim())),
    }
}

/// Stop a container this node started, by name; one that is already gone is
/// fine.
pub(crate) fn stop(name: &str) {
    if let Ok(cli) = apple_cli() {
        let _ = bakery_exec::stop_container(bakery_exec::Backend::Apple, &cli, &[], name);
    }
}

/// The address of a running container of this conversation's (its
/// editor's or one of its seats'), found by its `com.bakery.stream` label.
pub(crate) fn address_of(stream_id: &str) -> Option<std::net::Ipv4Addr> {
    let cli = apple_cli().ok()?;
    let out = std::process::Command::new(cli)
        .args(["list", "--format", "json"])
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    let running: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    running
        .as_array()?
        .iter()
        .filter(|c| has_label(c, "com.bakery.stream", stream_id))
        .find_map(bakery_exec::first_ipv4)
}

/// A started container seat, and the MCP endpoint its session gets.
pub(crate) struct Started {
    pub(crate) handle: BoxedAcpHandle,
    pub(crate) mcp_url: String,
}

/// A container seat's own Claude config, mounted where it reads it: the
/// folder writable, its settings, rules, skills and the rest read-only, as
/// on this Mac (`claude_settings`): an agent never rewrites what the next
/// session loads (its permissions, its hooks). Each is made first when
/// missing, so it can't be made inside either.
fn config_shares(seat_dir: PathBuf) -> Result<Vec<Share>, String> {
    let kept = bakery_exec::claude_settings(&seat_dir);
    for path in &kept {
        if !path.exists() {
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            let made = match name.ends_with(".json") || name.ends_with(".md") {
                true => std::fs::write(path, if name.ends_with(".json") { "{}" } else { "" }),
                false => std::fs::create_dir_all(path),
            };
            made.map_err(|e| format!("{}: {e}", path.display()))?;
        }
    }
    let folder = Share { target: Some(PathBuf::from(CLAUDE_CONFIG_AT)), ..Share::same_path(seat_dir, true) };
    let settings = kept.into_iter().map(|path| Share {
        target: Some(Path::new(CLAUDE_CONFIG_AT).join(path.file_name().unwrap_or_default())),
        ..Share::same_path(path, false)
    });
    Ok(std::iter::once(folder).chain(settings).collect())
}

#[allow(clippy::too_many_arguments)] // one call site, in pipe.rs's session start
pub(crate) fn spawn(
    state: &AppState,
    stream_id: &str,
    seat: crate::node::Seat,
    model_id: &str,
    workspace: &Path,
    trusted: bool,
    grants: &SpawnGrants,
    reach: Reach,
) -> Result<Started, String> {
    let argv = command(model_id)?;
    let Base {
        cli,
        network,
        gateway,
        mut env,
        mut shares,
        deny,
    } = base(state, stream_id, workspace, &grants.deny)?;
    let at = format!("{gateway}:{}", crate::seat_gateway::PORT);
    // A seat with a per-thread minted sign-in (the subscription, or Claude
    // native's own console/OpenRouter key) gets a stand-in directory, never
    // the real vault or key, and its tools pointed at this thread on the
    // gateway. The gateway swaps the stand-in the same way the host door
    // does. `provider` picks which of a model's rows (Claude native has
    // three) applies; every other model still has exactly one.
    let provider = crate::claude_providers::for_seat(state, stream_id, seat);
    let subscription = crate::swap::subscription_for_seat(state, model_id, provider.id());
    if subscription.is_some_and(|sub| sub.name() == "grok-cli") {
        let sub = subscription.unwrap();
        if !sub.signed_in(state) {
            return Err("Sign in with grok on this Mac first. A container seat uses it through the door.".to_string());
        }
        let host_dir = crate::grok_cli::seat_dir(state, stream_id, seat.as_str());
        let stand_in = crate::swap::minted(state, "grok-cli", stream_id);
        crate::grok_cli::write_stand_in(&host_dir, &stand_in)?;
        shares.push(bakery_exec::Share {
            target: Some(PathBuf::from("/grok-home")),
            ..bakery_exec::Share::same_path(host_dir, false)
        });
        env.push(("GROK_HOME".to_string(), "/grok-home".to_string()));
        env.push(("GROK_AUTH_PATH".to_string(), "/grok-home/auth.json".to_string()));
    } else if let Some(sub) = subscription {
        if !sub.signed_in(state) {
            let how = if provider == crate::claude_providers::Provider::Subscription {
                "Sign in to claude on this Mac".to_string()
            } else {
                format!("Give the Bakery a {} key (Settings)", provider.label())
            };
            return Err(format!("{how} first. A container seat uses it through the door."));
        }
        let seat_dir = crate::claude_native::prepare_seat_config(state, stream_id, seat.as_str(), provider, Some(Path::new(CLAUDE_CONFIG_AT)))?;
        env.push((
            "CLAUDE_CONFIG_DIR".to_string(),
            CLAUDE_CONFIG_AT.to_string(),
        ));
        // MCP tool search on behind the gateway's door, as on this Mac.
        let (name, on) = crate::claude_native::TOOL_SEARCH_ENV;
        env.push((name.to_string(), on.to_string()));
        shares.extend(config_shares(seat_dir)?);
    }
    // The stream's attachments, read-only, at the path the node names them by.
    shares.push(Share::same_path(crate::attachments::room_dir(state, stream_id), false));
    if reach == Reach::Container {
        // Read and write grants become mounts. Run grants name macOS
        // programs, which a Linux seat can't run; PATH entries likewise.
        shares.extend(grants.read.iter().map(|p| Share::granted(p.clone(), false)));
        shares.extend(grants.write.iter().map(|p| Share::granted(p.clone(), true)));
        env.extend(
            grants
                .env
                .iter()
                .filter(|(k, _)| k != bakery_exec::ACP_PATH_PREFIX_ENV)
                .cloned(),
        );
    }
    if let Some(sub) = subscription {
        let origin = crate::swap::subscription_origin_container(gateway, stream_id);
        let model_for_env = if sub.name() == "grok-cli" { "acp:grok" } else { model_id };
        let pinned = crate::swap::subscription_env_at(state, stream_id, model_for_env, provider.id(), &origin);
        env.retain(|(key, value)| sub.keep_env(key, value));
        env.retain(|(key, _)| !sub.pins_env(key));
        env.extend(pinned);
        if sub.name() == "grok-cli" {
            let base = env
                .iter()
                .find(|(key, _)| key == "GROK_CLI_CHAT_PROXY_BASE_URL")
                .map(|(_, value)| value.clone())
                .unwrap_or_default();
            env.retain(|(key, _)| key != "GROK_HOME" && key != "GROK_AUTH_PATH" && key != "GROK_PRODUCTION_CLI_CHAT_PROXY_BASE_URL");
            env.push(("GROK_HOME".to_string(), "/grok-home".to_string()));
            env.push(("GROK_AUTH_PATH".to_string(), "/grok-home/auth.json".to_string()));
            env.push(("GROK_PRODUCTION_CLI_CHAT_PROXY_BASE_URL".to_string(), base));
        }
    }
    // Its own git push and gh are held for the operator (`remote_ask`): the
    // helper and the stand-in go first on its PATH, git hands the helper
    // every push, and both ask at its gateway as its thread. The Mac's git
    // settings (its hooks folder) mean nothing here.
    shares.push(Share {
        target: Some(PathBuf::from(SEAT_BIN_AT)),
        ..Share::same_path(seat_bin(state)?, false)
    });
    env.retain(|(k, _)| !k.starts_with("GIT_CONFIG_"));
    env.extend(crate::git_hooks::config_env(&crate::remote_ask::git_config()));
    env.push(("PATH".to_string(), format!("{SEAT_BIN_AT}:{}", image_path())));

    let agent = crate::node::agent_id(stream_id, seat);
    let name = format!("bakery-{}", &blake3::hash(agent.as_bytes()).to_hex()[..12]);
    // Whose seat it is, for the Containers screen.
    let labels = [
        LABEL.to_string(),
        format!("com.bakery.stream={stream_id}"),
        format!("com.bakery.seat={}", seat.as_str()),
    ];
    let image = image();
    let size = seat_size(state, stream_id);
    let handle = bakery_exec::spawn_container_acp(&ContainerSeat {
        backend: bakery_exec::Backend::Apple,
        cli: &cli,
        host_env: &[],
        image: &image,
        name: &name,
        labels: &labels,
        workspace,
        write_workspace: trusted,
        shares: &shares,
        deny: &deny,
        network: Some(&network),
        env: &env,
        argv: &argv,
        cpus: Some(size.cpus),
        memory_gib: Some(size.memory_gib),
        protect_agent_settings: true,
    })?;
    Ok(Started {
        handle,
        mcp_url: format!("http://{at}/mcp"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A container seat's settings are read-only to it, over its writable
    /// config folder, as on this Mac; a missing one is made first.
    #[test]
    fn a_container_seat_cannot_rewrite_its_own_settings() {
        let dir = tempfile::tempdir().unwrap();
        let seat = dir.path().join("seat-config");
        std::fs::create_dir_all(&seat).unwrap();
        let shares = config_shares(seat.clone()).unwrap();
        assert!(shares[0].write && shares[0].target.as_deref() == Some(Path::new(CLAUDE_CONFIG_AT)));
        for name in ["settings.json", "CLAUDE.md", "rules", "skills", "plugins"] {
            let share = shares.iter().find(|s| s.path == seat.join(name)).unwrap_or_else(|| panic!("{name} not mounted"));
            assert!(!share.write, "{name} must be read-only");
            assert_eq!(share.target.as_deref(), Some(Path::new(CLAUDE_CONFIG_AT).join(name).as_path()));
            assert!(seat.join(name).exists(), "{name} made first");
        }
        assert_eq!(std::fs::read_to_string(seat.join("settings.json")).unwrap(), "{}");

        // Apple container binds folders only: the files are kept read-only
        // in the writable folder by the seat's init, never bound themselves.
        let workspace = dir.path().join("work");
        std::fs::create_dir_all(&workspace).unwrap();
        let argv = ["true".to_string()];
        let line = bakery_exec::interactive_run_args(&ContainerSeat {
            backend: bakery_exec::Backend::Apple,
            cli: Path::new("/opt/homebrew/bin/container"),
            host_env: &[],
            image: "bakery/workspace:dev",
            name: "w1-cfg",
            labels: &[],
            workspace: &workspace,
            write_workspace: true,
            shares: &shares,
            deny: &[],
            network: None,
            env: &[],
            argv: &argv,
            cpus: None,
            memory_gib: None,
            protect_agent_settings: false,
        })
        .unwrap()
        .join(" ");
        assert!(!line.contains("settings.json,readonly") && !line.contains("CLAUDE.md,readonly"), "{line}");
        for name in ["settings.json", "settings.local.json", "CLAUDE.md"] {
            assert!(line.contains(&format!(" {CLAUDE_CONFIG_AT}/{name} ")), "{name} kept by the init: {line}");
        }
        assert!(line.contains(&format!("target={CLAUDE_CONFIG_AT}/rules,readonly")), "{line}");
    }

    /// The operator's design: a project's containers are sealed together.
    /// Each project has its own internal network (Apple keeps networks from
    /// reaching one another), labelled with its project; a thread with no
    /// project has one of its own.
    #[test]
    fn each_project_has_its_own_network() {
        let dir = tempfile::tempdir().unwrap();
        let state = crate::testutil::state_with(dir.path(), std::sync::Arc::new(bakery_agent::StubModel));
        let mut threads = Vec::new();
        for name in ["farm", "shop"] {
            let folder = dir.path().join(name);
            std::fs::create_dir_all(&folder).unwrap();
            let root = crate::catalog::DirectoryRootSpec::Attach(folder.to_str().unwrap());
            let (project, thread) = crate::catalog::create_project(&state, name, Some(root)).unwrap();
            threads.push((project.project_id, thread.stream_id));
        }
        let [(farm, farm_thread), (shop, shop_thread)] = &threads[..] else { unreachable!() };
        let (farm_net, farm_labels) = network_of(&scope_of(&state, farm_thread));
        let (shop_net, _) = network_of(&scope_of(&state, shop_thread));
        assert!(farm_net.starts_with("bakery-p-") && shop_net.starts_with("bakery-p-"));
        assert_ne!(farm_net, shop_net, "never one network for two projects");
        assert_eq!(farm_labels, [LABEL.to_string(), format!("com.bakery.project={farm}")]);
        assert_eq!(network_of(&scope_of(&state, farm_thread)).0, farm_net, "the same one each time");
        assert_ne!(farm, shop);

        let (alone, labels) = network_of(&scope_of(&state, "wt:no-project"));
        assert!(alone.starts_with("bakery-t-"));
        assert_eq!(labels[1], "com.bakery.stream=wt:no-project");
    }

    /// A seat's push helper and gh stand-in speak git's protocol and gh's
    /// arguments, and ask at the gateway as the thread its credential
    /// names (the URL's user and password, sent as Basic).
    #[test]
    fn the_seat_helpers_ask_the_gateway_as_their_thread() {
        let Ok(node) = crate::state::resolve_executable_path("node") else {
            return; // no Node here: nothing to run them with
        };
        let dir = tempfile::tempdir().unwrap();
        let state = crate::testutil::state_with(dir.path(), std::sync::Arc::new(bakery_agent::StubModel));
        let bin = seat_bin(&state).unwrap();
        let repo = crate::testutil::make_repo(&dir.path().join("repo"));
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let port = server.server_addr().to_ip().unwrap().port();
        let asked = std::thread::spawn(move || {
            let mut asked = Vec::new();
            for answer in [
                r#"{"ok":false,"message":"","refs":[{"dst":"refs/heads/main","ok":true,"message":"pushed"},{"dst":"refs/heads/x","ok":false,"message":"a forced push is the operator's"}]}"#,
                r#"{"ok":true,"message":"opened","url":"https://github.com/o/r/pull/7"}"#,
            ] {
                let mut request = server.recv().unwrap();
                let auth = request.headers().iter().find(|h| h.field.equiv("Authorization")).map(|h| h.value.to_string());
                let mut body = String::new();
                request.as_reader().read_to_string(&mut body).unwrap();
                asked.push((request.url().to_string(), auth, body));
                request.respond(tiny_http::Response::from_string(answer)).unwrap();
            }
            asked
        });
        let run = |script: &str, args: &[&str], stdin: &str| {
            use std::io::Write;
            let mut child = std::process::Command::new(&node)
                .arg(bin.join(script))
                .args(args)
                .current_dir(&repo)
                .env("BAKERY_DOOR_URL", format!("http://wt-a:sig%2Fx@127.0.0.1:{port}"))
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
            let out = child.wait_with_output().unwrap();
            String::from_utf8_lossy(&out.stdout).to_string()
        };
        let said = run(
            "git-remote-bakery",
            &["origin", "bakery::https://example.com/o/r.git"],
            "capabilities\nlist for-push\npush refs/heads/main:refs/heads/main\npush +refs/heads/x:refs/heads/x\n\n\n",
        );
        assert_eq!(said, "push\noption\n\n\nok refs/heads/main\nerror refs/heads/x a forced push is the operator's\n\n");
        let body_file = dir.path().join("body.md");
        std::fs::write(&body_file, "Why it matters").unwrap();
        let opened = run("gh", &["pr", "create", "--title", "T", "--body-file", body_file.to_str().unwrap()], "");
        assert_eq!(opened, "https://github.com/o/r/pull/7\n");

        let asked = asked.join().unwrap();
        let basic = format!("Basic {}", bakery_agent::tool_wire::encode_base64(b"wt-a:sig/x"));
        let top = std::fs::canonicalize(&repo).unwrap().display().to_string();
        let push: serde_json::Value = serde_json::from_str(&asked[0].2).unwrap();
        assert_eq!((asked[0].0.as_str(), asked[0].1.as_deref()), ("/remote/ask", Some(basic.as_str())));
        assert_eq!(
            push,
            serde_json::json!({"kind": "push", "repo": top, "refs": [
                {"src": "refs/heads/main", "dst": "refs/heads/main", "force": false},
                {"src": "refs/heads/x", "dst": "refs/heads/x", "force": true},
            ]})
        );
        let gh: serde_json::Value = serde_json::from_str(&asked[1].2).unwrap();
        assert_eq!(gh, serde_json::json!({"kind": "gh", "repo": top, "args": ["pr", "create", "--title", "T", "--body", "Why it matters"]}));
        assert_eq!(asked[1].1.as_deref(), Some(basic.as_str()));
        assert!(image_path().starts_with("/home/agent/.cargo/bin:"), "{}", image_path());
    }

    #[test]
    fn a_seat_is_sized_from_this_mac_within_what_it_can_give() {
        let default = seat_default();
        assert!(
            default.cpus >= 1 && default.cpus <= host_cores(),
            "{default:?}"
        );
        assert!((4..=16).contains(&default.memory_gib), "{default:?}");
        assert_eq!(check_size(default), Ok(()));
        for too_much in [
            SeatSize {
                cpus: 0,
                memory_gib: 4,
            },
            SeatSize {
                cpus: host_cores() + 1,
                memory_gib: 4,
            },
            SeatSize {
                cpus: 1,
                memory_gib: host_memory_gib() + 1,
            },
        ] {
            assert!(check_size(too_much).is_err(), "{too_much:?}");
        }
    }

    #[test]
    fn only_runtimes_the_image_carries_start_in_a_container() {
        assert_eq!(
            command(crate::claude_native::ACCOUNT_ID).unwrap(),
            ["claude-agent-acp"]
        );
        let err = command("acp:grok").unwrap_err();
        assert!(err.contains("Host reach"), "{err}");
    }

    fn client() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bakery-client")
    }

    /// The image's Claude connector is the host's: the release bakery-client
    /// pins, passed in (the recipe sets no release of its own), and the patch
    /// pnpm applies to it on the host.
    #[test]
    fn the_seat_images_claude_connector_is_the_hosts() {
        let workspace = std::fs::read_to_string(client().join("pnpm-workspace.yaml")).unwrap();
        let version = crate::claude_native::ADAPTER_VERSION;
        let patch = crate::adapter_pin::patch_path(&workspace, crate::claude_native::ADAPTER_PACKAGE, version)
            .map(|rel| std::fs::read_to_string(client().join(rel)).unwrap())
            .unwrap_or_default();
        assert_eq!(ADAPTER_PATCH, patch, "the node carries bakery-client's patch for {version}");
        assert!(DOCKERFILE.contains("ARG CLAUDE_ACP_VERSION\n"), "no release of its own");
        assert!(!DOCKERFILE.contains("CLAUDE_ACP_VERSION=0"), "no release of its own");
        assert!(DOCKERFILE.contains("COPY claude-agent-acp.patch "));
        assert!(DOCKERFILE.contains("patch --batch --forward -p1 -d /usr/local/lib/node_modules/@agentclientprotocol/claude-agent-acp"));
    }

    /// The patch applies to the release pnpm installed: undone from
    /// bakery-client's patched copy, it leaves the release as published, which
    /// the image installs.
    #[test]
    fn the_patch_applies_to_the_release_the_image_installs() {
        if ADAPTER_PATCH.is_empty() {
            return;
        }
        let installed = client().join("node_modules/@agentclientprotocol/claude-agent-acp");
        assert!(installed.is_dir(), "run pnpm install in bakery-client: {} is missing", installed.display());
        let mut undo = std::process::Command::new("patch")
            .args(["--batch", "--reverse", "--dry-run", "-p1", "-d"])
            .arg(&installed)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        std::io::Write::write_all(&mut undo.stdin.take().unwrap(), ADAPTER_PATCH.as_bytes()).unwrap();
        let out = undo.wait_with_output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
    }

    /// Upgrading the host's connector, its release or its patch, makes the
    /// image stale, so it's built again.
    #[test]
    fn an_adapter_upgrade_changes_the_recipe() {
        let now = recipe_of(DOCKERFILE, "0.89.1", "patch");
        assert_eq!(now, recipe_of(DOCKERFILE, "0.89.1", "patch"));
        assert_ne!(now, recipe_of(DOCKERFILE, "0.90.0", "patch"));
        assert_ne!(now, recipe_of(DOCKERFILE, "0.89.1", "patch, ported"));
        assert_ne!(now, recipe_of(&format!("{DOCKERFILE}\n"), "0.89.1", "patch"));
    }

    /// Unasked, the image is built when it's stale (something used it), or
    /// missing once something runs in a container; never when current.
    #[test]
    fn the_image_is_built_unasked_only_when_its_wanted() {
        assert!(build_wanted(ImageState::Stale, false));
        assert!(build_wanted(ImageState::Missing, true));
        assert!(!build_wanted(ImageState::Missing, false));
        assert!(!build_wanted(ImageState::Current, true));

        let dir = tempfile::tempdir().unwrap();
        let state = crate::testutil::state_with(dir.path(), std::sync::Arc::new(bakery_agent::StubModel));
        assert!(!containers_in_use(&state));
        state.node.lock().unwrap().machine_access.reach.insert("s:seat".to_string(), Reach::Container);
        assert!(containers_in_use(&state));
    }

    /// A deploy keeps a build off for as long as bakery.sh holds its lock: a
    /// lock with no owner named, or a live one; one whose owner is gone is no
    /// lock, as bakery.sh decides.
    #[test]
    fn a_deploy_holds_a_build_off() {
        let dir = tempfile::tempdir().unwrap();
        let state = crate::testutil::state_with(dir.path(), std::sync::Arc::new(bakery_agent::StubModel));
        assert!(!deploy_running(&state));
        let lock = dir.path().join("deploy").join(".lock");
        std::fs::create_dir_all(&lock).unwrap();
        assert!(deploy_running(&state), "an owner not yet written");
        std::fs::write(lock.join("pid"), std::process::id().to_string()).unwrap();
        assert!(deploy_running(&state), "a live owner");
        let mut gone = std::process::Command::new("/usr/bin/true").spawn().unwrap();
        gone.wait().unwrap();
        std::fs::write(lock.join("pid"), gone.id().to_string()).unwrap();
        assert!(!deploy_running(&state), "its owner is gone");
    }

    #[test]
    fn an_image_is_current_only_with_this_recipe() {
        let inspect = serde_json::json!([{ "variants": [{ "config": { "config": {
            "Labels": { "com.bakery.recipe": "abc", "other": "x" }
        } } }] }]);
        assert!(has_label(&inspect, "com.bakery.recipe", "abc"));
        assert!(!has_label(&inspect, "com.bakery.recipe", "def"));
        assert!(!has_label(
            &serde_json::Value::Null,
            "com.bakery.recipe",
            "abc"
        ));
        // A container's own labels, as Apple container inspects them.
        let seat = serde_json::json!([{ "configuration": { "labels": { "com.bakery": "seat" } } }]);
        assert!(has_label(&seat, "com.bakery", "seat"));
    }
}
