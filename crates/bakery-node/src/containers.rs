//! The operator's container manager: containers, images, volumes and
//! networks on both of this Mac's backends (Apple container, and Docker in
//! Colima's default VM), plus compose stacks on Docker. Every call runs the
//! backend's own CLI and returns its JSON as the CLI prints it, so the UI and
//! the Baker see exactly what the backend reports.
//!
//! The Baker reaches these through the route catalog and its gate: listing
//! is read, starting, stopping and running run, and removing, pruning and
//! compose (which can mount any folder Colima shares) are destructive. An
//! agent reaches them only while the operator's containers switch is on for
//! its conversation ([`agent_may`]); its own sandbox blocks every Docker
//! socket and Apple container's CLI at every reach.

use crate::state::AppState;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

type HttpError = (StatusCode, String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Backend {
    /// Apple's `container`.
    Apple,
    /// Docker in Colima's default VM; never another Colima profile.
    Docker,
    /// Docker in OrbStack, by its own `orbstack` context, once the operator
    /// has installed it.
    Orbstack,
}

impl Backend {
    /// The CLI it speaks: OrbStack's is Docker's.
    pub(crate) fn dialect(self) -> Backend {
        match self {
            Backend::Orbstack => Backend::Docker,
            backend => backend,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Kind {
    Containers,
    Images,
    Volumes,
    Networks,
    /// Docker compose projects.
    Compose,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    List,
    Inspect,
    Start,
    Stop,
    Restart,
    Remove,
    Prune,
}

/// The CLI arguments for one operation, or why the backend has none.
fn args(backend: Backend, kind: Kind, op: Op, id: &str) -> Result<Vec<String>, String> {
    use {Backend::*, Kind::*, Op::*};
    let a: &[&str] = match (backend.dialect(), kind, op) {
        (Apple, Containers, List) => &["list", "--all", "--format", "json"],
        (Apple, Images, List) => &["image", "list", "--format", "json"],
        (Apple, Volumes, List) => &["volume", "list", "--format", "json"],
        (Apple, Networks, List) => &["network", "list", "--format", "json"],
        (Apple, Containers, Inspect) => &["inspect", id],
        (Apple, Images, Inspect) => &["image", "inspect", id],
        (Apple, Volumes, Inspect) => &["volume", "inspect", id],
        (Apple, Containers, Start) => &["start", id],
        (Apple, Containers, Stop) => &["stop", id],
        (Apple, Containers, Remove) => &["delete", id],
        (Apple, Images, Remove) => &["image", "delete", id],
        (Apple, Volumes, Remove) => &["volume", "delete", id],
        (Apple, Networks, Remove) => &["network", "delete", id],
        (Apple, Containers, Prune) => &["prune"],
        (Apple, Images, Prune) => &["image", "prune"],
        (Apple, Volumes, Prune) => &["volume", "prune"],
        (Apple, Networks, Prune) => &["network", "prune"],
        (Docker, Containers, List) => &["ps", "--all", "--no-trunc", "--format", "json"],
        (Docker, Images, List) => &["images", "--format", "json"],
        (Docker, Volumes, List) => &["volume", "ls", "--format", "json"],
        (Docker, Networks, List) => &["network", "ls", "--format", "json"],
        (Docker, Compose, List) => &["compose", "ls", "--all", "--format", "json"],
        (Docker, Containers, Inspect) => &["container", "inspect", id],
        (Docker, Images, Inspect) => &["image", "inspect", id],
        (Docker, Volumes, Inspect) => &["volume", "inspect", id],
        (Docker, Networks, Inspect) => &["network", "inspect", id],
        (Docker, Compose, Inspect) => &["compose", "-p", id, "ps", "--all", "--format", "json"],
        (Docker, Containers, Start) => &["start", id],
        (Docker, Containers, Stop) => &["stop", id],
        (Docker, Containers, Restart) => &["restart", id],
        (Docker, Compose, Restart) => &["compose", "-p", id, "restart"],
        (Docker, Containers, Remove) => &["rm", id],
        (Docker, Images, Remove) => &["rmi", id],
        (Docker, Volumes, Remove) => &["volume", "rm", id],
        (Docker, Networks, Remove) => &["network", "rm", id],
        (Docker, Compose, Remove) => &["compose", "-p", id, "down"],
        (Docker, Containers, Prune) => &["container", "prune", "--force"],
        (Docker, Images, Prune) => &["image", "prune", "--force"],
        (Docker, Volumes, Prune) => &["volume", "prune", "--force"],
        (Docker, Networks, Prune) => &["network", "prune", "--force"],
        _ => return Err(format!("{backend:?} has no {op:?} for {kind:?}").to_lowercase()),
    };
    Ok(a.iter().map(|s| s.to_string()).collect())
}

/// A name or id the CLI can't mistake for a flag.
fn checked(id: &str) -> Result<&str, HttpError> {
    let plain = !id.is_empty()
        && !id.starts_with('-')
        && id.len() <= 256
        && !id.chars().any(|c| c.is_whitespace() || c.is_control());
    plain.then_some(id).ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            format!("{id:?} is not a container, image, volume or network name"),
        )
    })
}

/// The backend's CLI, as the operator's shell finds it.
pub(crate) fn cli(backend: Backend) -> Result<Command, HttpError> {
    let name = match backend {
        Backend::Apple => "container",
        Backend::Docker | Backend::Orbstack => "docker",
    };
    let path = crate::state::resolve_executable_path(name).map_err(|_| {
        (
            StatusCode::CONFLICT,
            format!("{name} is not installed on this node's PATH"),
        )
    })?;
    let mut cmd = Command::new(path);
    match backend {
        Backend::Docker => {
            // Colima's default VM, pinned: other profiles may share the whole home.
            cmd.env("DOCKER_HOST", format!("unix://{}", colima_socket().display()));
        }
        // OrbStack's own context, whatever the terminal's current one is.
        Backend::Orbstack => {
            cmd.env_remove("DOCKER_HOST").env("DOCKER_CONTEXT", "orbstack");
        }
        Backend::Apple => {}
    }
    cmd.stdin(Stdio::null());
    Ok(cmd)
}

/// The socket of Colima's default VM, where the Docker backend is pinned.
fn colima_socket() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    Path::new(&home).join(".colima/default/docker.sock")
}

/// Runs the CLI: its stdout and stderr on success, its stderr as the error.
fn run(base: &Path, backend: Backend, args: &[String]) -> Result<(String, String), HttpError> {
    run_env(base, backend, args, &BTreeMap::new())
}

/// [`run`], with variables set for the CLI alone (a run passes them on by
/// name, so their values never show in the process list). A Docker request
/// starts Colima's default VM again first if the idle stop stopped it.
fn run_env(
    base: &Path,
    backend: Backend,
    args: &[String],
    env: &BTreeMap<String, String>,
) -> Result<(String, String), HttpError> {
    if backend == Backend::Docker {
        crate::engine::before_docker(base)?;
    }
    let out = cli(backend)?
        .args(args)
        .envs(env)
        .output()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    if !out.status.success() {
        if backend == Backend::Docker && !colima_socket().exists() {
            return Err((
                StatusCode::CONFLICT,
                "Colima isn't running: it was stopped outside the Bakery, which starts it again only after its own idle stop. Start Colima to use its Docker.".to_string(),
            ));
        }
        if backend == Backend::Orbstack && !crate::engine::orbstack_answers() {
            let why = if crate::engine::orbstack_installed() {
                "OrbStack is stopped: running a container or a compose project, or starting one, starts it again."
            } else {
                "OrbStack isn't installed."
            };
            return Err((StatusCode::CONFLICT, why.to_string()));
        }
        return Err((StatusCode::BAD_GATEWAY, stderr.trim().to_string()));
    }
    Ok((String::from_utf8_lossy(&out.stdout).into_owned(), stderr))
}

/// Before an action that needs its engine (a run, a start, a compose
/// project brought up): OrbStack started if it's stopped. A read (a list,
/// an inspect, logs, stats) never starts it, so an OrbStack the operator
/// quit stays quit until they ask for something to run.
fn wake(backend: Backend) -> Result<(), HttpError> {
    if backend == Backend::Orbstack {
        crate::engine::before_orbstack()?;
    }
    Ok(())
}

/// An action of the node's own (a project's service brought up): its
/// engine woken first, `env` passed to the CLI alone. Its stdout, or why
/// it failed.
pub(crate) fn run_action(base: &Path, backend: Backend, args: &[String], env: &BTreeMap<String, String>) -> Result<String, String> {
    wake(backend).map_err(|(_, e)| e)?;
    run_env(base, backend, args, env).map(|(out, _)| out).map_err(|(_, e)| e)
}

/// The CLI as the engine stands: never started, woken or restarted, for a
/// stop or a sweep that has nothing to do on an engine that's down.
pub(crate) fn quietly(backend: Backend, args: &[&str]) -> Option<String> {
    let up = match backend {
        Backend::Apple => true,
        Backend::Docker => colima_socket().exists(),
        Backend::Orbstack => crate::engine::orbstack_answers(),
    };
    let out = cli(backend).ok().filter(|_| up)?.args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The containers on `backend` whose labels `pick` takes, by name, on an
/// engine that is up.
pub(crate) fn labelled(backend: Backend, pick: impl Fn(&BTreeMap<String, String>) -> bool) -> Vec<String> {
    let Ok(list) = args(backend, Kind::Containers, Op::List, "") else {
        return Vec::new();
    };
    let list: Vec<&str> = list.iter().map(String::as_str).collect();
    let Some(out) = quietly(backend, &list) else {
        return Vec::new();
    };
    listing(backend, Kind::Containers, &parse_json(&out))
        .containers
        .into_iter()
        .filter(|c| pick(&c.labels))
        .map(|c| c.name)
        .collect()
}

/// One container, volume or network on an engine, by name, with its labels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Thing {
    pub(crate) backend: Backend,
    pub(crate) kind: Kind,
    pub(crate) name: String,
    pub(crate) labels: BTreeMap<String, String>,
}

