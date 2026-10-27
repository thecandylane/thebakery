//! The server-owned task driver (07d-B Phase 4 item 1) and proof execution
//! (item 2).
//!
//! **Events wake, facts decide.** Auto-accept can append a proposal's
//! decision *inside* the turn that produced it, before the turn's own
//! result is observable (`routes::agent::maybe_auto_accept`, called from
//! inside `agent_turn_inner`'s tool loop) — a driver that snapshots "a
//! proposal is pending" and then waits for a *later* decision event can wait
//! forever, since the decision may already be on the stream by the time it
//! starts waiting. Every step below re-derives status from Heart facts
//! (`drive_one_step`), never from a remembered pre-turn snapshot — the same
//! re-derivation is what makes restart recovery ([`recover_on_boot`]) work
//! with no special-cased "was a turn running" bookkeeping.
//!
//! **One driver call, one decision.** [`drive_one_step`] performs exactly
//! one decision (wait / consume-inbox / issue-a-turn / run-proof / stop) and
//! returns what happened — this is deliberately synchronous and directly
//! callable so tests can single-step a task's whole lifecycle without racing
//! a background thread. [`start`] is what actually spawns the always-on
//! background loop ([`drive_loop`]) that a live node runs.
//!
//! **Capacity + one-driver-per-task** share one admission map
//! ([`DriverRegistry`], the `state.rs:963-982` `admit_turn` pattern
//! generalized from "one turn per agent" to "one driver-thread per task,
//! capped at [`DRIVER_CAPACITY`] concurrently"). A task denied a slot for
//! capacity (not because it is already driven) is queued in `pending` and
//! promoted the moment another task's driver thread exits (`DriverLease`'s
//! `Drop`) — queued/blocked/awaiting_review tasks hold no slot at all,
//! because nothing ever calls [`start`] for them while they are in those
//! states.

