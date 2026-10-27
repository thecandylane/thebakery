//! Node-owned named terminal sessions (07c-STUDIO-SHELL §3).
//!
//! Inverts the pane-owns-pty model: a session's shell process, output ring
//! buffer, and writer live in a [`TerminalHandle`] held by [`TerminalRegistry`]
//! (on [`crate::state::AppState`]), spawned once by [`spawn_terminal`] and torn
//! down only by an explicit close ([`kill_terminal`]) or a socket discard
//! ([`fence_active_ptys`]). A WebSocket attach ([`routes::terminal`](crate::routes::terminal))
//! subscribes to the live broadcast and replays the ring buffer; dropping
//! that connection — pane closed, client reloaded — detaches the view only.
//! The process, and everything it has printed up to [`RING_CAPACITY`] bytes,
//! survives until something explicitly kills the session.

use crate::state::AppState;
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use serde::Serialize;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, watch};

/// Bounds memory per terminal, not what a live viewer sees — a live
/// broadcast subscriber gets every byte regardless; this only caps how much
/// history a *late* attach can replay before the oldest bytes are evicted.
const RING_CAPACITY: usize = 256 * 1024;

/// Reserved `stream_id` for terminals that belong to no Project/socket at
/// all — today, a Codex/Cursor "Sign in" login pane spawned from Settings
/// (07d-A3D1), which has no stream context to attach to. Real stream ids are
/// always `wt:`-prefixed, so this can never collide. Carries no Heart
/// history, ever — `admit_pty`'s `Ok(None)` no-history default (not
/// discarded, host allowed) is exactly this id's permanent state.
pub(crate) const SYSTEM_ACCOUNTS_STREAM_ID: &str = "sys:accounts";

/// Reserved `stream_id` for the operator's own shells on this Mac, which
/// belong to no thread: the same ones wherever the operator is. No Heart
/// history either, so never discarded and never a burn.
pub(crate) const HOST_STREAM_ID: &str = "sys:host";

/// Reserved `stream_id`s for a project's own terminals: `project:<id>`.
pub(crate) const PROJECT_STREAM_PREFIX: &str = "project:";

/// The project a `project:<id>` terminal stream is.
pub(crate) fn project_of_stream(stream_id: &str) -> Option<&str> {
    stream_id.strip_prefix(PROJECT_STREAM_PREFIX).filter(|id| !id.is_empty())
}

/// A terminal's output as text: its escape sequences (colors, cursor moves,
/// titles) and carriage returns dropped.
fn plain(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => match chars.next() {
                // CSI: parameters, then one final byte from @ to ~.
                Some('[') => {
                    for c in chars.by_ref() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                }
                // OSC: up to BEL, or ESC \.
                Some(']') => {
                    while let Some(c) = chars.next() {
                        if c == '\u{7}' {
                            break;
                        }
                        if c == '\u{1b}' {
                            chars.next();
                            break;
                        }
                    }
                }
                // A character set's designation: one character more.
                Some('(' | ')' | '*' | '+') => {
                    chars.next();
                }
                // Any other escape is two characters long.
                _ => {}
            },
            '\r' => {}
            c => out.push(c),
        }
    }
    out
}

/// Retained output plus a monotonically increasing byte offset, so an
/// attaching client can ask for "everything since offset N" and learn
/// whether some of that range was already evicted rather than silently
/// getting a shorter replay than it expected.
struct RingBuffer {
    buf: Vec<u8>,
    /// Total bytes ever pushed — never decreases, even as `buf` evicts old
    /// bytes to stay under [`RING_CAPACITY`].
    total_written: u64,
}

impl RingBuffer {
    fn new() -> Self {
        Self {
            buf: Vec::new(),
            total_written: 0,
        }
    }

    fn push(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
        self.total_written += data.len() as u64;
        if self.buf.len() > RING_CAPACITY {
            let excess = self.buf.len() - RING_CAPACITY;
            self.buf.drain(0..excess);
        }
    }

    /// Offset of the first byte still retained in `buf`.
    fn start_offset(&self) -> u64 {
        self.total_written - self.buf.len() as u64
    }

    /// Bytes from `from` onward, and whether `from` named an offset older
    /// than anything still retained (some history was truncated).
    fn since(&self, from: u64) -> (Vec<u8>, bool) {
        let start = self.start_offset();
        if from <= start {
            (self.buf.clone(), from < start)
        } else {
            let skip = (from - start) as usize;
            (self.buf.get(skip..).unwrap_or(&[]).to_vec(), false)
        }
    }

    fn end_offset(&self) -> u64 {
        self.total_written
    }

    /// What is still kept from `from` on: see [`Replay`].
    fn replay(&self, from: u64) -> Replay {
        let kept = self.start_offset();
        let start = from.clamp(kept, self.end_offset());
        Replay {
            start,
            bytes: self.buf[(start - kept) as usize..].to_vec(),
            dropped: start.saturating_sub(from),
        }
    }
}

/// One stretch of a terminal's live output, with where it starts in the
/// terminal's whole output, so a viewer can tell what it already has.
#[derive(Clone)]
pub(crate) struct Chunk {
    pub(crate) start: u64,
    pub(crate) bytes: Arc<[u8]>,
}

/// A terminal's output from some offset on, as far as it is still kept.
pub(crate) struct Replay {
    /// Where `bytes` starts in the terminal's whole output.
    pub(crate) start: u64,
    pub(crate) bytes: Vec<u8>,
    /// How much of what was asked for is gone: the bytes just before `start`.
    pub(crate) dropped: u64,
}

/// The wire projection of a [`TerminalHandle`] — everything Studio needs to
/// render a terminal tab and its status, nothing about the process itself.
#[derive(schemars::JsonSchema, Debug, Clone, Serialize)]
pub(crate) struct TerminalInfo {
    pub(crate) terminal_id: String,
    pub(crate) stream_id: String,
    pub(crate) name: String,
    pub(crate) mode: String,
    pub(crate) cwd: String,
    pub(crate) live: bool,
    pub(crate) created_ts: String,
    /// Who opened it: "human" for the operator, an agent's actor
    /// ("agent:<seat>", or "agent:<thread>:<seat>" from another thread), or
    /// "agent" for the stream's Agent terminal, which shows every agent's
    /// commands.
    pub(crate) opened_by: String,
}

