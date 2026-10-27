//! Shared daemon state (`AppState`) and the handful of Heart-adjacent helpers
//! (`append`, `known_streams`, `context_for`, ...) every route module needs.
//! Split out of `main.rs`; see that file's crate doc for the daemon's scope.

use crate::node::{
    Lifetime, Seat, SeatAssignment, SeatSwitch, SocketKind, SocketKindRecord, WorkspaceSet,
    SEAT_ASSIGNED_FAMILY, SEAT_SWITCH_FAMILY, SOCKET_KIND_FAMILY, SOCKET_WORKSPACE_FAMILY,
};
use crate::node::{NodeConfig, OwnershipClaim, OWNERSHIP_FAMILY};
use axum::http::StatusCode;
use bakery_agent::Model;
use bakery_core::{Event, NewEvent};
use bakery_heart::Heart;
use libc;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::broadcast;

/// The exact browser origins the studio ever runs from — the single source
/// both the HTTP `CorsLayer` and the PTY WebSocket's own Origin check read,
/// so the two can never drift into checking different lists. Fixed at
/// compile time — every normal `pnpm dev` / `cargo run` / Tauri workflow
/// uses exactly one of these, never anything dynamic.
pub(crate) const ALLOWED_ORIGINS: &[&str] = &[
    "http://localhost:1420",
    "http://127.0.0.1:1420",
    "tauri://localhost",
    // The Electron shell's app origin (bakery-client/electron/main.ts).
    "bakery://app",
    // Vite-only preview if used without Tauri port
    "http://localhost:5173",
    "http://127.0.0.1:5173",
];

/// One extra origin from `BAKERY_ALLOWED_ORIGIN`, on top of the fixed
/// [`ALLOWED_ORIGINS`] list — exists solely so a Playwright e2e harness
/// running its own Vite dev server on a dynamically-chosen port can reach
/// this node without either (a) fighting over the fixed 1420 port with a
/// real running instance, or (b) this allowlist ever widening to a
/// wildcard. Narrowly validated: must be exactly `http://127.0.0.1:<port>`
/// or `http://localhost:<port>` — no `https://`, no other host, no path —
/// and rejected (with a stderr note — the caller then fails its own origin
/// check loudly, which is the right failure mode for a misconfigured test
/// harness) if it's anything else. Never cached: re-read on every call so
/// tests can set/unset it freely without process-wide static state to reset.
fn extra_allowed_origin_from_env() -> Option<String> {
    let raw = std::env::var("BAKERY_ALLOWED_ORIGIN").ok()?;
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let refuse = |reason: &str| {
        eprintln!("bakery-node: BAKERY_ALLOWED_ORIGIN {raw:?} refused — {reason}");
    };
    let Some(rest) = raw.strip_prefix("http://") else {
        refuse("only a plain http:// loopback origin is ever accepted (never https://, never a wildcard)");
        return None;
    };
    let Some((host, port)) = rest.split_once(':') else {
        refuse("expected exactly http://<host>:<port>, with an explicit port");
        return None;
    };
    if host != "127.0.0.1" && host != "localhost" {
        refuse("host must be 127.0.0.1 or localhost — never any other host, never a wildcard");
        return None;
    }
    if port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) {
        refuse("port must be a plain number from 1-65535");
        return None;
    }
    let Ok(port_num) = port.parse::<u16>() else {
        refuse("port must be a plain number from 1-65535");
        return None;
    };
    // `0` parses fine as a `u16` but names no real listener — it's a
    // bind()-only "OS picks one" sentinel, never a port anything is actually
    // reachable on, so it must be refused here just like any other bogus
    // value (07d-A3D1 e2e-harness review, port-zero finding).
    if port_num == 0 {
        refuse("port must be a plain number from 1-65535, not 0");
        return None;
    }
    Some(format!("http://{host}:{port}"))
}

/// The full origin allowlist for this run: [`ALLOWED_ORIGINS`] plus, when
/// `BAKERY_ALLOWED_ORIGIN` names one, exactly one extra loopback origin —
/// see [`extra_allowed_origin_from_env`]. Both the `CorsLayer` (`main.rs`)
/// and [`origin_allowed`] read this so they can never drift apart.
pub(crate) fn allowed_origins() -> Vec<String> {
    let mut out: Vec<String> = ALLOWED_ORIGINS.iter().map(|s| s.to_string()).collect();
    if let Some(extra) = extra_allowed_origin_from_env() {
        if !out.iter().any(|o| o == &extra) {
            out.push(extra);
        }
    }
    out
}

/// `None` (no `Origin` header at all) is a non-browser caller — a browser
/// always sends `Origin` on a WebSocket handshake, same-origin included, so
/// there is no legitimate browser case this treats too leniently. Whatever
/// non-browser caller sent it still has to clear a ticket check right after
/// this, so a missing header is never sufficient on its own.
pub(crate) fn origin_allowed(headers: &axum::http::HeaderMap) -> bool {
    match headers.get(axum::http::header::ORIGIN) {
        Some(v) => allowed_origins()
            .iter()
            .any(|a| v.as_bytes() == a.as_bytes()),
        None => true,
    }
}

/// What a parked turn learns when its pending operation is decided
/// (Checkpoint 2 item 2). One shape for all three pending kinds: a file
/// proposal decision (accept / edited accept / reject / write failure) and
/// an `ask_user` answer.
#[derive(Debug, Clone)]
pub(crate) enum DecisionOutcome {
    /// Proposal accepted and written. `final_content` is the bytes actually
    /// written when the human edited them first.
    Accepted {
        human_edited: bool,
        final_content: Option<String>,
    },
    Rejected,
    /// The accept was decided but the disk write failed.
    WriteFailed(String),
    /// `ask_user` answered by the human.
    Answered(String),
    /// The operator stopped the turn while this operation was awaiting a
    /// human decision. This is deliberately distinct from `Rejected`: a
    /// stopped turn must never feed a synthetic rejection back into a model
    /// that is meant to terminate instead.
    Cancelled,
}

/// Park a turn on `event_hash` — returns the receiver the loop blocks on.
/// Wakes are advisory (see `decision_waiters`' doc); the caller re-derives
/// from Heart on every timeout tick too.
pub(crate) fn register_decision_waiter(
    st: &AppState,
    event_hash: &str,
) -> std::sync::mpsc::Receiver<DecisionOutcome> {
    let (tx, rx) = std::sync::mpsc::channel();
    st.decision_waiters
        .lock()
        .unwrap()
        .insert(event_hash.to_string(), tx);
    rx
}

pub(crate) fn unregister_decision_waiter(st: &AppState, event_hash: &str) {
    st.decision_waiters.lock().unwrap().remove(event_hash);
}

/// Notify (and consume) the waiter for `event_hash`, if any. Returns
/// whether a live turn was actually waiting — decision routes use that to
/// pick the honest activity phase (a waiting turn continues; a dead one's
/// proposal decision is terminal).
pub(crate) fn notify_decision(st: &AppState, event_hash: &str, outcome: DecisionOutcome) -> bool {
    let waiter = st.decision_waiters.lock().unwrap().remove(event_hash);
    match waiter {
        Some(tx) => tx.send(outcome).is_ok(),
        None => false,
    }
}

/// The operator token this process enforces: `BAKERY_OPERATOR_TOKEN` env
/// (trimmed, non-empty) wins for one process — the Playwright harness and
/// side-by-side smoke nodes need a knowable token without touching the
/// operator's real `node.json` — else the persisted `node.json` token
/// (minted at first load, so this is only ever empty if someone hand-edited
/// the file to an empty string; the auth layer then fails closed, matching
/// nothing).
pub(crate) fn resolve_operator_token(node: &crate::node::NodeConfig) -> String {
    match std::env::var("BAKERY_OPERATOR_TOKEN") {
        Ok(t) if !t.trim().is_empty() => t.trim().to_string(),
        _ => node.operator_token.clone().unwrap_or_default(),
    }
}

/// How long a minted `/events` ticket stays redeemable — same budget as the
/// terminal's.
pub(crate) const EVENTS_TICKET_TTL: std::time::Duration = std::time::Duration::from_secs(30);

/// 24 random bytes, hex — the same shape `routes::terminal::random_ticket`
/// mints for `/terminal/ws`.
pub(crate) fn mint_ws_ticket_string() -> String {
    use std::io::Read;
    let mut buf = [0u8; 24];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut buf))
        .expect("/dev/urandom must be readable");
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

pub(crate) fn mint_events_ticket(st: &AppState, ticket: String) {
    let mut tickets = st.events_tickets.lock().unwrap();
    let now = std::time::Instant::now();
    tickets.retain(|_, expires| *expires > now);
    tickets.insert(ticket, now + EVENTS_TICKET_TTL);
}

/// One-time: a matched ticket is burned; an expired or unknown one simply
/// fails. Same discipline as `consume_terminal_ticket`.
pub(crate) fn consume_events_ticket(st: &AppState, ticket: &str) -> bool {
    let mut tickets = st.events_tickets.lock().unwrap();
    let now = std::time::Instant::now();
    tickets.retain(|_, expires| *expires > now);
    tickets.remove(ticket).is_some()
}

/// A one-time, short-lived credential for opening `/terminal/ws`. Minted only
/// over `POST /terminal/ticket`, which sits behind the same CORS layer as
/// every other route — a page on a disallowed origin can still fire that
/// POST, but the browser hides its response body from that page's script,
/// so the ticket value itself never reaches hostile JS. WebSocket upgrades
/// are not subject to CORS at all, which is exactly the hole this closes:
/// without a ticket, any open tab could `new WebSocket(...)` straight into
/// The operator's login shell. Scoped to one `terminal_id`, not just a stream/mode —
/// a named terminal is a specific long-lived session, not a fresh spawn per
/// attach (07c-STUDIO-SHELL §3).
pub(crate) struct TerminalTicket {
    pub(crate) stream_id: String,
    pub(crate) terminal_id: String,
    pub(crate) expires_at: Instant,
}

/// One live interactive PTY session with a real, killable pid —
/// `crate::terminals::fence_active_ptys` reads these to kill and wait for
/// every session on a stream before discard (or a burn transition, host-mode
/// only) touches the filesystem out from under a shell that's still running.
/// `pid` is the pty leader's own pid, which is also its process group and
/// session id (portable-pty's pty allocation makes it a session leader) —
/// enough to kill the whole tree.
pub(crate) struct PtySession {
    pub(crate) pid: u32,
    pub(crate) mode: String,
}

/// Everything discard needs to know about "is anyone still attached" for one
/// stream, behind the one lock in [`Lifecycle`]. Unifying one-shot and
/// interactive bookkeeping here (rather than the two separate sets/maps
/// 04-LIFECYCLE used) is what makes admitting a new attach and starting a
/// discard mutually exclusive instead of a check-then-act race
/// (04b-FENCE §1: "discard vs attach is still fail-open").
#[derive(Default)]
pub(crate) struct StreamActivity {
    /// A one-shot `/env/provision`, `/env/exec`, or `/env/pty` call in
    /// flight — exclusive: a second concurrent one is refused outright
    /// rather than admitted alongside (two commands racing in one socket's
    /// writable runtime dir).
    pub(crate) one_shot: bool,
    /// Non-exclusive holds for the duration of `/agent/turn`,
    /// `/agent/accept`, `/agent/reject`, and `/files/*` — many of these can
    /// run concurrently (two file reads, a turn alongside a read) with
    /// nothing to mutually exclude, but discard still must not rm the
    /// worktree out from under a long-running Grok turn (04c-BURN §4).
    pub(crate) leases: u32,
    /// Interactive PTY attaches admitted (under the lock, atomically with
    /// the retiring/discarded — and for `host`, the burn — check) but not
    /// yet spawned, or already failed to spawn. No pid exists yet to kill,
    /// but a fence must still wait for this to clear: the pty that's about
    /// to exist was admitted before the fence started, so it's owed the
    /// same wait a spawned one gets. Tracked per mode (04c-BURN §1): a
    /// burn transition's fence only needs to wait on `host`, and must not
    /// hold up an unrelated `room` attach that happens to be mid-spawn, but
    /// it must NOT ignore pending the way a bare stream-wide count would.
    pub(crate) pty_pending_host: u32,
    pub(crate) pty_pending_room: u32,
    /// Spawned interactive sessions with a real, killable pid.
    pub(crate) ptys: Vec<PtySession>,
    /// Boxed ACP children admitted but not yet spawned (handshake in flight).
    /// Discard waits for these to clear exactly as it does for pty_pending.
    pub(crate) acp_pending: u32,
    /// Spawned ACP children with a real, killable pid — one per (stream, seat).
    pub(crate) acp_pids: Vec<u32>,
}

impl StreamActivity {
    fn pending_mut(&mut self, mode: &str) -> &mut u32 {
        if mode == "host" {
            &mut self.pty_pending_host
        } else {
            &mut self.pty_pending_room
        }
    }

    pub(crate) fn pending_for(&self, mode: &str) -> u32 {
        if mode == "host" {
            self.pty_pending_host
        } else {
            self.pty_pending_room
        }
    }

    fn is_empty(&self) -> bool {
        !self.one_shot
            && self.leases == 0
            && self.pty_pending_host == 0
            && self.pty_pending_room == 0
            && self.ptys.is_empty()
            && self.acp_pending == 0
            && self.acp_pids.is_empty()
    }
}

/// The one lock every attach route and discard/set_lifetime share, so
/// "can I attach right now" and "start retiring this stream" can never
/// interleave (04b-FENCE §1–§2). A single `std::sync::Mutex` rather than a
/// per-stream one: every critical section here is a handful of map
/// operations plus (at most) one Heart read/append, never an `.await` —
/// contention across unrelated streams is not worth the bookkeeping a
/// per-stream lock table would add.
#[derive(Default)]
pub(crate) struct Lifecycle {
    /// Streams whose native runtimes must restart as soon as their last
    /// running turn ends — a Machine access change that landed mid-turn (see
    /// [`restart_acp_runtimes_when_idle`]).
    pub(crate) restart_when_idle: HashSet<String>,
    /// Stream ids currently mid-discard, from the moment `begin_discard`
    /// commits to it until the `stream.discarded` event has landed. Checked
    /// under this same lock by every admission function, closing the window
    /// between "discard started" and "the event actually landed" that
    /// `stream_discarded` alone (a Heart read) cannot see.
    pub(crate) retiring: HashSet<String>,
    pub(crate) activity: HashMap<String, StreamActivity>,
    /// Stream ids with a `Burn` transition's fence currently running, from
    /// the moment `begin_burn_transition` commits the event until the fence
    /// ends (success, timeout, or panic unwind). `set_non_burn_lifetime`
    /// refuses every other lifetime write while a stream is in this set (and
    /// refuses `Burn` unconditionally — only `begin_burn_transition` may set it).
    ///
    /// An earlier version of this tried to let writes land freely and have
    /// the fence *detect* one that raced it, via a generation counter
    /// checked once per poll iteration. That is not actually safe: the
    /// fence reads state under this lock, releases it, and only *then*
    /// calls `kill_pty_session` (a real subprocess spawn — not
    /// instantaneous) — a write can land in that gap, after the check has
    /// already passed, making the check-then-kill sequence itself racy no
    /// matter how many times it re-checks. Refusing the write outright
    /// while this set contains the stream removes the gap instead of
    /// shrinking it (04d-GHOSTTY-LEASE §2 audit: "avoid merely adding
    /// another unlocked generation check; that only moves the race").
    pub(crate) burn_fencing: HashSet<String>,
    /// Turns currently in flight, keyed `(stream_id, Seat)` — the per-agent
    /// "busy" fact `/studio/state` projects (07d-A: "one conversation +
    /// busy/run per agent_id, not per socket"). Counted, not a bare
    /// presence set: two overlapping turns for the same agent must not let
    /// the first one's completion clear busy while the second is still
    /// running.
    pub(crate) running_turns: HashMap<(String, Seat), u32>,
    /// A restart waiting for running turns (`service`).
    pub(crate) restart: Option<crate::service::Waiting>,
}

/// Shared, cloneable daemon state. The Heart holds a rusqlite `Connection`
/// (`Send` but not `Sync`), so it lives behind a `Mutex`; the guard is only ever
/// held across synchronous DB calls, never across an `.await`.
#[derive(Clone)]
pub(crate) struct AppState {
    pub(crate) heart: Arc<Mutex<Heart>>,
    /// Serialized `Event` JSON is broadcast to every connected WebSocket.
    pub(crate) tx: broadcast::Sender<String>,
    /// `~/.bakery` — where `node.json` lives; needed to persist `node` after
    /// a runtime change (e.g. `POST /policy/write-mode`).
    pub(crate) base: Arc<PathBuf>,
    /// Base directory under which worktrees are materialized.
    pub(crate) worktree_base: Arc<PathBuf>,
    /// Swappable model seam (hot-reloaded after Setup / login) — the
    /// node-global default the legacy `/auth/*` / `/policy/write-mode`
    /// surfaces still manage. `/agent/turn` no longer reads this: a turn
    /// resolves its seat's own model via [`resolve_seat`] instead
    /// (06-SEATS.md §2: "not a node-global `Arc<dyn Model>` forever").
    pub(crate) model: Arc<Mutex<Arc<dyn Model>>>,
    /// This machine's identity + fallback policy + write mode
    /// (`~/.bakery/node.json`). Mutable: `write_mode` can change at runtime.
    pub(crate) node: Arc<Mutex<NodeConfig>>,
    /// Live, unconsumed PTY WebSocket tickets, keyed by the ticket string
    /// itself. See [`PtyTicket`].
    pub(crate) pty_tickets: Arc<Mutex<HashMap<String, TerminalTicket>>>,
    /// Attach admission + discard fencing state. See [`Lifecycle`].
    pub(crate) lifecycle: Arc<Mutex<Lifecycle>>,
    /// Node-owned named terminal sessions (07c-STUDIO-SHELL §3) — the
    /// process lives here, not in any one WebSocket handler, so closing a
    /// pane or reloading the client only ever detaches a view; the shell
    /// keeps running until an explicit close or a socket discard kills it.
    pub(crate) terminals: Arc<Mutex<crate::terminals::TerminalRegistry>>,
    /// Resolved per-(stream, seat) models — a read-through cache over the
    /// durable `seat.model.assigned` Heart fact, populated lazily by
    /// [`resolve_seat`] and invalidated by [`assign_seat_model`] on
    /// reassignment. Also the seam that lets a test hand a turn an arbitrary
    /// `Arc<dyn Model>` test double (`ScriptedModel`, `LoopingModel`, ...) —
    /// those can't be named by a model-id string the way `grok`/`claude`/
    /// `gemini` can, so [`seed_seat_model_for_test`] inserts here directly.
    pub(crate) seats: Arc<Mutex<HashMap<(String, Seat), Arc<dyn Model>>>>,
    /// One-shot `allow_once` grants from a pipe plug's declared-tool gate
    /// (`pipe::decide_child_tool_gate`) — checked and consumed (removed) the
    /// next time that `(stream, kind)` pair comes up, unlike
    /// `node.always_kinds` and `node.always_grants`, which hold until removed.
    pub(crate) child_tool_allow_once: Arc<Mutex<HashSet<(String, crate::pipe::ChildToolKind)>>>,
    /// One persistent boxed ACP child per (stream_id, seat).  `AcpSlot::Spawning`
    /// means a session thread is mid-handshake; `Live` means the session is
    /// ready and `chat()` can send a prompt directly.  Keyed identically to
    /// `seats` so reassignment can evict both at once.
    pub(crate) acp_sessions: Arc<Mutex<HashMap<(String, crate::node::Seat), crate::pipe::AcpSlot>>>,
    /// Serializes "create a system account login terminal" (07d-A3D1) into
    /// one atomic check-for-an-existing-live-one-else-spawn section — a
    /// `tokio::sync::Mutex` (not `std`) so it can be held across the
    /// `.await` that actually spawns the pty, closing the race two
    /// concurrent "Sign in" clicks would otherwise have. Rare and
    /// human-triggered; a single global lock here is not a throughput
    /// concern.
    pub(crate) system_login_lock: Arc<tokio::sync::Mutex<()>>,
    /// Serializes permanent-Baker bootstrap (07d-B Phase 1) into one atomic
    /// check→mint→persist section, so two concurrent first opens converge
    /// on a single stream instead of each minting their own. A `std` mutex:
    /// the whole section is synchronous and runs inside `spawn_blocking`.
    pub(crate) baker_bootstrap_lock: Arc<Mutex<()>>,
    /// Serialize every human-gated operation (proposal, `ask_user`, and
    /// child-tool permission), including Auto, through check/write/event.
    /// Stop uses the same lock to terminalize the exact pending gate before
    /// a late browser action can resume it.
    pub(crate) edit_decision_lock: Arc<Mutex<()>>,
    /// TTL cache over each linked provider's live model/effort/context
    /// catalog (07d-A3D1 Phase B) — see `crate::model_catalog`'s module doc
    /// for why this is a cache at all (a provider's list-models call is a
    /// real network round trip) and why it never calls the provider on
    /// every `/studio/state` render.
    pub(crate) catalog_cache: Arc<crate::model_catalog::ModelCatalogCache>,
    /// The server-owned live work-trace object per `agent_id` (07d-A5) —
    /// deliberately named `agent_activity`, not `activity`, to avoid any
    /// confusion with `Lifecycle.activity`/`StreamActivity` above (an
    /// unrelated, pre-existing discard-fencing lease counter keyed by
    /// `stream_id`). See [`AgentActivity`]/[`ActivityEntry`]'s doc comments
    /// for the full contract.
    pub(crate) agent_activity: Arc<Mutex<HashMap<String, ActivityEntry>>>,
    /// Ephemeral duplicate-work guards for the durable per-agent composer
    /// queues. The actual messages live in Heart (`agent_control`); this set
    /// is intentionally reconstructed empty on restart.
    pub(crate) queue_dispatchers: Arc<Mutex<HashSet<String>>>,
    /// Live MCP session tokens (07d-B Phase 2 Stage C item 2), minted at ACP
    /// spawn time for the permanent Baker stream or a task socket and read
    /// by `POST /mcp/call`/`GET /mcp/tools`. In-memory only by design — see
    /// `crate::mcp::mint_mcp_token`'s doc comment on restart recovery.
    pub(crate) mcp_tokens: Arc<crate::mcp::McpTokenRegistry>,
    /// The Stage D task driver's admission map — one driver thread per task,
    /// capped at `task_driver::DRIVER_CAPACITY` concurrently driven
    /// (07d-B Phase 4 item 1). See `task_driver::DriverRegistry`'s doc
    /// comment.
    pub(crate) task_drivers: Arc<crate::task_driver::DriverRegistry>,
    /// Bounded-staleness cache over each materialized task socket's baseline
    /// diff stat, keyed by the task's own stream id — `(added, removed,
    /// recomputed_at)` (07d-B Phase 4 item 3). Only `SocketInfo`'s Floor-row
    /// counters read this; `GET /task/review`, `POST /task/signoff`, and
    /// integration always recompute fresh (`task_review.rs`).
    pub(crate) task_diff_cache: Arc<Mutex<HashMap<String, (usize, usize, std::time::Instant)>>>,
    /// Bumped when a Heart family that the studio picture reads is committed.
    /// `routes::studio` returns its kept snapshot while this still matches.
    pub(crate) studio_input_gen: Arc<AtomicU64>,
    /// Last `StudioState` and the inputs it was built from.
    pub(crate) studio_cache: Arc<Mutex<Option<crate::routes::studio::StudioHeld>>>,
    /// One jobserver for every room's compiles: two tokens, created once
    /// when the process starts. `builds::extend` hands each room that
    /// client's `MAKEFLAGS` line.
    pub(crate) compile_jobs: jobserver::Client,
    /// The resolved operator bearer token this process enforces on every
    /// non-exempt HTTP route (`operator_auth::require_operator`):
    /// `BAKERY_OPERATOR_TOKEN` env when set, else `node.json`'s persisted
    /// token. Resolved once at boot; never sent to any spawned child.
    pub(crate) operator_token: Arc<String>,
    /// Continue-after-decision (Checkpoint 2 item 2): a turn that proposed
    /// an edit or asked a question parks here, keyed by the pending event's
    /// hash, until the decision/answer route notifies it — the outcome then
    /// becomes that tool call's result and the SAME turn continues. Wakes
    /// are advisory; the waiter re-derives the outcome from Heart facts on
    /// every wake (the auto-accept-inside-turn ordering rule).
    pub(crate) decision_waiters:
        Arc<Mutex<HashMap<String, std::sync::mpsc::Sender<DecisionOutcome>>>>,
    /// Live, unconsumed one-time tickets for the `/events` WebSocket
    /// (minted by the authenticated `POST /events/ticket`; WS upgrades
    /// cannot carry an Authorization header from a browser). Value is the
    /// expiry instant — same one-shot/TTL discipline as `pty_tickets`.
    pub(crate) events_tickets: Arc<Mutex<HashMap<String, std::time::Instant>>>,
    /// Test-only fallback so the ~30 tests written before seats existed can
    /// keep calling `state_with(dir, model)` unmodified: when neither the
    /// cache nor a Heart fact names a seat's model, [`resolve_seat`] falls
    /// back to this rather than failing the turn. Production `AppState`
    /// (`main.rs`) never sets it — this field and the branch that reads it
    /// compile out of a non-test build entirely, so a live node has no path
    /// that can ever substitute it for a real assignment.
    #[cfg(test)]
    pub(crate) test_default_model: Option<Arc<dyn Model>>,
}

