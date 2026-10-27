//! The engine room: what this Mac runs for the Bakery and its projects, each
//! read as its own tool reports it: Apple container, Tailscale (are the
//! phones and other machines reachable), Colima, Homebrew's services,
//! cloudflared and Supabase's local stacks (Compose projects are in the
//! containers list). Colima's default VM, the Bakery's Docker, shows what
//! it's given beside what it uses (inside, per container, and of this Mac's
//! memory), with the peak its programs reach and a size from it. Nothing
//! here changes any engine but Colima's default VM: its idle stop, on
//! unless the operator turns it off, stops it once nothing has used Docker
//! for a while, and a Docker action starts it again, at the size the
//! operator picked, if they did. A VM the operator stopped stays stopped.
//!
//! OrbStack is the Bakery's Docker once it's installed (the operator's
//! install, asked here), unless they pick Colima, the fallback and the other
//! choice, for the Bakery or one project. Its Docker is the `orbstack`
//! backend, through its own context, with the memory it grows and gives back
//! to macOS shown beside its use. A Docker action starts a stopped OrbStack
//! rather than turning to Colima, so a project's images and data stay on one
//! engine.
//!
//! A delete the operator confirmed starts each engine set up here that is
//! off, so what it holds of the deleted thing goes too, and stops it again.

use crate::containers::Backend;
use crate::state::AppState;
use axum::{extract::State, http::StatusCode, Json};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

const GIB: u64 = 1 << 30;

type Fail = (StatusCode, String);

fn operator_only() -> Result<(), Fail> {
    if crate::actor::agent_calling() {
        return Err((
            StatusCode::FORBIDDEN,
            "the engine room is the operator's".to_string(),
        ));
    }
    Ok(())
}

/// A tool's CLI, as the node's PATH finds it.
fn cli(name: &str) -> Option<PathBuf> {
    crate::state::resolve_executable_path(name).ok()
}

