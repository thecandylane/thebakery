//! The node as a service that runs all day, as the operator decided for
//! 24/7: its own log, kept to a few files under `~/.bakery/logs`; the Mac
//! kept from idling to sleep while agents work; and a restart (a deploy's,
//! a rollback's) that waits for running turns to finish, unless the
//! operator says now. Turns a restart still cuts off carry on by themselves
//! after it (`resume`). macOS starts and restarts the node itself through a
//! per-user LaunchAgent (`scripts/bakery.sh service install`).

use crate::state::AppState;
use axum::{extract::State, http::StatusCode, Json};
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The node's log, `node.log`, and the older ones beside it: five files in
/// all, the oldest dropped as a new one starts.
const LOG_FILES: usize = 5;
/// Where one log file ends and the next begins.
const LOG_BYTES: u64 = 10 * 1024 * 1024;
/// How long a restart waits for running turns before it goes ahead anyway.
const RESTART_WAIT: Duration = Duration::from_secs(600);

/// The `n`th older log beside `log`: `node.1.log`, `node.2.log`, …
fn older(log: &Path, n: usize) -> PathBuf {
    if n == 0 {
        return log.to_path_buf();
    }
    let stem = log.file_stem().unwrap_or_default().to_string_lossy();
    log.with_file_name(format!("{stem}.{n}.log"))
}

/// Copies what `from` says into `log` until it closes (and to `echo`, a
/// terminal, when there is one), starting a new file past `limit` and
/// keeping `files` in all.
fn copy_rotating(mut from: impl Read, log: &Path, mut echo: Option<std::fs::File>, limit: u64, files: usize) {
    let open = || std::fs::OpenOptions::new().create(true).append(true).open(log);
    let Ok(mut file) = open() else {
        return;
    };
    let mut size = file.metadata().map(|m| m.len()).unwrap_or(0);
    let mut buf = [0u8; 8192];
    loop {
        let n = match from.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        if let Some(terminal) = echo.as_mut() {
            if terminal.write_all(&buf[..n]).is_err() {
                echo = None;
            }
        }
        if size > 0 && size + n as u64 > limit {
            for k in (1..files).rev() {
                let _ = std::fs::rename(older(log, k - 1), older(log, k));
            }
            match open() {
                Ok(fresh) => (file, size) = (fresh, 0),
                Err(_) => return,
            }
        }
        if file.write_all(&buf[..n]).is_ok() {
            size += n as u64;
        }
    }
}

/// Everything this process prints goes to `~/.bakery/logs/node.log` from
/// here on, never the checkout, and still to its terminal when it runs in
/// one.
pub(crate) fn capture_output(base: &Path) -> std::io::Result<PathBuf> {
    use std::os::fd::FromRawFd;
    let dir = base.join("logs");
    std::fs::create_dir_all(&dir)?;
    let log = dir.join("node.log");
    let mut ends = [0; 2];
    // SAFETY: plain descriptor calls on descriptors this process owns; each
    // result is checked, and each new descriptor is owned by one File.
    unsafe {
        let echo = (libc::isatty(2) == 1)
            .then(|| libc::dup(2))
            .filter(|fd| *fd >= 0)
            .map(|fd| std::fs::File::from_raw_fd(fd));
        if libc::pipe(ends.as_mut_ptr()) != 0 {
            return Err(std::io::Error::last_os_error());
        }
        if libc::dup2(ends[1], 1) < 0 || libc::dup2(ends[1], 2) < 0 {
            return Err(std::io::Error::last_os_error());
        }
        libc::close(ends[1]);
        let from = std::fs::File::from_raw_fd(ends[0]);
        let at = log.clone();
        std::thread::spawn(move || copy_rotating(from, &at, echo, LOG_BYTES, LOG_FILES));
    }
    Ok(log)
}

/// How many agents' turns are running now.
pub(crate) fn running(state: &AppState) -> usize {
    state
        .lifecycle
        .lock()
        .unwrap()
        .running_turns
        .values()
        .map(|n| *n as usize)
        .sum()
}

