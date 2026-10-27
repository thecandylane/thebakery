//! Settings → Machine access: which parts of this machine the Baker's native
//! runtime may read, write, or run programs from, beyond its sandboxed
//! workspace. The operator grants them (node.json); Seatbelt enforces them
//! when a runtime starts; the host's secret stores
//! (`bakery_exec::default_deny_paths`) stay denied under any grant, because
//! those denies are emitted after every allow. The Baker sees its grants in
//! its session lines and can never change them.
//!
//! The scope dial: each conversation sits at one [`Reach`] — its room
//! (workspace plus grants) or the whole Mac (Host full). One deny list holds
//! at every level: the built-in one plus the operator's own additions
//! (`MachineAccess::deny`), which they can extend but never shrink below the
//! built-in one. [`scope_allows`] answers the same question for node tools
//! that act on a path for an agent.

use crate::state::AppState;
use axum::{extract::State, http::StatusCode, Json};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(schemars::JsonSchema, Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    Read,
    /// Read and write.
    Write,
    /// Read and run programs from it.
    Exec,
    /// Read, write, and run programs from it — a project the agent may
    /// build and test.
    Full,
}

impl Access {
    fn label(self) -> &'static str {
        match self {
            Access::Read => "read",
            Access::Write => "read+write",
            Access::Exec => "run+read",
            Access::Full => "read+write+run",
        }
    }

    /// Whether a grant of `self` already gives everything `wanted` asks for.
    fn covers(self, wanted: Access) -> bool {
        self == wanted || self == Access::Full || wanted == Access::Read
    }

    /// The narrowest access that gives both.
    fn union(self, other: Access) -> Access {
        if self.covers(other) {
            self
        } else if other.covers(self) {
            other
        } else {
            Access::Full
        }
    }
}

/// Who a grant reaches: the Baker, the other agents (project agents and task
/// workers), or both — each chosen independently.
#[derive(schemars::JsonSchema, Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    #[default]
    Baker,
    Agents,
    AllAgents,
}

impl Scope {
    fn reaches(self, baker: bool) -> bool {
        match self {
            Scope::Baker => baker,
            Scope::Agents => !baker,
            Scope::AllAgents => true,
        }
    }
}

/// One conversation's place on the scope dial, narrowest last.
#[derive(schemars::JsonSchema, Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Reach {
    /// The whole Mac, as the operator's own shell has it, except the deny
    /// list (Seatbelt, open by default). Docker and Apple container stay off.
    HostFull,
    /// Its workspace and the folders granted to it (Seatbelt, closed by
    /// default).
    #[default]
    HostRoom,
    /// A Linux container (`seat_container`): its workspace and the folders
    /// granted to it, reaching only model APIs and package registries.
    Container,
    /// The same container with its workspace only.
    Locked,
}

#[derive(schemars::JsonSchema, Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Grant {
    pub path: String,
    pub access: Access,
    #[serde(default)]
    pub scope: Scope,
    /// Put this directory on the runtime's PATH (run grants only).
    #[serde(default)]
    pub on_path: bool,
    /// The preset that added it, for display; `None` = added by hand.
    #[serde(default)]
    pub preset: Option<String>,
}

#[derive(schemars::JsonSchema, Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EnvVar {
    pub name: String,
    pub value: String,
    #[serde(default)]
    pub scope: Scope,
    #[serde(default)]
    pub preset: Option<String>,
}

#[derive(schemars::JsonSchema, Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MachineAccess {
    /// For the Baker, for every project, or both (each grant's `scope`).
    #[serde(default)]
    pub grants: Vec<Grant>,
    #[serde(default)]
    pub env: Vec<EnvVar>,
    /// One project's own environment, on top of the every-project grants:
    /// only that project's agents and tasks get it. Keyed by project id.
    #[serde(default)]
    pub projects: std::collections::BTreeMap<String, ProjectAccess>,
    /// The operator's own additions to the deny list, denied at every level
    /// like the built-in list, which no setting removes.
    #[serde(default)]
    pub deny: Vec<String>,
    /// Each conversation's place on the dial, by stream id; absent means its
    /// default: [`Reach::HostFull`] for the Baker and the Bakery's own
    /// workspaces, else its project's kind (`project_reach`). Set per agent
    /// (`POST /machine-access/reach`), never by saving the policy.
    #[serde(default)]
    pub reach: std::collections::BTreeMap<String, Reach>,
    /// Conversations whose agent may use containers, by stream id; off by
    /// default at every reach. Only through Bakery's container tools
    /// (`crate::containers::agent_may`), never a Docker socket or Apple
    /// container's own CLI. Set per agent (`POST /machine-access/containers`),
    /// never by saving the policy.
    #[serde(default)]
    pub containers: std::collections::BTreeSet<String>,
    /// Hosts container seats may also reach through their gateway, beyond
    /// the built-in ones (`seat_gateway::BUILT_IN`): the operator's own
    /// additions, which never remove a built-in one. A leading dot allows
    /// every subdomain.
    #[serde(default)]
    pub allow_hosts: Vec<String>,
    /// Each container seat's CPUs and memory, by stream id; absent means the
    /// default sized from this Mac (`seat_container::seat_default`). Set per
    /// agent (`POST /machine-access/seat-size`), never by saving the policy.
    #[serde(default)]
    pub seat_size: std::collections::BTreeMap<String, SeatSize>,
}

/// A container seat's share of this Mac.
#[derive(schemars::JsonSchema, Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct SeatSize {
    pub cpus: u32,
    pub memory_gib: u32,
}

#[derive(schemars::JsonSchema, Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectAccess {
    #[serde(default)]
    pub grants: Vec<Grant>,
    #[serde(default)]
    pub env: Vec<EnvVar>,
}

/// Variables the runtime's own environment is built from — never granted.
const RESERVED_ENV: &[&str] = &[
    "HOME",
    "TMPDIR",
    "PATH",
    "SHELL",
    "USER",
    "LOGNAME",
    "CLAUDE_CONFIG_DIR",
    "CLAUDE_CODE_TMPDIR",
];

/// Claude Code's own settings whose names only read like a credential: token
/// counts (code.claude.com/docs/en/env-vars).
const CLAUDE_CODE_TOKEN_COUNTS: &[&str] = &[
    "MAX_THINKING_TOKENS",
    "MAX_MCP_OUTPUT_TOKENS",
    "CLAUDE_CODE_MAX_OUTPUT_TOKENS",
    "CLAUDE_CODE_MAX_CONTEXT_TOKENS",
    "CLAUDE_CODE_FILE_READ_MAX_OUTPUT_TOKENS",
];

/// Claude Code's switches to another provider's sign-in, which would send
/// its requests past the Bakery's door.
const CLAUDE_CODE_PROVIDERS: &[&str] = &[
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "CLAUDE_CODE_USE_FOUNDRY",
    "CLAUDE_CODE_USE_MANTLE",
    "CLAUDE_CODE_USE_GATEWAY",
    "CLAUDE_CODE_USE_ANTHROPIC_AWS",
    "CLAUDE_CODE_USE_ANTHROPIC_GOOGLE_CLOUD",
];

/// A variable whose name marks it as a credential is refused: machine access
/// widens what the agent can reach, it never hands it a secret.
pub(crate) fn looks_like_secret(name: &str) -> bool {
    if CLAUDE_CODE_TOKEN_COUNTS.contains(&name) {
        return false;
    }
    name.starts_with("ANTHROPIC_")
        || name.ends_with("_AUTH")
        || [
            "TOKEN",
            "SECRET",
            "PASSWORD",
            "PASSWD",
            "PASSPHRASE",
            "API_KEY",
            "PRIVATE_KEY",
            "CLIENT_KEY",
            "CREDENTIAL",
            "HEADERS",
        ]
        .iter()
        .any(|marker| name.contains(marker))
}

/// Normalizes and checks a submitted policy: every path is stored canonical
/// (a folder that's gone keeps its row, shown missing: `View::missing`);
/// nothing too broad (`/`, `/Users`, HOME), nothing inside
/// Bakery's own state, nothing that is (or sits inside) an always-denied
/// secret store; PATH only for run grants; env names plain and non-secret.
pub(crate) fn validate(
    policy: MachineAccess,
    home: &Path,
    bakery_base: &Path,
) -> Result<MachineAccess, String> {
    let deny = validate_deny(policy.deny, home, bakery_base)?;
    let grants = validate_grants(policy.grants, home, bakery_base, &deny)?;
    let env = validate_env(policy.env)?;
    let mut projects = std::collections::BTreeMap::new();
    for (project_id, access) in policy.projects {
        let grants = validate_grants(access.grants, home, bakery_base, &deny)
            .map_err(|e| format!("project {project_id}: {e}"))?;
        let env = validate_env(access.env).map_err(|e| format!("project {project_id}: {e}"))?;
        if !grants.is_empty() || !env.is_empty() {
            projects.insert(project_id, ProjectAccess { grants, env });
        }
    }
    Ok(MachineAccess {
        grants,
        env,
        projects,
        deny,
        reach: policy.reach,
        containers: policy.containers,
        allow_hosts: validate_hosts(policy.allow_hosts)?,
        seat_size: policy.seat_size,
    })
}

fn canon(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}

/// `~/…` or an absolute path, canonical when it exists.
fn expand(raw: &str, home: &Path) -> Result<PathBuf, String> {
    let path = match raw.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None if raw == "~" => home.to_path_buf(),
        None => PathBuf::from(raw),
    };
    if !path.is_absolute() {
        return Err(format!("{raw}: use an absolute path (or ~/…)"));
    }
    Ok(canon(&path))
}

/// The operator's additions to the deny list. A path may not exist yet; it
/// is denied the moment it does. Refused: anything so broad no agent could
/// run, and anything already blocked.
fn validate_deny(
    requested: Vec<String>,
    home: &Path,
    bakery_base: &Path,
) -> Result<Vec<String>, String> {
    let home = canon(home);
    let floor: Vec<PathBuf> = std::iter::once(canon(bakery_base))
        .chain(bakery_exec::default_deny_paths().iter().map(|p| canon(p)))
        .collect();
    let mut deny: Vec<String> = Vec::new();
    for raw in requested {
        let path = expand(raw.trim(), &home)?;
        if path == Path::new("/") || path == Path::new("/Users") || path == home {
            return Err(format!(
                "{}: too broad — no agent could run; block the specific folders",
                path.display()
            ));
        }
        if let Some(blocked) = floor.iter().find(|f| path.starts_with(f)) {
            return Err(format!(
                "{}: already always blocked ({})",
                path.display(),
                blocked.display()
            ));
        }
        let path = path.display().to_string();
        if !deny.contains(&path) {
            deny.push(path);
        }
    }
    Ok(deny)
}

/// The built-in deny list and the operator's additions, as grants are
/// checked against them.
fn grant_denied(operator_deny: &[String]) -> Vec<PathBuf> {
    bakery_exec::default_deny_paths()
        .iter()
        .map(|p| canon(p))
        .chain(operator_deny.iter().map(PathBuf::from))
        .collect()
}

/// Why a resolved `path` is never granted: too broad, Bakery's own state,
/// or inside the deny list. `home` and `bakery_base` are canonical.
fn refused(path: &Path, home: &Path, bakery_base: &Path, denied: &[PathBuf]) -> Option<String> {
    if path == Path::new("/") || path == Path::new("/Users") || path == home {
        return Some(format!(
            "{}: too broad — grant the specific folders it needs",
            path.display()
        ));
    }
    if path.starts_with(bakery_base) {
        return Some(format!(
            "{}: Bakery's own state is never granted",
            path.display()
        ));
    }
    denied.iter().find(|d| path.starts_with(d)).map(|secret| {
        format!(
            "{}: inside {}, which is always denied",
            path.display(),
            secret.display()
        )
    })
}

fn validate_grants(
    requested: Vec<Grant>,
    home: &Path,
    bakery_base: &Path,
    operator_deny: &[String],
) -> Result<Vec<Grant>, String> {
    let home = canon(home);
    let bakery_base = canon(bakery_base);
    let denied = grant_denied(operator_deny);
    let mut grants: Vec<Grant> = Vec::new();
    for grant in requested {
        let raw = grant.path.trim();
        let path = match raw.strip_prefix("~/") {
            Some(rest) => home.join(rest),
            None if raw == "~" => home.clone(),
            None => PathBuf::from(raw),
        };
        if !path.is_absolute() {
            return Err(format!("{raw}: use an absolute path (or ~/…)"));
        }
        // A folder that's gone never blocks a save: its existing part is
        // resolved, the rest kept as written.
        let path = resolve(&path)?;
        if let Some(why) = refused(&path, &home, &bakery_base, &denied) {
            return Err(why);
        }
        if grant.on_path && grant.access != Access::Exec {
            return Err(format!(
                "{}: only a run grant can go on PATH",
                path.display()
            ));
        }
        let path = path.display().to_string();
        if grants.iter().any(|g| g.path == path && g.scope == grant.scope) {
            return Err(format!("{path}: granted twice"));
        }
        grants.push(Grant {
            path,
            access: grant.access,
            scope: grant.scope,
            on_path: grant.on_path,
            preset: grant.preset,
        });
    }
    Ok(grants)
}

/// The operator's additions to the hosts seats may reach: host names only
/// (an address could be this Mac's own loopback or the local network), each
/// with a dot, `.example.com` for every subdomain. Refused: what the
/// built-in list already allows.
fn validate_hosts(requested: Vec<String>) -> Result<Vec<String>, String> {
    let mut hosts: Vec<String> = Vec::new();
    for raw in requested {
        let host = raw.trim().to_ascii_lowercase();
        let name = host.strip_prefix('.').unwrap_or(&host);
        let labels: Vec<&str> = name.split('.').collect();
        let label_ok = |l: &&str| {
            !l.is_empty()
                && l.len() <= 63
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        };
        let plain = name.len() <= 253
            && labels.len() >= 2
            && labels.iter().all(label_ok)
            && !labels
                .last()
                .is_some_and(|l| l.chars().all(|c| c.is_ascii_digit()));
        if !plain {
            return Err(format!(
                "{}: a host name like registry.example.com, or .example.com for its subdomains",
                raw.trim()
            ));
        }
        let built_in = crate::seat_gateway::BUILT_IN;
        if built_in.contains(&host.as_str())
            || (!host.starts_with('.')
                && crate::seat_gateway::allowed(&format!("{host}:443"), built_in, 443))
        {
            return Err(format!("{host}: seats already reach it"));
        }
        if !hosts.contains(&host) {
            hosts.push(host);
        }
    }
    Ok(hosts)
}

fn validate_env(requested: Vec<EnvVar>) -> Result<Vec<EnvVar>, String> {
    let mut env: Vec<EnvVar> = Vec::new();
    for var in requested {
        let name = var.name.trim().to_string();
        let plain = !name.is_empty()
            && !name.starts_with(|c: char| c.is_ascii_digit())
            && name
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
        if !plain {
            return Err(format!("{name:?}: use an UPPER_CASE variable name"));
        }
        if RESERVED_ENV.contains(&name.as_str()) || name.starts_with("BAKERY_") {
            return Err(format!("{name}: set by Bakery itself, never granted"));
        }
        if looks_like_secret(&name) {
            return Err(format!(
                "{name}: looks like a credential — machine access never hands the agent a secret"
            ));
        }
        if CLAUDE_CODE_PROVIDERS.contains(&name.as_str()) {
            return Err(format!(
                "{name}: signs Claude Code in somewhere else — its requests go through the Bakery's door only"
            ));
        }
        if var.value.contains('\0') || var.value.len() > 4096 {
            return Err(format!("{name}: value must be plain text under 4 KiB"));
        }
        if env.iter().any(|e| e.name == name) {
            return Err(format!("{name}: set twice"));
        }
        env.push(EnvVar {
            name,
            value: var.value,
            scope: var.scope,
            preset: var.preset,
        });
    }
    Ok(env)
}