/// What is open — the sticky context bar's whole content.
#[derive(schemars::JsonSchema, Debug, Clone, Serialize)]
pub(crate) struct ActiveContext {
    pub(crate) stream_id: String,
    pub(crate) name: String,
    /// Repository (project) name.
    pub(crate) project: String,
    pub(crate) repo_root: String,
    pub(crate) branch: String,
    pub(crate) worktree_path: String,
    /// False when the stream is known to the Heart but its directory is gone.
    pub(crate) exists_on_disk: bool,
    /// True when `worktree_path` IS the repo's own working tree — the human
    /// opened a checkout directly rather than materialising a linked worktree.
    pub(crate) in_place: bool,
    pub(crate) owner_node_id: Option<String>,
    /// True only when *this node* ran `git worktree add` to create
    /// `worktree_path` (set once, on the original `project.onboarded` event —
    /// never re-derived). A manually attached worktree, or one adopted from
    /// disk with no Heart record of how it got there, is `false`: proven
    /// provenance, not inferred from where the path happens to sit
    /// (04-LIFECYCLE audit: "Bakery ownership is inferred, not proven").
    pub(crate) created_by_bakery: bool,
}

/// The operator's own name for a conversation: `{title}`, on it.
pub(crate) const CONVERSATION_RENAMED_FAMILY: &str = "conversation.renamed";

/// A stream as "recents" needs it: enough to re-open with one click.
#[derive(schemars::JsonSchema, Debug, Clone, Serialize)]
pub(crate) struct StreamRef {
    pub(crate) stream_id: String,
    /// The name it was opened with, which reopening it goes by.
    pub(crate) name: String,
    /// The name the operator gave it since, shown in place of `name`.
    pub(crate) title: Option<String>,
    pub(crate) project: String,
    pub(crate) repo_root: String,
    pub(crate) branch: String,
    pub(crate) worktree_path: String,
    pub(crate) exists_on_disk: bool,
    /// True when `worktree_path` IS the repo's own working tree — the human
    /// opened a checkout directly rather than materialising a linked worktree.
    pub(crate) in_place: bool,
    pub(crate) owner_node_id: Option<String>,
    pub(crate) last_ts: String,
    pub(crate) events: u64,
    /// `stream.lifetime.set`'s latest value, or `Slice` when no such event
    /// exists yet (STEERING: "slice — default for 'new session on this bug'").
    pub(crate) lifetime: crate::node::Lifetime,
    /// True once `stream.discarded` has landed — worktree/room are gone,
    /// Heart lineage is not.
    pub(crate) discarded: bool,
}

/// Append + broadcast. Every event in this daemon goes through here.
pub(crate) fn append(
    state: &AppState,
    stream_id: &str,
    actor: &str,
    family: &str,
    payload: String,
    causality: Vec<String>,
) -> Result<Event, String> {
    let new = NewEvent {
        stream_id: stream_id.to_string(),
        actor: crate::actor::attribute(actor),
        ts: chrono::Utc::now().to_rfc3339(),
        causality,
        layer: Some("L2".to_string()),
        sensitivity: "normal".to_string(),
        family: family.to_string(),
        payload,
    };
    let event = state
        .heart
        .lock()
        .unwrap()
        .append(new)
        .map_err(|e| e.to_string())?;
    // Best-effort broadcast; Err just means no subscribers.
    let _ = state
        .tx
        .send(serde_json::to_string(&event).unwrap_or_default());
    Ok(event)
}

/// Record that this node owns the write for `stream_id`, unless it already does.
///
/// PHASE: advisory only. There is no lease, no fence, and no remote claim — a
/// second node writing the same stream is prevented by *not existing yet*, not
/// by this record (CONTROL_PLANE §9 ungraceful handoff is still open).
pub(crate) fn claim_ownership(state: &AppState, stream_id: &str) -> Result<Option<String>, String> {
    let last = state
        .heart
        .lock()
        .unwrap()
        .last_of_family(stream_id, OWNERSHIP_FAMILY)
        .map_err(|e| e.to_string())?;
    let node_id = state.node.lock().unwrap().node_id.clone();
    if let Some(ev) = &last {
        let p: serde_json::Value = serde_json::from_str(&ev.payload).map_err(|e| e.to_string())?;
        if str_field(&p, "node_id").as_deref() == Some(node_id.as_str()) {
            return Ok(Some(node_id));
        }
    }
    let claim = OwnershipClaim::local(&state.node.lock().unwrap());
    let payload = serde_json::to_string(&claim).map_err(|e| e.to_string())?;
    append(state, stream_id, "node", OWNERSHIP_FAMILY, payload, vec![])?;
    Ok(Some(node_id))
}

/// Every stream carrying a `project.onboarded` event (any socket, opaque id
/// or not), newest activity first.
pub(crate) fn known_streams(state: &AppState) -> Result<Vec<StreamRef>, String> {
    let heart = state.heart.lock().unwrap();
    let summaries = heart.list_streams().map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for s in summaries {
        let Some(ev) = heart
            .first_of_family(&s.stream_id, "project.onboarded")
            .map_err(|e| e.to_string())?
        else {
            continue; // not a project stream
        };
        let p: serde_json::Value = match serde_json::from_str(&ev.payload) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let owner = heart
            .last_of_family(&s.stream_id, OWNERSHIP_FAMILY)
            .map_err(|e| e.to_string())?
            .and_then(|o| serde_json::from_str::<serde_json::Value>(&o.payload).ok())
            .and_then(|v| str_field(&v, "node_id"));
        let mut worktree_path = str_field(&p, "worktree_path").unwrap_or_default();
        let mut repo_root = str_field(&p, "repo_root")
            .or_else(|| str_field(&p, "repo"))
            .unwrap_or_default();
        let mut branch = str_field(&p, "branch").unwrap_or_default();
        // A workspace bound or moved later wins, as in `context_for`.
        if let Some(w) = heart
            .last_of_family(&s.stream_id, SOCKET_WORKSPACE_FAMILY)
            .ok()
            .flatten()
            .and_then(|e| serde_json::from_str::<WorkspaceSet>(&e.payload).ok())
        {
            worktree_path = w.worktree_path;
            repo_root = w.repo_root;
            branch = w.branch;
        }
        let in_place = same_path(&worktree_path, &repo_root);
        out.push(StreamRef {
            // 07d-A3: stream ids are opaque and server-minted, independent
            // of `name` — read the human name the payload actually stored
            // (`open_stream`'s `"name": name` field) rather than deriving it
            // from a `wt:` stream-id prefix that no longer exists.
            name: str_field(&p, "name").unwrap_or_default(),
            title: heart
                .last_of_family(&s.stream_id, CONVERSATION_RENAMED_FAMILY)
                .ok()
                .flatten()
                .and_then(|e| str_field(&serde_json::from_str(&e.payload).ok()?, "title")),
            lifetime: lifetime_of(&heart, &s.stream_id),
            discarded: discarded_of(&heart, &s.stream_id),
            stream_id: s.stream_id,
            project: str_field(&p, "project").unwrap_or_default(),
            repo_root,
            branch,
            exists_on_disk: std::path::Path::new(&worktree_path).is_dir(),
            in_place,
            worktree_path,
            owner_node_id: owner,
            last_ts: s.last_ts,
            events: s.events,
        });
    }
    Ok(out)
}

/// Latest `stream.lifetime.set` value, or `Slice` when the stream has never
/// had one. Takes an already-locked `Heart` so callers already holding the
/// lock (e.g. `known_streams`'s loop) don't deadlock re-acquiring it.
///
/// "No event at all" and "an event exists but can't be read" are different
/// facts and must not collapse to the same default: no event is the
/// documented `Slice` default, but a `stream.lifetime.set` event that fails
/// to parse (or a Heart read error) means we genuinely don't know what was
/// declared — silently treating that as `Slice` would waive `Living`'s
/// discard-confirmation gate on a fact we couldn't actually verify. Fail
/// closed to `Living` instead: worst case an already-`Slice` stream asks for
/// a confirm it didn't strictly need.
fn lifetime_of(heart: &Heart, stream_id: &str) -> crate::node::Lifetime {
    match lifetime_fact(heart, stream_id) {
        Ok(Some(l)) => l,
        Ok(None) => crate::node::Lifetime::default(),
        Err(()) => crate::node::Lifetime::Living,
    }
}

/// `Ok(Some(lifetime))` when a `stream.lifetime.set` event exists and parses;
/// `Ok(None)` when none exists yet (the documented `Slice` default); `Err(())`
/// when an event exists but the Heart read or the payload parse failed — the
/// fact is genuinely unknown, which is a *third* outcome, not interchangeable
/// with either default.
fn lifetime_fact(heart: &Heart, stream_id: &str) -> Result<Option<Lifetime>, ()> {
    match heart.last_of_family(stream_id, crate::node::LIFETIME_FAMILY) {
        Ok(Some(ev)) => serde_json::from_str::<crate::node::LifetimeClaim>(&ev.payload)
            .map(|c| Some(c.lifetime))
            .map_err(|_| ()),
        Ok(None) => Ok(None),
        Err(_) => Err(()),
    }
}

/// Whether a host-mode PTY or Ghostty open should be allowed for this
/// stream's current lifetime right now: `Slice` and `Living` yes, `Burn` no.
/// An unknown/unreadable fact ALSO says no — `lifetime_of`'s fail-closed
/// default of `Living` is correct for the discard confirm-gate (worst case:
/// an overprotective confirm prompt), but reusing it here would read as
/// "unreadable, therefore Living, therefore host is fine" — exactly the
/// wrong direction for a decision that hands out unsandboxed shell access
/// (04b-FENCE §5: "unknown lifetime is not Living-for-host").
fn host_allowed(heart: &Heart, stream_id: &str) -> bool {
    matches!(
        lifetime_fact(heart, stream_id),
        Ok(None) | Ok(Some(Lifetime::Slice)) | Ok(Some(Lifetime::Living))
    )
}

/// Whether `stream.discarded` has *cleanly* landed on this stream — a record
/// with either step's error present means discard only partially finished
/// and is retriable, not done (see `DiscardRecord`). Same fail-closed
/// reasoning as `lifetime_of`: a record that exists but won't parse (or a
/// Heart read error) is treated as discarded/blocked, not as a clean slate —
/// the safer direction here is refusing a new attach, not allowing one.
pub(crate) fn discarded_of(heart: &Heart, stream_id: &str) -> bool {
    match heart.last_of_family(stream_id, crate::node::DISCARD_FAMILY) {
        Ok(Some(ev)) => serde_json::from_str::<crate::node::DiscardRecord>(&ev.payload)
            .map(|r| r.worktree_error.is_none() && r.room_error.is_none() && r.chat_error.is_none())
            .unwrap_or(true),
        Ok(None) => false,
        Err(_) => true,
    }
}

/// Single-stream lookup for routes that don't already hold the Heart lock
/// (unlike `known_streams`'s loop, which calls `lifetime_of`/`discarded_of`
/// directly to avoid deadlocking on its own guard). No production route
/// reads a bare `Lifetime` on its own anymore — `begin_discard` and
/// `stream_host_allowed` cover the two real decisions that need one — so
/// this is test-only, asserting the fact a test just set actually landed.
#[cfg(test)]
pub(crate) fn stream_lifetime(state: &AppState, stream_id: &str) -> crate::node::Lifetime {
    lifetime_of(&state.heart.lock().unwrap(), stream_id)
}

/// Test-only, same as [`stream_lifetime`] above: no production route reads a
/// bare discarded flag on its own anymore — `admit_pty`/`admit_one_shot`/
/// `admit_activity`/`begin_discard` each fold the same `discarded_of` check
/// into their own atomic admission instead.
#[cfg(test)]
pub(crate) fn stream_discarded(state: &AppState, stream_id: &str) -> bool {
    discarded_of(&state.heart.lock().unwrap(), stream_id)
}

pub(crate) fn stream_host_allowed(state: &AppState, stream_id: &str) -> bool {
    host_allowed(&state.heart.lock().unwrap(), stream_id)
}

// ─── Attach/discard fencing (04b-FENCE) ────────────────────────────────────
//
// One lock (`AppState::lifecycle`) makes "can a new attach start" and "has a
// discard already claimed this stream" a single atomic decision instead of
// two routes each doing their own check-then-act. The rule throughout: this
// lock is always acquired BEFORE the Heart lock, never after — every
// function below that needs both nests the Heart lock inside a `lifecycle`
// guard, and nothing that already holds the Heart lock ever reaches back for
// `lifecycle`, so the two can never deadlock against each other.

/// Released when a one-shot `/env/provision`, `/env/exec`, or `/env/pty`
/// call finishes (success, error, or panic unwind) — the stream reads as
/// idle again the instant this drops.
pub(crate) struct OneShotGuard {
    state: AppState,
    stream_id: String,
}

impl Drop for OneShotGuard {
    fn drop(&mut self) {
        let mut g = self.state.lifecycle.lock().unwrap();
        if let Some(a) = g.activity.get_mut(&self.stream_id) {
            a.one_shot = false;
            if a.is_empty() {
                g.activity.remove(&self.stream_id);
            }
        }
    }
}

/// Admits a one-shot attach: under one lock, refuse a retiring/discarded
/// stream or a second concurrent one-shot, else mark the stream busy before
/// releasing the lock. A discard that checks `retiring` under this same lock
/// can therefore never observe "not retiring yet" and then have this attach
/// slip in afterward — either this runs first and discard's fencing sees it,
/// or discard's `retiring` insert runs first and this is refused
/// (04b-FENCE §1).
pub(crate) fn admit_one_shot(
    state: &AppState,
    stream_id: &str,
) -> Result<OneShotGuard, (StatusCode, String)> {
    let mut g = state.lifecycle.lock().unwrap();
    if g.retiring.contains(stream_id) {
        return Err((
            StatusCode::CONFLICT,
            "stream is being discarded".to_string(),
        ));
    }
    if discarded_of(&state.heart.lock().unwrap(), stream_id) {
        return Err((
            StatusCode::BAD_REQUEST,
            "stream has been discarded".to_string(),
        ));
    }
    let entry = g.activity.entry(stream_id.to_string()).or_default();
    if entry.one_shot {
        return Err((
            StatusCode::CONFLICT,
            format!("an attach is already in progress for {stream_id}"),
        ));
    }
    entry.one_shot = true;
    Ok(OneShotGuard {
        state: state.clone(),
        stream_id: stream_id.to_string(),
    })
}

/// An interactive PTY attach admitted but not yet backed by a real pid.
/// Holding this (rather than registering nothing until the child spawns) is
/// what closes the WS half of 04b-FENCE §1: the route that calls
/// [`admit_pty`] must do so *before* `ws.on_upgrade` ever returns a response,
/// so a discard that starts the instant after the upgrade response is sent
/// still sees this stream as occupied and waits for it, instead of scanning
/// an empty registry.
pub(crate) struct PtyPendingGuard {
    state: AppState,
    stream_id: String,
    mode: String,
    resolved: bool,
}

impl PtyPendingGuard {
    /// The child spawned successfully and got a real pid — move this
    /// stream's accounting from "pending" to "a killable session", still
    /// under one lock so there is no instant where the stream looks idle.
    pub(crate) fn promote(mut self, pid: u32) -> PtySessionGuard {
        self.resolved = true;
        let mut g = self.state.lifecycle.lock().unwrap();
        if let Some(a) = g.activity.get_mut(&self.stream_id) {
            let slot = a.pending_mut(&self.mode);
            *slot = slot.saturating_sub(1);
            a.ptys.push(PtySession {
                pid,
                mode: self.mode.clone(),
            });
        }
        PtySessionGuard {
            state: self.state.clone(),
            stream_id: self.stream_id.clone(),
            pid,
        }
    }
}

impl Drop for PtyPendingGuard {
    fn drop(&mut self) {
        // `promote` already moved this stream's accounting onto a
        // `PtySessionGuard` — nothing left for a pending-slot to release.
        if self.resolved {
            return;
        }
        let mut g = self.state.lifecycle.lock().unwrap();
        if let Some(a) = g.activity.get_mut(&self.stream_id) {
            let slot = a.pending_mut(&self.mode);
            *slot = slot.saturating_sub(1);
            if a.is_empty() {
                g.activity.remove(&self.stream_id);
            }
        }
    }
}

/// A spawned interactive PTY session with a real, killable pid, registered
/// for the lifetime of the WebSocket handler.
pub(crate) struct PtySessionGuard {
    state: AppState,
    stream_id: String,
    pid: u32,
}

impl Drop for PtySessionGuard {
    fn drop(&mut self) {
        let mut g = self.state.lifecycle.lock().unwrap();
        if let Some(a) = g.activity.get_mut(&self.stream_id) {
            a.ptys.retain(|s| s.pid != self.pid);
            if a.is_empty() {
                g.activity.remove(&self.stream_id);
            }
        }
    }
}

/// Admits an interactive PTY attach — same atomicity guarantee as
/// [`admit_one_shot`], but returns a [`PtyPendingGuard`] rather than
/// registering a pid immediately, since none exists until the pty actually
/// spawns.
///
/// For `mode == "host"`, the "is host allowed right now" check
/// ([`host_allowed`]) runs under the *same* lock acquisition as the
/// retiring/discarded check and the pending-count increment — not as a
/// separate call the caller makes afterward. That single-function
/// atomicity is what 04c-BURN §1 requires: a burn transition either fully
/// commits before this runs (in which case this correctly refuses) or
/// fully after (in which case this already incremented `pty_pending_host`,
/// which `fence_active_ptys`'s burn call now waits on — see below). A
/// Burn/unreadable lifetime never increments pending; there is nothing left
/// to fence.
pub(crate) fn admit_pty(
    state: &AppState,
    stream_id: &str,
    mode: &str,
) -> Result<PtyPendingGuard, (StatusCode, String)> {
    let mut g = state.lifecycle.lock().unwrap();
    if g.retiring.contains(stream_id) {
        return Err((
            StatusCode::CONFLICT,
            "stream is being discarded".to_string(),
        ));
    }
    let heart = state.heart.lock().unwrap();
    if discarded_of(&heart, stream_id) {
        drop(heart);
        return Err((
            StatusCode::BAD_REQUEST,
            "stream has been discarded".to_string(),
        ));
    }
    if mode == "host" && !host_allowed(&heart, stream_id) {
        drop(heart);
        return Err((
            StatusCode::FORBIDDEN,
            "burn (or lifetime unreadable) — host terminal refused".to_string(),
        ));
    }
    drop(heart);
    let entry = g.activity.entry(stream_id.to_string()).or_default();
    *entry.pending_mut(mode) += 1;
    Ok(PtyPendingGuard {
        state: state.clone(),
        stream_id: stream_id.to_string(),
        mode: mode.to_string(),
        resolved: false,
    })
}

/// Released when a discard finishes (success, refusal, or a fencing
/// timeout) — clears `retiring` unconditionally so a stream that fails to
/// discard isn't left permanently refusing every attach route.
pub(crate) struct RetiringGuard {
    state: AppState,
    stream_id: String,
}

impl Drop for RetiringGuard {
    fn drop(&mut self) {
        self.state
            .lifecycle
            .lock()
            .unwrap()
            .retiring
            .remove(&self.stream_id);
    }
}

/// Atomically decides whether a discard may begin: refuses a concurrent
/// discard, a stream already fully discarded, and (unless `confirm`) a
/// stream whose lifetime is `Living` at this exact instant. All of that — the
/// lifetime read, the confirm decision, and setting `retiring` — happens
/// under one lock acquisition, so a `set_lifetime` call cannot land between
/// "discard read the lifetime" and "discard committed to retiring": either
/// it fully precedes this call or fully follows it (04b-FENCE §2: "a stream
/// that is Living at decision time still needs confirm:true").
pub(crate) fn begin_discard(
    state: &AppState,
    stream_id: &str,
    confirm: bool,
) -> Result<(RetiringGuard, Lifetime), (StatusCode, String)> {
    let mut g = state.lifecycle.lock().unwrap();
    if g.retiring.contains(stream_id) {
        return Err((
            StatusCode::CONFLICT,
            "a discard is already in progress for this stream".to_string(),
        ));
    }
    let (lifetime, discarded) = {
        let heart = state.heart.lock().unwrap();
        (
            lifetime_of(&heart, stream_id),
            discarded_of(&heart, stream_id),
        )
    };
    if discarded {
        return Err((
            StatusCode::BAD_REQUEST,
            "stream already discarded".to_string(),
        ));
    }
    if lifetime == Lifetime::Living && !confirm {
        return Err((
            StatusCode::BAD_REQUEST,
            "living session — pass confirm:true to discard".to_string(),
        ));
    }
    g.retiring.insert(stream_id.to_string());
    Ok((
        RetiringGuard {
            state: state.clone(),
            stream_id: stream_id.to_string(),
        },
        lifetime,
    ))
}

/// Sets a stream's declared lifetime to `Slice` or `Living` under the same
/// lock [`begin_discard`] uses for its own read-decide-commit section, so the
/// two can never interleave (04b-FENCE §2) — appending the event while
/// holding the lock means a concurrent discard either reads the old lifetime
/// and commits to retiring before this can run, or reads the new one after
/// this has fully landed, never something in between.
///
/// Refuses `Burn` outright — [`begin_burn_transition`] is the only path
/// allowed to commit that lifetime, since only it also marks `burn_fencing`
/// active in the same lock hold. A `Burn` written through this function
/// instead would land with no fence at all, which is exactly the unfenced
/// write 04d-GHOSTTY-LEASE §2 closed off; the name says so, so a future
/// caller reaching for "the function that sets a lifetime" doesn't land here
/// for Burn by accident.
///
/// Also refuses while a `Burn` transition is fencing this stream (see
/// [`begin_burn_transition`]) — a `Burn` commits and holds that flag for
/// its *whole* fence, not just the moment of the write, specifically so
/// nothing (including this function) can change the lifetime out from
/// under it while it's mid-kill (04d-GHOSTTY-LEASE §2).
pub(crate) fn set_non_burn_lifetime(
    state: &AppState,
    stream_id: &str,
    lifetime: Lifetime,
) -> Result<(), (StatusCode, String)> {
    if lifetime == Lifetime::Burn {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            "set_non_burn_lifetime must never be called with Burn — use begin_burn_transition"
                .to_string(),
        ));
    }
    let g = state.lifecycle.lock().unwrap();
    if g.retiring.contains(stream_id) {
        return Err((
            StatusCode::CONFLICT,
            "stream is being discarded".to_string(),
        ));
    }
    if g.burn_fencing.contains(stream_id) {
        return Err((
            StatusCode::CONFLICT,
            "a burn transition is still fencing this stream — retry shortly".to_string(),
        ));
    }
    if discarded_of(&state.heart.lock().unwrap(), stream_id) {
        return Err((
            StatusCode::BAD_REQUEST,
            "stream has been discarded".to_string(),
        ));
    }
    let payload = serde_json::to_string(&crate::node::LifetimeClaim { lifetime })
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    append(
        state,
        stream_id,
        "human",
        crate::node::LIFETIME_FAMILY,
        payload,
        vec![],
    )
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    drop(g);
    Ok(())
}