use crate::node::Seat;
use crate::routes::agent::{agent_turn_inner, AgentTurnReq};
use crate::state::{append, AppState};
use crate::tasks::{
    TaskContract, TaskMessage, TaskMessageDelivered, TaskRecord, TaskStatus, TaskStatusChanged,
    TASK_MESSAGE_DELIVERED_FAMILY, TASK_MESSAGE_FAMILY, TASK_STATUS_FAMILY,
};
use axum::http::StatusCode;
use bakery_core::Event;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Actor recorded on every fact the driver itself appends (status
/// transitions it decides, proof-run records) — distinct from `"human"`
/// (a person clicked something), `"agent:baker"`/`"agent:worker"` (a model
/// called a declared tool), and `"node"` (this node's own lifecycle/ownership
/// bookkeeping, `state::claim_ownership`'s precedent) only in that this is a
/// *coordinator* decision, not a model's — reusing `"node"` would blur "the
/// daemon observed a fact and recorded provenance" with "the daemon's own
/// scheduler decided the task's fate." Kept as its own actor string so an
/// event's provenance always answers "who/what really decided this."
const DRIVER_ACTOR: &str = "task-driver";

/// The effective turn budget for a task: the contract's own explicit
/// `turn_budget` wins, else the operator's node-wide
/// `NodeConfig.task_turn_budget`, else **no ceiling** — a hidden fixed
/// default (this used to be a silent 40) is exactly the kind of quota the
/// operator asked to remove: work continues until completion, review,
/// cancellation, a real error, or the stall guard — never an arbitrary
/// number nobody configured. Cancellation, bounded exec I/O, and operation
/// timeouts all remain.
fn resolved_turn_budget(state: &AppState, task: &TaskRecord) -> Option<u32> {
    task.contract
        .turn_budget
        .or_else(|| state.node.lock().unwrap().task_turn_budget.map(|n| n.get()))
}

/// Bound on concurrently *driven* tasks (07d-B Phase 4 item 1) — queued,
/// blocked, and awaiting-review tasks hold no slot (nothing calls [`start`]
/// for them), so this only ever gates how many task drivers run turns at
/// once.
pub(crate) const DRIVER_CAPACITY: usize = 4;

/// Poll interval while [`drive_one_step`] reports [`StepOutcome::Waiting`]
/// (an undecided gate). A real broadcast-subscription wake
/// (`state.tx`, per the spec's "wake sources") would react faster; this
/// bounded periodic tick is the simpler, still-correct fallback actually
/// implemented here — see this module's top doc comment and this pass's
/// `remaining_work` note on the trade-off.
const WAIT_TICK: Duration = Duration::from_millis(120);

/// Cap on a single proof/reviewed file body kept in a wire response —
/// generous for real source files, but a multi-megabyte generated artifact
/// must not blow up `GET /task/review`'s payload.
pub(crate) const MAX_REVIEW_FILE_BYTES: usize = 256_000;

#[derive(Default)]
struct DriverRegistryInner {
    /// `task_id`s currently holding a driven slot.
    active: HashSet<String>,
    /// `(project_id, task_id)` waiting for a slot, oldest first.
    pending: Vec<(String, String)>,
}

/// The admission map — one per `AppState` (see `AppState::task_drivers`).
pub(crate) struct DriverRegistry {
    inner: Mutex<DriverRegistryInner>,
}

impl Default for DriverRegistry {
    fn default() -> Self {
        DriverRegistry {
            inner: Mutex::new(DriverRegistryInner::default()),
        }
    }
}

/// Held for the lifetime of one task's driver thread; dropping it frees the
/// slot and promotes the oldest pending task, if any and if capacity allows
/// (it always does — a slot was just freed).
struct DriverLease {
    state: AppState,
    task_id: String,
}

impl Drop for DriverLease {
    fn drop(&mut self) {
        let registry = state_registry(&self.state);
        // Looped, not "pop one and stop": a task can sit in `pending` and go
        // stale there (cancelled, or blocked by some other path) before its
        // turn to be promoted ever comes. Popping exactly one and giving up
        // when it turns out stale would starve every task behind it
        // indefinitely despite a genuinely free slot — this loop keeps
        // trying the next pending entry until one is actually spawned or
        // the queue is empty.
        loop {
            let candidate = {
                let mut g = registry.inner.lock().unwrap();
                g.active.remove(&self.task_id);
                if g.active.len() >= DRIVER_CAPACITY || g.pending.is_empty() {
                    None
                } else {
                    let next = g.pending.remove(0);
                    // Provisionally admitted while still holding the lock —
                    // the same slot can't be double-promoted by a second
                    // concurrent `Drop` racing this one.
                    g.active.insert(next.1.clone());
                    Some(next)
                }
            };
            let Some((project_id, task_id)) = candidate else {
                break;
            };
            match crate::tasks::find_task(&self.state, &project_id, &task_id) {
                Ok(t) if !t.retired && t.status == TaskStatus::Queued => {
                    // Durable, honest fact (07d-B Phase 4 item 3): the row
                    // was recorded `queued` (`try_admit`'s capacity-deny
                    // path) while it sat here with no driver behind it —
                    // flip it back to `running` BEFORE spawning, so Heart
                    // facts never say `running` with nothing driving it.
                    set_status(
                        &self.state,
                        &project_id,
                        &task_id,
                        TaskStatus::Running,
                        Some("driver capacity freed".to_string()),
                    );
                    let lease = DriverLease {
                        state: self.state.clone(),
                        task_id: task_id.clone(),
                    };
                    spawn_driver_thread(self.state.clone(), project_id, task_id, lease);
                    break;
                }
                _ => {
                    // Stale: no longer legitimately queued (cancelled,
                    // retired, or otherwise moved on while it waited).
                    // Release the slot provisionally taken for it above and
                    // try the next pending entry instead of leaking the slot
                    // or spawning a driver for a dead row.
                    registry.inner.lock().unwrap().active.remove(&task_id);
                }
            }
        }
    }
}

fn state_registry(state: &AppState) -> Arc<DriverRegistry> {
    state.task_drivers.clone()
}

/// Admit `task_id` for driving now, or queue it — never both, never neither.
/// `Ok(true)` means a lease was taken (caller must spawn the loop); `Ok(false)`
/// means it was queued (or was already active, a harmless no-op); the
/// `bool` distinguishes them so [`start`] knows whether to spawn.
fn try_admit(state: &AppState, project_id: &str, task_id: &str) -> Option<DriverLease> {
    let registry = state_registry(state);
    let mut g = registry.inner.lock().unwrap();
    if g.active.contains(task_id) {
        return None; // already driven — not an error, just nothing to do
    }
    if g.active.len() >= DRIVER_CAPACITY {
        let already_queued = g.pending.iter().any(|(_, t)| t == task_id);
        if !already_queued {
            g.pending
                .push((project_id.to_string(), task_id.to_string()));
        }
        drop(g);
        if !already_queued {
            // Durable, honest fact (07d-B Phase 4 item 3: "queued... waiting
            // on driver capacity" is distinct from `running`). Without this
            // the task stays recorded `running` in every projection
            // (`/task/list`, the sidebar, the Floor) while nothing actually
            // drives it, AND the queue itself is otherwise pure in-memory
            // state a restart would silently lose while the task still
            // claims to be running.
            set_status(
                state,
                project_id,
                task_id,
                TaskStatus::Queued,
                Some("waiting on driver capacity".to_string()),
            );
        }
        return None;
    }
    g.active.insert(task_id.to_string());
    Some(DriverLease {
        state: state.clone(),
        task_id: task_id.to_string(),
    })
}

/// Start (or queue) this task's driver. Idempotent: a task already being
/// driven is left alone; a task not currently `Running` (queued on a
/// dependency, blocked, awaiting review, terminal) is neither admitted nor
/// queued — [`drive_one_step`]'s own status check would immediately return
/// [`StepOutcome::Terminal`]/stop for it anyway, so admitting it would only
/// occupy a slot doing nothing.
pub(crate) fn start(state: &AppState, project_id: &str, task_id: &str) {
    match crate::tasks::find_task(state, project_id, task_id) {
        Ok(t) if t.status == TaskStatus::Running && !t.retired => {}
        _ => return,
    }
    if let Some(lease) = try_admit(state, project_id, task_id) {
        spawn_driver_thread(
            state.clone(),
            project_id.to_string(),
            task_id.to_string(),
            lease,
        );
    }
}

fn spawn_driver_thread(state: AppState, project_id: String, task_id: String, lease: DriverLease) {
    std::thread::spawn(move || {
        drive_loop(&state, &project_id, &task_id);
        drop(lease);
    });
}

/// The always-on loop a live node runs per driven task: single-step until a
/// step reports the task is no longer this driver's business (terminal,
/// just transitioned to blocked, or cancelled), sleeping the bounded tick
/// between `Waiting` steps. Never budget/inbox-consuming while waiting.
fn drive_loop(state: &AppState, project_id: &str, task_id: &str) {
    loop {
        match drive_one_step(state, project_id, task_id) {
            StepOutcome::Waiting => std::thread::sleep(WAIT_TICK),
            StepOutcome::Stop => return,
            StepOutcome::IssuedTurn { .. } | StepOutcome::ProofRan { .. } => {}
        }
    }
}

/// What one [`drive_one_step`] call did.
#[derive(Debug)]
pub(crate) enum StepOutcome {
    /// A turn was issued (inbox-driven or autonomous) and returned normally
    /// (including ending in `ask_user`, which this step already turned into
    /// a `Blocked` status change before returning). `event` is read by
    /// tests only (`drive_loop` only needs to know a turn happened, not
    /// which); kept on the variant rather than dropped because the fact "the
    /// driver just issued this exact event" is genuinely part of the
    /// contract a caller may reasonably want, not incidental.
    #[allow(dead_code)]
    IssuedTurn { event: Event },
    /// Proof ran to a verdict — status was updated accordingly either way.
    /// `passed` is read by tests to distinguish the two verdicts; `drive_loop`
    /// itself treats both alike (loop again either way — a failure already
    /// queued its own inbox message to re-drive from).
    #[allow(dead_code)]
    ProofRan { passed: bool },
    /// An undecided gate exists; try again after [`WAIT_TICK`]. Consumes no
    /// budget, delivers no message.
    Waiting,
    /// The task is no longer this driver's business right now: terminal
    /// (`cancelled`/`done`/`awaiting_review`/retired), gated on a dependency
    /// (`queued`), or it just transitioned to `blocked` this step (budget
    /// exhaustion, `ask_user`, or a turn that errored outright). The
    /// background loop stops; a future `start()` call (inbox-message resume,
    /// a fresh `task.start`, boot recovery) is what drives it again.
    Stop,
}

/// One decision, from durable facts alone — see this module's top doc
/// comment on why re-deriving instead of remembering is load-bearing.
pub(crate) fn drive_one_step(state: &AppState, project_id: &str, task_id: &str) -> StepOutcome {
    let task = match crate::tasks::find_task(state, project_id, task_id) {
        Ok(t) => t,
        Err(_) => return StepOutcome::Stop,
    };
    if task.retired || task.status != TaskStatus::Running {
        return StepOutcome::Stop;
    }

    if let Some(summary) = claimed_done_summary(&task) {
        return run_proof_step(state, project_id, &task, &summary);
    }

    if has_undecided_gate(state, &task.stream_id) {
        return StepOutcome::Waiting;
    }

    if let Some((message_hash, msg_actor, text)) =
        next_undelivered_message(state, project_id, task_id)
    {
        deliver_and_issue_turn(
            state,
            project_id,
            &task,
            text,
            Some(message_hash),
            &msg_actor,
        )
    } else {
        let used = turns_issued(state, &task.stream_id);
        if let Some(budget) = resolved_turn_budget(state, &task) {
            if u64::from(used) >= u64::from(budget) {
                let reason = format!("turn budget exhausted ({used}/{budget} turns used)");
                return if set_blocked_then_check_resume(state, project_id, task_id, reason, None) {
                    StepOutcome::Waiting
                } else {
                    StepOutcome::Stop
                };
            }
        }
        let instruction = if used == 0 {
            opening_instruction(&task)
        } else {
            "Continue the task. Call task_complete once its scope is done.".to_string()
        };
        deliver_and_issue_turn(state, project_id, &task, instruction, None, DRIVER_ACTOR)
    }
}

/// `task.status_reason` for a `Running` task whose latest status change was
/// `task_complete`'s `claimed_done: {summary}` marker
/// (`routes::agent::dispatch_task_complete`) — the *only* signal that moves
/// a task toward proof (Phase 4 item 1: "a completed chat turn is not a
/// completed task").
fn claimed_done_summary(task: &TaskRecord) -> Option<String> {
    task.status_reason
        .as_deref()
        .and_then(|r| r.strip_prefix("claimed_done: "))
        .map(str::to_string)
}

/// Every `agent.turn.started` the DRIVER ITSELF issued on this stream — a
/// durable, restart-safe counter (no in-memory budget state to lose across a
/// crash), counted rather than remembered so [`recover_on_boot`]'s honesty
/// holds: the budget a resumed task sees is exactly what actually ran, not
/// what a dead process thought it had run.
///
/// Filtered to `actor == `[`DRIVER_ACTOR`] — now that `agent.turn.started`
/// carries an honest initiator (`AgentTurnReq::actor`), a human (or Baker)
/// steering message relayed through [`deliver_and_issue_turn`] is attributed
/// to ITS OWN actor, not the driver, so it must not silently burn the
/// contract's `turn_budget` the way an autonomous continuation does.
fn turns_issued(state: &AppState, stream_id: &str) -> u32 {
    state
        .heart
        .lock()
        .unwrap()
        .read_stream(stream_id)
        .map(|events| {
            events
                .iter()
                .filter(|e| e.family == "agent.turn.started" && e.actor == DRIVER_ACTOR)
                .count() as u32
        })
        .unwrap_or(0)
}

fn opening_instruction(task: &TaskRecord) -> String {
    let TaskContract {
        scope,
        owned_files,
        depends_on,
        proof,
        ..
    } = &task.contract;
    let mut s = format!("Task: {}\n", task.title);
    if !scope.trim().is_empty() {
        s.push_str(&format!("Scope: {scope}\n"));
    }
    if !owned_files.is_empty() {
        s.push_str(&format!(
            "Owned files (advisory, not enforced): {}\n",
            owned_files.join(", ")
        ));
    }
    if !depends_on.is_empty() {
        s.push_str(&format!(
            "Depends on (already integrated): {}\n",
            depends_on.join(", ")
        ));
    }
    if !proof.is_empty() {
        let rendered: Vec<String> = proof.iter().map(|argv| argv.join(" ")).collect();
        s.push_str(&format!(
            "This task will be proven with: {}\n",
            rendered.join(" ; ")
        ));
    }
    s.push_str("When the scope above is genuinely done, call task_complete with a summary — a plain finished reply does not complete the task.");
    s
}

/// Any undecided write proposal on this exact stream (`routes::operator::
/// gates_inner`'s own "pending status, no accept/reject cites it" rule,
/// filtered to one socket). This is the "wait" half of "events wake, facts
/// decide": a driver step reaching here re-derives from the same projection
/// `GET /gates` renders, never from a snapshot taken before the triggering
/// turn returned.
///
/// **Known narrowing:** this checks the write-proposal gate only. The
/// spec's ACP "child-tool gate" (`pipe::decide_child_tool_gate`) is a
/// separate, more involved approval surface this pass does not wire the
/// driver into — a task worker running under Manual-mode native ACP child
/// tools would not be honored by this wait check. HTTP-transport task
/// workers (this pass's tested path) are fully covered.
pub(crate) fn has_undecided_gate(state: &AppState, stream_id: &str) -> bool {
    crate::routes::operator::gates_inner(state)
        .map(|gates| gates.iter().any(|g| g.stream_id == stream_id))
        .unwrap_or(false)
}

/// The oldest `task.message` for `task_id` with no matching
/// `task.message.delivered` yet — `(event_hash, actor, text)` so the caller
/// can acknowledge the exact fact it consumed AND attribute the turn it
/// issues to whoever actually authored the message (`"human"`,
/// `"agent:baker"`, ...), never blanket-mislabeling it as the driver's own
/// decision.
fn next_undelivered_message(
    state: &AppState,
    project_id: &str,
    task_id: &str,
) -> Option<(String, String, String)> {
    let heart = state.heart.lock().unwrap();
    let events = heart.read_stream(project_id).ok()?;
    let mut delivered: HashSet<String> = HashSet::new();
    for ev in events
        .iter()
        .filter(|e| e.family == TASK_MESSAGE_DELIVERED_FAMILY)
    {
        if let Ok(d) = serde_json::from_str::<TaskMessageDelivered>(&ev.payload) {
            if d.task_id == task_id {
                delivered.insert(d.message_hash);
            }
        }
    }
    for ev in events.iter().filter(|e| e.family == TASK_MESSAGE_FAMILY) {
        let Ok(m) = serde_json::from_str::<TaskMessage>(&ev.payload) else {
            continue;
        };
        if m.task_id == task_id && !delivered.contains(&ev.event_hash) {
            return Some((ev.event_hash.clone(), ev.actor.clone(), m.text));
        }
    }
    None
}

/// Issue exactly one turn as `Seat::Orchestrator` — the seat every task
/// socket's turns actually run under (`tasks::task_cancel_as`'s own doc
/// comment; task sockets are never factory-kind, so `default_seat_for`
/// would pick this anyway — named explicitly here for the same reason
/// `task_cancel_as` enumerates seats explicitly rather than trusting a
/// default) — then, ONLY if the turn actually ran, mark the inbox message
/// (if any) delivered.
///
/// Delivery is recorded *after* the turn succeeds, never before: a turn
/// refused for a conflicting lease (`agent_turn_inner`'s admission check —
/// a human/Baker also holding this agent's turn lease) or one that fails
/// outright never reached the model at all, so marking the message
/// delivered first would permanently drop it — the next step would skip the
/// now-falsely-acknowledged hash and drive an unrelated autonomous
/// continuation instead, silently discarding the human's own steering
/// message (the bug this ordering fixes). `Ok(_)` covers `ask_user` too:
/// the model DID receive the instruction there, it just ended by asking a
/// question, so the message genuinely was delivered.
fn deliver_and_issue_turn(
    state: &AppState,
    project_id: &str,
    task: &TaskRecord,
    instruction: String,
    message_hash: Option<String>,
    turn_actor: &str,
) -> StepOutcome {
    let req = AgentTurnReq {
        stream_id: task.stream_id.clone(),
        file_path: None,
        instruction,
        seat: Some(Seat::Orchestrator),
        model_id: None,
        effort: None,
        actor: Some(turn_actor.to_string()),
    };
    let result = agent_turn_inner(state, &req);
    if result.is_ok() {
        if let Some(hash) = &message_hash {
            let _ = append(
                state,
                project_id,
                DRIVER_ACTOR,
                TASK_MESSAGE_DELIVERED_FAMILY,
                serde_json::to_string(&TaskMessageDelivered {
                    task_id: task.task_id.clone(),
                    message_hash: hash.clone(),
                })
                .unwrap(),
                vec![hash.clone()],
            );
        }
    }
    match result {
        Ok(ev) if ev.family == "agent.ask.pending" => {
            let question = ask_user_summary(&ev.payload);
            // The message this turn consumed (if any) was already marked
            // delivered just above, so it can never self-trigger the
            // race-reconciliation check below — only a genuinely NEW,
            // still-undelivered message does.
            if set_blocked_then_check_resume(
                state,
                project_id,
                &task.task_id,
                format!("asked: {question}"),
                None,
            ) {
                StepOutcome::Waiting
            } else {
                StepOutcome::Stop
            }
        }
        Ok(ev) => StepOutcome::IssuedTurn { event: ev },
        Err((StatusCode::CONFLICT, _)) => {
            // Another turn is already admitted for this agent — this driver
            // is meant to be the only issuer, so this only happens if a
            // human/Baker also called `/agent/turn` directly on this task
            // socket. Wait rather than declaring the task stuck; the
            // message (if any) stays undelivered per this function's own
            // doc comment, and will be retried the next tick.
            StepOutcome::Waiting
        }
        Err((_, msg)) => {
            // `message_hash` stays undelivered here (never marked above,
            // since `result` is `Err`) — excluded from the race check below
            // so THIS exact message cannot immediately retrigger a resume
            // and busy-loop retrying a turn that just failed against it.
            if set_blocked_then_check_resume(
                state,
                project_id,
                &task.task_id,
                format!("turn failed: {msg}"),
                message_hash.as_deref(),
            ) {
                StepOutcome::Waiting
            } else {
                StepOutcome::Stop
            }
        }
    }
}

fn ask_user_summary(payload: &str) -> String {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(payload) else {
        return "the agent asked a question".to_string();
    };
    v.get("questions")
        .and_then(|q| q.as_array())
        .and_then(|arr| arr.first())
        .and_then(|q| q.get("question"))
        .and_then(|q| q.as_str())
        .unwrap_or("the agent asked a question")
        .to_string()
}

fn set_status(
    state: &AppState,
    project_id: &str,
    task_id: &str,
    status: TaskStatus,
    reason: Option<String>,
) {
    let _ = append(
        state,
        project_id,
        DRIVER_ACTOR,
        TASK_STATUS_FAMILY,
        serde_json::to_string(&TaskStatusChanged {
            task_id: task_id.to_string(),
            status,
            reason,
        })
        .unwrap(),
        vec![],
    );
}

/// Transitions `task_id` to `Blocked`, then immediately re-derives whether
/// an undelivered `task.message` exists — closing the race window between a
/// turn ending and this exact append. Without this, a message that lands in
/// that gap is neither delivered (the turn that would have consumed it
/// already returned) nor a wake (`deliver_message_and_maybe_resume` only
/// fires when it OBSERVES the task ALREADY `Blocked` — a snapshot possibly
/// taken before this very append lands) — the task would sit `Blocked`
/// forever with an unread inbox.
///
/// `excluding_hash` is the one message THIS step may have already
/// intentionally left undelivered (`deliver_and_issue_turn`'s turn-failure
/// path: a turn that failed while consuming a message keeps that exact
/// message queued, on purpose, for a retry) — it must NOT be allowed to
/// immediately retrigger this same check, or every failing retry would
/// busy-loop at [`WAIT_TICK`] against the very message that just failed.
///
/// Returns `true` when it reconciled straight back to `Running` — the
/// caller MUST report [`StepOutcome::Waiting`] in that case, never `Stop`:
/// `Stop` ends this driver's background thread, and nothing else would ever
/// start a new one for a task Heart facts now say is `Running`.
fn set_blocked_then_check_resume(
    state: &AppState,
    project_id: &str,
    task_id: &str,
    reason: String,
    excluding_hash: Option<&str>,
) -> bool {
    set_status(
        state,
        project_id,
        task_id,
        TaskStatus::Blocked,
        Some(reason),
    );
    match next_undelivered_message(state, project_id, task_id) {
        Some((hash, ..)) if Some(hash.as_str()) != excluding_hash => {
            set_status(
                state,
                project_id,
                task_id,
                TaskStatus::Running,
                Some("resumed — a message arrived while blocking".to_string()),
            );
            true
        }
        _ => false,
    }
}

// ─── Proof (Phase 4 item 2) ──────────────────────────────────────────────

/// One proof command's recorded outcome — audit trail on the task's own
/// stream (chat-visible), same "every op is an event" posture the rest of
/// this slice holds to.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct ProofRan {
    task_id: String,
    argv: Vec<String>,
    exit_code: i32,
    stdout: String,
    stderr: String,
    ok: bool,
}

