//! This Mac's own tools, as the operator decided, rather than rebuilding
//! them: Amphetamine (their keep-awake app, closed lid included), driven
//! from the Bakery through its own scripting as the operator's own actions;
//! Macs Fan Control, which has no way in but its window, opened from it; and
//! the heat guard: this Mac's thermal state, temperatures and fans, read
//! without admin, with agents' new turns held while it runs hot; and the
//! meters the app's resource pill shows (disk, memory, CPU, heat), each
//! with its level, a few minutes of history, and word to the app whenever a
//! level changes.

use crate::state::AppState;
use axum::{http::StatusCode, Json};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

type Fail = (StatusCode, String);

fn operator_only() -> Result<(), Fail> {
    if crate::actor::agent_calling() {
        return Err((
            StatusCode::FORBIDDEN,
            "this Mac's own tools are the operator's".to_string(),
        ));
    }
    Ok(())
}

fn osascript(args: &[&str]) -> Result<String, String> {
    let out = std::process::Command::new("/usr/bin/osascript")
        .args(args)
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

// ---- Amphetamine ---------------------------------------------------------

const AMPHETAMINE: &str = "/Applications/Amphetamine.app";

/// Amphetamine as it stands.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(crate) struct Amphetamine {
    pub(crate) installed: bool,
    /// A session keeps this Mac awake.
    pub(crate) active: bool,
    /// Seconds left in a timed session; none for one with no end.
    pub(crate) seconds_left: Option<u64>,
    /// Closed-display mode: awake with the lid shut.
    pub(crate) closed_display: bool,
}

/// What Amphetamine's scripting says: its session, the time left, and
/// closed-display mode, as one line (`true, 0, false`). The time left is
/// negative with no timed session (-1 for one with no end).
fn amphetamine_read(said: &str) -> Option<Amphetamine> {
    let parts: Vec<&str> = said.split(',').map(str::trim).collect();
    let [active, left, closed] = parts.as_slice() else {
        return None;
    };
    let left: i64 = left.parse().ok()?;
    Some(Amphetamine {
        installed: true,
        active: *active == "true",
        seconds_left: u64::try_from(left).ok().filter(|&left| left > 0),
        closed_display: *closed == "true",
    })
}