/// Released when a `Burn` transition's fence ends — success, timeout, or
/// panic unwind — clearing `burn_fencing` so the stream accepts lifetime
/// writes again.
pub(crate) struct BurnFenceGuard {
    state: AppState,
    stream_id: String,
}

fn close_acp_slots_for_stream(state: &AppState, stream_id: &str) -> Vec<u32> {
    let keys: Vec<_> = state
        .acp_sessions
        .lock()
        .unwrap()
        .keys()
        .filter(|(sid, _)| sid == stream_id)
        .cloned()
        .collect();
    let mut pids = Vec::new();
    let mut sessions = state.acp_sessions.lock().unwrap();
    for key in keys {
        let Some(slot) = sessions.remove(&key) else {
            continue;
        };
        match slot {
            crate::pipe::AcpSlot::Live(live) => {
                live.close();
                pids.push(live.pid);
                sessions.insert(
                    key,
                    crate::pipe::AcpSlot::Closing {
                        generation: live.generation,
                    },
                );
            }
            crate::pipe::AcpSlot::Spawning { generation, closed } => {
                closed.store(true, std::sync::atomic::Ordering::Release);
                sessions.insert(key, crate::pipe::AcpSlot::Closing { generation });
            }
            closing @ crate::pipe::AcpSlot::Closing { .. } => {
                sessions.insert(key, closing);
            }
        }
    }
    pids
}

fn revoke_cached_acp_models_for_stream(state: &AppState, stream_id: &str) -> Vec<Arc<dyn Model>> {
    let mut cache = state.seats.lock().unwrap();
    let keys: Vec<_> = cache
        .iter()
        // Native and named ACP assignments carry their distinct model ids;
        // transport, not one legacy name literal, is the execution boundary.
        .filter(|((sid, _), model)| {
            sid == stream_id && model.transport() == bakery_agent::Transport::Acp
        })
        .map(|(key, _)| key.clone())
        .collect();
    let mut revoked = Vec::with_capacity(keys.len());
    for key in keys {
        if let Some(model) = cache.remove(&key) {
            model.revoke();
            revoked.push(model);
        }
    }
    revoked
}

impl Drop for BurnFenceGuard {
    fn drop(&mut self) {
        self.state
            .lifecycle
            .lock()
            .unwrap()
            .burn_fencing
            .remove(&self.stream_id);
    }
}

/// Commits a stream's lifetime to `Burn` and marks its fence active, both
/// under one lock acquisition — refuses a stream already mid-discard or
/// already being burn-fenced, same as [`begin_discard`]. The returned guard
/// must be held for the *entire* fence (through `fence_active_ptys`
/// returning), not just this commit: while it's held, [`set_non_burn_lifetime`]
/// refuses every other lifetime write, which is what actually closes the
/// check-to-kill race a generation counter could only ever detect after it
/// already happened (04d-GHOSTTY-LEASE §2 audit) — the fence's kill loop
/// reads live pids without holding this lock across the real
/// `kill_pty_session` call (a subprocess spawn, not instantaneous), so
/// nothing short of making a second write structurally impossible during
/// that whole window is actually safe.
pub(crate) fn begin_burn_transition(
    state: &AppState,
    stream_id: &str,
) -> Result<BurnFenceGuard, (StatusCode, String)> {
    let mut g = state.lifecycle.lock().unwrap();
    if g.retiring.contains(stream_id) {
        return Err((
            StatusCode::CONFLICT,
            "stream is being discarded".to_string(),
        ));
    }
    if g.burn_fencing.contains(stream_id) {
        return Err((
            StatusCode::CONFLICT,
            "a burn transition is already fencing this stream".to_string(),
        ));
    }
    if discarded_of(&state.heart.lock().unwrap(), stream_id) {
        return Err((
            StatusCode::BAD_REQUEST,
            "stream has been discarded".to_string(),
        ));
    }
    let payload = serde_json::to_string(&crate::node::LifetimeClaim {
        lifetime: Lifetime::Burn,
    })
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    append(
        state,
        stream_id,
        "human",
        crate::node::LIFETIME_FAMILY,
        payload,
        vec![],
    )
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    g.burn_fencing.insert(stream_id.to_string());

    // Authority changes with the Heart fact, not when a cached model happens
    // to be resolved again. Revoke every retained ACP Arc and close every
    // generation while the same lifecycle admission lock still excludes a
    // stale trusted spawn. The async fence below only has to kill/reap.
    let revoked_models = revoke_cached_acp_models_for_stream(state, stream_id);
    let mut pids = close_acp_slots_for_stream(state, stream_id);
    if let Some(activity) = g.activity.get(stream_id) {
        pids.extend(activity.acp_pids.iter().copied());
    }
    drop(g);
    pids.sort_unstable();
    pids.dedup();
    for pid in pids {
        bakery_exec::kill_process_group(pid);
    }
    drop(revoked_models);
    Ok(BurnFenceGuard {
        state: state.clone(),
        stream_id: stream_id.to_string(),
    })
}

/// Released when `/agent/turn`, `/agent/accept`, `/agent/reject`, or
/// `/files/*` finishes (success, error, or panic unwind).
pub(crate) struct ActivityLease {
    state: AppState,
    stream_id: String,
}

impl Drop for ActivityLease {
    fn drop(&mut self) {
        let mut g = self.state.lifecycle.lock().unwrap();
        if let Some(a) = g.activity.get_mut(&self.stream_id) {
            a.leases = a.leases.saturating_sub(1);
            if a.is_empty() {
                g.activity.remove(&self.stream_id);
            }
        }
    }
}

/// Released when the turn that created it (`admit_turn`) finishes (success,
/// error, or panic unwind) — decrements this agent's `running_turns` count.
pub(crate) struct TurnLease {
    state: AppState,
    key: (String, Seat),
}

impl Drop for TurnLease {
    fn drop(&mut self) {
        // A grant change that landed mid-turn restarts this stream's runtimes
        // now, while this lease still holds the agent — so no queued turn can
        // start on the old sandbox in between — once no other turn on the
        // stream is still running.
        // ponytail: another seat could be admitted between this check and the
        // restart; Baker conversations run one seat, so it never happens there.
        let restart = {
            let mut g = self.state.lifecycle.lock().unwrap();
            let running_here: u32 = g
                .running_turns
                .iter()
                .filter(|((stream, _), _)| *stream == self.key.0)
                .map(|(_, n)| *n)
                .sum();
            running_here <= 1 && g.restart_when_idle.remove(&self.key.0)
        };
        if restart {
            restart_acp_runtimes(&self.state, &self.key.0);
        }
        // A switch of this seat asked for while its turn ran lands now,
        // while this lease still holds the agent, so its next turn starts on
        // it. That none waits is read under the lock the switch is decided
        // under, and the seat let go under it too: one asked for meanwhile
        // lands here, or finds the seat idle and lands at once. One that
        // replaces or cancels the switch being landed is read again here
        // (`Landing::Waiting`). One that can't land is tried at most a few
        // times, then recorded as failed, and waits no more. Unwinding, or after landing one panicked,
        // none is tried: the seat is let go regardless.
        // ponytail: landing it ends the stream's MCP tokens, as any switch
        // does; another seat's turn running on the stream would lose its
        // own. Baker conversations run one seat.
        let mut tries = if std::thread::panicking() { 3 } else { 0 };
        loop {
            let mut g = self.state.lifecycle.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let waiting = tries < 3
                && seat_switch_waiting_locked(&self.state.heart.lock().unwrap_or_else(std::sync::PoisonError::into_inner), &self.key.0, self.key.1)
                    .is_some();
            if !waiting {
                if let Some(n) = g.running_turns.get_mut(&self.key) {
                    *n = n.saturating_sub(1);
                    if *n == 0 {
                        g.running_turns.remove(&self.key);
                    }
                }
                break;
            }
            drop(g);
            if tries > 0 {
                std::thread::sleep(Duration::from_millis(250));
            }
            tries += 1;
            let (state, key) = (&self.state, &self.key);
            let last = tries >= 3;
            let landed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                crate::routes::seats::switch_waiting(state, &key.0, key.1, last)
            }));
            if landed.is_err() {
                tries = 3;
            }
        }
        // If a follow-up arrived while this turn was running, wake the
        // durable dispatcher after releasing `running_turns`. It will
        // derive the queue from Heart again, so this Drop carries no state.
        crate::agent_control::schedule_dispatch(&self.state, &self.key.0, self.key.1);
    }
}

/// Atomically admits `(stream_id, seat)` for one turn, refusing a second
/// concurrent one for the exact same agent (07d-A1 §5: "one in-flight turn
/// per `agent_id`... no queue in this slice"). Two agents on one socket
/// still run fully concurrently — this key is per-agent, not per-socket —
/// [`admit_activity`] is the separate, non-exclusive socket-wide hold this
/// composes with. Marks `(stream_id, seat)` busy for the lease's lifetime;
/// `/studio/state` projects that fact from `running_turns` directly.
pub(crate) fn admit_turn(
    state: &AppState,
    stream_id: &str,
    seat: Seat,
) -> Result<TurnLease, (StatusCode, String)> {
    let key = (stream_id.to_string(), seat);
    {
        let mut g = state.lifecycle.lock().unwrap();
        let count = g.running_turns.entry(key.clone()).or_insert(0);
        if *count > 0 {
            return Err((
                StatusCode::CONFLICT,
                format!(
                    "a turn is already running for the {} seat on this socket",
                    seat.as_str()
                ),
            ));
        }
        *count += 1;
    }
    let lease = TurnLease {
        state: state.clone(),
        key,
    };
    // A switch still waiting from a turn that ended without its lease
    // letting go (the node restarted under it) lands before this one starts.
    if seat_switch_waiting_locked(&state.heart.lock().unwrap(), stream_id, seat).is_some() {
        crate::routes::seats::switch_waiting(state, stream_id, seat, true);
    }
    Ok(lease)
}

/// Whether a turn is currently in flight for this specific agent — the
/// source `routes::studio::AgentInfo.busy` reads — production code snapshots
/// `Lifecycle.running_turns` directly instead (`routes::studio::studio_state`),
/// never while holding `heart` (07d-A0: locking `lifecycle` from inside a
/// `heart`-held section inverts `admit_activity`'s lifecycle-then-heart
/// order). Test-only convenience wrapper; `#[cfg(test)]` so it compiles out
/// of a real build entirely rather than sitting unused. Per-`(stream_id,
/// seat)`, not per-stream: the other configured agent on the same socket
/// running a turn must never make this one read busy.
#[cfg(test)]
pub(crate) fn is_turn_running(state: &AppState, stream_id: &str, seat: Seat) -> bool {
    state
        .lifecycle
        .lock()
        .unwrap()
        .running_turns
        .get(&(stream_id.to_string(), seat))
        .is_some_and(|&n| n > 0)
}

// ─── Agent activity — Live Work Trace (07d-A5) ──────────────────────────────

/// Cap on [`AgentActivity::recent_completed_tools`] — enough to eyeball
/// recent history without growing unbounded across a very long turn.
const RECENT_TOOLS_CAP: usize = 5;
/// Tail-bound on [`ActivityPhase::Streaming`]'s accumulated preview text —
/// the same "never buffer unbounded" discipline `bakery_agent::sse`'s own
/// `MAX_STREAMED_TEXT_BYTES` applies, just tail-bounded here instead: a live
/// preview only ever needs the END of what's been said so far.
const STREAM_PREVIEW_CHARS: usize = 8000;

/// The server-owned live-activity registry (07d-A5) — keyed by `agent_id`,
/// the same identity `Lifecycle.running_turns` uses, since at most one turn
/// (and therefore at most one live activity) can ever exist per agent at a
/// time (`admit_turn`'s own exclusivity guarantees this). Lives in
/// `AppState.agent_activity`, **not** inside `Lifecycle` — unlike
/// `running_turns`/`retiring`/`burn_fencing`, nothing here participates in
/// discard/burn fencing, so there is no reason to share that lock and take
/// on its ordering rules.
///
/// Deliberately has **no `Drop`-based cleanup**, unlike [`TurnLease`]: an
/// entry is superseded only by the next `admit_turn` for the same
/// `agent_id` (a fresh turn always starts a fresh entry) or transitioned by
/// a later decision route (`/agent/accept`, `/agent/reject`, `/agent/pipe/
/// gate/decide`) — a proposal nobody has decided yet must keep reading
/// `AwaitingApproval` forever, which is correct, not a leak to plug.
pub(crate) struct ActivityEntry {
    pub(crate) snapshot: AgentActivity,
    /// Cloned into the `cancel: &AtomicBool` parameter of `bakery_agent::
    /// Model::chat_turn_streaming` for the turn this entry belongs to, and
    /// flipped by `/agent/turn/cancel`. `Arc` so the cancel route (which
    /// only ever locks `agent_activity` briefly) and the turn's own
    /// long-running closure (which holds its own clone for the whole call)
    /// never need to hold the map's lock at the same time.
    pub(crate) cancel: Arc<std::sync::atomic::AtomicBool>,
    /// Characters the turn has streamed so far (reply and thinking), the
    /// base of `AgentActivity.live_tokens`. A fresh turn starts a fresh
    /// entry, so it starts at zero.
    pub(crate) streamed_chars: u64,
}

/// The wire shape `GET /agent/activity` returns and every activity
/// WebSocket push carries verbatim — see `main.rs`'s route registration and
/// `routes::agent`'s handler. `#[serde(tag = "phase", rename_all =
/// "snake_case")]` on [`ActivityPhase`] mirrors the existing
/// `bakery_agent::ModelCatalog`'s `#[serde(tag = "status", ...)]` precedent.
#[derive(schemars::JsonSchema, Debug, Clone, Serialize)]
pub(crate) struct AgentActivity {
    pub(crate) agent_id: String,
    pub(crate) stream_id: String,
    pub(crate) turn_id: String,
    pub(crate) phase: ActivityPhase,
    /// When `phase` became this kind of phase: a live "Thinking… 4s" counts
    /// from it while contacting the model (W1-T).
    pub(crate) phase_since: String,
    pub(crate) turn_started_at: String,
    pub(crate) recent_completed_tools: std::collections::VecDeque<CompletedTool>,
    /// Always `None` this slice — real token/context numbers are
    /// 07d-A3D1 Phase C's job; this field only carries the shape through
    /// end to end so that phase has nothing left to plumb once it lands.
    pub(crate) usage: Option<TurnUsage>,
    /// An estimate of the tokens this turn has produced so far, counted
    /// from what streams (see [`estimate_tokens`]); the real figure only
    /// arrives with the turn's end.
    pub(crate) live_tokens: u64,
    pub(crate) cancel_requested: bool,
    /// The operating mode this turn runs in (a read-only Ask's Plan, say),
    /// which its MCP tool calls follow (`api_tools::Caller::mcp`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) mode: Option<crate::agent_control::OperatingMode>,
}

#[derive(schemars::JsonSchema, Debug, Clone, Serialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub(crate) enum ActivityPhase {
    ContactingModel,
    /// `text` is the tail-bounded ([`STREAM_PREVIEW_CHARS`]) accumulation
    /// of every delta flushed so far this step — a full current snapshot,
    /// never a delta itself, so a client that missed intermediate
    /// broadcasts (a lagged WS receiver, or one that just bootstrapped via
    /// `GET /agent/activity`) is always caught up by the next message.
    Streaming {
        text: String,
    },
    RunningTool {
        tool: String,
        call_id: String,
        started_at: String,
        kind: ToolRunKind,
        /// A short, one-line preview of what the tool is actually doing — the
        /// shell command for a Terminal/Execute call, the path for a file
        /// tool — so the live work strip shows the real action instead of only
        /// the tool's display name (which is how "running Terminal …" hid the
        /// command that was stuck). `null` when the call carried no
        /// inspectable input.
        preview: Option<String>,
        /// The newest line a running command printed, when its runtime
        /// streams output (a native command call); `null` otherwise.
        output: Option<String>,
    },
    /// Set the instant a proposal/gate is posted and — in Manual write
    /// mode — genuinely observable over the WebSocket; in Auto/Rules mode
    /// `maybe_auto_accept` supersedes it within the very same call, before
    /// any client can ever see it broadcast. Expected, not a bug: this
    /// phase's only job is to be correct at the instant it's read, not to
    /// guarantee visibility for a fixed minimum duration.
    AwaitingApproval {
        kind: ApprovalKind,
        event_hash: String,
    },
    Complete {
        note: Option<String>,
    },
    Error {
        reason: String,
    },
    Cancelled,
}

impl ActivityPhase {
    /// A turn in one of these phases has already ended — nothing can still
    /// be "in flight" against it. Used to (a) clear a stale
    /// `cancel_requested` once a turn reaches one of these (07d-A5 review
    /// #6) and (b) refuse `POST /agent/turn/cancel` against a turn that
    /// already finished, rather than silently reporting success (review
    /// #8).
    fn is_terminal(&self) -> bool {
        matches!(
            self,
            ActivityPhase::Complete { .. } | ActivityPhase::Error { .. } | ActivityPhase::Cancelled
        )
    }

    /// The phase's name on the wire (its `phase` tag).
    pub(crate) fn kind(&self) -> &'static str {
        match self {
            ActivityPhase::ContactingModel => "contacting_model",
            ActivityPhase::Streaming { .. } => "streaming",
            ActivityPhase::RunningTool { .. } => "running_tool",
            ActivityPhase::AwaitingApproval { .. } => "awaiting_approval",
            ActivityPhase::Complete { .. } => "complete",
            ActivityPhase::Error { .. } => "error",
            ActivityPhase::Cancelled => "cancelled",
        }
    }
}

#[derive(schemars::JsonSchema, Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ToolRunKind {
    Tool,
    /// A cheap, cosmetic-only heuristic (`routes::agent::looks_like_test_run`)
    /// — never a claim that tests genuinely ran, just a friendlier phase
    /// label for the common case.
    Testing,
}

#[derive(schemars::JsonSchema, Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ApprovalKind {
    FileEdit,
    ChildTool,
    /// An `ask_user` question awaiting `POST /agent/answer` — the answer
    /// resumes the SAME parked turn (Checkpoint 2 item 2).
    Question,
}

#[derive(schemars::JsonSchema, Debug, Clone, Serialize)]
pub(crate) struct CompletedTool {
    pub(crate) tool: String,
    pub(crate) call_id: String,
    pub(crate) ok: bool,
    pub(crate) finished_at: String,
}

/// See [`AgentActivity::usage`] — every field stays `None` until 07d-A3D1
/// Phase C populates it from what a provider's own response actually
/// reports.
#[derive(schemars::JsonSchema, Debug, Clone, Serialize)]
pub(crate) struct TurnUsage {
    pub(crate) tokens_in: Option<u64>,
    pub(crate) tokens_out: Option<u64>,
    pub(crate) context_used: Option<u64>,
    pub(crate) context_window: Option<u64>,
    pub(crate) thought_tokens: Option<u64>,
    pub(crate) cache_read_tokens: Option<u64>,
    pub(crate) cache_write_tokens: Option<u64>,
    pub(crate) cost: Option<bakery_agent::TurnCost>,
    /// Wall time from the turn's start to its usage being recorded.
    pub(crate) duration_ms: Option<u64>,
}

/// Broadcasts `snapshot` over the SAME `/events` WebSocket every Heart
/// event rides, wrapped in a `{"kind":"activity",...}` envelope a
/// `bakery_core::Event` can never structurally produce (no such top-level
/// key) — `events.ts`'s existing `BakeryEvent` consumers branch past this
/// unchanged, per 07d-A5's design. Deliberately calls `state.tx.send`
/// directly, never `state::append` — an activity update is never written
/// to Heart; only the already-existing boundary events
/// (`agent.turn.started`/`done`/`tool.called`/...) are.
fn broadcast_activity(state: &AppState, snapshot: &AgentActivity) {
    let _ = state.tx.send(activity_envelope(snapshot));
}

/// How every activity envelope begins: its route comes first, so a
/// window's socket reads whose frame it is ([`activity_route`]) without
/// parsing the snapshot behind it.
const ACTIVITY_HEAD: &str = r#"{"kind":"activity","route":"#;

/// Whose activity frame this is, and the phase it says.
#[derive(Debug, PartialEq, Serialize, serde::Deserialize)]
pub(crate) struct ActivityRoute {
    pub(crate) stream_id: String,
    pub(crate) agent_id: String,
    pub(crate) phase: String,
}