/// One node-owned terminal session: a real pty-backed child process plus the
/// state an attach needs (ring buffer, live broadcast, writer). Outlives any
/// single WebSocket connection — only [`kill_terminal`] or
/// [`fence_active_ptys`] ends it.
pub(crate) struct TerminalHandle {
    pub(crate) terminal_id: String,
    pub(crate) stream_id: String,
    pub(crate) mode: String,
    pub(crate) cwd: String,
    pub(crate) created_ts: String,
    pub(crate) opened_by: String,
    name: StdMutex<String>,
    alive: AtomicBool,
    /// Set once, right before `alive` flips false — an attached WS reads
    /// this (via [`Self::watch_alive`]'s change notification) to send a real
    /// close reason instead of hanging or closing with none. `None` until
    /// the process actually exits; a session killed before it ever produced
    /// a pid never reaches this (see `spawn_terminal`'s early-bail path).
    exit_code: StdMutex<Option<u32>>,
    alive_tx: watch::Sender<bool>,
    /// A system account login terminal's true identity (07d-A3D1 review
    /// finding 1) — `None` for an ordinary host/room terminal. Never inferred
    /// from `name`: `name` is free-form and renameable
    /// ([`TerminalHandle::rename`]), so it must never decide *which* login
    /// command a session is actually running. [`TerminalRegistry::find_live_login`]
    /// is the only thing that reads this.
    login_command: Option<bakery_agent::cli_tools::LoginCommand>,
    /// A command typed for the operator waits at its prompt: they have not
    /// run it, or cleared it with Ctrl-C, yet.
    typed: AtomicBool,
    /// Closed with Ctrl-C first ([`kill_terminal`]): a program that cleans up
    /// after itself on an interrupt, like `container build`.
    interrupt_first: AtomicBool,
    /// The process behind the session — `None` for a stream's Agent
    /// terminal, which only shows what the node writes to it.
    pty: Option<PtyIo>,
    /// The command call whose block is the last one written to an Agent
    /// terminal (empty before the first): one record for every writer, so
    /// no output lands under another call's `$ command` header.
    last_call: StdMutex<String>,
    ring: StdMutex<RingBuffer>,
    output_tx: broadcast::Sender<Chunk>,
}

struct PtyIo {
    pid: u32,
    writer: StdMutex<Box<dyn Write + Send>>,
    master: StdMutex<Box<dyn MasterPty + Send>>,
}

impl TerminalHandle {
    pub(crate) fn info(&self) -> TerminalInfo {
        TerminalInfo {
            terminal_id: self.terminal_id.clone(),
            stream_id: self.stream_id.clone(),
            name: self.name.lock().unwrap().clone(),
            mode: self.mode.clone(),
            cwd: self.cwd.clone(),
            live: self.alive.load(Ordering::Acquire),
            created_ts: self.created_ts.clone(),
            opened_by: self.opened_by.clone(),
        }
    }

    pub(crate) fn rename(&self, name: String) {
        *self.name.lock().unwrap() = name;
    }

    pub(crate) fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Acquire)
    }

    /// The exit code the underlying process reaped with, once it has —
    /// `None` while still alive, or if it never got as far as a real pid.
    pub(crate) fn exit_code(&self) -> Option<u32> {
        *self.exit_code.lock().unwrap()
    }

    /// A fresh subscription to this session's liveness — resolves once,
    /// right when `alive` flips to false, distinct from
    /// [`Self::subscribe`]'s live output stream (which a slow-but-alive
    /// process can go quiet on for unrelated reasons).
    pub(crate) fn watch_alive(&self) -> watch::Receiver<bool> {
        self.alive_tx.subscribe()
    }

    /// A view attaching from `from`: its live subscription and what is still
    /// kept from `from` on, taken together under the ring's lock (which
    /// [`Self::push_output`] sends under too), so each byte reaches the view
    /// once, in the replay or live after it.
    pub(crate) fn attach_since(&self, from: u64) -> (broadcast::Receiver<Chunk>, Replay) {
        let ring = self.ring.lock().unwrap();
        (self.output_tx.subscribe(), ring.replay(from))
    }

    /// What is still kept from `from` on, for a view that fell behind.
    pub(crate) fn kept_since(&self, from: u64) -> Replay {
        self.ring.lock().unwrap().replay(from)
    }

    /// `(bytes since `from`, truncated, end_offset)`.
    pub(crate) fn replay_since(&self, from: u64) -> (Vec<u8>, bool, u64) {
        let ring = self.ring.lock().unwrap();
        let (bytes, truncated) = ring.since(from);
        (bytes, truncated, ring.end_offset())
    }

    /// Closed with Ctrl-C first, as typed in its pane: see [`kill_terminal`].
    pub(crate) fn interrupt_first(&self) {
        self.interrupt_first.store(true, Ordering::Release);
    }

    /// A live shell with nothing running in it: the shell holds the
    /// foreground, so typed text waits at its prompt.
    pub(crate) fn is_idle(&self) -> bool {
        let Some(pty) = &self.pty else { return false };
        let leader = pty.master.lock().unwrap().process_group_leader();
        self.is_alive() && leader == Some(pty.pid as i32)
    }

    /// The last `max_bytes` it printed, as text.
    pub(crate) fn text_tail(&self, max_bytes: usize) -> String {
        let ring = self.ring.lock().unwrap();
        plain(&ring.buf[ring.buf.len().saturating_sub(max_bytes)..])
    }

    /// Types `text` at its prompt for the operator to run.
    pub(crate) fn type_for_operator(&self, text: &str) -> std::io::Result<()> {
        self.write_input(text.as_bytes())?;
        self.typed.store(true, Ordering::Relaxed);
        Ok(())
    }

    /// Something typed for the operator still waits at its prompt.
    #[allow(dead_code)]
    pub(crate) fn holds_typed(&self) -> bool {
        self.typed.load(Ordering::Relaxed)
    }

    pub(crate) fn write_input(&self, bytes: &[u8]) -> std::io::Result<()> {
        // Enter runs what waits at the prompt; Ctrl-C clears it.
        if bytes.iter().any(|b| matches!(b, b'\r' | b'\n' | 0x03)) {
            self.typed.store(false, Ordering::Relaxed);
        }
        match &self.pty {
            Some(pty) => pty.writer.lock().unwrap().write_all(bytes),
            None => Ok(()),
        }
    }

    pub(crate) fn resize(&self, cols: u16, rows: u16) {
        if let Some(pty) = &self.pty {
            let _ = pty.master.lock().unwrap().resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            });
        }
    }

    /// Output the node writes itself (an Agent terminal), recorded and sent
    /// exactly like a process's own.
    fn push_output(&self, bytes: Vec<u8>) {
        let mut ring = self.ring.lock().unwrap();
        let start = ring.end_offset();
        ring.push(&bytes);
        // Sent under the ring's lock: see `attach_since`. Best-effort — Err
        // just means no live viewer.
        let _ = self.output_tx.send(Chunk { start, bytes: bytes.into() });
    }

    /// `text` as part of `call`'s block of an Agent terminal: see
    /// [`agent_terminal_write`].
    fn write_call(&self, call: &str, command: &str, text: &str, own_line: bool) {
        // One writer at a time, so the block it finds open is still the last
        // when its bytes land.
        let mut last = self.last_call.lock().unwrap();
        let header = *last != call;
        if !header && text.is_empty() {
            return;
        }
        let mid_line = self
            .ring
            .lock()
            .unwrap()
            .buf
            .last()
            .is_some_and(|b| *b != b'\n');
        let mut out = String::with_capacity(command.len() + text.len() + 8);
        if mid_line && (own_line || header) {
            out.push_str("\r\n");
        }
        if header {
            out.push_str(&call_mark(call));
            push_terminal_lines(&mut out, &format!("$ {command}\n"));
            *last = call.to_string();
        }
        push_terminal_lines(&mut out, text);
        self.push_output(out.into_bytes());
    }

    /// Ends an Agent terminal: attached views close, the registry forgets it.
    fn end_without_process(&self, state: &AppState) {
        self.alive.store(false, Ordering::Release);
        let _ = self.alive_tx.send(false);
        state
            .terminals
            .lock()
            .unwrap()
            .sessions
            .remove(&self.terminal_id);
    }
}