fn amphetamine_status() -> Result<Amphetamine, String> {
    if !std::path::Path::new(AMPHETAMINE).is_dir() {
        return Ok(Amphetamine {
            installed: false,
            active: false,
            seconds_left: None,
            closed_display: false,
        });
    }
    let said = osascript(&[
        "-e",
        r#"tell application "Amphetamine" to return {session is active, session time remaining, closed display mode enabled}"#,
    ])?;
    amphetamine_read(&said).ok_or_else(|| format!("Amphetamine said {said:?}"))
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct AmphetamineReq {
    /// `start`, `end`, `closed_display_on` or `closed_display_off`.
    pub(crate) action: String,
    /// For `start`: minutes; none, or 0, for a session with no end.
    #[serde(default)]
    pub(crate) minutes: Option<u32>,
}

/// The AppleScript an action is, in Amphetamine's own words.
fn amphetamine_script(req: &AmphetamineReq) -> Result<String, String> {
    let tell = |what: &str| format!(r#"tell application "Amphetamine" to {what}"#);
    match req.action.as_str() {
        "start" => {
            let minutes = req.minutes.unwrap_or(0);
            let interval = if minutes == 0 { "0" } else { "minutes" };
            Ok(tell(&format!(
                "start new session with options {{duration:{minutes}, interval:{interval}, displaySleepAllowed:true}}"
            )))
        }
        "end" => Ok(tell("end session")),
        "closed_display_on" => Ok(tell("enable closed display mode")),
        "closed_display_off" => Ok(tell("disable closed display mode")),
        other => Err(format!(
            "unknown action {other:?}: start, end, closed_display_on or closed_display_off"
        )),
    }
}

/// `GET /mac/amphetamine`: Amphetamine as it stands. Asking it the first
/// time brings macOS's Automation question (the Bakery controlling it).
pub(crate) async fn amphetamine_route() -> Result<Json<Amphetamine>, Fail> {
    operator_only()?;
    crate::actor::spawn_blocking(amphetamine_status)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map(Json)
        .map_err(|e| (StatusCode::BAD_GATEWAY, e))
}

/// `POST /mac/amphetamine`: the operator's own press, done through
/// Amphetamine's scripting; it answers how Amphetamine stands after.
pub(crate) async fn amphetamine_act(
    Json(req): Json<AmphetamineReq>,
) -> Result<Json<Amphetamine>, Fail> {
    operator_only()?;
    let script = amphetamine_script(&req).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    crate::actor::spawn_blocking(move || {
        osascript(&["-e", &script])?;
        amphetamine_status()
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map(Json)
    .map_err(|e| (StatusCode::BAD_GATEWAY, e))
}

/// `POST /mac/fans/open`: Macs Fan Control, opened; it has no other way in.
pub(crate) async fn open_fans() -> Result<StatusCode, Fail> {
    operator_only()?;
    let opened = std::process::Command::new("/usr/bin/open")
        .args(["-a", "Macs Fan Control"])
        .status()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    if opened.success() {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err((
            StatusCode::NOT_FOUND,
            "Macs Fan Control isn't installed".to_string(),
        ))
    }
}

// ---- The heat guard ------------------------------------------------------

/// This Mac's thermal state, as macOS reports it to apps:
/// `nominal`, `fair`, `serious` or `critical`.
fn thermal() -> Option<&'static str> {
    let said = osascript(&[
        "-l",
        "JavaScript",
        "-e",
        r#"ObjC.import("Foundation"); $.NSProcessInfo.processInfo.thermalState"#,
    ])
    .ok()?;
    ["nominal", "fair", "serious", "critical"]
        .get(said.parse::<usize>().ok()?)
        .copied()
}

/// The System Management Controller's sensors, read as any user may.
mod smc {
    use std::ffi::{c_char, c_void};

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct Vers {
        major: u8,
        minor: u8,
        build: u8,
        reserved: u8,
        release: u16,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct PLimit {
        version: u16,
        length: u16,
        cpu: u32,
        gpu: u32,
        mem: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct KeyInfo {
        size: u32,
        kind: u32,
        attributes: u8,
    }

    /// AppleSMC's call, as its user client takes it.
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct Call {
        key: u32,
        vers: Vers,
        plimit: PLimit,
        info: KeyInfo,
        result: u8,
        status: u8,
        command: u8,
        index: u32,
        bytes: [u8; 32],
    }

    const _: () = assert!(std::mem::size_of::<Call>() == 80);
    const READ_BYTES: u8 = 5;
    const READ_INDEX: u8 = 8;
    const READ_INFO: u8 = 9;
    const SMC_CALL: u32 = 2;

    extern "C" {
        /// This process's task port (C's `mach_task_self()`).
        static mach_task_self_: u32;
    }

    #[link(name = "IOKit", kind = "framework")]
    extern "C" {
        fn IOServiceMatching(name: *const c_char) -> *mut c_void;
        fn IOServiceGetMatchingService(main_port: u32, matching: *mut c_void) -> u32;
        fn IOServiceOpen(service: u32, owning_task: u32, kind: u32, connect: *mut u32) -> i32;
        fn IOServiceClose(connect: u32) -> i32;
        fn IOObjectRelease(object: u32) -> i32;
        fn IOConnectCallStructMethod(
            connection: u32,
            selector: u32,
            input: *const c_void,
            input_size: usize,
            output: *mut c_void,
            output_size: *mut usize,
        ) -> i32;
    }

    pub(super) struct Smc(u32);

    impl Smc {
        pub(super) fn open() -> Option<Smc> {
            // SAFETY: IOKit calls with a NUL-terminated name and an out
            // pointer to a local; the service is released after opening.
            unsafe {
                let service =
                    IOServiceGetMatchingService(0, IOServiceMatching(c"AppleSMC".as_ptr()));
                if service == 0 {
                    return None;
                }
                let mut connection = 0u32;
                let opened = IOServiceOpen(service, mach_task_self_, 0, &mut connection);
                IOObjectRelease(service);
                (opened == 0).then_some(Smc(connection))
            }
        }

        fn call(&self, input: &Call) -> Option<Call> {
            let mut output = Call::default();
            let mut size = std::mem::size_of::<Call>();
            // SAFETY: both are AppleSMC's 80-byte call, as declared above.
            let called = unsafe {
                IOConnectCallStructMethod(
                    self.0,
                    SMC_CALL,
                    input as *const Call as *const c_void,
                    std::mem::size_of::<Call>(),
                    &mut output as *mut Call as *mut c_void,
                    &mut size,
                )
            };
            (called == 0 && output.result == 0).then_some(output)
        }

        /// A key's type and bytes.
        pub(super) fn read(&self, key: u32) -> Option<(u32, Vec<u8>)> {
            let info = self
                .call(&Call {
                    key,
                    command: READ_INFO,
                    ..Default::default()
                })?
                .info;
            let mut call = Call {
                key,
                command: READ_BYTES,
                ..Default::default()
            };
            call.info.size = info.size;
            let out = self.call(&call)?;
            let size = (info.size as usize).min(32);
            Some((info.kind, out.bytes[..size].to_vec()))
        }

        /// The key at `index` in the controller's list.
        pub(super) fn key_at(&self, index: u32) -> Option<u32> {
            let call = Call {
                command: READ_INDEX,
                index,
                ..Default::default()
            };
            self.call(&call).map(|out| out.key)
        }
    }

    impl Drop for Smc {
        fn drop(&mut self) {
            // SAFETY: the connection this opened.
            unsafe { IOServiceClose(self.0) };
        }
    }

    /// A four-letter key as the controller numbers it.
    pub(super) fn key(name: &str) -> u32 {
        u32::from_be_bytes(name.as_bytes().try_into().unwrap_or([0; 4]))
    }

    /// A reading in its own type.
    pub(super) fn value(kind: u32, bytes: &[u8]) -> Option<f32> {
        let at = |n: usize| bytes.get(..n);
        match &kind.to_be_bytes() {
            b"flt " => Some(f32::from_le_bytes(at(4)?.try_into().ok()?)),
            b"fpe2" => Some(u16::from_be_bytes(at(2)?.try_into().ok()?) as f32 / 4.0),
            b"sp78" => Some(i16::from_be_bytes(at(2)?.try_into().ok()?) as f32 / 256.0),
            b"ui8 " => Some(*bytes.first()? as f32),
            b"ui16" => Some(u16::from_be_bytes(at(2)?.try_into().ok()?) as f32),
            b"ui32" => Some(u32::from_be_bytes(at(4)?.try_into().ok()?) as f32),
            _ => None,
        }
    }
}

/// One fan.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub(crate) struct Fan {
    pub(crate) rpm: f32,
    pub(crate) min: Option<f32>,
    pub(crate) max: Option<f32>,
}

#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub(crate) struct Heat {
    /// `nominal`, `fair`, `serious` or `critical`, as macOS reports it.
    pub(crate) thermal: Option<String>,
    /// The hottest of its temperature sensors, in °C.
    pub(crate) hottest_c: Option<f32>,
    pub(crate) fans: Vec<Fan>,
    /// Agents' new turns wait while this Mac runs hot.
    pub(crate) holding: bool,
}

/// The temperature keys this Mac's controller has, found once.
fn temperature_keys(smc: &smc::Smc) -> &'static [u32] {
    static KEYS: std::sync::OnceLock<Vec<u32>> = std::sync::OnceLock::new();
    KEYS.get_or_init(|| {
        let count = smc
            .read(smc::key("#KEY"))
            .and_then(|(kind, bytes)| smc::value(kind, &bytes))
            .unwrap_or(0.0) as u32;
        (0..count)
            .filter_map(|i| smc.key_at(i))
            .filter(|k| k.to_be_bytes()[0] == b'T')
            .collect()
    })
}

fn sensors() -> (Option<f32>, Vec<Fan>) {
    let Some(smc) = smc::Smc::open() else {
        return (None, Vec::new());
    };
    let reading = |name: String| {
        smc.read(smc::key(&name))
            .and_then(|(kind, bytes)| smc::value(kind, &bytes))
    };
    let hottest = temperature_keys(&smc)
        .iter()
        .filter_map(|k| {
            smc.read(*k)
                .and_then(|(kind, bytes)| smc::value(kind, &bytes))
        })
        // Unplugged sensors read 0 or nonsense.
        .filter(|c| (5.0..130.0).contains(c))
        .fold(None, |hottest: Option<f32>, c| {
            Some(hottest.map_or(c, |h| h.max(c)))
        });
    let count = reading("FNum".to_string()).unwrap_or(0.0) as u32;
    let fans = (0..count)
        .filter_map(|i| {
            Some(Fan {
                rpm: reading(format!("F{i}Ac"))?,
                min: reading(format!("F{i}Mn")),
                max: reading(format!("F{i}Mx")),
            })
        })
        .collect();
    (hottest, fans)
}

static HOLDING: AtomicBool = AtomicBool::new(false);

/// The heat guard's latest reading, for the meters.
static LAST_HEAT: Mutex<Option<Heat>> = Mutex::new(None);

/// Whether agents' new turns wait for this Mac to cool down.
pub(crate) fn holding() -> bool {
    HOLDING.load(Ordering::Relaxed)
}

/// This Mac's heat, now.
pub(crate) fn heat() -> Heat {
    let (hottest_c, fans) = sensors();
    Heat {
        thermal: thermal().map(str::to_string),
        hottest_c,
        fans,
        holding: holding(),
    }
}

/// From now on, a look at this Mac's thermal state every half minute: at
/// `serious` or worse, agents' new turns wait (running ones finish), and
/// once it's back to `fair` they go on. The app hears each change as a
/// `{"kind":"heat"}` envelope.
pub(crate) fn guard(state: AppState) {
    std::thread::spawn(move || loop {
        let state_now = thermal();
        let hot = matches!(state_now, Some("serious" | "critical"));
        let (hottest_c, fans) = sensors();
        *LAST_HEAT.lock().unwrap() = Some(Heat {
            thermal: state_now.map(str::to_string),
            hottest_c,
            fans,
            holding: hot,
        });
        if hot != HOLDING.swap(hot, Ordering::Relaxed) {
            let mut envelope = serde_json::to_value(heat()).unwrap_or_default();
            envelope["kind"] = "heat".into();
            let _ = state.tx.send(envelope.to_string());
            if !hot {
                // What waited goes on.
                crate::agent_control::recover_on_boot(&state, &Default::default());
            }
        }
        std::thread::sleep(Duration::from_secs(30));
    });
}

/// `GET /mac/heat`: this Mac's thermal state, hottest sensor and fans.
pub(crate) async fn heat_route() -> Result<Json<Heat>, Fail> {
    crate::actor::spawn_blocking(heat)
        .await
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

// ---- The meters ----------------------------------------------------------

/// The Mach calls the meters use, as the C headers give them.
mod mach {
    use std::ffi::c_int;

    pub(super) const HOST_CPU_LOAD_INFO: c_int = 3;
    pub(super) const HOST_VM_INFO64: c_int = 4;

    /// `vm_statistics64`.
    #[repr(C)]
    #[derive(Default)]
    pub(super) struct VmStatistics64 {
        pub(super) free_count: u32,
        pub(super) active_count: u32,
        pub(super) inactive_count: u32,
        pub(super) wire_count: u32,
        pub(super) zero_fill_count: u64,
        pub(super) reactivations: u64,
        pub(super) pageins: u64,
        pub(super) pageouts: u64,
        pub(super) faults: u64,
        pub(super) cow_faults: u64,
        pub(super) lookups: u64,
        pub(super) hits: u64,
        pub(super) purges: u64,
        pub(super) purgeable_count: u32,
        pub(super) speculative_count: u32,
        pub(super) decompressions: u64,
        pub(super) compressions: u64,
        pub(super) swapins: u64,
        pub(super) swapouts: u64,
        pub(super) compressor_page_count: u32,
        pub(super) throttled_count: u32,
        pub(super) external_page_count: u32,
        pub(super) internal_page_count: u32,
        pub(super) total_uncompressed_pages_in_compressor: u64,
    }

    extern "C" {
        pub(super) fn mach_host_self() -> u32;
        pub(super) fn host_statistics(
            host: u32,
            flavor: c_int,
            info: *mut c_int,
            count: *mut u32,
        ) -> c_int;
        pub(super) fn host_statistics64(
            host: u32,
            flavor: c_int,
            info: *mut c_int,
            count: *mut u32,
        ) -> c_int;
    }
}

/// A `sysctl` value by name, as any user may read it.
fn sysctl<T: Default>(name: &str) -> Option<T> {
    let name = std::ffi::CString::new(name).ok()?;
    let mut value = T::default();
    let mut size = std::mem::size_of::<T>();
    let read = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            &mut value as *mut T as *mut libc::c_void,
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    (read == 0).then_some(value)
}

/// `xsw_usage`, what `vm.swapusage` answers.
#[repr(C)]
#[derive(Default)]
struct SwapUsage {
    total: u64,
    avail: u64,
    used: u64,
    pagesize: u32,
    encrypted: i32,
}

/// The CPU's ticks since boot: busy (user, system, nice) and all.
fn cpu_ticks() -> Option<(u64, u64)> {
    let mut ticks = [0i32; 4];
    let mut count = 4u32;
    let read = unsafe {
        mach::host_statistics(
            mach::mach_host_self(),
            mach::HOST_CPU_LOAD_INFO,
            ticks.as_mut_ptr(),
            &mut count,
        )
    };
    if read != 0 {
        return None;
    }
    let [user, system, idle, nice] = ticks.map(|t| t as u32 as u64);
    Some((user + system + nice, user + system + idle + nice))
}

/// The share of the CPU busy between two readings, in percent.
fn cpu_percent(before: (u64, u64), after: (u64, u64)) -> f64 {
    let (busy, all) = (
        after.0.saturating_sub(before.0),
        after.1.saturating_sub(before.1),
    );
    if all == 0 {
        0.0
    } else {
        busy as f64 * 100.0 / all as f64
    }
}

/// Memory as Activity Monitor counts what's used (app memory, wired and
/// compressed), its total, macOS's own word on its pressure, and swap.
fn memory_now() -> Option<(u64, u64, &'static str, Option<u64>)> {
    let mut stats = mach::VmStatistics64::default();
    let mut count = (std::mem::size_of::<mach::VmStatistics64>() / 4) as u32;
    let read = unsafe {
        mach::host_statistics64(
            mach::mach_host_self(),
            mach::HOST_VM_INFO64,
            &mut stats as *mut mach::VmStatistics64 as *mut i32,
            &mut count,
        )
    };
    if read != 0 {
        return None;
    }
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as u64;
    let pages = (stats
        .internal_page_count
        .saturating_sub(stats.purgeable_count)) as u64
        + stats.wire_count as u64
        + stats.compressor_page_count as u64;
    let total = sysctl::<u64>("hw.memsize")?;
    let pressure = match sysctl::<i32>("kern.memorystatus_vm_pressure_level") {
        Some(4) => "critical",
        Some(2) => "warn",
        _ => "normal",
    };
    let swap = sysctl::<SwapUsage>("vm.swapusage").map(|s| s.used);
    Some(((pages * page).min(total), total, pressure, swap))
}

#[derive(Debug, Clone, Copy, Serialize, schemars::JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MeterLevel {
    Ok,
    Warn,
    Bad,
}

#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub(crate) struct DiskMeter {
    pub(crate) level: MeterLevel,
    pub(crate) free: u64,
    pub(crate) total: u64,
}

#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub(crate) struct MemoryMeter {
    pub(crate) level: MeterLevel,
    /// What Activity Monitor calls Memory Used.
    pub(crate) used: u64,
    pub(crate) total: u64,
    /// `normal`, `warn` or `critical`, as macOS says it.
    pub(crate) pressure: String,
    pub(crate) swap_used: Option<u64>,
}