/// Every container, volume and network on `backend`, as its engine stands
/// (none is started); `None` when it doesn't answer.
pub(crate) fn things(backend: Backend) -> Option<Vec<Thing>> {
    let list = |kind: Kind| -> Option<Listing> {
        let args = args(backend, kind, Op::List, "").ok()?;
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        Some(listing(backend, kind, &parse_json(&quietly(backend, &args)?)))
    };
    let thing = |kind, name, labels| Thing { backend, kind, name, labels };
    let mut found: Vec<Thing> = list(Kind::Containers)?
        .containers
        .into_iter()
        .map(|c| thing(Kind::Containers, c.name, c.labels))
        .collect();
    let volumes = list(Kind::Volumes).into_iter().flat_map(|l| l.volumes);
    found.extend(volumes.map(|v| thing(Kind::Volumes, v.name, v.labels)));
    let networks = list(Kind::Networks).into_iter().flat_map(|l| l.networks);
    found.extend(networks.map(|n| thing(Kind::Networks, n.name, n.labels)));
    Some(found)
}

/// Whether `backend`'s engine is set up on this Mac, running or not.
pub(crate) fn set_up(backend: Backend) -> bool {
    match backend {
        Backend::Apple => cli(Backend::Apple).is_ok(),
        Backend::Docker => colima_socket().parent().is_some_and(Path::is_dir),
        Backend::Orbstack => crate::engine::orbstack_installed(),
    }
}

/// Removes one container (stopped first), volume or network, on an engine
/// that is up. One no longer on its engine is removed already: a `--rm`
/// container goes once it stops, and a thread's discard stops its seats,
/// shells, Code editor and service copies.
pub(crate) fn remove_thing(base: &Path, thing: &Thing) -> Result<(), String> {
    let name = checked(&thing.name).map_err(|(_, e)| e)?;
    if thing.kind == Kind::Containers {
        // One already stopped says so; the removal is what answers.
        let _ = run(base, thing.backend, &args(thing.backend, thing.kind, Op::Stop, name)?);
    }
    let remove = args(thing.backend, thing.kind, Op::Remove, name)?;
    let Err((_, e)) = run(base, thing.backend, &remove) else {
        return Ok(());
    };
    let there = |left: Vec<Thing>| left.iter().any(|t| t.kind == thing.kind && t.name == thing.name);
    match things(thing.backend).map(there) {
        Some(false) => Ok(()),
        _ => Err(e),
    }
}

/// The CLI's JSON: one document, or (Docker) one document per line.
pub(crate) fn parse_json(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|_| {
        Value::Array(
            text.lines()
                .filter_map(|line| serde_json::from_str(line).ok())
                .collect(),
        )
    })
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, HttpError> + Send + 'static,
) -> Result<T, HttpError> {
    crate::actor::spawn_blocking(f)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
}

fn bad(e: String) -> HttpError {
    (StatusCode::BAD_REQUEST, e)
}

#[derive(Deserialize, schemars::JsonSchema)]
pub(crate) struct ListQuery {
    backend: Backend,
    kind: Kind,
}

/// `GET /containers`: everything of one kind on one backend, as rows both
/// backends share.
pub(crate) async fn list(State(st): State<AppState>, Query(q): Query<ListQuery>) -> Result<Json<Listing>, HttpError> {
    blocking(move || {
        let args = args(q.backend, q.kind, Op::List, "").map_err(bad)?;
        let (out, _) = run(&st.base, q.backend, &args)?;
        Ok(Json(listing(q.backend, q.kind, &parse_json(&out))))
    })
    .await
}

// ─── Rows ───────────────────────────────────────────────────────────────────
//
// Both backends' JSON, normalised once, for the Containers screen and the
// Baker alike.

/// A port a container publishes on this Mac, or only exposes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub(crate) struct Port {
    /// On this Mac; `None` when the port is only exposed.
    host: Option<u16>,
    container: u16,
    protocol: String,
}

/// What Bakery started a container as, from its labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Role {
    /// An agent's seat.
    Seat,
    /// A conversation's editor.
    Editor,
    /// One started with `containers_run`.
    Run,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct ContainerRow {
    id: String,
    name: String,
    image: String,
    running: bool,
    /// The backend's own words, e.g. "Up 4 hours (healthy)" or "running".
    status: String,
    ports: Vec<Port>,
    /// Its address on its network, where the backend lists it (Apple
    /// container).
    address: Option<String>,
    labels: BTreeMap<String, String>,
    /// Set when Bakery started it.
    role: Option<Role>,
    /// The conversation it belongs to (a seat's or an editor's).
    stream_id: Option<String>,
    /// The agent's seat in that conversation.
    seat: Option<String>,
    /// Who asked for a run: "human", or an agent ("agent:<seat>").
    started_by: Option<String>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct ImageRow {
    id: String,
    /// `repository:tag`, or the id when it has none.
    name: String,
    /// e.g. "438MB" (Docker's words) or "824 MB".
    size: String,
    /// e.g. "2 weeks ago" or "2026-09-30".
    created: String,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct VolumeRow {
    name: String,
    driver: String,
    /// Docker's own words; on Apple container, its disk image's capacity,
    /// not what is used.
    size: String,
    labels: BTreeMap<String, String>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct NetworkRow {
    id: String,
    name: String,
    /// e.g. "bridge", or Apple container's "nat" and "hostOnly".
    driver: String,
    /// No route out of this Mac (Docker's internal, Apple's host-only).
    internal: bool,
    subnet: Option<String>,
    gateway: Option<String>,
    labels: BTreeMap<String, String>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct ComposeRow {
    name: String,
    /// e.g. "running(3)".
    status: String,
    files: Vec<String>,
}

/// One kind's rows on one backend; the other kinds' lists are empty.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct Listing {
    backend: Backend,
    kind: Kind,
    containers: Vec<ContainerRow>,
    images: Vec<ImageRow>,
    volumes: Vec<VolumeRow>,
    networks: Vec<NetworkRow>,
    compose: Vec<ComposeRow>,
}

fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        _ => String::new(),
    }
}

fn some(s: String) -> Option<String> {
    (!s.is_empty()).then_some(s)
}

/// Docker prints labels as "k=v,k2=v2"; Apple container as an object.
fn labels_of(v: &Value) -> BTreeMap<String, String> {
    match v {
        Value::String(s) => s
            .split(',')
            .filter_map(|pair| pair.split_once('='))
            .filter(|(k, _)| !k.is_empty())
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        Value::Object(m) => m.iter().map(|(k, v)| (k.clone(), text(v))).collect(),
        _ => BTreeMap::new(),
    }
}

/// Apple container keeps most of an item under `configuration`.
fn config(item: &Value) -> &Value {
    item.get("configuration")
        .filter(|c| c.is_object())
        .unwrap_or(item)
}

/// A byte count in decimal units: "1.5 GB".
fn size_text(n: u64) -> String {
    let units = ["B", "KB", "MB", "GB", "TB"];
    let (mut value, mut unit) = (n as f64, 0);
    while value >= 1000.0 && unit < units.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if value >= 100.0 || unit == 0 {
        format!("{} {}", value.round(), units[unit])
    } else {
        format!("{value:.1} {}", units[unit])
    }
}

/// "0.0.0.0:55322->5432/tcp, [::]:55322->5432/tcp, 8080/tcp".
/// ponytail: a published range ("8000-8002->...") is skipped; parse ranges when one turns up.
fn docker_ports(text: &str) -> Vec<Port> {
    let mut out: Vec<Port> = Vec::new();
    for part in text.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let (host, inside) = match part.rsplit_once("->") {
            Some((left, right)) => (
                left.rsplit_once(':').and_then(|(_, p)| p.parse().ok()),
                right,
            ),
            None => (None, part),
        };
        let (port, protocol) = inside.split_once('/').unwrap_or((inside, "tcp"));
        let Ok(container) = port.parse() else {
            continue;
        };
        let port = Port {
            host,
            container,
            protocol: protocol.to_string(),
        };
        if !out.contains(&port) {
            out.push(port);
        }
    }
    out
}

/// Apple container's `publishedPorts`, each a run of `count` ports.
fn apple_ports(v: &Value) -> Vec<Port> {
    let mut out = Vec::new();
    for p in v.as_array().into_iter().flatten() {
        let (Some(host), Some(inside)) = (p["hostPort"].as_u64(), p["containerPort"].as_u64())
        else {
            continue;
        };
        let protocol = p["proto"].as_str().unwrap_or("tcp").to_string();
        for i in 0..p["count"].as_u64().unwrap_or(1).max(1) {
            let (Ok(host), Ok(container)) = (u16::try_from(host + i), u16::try_from(inside + i))
            else {
                break;
            };
            out.push(Port {
                host: Some(host),
                container,
                protocol: protocol.clone(),
            });
        }
    }
    out
}

fn container_row(backend: Backend, item: &Value) -> ContainerRow {
    let (id, name, image, state, status, ports, address, labels) = match backend {
        Backend::Docker | Backend::Orbstack => {
            let id = text(&item["ID"]);
            let names = text(&item["Names"]);
            let name = names.split(',').next().unwrap_or_default().to_string();
            let state = text(&item["State"]);
            let status = some(text(&item["Status"])).unwrap_or_else(|| state.clone());
            let name = some(name).unwrap_or_else(|| id.chars().take(12).collect());
            (
                id,
                name,
                text(&item["Image"]),
                state,
                status,
                docker_ports(&text(&item["Ports"])),
                None,
                labels_of(&item["Labels"]),
            )
        }
        Backend::Apple => {
            let c = config(item);
            let id = some(text(&item["id"])).unwrap_or_else(|| text(&c["id"]));
            // Older releases print the state as a bare string.
            let state = match &item["status"] {
                Value::String(s) => s.clone(),
                other => text(&other["state"]),
            };
            (
                id.clone(),
                id,
                text(&c["image"]["reference"]),
                state.clone(),
                state,
                apple_ports(&c["publishedPorts"]),
                bakery_exec::first_ipv4(&item["status"]).map(|a| a.to_string()),
                labels_of(&c["labels"]),
            )
        }
    };
    let role = match labels.get("com.bakery").map(String::as_str) {
        Some("seat") => Some(Role::Seat),
        Some("code") => Some(Role::Editor),
        _ => labels.contains_key("com.bakery.run").then_some(Role::Run),
    };
    ContainerRow {
        running: state == "running",
        stream_id: labels.get("com.bakery.stream").cloned(),
        seat: labels.get("com.bakery.seat").cloned(),
        started_by: labels.get("com.bakery.run").cloned(),
        role,
        id,
        name,
        image,
        status,
        ports,
        address,
        labels,
    }
}