/// Runs the contract's `proof` argv lists in order in the task's own room
/// (`bakery_exec::room_for_run`/`attach_exec` — the exact machinery
/// `tools::Jail::run` uses), stopping at the first failure. `Ok(true)` means
/// every command exited 0; `Ok(false)` carries the first failure's combined
/// output (for the next turn's instruction); `Err` means proof could not
/// even be attempted (no materialized workspace — a genuine misconfiguration,
/// not a proof failure the agent can fix by trying again).
fn run_proof(state: &AppState, task: &TaskRecord) -> Result<Result<(), String>, String> {
    if task.contract.proof.is_empty() {
        return Ok(Ok(()));
    }
    let ctx = crate::state::context_for(state, &task.stream_id)
        .map_err(|e| format!("cannot resolve task workspace: {e}"))?;
    if ctx.worktree_path.is_empty() || !std::path::Path::new(&ctx.worktree_path).is_dir() {
        return Err("proof commands are declared but this task has no materialized workspace to run them in".to_string());
    }
    let root = std::path::Path::new(&ctx.worktree_path);
    let cache_root = crate::state::cache_root(state);
    let _guard = crate::state::admit_one_shot(state, &task.stream_id).map_err(|(_, msg)| msg)?;
    // Same room the task's own `run` tool gets: the socket's real toolchain
    // (a task on a mise-pinned project must prove itself with that
    // toolchain, not a thin room), a writable workspace (proof commands are
    // builds/tests — `cargo test` writing `target/` IS the point), and the
    // linked-worktree git grants.
    let toolchain = crate::catalog::stacks_for_socket(state, &task.stream_id).toolchain;
    let env = bakery_exec::room_for_run(root, &cache_root, &task.stream_id, &toolchain)?;
    let (extra_read, extra_write) = match crate::git::linked_worktree_git_dirs(root) {
        Some((shared, own)) => (vec![shared], vec![own]),
        None => (Vec::new(), Vec::new()),
    };
    let granted = crate::machine_access::for_stream(state, &task.stream_id);

    for argv in &task.contract.proof {
        if argv.is_empty() {
            continue;
        }
        let out = bakery_exec::attach_exec_workspace_rw(
            &env,
            root,
            &bakery_exec::WorkspaceRun {
                argv,
                extra_deny_read: &granted.deny,
                timeout: bakery_exec::DEFAULT_WORKSPACE_RUN_TIMEOUT,
                extra_read: &extra_read,
                extra_write: &extra_write,
                extra_exec: &granted.exec,
                whole_machine: None,
                path: &granted.path,
                env: &granted.env,
                append_only: &[],
            },
        )?;
        let ok = out.exit_code == 0;
        let _ = append(
            state,
            &task.stream_id,
            DRIVER_ACTOR,
            "task.proof.ran",
            serde_json::to_string(&ProofRan {
                task_id: task.task_id.clone(),
                argv: argv.clone(),
                exit_code: out.exit_code,
                stdout: crate::agent_loop::truncate(&out.stdout, MAX_REVIEW_FILE_BYTES),
                stderr: crate::agent_loop::truncate(&out.stderr, MAX_REVIEW_FILE_BYTES),
                ok,
            })
            .unwrap(),
            vec![],
        );
        if !ok {
            return Ok(Err(format!(
                "proof command {argv:?} failed (exit {}):\nstdout:\n{}\nstderr:\n{}",
                out.exit_code, out.stdout, out.stderr
            )));
        }
    }
    Ok(Ok(()))
}