/// `snapshot` as the envelope the events socket carries.
pub(crate) fn activity_envelope(snapshot: &AgentActivity) -> String {
    let route = ActivityRoute {
        stream_id: snapshot.stream_id.clone(),
        agent_id: snapshot.agent_id.clone(),
        phase: snapshot.phase.kind().to_string(),
    };
    let route = serde_json::to_string(&route).unwrap_or_else(|_| "{}".into());
    let activity = serde_json::to_string(snapshot).unwrap_or_else(|_| "{}".into());
    format!(r#"{ACTIVITY_HEAD}{route},"activity":{activity}}}"#)
}

/// The route of an activity envelope; `None` for any other frame.
pub(crate) fn activity_route(frame: &str) -> Option<ActivityRoute> {
    let rest = frame.strip_prefix(ACTIVITY_HEAD)?;
    serde_json::Deserializer::from_str(rest)
        .into_iter::<ActivityRoute>()
        .next()?
        .ok()
}

/// The frame a window gets for an agent on a conversation it does not
/// follow: only its phase, for that conversation's glyph. Marked `slim`, so
/// the page hands it only to its every-conversation listeners.
pub(crate) fn slim_activity(route: &ActivityRoute) -> String {
    serde_json::json!({
        "kind": "activity",
        "slim": true,
        "activity": {
            "stream_id": route.stream_id,
            "agent_id": route.agent_id,
            "phase": { "phase": route.phase },
        },
    })
    .to_string()
}

/// The live activity of every agent on `stream_id` now, as envelopes.
pub(crate) fn activity_envelopes_for(state: &AppState, stream_id: &str) -> Vec<String> {
    let all = state.agent_activity.lock().unwrap_or_else(|e| e.into_inner());
    all.values()
        .filter(|entry| entry.snapshot.stream_id == stream_id)
        .map(|entry| activity_envelope(&entry.snapshot))
        .collect()
}

/// Starts a fresh live-activity entry for `agent_id` — called once per turn,
/// right after `admit_turn` succeeds, before `agent.turn.started` is even
/// appended. Unconditionally overwrites any entry already there (a finished
/// prior turn's `Complete`/`Error`/`Cancelled` snapshot, or — in the
/// pathological case of a crashed prior attempt — a stale one) since a
/// fresh `admit_turn` success is only possible when no turn for this agent
/// is currently in flight. Returns the `Arc<AtomicBool>` to pass as
/// `chat_turn_streaming`'s `cancel` parameter.
pub(crate) fn start_agent_activity(
    state: &AppState,
    stream_id: &str,
    agent_id: &str,
    turn_id: &str,
) -> Arc<std::sync::atomic::AtomicBool> {
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let snapshot = AgentActivity {
        agent_id: agent_id.to_string(),
        stream_id: stream_id.to_string(),
        turn_id: turn_id.to_string(),
        phase: ActivityPhase::ContactingModel,
        phase_since: chrono::Utc::now().to_rfc3339(),
        turn_started_at: chrono::Utc::now().to_rfc3339(),
        recent_completed_tools: std::collections::VecDeque::new(),
        usage: None,
        live_tokens: 0,
        cancel_requested: false,
        mode: None,
    };
    broadcast_activity(state, &snapshot);
    state.agent_activity.lock().unwrap().insert(
        agent_id.to_string(),
        ActivityEntry {
            snapshot,
            cancel: cancel.clone(),
            streamed_chars: 0,
        },
    );
    cancel
}

/// Applies a phase transition to an already-locked entry, clearing a stale
/// `cancel_requested` whenever the new phase is terminal (07d-A5 review #6,
/// and the fix-verification follow-up that found `resolve_awaiting_approval`
/// needed the exact same clear — both are real ways a tracked turn can reach
/// a terminal phase, and neither must leave a stale `cancel_requested:true`
/// behind): either a cancel raced the turn to completion and won (phase is
/// already `Cancelled` — a harmless no-op restatement) or it lost
/// (`Complete`/`Error`), in which case a lingering `true` would misleadingly
/// imply a live cancel is still pending against a turn that already ended.
/// Sets the phase, and when it began if it is a new kind of phase.
fn set_phase(snapshot: &mut AgentActivity, phase: ActivityPhase) {
    if std::mem::discriminant(&snapshot.phase) != std::mem::discriminant(&phase) {
        snapshot.phase_since = chrono::Utc::now().to_rfc3339();
    }
    snapshot.phase = phase;
}

fn apply_phase_locked(entry: &mut ActivityEntry, phase: ActivityPhase) {
    if phase.is_terminal() {
        entry.snapshot.cancel_requested = false;
    }
    set_phase(&mut entry.snapshot, phase);
}

/// Best-effort phase transition — a missing entry (the turn already ended,
/// or never started one, e.g. an ACP transport turn that predates this
/// slice's per-step granularity) is silently a no-op, never a turn failure.
pub(crate) fn set_agent_phase(state: &AppState, agent_id: &str, phase: ActivityPhase) {
    let mut guard = state.agent_activity.lock().unwrap();
    let Some(entry) = guard.get_mut(agent_id) else {
        return;
    };
    apply_phase_locked(entry, phase);
    let snapshot = entry.snapshot.clone();
    drop(guard);
    broadcast_activity(state, &snapshot);
}

/// Models average about four characters a token, so streamed characters
/// divided by four, rounded up, is the live count.
fn estimate_tokens(chars: u64) -> u64 {
    chars.div_ceil(4)
}

/// The thinking count is broadcast only when it passes into a new multiple
/// of this many tokens: thought chunks arrive far faster than a counter
/// needs repainting.
const THOUGHT_BROADCAST_TOKENS: u64 = 8;

/// Adds streamed characters to the turn's count.
fn count_streamed(entry: &mut ActivityEntry, chars: usize) {
    entry.streamed_chars += chars as u64;
    entry.snapshot.live_tokens = estimate_tokens(entry.streamed_chars);
}

/// Counts a chunk of the agent's own thinking toward the turn's live
/// tokens. Fenced to `turn_id` like the text path, so a late callback from
/// an old process never counts into a newer turn.
pub(crate) fn note_thought_for_turn(state: &AppState, agent_id: &str, turn_id: &str, chars: usize) {
    let mut guard = state.agent_activity.lock().unwrap();
    let Some(entry) = guard.get_mut(agent_id) else {
        return;
    };
    if entry.snapshot.turn_id != turn_id || entry.snapshot.phase.is_terminal() {
        return;
    }
    let before = entry.snapshot.live_tokens;
    count_streamed(entry, chars);
    if before / THOUGHT_BROADCAST_TOKENS == entry.snapshot.live_tokens / THOUGHT_BROADCAST_TOKENS {
        return;
    }
    let snapshot = entry.snapshot.clone();
    drop(guard);
    broadcast_activity(state, &snapshot);
}

/// Appends one streamed text chunk onto the current `Streaming` phase
/// (creating it if the phase was something else, e.g. the first delta of a
/// step), tail-bounding at [`STREAM_PREVIEW_CHARS`]. Called by
/// `routes::agent`'s coalescing buffer, never once per raw provider-level
/// delta — see that call site for the flush cadence.
pub(crate) fn append_streaming_text(state: &AppState, agent_id: &str, chunk: &str) {
    let mut guard = state.agent_activity.lock().unwrap();
    let Some(entry) = guard.get_mut(agent_id) else {
        return;
    };
    let mut text = match &entry.snapshot.phase {
        ActivityPhase::Streaming { text } => text.clone(),
        _ => String::new(),
    };
    text.push_str(chunk);
    if text.chars().count() > STREAM_PREVIEW_CHARS {
        let skip = text.chars().count() - STREAM_PREVIEW_CHARS;
        text = text.chars().skip(skip).collect();
    }
    set_phase(&mut entry.snapshot, ActivityPhase::Streaming { text });
    count_streamed(entry, chunk.chars().count());
    let snapshot = entry.snapshot.clone();
    drop(guard);
    broadcast_activity(state, &snapshot);
}

/// Generation/background-callback-safe streaming update.  Native ACP
/// notifications arrive on a persistent session task, so checking the turn
/// id while holding the activity lock is essential: a pre-cancel callback
/// from an old process must never repaint a newer turn's preview.
pub(crate) fn append_streaming_text_for_turn(
    state: &AppState,
    agent_id: &str,
    turn_id: &str,
    chunk: &str,
) -> bool {
    let mut guard = state.agent_activity.lock().unwrap();
    let Some(entry) = guard.get_mut(agent_id) else {
        return false;
    };
    if entry.snapshot.turn_id != turn_id || entry.snapshot.phase.is_terminal() {
        return false;
    }
    let mut text = match &entry.snapshot.phase {
        ActivityPhase::Streaming { text } => text.clone(),
        _ => String::new(),
    };
    text.push_str(chunk);
    if text.chars().count() > STREAM_PREVIEW_CHARS {
        let skip = text.chars().count() - STREAM_PREVIEW_CHARS;
        text = text.chars().skip(skip).collect();
    }
    set_phase(&mut entry.snapshot, ActivityPhase::Streaming { text });
    count_streamed(entry, chunk.chars().count());
    let snapshot = entry.snapshot.clone();
    drop(guard);
    broadcast_activity(state, &snapshot);
    true
}

/// Pushes one finished tool call onto the bounded recent-tools ring. Called
/// right after the tool's own Heart event (`agent.tool.called` or
/// whichever terminal event that branch produces) has been appended, so the
/// two never disagree about whether the call actually finished.
pub(crate) fn push_completed_tool(state: &AppState, agent_id: &str, tool: CompletedTool) {
    let mut guard = state.agent_activity.lock().unwrap();
    let Some(entry) = guard.get_mut(agent_id) else {
        return;
    };
    push_tool_locked(entry, tool);
    let snapshot = entry.snapshot.clone();
    drop(guard);
    broadcast_activity(state, &snapshot);
}

fn push_tool_locked(entry: &mut ActivityEntry, tool: CompletedTool) {
    entry.snapshot.recent_completed_tools.push_back(tool);
    while entry.snapshot.recent_completed_tools.len() > RECENT_TOOLS_CAP {
        entry.snapshot.recent_completed_tools.pop_front();
    }
}

/// A native runtime's tool call ended. It joins the recent-tools ring, and
/// when it is the step the live strip shows, the strip moves on to
/// `ContactingModel` ("thinking…") at once, instead of showing a finished
/// call as running until the next text streams. Fenced to `turn_id`, like
/// every native callback, so a late update never repaints a newer turn.
pub(crate) fn end_native_tool(
    state: &AppState,
    agent_id: &str,
    turn_id: &str,
    call_id: &str,
    title: Option<String>,
    ok: bool,
) {
    let mut guard = state.agent_activity.lock().unwrap();
    let Some(entry) = guard.get_mut(agent_id) else {
        return;
    };
    if entry.snapshot.turn_id != turn_id || entry.snapshot.phase.is_terminal() {
        return;
    }
    let shown = match &entry.snapshot.phase {
        ActivityPhase::RunningTool {
            call_id: running,
            tool,
            ..
        } if running == call_id => Some(tool.clone()),
        _ => None,
    };
    if let Some(tool) = title.or_else(|| shown.clone()) {
        push_tool_locked(
            entry,
            CompletedTool {
                tool,
                call_id: call_id.to_string(),
                ok,
                finished_at: chrono::Utc::now().to_rfc3339(),
            },
        );
    }
    // ponytail: a parallel call still running is not shown again; its next
    // output line brings it back (`show_running_command`).
    if shown.is_some() {
        set_phase(&mut entry.snapshot, ActivityPhase::ContactingModel);
    }
    let snapshot = entry.snapshot.clone();
    drop(guard);
    broadcast_activity(state, &snapshot);
}

/// Composes a provider's raw report (`bakery_agent::TurnUsage` —
/// tokens only) into this node's own `TurnUsage` shape (07d-A3D1 Phase C):
/// `context_used` is derived from whichever of the two token counts are
/// actually present (never fabricated when both are absent); `context_window`
/// is populated only when `override_model_id` is `Some` — without a
/// per-turn model override this node only ever knows the provider *bucket*
/// ("claude"/"grok"/"gemini"), never the specific secrets-configured
/// variant actually running, so there is no catalog entry to honestly key
/// on. A runtime's own `detail` rides along as reported, and its context
/// reading, when it has one, replaces both derived halves. Returns `None`
/// (skip the whole `agent.turn.usage` event) when the provider reported
/// nothing at all.
pub(crate) fn compose_turn_usage(
    state: &AppState,
    provider: &str,
    override_model_id: Option<&str>,
    reported: Option<bakery_agent::TurnUsage>,
    detail: Option<bakery_agent::TurnUsageDetail>,
) -> Option<TurnUsage> {
    let reported = reported.unwrap_or_default();
    let detail = detail.unwrap_or_default();
    if reported.tokens_in.is_none()
        && reported.tokens_out.is_none()
        && detail == bakery_agent::TurnUsageDetail::default()
    {
        return None;
    }
    let mut context_window =
        override_model_id.and_then(|id| state.catalog_cache.cached_context_window(provider, id));
    let mut context_used = match (reported.tokens_in, reported.tokens_out) {
        (Some(i), Some(o)) => Some(i + o),
        (Some(i), None) => Some(i),
        (None, Some(o)) => Some(o),
        (None, None) => None,
    };
    if detail.context_window.is_some() {
        context_used = detail.context_used;
        context_window = detail.context_window;
    }
    Some(TurnUsage {
        tokens_in: reported.tokens_in,
        tokens_out: reported.tokens_out,
        context_used,
        context_window,
        thought_tokens: detail.thought_tokens,
        cache_read_tokens: detail.cache_read_tokens,
        cache_write_tokens: detail.cache_write_tokens,
        cost: detail.cost,
        duration_ms: None,
    })
}

/// Records a turn's usage onto its live activity snapshot (07d-A3D1 Phase
/// C) — best-effort, a missing entry (the turn already ended, or this
/// transport never started activity tracking, e.g. ACP) is silently a
/// no-op. Finally populates [`AgentActivity::usage`], `None` since 07d-A5
/// explicitly deferred it to this phase.
pub(crate) fn record_turn_usage(state: &AppState, agent_id: &str, usage: TurnUsage) {
    let mut guard = state.agent_activity.lock().unwrap();
    let Some(entry) = guard.get_mut(agent_id) else {
        return;
    };
    entry.snapshot.usage = Some(usage);
    let snapshot = entry.snapshot.clone();
    drop(guard);
    broadcast_activity(state, &snapshot);
}

/// Transitions `agent_id`'s activity to `phase` **only if** its current
/// phase is still `AwaitingApproval` for exactly `event_hash` — the guard a
/// decision route (`/agent/accept`, `/agent/reject`, `/agent/pipe/gate/
/// decide`) needs so a stale/duplicate decision (already refused by the
/// route's own idempotency check, but resolved here defensively too) can
/// never clobber a newer turn's fresh activity for the same agent.
pub(crate) fn resolve_awaiting_approval(
    state: &AppState,
    agent_id: &str,
    event_hash: &str,
    phase: ActivityPhase,
) {
    let mut guard = state.agent_activity.lock().unwrap();
    let Some(entry) = guard.get_mut(agent_id) else {
        return;
    };
    let still_awaiting_this_one = matches!(
        &entry.snapshot.phase,
        ActivityPhase::AwaitingApproval { event_hash: h, .. } if h == event_hash
    );
    if !still_awaiting_this_one {
        return;
    }
    apply_phase_locked(entry, phase);
    let snapshot = entry.snapshot.clone();
    drop(guard);
    broadcast_activity(state, &snapshot);
}

/// Records the operating mode the agent's current turn runs in.
pub(crate) fn set_turn_mode(state: &AppState, agent_id: &str, mode: crate::agent_control::OperatingMode) {
    if let Some(entry) = state.agent_activity.lock().unwrap().get_mut(agent_id) {
        entry.snapshot.mode = Some(mode);
    }
}

/// `GET /agent/activity`'s read — a plain clone under the lock, `None` when
/// no turn is (or has ever been) tracked for this agent.
pub(crate) fn agent_activity_snapshot(state: &AppState, agent_id: &str) -> Option<AgentActivity> {
    state
        .agent_activity
        .lock()
        .unwrap()
        .get(agent_id)
        .map(|e| e.snapshot.clone())
}

/// `POST /agent/turn/cancel`'s effect — flips the shared flag `chat_turn_
/// streaming` (and `agent_tool_turn`'s own loop) polls, marks
/// `cancel_requested` so a client sees "cancelling…" immediately, and
/// broadcasts that fact right away (before the turn has actually stopped —
/// only the turn loop itself later appends `agent.turn.cancelled`, since
/// only it can honestly know whether cancellation actually won the race
/// against the turn finishing on its own). Returns `false` (a clear
/// refusal, never a silent no-op) when no turn is tracked for this agent,
/// or when the tracked turn has already reached a terminal phase (07d-A5
/// review #8) — an abandoned/late client hitting Cancel after the turn
/// already finished must not be told it worked.
pub(crate) fn request_cancel(state: &AppState, agent_id: &str) -> bool {
    let mut guard = state.agent_activity.lock().unwrap();
    let Some(entry) = guard.get_mut(agent_id) else {
        return false;
    };
    if entry.snapshot.phase.is_terminal() {
        return false;
    }
    entry
        .cancel
        .store(true, std::sync::atomic::Ordering::Release);
    entry.snapshot.cancel_requested = true;
    let snapshot = entry.snapshot.clone();
    drop(guard);
    broadcast_activity(state, &snapshot);
    true
}

/// Admits a hold on `/agent/turn`, `/agent/accept`, `/agent/reject`, or
/// `/files/*` for the duration of the call — same retiring/discarded gate as
/// [`admit_one_shot`], but non-exclusive: unlike two env attaches racing one
/// writable runtime dir, two of these have nothing to mutually exclude (two
/// file reads, or a turn alongside a read, are both fine concurrently).
/// What they share with `admit_one_shot` is that discard must not rm the
/// worktree out from under one that's still running — a long Grok turn is
/// exactly the "counted activity" 04c-BURN §4 asks these routes to carry,
/// the same as env's one-shot already did.
pub(crate) fn admit_activity(
    state: &AppState,
    stream_id: &str,
) -> Result<ActivityLease, (StatusCode, String)> {
    let mut g = state.lifecycle.lock().unwrap();
    if g.retiring.contains(stream_id) {
        return Err((
            StatusCode::CONFLICT,
            "stream is being discarded".to_string(),
        ));
    }
    if discarded_of(&state.heart.lock().unwrap(), stream_id) {
        return Err((
            StatusCode::BAD_REQUEST,
            "stream has been discarded".to_string(),
        ));
    }
    g.activity.entry(stream_id.to_string()).or_default().leases += 1;
    Ok(ActivityLease {
        state: state.clone(),
        stream_id: stream_id.to_string(),
    })
}

/// Bounded wait for a one-shot attach or a non-exclusive activity lease
/// admitted just before `retiring` was set — the respective `admit_*`
/// refuses anything new the instant retiring is true, but something
/// admitted a moment earlier is still running and must finish (or time out)
/// before the worktree/room are touched, never raced past. Returns `false`
/// on timeout — the caller must abort the discard rather than proceed
/// (04b-FENCE §3: "fence timeout = abort cleanup, not `rm` anyway").
pub(crate) async fn wait_for_one_shot_clear(
    state: &AppState,
    stream_id: &str,
    timeout: Duration,
) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        let busy = state
            .lifecycle
            .lock()
            .unwrap()
            .activity
            .get(stream_id)
            .map(|a| a.one_shot || a.leases > 0)
            .unwrap_or(false);
        if !busy {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

pub(crate) fn str_field(v: &serde_json::Value, key: &str) -> Option<String> {
    v.get(key).and_then(|x| x.as_str()).map(str::to_string)
}

/// Do two paths name the same place? Canonicalize both sides so `~/dev/widget`
/// and a symlinked equivalent compare equal; fall back to a plain string compare
/// when canonicalize fails, so a directory that has been deleted is never
/// misreported (and never panics).
///
/// Used for two questions: "is this worktree the repo's own checkout?"
/// (in_place) and "is the caller asking for the tree this stream already owns?"
pub(crate) fn same_path(a: &str, b: &str) -> bool {
    match (
        std::path::Path::new(a).canonicalize(),
        std::path::Path::new(b).canonicalize(),
    ) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

/// Resolve a stream's worktree context from its `project.onboarded` event —
/// **except** for `worktree_path`/`repo_root`/`branch`, which a newer
/// `socket.workspace.set` fact overrides when one exists (07d-B Phase 3:
/// task sockets mint chat-only, with an empty onboarded workspace, and bind
/// a real one later at `task.start` materialization time without a second
/// `project.onboarded` append). Latest-wins, same posture as every other
/// socket-stream fact; a newest record that exists but fails to parse fails
/// the whole call closed rather than silently falling back to the stale
/// onboarded fields — the same "don't skip a bad newer record" rule
/// `latest_seat_model_id` documents.
pub(crate) fn context_for(state: &AppState, stream_id: &str) -> Result<ActiveContext, String> {
    let onboarded = state
        .heart
        .lock()
        .unwrap()
        .first_of_family(stream_id, "project.onboarded")
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("stream {stream_id} has no project.onboarded event"))?;
    let p: serde_json::Value =
        serde_json::from_str(&onboarded.payload).map_err(|e| e.to_string())?;
    let mut worktree_path = str_field(&p, "worktree_path")
        .ok_or_else(|| "onboarded event missing worktree_path".to_string())?;
    let mut repo_root = str_field(&p, "repo_root")
        .or_else(|| str_field(&p, "repo"))
        .unwrap_or_default();
    let mut branch = str_field(&p, "branch").unwrap_or_default();
    let mut created_by_bakery = p
        .get("created_by_bakery")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if let Some(ev) = state
        .heart
        .lock()
        .unwrap()
        .last_of_family(stream_id, SOCKET_WORKSPACE_FAMILY)
        .map_err(|e| e.to_string())?
    {
        let w: WorkspaceSet = serde_json::from_str(&ev.payload)
            .map_err(|e| format!("socket.workspace.set unreadable: {e}"))?;
        worktree_path = w.worktree_path;
        repo_root = w.repo_root;
        branch = w.branch;
        // Provenance travels WITH the fact now (`WorkspaceSet::created_by_bakery`)
        // rather than being assumed true here: `tasks::task_start` can also
        // adopt an already-existing directory it did not create (crash
        // recovery, or a human's pre-existing worktree), and that append
        // honestly records `false` — hard-coding `true` here would let
        // discard force-delete a worktree Bakery never made.
        created_by_bakery = w.created_by_bakery;
    }

    let in_place = same_path(&worktree_path, &repo_root);
    Ok(ActiveContext {
        stream_id: stream_id.to_string(),
        name: str_field(&p, "name").unwrap_or_default(),
        project: str_field(&p, "project").unwrap_or_default(),
        repo_root,
        branch,
        exists_on_disk: std::path::Path::new(&worktree_path).is_dir(),
        in_place,
        worktree_path,
        owner_node_id: None,
        created_by_bakery,
    })
}

/// A jail rooted at the stream's worktree. Every agent-facing path — proposal,
/// tool call, and the Accept write — resolves through this.
pub(crate) fn jail_for(state: &AppState, stream_id: &str) -> Result<crate::tools::Jail, String> {
    let ctx = context_for(state, stream_id)?;
    crate::tools::Jail::new(std::path::Path::new(&ctx.worktree_path))
}

/// `<base>/cache` — the one root every room-provisioning caller shares
/// (`routes::env`'s HTTP attach routes and the agent tool loop's `run`
/// alike), so two attach paths for the same stream always land in the same
/// `envs/<digest>` / `sockets/<stream_id>`, never two different caches.
pub(crate) fn cache_root(state: &AppState) -> PathBuf {
    state.base.join("cache")
}

// ─── Seats (06-SEATS.md) ────────────────────────────────────────────────────
//
// Seats ≠ models (STEERING.md § Models): a seat (orchestrator / worker /
// baker) is a durable role on a stream; the model id behind it can change
// independently. `seat.model.assigned` is the Heart fact; `AppState::seats`
// is a resolved-model cache over it, read-through on first use per (stream,
// seat) and invalidated on reassignment.

/// Latest `seat.model.assigned` event naming `seat`, or `None` when nothing
/// has ever been assigned (`Ok(None)`), or a genuinely unreadable record
/// stands between here and the answer (`Err(())`).
///
/// Scans newest-first and returns on the **first** event this stream's
/// family holds, full stop — not the first one that happens to parse. An
/// earlier version skipped a record it couldn't parse and kept scanning
/// backwards for an older, valid one for the same seat; that is exactly the
/// wrong direction (06-SEATS audit item 5 — "malformed newer
/// `seat.model.assigned` fails closed, don't skip to an older good one"): a
/// stream could have Slice/Living-style semantics where a *newer* malformed
/// record silently defers to a *stale* older assignment, e.g. a `worker`
/// seat someone tried to repoint at `claude` (write landed, then got
/// corrupted or written by a future schema this build can't parse) quietly
/// keeps answering with whatever `grok` id it had before. Any record this
/// build can't parse — for this seat or any other, since a family this
/// broad can't be attributed to a seat without parsing it first — must stop
/// the scan and fail closed, not be treated as noise to scan past.
pub(crate) fn latest_seat_model_id(
    heart: &Heart,
    stream_id: &str,
    seat: Seat,
) -> Result<Option<String>, ()> {
    let events = heart
        .events_of_family(stream_id, SEAT_ASSIGNED_FAMILY)
        .map_err(|_| ())?;
    for ev in events.iter().rev() {
        let a: SeatAssignment = serde_json::from_str(&ev.payload).map_err(|_| ())?;
        if a.seat == seat {
            return Ok(Some(a.model_id));
        }
    }
    Ok(None)
}

/// This stream's durable, server-recorded kind — factory (Baker) or
/// project — read from the NEWEST `socket.kind.recorded` fact only, never
/// inferred from name/branch/stream_id (07d-A1 correction). `last_of_family`
/// looks at that one record and nothing else: no record at all, or the
/// newest one being unreadable, both fail closed to `Project` — the
/// narrower, non-elevated authority. Deliberately does not fall back to an
/// older valid record the way `latest_seat_model_id` treats a same-seat
/// scan (07d-A1 correction #2: skipping backward past a bad newest record
/// could resurrect a stale kind this stream no longer carries — there is
/// exactly one legitimate writer, `baker_assign_inner`, so "the newest
/// record is unreadable" only ever means real corruption, not a normal
/// multi-writer race worth searching through). No legacy migration for
/// streams that predate this fact (07d-A1: "No legacy migration is
/// needed").
pub(crate) fn socket_kind(heart: &Heart, stream_id: &str) -> SocketKind {
    heart
        .last_of_family(stream_id, SOCKET_KIND_FAMILY)
        .ok()
        .flatten()
        .and_then(|ev| serde_json::from_str::<SocketKindRecord>(&ev.payload).ok())
        .map(|r| r.kind)
        .unwrap_or(SocketKind::Project)
}

/// This stream's durable `socket.project.set` fact — `(project_id, root_id)`,
/// or `(None, None)` (Ungrouped) when the record is missing or the newest one
/// is unreadable. Exact same fail-closed, no-backward-search posture as
/// [`socket_kind`] and for the same reason: there is exactly one legitimate
/// writer per membership change, so a bad newest record only ever means
/// corruption, never a race worth searching through (07d-A3: "Malformed
/// newest catalog/membership facts never fall back to paths or older valid
/// events").
pub(crate) fn socket_project(heart: &Heart, stream_id: &str) -> (Option<String>, Option<String>) {
    heart
        .last_of_family(stream_id, crate::node::SOCKET_PROJECT_FAMILY)
        .ok()
        .flatten()
        .and_then(|ev| serde_json::from_str::<crate::node::SocketProjectSet>(&ev.payload).ok())
        .map(|r| (r.project_id, r.root_id))
        .unwrap_or((None, None))
}

/// This stream's durable `socket.task.set` fact (07d-B Phase 3 item 2) —
/// `Some(task_id)` once a task socket's identity has been minted, `None` for
/// every other socket (or an unreadable newest record — same fail-closed,
/// no-backward-search posture as [`socket_kind`]/[`socket_project`]: there is
/// exactly one legitimate writer, `tasks::task_create`, so a bad newest
/// record only ever means corruption). The one reader of this fact that
/// matters operationally: the HTTP tool loop's "is this a task socket"
/// check (07d-B Phase 2 item 4 — `task_complete` is advertised only here).
pub(crate) fn socket_task_id(heart: &Heart, stream_id: &str) -> Option<String> {
    heart
        .last_of_family(stream_id, crate::node::SOCKET_TASK_FAMILY)
        .ok()
        .flatten()
        .and_then(|ev| serde_json::from_str::<crate::node::SocketTaskSet>(&ev.payload).ok())
        .map(|r| r.task_id)
}

/// Record a stream's declared model id for one seat. Does not itself resolve
/// or validate the model id against live credentials — STEERING.md ties the
/// fail-closed check to `/agent/turn`, not to assignment, so a seat can be
/// pinned to `claude` before Setup ever adds an API key. What this DOES do is
/// evict any cached resolution for `(stream_id, seat)`: a reassignment must
/// never keep answering turns with the model the seat used to point at.
///
/// Held under the **lifecycle** lock, not just the `seats` cache lock
/// (06-SEATS audit item 6: "`/seat/assign` under the lifecycle lock — no
/// discard race"). The route used to call the read-only
/// `refuse_if_retiring_or_discarded` before this, which is exactly the
/// "check, release, act" shape 04b-FENCE's whole fencing design exists to
/// avoid elsewhere: a discard could commit to `retiring` in the gap between
/// that check and this function's own Heart append, landing an assignment
/// on a stream that's mid-teardown. Acquiring `lifecycle` first — and
/// checking `retiring`/`discarded_of` under it, same as
/// [`set_non_burn_lifetime`]/[`begin_discard`]/[`begin_burn_transition`] —
/// makes the two mutually exclusive instead of racy. Lock order is
/// `lifecycle` → `seats` → `heart`, consistent with every other function
/// that touches more than one of these (never the reverse), so this can
/// never deadlock against [`resolve_seat`]'s own `seats` → `heart` order.
///
/// As a decision: with a `reason`, the payload also records it and the model
/// the seat had; `because` (an event's hash, such as a
/// `model.quota_exhausted`) becomes the event's causality.
#[cfg(test)]
pub(crate) fn assign_seat_model_for(
    state: &AppState,
    stream_id: &str,
    seat: Seat,
    model_id: &str,
    reason: Option<&str>,
    because: Option<&str>,
    provider: Option<&str>,
) -> Result<Event, (StatusCode, String)> {
    assign_seat_model_or_wait(state, stream_id, seat, model_id, reason, because, provider, Landing::Now).map(Switched::into_event)
}

/// When a seat's switch lands.
#[derive(Clone, Copy)]
pub(crate) enum Landing<'a> {
    /// At once.
    Now,
    /// While the seat's turn runs, when that turn ends; otherwise at once.
    AfterTurn,
    /// The waiting switch this `seat.model.pending` (by its hash) records,
    /// only while it is still the newest one waiting: one asked for after
    /// it was read replaces or cancels it.
    Waiting(&'a str),
}

/// What became of a seat's switch.
#[derive(Debug)]
pub(crate) enum Switched {
    /// It landed: the seat's new `seat.model.assigned`.
    Now(Event),
    /// It waits for the seat's running turn to end (`seat.model.pending`);
    /// or, back to where the seat already is, it cancelled a waiting one,
    /// or there was nothing to switch and this is the seat's assignment.
    Later(Event),
}

impl Switched {
    pub(crate) fn into_event(self) -> Event {
        match self {
            Switched::Now(event) | Switched::Later(event) => event,
        }
    }
}

/// [`assign_seat_model_for`]; with [`Landing::AfterTurn`], while `seat`'s turn runs, the
/// switch waits for that turn to end instead of cutting it off: a
/// `seat.model.pending` fact the turn's lease applies ([`TurnLease`]), and
/// until then the seat's assignment, its process and its sign-in at the
/// door stay as they are. Decided under the lifecycle lock the lease checks
/// before it lets the seat go, so a switch asked for as a turn ends either
/// waits for that lease or lands at once, never neither.
#[allow(clippy::too_many_arguments)]
pub(crate) fn assign_seat_model_or_wait(
    state: &AppState,
    stream_id: &str,
    seat: Seat,
    model_id: &str,
    reason: Option<&str>,
    because: Option<&str>,
    provider: Option<&str>,
    landing: Landing,
) -> Result<Switched, (StatusCode, String)> {
    let model_id = model_id.trim();
    if model_id.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "model_id must not be empty".to_string(),
        ));
    }
    let provider = crate::claude_providers::validate(model_id, provider)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    let reason: Option<String> = reason.map(|r| r.chars().take(500).collect());
    let from = reason.as_ref().and_then(|_| {
        latest_seat_model_id(&state.heart.lock().unwrap(), stream_id, seat)
            .ok()
            .flatten()
    });
    let payload = serde_json::to_string(&SeatAssignment {
        seat,
        model_id: model_id.to_string(),
        reason: reason.clone(),
        from,
        provider: provider.clone(),
    })
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let lifecycle = state.lifecycle.lock().unwrap();
    if lifecycle.retiring.contains(stream_id) {
        return Err((
            StatusCode::CONFLICT,
            "stream is being discarded".to_string(),
        ));
    }
    if discarded_of(&state.heart.lock().unwrap(), stream_id) {
        return Err((
            StatusCode::BAD_REQUEST,
            "stream has been discarded".to_string(),
        ));
    }
    if let Landing::Waiting(hash) = landing {
        let newest = seat_switch_waiting_locked(&state.heart.lock().unwrap(), stream_id, seat);
        if newest.is_none_or(|(event, _)| event.event_hash != hash) {
            return Err((
                StatusCode::CONFLICT,
                "it was replaced or cancelled before it landed".to_string(),
            ));
        }
    }
    let acp_key = (stream_id.to_string(), seat);
    if let Landing::AfterTurn = landing {
        let switch = SeatSwitch { seat, model_id: model_id.to_string(), provider: provider.clone(), reason: reason.clone(), over: None };
        let running = lifecycle.running_turns.get(&acp_key).is_some_and(|n| *n > 0);
        // Idle, back where the seat is while a switch still waits (one a
        // restart left): it cancels that switch, and the seat's process runs on.
        let cancels = !running && {
            let heart = state.heart.lock().unwrap();
            seat_switch_waiting_locked(&heart, stream_id, seat).is_some()
                && latest_seat_assignment_locked(&heart, stream_id, seat)
                    .ok()
                    .flatten()
                    .is_some_and(|(_, now)| same_place(&now, &switch))
        };
        if running || cancels {
            let waiting = wait_for_turn_end(state, stream_id, switch, because);
            drop(lifecycle);
            return waiting.map(Switched::Later);
        }
    }
    let close_target = {
        let mut sessions = state.acp_sessions.lock().unwrap();
        match sessions.get(&acp_key) {
            Some(crate::pipe::AcpSlot::Live(live)) => {
                let generation = live.generation;
                let pid = live.pid;
                live.close();
                sessions.insert(
                    acp_key.clone(),
                    crate::pipe::AcpSlot::Closing { generation },
                );
                Some((generation, Some(pid)))
            }
            Some(crate::pipe::AcpSlot::Spawning { generation, closed }) => {
                let generation = *generation;
                closed.store(true, std::sync::atomic::Ordering::Release);
                sessions.insert(
                    acp_key.clone(),
                    crate::pipe::AcpSlot::Closing { generation },
                );
                Some((generation, None))
            }
            Some(crate::pipe::AcpSlot::Closing { .. }) => {
                return Err((
                    StatusCode::CONFLICT,
                    "the seat's prior ACP session is still closing — retry shortly".to_string(),
                ));
            }
            None => None,
        }
    };

    let mut cache = state.seats.lock().unwrap();
    let append_result = append(
        state,
        stream_id,
        "human",
        SEAT_ASSIGNED_FAMILY,
        payload,
        because.map(|b| vec![b.to_string()]).unwrap_or_default(),
    )
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e));
    let old_model = if append_result.is_ok() {
        cache.remove(&(stream_id.to_string(), seat))
    } else {
        None
    };
    if let Some(model) = &old_model {
        model.revoke();
    }
    // The seat is about to point at a different model (or none): any MCP
    // token minted for this stream under the OLD assignment must not keep
    // granting its scope for the rest of this process's life once the new
    // assignment lands — see `crate::mcp`'s module doc. Safe to call
    // unconditionally (including on a stream that never had a token, or on
    // a failed append below): a fresh token for the new assignment is only
    // ever minted lazily, on this stream's NEXT `resolve_model_id_for_seat`
    // call, strictly after this point, so there is nothing to race.
    crate::mcp::evict_mcp_tokens_for_stream(state, stream_id);

    drop(cache);
    drop(lifecycle);
    drop(old_model);

    // The session thread owns the Child handle and is the only code that can
    // reap it. Keep the generation-tagged Closing sentinel until that exact
    // thread removes it; an older thread can never clear a newer generation.
    if let Some((generation, pid)) = close_target {
        if let Some(pid) = pid {
            bakery_exec::kill_process_group(pid);
        }
        if !wait_for_acp_generation_close(state, &acp_key, generation, pid, Duration::from_secs(4))
        {
            let mut sessions = state.acp_sessions.lock().unwrap();
            if !sessions.contains_key(&acp_key) {
                sessions.insert(
                    acp_key.clone(),
                    crate::pipe::AcpSlot::Closing { generation },
                );
            }
            let detail = if append_result.is_ok() {
                "seat assignment was recorded"
            } else {
                "seat assignment failed"
            };
            return Err((
                StatusCode::CONFLICT,
                format!(
                    "{detail}, and the prior ACP session did not reap; the seat remains closed"
                ),
            ));
        }
    }

    append_result.map(Switched::Now)
}