fn image_row(backend: Backend, item: &Value) -> ImageRow {
    match backend {
        Backend::Docker | Backend::Orbstack => {
            let id = text(&item["ID"]);
            let (repo, tag) = (text(&item["Repository"]), text(&item["Tag"]));
            let name = match (repo.as_str(), tag.as_str()) {
                ("" | "<none>", _) => id.clone(),
                (repo, "" | "<none>") => repo.to_string(),
                (repo, tag) => format!("{repo}:{tag}"),
            };
            ImageRow {
                name,
                size: text(&item["Size"]),
                created: some(text(&item["CreatedSince"]))
                    .unwrap_or_else(|| text(&item["CreatedAt"])),
                id,
            }
        }
        Backend::Apple => {
            let c = config(item);
            let name = some(text(&c["name"])).unwrap_or_else(|| text(&item["id"]));
            // This Mac's own variant of a multi-platform image, else the first.
            let arch = if cfg!(target_arch = "aarch64") {
                "arm64"
            } else {
                "amd64"
            };
            let variants = item["variants"].as_array().cloned().unwrap_or_default();
            let size = variants
                .iter()
                .find(|v| v["platform"]["architecture"] == arch)
                .or(variants.first())
                .and_then(|v| v["size"].as_u64())
                .map(size_text)
                .unwrap_or_default();
            ImageRow {
                id: some(text(&item["id"])).unwrap_or_else(|| name.clone()),
                name,
                size,
                created: text(&c["creationDate"]).chars().take(10).collect(),
            }
        }
    }
}

fn volume_row(backend: Backend, item: &Value) -> VolumeRow {
    match backend {
        Backend::Docker | Backend::Orbstack => VolumeRow {
            name: text(&item["Name"]),
            driver: text(&item["Driver"]),
            size: text(&item["Size"]),
            labels: labels_of(&item["Labels"]),
        },
        Backend::Apple => {
            let c = config(item);
            VolumeRow {
                name: some(text(&c["name"])).unwrap_or_else(|| text(&item["id"])),
                driver: text(&c["driver"]),
                size: c["sizeInBytes"].as_u64().map(size_text).unwrap_or_default(),
                labels: labels_of(&c["labels"]),
            }
        }
    }
}

fn network_row(backend: Backend, item: &Value) -> NetworkRow {
    match backend {
        Backend::Docker | Backend::Orbstack => NetworkRow {
            id: text(&item["ID"]),
            name: text(&item["Name"]),
            driver: text(&item["Driver"]),
            internal: matches!(&item["Internal"], Value::Bool(true))
                || text(&item["Internal"]) == "true",
            subnet: None,
            gateway: None,
            labels: labels_of(&item["Labels"]),
        },
        Backend::Apple => {
            let c = config(item);
            let mode = text(&c["mode"]);
            NetworkRow {
                id: text(&item["id"]),
                name: some(text(&c["name"])).unwrap_or_else(|| text(&item["id"])),
                internal: mode == "hostOnly",
                driver: mode,
                subnet: some(text(&item["status"]["ipv4Subnet"])),
                gateway: some(text(&item["status"]["ipv4Gateway"])),
                labels: labels_of(&c["labels"]),
            }
        }
    }
}

fn compose_row(item: &Value) -> ComposeRow {
    ComposeRow {
        name: text(&item["Name"]),
        status: text(&item["Status"]),
        files: text(&item["ConfigFiles"])
            .split(',')
            .map(str::trim)
            .filter(|f| !f.is_empty())
            .map(String::from)
            .collect(),
    }
}

/// One kind's rows from a backend's list JSON.
fn listing(backend: Backend, kind: Kind, items: &Value) -> Listing {
    let items: &[Value] = items.as_array().map(Vec::as_slice).unwrap_or_default();
    let mut out = Listing {
        backend,
        kind,
        containers: Vec::new(),
        images: Vec::new(),
        volumes: Vec::new(),
        networks: Vec::new(),
        compose: Vec::new(),
    };
    match kind {
        Kind::Containers => {
            out.containers = items.iter().map(|i| container_row(backend, i)).collect()
        }
        Kind::Images => out.images = items.iter().map(|i| image_row(backend, i)).collect(),
        Kind::Volumes => out.volumes = items.iter().map(|i| volume_row(backend, i)).collect(),
        Kind::Networks => out.networks = items.iter().map(|i| network_row(backend, i)).collect(),
        Kind::Compose => out.compose = items.iter().map(compose_row).collect(),
    }
    out
}

#[derive(Deserialize, schemars::JsonSchema)]
pub(crate) struct ItemQuery {
    backend: Backend,
    kind: Kind,
    /// Name or id.
    id: String,
}

/// `GET /containers/inspect`: one item, as the backend describes it.
pub(crate) async fn inspect(State(st): State<AppState>, Query(q): Query<ItemQuery>) -> Result<Json<Value>, HttpError> {
    blocking(move || {
        let args = args(q.backend, q.kind, Op::Inspect, checked(&q.id)?).map_err(bad)?;
        Ok(Json(for_agent_output(parse_json(
            &run(&st.base, q.backend, &args)?.0,
        ))))
    })
    .await
}

#[derive(Deserialize, schemars::JsonSchema)]
pub(crate) struct LogsQuery {
    backend: Backend,
    /// Container name or id.
    id: String,
    /// Last lines to show (default 200, at most 5000).
    #[serde(default)]
    tail: Option<u32>,
}

/// `GET /containers/logs`: a container's last lines, stdout and stderr.
pub(crate) async fn logs(State(st): State<AppState>, Query(q): Query<LogsQuery>) -> Result<Json<Value>, HttpError> {
    blocking(move || {
        let id = checked(&q.id)?;
        let tail = q.tail.unwrap_or(200).min(5000).to_string();
        let flag = match q.backend {
            Backend::Apple => "-n",
            Backend::Docker | Backend::Orbstack => "--tail",
        };
        let args = ["logs", flag, &tail, id].map(String::from);
        // Docker replays the container's stderr on its own stderr.
        let (out, err) = run(&st.base, q.backend, &args)?;
        Ok(Json(json!({ "text": out + &err })))
    })
    .await
}

#[derive(Deserialize, schemars::JsonSchema)]
pub(crate) struct BackendQuery {
    backend: Backend,
}

/// `GET /containers/stats`: CPU, memory and I/O of every running container.
pub(crate) async fn stats(State(st): State<AppState>, Query(q): Query<BackendQuery>) -> Result<Json<Value>, HttpError> {
    blocking(move || {
        let args = ["stats", "--no-stream", "--format", "json"].map(String::from);
        Ok(Json(parse_json(&run(&st.base, q.backend, &args)?.0)))
    })
    .await
}