/// The mode, and the name, of a stream's read-only Agent terminal: every
/// command an agent runs on that stream — native or Bakery's own `run` —
/// as `$ command`, its output, and `[exit N]`, each under its own header.
/// Created on first use.
pub(crate) const AGENT_TERMINAL_MODE: &str = "agent";

/// The OSC each Agent terminal block's `$ command` header opens with,
/// naming its call: a thread's command row scrolls the Agent terminal to
/// it. A terminal shows nothing for it, and [`plain`] drops it.
pub(crate) const CALL_MARK_OSC: u32 = 7766;

fn call_mark(call: &str) -> String {
    let call: String = call.chars().filter(|c| !c.is_control()).collect();
    format!("\u{1b}]{CALL_MARK_OSC};{call}\u{7}")
}

/// An Agent terminal's bytes as they show: the call marks, which print
/// nothing, left out.
#[cfg(test)]
pub(crate) fn shown(bytes: &[u8]) -> String {
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    let mark = format!("\u{1b}]{CALL_MARK_OSC};");
    let (mut out, mut rest) = (String::new(), text.as_str());
    while let Some(i) = rest.find(&mark) {
        out.push_str(&rest[..i]);
        let after = &rest[i + mark.len()..];
        rest = after.find('\u{7}').map_or("", |end| &after[end + 1..]);
    }
    out + rest
}

/// `stream_id`'s Agent terminal, created when it has none — `None` while
/// the stream is being discarded.
pub(crate) fn agent_terminal(state: &AppState, stream_id: &str) -> Option<Arc<TerminalHandle>> {
    if state.lifecycle.lock().unwrap().retiring.contains(stream_id) {
        return None;
    }
    let mut reg = state.terminals.lock().unwrap();
    if let Some(found) = reg
        .sessions
        .values()
        .find(|h| h.stream_id == stream_id && h.pty.is_none())
    {
        return Some(found.clone());
    }
    let (output_tx, _rx) = broadcast::channel::<Chunk>(1024);
    let (alive_tx, _alive_rx) = watch::channel(true);
    let handle = Arc::new(TerminalHandle {
        terminal_id: random_id(),
        stream_id: stream_id.to_string(),
        mode: AGENT_TERMINAL_MODE.to_string(),
        cwd: String::new(),
        created_ts: chrono::Utc::now().to_rfc3339(),
        opened_by: "agent".to_string(),
        name: StdMutex::new("Agent".to_string()),
        alive: AtomicBool::new(true),
        exit_code: StdMutex::new(None),
        alive_tx,
        login_command: None,
        typed: AtomicBool::new(false),
        interrupt_first: AtomicBool::new(false),
        pty: None,
        last_call: StdMutex::new(String::new()),
        ring: StdMutex::new(RingBuffer::new()),
        output_tx,
    });
    reg.sessions
        .insert(handle.terminal_id.clone(), handle.clone());
    Some(handle)
}

/// Writes `text` to `stream_id`'s Agent terminal as part of `call`'s block:
/// under its `$ command` header, which goes first unless `call`'s was the
/// last block written. Every writer shares that one record — each seat of
/// the stream, and the `run` tool — and calls in flight at once take turns
/// on it, so output never lands under another command's header. `text` uses
/// plain `\n`; a terminal needs `\r\n`. `own_line` starts it on a fresh
/// line, so an exit marker never runs on from output that ended mid-line.
/// An empty `text` just opens the block.
pub(crate) fn agent_terminal_write(
    state: &AppState,
    stream_id: &str,
    call: &str,
    command: &str,
    text: &str,
    own_line: bool,
) {
    let Some(handle) = agent_terminal(state, stream_id) else {
        return;
    };
    handle.write_call(call, command, text, own_line);
}

/// `text` for a terminal: its plain `\n` line ends as `\r\n`.
fn push_terminal_lines(out: &mut String, text: &str) {
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            out.push_str("\r\n");
        }
        out.push_str(line.strip_suffix('\r').unwrap_or(line));
    }
}

#[derive(Default)]
pub(crate) struct TerminalRegistry {
    sessions: HashMap<String, Arc<TerminalHandle>>,
}

impl TerminalRegistry {
    pub(crate) fn get(&self, terminal_id: &str) -> Option<Arc<TerminalHandle>> {
        self.sessions.get(terminal_id).cloned()
    }

    pub(crate) fn list_for_stream(&self, stream_id: &str) -> Vec<TerminalInfo> {
        let mut out: Vec<TerminalInfo> = self
            .sessions
            .values()
            .filter(|h| h.stream_id == stream_id)
            .map(|h| h.info())
            .collect();
        out.sort_by(|a, b| a.created_ts.cmp(&b.created_ts));
        out
    }