#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub(crate) struct CpuMeter {
    pub(crate) level: MeterLevel,
    /// Across all cores, since the last reading.
    pub(crate) percent: f64,
    pub(crate) load1: f64,
    pub(crate) cores: u32,
}

#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub(crate) struct HeatMeter {
    pub(crate) level: MeterLevel,
    pub(crate) thermal: Option<String>,
    pub(crate) hottest_c: Option<f32>,
    pub(crate) holding: bool,
}

#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub(crate) struct MeterSample {
    pub(crate) at: String,
    pub(crate) cpu_percent: f64,
    pub(crate) memory_used: u64,
}

#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub(crate) struct Meters {
    pub(crate) disk: DiskMeter,
    pub(crate) memory: MemoryMeter,
    pub(crate) cpu: CpuMeter,
    pub(crate) heat: HeatMeter,
    /// One reading every [`METER_EVERY`], the last ten minutes, oldest first.
    pub(crate) history: Vec<MeterSample>,
}

/// How often the meters read, and how many readings the history keeps.
const METER_EVERY: Duration = Duration::from_secs(15);
const HISTORY: usize = 40;

fn disk_level(free: u64, total: u64) -> MeterLevel {
    if free < total / 10 {
        MeterLevel::Bad
    } else if free < total / 100 * 15 {
        MeterLevel::Warn
    } else {
        MeterLevel::Ok
    }
}