/// A command's stdout, when it succeeds.
fn stdout(cmd: &mut Command) -> Option<String> {
    let out = cmd.stdin(Stdio::null()).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// A CLI's JSON as rows: an array, or one document per line.
fn rows(text: &str) -> Vec<Value> {
    match crate::containers::parse_json(text) {
        Value::Array(rows) => rows,
        one => vec![one],
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema, PartialEq)]
pub(crate) struct TailnetPeer {
    /// Its name on the tailnet.
    pub(crate) name: String,
    /// `iOS`, `android`, `linux`, `macOS`, `windows`…
    pub(crate) os: String,
    pub(crate) online: bool,
}

#[derive(Debug, Serialize, schemars::JsonSchema, PartialEq)]
pub(crate) struct TailnetStatus {
    pub(crate) connected: bool,
    /// This Mac's name on the tailnet.
    pub(crate) name: Option<String>,
    /// The other machines and phones, those online first.
    pub(crate) peers: Vec<TailnetPeer>,
}

/// A machine's tailnet name: its MagicDNS label (an iPhone's host name is
/// `localhost`), else its host name.
fn tailnet_name(node: &Value) -> Option<String> {
    node["DNSName"]
        .as_str()
        .and_then(|dns| dns.split('.').next())
        .filter(|label| !label.is_empty())
        .or(node["HostName"].as_str())
        .map(str::to_string)
}

/// The tailnet, from `tailscale status --json`.
fn tailnet(status: &Value) -> TailnetStatus {
    let mut peers: Vec<TailnetPeer> = status["Peer"]
        .as_object()
        .into_iter()
        .flat_map(|peers| peers.values())
        .map(|p| TailnetPeer {
            name: tailnet_name(p).unwrap_or_default(),
            os: p["OS"].as_str().unwrap_or_default().to_string(),
            online: p["Online"].as_bool().unwrap_or(false),
        })
        .collect();
    peers.sort_by(|a, b| (!a.online, &a.name).cmp(&(!b.online, &b.name)));
    TailnetStatus {
        connected: status["BackendState"] == "Running",
        name: tailnet_name(&status["Self"]),
        peers,
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema, PartialEq)]
pub(crate) struct ColimaVm {
    /// Its profile: `default` is the one the Bakery's Docker uses.
    pub(crate) name: String,
    pub(crate) running: bool,
    pub(crate) cpus: Option<u64>,
    /// Bytes.
    pub(crate) memory: Option<u64>,
}

/// Colima's VMs, from `colima list --json`.
fn colima_vms(rows: &[Value]) -> Vec<ColimaVm> {
    rows.iter()
        .map(|v| ColimaVm {
            name: v["name"].as_str().unwrap_or_default().to_string(),
            running: v["status"]
                .as_str()
                .is_some_and(|s| s.eq_ignore_ascii_case("running")),
            cpus: v["cpus"].as_u64(),
            memory: v["memory"].as_u64(),
        })
        .collect()
}

#[derive(Debug, Serialize, schemars::JsonSchema, PartialEq)]
pub(crate) struct BrewService {
    pub(crate) name: String,
    /// As Homebrew says: `started`, `stopped`, `error`, `none`…
    pub(crate) status: String,
}

/// Homebrew's services, from `brew services list --json`.
fn brew_services(rows: &[Value]) -> Vec<BrewService> {
    rows.iter()
        .map(|s| BrewService {
            name: s["name"].as_str().unwrap_or_default().to_string(),
            status: s["status"].as_str().unwrap_or("none").to_string(),
        })
        .collect()
}

/// Supabase's local stacks: its containers' project label, once each.
fn supabase_stacks(rows: &[Value]) -> Vec<String> {
    let mut stacks: Vec<String> = rows
        .iter()
        .filter_map(|c| {
            c["Labels"]
                .as_str()?
                .split(',')
                .find_map(|label| label.strip_prefix("com.supabase.cli.project="))
                .map(str::to_string)
        })
        .collect();
    stacks.sort();
    stacks.dedup();
    stacks
}

/// The `status` row of `container system status`'s table.
fn container_status(table: &str) -> Option<String> {
    table.lines().find_map(|line| {
        let mut words = line.split_whitespace();
        (words.next() == Some("status"))
            .then(|| words.next().map(str::to_string))
            .flatten()
    })
}

/// Apple container's system service: `running`, or what it says instead.
fn apple_container(cli: &Path) -> String {
    match Command::new(cli)
        .args(["system", "status"])
        .stdin(Stdio::null())
        .output()
    {
        Ok(out) if out.status.success() => container_status(&String::from_utf8_lossy(&out.stdout))
            .unwrap_or_else(|| "running".to_string()),
        Ok(out) => {
            let said = String::from_utf8_lossy(&out.stderr).trim().to_string();
            if said.is_empty() {
                "stopped".to_string()
            } else {
                said
            }
        }
        Err(e) => e.to_string(),
    }
}

/// Docker in Colima's default VM, as the containers tools reach it.
fn docker(args: &[&str]) -> Option<String> {
    stdout(crate::containers::cli(Backend::Docker).ok()?.args(args))
}

fn colima_running(colima: &Path) -> bool {
    Command::new(colima)
        .arg("status")
        .stdin(Stdio::null())
        .output()
        .is_ok_and(|out| out.status.success())
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct EngineRoom {
    /// Apple container's system service: `running`, or what it says
    /// instead; absent when it isn't installed.
    pub(crate) apple_container: Option<String>,
    /// Absent when Tailscale isn't installed.
    pub(crate) tailscale: Option<TailnetStatus>,
    /// Colima's VMs; absent when Colima isn't installed.
    pub(crate) colima: Option<Vec<ColimaVm>>,
    /// What Colima's default VM uses, while it runs.
    pub(crate) colima_use: Option<ColimaUse>,
    /// The most its programs held on any day of the last fortnight.
    pub(crate) colima_peak: Option<u64>,
    /// A memory size from that peak, in GiB: half again as much, at least 2.
    pub(crate) colima_suggested_gib: Option<u32>,
    /// The size the operator picked, applied at the VM's next start.
    pub(crate) colima_next_size: Option<ColimaSize>,
    /// OrbStack, once installed.
    pub(crate) orbstack: Option<OrbStack>,
    /// The Bakery's Docker engine where a project picks none: `colima` or
    /// `orbstack`.
    pub(crate) docker_default: String,
    /// Colima's idle stop is on (unless the operator turned it off).
    pub(crate) colima_idle_stop: bool,
    /// The idle stop stopped Colima's default VM: a Docker action starts it.
    pub(crate) colima_idle_stopped: bool,
    pub(crate) brew_services: Vec<BrewService>,
    /// cloudflared runs, as a Homebrew service or on its own; absent when it
    /// isn't installed.
    pub(crate) cloudflared: Option<bool>,
    /// Supabase's local stacks running in Colima, by project.
    pub(crate) supabase: Vec<String>,
}

fn engine_room(state: &AppState) -> EngineRoom {
    let base = state.base.as_path();
    let tailscale = crate::devices::tailscale()
        .and_then(|cli| stdout(Command::new(cli).args(["status", "--json"])))
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .map(|status| tailnet(&status));
    let colima_cli = cli("colima");
    let colima = colima_cli
        .as_ref()
        .and_then(|cli| stdout(Command::new(cli).args(["list", "--json"])))
        .map(|text| colima_vms(&rows(&text)));
    let default_runs = colima
        .iter()
        .flatten()
        .any(|vm| vm.name == "default" && vm.running);
    let brew_services = cli("brew")
        .and_then(|cli| stdout(Command::new(cli).args(["services", "list", "--json"])))
        .map(|text| brew_services(&rows(&text)))
        .unwrap_or_default();
    let cloudflared = cli("cloudflared").map(|_| {
        brew_services
            .iter()
            .any(|s| s.name == "cloudflared" && s.status == "started")
            || stdout(Command::new("/usr/bin/pgrep").args(["-x", "cloudflared"])).is_some()
    });
    let supabase = default_runs
        .then(|| {
            docker(&[
                "ps",
                "--filter",
                "label=com.supabase.cli.project",
                "--format",
                "json",
            ])
        })
        .flatten()
        .map(|text| supabase_stacks(&rows(&text)))
        .unwrap_or_default();
    let colima_use = colima_cli
        .as_ref()
        .filter(|_| default_runs)
        .and_then(|cli| colima_use(&crate::machine_access::home_of(state), cli));
    let colima_peak = peak(base);
    EngineRoom {
        apple_container: cli("container").map(|cli| apple_container(&cli)),
        tailscale,
        colima,
        colima_use,
        colima_peak,
        colima_suggested_gib: colima_peak.map(suggest),
        colima_next_size: next_size(base),
        orbstack: orbstack(),
        docker_default: default_engine(base).to_string(),
        colima_idle_stop: idle_stop_on(base),
        colima_idle_stopped: idle_stopped(base).exists(),
        brew_services,
        cloudflared,
        supabase,
    }
}

/// `GET /engine`: what this Mac runs for the Bakery, each as its tool says.
pub(crate) async fn engine_route(State(st): State<AppState>) -> Result<Json<EngineRoom>, Fail> {
    operator_only()?;
    crate::actor::spawn_blocking(move || engine_room(&st))
        .await
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

// ---- Colima's default VM: its use, its size, its idle stop ---------------

#[derive(Debug, Serialize, schemars::JsonSchema, PartialEq)]
pub(crate) struct ContainerUse {
    pub(crate) name: String,
    /// Bytes.
    pub(crate) memory: u64,
    pub(crate) cpu_percent: f64,
}

#[derive(Debug, Serialize, schemars::JsonSchema, PartialEq)]
pub(crate) struct ColimaUse {
    /// The VM's memory as its own Linux counts it.
    pub(crate) guest_total: u64,
    /// What its programs hold: neither free nor cache.
    pub(crate) guest_programs: u64,
    /// Its page cache, which it gives up when programs need more.
    pub(crate) guest_cache: u64,
    pub(crate) guest_free: u64,
    /// Each running container's own use.
    pub(crate) containers: Vec<ContainerUse>,
    /// What the VM holds of this Mac's memory (its process's footprint), which
    /// only a stop gives back.
    pub(crate) on_mac: Option<u64>,
}

/// The guest's own count (`/proc/meminfo`): total, programs, cache, free.
fn meminfo(text: &str) -> Option<(u64, u64, u64, u64)> {
    let kib = |key: &str| {
        text.lines()
            .find_map(|l| {
                l.strip_prefix(key)?
                    .trim()
                    .strip_suffix("kB")?
                    .trim()
                    .parse::<u64>()
                    .ok()
            })
            .map(|k| k * 1024)
    };
    let (total, free) = (kib("MemTotal:")?, kib("MemFree:")?);
    let cache = kib("Buffers:").unwrap_or(0)
        + kib("Cached:").unwrap_or(0)
        + kib("SReclaimable:").unwrap_or(0);
    Some((total, total.saturating_sub(free + cache), cache, free))
}

/// Sizes as Docker's stats and macOS's footprint print them: `12.3MiB`,
/// `1.2GiB`, `7161 MB`.
fn size_bytes(said: &str) -> Option<u64> {
    let said = said.trim();
    let digits = said.find(|c: char| c.is_ascii_alphabetic())?;
    let number: f64 = said[..digits].trim().parse().ok()?;
    let scale = match said[digits..].trim() {
        "B" => 1.0,
        "KiB" | "KB" | "kB" => 1024.0,
        "MiB" | "MB" => 1024.0 * 1024.0,
        "GiB" | "GB" => GIB as f64,
        _ => return None,
    };
    Some((number * scale) as u64)
}

/// `docker stats --no-stream --format json`, one line a container.
fn container_use(rows: &[Value]) -> Vec<ContainerUse> {
    rows.iter()
        .map(|r| ContainerUse {
            name: r["Name"].as_str().unwrap_or_default().to_string(),
            memory: r["MemUsage"]
                .as_str()
                .and_then(|m| size_bytes(m.split('/').next().unwrap_or_default()))
                .unwrap_or(0),
            cpu_percent: r["CPUPerc"]
                .as_str()
                .and_then(|c| c.trim_end_matches('%').parse().ok())
                .unwrap_or(0.0),
        })
        .collect()
}

/// `footprint -p <pid>`'s `Footprint: 7161 MB`.
fn footprint_bytes(text: &str) -> Option<u64> {
    let after = text.split("Footprint:").nth(1)?;
    size_bytes(after.split('(').next()?)
}

/// What Colima's default VM uses: inside, per container, and of this Mac.
fn colima_use(home: &Path, colima: &Path) -> Option<ColimaUse> {
    let text = stdout(Command::new(colima).args(["ssh", "--", "cat", "/proc/meminfo"]))?;
    let (guest_total, guest_programs, guest_cache, guest_free) = meminfo(&text)?;
    let containers = docker(&["stats", "--no-stream", "--format", "json"])
        .map(|text| container_use(&rows(&text)))
        .unwrap_or_default();
    // The VM's own process (Virtualization's, not Lima's host agent): the
    // one holding its disk open (`colima` is the default profile's
    // instance).
    let disk = home.join(".colima/_lima/colima/disk");
    let on_mac = stdout(Command::new("/usr/sbin/lsof").arg("-t").arg(&disk))
        .and_then(|pids| pids.lines().next().map(str::to_string))
        .and_then(|pid| stdout(Command::new("/usr/bin/footprint").args(["-p", pid.trim()])))
        .and_then(|text| footprint_bytes(&text));
    Some(ColimaUse {
        guest_total,
        guest_programs,
        guest_cache,
        guest_free,
        containers,
        on_mac,
    })
}

fn dir(base: &Path) -> PathBuf {
    base.join("engine")
}

/// Each day's peak of what the VM's programs held, the last fortnight.
fn peaks_file(base: &Path) -> PathBuf {
    dir(base).join("colima-peaks.json")
}

fn peaks(base: &Path) -> BTreeMap<String, u64> {
    std::fs::read_to_string(peaks_file(base))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn record_peak(base: &Path, day: &str, programs: u64) {
    let mut peaks = peaks(base);
    let today = peaks.entry(day.to_string()).or_insert(0);
    *today = (*today).max(programs);
    while peaks.len() > 14 {
        peaks.pop_first();
    }
    let _ = std::fs::create_dir_all(dir(base)).and_then(|()| {
        std::fs::write(
            peaks_file(base),
            serde_json::to_string(&peaks).unwrap_or_default(),
        )
    });
}

fn peak(base: &Path) -> Option<u64> {
    peaks(base).into_values().max()
}

/// A memory size from a peak: half again as much, in whole GiB, at least 2.
// ponytail: one rule of thumb; the operator picks the size either way.
fn suggest(peak: u64) -> u32 {
    ((peak as f64 * 1.5 / GIB as f64).ceil() as u32).max(2)
}

/// The size the operator picked for Colima's default VM.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, schemars::JsonSchema, PartialEq)]
pub(crate) struct ColimaSize {
    pub(crate) memory_gib: u32,
    /// None keeps its CPUs.
    #[serde(default)]
    pub(crate) cpus: Option<u32>,
}

/// Present while a size waits for the VM's next start.
fn size_file(base: &Path) -> PathBuf {
    dir(base).join("colima-next-size.json")
}

fn next_size(base: &Path) -> Option<ColimaSize> {
    std::fs::read_to_string(size_file(base))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct ColimaSizeReq {
    /// The size for the VM's next start; none takes back a size not yet
    /// applied.
    pub(crate) size: Option<ColimaSize>,
}

/// `POST /engine/colima/size`: the operator picks the size Colima's default
/// VM starts with next (the idle stop's next start applies it); it answers
/// the size waiting.
pub(crate) async fn size_route(
    State(st): State<AppState>,
    Json(req): Json<ColimaSizeReq>,
) -> Result<Json<Option<ColimaSize>>, Fail> {
    operator_only()?;
    let file = size_file(&st.base);
    let set = match req.size {
        Some(size) if size.memory_gib < 1 || size.cpus == Some(0) => {
            return Err((
                StatusCode::BAD_REQUEST,
                "a VM needs at least 1 GiB and 1 CPU".to_string(),
            ))
        }
        Some(size) => std::fs::create_dir_all(dir(&st.base))
            .and_then(|()| std::fs::write(&file, serde_json::to_string(&size).unwrap_or_default())),
        None => match std::fs::remove_file(&file) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        },
    };
    set.map(|()| Json(next_size(&st.base))).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("{}: {e}", file.display()),
        )
    })
}

/// Present when the operator turned Colima's idle stop off; it's on
/// otherwise.
fn idle_stop_off(base: &Path) -> PathBuf {
    dir(base).join("colima-idle-stop-off")
}

fn idle_stop_on(base: &Path) -> bool {
    !idle_stop_off(base).exists()
}

/// Present while the idle stop has Colima's default VM stopped. A VM the
/// operator stopped themselves stays as they left it.
fn idle_stopped(base: &Path) -> PathBuf {
    dir(base).join("colima-idle-stopped")
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct IdleStopReq {
    pub(crate) on: bool,
}

/// `POST /engine/colima/idle-stop`: the operator turns Colima's idle stop on
/// or off; it answers the setting.
pub(crate) async fn idle_stop_route(
    State(st): State<AppState>,
    Json(req): Json<IdleStopReq>,
) -> Result<Json<bool>, Fail> {
    operator_only()?;
    let flag = idle_stop_off(&st.base);
    let set = if req.on {
        match std::fs::remove_file(&flag) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    } else {
        std::fs::create_dir_all(dir(&st.base)).and_then(|()| std::fs::write(&flag, ""))
    };
    set.map(|()| Json(idle_stop_on(&st.base))).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("{}: {e}", flag.display()),
        )
    })
}

