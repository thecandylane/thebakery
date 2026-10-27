//! A project's own services, as the operator decided: Redis and Postgres,
//! each a container of the project's (`bakery-<project>-<service>`) on the
//! engine its setup names ("Services run in"): Apple container on the
//! project's own network, or OrbStack or Colima on the project's Docker
//! network. Its data lives in a volume of its own; it serves every thread
//! of the project, the operator's runs in its terminals included. Each
//! publishes on loopback on a port of its own in the [`block`], which only
//! its project's rooms on this Mac reach; the project's Apple-container
//! seats reach it on their network. A thread may start a separate copy for
//! destructive work (a migration, a flush) that keeps nothing, its port
//! reached by that thread's rooms alone, gone when it's done. The images
//! are the official ones, run as they are: no project code runs here. The
//! Homebrew Redis the Bakery ran before stops at the node's start
//! ([`sweep_copies`]); its data stays in its folder.

use crate::containers::Backend;
use crate::machine_access::Reach;
use crate::node::ProjectStacks;
use crate::state::AppState;
use axum::{extract::State, http::StatusCode, Json};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The services a project may have.
pub(crate) const KINDS: &[&str] = &["redis", "postgres"];

/// A service's image, its own port, and where it keeps its data.
struct Preset {
    image: &'static str,
    port: u16,
    data: &'static str,
}

fn preset(service: &str) -> Option<Preset> {
    match service {
        "redis" => Some(Preset {
            image: "docker.io/library/redis:8",
            port: 6379,
            data: "/data",
        }),
        // From 18 its data sits in a folder per major version under here.
        "postgres" => Some(Preset {
            image: "docker.io/library/postgres:18",
            port: 5432,
            data: "/var/lib/postgresql",
        }),
        _ => None,
    }
}

/// What it runs: Redis saving as the Bakery's always has, or (a copy)
/// keeping nothing; Postgres as its image has it.
fn command(service: &str, keeps: bool) -> Vec<String> {
    let save = if keeps { "3600 1 300 100 60 10000" } else { "" };
    match service {
        "redis" => ["redis-server", "--save", save, "--appendonly", "no"]
            .map(String::from)
            .to_vec(),
        _ => Vec::new(),
    }
}

/// Each project service's port, kept across restarts
/// (`~/.bakery/services.json`, `<project>/<service>` → port).
fn kept_file(state: &AppState) -> PathBuf {
    state.base.join("services.json")
}

fn kept(state: &AppState) -> HashMap<String, u16> {
    std::fs::read_to_string(kept_file(state))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn one_at_a_time() -> std::sync::MutexGuard<'static, ()> {
    static ONE: Mutex<()> = Mutex::new(());
    ONE.lock().unwrap_or_else(|e| e.into_inner())
}

/// One start at a time for each service; the others don't wait on it.
fn starting(key: &str) -> Arc<Mutex<()>> {
    static STARTING: Mutex<BTreeMap<String, Arc<Mutex<()>>>> = Mutex::new(BTreeMap::new());
    let mut all = STARTING.lock().unwrap_or_else(|e| e.into_inner());
    all.entry(key.to_string()).or_default().clone()
}

/// A port on this Mac for one of a project's containers to publish (`what`:
/// the container and its own port): in the services block, so the
/// project's Mac-sandbox rooms reach it (`env_for`) and no other project's
/// do; the same one each time. The Bakery's own runs, tied to no project,
/// keep theirs under `bakery`, which no room reaches.
// ponytail: a kept port never goes back to the block (500 for every
// project); free it when its container goes, if the block ever fills.
pub(crate) fn published_port(state: &AppState, project_id: &str, what: &str) -> Result<u16, String> {
    let _one = one_at_a_time();
    let key = format!("published/{what}");
    let known = kept(state).contains_key(&format!("{project_id}/{key}"));
    if !known && published(state, project_id).len() >= PUBLISHED_CAP {
        return Err(format!(
            "this project already keeps {PUBLISHED_CAP} published ports on this Mac; run its containers under names it used before"
        ));
    }
    port_for(state, project_id, &key)
}

/// How many published ports one project keeps: a loop of runs under new
/// names fills its share, never the whole block.
const PUBLISHED_CAP: usize = 32;

/// Whether a port in the block is kept for anything but `project_id`'s own
/// published containers: another project's, a service, or the Bakery's.
pub(crate) fn held_elsewhere(state: &AppState, project_id: &str, port: u16) -> bool {
    let own = format!("{project_id}/published/");
    kept(state).into_iter().any(|(key, kept)| kept == port && !key.starts_with(&own))
        || copies().lock().unwrap().iter().any(|c| c.port == port)
}

/// The ports a project's containers publish.
fn published(state: &AppState, project_id: &str) -> Vec<u16> {
    let prefix = format!("{project_id}/published/");
    kept(state).into_iter().filter(|(key, _)| key.starts_with(&prefix)).map(|(_, port)| port).collect()
}

/// This Mac's ports for the Bakery's services and the containers it
/// publishes ([`bakery_exec::SERVICES`]). Tests take a block of their own,
/// clear of the live node's.
pub(crate) fn block() -> std::ops::Range<u16> {
    if cfg!(test) {
        45500..45600
    } else {
        bakery_exec::SERVICES
    }
}

/// A free port in the block that no project kept and no copy holds.
fn free_port(taken: &HashSet<u16>) -> Option<u16> {
    block()
        .filter(|p| !taken.contains(p))
        .find(|p| std::net::TcpListener::bind(("127.0.0.1", *p)).is_ok())
}