/// A switch asked for while `switch.seat`'s turn runs, under the lifecycle
/// lock the caller holds: recorded over the seat's newest assignment, to
/// apply when that turn ends. Back to where the seat already is, it cancels
/// the one waiting, if one is; with none waiting it asks nothing, and the
/// answer is the seat's own assignment.
fn wait_for_turn_end(
    state: &AppState,
    stream_id: &str,
    mut switch: SeatSwitch,
    because: Option<&str>,
) -> Result<Event, (StatusCode, String)> {
    let unreadable = || (StatusCode::INTERNAL_SERVER_ERROR, "the seat's assignment record is unreadable".to_string());
    let (assigned, waiting) = {
        let heart = state.heart.lock().unwrap();
        let assigned = latest_seat_assignment_locked(&heart, stream_id, switch.seat).map_err(|()| unreadable())?;
        (assigned, seat_switch_waiting_locked(&heart, stream_id, switch.seat).is_some())
    };
    match assigned {
        Some((event, now)) if !waiting && same_place(&now, &switch) => Ok(event),
        assigned => {
            switch.over = assigned.map(|(event, _)| event.event_hash);
            let payload = serde_json::to_string(&switch).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
            let causality = because.map(|b| vec![b.to_string()]).unwrap_or_default();
            append(state, stream_id, "human", SEAT_SWITCH_FAMILY, payload, causality)
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
        }
    }
}

/// The seat's newest `seat.model.assigned` and its event; an unreadable
/// record fails, as [`latest_seat_model_id`] does.
fn latest_seat_assignment_locked(heart: &Heart, stream_id: &str, seat: Seat) -> Result<Option<(Event, SeatAssignment)>, ()> {
    let events = heart.events_of_family(stream_id, SEAT_ASSIGNED_FAMILY).map_err(|_| ())?;
    for event in events.into_iter().rev() {
        let assigned: SeatAssignment = serde_json::from_str(&event.payload).map_err(|_| ())?;
        if assigned.seat == seat {
            return Ok(Some((event, assigned)));
        }
    }
    Ok(None)
}

/// Whether a switch goes where the seat already is: the same model and, for
/// Claude native, the same provider (none named is the subscription).
fn same_place(now: &SeatAssignment, switch: &SeatSwitch) -> bool {
    let provider = |p: &Option<String>| p.as_deref().and_then(crate::claude_providers::Provider::parse).unwrap_or_default();
    now.model_id.eq_ignore_ascii_case(&switch.model_id) && provider(&now.provider) == provider(&switch.provider)
}

/// The switch waiting for `seat`'s turn to end, if one does, with its
/// `seat.model.pending` (whose causality is the event that caused it): the
/// seat's newest, made over its newest assignment, to anywhere but where
/// the seat already is, and not one that failed to land. None when a record
/// can't be read.
pub(crate) fn seat_switch_waiting_locked(heart: &Heart, stream_id: &str, seat: Seat) -> Option<(Event, SeatSwitch)> {
    let (event, switch, failed) = newest_switch_locked(heart, stream_id, seat)?;
    failed.is_none().then_some((event, switch))
}

/// The seat's newest switch when it failed to land and nothing has moved the
/// seat since: its `seat.model.pending`, where it went, and why it failed.
pub(crate) fn seat_switch_failed_locked(heart: &Heart, stream_id: &str, seat: Seat) -> Option<(Event, SeatSwitch, String)> {
    let (event, switch, failed) = newest_switch_locked(heart, stream_id, seat)?;
    Some((event, switch, failed?))
}

/// The seat's newest switch, made over its newest assignment, to anywhere
/// but where the seat already is, and why it failed to land when it did
/// (`seat.model.switch_failed`).
fn newest_switch_locked(heart: &Heart, stream_id: &str, seat: Seat) -> Option<(Event, SeatSwitch, Option<String>)> {
    let mut newest = None;
    for event in heart.events_of_family(stream_id, SEAT_SWITCH_FAMILY).ok()?.into_iter().rev() {
        let switch: SeatSwitch = serde_json::from_str(&event.payload).ok()?;
        if switch.seat == seat {
            newest = Some((event, switch));
            break;
        }
    }
    let (event, switch) = newest?;
    let assigned = latest_seat_assignment_locked(heart, stream_id, seat).ok()?;
    let over = assigned.as_ref().map(|(event, _)| event.event_hash.as_str());
    if switch.over.as_deref() != over || assigned.as_ref().is_some_and(|(_, now)| same_place(now, &switch)) {
        return None;
    }
    let failed = heart
        .events_of_family(stream_id, crate::node::SEAT_SWITCH_FAILED_FAMILY)
        .ok()?
        .into_iter()
        .rev()
        .find(|e| e.causality.contains(&event.event_hash))
        .and_then(|e| serde_json::from_str::<crate::node::SeatSwitchFailed>(&e.payload).ok())
        .map(|f| f.error);
    Some((event, switch, failed))
}

/// An assignment with no reason or cause, as the tests set one.
#[cfg(test)]
pub(crate) fn assign_seat_model(
    state: &AppState,
    stream_id: &str,
    seat: Seat,
    model_id: &str,
) -> Result<Event, (StatusCode, String)> {
    assign_seat_model_for(state, stream_id, seat, model_id, None, None, None)
}

fn wait_for_acp_generation_close(
    state: &AppState,
    key: &(String, Seat),
    generation: u64,
    pid: Option<u32>,
    timeout: Duration,
) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        let generation_present = state
            .acp_sessions
            .lock()
            .unwrap()
            .get(key)
            .is_some_and(|slot| slot.generation() == generation);
        let pid_alive = pid.is_some_and(|pid| unsafe { libc::kill(pid as libc::pid_t, 0) } == 0);
        if !generation_present && !pid_alive {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Node-wide (not stream-specific) half of ACP spawn configuration: which
/// binary, which extra argv, which auth/config copy. Shared by
/// [`resolve_model_id_for_seat`] (real spawn) and `routes::studio`'s
/// readiness projection, so the two can never validate ACP setup
/// differently. Checks the binary actually exists on disk — spawning would
/// fail on a missing binary anyway (`bakery_exec::stage_acp_binary`'s first
/// step is `std::fs::read`), so failing here is the same fail-closed
/// decision, just earlier and with a message a human can act on.
pub(crate) fn resolve_acp_binary_config(
    state: &AppState,
) -> Result<(PathBuf, Vec<String>, Option<crate::pipe::AcpConfigCopyPlan>), String> {
    // Prefer BAKERY_ACP_CLI env (one-process override) over node.json.
    let binary_s = std::env::var("BAKERY_ACP_CLI")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            state
                .node
                .lock()
                .unwrap()
                .acp_cli
                .clone()
                .filter(|s| !s.trim().is_empty())
        });
    let binary = match binary_s {
        Some(b) => resolve_executable_path(&b).map_err(|()| {
            // Never echo the configured path back through /studio/state — a
            // private absolute path is not a "human-readable reason", it's a
            // filesystem layout leak.
            "Setup needed: configured acp_cli binary not found — check acp_cli in ~/.bakery/node.json or BAKERY_ACP_CLI, or install it on PATH".to_string()
        })?,
        None => return Err(
            "Setup needed: set BAKERY_ACP_CLI env or add \"acp_cli\": \"/path/to/cli\" to ~/.bakery/node.json".to_string()
        ),
    };
    // argv suffix: BAKERY_ACP_ARGS (whitespace-split) overrides node.acp_args.
    let argv_suffix = std::env::var("BAKERY_ACP_ARGS")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.split_whitespace().map(str::to_owned).collect::<Vec<_>>())
        .unwrap_or_else(|| state.node.lock().unwrap().acp_args.clone());
    // Auth/config copy is a pair: a narrow host source plus the exact
    // relative location the CLI expects inside its isolated seat HOME.
    // A half-configured environment or the old source-only node field is
    // a Setup error, never permission to spawn silently without auth.
    let env_config_source = std::env::var("BAKERY_ACP_CONFIG_SRC")
        .ok()
        .filter(|s| !s.trim().is_empty());
    let env_config_destination = std::env::var("BAKERY_ACP_CONFIG_DEST")
        .ok()
        .filter(|s| !s.trim().is_empty());
    let config_copy = match (env_config_source, env_config_destination) {
        (Some(source), Some(home_relative_destination)) => Some(crate::pipe::AcpConfigCopyPlan {
            source: PathBuf::from(source),
            home_relative_destination: PathBuf::from(home_relative_destination),
        }),
        (None, None) => {
            let node = state.node.lock().unwrap();
            if node
                .acp_config_src
                .as_deref()
                .is_some_and(|s| !s.trim().is_empty())
            {
                return Err(
                    "Setup needed: replace source-only acp_config_src with acp_config { source, home_relative_destination }"
                        .to_string(),
                );
            }
            node.acp_config
                .as_ref()
                .map(|copy| crate::pipe::AcpConfigCopyPlan {
                    source: PathBuf::from(&copy.source),
                    home_relative_destination: PathBuf::from(&copy.home_relative_destination),
                })
        }
        _ => {
            return Err(
                "Setup needed: BAKERY_ACP_CONFIG_SRC and BAKERY_ACP_CONFIG_DEST must be set together"
                    .to_string(),
            );
        }
    };
    if let Some(ref copy) = config_copy {
        validate_acp_config_copy(
            state,
            copy,
            "acp_config.source",
            "acp_config.home_relative_destination",
        )?;
    }
    Ok((binary, argv_suffix, config_copy))
}

/// Resolve a configured ACP binary (`acp_cli`, or a named plug's
/// `argv[0]`) to an absolute, existing file path (07d-A3B review #5: a
/// normal executable name like `"codex"` or `"claude"` must resolve
/// through PATH the same way `std::process::Command`/`execvp` would, not
/// require an absolute path — checking `PathBuf::from(name).is_file()`
/// alone only ever matches a path that is already absolute or relative to
/// the node's own cwd). A name containing a path separator is treated as a
/// literal path (relative or absolute) and checked directly, exactly
/// mirroring `execvp`'s own rule of "a slash anywhere disables PATH
/// search"; a bare name is looked up against every `PATH` directory in
/// order, first match wins.
pub(crate) fn resolve_executable_path(cmd: &str) -> Result<PathBuf, ()> {
    let candidate = PathBuf::from(cmd);
    if cmd.contains(std::path::MAIN_SEPARATOR) {
        return if candidate.is_file() {
            Ok(candidate)
        } else {
            Err(())
        };
    }
    let path_var = std::env::var_os("PATH").ok_or(())?;
    for dir in std::env::split_paths(&path_var) {
        let full = dir.join(cmd);
        if full.is_file() {
            return Ok(full);
        }
    }
    Err(())
}

/// Shared by [`resolve_acp_binary_config`] (the legacy singular row) and
/// [`resolve_acp_plug_config`] (a named plug): validate the same two things
/// `copy_config_into_seat_home` will validate at spawn time, so a broken
/// config source/destination shows up in the readiness projection instead of
/// only surfacing once a turn actually tries to spawn. Sanitized: these
/// checks can fail with an absolute host path in the error, which must never
/// reach `/studio/state` — `source_field`/`dest_field` name the *config key*
/// in the error, never the configured value.
fn validate_acp_config_copy(
    state: &AppState,
    copy: &crate::pipe::AcpConfigCopyPlan,
    source_field: &str,
    dest_field: &str,
) -> Result<(), String> {
    let host_home = state
        .base
        .parent()
        .ok_or_else(|| "Setup needed: Bakery base has no host HOME parent".to_string())?;
    crate::pipe::validated_config_source(&copy.source, host_home, &state.base)
        .map_err(|_| format!("Setup needed: ACP config source is invalid — check {source_field} in ~/.bakery/node.json"))?;
    crate::pipe::normalized_home_destination(&copy.home_relative_destination)
        .map_err(|_| format!("Setup needed: ACP config destination is invalid — check {dest_field} in ~/.bakery/node.json"))?;
    Ok(())
}