/// How often Colima's default VM is looked at, and how many looks in a row
/// must find it idle before the idle stop stops it.
const LOOK: Duration = Duration::from_secs(180);
const IDLE_LOOKS: u32 = 3;

/// One change to Colima's default VM at a time: the idle stop's stop, or a
/// Docker request's start.
static CHANGING: Mutex<()> = Mutex::new(());
/// When the Bakery last ran Docker for someone, in seconds since 1970.
static USED: AtomicU64 = AtomicU64::new(0);

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The Bakery ran Docker for someone within the last look.
fn used_lately() -> bool {
    now_secs().saturating_sub(USED.load(Ordering::Relaxed)) < LOOK.as_secs()
}

/// No container runs, no Docker command is under way (a build, a pull, a
/// compose, Supabase's CLI) and the Bakery ran none lately. Unsure is busy.
fn docker_idle() -> bool {
    // ponytail: Docker clients named here only; one that talks to the socket
    // itself (a dev container, testcontainers) mid-pull goes unseen. Count
    // the socket's connections if that bites.
    let docker_at_work = ["docker", "docker-compose", "supabase"]
        .iter()
        .any(|name| stdout(Command::new("/usr/bin/pgrep").args(["-x", name])).is_some());
    !used_lately()
        && !docker_at_work
        && docker(&["ps", "--quiet"]).is_some_and(|ids| ids.trim().is_empty())
}