/// What one runtime start gets beyond its sandbox — Baker conversations only.
#[derive(Debug, Default)]
pub(crate) struct SpawnGrants {
    pub(crate) read: Vec<PathBuf>,
    pub(crate) write: Vec<PathBuf>,
    pub(crate) exec: Vec<PathBuf>,
    /// Includes one `ACP_PATH_PREFIX_ENV` entry per PATH grant, in order.
    pub(crate) env: Vec<(String, String)>,
    /// The PATH grants themselves, in order, for a command room.
    pub(crate) path: Vec<PathBuf>,
    /// The operator's own deny list; the sandbox adds the built-in one.
    pub(crate) deny: Vec<PathBuf>,
    /// `Some` when this conversation is set to Host full.
    pub(crate) whole_machine: Option<bakery_exec::WholeMachine>,
}

/// This conversation's place on the dial. Host full needs a trusted
/// conversation: an untrusted one stays in its room whatever is stored.
pub(crate) fn reach_for(state: &AppState, stream_id: &str) -> Reach {
    // The node's lock goes before the project's reach takes the Heart's.
    let set = state.node.lock().unwrap().machine_access.reach.get(stream_id).copied();
    let stored = set.unwrap_or_else(|| project_reach(state, stream_id));
    if stored == Reach::HostFull && !crate::state::stream_host_allowed(state, stream_id) {
        return Reach::HostRoom;
    }
    stored
}

/// Where a thread runs when nobody set it: the whole Mac for the Baker and
/// the Bakery's own workspaces (their orcs and workers) until the operator
/// gates them; else its project's sandbox kind, a Linux container seat for
/// `container`, else its room on this Mac.
fn project_reach(state: &AppState, stream_id: &str) -> Reach {
    if bakery_own(state, stream_id) {
        return Reach::HostFull;
    }
    match crate::catalog::stacks_for_socket(state, stream_id)
        .sandbox
        .as_str()
    {
        "container" => Reach::Container,
        _ => Reach::default(),
    }
}

/// The Baker's own conversations, and those in one of the Bakery's own
/// workspaces with no catalog project of their own.
fn bakery_own(state: &AppState, stream_id: &str) -> bool {
    if crate::baker_ops::is_baker_scope_stream(state, stream_id) {
        return true;
    }
    let heart = state.heart.lock().unwrap();
    crate::state::socket_project(&heart, stream_id).0.is_none()
        && crate::workspaces::in_bakery_workspace_locked(&heart, stream_id)
}

/// What a Host full sandbox is built from on this machine.
pub(crate) fn whole_machine(state: &AppState) -> bakery_exec::WholeMachine {
    bakery_exec::WholeMachine {
        home: home_of(state),
        bakery_root: state.base.to_path_buf(),
        // The node runs from the operator's shell, so this is their PATH.
        path: std::env::var("PATH").unwrap_or_default(),
    }
}

/// The whole deny list: Bakery's own state, the built-in secret stores, and
/// the operator's additions.
pub(crate) fn deny_list(state: &AppState) -> Vec<PathBuf> {
    let operator = state.node.lock().unwrap().machine_access.deny.clone();
    std::iter::once(state.base.to_path_buf())
        .chain(bakery_exec::default_deny_paths())
        .chain(operator.into_iter().map(PathBuf::from))
        .map(|p| canon(&p))
        .collect()
}

/// The always-blocked path `path` falls under — the operator's deny list, the
/// built-in one, Bakery's own folder — if any. Bakery's own folder doesn't
/// count inside `workspace`: a chat's folder lives in it.
pub(crate) fn always_blocked(state: &AppState, path: &Path, workspace: &Path) -> Option<PathBuf> {
    let base = canon(&state.base);
    let in_workspace = path.starts_with(workspace);
    deny_list(state)
        .into_iter()
        .find(|d| path.starts_with(d) && !(in_workspace && *d == base))
}

/// What the app shows from `path` is a secret: a credential file, or one on
/// the always-blocked list. A reply that carries one says `secret: true`, and
/// while the app shows it no desktop screenshot is taken.
pub(crate) fn is_secret(state: &AppState, path: &Path, workspace: &Path) -> bool {
    bakery_exec::is_credential_filename(path) || always_blocked(state, path, workspace).is_some()
}

/// `path` with every part that exists resolved (symlinks included); the part
/// that doesn't exist yet is kept as written, and may not climb with `..`.
fn resolve(path: &Path) -> Result<PathBuf, String> {
    let mut missing = Vec::new();
    let mut at = path;
    loop {
        if let Ok(mut real) = at.canonicalize() {
            real.extend(missing.iter().rev());
            return Ok(real);
        }
        let (Some(name), Some(parent)) = (at.file_name(), at.parent()) else {
            return Err(format!("{}: not a usable path", path.display()));
        };
        missing.push(name.to_os_string());
        at = parent;
    }
}

/// Read-only check for node tools that act on a path for an agent: `Ok` when
/// `stream_id`'s place on the dial reaches `path` for `access`, `Err` saying
/// why not. The deny list wins at every level; Bakery's own state is
/// reachable only as the conversation's own workspace; what the operator's
/// sessions run later is never writable. A relative path is taken inside the
/// conversation's workspace.
pub(crate) fn scope_allows(
    state: &AppState,
    stream_id: &str,
    path: &Path,
    access: Access,
) -> Result<(), String> {
    let workspace = crate::state::context_for(state, stream_id)
        .ok()
        .map(|ctx| canon(Path::new(&ctx.worktree_path)));
    let wanted = match &workspace {
        _ if path.is_absolute() => path.to_path_buf(),
        Some(w) => w.join(path),
        None => return Err(format!("{}: relative, with no workspace", path.display())),
    };
    let real = resolve(&wanted)?;
    let in_workspace = workspace.as_ref().is_some_and(|w| real.starts_with(w));
    let base = canon(&state.base);
    if let Some(blocked) = deny_list(state)
        .into_iter()
        .find(|d| real.starts_with(d) && !(in_workspace && *d == base))
    {
        return Err(format!(
            "{} is always blocked ({}) — never work around it",
            real.display(),
            blocked.display()
        ));
    }
    if bakery_exec::is_credential_filename(&real) {
        return Err(bakery_exec::CREDENTIAL_SURFACE_DENIED.to_string());
    }
    let writes = matches!(access, Access::Write | Access::Full);
    if writes && whole_machine(state).runs_later(&real) {
        return Err(format!(
            "{}: your shell, login items or git run this later, outside any sandbox — never writable",
            real.display()
        ));
    }
    if in_workspace {
        if writes && !crate::state::stream_host_allowed(state, stream_id) {
            return Err(format!("{}: this workspace is read-only", real.display()));
        }
        return Ok(());
    }
    match reach_for(state, stream_id) {
        Reach::HostFull => return Ok(()),
        Reach::Locked => {}
        Reach::HostRoom | Reach::Container => {
            if policy_for(state, stream_id)
                .grants
                .iter()
                .any(|g| real.starts_with(&g.path) && g.access.covers(access))
            {
                return Ok(());
            }
        }
    }
    // The Baker asks for a folder itself; any other agent asks the operator.
    let ask = if crate::baker_ops::is_baker_scope_stream(state, stream_id) {
        "call access_request for it; the operator approves it once"
    } else {
        "ask the operator for more, never work around it"
    };
    Err(format!(
        "{} is outside your workspace and not granted ({}) — {ask}",
        real.display(),
        access.label()
    ))
}

/// The grants and variables that reach `stream_id`'s runtime.
fn policy_for(state: &AppState, stream_id: &str) -> MachineAccess {
    let baker = crate::baker_ops::is_baker_scope_stream(state, stream_id);
    let project = (!baker)
        .then(|| crate::state::socket_project(&state.heart.lock().unwrap(), stream_id).0)
        .flatten();
    policy_as(state, baker, project.as_deref())
}

/// The Baker gets its own and the both-ways grants; any other agent gets
/// the every-project grants plus its own project's environment. Each is
/// granted where it resolves now, checked as a save checks it: a folder
/// that's gone is granted to no one (a start would make it again), and one
/// that came back as a link to what is never granted stays out.
fn policy_as(state: &AppState, baker: bool, project: Option<&str>) -> MachineAccess {
    let mut policy = state.node.lock().unwrap().machine_access.clone();
    policy.grants.retain(|g| g.scope.reaches(baker));
    policy.env.retain(|v| v.scope.reaches(baker));
    if let Some(own) = project.and_then(|id| policy.projects.remove(id)) {
        policy.grants.extend(own.grants);
        policy.env.extend(own.env);
    }
    let checks = GrantChecks::of(state, &policy);
    policy.grants.retain_mut(|g| match checks.now(&g.path) {
        Ok(real) => {
            g.path = real.display().to_string();
            true
        }
        Err(_) => false,
    });
    policy.projects.clear();
    policy
}

/// What a grant is checked against as a runtime starts.
struct GrantChecks {
    home: PathBuf,
    base: PathBuf,
    denied: Vec<PathBuf>,
}

impl GrantChecks {
    fn of(state: &AppState, policy: &MachineAccess) -> GrantChecks {
        GrantChecks {
            home: canon(&home_of(state)),
            base: canon(&state.base),
            denied: grant_denied(&policy.deny),
        }
    }

    /// Where `path` resolves now, or why it isn't granted: `None` when it's
    /// gone.
    fn now(&self, path: &str) -> Result<PathBuf, Option<String>> {
        let real = Path::new(path).canonicalize().map_err(|_| None)?;
        match refused(&real, &self.home, &self.base, &self.denied) {
            Some(why) => Err(Some(why)),
            None => Ok(real),
        }
    }
}

/// The granted folders granted to no one now, every project's included:
/// gone (`missing`), or resolving to what is never granted (`blocked`, with
/// why).
fn unusable(state: &AppState, policy: &MachineAccess) -> (Vec<String>, std::collections::BTreeMap<String, String>) {
    let checks = GrantChecks::of(state, policy);
    let (mut gone, mut blocked) = (Vec::new(), std::collections::BTreeMap::new());
    for g in policy.grants.iter().chain(policy.projects.values().flat_map(|p| p.grants.iter())) {
        match checks.now(&g.path) {
            Ok(_) => {}
            Err(None) => gone.push(g.path.clone()),
            Err(Some(why)) => {
                blocked.insert(g.path.clone(), why);
            }
        }
    }
    gone.sort();
    gone.dedup();
    (gone, blocked)
}

/// What a thread's runtime is granted as it starts: its door opened, its
/// Mac sandbox readied and its project's sign-ins' stand-ins set.
pub(crate) fn for_stream(state: &AppState, stream_id: &str) -> SpawnGrants {
    grants_for(state, stream_id, true)
}

/// [`for_stream`]'s grants as the agents list shows them, with nothing
/// opened or readied: showing an agent never starts its room's setup.
pub(crate) fn shown_for(state: &AppState, stream_id: &str) -> SpawnGrants {
    grants_for(state, stream_id, false)
}

fn grants_for(state: &AppState, stream_id: &str, starting: bool) -> SpawnGrants {
    let mut policy = policy_for(state, stream_id);
    let home = home_of(state);
    policy.grants.extend(working_path(&home));
    policy.env.extend(working_env(&home));
    let reach = reach_for(state, stream_id);
    crate::xcode_env::extend(state, stream_id, reach, &mut policy);
    let mut git = Vec::new();
    // Git rule 3: an agent's git runs the Bakery's hooks (`git_hooks`).
    if let Some((hooks, settings)) = crate::git_hooks::for_agents(state) {
        policy.grants.push(grant(&hooks, Access::Exec, false, "git-hooks"));
        git.extend(settings);
    }
    // In a room on this Mac, its own git push and gh are held for the
    // operator (`remote_ask`): the helper and the stand-in go first on its
    // PATH, and they ask at its thread's door.
    let mac = matches!(reach, Reach::HostRoom | Reach::HostFull);
    let shims = remote_binary(state).filter(|_| mac).and_then(|b| crate::remote_ask::shims(&b, &crate::state::cache_root(state)).ok());
    if let Some((bin, staged)) = shims {
        policy.grants.insert(0, grant(&bin, Access::Exec, true, "remote-ask"));
        policy.grants.push(grant(&staged, Access::Exec, false, "remote-ask"));
        git.extend(crate::remote_ask::git_config());
    }
    if !git.is_empty() {
        policy.env.extend(crate::git_hooks::config_env(&git).into_iter().map(|(name, value)| EnvVar {
            name,
            value,
            scope: Scope::AllAgents,
            preset: Some("git".to_string()),
        }));
    }
    let mut grants = spawn_grants(policy);
    if mac {
        crate::builds::extend(state, stream_id, &mut grants);
    }
    if !starting {
        if reach == Reach::HostRoom {
            crate::mac_sandbox::extend(state, stream_id, &mut grants, false);
        }
        return grants;
    }
    match reach {
        Reach::HostFull => {
            grants.whole_machine = Some(host_full(state, &mut grants.env));
            // Its project's own services (`services`).
            grants.env.extend(crate::services::env_for(state, stream_id));
            // No closed ports here, but its programs still ask at its door.
            if let Ok(port) = crate::doors::door(state, stream_id) {
                grants.env.push((crate::doors::URL_ENV.to_string(), format!("http://127.0.0.1:{port}")));
            }
        }
        Reach::HostRoom => {
            // Its room reaches the Bakery only through its own door.
            grants
                .env
                .extend(crate::doors::room_env(state, stream_id));
            // Its project's own services, which only its rooms reach.
            grants.env.extend(crate::services::env_for(state, stream_id));
            // Its project's sign-ins, as stand-ins its door makes real, and
            // the door's own authority to trust for Google's.
            if let Ok(door) = crate::doors::door(state, stream_id) {
                let swap = crate::swap::env_for(state, stream_id, door);
                if swap.iter().any(|(name, _)| name == "SSL_CERT_FILE") {
                    grants.read.push(crate::swap_tls::trust_dir(&state.base));
                }
                grants.env.extend(swap);
            }
            // The secret-named files the operator let its project's builds
            // read (`secret_reads`).
            if let Ok(context) = crate::state::context_for(state, stream_id) {
                let folder = PathBuf::from(&context.worktree_path);
                let folder = folder.canonicalize().unwrap_or(folder);
                let readable = crate::secret_reads::readable_in(state, stream_id, &folder);
                if !readable.is_empty() {
                    let lines: Vec<String> = readable.iter().map(|p| p.display().to_string()).collect();
                    grants.env.push((bakery_exec::READABLE_ENV.to_string(), lines.join("\n")));
                }
            }
            crate::mac_sandbox::extend(state, stream_id, &mut grants, true);
        }
        _ => {}
    }
    grants
}