    /// The live system login terminal for `command`, if one already exists —
    /// identified by [`TerminalHandle::login_command`] (never `name`, which a
    /// human or another tab can freely rename). Callers wanting this to be
    /// race-free must hold `AppState::system_login_lock` for the whole
    /// check-then-maybe-spawn section, not just this lookup.
    pub(crate) fn find_live_login(
        &self,
        stream_id: &str,
        command: bakery_agent::cli_tools::LoginCommand,
    ) -> Option<Arc<TerminalHandle>> {
        self.sessions
            .values()
            .find(|h| h.stream_id == stream_id && h.login_command == Some(command) && h.is_alive())
            .cloned()
    }

    /// A free default name for a new terminal of `mode` on `stream_id`:
    /// the bare mode name if untaken, else `{mode}-2`, `{mode}-3`, ...
    fn free_name(&self, stream_id: &str, mode: &str) -> String {
        let names: Vec<String> = self
            .sessions
            .values()
            .filter(|h| h.stream_id == stream_id)
            .map(|h| h.name.lock().unwrap().clone())
            .collect();
        if !names.iter().any(|n| n == mode) {
            return mode.to_string();
        }
        let mut n = 2;
        loop {
            let candidate = format!("{mode}-{n}");
            if !names.iter().any(|existing| existing == &candidate) {
                return candidate;
            }
            n += 1;
        }
    }
}

fn random_id() -> String {
    let mut buf = [0u8; 12];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut buf))
        .expect("/dev/urandom must be readable");
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// Kills every process sharing `leader_pid`'s session, not just its own
/// process group — an interactive shell on a real pty runs with job control
/// on, and each backgrounded job gets its own group specifically so it can
/// be foregrounded/backgrounded independently of the shell. A session id
/// equals its leader's pid by definition, so `getsid` on every live pid
/// (not `ps`'s platform-specific "sess" column, which prints an opaque
/// kernel address on macOS, not a comparable id) finds them all. Falls back
/// to a plain process-group kill if enumeration itself fails — better an
/// incomplete kill than none.
pub(crate) fn kill_pty_session(leader_pid: u32) {
    let target_pid = leader_pid as libc::pid_t;
    let target_sid = unsafe { libc::getsid(target_pid) };
    let Ok(output) = std::process::Command::new("ps")
        .args(["-Ao", "pid="])
        .output()
    else {
        bakery_exec::kill_process_group(leader_pid);
        return;
    };
    if target_sid < 0 || !output.status.success() {
        bakery_exec::kill_process_group(leader_pid);
        return;
    }

    let mut killed_any = false;
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let Ok(pid) = line.trim().parse::<libc::pid_t>() else {
            continue;
        };
        if unsafe { libc::getsid(pid) } == target_sid {
            unsafe { libc::kill(pid, libc::SIGKILL) };
            killed_any = true;
        }
    }
    if !killed_any {
        bakery_exec::kill_process_group(leader_pid);
    }
}

/// Guarantees a spawned pty child dies exactly once, on every exit path —
/// early bail right after spawn, the ordinary end of a session, or an
/// explicit kill — by running in `Drop`.
struct KillChildOnDrop(Option<Box<dyn Child + Send + Sync>>);
impl KillChildOnDrop {
    /// Kills and reaps the child, returning its exit status when the wait
    /// itself succeeds — a no-op wait if it had already exited on its own.
    /// Idempotent: a second call (or `Drop` afterward) sees `self.0` already
    /// `None` and does nothing.
    fn kill_and_reap(&mut self) -> Option<portable_pty::ExitStatus> {
        let mut child = self.0.take()?;
        match child.process_id() {
            Some(pid) => kill_pty_session(pid),
            None => {
                let _ = child.kill();
            }
        }
        child.wait().ok()
    }
}
impl Drop for KillChildOnDrop {
    fn drop(&mut self) {
        self.kill_and_reap();
    }
}

/// Everything a caller (the `/terminal/create` route, or a test) needs to
/// hand this module to spawn a session — resolved program/args/cwd/mode are
/// its job, not this one's, exactly like the one-shot `env_pty`/`env_exec`
/// split already keeps that resolution logic in one place
/// (`routes::env::resolve_room`) for the argv-attach paths.
pub(crate) struct SpawnSpec {
    pub(crate) stream_id: String,
    pub(crate) name: Option<String>,
    pub(crate) mode: String,
    pub(crate) program: PathBuf,
    pub(crate) args: Vec<String>,
    pub(crate) cwd: PathBuf,
    /// Room mode's own unique Seatbelt profile — removed once the shell
    /// exits, same lifecycle as the one-shot attach paths already give it.
    pub(crate) profile_path: Option<PathBuf>,
    /// Set only for a system account login pane (07d-A3D1) — the terminal's
    /// true identity; see [`TerminalHandle::login_command`].
    pub(crate) login_command: Option<bakery_agent::cli_tools::LoginCommand>,
    /// `Some` narrows the child's environment to exactly these pairs (plus
    /// `TERM`, always set below) instead of the host/room default of
    /// inheriting bakery-node's own complete process environment — a
    /// Codex/Cursor login process has no business seeing this node's other
    /// providers' API keys (07d-A3D1 review finding 3). `None` preserves
    /// today's host/room behavior exactly.
    pub(crate) env: Option<Vec<(String, String)>>,
}