/// While agents work, the Mac doesn't idle to sleep: `caffeinate -i`, tied
/// to this process (`-w`), so it ends with the node whatever happens. The
/// display still sleeps, and a closed lid still sleeps the Mac.
// ponytail: looked at every few seconds; a turn that starts and ends
// between two looks never holds it.
pub(crate) fn keep_awake_while_working(state: AppState) {
    std::thread::spawn(move || {
        let mut held: Option<std::process::Child> = None;
        loop {
            match (running(&state) > 0, held.is_some()) {
                (true, false) => {
                    held = std::process::Command::new("/usr/bin/caffeinate")
                        .args(["-i", "-w", &std::process::id().to_string()])
                        .spawn()
                        .ok()
                }
                (false, true) => {
                    if let Some(mut awake) = held.take() {
                        let _ = awake.kill();
                        let _ = awake.wait();
                    }
                }
                _ => {}
            }
            std::thread::sleep(Duration::from_secs(5));
        }
    });
}

/// A restart waiting for running turns.
#[derive(Clone)]
pub(crate) struct Waiting {
    since: DateTime<Utc>,
    until: Instant,
    now: bool,
}

/// Whether a restart is waiting: queued messages wait for the node that
/// comes back instead of starting now.
pub(crate) fn restarting(state: &AppState) -> bool {
    state.lifecycle.lock().unwrap().restart.is_some()
}

/// The wait, live to the app as it changes: a `{"kind":"restart-wait"}`
/// envelope on its event socket ([`RestartWait`]'s fields), `restarting`
/// once it goes ahead.
fn tell_app(state: &AppState, restarting: bool) {
    if let Some(wait) = restart_wait(state) {
        let mut envelope = serde_json::to_value(&wait).unwrap_or_default();
        envelope["kind"] = "restart-wait".into();
        envelope["restarting"] = restarting.into();
        let _ = state.tx.send(envelope.to_string());
    }
}