#[derive(Deserialize, schemars::JsonSchema)]
pub(crate) struct ContainerReq {
    backend: Backend,
    /// Container name or id.
    id: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub(crate) struct ItemReq {
    backend: Backend,
    kind: Kind,
    /// Name or id (a project's name for `compose`).
    id: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub(crate) struct KindReq {
    backend: Backend,
    kind: Kind,
}

fn act(base: &Path, backend: Backend, kind: Kind, op: Op, id: &str) -> Result<Json<Value>, HttpError> {
    let (out, _) = run(base, backend, &args(backend, kind, op, id).map_err(bad)?)?;
    Ok(Json(json!({ "ok": true, "output": out.trim() })))
}

/// `POST /containers/start`.
pub(crate) async fn start(
    State(st): State<AppState>,
    Json(r): Json<ContainerReq>,
) -> Result<Json<Value>, HttpError> {
    blocking(move || {
        wake(r.backend)?;
        act(&st.base, r.backend, Kind::Containers, Op::Start, checked(&r.id)?)
    })
    .await
}

/// `POST /containers/stop`.
pub(crate) async fn stop(State(st): State<AppState>, Json(r): Json<ContainerReq>) -> Result<Json<Value>, HttpError> {
    blocking(move || act(&st.base, r.backend, Kind::Containers, Op::Stop, checked(&r.id)?)).await
}

/// `POST /containers/restart`: a container, or a whole compose project,
/// stops and starts again with its files kept (Apple container has no
/// restart of its own: a stop, then a start). The Bakery's own are refused
/// while in use; they run with `--rm`, so they start fresh instead.
pub(crate) async fn restart(
    State(st): State<AppState>,
    Json(r): Json<ItemReq>,
) -> Result<Json<Value>, HttpError> {
    blocking(move || {
        let id = checked(&r.id)?;
        wake(r.backend)?;
        if r.kind == Kind::Containers {
            if let Some(done) = restart_bakerys_own(&st, r.backend, id)? {
                return Ok(done);
            }
            if r.backend == Backend::Apple {
                act(&st.base, r.backend, r.kind, Op::Stop, id).map(drop)?;
                return act(&st.base, r.backend, r.kind, Op::Start, id);
            }
        }
        act(&st.base, r.backend, r.kind, Op::Restart, id)
    })
    .await
}

/// One of the Bakery's own containers, known by its labels: refused while in
/// use, else stopped to start fresh (an agent's sandbox with its next turn,
/// the editor the next time Code opens). `None` for anyone else's.
fn restart_bakerys_own(
    state: &AppState,
    backend: Backend,
    id: &str,
) -> Result<Option<Json<Value>>, HttpError> {
    let inspect = args(backend, Kind::Containers, Op::Inspect, id).map_err(bad)?;
    let found = parse_json(&run(&state.base, backend, &inspect)?.0);
    let item = found.get(0).unwrap_or(&found);
    let labels = match backend {
        Backend::Docker | Backend::Orbstack => labels_of(&item["Config"]["Labels"]),
        Backend::Apple => labels_of(&config(item)["labels"]),
    };
    restart_labelled(state, &labels, id)
}

fn restart_labelled(
    state: &AppState,
    labels: &BTreeMap<String, String>,
    id: &str,
) -> Result<Option<Json<Value>>, HttpError> {
    let Some(stream) = labels.get("com.bakery.stream") else {
        return Ok(None);
    };
    let fresh = |output: &str| Ok(Some(Json(json!({ "ok": true, "output": output }))));
    match labels.get("com.bakery").map(String::as_str) {
        Some("seat") => {
            crate::agents::restart_runtime_inner(state, stream)
                .map_err(|why| (StatusCode::CONFLICT, format!("an agent works in it: {why}")))?;
            // A sandbox left from before the node started has no runtime here.
            crate::seat_container::stop(id);
            fresh("its agent starts in a fresh sandbox with its next turn")
        }
        Some("code") if crate::views::is_open(stream, "code") => Err((
            StatusCode::CONFLICT,
            "the Code panel shows it: close the panel first".to_string(),
        )),
        Some("code") => {
            crate::seat_container::stop(id);
            fresh("it starts fresh the next time Code opens")
        }
        _ => Ok(None),
    }
}

/// `POST /containers/remove`: one stopped container, image, volume or
/// network, or a whole compose project (`down`).
pub(crate) async fn remove(State(st): State<AppState>, Json(r): Json<ItemReq>) -> Result<Json<Value>, HttpError> {
    blocking(move || act(&st.base, r.backend, r.kind, Op::Remove, checked(&r.id)?)).await
}

/// `POST /containers/prune`: every unused item of one kind.
pub(crate) async fn prune(State(st): State<AppState>, Json(r): Json<KindReq>) -> Result<Json<Value>, HttpError> {
    blocking(move || act(&st.base, r.backend, r.kind, Op::Prune, "")).await
}

#[derive(Deserialize, schemars::JsonSchema)]
pub(crate) struct ComposeUpReq {
    /// The compose file to bring up.
    file: String,
    /// docker (Colima's default VM, the default) or orbstack.
    #[serde(default)]
    backend: Option<Backend>,
}

/// `POST /containers/compose/up`: start a compose project, detached, as
/// one of its project's stacks (the project whose folder holds the file):
/// named for the file, `bakery-<project>-<its path in the folder>`, so two
/// files are two stacks, each joined to the project's own network, and its
/// ports on this Mac moved into the project's part of the fenced block,
/// which only its rooms reach. A file in no project keeps its own name, its
/// ports in the block too, reached by no room. The changes are an override
/// kept in the node's own state: the project's files stay as they are.
pub(crate) async fn compose_up(
    State(st): State<AppState>,
    Json(r): Json<ComposeUpReq>,
) -> Result<Json<Value>, HttpError> {
    blocking(move || {
        let file = std::path::Path::new(&r.file);
        if !file.is_absolute() || !file.is_file() {
            return Err(bad(format!("{}: not a compose file", r.file)));
        }
        let backend = r.backend.unwrap_or(Backend::Docker);
        if backend == Backend::Apple {
            return Err(bad("compose runs on Docker: Colima's or OrbStack's".to_string()));
        }
        let project = compose_stack(&st, file);
        let mut stack = vec!["compose".to_string()];
        if let Some((_, name)) = &project {
            stack.extend(["-p".to_string(), name.clone()]);
        }
        stack.extend(["-f".to_string(), r.file.clone()]);
        wake(backend)?;
        let config = [&stack[..], &["config", "--format", "json"].map(String::from)].concat();
        let config = parse_json(&run(&st.base, backend, &config)?.0);
        let name = config["name"].as_str().unwrap_or_default();
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            return Err(bad(format!("{}: compose gave it no plain project name", r.file)));
        }
        let network = match &project {
            Some((project, _)) => Some(project_network(&st.base, backend, project)?),
            None => None,
        };
        let scope = project.map_or_else(|| "bakery".to_string(), |(project, _)| project);
        let (text, published) = compose_override(&config, network.as_deref(), |what| {
            crate::services::published_port(&st, &scope, &format!("compose/{name}/{what}"))
        })
        .map_err(bad)?;
        let over = st.base.join("compose").join(format!("{name}.yml"));
        std::fs::create_dir_all(st.base.join("compose"))
            .and_then(|()| std::fs::write(&over, text))
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{}: {e}", over.display())))?;
        let up = ["-f".to_string(), over.display().to_string(), "up".into(), "--detach".into()];
        let (out, err) = run(&st.base, backend, &[&stack[..], &up].concat())?;
        // Compose reports progress on stderr.
        Ok(Json(json!({ "ok": true, "output": (out + &err).trim(), "published": published })))
    })
    .await
}

/// The project whose folder holds a compose file, and the file's stack in
/// it: `bakery-<project>-<the file's path in the folder>`.
fn compose_stack(state: &AppState, file: &Path) -> Option<(String, String)> {
    let file = file.canonicalize().ok()?;
    crate::catalog::known_projects(state).ok()?.into_iter().find_map(|p| {
        let path = p.roots.iter().find_map(|r| file.strip_prefix(&r.path).ok())?;
        let name = compose_name(&format!("{}-{}", p.project_id, path.with_extension("").display()));
        Some((p.project_id, name))
    })
}

/// `bakery-<what>`, in the letters compose and Docker take.
pub(crate) fn compose_name(what: &str) -> String {
    let plain: String = what
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '-' })
        .collect();
    format!("bakery-{plain}")
}

/// The project's own Docker network, `bakery-<project>`, which each of its
/// stacks and services joins: made the first time one comes up.
pub(crate) fn project_network(base: &Path, backend: Backend, project_id: &str) -> Result<String, HttpError> {
    crate::engine::note_project_use(base, project_id, backend);
    let name = compose_name(project_id);
    let inspect = ["network", "inspect", &name].map(String::from);
    if run(base, backend, &inspect).is_err() {
        let create = ["network", "create", "--label", "com.bakery=project", "--label"]
            .map(String::from)
            .into_iter()
            .chain([format!("com.bakery.project={project_id}"), name.clone()])
            .collect::<Vec<_>>();
        // Another of its stacks may have made it just now.
        if let Err(e) = run(base, backend, &create) {
            run(base, backend, &inspect).map_err(|_| e)?;
        }
    }
    Ok(name)
}

/// The override for a compose project (`config`: compose's own resolved
/// JSON). Its ports move into the fenced block: each service's ports
/// replaced by its container ports, published on 127.0.0.1 at
/// `port("<service>/<port>/<protocol>")`; a service on the host's network
/// publishes nothing. With its project's `network`, each service joins that
/// too, after its own: a name resolves on the service's own networks first
/// (Docker's resolver goes by gateway priority), so a `db` in this stack is
/// never another stack's `db`, and other stacks reach it by its container
/// name. Also what it published, for the operator.
fn compose_override(
    config: &Value,
    network: Option<&str>,
    mut port: impl FnMut(&str) -> Result<u16, String>,
) -> Result<(String, Vec<Value>), String> {
    // A JSON string is a YAML one too, whatever the name.
    let quoted = |name: &str| serde_json::to_string(name).unwrap_or_default();
    let mut text = String::new();
    let mut published = Vec::new();
    for (service, spec) in config["services"].as_object().into_iter().flatten() {
        let mut block = String::new();
        if let (Some(_), None) = (network, spec["network_mode"].as_str()) {
            let own: Vec<&String> = spec["networks"].as_object().map(|n| n.keys().collect()).unwrap_or_default();
            block.push_str("    networks:\n");
            for key in own.iter().map(|k| k.as_str()).chain(own.is_empty().then_some("default")) {
                block.push_str(&format!("      {}: {{}}\n", quoted(key)));
            }
            block.push_str("      \"bakery-project\": {\"gw_priority\": -1}\n");
        }
        let mut seen = std::collections::BTreeSet::new();
        let mut ports = String::new();
        let host_network = spec["network_mode"] == "host";
        for p in spec["ports"].as_array().into_iter().flatten().filter(|_| !host_network) {
            let Some(target) = p["target"].as_u64() else {
                continue;
            };
            let protocol = match p["protocol"].as_str() {
                Some("udp") => "udp",
                Some("sctp") => "sctp",
                _ => "tcp",
            };
            if !seen.insert((target, protocol)) {
                continue;
            }
            let host = port(&format!("{service}/{target}/{protocol}"))?;
            ports.push_str(&format!("      - \"127.0.0.1:{host}:{target}/{protocol}\"\n"));
            published.push(json!({ "service": service, "host": host, "container": target, "protocol": protocol }));
        }
        if !ports.is_empty() {
            block.push_str(&format!("    ports: !override\n{ports}"));
        }
        if !block.is_empty() {
            text.push_str(&format!("  {}:\n{block}", quoted(service)));
        }
    }
    let mut over = match network {
        Some(network) => format!("networks:\n  \"bakery-project\":\n    name: {}\n    external: true\n", quoted(network)),
        None => String::new(),
    };
    if text.is_empty() {
        over.push_str("services: {}\n");
    } else {
        over.push_str(&format!("services:\n{text}"));
    }
    Ok((over, published))
}