/// Stops Colima's default VM, marked first so a Docker action starts it;
/// not if a Docker request came meanwhile.
fn stop_colima(base: &Path, colima: &Path) {
    let _one_at_a_time = CHANGING.lock().unwrap_or_else(|e| e.into_inner());
    if used_lately() {
        return;
    }
    let marked =
        std::fs::create_dir_all(dir(base)).and_then(|()| std::fs::write(idle_stopped(base), ""));
    if marked.is_err() {
        return;
    }
    let stopped = Command::new(colima)
        .arg("stop")
        .stdin(Stdio::null())
        .output()
        .is_ok_and(|out| out.status.success());
    if !stopped {
        let _ = std::fs::remove_file(idle_stopped(base));
    }
}

/// From now on, a look at Colima's default VM every few minutes while it
/// runs: the day's peak of what its programs hold is kept, and with the
/// idle stop on, it stops once it has sat idle at [`IDLE_LOOKS`] looks in a
/// row.
pub(crate) fn watch(state: AppState) {
    std::thread::spawn(move || {
        let mut idle_looks = 0;
        loop {
            std::thread::sleep(LOOK);
            let Some(colima) = cli("colima").filter(|c| colima_running(c)) else {
                idle_looks = 0;
                continue;
            };
            let home = crate::machine_access::home_of(&state);
            if let Some(used) = colima_use(&home, &colima) {
                let day = chrono::Local::now().format("%Y-%m-%d").to_string();
                record_peak(&state.base, &day, used.guest_programs);
            }
            idle_looks = if idle_stop_on(&state.base) && docker_idle() {
                idle_looks + 1
            } else {
                0
            };
            if idle_looks >= IDLE_LOOKS {
                stop_colima(&state.base, &colima);
                idle_looks = 0;
            }
        }
    });
}

/// `colima start`'s words: at the size the operator picked, if they did.
fn start_args(size: Option<ColimaSize>) -> Vec<String> {
    let mut args = vec!["start".to_string()];
    if let Some(size) = size {
        args.extend(["--memory".to_string(), size.memory_gib.to_string()]);
        if let Some(cpus) = size.cpus {
            args.extend(["--cpus".to_string(), cpus.to_string()]);
        }
    }
    args
}