fn memory_level(pressure: &str) -> MeterLevel {
    match pressure {
        "critical" => MeterLevel::Bad,
        "warn" => MeterLevel::Warn,
        _ => MeterLevel::Ok,
    }
}

/// CPU held high for the last minute of readings.
fn cpu_level(recent: &[f64]) -> MeterLevel {
    let minute = (60 / METER_EVERY.as_secs()) as usize;
    if recent.len() < minute {
        return MeterLevel::Ok;
    }
    let held = recent[recent.len() - minute..]
        .iter()
        .copied()
        .fold(f64::MAX, f64::min);
    if held >= 95.0 {
        MeterLevel::Bad
    } else if held >= 85.0 {
        MeterLevel::Warn
    } else {
        MeterLevel::Ok
    }
}

fn heat_level(thermal: Option<&str>, holding: bool) -> MeterLevel {
    match thermal {
        _ if holding => MeterLevel::Bad,
        Some("serious" | "critical") => MeterLevel::Bad,
        Some("fair") => MeterLevel::Warn,
        _ => MeterLevel::Ok,
    }
}

/// The meters' latest reading, with their history.
static METERS: Mutex<Option<Meters>> = Mutex::new(None);

/// One reading of every meter, `percent` being the CPU's since the last.
fn read_meters(state: &AppState, percent: f64, history: &VecDeque<MeterSample>) -> Meters {
    let (free, total) = crate::disk::space(&state.base).unwrap_or((0, 0));
    let (used, mem_total, pressure, swap_used) = memory_now().unwrap_or((0, 0, "normal", None));
    let mut load = [0f64; 3];
    unsafe { libc::getloadavg(load.as_mut_ptr(), 3) };
    let recent: Vec<f64> = history.iter().map(|s| s.cpu_percent).collect();
    let heat = LAST_HEAT.lock().unwrap().clone();
    let (thermal, hottest_c) = heat
        .map(|h| (h.thermal, h.hottest_c))
        .unwrap_or((None, None));
    Meters {
        disk: DiskMeter {
            level: disk_level(free, total),
            free,
            total,
        },
        memory: MemoryMeter {
            level: memory_level(pressure),
            used,
            total: mem_total,
            pressure: pressure.to_string(),
            swap_used,
        },
        cpu: CpuMeter {
            level: cpu_level(&recent),
            percent,
            load1: load[0],
            cores: std::thread::available_parallelism().map_or(1, |n| n.get() as u32),
        },
        heat: HeatMeter {
            level: heat_level(thermal.as_deref(), holding()),
            thermal,
            hottest_c,
            holding: holding(),
        },
        history: history.iter().cloned().collect(),
    }
}