/// Node-wide half of ACP spawn configuration for one **named** plug
/// (07d-A3B), keyed by `AcpPlugConfig.id` — the multi-plug sibling of
/// [`resolve_acp_binary_config`]'s single legacy row. Shared by
/// [`resolve_model_id_for_seat`] (real spawn) and `routes::studio`'s
/// readiness projection, same reasoning as the legacy resolver: the two must
/// never validate a plug's setup differently.
pub(crate) fn resolve_acp_plug_config(
    state: &AppState,
    plug_id: &str,
) -> Result<(PathBuf, Vec<String>, Option<crate::pipe::AcpConfigCopyPlan>), String> {
    let plug = {
        let node = state.node.lock().unwrap();
        node.acp_plugs.iter().find(|p| p.id == plug_id).cloned()
    }
    .ok_or_else(|| format!("Setup needed: no ACP plug configured with id '{plug_id}' — add it to acp_plugs in ~/.bakery/node.json"))?;

    if plug.argv.is_empty() {
        return Err(format!(
            "Setup needed: ACP plug '{plug_id}' has an empty argv — argv[0] must be the binary"
        ));
    }
    // Never echo the configured value back through /studio/state — same
    // sanitization rule as the legacy row.
    let binary = resolve_executable_path(&plug.argv[0]).map_err(|()| {
        format!("Setup needed: ACP plug '{plug_id}' binary not found — check acp_plugs in ~/.bakery/node.json, or install it on PATH")
    })?;
    let argv_suffix = plug.argv[1..].to_vec();
    let config_copy = plug
        .config
        .as_ref()
        .map(|copy| crate::pipe::AcpConfigCopyPlan {
            source: PathBuf::from(&copy.source),
            home_relative_destination: PathBuf::from(&copy.home_relative_destination),
        });
    if let Some(ref copy) = config_copy {
        validate_acp_config_copy(
            state,
            copy,
            &format!("acp_plugs[id={plug_id}].config.source"),
            &format!("acp_plugs[id={plug_id}].config.home_relative_destination"),
        )?;
    }
    validate_acp_plug_env(&plug.env)?;
    Ok((binary, argv_suffix, config_copy))
}

/// Directory env the boxed ACP child may receive. Token-shaped keys are not
/// on this list — those stay in `secrets.json` and the vendor CLI's own store.
pub(crate) const ACP_CHILD_ENV_ALLOWLIST: &[&str] =
    &["CODEX_HOME", "AGY_BIN", "AGY_CONVERSATIONS_DIR"];

pub(crate) fn validate_acp_plug_env(
    env: &std::collections::HashMap<String, String>,
) -> Result<(), String> {
    for key in env.keys() {
        if !ACP_CHILD_ENV_ALLOWLIST.iter().any(|k| *k == key.as_str()) {
            return Err(format!(
                "Setup needed: ACP env key '{key}' is not allowlisted — use CODEX_HOME, AGY_BIN, or AGY_CONVERSATIONS_DIR"
            ));
        }
    }
    Ok(())
}

pub(crate) fn acp_plug_extra_env(
    plug: &crate::node::AcpPlugConfig,
) -> Result<Vec<(String, String)>, String> {
    validate_acp_plug_env(&plug.env)?;
    Ok(plug
        .env
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect())
}

/// Node-wide readiness for one configured openai-compat plug (07d-A3B) — a
/// syntax-only check (no network call) so `/studio/state` can report Setup
/// before a turn ever tries to reach the endpoint.
pub(crate) fn openai_compat_plug_ready(
    plug: &crate::node::OpenAiCompatPlugConfig,
) -> Result<(), String> {
    bakery_agent::openai_compat::validate_base_url(&plug.base_url)
}

/// This stream's MCP spawn config for an ACP session (07d-B Phase 2 Stage C
/// item 3) — `None` for an ordinary project socket (today's `mcp_servers:
/// vec![]` behavior, unchanged). `Some` mints a FRESH per-session token
/// (`crate::mcp::mint_mcp_token`) bound to `(stream_id, seat, scope)` and
/// points the shim at this same running node's own binary and its own
/// loopback address — never at anything a caller could supply.
pub(crate) fn mcp_spawn_config_for(
    state: &AppState,
    stream_id: &str,
    seat: Seat,
) -> Result<Option<crate::pipe::McpSpawnConfig>, String> {
    let Some(scope) = crate::mcp::mcp_scope_for_stream(state, stream_id) else {
        return Ok(None);
    };
    let token = crate::mcp::mint_mcp_token(state, stream_id, seat, scope);
    let command = std::env::current_exe().map_err(|e| {
        format!("Setup needed: cannot resolve this node's own binary path for the MCP shim: {e}")
    })?;
    // A room on this Mac reaches the Bakery only at its thread's own door
    // (`doors`); a container seat's MCP goes through its gateway instead.
    let base_addr = match crate::machine_access::reach_for(state, stream_id) {
        crate::machine_access::Reach::Container | crate::machine_access::Reach::Locked => {
            std::env::var("BAKERY_NODE_ADDR").unwrap_or_else(|_| "127.0.0.1:7717".to_string())
        }
        _ => format!("127.0.0.1:{}", crate::doors::door(state, stream_id)?),
    };
    Ok(Some(crate::pipe::McpSpawnConfig {
        command,
        args: vec!["mcp-shim".to_string()],
        env: vec![
            ("BAKERY_MCP_TOKEN".to_string(), token),
            (
                "BAKERY_MCP_BASE_URL".to_string(),
                format!("http://{base_addr}"),
            ),
        ],
    }))
}

/// Resolve one seat's model id to a live client — the one point where a
/// pipe plug's own model id (`pipe::ACP_MODEL_ID`) is recognized ahead
/// of `bakery_agent::resolve_seat_model`'s HTTP-API providers, since a
/// piped plug needs this stream's own worktree path (`context_for`) to
/// construct, which a plain provider string never does.
fn resolve_model_id_for_seat(
    state: &AppState,
    stream_id: &str,
    seat: Seat,
    model_id: &str,
) -> Result<Arc<dyn Model>, String> {
    let trimmed = model_id.trim();
    // Built-in native Claude agent. This is deliberately a separate model
    // identity from the existing `claude-code` Bakery-owned provider loop:
    // old Heart assignments therefore keep resolving exactly as before.
    if trimmed.eq_ignore_ascii_case(crate::claude_native::ACCOUNT_ID) {
        let ctx = context_for(state, stream_id)?;
        let workspace = PathBuf::from(ctx.worktree_path);
        let trusted = stream_host_allowed(state, stream_id);
        let mcp = mcp_spawn_config_for(state, stream_id, seat)?;
        let provider = crate::claude_providers::for_seat(state, stream_id, seat);
        let config = crate::claude_native::acp_spawn_config(state, stream_id, seat.as_str(), provider, trusted, mcp)?;
        return Ok(Arc::new(crate::pipe::AcpPipeModel::new(
            state.clone(),
            stream_id.to_string(),
            seat,
            workspace,
            config,
            crate::claude_native::ACCOUNT_ID.to_string(),
        )));
    }
    if trimmed.eq_ignore_ascii_case(crate::pipe::ACP_MODEL_ID)
        || trimmed.eq_ignore_ascii_case("grok-acp")
    {
        let (binary, argv_suffix, config_copy) = resolve_acp_binary_config(state)?;
        let ctx = context_for(state, stream_id)?;
        let workspace = PathBuf::from(ctx.worktree_path);
        // Non-Burn streams are trusted: the ACP child may write the worktree.
        let trusted = stream_host_allowed(state, stream_id);
        // The legacy singular row has no `mcp_transport` field to gate on
        // (07d-B Phase 2 Stage C item 4: "default stdio for the legacy
        // acp_cli row") — it always gets MCP wiring when the stream calls
        // for it.
        let mcp = mcp_spawn_config_for(state, stream_id, seat)?;
        let config = crate::pipe::AcpSpawnConfig {
            binary,
            argv_suffix,
            config_copy,
            extra_env: vec![],
            trusted,
            mcp,
            extra_process_exec_dirs: vec![],
            credential_dirs: vec![],
            read_only: Vec::new(),
        };
        return Ok(Arc::new(crate::pipe::AcpPipeModel::new(
            state.clone(),
            stream_id.to_string(),
            seat,
            workspace,
            config,
            trimmed.to_string(),
        )));
    }

    // Named ACP plug (07d-A3B: `acp:grok` / `acp:claude` / `acp:codex`, or any
    // operator-chosen id) — checked ahead of `bakery_agent::resolve_seat_model`
    // the same way the legacy singular row is, since a plug needs this
    // stream's own worktree path to construct.
    let acp_plug = {
        let node = state.node.lock().unwrap();
        node.acp_plugs.iter().find(|p| p.id == trimmed).cloned()
    };
    if let Some(plug) = acp_plug {
        let (binary, argv_suffix, config_copy) = resolve_acp_plug_config(state, &plug.id)?;
        let extra_env = acp_plug_extra_env(&plug)?;
        let ctx = context_for(state, stream_id)?;
        let workspace = PathBuf::from(ctx.worktree_path);
        let trusted = stream_host_allowed(state, stream_id);
        // 07d-B Phase 2 Stage C item 4: a plug marked `mcp_transport: "none"`
        // cannot host the permanent Baker or a task worker — refuse the
        // spawn honestly rather than silently booting it without MCP tools.
        // `routes::studio::model_ready_for_socket` is supposed to have
        // already kept this plug off the picker for such a socket; this is
        // the defense-in-depth refusal for whatever reaches here anyway.
        if crate::mcp::mcp_required(state, stream_id) && !plug.supports_mcp_stdio() {
            return Err(format!(
                "Setup needed: ACP plug '{}' has mcp_transport=\"none\" and cannot host the permanent Baker or a task worker",
                plug.id
            ));
        }
        // Any other agent on a plug that can't carry MCP runs without its
        // Bakery tools.
        let mcp = match plug.supports_mcp_stdio() {
            true => mcp_spawn_config_for(state, stream_id, seat)?,
            false => None,
        };
        let config = crate::pipe::AcpSpawnConfig {
            binary,
            argv_suffix,
            config_copy,
            extra_env,
            trusted,
            mcp,
            extra_process_exec_dirs: vec![],
            credential_dirs: vec![],
            read_only: Vec::new(),
        };
        return Ok(Arc::new(crate::pipe::AcpPipeModel::new(
            state.clone(),
            stream_id.to_string(),
            seat,
            workspace,
            config,
            plug.id,
        )));
    }

    // Named openai-compat plug (07d-A3B: a local/self-hosted OpenAI-shaped
    // endpoint). No worktree/stream dependency — build it directly.
    let oc_plug = {
        let node = state.node.lock().unwrap();
        node.openai_compat_plugs
            .iter()
            .find(|p| p.id == trimmed)
            .cloned()
    };
    if let Some(plug) = oc_plug {
        let bearer = bakery_agent::openai_compat::OpenAiCompatModel::bearer_from_secrets(&plug.id);
        let model =
            bakery_agent::OpenAiCompatModel::new(plug.id, plug.base_url, plug.model, bearer)?;
        return Ok(Arc::new(model));
    }

    bakery_agent::resolve_seat_model(model_id)
}

/// The stream id a model probe's session runs under — never a conversation's.
pub(crate) fn probe_stream_id(account_id: &str) -> String {
    format!("probe:{}", account_id.trim())
}

/// A throwaway native model for `account_id`, to ask the adapter what it
/// offers when no conversation has a session on it (see
/// `account_adapter::probe_native_catalog`). It runs in an empty scratch
/// directory with no Bakery tools, and closes its session when dropped.
/// Refuses an account that is not a native/ACP adapter.
pub(crate) fn probe_model_for_account(
    state: &AppState,
    account_id: &str,
) -> Result<crate::pipe::AcpPipeModel, String> {
    let account = account_id.trim();
    let scratch: String = account
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    let workspace = state.base.join("probe").join(scratch);
    std::fs::create_dir_all(&workspace)
        .map_err(|e| format!("cannot create the model probe's scratch directory: {e}"))?;
    let config = if account.eq_ignore_ascii_case(crate::claude_native::ACCOUNT_ID) {
        crate::claude_native::acp_spawn_config(
            state,
            &probe_stream_id(account),
            "probe",
            crate::claude_providers::Provider::Subscription,
            true,
            None,
        )?
    } else if account.eq_ignore_ascii_case(crate::pipe::ACP_MODEL_ID)
        || account.eq_ignore_ascii_case("grok-acp")
    {
        let (binary, argv_suffix, config_copy) = resolve_acp_binary_config(state)?;
        crate::pipe::AcpSpawnConfig {
            binary,
            argv_suffix,
            config_copy,
            extra_env: vec![],
            trusted: true,
            mcp: None,
            extra_process_exec_dirs: vec![],
            credential_dirs: vec![],
            read_only: Vec::new(),
        }
    } else {
        let plug = state
            .node
            .lock()
            .unwrap()
            .acp_plugs
            .iter()
            .find(|p| p.id == account)
            .cloned()
            .ok_or_else(|| format!("{account} is not a native or ACP adapter account"))?;
        let (binary, argv_suffix, config_copy) = resolve_acp_plug_config(state, &plug.id)?;
        crate::pipe::AcpSpawnConfig {
            binary,
            argv_suffix,
            config_copy,
            extra_env: acp_plug_extra_env(&plug)?,
            trusted: true,
            mcp: None,
            extra_process_exec_dirs: vec![],
            credential_dirs: vec![],
            read_only: Vec::new(),
        }
    };
    Ok(crate::pipe::AcpPipeModel::new(
        state.clone(),
        probe_stream_id(account),
        Seat::Orchestrator,
        workspace,
        config,
        account.to_string(),
    ))
}

/// The live `Model` a turn on this stream's `seat` should use. Order:
///
/// 1. An already-resolved cache entry — also how a test double reaches a
///    turn, since `ScriptedModel`/`LoopingModel`/... can't be named by a
///    model-id string the way `grok`/`claude`/`gemini` can.
/// 2. The durable Heart fact, resolved fresh via
///    [`bakery_agent::resolve_seat_model`] and cached for next time — this is
///    what makes an assignment survive a node restart even though the cache
///    itself starts empty.
/// 3. `#[cfg(test)]`'s single default model, so the tests written before
///    seats existed keep working unmodified. Production has no third step:
///    a stream with nothing assigned to `seat` fails the turn outright
///    (STEERING.md § Models: "no credentials and no local model → Setup,
///    not a toy loop" applies just as much to "nobody assigned a seat").
///
/// The whole check-read-populate sequence holds one `seats` lock acquisition
/// (not one per step): releasing it between the cache check and the Heart
/// read would let [`assign_seat_model`]'s evict land in that exact gap, and
/// this function's own `insert` right after would then resurrect the entry
/// it was supposed to clear — the same "release the lock mid-sequence"
/// shape 04d-GHOSTTY-LEASE's audit rejected for the burn fence, here between
/// a read and a cache write instead of a check and a kill.
pub(crate) fn resolve_seat(
    state: &AppState,
    stream_id: &str,
    seat: Seat,
) -> Result<Arc<dyn Model>, String> {
    let key = (stream_id.to_string(), seat);
    let mut cache = state.seats.lock().unwrap();
    if let Some(m) = cache.get(&key) {
        return Ok(m.clone());
    }
    // Bound to a `let` (not matched inline) specifically so the temporary
    // `heart` guard drops at this statement's end, not at the end of the
    // whole `match` below: `resolve_model_id_for_seat`'s pipe-plug arm
    // calls `context_for`, which locks `heart` again — matching inline
    // would keep the first guard alive through that whole arm body (Rust
    // extends a match scrutinee's temporaries across every arm) and
    // self-deadlock on a `std::sync::Mutex`, which is not reentrant.
    let fact = latest_seat_model_id(&state.heart.lock().unwrap(), stream_id, seat);
    match fact {
        Ok(Some(model_id)) => {
            let model = resolve_model_id_for_seat(state, stream_id, seat, &model_id)?;
            cache.insert(key, model.clone());
            Ok(model)
        }
        // A record exists but this build can't read it — fail closed
        // outright, never fall through to the `#[cfg(test)]` default below
        // (06-SEATS audit item 5): that seam is for "nobody assigned
        // anything", not "something's on fire, paper over it in tests too".
        Err(()) => Err(format!(
            "the {} seat's latest assignment record is unreadable — reassign it",
            seat.as_str()
        )),
        Ok(None) => {
            #[cfg(test)]
            if let Some(m) = &state.test_default_model {
                return Ok(m.clone());
            }
            Err(format!(
                "no model assigned to the {} seat for this stream — open Setup and assign one",
                seat.as_str()
            ))
        }
    }
}

/// [`resolve_seat`], plus a per-turn `model_id` override (07d-A3D1 Phase C)
/// — a specific catalog variant within whatever provider the seat is
/// already assigned to (never a cross-provider or plug swap). Wraps
/// `resolve_seat` completely unmodified: the override is built fresh via
/// `Model::with_overridden_model_id`, never inserted into `state.seats`,
/// and dropped once the turn returns — `/seat/assign`'s persisted default
/// and `AppState::seats` are untouched either way. `None`/empty is treated
/// as "no override" (the same seat resolution as always). A transport with
/// no per-turn variant concept (ACP, openai-compat, Claude Code) fails the
/// turn clearly instead of silently ignoring the override — an unhonored
/// model override is a trust problem (the wrong model running with no
/// signal), unlike an unhonored effort override.
pub(crate) fn resolve_seat_with_override(
    state: &AppState,
    stream_id: &str,
    seat: Seat,
    model_id_override: Option<&str>,
) -> Result<Arc<dyn Model>, String> {
    let base = resolve_seat(state, stream_id, seat)?;
    let Some(id) = normalize_override(model_id_override) else {
        return Ok(base);
    };
    base.with_overridden_model_id(id).ok_or_else(|| {
        format!(
            "the {} seat's assigned model ({}) has no per-turn model override",
            seat.as_str(),
            base.name()
        )
    })
}

/// Treats a blank/whitespace-only per-turn override the same as "not
/// supplied" (07d-A3D1 Phase C review fix) — shared by every call site that
/// decides whether a `model_id`/`effort` override actually applies
/// (`resolve_seat_with_override` above, `agent_turn_after_started`'s
/// `set_effort` call, and `append_turn_usage_if_any`'s usage-event
/// composition), so a caller sending `""`/`"   "` is treated identically
/// everywhere instead of only where each call site happened to remember to
/// trim.
pub(crate) fn normalize_override(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|s| !s.is_empty())
}

/// Seed a `(stream, seat)` resolution directly, bypassing the Heart entirely
/// — the only way a test can hand a turn a `ScriptedModel`/`LoopingModel`/
/// `FailingChatModel` double for a *specific* seat (as opposed to
/// `state_with`'s blanket `test_default_model`, which answers for any seat
/// with nothing more specific configured).
#[cfg(test)]
pub(crate) fn seed_seat_model_for_test(
    state: &AppState,
    stream_id: &str,
    seat: Seat,
    model: Arc<dyn Model>,
) {
    state
        .seats
        .lock()
        .unwrap()
        .insert((stream_id.to_string(), seat), model);
}

// ─── ACP lifecycle (06c-A) ──────────────────────────────────────────────────
//
// Mirrors the PTY admission/fence pattern: `admit_acp_child` → `AcpPendingGuard`
// → `.promote(pid)` → `AcpChildGuard`.  Discard calls `fence_acp_children`
// alongside `fence_active_ptys` so the stream is never torn down while a
// boxed ACP child is still running.

pub(crate) struct AcpPendingGuard {
    state: AppState,
    stream_id: String,
    resolved: bool,
}

impl AcpPendingGuard {
    pub(crate) fn promote(mut self, pid: u32) -> AcpChildGuard {
        self.resolved = true;
        let mut g = self.state.lifecycle.lock().unwrap();
        if let Some(a) = g.activity.get_mut(&self.stream_id) {
            a.acp_pending = a.acp_pending.saturating_sub(1);
            a.acp_pids.push(pid);
        }
        AcpChildGuard {
            state: self.state.clone(),
            stream_id: self.stream_id.clone(),
            pid,
        }
    }
}

impl Drop for AcpPendingGuard {
    fn drop(&mut self) {
        if self.resolved {
            return;
        }
        let mut g = self.state.lifecycle.lock().unwrap();
        if let Some(a) = g.activity.get_mut(&self.stream_id) {
            a.acp_pending = a.acp_pending.saturating_sub(1);
            if a.is_empty() {
                g.activity.remove(&self.stream_id);
            }
        }
    }
}

pub(crate) struct AcpChildGuard {
    state: AppState,
    stream_id: String,
    pid: u32,
}

impl Drop for AcpChildGuard {
    fn drop(&mut self) {
        let mut g = self.state.lifecycle.lock().unwrap();
        if let Some(a) = g.activity.get_mut(&self.stream_id) {
            a.acp_pids.retain(|p| *p != self.pid);
            if a.is_empty() {
                g.activity.remove(&self.stream_id);
            }
        }
    }
}

/// Admits a pending ACP child (handshake in flight) — same retiring/discarded
/// gate as `admit_one_shot`, but the slot is `acp_pending` rather than
/// `one_shot` because multiple seats can have simultaneous handshakes.
pub(crate) fn admit_acp_child(
    state: &AppState,
    stream_id: &str,
    trusted: bool,
) -> Result<AcpPendingGuard, String> {
    let mut g = state.lifecycle.lock().unwrap();
    if g.retiring.contains(stream_id) {
        return Err("stream is being discarded".to_string());
    }
    let heart = state.heart.lock().unwrap();
    if discarded_of(&heart, stream_id) {
        return Err("stream has been discarded".to_string());
    }
    if trusted && !host_allowed(&heart, stream_id) {
        return Err(
            "burn (or lifetime unreadable) — trusted ACP spawn refused; resolve the seat again"
                .to_string(),
        );
    }
    drop(heart);
    g.activity
        .entry(stream_id.to_string())
        .or_default()
        .acp_pending += 1;
    Ok(AcpPendingGuard {
        state: state.clone(),
        stream_id: stream_id.to_string(),
        resolved: false,
    })
}

/// Kill every boxed ACP child on `stream_id` — called by discard/reassign
/// alongside `fence_active_ptys` to ensure no ACP process is left alive when
/// the worktree/room are removed.  Removing the session slots from
/// `acp_sessions` causes the session threads to see a dropped sender and exit;
/// the explicit pid kill ensures prompt termination even if a thread is
/// blocked on ACP I/O.
#[allow(dead_code)]
pub(crate) fn fence_acp_children(state: &AppState, stream_id: &str) {
    // Collect pids under the lifecycle lock.
    let pids: Vec<u32> = {
        let g = state.lifecycle.lock().unwrap();
        g.activity
            .get(stream_id)
            .map(|a| a.acp_pids.clone())
            .unwrap_or_default()
    };
    // Kill outside the lock — kill_process_group is a syscall, not
    // instantaneous, and holding the lifecycle lock across it would block
    // every other admission/fence call for the duration.
    for pid in &pids {
        bakery_exec::kill_process_group(*pid);
    }
    // Also evict all session slots for this stream so the session threads
    // see closed prompt senders and exit cleanly.
    let mut sessions = state.acp_sessions.lock().unwrap();
    sessions.retain(|(sid, _), _| sid != stream_id);
}

/// Restart a stream's ACP runtimes (Restart runtime; a Machine access save):
/// the next turn spawns a fresh process under the current grants. The cached
/// model goes too, the same as on a seat reassignment — its spawn config
/// carries the MCP token minted for the old process, which is evicted here,
/// so respawning from it would hand the new process a dead token and every
/// Bakery op would fail with "unknown or expired MCP token".
pub(crate) fn restart_acp_runtimes(state: &AppState, stream_id: &str) {
    fence_acp_children(state, stream_id);
    revoke_cached_acp_models_for_stream(state, stream_id);
    crate::mcp::evict_mcp_tokens_for_stream(state, stream_id);
}