/// Spawns a real pty-backed shell and registers it under a fresh
/// `terminal_id`, admitted under `pending` (promoted to a killable
/// registration the instant a pid exists) exactly like the pre-07c
/// WS-per-pty model did — discard/burn fencing ([`fence_active_ptys`]) does
/// not need to know this session is now node-owned rather than
/// connection-owned, since it still finds it the same way.
pub(crate) fn spawn_terminal(
    state: &AppState,
    spec: SpawnSpec,
    pending: crate::state::PtyPendingGuard,
) -> Result<TerminalInfo, String> {
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| format!("openpty: {e}"))?;

    let mut cmd = CommandBuilder::new(&spec.program);
    cmd.args(&spec.args);
    cmd.cwd(&spec.cwd);
    if let Some(pairs) = &spec.env {
        // `CommandBuilder::new` seeds a full copy of this process's own
        // environment (`portable_pty::cmdbuilder::get_base_env` calls
        // `std::env::vars_os()`) — `env_clear` drops that inherited copy
        // entirely before the narrow allowlist below is set.
        cmd.env_clear();
        for (k, v) in pairs {
            cmd.env(k, v);
        }
    }
    cmd.env("TERM", "xterm-256color");

    let child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| format!("spawn: {e}"))?;
    drop(pair.slave); // belongs to the child now; holding it open keeps the pty alive past exit

    let Some(pid) = child.process_id() else {
        // KillChildOnDrop kills+reaps on drop even without a registered pid.
        let _guard = KillChildOnDrop(Some(child));
        return Err("spawned child has no pid — session refused".to_string());
    };

    // Guarded immediately: `try_clone_reader`/`take_writer` below can still
    // fail after a real pid exists, and portable_pty's bare `Child` does not
    // kill on drop. An early `?` return here must not orphan a live process
    // outside every kill path — the pid isn't registered yet, so neither
    // explicit close nor `fence_active_ptys` on discard could ever find it.
    let child_guard = KillChildOnDrop(Some(child));

    let reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| format!("clone reader: {e}"))?;
    let writer = pair
        .master
        .take_writer()
        .map_err(|e| format!("take writer: {e}"))?;

    let terminal_id = random_id();
    let name = {
        let reg = state.terminals.lock().unwrap();
        spec.name
            .unwrap_or_else(|| reg.free_name(&spec.stream_id, &spec.mode))
    };
    let (output_tx, _rx) = broadcast::channel::<Chunk>(1024);
    let (alive_tx, _alive_rx) = watch::channel(true);
    let handle = Arc::new(TerminalHandle {
        terminal_id: terminal_id.clone(),
        stream_id: spec.stream_id.clone(),
        mode: spec.mode.clone(),
        cwd: spec.cwd.to_string_lossy().into_owned(),
        created_ts: chrono::Utc::now().to_rfc3339(),
        opened_by: crate::actor::attribute("human"),
        name: StdMutex::new(name),
        alive: AtomicBool::new(true),
        exit_code: StdMutex::new(None),
        alive_tx,
        login_command: spec.login_command,
        typed: AtomicBool::new(false),
        interrupt_first: AtomicBool::new(false),
        pty: Some(PtyIo {
            pid,
            writer: StdMutex::new(Box::new(writer)),
            master: StdMutex::new(pair.master),
        }),
        last_call: StdMutex::new(String::new()),
        ring: StdMutex::new(RingBuffer::new()),
        output_tx,
    });

    state
        .terminals
        .lock()
        .unwrap()
        .sessions
        .insert(terminal_id.clone(), handle.clone());
    let info = handle.info();

    // Promoted from `pending` (admitted atomically with the retiring/
    // discarded — and for host mode, the burn — check, before this ever
    // spawned) to a real, killable registration now that a pid exists.
    // Held by the background task below for the session's *entire* life,
    // not just one connection — that's the actual ownership inversion.
    let pty_session_guard = pending.promote(pid);

    // Blocking OS-thread read loop: `Read` on a pty master has no async
    // story here. Exits on its own once the pty closes (EOF).
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
    std::thread::spawn(move || {
        let mut reader = reader;
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.blocking_send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });

    struct RemoveProfileOnDrop(Option<PathBuf>);
    impl Drop for RemoveProfileOnDrop {
        fn drop(&mut self) {
            if let Some(p) = &self.0 {
                let _ = std::fs::remove_file(p);
            }
        }
    }

    let app_state = state.clone();
    let handle_for_task = handle.clone();
    let profile_path = spec.profile_path;
    tokio::spawn(async move {
        let child_guard = child_guard;
        let _pty_session_guard = pty_session_guard;
        let _remove_profile = RemoveProfileOnDrop(profile_path);
        while let Some(bytes) = rx.recv().await {
            handle_for_task.push_output(bytes);
        }
        // kill+reap — a no-op wait if the child already exited on its own —
        // captured explicitly (rather than left to `child_guard`'s `Drop`)
        // so an attached WS's close reason can name the real exit code.
        let mut child_guard = child_guard;
        if let Some(status) = child_guard.kill_and_reap() {
            *handle_for_task.exit_code.lock().unwrap() = Some(status.exit_code());
        }
        handle_for_task.alive.store(false, Ordering::Release);
        let _ = handle_for_task.alive_tx.send(false);
        app_state
            .terminals
            .lock()
            .unwrap()
            .sessions
            .remove(&handle_for_task.terminal_id);
    });

    Ok(info)
}

/// Explicit close: kills this one terminal's process tree and waits
/// (bounded) for its background task to confirm it and deregister, then
/// removes it from the registry regardless (a session that failed to
/// self-deregister within the deadline must not linger as a phantom "still
/// running" row). Detaching a WebSocket view must never call this — only an
/// explicit human close action, or [`fence_active_ptys`] on discard.
/// How long a terminal closed with Ctrl-C first gets to end on its own.
const INTERRUPT_WAIT: Duration = Duration::from_secs(300);

pub(crate) async fn kill_terminal(state: &AppState, terminal_id: &str) -> Result<(), String> {
    let Some(handle) = state.terminals.lock().unwrap().get(terminal_id) else {
        return Err(format!("no such terminal: {terminal_id}"));
    };
    let Some(pty) = &handle.pty else {
        handle.end_without_process(state);
        return Ok(());
    };
    if handle.interrupt_first.load(Ordering::Acquire) && handle.is_alive() {
        // Ctrl-C, as typed in its pane: `container build` cancels its build
        // and removes its export (gigabytes, in Apple container's own folder)
        // on one, where a kill leaves it behind. It ends on its own; a kill
        // follows only if it hasn't within INTERRUPT_WAIT.
        unsafe { libc::kill(-(pty.pid as libc::pid_t), libc::SIGINT) };
        let (state, id) = (state.clone(), terminal_id.to_string());
        tokio::spawn(async move {
            let deadline = Instant::now() + INTERRUPT_WAIT;
            while handle.is_alive() && Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            if let Some(pty) = handle.pty.as_ref().filter(|_| handle.is_alive()) {
                kill_pty_session(pty.pid);
            }
            let deadline = Instant::now() + Duration::from_secs(5);
            while handle.is_alive() && Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            state.terminals.lock().unwrap().sessions.remove(&id);
        });
        return Ok(());
    }
    kill_pty_session(pty.pid);
    let deadline = Instant::now() + Duration::from_secs(5);
    while handle.is_alive() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    state.terminals.lock().unwrap().sessions.remove(terminal_id);
    Ok(())
}