/// From now on, every meter read every [`METER_EVERY`]; the app hears a
/// `{"kind":"mac-meters"}` envelope (the meters without their history)
/// whenever a level changes, up or down.
pub(crate) fn meters_watch(state: AppState) {
    std::thread::spawn(move || {
        let mut before = cpu_ticks();
        let mut history: VecDeque<MeterSample> = VecDeque::new();
        let mut levels = None;
        std::thread::sleep(Duration::from_secs(1));
        loop {
            let now = cpu_ticks();
            let percent = before.zip(now).map_or(0.0, |(b, n)| cpu_percent(b, n));
            before = now;
            let used = memory_now().map_or(0, |m| m.0);
            history.push_back(MeterSample {
                at: chrono::Utc::now().to_rfc3339(),
                cpu_percent: percent,
                memory_used: used,
            });
            while history.len() > HISTORY {
                history.pop_front();
            }
            let meters = read_meters(&state, percent, &history);
            let now_levels = Some((
                meters.disk.level,
                meters.memory.level,
                meters.cpu.level,
                meters.heat.level,
            ));
            if levels.is_some() && now_levels != levels {
                let mut envelope = serde_json::to_value(&meters).unwrap_or_default();
                if let Some(fields) = envelope.as_object_mut() {
                    fields.remove("history");
                }
                envelope["kind"] = "mac-meters".into();
                let _ = state.tx.send(envelope.to_string());
            }
            levels = now_levels;
            *METERS.lock().unwrap() = Some(meters);
            std::thread::sleep(METER_EVERY);
        }
    });
}