/// The project service's own port: the one it had, else a free one, kept.
fn port_for(state: &AppState, project_id: &str, service: &str) -> Result<u16, String> {
    let key = format!("{project_id}/{service}");
    let mut kept = kept(state);
    if let Some(port) = kept.get(&key) {
        return Ok(*port);
    }
    let mut taken: HashSet<u16> = kept.values().copied().collect();
    taken.extend(copies().lock().unwrap().iter().map(|c| c.port));
    let port = free_port(&taken).ok_or("every service port on this Mac is in use")?;
    kept.insert(key, port);
    let file = kept_file(state);
    let fresh = file.with_extension("json.new");
    std::fs::write(&fresh, serde_json::to_string(&kept).unwrap_or_default())
        .and_then(|_| std::fs::rename(&fresh, &file))
        .map_err(|e| format!("{}: {e}", file.display()))?;
    Ok(port)
}

/// The project's Postgres password, made once and kept with the node,
/// readable by it alone.
fn password(state: &AppState, project_id: &str) -> Result<String, String> {
    use std::os::unix::fs::OpenOptionsExt;
    static ONE: Mutex<()> = Mutex::new(());
    let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
    let file = state.base.join("service-passwords.json");
    let mut kept: HashMap<String, String> = std::fs::read_to_string(&file)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    if let Some(password) = kept.get(project_id) {
        return Ok(password.clone());
    }
    let password = crate::state::mint_ws_ticket_string();
    kept.insert(project_id.to_string(), password.clone());
    let fresh = file.with_extension("json.new");
    let _ = std::fs::remove_file(&fresh);
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&fresh)
        .and_then(|mut f| f.write_all(serde_json::to_string(&kept).unwrap_or_default().as_bytes()))
        .and_then(|()| std::fs::rename(&fresh, &file))
        .map_err(|e| format!("{}: {e}", file.display()))?;
    Ok(password)
}

/// What a service's room needs besides its address: Postgres's password.
fn secret_of(state: &AppState, project_id: &str, service: &str) -> String {
    match service {
        "postgres" => password(state, project_id).unwrap_or_default(),
        _ => String::new(),
    }
}

/// Whether `service` answers on `port`: Redis to PING, Postgres to an SSL
/// request, which it answers yes or no before any sign-in.
fn alive(service: &str, port: u16) -> bool {
    let at = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let Ok(mut conn) = std::net::TcpStream::connect_timeout(&at, Duration::from_millis(500)) else {
        return false;
    };
    let _ = conn.set_read_timeout(Some(Duration::from_secs(2)));
    let (ask, yes): (&[u8], &[&[u8]]) = match service {
        "postgres" => (&[0, 0, 0, 8, 4, 210, 22, 47], &[b"S", b"N"]),
        _ => (b"PING\r\n", &[b"+PONG"]),
    };
    let mut answer = [0u8; 7];
    conn.write_all(ask).is_ok()
        && conn
            .read(&mut answer)
            .is_ok_and(|n| yes.iter().any(|y| answer[..n].starts_with(y)))
}

/// Where a project's services run: its setup's "Services run in".
pub(crate) fn engine_of(state: &AppState, stacks: &ProjectStacks) -> Backend {
    match stacks.containers.as_str() {
        "orbstack" | "colima" => crate::engine::docker_of(state, &stacks.containers),
        _ => Backend::Apple,
    }
}

/// A project's service container, `bakery-<project>-<service>`; its volume
/// has the same name.
pub(crate) fn name_of(project_id: &str, service: &str) -> String {
    crate::containers::compose_name(&format!("{project_id}-{service}"))
}

/// The project's own network on `backend`, made the first time: on Apple
/// container the one its seats are on.
pub(crate) fn network(state: &AppState, backend: Backend, project_id: &str) -> Result<String, String> {
    match backend {
        Backend::Apple => {
            crate::engine::note_project_use(&state.base, project_id, backend);
            let (name, labels) = crate::seat_container::network_of(&format!("project:{project_id}"));
            bakery_exec::ensure_seat_network(&crate::seat_container::apple_cli()?, &name, &labels)?;
            Ok(name)
        }
        _ => crate::containers::project_network(&state.base, backend, project_id).map_err(|(_, e)| e),
    }
}

/// The words that run one of a project's services: on its project's
/// `network`, published on loopback at `port`, its data in its own volume.
/// A copy (`thread`) keeps nothing and is removed when it stops. Each
/// carries the node that ran it (`node`), so another node on this Mac (a
/// scratch one, a preview) never takes it for its own.
fn run_args(
    node: &str,
    project_id: &str,
    service: &str,
    name: &str,
    network: &str,
    port: u16,
    thread: Option<&str>,
) -> Result<Vec<String>, String> {
    let preset = preset(service).ok_or_else(|| format!("the Bakery has no {service}"))?;
    let kind = if thread.is_some() { "service-copy" } else { "service" };
    let mut args: Vec<String> = vec![
        "run".into(),
        "--detach".into(),
        "--name".into(),
        name.into(),
        "--label".into(),
        format!("com.bakery={kind}"),
        "--label".into(),
        format!("com.bakery.project={project_id}"),
        "--label".into(),
        format!("com.bakery.service={service}"),
        "--label".into(),
        format!("com.bakery.node={node}"),
    ];
    match thread {
        Some(thread) => args.extend(["--label".into(), format!("com.bakery.stream={thread}"), "--rm".into()]),
        None => args.extend(["--mount".into(), format!("type=volume,source={name},target={}", preset.data)]),
    }
    args.extend([
        "--network".into(),
        network.into(),
        "--publish".into(),
        format!("127.0.0.1:{port}:{}", preset.port),
    ]);
    if service == "postgres" {
        // By name: its value never shows in the process list.
        args.extend(["--env".into(), "POSTGRES_PASSWORD".into()]);
    }
    args.push(preset.image.into());
    args.extend(command(service, thread.is_none()));
    Ok(args)
}