/// Starts waiting for running turns, then calls `exit` for the restart:
/// once none runs, at the limit, or when the operator says now.
fn restart_when_idle(state: AppState, exit: fn()) {
    {
        let mut lifecycle = state.lifecycle.lock().unwrap();
        if lifecycle.restart.is_some() {
            return;
        }
        lifecycle.restart = Some(Waiting {
            since: Utc::now(),
            until: Instant::now() + RESTART_WAIT,
            now: false,
        });
    }
    println!("bakery-node: restarting once running turns finish");
    tell_app(&state, false);
    std::thread::spawn(move || {
        let mut turns = running(&state);
        loop {
            let told = {
                let lifecycle = state.lifecycle.lock().unwrap();
                lifecycle
                    .restart
                    .as_ref()
                    .is_none_or(|w| w.now || Instant::now() >= w.until)
            };
            let now = running(&state);
            if told || now == 0 {
                println!("bakery-node: restarting now");
                tell_app(&state, true);
                exit();
                return;
            }
            if now != turns {
                turns = now;
                tell_app(&state, false);
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    });
}

/// `SIGUSR1`, from `scripts/bakery.sh` before a restart, deploy or
/// rollback: finish running turns, then exit for it. `SIGTERM` (a stop, a
/// logout): exit at once, cleanly, so the LaunchAgent leaves it stopped; it
/// restarts only a node that crashed.
pub(crate) fn restart_on_signal(state: AppState) {
    use tokio::signal::unix::{signal, SignalKind};
    tokio::spawn(async move {
        let Ok(mut asked) = signal(SignalKind::user_defined1()) else {
            return;
        };
        while asked.recv().await.is_some() {
            restart_when_idle(state.clone(), || std::process::exit(0));
        }
    });
    tokio::spawn(async {
        if let Ok(mut stop) = signal(SignalKind::terminate()) {
            if stop.recv().await.is_some() {
                std::process::exit(0);
            }
        }
    });
}

#[derive(schemars::JsonSchema, Debug, Clone, Serialize)]
pub(crate) struct RestartWait {
    /// When the restart was asked for (RFC 3339).
    pub(crate) since: String,
    /// Turns it still waits for.
    pub(crate) turns: usize,
    /// Seconds until it goes ahead anyway.
    pub(crate) seconds_left: u64,
}

/// The restart waiting for running turns, if one is: what Settings shows
/// beside its Restart now.
pub(crate) fn restart_wait(state: &AppState) -> Option<RestartWait> {
    let wait = state.lifecycle.lock().unwrap().restart.clone()?;
    Some(RestartWait {
        since: wait.since.to_rfc3339(),
        turns: running(state),
        seconds_left: wait.until.saturating_duration_since(Instant::now()).as_secs(),
    })
}

/// `POST /control/restart/now`: the waiting restart goes ahead at once; the
/// turns it cuts off carry on after it.
pub(crate) async fn restart_now(State(st): State<AppState>) -> Result<Json<Option<RestartWait>>, (StatusCode, String)> {
    {
        let mut lifecycle = st.lifecycle.lock().unwrap();
        let Some(wait) = lifecycle.restart.as_mut() else {
            return Err((StatusCode::CONFLICT, "no restart is waiting".to_string()));
        };
        wait.now = true;
    }
    Ok(Json(restart_wait(&st)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// The log starts a new file past its limit and keeps five in all.
    #[test]
    fn the_log_keeps_five_files_the_oldest_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("node.log");
        // One line a read, as a process prints them.
        struct Lines(Vec<Vec<u8>>);
        impl Read for Lines {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                if self.0.is_empty() {
                    return Ok(0);
                }
                let line = self.0.remove(0);
                buf[..line.len()].copy_from_slice(&line);
                Ok(line.len())
            }
        }
        let lines = Lines((0..12).map(|n| format!("line {n:02}\n").into_bytes()).collect());
        copy_rotating(lines, &log, None, 16, 5);
        let read = |p: PathBuf| std::fs::read_to_string(p).unwrap_or_default();
        assert_eq!(read(log.clone()), "line 10\nline 11\n", "the newest");
        assert_eq!(read(older(&log, 1)), "line 08\nline 09\n");
        assert_eq!(read(older(&log, 4)), "line 02\nline 03\n", "the oldest kept");
        assert!(!older(&log, 5).exists(), "five files in all");
    }

    /// A restart waits for running turns, and queued messages wait for the
    /// node that comes back; it goes ahead once none runs, or at once when
    /// the operator says now.
    #[test]
    fn a_restart_waits_for_running_turns_unless_told_now() {
        static EXITED: AtomicUsize = AtomicUsize::new(0);
        fn exit() {
            EXITED.fetch_add(1, Ordering::SeqCst);
        }
        let until = |n: usize| {
            let deadline = Instant::now() + Duration::from_secs(60);
            while EXITED.load(Ordering::SeqCst) < n && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(50));
            }
            EXITED.load(Ordering::SeqCst)
        };
        let dir = tempfile::tempdir().unwrap();
        let state = crate::testutil::state_with(dir.path(), Arc::new(bakery_agent::StubModel));
        let seat = crate::node::Seat::Orchestrator;
        let lease = crate::state::admit_turn(&state, "wt:restart", seat).unwrap();
        let mut app = state.tx.subscribe();
        restart_when_idle(state.clone(), exit);
        let told: serde_json::Value = serde_json::from_str(&app.try_recv().unwrap()).unwrap();
        assert_eq!((told["kind"].as_str(), told["turns"].as_u64()), (Some("restart-wait"), Some(1)));
        assert!(restarting(&state));
        let agent = crate::node::agent_id("wt:restart", seat);
        assert!(crate::agent_control::dispatch_is_paused(&state, "wt:restart", &agent), "queued messages wait");
        std::thread::sleep(Duration::from_millis(1200));
        assert_eq!(EXITED.load(Ordering::SeqCst), 0, "a turn still runs");
        assert_eq!(restart_wait(&state).unwrap().turns, 1);
        drop(lease);
        assert_eq!(until(1), 1, "gone once none runs");

        // Restart now: at once, though a turn runs.
        let other = crate::testutil::state_with(&dir.path().join("other"), Arc::new(bakery_agent::StubModel));
        let _lease = crate::state::admit_turn(&other, "wt:restart-now", seat).unwrap();
        restart_when_idle(other.clone(), exit);
        let runtime = tokio::runtime::Runtime::new().unwrap();
        assert_eq!(runtime.block_on(restart_now(State(other.clone()))).unwrap().0.unwrap().turns, 1);
        assert_eq!(until(2), 2, "now");
    }
}