/// `GET /mac/meters`: disk, memory, CPU and heat, each with its level, and
/// the last ten minutes of CPU and memory.
pub(crate) async fn meters_route(
    axum::extract::State(st): axum::extract::State<AppState>,
) -> Result<Json<Meters>, Fail> {
    operator_only()?;
    if let Some(meters) = METERS.lock().unwrap().clone() {
        return Ok(Json(meters));
    }
    crate::actor::spawn_blocking(move || read_meters(&st, 0.0, &VecDeque::new()))
        .await
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amphetamine_is_asked_in_its_own_words() {
        let ask = |action: &str, minutes: Option<u32>| {
            amphetamine_script(&AmphetamineReq {
                action: action.into(),
                minutes,
            })
        };
        assert_eq!(
            ask("start", None).unwrap(),
            r#"tell application "Amphetamine" to start new session with options {duration:0, interval:0, displaySleepAllowed:true}"#
        );
        assert!(ask("start", Some(90))
            .unwrap()
            .contains("{duration:90, interval:minutes,"));
        assert_eq!(
            ask("end", None).unwrap(),
            r#"tell application "Amphetamine" to end session"#
        );
        assert!(ask("closed_display_on", None)
            .unwrap()
            .ends_with("enable closed display mode"));
        assert!(ask("sleep", None).is_err());
        let read = amphetamine_read("true, 3540, false").unwrap();
        assert!(read.active && read.seconds_left == Some(3540) && !read.closed_display);
        assert_eq!(
            amphetamine_read("false, 0, true").unwrap().seconds_left,
            None
        );
        let ended = amphetamine_read("false, -3, false").unwrap();
        assert!(!ended.active && ended.seconds_left.is_none(), "{ended:?}");
        let endless = amphetamine_read("true, -1, false").unwrap();
        assert!(endless.active && endless.seconds_left.is_none(), "{endless:?}");
        assert!(amphetamine_read("nonsense").is_none());
    }

    #[test]
    fn the_controllers_readings_decode_in_their_own_types() {
        let kind = |k: &str| smc::key(k);
        assert_eq!(smc::value(kind("flt "), &42.5f32.to_le_bytes()), Some(42.5));
        assert_eq!(smc::value(kind("fpe2"), &[0x1f, 0x40]), Some(2000.0));
        assert_eq!(smc::value(kind("sp78"), &[0x2d, 0x80]), Some(45.5));
        assert_eq!(smc::value(kind("ui8 "), &[2]), Some(2.0));
        assert_eq!(smc::value(kind("ch8*"), &[1]), None);
    }

    /// On this Mac: its thermal state and sensors read, without admin or a
    /// prompt.
    #[test]
    fn this_macs_heat_reads_without_admin() {
        let heat = heat();
        assert!(heat.thermal.is_some(), "{heat:?}");
        assert!(heat.hottest_c.is_some_and(|c| c > 5.0), "{heat:?}");
        assert!(!heat.holding);
    }

    #[test]
    fn each_meter_has_its_level_and_reads_without_admin() {
        assert_eq!(disk_level(9, 100), MeterLevel::Bad);
        assert_eq!(disk_level(14, 100), MeterLevel::Warn);
        assert_eq!(disk_level(15, 100), MeterLevel::Ok);
        assert_eq!(
            (
                memory_level("critical"),
                memory_level("warn"),
                memory_level("normal")
            ),
            (MeterLevel::Bad, MeterLevel::Warn, MeterLevel::Ok)
        );
        assert_eq!(
            cpu_level(&[99.0, 99.0, 99.0]),
            MeterLevel::Ok,
            "not yet a minute"
        );
        assert_eq!(cpu_level(&[10.0, 96.0, 97.0, 99.0, 95.5]), MeterLevel::Bad);
        assert_eq!(
            cpu_level(&[99.0, 99.0, 86.0, 99.0]),
            MeterLevel::Warn,
            "held at 85 or more"
        );
        assert_eq!(
            cpu_level(&[99.0, 99.0, 50.0, 99.0]),
            MeterLevel::Ok,
            "a dip breaks it"
        );
        assert_eq!(heat_level(Some("fair"), false), MeterLevel::Warn);
        assert_eq!(
            heat_level(Some("nominal"), true),
            MeterLevel::Bad,
            "holding is bad"
        );
        assert_eq!(heat_level(Some("serious"), false), MeterLevel::Bad);
        assert_eq!(cpu_percent((100, 400), (150, 500)), 50.0);

        let (used, total, pressure, _) = memory_now().expect("Mach answers any user");
        assert!(total > 0 && used > 0 && used <= total, "{used} of {total}");
        assert!(["normal", "warn", "critical"].contains(&pressure));
        assert!(cpu_ticks().is_some_and(|(busy, all)| busy <= all && all > 0));
    }
}