/// The binary a room's push helper and gh stand-in run: this node's own. A
/// test node names one in `remote-binary` in its folder, or has none.
fn remote_binary(state: &AppState) -> Option<PathBuf> {
    if cfg!(test) {
        return std::fs::read_to_string(state.base.join("remote-binary")).ok().map(|p| PathBuf::from(p.trim()));
    }
    std::env::current_exe().ok()
}

/// Host full for one runtime start. The agent keeps its own HOME, so tools
/// that keep their install under the operator's home are pointed back at it.
fn host_full(state: &AppState, env: &mut Vec<(String, String)>) -> bakery_exec::WholeMachine {
    let machine = whole_machine(state);
    // ponytail: rustup only; name more tool homes here as tools need them.
    let rustup = machine.home.join(".rustup");
    if rustup.is_dir() {
        env.push(("RUSTUP_HOME".to_string(), rustup.display().to_string()));
    }
    machine
}

fn spawn_grants(policy: MachineAccess) -> SpawnGrants {
    let mut out = SpawnGrants {
        deny: policy.deny.iter().map(PathBuf::from).collect(),
        ..SpawnGrants::default()
    };
    for grant in policy.grants {
        let path = PathBuf::from(&grant.path);
        if grant.on_path {
            out.env.push((
                bakery_exec::ACP_PATH_PREFIX_ENV.to_string(),
                grant.path.clone(),
            ));
            out.path.push(path.clone());
        }
        match grant.access {
            Access::Read => out.read.push(path),
            Access::Write => out.write.push(path),
            Access::Exec => out.exec.push(path),
            Access::Full => {
                out.write.push(path.clone());
                out.exec.push(path);
            }
        }
    }
    out.env
        .extend(policy.env.into_iter().map(|var| (var.name, var.value)));
    out
}

/// What every agent runs by name without the operator granting it, read and
/// run only: mise's tools and Homebrew's (gh among them), in that order, as
/// the operator's own shell finds them. Credential-less: the Keychain is
/// closed to every room, and an agent's own home holds no sign-in.
fn working_path(home: &Path) -> Vec<Grant> {
    let mut grants = Vec::new();
    let mise = home.join(".local/share/mise");
    if mise.join("shims").is_dir() {
        grants.push(grant(&mise, Access::Exec, false, "working-path"));
        grants.push(grant(&mise.join("shims"), Access::Exec, true, "working-path"));
        for dir in [home.join(".config/mise"), home.join(".local/state/mise")] {
            if dir.is_dir() {
                grants.push(grant(&dir, Access::Read, false, "working-path"));
            }
        }
    }
    // rustup's cargo and rustc, which pick the toolchain a project pins: its
    // proxies, its installed toolchains and its record of them, never
    // ~/.cargo as a whole (the crates.io token lives there); crates download
    // into the room's own CARGO_HOME (`working_env`).
    let (cargo_bin, rustup) = (home.join(".cargo/bin"), home.join(".rustup"));
    if cargo_bin.join("cargo").is_file() && rustup.join("toolchains").is_dir() {
        grants.push(grant(&cargo_bin, Access::Exec, true, "working-path"));
        grants.push(grant(&rustup.join("toolchains"), Access::Exec, false, "working-path"));
        for kept in ["settings.toml", "update-hashes"] {
            grants.push(grant(&rustup.join(kept), Access::Read, false, "working-path"));
        }
    }
    if let Some(prefix) = ["/opt/homebrew", "/usr/local"].map(Path::new).into_iter().find(|p| p.join("bin/brew").is_file()) {
        grants.push(grant(prefix, Access::Exec, false, "working-path"));
        grants.push(grant(&prefix.join("bin"), Access::Exec, true, "working-path"));
    }
    grants
}

/// The working path alone, as a room takes it: what the operator's own
/// shell in a thread's room runs by name, as the thread's agents do.
pub(crate) fn working_tools(state: &AppState, stream_id: &str) -> SpawnGrants {
    let home = home_of(state);
    let mut policy = MachineAccess { grants: working_path(&home), env: working_env(&home), ..MachineAccess::default() };
    crate::xcode_env::extend(state, stream_id, reach_for(state, stream_id), &mut policy);
    let mut grants = spawn_grants(policy);
    crate::builds::extend(state, stream_id, &mut grants);
    grants
}

/// mise and rustup inside a room: the operator's setup, since an agent's HOME
/// is its own; cargo's crates go into the room's own CARGO_HOME (a thread's
/// project's, once it has one: `builds`), so the operator's registry, which
/// their own builds use, is never changed.
fn working_env(home: &Path) -> Vec<EnvVar> {
    let mut vars: Vec<(&str, String)> = Vec::new();
    let mise = home.join(".local/share/mise");
    if mise.join("shims").is_dir() {
        vars.extend([
            ("MISE_DATA_DIR", mise.display().to_string()),
            ("MISE_CONFIG_DIR", home.join(".config/mise").display().to_string()),
            ("MISE_STATE_DIR", home.join(".local/state/mise").display().to_string()),
        ]);
    }
    if home.join(".cargo/bin/cargo").is_file() && home.join(".rustup/toolchains").is_dir() {
        vars.extend([
            ("RUSTUP_HOME", home.join(".rustup").display().to_string()),
            ("CARGO_HOME", format!("{}/.cargo", bakery_exec::ROOM_HOME_ENV_VALUE)),
        ]);
    }
    vars.into_iter()
        .map(|(name, value)| EnvVar {
            name: name.to_string(),
            value,
            scope: Scope::AllAgents,
            preset: Some("working-path".to_string()),
        })
        .collect()
}

/// Whether `stream_id`'s agent may use containers, through Bakery's tools.
pub(crate) fn containers_on(state: &AppState, stream_id: &str) -> bool {
    state
        .node
        .lock()
        .unwrap()
        .machine_access
        .containers
        .contains(stream_id)
}

/// One session line telling an agent what it was granted, so it uses those
/// tools instead of concluding the machine is out of reach. Always present
/// for the Baker; for other agents only when a grant or containers reach
/// them.
pub(crate) fn session_line(state: &AppState, stream_id: &str) -> Option<String> {
    let line = match (reach_line(state, stream_id), mac_line(state, stream_id)) {
        (Some(line), Some(mac)) => Some(format!("{line}. {mac}")),
        (line, mac) => line.or(mac),
    };
    let line = match (line, crate::services::session_line(state, stream_id)) {
        (Some(line), Some(services)) => Some(format!("{line}. {services}")),
        (line, services) => line.or(services),
    };
    let line = match (line, crate::swap::session_line(state, stream_id)) {
        (Some(line), Some(sign_ins)) => Some(format!("{line}. {sign_ins}")),
        (line, sign_ins) => line.or(sign_ins),
    };
    if !containers_on(state, stream_id) {
        return line;
    }
    let engine = match crate::engine::docker_for(state, stream_id) {
        crate::containers::Backend::Orbstack => "orbstack (OrbStack)",
        _ => "docker (Colima)",
    };
    let containers = format!("containers: on, only through Bakery's container tools (containers_list, containers_run and the rest), never a Docker socket or Apple's container CLI; this project's Docker is backend {engine}; a mount reaches only what you reach yourself, and compose files stay with the operator");
    Some(match line {
        Some(line) => format!("{line}. {containers}"),
        None => containers,
    })
}

/// The Mac sandbox's toolchains, for a thread in its room on this Mac.
fn mac_line(state: &AppState, stream_id: &str) -> Option<String> {
    (reach_for(state, stream_id) == Reach::HostRoom)
        .then(|| crate::mac_sandbox::session_line(state, stream_id))
        .flatten()
}

fn reach_line(state: &AppState, stream_id: &str) -> Option<String> {
    match reach_for(state, stream_id) {
        Reach::HostFull => return Some(
            "machine access: the whole Mac, as the operator's own shell has it, except what is always blocked (Bakery's own state, ~/.ssh, ~/.claude, the Keychain, credential files, the operator's own list, and the files their shell, login items and git run later); Docker's socket and Apple container's CLI stay blocked. \"Operation not permitted\" means blocked: never work around it"
                .to_string(),
        ),
        Reach::Locked => return Some(
            "machine access: a Linux container with only this workspace; the network reaches model APIs, package registries (crates.io, npm, PyPI, GitHub) and the hosts the operator added, through a proxy, nothing else. Ask the operator for more, never work around it"
                .to_string(),
        ),
        Reach::Container | Reach::HostRoom => {}
    }
    let baker = crate::baker_ops::is_baker_scope_stream(state, stream_id);
    let policy = policy_for(state, stream_id);
    if !baker && policy.grants.is_empty() && policy.env.is_empty() {
        return None;
    }
    let list = |access: Access| {
        policy
            .grants
            .iter()
            .filter(|g| g.access == access)
            .map(|g| g.path.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut parts = Vec::new();
    for access in [Access::Full, Access::Exec, Access::Write, Access::Read] {
        let paths = list(access);
        if !paths.is_empty() {
            parts.push(format!("{}: {paths}", access.label()));
        }
    }
    let on_path: Vec<&str> = policy
        .grants
        .iter()
        .filter(|g| g.on_path)
        .map(|g| g.path.as_str())
        .collect();
    if !on_path.is_empty() {
        parts.push(format!("on PATH: {}", on_path.join(", ")));
    }
    // The Baker asks for a folder itself; any other agent asks the operator.
    let more = if baker {
        "for any other folder — a project to check out, a fresh clone or build — call access_request first and end your turn; the operator approves it once and this conversation continues with it reachable. \"Operation not permitted\" on a path means it is not granted yet: ask, never work around it"
    } else {
        "ask the operator for more, never work around it"
    };
    Some(if parts.is_empty() {
        format!("machine access: only this workspace; {more}")
    } else {
        format!(
            "machine access (granted by the operator in Settings → Machine access) — {}. {more}",
            parts.join("; ")
        )
    })
}

#[derive(schemars::JsonSchema, Debug, Clone, Serialize)]
pub(crate) struct Preset {
    pub(crate) id: &'static str,
    pub(crate) label: &'static str,
    pub(crate) detail: String,
    pub(crate) grants: Vec<Grant>,
    pub(crate) env: Vec<EnvVar>,
    /// Why it can't be offered on this machine, when it can't.
    pub(crate) unavailable: Option<String>,
}

fn grant(path: &Path, access: Access, on_path: bool, preset: &str) -> Grant {
    Grant {
        path: path.display().to_string(),
        access,
        scope: Scope::Baker,
        on_path,
        preset: Some(preset.to_string()),
    }
}

fn unavailable_preset(id: &'static str, label: &'static str, why: &str) -> Preset {
    Preset {
        id,
        label,
        detail: String::new(),
        grants: Vec::new(),
        env: Vec::new(),
        unavailable: Some(why.to_string()),
    }
}

/// The install root of a tool found on PATH: the nearest folder holding both
/// its PATH folder and its real (symlink-resolved) file — `/opt/homebrew`
/// for Homebrew, the version folder for nvm — or just the real file's folder
/// when those only meet somewhere far too broad to grant. Returns that root
/// and the (canonical) PATH folder.
fn install_root(on_path: &Path, home: &Path) -> Option<(PathBuf, PathBuf)> {
    let bin = on_path.parent()?.canonicalize().ok()?;
    let real_dir = on_path.canonicalize().ok()?.parent()?.to_path_buf();
    let broad = |p: &Path| p.components().count() <= 2 || p == home;
    let shared = bin
        .ancestors()
        .find(|a| real_dir.starts_with(a))
        .map(Path::to_path_buf);
    let root = match shared {
        // Not a link out of its folder: grant its install prefix, which holds
        // what it loads beside bin/ (Python's lib/, a runtime's share/).
        Some(shared) if shared == bin => bin
            .parent()
            .filter(|prefix| !broad(prefix))
            .map_or(bin.clone(), Path::to_path_buf),
        Some(shared) if !broad(&shared) => shared,
        _ => real_dir,
    };
    Some((root, bin))
}

/// Offers built from what this machine actually has, found the way the
/// operator's own shell finds it (the node's PATH). Nothing is granted until
/// the operator adds a preset.
pub(crate) fn presets(home: &Path) -> Vec<Preset> {
    let mut out = Vec::new();

    let dev_dirs: Vec<Grant> = bakery_exec::apple_developer_dirs()
        .iter()
        .filter(|d| d.is_dir())
        .map(|d| grant(d, Access::Exec, false, "xcode"))
        .collect();
    out.push(if dev_dirs.is_empty() {
        unavailable_preset(
            "xcode",
            "Xcode command line tools",
            "no Xcode command line tools found (xcode-select --install)",
        )
    } else {
        Preset {
            id: "xcode",
            label: "Xcode command line tools",
            detail: "git, cc and clang through their /usr/bin shims — needed for git and for linking Rust."
                .to_string(),
            grants: dev_dirs,
            env: Vec::new(),
            unavailable: None,
        }
    });

    let sysroot = crate::state::resolve_executable_path("rustc")
        .ok()
        .and_then(|rustc| {
            let out = std::process::Command::new(rustc)
                .args(["--print", "sysroot"])
                .output()
                .ok()?;
            out.status
                .success()
                .then(|| PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()))
        })
        .filter(|p| p.is_dir());
    out.push(match sysroot {
        Some(sysroot) => Preset {
            id: "rust",
            label: "Rust toolchain",
            detail: format!(
                "cargo and rustc from {}. Crates download into the Baker's own private cargo home, so ~/.cargo is never touched. Linking needs the Xcode tools.",
                sysroot.display()
            ),
            grants: vec![
                grant(&sysroot, Access::Exec, false, "rust"),
                grant(&sysroot.join("bin"), Access::Exec, true, "rust"),
            ],
            env: Vec::new(),
            unavailable: None,
        },
        None => unavailable_preset("rust", "Rust toolchain", "rustc is not on this node's PATH"),
    });

    let mut js: Vec<Grant> = Vec::new();
    let mut found = Vec::new();
    for tool in ["node", "pnpm"] {
        let Some((root, bin)) = crate::state::resolve_executable_path(tool)
            .ok()
            .and_then(|p| install_root(&p, home))
        else {
            continue;
        };
        found.push(tool);
        for g in [
            grant(&root, Access::Exec, false, "node"),
            grant(&bin, Access::Exec, true, "node"),
        ] {
            if let Some(existing) = js.iter_mut().find(|x| x.path == g.path) {
                existing.on_path |= g.on_path;
            } else {
                js.push(g);
            }
        }
    }
    out.push(if found.is_empty() {
        unavailable_preset(
            "node",
            "Node.js and pnpm",
            "neither node nor pnpm is on this node's PATH",
        )
    } else {
        Preset {
            id: "node",
            label: "Node.js and pnpm",
            detail: format!(
                "{} from where your shell finds them — e.g. pnpm check and pnpm build in this workspace.",
                found.join(" and ")
            ),
            grants: js,
            env: Vec::new(),
            unavailable: None,
        }
    });

    // The python3 your shell runs (mise, pyenv, Homebrew…) — never Apple's
    // /usr/bin shim, which already answers inside the sandbox via the Xcode
    // tools and silently runs a different Python.
    let python = crate::state::resolve_executable_path("python3")
        .ok()
        .filter(|p| !p.starts_with("/usr/bin"))
        .and_then(|p| install_root(&p, home));
    out.push(match python {
        Some((root, bin)) => Preset {
            id: "python",
            label: "Python",
            detail: format!(
                "python3 and pip from {} — the Python your shell runs. Virtual environments live in the workspace; pip's cache in the agent's private home.",
                root.display()
            ),
            grants: vec![
                grant(&root, Access::Exec, false, "python"),
                grant(&bin, Access::Exec, true, "python"),
            ],
            env: Vec::new(),
            unavailable: None,
        },
        None => unavailable_preset(
            "python",
            "Python",
            "only Apple's python3 is on this node's PATH",
        ),
    });

    // Flutter pinned per project by FVM, or a flutter on PATH. Flutter locks
    // and updates its own bin/cache on every run, so the SDK needs write;
    // packages download into each agent's private pub cache, never yours.
    let fvm = std::env::var_os("FVM_CACHE_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join("fvm"))
        .join("versions");
    let mut sdks: Vec<PathBuf> = std::fs::read_dir(&fvm)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.join("bin/flutter").is_file())
                .collect()
        })
        .unwrap_or_default();
    if let Some(sdk) = crate::state::resolve_executable_path("flutter")
        .ok()
        .and_then(|p| p.canonicalize().ok())
        .and_then(|p| p.parent()?.parent().map(Path::to_path_buf))
    {
        if !sdks.contains(&sdk) {
            sdks.push(sdk);
        }
    }
    sdks.sort();
    out.push(if sdks.is_empty() {
        unavailable_preset(
            "flutter",
            "Flutter",
            "no Flutter SDK found (FVM versions or flutter on PATH)",
        )
    } else {
        let mut grants = Vec::new();
        for (i, sdk) in sdks.iter().enumerate() {
            grants.push(grant(sdk, Access::Write, false, "flutter"));
            // Only the newest goes on PATH; `fvm flutter` picks a project's pin.
            grants.push(grant(&sdk.join("bin"), Access::Exec, i + 1 == sdks.len(), "flutter"));
        }
        Preset {
            id: "flutter",
            label: "Flutter",
            detail: format!(
                "{} — read + write on the SDK, because Flutter locks and updates its own bin/cache. Packages download into each agent's private pub cache, so ~/.pub-cache is never touched. Needs the Xcode tools for git.",
                sdks.iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            grants,
            env: Vec::new(),
            unavailable: None,
        }
    });

    // Homebrew's whole prefix: uv, dart, pod, ruby, fvm and the rest.
    let brew = crate::state::resolve_executable_path("brew")
        .ok()
        .and_then(|p| p.parent()?.parent().map(Path::to_path_buf))
        .filter(|prefix| prefix.join("bin").is_dir());
    out.push(match brew {
        Some(prefix) => Preset {
            id: "homebrew",
            label: "Homebrew",
            detail: format!(
                "everything Homebrew installed in {} (uv, dart, pod, ruby, fvm…) — read and run only; agents can't install or change packages.",
                prefix.display()
            ),
            grants: vec![
                grant(&prefix, Access::Exec, false, "homebrew"),
                grant(&prefix.join("bin"), Access::Exec, true, "homebrew"),
            ],
            env: Vec::new(),
            unavailable: None,
        },
        None => unavailable_preset("homebrew", "Homebrew", "brew is not on this node's PATH"),
    });

    let gitconfig = [home.join(".gitconfig"), home.join(".config/git/config")]
        .into_iter()
        .find(|p| p.is_file());
    out.push(match gitconfig {
        Some(path) => Preset {
            id: "git-identity",
            label: "Git identity",
            detail: format!(
                "read {} so git knows your name and settings.",
                path.display()
            ),
            grants: vec![grant(&path, Access::Read, false, "git-identity")],
            env: vec![EnvVar {
                name: "GIT_CONFIG_GLOBAL".to_string(),
                value: path.display().to_string(),
                scope: Scope::Baker,
                preset: Some("git-identity".to_string()),
            }],
            unavailable: None,
        },
        None => unavailable_preset(
            "git-identity",
            "Git identity",
            "git has no name or email on this machine — run git config --global user.name / user.email, then add this",
        ),
    });
    out
}