/// Starts one of a project's services on `port` (`thread`: that thread's
/// copy) on the engine its setup names, and waits until it answers. The
/// project's own one, stopped, starts again with its data.
fn start(
    state: &AppState,
    project_id: &str,
    stacks: &ProjectStacks,
    service: &str,
    port: u16,
    thread: Option<&str>,
) -> Result<(String, Backend), String> {
    let backend = engine_of(state, stacks);
    let base = &state.base;
    let mut name = name_of(project_id, service);
    if thread.is_some() {
        name = format!("{name}-copy-{port}");
    }
    let env = match service {
        "postgres" => BTreeMap::from([("POSTGRES_PASSWORD".to_string(), password(state, project_id)?)]),
        _ => BTreeMap::new(),
    };
    let again = thread.is_none() && crate::containers::run_action(base, backend, &["start".into(), name.clone()], &env).is_ok();
    if !again {
        let network = network(state, backend, project_id)?;
        // Apple container makes a named volume only when asked to.
        if thread.is_none() && backend == Backend::Apple && crate::containers::quietly(backend, &["volume", "inspect", name.as_str()]).is_none() {
            let label = format!("com.bakery.project={project_id}");
            let create = ["volume", "create", "--label", label.as_str(), name.as_str()].map(String::from);
            crate::containers::run_action(base, backend, &create, &BTreeMap::new())?;
        }
        let node = node_label(state);
        let args = run_args(&node, project_id, service, &name, &network, port, thread)?;
        crate::containers::run_action(base, backend, &args, &env)?;
    }
    let deadline = Instant::now() + Duration::from_secs(60);
    while !alive(service, port) {
        if Instant::now() > deadline {
            return Err(format!("{service} on port {port} never answered; its container is {name}"));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Ok((name, backend))
}

/// The project's services, as its setup names them.
fn of_project(state: &AppState, project_id: &str) -> Vec<String> {
    crate::catalog::project_by_id(state, project_id)
        .map(|p| p.stacks.services)
        .unwrap_or_default()
}

/// Where a service is, its password left out.
fn url(service: &str, host: &str, port: u16) -> String {
    match service {
        "postgres" => format!("postgres://postgres@{host}:{port}/postgres"),
        _ => format!("{service}://{host}:{port}"),
    }
}

/// The variables a room finds a service by at `host:port`, under the names
/// its programs read: Redis's as GreenLeaf's code does, Postgres's as libpq
/// and most frameworks do.
fn address_env(service: &str, host: &str, port: u16, password: &str) -> Vec<(String, String)> {
    let pairs: Vec<(&str, String)> = match service {
        "redis" => vec![
            ("REDIS_HOST", host.to_string()),
            ("REDIS_PORT", port.to_string()),
            ("REDIS_URL", url(service, host, port)),
        ],
        "postgres" => vec![
            ("PGHOST", host.to_string()),
            ("PGPORT", port.to_string()),
            ("PGUSER", "postgres".to_string()),
            ("PGDATABASE", "postgres".to_string()),
            ("PGPASSWORD", password.to_string()),
            ("DATABASE_URL", format!("postgres://postgres:{password}@{host}:{port}/postgres")),
        ],
        _ => Vec::new(),
    };
    pairs.into_iter().map(|(name, value)| (name.to_string(), value)).collect()
}

/// One of a project's services, as it runs.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub(crate) struct ServiceRunning {
    pub(crate) service: String,
    pub(crate) port: u16,
    /// `redis://127.0.0.1:<port>`, or Postgres's without its password.
    pub(crate) url: String,
    pub(crate) running: bool,
}

/// The project's services, started where they aren't running yet.
pub(crate) fn ensure(state: &AppState, project_id: &str) -> Vec<Result<ServiceRunning, String>> {
    let Some(project) = crate::catalog::project_by_id(state, project_id) else {
        return Vec::new();
    };
    project
        .stacks
        .services
        .iter()
        .map(|service| {
            let port = {
                let _one = one_at_a_time();
                port_for(state, project_id, service)?
            };
            let lock = starting(&format!("{project_id}/{service}"));
            let _held = lock.lock().unwrap_or_else(|e| e.into_inner());
            if !alive(service, port) {
                start(state, project_id, &project.stacks, service, port, None)?;
            }
            Ok(ServiceRunning {
                url: url(service, "127.0.0.1", port),
                service: service.clone(),
                port,
                running: true,
            })
        })
        .collect()
}

/// The project's services as they stand, started or not.
pub(crate) fn status(state: &AppState, project_id: &str) -> Vec<ServiceRunning> {
    let kept = kept(state);
    of_project(state, project_id)
        .into_iter()
        .filter_map(|service| {
            let port = *kept.get(&format!("{project_id}/{service}"))?;
            Some(ServiceRunning {
                url: url(&service, "127.0.0.1", port),
                running: alive(&service, port),
                service,
                port,
            })
        })
        .collect()
}

/// A project's services stop on the engine `stacks` names (a retired
/// project's, or the one its setup moved them from); their data stays in
/// their volumes. An engine that's down has nothing running to stop.
pub(crate) fn stop_project(state: &AppState, project_id: &str, stacks: &ProjectStacks) {
    let backend = engine_of(state, stacks);
    for service in KINDS {
        let _ = crate::containers::quietly(backend, &["stop", name_of(project_id, service).as_str()]);
    }
}

/// A thread's separate copy of one of its project's services.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub(crate) struct ServiceCopy {
    pub(crate) stream_id: String,
    pub(crate) service: String,
    pub(crate) port: u16,
    /// Where its thread reaches it, any password left out.
    pub(crate) url: String,
    /// When it started (RFC 3339).
    pub(crate) since: String,
    #[serde(skip)]
    #[schemars(skip)]
    name: String,
    #[serde(skip)]
    #[schemars(skip)]
    backend: Backend,
}