/// [`restart_acp_runtimes`] now if no turn is running on the stream, else the
/// moment its last running turn ends (`TurnLease`'s drop) — never under a
/// live turn. Returns whether it restarted now.
pub(crate) fn restart_acp_runtimes_when_idle(state: &AppState, stream_id: &str) -> bool {
    let busy = {
        let mut g = state.lifecycle.lock().unwrap();
        let busy = g
            .running_turns
            .iter()
            .any(|((stream, _), n)| stream == stream_id && *n > 0);
        if busy {
            g.restart_when_idle.insert(stream_id.to_string());
        }
        busy
    };
    if !busy {
        restart_acp_runtimes(state, stream_id);
    }
    !busy
}

/// Async fence: kill every ACP child on `stream_id` and wait (bounded) for
/// both `acp_pending` and `acp_pids` to clear.  Mirrors
/// `fence_active_ptys_with_deadline` in `routes/env.rs`.  Returns `true` if
/// clean, `false` on timeout (caller must abort the discard, not proceed).
pub(crate) async fn fence_acp_with_deadline(
    state: &AppState,
    stream_id: &str,
    timeout: Duration,
) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        let (pending, mut pids): (u32, Vec<u32>) = {
            let g = state.lifecycle.lock().unwrap();
            match g.activity.get(stream_id) {
                Some(a) => (a.acp_pending, a.acp_pids.clone()),
                None => (0, Vec::new()),
            }
        };
        pids.extend(close_acp_slots_for_stream(state, stream_id));
        pids.sort_unstable();
        pids.dedup();
        for pid in &pids {
            bakery_exec::kill_process_group(*pid);
        }
        let slots_clear = !state
            .acp_sessions
            .lock()
            .unwrap()
            .keys()
            .any(|(sid, _)| sid == stream_id);
        if pids.is_empty() && pending == 0 && slots_clear {
            // Every ACP session for this stream is confirmed gone — any MCP
            // token minted for it can no longer be presented by a live
            // child, so it must not keep granting its scope for the rest of
            // this process's life (`crate::mcp`'s module doc).
            crate::mcp::evict_mcp_tokens_for_stream(state, stream_id);
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[cfg(test)]
mod turn_lease_tests {
    use super::*;
    use bakery_agent::StubModel;

    #[test]
    fn admit_turn_marks_busy_and_drop_clears_it() {
        let dir = tempfile::tempdir().unwrap();
        let state = crate::testutil::state_with(dir.path(), Arc::new(StubModel));

        assert!(!is_turn_running(&state, "wt:x", Seat::Orchestrator));
        let lease = admit_turn(&state, "wt:x", Seat::Orchestrator).unwrap();
        assert!(is_turn_running(&state, "wt:x", Seat::Orchestrator));
        // A different seat on the same stream is unaffected.
        assert!(!is_turn_running(&state, "wt:x", Seat::Worker));

        drop(lease);
        assert!(!is_turn_running(&state, "wt:x", Seat::Orchestrator));
    }

    /// 07d-A1 §5: a second admit for the exact same agent while the first is
    /// still live is refused with 409, not queued or silently counted — the
    /// first lease's busy fact is unaffected by the refused second attempt,
    /// and a fresh admit succeeds once the first lease drops.
    #[test]
    fn a_second_admit_for_the_same_agent_is_refused_while_the_first_is_live() {
        let dir = tempfile::tempdir().unwrap();
        let state = crate::testutil::state_with(dir.path(), Arc::new(StubModel));

        let first = admit_turn(&state, "wt:y", Seat::Worker).unwrap();
        assert!(is_turn_running(&state, "wt:y", Seat::Worker));

        let err = match admit_turn(&state, "wt:y", Seat::Worker) {
            Err(e) => e,
            Ok(_) => panic!("a second admit for the same still-running agent must be refused"),
        };
        assert_eq!(err.0, StatusCode::CONFLICT);
        assert!(
            is_turn_running(&state, "wt:y", Seat::Worker),
            "the refused second attempt must not disturb the first lease"
        );

        drop(first);
        assert!(!is_turn_running(&state, "wt:y", Seat::Worker));

        // Once idle, a fresh admit for the same agent succeeds again.
        let third = admit_turn(&state, "wt:y", Seat::Worker).unwrap();
        assert!(is_turn_running(&state, "wt:y", Seat::Worker));
        drop(third);
        assert!(!is_turn_running(&state, "wt:y", Seat::Worker));
    }
}

#[cfg(test)]
mod agent_activity_tests {
    use super::*;
    use bakery_agent::StubModel;

    #[test]
    fn start_agent_activity_seeds_contacting_model_and_a_fresh_cancel_flag() {
        let dir = tempfile::tempdir().unwrap();
        let state = crate::testutil::state_with(dir.path(), Arc::new(StubModel));

        assert!(
            agent_activity_snapshot(&state, "wt:a:orchestrator").is_none(),
            "nothing tracked before start"
        );
        let cancel = start_agent_activity(&state, "wt:a", "wt:a:orchestrator", "turn_1");
        assert!(
            !cancel.load(std::sync::atomic::Ordering::Acquire),
            "a fresh turn's cancel flag starts unset"
        );

        let snapshot = agent_activity_snapshot(&state, "wt:a:orchestrator").unwrap();
        assert_eq!(snapshot.agent_id, "wt:a:orchestrator");
        assert_eq!(snapshot.stream_id, "wt:a");
        assert_eq!(snapshot.turn_id, "turn_1");
        assert!(matches!(snapshot.phase, ActivityPhase::ContactingModel));
        assert!(snapshot.recent_completed_tools.is_empty());
        assert!(snapshot.usage.is_none());
        assert!(!snapshot.cancel_requested);
    }

    /// A fresh `admit_turn` success is only possible when no turn for this
    /// agent is currently in flight — so a second `start_agent_activity`
    /// for the same agent (a brand-new turn) must simply overwrite whatever
    /// terminal snapshot the previous one left behind.
    #[test]
    fn start_agent_activity_overwrites_a_stale_entry_for_the_same_agent() {
        let dir = tempfile::tempdir().unwrap();
        let state = crate::testutil::state_with(dir.path(), Arc::new(StubModel));

        let _first_cancel = start_agent_activity(&state, "wt:b", "wt:b:orchestrator", "turn_1");
        set_agent_phase(
            &state,
            "wt:b:orchestrator",
            ActivityPhase::Complete { note: None },
        );
        assert!(matches!(
            agent_activity_snapshot(&state, "wt:b:orchestrator")
                .unwrap()
                .phase,
            ActivityPhase::Complete { .. }
        ));

        let second_cancel = start_agent_activity(&state, "wt:b", "wt:b:orchestrator", "turn_2");
        let snapshot = agent_activity_snapshot(&state, "wt:b:orchestrator").unwrap();
        assert_eq!(snapshot.turn_id, "turn_2");
        assert!(
            matches!(snapshot.phase, ActivityPhase::ContactingModel),
            "a fresh turn must reset the phase"
        );
        assert!(
            !second_cancel.load(std::sync::atomic::Ordering::Acquire),
            "the new turn gets its own fresh flag"
        );
    }

    #[test]
    fn set_agent_phase_is_a_silent_no_op_for_an_untracked_agent() {
        let dir = tempfile::tempdir().unwrap();
        let state = crate::testutil::state_with(dir.path(), Arc::new(StubModel));
        // No panic, no entry created — just nothing happens.
        set_agent_phase(
            &state,
            "wt:never-started:orchestrator",
            ActivityPhase::Complete { note: None },
        );
        assert!(agent_activity_snapshot(&state, "wt:never-started:orchestrator").is_none());
    }

    #[test]
    fn append_streaming_text_accumulates_and_tail_bounds() {
        let dir = tempfile::tempdir().unwrap();
        let state = crate::testutil::state_with(dir.path(), Arc::new(StubModel));
        start_agent_activity(&state, "wt:c", "wt:c:orchestrator", "turn_1");

        append_streaming_text(&state, "wt:c:orchestrator", "Hello, ");
        append_streaming_text(&state, "wt:c:orchestrator", "world!");
        match agent_activity_snapshot(&state, "wt:c:orchestrator")
            .unwrap()
            .phase
        {
            ActivityPhase::Streaming { text } => assert_eq!(text, "Hello, world!"),
            other => panic!("expected Streaming, got {other:?}"),
        }

        // Tail-bound: a chunk that pushes well past STREAM_PREVIEW_CHARS
        // must leave only the tail, never grow unbounded.
        let huge = "x".repeat(STREAM_PREVIEW_CHARS + 500);
        append_streaming_text(&state, "wt:c:orchestrator", &huge);
        match agent_activity_snapshot(&state, "wt:c:orchestrator")
            .unwrap()
            .phase
        {
            ActivityPhase::Streaming { text } => {
                assert_eq!(text.chars().count(), STREAM_PREVIEW_CHARS);
                assert!(
                    text.chars().all(|c| c == 'x'),
                    "must be the tail of the huge chunk, not the greeting"
                );
            }
            other => panic!("expected Streaming, got {other:?}"),
        }
    }

    #[test]
    fn append_streaming_text_starts_a_fresh_phase_when_the_current_one_is_not_streaming() {
        let dir = tempfile::tempdir().unwrap();
        let state = crate::testutil::state_with(dir.path(), Arc::new(StubModel));
        start_agent_activity(&state, "wt:d", "wt:d:orchestrator", "turn_1");
        // Still ContactingModel — the very first delta of a step must
        // start a brand-new Streaming phase, not error or no-op.
        append_streaming_text(&state, "wt:d:orchestrator", "first token");
        match agent_activity_snapshot(&state, "wt:d:orchestrator")
            .unwrap()
            .phase
        {
            ActivityPhase::Streaming { text } => assert_eq!(text, "first token"),
            other => panic!("expected Streaming, got {other:?}"),
        }
    }

    fn live_tokens(state: &AppState, agent: &str) -> u64 {
        agent_activity_snapshot(state, agent).unwrap().live_tokens
    }

    #[test]
    fn live_tokens_estimate_four_characters_a_token_rounded_up() {
        assert_eq!(estimate_tokens(0), 0);
        assert_eq!(estimate_tokens(1), 1);
        assert_eq!(estimate_tokens(4), 1);
        assert_eq!(estimate_tokens(5), 2);

        let dir = tempfile::tempdir().unwrap();
        let state = crate::testutil::state_with(dir.path(), Arc::new(StubModel));
        start_agent_activity(&state, "wt:t1", "wt:t1:orchestrator", "turn_1");
        assert_eq!(live_tokens(&state, "wt:t1:orchestrator"), 0);
        append_streaming_text(&state, "wt:t1:orchestrator", "twelve chars");
        assert_eq!(live_tokens(&state, "wt:t1:orchestrator"), 3);
        assert!(append_streaming_text_for_turn(&state, "wt:t1:orchestrator", "turn_1", "abcde"));
        assert_eq!(live_tokens(&state, "wt:t1:orchestrator"), 5, "17 characters");
    }

    #[test]
    fn live_tokens_count_thinking_along_with_the_reply() {
        let dir = tempfile::tempdir().unwrap();
        let state = crate::testutil::state_with(dir.path(), Arc::new(StubModel));
        start_agent_activity(&state, "wt:t2", "wt:t2:orchestrator", "turn_1");
        note_thought_for_turn(&state, "wt:t2:orchestrator", "turn_1", 40);
        assert_eq!(live_tokens(&state, "wt:t2:orchestrator"), 10);
        append_streaming_text(&state, "wt:t2:orchestrator", &"x".repeat(20));
        assert_eq!(live_tokens(&state, "wt:t2:orchestrator"), 15);
    }

    #[test]
    fn live_tokens_start_over_with_a_new_turn() {
        let dir = tempfile::tempdir().unwrap();
        let state = crate::testutil::state_with(dir.path(), Arc::new(StubModel));
        start_agent_activity(&state, "wt:t3", "wt:t3:orchestrator", "turn_1");
        note_thought_for_turn(&state, "wt:t3:orchestrator", "turn_1", 400);
        assert_eq!(live_tokens(&state, "wt:t3:orchestrator"), 100);
        start_agent_activity(&state, "wt:t3", "wt:t3:orchestrator", "turn_2");
        assert_eq!(live_tokens(&state, "wt:t3:orchestrator"), 0);
        note_thought_for_turn(&state, "wt:t3:orchestrator", "turn_2", 4);
        assert_eq!(live_tokens(&state, "wt:t3:orchestrator"), 1);
    }

    #[test]
    fn a_stale_turns_thoughts_and_text_are_not_counted() {
        let dir = tempfile::tempdir().unwrap();
        let state = crate::testutil::state_with(dir.path(), Arc::new(StubModel));
        start_agent_activity(&state, "wt:t4", "wt:t4:orchestrator", "turn_2");
        note_thought_for_turn(&state, "wt:t4:orchestrator", "turn_1", 400);
        assert!(!append_streaming_text_for_turn(&state, "wt:t4:orchestrator", "turn_1", "stale"));
        assert_eq!(live_tokens(&state, "wt:t4:orchestrator"), 0);
        set_agent_phase(&state, "wt:t4:orchestrator", ActivityPhase::Complete { note: None });
        note_thought_for_turn(&state, "wt:t4:orchestrator", "turn_2", 400);
        assert_eq!(live_tokens(&state, "wt:t4:orchestrator"), 0, "a finished turn counts nothing more");
    }

    #[test]
    fn a_thought_chunk_broadcasts_only_when_the_count_enters_a_new_multiple_of_eight() {
        let dir = tempfile::tempdir().unwrap();
        let state = crate::testutil::state_with(dir.path(), Arc::new(StubModel));
        start_agent_activity(&state, "wt:t5", "wt:t5:orchestrator", "turn_1");
        let mut rx = state.tx.subscribe();

        // 3 -> 7 tokens: still inside the first bucket of eight.
        note_thought_for_turn(&state, "wt:t5:orchestrator", "turn_1", 12);
        note_thought_for_turn(&state, "wt:t5:orchestrator", "turn_1", 16);
        assert!(rx.try_recv().is_err(), "no broadcast inside a bucket");
        // 7 -> 8 tokens: crosses into the next.
        note_thought_for_turn(&state, "wt:t5:orchestrator", "turn_1", 4);
        let sent: serde_json::Value = serde_json::from_str(&rx.try_recv().unwrap()).unwrap();
        assert_eq!(sent["activity"]["live_tokens"], 8);
        assert!(rx.try_recv().is_err());
        // The count itself was never held back.
        assert_eq!(live_tokens(&state, "wt:t5:orchestrator"), 8);
    }

    #[test]
    fn push_completed_tool_caps_at_the_recent_tools_limit() {
        let dir = tempfile::tempdir().unwrap();
        let state = crate::testutil::state_with(dir.path(), Arc::new(StubModel));
        start_agent_activity(&state, "wt:e", "wt:e:orchestrator", "turn_1");

        for i in 0..(RECENT_TOOLS_CAP + 3) {
            push_completed_tool(
                &state,
                "wt:e:orchestrator",
                CompletedTool {
                    tool: format!("tool_{i}"),
                    call_id: format!("call_{i}"),
                    ok: true,
                    finished_at: "2026-01-01T00:00:00Z".to_string(),
                },
            );
        }
        let snapshot = agent_activity_snapshot(&state, "wt:e:orchestrator").unwrap();
        assert_eq!(snapshot.recent_completed_tools.len(), RECENT_TOOLS_CAP);
        // Oldest entries are dropped first — the ring keeps the most recent.
        let names: Vec<&str> = snapshot
            .recent_completed_tools
            .iter()
            .map(|t| t.tool.as_str())
            .collect();
        assert_eq!(
            names,
            vec!["tool_3", "tool_4", "tool_5", "tool_6", "tool_7"]
        );
    }

    #[test]
    fn a_finished_native_tool_moves_the_live_strip_on_to_thinking() {
        let dir = tempfile::tempdir().unwrap();
        let state = crate::testutil::state_with(dir.path(), Arc::new(StubModel));
        let agent = "wt:n:orchestrator";
        start_agent_activity(&state, "wt:n", agent, "turn_1");
        set_agent_phase(
            &state,
            agent,
            ActivityPhase::RunningTool {
                tool: "Read b".into(),
                call_id: "b".into(),
                started_at: "t".into(),
                kind: ToolRunKind::Tool,
                preview: None,
                output: None,
            },
        );
        let snapshot = || agent_activity_snapshot(&state, agent).unwrap();

        // A parallel call the strip is not showing ends: listed, strip kept.
        end_native_tool(&state, agent, "turn_1", "a", Some("Read a".into()), true);
        assert!(matches!(snapshot().phase, ActivityPhase::RunningTool { .. }));
        // A late update from an older turn changes nothing.
        end_native_tool(&state, agent, "turn_0", "b", None, true);
        assert_eq!(snapshot().recent_completed_tools.len(), 1);
        // The call on screen ends: thinking at once, listed by its shown name.
        end_native_tool(&state, agent, "turn_1", "b", None, false);
        let done = snapshot();
        assert!(matches!(done.phase, ActivityPhase::ContactingModel));
        let listed: Vec<(&str, bool)> = done
            .recent_completed_tools
            .iter()
            .map(|t| (t.tool.as_str(), t.ok))
            .collect();
        assert_eq!(listed, vec![("Read a", true), ("Read b", false)]);
    }

    #[test]
    fn resolve_awaiting_approval_only_transitions_when_the_event_hash_matches() {
        let dir = tempfile::tempdir().unwrap();
        let state = crate::testutil::state_with(dir.path(), Arc::new(StubModel));
        start_agent_activity(&state, "wt:f", "wt:f:orchestrator", "turn_1");
        set_agent_phase(
            &state,
            "wt:f:orchestrator",
            ActivityPhase::AwaitingApproval {
                kind: ApprovalKind::FileEdit,
                event_hash: "real_hash".to_string(),
            },
        );

        // A stale/mismatched hash (e.g. a superseded turn's own proposal)
        // must never clobber this still-pending approval.
        resolve_awaiting_approval(
            &state,
            "wt:f:orchestrator",
            "some_other_hash",
            ActivityPhase::Complete { note: None },
        );
        assert!(matches!(
            agent_activity_snapshot(&state, "wt:f:orchestrator")
                .unwrap()
                .phase,
            ActivityPhase::AwaitingApproval { .. }
        ));

        resolve_awaiting_approval(
            &state,
            "wt:f:orchestrator",
            "real_hash",
            ActivityPhase::Complete { note: None },
        );
        assert!(matches!(
            agent_activity_snapshot(&state, "wt:f:orchestrator")
                .unwrap()
                .phase,
            ActivityPhase::Complete { .. }
        ));
    }

    /// Fix-verification follow-up: `resolve_awaiting_approval` is a second
    /// mutation path (alongside `set_agent_phase`) that can transition a
    /// tracked turn into a terminal phase — a cancel requested while
    /// genuinely `AwaitingApproval` (non-terminal, so `request_cancel`
    /// succeeds) must not leave a stale `cancel_requested: true` once the
    /// human's Accept/Reject resolves that same gate into `Complete`.
    #[test]
    fn resolve_awaiting_approval_also_clears_a_stale_cancel_requested_once_terminal() {
        let dir = tempfile::tempdir().unwrap();
        let state = crate::testutil::state_with(dir.path(), Arc::new(StubModel));
        start_agent_activity(&state, "wt:f2", "wt:f2:orchestrator", "turn_1");
        set_agent_phase(
            &state,
            "wt:f2:orchestrator",
            ActivityPhase::AwaitingApproval {
                kind: ApprovalKind::FileEdit,
                event_hash: "h1".to_string(),
            },
        );
        assert!(
            request_cancel(&state, "wt:f2:orchestrator"),
            "AwaitingApproval is non-terminal — cancel must succeed"
        );
        assert!(
            agent_activity_snapshot(&state, "wt:f2:orchestrator")
                .unwrap()
                .cancel_requested
        );

        resolve_awaiting_approval(
            &state,
            "wt:f2:orchestrator",
            "h1",
            ActivityPhase::Complete { note: None },
        );
        let snapshot = agent_activity_snapshot(&state, "wt:f2:orchestrator").unwrap();
        assert!(matches!(snapshot.phase, ActivityPhase::Complete { .. }));
        assert!(
            !snapshot.cancel_requested,
            "resolve_awaiting_approval must clear cancel_requested on a terminal transition too"
        );
    }

    #[test]
    fn resolve_awaiting_approval_is_a_noop_when_the_current_phase_is_not_awaiting_approval_at_all()
    {
        let dir = tempfile::tempdir().unwrap();
        let state = crate::testutil::state_with(dir.path(), Arc::new(StubModel));
        start_agent_activity(&state, "wt:g", "wt:g:orchestrator", "turn_1");
        // Phase is ContactingModel, not AwaitingApproval — a late/duplicate
        // decision must not force a Complete transition out of thin air.
        resolve_awaiting_approval(
            &state,
            "wt:g:orchestrator",
            "any_hash",
            ActivityPhase::Complete { note: None },
        );
        assert!(matches!(
            agent_activity_snapshot(&state, "wt:g:orchestrator")
                .unwrap()
                .phase,
            ActivityPhase::ContactingModel
        ));
    }

    #[test]
    fn request_cancel_refuses_for_an_unknown_agent_and_flips_the_flag_for_a_known_one() {
        let dir = tempfile::tempdir().unwrap();
        let state = crate::testutil::state_with(dir.path(), Arc::new(StubModel));

        assert!(
            !request_cancel(&state, "wt:h:orchestrator"),
            "no turn tracked — must refuse, not silently succeed"
        );

        let cancel = start_agent_activity(&state, "wt:h", "wt:h:orchestrator", "turn_1");
        assert!(!cancel.load(std::sync::atomic::Ordering::Acquire));
        assert!(request_cancel(&state, "wt:h:orchestrator"));
        assert!(
            cancel.load(std::sync::atomic::Ordering::Acquire),
            "the same Arc the turn holds must observe the flip"
        );
        assert!(
            agent_activity_snapshot(&state, "wt:h:orchestrator").unwrap().cancel_requested,
            "the snapshot must reflect cancel_requested immediately, before the turn has actually stopped"
        );
    }

    /// 07d-A5 review #8: a client that hits Cancel against a turn that has
    /// already reached a terminal phase (it raced the turn finishing on its
    /// own, or the client is simply late) must get a clear refusal, never a
    /// false-positive "cancelled": true.
    #[test]
    fn request_cancel_refuses_once_the_tracked_turn_is_already_terminal() {
        let dir = tempfile::tempdir().unwrap();
        let state = crate::testutil::state_with(dir.path(), Arc::new(StubModel));
        start_agent_activity(&state, "wt:i", "wt:i:orchestrator", "turn_1");
        set_agent_phase(
            &state,
            "wt:i:orchestrator",
            ActivityPhase::Complete { note: None },
        );

        assert!(
            !request_cancel(&state, "wt:i:orchestrator"),
            "the turn already finished — cancel must refuse, not silently succeed"
        );
        assert!(
            !agent_activity_snapshot(&state, "wt:i:orchestrator")
                .unwrap()
                .cancel_requested,
            "a refused cancel must never stamp cancel_requested onto the terminal snapshot"
        );
    }

    /// 07d-A5 review #6: a cancel that lands right as a turn completes on
    /// its own must not leave a stale `cancel_requested: true` on the
    /// resulting terminal snapshot — reaching a terminal phase always
    /// clears it, whether the cancel won the race (Cancelled) or lost it
    /// (Complete/Error).
    #[test]
    fn set_agent_phase_clears_a_stale_cancel_requested_once_the_phase_is_terminal() {
        let dir = tempfile::tempdir().unwrap();
        let state = crate::testutil::state_with(dir.path(), Arc::new(StubModel));
        start_agent_activity(&state, "wt:j", "wt:j:orchestrator", "turn_1");
        assert!(request_cancel(&state, "wt:j:orchestrator"));
        assert!(
            agent_activity_snapshot(&state, "wt:j:orchestrator")
                .unwrap()
                .cancel_requested
        );

        // The cancel lost the race — the turn finished on its own anyway.
        set_agent_phase(
            &state,
            "wt:j:orchestrator",
            ActivityPhase::Complete { note: None },
        );
        let snapshot = agent_activity_snapshot(&state, "wt:j:orchestrator").unwrap();
        assert!(matches!(snapshot.phase, ActivityPhase::Complete { .. }));
        assert!(
            !snapshot.cancel_requested,
            "Complete must never carry a stale cancel_requested:true"
        );
    }
}

#[cfg(test)]
mod acp_reassignment_tests {
    use super::*;
    use bakery_agent::StubModel;

    #[test]
    fn close_timeout_leaves_the_exact_generation_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        let state = crate::testutil::state_with(dir.path(), Arc::new(StubModel));
        let key = ("wt:closing-timeout".to_string(), Seat::Orchestrator);
        let generation = 42;
        state
            .acp_sessions
            .lock()
            .unwrap()
            .insert(key.clone(), crate::pipe::AcpSlot::Closing { generation });

        assert!(!wait_for_acp_generation_close(
            &state,
            &key,
            generation,
            None,
            Duration::from_millis(1),
        ));
        assert_eq!(
            state
                .acp_sessions
                .lock()
                .unwrap()
                .get(&key)
                .map(crate::pipe::AcpSlot::generation),
            Some(generation),
            "a timed-out close must remain blocked, never look idle"
        );
    }
}