/// Kills every terminal owned by `stream_id`, optionally scoped to one mode.
/// Called by socket discard (every mode, before filesystem cleanup) and by a
/// `Living` → `Burn` lifetime transition (host-mode only). Reuses the exact
/// pid-tracking [`crate::state::Lifecycle::activity`] already gives every
/// interactive pty — a node-owned terminal is admitted through
/// [`crate::state::admit_pty`] exactly like the old WS-per-pty model was, so
/// this fence needs no terminal-registry-specific logic to find them.
pub(crate) async fn fence_active_ptys(
    state: &AppState,
    stream_id: &str,
    only_mode: Option<&str>,
) -> bool {
    fence_active_ptys_with_deadline(state, stream_id, only_mode, Duration::from_secs(5)).await
}

/// Split out so a test proving the timeout-abort path itself doesn't have to
/// wait out the real deadline to do it.
pub(crate) async fn fence_active_ptys_with_deadline(
    state: &AppState,
    stream_id: &str,
    only_mode: Option<&str>,
    timeout: Duration,
) -> bool {
    // A discard also ends the Agent terminal, which has no pid to fence.
    if only_mode.is_none() {
        let agent = state
            .terminals
            .lock()
            .unwrap()
            .sessions
            .values()
            .find(|h| h.stream_id == stream_id && h.pty.is_none())
            .cloned();
        if let Some(agent) = agent {
            agent.end_without_process(state);
        }
    }
    let deadline = Instant::now() + timeout;
    loop {
        let (pending, pids): (u32, Vec<u32>) = {
            let g = state.lifecycle.lock().unwrap();
            match g.activity.get(stream_id) {
                Some(a) => {
                    let pending = match only_mode {
                        None => a.pending_for("host") + a.pending_for("room"),
                        Some(m) => a.pending_for(m),
                    };
                    let pids = a
                        .ptys
                        .iter()
                        .filter(|s| only_mode.is_none_or(|m| s.mode == m))
                        .map(|s| s.pid)
                        .collect();
                    (pending, pids)
                }
                None => (0, Vec::new()),
            }
        };
        for pid in &pids {
            kill_pty_session(*pid);
        }
        if pids.is_empty() && pending == 0 {
            // Registry entries for any pids just killed self-deregister
            // asynchronously as their background tasks observe the pty
            // close; nothing left for this fence to remove directly.
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pane closed with Ctrl-C first (a build's) is interrupted, cleans up
    /// as its program does on an interrupt, and ends on its own; a plain
    /// close would have killed it before it could.
    #[tokio::test]
    async fn a_pane_closed_with_an_interrupt_cleans_up_before_it_ends() {
        use crate::testutil::state_with;
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(bakery_agent::StubModel));
        let cleaned = dir.path().join("cleaned");
        let script = format!("trap 'touch {}; exit 0' INT; echo ready; while :; do sleep 0.1; done", cleaned.display());
        let pending = crate::state::admit_pty(&state, SYSTEM_ACCOUNTS_STREAM_ID, "host").unwrap();
        let spec = SpawnSpec {
            stream_id: SYSTEM_ACCOUNTS_STREAM_ID.to_string(),
            name: Some("build".to_string()),
            mode: "host".to_string(),
            program: PathBuf::from("/bin/sh"),
            args: vec!["-c".to_string(), script],
            cwd: dir.path().to_path_buf(),
            profile_path: None,
            login_command: None,
            env: Some(vec![("PATH".to_string(), "/usr/bin:/bin".to_string())]),
        };
        let info = spawn_terminal(&state, spec, pending).unwrap();
        let handle = state.terminals.lock().unwrap().get(&info.terminal_id).unwrap();
        handle.interrupt_first();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while !String::from_utf8_lossy(&handle.replay_since(0).0).contains("ready") {
            assert!(std::time::Instant::now() < deadline, "the pane never started");
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        kill_terminal(&state, &info.terminal_id).await.unwrap();
        while handle.is_alive() {
            assert!(std::time::Instant::now() < deadline, "the pane never ended");
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(cleaned.exists(), "it cleaned up on the interrupt");
        assert_eq!(handle.exit_code(), Some(0));
    }

    #[test]
    fn a_terminals_output_reads_as_text() {
        let raw = b"\x1b]0;title\x07\x1b[1;32m$ \x1b[0mls\r\nfile.txt\r\n\x1b(B";
        assert_eq!(plain(raw), "$ ls\nfile.txt\n");
    }

    fn ring_with(bytes: &[&[u8]]) -> RingBuffer {
        let mut r = RingBuffer::new();
        for b in bytes {
            r.push(b);
        }
        r
    }

    #[test]
    fn ring_buffer_replays_everything_from_offset_zero() {
        let r = ring_with(&[b"hello ", b"world"]);
        let (bytes, truncated, end) = {
            let (b, t) = r.since(0);
            (b, t, r.end_offset())
        };
        assert_eq!(bytes, b"hello world");
        assert!(!truncated);
        assert_eq!(end, 11);
    }

    #[test]
    fn ring_buffer_replays_only_the_tail_after_a_mid_offset() {
        let r = ring_with(&[b"hello ", b"world"]);
        let (bytes, truncated) = r.since(6);
        assert_eq!(bytes, b"world");
        assert!(!truncated);
    }

    #[test]
    fn ring_buffer_reports_truncation_past_capacity() {
        let mut r = RingBuffer::new();
        let chunk = vec![b'x'; RING_CAPACITY / 2];
        r.push(&chunk);
        r.push(&chunk);
        r.push(&chunk); // now 1.5x capacity — the first chunk was evicted
        let (bytes, truncated) = r.since(0);
        assert!(
            truncated,
            "asking from offset 0 after eviction must report truncation"
        );
        assert_eq!(
            bytes.len(),
            RING_CAPACITY,
            "retained bytes are capped at RING_CAPACITY"
        );
    }

    #[test]
    fn ring_buffer_does_not_report_truncation_for_a_still_retained_offset() {
        let mut r = RingBuffer::new();
        let chunk = vec![b'x'; RING_CAPACITY / 2];
        r.push(&chunk); // offset 0..half
        r.push(&chunk); // offset half..full — nothing evicted yet
        let (_bytes, truncated) = r.since(0);
        assert!(
            !truncated,
            "nothing was evicted yet — offset 0 is still fully retained"
        );
    }

    /// A stream's Agent terminal is a read-only session the node writes:
    /// listed with the stream's terminals, terminal line endings, its own
    /// lines kept apart from output, typing ignored, and Close ends it.
    #[tokio::test]
    async fn the_agent_terminal_shows_what_the_node_writes_and_closes_cleanly() {
        use crate::testutil::*;
        use bakery_agent::StubModel;

        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let write = |text: &str, own_line| {
            agent_terminal_write(&state, "wt:agent", "call-1", "cargo test", text, own_line)
        };
        write("", true);
        write("running 3 tests\nok", false);
        write("[exit 0]\n", true);

        let listed = state.terminals.lock().unwrap().list_for_stream("wt:agent");
        assert_eq!(listed.len(), 1, "one Agent terminal per stream");
        assert_eq!(listed[0].mode, AGENT_TERMINAL_MODE);
        assert_eq!(listed[0].name, "Agent");
        let handle = state
            .terminals
            .lock()
            .unwrap()
            .get(&listed[0].terminal_id)
            .unwrap();
        handle.write_input(b"typed\n").unwrap();
        let (bytes, truncated, _) = handle.replay_since(0);
        assert!(!truncated);
        assert_eq!(shown(&bytes), "$ cargo test\r\nrunning 3 tests\r\nok\r\n[exit 0]\r\n");
        // Its block opens with its call's mark, which reads as nothing.
        let raw = String::from_utf8(bytes.clone()).unwrap();
        assert!(raw.starts_with("\u{1b}]7766;call-1\u{7}$ cargo test\r\n"), "{raw:?}");
        assert_eq!(plain(&bytes), "$ cargo test\nrunning 3 tests\nok\n[exit 0]\n");

        kill_terminal(&state, &listed[0].terminal_id).await.unwrap();
        assert!(!handle.is_alive(), "attached views are told it ended");
        assert!(state
            .terminals
            .lock()
            .unwrap()
            .list_for_stream("wt:agent")
            .is_empty());
    }

    /// Calls running side by side — on one seat, on two, or a `run` beside a
    /// runtime's own command — take turns on one Agent terminal: output lands
    /// under its own call's `$ command` header, printed again whenever another
    /// call's block was written since, and a header never runs on from
    /// output that ended mid-line.
    #[test]
    fn output_lands_under_its_own_calls_header_whoever_wrote_last() {
        use crate::testutil::*;
        use bakery_agent::StubModel;

        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let write = |call: &str, command: &str, text: &str, own_line: bool| {
            agent_terminal_write(&state, "wt:blocks", call, command, text, own_line)
        };

        write("a", "cargo test", "", true);
        write("b", "ls", "", true);
        write("b", "ls", "one\n", false);
        write("a", "cargo test", "running 2 tests\n", false);
        write("b", "ls", "[exit 0]\n", true);
        write("a", "cargo test", "ok", false);
        write("a", "cargo test", "[exit 0]\n", true);
        write("a", "cargo test", "tail", false);
        write("b", "ls", "x\n", false);
        // Nothing to say and its block already last: nothing is written.
        write("b", "ls", "", true);

        let id = state.terminals.lock().unwrap().list_for_stream("wt:blocks")[0]
            .terminal_id
            .clone();
        let handle = state.terminals.lock().unwrap().get(&id).unwrap();
        let (bytes, _, _) = handle.replay_since(0);
        assert_eq!(
            shown(&bytes),
            [
                "$ cargo test\n",
                "$ ls\n",
                "one\n",
                "$ cargo test\nrunning 2 tests\n",
                "$ ls\n[exit 0]\n",
                "$ cargo test\nok\n[exit 0]\n",
                "tail\n$ ls\nx\n",
            ]
            .concat()
            .replace('\n', "\r\n")
        );
        // Every header opens with its own call's mark, at the start of its line.
        let raw = String::from_utf8(bytes).unwrap();
        let marks: Vec<&str> = raw.split("\u{1b}]7766;").skip(1).map(|m| &m[..m.find('\u{7}').unwrap()]).collect();
        assert_eq!(marks, ["a", "b", "a", "b", "a", "b"]);
        assert!(raw.contains("tail\r\n\u{1b}]7766;b\u{7}$ ls\r\n"), "{raw:?}");
    }

    /// A view attaching gets each byte once, in its replay or live after it,
    /// each live chunk saying where it starts; one that asks from an offset
    /// the ring no longer keeps learns how much is gone.
    #[test]
    fn a_view_gets_each_byte_once_and_learns_what_was_dropped() {
        use crate::testutil::*;
        use bakery_agent::StubModel;

        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let handle = agent_terminal(&state, "wt:offsets").unwrap();
        handle.push_output(b"hello ".to_vec());
        let (mut live, replay) = handle.attach_since(0);
        assert_eq!((replay.start, replay.bytes.as_slice(), replay.dropped), (0, b"hello ".as_slice(), 0));
        handle.push_output(b"world".to_vec());
        let chunk = live.try_recv().unwrap();
        assert_eq!((chunk.start, &*chunk.bytes), (6, b"world".as_slice()));
        assert!(live.try_recv().is_err(), "nothing twice");

        // Resuming from where it left off replays the rest, and nothing more.
        let resumed = handle.kept_since(6);
        assert_eq!((resumed.start, resumed.bytes.as_slice(), resumed.dropped), (6, b"world".as_slice(), 0));
        assert!(handle.kept_since(11).bytes.is_empty());

        // Past the ring's capacity the oldest bytes go, and saying so is exact.
        let mut ring = RingBuffer::new();
        ring.push(&vec![b'x'; RING_CAPACITY]);
        ring.push(b"tail");
        let late = ring.replay(0);
        assert_eq!((late.start, late.dropped, late.bytes.len()), (4, 4, RING_CAPACITY));
        assert!(late.bytes.ends_with(b"tail"));
        let beyond = ring.replay(ring.end_offset() + 10);
        assert_eq!((beyond.start, beyond.dropped, beyond.bytes.len()), (ring.end_offset(), 0, 0));
    }

    #[test]
    fn free_name_defaults_to_bare_mode_then_numbers() {
        let reg = TerminalRegistry::default();
        assert_eq!(reg.free_name("wt:x", "host"), "host");
    }

    /// 03c bug 2: a `return` between a successful spawn and the main loop
    /// used to leak the child. Proves `KillChildOnDrop` alone — no WS, no
    /// registry — kills a whole session (leader + a backgrounded,
    /// HUP-ignoring descendant) the instant it drops, exactly the moment an
    /// early bail after spawn (`try_clone_reader`/`take_writer` failing)
    /// would trigger it.
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs a macOS PTY/shell path")]
    fn kill_child_on_drop_kills_a_backgrounded_hup_ignoring_descendant_immediately() {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        let mut cmd = CommandBuilder::new("/bin/zsh");
        cmd.arg("-i");
        cmd.env("TERM", "xterm-256color");
        let child = pair.slave.spawn_command(cmd).unwrap();
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader().unwrap();
        let mut writer = pair.master.take_writer().unwrap();

        // Simulate "spawn succeeded, then the very next step failed" —
        // install the guard immediately, exactly like `spawn_terminal`.
        let guard = KillChildOnDrop(Some(child));

        writer
            .write_all(b"(trap '' HUP; sleep 30) & echo GOT_PID=$!\n")
            .unwrap();

        // The shell's output, read on a thread of its own, so a shell that
        // never prints the pid fails with what it did print instead of
        // hanging. An interactive zsh loads the operator's own setup first:
        // a long wait.
        let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        let mut out = String::new();
        let pid: libc::pid_t = loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            match rx.recv_timeout(left) {
                Ok(chunk) => out.push_str(&String::from_utf8_lossy(&chunk)),
                Err(_) => panic!("zsh never printed the backgrounded pid within 60 s; it printed: {out:?}"),
            }
            if let Some(line) = out.lines().find(|l| {
                l.rsplit("GOT_PID=")
                    .next()
                    .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()))
            }) {
                let digits: String = line
                    .rsplit("GOT_PID=")
                    .next()
                    .unwrap()
                    .chars()
                    .take_while(|c| c.is_ascii_digit())
                    .collect();
                if let Ok(p) = digits.parse() {
                    break p;
                }
            }
        };

        drop(guard);

        let mut alive = true;
        for _ in 0..30 {
            alive = unsafe { libc::kill(pid, 0) } == 0;
            if !alive {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(
            !alive,
            "the backgrounded descendant must be dead immediately after the guard drops"
        );
    }

    /// A fake `Child` whose `process_id()` is always `None` — portable-pty's
    /// real local spawns always report a pid on this platform, so a missing
    /// one can't be fabricated through a real pty; this proves the fallback
    /// path directly instead.
    #[derive(Debug)]
    struct FakeNoPidChild {
        killed: Arc<std::sync::atomic::AtomicBool>,
    }
    impl portable_pty::ChildKiller for FakeNoPidChild {
        fn kill(&mut self) -> std::io::Result<()> {
            self.killed.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
        fn clone_killer(&self) -> Box<dyn portable_pty::ChildKiller + Send + Sync> {
            Box::new(FakeNoPidChild {
                killed: self.killed.clone(),
            })
        }
    }
    impl portable_pty::Child for FakeNoPidChild {
        fn try_wait(&mut self) -> std::io::Result<Option<portable_pty::ExitStatus>> {
            Ok(Some(portable_pty::ExitStatus::with_exit_code(0)))
        }
        fn wait(&mut self) -> std::io::Result<portable_pty::ExitStatus> {
            Ok(portable_pty::ExitStatus::with_exit_code(0))
        }
        fn process_id(&self) -> Option<u32> {
            None
        }
    }

    /// 04c-BURN §3: "missing PID after spawn is fatal... kill what you
    /// can" — without a pid to fan a session-wide kill out from,
    /// `KillChildOnDrop` must fall back to the child's own (pid-less)
    /// `kill()` rather than silently doing nothing.
    #[test]
    fn kill_child_on_drop_falls_back_to_child_kill_when_no_pid_is_available() {
        let killed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let child: Box<dyn portable_pty::Child + Send + Sync> = Box::new(FakeNoPidChild {
            killed: killed.clone(),
        });
        drop(KillChildOnDrop(Some(child)));
        assert!(
            killed.load(std::sync::atomic::Ordering::SeqCst),
            "a missing pid must still trigger child.kill(), not silently do nothing"
        );
    }

    /// 07d-A3D1: `kill_and_reap` must hand back the real exit status instead
    /// of `Drop` silently swallowing it — that's what lets an attached WS
    /// name a real exit code. Idempotent: a second call after the child is
    /// taken returns `None`, and `Drop` afterward is then a no-op, exactly
    /// as before this change.
    #[test]
    fn kill_and_reap_returns_the_exit_status_instead_of_discarding_it() {
        let killed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let child: Box<dyn portable_pty::Child + Send + Sync> = Box::new(FakeNoPidChild {
            killed: killed.clone(),
        });
        let mut guard = KillChildOnDrop(Some(child));

        let status = guard.kill_and_reap();

        assert_eq!(status.map(|s| s.exit_code()), Some(0));
        assert!(killed.load(std::sync::atomic::Ordering::SeqCst));
        assert!(
            guard.kill_and_reap().is_none(),
            "a second call must be a no-op, not double-kill"
        );
    }

    /// 04d-GHOSTTY-LEASE §1: `ghostty_open` holds the exact same host-launch
    /// reservation a host terminal create does, through `spawn()` and no
    /// further — simulated here without a real `ghostty` binary by
    /// admitting and holding, then dropping, exactly as the route itself
    /// does around its `Command::spawn()` call. A fence started while the
    /// reservation is held must not clear until it's released.
    #[tokio::test]
    async fn a_ghostty_shaped_host_lease_blocks_the_fence_until_dropped() {
        use crate::routes::project::open_stream;
        use crate::testutil::*;
        use bakery_agent::StubModel;

        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("ghostty"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        open_stream(&state, &open_req(&repo, "ghostty")).unwrap();

        let lease = crate::state::admit_pty(&state, "wt:ghostty", "host").unwrap();

        assert!(
            !fence_active_ptys_with_deadline(
                &state,
                "wt:ghostty",
                None,
                Duration::from_millis(200)
            )
            .await,
            "a held launch reservation must block the fence until it's released"
        );

        drop(lease); // `ghostty_open` drops its lease the instant spawn() returns
        assert!(
            fence_active_ptys_with_deadline(&state, "wt:ghostty", None, Duration::from_millis(200))
                .await,
            "the fence must clear immediately once the reservation is released"
        );
    }
}