fn copies() -> &'static Mutex<Vec<ServiceCopy>> {
    static COPIES: Mutex<Vec<ServiceCopy>> = Mutex::new(Vec::new());
    &COPIES
}

/// The thread's copies running now.
pub(crate) fn copies_of(stream_id: &str) -> Vec<ServiceCopy> {
    copies()
        .lock()
        .unwrap()
        .iter()
        .filter(|c| c.stream_id == stream_id)
        .cloned()
        .collect()
}

fn project_of(state: &AppState, stream_id: &str) -> Option<String> {
    let heart = state.heart.lock().unwrap();
    crate::state::socket_project(&heart, stream_id).0
}

/// Whether the thread runs in a container on its project's Apple network.
fn contained(state: &AppState, stream_id: &str) -> bool {
    matches!(crate::machine_access::reach_for(state, stream_id), Reach::Container | Reach::Locked)
}

/// A separate copy of the thread's project's `service`: its own port and
/// container, nothing kept, for the thread alone.
pub(crate) fn start_copy(state: &AppState, stream_id: &str, service: &str) -> Result<ServiceCopy, String> {
    let project_id = project_of(state, stream_id).ok_or("this thread belongs to no project")?;
    let stacks = crate::catalog::project_by_id(state, &project_id)
        .map(|p| p.stacks)
        .unwrap_or_default();
    if !stacks.services.iter().any(|s| s == service) {
        return Err(format!(
            "this project has no {service}: its setup names its services"
        ));
    }
    // The port is picked and held under the lock; the start (a pull, its
    // wait) runs outside it, so nobody else's ports wait on this copy.
    let port = {
        let _one = one_at_a_time();
        let mut taken: HashSet<u16> = kept(state).values().copied().collect();
        let mut all = copies().lock().unwrap();
        taken.extend(all.iter().map(|c| c.port));
        let port = free_port(&taken).ok_or("every service port on this Mac is in use")?;
        all.push(ServiceCopy {
            stream_id: stream_id.to_string(),
            service: service.to_string(),
            port,
            url: String::new(),
            since: chrono::Utc::now().to_rfc3339(),
            name: String::new(),
            backend: Backend::Apple,
        });
        port
    };
    let started = start(state, &project_id, &stacks, service, port, Some(stream_id)).and_then(|(name, backend)| {
        // A seat on the project's Apple network reaches it there.
        let address = match (contained(state, stream_id), backend, preset(service)) {
            (true, Backend::Apple, Some(preset)) => {
                let cli = crate::seat_container::apple_cli()?;
                let ip = bakery_exec::container_ipv4(bakery_exec::Backend::Apple, &cli, &[], &name)?;
                url(service, &ip.to_string(), preset.port)
            }
            _ => url(service, "127.0.0.1", port),
        };
        Ok((name, backend, address))
    });
    let mut all = copies().lock().unwrap();
    let at = all.iter().position(|c| c.port == port && c.stream_id == stream_id);
    let (name, backend, address) = match (started, at) {
        (Ok(started), Some(_)) => started,
        // Stopped while it started: it goes now.
        (Ok((name, backend, _)), None) => {
            drop(all);
            let _ = crate::containers::quietly(backend, &["stop", name.as_str()]);
            return Err("the copy was stopped while it started".to_string());
        }
        (Err(e), at) => {
            if let Some(at) = at {
                all.remove(at);
            }
            return Err(e);
        }
    };
    let at = at.unwrap_or_default();
    let copy = &mut all[at];
    (copy.name, copy.backend, copy.url) = (name, backend, address);
    Ok(copy.clone())
}

/// The thread's copy on `port` stops, and goes with what it held.
pub(crate) fn stop_copy(stream_id: &str, port: u16) -> Result<(), String> {
    let copy = {
        let mut all = copies().lock().unwrap();
        let at = all
            .iter()
            .position(|c| c.stream_id == stream_id && c.port == port)
            .ok_or_else(|| format!("this thread has no copy on port {port}"))?;
        all.remove(at)
    };
    let _ = crate::containers::quietly(copy.backend, &["stop", copy.name.as_str()]);
    Ok(())
}

/// The thread's copies, all stopped: it was put away.
pub(crate) fn stop_copies(stream_id: &str) {
    for copy in copies_of(stream_id) {
        let _ = stop_copy(stream_id, copy.port);
    }
}