/// Before the Bakery runs Docker for someone (any request, a list too): a
/// use, which holds the idle stop off, and Colima's default VM started again
/// if the idle stop stopped it, at the size the operator picked. A VM the
/// operator stopped stays stopped.
pub(crate) fn before_docker(base: &Path) -> Result<(), Fail> {
    let _one_at_a_time = CHANGING.lock().unwrap_or_else(|e| e.into_inner());
    USED.store(now_secs(), Ordering::Relaxed);
    if !idle_stopped(base).exists() {
        return Ok(());
    }
    let colima = cli("colima").ok_or((
        StatusCode::CONFLICT,
        "colima is not installed on this node's PATH".to_string(),
    ))?;
    if !colima_running(&colima) {
        let size = next_size(base);
        let out = Command::new(&colima)
            .args(start_args(size))
            .stdin(Stdio::null())
            .output()
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        if !out.status.success() {
            return Err((
                StatusCode::BAD_GATEWAY,
                format!(
                    "Colima didn't start: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ),
            ));
        }
        if size.is_some() {
            let _ = std::fs::remove_file(size_file(base));
        }
    }
    let _ = std::fs::remove_file(idle_stopped(base));
    Ok(())
}

// ---- OrbStack, the default Docker engine once installed ------------------

const ORBSTACK_APP: &str = "/Applications/OrbStack.app";

pub(crate) fn orbstack_installed() -> bool {
    Path::new(ORBSTACK_APP).is_dir()
}

/// How long OrbStack has to answer once started.
const ORBSTACK_START: Duration = Duration::from_secs(60);

/// OrbStack's Docker answers, through its own context.
pub(crate) fn orbstack_answers() -> bool {
    crate::containers::cli(Backend::Orbstack).is_ok_and(|mut docker| {
        docker
            .args(["info", "--format", "{{.ID}}"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    })
}

/// Before an action that needs OrbStack (a run, a start, a compose project
/// brought up; never a read, so an OrbStack the operator quit stays quit
/// until they ask for something to run): OrbStack started if it's installed
/// but stopped, never Colima instead, so a project's images and data stay
/// with one engine (the operator's answer). A stopped OrbStack
/// refuses Docker's connection rather than starting by itself; `orb start`
/// starts it without its window (OrbStack's docs), and the app, opened in
/// the background, when its CLI isn't on the node's PATH.
pub(crate) fn before_orbstack() -> Result<(), Fail> {
    static STARTING: Mutex<()> = Mutex::new(());
    let _one_at_a_time = STARTING.lock().unwrap_or_else(|e| e.into_inner());
    if orbstack_answers() {
        return Ok(());
    }
    if !orbstack_installed() {
        return Err((StatusCode::CONFLICT, "OrbStack isn't installed".to_string()));
    }
    let start = match cli("orb") {
        Some(orb) => Command::new(orb).arg("start").stdin(Stdio::null()).output(),
        None => Command::new("/usr/bin/open").args(["-g", "-a", ORBSTACK_APP]).stdin(Stdio::null()).output(),
    }
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    if !start.status.success() {
        return Err((
            StatusCode::BAD_GATEWAY,
            format!("OrbStack didn't start: {}", String::from_utf8_lossy(&start.stderr).trim()),
        ));
    }
    let until = std::time::Instant::now() + ORBSTACK_START;
    while std::time::Instant::now() < until {
        if orbstack_answers() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    Err((
        StatusCode::GATEWAY_TIMEOUT,
        format!("OrbStack started, but its Docker didn't answer within {} s", ORBSTACK_START.as_secs()),
    ))
}

/// Where the engines a project ran something on are noted: one empty file
/// an engine, `engine/used/<project>/<engine>`.
fn used_dir(base: &Path, project_id: &str) -> PathBuf {
    dir(base).join("used").join(project_id)
}

fn engine_name(backend: Backend) -> &'static str {
    match backend {
        Backend::Apple => "apple",
        Backend::Docker => "colima",
        Backend::Orbstack => "orbstack",
    }
}

/// Notes that `project_id` ran something on `backend` (its network was
/// made or found there), so deleting it clears that engine.
pub(crate) fn note_project_use(base: &Path, project_id: &str, backend: Backend) {
    let at = used_dir(base, project_id);
    let _ = std::fs::create_dir_all(&at).and_then(|()| std::fs::write(at.join(engine_name(backend)), ""));
}

/// The engines `project_id` is noted to have run something on.
pub(crate) fn project_used(base: &Path, project_id: &str) -> Vec<Backend> {
    let at = used_dir(base, project_id);
    [Backend::Apple, Backend::Docker, Backend::Orbstack]
        .into_iter()
        .filter(|b| at.join(engine_name(*b)).exists())
        .collect()
}

/// The command's own words when it fails.
fn ran(cmd: &mut Command) -> Result<(), String> {
    let out = cmd.stdin(Stdio::null()).output().map_err(|e| e.to_string())?;
    match out.status.success() {
        true => Ok(()),
        false => Err(String::from_utf8_lossy(&out.stderr).trim().to_string()),
    }
}

/// Starts an engine that is set up but off, for a delete to clear what it
/// holds: Apple container's system service, Colima's default VM (at the
/// size the operator picked), OrbStack. Answers whether Colima's idle stop
/// had stopped it, which [`stop_after_delete`] puts back as it was.
pub(crate) fn start_for_delete(base: &Path, backend: Backend) -> Result<bool, String> {
    match backend {
        Backend::Apple => {
            let container = cli("container").ok_or("container is not installed on this node's PATH")?;
            ran(Command::new(container).args(["system", "start"])).map(|()| false)
        }
        Backend::Docker => {
            let _one_at_a_time = CHANGING.lock().unwrap_or_else(|e| e.into_inner());
            let colima = cli("colima").ok_or("colima is not installed on this node's PATH")?;
            let idle = idle_stopped(base).exists();
            let size = next_size(base);
            ran(Command::new(colima).args(start_args(size)))?;
            if size.is_some() {
                let _ = std::fs::remove_file(size_file(base));
            }
            Ok(idle)
        }
        Backend::Orbstack => before_orbstack().map(|()| false).map_err(|(_, e)| e),
    }
}

/// Stops an engine [`start_for_delete`] started: off again, as it was
/// before; a Colima the idle stop had stopped starts again for the next
/// Docker request, as before.
pub(crate) fn stop_after_delete(base: &Path, backend: Backend, idle_stopped_before: bool) -> Result<(), String> {
    match backend {
        Backend::Apple => {
            let container = cli("container").ok_or("container is not installed on this node's PATH")?;
            ran(Command::new(container).args(["system", "stop"]))
        }
        Backend::Docker => {
            let _one_at_a_time = CHANGING.lock().unwrap_or_else(|e| e.into_inner());
            let colima = cli("colima").ok_or("colima is not installed on this node's PATH")?;
            ran(Command::new(colima).arg("stop"))?;
            if idle_stopped_before {
                std::fs::create_dir_all(dir(base))
                    .and_then(|()| std::fs::write(idle_stopped(base), ""))
                    .map_err(|e| e.to_string())?;
            }
            Ok(())
        }
        Backend::Orbstack => match cli("orb") {
            Some(orb) => ran(Command::new(orb).arg("stop")),
            None => ran(Command::new("/usr/bin/osascript").args(["-e", "quit app \"OrbStack\""])),
        },
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema, PartialEq)]
pub(crate) struct OrbStack {
    /// Its Docker answers, through its `orbstack` context.
    pub(crate) running: bool,
    /// The memory and CPUs its Docker sees: OrbStack grows into that memory
    /// as its containers need it and gives back to macOS what they free.
    pub(crate) memory: Option<u64>,
    pub(crate) cpus: Option<u64>,
    /// Each running container's own use.
    pub(crate) containers: Vec<ContainerUse>,
    /// What OrbStack holds of this Mac's memory now.
    pub(crate) on_mac: Option<u64>,
}

/// OrbStack, as its own Docker reports it; none when it isn't installed.
fn orbstack() -> Option<OrbStack> {
    if !orbstack_installed() {
        return None;
    }
    let docker = |args: &[&str]| stdout(crate::containers::cli(Backend::Orbstack).ok()?.args(args));
    let info = docker(&["info", "--format", "json"])
        .and_then(|text| serde_json::from_str::<Value>(&text).ok());
    let containers = info
        .as_ref()
        .and_then(|_| docker(&["stats", "--no-stream", "--format", "json"]))
        .map(|text| container_use(&rows(&text)))
        .unwrap_or_default();
    Some(OrbStack {
        running: info.is_some(),
        memory: info.as_ref().and_then(|i| i["MemTotal"].as_u64()),
        cpus: info.as_ref().and_then(|i| i["NCPU"].as_u64()),
        containers,
        on_mac: orbstack_footprint(),
    })
}

/// The footprint of OrbStack's own processes and of any Virtualization
/// process holding an OrbStack file open, summed.
// ponytail: matched by name, not yet seen on a Mac with OrbStack installed;
// check the match against a real one on its first look.
fn orbstack_footprint() -> Option<u64> {
    let pids = |pattern: &str| {
        stdout(Command::new("/usr/bin/pgrep").args(["-f", pattern]))
            .unwrap_or_default()
            .lines()
            .map(|pid| pid.trim().to_string())
            .collect::<Vec<_>>()
    };
    let mut own = pids("OrbStack");
    for pid in pids("com.apple.Virtualization.VirtualMachine") {
        let files =
            stdout(Command::new("/usr/sbin/lsof").args(["-Fn", "-p", &pid])).unwrap_or_default();
        if files.to_lowercase().contains("orbstack") {
            own.push(pid);
        }
    }
    own.sort();
    own.dedup();
    let total: u64 = own
        .iter()
        .filter_map(|pid| stdout(Command::new("/usr/bin/footprint").args(["-p", pid])))
        .filter_map(|text| footprint_bytes(&text))
        .sum();
    (total > 0).then_some(total)
}

/// The operator's pick of the Bakery's Docker engine, when they made one.
fn default_file(base: &Path) -> PathBuf {
    dir(base).join("docker-default")
}

/// The engine a pick comes to (`None`: no pick), as the operator decided:
/// Colima when picked; else OrbStack once it's installed; Colima while it
/// isn't, a pick of OrbStack included.
fn engine_of(pick: Option<&str>, orbstack_installed: bool) -> &'static str {
    match (pick, orbstack_installed) {
        (Some("colima"), _) => "colima",
        (_, true) => "orbstack",
        (_, false) => "colima",
    }
}

/// The Bakery's Docker engine where a project picks none.
fn default_engine(base: &Path) -> &'static str {
    let pick = std::fs::read_to_string(default_file(base)).ok();
    engine_of(pick.as_deref().map(str::trim), orbstack_installed())
}

/// A thread's Docker: its project's pick, else the Bakery's default.
pub(crate) fn docker_for(state: &AppState, stream_id: &str) -> Backend {
    docker_of(state, &crate::catalog::stacks_for_socket(state, stream_id).containers)
}

/// The Docker a "Services run in" pick comes to: the one it names, else
/// the Bakery's default.
pub(crate) fn docker_of(state: &AppState, pick: &str) -> Backend {
    let engine = match pick {
        "colima" | "orbstack" => engine_of(Some(pick), orbstack_installed()),
        _ => default_engine(&state.base),
    };
    if engine == "orbstack" {
        Backend::Orbstack
    } else {
        Backend::Docker
    }
}

fn shown(engine: &str) -> &'static str {
    if engine == "orbstack" {
        "OrbStack"
    } else {
        "Colima"
    }
}