#[derive(schemars::JsonSchema, Debug, Clone, Serialize)]
pub(crate) struct LiveRuntime {
    pub(crate) stream_id: String,
    pub(crate) busy: bool,
    pub(crate) baker: bool,
}

#[derive(schemars::JsonSchema, Debug, Clone, Serialize)]
pub(crate) struct View {
    pub(crate) policy: MachineAccess,
    pub(crate) presets: Vec<Preset>,
    /// Denied under every grant, emitted after every allow.
    pub(crate) always_denied: Vec<String>,
    /// The hosts every container seat may reach; the operator adds more.
    pub(crate) seat_hosts: Vec<String>,
    /// Native runtimes running now — started under the grants in force then.
    pub(crate) live: Vec<LiveRuntime>,
    /// Granted folders that are gone: kept, and granted to no one until they
    /// are back.
    pub(crate) missing: Vec<String>,
    /// Granted folders that now resolve to what is never granted (a link to
    /// a secret store, say), by path, with why: kept, and granted to no one.
    pub(crate) blocked: std::collections::BTreeMap<String, String>,
    /// The operator's home, which the screen shows as `~`.
    pub(crate) home: String,
}

pub(crate) fn home_of(state: &AppState) -> PathBuf {
    state
        .base
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from(std::env::var("HOME").unwrap_or_default()))
}

fn live_runtimes(state: &AppState) -> Vec<LiveRuntime> {
    let keys: Vec<(String, crate::node::Seat)> =
        state.acp_sessions.lock().unwrap().keys().cloned().collect();
    let running = state.lifecycle.lock().unwrap().running_turns.clone();
    let mut out: Vec<LiveRuntime> = Vec::new();
    for key in keys {
        let busy = running.get(&key).is_some_and(|&n| n > 0);
        if let Some(seen) = out.iter_mut().find(|r| r.stream_id == key.0) {
            seen.busy |= busy;
            continue;
        }
        out.push(LiveRuntime {
            baker: crate::baker_ops::is_baker_scope_stream(state, &key.0),
            stream_id: key.0,
            busy,
        });
    }
    out.sort_by(|a, b| a.stream_id.cmp(&b.stream_id));
    out
}

pub(crate) fn view(state: &AppState) -> View {
    // Cloned out on its own statement: as a field of the literal below, the
    // guard lived until the literal was done — across `live_runtimes`,
    // which takes this same lock, deadlocking the whole node.
    let policy = state.node.lock().unwrap().machine_access.clone();
    let (missing, blocked) = unusable(state, &policy);
    View {
        missing,
        blocked,
        home: home_of(state).display().to_string(),
        policy,
        presets: presets(&home_of(state)),
        // Bakery's own folder first: every agent reaches only its own home
        // and workspace inside it.
        always_denied: std::iter::once(state.base.to_path_buf())
            .chain(bakery_exec::default_deny_paths())
            .map(|p| p.display().to_string())
            .collect(),
        seat_hosts: crate::seat_gateway::BUILT_IN
            .iter()
            .map(|h| h.to_string())
            .collect(),
        live: live_runtimes(state),
    }
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct Saved {
    #[serde(flatten)]
    pub(crate) view: View,
    /// Idle runtimes stopped so their next turn starts under the new grants.
    pub(crate) restarted: Vec<String>,
    /// Mid-turn runtimes; each restarts under the new grants when its turn
    /// ends.
    pub(crate) busy: Vec<String>,
}

/// Grants are fixed into a runtime's sandbox when it starts, so an idle
/// runtime is stopped here; its next turn starts a fresh one (history is
/// replayed into it). A runtime mid-turn is never killed: it restarts the
/// moment its turn ends.
fn restart_idle_runtimes(state: &AppState) -> (Vec<String>, Vec<String>) {
    let (mut restarted, mut busy) = (Vec::new(), Vec::new());
    for runtime in live_runtimes(state) {
        if crate::state::restart_acp_runtimes_when_idle(state, &runtime.stream_id) {
            restarted.push(runtime.stream_id);
        } else {
            busy.push(runtime.stream_id);
        }
    }
    (restarted, busy)
}

// ─── Asking for a folder ────────────────────────────────────────────────────

/// A Baker conversation asked the operator for a folder (`access_request`).
pub(crate) const ACCESS_REQUESTED: &str = "agent.machine_access.requested";
/// The operator's answer; its causality cites the request.
pub(crate) const ACCESS_DECIDED: &str = "agent.machine_access.decided";

#[derive(Debug, Deserialize)]
pub(crate) struct AccessRequest {
    pub(crate) path: String,
    pub(crate) access: String,
    #[serde(default)]
    pub(crate) reason: String,
}

fn parse_access(word: &str) -> Result<Access, String> {
    match word.trim().to_ascii_lowercase().as_str() {
        "read" => Ok(Access::Read),
        "write" => Ok(Access::Write),
        "run" | "exec" => Ok(Access::Exec),
        "full" => Ok(Access::Full),
        other => Err(format!(
            "access must be read, write, run, or full (read, write and run) — not {other:?}"
        )),
    }
}

fn is_decided(events: &[bakery_core::Event], request: &str) -> bool {
    events
        .iter()
        .any(|e| e.family == ACCESS_DECIDED && e.causality.iter().any(|c| c == request))
}

/// `access_request`: record the ask in the Baker's chat, where the operator
/// allows or denies it. Nothing is reachable until they allow it. The path
/// passes the same checks as Settings: it must exist, must not be too broad,
/// and is never Bakery's own state or a secret store.
pub(crate) fn request_access(
    state: &AppState,
    stream_id: &str,
    req: AccessRequest,
) -> Result<String, String> {
    if !crate::baker_ops::is_baker_scope_stream(state, stream_id) {
        return Err("access_request belongs to a Baker conversation".to_string());
    }
    let access = parse_access(&req.access)?;
    let asked = Grant {
        path: req.path,
        access,
        scope: Scope::Baker,
        on_path: false,
        preset: None,
    };
    let deny = state.node.lock().unwrap().machine_access.deny.clone();
    let path = validate_grants(vec![asked], &home_of(state), &state.base, &deny)?
        .remove(0)
        .path;
    if policy_for(state, stream_id)
        .grants
        .iter()
        .any(|g| Path::new(&path).starts_with(&g.path) && g.access.covers(access))
    {
        // Granted while this turn was running: its runtime restarts with the
        // grant when the turn ends, so carry on from the next one.
        let restarting = state
            .lifecycle
            .lock()
            .unwrap()
            .restart_when_idle
            .contains(stream_id);
        if !restarting {
            return Ok(format!("{path} is already granted to you ({}) — go ahead.", access.label()));
        }
        continue_after_grant(state, stream_id, &path, access)?;
        return Ok(format!(
            "{path} was just granted to you ({}); it reaches you from your next turn. End this turn now with one short line — Bakery continues this conversation by itself.",
            access.label()
        ));
    }
    let events = state
        .heart
        .lock()
        .unwrap()
        .read_stream(stream_id)
        .map_err(|e| e.to_string())?;
    let waiting = events.iter().any(|e| {
        let p: serde_json::Value = serde_json::from_str(&e.payload).unwrap_or_default();
        e.family == ACCESS_REQUESTED
            && p["path"] == path.as_str()
            && p["access"] == serde_json::json!(access)
            && !is_decided(&events, &e.event_hash)
    });
    if !waiting {
        crate::state::append(
            state,
            stream_id,
            "baker",
            ACCESS_REQUESTED,
            serde_json::json!({
                "agent_id": crate::node::agent_id(stream_id, crate::node::Seat::Baker),
                "seat": crate::node::Seat::Baker,
                "path": path,
                "access": access,
                "reason": req.reason.trim(),
            })
            .to_string(),
            vec![],
        )?;
    }
    Ok(format!(
        "Asked the operator for {} access to {path}. End this turn now with one short line saying what you asked for and why. Once they allow it, Bakery restarts your runtime with the folder reachable and continues this conversation by itself; do not try the folder before then.",
        access.label()
    ))
}

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct DecideAccess {
    pub(crate) stream_id: String,
    pub(crate) event_hash: String,
    pub(crate) allow: bool,
}

/// The operator's answer to one `access_request`. Allow adds the grant
/// (Baker scope, so it shows and can be removed in Settings → Machine
/// access), restarts the conversation's runtime under it — now, or when its
/// turn ends — and queues a message so the conversation picks up where it
/// stopped.
pub(crate) fn decide_access(
    state: &AppState,
    req: &DecideAccess,
) -> Result<bakery_core::Event, (StatusCode, String)> {
    let internal = |e: String| (StatusCode::INTERNAL_SERVER_ERROR, e);
    let one_decision = state.edit_decision_lock.lock().unwrap();
    let events = state
        .heart
        .lock()
        .unwrap()
        .read_stream(&req.stream_id)
        .map_err(|e| internal(e.to_string()))?;
    let request = events
        .iter()
        .find(|e| e.event_hash == req.event_hash && e.family == ACCESS_REQUESTED)
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                "no such folder request in this conversation".to_string(),
            )
        })?;
    if is_decided(&events, &request.event_hash) {
        return Err((
            StatusCode::CONFLICT,
            "this folder request was already answered".to_string(),
        ));
    }
    let asked: serde_json::Value = serde_json::from_str(&request.payload).unwrap_or_default();
    let path = asked["path"].as_str().unwrap_or_default().to_string();
    let access: Access = serde_json::from_value(asked["access"].clone())
        .map_err(|e| internal(format!("unreadable folder request: {e}")))?;
    if req.allow {
        let mut node = state.node.lock().unwrap();
        let mut policy = node.machine_access.clone();
        match policy
            .grants
            .iter_mut()
            .find(|g| g.path == path && g.scope == Scope::Baker)
        {
            Some(g) => g.access = g.access.union(access),
            None => policy.grants.push(Grant {
                path: path.clone(),
                access,
                scope: Scope::Baker,
                on_path: false,
                preset: None,
            }),
        }
        node.machine_access = validate(policy, &home_of(state), &state.base)
            .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
        node.save(&state.base).map_err(internal)?;
    }
    let decided = crate::state::append(
        state,
        &req.stream_id,
        "human",
        ACCESS_DECIDED,
        serde_json::json!({
            "decided_event": request.event_hash,
            "allow": req.allow,
            "path": path,
            "access": access,
            "agent_id": asked["agent_id"],
        })
        .to_string(),
        vec![request.event_hash.clone()],
    )
    .map_err(internal)?;
    drop(one_decision);
    if req.allow {
        // The grant reaches every Baker conversation, so every live Baker
        // runtime restarts under it — now, or when its turn ends.
        let mut bakers: Vec<String> = live_runtimes(state)
            .into_iter()
            .filter(|r| r.baker)
            .map(|r| r.stream_id)
            .collect();
        if !bakers.contains(&req.stream_id) {
            bakers.push(req.stream_id.clone());
        }
        for stream in &bakers {
            crate::state::restart_acp_runtimes_when_idle(state, stream);
        }
        continue_after_grant(state, &req.stream_id, &path, access).map_err(internal)?;
    }
    Ok(decided)
}