/// At the node's start. Copies a restart left running go, the turns they
/// served having ended, on whichever engines are up (none is started). And
/// the Homebrew Redis the Bakery ran before its services were containers
/// stops: each project's saves first, and its data stays in its folder.
pub(crate) fn sweep_copies(state: &AppState) {
    let node = node_label(state);
    for backend in [Backend::Apple, Backend::Docker, Backend::Orbstack] {
        for name in crate::containers::labelled(backend, |labels| sweeps(labels, &node)) {
            let _ = crate::containers::quietly(backend, &["stop", name.as_str()]);
        }
    }
    let projects = state.base.join("cache/projects");
    for project in std::fs::read_dir(projects).into_iter().flatten().flatten() {
        let services = project.path().join("services");
        retire(&services.join("redis"));
        for copy in std::fs::read_dir(services.join("copies")).into_iter().flatten().flatten() {
            retire(&copy.path());
            let _ = std::fs::remove_dir_all(copy.path());
        }
    }
}

/// Whether the start sweep stops a container: a service copy of this
/// node's, or one an older build ran before copies carried their node.
fn sweeps(labels: &BTreeMap<String, String>, node: &str) -> bool {
    labels.get("com.bakery").is_some_and(|kind| kind == "service-copy")
        && labels.get("com.bakery.node").is_none_or(|of| of == node)
}

/// What a service container says of the node that ran it: a hash of its
/// node id, never the id itself (which carries the Mac's name), since
/// container listings reach agents as they are.
fn node_label(state: &AppState) -> String {
    let id = state.node.lock().unwrap().node_id.clone();
    blake3::hash(id.as_bytes()).to_hex()[..16].to_string()
}