/// What moving Docker work from one engine to another means for what's
/// already there, for the app to say on any switch.
pub(crate) fn switch_note(from: &str, to: &str) -> String {
    format!(
        "Images and volumes stay with the engine that has them: on {}, images are pulled or built again, and volumes, with their data, stay on {} until moved.",
        shown(to),
        shown(from)
    )
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct DockerDefaultReq {
    /// `colima` or `orbstack`.
    pub(crate) engine: String,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct DockerDefault {
    pub(crate) engine: String,
    /// What the switch means for what's already there; none when nothing
    /// changed.
    pub(crate) note: Option<String>,
}

/// `POST /engine/docker-default`: the operator picks the Bakery's Docker
/// engine for projects that pick none.
pub(crate) async fn docker_default_route(
    State(st): State<AppState>,
    Json(req): Json<DockerDefaultReq>,
) -> Result<Json<DockerDefault>, Fail> {
    operator_only()?;
    let engine = match req.engine.as_str() {
        "colima" => "colima",
        "orbstack" => "orbstack",
        other => {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("{other:?}: colima or orbstack"),
            ))
        }
    };
    if engine == "orbstack" && !orbstack_installed() {
        return Err((
            StatusCode::CONFLICT,
            "OrbStack isn't installed: install it first".to_string(),
        ));
    }
    let from = default_engine(&st.base);
    let file = default_file(&st.base);
    std::fs::create_dir_all(dir(&st.base))
        .and_then(|()| std::fs::write(&file, engine))
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("{}: {e}", file.display()),
            )
        })?;
    Ok(Json(DockerDefault {
        engine: engine.to_string(),
        note: (from != engine).then(|| switch_note(from, engine)),
    }))
}