fn run_proof_step(
    state: &AppState,
    project_id: &str,
    task: &TaskRecord,
    summary: &str,
) -> StepOutcome {
    match run_proof(state, task) {
        Ok(Ok(())) => {
            set_status(
                state,
                project_id,
                &task.task_id,
                TaskStatus::AwaitingReview,
                Some(format!("proof passed: {summary}")),
            );
            StepOutcome::ProofRan { passed: true }
        }
        Ok(Err(failure)) => {
            // Clear the `claimed_done` marker (status stays `Running`) and
            // feed the failure through the SAME inbox path a human message
            // would take — Phase 4 item 1: "proof failure → back to running
            // with the failure output as the next turn's instruction," and
            // this reuses the one durable delivery mechanism rather than a
            // second ad hoc "next instruction" channel.
            set_status(
                state,
                project_id,
                &task.task_id,
                TaskStatus::Running,
                Some("proof failed — see task.message".to_string()),
            );
            let _ = append(
                state,
                project_id,
                DRIVER_ACTOR,
                TASK_MESSAGE_FAMILY,
                serde_json::to_string(&TaskMessage {
                    task_id: task.task_id.clone(),
                    text: failure,
                })
                .unwrap(),
                vec![],
            );
            StepOutcome::ProofRan { passed: false }
        }
        Err(misconfigured) => {
            if set_blocked_then_check_resume(state, project_id, &task.task_id, misconfigured, None)
            {
                StepOutcome::Waiting
            } else {
                StepOutcome::Stop
            }
        }
    }
}

// ─── Floor-row baseline diff (Phase 4 item 3) ────────────────────────────

/// Every materialized-git task socket's `(worktree_path, base_commit)`,
/// keyed by the task's OWN stream id — one pass over every known project's
/// stream using an ALREADY-LOCKED `Heart`, so `routes::studio::studio_state`
/// can call this in its own separate pre-pass (the `running_turns` pattern:
/// a short-lived lock-then-drop section before the main per-socket loop
/// takes `heart` again) without ever nesting two `heart.lock()` calls on the
/// same thread. A task with no `task.workspace.materialized` fact yet
/// (chat-only, or not yet started) is simply absent — callers treat a
/// missing entry as "no baseline diff to show," never as an error.
pub(crate) fn task_baselines_locked(
    heart: &bakery_heart::Heart,
) -> std::collections::HashMap<String, (String, String)> {
    let mut out = std::collections::HashMap::new();
    let Ok(projects) = crate::catalog::known_projects_locked(heart) else {
        return out;
    };
    for p in projects {
        let Ok(events) = heart.read_stream(&p.project_id) else {
            continue;
        };
        let mut task_streams: std::collections::HashMap<String, String> = Default::default();
        let mut base_commits: std::collections::HashMap<String, String> = Default::default();
        for ev in &events {
            if ev.family == crate::tasks::TASK_CREATED_FAMILY {
                if let Ok(c) = serde_json::from_str::<crate::tasks::TaskCreated>(&ev.payload) {
                    task_streams.insert(c.task_id, c.stream_id);
                }
            } else if ev.family == crate::tasks::TASK_WORKSPACE_MATERIALIZED_FAMILY {
                if let Ok(m) =
                    serde_json::from_str::<crate::tasks::TaskWorkspaceMaterialized>(&ev.payload)
                {
                    base_commits.insert(m.task_id, m.base_commit);
                }
            }
        }
        for (task_id, stream_id) in task_streams {
            let Some(base_commit) = base_commits.get(&task_id) else {
                continue;
            };
            let Ok(Some(ws_ev)) =
                heart.last_of_family(&stream_id, crate::node::SOCKET_WORKSPACE_FAMILY)
            else {
                continue;
            };
            let Ok(ws) = serde_json::from_str::<crate::node::WorkspaceSet>(&ws_ev.payload) else {
                continue;
            };
            out.insert(stream_id, (ws.worktree_path, base_commit.clone()));
        }
    }
    out
}

/// How long a Floor row's `changed_added`/`changed_removed` may lag reality
/// — "bounded staleness," per Phase 4 item 3. The three surfaces that must
/// never trust a cache at all (review presentation, sign-off, integration —
/// all in `task_review.rs`) call `task_review::diff_counts`/`compute_files`
/// directly and never touch this cache.
const CHANGED_COUNTS_TTL: Duration = Duration::from_secs(3);

/// Cached `(added, removed)` for one task socket's baseline diff, recomputed
/// at most once per [`CHANGED_COUNTS_TTL`] (also naturally refreshed at
/// every driver wake, since a driven task's own steps call this indirectly
/// through the next `/studio/state` poll — no separate invalidation hook is
/// wired for native ACP/terminal edits in this pass; the bounded TTL is what
/// catches those instead of a filesystem watch).
pub(crate) fn changed_counts_cached(
    state: &AppState,
    task_stream_id: &str,
    worktree_path: &str,
    base_commit: &str,
) -> (usize, usize) {
    {
        let cache = state.task_diff_cache.lock().unwrap();
        if let Some((added, removed, at)) = cache.get(task_stream_id) {
            if at.elapsed() < CHANGED_COUNTS_TTL {
                return (*added, *removed);
            }
        }
    }
    let counts = crate::task_review::diff_counts(worktree_path, base_commit).unwrap_or((0, 0));
    state.task_diff_cache.lock().unwrap().insert(
        task_stream_id.to_string(),
        (counts.0, counts.1, std::time::Instant::now()),
    );
    counts
}

// ─── Wake: message resume + boot recovery ───────────────────────────────

/// Called after every `task.message` append (`tasks::task_message_as`), for
/// every actor. A no-op unless the task is currently `Blocked` — in which
/// case this IS the resume transition (Phase 4 item 1's `ask_user` rule,
/// generalized to every `Blocked` reason: turn-budget exhaustion, a turn
/// error, and restart recovery's "node restarted mid-turn" all resume the
/// same way a human's answer to a question does).
pub(crate) fn deliver_message_and_maybe_resume(state: &AppState, project_id: &str, task_id: &str) {
    let Ok(task) = crate::tasks::find_task(state, project_id, task_id) else {
        return;
    };
    if task.retired || task.status != TaskStatus::Blocked {
        return;
    }
    set_status(
        state,
        project_id,
        task_id,
        TaskStatus::Running,
        Some("resumed by an inbox message".to_string()),
    );
    start(state, project_id, task_id);
}