/// Stops a Redis the Bakery ran from Homebrew in `dir`, by its pid file, and
/// waits for it to go; a pid that is no Redis any more is left alone.
fn retire(dir: &Path) {
    let pid = std::fs::read_to_string(dir.join("redis.pid"))
        .ok()
        .and_then(|p| p.trim().parse::<i32>().ok());
    let Some(pid) = pid else {
        return;
    };
    let config = std::fs::read_to_string(dir.join("redis.conf")).unwrap_or_default();
    let running = std::process::Command::new("/bin/ps")
        .args(["-o", "command=", "-p", &pid.to_string()])
        .output();
    let command = running.map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
    if !bakerys_redis(&command, &config) {
        return;
    }
    // SAFETY: a signal to the Redis its own pid file names.
    unsafe { libc::kill(pid, libc::SIGTERM) };
    let deadline = Instant::now() + Duration::from_secs(10);
    // SAFETY: signal 0 only asks whether it is still there.
    while unsafe { libc::kill(pid, 0) } == 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Whether a process's command line is the Redis the Bakery ran from this
/// folder: redis-server listening on the port its own `redis.conf` names.
/// A stale pid file whose pid is now the operator's own Redis (Homebrew's
/// service on 6379, say) is never theirs.
fn bakerys_redis(command: &str, config: &str) -> bool {
    let port = config
        .lines()
        .find_map(|line| line.strip_prefix("port "))
        .and_then(|p| p.trim().parse::<u16>().ok());
    let Some(port) = port else {
        return false;
    };
    let mut words = command.split_whitespace();
    words.next().is_some_and(|program| program.ends_with("redis-server"))
        && words.any(|word| word == format!("127.0.0.1:{port}"))
}

/// What a thread's rooms on this Mac get: the ports they may reach (its
/// project's services and published containers, and its own copies), and
/// where its project's services are, under the names their programs read.
pub(crate) fn env_for(state: &AppState, stream_id: &str) -> Vec<(String, String)> {
    let mut reach: Vec<u16> = copies_of(stream_id).iter().map(|c| c.port).collect();
    let mut env = Vec::new();
    if let Some(project_id) = project_of(state, stream_id) {
        reach.extend(published(state, &project_id));
        for running in ensure(state, &project_id) {
            let running = match running {
                Ok(running) => running,
                Err(e) => {
                    eprintln!("services: {project_id}: {e}");
                    continue;
                }
            };
            reach.push(running.port);
            let secret = secret_of(state, &project_id, &running.service);
            env.extend(address_env(&running.service, "127.0.0.1", running.port, &secret));
        }
    }
    let reach: Vec<String> = reach.iter().map(u16::to_string).collect();
    env.push((bakery_exec::SERVICES_ENV.to_string(), reach.join(",")));
    env
}

/// What a container on its project's Apple network gets: each of the
/// project's services at its own address there, when they run on Apple
/// container. On OrbStack or Colima they're out of its reach until its door
/// bridges them.
pub(crate) fn container_env(state: &AppState, project_id: &str) -> Vec<(String, String)> {
    let Some(project) = crate::catalog::project_by_id(state, project_id) else {
        return Vec::new();
    };
    if project.stacks.services.is_empty() || engine_of(state, &project.stacks) != Backend::Apple {
        return Vec::new();
    }
    let Ok(cli) = crate::seat_container::apple_cli() else {
        return Vec::new();
    };
    let mut env = Vec::new();
    for running in ensure(state, project_id) {
        let running = match running {
            Ok(running) => running,
            Err(e) => {
                eprintln!("services: {project_id}: {e}");
                continue;
            }
        };
        let name = name_of(project_id, &running.service);
        let ip = bakery_exec::container_ipv4(bakery_exec::Backend::Apple, &cli, &[], &name);
        if let (Ok(ip), Some(preset)) = (ip, preset(&running.service)) {
            let secret = secret_of(state, project_id, &running.service);
            env.extend(address_env(&running.service, &ip.to_string(), preset.port, &secret));
        }
    }
    env
}

/// The copies running for the project's threads.
pub(crate) fn project_copies(state: &AppState, project_id: &str) -> Vec<ServiceCopy> {
    let all = copies().lock().unwrap().clone();
    all.into_iter()
        .filter(|c| project_of(state, &c.stream_id).as_deref() == Some(project_id))
        .collect()
}

/// What a thread's agent is told of its project's services, once a session.
pub(crate) fn session_line(state: &AppState, stream_id: &str) -> Option<String> {
    let project_id = project_of(state, stream_id)?;
    let stacks = crate::catalog::project_by_id(state, &project_id)?.stacks;
    if stacks.services.is_empty() {
        return None;
    }
    let services = stacks.services.join(", ");
    let backend = engine_of(state, &stacks);
    if contained(state, stream_id) && backend != Backend::Apple {
        let engine = if backend == Backend::Orbstack { "OrbStack" } else { "Colima" };
        return Some(format!(
            "services: this project's own {services} run on {engine}, which this container doesn't reach yet"
        ));
    }
    let names: Vec<String> = stacks
        .services
        .iter()
        .flat_map(|s| address_env(s, "", 0, ""))
        .map(|(name, _)| name)
        .collect();
    Some(format!(
        "services: this project's own {services} ({} are set), shared by its threads and the operator; for destructive work (a flush, a migration) start a separate copy of your own with service_copy, and stop it with service_copy_stop when done",
        names.join(", ")
    ))
}

#[derive(Deserialize, schemars::JsonSchema)]
pub(crate) struct ServiceCopyReq {
    pub(crate) stream_id: String,
    /// `redis` or `postgres`.
    pub(crate) service: String,
}

/// `POST /project/service/copy`: a separate copy of a project service for
/// destructive work, the thread's alone; stop it when done. Its rooms reach
/// it from their next command.
pub(crate) async fn copy_route(
    State(st): State<AppState>,
    Json(req): Json<ServiceCopyReq>,
) -> Result<Json<ServiceCopy>, (StatusCode, String)> {
    crate::actor::spawn_blocking(move || start_copy(&st, &req.stream_id, &req.service))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map(Json)
        .map_err(|e| (StatusCode::CONFLICT, e))
}

#[derive(Deserialize, schemars::JsonSchema)]
pub(crate) struct ServiceStopReq {
    pub(crate) stream_id: String,
    pub(crate) port: u16,
}

/// `POST /project/service/copy/stop`: the thread's copy stops, its data
/// gone; the thread's copies left are the answer.
pub(crate) async fn stop_route(Json(req): Json<ServiceStopReq>) -> Result<Json<Vec<ServiceCopy>>, (StatusCode, String)> {
    crate::actor::spawn_blocking(move || {
        stop_copy(&req.stream_id, req.port).map(|()| copies_of(&req.stream_id))
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map(Json)
    .map_err(|e| (StatusCode::NOT_FOUND, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// A project's containers publish on ports of its own in the block, the
    /// same each time, which its rooms reach and no other project's do.
    #[test]
    fn a_projects_published_ports_are_its_own_rooms_alone() {
        let (dir, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let state = crate::testutil::state_with(dir.path(), Arc::new(bakery_agent::StubModel));
        let make = |name: &str| {
            let app = work.path().join(name);
            std::fs::create_dir_all(&app).unwrap();
            let root = crate::catalog::DirectoryRootSpec::Attach(app.to_str().unwrap());
            let (project, thread) = crate::catalog::create_project(&state, name, Some(root)).unwrap();
            (project.project_id, thread.stream_id)
        };
        let (farm, farm_thread) = make("farm");
        let (shop, shop_thread) = make("shop");
        let port = published_port(&state, &farm, "db/5432").unwrap();
        assert!(block().contains(&port));
        assert_eq!(published_port(&state, &farm, "db/5432").unwrap(), port, "the same each time");
        let other = published_port(&state, &shop, "db/5432").unwrap();
        assert_ne!(other, port);

        let reach = |sid: &str| -> Vec<u16> {
            let env = env_for(&state, sid);
            let (_, ports) = env.iter().find(|(k, _)| k == bakery_exec::SERVICES_ENV).unwrap();
            ports.split(',').filter_map(|p| p.parse().ok()).collect()
        };
        assert_eq!(reach(&farm_thread), [port]);
        assert_eq!(reach(&shop_thread), [other]);
    }

    /// A project's service is a container of its own on its project's
    /// network, published on loopback, its data in its own volume, its
    /// password passed by name; a thread's copy keeps nothing, carries its
    /// thread, and goes when it stops.
    #[test]
    fn a_service_runs_in_its_own_container_on_its_projects_network() {
        let main = run_args("node_1", "project_x", "postgres", "bakery-project_x-postgres", "bakery-p-1", 45501, None)
            .unwrap()
            .join(" ");
        assert_eq!(
            main,
            "run --detach --name bakery-project_x-postgres --label com.bakery=service --label com.bakery.project=project_x --label com.bakery.service=postgres --label com.bakery.node=node_1 --mount type=volume,source=bakery-project_x-postgres,target=/var/lib/postgresql --network bakery-p-1 --publish 127.0.0.1:45501:5432 --env POSTGRES_PASSWORD docker.io/library/postgres:18"
        );
        let copy = run_args("node_1", "project_x", "redis", "bakery-project_x-redis-copy-45502", "bakery-project_x", 45502, Some("wt:a"))
            .unwrap();
        let text = copy.join(" ");
        assert!(text.contains("--label com.bakery=service-copy --label com.bakery.project=project_x"), "{text}");
        assert!(text.contains("--label com.bakery.node=node_1"), "its node: another node's sweep leaves it");
        assert!(text.contains("--label com.bakery.stream=wt:a --rm --network bakery-project_x --publish 127.0.0.1:45502:6379"), "{text}");
        assert!(!text.contains("--mount"), "{text}");
        assert_eq!(copy[copy.len() - 5..], ["redis-server", "--save", "", "--appendonly", "no"]);
        assert!(run_args("node_1", "project_x", "mysql", "n", "net", 45503, None).is_err());
        assert_eq!(name_of("project_X", "redis"), "bakery-project_x-redis");
    }

    /// A room finds each service by the names its programs read; what the
    /// Bakery shows of where one is never carries its password.
    #[test]
    fn rooms_find_each_service_by_the_names_their_programs_read() {
        let redis = address_env("redis", "127.0.0.1", 45501, "");
        assert_eq!(
            redis,
            [
                ("REDIS_HOST".to_string(), "127.0.0.1".to_string()),
                ("REDIS_PORT".to_string(), "45501".to_string()),
                ("REDIS_URL".to_string(), "redis://127.0.0.1:45501".to_string()),
            ]
        );
        let postgres: BTreeMap<String, String> = address_env("postgres", "192.168.64.5", 5432, "pw0").into_iter().collect();
        assert_eq!(postgres["DATABASE_URL"], "postgres://postgres:pw0@192.168.64.5:5432/postgres");
        assert_eq!(postgres["PGPASSWORD"], "pw0");
        assert_eq!((postgres["PGHOST"].as_str(), postgres["PGPORT"].as_str()), ("192.168.64.5", "5432"));
        assert_eq!(url("postgres", "127.0.0.1", 45502), "postgres://postgres@127.0.0.1:45502/postgres");
    }

    /// A project keeps a bounded share of the block, and a port kept for
    /// anything else is never its to take.
    #[test]
    fn a_project_keeps_a_bounded_share_of_the_block() {
        let dir = tempfile::tempdir().unwrap();
        let state = crate::testutil::state_with(dir.path(), Arc::new(bakery_agent::StubModel));
        let mut kept = HashMap::new();
        for n in 0..PUBLISHED_CAP {
            kept.insert(format!("project_a/published/c{n}/80"), 45000 + n as u16);
        }
        kept.insert("project_b/redis".to_string(), 45400);
        std::fs::write(kept_file(&state), serde_json::to_string(&kept).unwrap()).unwrap();
        assert_eq!(published_port(&state, "project_a", "c3/80").unwrap(), 45003, "one it keeps");
        assert!(published_port(&state, "project_a", "new/80").is_err(), "no more");
        assert!(held_elsewhere(&state, "project_a", 45400));
        assert!(!held_elsewhere(&state, "project_a", 45003), "its own");
        assert!(held_elsewhere(&state, "project_b", 45003));
    }

    /// A node's sweep takes its own copies and older builds' unlabelled
    /// ones, never another node's or a project's own service.
    #[test]
    fn a_nodes_sweep_takes_its_own_and_unlabelled_copies() {
        let labels = |pairs: &[(&str, &str)]| -> BTreeMap<String, String> {
            pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
        };
        assert!(sweeps(&labels(&[("com.bakery", "service-copy"), ("com.bakery.node", "aa")]), "aa"));
        assert!(sweeps(&labels(&[("com.bakery", "service-copy")]), "aa"), "an older build's");
        assert!(!sweeps(&labels(&[("com.bakery", "service-copy"), ("com.bakery.node", "bb")]), "aa"));
        assert!(!sweeps(&labels(&[("com.bakery", "service"), ("com.bakery.node", "aa")]), "aa"));
    }

    /// A container names its node by a hash that carries no part of the
    /// node id.
    #[test]
    fn a_container_names_its_node_by_a_hash() {
        let dir = tempfile::tempdir().unwrap();
        let state = crate::testutil::state_with(dir.path(), Arc::new(bakery_agent::StubModel));
        let id = state.node.lock().unwrap().node_id.clone();
        let label = node_label(&state);
        assert_eq!(label.len(), 16);
        assert!(label.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(!id.contains(&label) && !label.contains(&id), "{id} {label}");
        assert_eq!(node_label(&state), label, "the same each time");
    }

    /// The node stops only the Redis it ran itself: the operator's own,
    /// even under a stale pid, is never touched.
    #[test]
    fn only_the_bakerys_own_redis_is_retired() {
        let config = "port 46003\nbind 127.0.0.1\n";
        assert!(bakerys_redis("/opt/homebrew/bin/redis-server 127.0.0.1:46003\n", config));
        assert!(!bakerys_redis("/opt/homebrew/opt/redis/bin/redis-server 127.0.0.1:6379", config));
        assert!(!bakerys_redis("/usr/bin/vim 127.0.0.1:46003", config));
        assert!(!bakerys_redis("/opt/homebrew/bin/redis-server 127.0.0.1:46003", "bind 127.0.0.1\n"));
    }

    /// Each service is alive only when it answers as itself: Redis to PING,
    /// Postgres to an SSL request; a port that takes the connection and
    /// says nothing is not.
    #[test]
    fn a_service_is_alive_only_when_it_answers_as_itself() {
        // This Mac's network filter holds a fresh test binary's first
        // loopback connection for a while; let it, before timing any.
        let warm = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let _ = std::net::TcpStream::connect(warm.local_addr().unwrap());
        let serve = |answer: &'static [u8]| {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            std::thread::spawn(move || {
                if let Ok((mut conn, _)) = listener.accept() {
                    let mut ask = [0u8; 8];
                    let _ = conn.read(&mut ask);
                    let _ = conn.write_all(answer);
                }
            });
            port
        };
        assert!(alive("postgres", serve(b"N")));
        assert!(alive("redis", serve(b"+PONG\r\n")));
        assert!(!alive("postgres", serve(b"")));
        assert!(!alive("redis", serve(b"-ERR")));
    }

    /// Live, on this Mac: a project's Redis and Postgres as its containers
    /// on its own network, published on the tests' own block, reached from
    /// its rooms by the names their programs read (and, on Apple container,
    /// from its seats at their own addresses); a thread's copy that goes
    /// when it's done; stopped with the project, their data kept.
    /// Everything it made is removed when it ends, a failure included.
    /// Pulls the official images.
    ///   cargo test -p bakery-node services::tests::live -- --ignored --nocapture
    #[test]
    #[ignore = "needs Apple container and pulls redis and postgres"]
    fn live_a_projects_services_run_as_its_apple_containers() {
        live("apple", Backend::Apple);
    }

    #[test]
    #[ignore = "needs Colima running and pulls redis and postgres"]
    fn live_a_projects_services_run_as_its_colima_containers() {
        live("colima", Backend::Docker);
    }

    fn live(containers: &str, backend: Backend) {
        let (dir, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let state = crate::testutil::state_with(dir.path(), Arc::new(bakery_agent::StubModel));
        let app = work.path().join("farm");
        std::fs::create_dir_all(&app).unwrap();
        let root = crate::catalog::DirectoryRootSpec::Attach(app.to_str().unwrap());
        let (project, thread) = crate::catalog::create_project(&state, "farm", Some(root)).unwrap();
        let p = project.project_id.clone();
        let sid = thread.stream_id.as_str();
        // Whatever it made goes, even when an assertion fails.
        struct Gone<'a>(&'a str, &'a str, Backend);
        impl Drop for Gone<'_> {
            fn drop(&mut self) {
                stop_copies(self.1);
                let apple = self.2 == Backend::Apple;
                let network = match apple {
                    true => crate::seat_container::network_of(&format!("project:{}", self.0)).0,
                    false => crate::containers::compose_name(self.0),
                };
                for service in KINDS {
                    let name = name_of(self.0, service);
                    let (container, volume) = if apple { (["delete", "--force"], ["volume", "delete"]) } else { (["rm", "--force"], ["volume", "rm"]) };
                    let _ = crate::containers::quietly(self.2, &[container[0], container[1], name.as_str()]);
                    let _ = crate::containers::quietly(self.2, &[volume[0], volume[1], name.as_str()]);
                }
                let remove = if apple { "delete" } else { "rm" };
                let _ = crate::containers::quietly(self.2, &["network", remove, network.as_str()]);
            }
        }
        let _gone = Gone(&p, sid, backend);
        let stacks = ProjectStacks {
            services: vec!["redis".to_string(), "postgres".to_string()],
            containers: containers.to_string(),
            ..Default::default()
        };
        crate::catalog::set_project_stacks(&state, &p, stacks.clone()).unwrap();
        assert_eq!(engine_of(&state, &stacks), backend);

        let env: BTreeMap<String, String> = env_for(&state, sid).into_iter().collect();
        let redis: u16 = env["REDIS_PORT"].parse().unwrap();
        let postgres: u16 = env["PGPORT"].parse().unwrap();
        assert!(block().contains(&redis) && block().contains(&postgres), "{env:?}");
        assert!(alive("redis", redis) && alive("postgres", postgres));
        let reach = &env[bakery_exec::SERVICES_ENV];
        assert!([redis, postgres].iter().all(|p| reach.split(',').any(|r| r == p.to_string())), "{reach}");
        assert!(env["DATABASE_URL"].ends_with(&format!("@127.0.0.1:{postgres}/postgres")));

        let inside: BTreeMap<String, String> = container_env(&state, &p).into_iter().collect();
        if backend == Backend::Apple {
            assert_eq!((inside["REDIS_PORT"].as_str(), inside["PGPORT"].as_str()), ("6379", "5432"));
            assert_ne!(inside["PGHOST"], "127.0.0.1");
        } else {
            assert!(inside.is_empty(), "its Apple seats don't reach Docker until the door bridges it");
        }

        let copy = start_copy(&state, sid, "redis").unwrap();
        assert!(alive("redis", copy.port) && copy.port != redis);
        stop_copy(sid, copy.port).unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        while alive("redis", copy.port) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(200));
        }
        assert!(!alive("redis", copy.port), "gone when done");

        stop_project(&state, &p, &stacks);
        assert!(!alive("redis", redis) && !alive("postgres", postgres), "stops with its project");
        let again: BTreeMap<String, String> = env_for(&state, sid).into_iter().collect();
        assert_eq!(again["PGPASSWORD"], env["PGPASSWORD"], "the same password");
        assert!(alive("postgres", postgres), "starts again on its kept port, its data kept");
    }
}