/// Queue the conversation to carry on now that `path` is reachable; it runs
/// once the agent is idle, after its runtime restarted under the grant.
fn continue_after_grant(
    state: &AppState,
    stream_id: &str,
    path: &str,
    access: Access,
) -> Result<(), String> {
    crate::agent_control::enqueue_as(
        state,
        crate::routes::agent::AgentTurnReq {
            stream_id: stream_id.to_string(),
            file_path: None,
            instruction: format!(
                "The operator allowed {} access to {path}. Continue where you left off.",
                access.label()
            ),
            seat: Some(crate::node::Seat::Baker),
            model_id: None,
            effort: None,
            actor: None,
        },
        vec![],
        false,
        "bakery",
    )
    .map(drop)
}

pub(crate) async fn decide_access_request(
    State(st): State<AppState>,
    Json(req): Json<DecideAccess>,
) -> Result<Json<bakery_core::Event>, (StatusCode, String)> {
    tokio::task::spawn_blocking(move || decide_access(&st, &req).map(Json))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
}

pub(crate) async fn machine_access(
    State(st): State<AppState>,
) -> Result<Json<View>, (StatusCode, String)> {
    tokio::task::spawn_blocking(move || view(&st))
        .await
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

pub(crate) async fn set_machine_access(
    State(st): State<AppState>,
    Json(policy): Json<MachineAccess>,
) -> Result<Json<Saved>, (StatusCode, String)> {
    tokio::task::spawn_blocking(move || {
        let mut policy =
            validate(policy, &home_of(&st), &st.base).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
        {
            let mut node = st.node.lock().unwrap();
            // Set per agent, never by a policy save that may be stale.
            policy.reach = node.machine_access.reach.clone();
            policy.containers = node.machine_access.containers.clone();
            policy.seat_size = node.machine_access.seat_size.clone();
            node.machine_access = policy;
            node.save(&st.base)
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
        }
        let (restarted, busy) = restart_idle_runtimes(&st);
        Ok(Json(Saved {
            view: view(&st),
            restarted,
            busy,
        }))
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
}

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct ReachReq {
    stream_id: String,
    reach: Reach,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct ReachSet {
    reach: Reach,
    /// False while a turn runs: the runtime restarts under it when the turn
    /// ends.
    restarted: bool,
}

/// `POST /machine-access/reach`: one conversation's place on the dial. Its
/// runtime restarts under it, now or when its turn ends.
pub(crate) async fn set_reach(
    State(st): State<AppState>,
    Json(req): Json<ReachReq>,
) -> Result<Json<ReachSet>, (StatusCode, String)> {
    tokio::task::spawn_blocking(move || {
        crate::state::context_for(&st, &req.stream_id).map_err(|e| (StatusCode::NOT_FOUND, e))?;
        if req.reach == Reach::HostFull && !crate::state::stream_host_allowed(&st, &req.stream_id) {
            return Err((
                StatusCode::BAD_REQUEST,
                "an untrusted conversation stays in its room".to_string(),
            ));
        }
        {
            // Kept only where it differs from its default.
            let default = project_reach(&st, &req.stream_id);
            let mut node = st.node.lock().unwrap();
            let stored = &mut node.machine_access.reach;
            match req.reach {
                reach if reach == default => stored.remove(&req.stream_id),
                reach => stored.insert(req.stream_id.clone(), reach),
            };
            node.save(&st.base)
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
        }
        Ok(Json(ReachSet {
            reach: req.reach,
            restarted: crate::state::restart_acp_runtimes_when_idle(&st, &req.stream_id),
        }))
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
}

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct SeatSizeReq {
    stream_id: String,
    /// `None` returns the seat to the default sized from this Mac.
    size: Option<SeatSize>,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct SeatSizeSet {
    /// What the seat gets from its next start.
    size: SeatSize,
    /// False while a turn runs: the runtime restarts when the turn ends.
    restarted: bool,
}

/// `POST /machine-access/seat-size`: one container seat's CPUs and memory.
/// Its runtime restarts under them, now or when its turn ends.
pub(crate) async fn set_seat_size(
    State(st): State<AppState>,
    Json(req): Json<SeatSizeReq>,
) -> Result<Json<SeatSizeSet>, (StatusCode, String)> {
    tokio::task::spawn_blocking(move || {
        crate::state::context_for(&st, &req.stream_id).map_err(|e| (StatusCode::NOT_FOUND, e))?;
        if let Some(size) = req.size {
            crate::seat_container::check_size(size).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
        }
        {
            let mut node = st.node.lock().unwrap();
            let stored = &mut node.machine_access.seat_size;
            match req.size {
                Some(size) => stored.insert(req.stream_id.clone(), size),
                None => stored.remove(&req.stream_id),
            };
            node.save(&st.base)
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
        }
        Ok(Json(SeatSizeSet {
            size: crate::seat_container::seat_size(&st, &req.stream_id),
            restarted: crate::state::restart_acp_runtimes_when_idle(&st, &req.stream_id),
        }))
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
}

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct ContainersReq {
    stream_id: String,
    on: bool,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct ContainersSet {
    on: bool,
    /// False while a turn runs: the runtime restarts when the turn ends.
    restarted: bool,
}

/// `POST /machine-access/containers`: whether one conversation's agent may
/// use containers, through Bakery's container tools only. Its runtime
/// restarts so its session line says so, now or when its turn ends; the
/// tools check the switch on every call.
pub(crate) async fn set_containers(
    State(st): State<AppState>,
    Json(req): Json<ContainersReq>,
) -> Result<Json<ContainersSet>, (StatusCode, String)> {
    tokio::task::spawn_blocking(move || {
        crate::state::context_for(&st, &req.stream_id).map_err(|e| (StatusCode::NOT_FOUND, e))?;
        {
            let mut node = st.node.lock().unwrap();
            let stored = &mut node.machine_access.containers;
            if req.on {
                stored.insert(req.stream_id.clone());
            } else {
                stored.remove(&req.stream_id);
            }
            node.save(&st.base)
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
        }
        Ok(Json(ContainersSet {
            on: req.on,
            restarted: crate::state::restart_acp_runtimes_when_idle(&st, &req.stream_id),
        }))
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
}

// ─── Check ──────────────────────────────────────────────────────────────────

/// Run inside a sandbox built exactly like a Baker runtime's: which tools
/// work, and whether the boundaries hold. Every write probe removes what it
/// made the moment it succeeds, so a broken boundary never leaves a file.
const CHECK_SCRIPT: &str = r#"#!/bin/sh
tool() {
  name="$1"; shift
  where=$(command -v "$1" 2>/dev/null)
  out=$("$@" 2>&1 </dev/null); code=$?
  # Xcode's shims log harmless sandbox noise (their cache, FSEvents); skip it.
  line=$(printf '%s\n' "$out" | grep -v -e "couldn't create cache file" -e "xcodebuild\[" -e "DVTFilePathFSEvents" | head -n 1)
  printf 'tool\t%s\t%s\t%s\t%s\n' "$name" "$code" "$where" "$line"
}
guard() {
  name="$1"; shift
  if err=$("$@" 2>&1 >/dev/null </dev/null); then r=allowed
  else case "$err" in
    *"not permitted"*) r=denied ;;
    *"No such file"*) r=absent ;;
    *) r="denied: $err" ;;
  esac; fi
  printf 'guard\t%s\t%s\n' "$name" "$r"
}
touch_rm() { touch "$1" && rm -f "$1"; }
mkdir_rmdir() { mkdir "$1" && rmdir "$1"; }
guard "read ~/.ssh" ls "$PROBE_HOME/.ssh"
guard "read your Keychains" ls "$PROBE_HOME/Library/Keychains"
guard "read the GitHub CLI's token folder" ls "$PROBE_HOME/.config/gh"
guard "read Bakery's own folder" ls "$PROBE_HOME/.bakery"
guard "read Bakery's own database" ls "$PROBE_HOME/.bakery/heart.db"
guard "reach Docker" /usr/bin/nc -U -w 1 "$PROBE_HOME/.colima/default/docker.sock"
if [ "$PROBE_REACH" = host_full ]; then
  # Appending nothing changes nothing, even where it is allowed.
  if [ -e "$PROBE_HOME/.zshrc" ]; then
    guard "change your shell startup files" sh -c ': >> "$PROBE_HOME/.zshrc"'
  fi
else
  guard "write in your home folder" touch_rm "$PROBE_HOME/.bakery-sandbox-probe-$$"
fi
if [ -d "$PROBE_WS/.claude" ]; then
  guard "change the project's .claude folder" touch_rm "$PROBE_WS/.claude/.bakery-sandbox-probe-$$"
else
  guard "create a .claude folder in the project" mkdir_rmdir "$PROBE_WS/.claude"
fi
tool git git --version
tool cc cc --version
tool cargo cargo --version
tool rustc rustc --version
tool node node --version
tool pnpm pnpm --version
tool python3 python3 --version
tool pip3 pip3 --version
tool uv uv --version
tool dart dart --version
tool flutter flutter --version
tool fvm fvm --version
tool pod pod --version
printf 'path\t%s\n' "$PATH"
"#;

#[derive(schemars::JsonSchema, Debug, Serialize, PartialEq)]
pub(crate) struct ToolProbe {
    pub(crate) name: String,
    pub(crate) works: bool,
    /// The file that actually ran — e.g. Apple's /usr/bin/python3 or yours.
    pub(crate) path: String,
    /// Its first line of output — the version, or why it didn't run.
    pub(crate) detail: String,
}

#[derive(schemars::JsonSchema, Debug, Serialize, PartialEq)]
pub(crate) struct GuardProbe {
    pub(crate) what: String,
    /// `denied` (held), `absent` (nothing there to protect), or `allowed`.
    pub(crate) result: String,
    pub(crate) held: bool,
}

#[derive(schemars::JsonSchema, Debug, Serialize, Default, PartialEq)]
pub(crate) struct CheckResult {
    pub(crate) tools: Vec<ToolProbe>,
    pub(crate) guards: Vec<GuardProbe>,
    pub(crate) path: String,
}

fn parse_check(out: &str) -> CheckResult {
    let mut result = CheckResult::default();
    for line in out.lines() {
        let fields: Vec<&str> = line.splitn(5, '\t').collect();
        match fields.as_slice() {
            ["tool", name, code, path, detail] => result.tools.push(ToolProbe {
                name: name.to_string(),
                works: *code == "0",
                path: path.to_string(),
                detail: detail.to_string(),
            }),
            ["guard", what, outcome] => result.guards.push(GuardProbe {
                what: what.to_string(),
                held: *outcome != "allowed",
                result: outcome.to_string(),
            }),
            ["path", path] => result.path = path.to_string(),
            _ => {}
        }
    }
    result
}

/// Checks as the Baker (its own workspace) or as any other agent (a scratch
/// workspace and a stream no Baker grant reaches).
fn run_check(state: &AppState, scope: Scope, project: Option<&str>) -> Result<CheckResult, String> {
    use std::io::Read;
    let cache_root = crate::state::cache_root(state);
    let (stream_id, workspace) = match scope {
        Scope::Baker => {
            let stream_id = state
                .node
                .lock()
                .unwrap()
                .baker_stream_id
                .clone()
                .ok_or("there is no Baker yet — open the Baker once, then check")?;
            let workspace =
                PathBuf::from(crate::state::context_for(state, &stream_id)?.worktree_path);
            (stream_id, workspace)
        }
        Scope::Agents | Scope::AllAgents => {
            let workspace = cache_root.join("machine-access-check").join("workspace");
            std::fs::create_dir_all(&workspace).map_err(|e| e.to_string())?;
            ("machine-access-check-agent".to_string(), workspace)
        }
    };
    let env = bakery_exec::ensure_thin_room(&workspace, &cache_root, &stream_id)?;
    let script = cache_root.join("machine-access-check.sh");
    std::fs::write(&script, CHECK_SCRIPT).map_err(|e| e.to_string())?;
    let staged = bakery_exec::stage_acp_binary(&script, &cache_root)?;
    // Its own home, never a real Baker's.
    let seat_home = bakery_exec::seat_acp_home(&cache_root, "machine-access-check", "baker");
    let mut grants = spawn_grants(policy_as(state, scope == Scope::Baker, project));
    // As the Baker: at the Baker's own place on the dial.
    let reach = match scope {
        Scope::Baker => reach_for(state, &stream_id),
        Scope::Agents | Scope::AllAgents => Reach::HostRoom,
    };
    if reach == Reach::HostFull {
        grants.whole_machine = Some(host_full(state, &mut grants.env));
    }
    let mut extra_env = grants.env.clone();
    extra_env.push(("PROBE_HOME".into(), home_of(state).display().to_string()));
    extra_env.push(("PROBE_WS".into(), workspace.display().to_string()));
    let reach_name = serde_json::to_value(reach)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default();
    extra_env.push(("PROBE_REACH".into(), reach_name));
    let handle = bakery_exec::spawn_boxed_acp(
        &env,
        &cache_root,
        &staged,
        &seat_home,
        &workspace,
        &grants.deny,
        &extra_env,
        &[],
        crate::state::stream_host_allowed(state, &stream_id),
        &grants.exec,
        &grants.write,
        &grants.read,
        grants.whole_machine.as_ref(),
        &[],
    )?;
    let (_, stdin, mut stdout, _stderr, kill) = handle.into_io_and_killer();
    drop(stdin);
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut out = String::new();
        let _ = stdout.read_to_string(&mut out);
        let _ = tx.send(out);
    });
    let out = rx.recv_timeout(std::time::Duration::from_secs(90));
    kill();
    out.map(|out| parse_check(&out))
        .map_err(|_| "the check did not finish within 90 s".to_string())
}

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct CheckReq {
    #[serde(default)]
    scope: Scope,
    /// Check as an agent of this project (its own environment included).
    #[serde(default)]
    project_id: Option<String>,
}

pub(crate) async fn check_machine_access(
    State(st): State<AppState>,
    Json(req): Json<CheckReq>,
) -> Result<Json<CheckResult>, (StatusCode, String)> {
    tokio::task::spawn_blocking(move || run_check(&st, req.scope, req.project_id.as_deref()))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map(Json)
        .map_err(|e| (StatusCode::CONFLICT, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;
    use bakery_agent::StubModel;
    use std::sync::Arc;

    /// The check runs in a real Baker sandbox: every boundary holds, and a
    /// tool with no grant (Rust, here) is reported as not available.
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn the_check_proves_the_boundaries_hold_in_a_real_baker_sandbox() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = crate::routes::project::open_stream(&state, &open_req(&repo, "check"))
            .unwrap()
            .context
            .stream_id;
        state.node.lock().unwrap().baker_stream_id = Some(stream_id.clone());
        set_reach(&state, &stream_id, Reach::HostRoom);

        let result = run_check(&state, Scope::Baker, None).unwrap();
        assert!(!result.guards.is_empty(), "{result:?}");
        for guard in &result.guards {
            assert!(guard.held, "{} was {}", guard.what, guard.result);
        }
        assert!(
            !repo.join(".claude").exists(),
            "the probe never leaves a folder behind"
        );
        let cargo = result.tools.iter().find(|t| t.name == "cargo").unwrap();
        assert!(!cargo.works, "no Rust grant, so no cargo: {cargo:?}");
        assert!(result.path.contains("/usr/bin"), "{}", result.path);
    }

    /// What the Check button reports on this machine with every preset it
    /// offers granted. Machine-dependent, so only on request:
    /// `cargo test -p bakery-node check_with_every_preset -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn check_with_every_preset() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = crate::routes::project::open_stream(&state, &open_req(&repo, "presets"))
            .unwrap()
            .context
            .stream_id;
        let home = PathBuf::from(std::env::var("HOME").unwrap());
        let mut all = MachineAccess::default();
        for preset in presets(&home) {
            all.grants.extend(preset.grants);
            all.env.extend(preset.env);
        }
        {
            let mut node = state.node.lock().unwrap();
            node.baker_stream_id = Some(stream_id.clone());
            node.machine_access = validate(all, &home, &home.join(".bakery")).unwrap();
        }
        set_reach(&state, &stream_id, Reach::HostRoom);
        let result = run_check(&state, Scope::Baker, None).unwrap();
        for g in &result.guards {
            println!("guard  {:<42} {}", g.what, g.result);
        }
        for t in &result.tools {
            println!("tool   {:<9} {:<5} {}", t.name, t.works, t.detail);
        }
        println!("PATH   {}", result.path);
        assert!(result.guards.iter().all(|g| g.held));
    }

    #[test]
    fn access_levels_cover_and_combine() {
        assert!(Access::Full.covers(Access::Write) && Access::Full.covers(Access::Exec));
        assert!(Access::Write.covers(Access::Read) && Access::Exec.covers(Access::Read));
        assert!(!Access::Read.covers(Access::Write) && !Access::Exec.covers(Access::Write));
        assert_eq!(Access::Write.union(Access::Exec), Access::Full);
        assert_eq!(Access::Read.union(Access::Write), Access::Write);
        let full = spawn_grants(MachineAccess {
            grants: vec![Grant {
                path: "/p".to_string(),
                access: Access::Full,
                scope: Scope::Baker,
                on_path: false,
                preset: None,
            }],
            ..Default::default()
        });
        assert_eq!((full.write.len(), full.exec.len()), (1, 1), "full = write + run");
    }

    /// The Baker asks for a folder in its chat; asking grants nothing. Allow
    /// saves a Baker grant and queues the conversation to carry on; a second
    /// answer is refused; Deny grants nothing and queues nothing.
    #[test]
    fn the_baker_asks_for_a_folder_and_only_an_allow_grants_it() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let baker = crate::routes::project::open_stream(&state, &open_req(&repo, "baker"))
            .unwrap()
            .context
            .stream_id;
        state.node.lock().unwrap().baker_stream_id = Some(baker.clone());
        // Queued messages stay queued: this test checks them, it runs no turn.
        crate::agent_control::pause_dispatch(&state, &baker, crate::node::Seat::Baker).unwrap();
        let (project, declined) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let ask = |stream: &str, path: &Path, access: &str| {
            request_access(
                &state,
                stream,
                AccessRequest {
                    path: path.display().to_string(),
                    access: access.to_string(),
                    reason: "check out the clone".to_string(),
                },
            )
        };
        let events = || state.heart.lock().unwrap().read_stream(&baker).unwrap();
        let requests = || {
            events()
                .into_iter()
                .filter(|e| e.family == ACCESS_REQUESTED)
                .collect::<Vec<_>>()
        };
        let queued = || {
            events()
                .iter()
                .filter(|e| e.family == crate::agent_control::FOLLOW_UP_QUEUED)
                .count()
        };

        // The folders granted, beside the Baker's shared build folder and
        // what `builds::extend` gives every room on this Mac (its cargo home,
        // its worktree's own target).
        let asked = |granted: &SpawnGrants| -> Vec<PathBuf> {
            let builds = crate::builds::folder(&state, crate::builds::BAKERY);
            let mut every_room = SpawnGrants::default();
            crate::builds::extend(&state, &baker, &mut every_room);
            granted.write.iter().filter(|p| !p.starts_with(&builds) && !every_room.write.contains(p)).cloned().collect()
        };
        assert!(ask("wt:not-the-baker", project.path(), "full").is_err());
        let too_broad = ask(&baker, &home_of(&state), "read").unwrap_err();
        assert!(too_broad.contains("too broad"), "{too_broad}");
        assert!(ask(&baker, project.path(), "everything").is_err());

        let said = ask(&baker, project.path(), "full").unwrap();
        assert!(said.contains("End this turn"), "{said}");
        ask(&baker, project.path(), "full").unwrap();
        assert_eq!(requests().len(), 1, "asking twice records one request");
        assert!(asked(&for_stream(&state, &baker)).is_empty(), "asking grants nothing");

        let decide = |hash: &str, allow: bool| {
            decide_access(
                &state,
                &DecideAccess {
                    stream_id: baker.clone(),
                    event_hash: hash.to_string(),
                    allow,
                },
            )
        };
        let request = requests()[0].event_hash.clone();
        assert_eq!(decide(&request, true).unwrap().family, ACCESS_DECIDED);
        let granted = project.path().canonicalize().unwrap();
        let reach = for_stream(&state, &baker);
        assert_eq!(asked(&reach), vec![granted.clone()]);
        assert!(reach.exec.contains(&granted), "beside the working PATH: {:?}", reach.exec);
        assert_eq!(queued(), 1, "the conversation is queued to carry on");
        assert_eq!(decide(&request, true).unwrap_err().0, StatusCode::CONFLICT);
        std::fs::create_dir(granted.join("src")).unwrap();
        let inside = ask(&baker, &granted.join("src"), "read").unwrap();
        assert!(inside.contains("already granted"), "a folder inside a grant: {inside}");

        // Granted while a turn runs: that turn's runtime lacks it until the
        // turn ends, so asking again says so and queues the carry-on.
        let lease = crate::state::admit_turn(&state, &baker, crate::node::Seat::Baker).unwrap();
        assert!(!crate::state::restart_acp_runtimes_when_idle(&state, &baker));
        let mid_turn = ask(&baker, &granted, "read").unwrap();
        assert!(mid_turn.contains("next turn"), "{mid_turn}");
        assert_eq!(queued(), 2);
        drop(lease);

        ask(&baker, declined.path(), "write").unwrap();
        let second = requests()[1].event_hash.clone();
        decide(&second, false).unwrap();
        assert_eq!(asked(&for_stream(&state, &baker)), vec![granted], "a deny grants nothing");
        assert_eq!(queued(), 2, "a deny queues nothing");
    }

    /// A grant change that lands mid-turn restarts the runtime the moment
    /// that turn ends, never under it.
    #[test]
    fn a_runtime_restart_waits_for_the_running_turn() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let lease = crate::state::admit_turn(&state, "wt:busy", crate::node::Seat::Baker).unwrap();
        assert!(!crate::state::restart_acp_runtimes_when_idle(&state, "wt:busy"));
        assert!(state.lifecycle.lock().unwrap().restart_when_idle.contains("wt:busy"));
        drop(lease);
        assert!(!state.lifecycle.lock().unwrap().restart_when_idle.contains("wt:busy"));
        assert!(crate::state::restart_acp_runtimes_when_idle(&state, "wt:busy"));
    }

    /// A grant reaches the Baker, or every agent — never an agent it wasn't
    /// given to. The check as "any agent" runs with only the shared grants.
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn each_agent_gets_only_the_grants_meant_for_it() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let baker_only = tempfile::tempdir().unwrap();
        let agents_only = tempfile::tempdir().unwrap();
        let everyone = tempfile::tempdir().unwrap();
        let grant = |path: &Path, scope| Grant {
            path: path.canonicalize().unwrap().display().to_string(),
            access: Access::Read,
            scope,
            on_path: false,
            preset: None,
        };
        {
            let mut node = state.node.lock().unwrap();
            node.baker_stream_id = Some("wt:baker".to_string());
            node.machine_access.grants = vec![
                grant(baker_only.path(), Scope::Baker),
                grant(agents_only.path(), Scope::Agents),
                grant(everyone.path(), Scope::AllAgents),
            ];
        }
        let canon = |d: &tempfile::TempDir| d.path().canonicalize().unwrap();
        assert_eq!(
            for_stream(&state, "wt:baker").read,
            vec![canon(&baker_only), canon(&everyone)],
            "the Baker never gets an agents-only grant"
        );
        assert_eq!(
            for_stream(&state, "wt:project").read,
            vec![canon(&agents_only), canon(&everyone)],
            "a project agent never gets a Baker-only grant"
        );
        assert!(session_line(&state, "wt:project").is_some());
        state.node.lock().unwrap().machine_access.grants =
            vec![grant(baker_only.path(), Scope::Baker)];
        assert!(
            session_line(&state, "wt:project").is_none(),
            "no grant reaches it, so no line"
        );

        let result = run_check(&state, Scope::Agents, None).unwrap();
        assert!(result.guards.iter().all(|g| g.held), "{result:?}");
    }

    /// A project's own environment reaches that project's agents only —
    /// never another project's, never the Baker's — on top of the
    /// every-project grants; and an empty project entry is dropped on save.
    #[test]
    fn a_projects_own_environment_reaches_only_its_agents() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let (farm, farm_socket) = crate::catalog::create_project(&state, "Farm", None).unwrap();
        let (_, other_socket) = crate::catalog::create_project(&state, "Other", None).unwrap();
        let every = tempfile::tempdir().unwrap();
        let farm_only = tempfile::tempdir().unwrap();
        let grant = |path: &Path, scope| Grant {
            path: path.canonicalize().unwrap().display().to_string(),
            access: Access::Read,
            scope,
            on_path: false,
            preset: None,
        };
        let var = |name: &str| EnvVar {
            name: name.into(),
            value: "1".into(),
            scope: Scope::Agents,
            preset: None,
        };
        let mut policy = MachineAccess {
            grants: vec![grant(every.path(), Scope::Agents)],
            ..Default::default()
        };
        policy.projects.insert(
            farm.project_id.clone(),
            ProjectAccess {
                grants: vec![grant(farm_only.path(), Scope::Agents)],
                env: vec![var("FARM_MODE")],
            },
        );
        policy
            .projects
            .insert("empty".into(), ProjectAccess::default());
        let policy = validate(policy, &home_of(&state), &state.base).unwrap();
        assert!(!policy.projects.contains_key("empty"));
        {
            let mut node = state.node.lock().unwrap();
            node.baker_stream_id = Some("wt:baker".into());
            node.machine_access = policy;
        }
        let canon = |d: &tempfile::TempDir| d.path().canonicalize().unwrap();

        let farm_agent = for_stream(&state, &farm_socket.stream_id);
        assert_eq!(farm_agent.read, vec![canon(&every), canon(&farm_only)]);
        assert!(farm_agent.env.iter().any(|(k, _)| k == "FARM_MODE"));
        assert_eq!(
            for_stream(&state, &other_socket.stream_id).read,
            vec![canon(&every)]
        );
        assert!(
            for_stream(&state, "wt:baker").read.is_empty(),
            "the Baker gets neither"
        );
    }

    #[test]
    fn check_output_parses_tools_guards_and_path() {
        let out = "guard\tread ~/.ssh\tdenied\nguard\twrite home\tallowed\ntool\tgit\t0\t/usr/bin/git\tgit version 2\ntool\tflutter\t127\t\tnot found\npath\t/a:/b\n";
        let result = parse_check(out);
        assert_eq!(result.guards.len(), 2);
        assert!(result.guards[0].held && !result.guards[1].held);
        assert!(result.tools[0].works && !result.tools[1].works);
        assert_eq!(result.tools[0].path, "/usr/bin/git");
        assert_eq!(result.path, "/a:/b");
    }

    fn policy(grants: Vec<(&Path, Access, bool)>, env: Vec<(&str, &str)>) -> MachineAccess {
        MachineAccess {
            grants: grants
                .into_iter()
                .map(|(path, access, on_path)| Grant {
                    path: path.display().to_string(),
                    access,
                    scope: Scope::Baker,
                    on_path,
                    preset: None,
                })
                .collect(),
            env: env
                .into_iter()
                .map(|(name, value)| EnvVar {
                    name: name.to_string(),
                    value: value.to_string(),
                    scope: Scope::Baker,
                    preset: None,
                })
                .collect(),
            ..Default::default()
        }
    }

    /// The operator is kept from granting what defeats the boundary: the
    /// whole disk or home, Bakery's own state, PATH on a folder that can't
    /// run, or a credential through env.
    #[test]
    fn validation_refuses_grants_that_defeat_the_boundary() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let base = home.join(".bakery");
        let tools = home.join("tools/bin");
        std::fs::create_dir_all(&base).unwrap();
        std::fs::create_dir_all(&tools).unwrap();
        let check = |grants, env| validate(policy(grants, env), &home, &base);

        assert!(check(vec![(Path::new("/"), Access::Read, false)], vec![]).is_err());
        assert!(check(vec![(home.as_path(), Access::Read, false)], vec![]).is_err());
        assert!(check(vec![(base.as_path(), Access::Read, false)], vec![]).is_err());
        assert!(check(vec![(tools.as_path(), Access::Read, true)], vec![]).is_err());
        let climbs = home.join("gone/..");
        assert!(check(vec![(climbs.as_path(), Access::Read, false)], vec![]).is_err());
        assert!(check(vec![], vec![("GITHUB_TOKEN", "x")]).is_err());
        assert!(check(vec![], vec![("PATH", "/x")]).is_err());
        assert!(check(vec![], vec![("lower", "x")]).is_err());
        // Claude Code's own settings pass, token counts among them; its
        // sign-ins never do, whatever they are called.
        for setting in [
            "MAX_THINKING_TOKENS",
            "CLAUDE_CODE_MAX_OUTPUT_TOKENS",
            "MAX_MCP_OUTPUT_TOKENS",
            "CLAUDE_CODE_FILE_READ_MAX_OUTPUT_TOKENS",
            "BASH_DEFAULT_TIMEOUT_MS",
            "BASH_MAX_TIMEOUT_MS",
            "ENABLE_TOOL_SEARCH",
        ] {
            assert!(check(vec![], vec![(setting, "1")]).is_ok(), "{setting}");
        }
        for sign_in in [
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            "ANTHROPIC_BASE_URL",
            "CLAUDE_CODE_OAUTH_TOKEN",
            "CLAUDE_CODE_USE_BEDROCK",
            "CLAUDE_CODE_SKIP_VERTEX_AUTH",
            "CLAUDE_CODE_CLIENT_KEY_PASSPHRASE",
            "OTEL_EXPORTER_OTLP_HEADERS",
            "NPM_CONFIG__AUTH",
        ] {
            assert!(check(vec![], vec![(sign_in, "1")]).is_err(), "{sign_in}");
        }

        let ok = check(
            vec![(tools.as_path(), Access::Exec, true)],
            vec![("CARGO_TERM_COLOR", "never")],
        )
        .unwrap();
        assert_eq!(
            ok.grants[0].path,
            tools.canonicalize().unwrap().display().to_string()
        );
    }

    /// A folder that IS a host secret store (or sits inside one) is refused
    /// outright rather than granted and then silently denied.
    #[test]
    fn validation_refuses_the_hosts_secret_stores() {
        let real_home = PathBuf::from(std::env::var("HOME").unwrap());
        let ssh = real_home.join(".ssh");
        if !ssh.is_dir() {
            return;
        }
        let err = validate(
            policy(vec![(ssh.as_path(), Access::Read, false)], vec![]),
            &real_home,
            &real_home.join(".bakery"),
        )
        .unwrap_err();
        assert!(err.contains("always denied"), "{err}");
    }

    /// A tool reached through a symlink is granted at the folder holding both
    /// its PATH entry and its real file, so the link and its target both run.
    #[test]
    fn a_linked_tool_is_granted_at_its_install_root() {
        let root = tempfile::tempdir().unwrap();
        let prefix = root.path().join("opt/brew");
        let real = prefix.join("Cellar/tool/1.0/bin/tool");
        std::fs::create_dir_all(real.parent().unwrap()).unwrap();
        std::fs::write(&real, "").unwrap();
        let link = prefix.join("bin/tool");
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let (grant_root, bin) = install_root(&link, Path::new("/nowhere")).unwrap();
        assert_eq!(grant_root, prefix.canonicalize().unwrap());
        assert_eq!(bin, prefix.join("bin").canonicalize().unwrap());

        // A tool that lives in its own bin/ (mise's python3) is granted at its
        // install prefix, so what it loads beside bin/ is readable too.
        let python = root.path().join("mise/installs/python/3.12.14");
        std::fs::create_dir_all(python.join("bin")).unwrap();
        std::fs::create_dir_all(python.join("lib/python3.12")).unwrap();
        std::fs::write(python.join("bin/python3"), "").unwrap();
        let (grant_root, bin) =
            install_root(&python.join("bin/python3"), Path::new("/nowhere")).unwrap();
        assert_eq!(grant_root, python.canonicalize().unwrap());
        assert_eq!(bin, python.join("bin").canonicalize().unwrap());
    }

    // ── The scope dial and the deny list ────────────────────────────────────

    fn set_reach(state: &AppState, stream_id: &str, reach: Reach) {
        state
            .node
            .lock()
            .unwrap()
            .machine_access
            .reach
            .insert(stream_id.to_string(), reach);
    }

    fn real(p: &Path) -> String {
        p.canonicalize().unwrap().display().to_string()
    }

    /// The operator extends the deny list. Nothing they save shrinks it below
    /// the built-in list, and no grant reaches into what they denied.
    #[test]
    fn the_deny_list_extends_but_never_shrinks() {
        let home = tempfile::tempdir().unwrap();
        let base = home.path().join(".bakery");
        let private = home.path().join("private");
        std::fs::create_dir_all(&base).unwrap();
        std::fs::create_dir_all(private.join("inner")).unwrap();
        let save = |deny: &[&str], grants: Vec<Grant>| {
            validate(
                MachineAccess {
                    deny: deny.iter().map(|d| d.to_string()).collect(),
                    grants,
                    ..Default::default()
                },
                home.path(),
                &base,
            )
        };

        let saved = save(&["~/private", "~/private", "~/not-yet"], vec![]).unwrap();
        assert_eq!(
            saved.deny,
            [real(&private), format!("{}/not-yet", real(home.path()))]
        );
        for (entry, why) in [
            ("relative/path", "absolute"),
            ("/", "too broad"),
            ("~", "too broad"),
            ("~/.bakery/cache", "already always blocked"),
        ] {
            let err = save(&[entry], vec![]).unwrap_err();
            assert!(err.contains(why), "{entry}: {err}");
        }
        let inside = Grant {
            path: private.join("inner").display().to_string(),
            access: Access::Read,
            scope: Scope::Baker,
            on_path: false,
            preset: None,
        };
        let err = save(&["~/private"], vec![inside]).unwrap_err();
        assert!(err.contains("always denied"), "{err}");
    }

    /// The operator adds hosts seats may reach the way they add to the deny
    /// list: names only (an address could be this Mac's own loopback or the
    /// local network), and nothing the built-in list already allows.
    #[test]
    fn seat_hosts_are_names_the_built_in_list_lacks() {
        let save = |hosts: &[&str]| validate_hosts(hosts.iter().map(|h| h.to_string()).collect());
        assert_eq!(
            save(&[
                " Registry.Example.COM ",
                ".corp.example",
                "registry.example.com"
            ])
            .unwrap(),
            ["registry.example.com", ".corp.example"]
        );
        for (entry, why) in [
            ("localhost", "a host name"),
            ("10.0.0.1", "a host name"),
            ("[::1]", "a host name"),
            ("*.example.com", "a host name"),
            ("-bad.example.com", "a host name"),
            ("crates.io", "already reach"),
            ("objects.githubusercontent.com", "already reach"),
            (".githubusercontent.com", "already reach"),
        ] {
            let err = save(&[entry]).unwrap_err();
            assert!(err.contains(why), "{entry}: {err}");
        }
    }

    /// `scope_allows` gives node tools the same answer the sandbox gives the
    /// agent: workspace and grants in its room, the whole machine in Host
    /// full, and the deny list, Bakery's own state, credential files and
    /// what runs later never.
    #[test]
    fn scope_allows_follows_the_dial_and_the_deny_list() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = crate::routes::project::open_stream(&state, &open_req(&repo, "scope"))
            .unwrap()
            .context
            .stream_id;
        let outside = tempfile::tempdir().unwrap();
        let notes = outside.path().join("notes.txt");
        std::fs::write(&notes, "notes").unwrap();
        let private = outside.path().join("private");
        std::fs::create_dir_all(&private).unwrap();
        let allows = |path: &Path, access: Access| scope_allows(&state, &stream_id, path, access);

        // Its room: the workspace, relative paths included, even new ones.
        assert_eq!(allows(Path::new("src/new.rs"), Access::Write), Ok(()));
        let err = allows(&notes, Access::Read).unwrap_err();
        assert!(err.contains("not granted"), "{err}");
        state.node.lock().unwrap().machine_access.grants.push(Grant {
            path: real(outside.path()),
            access: Access::Read,
            scope: Scope::AllAgents,
            on_path: false,
            preset: None,
        });
        assert_eq!(allows(&notes, Access::Read), Ok(()));
        assert!(allows(&notes, Access::Write).is_err(), "a read grant never writes");

        // Host full: the machine.
        set_reach(&state, &stream_id, Reach::HostFull);
        assert_eq!(reach_for(&state, &stream_id), Reach::HostFull);
        assert_eq!(allows(&notes, Access::Write), Ok(()));

        // Never, at any level.
        state
            .node
            .lock()
            .unwrap()
            .machine_access
            .deny
            .push(real(&private));
        let err = allows(&private.join("a.txt"), Access::Read).unwrap_err();
        assert!(err.contains("always blocked"), "{err}");
        let err = allows(&dir.path().join("cache/seats/x"), Access::Read).unwrap_err();
        assert!(err.contains("always blocked"), "Bakery's own state: {err}");
        assert!(allows(&outside.path().join(".env"), Access::Read).is_err());
        let zshrc = home_of(&state).join(".zshrc");
        let err = allows(&zshrc, Access::Write).unwrap_err();
        assert!(err.contains("never writable"), "{err}");
        assert!(allows(&outside.path().join("a/../../x"), Access::Read).is_err());
    }

    /// Host full needs a trusted conversation, and reaches the runtime only
    /// through its spawn grants; an untrusted one stays in its room.
    #[test]
    fn host_full_reaches_the_runtime_only_for_a_trusted_conversation() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let open = |name: &str| {
            crate::routes::project::open_stream(&state, &open_req(&repo, name))
                .unwrap()
                .context
                .stream_id
        };
        let (trusted, untrusted) = (open("trusted"), open("untrusted"));
        // An unreadable lifetime fails closed: not trusted.
        crate::state::append(
            &state,
            &untrusted,
            "human",
            crate::node::LIFETIME_FAMILY,
            "not json".to_string(),
            vec![],
        )
        .unwrap();
        for stream in [&trusted, &untrusted] {
            set_reach(&state, stream, Reach::HostFull);
        }

        let grants = for_stream(&state, &trusted);
        let machine = grants.whole_machine.expect("Host full reaches the spawn");
        assert_eq!(machine.bakery_root, *state.base);
        assert!(session_line(&state, &trusted).unwrap().contains("the whole Mac"));

        assert_eq!(reach_for(&state, &untrusted), Reach::HostRoom);
        assert!(for_stream(&state, &untrusted).whole_machine.is_none());
    }

    /// The Baker and the Bakery's own workspaces, orc and workers, start on
    /// the whole Mac until the operator gates them; a catalog project's
    /// agents and workspaces start in their room.
    #[tokio::test]
    async fn the_bakerys_own_start_on_the_whole_mac_and_projects_in_their_room() {
        let dir = tempfile::tempdir().unwrap();
        let repo = bakery_repo_for_test(dir.path());
        run_git(&repo, &["add", "."]);
        run_git(&repo, &["commit", "-qm", "the bakery"]);
        let state = state_with_bakery_root(dir.path(), &repo);
        let baker = crate::routes::baker::ensure_permanent_baker(&state, None, None).unwrap().stream_id;
        let thread = crate::baker_threads::create_thread(&state, &Default::default(), "human", None, None)
            .unwrap()
            .thread
            .stream_id;
        let orc = crate::workspaces::create_workspace(&state, "Fixes", None, "human").unwrap().stream_id;
        let req = crate::baker_threads::CreateThreadReq { title: Some("worker".into()), ..Default::default() };
        let worker = crate::baker_threads::create_peer(&state, &req, &orc, "human").unwrap().thread.stream_id;
        for own in [&baker, &thread, &orc, &worker] {
            assert_eq!(reach_for(&state, own), Reach::HostFull, "{own}");
            assert!(for_stream(&state, own).whole_machine.is_some(), "{own}");
        }

        let local = make_repo(&dir.path().join("local"));
        let root = crate::catalog::DirectoryRootSpec::Attach(local.to_str().unwrap());
        let (project, agent) = crate::catalog::create_project(&state, "Local", Some(root)).unwrap();
        let theirs = crate::workspaces::create_workspace(&state, "Styling", Some(&project.project_id), "human")
            .unwrap()
            .stream_id;
        for stream in [&agent.stream_id, &theirs] {
            assert_eq!(reach_for(&state, stream), Reach::HostRoom, "{stream}");
            assert!(for_stream(&state, stream).whole_machine.is_none(), "{stream}");
        }

        // The operator gates one: only that one goes to its room, and stays.
        let gate = ReachReq { stream_id: thread.clone(), reach: Reach::HostRoom };
        super::set_reach(State(state.clone()), Json(gate)).await.unwrap();
        assert_eq!(reach_for(&state, &thread), Reach::HostRoom);
        assert_eq!(reach_for(&state, &baker), Reach::HostFull);
    }

    /// A granted folder that's gone never blocks a save: its row is kept and
    /// shown missing, granted to no runtime (which would make it again)
    /// until it is back, and never when it comes back as a link to what is
    /// never granted.
    #[tokio::test]
    async fn a_folder_thats_gone_saves_and_shows_missing() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        state.node.lock().unwrap().baker_stream_id = Some("wt:baker".into());
        let outside = tempfile::tempdir().unwrap();
        let (kept, gone) = (outside.path().join("kept"), outside.path().join(".worktrees"));
        std::fs::create_dir_all(&kept).unwrap();
        std::fs::create_dir_all(&gone).unwrap();
        let mut saved = validate(
            policy(vec![(kept.as_path(), Access::Read, false), (gone.as_path(), Access::Write, false)], vec![]),
            &home_of(&state),
            &state.base,
        )
        .unwrap();
        let own = Grant { scope: Scope::Agents, ..saved.grants[1].clone() };
        saved.projects.insert("farm".into(), ProjectAccess { grants: vec![own], env: vec![] });
        let gone_at = real(&gone);
        std::fs::remove_dir(&gone).unwrap();

        let Json(out) = set_machine_access(State(state.clone()), Json(saved)).await.unwrap();
        assert_eq!(out.view.policy.grants.len(), 2, "the row is kept");
        assert!(out.view.policy.projects.contains_key("farm"));
        assert_eq!(out.view.missing, vec![gone_at.clone()]);
        assert_eq!(view(&state).missing, vec![gone_at.clone()]);
        let granted = for_stream(&state, "wt:baker");
        assert_eq!(granted.read, vec![PathBuf::from(real(&kept))]);
        assert!(!granted.write.contains(&PathBuf::from(&gone_at)), "granted to no one");
        assert!(!gone.exists(), "and never made again");

        // Back as a link to what is never granted (Bakery's own state, here):
        // shown blocked, and still granted to no one.
        std::os::unix::fs::symlink(&*state.base, &gone).unwrap();
        let shown = view(&state);
        assert!(shown.missing.is_empty());
        assert!(shown.blocked[&gone_at].contains("Bakery's own state"), "{:?}", shown.blocked);
        let granted = for_stream(&state, "wt:baker");
        let base = state.base.canonicalize().unwrap();
        assert!(!granted.write.iter().any(|p| *p == base || *p == PathBuf::from(&gone_at)), "{:?}", granted.write);
        std::fs::remove_file(&gone).unwrap();

        std::fs::create_dir_all(&gone).unwrap();
        assert!(view(&state).missing.is_empty());
        assert!(for_stream(&state, "wt:baker").write.contains(&PathBuf::from(&gone_at)), "granted again");
    }

    /// The Check runs a real Host full sandbox: the runtime starts, finds the
    /// node's own PATH, and every guard still holds.
    /// Every agent runs mise's and Homebrew's tools by name, read and run
    /// only, mise's shims first; mise finds the operator's own setup from
    /// inside its room.
    #[test]
    fn every_agent_runs_mise_and_homebrew_tools_by_name() {
        let home = tempfile::tempdir().unwrap();
        let shims = home.path().join(".local/share/mise/shims");
        assert!(!working_path(home.path()).iter().any(|g| g.path.contains("mise")), "no mise, no mise grants");
        assert!(working_env(home.path()).is_empty());
        std::fs::create_dir_all(&shims).unwrap();
        let grants = working_path(home.path());
        assert!(grants.iter().all(|g| matches!(g.access, Access::Exec | Access::Read)), "read and run only");
        let on_path: Vec<&str> = grants.iter().filter(|g| g.on_path).map(|g| g.path.as_str()).collect();
        assert_eq!(on_path.first().copied(), Some(shims.display().to_string()).as_deref(), "{on_path:?}");
        let names: Vec<String> = working_env(home.path()).into_iter().map(|v| v.name).collect();
        assert_eq!(names, ["MISE_DATA_DIR", "MISE_CONFIG_DIR", "MISE_STATE_DIR"]);
    }

    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn the_check_runs_a_real_host_full_sandbox() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = crate::routes::project::open_stream(&state, &open_req(&repo, "full"))
            .unwrap()
            .context
            .stream_id;
        state.node.lock().unwrap().baker_stream_id = Some(stream_id.clone());
        set_reach(&state, &stream_id, Reach::HostFull);

        let result = run_check(&state, Scope::Baker, None).unwrap();
        assert!(!result.guards.is_empty(), "{result:?}");
        for guard in &result.guards {
            assert!(guard.held, "{} was {}", guard.what, guard.result);
        }
        assert!(
            !result.guards.iter().any(|g| g.what == "write in your home folder"),
            "Host full may write the home folder; the Check doesn't flag it"
        );
        let node_path = std::env::var("PATH").unwrap();
        assert!(result.path.ends_with(&node_path), "{}", result.path);
    }

    fn probe(name: &str, command: String) -> String {
        format!("{command} >/dev/null 2>&1 && printf '{name}=yes ' || printf '{name}=no '; ")
    }

    /// One shell command in a command room with these grants: what it printed.
    fn command_room(granted: &SpawnGrants, script: &str) -> String {
        let (ws, cache) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let env = bakery_exec::room_for_run(ws.path(), cache.path(), "wt:command-room", "none").unwrap();
        let argv = ["/bin/sh".to_string(), "-c".to_string(), script.to_string()];
        let run = bakery_exec::WorkspaceRun {
            argv: &argv,
            extra_deny_read: &granted.deny,
            timeout: std::time::Duration::from_secs(60),
            extra_read: &granted.read,
            extra_write: &[],
            extra_exec: &granted.exec,
            whole_machine: None,
            path: &granted.path,
            env: &granted.env,
            append_only: &[],
        };
        let out = bakery_exec::attach_exec_workspace_rw(&env, ws.path(), &run).unwrap();
        format!("{}{}", out.stdout, out.stderr)
    }

    /// Live, on this Mac: a second thread of a project builds a crate with a
    /// C dependency (bundled SQLite) in a room of its own after a first
    /// thread built it, against the same build in a project that shares
    /// nothing with it. Needs the operator's Rust and their crates cache
    /// with libsqlite3-sys 0.28 and its build dependencies; the cache is
    /// cloned into each project's cargo home and the builds are offline, so
    /// no download is timed (an unshared build would pay it as well). Prints
    /// the three times.
    ///   cargo test -p bakery-node machine_access::tests::live_a_second -- --ignored --nocapture
    #[test]
    #[ignore = "builds a crate with a C dependency in real rooms"]
    fn live_a_second_threads_build_reuses_the_first() {
        use std::time::{Duration, Instant};
        let home = PathBuf::from(std::env::var("HOME").unwrap());
        let (dir, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let state = state_with(dir.path(), Arc::new(StubModel));
        let (farm, first, second) = crate::builds::project_with_two_threads(&state, work.path(), "farm");
        let (shop, other, _) = crate::builds::project_with_two_threads(&state, work.path(), "shop");
        let fixture = |at: &Path| {
            std::fs::create_dir_all(at.join("src")).unwrap();
            let manifest = "[package]\nname = \"cdep\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nlibsqlite3-sys = { version = \"=0.28.0\", features = [\"bundled\"] }\n\n[workspace]\n";
            std::fs::write(at.join("Cargo.toml"), manifest).unwrap();
            let main = "fn main() { println!(\"sqlite {}\", unsafe { libsqlite3_sys::sqlite3_libversion_number() }); }\n";
            std::fs::write(at.join("src/main.rs"), main).unwrap();
        };
        let build = |stream: &str, key: &str, label: &str| -> Duration {
            let mut granted = spawn_grants(MachineAccess { grants: working_path(&home), env: working_env(&home), ..Default::default() });
            crate::builds::extend(&state, stream, &mut granted);
            granted.env.push(("CARGO_BUILD_JOBS".to_string(), "4".to_string()));
            // The operator's crates, cloned in as the Mac sandbox seeds them.
            let registry = crate::builds::cargo_home(&state, key).join("registry");
            std::fs::create_dir_all(&registry).unwrap();
            for part in ["index", "cache", "src"] {
                if !registry.join(part).exists() {
                    let cloned = std::process::Command::new("/bin/cp")
                        .arg("-Rc")
                        .arg(home.join(".cargo/registry").join(part))
                        .arg(registry.join(part))
                        .status()
                        .unwrap();
                    assert!(cloned.success(), "the operator's crates cache");
                }
            }
            let ws = tempfile::tempdir_in(work.path()).unwrap();
            fixture(ws.path());
            let cache = tempfile::tempdir().unwrap();
            let env = bakery_exec::room_for_run(ws.path(), cache.path(), stream, "none").unwrap();
            let script = r#"cargo build --offline 2>&1 | tail -4; "$CARGO_TARGET_DIR"/debug/cdep"#;
            let argv = ["/bin/sh".to_string(), "-c".to_string(), script.to_string()];
            let run = bakery_exec::WorkspaceRun {
                argv: &argv,
                extra_deny_read: &granted.deny,
                timeout: Duration::from_secs(900),
                extra_read: &granted.read,
                extra_write: &granted.write,
                extra_exec: &granted.exec,
                whole_machine: None,
                path: &granted.path,
                env: &granted.env,
                append_only: &[],
            };
            let started = Instant::now();
            let out = bakery_exec::attach_exec_workspace_rw(&env, ws.path(), &run).unwrap();
            let took = started.elapsed();
            println!("{label}: {took:.1?}\n{}{}", out.stdout, out.stderr);
            assert!(out.stdout.contains("Finished") && out.stdout.contains("sqlite 3"), "{}{}", out.stdout, out.stderr);
            took
        };
        let cold = build(&first, &farm, "first thread, nothing built yet");
        let reused = build(&second, &farm, "second thread of the same project");
        let alone = build(&other, &shop, "a thread of another project (nothing shared)");
        println!("first {cold:.1?} · second {reused:.1?} · nothing shared {alone:.1?}");
        assert!(reused < alone, "the second thread builds faster than one with nothing shared");
    }

    /// The same command run by a native agent's tool in its room: what the
    /// fake agent reported.
    fn native_room(granted: &SpawnGrants, script: &str) -> String {
        let (ws, cache) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let env = bakery_exec::room_for_run(ws.path(), cache.path(), "wt:native-room", "none").unwrap();
        let staged = bakery_exec::stage_acp_binary(&fake_acp_agent_binary(), cache.path()).unwrap();
        let seat_home = bakery_exec::seat_acp_home(cache.path(), "wt:native-room", "orchestrator");
        let mut vars = granted.env.clone();
        vars.push(("BAKERY_FAKE_ACP_EXEC".to_string(), script.to_string()));
        let (exec, read) = (&granted.exec, &granted.read);
        let handle = bakery_exec::spawn_boxed_acp(
            &env, cache.path(), &staged, &seat_home, ws.path(), &granted.deny, &vars, &[], true, exec, &[], read, None, &[],
        )
        .unwrap();
        native_reply(handle)
    }

    /// A native room's fake agent, prompted once: what its command reported.
    fn native_reply(handle: bakery_exec::BoxedAcpHandle) -> String {
        use std::io::{BufRead, BufReader, Write};
        let (_, mut stdin, stdout, _stderr, kill) = handle.into_io_and_killer();
        for message in [
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1,"clientInfo":{"name":"t","version":"0"}}}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"session/new","params":{"cwd":"/","mcpServers":[]}}"#,
            r#"{"jsonrpc":"2.0","id":3,"method":"session/prompt","params":{"sessionId":"fake-session","prompt":[{"type":"text","text":"go"}]}}"#,
        ] {
            writeln!(stdin, "{message}").unwrap();
        }
        let mut said = String::new();
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if line.contains("\"id\":3") {
                break;
            }
            if line.contains("exec status=") {
                said = line;
            }
        }
        kill();
        said
    }

    /// A conversation with a new commit, whose origin is a local bare repo
    /// it reaches by file://, and the room grants it gets: the worktree's
    /// shared git folder to read and its own to write, as the run tool lays
    /// them.
    struct Pushing {
        _dir: tempfile::TempDir,
        state: AppState,
        ctx: crate::state::ActiveContext,
        origin: PathBuf,
        granted: SpawnGrants,
        read: Vec<PathBuf>,
        own: PathBuf,
    }

    fn pushing() -> Pushing {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        std::fs::write(state.base.join("remote-binary"), bakery_node_binary().display().to_string()).unwrap();
        let local = crate::git::tests::behind_its_origin(dir.path());
        let origin = dir.path().join("origin.git");
        run_git(&local, &["remote", "set-url", "origin", &format!("file://{}", origin.display())]);
        let ctx = crate::routes::project::open_stream(&state, &open_req(&local, "pushes")).unwrap().context;
        set_reach(&state, &ctx.stream_id, Reach::HostRoom);
        let wt = std::path::Path::new(&ctx.worktree_path);
        std::fs::write(wt.join("one.txt"), "x\n").unwrap();
        run_git(wt, &["add", "."]);
        run_git(wt, &["commit", "-qm", "one"]);
        let granted = for_stream(&state, &ctx.stream_id);
        let (shared, own) = crate::git::linked_worktree_git_dirs(wt).unwrap();
        let read = granted.read.iter().cloned().chain([shared]).collect();
        Pushing { _dir: dir, state, ctx, origin, granted, read, own }
    }

    impl Pushing {
        fn sent(&self) -> Option<String> {
            crate::git::run(&self.origin, &["rev-parse", "--verify", "-q", &self.ctx.branch]).ok().map(|s| s.trim().to_string())
        }

        fn head(&self) -> String {
            crate::git::run(std::path::Path::new(&self.ctx.worktree_path), &["rev-parse", "HEAD"]).unwrap().trim().to_string()
        }

        /// The card waits; approved, the same push goes out.
        fn waits_then_goes_out(&self, push: impl Fn() -> String) {
            let first = push();
            assert!(first.contains("waiting for the operator"), "{first}");
            assert_eq!(self.sent(), None, "nothing went out");
            let card = crate::remote::waiting(&self.state).items.into_iter().find(|w| w.stream_id == self.ctx.stream_id).expect("its card");
            assert_eq!(card.tool, "git_push");
            let approve = crate::pipe::ChildToolGateDecisionKind::AllowOnce;
            crate::pipe::decide_child_tool_gate(&self.state, &self.ctx.stream_id, &card.card, approve).unwrap();
            let second = push();
            assert_eq!(self.sent(), Some(self.head()), "approved, it went out: {second}");
        }
    }

    /// An agent's own `git push` in a command room goes to the operator: the
    /// room's git hands it to the Bakery's helper, which asks at the thread's
    /// door. A Mac-sandbox project's room reaches that door (its network is
    /// this Mac); a room with no network at all doesn't.
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn a_command_rooms_own_git_push_waits_for_the_operator() {
        let p = pushing();
        let wt = std::path::Path::new(&p.ctx.worktree_path);
        let room = bakery_exec::room_for_run(wt, &crate::state::cache_root(&p.state), &p.ctx.stream_id, "none").unwrap();
        let mut env = p.granted.env.clone();
        env.push((bakery_exec::NET_ENV.to_string(), "local".to_string()));
        let argv = ["/bin/sh".to_string(), "-c".to_string(), format!("git push origin {} 2>&1", p.ctx.branch)];
        p.waits_then_goes_out(|| {
            let run = bakery_exec::WorkspaceRun {
                argv: &argv,
                extra_deny_read: &p.granted.deny,
                // Past the helper's own wait for its first connection
                // (`doors::FIRST_CONNECTION_WAIT`) and its ask.
                timeout: crate::doors::FIRST_CONNECTION_WAIT + std::time::Duration::from_secs(180),
                extra_read: &p.read,
                extra_write: std::slice::from_ref(&p.own),
                extra_exec: &p.granted.exec,
                whole_machine: None,
                path: &p.granted.path,
                env: &env,
                append_only: &[],
            };
            bakery_exec::attach_exec_workspace_rw(&room, wt, &run).unwrap().stdout
        });
    }

    /// The same from a native agent's own shell, as Claude Code's Bash runs
    /// it in its room.
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn a_native_rooms_own_git_push_waits_for_the_operator() {
        let p = pushing();
        let wt = std::path::Path::new(&p.ctx.worktree_path);
        let cache = crate::state::cache_root(&p.state);
        let room = bakery_exec::room_for_run(wt, &cache, &p.ctx.stream_id, "none").unwrap();
        let staged = bakery_exec::stage_acp_binary(&fake_acp_agent_binary(), &cache).unwrap();
        let seat_home = bakery_exec::seat_acp_home(&cache, &p.ctx.stream_id, "orchestrator");
        let script = format!("cd '{}' && git push origin {} 2>&1 | grep -v -e xcrun -e DVTFilePath", wt.display(), p.ctx.branch);
        p.waits_then_goes_out(|| {
            let mut vars = p.granted.env.clone();
            vars.push(("BAKERY_FAKE_ACP_EXEC".to_string(), script.clone()));
            let handle = bakery_exec::spawn_boxed_acp(
                &room, &cache, &staged, &seat_home, wt, &p.granted.deny, &vars, &[], true, &p.granted.exec,
                std::slice::from_ref(&p.own), &p.read, None, &[],
            )
            .unwrap();
            native_reply(handle)
        });
    }

    /// rustup's cargo runs in every room with the operator's toolchains, each
    /// room downloading crates into its own CARGO_HOME; nothing else of
    /// ~/.cargo is reachable, never the crates.io token.
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn a_rooms_cargo_reaches_rustup_but_never_the_crates_io_token() {
        let home = tempfile::tempdir().unwrap();
        let h = home.path().canonicalize().unwrap();
        let plant = |rel: &str, text: &str| {
            let path = h.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, text).unwrap();
        };
        plant(".cargo/bin/cargo", "#!/bin/sh\n");
        plant(".rustup/toolchains/stable-aarch64-apple-darwin/bin/rustc", "");
        plant(".rustup/settings.toml", "default_toolchain = \"stable\"\n");
        plant(".rustup/update-hashes/stable-aarch64-apple-darwin", "");
        plant(".cargo/credentials.toml", "[registry]\ntoken = \"synthetic-not-a-real-token\"\n");
        plant(".cargo/config.toml", "[net]\n");
        let vars: Vec<String> = working_env(&h).into_iter().map(|v| format!("{}={}", v.name, v.value)).collect();
        assert!(vars.contains(&format!("RUSTUP_HOME={}", h.join(".rustup").display())), "{vars:?}");
        assert!(vars.contains(&format!("CARGO_HOME={}/.cargo", bakery_exec::ROOM_HOME_ENV_VALUE)), "{vars:?}");

        let granted = spawn_grants(MachineAccess { grants: working_path(&h), env: working_env(&h), ..Default::default() });
        let at = |rel: &str| h.join(rel).display().to_string();
        let script = [
            probe("bin", format!("ls '{}'", at(".cargo/bin"))),
            probe("toolchain", format!("ls '{}'", at(".rustup/toolchains/stable-aarch64-apple-darwin/bin"))),
            probe("settings", format!("cat '{}'", at(".rustup/settings.toml"))),
            probe("token", format!("cat '{}'", at(".cargo/credentials.toml"))),
            probe("config", format!("cat '{}'", at(".cargo/config.toml"))),
            probe("own", r#"test "$CARGO_HOME" = "$HOME/.cargo""#.to_string()),
        ]
        .concat();
        let want = "bin=yes toolchain=yes settings=yes token=no config=no own=yes";
        let command = command_room(&granted, &script);
        assert!(command.contains(want), "command room: {command}");
        let native = native_room(&granted, &script);
        assert!(native.contains(want), "native room: {native}");
    }
}