/// Boot recovery: every task recorded `Running` when this node starts lost
/// its driver with the previous process. It carries on by itself, as
/// conversations do (`resume`): the turn the restart cut off is closed,
/// and its driver starts again. Work restarts cut off three times in a row
/// is `Blocked` instead, honestly, and waits for the operator (a message
/// brings it back, see [`deliver_message_and_maybe_resume`]).
pub(crate) fn recover_on_boot(state: &AppState) {
    let heart = state.heart.lock().unwrap();
    let projects = crate::catalog::known_projects_locked(&heart).unwrap_or_default();
    drop(heart);
    for p in projects {
        for t in crate::tasks::task_list(state, &p.project_id) {
            if t.retired || t.status != TaskStatus::Running {
                continue;
            }
            let streak = crate::resume::close_after_restart(state, &t.stream_id);
            if streak.is_some_and(|n| n >= crate::resume::RESTART_RESUMES) {
                let why = "the Bakery restarted on this work three times in a row; it waits for you";
                set_status(state, &p.project_id, &t.task_id, TaskStatus::Blocked, Some(why.to_string()));
            } else {
                let why = "carried on after the Bakery restarted";
                set_status(state, &p.project_id, &t.task_id, TaskStatus::Running, Some(why.to_string()));
                start(state, &p.project_id, &t.task_id);
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    /// Direct admission-map probe (07d-B Phase 4 acceptance: "capacity...
    /// observable via the admission map").
    pub(crate) fn active_count(state: &AppState) -> usize {
        state_registry(state).inner.lock().unwrap().active.len()
    }

    pub(crate) fn pending_task_ids(state: &AppState) -> Vec<String> {
        state_registry(state)
            .inner
            .lock()
            .unwrap()
            .pending
            .iter()
            .map(|(_, t)| t.clone())
            .collect()
    }

    pub(crate) fn is_active(state: &AppState, task_id: &str) -> bool {
        state_registry(state)
            .inner
            .lock()
            .unwrap()
            .active
            .contains(task_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::Seat as NodeSeat;
    use crate::tasks::{TaskCancelReq, TaskCreateReq, TaskMessageReq, TaskStartReq};
    use crate::testutil::*;
    use bakery_agent::{ChatMsg, ChatReply, EditRequest, Model, ProposedEdit, StubModel, ToolDecl};
    use std::sync::{Arc, Condvar, Mutex};

    fn project_with_git_root(state: &AppState, dir: &std::path::Path) -> String {
        let repo = make_repo(&dir.join("repo"));
        crate::catalog::create_project(
            state,
            "P",
            Some(crate::catalog::DirectoryRootSpec::Attach(
                &repo.to_string_lossy(),
            )),
        )
        .unwrap()
        .0
        .project_id
    }

    fn chat_only_project(state: &AppState) -> String {
        crate::catalog::create_project(state, "Chat P", None)
            .unwrap()
            .0
            .project_id
    }

    fn start_task(
        state: &AppState,
        project_id: &str,
        title: &str,
        work_order: &str,
        contract: TaskContract,
    ) -> (String, String) {
        let created = crate::tasks::task_create(
            state,
            TaskCreateReq {
                parent_project_id: project_id.to_string(),
                title: title.to_string(),
                work_order: work_order.to_string(),
                contract,
            },
        )
        .unwrap();
        crate::tasks::task_start(
            state,
            TaskStartReq {
                project_id: project_id.to_string(),
                task_id: created.task.task_id.clone(),
            },
        )
        .unwrap();
        (created.task.task_id, created.task.stream_id)
    }

    fn seed(state: &AppState, stream_id: &str, model: Arc<dyn Model>) {
        crate::state::seed_seat_model_for_test(state, stream_id, NodeSeat::Orchestrator, model);
    }

    fn task_status(
        state: &AppState,
        project_id: &str,
        task_id: &str,
    ) -> (TaskStatus, Option<String>) {
        let t = crate::tasks::find_task(state, project_id, task_id).unwrap();
        (t.status, t.status_reason)
    }

    /// Scripted, chat-only-turn model — every reply is plain text or a
    /// declared-tool JSON call, replayed in order (`ScriptedModel`'s own
    /// shape, duplicated locally because `testutil::ScriptedModel` is
    /// `pub(crate)` but its `captured`/`seen` bookkeeping is more than this
    /// module's tests need).
    struct Turns(Mutex<std::collections::VecDeque<&'static str>>);
    impl Turns {
        fn new(replies: &[&'static str]) -> Arc<dyn Model> {
            Arc::new(Turns(Mutex::new(replies.iter().copied().collect())))
        }
    }
    impl Model for Turns {
        fn name(&self) -> &str {
            "turns"
        }
        fn propose_edit(&self, _req: &EditRequest) -> Result<ProposedEdit, String> {
            Err("chat only".into())
        }
        fn supports_chat(&self) -> bool {
            true
        }
        fn chat_turn(
            &self,
            _system: &str,
            _messages: &[ChatMsg],
            _tools: &[ToolDecl],
        ) -> Result<ChatReply, String> {
            let raw = self
                .0
                .lock()
                .unwrap()
                .pop_front()
                .ok_or("script exhausted")?;
            Ok(crate::agent_loop::chat_reply_from_scripted_text(raw))
        }
    }

    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn scripted_task_reaches_task_complete_then_proof_true_then_awaiting_review() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let project_id = project_with_git_root(&state, dir.path());
        let contract = TaskContract {
            proof: vec![vec!["true".to_string()]],
            ..Default::default()
        };
        let (task_id, stream_id) =
            start_task(&state, &project_id, "Do the thing", "wo-1", contract);
        seed(
            &state,
            &stream_id,
            Turns::new(&[
                "thinking about it",
                r#"{"tool":"task_complete","args":{"summary":"all done"}}"#,
                "wrapped up",
            ]),
        );

        assert!(
            matches!(
                drive_one_step(&state, &project_id, &task_id),
                StepOutcome::IssuedTurn { .. }
            ),
            "turn 1"
        );
        assert_eq!(
            task_status(&state, &project_id, &task_id).0,
            TaskStatus::Running
        );

        assert!(
            matches!(
                drive_one_step(&state, &project_id, &task_id),
                StepOutcome::IssuedTurn { .. }
            ),
            "turn 2 (task_complete)"
        );
        let (status, reason) = task_status(&state, &project_id, &task_id);
        assert_eq!(status, TaskStatus::Running);
        assert!(reason.unwrap().starts_with("claimed_done:"));

        match drive_one_step(&state, &project_id, &task_id) {
            StepOutcome::ProofRan { passed: true } => {}
            other => panic!("expected proof to pass: {other:?}"),
        }
        let (status, reason) = task_status(&state, &project_id, &task_id);
        assert_eq!(status, TaskStatus::AwaitingReview);
        assert!(reason.unwrap().starts_with("proof passed"));

        assert!(
            matches!(
                drive_one_step(&state, &project_id, &task_id),
                StepOutcome::Stop
            ),
            "awaiting_review must not be driven further"
        );
    }

    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn proof_failure_loops_back_to_running_with_the_failure_output_as_the_next_instruction() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let project_id = project_with_git_root(&state, dir.path());
        let contract = TaskContract {
            proof: vec![vec!["false".to_string()]],
            ..Default::default()
        };
        let (task_id, stream_id) =
            start_task(&state, &project_id, "Do the thing", "wo-2", contract);
        seed(
            &state,
            &stream_id,
            Turns::new(&[
                r#"{"tool":"task_complete","args":{"summary":"done"}}"#,
                "ended turn 1",
                "noted the failure",
            ]),
        );

        drive_one_step(&state, &project_id, &task_id); // claim
        match drive_one_step(&state, &project_id, &task_id) {
            StepOutcome::ProofRan { passed: false } => {}
            other => panic!("expected proof to fail: {other:?}"),
        }
        let (status, reason) = task_status(&state, &project_id, &task_id);
        assert_eq!(
            status,
            TaskStatus::Running,
            "a failed proof must loop back to running, never stall as blocked"
        );
        assert!(reason.unwrap().contains("proof failed"));

        // The failure output must reach the model as the very next turn's
        // instruction, delivered through the ordinary inbox path.
        drive_one_step(&state, &project_id, &task_id);
        let events = state.heart.lock().unwrap().read_stream(&stream_id).unwrap();
        let last_started = events
            .iter()
            .rev()
            .find(|e| e.family == "agent.turn.started")
            .unwrap();
        let payload: serde_json::Value = serde_json::from_str(&last_started.payload).unwrap();
        let instruction = payload["instruction"].as_str().unwrap();
        assert!(instruction.contains("proof command"), "got: {instruction}");
        assert!(
            instruction.contains("false"),
            "the failing argv must be named in the instruction: {instruction}"
        );

        let project_events = state
            .heart
            .lock()
            .unwrap()
            .read_stream(&project_id)
            .unwrap();
        assert!(
            project_events
                .iter()
                .any(|e| e.family == TASK_MESSAGE_DELIVERED_FAMILY),
            "the failure message must be marked delivered like any other inbox entry"
        );
    }

    #[test]
    fn an_inbox_message_is_delivered_before_the_next_autonomous_turn() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let project_id = chat_only_project(&state);
        let (task_id, stream_id) = start_task(
            &state,
            &project_id,
            "Research",
            "wo-3",
            TaskContract::default(),
        );
        seed(
            &state,
            &stream_id,
            Turns::new(&[
                "opening turn done",
                "handled the steering",
                "autonomous again",
            ]),
        );

        drive_one_step(&state, &project_id, &task_id); // opening turn

        let msg = crate::tasks::task_message(
            &state,
            TaskMessageReq {
                project_id: project_id.clone(),
                task_id: task_id.clone(),
                text: "steer this way".into(),
            },
        )
        .unwrap();

        drive_one_step(&state, &project_id, &task_id); // must consume the queued message next, not go autonomous
        let events = state.heart.lock().unwrap().read_stream(&stream_id).unwrap();
        let turns: Vec<_> = events
            .iter()
            .filter(|e| e.family == "agent.turn.started")
            .collect();
        assert_eq!(turns.len(), 2);
        let instruction = serde_json::from_str::<serde_json::Value>(&turns[1].payload).unwrap()
            ["instruction"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(
            instruction, "steer this way",
            "the queued message must precede the next autonomous turn"
        );

        let project_events = state
            .heart
            .lock()
            .unwrap()
            .read_stream(&project_id)
            .unwrap();
        let delivered = project_events
            .iter()
            .find(|e| e.family == TASK_MESSAGE_DELIVERED_FAMILY)
            .map(|e| serde_json::from_str::<TaskMessageDelivered>(&e.payload).unwrap())
            .expect("task.message.delivered must land");
        assert_eq!(delivered.message_hash, msg.event.event_hash);

        drive_one_step(&state, &project_id, &task_id); // now genuinely autonomous
        let events = state.heart.lock().unwrap().read_stream(&stream_id).unwrap();
        let turns: Vec<_> = events
            .iter()
            .filter(|e| e.family == "agent.turn.started")
            .collect();
        assert_eq!(turns.len(), 3);
        let instruction = serde_json::from_str::<serde_json::Value>(&turns[2].payload).unwrap()
            ["instruction"]
            .as_str()
            .unwrap()
            .to_string();
        assert_ne!(
            instruction, "steer this way",
            "the third turn must be the driver's own autonomous continuation"
        );
    }

    #[test]
    fn auto_accept_inside_the_turn_does_not_deadlock_the_driver() {
        let dir = tempfile::tempdir().unwrap();
        // The node-wide write mode gates nothing now (Manual here proves it);
        // the task agent's own Auto is what decides inside the turn.
        let state = state_with_write_mode(
            dir.path(),
            Arc::new(StubModel),
            crate::node::WriteMode::Manual,
        );
        let project_id = project_with_git_root(&state, dir.path());
        let (task_id, stream_id) = start_task(
            &state,
            &project_id,
            "Write a file",
            "wo-4",
            TaskContract::default(),
        );
        crate::agent_control::set_operating_mode(
            &state,
            &stream_id,
            Seat::Orchestrator,
            crate::agent_control::OperatingMode::Auto,
        )
        .unwrap();
        seed(
            &state,
            &stream_id,
            Turns::new(&[
                r#"{"tool":"write_file","args":{"path":"f.txt","content":"hi\n","rationale":"test"}}"#,
                r#"{"tool":"task_complete","args":{"summary":"wrote it"}}"#,
                "done",
            ]),
        );

        // Turn 1 proposes a write; Auto mode decides it INSIDE that same
        // turn — and with continue-after-decision the SAME turn then
        // consumes the outcome, calls task_complete, and finishes. A
        // driver that assumed "a proposal must still be pending after this
        // call" would wait forever; it must re-derive and go straight on.
        assert!(matches!(
            drive_one_step(&state, &project_id, &task_id),
            StepOutcome::IssuedTurn { .. }
        ));
        assert!(
            !has_undecided_gate(&state, &stream_id),
            "auto-accept must have already decided the proposal inside the turn"
        );
        let (status, reason) = task_status(&state, &project_id, &task_id);
        assert_eq!(status, TaskStatus::Running);
        assert!(
            reason.unwrap().starts_with("claimed_done"),
            "the continued turn reached task_complete"
        );

        let ctx = crate::state::context_for(&state, &stream_id).unwrap();
        assert_eq!(
            std::fs::read_to_string(std::path::Path::new(&ctx.worktree_path).join("f.txt"))
                .unwrap(),
            "hi\n",
            "Auto mode must have actually written the accepted content to disk"
        );

        // Next step: proof (empty contract → passes) → awaiting review.
        assert!(matches!(
            drive_one_step(&state, &project_id, &task_id),
            StepOutcome::ProofRan { .. } | StepOutcome::Waiting | StepOutcome::Stop
        ));
        assert_eq!(
            task_status(&state, &project_id, &task_id).0,
            TaskStatus::AwaitingReview
        );
    }

    #[test]
    fn ask_user_parks_the_turn_and_the_answer_resumes_it() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let project_id = chat_only_project(&state);
        let (task_id, stream_id) = start_task(
            &state,
            &project_id,
            "Research",
            "wo-5",
            TaskContract::default(),
        );
        seed(
            &state,
            &stream_id,
            Turns::new(&[
                r#"{"tool":"ask_user","args":{"questions":[{"question":"which approach?","options":["a","b"]}]}}"#,
                "proceeding with the chosen approach",
            ]),
        );

        // Continue-after-decision: the driver's turn PARKS at the question
        // (task stays Running, the question is the live pending gate) and
        // the human's answer resumes that same turn.
        let (st2, pid2, tid2) = (state.clone(), project_id.clone(), task_id.clone());
        let driver = std::thread::spawn(move || drive_one_step(&st2, &pid2, &tid2));

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let ask_hash = loop {
            let events = state
                .heart
                .lock()
                .unwrap()
                .read_stream(&stream_id)
                .unwrap_or_default();
            if let Some(ev) = events.iter().find(|e| e.family == "agent.ask.pending") {
                break ev.event_hash.clone();
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the question must land on the stream"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        assert_eq!(
            task_status(&state, &project_id, &task_id).0,
            TaskStatus::Running,
            "asking does not block the task — the turn is parked, not dead"
        );

        crate::routes::agent::agent_answer_inner(
            &state,
            &crate::routes::agent::AgentAnswerReq {
                stream_id: stream_id.clone(),
                event_hash: ask_hash.clone(),
                answer: "a".into(),
                answers: None,
            },
        )
        .unwrap();

        assert!(matches!(
            driver.join().unwrap(),
            StepOutcome::IssuedTurn { .. }
        ));
        let events = state.heart.lock().unwrap().read_stream(&stream_id).unwrap();
        assert!(
            events.iter().any(|e| e.family == "agent.ask.answered"),
            "the answer is durable"
        );
        let done = events
            .iter()
            .find(|e| e.family == "agent.turn.done")
            .expect("the SAME turn finished after the answer");
        assert!(done.payload.contains("proceeding with the chosen approach"));
    }

    #[test]
    fn cancel_mid_task_stops_the_driver_and_the_task_stays_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let project_id = chat_only_project(&state);
        let (task_id, stream_id) =
            start_task(&state, &project_id, "Long", "wo-6", TaskContract::default());
        seed(
            &state,
            &stream_id,
            Turns::new(&["turn 1", "would be turn 2"]),
        );

        drive_one_step(&state, &project_id, &task_id);
        crate::tasks::task_cancel(
            &state,
            TaskCancelReq {
                project_id: project_id.clone(),
                task_id: task_id.clone(),
                reason: Some("changed mind".into()),
            },
        )
        .unwrap();

        assert!(matches!(
            drive_one_step(&state, &project_id, &task_id),
            StepOutcome::Stop
        ));
        assert_eq!(
            task_status(&state, &project_id, &task_id).0,
            TaskStatus::Cancelled,
            "cancel must win and stay won — no further turn may run"
        );
    }

    #[test]
    fn turn_budget_exhaustion_blocks_the_task_instead_of_stalling_silently() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let project_id = chat_only_project(&state);
        let contract = TaskContract {
            turn_budget: Some(1),
            ..Default::default()
        };
        let (task_id, stream_id) = start_task(&state, &project_id, "Research", "wo-7", contract);
        seed(&state, &stream_id, Turns::new(&["only turn"]));

        assert!(matches!(
            drive_one_step(&state, &project_id, &task_id),
            StepOutcome::IssuedTurn { .. }
        ));
        assert!(matches!(
            drive_one_step(&state, &project_id, &task_id),
            StepOutcome::Stop
        ));
        let (status, reason) = task_status(&state, &project_id, &task_id);
        assert_eq!(status, TaskStatus::Blocked);
        assert!(reason.unwrap().contains("turn budget exhausted"));
    }

    /// Checkpoint 2 item 4: work budgets are explicit and configurable —
    /// contract wins, node config backstops, and with neither there is NO
    /// ceiling (the silent 40 is gone; the stall guard and cancellation are
    /// the real limits).
    #[test]
    fn turn_budget_resolution_has_no_hidden_default() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let project_id = chat_only_project(&state);
        let (task_id, _sid) = start_task(
            &state,
            &project_id,
            "Budget",
            "wo-budget",
            TaskContract::default(),
        );
        let task = crate::tasks::find_task(&state, &project_id, &task_id).unwrap();

        assert_eq!(
            resolved_turn_budget(&state, &task),
            None,
            "no contract + no config = no ceiling"
        );

        state.node.lock().unwrap().task_turn_budget = std::num::NonZeroU32::new(7);
        assert_eq!(
            resolved_turn_budget(&state, &task),
            Some(7),
            "node config backstops"
        );

        let contract = TaskContract {
            turn_budget: Some(2),
            ..Default::default()
        };
        let (task_id2, _sid2) = start_task(&state, &project_id, "Budget2", "wo-budget", contract);
        let task2 = crate::tasks::find_task(&state, &project_id, &task_id2).unwrap();
        assert_eq!(
            resolved_turn_budget(&state, &task2),
            Some(2),
            "the contract's own budget wins"
        );
    }

    #[test]
    fn boot_recovery_carries_a_running_task_on() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let project_id = chat_only_project(&state);
        let (task_id, _stream_id) = start_task(
            &state,
            &project_id,
            "Research",
            "wo-8",
            TaskContract::default(),
        );
        assert_eq!(
            task_status(&state, &project_id, &task_id).0,
            TaskStatus::Running
        );

        recover_on_boot(&state);

        let reasons: Vec<String> = state
            .heart
            .lock()
            .unwrap()
            .events_of_family(&project_id, TASK_STATUS_FAMILY)
            .unwrap()
            .iter()
            .filter_map(|e| serde_json::from_str::<TaskStatusChanged>(&e.payload).ok())
            .filter(|c| c.task_id == task_id)
            .filter_map(|c| c.reason)
            .collect();
        assert!(
            reasons.iter().any(|r| r == "carried on after the Bakery restarted"),
            "{reasons:?}"
        );
    }

    /// Work restarts cut off three times in a row is honestly blocked and
    /// waits for the operator, never falsely running.
    #[test]
    fn work_restarts_cut_off_three_times_in_a_row_waits_for_the_operator() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let project_id = chat_only_project(&state);
        let (task_id, stream_id) = start_task(
            &state,
            &project_id,
            "Research",
            "wo-8b",
            TaskContract::default(),
        );
        let agent_id = crate::node::agent_id(&stream_id, crate::node::Seat::Orchestrator);
        let add = |family: &str, payload: serde_json::Value| {
            append(&state, &stream_id, "node", family, payload.to_string(), vec![]).unwrap();
        };
        for n in 0..4 {
            let turn = format!("t{n}");
            add("agent.turn.started", serde_json::json!({"agent_id": agent_id, "turn_id": turn, "seat": "orchestrator", "instruction": "work"}));
            if n < 3 {
                add("agent.turn.failed", serde_json::json!({"agent_id": agent_id, "turn_id": turn, "closed_by_resume": true, "after_restart": true}));
            }
        }

        recover_on_boot(&state);

        let (status, reason) = task_status(&state, &project_id, &task_id);
        assert_eq!(status, TaskStatus::Blocked);
        assert!(reason.unwrap().contains("three times in a row"));
    }

    #[test]
    fn a_blocked_task_resumes_to_running_when_an_inbox_message_lands() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let project_id = chat_only_project(&state);
        let (task_id, _stream_id) = start_task(
            &state,
            &project_id,
            "Research",
            "wo-9",
            TaskContract::default(),
        );
        set_status(
            &state,
            &project_id,
            &task_id,
            TaskStatus::Blocked,
            Some("waits for the operator".to_string()),
        );
        assert_eq!(
            task_status(&state, &project_id, &task_id).0,
            TaskStatus::Blocked
        );

        crate::tasks::task_message(
            &state,
            TaskMessageReq {
                project_id: project_id.clone(),
                task_id: task_id.clone(),
                text: "please continue".into(),
            },
        )
        .unwrap();

        assert_eq!(
            task_status(&state, &project_id, &task_id).0,
            TaskStatus::Running,
            "a message must resume a blocked task"
        );
    }

    struct Gate {
        released: Mutex<bool>,
        cv: Condvar,
    }
    struct GateModel(Arc<Gate>);
    impl Model for GateModel {
        fn name(&self) -> &str {
            "gate"
        }
        fn propose_edit(&self, _req: &EditRequest) -> Result<ProposedEdit, String> {
            Err("chat only".into())
        }
        fn supports_chat(&self) -> bool {
            true
        }
        fn chat_turn(
            &self,
            _system: &str,
            _messages: &[ChatMsg],
            _tools: &[ToolDecl],
        ) -> Result<ChatReply, String> {
            let mut g = self.0.released.lock().unwrap();
            if !*g {
                while !*g {
                    g = self.0.cv.wait(g).unwrap();
                }
                return Ok(ChatReply::Text("released".into()));
            }
            Err("gate already spent — stop driving".into())
        }
    }

    /// 07d-B Phase 4 acceptance: "capacity: 5 started tasks, at most 4
    /// driven concurrently (observable via the admission map)."
    #[test]
    fn at_most_four_tasks_are_driven_concurrently_and_a_freed_slot_promotes_the_fifth() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let project_id = chat_only_project(&state);

        let mut task_ids = Vec::new();
        let mut gates: Vec<Arc<Gate>> = Vec::new();
        for i in 0..5 {
            let (task_id, stream_id) = start_task(
                &state,
                &project_id,
                &format!("T{i}"),
                &format!("wo-cap-{i}"),
                TaskContract::default(),
            );
            let gate = Arc::new(Gate {
                released: Mutex::new(false),
                cv: Condvar::new(),
            });
            seed(&state, &stream_id, Arc::new(GateModel(gate.clone())));
            task_ids.push(task_id.clone());
            gates.push(gate);
            start(&state, &project_id, &task_id);
        }

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while test_support::active_count(&state) < DRIVER_CAPACITY
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            test_support::active_count(&state),
            DRIVER_CAPACITY,
            "at most DRIVER_CAPACITY tasks may be driven at once"
        );
        let pending = test_support::pending_task_ids(&state);
        assert_eq!(
            pending.len(),
            1,
            "the 5th started task must be queued for capacity, not dropped or over-admitted"
        );
        let pending_id = pending[0].clone();
        assert!(task_ids.contains(&pending_id));

        // The row a driver denied for capacity must be RECORDED `queued`,
        // never left `running` with nothing actually driving it — the
        // finding this pass fixes: a durable, honest Heart fact, not just
        // in-memory admission-map bookkeeping.
        let (status, reason) = task_status(&state, &project_id, &pending_id);
        assert_eq!(
            status,
            TaskStatus::Queued,
            "a task denied a driver slot for capacity must be recorded `queued`, not `running`"
        );
        assert!(
            reason.unwrap().contains("driver capacity"),
            "the reason must name capacity, per the spec's `queued` reasons"
        );

        let active_idx = task_ids
            .iter()
            .position(|t| test_support::is_active(&state, t))
            .unwrap();
        {
            let mut g = gates[active_idx].released.lock().unwrap();
            *g = true;
        }
        gates[active_idx].cv.notify_all();

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !test_support::is_active(&state, &pending_id) && std::time::Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            test_support::is_active(&state, &pending_id),
            "a freed slot must promote the queued task"
        );
        assert_eq!(
            task_status(&state, &project_id, &pending_id).0,
            TaskStatus::Running,
            "a promoted task must be flipped back to `running` before it is actually driven"
        );

        // Let every remaining gated thread finish so no test leaks a
        // permanently blocked background thread past this function.
        for g in &gates {
            let mut l = g.released.lock().unwrap();
            *l = true;
            g.cv.notify_all();
        }
        std::thread::sleep(Duration::from_millis(200));
    }

    /// 07d-B Phase 4 acceptance, extended: a task that goes stale (cancelled)
    /// while it sits in `pending` must not starve the task queued behind it
    /// — a freed slot keeps trying the next pending entry instead of giving
    /// up after one stale pop.
    #[test]
    fn a_cancelled_pending_task_does_not_starve_the_one_queued_behind_it() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let project_id = chat_only_project(&state);

        let mut task_ids = Vec::new();
        let mut gates: Vec<Arc<Gate>> = Vec::new();
        // DRIVER_CAPACITY active + 2 pending: [stale, survivor].
        for i in 0..(DRIVER_CAPACITY + 2) {
            let (task_id, stream_id) = start_task(
                &state,
                &project_id,
                &format!("T{i}"),
                &format!("wo-starve-{i}"),
                TaskContract::default(),
            );
            let gate = Arc::new(Gate {
                released: Mutex::new(false),
                cv: Condvar::new(),
            });
            seed(&state, &stream_id, Arc::new(GateModel(gate.clone())));
            task_ids.push(task_id.clone());
            gates.push(gate);
            start(&state, &project_id, &task_id);
        }

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while test_support::active_count(&state) < DRIVER_CAPACITY
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(test_support::active_count(&state), DRIVER_CAPACITY);
        let pending = test_support::pending_task_ids(&state);
        assert_eq!(
            pending.len(),
            2,
            "two of the started tasks must be queued for capacity"
        );
        let (stale_id, survivor_id) = (pending[0].clone(), pending[1].clone());

        // The stale one goes terminal while it waits — `task_cancel` on a
        // `Queued` task must still win.
        crate::tasks::task_cancel(
            &state,
            TaskCancelReq {
                project_id: project_id.clone(),
                task_id: stale_id.clone(),
                reason: Some("no longer needed".into()),
            },
        )
        .unwrap();
        assert_eq!(
            task_status(&state, &project_id, &stale_id).0,
            TaskStatus::Cancelled
        );

        // Free exactly one slot.
        let active_idx = task_ids
            .iter()
            .position(|t| test_support::is_active(&state, t))
            .unwrap();
        {
            let mut g = gates[active_idx].released.lock().unwrap();
            *g = true;
        }
        gates[active_idx].cv.notify_all();

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !test_support::is_active(&state, &survivor_id) && std::time::Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            test_support::is_active(&state, &survivor_id),
            "the freed slot must skip the stale entry and promote the one behind it, not sit idle"
        );
        assert_eq!(
            task_status(&state, &project_id, &stale_id).0,
            TaskStatus::Cancelled,
            "the stale entry must stay cancelled, never spawned"
        );

        for g in &gates {
            let mut l = g.released.lock().unwrap();
            *l = true;
            g.cv.notify_all();
        }
        std::thread::sleep(Duration::from_millis(200));
    }

    /// 07d-B Phase 4 item 1 (finding: "deliver only after the turn
    /// succeeds"): a human's steering message must survive a turn refused
    /// because another turn already holds this agent's lease — it must
    /// still be sitting in the inbox afterward, and get delivered for real
    /// on the very next successful step, never silently consumed.
    #[test]
    fn a_message_stays_undelivered_when_the_turn_is_refused_for_a_conflicting_lease() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let project_id = chat_only_project(&state);
        let (task_id, stream_id) = start_task(
            &state,
            &project_id,
            "Research",
            "wo-conflict",
            TaskContract::default(),
        );
        seed(&state, &stream_id, Turns::new(&["handled the steering"]));

        let msg = crate::tasks::task_message(
            &state,
            TaskMessageReq {
                project_id: project_id.clone(),
                task_id: task_id.clone(),
                text: "steer this way".into(),
            },
        )
        .unwrap();

        // Simulate a concurrent human/Baker turn already holding this
        // agent's lease — `agent_turn_inner`'s own admission check refuses
        // the driver's attempt with CONFLICT.
        let turn_guard =
            crate::state::admit_turn(&state, &stream_id, NodeSeat::Orchestrator).unwrap();
        assert!(
            matches!(
                drive_one_step(&state, &project_id, &task_id),
                StepOutcome::Waiting
            ),
            "a lease conflict must wait, not stop or drop the message"
        );

        let project_events = state
            .heart
            .lock()
            .unwrap()
            .read_stream(&project_id)
            .unwrap();
        assert!(
            !project_events.iter().any(|e| e.family == TASK_MESSAGE_DELIVERED_FAMILY),
            "a message must never be marked delivered when the turn that would carry it was refused"
        );

        drop(turn_guard);
        assert!(
            matches!(
                drive_one_step(&state, &project_id, &task_id),
                StepOutcome::IssuedTurn { .. }
            ),
            "the very next step must now succeed"
        );

        let project_events = state
            .heart
            .lock()
            .unwrap()
            .read_stream(&project_id)
            .unwrap();
        let delivered = project_events
            .iter()
            .find(|e| e.family == TASK_MESSAGE_DELIVERED_FAMILY)
            .map(|e| serde_json::from_str::<TaskMessageDelivered>(&e.payload).unwrap())
            .expect("the message must be delivered once a turn actually carries it");
        assert_eq!(delivered.message_hash, msg.event.event_hash);
    }

    /// Same finding, the other refused-turn path: a turn that fails outright
    /// (not a lease conflict) must also leave its message undelivered.
    #[test]
    fn a_message_stays_undelivered_when_the_turn_that_would_carry_it_fails_outright() {
        struct AlwaysErrors;
        impl Model for AlwaysErrors {
            fn name(&self) -> &str {
                "always-errors"
            }
            fn propose_edit(&self, _req: &EditRequest) -> Result<ProposedEdit, String> {
                Err("chat only".into())
            }
            fn supports_chat(&self) -> bool {
                true
            }
            fn chat_turn(
                &self,
                _system: &str,
                _messages: &[ChatMsg],
                _tools: &[ToolDecl],
            ) -> Result<ChatReply, String> {
                Err("the model backend is unavailable".into())
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let project_id = chat_only_project(&state);
        let (task_id, stream_id) = start_task(
            &state,
            &project_id,
            "Research",
            "wo-fail",
            TaskContract::default(),
        );
        seed(&state, &stream_id, Arc::new(AlwaysErrors));

        let msg = crate::tasks::task_message(
            &state,
            TaskMessageReq {
                project_id: project_id.clone(),
                task_id: task_id.clone(),
                text: "please try this".into(),
            },
        )
        .unwrap();

        assert!(matches!(
            drive_one_step(&state, &project_id, &task_id),
            StepOutcome::Stop
        ));
        let (status, reason) = task_status(&state, &project_id, &task_id);
        assert_eq!(status, TaskStatus::Blocked);
        assert!(reason.unwrap().contains("turn failed"));

        let project_events = state
            .heart
            .lock()
            .unwrap()
            .read_stream(&project_id)
            .unwrap();
        assert!(
            !project_events
                .iter()
                .any(|e| e.family == TASK_MESSAGE_DELIVERED_FAMILY),
            "a message must never be marked delivered when the turn that carried it errored"
        );
        let _ = msg; // identity already asserted via absence of ANY delivered fact above
    }

    /// 07d-B Phase 4 (actor honesty): the driver's own decisions — the
    /// opening instruction and the autonomous "continue" — must be
    /// attributed to the driver, never to "human"; a real human steering
    /// message relayed through the same delivery path keeps ITS OWN actor.
    #[test]
    fn driver_issued_turns_are_attributed_to_the_driver_not_the_human() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let project_id = chat_only_project(&state);
        let (task_id, stream_id) = start_task(
            &state,
            &project_id,
            "Research",
            "wo-actor",
            TaskContract::default(),
        );
        seed(
            &state,
            &stream_id,
            Turns::new(&[
                "opening turn done",
                "handled the steering",
                "autonomous again",
            ]),
        );

        drive_one_step(&state, &project_id, &task_id); // opening instruction
        crate::tasks::task_message(
            &state,
            TaskMessageReq {
                project_id: project_id.clone(),
                task_id: task_id.clone(),
                text: "steer this way".into(),
            },
        )
        .unwrap();
        drive_one_step(&state, &project_id, &task_id); // delivers the human message
        drive_one_step(&state, &project_id, &task_id); // autonomous continuation

        let events = state.heart.lock().unwrap().read_stream(&stream_id).unwrap();
        let started: Vec<_> = events
            .iter()
            .filter(|e| e.family == "agent.turn.started")
            .collect();
        assert_eq!(started.len(), 3);
        assert_eq!(
            started[0].actor, DRIVER_ACTOR,
            "the opening contract instruction is the driver's own decision"
        );
        assert_eq!(
            started[1].actor, "human",
            "a relayed human steering message must keep the human as its actor"
        );
        assert_eq!(
            started[2].actor, DRIVER_ACTOR,
            "the autonomous continuation is the driver's own decision"
        );
    }

    /// 07d-B Phase 4 (turn budget honesty): a human steering turn relayed
    /// through the driver must not itself count against `turn_budget` — only
    /// turns the driver decided to issue on its own do.
    #[test]
    fn human_steering_turns_do_not_consume_the_driver_turn_budget() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let project_id = chat_only_project(&state);
        let (task_id, stream_id) = start_task(
            &state,
            &project_id,
            "Research",
            "wo-budget-actor",
            TaskContract::default(),
        );
        seed(
            &state,
            &stream_id,
            Turns::new(&[
                "opening turn done",
                "handled the steering",
                "autonomous again",
            ]),
        );

        drive_one_step(&state, &project_id, &task_id); // opening (driver) — used = 1
        assert_eq!(turns_issued(&state, &stream_id), 1);

        crate::tasks::task_message(
            &state,
            TaskMessageReq {
                project_id: project_id.clone(),
                task_id: task_id.clone(),
                text: "steer this way".into(),
            },
        )
        .unwrap();
        drive_one_step(&state, &project_id, &task_id); // human-relayed — must NOT bump the driver count
        assert_eq!(
            turns_issued(&state, &stream_id),
            1,
            "a human steering turn must not consume the driver's own turn budget"
        );

        drive_one_step(&state, &project_id, &task_id); // autonomous continuation (driver) — used = 2
        assert_eq!(turns_issued(&state, &stream_id), 2);
    }

    /// Direct unit coverage of the race-reconciliation helper itself
    /// (07d-B Phase 4: the window between a turn ending and its `Blocked`
    /// append): an undelivered message already sitting in the inbox at the
    /// moment of the transition must flip it straight back to `Running`
    /// rather than leaving it `Blocked` with nothing left to wake it.
    #[test]
    fn a_message_present_at_a_blocked_transition_resumes_immediately_instead_of_stalling() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let project_id = chat_only_project(&state);
        let (task_id, _stream_id) = start_task(
            &state,
            &project_id,
            "Research",
            "wo-race",
            TaskContract::default(),
        );

        crate::tasks::task_message(
            &state,
            TaskMessageReq {
                project_id: project_id.clone(),
                task_id: task_id.clone(),
                text: "still here".into(),
            },
        )
        .unwrap();

        let resumed = set_blocked_then_check_resume(
            &state,
            &project_id,
            &task_id,
            "asked: something".to_string(),
            None,
        );
        assert!(
            resumed,
            "an undelivered message must reconcile a Blocked transition straight back to Running"
        );
        assert_eq!(
            task_status(&state, &project_id, &task_id).0,
            TaskStatus::Running
        );
    }

    /// The self-trigger guard: the exact message a failed turn already left
    /// undelivered (on purpose, for retry) must not immediately resume the
    /// task itself — that would busy-loop the same failing turn forever.
    #[test]
    fn a_blocked_transition_does_not_self_trigger_on_the_message_it_just_failed_to_deliver() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let project_id = chat_only_project(&state);
        let (task_id, _stream_id) = start_task(
            &state,
            &project_id,
            "Research",
            "wo-race-2",
            TaskContract::default(),
        );

        let msg = crate::tasks::task_message(
            &state,
            TaskMessageReq {
                project_id: project_id.clone(),
                task_id: task_id.clone(),
                text: "will fail".into(),
            },
        )
        .unwrap();

        let resumed = set_blocked_then_check_resume(
            &state,
            &project_id,
            &task_id,
            "turn failed: boom".to_string(),
            Some(&msg.event.event_hash),
        );
        assert!(!resumed, "the message this failed turn already left undelivered must not immediately retrigger it");
        assert_eq!(
            task_status(&state, &project_id, &task_id).0,
            TaskStatus::Blocked
        );
    }

    /// A genuinely NEW message (a different hash) must still trigger the
    /// reconciliation even while an excluded hash is also present — the
    /// exclusion is per-message, not a blanket "never resume."
    #[test]
    fn a_different_message_still_triggers_resume_even_with_an_excluded_hash_present() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let project_id = chat_only_project(&state);
        let (task_id, _stream_id) = start_task(
            &state,
            &project_id,
            "Research",
            "wo-race-3",
            TaskContract::default(),
        );

        let failed_msg = crate::tasks::task_message(
            &state,
            TaskMessageReq {
                project_id: project_id.clone(),
                task_id: task_id.clone(),
                text: "will fail".into(),
            },
        )
        .unwrap();
        // The exclusion target is already delivered (as a failed turn's
        // instruction really would be, in a real driver step) so it is not
        // itself the "next undelivered message" this checks.
        crate::state::append(
            &state,
            &project_id,
            DRIVER_ACTOR,
            TASK_MESSAGE_DELIVERED_FAMILY,
            serde_json::to_string(&TaskMessageDelivered {
                task_id: task_id.clone(),
                message_hash: failed_msg.event.event_hash.clone(),
            })
            .unwrap(),
            vec![],
        )
        .unwrap();
        let new_msg = crate::tasks::task_message(
            &state,
            TaskMessageReq {
                project_id: project_id.clone(),
                task_id: task_id.clone(),
                text: "a fresh instruction".into(),
            },
        )
        .unwrap();

        let resumed = set_blocked_then_check_resume(
            &state,
            &project_id,
            &task_id,
            "turn failed: boom".to_string(),
            Some(&failed_msg.event.event_hash),
        );
        assert!(
            resumed,
            "a different, still-undelivered message must still trigger the resume"
        );
        assert_eq!(
            task_status(&state, &project_id, &task_id).0,
            TaskStatus::Running
        );
        let _ = new_msg;
    }
}