#[cfg(test)]
mod mcp_spawn_config_tests {
    use super::*;
    use bakery_agent::StubModel;

    /// The defect this guards: `mcp_spawn_config_for` is the one real
    /// decision function between "an ACP session is being resolved" and
    /// "this session gets `mcp_servers`", but every existing ACP test
    /// constructed `AcpSpawnConfig`/`McpSpawnConfig` by hand rather than
    /// going through it — so a future reorder in `mcp_scope_for_stream`, or
    /// `current_exe()` behaving unexpectedly, could silently boot a real
    /// Baker ACP session with zero Bakery tools and the whole suite would
    /// still be green. Drive the actual function for the permanent Baker
    /// stream, assert it returns `Some`, and assert the token it minted is
    /// really live in `state.mcp_tokens`, bound to the expected scope — not
    /// just that SOME config came back.
    #[test]
    fn mcp_spawn_config_for_returns_some_for_the_permanent_baker_with_a_live_bound_token() {
        let dir = tempfile::tempdir().unwrap();
        let state = crate::testutil::state_with(dir.path(), Arc::new(StubModel));
        let baker_home = crate::routes::baker::ensure_permanent_baker(&state, None, None).unwrap();

        let config = mcp_spawn_config_for(&state, &baker_home.stream_id, Seat::Baker)
            .expect("resolving the current exe path must not fail in a test")
            .expect("the permanent Baker stream must get an MCP spawn config");

        let token = config
            .env
            .iter()
            .find(|(k, _)| k == "BAKERY_MCP_TOKEN")
            .map(|(_, v)| v.clone())
            .expect("McpSpawnConfig must carry the minted token as BAKERY_MCP_TOKEN");
        let binding = state
            .mcp_tokens
            .lock()
            .unwrap()
            .get(&token)
            .cloned()
            .expect(
                "the token mcp_spawn_config_for minted must actually be live in state.mcp_tokens",
            );
        assert_eq!(binding.stream_id, baker_home.stream_id);
        assert_eq!(binding.seat, Seat::Baker);
        assert_eq!(binding.scope, crate::mcp::McpScope::BakerOps);
    }

    /// Same proof, the task-socket side: a materialized task's own stream
    /// gets `WorkerTools`, never `BakerOps`.
    #[test]
    fn mcp_spawn_config_for_returns_some_for_a_materialized_task_socket_scoped_to_worker_tools() {
        let dir = tempfile::tempdir().unwrap();
        let state = crate::testutil::state_with(dir.path(), Arc::new(StubModel));
        let project_id = crate::catalog::create_project(&state, "P", None)
            .unwrap()
            .0
            .project_id;
        let created = crate::tasks::task_create(
            &state,
            crate::tasks::TaskCreateReq {
                parent_project_id: project_id.clone(),
                title: "T".into(),
                work_order: "wo".into(),
                contract: Default::default(),
            },
        )
        .unwrap();
        crate::tasks::task_start(
            &state,
            crate::tasks::TaskStartReq {
                project_id,
                task_id: created.task.task_id.clone(),
            },
        )
        .unwrap();

        let config = mcp_spawn_config_for(&state, &created.task.stream_id, Seat::Orchestrator)
            .unwrap()
            .expect("a materialized task socket must get an MCP spawn config");
        let token = config
            .env
            .iter()
            .find(|(k, _)| k == "BAKERY_MCP_TOKEN")
            .map(|(_, v)| v.clone())
            .unwrap();
        let binding = state
            .mcp_tokens
            .lock()
            .unwrap()
            .get(&token)
            .cloned()
            .unwrap();
        assert_eq!(binding.stream_id, created.task.stream_id);
        assert_eq!(binding.scope, crate::mcp::McpScope::WorkerTools);
    }

    /// An ordinary project socket (neither the permanent Baker nor a task)
    /// gets the agents' Bakery tools; one given none of them gets no MCP
    /// spawn config and mints no token at all.
    #[test]
    fn mcp_spawn_config_for_an_ordinary_project_socket_is_its_agent_tools() {
        let dir = tempfile::tempdir().unwrap();
        let state = crate::testutil::state_with(dir.path(), Arc::new(StubModel));
        let socket_id = crate::catalog::create_project(&state, "P", None)
            .unwrap()
            .1
            .stream_id;
        let config = mcp_spawn_config_for(&state, &socket_id, Seat::Orchestrator).unwrap().unwrap();
        let token = &config.env.iter().find(|(k, _)| k == "BAKERY_MCP_TOKEN").unwrap().1;
        let binding = state.mcp_tokens.lock().unwrap().get(token).cloned().unwrap();
        assert_eq!(binding.scope, crate::mcp::McpScope::Agent);

        crate::api_tools::set_tools_limit(&state, &socket_id, crate::api_tools::ToolsLimit::None).unwrap();
        let before = state.mcp_tokens.lock().unwrap().len();
        let config = mcp_spawn_config_for(&state, &socket_id, Seat::Orchestrator).unwrap();
        assert!(
            config.is_none(),
            "a socket given none of the Bakery's tools gets no MCP spawn config"
        );
        assert_eq!(
            state.mcp_tokens.lock().unwrap().len(),
            before,
            "no token may be minted for it either"
        );
    }

    /// The operator credential must never travel to a spawned child in any
    /// form: not in the MCP spawn env (only the per-session MCP token and
    /// the base URL belong there), and structurally not via the ACP child
    /// env allowlist (which admits only named directory variables). This is
    /// the "don't give children the operator credential" invariant as a
    /// test, not a comment.
    #[test]
    fn operator_token_never_appears_in_any_child_spawn_env() {
        let dir = tempfile::tempdir().unwrap();
        let state = crate::testutil::state_with(dir.path(), Arc::new(StubModel));
        let operator_token = state.operator_token.to_string();
        assert!(!operator_token.is_empty());

        let baker_home = crate::routes::baker::ensure_permanent_baker(&state, None, None).unwrap();
        let config = mcp_spawn_config_for(&state, &baker_home.stream_id, Seat::Baker)
            .unwrap()
            .expect("baker gets an MCP spawn config");
        for (k, v) in &config.env {
            assert_ne!(
                v, &operator_token,
                "operator token leaked into child env var {k}"
            );
            assert!(
                k == "BAKERY_MCP_TOKEN" || k == "BAKERY_MCP_BASE_URL",
                "unexpected child env var {k} — the MCP spawn env is a closed two-variable set"
            );
        }

        // The ACP plug env allowlist is a closed set of directory variables;
        // no token-shaped name is admissible through it.
        for allowed in crate::state::ACP_CHILD_ENV_ALLOWLIST {
            assert!(
                !allowed.to_ascii_lowercase().contains("token")
                    && !allowed.to_ascii_lowercase().contains("key"),
                "ACP child env allowlist must never admit a credential-shaped variable: {allowed}"
            );
        }
    }

    /// The defect this guards (review finding, "MCP tokens ... never
    /// revoked"): reassigning a stream's seat model used to leave every
    /// previously minted MCP token for that stream live for the rest of the
    /// process, no matter how many times the seat got reassigned.
    /// `assign_seat_model` must evict them.
    /// Restart runtime and a Machine access save kill a stream's ACP process
    /// and evict its MCP token. The cached model has to go with it: its spawn
    /// config still carries that token, so a respawn from it would hand the
    /// new process a dead token and every Bakery op would fail.
    #[test]
    fn restarting_acp_runtimes_drops_the_cached_model_along_with_its_token() {
        struct AcpStub;
        impl Model for AcpStub {
            fn transport(&self) -> bakery_agent::Transport {
                bakery_agent::Transport::Acp
            }
            fn propose_edit(
                &self,
                _req: &bakery_agent::EditRequest,
            ) -> Result<bakery_agent::ProposedEdit, String> {
                Err("unused".into())
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let state = crate::testutil::state_with(dir.path(), Arc::new(StubModel));
        let baker_home = crate::routes::baker::ensure_permanent_baker(&state, None, None).unwrap();
        let sid = baker_home.stream_id.clone();
        let cached: Arc<dyn Model> = Arc::new(AcpStub);
        state.seats.lock().unwrap().insert((sid.clone(), Seat::Baker), cached);
        let token =
            crate::mcp::mint_mcp_token(&state, &sid, Seat::Baker, crate::mcp::McpScope::BakerOps);

        restart_acp_runtimes(&state, &sid);

        assert!(
            !state.seats.lock().unwrap().contains_key(&(sid.clone(), Seat::Baker)),
            "the next turn must build a fresh model, which mints a live token"
        );
        assert!(!state.mcp_tokens.lock().unwrap().contains_key(&token));
    }

    #[test]
    fn assign_seat_model_evicts_the_streams_mcp_tokens() {
        let dir = tempfile::tempdir().unwrap();
        let state = crate::testutil::state_with(dir.path(), Arc::new(StubModel));
        let baker_home = crate::routes::baker::ensure_permanent_baker(&state, None, None).unwrap();
        let stale_token = crate::mcp::mint_mcp_token(
            &state,
            &baker_home.stream_id,
            Seat::Baker,
            crate::mcp::McpScope::BakerOps,
        );
        assert!(state.mcp_tokens.lock().unwrap().contains_key(&stale_token));

        assign_seat_model(
            &state,
            &baker_home.stream_id,
            Seat::Baker,
            "acp:some-other-plug",
        )
        .unwrap();

        assert!(
            !state.mcp_tokens.lock().unwrap().contains_key(&stale_token),
            "reassigning the seat's model must evict the stream's prior MCP token"
        );
    }

    /// Same defect, the discard-side eviction point: once
    /// `fence_acp_with_deadline` confirms every ACP session for a stream is
    /// gone (the ordinary case here — nothing was ever registered for this
    /// stream, so the fence is satisfied on its very first check), it must
    /// not leave that stream's MCP token behind either.
    #[tokio::test]
    async fn fence_acp_with_deadline_evicts_the_streams_mcp_tokens_once_confirmed_clear() {
        let dir = tempfile::tempdir().unwrap();
        let state = crate::testutil::state_with(dir.path(), Arc::new(StubModel));
        let baker_home = crate::routes::baker::ensure_permanent_baker(&state, None, None).unwrap();
        let stale_token = crate::mcp::mint_mcp_token(
            &state,
            &baker_home.stream_id,
            Seat::Baker,
            crate::mcp::McpScope::BakerOps,
        );

        let cleared =
            fence_acp_with_deadline(&state, &baker_home.stream_id, Duration::from_millis(200))
                .await;
        assert!(
            cleared,
            "nothing was ever registered for this stream — the fence must clear immediately"
        );
        assert!(
            !state.mcp_tokens.lock().unwrap().contains_key(&stale_token),
            "a confirmed-clear ACP fence must evict the stream's MCP token"
        );
    }
}

// ─── 07d-A3B: named openai-compat plugs ─────────────────────────────────────

#[cfg(test)]
mod openai_compat_resolve_tests {
    use super::*;
    use bakery_agent::StubModel;

    /// One-request fake listener built on `tiny_http` — same fixture shape
    /// `bakery_agent::openai_compat`'s own tests use (07d-A3B: "Fake HTTP
    /// server in tests, not live Hugging Face"). A hand-rolled raw-socket
    /// version of this fixture was measurably flaky under heavy parallel
    /// `cargo test` load; `tiny_http`'s real HTTP/1.1 framing is not.
    fn spawn_fake_openai_server(body: &'static str) -> String {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let addr = server.server_addr().to_ip().unwrap();
        std::thread::spawn(move || {
            if let Ok(request) = server.recv() {
                let response = tiny_http::Response::from_string(body).with_header(
                    tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..])
                        .unwrap(),
                );
                let _ = request.respond(response);
            }
        });
        format!("http://{addr}")
    }

    /// `resolve_model_id_for_seat`'s openai-compat branch reads the bearer
    /// slot via `bakery_agent::secrets::plug_secret`, which hits the real
    /// `$HOME/.bakery/secrets.json` unless isolated (06b-PIPE.md: "Do not:
    /// Live secrets.json in tests") — both tests below must run inside
    /// `with_isolated_credentials`, not just seed `node.openai_compat_plugs`.
    #[test]
    fn resolve_model_id_for_seat_boots_a_named_openai_compat_plug_against_a_fake_listener() {
        let dir = tempfile::tempdir().unwrap();
        let secrets_path = dir.path().join("secrets.json");
        bakery_agent::secrets::test_support::with_isolated_credentials(&secrets_path, || {
            let state = crate::testutil::state_with(dir.path(), Arc::new(StubModel));
            let body = r#"{"choices":[{"message":{"role":"assistant","content":"hello from the fake local model"}}]}"#;
            let base_url = spawn_fake_openai_server(body);
            state.node.lock().unwrap().openai_compat_plugs =
                vec![crate::node::OpenAiCompatPlugConfig {
                    id: "local:ollama".into(),
                    label: Some("Local Ollama".into()),
                    base_url,
                    model: "test-model".into(),
                }];

            let model = resolve_model_id_for_seat(
                &state,
                "wt:openai-compat-test",
                Seat::Orchestrator,
                "local:ollama",
            )
            .unwrap();
            assert_eq!(model.name(), "local:ollama");
            let reply = model
                .chat("system", &[bakery_agent::ChatMsg::user("hi")])
                .unwrap();
            assert_eq!(reply, "hello from the fake local model");
        });
    }

    #[test]
    fn resolve_model_id_for_seat_fails_setup_on_a_bad_openai_compat_base_url_with_no_network_call()
    {
        let dir = tempfile::tempdir().unwrap();
        let secrets_path = dir.path().join("secrets.json");
        bakery_agent::secrets::test_support::with_isolated_credentials(&secrets_path, || {
            let state = crate::testutil::state_with(dir.path(), Arc::new(StubModel));
            state.node.lock().unwrap().openai_compat_plugs =
                vec![crate::node::OpenAiCompatPlugConfig {
                    id: "local:broken".into(),
                    label: None,
                    base_url: "not-a-url".into(),
                    model: "test-model".into(),
                }];

            let err = resolve_model_id_for_seat(
                &state,
                "wt:openai-compat-badurl",
                Seat::Orchestrator,
                "local:broken",
            )
            .map(|_| ())
            .unwrap_err();
            assert!(err.to_ascii_lowercase().contains("setup needed"), "{err}");
        });
    }

    #[test]
    fn openai_compat_plug_ready_matches_what_resolve_would_do() {
        let plug = crate::node::OpenAiCompatPlugConfig {
            id: "local:x".into(),
            label: None,
            base_url: "not-a-url".into(),
            model: "m".into(),
        };
        assert!(openai_compat_plug_ready(&plug).is_err());

        let plug_ok = crate::node::OpenAiCompatPlugConfig {
            id: "local:y".into(),
            label: None,
            base_url: "http://127.0.0.1:1".into(),
            model: "m".into(),
        };
        assert!(
            openai_compat_plug_ready(&plug_ok).is_ok(),
            "syntax-only check must not attempt a network call"
        );
    }
}

#[cfg(test)]
mod origin_tests {
    use super::*;

    fn header_map_with_origin(origin: &str) -> axum::http::HeaderMap {
        let mut h = axum::http::HeaderMap::new();
        h.insert(axum::http::header::ORIGIN, origin.parse().unwrap());
        h
    }

    #[test]
    fn origin_allowed_accepts_no_origin_header() {
        // Non-browser tooling (curl, a test's own WS client) — a ticket
        // check right after this is what actually gates it.
        assert!(origin_allowed(&axum::http::HeaderMap::new()));
    }

    #[test]
    fn origin_allowed_accepts_every_allowlisted_origin() {
        for o in ALLOWED_ORIGINS {
            assert!(origin_allowed(&header_map_with_origin(o)), "{o}");
        }
    }

    #[test]
    fn origin_allowed_rejects_a_disallowed_origin() {
        assert!(!origin_allowed(&header_map_with_origin(
            "https://evil.example"
        )));
    }

    // ─── BAKERY_ALLOWED_ORIGIN (dynamic-port e2e harness) ──────────────────

    static ORIGIN_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Serializes on a lock private to this module — nothing else in the
    /// crate touches `BAKERY_ALLOWED_ORIGIN`, so this doesn't need the
    /// cross-crate `cli_tools::test_support` machinery.
    fn with_allowed_origin_env<T>(value: Option<&str>, f: impl FnOnce() -> T) -> T {
        let _guard = ORIGIN_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var("BAKERY_ALLOWED_ORIGIN").ok();
        match value {
            Some(v) => std::env::set_var("BAKERY_ALLOWED_ORIGIN", v),
            None => std::env::remove_var("BAKERY_ALLOWED_ORIGIN"),
        }
        let out = f();
        match prev {
            Some(v) => std::env::set_var("BAKERY_ALLOWED_ORIGIN", v),
            None => std::env::remove_var("BAKERY_ALLOWED_ORIGIN"),
        }
        out
    }

    #[test]
    fn extra_allowed_origin_accepts_a_loopback_http_origin_with_a_dynamic_port() {
        with_allowed_origin_env(Some("http://127.0.0.1:54321"), || {
            assert!(origin_allowed(&header_map_with_origin(
                "http://127.0.0.1:54321"
            )));
        });
    }

    #[test]
    fn extra_allowed_origin_accepts_localhost_host_too() {
        with_allowed_origin_env(Some("http://localhost:54321"), || {
            assert!(origin_allowed(&header_map_with_origin(
                "http://localhost:54321"
            )));
        });
    }

    #[test]
    fn extra_allowed_origin_never_lets_an_unrelated_origin_through() {
        with_allowed_origin_env(Some("http://127.0.0.1:54321"), || {
            assert!(!origin_allowed(&header_map_with_origin(
                "https://evil.example"
            )));
            assert!(!origin_allowed(&header_map_with_origin(
                "http://127.0.0.1:1"
            )));
        });
    }

    #[test]
    fn extra_allowed_origin_rejects_https() {
        with_allowed_origin_env(Some("https://127.0.0.1:54321"), || {
            assert!(!origin_allowed(&header_map_with_origin(
                "https://127.0.0.1:54321"
            )));
        });
    }

    #[test]
    fn extra_allowed_origin_rejects_a_non_loopback_host() {
        with_allowed_origin_env(Some("http://example.com:54321"), || {
            assert!(!origin_allowed(&header_map_with_origin(
                "http://example.com:54321"
            )));
        });
    }

    #[test]
    fn extra_allowed_origin_rejects_a_wildcard() {
        with_allowed_origin_env(Some("*"), || {
            assert!(!origin_allowed(&header_map_with_origin(
                "http://127.0.0.1:54321"
            )));
        });
    }

    #[test]
    fn extra_allowed_origin_rejects_a_missing_port() {
        with_allowed_origin_env(Some("http://127.0.0.1"), || {
            assert!(!origin_allowed(&header_map_with_origin("http://127.0.0.1")));
        });
    }

    #[test]
    fn extra_allowed_origin_rejects_port_zero() {
        // "0" parses fine as a u16 but names no real listener — bind()'s
        // "OS picks one" sentinel, never a port anything is reachable on.
        with_allowed_origin_env(Some("http://127.0.0.1:0"), || {
            assert!(!origin_allowed(&header_map_with_origin(
                "http://127.0.0.1:0"
            )));
        });
    }

    #[test]
    fn no_extra_allowed_origin_env_leaves_the_fixed_list_unchanged() {
        with_allowed_origin_env(None, || {
            assert_eq!(allowed_origins().len(), ALLOWED_ORIGINS.len());
            assert!(!origin_allowed(&header_map_with_origin(
                "http://127.0.0.1:54321"
            )));
        });
    }
}

#[cfg(test)]
mod socket_kind_tests {
    use super::*;
    use bakery_agent::StubModel;
    use std::sync::Arc;

    #[test]
    fn no_record_at_all_defaults_to_project() {
        let dir = tempfile::tempdir().unwrap();
        let state = crate::testutil::state_with(dir.path(), Arc::new(StubModel));
        assert_eq!(
            socket_kind(&state.heart.lock().unwrap(), "wt:nokind"),
            SocketKind::Project
        );
    }

    #[test]
    fn a_valid_newest_record_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let state = crate::testutil::state_with(dir.path(), Arc::new(StubModel));
        append(
            &state,
            "wt:kindok",
            "human",
            SOCKET_KIND_FAMILY,
            serde_json::to_string(&SocketKindRecord {
                kind: SocketKind::Factory,
            })
            .unwrap(),
            vec![],
        )
        .unwrap();
        assert_eq!(
            socket_kind(&state.heart.lock().unwrap(), "wt:kindok"),
            SocketKind::Factory
        );
    }

    /// 07d-A1 correction #2: a malformed NEWEST record fails closed to
    /// `Project` — it must never skip backward to an older, valid `Factory`
    /// record the way `latest_seat_model_id`'s same-seat scan does. There is
    /// exactly one legitimate writer for this family, so "the newest record
    /// is unreadable" only means real corruption, never a normal race worth
    /// searching through.
    #[test]
    fn a_malformed_newest_record_fails_closed_and_does_not_fall_back_to_an_older_good_one() {
        let dir = tempfile::tempdir().unwrap();
        let state = crate::testutil::state_with(dir.path(), Arc::new(StubModel));
        append(
            &state,
            "wt:kindbad",
            "human",
            SOCKET_KIND_FAMILY,
            serde_json::to_string(&SocketKindRecord {
                kind: SocketKind::Factory,
            })
            .unwrap(),
            vec![],
        )
        .unwrap();
        // A newer, unreadable record for the same stream.
        append(
            &state,
            "wt:kindbad",
            "human",
            SOCKET_KIND_FAMILY,
            "not valid json at all".to_string(),
            vec![],
        )
        .unwrap();

        assert_eq!(
            socket_kind(&state.heart.lock().unwrap(), "wt:kindbad"),
            SocketKind::Project,
            "a corrupted newest record must fail closed, not resurrect the older Factory record"
        );
    }
}