#[derive(Deserialize, schemars::JsonSchema)]
pub(crate) struct MountReq {
    /// A host folder, absolute (a file too, on Docker).
    source: String,
    /// Where the container sees it, absolute.
    target: String,
    /// Read and write; read-only otherwise.
    #[serde(default)]
    write: bool,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub(crate) struct PortReq {
    /// The port on this Mac, published on 127.0.0.1 only, inside the
    /// Bakery's block (46000–46499). Leave it out: the Bakery picks the
    /// project's own, the same each time, which only that project's rooms
    /// reach.
    #[serde(default)]
    host: Option<u16>,
    /// The container's port.
    container: u16,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub(crate) struct RunReq {
    backend: Backend,
    /// The image, e.g. `docker.io/library/postgres:17`.
    image: String,
    /// The container's name; the backend picks one when absent.
    #[serde(default)]
    name: Option<String>,
    /// The command and its arguments, in place of the image's own.
    #[serde(default)]
    command: Vec<String>,
    #[serde(default)]
    mounts: Vec<MountReq>,
    #[serde(default)]
    ports: Vec<PortReq>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    /// The project it's for, when the operator runs it (an agent's run is
    /// its own project's): its ports are that project's.
    #[serde(default)]
    project_id: Option<String>,
}

/// The project a run's ports are kept for: an agent's own thread's, or the
/// one the operator named; else `bakery`, which no room reaches.
fn run_scope(state: &AppState, named: Option<&str>) -> String {
    let caller = crate::actor::Caller::of(state);
    let project = match caller.thread {
        Some(_) => caller.project,
        None => named.map(str::to_string),
    };
    project.unwrap_or_else(|| "bakery".to_string())
}

/// `POST /containers/run`: start a container from an image, detached, its
/// ports on 127.0.0.1 only. For an agent, what is always blocked and any
/// credential file inside a mount stay hidden from the container.
pub(crate) async fn run_container(
    State(st): State<AppState>,
    Json(r): Json<RunReq>,
) -> Result<Json<Ran>, HttpError> {
    blocking(move || {
        let mut r = r;
        let guard = for_agent().then(|| Guard {
            deny: crate::machine_access::deny_list(&st),
            machine: crate::machine_access::whole_machine(&st),
        });
        // Each port on this Mac is its project's, in the fenced block.
        let scope = run_scope(&st, r.project_id.as_deref());
        let who = r.name.clone().unwrap_or_else(|| r.image.clone());
        // Checked before any port is kept: a refused run keeps none.
        let open: Vec<usize> = (0..r.ports.len()).filter(|&i| r.ports[i].host.is_none()).collect();
        for &i in &open {
            r.ports[i].host = Some(crate::services::block().start);
        }
        run_args(&r, guard.as_ref(), None).map_err(bad)?;
        let typed = (0..r.ports.len()).filter(|i| !open.contains(i)).filter_map(|i| r.ports[i].host);
        for host in typed {
            if crate::services::held_elsewhere(&st, &scope, host) {
                return Err(bad(format!("this Mac's port {host} is kept for something else: leave it out and the Bakery picks the project's own")));
            }
        }
        for &i in &open {
            let what = format!("{who}/{}", r.ports[i].container);
            r.ports[i].host = Some(crate::services::published_port(&st, &scope, &what).map_err(bad)?);
        }
        // Its engine first: making the project's network needs it running.
        wake(r.backend)?;
        // A project's run joins the project's network; the Bakery's own
        // runs (tied to no project) keep the engine's default.
        let network = match scope.as_str() {
            "bakery" => None,
            project => Some(crate::services::network(&st, r.backend, project).map_err(bad)?),
        };
        let args = run_args(&r, guard.as_ref(), network.as_deref()).map_err(bad)?;
        let (out, _) = run_env(&st.base, r.backend, &args, &r.env)?;
        Ok(Json(Ran {
            ok: true,
            id: out.trim().to_string(),
            published: r.ports.iter().filter_map(|p| Some(PublishedPort { host: p.host?, container: p.container })).collect(),
        }))
    })
    .await
}

#[derive(Serialize, schemars::JsonSchema)]
pub(crate) struct Ran {
    ok: bool,
    /// The new container's id.
    id: String,
    /// Its ports, as this Mac publishes them on 127.0.0.1.
    published: Vec<PublishedPort>,
}

#[derive(Serialize, schemars::JsonSchema)]
pub(crate) struct PublishedPort {
    host: u16,
    container: u16,
}

/// What an agent's run keeps from its container, as the agent's own room
/// would: the deny list and credential files stay hidden, and in a mount it
/// may write, git's folders and what the operator's sessions run later stay
/// read-only.
struct Guard {
    deny: Vec<PathBuf>,
    machine: bakery_exec::WholeMachine,
}

/// One path inside a mount and how it is covered.
#[derive(Debug, PartialEq)]
enum Cover {
    /// An empty tmpfs over a folder; the VM's /dev/null over a file.
    Hide(PathBuf),
    /// Mounted again over itself, read-only.
    ReadOnly(PathBuf),
}

/// What `guard` covers inside `source` (canonical), each before anything
/// inside it, as the mounts must be laid.
fn covers(source: &Path, write: bool, guard: &Guard) -> Vec<Cover> {
    fn walk(dir: &Path, write: bool, guard: &Guard, out: &mut Vec<Cover>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let (path, name) = (entry.path(), entry.file_name());
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if guard.deny.contains(&path) {
                // Hidden whole already; nothing inside may show again.
                continue;
            }
            if kind.is_dir() {
                if write && (name == ".git" || guard.machine.runs_later(&path)) {
                    out.push(Cover::ReadOnly(path.clone()));
                }
                // ponytail: git, dependency and build folders aren't walked; one walk per run.
                if !matches!(name.to_str(), Some(".git" | "node_modules" | "target")) {
                    walk(&path, write, guard, out);
                }
            } else if bakery_exec::is_hidden_credential(&path) {
                out.push(Cover::Hide(path));
            } else if write && guard.machine.runs_later(&path) {
                out.push(Cover::ReadOnly(path));
            }
        }
    }
    let inside = |d: &PathBuf| d.starts_with(source) && d != source;
    let mut out: Vec<Cover> = guard
        .deny
        .iter()
        .filter(|d| inside(d) && d.exists())
        // One inside another denied path is hidden with it.
        .filter(|d| {
            !guard
                .deny
                .iter()
                .any(|o| inside(o) && *d != o && d.starts_with(o))
        })
        .map(|d| Cover::Hide(d.clone()))
        .collect();
    walk(source, write, guard, &mut out);
    out
}

/// A path as a mount field: both CLIs split mounts on `:` and `,`.
fn field(p: &Path) -> Result<String, String> {
    let text = p.to_string_lossy();
    if text.contains([':', ',']) {
        return Err(format!("{text}: a path with ':' or ',' can't be mounted"));
    }
    Ok(text.into_owned())
}

/// The CLI words for a run; `guard` when an agent asked. Apple container
/// mounts folders only, so a file it would have to cover stops the run.
fn run_args(r: &RunReq, guard: Option<&Guard>, network: Option<&str>) -> Result<Vec<String>, String> {
    let plain = |id: &str| checked(id).map(str::to_string).map_err(|(_, e)| e);
    let who = crate::actor::attribute("human");
    let mut args: Vec<String> = vec![
        "run".into(),
        "--detach".into(),
        "--label".into(),
        format!("com.bakery.run={who}"),
    ];
    if let Some(name) = &r.name {
        args.extend(["--name".into(), plain(name)?]);
    }
    // Its project's own network, never the engine's shared default.
    if let Some(network) = network {
        args.extend(["--network".into(), network.to_string()]);
    }
    for m in &r.mounts {
        let target = Path::new(&m.target);
        if !Path::new(&m.source).is_absolute() || !target.is_absolute() || target == Path::new("/")
        {
            return Err(format!(
                "{} → {}: both must be absolute, and the target not /",
                m.source, m.target
            ));
        }
        let source = Path::new(&m.source)
            .canonicalize()
            .map_err(|e| format!("{}: {e}", m.source))?;
        if r.backend == Backend::Apple && !source.is_dir() {
            return Err(format!("{}: Apple container mounts folders only", m.source));
        }
        let readonly = if m.write { "" } else { ",readonly" };
        args.push("--mount".into());
        args.push(format!(
            "type=bind,source={},target={}{readonly}",
            field(&source)?,
            field(target)?
        ));
        let Some(guard) = guard else { continue };
        for cover in covers(&source, m.write, guard) {
            let (Cover::Hide(path) | Cover::ReadOnly(path)) = &cover;
            let inside = field(&target.join(path.strip_prefix(&source).unwrap_or(path)))?;
            if r.backend == Backend::Apple && !path.is_dir() {
                let what = match cover {
                    Cover::Hide(_) => "hidden",
                    Cover::ReadOnly(_) => "read-only",
                };
                return Err(format!(
                    "{} has to stay {what}, and Apple container can't cover a single file inside a mounted folder; mount a folder without it",
                    path.display()
                ));
            }
            match cover {
                Cover::Hide(_) if path.is_dir() => args.extend(["--tmpfs".into(), inside]),
                Cover::Hide(_) => args.extend([
                    "--mount".into(),
                    format!("type=bind,source=/dev/null,target={inside},readonly"),
                ]),
                Cover::ReadOnly(_) => args.extend([
                    "--mount".into(),
                    format!("type=bind,source={},target={inside},readonly", field(path)?),
                ]),
            }
        }
    }
    for p in &r.ports {
        let host = p.host.ok_or("a published port needs this Mac's port")?;
        if p.container == 0 {
            return Err("ports start at 1".to_string());
        }
        let block = crate::services::block();
        if !block.contains(&host) {
            return Err(format!(
                "this Mac's port {host} is outside the Bakery's block ({}–{}): leave it out and the Bakery picks the project's own",
                block.start,
                block.end - 1
            ));
        }
        args.extend(["--publish".into(), format!("127.0.0.1:{host}:{}", p.container)]);
    }
    for name in r.env.keys() {
        let plain_name = name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !plain_name {
            return Err(format!("{name:?} is not a variable name"));
        }
        args.extend(["--env".into(), name.clone()]);
    }
    args.push(plain(&r.image)?);
    args.extend(r.command.iter().cloned());
    Ok(args)
}

// ─── Agents ─────────────────────────────────────────────────────────────────

/// The container tools a task worker gets while its conversation's switch
/// is on: the container routes, but not compose files or the seat image,
/// which stay with the operator, nor restart, the operator's and the
/// Baker's.
fn worker_tool(t: &crate::api_catalog::Tool) -> bool {
    t.path.starts_with("/containers")
        && t.path != "/containers/compose/up"
        && t.path != "/containers/restart"
        && !t.path.starts_with("/containers/seat-image")
}

pub(crate) fn is_worker_tool(name: &str) -> bool {
    crate::api_catalog::find(name).is_some_and(worker_tool)
}

/// Their declarations, as the Baker's menu words them.
pub(crate) fn worker_decls() -> Vec<bakery_agent::ToolDecl> {
    crate::api_catalog::tools()
        .iter()
        .filter(|t| worker_tool(t))
        .map(|t| bakery_agent::ToolDecl {
            name: t.name.clone(),
            description: format!("{} [{}]", t.summary, t.risk.as_str()),
            parameters: t.input.clone(),
        })
        .collect()
}

/// Whether an agent's tool call, not the operator's UI, runs this handler.
fn for_agent() -> bool {
    crate::actor::attribute("human") != "human"
}

/// What an agent sees of the backend's JSON: every `KEY=value` list
/// (Docker's `Env`, Apple's `environment`) with its values hidden, since a
/// stack's environment holds its keys.
fn for_agent_output(mut v: Value) -> Value {
    fn hide_env(v: &mut Value) {
        match v {
            Value::Object(map) => {
                for (key, value) in map.iter_mut() {
                    match value {
                        Value::Array(items) if key == "Env" || key == "environment" => {
                            for item in items {
                                let name = item.as_str().and_then(|s| s.split_once('='));
                                if let Some((name, _)) = name {
                                    *item = Value::String(format!("{name}=(hidden)"));
                                }
                            }
                        }
                        _ => hide_env(value),
                    }
                }
            }
            Value::Array(items) => items.iter_mut().for_each(hide_env),
            _ => {}
        }
    }
    if for_agent() {
        hide_env(&mut v);
    }
    v
}

/// The containers switch, for `api_tools::call`: a container tool needs it
/// on for the calling conversation; compose files stay with the operator;
/// and a run mounts only what the agent reaches itself. Route handlers run
/// in the unsandboxed node, so this is the boundary.
pub(crate) fn agent_may(
    state: &AppState,
    stream_id: &str,
    tool: &str,
    args: &Value,
) -> Result<(), String> {
    let Some(path) = crate::api_catalog::find(tool).map(|t| t.path) else {
        return Ok(());
    };
    if !path.starts_with("/containers") {
        return Ok(());
    }
    if !crate::machine_access::containers_on(state, stream_id) {
        return Err("containers are off for this conversation; the operator turns them on in Settings → Agents. Never reach Docker or Apple container any other way".to_string());
    }
    match path {
        "/containers/compose/up" => Err("compose files stay with the operator: one can mount any folder and read the node's own environment. Ask the operator to bring the stack up, or run single containers with containers_run".to_string()),
        "/containers/run" => match serde_json::from_value::<RunReq>(args.clone()) {
            // Another project's port would take its place for that
            // project's rooms.
            Ok(run) if run.ports.iter().any(|p| p.host.is_some()) => Err("leave out this Mac's port: the Bakery picks your project's own, which only its rooms reach".to_string()),
            Ok(run) => run
                .mounts
                .iter()
                .try_for_each(|m| agent_mount(state, stream_id, run.backend, m)),
            // The route itself says what is wrong with the request.
            Err(_) => Ok(()),
        },
        _ => Ok(()),
    }
}

/// One mount an agent asks for: only what it reaches itself
/// (`machine_access::scope_allows`: the deny list, credential files, its
/// place on the dial), never a folder that holds the whole home folder, and
/// on Docker only from inside the home folder: Colima's VM shares folders
/// from there, so any other source is the VM's own, its Docker socket
/// included.
fn agent_mount(
    state: &AppState,
    stream_id: &str,
    backend: Backend,
    mount: &MountReq,
) -> Result<(), String> {
    use crate::machine_access::Access;
    let source = Path::new(&mount.source);
    if !source.is_absolute() {
        return Err(format!("{}: use an absolute path", mount.source));
    }
    let access = if mount.write {
        Access::Write
    } else {
        Access::Read
    };
    crate::machine_access::scope_allows(state, stream_id, source, access)
        .map_err(|why| format!("{} can't be mounted: {why}", mount.source))?;
    let canon = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let home = canon(&crate::machine_access::whole_machine(state).home);
    let real = canon(source);
    if home.starts_with(&real) {
        return Err(format!(
            "{}: too broad, it holds the whole home folder; mount the folder you need",
            mount.source
        ));
    }
    if backend.dialect() == Backend::Docker && !real.starts_with(&home) {
        return Err(format!(
            "{}: Docker mounts come from the home folder only; anything else is the engine VM's own",
            mount.source
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only an action that needs its engine starts OrbStack; a read on an
    /// OrbStack that isn't there says so, never falling back to Colima.
    /// (OrbStack's own start is the operator's QA: it isn't installed on
    /// every Mac this runs.)
    #[test]
    fn only_an_action_starts_orbstack_and_a_read_says_what_it_found() {
        assert!(wake(Backend::Docker).is_ok() && wake(Backend::Apple).is_ok(), "nothing to start");
        if crate::engine::orbstack_installed() {
            return;
        }
        assert_eq!(wake(Backend::Orbstack).unwrap_err().0, StatusCode::CONFLICT);
        if cli(Backend::Orbstack).is_ok() {
            let dir = tempfile::tempdir().unwrap();
            let read = run(dir.path(), Backend::Orbstack, &["ps".to_string()]).unwrap_err();
            assert_eq!(read, (StatusCode::CONFLICT, "OrbStack isn't installed.".to_string()));
        }
    }

    #[test]
    fn each_backend_gets_its_own_cli_words() {
        let words = |b, k, o, id| args(b, k, o, id).unwrap().join(" ");
        assert_eq!(
            words(Backend::Apple, Kind::Containers, Op::List, ""),
            "list --all --format json"
        );
        assert_eq!(
            words(Backend::Docker, Kind::Containers, Op::List, ""),
            "ps --all --no-trunc --format json"
        );
        assert_eq!(
            words(Backend::Apple, Kind::Volumes, Op::Remove, "v"),
            "volume delete v"
        );
        assert_eq!(
            words(Backend::Docker, Kind::Volumes, Op::Remove, "v"),
            "volume rm v"
        );
        assert_eq!(
            words(Backend::Docker, Kind::Compose, Op::Remove, "web"),
            "compose -p web down"
        );
        assert_eq!(
            words(Backend::Docker, Kind::Images, Op::Prune, ""),
            "image prune --force"
        );
        assert!(
            args(Backend::Apple, Kind::Compose, Op::List, "").is_err(),
            "compose is Docker's"
        );
        assert!(args(Backend::Docker, Kind::Images, Op::Start, "x").is_err());
        assert_eq!(
            words(Backend::Docker, Kind::Compose, Op::Restart, "web"),
            "compose -p web restart"
        );
        assert_eq!(
            words(Backend::Docker, Kind::Containers, Op::Restart, "db"),
            "restart db"
        );
        assert!(
            args(Backend::Apple, Kind::Containers, Op::Restart, "x").is_err(),
            "Apple container restarts as a stop and a start"
        );
    }

    /// Anyone's container restarts as itself; an agent's sandbox is refused
    /// while its turn runs, and the operator hears why.
    #[test]
    fn a_sandbox_in_use_is_never_restarted_under_its_agent() {
        let dir = tempfile::tempdir().unwrap();
        let state =
            crate::testutil::state_with(dir.path(), std::sync::Arc::new(bakery_agent::StubModel));
        let theirs = BTreeMap::from([("com.supabase.cli.project".to_string(), "app".to_string())]);
        assert!(restart_labelled(&state, &theirs, "supabase_db_app")
            .unwrap()
            .is_none());

        let sandbox = BTreeMap::from([
            ("com.bakery".to_string(), "seat".to_string()),
            ("com.bakery.stream".to_string(), "wt:busy".to_string()),
            ("com.bakery.seat".to_string(), "orchestrator".to_string()),
        ]);
        state
            .lifecycle
            .lock()
            .unwrap()
            .running_turns
            .insert(("wt:busy".to_string(), crate::node::Seat::Orchestrator), 1);
        let (code, why) = restart_labelled(&state, &sandbox, "bakery-0123").unwrap_err();
        assert_eq!(code, StatusCode::CONFLICT);
        assert!(why.contains("a turn is running"), "{why}");
    }

    #[test]
    fn names_that_look_like_flags_are_refused() {
        for bad in ["", "-f", "--all", "a b", "a\nb"] {
            assert!(checked(bad).is_err(), "{bad:?}");
        }
        assert_eq!(checked("supabase_db_1").unwrap(), "supabase_db_1");
        assert_eq!(checked("sha256:abc").unwrap(), "sha256:abc");
    }

    #[test]
    fn both_json_shapes_parse() {
        assert_eq!(parse_json(r#"[{"a":1}]"#), json!([{"a":1}]));
        assert_eq!(
            parse_json("{\"a\":1}\n{\"a\":2}\n"),
            json!([{"a":1},{"a":2}])
        );
        assert_eq!(parse_json(""), json!([]));
    }

    fn postgres(backend: Backend, mounts: Vec<MountReq>) -> RunReq {
        RunReq {
            backend,
            image: "docker.io/library/postgres:17".into(),
            name: Some("db".into()),
            command: vec!["postgres".into(), "-c".into(), "fsync=off".into()],
            mounts,
            ports: vec![PortReq {
                host: Some(crate::services::block().start + 33),
                container: 5432,
            }],
            env: BTreeMap::from([("POSTGRES_PASSWORD".into(), "value-never-in-argv".into())]),
            project_id: None,
        }
    }

    fn mount(source: &Path, target: &str, write: bool) -> MountReq {
        MountReq {
            source: source.display().to_string(),
            target: target.into(),
            write,
        }
    }

    #[test]
    fn a_run_publishes_on_loopback_and_passes_variables_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().canonicalize().unwrap();
        let args = run_args(
            &postgres(Backend::Apple, vec![mount(&data, "/data", false)]),
            None,
            Some("bakery-p-1"),
        );
        let text = args.unwrap().join(" ");
        assert!(
            text.starts_with("run --detach --label com.bakery.run=human --name db --network bakery-p-1 --mount "),
            "on its project's network: {text}"
        );
        let bind = format!("type=bind,source={},target=/data,readonly", data.display());
        assert!(text.contains(&bind), "{text}");
        let published = format!("--publish 127.0.0.1:{}:5432", crate::services::block().start + 33);
        assert!(text.contains(&published), "{text}");
        assert!(
            text.ends_with(
                "--env POSTGRES_PASSWORD docker.io/library/postgres:17 postgres -c fsync=off"
            ),
            "{text}"
        );
        assert!(!text.contains("value-never-in-argv"), "{text}");

        let refused = [
            RunReq {
                image: "--privileged".into(),
                ..postgres(Backend::Docker, vec![])
            },
            RunReq {
                env: BTreeMap::from([("A=B".into(), String::new())]),
                ..postgres(Backend::Docker, vec![])
            },
            RunReq {
                ports: vec![PortReq {
                    host: Some(crate::services::block().start + 33),
                    container: 0,
                }],
                ..postgres(Backend::Docker, vec![])
            },
            // A port on this Mac outside the Bakery's fenced block, which
            // every room would reach.
            RunReq {
                ports: vec![PortReq {
                    host: Some(5433),
                    container: 5432,
                }],
                ..postgres(Backend::Docker, vec![])
            },
            postgres(Backend::Docker, vec![mount(Path::new("rel"), "/x", false)]),
            postgres(Backend::Docker, vec![mount(&data, "/", true)]),
        ];
        for r in refused {
            assert!(run_args(&r, None, None).is_err());
        }
    }

    /// An agent's mount keeps what its room keeps: the deny list and
    /// credential files hidden, and where it may write, git's folders and
    /// what the operator's sessions run later read-only.
    #[test]
    fn an_agents_mount_keeps_what_its_room_keeps() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().canonicalize().unwrap();
        let app = home.join("Dev/app");
        for folder in [".git/hooks", "secrets/.git", "node_modules/x", "src"] {
            std::fs::create_dir_all(app.join(folder)).unwrap();
        }
        for file in [".env", "node_modules/x/.env", "src/main.rs"] {
            std::fs::write(app.join(file), "").unwrap();
        }
        // A package's certificate bundle stays visible (the operator's
        // decision); it is no secret.
        let certifi = app.join(".venv/lib/python3.12/site-packages/certifi");
        std::fs::create_dir_all(&certifi).unwrap();
        std::fs::write(certifi.join("cacert.pem"), "").unwrap();
        std::fs::create_dir_all(home.join("Library/LaunchAgents")).unwrap();
        let guard = Guard {
            deny: vec![
                app.join("secrets"),
                app.join("secrets/.git"),
                home.join(".ssh"),
            ],
            machine: bakery_exec::WholeMachine {
                home: home.clone(),
                bakery_root: home.join(".bakery"),
                path: String::new(),
            },
        };
        let hide = |p: &str| Cover::Hide(app.join(p));
        let read = covers(&app, false, &guard);
        assert_eq!(read.len(), 2, "{read:?}");
        assert!(
            read.contains(&hide("secrets")) && read.contains(&hide(".env")),
            "{read:?}"
        );
        let write = covers(&app, true, &guard);
        assert_eq!(write.len(), 3, "{write:?}");
        assert!(
            write.contains(&Cover::ReadOnly(app.join(".git"))),
            "{write:?}"
        );
        assert_eq!(
            covers(&home.join("Library"), true, &guard),
            [Cover::ReadOnly(home.join("Library/LaunchAgents"))]
        );

        let docker = run_args(
            &postgres(Backend::Docker, vec![mount(&app, "/w", true)]),
            Some(&guard),
            None,
        );
        let text = docker.unwrap().join(" ");
        for cover in [
            "--tmpfs /w/secrets",
            "--mount type=bind,source=/dev/null,target=/w/.env,readonly",
            &format!(
                "--mount type=bind,source={},target=/w/.git,readonly",
                app.join(".git").display()
            ),
        ] {
            assert!(text.contains(cover), "{cover}: {text}");
        }
        let apple = run_args(
            &postgres(Backend::Apple, vec![mount(&app, "/w", true)]),
            Some(&guard),
            None,
        );
        let err = apple.unwrap_err();
        assert!(err.contains(".env has to stay hidden"), "{err}");
    }

    #[test]
    fn agents_see_no_environment_values() {
        let seen = json!([{
            "Config": { "Env": ["PATH=/bin", "JWT_SECRET=abc"] },
            "configuration": { "initProcess": { "environment": ["KEY=v"] } },
        }]);
        assert_eq!(
            for_agent_output(seen.clone()),
            seen,
            "the operator sees it all"
        );
        let agent = crate::actor::with("agent:baker".into(), || for_agent_output(seen));
        assert_eq!(
            agent,
            json!([{
                "Config": { "Env": ["PATH=(hidden)", "JWT_SECRET=(hidden)"] },
                "configuration": { "initProcess": { "environment": ["KEY=(hidden)"] } },
            }])
        );
    }

    /// Off, no container tool works for the agent; on, compose files stay
    /// with the operator and a run mounts only what the agent reaches.
    #[test]
    fn the_switch_and_the_agents_reach_decide_what_it_may_mount() {
        use crate::testutil::*;
        use std::sync::Arc;
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(bakery_agent::StubModel));
        let stream = crate::routes::project::open_stream(&state, &open_req(&repo, "boxes"))
            .unwrap()
            .context
            .stream_id;
        let workspace = crate::state::context_for(&state, &stream)
            .unwrap()
            .worktree_path;
        let run = |source: &str, backend: &str| json!({ "backend": backend, "image": "alpine", "mounts": [{ "source": source, "target": "/w" }] });
        let may = |tool: &str, args: &Value| agent_may(&state, &stream, tool, args);

        let err = may("containers_list", &json!({})).unwrap_err();
        assert!(err.contains("containers are off"), "{err}");
        assert_eq!(
            may("machine_access", &json!({})),
            Ok(()),
            "not a container tool"
        );

        let on = |stream: &str| {
            let mut node = state.node.lock().unwrap();
            node.machine_access.containers.insert(stream.to_string());
        };
        on(&stream);
        assert_eq!(may("containers_list", &json!({})), Ok(()));
        let err = may("containers_compose_up", &json!({ "file": "/x" })).unwrap_err();
        assert!(
            err.contains("compose files stay with the operator"),
            "{err}"
        );
        assert_eq!(may("containers_run", &run(&workspace, "apple")), Ok(()));
        let ports = |port: Value| json!({ "backend": "apple", "image": "alpine", "ports": [port] });
        let err = may("containers_run", &ports(json!({ "host": 46001, "container": 80 }))).unwrap_err();
        assert!(err.contains("leave out this Mac's port"), "{err}");
        assert_eq!(may("containers_run", &ports(json!({ "container": 80 }))), Ok(()));
        let outside = tempfile::tempdir().unwrap();
        let outside = outside.path().display().to_string();
        let err = may("containers_run", &run(&outside, "apple")).unwrap_err();
        assert!(err.contains("not granted"), "{err}");

        // Host full reaches more: never the whole home, and on Docker only
        // what Colima shares from it.
        let full = crate::machine_access::Reach::HostFull;
        state
            .node
            .lock()
            .unwrap()
            .machine_access
            .reach
            .insert(stream.clone(), full);
        assert_eq!(may("containers_run", &run(&outside, "apple")), Ok(()));
        let home = crate::machine_access::whole_machine(&state).home;
        let err = may("containers_run", &run(&home.display().to_string(), "apple")).unwrap_err();
        assert!(err.contains("too broad"), "{err}");
        let err = may("containers_run", &run("/usr", "docker")).unwrap_err();
        assert!(err.contains("home folder only"), "{err}");
    }

    /// A run's ports and a compose stack are their project's: an agent's
    /// own thread's whatever it names, the one the operator names, the one
    /// whose folder holds the compose file; else the Bakery's, which no
    /// room reaches.
    #[test]
    fn runs_and_stacks_belong_to_their_project() {
        use std::sync::Arc;
        let (dir, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let state = crate::testutil::state_with(dir.path(), Arc::new(bakery_agent::StubModel));
        let app = work.path().join("farm");
        std::fs::create_dir_all(&app).unwrap();
        let root = crate::catalog::DirectoryRootSpec::Attach(app.to_str().unwrap());
        let (project, thread) = crate::catalog::create_project(&state, "farm", Some(root)).unwrap();
        let p = project.project_id;

        assert_eq!(run_scope(&state, Some("project_other")), "project_other");
        assert_eq!(run_scope(&state, None), "bakery");
        let agent = crate::actor::agent(&thread.stream_id, crate::node::Seat::Orchestrator);
        assert_eq!(crate::actor::with(agent, || run_scope(&state, Some("project_other"))), p);

        // Each file its own stack, named for its path in the folder.
        std::fs::create_dir_all(app.join("api")).unwrap();
        let outside = work.path().join("compose.yml");
        for (file, stack) in [
            (app.join("compose.yml"), "compose"),
            (app.join("docker-compose.test.yml"), "docker-compose-test"),
            (app.join("api/compose.yaml"), "api-compose"),
        ] {
            std::fs::write(&file, "services: {}\n").unwrap();
            let named = (p.clone(), format!("bakery-{p}-{stack}"));
            assert_eq!(compose_stack(&state, &file), Some(named));
        }
        std::fs::write(&outside, "services: {}\n").unwrap();
        assert_eq!(compose_stack(&state, &outside), None);
        assert_eq!(compose_name("project_01HX.y"), "bakery-project_01hx-y");
    }

    /// A compose project's ports move into the block, on 127.0.0.1: each
    /// container port once, as the project keeps it; a service on the
    /// host's network publishes nothing; a name YAML would read otherwise
    /// stays one name.
    #[test]
    fn a_compose_projects_ports_move_into_the_block() {
        let config = json!({
            "name": "bakery-project_x",
            "services": {
                "web": { "ports": [
                    { "target": 80, "published": "8080", "protocol": "tcp" },
                    { "target": 80, "published": "8081", "protocol": "tcp" },
                    { "target": 53, "published": "5353", "protocol": "udp", "host_ip": "0.0.0.0" },
                ] },
                "db": { "expose": ["5432"] },
                "net": { "network_mode": "host", "ports": [{ "target": 9000, "protocol": "tcp" }] },
                "a: b": { "ports": [{ "target": 3000, "protocol": "tcp" }] },
            },
        });
        let mut asked = Vec::new();
        let (text, published) = compose_override(&config, None, |what| {
            asked.push(what.to_string());
            Ok(match what {
                "web/80/tcp" => 46001,
                "web/53/udp" => 46002,
                _ => 46003,
            })
        })
        .unwrap();
        asked.sort();
        assert_eq!(asked, ["a: b/3000/tcp", "web/53/udp", "web/80/tcp"]);
        assert!(text.starts_with("services:\n"), "{text}");
        let web = "  \"web\":\n    ports: !override\n      - \"127.0.0.1:46001:80/tcp\"\n      - \"127.0.0.1:46002:53/udp\"\n";
        assert!(text.contains(web), "{text}");
        assert!(text.contains("  \"a: b\":\n    ports: !override\n      - \"127.0.0.1:46003:3000/tcp\"\n"), "{text}");
        assert!(!text.contains("db") && !text.contains("9000"), "{text}");
        assert_eq!(published.len(), 3);
        assert_eq!(published.iter().filter(|p| p["service"] == "web").count(), 2);

        let none = compose_override(&json!({ "services": { "db": {} } }), None, |_| unreachable!()).unwrap();
        assert_eq!(none, ("services: {}\n".to_string(), vec![]));
    }

    /// In a project, each of a stack's services joins the project's network
    /// as well as its own, behind them, so its own `db` answers first; a
    /// service on the host's network or another container's joins nothing.
    #[test]
    fn a_projects_stacks_share_its_network_behind_their_own() {
        let config = json!({
            "services": {
                "web": { "networks": { "default": null }, "ports": [{ "target": 80, "protocol": "tcp" }] },
                "db": { "networks": { "default": null, "backend": { "aliases": ["store"] } } },
                "bare": {},
                "net": { "network_mode": "host" },
                "side": { "network_mode": "service:web" },
            },
        });
        let (text, _) = compose_override(&config, Some("bakery-project_x"), |_| Ok(46001)).unwrap();
        assert!(
            text.starts_with("networks:\n  \"bakery-project\":\n    name: \"bakery-project_x\"\n    external: true\nservices:\n"),
            "{text}"
        );
        let behind = "      \"bakery-project\": {\"gw_priority\": -1}\n";
        let web = format!("  \"web\":\n    networks:\n      \"default\": {{}}\n{behind}    ports: !override\n      - \"127.0.0.1:46001:80/tcp\"\n");
        assert!(text.contains(&web), "{text}");
        assert!(text.contains("      \"backend\": {}\n"), "{text}");
        assert!(text.contains(&format!("  \"bare\":\n    networks:\n      \"default\": {{}}\n{behind}")), "{text}");
        assert_eq!(text.matches(behind).count(), 3, "{text}");
        assert!(!text.contains("\"net\"") && !text.contains("\"side\""), "{text}");
    }

    /// Both backends' list JSON become the same rows. The shapes are the
    /// operator's own output, trimmed (bakery-ui's containersModel.check.ts),
    /// plus Apple container's networks and volumes.
    #[test]
    fn both_backends_list_as_the_same_rows() {
        let docker = json!([
            {
                "ID": "fffd317c7915aa",
                "Names": "supabase_db_demo",
                "Image": "public.ecr.aws/supabase/postgres:17.6.1.106",
                "State": "running",
                "Status": "Up 4 hours (healthy)",
                "Ports": "0.0.0.0:55322->5432/tcp, [::]:55322->5432/tcp, 8080/tcp",
                "Labels": "com.docker.compose.project=demo,com.bakery.run=human",
            },
            { "ID": "cdbf606b3b82bb", "Names": "", "Image": "hello-world", "State": "exited", "Status": "Exited (0) 17 hours ago", "Ports": "", "Labels": "" },
        ]);
        let rows = listing(Backend::Docker, Kind::Containers, &docker).containers;
        assert!(rows[0].running && !rows[1].running);
        assert_eq!(rows[0].name, "supabase_db_demo");
        assert_eq!(rows[1].name, "cdbf606b3b82", "no name: the short id");
        let p = |host, container| Port {
            host,
            container,
            protocol: "tcp".into(),
        };
        assert_eq!(rows[0].ports, [p(Some(55322), 5432), p(None, 8080)]);
        assert_eq!(rows[0].role, Some(Role::Run));
        assert_eq!(rows[0].started_by.as_deref(), Some("human"));
        assert_eq!(rows[0].labels["com.docker.compose.project"], "demo");

        let apple = json!([
            {
                "id": "bakery-0123456789ab",
                "configuration": {
                    "id": "bakery-0123456789ab",
                    "image": { "reference": "bakery/workspace:dev" },
                    "publishedPorts": [{ "hostAddress": "127.0.0.1", "hostPort": 5433, "containerPort": 5432, "proto": "tcp", "count": 2 }],
                    "labels": { "com.bakery": "seat", "com.bakery.stream": "wt:x", "com.bakery.seat": "orchestrator" },
                },
                "status": { "state": "running", "networks": [{ "ipv4Address": "192.168.128.4/24" }] },
            },
            { "id": "old", "configuration": { "image": { "reference": "alpine" } }, "status": "stopped" },
        ]);
        let rows = listing(Backend::Apple, Kind::Containers, &apple).containers;
        assert!(rows[0].running && rows[0].status == "running");
        assert!(!rows[1].running && rows[1].status == "stopped");
        assert_eq!(rows[0].image, "bakery/workspace:dev");
        assert_eq!(rows[0].ports, [p(Some(5433), 5432), p(Some(5434), 5433)]);
        assert_eq!(rows[0].address.as_deref(), Some("192.168.128.4"));
        assert_eq!(rows[0].role, Some(Role::Seat));
        assert_eq!(
            (rows[0].stream_id.as_deref(), rows[0].seat.as_deref()),
            (Some("wt:x"), Some("orchestrator"))
        );

        let images = listing(
            Backend::Docker,
            Kind::Images,
            &json!([
                { "ID": "abc", "Repository": "postgres", "Tag": "17", "Size": "438MB", "CreatedSince": "2 weeks ago" },
                { "ID": "def", "Repository": "<none>", "Tag": "<none>", "Size": "1MB" },
            ]),
        )
        .images;
        assert_eq!(
            (images[0].name.as_str(), images[1].name.as_str()),
            ("postgres:17", "def")
        );
        let images = listing(
            Backend::Apple,
            Kind::Images,
            &json!([{
                "id": "966",
                "configuration": { "name": "bakery/workspace:w2-s7", "creationDate": "2026-10-08T22:41:53Z" },
                "variants": [
                    { "platform": { "architecture": "amd64" }, "size": 1 },
                    { "platform": { "architecture": "arm64" }, "size": 823_808_753u64 },
                ],
            }]),
        )
        .images;
        assert_eq!(images[0].name, "bakery/workspace:w2-s7");
        assert_eq!(images[0].created, "2026-10-08");
        if cfg!(target_arch = "aarch64") {
            assert_eq!(images[0].size, "824 MB");
        }

        let volumes = listing(
            Backend::Apple,
            Kind::Volumes,
            &json!([{ "id": "data", "configuration": { "name": "data", "driver": "local", "sizeInBytes": 1_500_000_000u64, "labels": { "a": "b" } } }]),
        )
        .volumes;
        assert_eq!(
            (volumes[0].name.as_str(), volumes[0].size.as_str()),
            ("data", "1.5 GB")
        );
        let networks = listing(
            Backend::Apple,
            Kind::Networks,
            &json!([{
                "id": "bakery-seats",
                "configuration": { "name": "bakery-seats", "mode": "hostOnly", "labels": { "com.bakery": "seat" } },
                "status": { "ipv4Gateway": "192.168.128.1", "ipv4Subnet": "192.168.128.0/24" },
            }]),
        )
        .networks;
        assert!(networks[0].internal && networks[0].gateway.as_deref() == Some("192.168.128.1"));
        let docker_net = listing(
            Backend::Docker,
            Kind::Networks,
            &json!([{ "ID": "n1", "Name": "bridge", "Driver": "bridge", "Internal": "false", "Labels": "" }]),
        )
        .networks;
        assert!(!docker_net[0].internal && docker_net[0].driver == "bridge");
        let compose = listing(
            Backend::Docker,
            Kind::Compose,
            &json!([{ "Name": "demo", "Status": "running(3)", "ConfigFiles": "/x/compose.yml,/x/override.yml" }]),
        )
        .compose;
        assert_eq!(compose[0].files, ["/x/compose.yml", "/x/override.yml"]);
        assert!(listing(Backend::Docker, Kind::Containers, &json!("nope"))
            .containers
            .is_empty());
    }

    // ── Live: each backend, acting only on the test's own container ────────
    //
    //   cargo test -p bakery-node containers::tests::live -- --ignored

    async fn manages_its_own_container(backend: Backend) {
        let name = format!("w2-containers-api-{}", std::process::id());
        let made = cli(backend)
            .unwrap()
            .args([
                "run",
                "--detach",
                "--name",
                &name,
                "--label",
                "com.bakery.w2=containers-api",
            ])
            .args([
                "docker.io/library/alpine:3.22",
                "sh",
                "-c",
                "echo hello; sleep 300",
            ])
            .output()
            .unwrap();
        assert!(
            made.status.success(),
            "{}",
            String::from_utf8_lossy(&made.stderr)
        );
        let dir = tempfile::tempdir().unwrap();
        let state =
            crate::testutil::state_with(dir.path(), std::sync::Arc::new(bakery_agent::StubModel));
        let st = || State(state.clone());
        let listed = |kind| list(st(), Query(ListQuery { backend, kind }));
        let mine = |listing: &Listing| listing.containers.iter().any(|c| c.name == name);
        let containers = |id: &str| ContainerReq {
            backend,
            id: id.to_string(),
        };

        let row = listed(Kind::Containers).await.unwrap().0;
        assert!(mine(&row), "{row:?}");
        let item = ItemQuery {
            backend,
            kind: Kind::Containers,
            id: name.clone(),
        };
        let inspected = inspect(st(), Query(item)).await.unwrap().0;
        assert!(inspected.to_string().contains(&name), "{inspected}");
        let lines = LogsQuery {
            backend,
            id: name.clone(),
            tail: Some(10),
        };
        assert!(logs(st(), Query(lines)).await.unwrap().0["text"]
            .as_str()
            .unwrap()
            .contains("hello"));
        assert_eq!(stop(st(), Json(containers(&name))).await.unwrap().0["ok"], true);
        assert_eq!(
            start(st(), Json(containers(&name)))
                .await
                .unwrap()
                .0["ok"],
            true
        );
        let again = ItemReq {
            backend,
            kind: Kind::Containers,
            id: name.clone(),
        };
        assert_eq!(
            restart(st(), Json(again)).await.unwrap().0["ok"],
            true
        );
        let row = listed(Kind::Containers).await.unwrap().0;
        assert!(
            row.containers.iter().any(|c| c.name == name && c.running),
            "restarted, it runs again: {row:?}"
        );
        assert_eq!(stop(st(), Json(containers(&name))).await.unwrap().0["ok"], true);
        let gone = ItemReq {
            backend,
            kind: Kind::Containers,
            id: name.clone(),
        };
        assert_eq!(remove(st(), Json(gone)).await.unwrap().0["ok"], true);
        assert!(!mine(&listed(Kind::Containers).await.unwrap().0));
        let flag = ContainerReq {
            backend,
            id: "--all".to_string(),
        };
        assert_eq!(
            stop(st(), Json(flag)).await.unwrap_err().0,
            StatusCode::BAD_REQUEST
        );
    }

    #[tokio::test]
    #[ignore = "needs Apple container"]
    async fn live_apple_manages_its_own_container() {
        manages_its_own_container(Backend::Apple).await;
    }

    #[tokio::test]
    #[ignore = "needs Colima's Docker"]
    async fn live_docker_manages_its_own_container() {
        manages_its_own_container(Backend::Docker).await;
    }
}