/// `POST /engine/orbstack/install`: the operator's yes to installing
/// OrbStack (Homebrew's cask). It installs in the background, and the app
/// hears how it went as a `{"kind":"engine-install"}` envelope.
pub(crate) async fn orbstack_install_route(
    State(st): State<AppState>,
) -> Result<(StatusCode, Json<String>), Fail> {
    use std::sync::atomic::{AtomicBool, Ordering};
    static INSTALLING: AtomicBool = AtomicBool::new(false);
    operator_only()?;
    if orbstack_installed() {
        return Err((StatusCode::CONFLICT, "OrbStack is installed".to_string()));
    }
    let brew = cli("brew").ok_or((StatusCode::CONFLICT, "Homebrew isn't installed".to_string()))?;
    if INSTALLING.swap(true, Ordering::SeqCst) {
        return Err((StatusCode::CONFLICT, "OrbStack is installing".to_string()));
    }
    let tell = |state: &AppState, status: &str, said: &str| {
        let envelope = serde_json::json!({ "kind": "engine-install", "engine": "orbstack", "status": status, "said": said });
        let _ = state.tx.send(envelope.to_string());
    };
    tell(&st, "installing", "");
    std::thread::spawn(move || {
        let out = Command::new(brew)
            .args(["install", "--cask", "orbstack"])
            .stdin(Stdio::null())
            .output();
        INSTALLING.store(false, Ordering::SeqCst);
        match out {
            Ok(out) if out.status.success() => tell(&st, "installed", ""),
            Ok(out) => {
                let said = String::from_utf8_lossy(&out.stderr);
                let last: Vec<&str> = said.lines().rev().take(5).collect();
                tell(
                    &st,
                    "failed",
                    &last.into_iter().rev().collect::<Vec<_>>().join("\n"),
                );
            }
            Err(e) => tell(&st, "failed", &e.to_string()),
        }
    });
    Ok((StatusCode::ACCEPTED, Json("installing".to_string())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_tool_is_read_as_it_reports_itself() {
        let status: Value = serde_json::from_str(
            r#"{"BackendState":"Running","Self":{"HostName":"work-laptop","DNSName":"work-laptop.tail.ts.net."},"Peer":{
              "a":{"HostName":"localhost","DNSName":"my-phone.tail.ts.net.","OS":"iOS","Online":true},
              "b":{"HostName":"server","OS":"linux","Online":false},
              "c":{"HostName":"build-mini","DNSName":"build-mini.tail.ts.net.","OS":"macOS","Online":true}}}"#,
        )
        .unwrap();
        let net = tailnet(&status);
        assert!(net.connected);
        assert_eq!(net.name.as_deref(), Some("work-laptop"));
        let peers: Vec<(&str, &str, bool)> = net
            .peers
            .iter()
            .map(|p| (p.name.as_str(), p.os.as_str(), p.online))
            .collect();
        assert_eq!(
            peers,
            [
                ("build-mini", "macOS", true),
                ("my-phone", "iOS", true),
                ("server", "linux", false),
            ],
            "online first, the iPhone by its tailnet name"
        );

        let vms = colima_vms(&rows(
            "{\"name\":\"default\",\"status\":\"Running\",\"cpus\":8,\"memory\":25769803776}\n{\"name\":\"lagtest\",\"status\":\"Stopped\",\"cpus\":4}",
        ));
        assert_eq!(
            vms.iter()
                .map(|v| (v.name.as_str(), v.running))
                .collect::<Vec<_>>(),
            [("default", true), ("lagtest", false)]
        );
        assert_eq!(vms[0].memory, Some(25769803776));
        let one = colima_vms(&rows(r#"{"name":"default","status":"Stopped"}"#));
        assert_eq!(one.len(), 1, "one profile is one row");

        let services = brew_services(&rows(
            r#"[{"name":"redis","status":"started"},{"name":"cloudflared","status":"none"}]"#,
        ));
        assert_eq!(
            services[0],
            BrewService {
                name: "redis".into(),
                status: "started".into()
            }
        );
        let stacks = supabase_stacks(&rows(
            "{\"Labels\":\"com.docker.compose.project=farm,com.supabase.cli.project=farm\"}\n{\"Labels\":\"com.supabase.cli.project=farm\"}\n{\"Labels\":\"other=1\"}",
        ));
        assert_eq!(stacks, ["farm"]);
        assert_eq!(
            container_status("FIELD   VALUE\nstatus  running\nclient.version  1.5.0").as_deref(),
            Some("running")
        );
    }

    /// On unless the operator turns it off; a size waits for the next start
    /// the idle stop makes; a Docker action leaves a VM the idle stop never
    /// stopped alone (no Colima command runs here).
    #[tokio::test]
    async fn colimas_idle_stop_is_on_and_a_new_size_waits_for_its_next_start() {
        let dir = tempfile::tempdir().unwrap();
        let state =
            crate::testutil::state_with(dir.path(), std::sync::Arc::new(bakery_agent::StubModel));
        assert!(idle_stop_on(dir.path()), "on unless turned off");
        let set = |on| idle_stop_route(State(state.clone()), Json(IdleStopReq { on }));
        assert!(!set(false).await.unwrap().0);
        assert!(!idle_stop_on(dir.path()));
        assert!(set(true).await.unwrap().0);
        assert!(set(true).await.unwrap().0, "on twice is still on");

        let size = ColimaSize {
            memory_gib: 8,
            cpus: Some(4),
        };
        let pick = |size| size_route(State(state.clone()), Json(ColimaSizeReq { size }));
        assert_eq!(pick(Some(size)).await.unwrap().0, Some(size));
        assert_eq!(
            start_args(next_size(dir.path())),
            ["start", "--memory", "8", "--cpus", "4"]
        );
        let none = ColimaSize {
            memory_gib: 0,
            cpus: None,
        };
        assert_eq!(
            pick(Some(none)).await.unwrap_err().0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(pick(None).await.unwrap().0, None, "taken back");
        assert_eq!(start_args(None), ["start"]);
        assert!(before_docker(dir.path()).is_ok());
    }

    /// A Docker request holds the idle stop off: within a look of one,
    /// Colima's default VM is neither stopped nor marked.
    #[test]
    fn a_docker_request_holds_the_idle_stop_off() {
        let dir = tempfile::tempdir().unwrap();
        before_docker(dir.path()).unwrap();
        assert!(used_lately());
        stop_colima(dir.path(), Path::new("/nonexistent/colima"));
        assert!(!idle_stopped(dir.path()).exists());
    }

    #[test]
    fn colimas_use_is_read_as_each_tool_counts_it() {
        let text = "MemTotal:       24556980 kB\nMemFree:        18939760 kB\nMemAvailable:   23902328 kB\nBuffers:          393992 kB\nCached:          4576956 kB\nSReclaimable:     312856 kB\n";
        let (total, programs, cache, free) = meminfo(text).unwrap();
        assert_eq!((total, free), (24556980 * 1024, 18939760 * 1024));
        assert_eq!(cache, (393992 + 4576956 + 312856) * 1024);
        assert_eq!(
            programs,
            (24556980 - 18939760 - 393992 - 4576956 - 312856) * 1024
        );
        let stats = container_use(&rows(
            r#"{"Name":"db","MemUsage":"512MiB / 23.42GiB","CPUPerc":"1.25%"}"#,
        ));
        assert_eq!(
            stats[0],
            ContainerUse {
                name: "db".into(),
                memory: 512 << 20,
                cpu_percent: 1.25
            }
        );
        assert_eq!(footprint_bytes("com.apple.Virtualization.VirtualMachine [94844]: 64-bit    Footprint: 7161 MB (16384 bytes per page)"), Some(7161 << 20));

        let dir = tempfile::tempdir().unwrap();
        assert_eq!(peak(dir.path()), None);
        record_peak(dir.path(), "2026-10-10", 3 * GIB);
        record_peak(dir.path(), "2026-10-11", GIB);
        record_peak(dir.path(), "2026-10-11", 2 * GIB);
        assert_eq!(
            peaks(dir.path())["2026-10-11"],
            2 * GIB,
            "a day keeps its highest"
        );
        assert_eq!(peak(dir.path()), Some(3 * GIB));
        for day in 1..=20 {
            record_peak(dir.path(), &format!("2026-11-{day:02}"), GIB);
        }
        assert_eq!(peaks(dir.path()).len(), 14, "the last fortnight");
        assert_eq!(peak(dir.path()), Some(GIB), "older peaks go");
        assert_eq!(
            (suggest(GIB / 2), suggest(3 * GIB), suggest(5 * GIB)),
            (2, 5, 8)
        );
    }

    /// The Bakery's default engine, each project's own pick over it, and
    /// what a switch says (OrbStack isn't installed on every Mac this runs).
    #[tokio::test]
    async fn a_projects_docker_is_its_own_pick_else_the_bakerys_default() {
        // The operator's decision: OrbStack once installed, unless Colima
        // is picked; Colima while it isn't, a pick of OrbStack included.
        assert_eq!(engine_of(None, true), "orbstack");
        assert_eq!(engine_of(None, false), "colima");
        assert_eq!(engine_of(Some("colima"), true), "colima");
        assert_eq!(engine_of(Some("orbstack"), true), "orbstack");
        assert_eq!(engine_of(Some("orbstack"), false), "colima");
        if !orbstack_installed() {
            assert_eq!(before_orbstack().unwrap_err().0, StatusCode::CONFLICT, "nothing to start");
        }

        let dir = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let state =
            crate::testutil::state_with(dir.path(), std::sync::Arc::new(bakery_agent::StubModel));
        let root = crate::catalog::DirectoryRootSpec::Attach(work.path().to_str().unwrap());
        let (project, thread) = crate::catalog::create_project(&state, "farm", Some(root)).unwrap();
        let orbstack_or_colima = if orbstack_installed() { Backend::Orbstack } else { Backend::Docker };
        assert_eq!(default_engine(dir.path()), engine_of(None, orbstack_installed()));
        assert_eq!(docker_for(&state, &thread.stream_id), orbstack_or_colima);
        let stacks = crate::node::ProjectStacks {
            containers: "orbstack".into(),
            ..Default::default()
        };
        crate::catalog::set_project_stacks(&state, &project.project_id, stacks).unwrap();
        assert_eq!(
            docker_for(&state, &thread.stream_id),
            orbstack_or_colima,
            "its own pick, Colima's while OrbStack isn't installed"
        );

        let pick = |engine: &str| {
            docker_default_route(
                State(state.clone()),
                Json(DockerDefaultReq {
                    engine: engine.into(),
                }),
            )
        };
        assert_eq!(pick("podman").await.unwrap_err().0, StatusCode::BAD_REQUEST);
        if !orbstack_installed() {
            assert_eq!(
                pick("orbstack").await.unwrap_err().0,
                StatusCode::CONFLICT,
                "installed first"
            );
        }
        let before = default_engine(dir.path());
        let colima = pick("colima").await.unwrap().0;
        assert_eq!(colima.engine, "colima");
        assert_eq!(colima.note.is_some(), before != "colima", "a note only when it changed");
        assert_eq!(default_engine(dir.path()), "colima", "the operator's pick holds");
        assert_eq!(
            switch_note("colima", "orbstack"),
            "Images and volumes stay with the engine that has them: on OrbStack, images are pulled or built again, and volumes, with their data, stay on Colima until moved."
        );
    }
}
