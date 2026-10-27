//! Durable, per-agent composer controls.
//!
//! A composer is allowed to disappear at any time.  Its follow-ups, a Stop
//! request, and a mid-turn steer therefore cannot live in a websocket or a
//! panel object.  This module records all three as Heart facts and derives
//! the next dispatch from those facts.  The small in-memory dispatcher set is
//! only a duplicate-work guard; losing it on restart is safe because
//! [`recover_on_boot`] rebuilds work from the event log.

use crate::node::Seat;
use crate::routes::agent::{agent_turn_inner_with_control, AgentTurnReq};
use crate::state::{append, AppState};
use bakery_core::Event;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

pub(crate) const FOLLOW_UP_QUEUED: &str = "agent.follow_up.queued";
pub(crate) const STEER_QUEUED: &str = "agent.steer.queued";
pub(crate) const STEER_APPLIED: &str = "agent.steer.applied";
pub(crate) const DISPATCH_PAUSED: &str = "agent.dispatch.paused";
pub(crate) const DISPATCH_RESUMED: &str = "agent.dispatch.resumed";
pub(crate) const OPERATING_MODE_SET: &str = "agent.operating_mode.set";
pub(crate) const QUEUE_FAILED: &str = "agent.queue.failed";
/// Waiting follow-ups folded into one steer for the running turn
/// ([`steer_all_queued`]); its causality consumes every merged item.
pub(crate) const QUEUE_MERGED: &str = "agent.queue.merged";
/// The operator took a waiting message back to edit or drop it
/// ([`withdraw`]); its causality consumes it, so it never sends.
pub(crate) const QUEUE_WITHDRAWN: &str = "agent.queue.withdrawn";

/// The composer capability mode, deliberately separate from the node-wide
/// write gate (`node::WriteMode`).  The mode travels with a queued message so
/// a later picker change cannot silently broaden an instruction already in
/// the durable queue.
#[derive(schemars::JsonSchema, Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum OperatingMode {
    #[default]
    Manual,
    Plan,
    AutoEdit,
    Auto,
}

impl OperatingMode {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Plan => "plan",
            Self::AutoEdit => "auto_edit",
            Self::Auto => "auto",
        }
    }

    pub(crate) fn from_id(value: &str) -> Option<Self> {
        match value {
            "manual" => Some(Self::Manual),
            "plan" => Some(Self::Plan),
            "auto_edit" => Some(Self::AutoEdit),
            "auto" => Some(Self::Auto),
            _ => None,
        }
    }

    /// Capability policy is enforced both by hiding declarations and again
    /// at dispatch time; the latter is the security boundary in case a model
    /// calls a name it was never offered. Plan is structurally read-only.
    /// Auto Edit may change files but cannot launch commands, use the web,
    /// or invoke an operator extension; only Auto receives those broader
    /// capabilities (and their independent global gates).
    pub(crate) fn allows_tool(self, name: &str) -> bool {
        // The Baker's own tools carry a risk grade, and `api_tools::call`
        // gates every call by it and this mode, on every runtime: reads run in
        // any mode, Plan refuses the rest, the others ask first as needed.
        if crate::api_tools::is_tool(name) {
            return true;
        }
        match self {
            Self::Plan => {
                matches!(
                    name,
                    "read_file" | "list_dir" | "glob" | "grep" | "web_fetch" | "web_search"
                )
            }
            Self::AutoEdit => matches!(
                name,
                "read_file"
                    | "list_dir"
                    | "glob"
                    | "grep"
                    | "edit"
                    | "write_file"
                    | "todo_write"
                    | "ask_user"
            ),
            Self::Manual | Self::Auto => true,
        }
    }

    pub(crate) fn blocked_tool_message(self, name: &str) -> String {
        match self {
            Self::Plan => format!("{name} is blocked in Plan mode. Explain findings only; do not propose or make edits."),
            Self::AutoEdit => format!(
                "{name} is blocked in Auto Edit mode. It may edit files, but commands, web access, and operator tools need Auto mode or an explicit human decision."
            ),
            Self::Manual | Self::Auto => format!("{name} is not allowed by this operating mode."),
        }
    }

    pub(crate) fn allows_auto_edit(self) -> bool {
        matches!(self, Self::AutoEdit | Self::Auto)
    }
}

/// Per-turn data that is not part of the legacy `/agent/turn` request.  The
/// public direct-turn route stays source-compatible; queued dispatches carry
/// the durable queue-event causality here, and Plan for a read-only one.
#[derive(Debug, Clone, Default)]
pub(crate) struct TurnControl {
    pub(crate) queue_event_hash: Option<String>,
    pub(crate) mode: Option<OperatingMode>,
    pub(crate) attachment_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct QueuedInstruction {
    agent_id: String,
    seat: Seat,
    instruction: String,
    #[serde(default)]
    file_path: Option<String>,
    #[serde(default)]
    attachment_ids: Vec<String>,
    /// A read-only one (Plan): an Ask that must not change files. Every
    /// other runs on what the selector and mode pill show when it starts.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    read_only: bool,
    /// Who queued this, when it was not the human — e.g.
    /// `agent:<its thread>:baker` for a brief one Baker thread hands another.
    /// Carried into the dispatched turn's `agent.turn.started` actor so an
    /// agent's message never reads as the operator's own words. Absent on
    /// every human entry and on every entry written before this field
    /// existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    actor: Option<String>,
    /// A message another conversation sent (`messages`): who, and which.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    message: Option<crate::messages::MessageNote>,
}

impl QueuedInstruction {
    fn request(&self, stream_id: &str) -> AgentTurnReq {
        AgentTurnReq {
            stream_id: stream_id.to_string(),
            file_path: self.file_path.clone(),
            instruction: self.instruction.clone(),
            seat: Some(self.seat),
            model_id: None,
            effort: None,
            actor: self.actor.clone(),
        }
    }

    /// Its mode: Plan for a read-only one, else what the pill shows now.
    fn mode(&self, state: &AppState, stream_id: &str) -> OperatingMode {
        match self.read_only {
            true => OperatingMode::Plan,
            false => operating_mode_for(state, stream_id, &self.agent_id),
        }
    }
}

#[derive(Debug, Clone)]
struct PendingInstruction {
    event: Event,
    item: QueuedInstruction,
}

#[derive(Debug, Clone)]
pub(crate) struct PendingSteer {
    pub(crate) event_hash: String,
    pub(crate) instruction: String,
    pub(crate) attachment_ids: Vec<String>,
    /// Who queued it, when not the operator.
    pub(crate) actor: Option<String>,
}

fn payload_agent_id(event: &Event) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(&event.payload)
        .ok()?
        .get("agent_id")?
        .as_str()
        .map(str::to_string)
}

/// Whether `event` settles the queued messages its causality names: their
/// turn started, a steer was delivered, or they failed, were merged into a
/// steer, were taken back, or were notices a newer one replaced.
fn consumes(event: &Event) -> bool {
    matches!(
        event.family.as_str(),
        "agent.turn.started" | STEER_APPLIED | QUEUE_FAILED | QUEUE_MERGED | QUEUE_WITHDRAWN | crate::messages::REPLACED
    )
}

fn consumed_hashes(events: &[Event]) -> HashSet<String> {
    events
        .iter()
        .filter(|e| consumes(e))
        .flat_map(|e| e.causality.iter().cloned())
        .collect()
}

fn pending_for_agent(state: &AppState, stream_id: &str, agent_id: &str) -> Vec<PendingInstruction> {
    let events = state
        .heart
        .lock()
        .unwrap()
        .read_stream(stream_id)
        .unwrap_or_default();
    let consumed = consumed_hashes(&events);
    events
        .into_iter()
        .filter(|e| {
            matches!(e.family.as_str(), FOLLOW_UP_QUEUED | STEER_QUEUED)
                && !consumed.contains(&e.event_hash)
        })
        .filter_map(|event| {
            let item = serde_json::from_str::<QueuedInstruction>(&event.payload).ok()?;
            (item.agent_id == agent_id).then_some(PendingInstruction { event, item })
        })
        .collect()
}

/// Every decision on a queued message's fate (its turn starting, a steer
/// delivered or merged, a withdraw) is taken under this lock after checking
/// it is still waiting, so a withdraw and a delivery never both happen. It
/// holds the steers on their way into a native turn right now ([`SteerClaim`]).
fn decisions() -> &'static std::sync::Mutex<HashSet<String>> {
    static DECISIONS: std::sync::OnceLock<std::sync::Mutex<HashSet<String>>> = std::sync::OnceLock::new();
    DECISIONS.get_or_init(Default::default)
}

fn waiting(state: &AppState, stream_id: &str, hash: &str) -> bool {
    let events = state.heart.lock().unwrap().read_stream(stream_id).unwrap_or_default();
    events.iter().any(|e| e.event_hash == hash) && !consumed_hashes(&events).contains(hash)
}

/// Runs `consume`, the append that consumes queued message `hash`, only while
/// it is still waiting: `None` when it was withdrawn (or sent) first.
pub(crate) fn consume_if_waiting<T>(
    state: &AppState,
    stream_id: &str,
    hash: Option<&str>,
    consume: impl FnOnce() -> T,
) -> Option<T> {
    let Some(hash) = hash else { return Some(consume()) };
    let _decisions = decisions().lock().unwrap();
    waiting(state, stream_id, hash).then(consume)
}

/// A steer on its way into a running native turn: past withdrawing. Let go
/// when dropped, whether the runtime took it or not.
pub(crate) struct SteerClaim(String);

impl Drop for SteerClaim {
    fn drop(&mut self) {
        decisions().lock().unwrap().remove(&self.0);
    }
}

/// [`peek_next_steer`], claimed for delivery.
pub(crate) fn claim_next_steer(state: &AppState, stream_id: &str, seat: Seat) -> Option<(PendingSteer, SteerClaim)> {
    let mut claimed = decisions().lock().unwrap();
    let steer = peek_next_steer(state, stream_id, seat)?;
    claimed.insert(steer.event_hash.clone());
    let claim = SteerClaim(steer.event_hash.clone());
    Some((steer, claim))
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct Withdrawn {
    pub(crate) instruction: String,
    pub(crate) file_path: Option<String>,
    pub(crate) attachment_ids: Vec<String>,
}

/// Why `withdraw` refused: no such waiting message (404), or it is past
/// taking back (409).
pub(crate) enum WithdrawRefused {
    Unknown,
    Gone(&'static str),
}

/// The operator takes back a message still waiting, a follow-up or a steer
/// not yet delivered, to edit or drop it: "i have until it gets sent to hit
/// the up arrow". Its withdrawn fact consumes it, and its text comes back.
pub(crate) fn withdraw(state: &AppState, stream_id: &str, hash: &str) -> Result<Withdrawn, WithdrawRefused> {
    let claimed = decisions().lock().unwrap();
    if claimed.contains(hash) {
        return Err(WithdrawRefused::Gone("it is being delivered right now"));
    }
    let events = state.heart.lock().unwrap().read_stream(stream_id).unwrap_or_default();
    let queued = events
        .iter()
        .find(|e| e.event_hash == hash && matches!(e.family.as_str(), FOLLOW_UP_QUEUED | STEER_QUEUED))
        .ok_or(WithdrawRefused::Unknown)?;
    let item: QueuedInstruction = serde_json::from_str(&queued.payload).map_err(|_| WithdrawRefused::Unknown)?;
    let consumer = |e: &&Event| consumes(e) && e.causality.iter().any(|c| c == hash);
    if let Some(after) = events.iter().find(consumer) {
        return Err(WithdrawRefused::Gone(match after.family.as_str() {
            QUEUE_MERGED => "it was already merged into a steer",
            QUEUE_WITHDRAWN => "it was already taken back",
            QUEUE_FAILED => "it already failed to send",
            crate::messages::REPLACED => "a newer notice replaced it",
            _ => "it was already sent",
        }));
    }
    let payload = serde_json::json!({ "agent_id": item.agent_id, "seat": item.seat });
    append(state, stream_id, "human", QUEUE_WITHDRAWN, payload.to_string(), vec![hash.to_string()])
        .map_err(|_| WithdrawRefused::Gone("its withdrawal could not be recorded"))?;
    drop(claimed);
    Ok(Withdrawn { instruction: item.instruction, file_path: item.file_path, attachment_ids: item.attachment_ids })
}

/// Instructions queued for this seat that have not started yet.
pub(crate) fn pending_count(state: &AppState, stream_id: &str, seat: Seat) -> usize {
    pending_for_agent(state, stream_id, &crate::node::agent_id(stream_id, seat)).len()
}

/// Current pause state: a restart or this Mac's hold, or Stop
/// ([`stopped`]).
pub(crate) fn dispatch_is_paused(state: &AppState, stream_id: &str, agent_id: &str) -> bool {
    // A restart waits for running turns: queued messages wait for the node
    // that comes back.
    held(state) || stopped(state, stream_id, agent_id)
}

/// Whether Stop paused this agent's queue, projected only from durable
/// events. The last event for this exact agent wins; other seats on the
/// same socket remain live.
pub(crate) fn stopped(state: &AppState, stream_id: &str, agent_id: &str) -> bool {
    state
        .heart
        .lock()
        .unwrap()
        .read_stream(stream_id)
        .unwrap_or_default()
        .iter()
        .rev()
        .find_map(|event| {
            if !matches!(event.family.as_str(), DISPATCH_PAUSED | DISPATCH_RESUMED) {
                return None;
            }
            (payload_agent_id(event).as_deref() == Some(agent_id))
                .then_some(event.family == DISPATCH_PAUSED)
        })
        .unwrap_or(false)
}

/// Latest explicitly selected durable operating mode for one agent. `None`
/// means this agent predates the composer mode picker, so legacy direct turns
/// must retain the node-wide write-mode behavior they had before the picker
/// existed.
pub(crate) fn configured_operating_mode(
    state: &AppState,
    stream_id: &str,
    agent_id: &str,
) -> Option<OperatingMode> {
    state
        .heart
        .lock()
        .unwrap()
        .read_stream(stream_id)
        .unwrap_or_default()
        .iter()
        .rev()
        .find_map(|event| {
            if event.family != OPERATING_MODE_SET
                || payload_agent_id(event).as_deref() != Some(agent_id)
            {
                return None;
            }
            serde_json::from_str::<serde_json::Value>(&event.payload)
                .ok()?
                .get("mode")
                .cloned()
                .and_then(|v| serde_json::from_value(v).ok())
        })
}

/// Latest durable operating mode for one agent. This is intentionally an
/// event projection, not a pane preference: a reopened pane shows the same
/// choice, while callers that need legacy compatibility can distinguish an
/// absent choice through [`configured_operating_mode`].
pub(crate) fn operating_mode_for(
    state: &AppState,
    stream_id: &str,
    agent_id: &str,
) -> OperatingMode {
    configured_operating_mode(state, stream_id, agent_id).unwrap_or_default()
}

pub(crate) fn set_operating_mode(
    state: &AppState,
    stream_id: &str,
    seat: Seat,
    mode: OperatingMode,
) -> Result<Event, String> {
    let agent_id = crate::node::agent_id(stream_id, seat);
    append(
        state,
        stream_id,
        "human",
        OPERATING_MODE_SET,
        serde_json::json!({ "agent_id": agent_id, "seat": seat, "mode": mode }).to_string(),
        vec![],
    )
}

pub(crate) fn enqueue(
    state: &AppState,
    req: AgentTurnReq,
    attachment_ids: Vec<String>,
    steer: bool,
) -> Result<Event, String> {
    enqueue_as(state, req, attachment_ids, steer, "human")
}

/// [`enqueue`] on behalf of `actor` — the same durable queue, pause, and
/// recovery semantics; only the recorded initiator differs.
pub(crate) fn enqueue_as(
    state: &AppState,
    req: AgentTurnReq,
    attachment_ids: Vec<String>,
    steer: bool,
    actor: &str,
) -> Result<Event, String> {
    enqueue_in(state, req, attachment_ids, steer, actor, false)
}

/// Queue the Bakery's own message to a conversation's default seat (the
/// Baker's, or its orchestrator's); it runs once the agent is idle.
pub(crate) fn tell(state: &AppState, stream_id: &str, instruction: String) -> Result<(), String> {
    let seat = if crate::baker_ops::is_baker_scope_stream(state, stream_id) {
        crate::node::Seat::Baker
    } else {
        crate::node::Seat::Orchestrator
    };
    enqueue_as(
        state,
        AgentTurnReq {
            stream_id: stream_id.to_string(),
            file_path: None,
            instruction,
            seat: Some(seat),
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

/// [`enqueue_as`], read-only (Plan, a read-only Ask) when `read_only`.
pub(crate) fn enqueue_in(
    state: &AppState,
    req: AgentTurnReq,
    attachment_ids: Vec<String>,
    steer: bool,
    actor: &str,
    read_only: bool,
) -> Result<Event, String> {
    enqueue_with(state, req, attachment_ids, steer, actor, read_only, None)
}

/// Settles the notices a newer one replaces on `stream_id`'s `seat`
/// (`messages::REPLACED`): the unread notes given, and every message still
/// waiting there that `stale` picks, decided under the queue's lock so none
/// of them starts its turn meanwhile. Nothing when there are none.
pub(crate) fn replace_waiting(
    state: &AppState,
    stream_id: &str,
    seat: Seat,
    notes: Vec<String>,
    stale: impl Fn(&crate::messages::MessageNote) -> bool,
    by: &str,
) -> Result<(), String> {
    let agent_id = crate::node::agent_id(stream_id, seat);
    let _decisions = decisions().lock().unwrap();
    let mut replaced = notes;
    replaced.extend(
        pending_for_agent(state, stream_id, &agent_id)
            .into_iter()
            .filter(|p| p.event.family == FOLLOW_UP_QUEUED && p.item.message.as_ref().is_some_and(&stale))
            .map(|p| p.event.event_hash),
    );
    if replaced.is_empty() {
        return Ok(());
    }
    let fact = serde_json::json!({ "agent_id": agent_id, "seat": seat, "by": by });
    append(state, stream_id, "node", crate::messages::REPLACED, fact.to_string(), replaced).map(drop)
}

/// Runs `then` only while a queued message that `waits` matches is still
/// waiting for `stream_id`'s `seat`. It runs under the hold every queued
/// turn's start is decided under ([`decisions`]), so the turn that takes that
/// message up starts after `then`. `None` when none waits.
pub(crate) fn while_waiting<T>(
    state: &AppState,
    stream_id: &str,
    seat: Seat,
    waits: impl Fn(&crate::messages::MessageNote) -> bool,
    then: impl FnOnce() -> T,
) -> Option<T> {
    let agent_id = crate::node::agent_id(stream_id, seat);
    let _decisions = decisions().lock().unwrap();
    pending_for_agent(state, stream_id, &agent_id)
        .iter()
        .any(|p| p.event.family == FOLLOW_UP_QUEUED && p.item.message.as_ref().is_some_and(&waits))
        .then(then)
}

/// Queue another conversation's message (`messages`) on `req`'s agent, from
/// `actor`, the sender.
pub(crate) fn enqueue_message(
    state: &AppState,
    req: AgentTurnReq,
    actor: &str,
    message: crate::messages::MessageNote,
) -> Result<Event, String> {
    enqueue_with(state, req, vec![], false, actor, false, Some(message))
}

fn enqueue_with(
    state: &AppState,
    req: AgentTurnReq,
    attachment_ids: Vec<String>,
    steer: bool,
    actor: &str,
    read_only: bool,
    message: Option<crate::messages::MessageNote>,
) -> Result<Event, String> {
    // Queued inside an agent's route call, the turn it starts is that agent's.
    let actor = crate::actor::attribute(actor);
    let actor = actor.as_str();
    let seat = req
        .seat
        .expect("queue routes resolve a seat before enqueueing");
    let agent_id = crate::node::agent_id(&req.stream_id, seat);
    let payload = QueuedInstruction {
        agent_id: agent_id.clone(),
        seat,
        instruction: req.instruction,
        file_path: req.file_path,
        attachment_ids,
        read_only,
        actor: (actor != "human").then(|| actor.to_string()),
        message,
    };
    let event = append(
        state,
        &req.stream_id,
        actor,
        if steer {
            STEER_QUEUED
        } else {
            FOLLOW_UP_QUEUED
        },
        serde_json::to_string(&payload).map_err(|e| e.to_string())?,
        vec![],
    )?;
    // A steer is delivered by the active loop at a safe boundary.  If no
    // loop is active, the dispatcher treats it as the next normal turn so a
    // user never loses a steer typed just as a turn ended.  Sending one is
    // also the go-ahead for a paused queue, so a steer never waits on Stop.
    let paused = dispatch_is_paused(state, &req.stream_id, &agent_id);
    if steer && paused {
        resume_dispatch(state, &req.stream_id, seat)?;
    } else if !paused {
        schedule_dispatch(state, &req.stream_id, seat);
    }
    Ok(event)
}

/// How long a message to an idle agent waits for its turn to begin before
/// its send answers with the queued fact alone. The turn then starts on its
/// own, as any queued one does; the wait only keeps an idle agent's message
/// from ever reading as waiting.
const START_WAIT: std::time::Duration = std::time::Duration::from_secs(3);

/// What a composer send came to: the queued message, and, when the agent was
/// idle and its turn began within [`START_WAIT`], that turn's own
/// `agent.turn.started`. On the wire it is the queued event itself, with
/// `started` beside its fields when there is one.
#[derive(Debug, Serialize)]
pub(crate) struct Queued {
    #[serde(flatten)]
    pub(crate) event: Event,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) started: Option<Event>,
}

/// Whether a message queued to this agent now would run next and at once:
/// no turn running, the queue neither paused nor held (a Stop, a restart),
/// nothing waiting ahead of it and, in Manual, no change from the agent's
/// last turn still up for review.
fn starts_at_once(state: &AppState, stream_id: &str, seat: Seat, agent_id: &str, mode: OperatingMode) -> bool {
    let running = state
        .lifecycle
        .lock()
        .unwrap()
        .running_turns
        .get(&(stream_id.to_string(), seat))
        .is_some_and(|&count| count > 0);
    !running
        && !dispatch_is_paused(state, stream_id, agent_id)
        && pending_for_agent(state, stream_id, agent_id).is_empty()
        && (mode != OperatingMode::Manual || crate::snapshots::waiting_for_review(state, stream_id, agent_id).is_none())
}

/// How often a send that waits for its turn looks at what changes only in
/// memory, between the appends that wake it.
const START_TICK: std::time::Duration = std::time::Duration::from_millis(50);

/// Whether a node restart or this Mac's hold keeps queued messages from
/// starting (see [`dispatch_is_paused`], which also reads Stop from the stream).
fn held(state: &AppState) -> bool {
    crate::service::restarting(state) || crate::this_mac::holding()
}

/// The `agent.turn.started` that consumes queued message `queued`, once it is
/// appended. `None` as soon as it is known not to come now: the message is
/// settled some other way (it failed, was merged or was taken back), another
/// turn took the agent first, Stop paused its queue or a hold came on, or the
/// dispatcher is gone with the message still waiting (its admission was
/// refused); and `None` when `wait` runs out. Woken by the appends
/// themselves; between them it looks only at what changes in memory, a few
/// times a second.
async fn turn_started_for(
    state: &AppState,
    events: &mut tokio::sync::broadcast::Receiver<String>,
    stream_id: &str,
    agent_id: &str,
    queued: &str,
    wait: std::time::Duration,
) -> Option<Event> {
    use tokio::sync::broadcast::error::RecvError;
    // What one append says of the message: its turn started (`Some(Some)`),
    // it will not start now (`Some(None)`), or nothing about it (`None`).
    let verdict = |e: &Event| -> Option<Option<Event>> {
        if e.stream_id != stream_id {
            return None;
        }
        if consumes(e) && e.causality.iter().any(|c| c == queued) {
            return Some((e.family == "agent.turn.started").then(|| e.clone()));
        }
        // Another turn took the agent first, or Stop paused its queue.
        let takes_it = matches!(e.family.as_str(), "agent.turn.started" | DISPATCH_PAUSED)
            && payload_agent_id(e).as_deref() == Some(agent_id);
        takes_it.then_some(None)
    };
    // The same, read from the stream, for what came after the message.
    let in_stream = || -> Option<Option<Event>> {
        let all = state.heart.lock().unwrap().read_stream(stream_id).unwrap_or_default();
        all.iter().skip_while(|e| e.event_hash != queued).skip(1).find_map(|e| verdict(e))
    };
    let deadline = tokio::time::Instant::now() + wait;
    // Since when the agent has had no dispatcher.
    let mut gone_since: Option<tokio::time::Instant> = None;
    loop {
        let tick = (tokio::time::Instant::now() + START_TICK).min(deadline);
        match tokio::time::timeout_at(tick, events.recv()).await {
            Ok(Err(RecvError::Closed)) => return None,
            // Missed some events: look at the stream itself.
            Ok(Err(RecvError::Lagged(_))) => {
                if let Some(v) = in_stream() {
                    return v;
                }
            }
            // An activity envelope is no event: skipped.
            Ok(Ok(raw)) => {
                if let Some(v) = serde_json::from_str::<Event>(&raw).ok().and_then(|e| verdict(&e)) {
                    return v;
                }
            }
            Err(_) => {}
        }
        let now = tokio::time::Instant::now();
        if now >= deadline || held(state) {
            return None;
        }
        // A dispatcher that is gone while the message still waits will not
        // start it: its admission was refused, or the queue stopped under it.
        // Only an absence that lasts counts; a dispatcher that re-arms itself
        // is gone for an instant.
        if state.queue_dispatchers.lock().unwrap().contains(agent_id) {
            gone_since = None;
        } else if now.duration_since(*gone_since.get_or_insert(now)) >= START_TICK * 2 {
            return in_stream().flatten();
        }
    }
}

/// [`enqueue_in`] for the composer. A message to an idle agent answers once
/// its turn has begun, with that turn's `agent.turn.started` beside the
/// queued fact, so it lands in the thread and never reads as waiting. Every
/// other send (a steer, a running turn, a paused or held queue, a change
/// waiting for review, messages ahead of it) answers as soon as it is queued,
/// and so does one that turns out not to be first (another turn started, a
/// Stop, a hold).
pub(crate) async fn enqueue_awaiting_start(
    state: &AppState,
    req: AgentTurnReq,
    attachment_ids: Vec<String>,
    steer: bool,
    actor: &str,
    read_only: bool,
) -> Result<Queued, String> {
    enqueue_awaiting_start_for(state, req, attachment_ids, steer, actor, read_only, START_WAIT).await
}

/// [`enqueue_awaiting_start`] with its longest wait given: a test under load
/// needs far longer than a person at the composer does.
async fn enqueue_awaiting_start_for(
    state: &AppState,
    req: AgentTurnReq,
    attachment_ids: Vec<String>,
    steer: bool,
    actor: &str,
    read_only: bool,
    wait: std::time::Duration,
) -> Result<Queued, String> {
    let stream_id = req.stream_id.clone();
    let seat = req
        .seat
        .expect("queue routes resolve a seat before enqueueing");
    let agent_id = crate::node::agent_id(&stream_id, seat);
    let mode = match read_only {
        true => OperatingMode::Plan,
        false => operating_mode_for(state, &stream_id, &agent_id),
    };
    // Judged before the append, since the queued message's own dispatch can
    // start the turn and make an idle agent look busy; subscribed before it,
    // so the turn's start cannot slip by in between.
    let mut events = (!steer && starts_at_once(state, &stream_id, seat, &agent_id, mode))
        .then(|| state.tx.subscribe());
    let event = enqueue_in(state, req, attachment_ids, steer, actor, read_only)?;
    let started = match events.as_mut() {
        Some(events) => turn_started_for(state, events, &stream_id, &agent_id, &event.event_hash, wait).await,
        None => None,
    };
    Ok(Queued { event, started })
}

/// The operator's "steer with all of them now": every follow-up waiting for
/// one agent becomes ONE steer for its running turn, joined in submission
/// order. The merge fact's causality consumes every original, so exactly-once
/// holds — none can still dispatch as its own follow-up — and the fact itself
/// records the combined text, so nothing is lost if the steer write fails.
///
/// Only while a turn is running, decided under the lifecycle lock the
/// dispatcher's own admission takes: with a turn live, the dispatcher cannot
/// start one of these follow-ups underneath the merge. Attachments travel
/// with the steer; a follow-up's selected file is named in the steer's text,
/// since a running turn's own file context is already set.
pub(crate) fn steer_all_queued(
    state: &AppState,
    stream_id: &str,
    seat: Seat,
) -> Result<Event, String> {
    steer_queued(state, stream_id, seat, None)
}

/// "Steer with this one now": ONE waiting follow-up becomes a steer for the
/// running turn. The same merge fact as [`steer_all_queued`] consumes it, so
/// it cannot also dispatch as its own follow-up, and every other queued
/// message keeps its place. Same refusals, plus a message that is no longer
/// waiting.
pub(crate) fn steer_queued_item(
    state: &AppState,
    stream_id: &str,
    seat: Seat,
    event_hash: &str,
) -> Result<Event, String> {
    steer_queued(state, stream_id, seat, Some(event_hash))
}

/// `only`: steer just that queued follow-up instead of every waiting one.
fn steer_queued(
    state: &AppState,
    stream_id: &str,
    seat: Seat,
    only: Option<&str>,
) -> Result<Event, String> {
    let agent_id = crate::node::agent_id(stream_id, seat);
    let (combined, attachment_ids) = {
        let lifecycle = state.lifecycle.lock().unwrap();
        let running = lifecycle
            .running_turns
            .get(&(stream_id.to_string(), seat))
            .is_some_and(|&count| count > 0);
        if !running {
            return Err(
                "no turn is running for this agent — queued messages send on their own once it is idle"
                    .to_string(),
            );
        }
        let _decisions = decisions().lock().unwrap();
        let follow_ups: Vec<PendingInstruction> = pending_for_agent(state, stream_id, &agent_id)
            .into_iter()
            .filter(|p| p.event.family == FOLLOW_UP_QUEUED)
            .filter(|p| only.is_none_or(|hash| p.event.event_hash == hash))
            .collect();
        if follow_ups.is_empty() {
            return Err(match only {
                Some(_) => "that message is no longer waiting in the queue".to_string(),
                None => "nothing is queued for this agent".to_string(),
            });
        }
        let attachment_ids: Vec<String> = follow_ups
            .iter()
            .flat_map(|p| p.item.attachment_ids.iter().cloned())
            .collect();
        let combined = follow_ups
            .iter()
            .map(|p| {
                let text = p.item.instruction.trim();
                match p.item.file_path.as_deref().map(str::trim).filter(|f| !f.is_empty()) {
                    Some(file) if text.is_empty() => format!("(about {file})"),
                    Some(file) => format!("{text}\n(about {file})"),
                    None => text.to_string(),
                }
            })
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");
        append(
            state,
            stream_id,
            "human",
            QUEUE_MERGED,
            serde_json::json!({
                "agent_id": agent_id,
                "seat": seat,
                "into": "steer",
                "count": follow_ups.len(),
                "instruction": combined,
                "attachment_ids": attachment_ids,
            })
            .to_string(),
            follow_ups
                .iter()
                .map(|p| p.event.event_hash.clone())
                .collect(),
        )?;
        (combined, attachment_ids)
    };
    enqueue(
        state,
        AgentTurnReq {
            stream_id: stream_id.to_string(),
            file_path: None,
            instruction: combined,
            seat: Some(seat),
            model_id: None,
            effort: None,
            actor: None,
        },
        attachment_ids,
        true,
    )
}

pub(crate) fn pause_dispatch(
    state: &AppState,
    stream_id: &str,
    seat: Seat,
) -> Result<Event, String> {
    let agent_id = crate::node::agent_id(stream_id, seat);
    append(
        state,
        stream_id,
        "human",
        DISPATCH_PAUSED,
        serde_json::json!({ "agent_id": agent_id, "seat": seat }).to_string(),
        vec![],
    )
}

pub(crate) fn resume_dispatch(
    state: &AppState,
    stream_id: &str,
    seat: Seat,
) -> Result<Event, String> {
    let agent_id = crate::node::agent_id(stream_id, seat);
    let event = append(
        state,
        stream_id,
        "human",
        DISPATCH_RESUMED,
        serde_json::json!({ "agent_id": agent_id, "seat": seat }).to_string(),
        vec![],
    )?;
    schedule_dispatch(state, stream_id, seat);
    Ok(event)
}

/// Called only at an HTTP turn's model/tool safe boundaries.  It never aborts
/// a tool that has already been handed to the OS; it consumes the oldest
/// durable steer, records that causality, and returns its text plus its
/// attachments resolved exactly as a normal turn's are (`is_native`: what
/// this model reads as itself). Every attachment resolves — a file the model
/// cannot open becomes a line naming it — so no steer is ever held back.
pub(crate) fn take_next_steer(
    state: &AppState,
    stream_id: &str,
    seat: Seat,
    turn_id: &str,
    is_native: &dyn Fn(&str) -> bool,
    inline: usize,
) -> Result<Option<(String, crate::attachments::TurnAttachments)>, String> {
    let next = {
        let _decisions = decisions().lock().unwrap();
        let Some(next) = peek_next_steer(state, stream_id, seat) else {
            return Ok(None);
        };
        mark_steer_applied(state, stream_id, seat, turn_id, &next)?;
        next
    };
    let agent_id = crate::node::agent_id(stream_id, seat);
    let attachments = crate::attachments::turn_attachments(
        state,
        stream_id,
        &agent_id,
        &next.attachment_ids,
        is_native,
        inline,
    );
    Ok(Some((next.instruction, attachments)))
}

/// Inspect, but do not consume, the oldest steer for an active native turn.
/// ACP's `_session/steering` can answer `promptRequired`; retaining the
/// queue event until an `injected` acknowledgement is what makes that race
/// lossless.
pub(crate) fn peek_next_steer(
    state: &AppState,
    stream_id: &str,
    seat: Seat,
) -> Option<PendingSteer> {
    let agent_id = crate::node::agent_id(stream_id, seat);
    if dispatch_is_paused(state, stream_id, &agent_id) {
        return None;
    }
    pending_for_agent(state, stream_id, &agent_id)
        .into_iter()
        .find(|p| p.event.family == STEER_QUEUED)
        .map(|pending| PendingSteer {
            event_hash: pending.event.event_hash,
            instruction: pending.item.instruction,
            attachment_ids: pending.item.attachment_ids,
            actor: pending.item.actor,
        })
}

pub(crate) fn mark_steer_applied(
    state: &AppState,
    stream_id: &str,
    seat: Seat,
    turn_id: &str,
    steer: &PendingSteer,
) -> Result<Event, String> {
    let agent_id = crate::node::agent_id(stream_id, seat);
    append(
        state,
        stream_id,
        steer.actor.as_deref().unwrap_or("human"),
        STEER_APPLIED,
        serde_json::json!({
            "agent_id": agent_id,
            "seat": seat,
            "turn_id": turn_id,
            "instruction": steer.instruction,
            "attachment_ids": steer.attachment_ids,
        })
        .to_string(),
        vec![steer.event_hash.clone()],
    )
}

/// Whether the oldest message waiting for this agent is there and is not held
/// back by a review: a Manual turn does not start while changes from the
/// agent's last turn are still up for review (`turn_holding` refuses it the
/// same way).
fn next_can_start(state: &AppState, stream_id: &str, agent_id: &str) -> bool {
    pending_for_agent(state, stream_id, agent_id).first().is_some_and(|next| {
        next.item.mode(state, stream_id) != OperatingMode::Manual
            || crate::snapshots::waiting_for_review(state, stream_id, agent_id).is_none()
    })
}

/// Starts at most one short-lived dispatcher per agent.  The dispatcher
/// holds no queue state itself: every iteration re-derives the oldest
/// unconsumed item from Heart, then `agent.turn.started` consumes it via a
/// causal edge.  A process crash therefore leaves the item pending, rather
/// than silently dropping it.
pub(crate) fn schedule_dispatch(state: &AppState, stream_id: &str, seat: Seat) {
    let agent_id = crate::node::agent_id(stream_id, seat);
    {
        let mut active = state.queue_dispatchers.lock().unwrap();
        if !active.insert(agent_id.clone()) {
            return;
        }
    }
    let state = state.clone();
    let stream_id = stream_id.to_string();
    std::thread::spawn(move || {
        loop {
            if dispatch_is_paused(&state, &stream_id, &agent_id) {
                break;
            }
            let Some(next) = pending_for_agent(&state, &stream_id, &agent_id)
                .into_iter()
                .next()
            else {
                break;
            };
            let request = next.item.request(&stream_id);
            let control = TurnControl {
                queue_event_hash: Some(next.event.event_hash.clone()),
                mode: next.item.read_only.then_some(OperatingMode::Plan),
                attachment_ids: next.item.attachment_ids.clone(),
            };
            match agent_turn_inner_with_control(&state, &request, control) {
                Ok(_) => continue,
                // An existing direct turn owns this agent.  Its `TurnLease`
                // Drop schedules us again; leave the durable item untouched.
                Err((axum::http::StatusCode::CONFLICT, _)) => break,
                Err((status, reason)) => {
                    // The event remains auditable but is marked consumed so
                    // one invalid queued instruction cannot spin forever and
                    // starve every later message.
                    let _ = append(
                        &state,
                        &stream_id,
                        "node",
                        QUEUE_FAILED,
                        serde_json::json!({
                            "agent_id": agent_id,
                            "seat": seat,
                            "status": status.as_u16(),
                            "reason": reason,
                        })
                        .to_string(),
                        vec![next.event.event_hash],
                    );
                }
            }
        }
        state.queue_dispatchers.lock().unwrap().remove(&agent_id);
        // Close the check/remove race: a message may have arrived after the
        // final derive but before the guard was released. Not while a turn is
        // running: the item that just met CONFLICT is still pending, so
        // re-arming here would conflict again in a tight loop for as long as
        // that turn (or the human decision it is parked on) lasts. Its
        // `TurnLease` Drop wakes the dispatcher once it ends. Nor while the
        // next message waits for a review (`next_can_start`): the same
        // refusal, for as long as the review lasts, and deciding the last
        // change wakes the queue (`snapshots::decided`).
        let turn_running = state
            .lifecycle
            .lock()
            .unwrap()
            .running_turns
            .get(&(stream_id.clone(), seat))
            .is_some_and(|&count| count > 0);
        if !turn_running
            && !dispatch_is_paused(&state, &stream_id, &agent_id)
            && next_can_start(&state, &stream_id, &agent_id)
        {
            schedule_dispatch(&state, &stream_id, seat);
        } else {
            // Every turn's end, direct or queued, passes here once its queue
            // is drained: a Baker thread's batch of work is done, so its
            // acceptance checks run now (once it is idle; `prove` decides),
            // and a thread an agent opened tells it (`tell_spawner` decides).
            if seat == Seat::Baker {
                if let Err(e) = crate::baker_threads::prove(&state, &stream_id) {
                    eprintln!("bakery-node: {stream_id}'s acceptance checks did not start: {e}");
                }
            }
            crate::messages::tell_spawner(&state, &stream_id, seat);
        }
    });
}

/// Rebuild dispatch work after node startup.  `queue_dispatchers` starts
/// empty, so only durable events participate in this recovery. Agents in
/// `resuming` are carrying a cut-off turn on (`resume::on_boot`), which
/// dispatches their queue once it has.
pub(crate) fn recover_on_boot(state: &AppState, resuming: &std::collections::HashSet<String>) {
    let stream_ids: Vec<String> = state
        .heart
        .lock()
        .unwrap()
        .list_streams()
        .unwrap_or_default()
        .into_iter()
        .map(|s| s.stream_id)
        .collect();
    for stream_id in stream_ids {
        let events = state
            .heart
            .lock()
            .unwrap()
            .read_stream(&stream_id)
            .unwrap_or_default();
        let consumed = consumed_hashes(&events);
        let targets: Vec<Seat> = events
            .iter()
            .filter(|e| matches!(e.family.as_str(), FOLLOW_UP_QUEUED | STEER_QUEUED))
            .filter(|e| !consumed.contains(&e.event_hash))
            .filter_map(|e| {
                serde_json::from_str::<QueuedInstruction>(&e.payload)
                    .ok()
                    .map(|q| q.seat)
            })
            .collect();
        for seat in targets {
            let agent_id = crate::node::agent_id(&stream_id, seat);
            if !resuming.contains(&agent_id) && !dispatch_is_paused(state, &stream_id, &agent_id) {
                schedule_dispatch(state, &stream_id, seat);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Baker's tools stay offered in every mode: their own gate
    /// (`api_tools::gate`) refuses or asks by grade, so Plan still reads and
    /// a write in Auto Edit asks instead of vanishing.
    #[test]
    fn the_bakers_tools_are_offered_in_every_mode_and_gated_by_grade() {
        for mode in [
            OperatingMode::Manual,
            OperatingMode::Plan,
            OperatingMode::AutoEdit,
            OperatingMode::Auto,
        ] {
            for op in crate::baker_ops::BAKER_OPS {
                assert!(mode.allows_tool(op.wire), "{} in {mode:?}", op.wire);
            }
            assert!(mode.allows_tool(crate::api_tools::CALL), "{mode:?}");
        }
        assert!(!OperatingMode::Plan.allows_tool("run"), "core tools keep their own rule");
    }
    use crate::routes::project::open_stream;
    use crate::testutil::{make_repo, open_req, state_with, StubModel};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    fn queue_request(stream_id: &str, instruction: &str) -> AgentTurnReq {
        AgentTurnReq {
            stream_id: stream_id.to_string(),
            file_path: None,
            instruction: instruction.to_string(),
            seat: Some(Seat::Orchestrator),
            model_id: None,
            effort: None,
            actor: None,
        }
    }

    fn started_count(state: &AppState, stream_id: &str, queued_hash: &str) -> usize {
        state
            .heart
            .lock()
            .unwrap()
            .read_stream(stream_id)
            .unwrap()
            .iter()
            .filter(|event| {
                event.family == "agent.turn.started"
                    && event.causality.iter().any(|hash| hash == queued_hash)
            })
            .count()
    }

    fn wait_for_started(state: &AppState, stream_id: &str, queued_hash: &str) {
        // A dispatch is its own thread: under a full suite's load it can take
        // seconds to be scheduled at all.
        let deadline = Instant::now() + Duration::from_secs(60);
        while Instant::now() < deadline {
            if started_count(state, stream_id, queued_hash) == 1 {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("queued instruction {queued_hash} was not dispatched");
    }

    /// A message queued while a turn is running waits for that turn's own
    /// `TurnLease` Drop. The dispatcher meets CONFLICT and yields; re-arming
    /// itself on the spot would conflict again in a tight loop, re-reading the
    /// whole stream on every pass, for as long as the turn — or the human
    /// decision it is parked on — lasts.
    #[test]
    fn a_dispatcher_yields_to_a_running_turn_instead_of_re_arming() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = open_stream(&state, &open_req(&repo, "queue-yield"))
            .unwrap()
            .context
            .stream_id;
        let seat = Seat::Orchestrator;
        let agent_id = crate::node::agent_id(&stream_id, seat);
        state
            .lifecycle
            .lock()
            .unwrap()
            .running_turns
            .insert((stream_id.clone(), seat), 1);
        let queued = enqueue(
            &state,
            queue_request(&stream_id, "wait for the running turn"),
            vec![],
            false,
        )
        .unwrap();

        // Let the first dispatcher meet CONFLICT and finish (its guard, taken
        // as it was scheduled, goes when it ends; under load that can take
        // seconds), then sample the guard: one that re-arms itself holds it
        // at nearly every instant.
        let deadline = Instant::now() + Duration::from_secs(60);
        while state.queue_dispatchers.lock().unwrap().contains(&agent_id) {
            assert!(Instant::now() < deadline, "the first dispatcher never finished");
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut held = 0;
        for _ in 0..50 {
            if state.queue_dispatchers.lock().unwrap().contains(&agent_id) {
                held += 1;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(held, 0, "the dispatcher must yield while a turn is running");
        assert_eq!(started_count(&state, &stream_id, &queued.event_hash), 0);
        assert_eq!(pending_for_agent(&state, &stream_id, &agent_id).len(), 1);

        // The turn ends; `TurnLease::drop` is what wakes the dispatcher.
        state
            .lifecycle
            .lock()
            .unwrap()
            .running_turns
            .remove(&(stream_id.clone(), seat));
        schedule_dispatch(&state, &stream_id, seat);
        wait_for_started(&state, &stream_id, &queued.event_hash);
        // A completion Drop that re-checks its queue must not start it twice.
        std::thread::sleep(Duration::from_millis(80));
        assert_eq!(started_count(&state, &stream_id, &queued.event_hash), 1);
    }

    /// "Steer with all of them now": every waiting follow-up becomes ONE
    /// steer for the running turn, exactly once — only while a turn runs, and
    /// never for a follow-up carrying context a steer cannot hold.
    #[test]
    fn a_read_only_turn_runs_in_plan_whatever_the_conversation_and_its_tool_calls_follow() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = open_stream(&state, &open_req(&repo, "ask")).unwrap().context.stream_id;
        let seat = Seat::Orchestrator;
        let agent_id = crate::node::agent_id(&stream_id, seat);
        set_operating_mode(&state, &stream_id, seat, OperatingMode::Auto).unwrap();
        // Paused so nothing dispatches underneath the assertions.
        pause_dispatch(&state, &stream_id, seat).unwrap();
        let read_only = |event: &Event| serde_json::from_str::<serde_json::Value>(&event.payload).unwrap()["read_only"].clone();
        let ask = enqueue_in(&state, queue_request(&stream_id, "why this hunk?"), vec![], false, "human", true).unwrap();
        assert_eq!(read_only(&ask), true);
        let plain = enqueue(&state, queue_request(&stream_id, "carry on"), vec![], false).unwrap();
        assert_eq!(read_only(&plain), serde_json::Value::Null, "other messages run on the mode pill when they start");
        let pending = pending_for_agent(&state, &stream_id, &agent_id);
        let modes: Vec<OperatingMode> = pending.iter().map(|p| p.item.mode(&state, &stream_id)).collect();
        assert_eq!(modes, [OperatingMode::Plan, OperatingMode::Auto]);

        // While that turn runs, a native runtime's Bakery tool calls (MCP)
        // follow the turn's mode, not the conversation's.
        crate::state::start_agent_activity(&state, &stream_id, &agent_id, "turn_ask");
        crate::state::set_turn_mode(&state, &agent_id, OperatingMode::Plan);
        assert_eq!(crate::api_tools::Caller::mcp(&state, &stream_id, seat).mode, OperatingMode::Plan);
        crate::state::start_agent_activity(&state, &stream_id, &agent_id, "turn_next");
        assert_eq!(
            crate::api_tools::Caller::mcp(&state, &stream_id, seat).mode,
            OperatingMode::Auto,
            "a turn with no recorded mode follows the conversation"
        );
    }

    #[test]
    fn steer_all_queued_merges_waiting_follow_ups_into_one_steer_exactly_once() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = open_stream(&state, &open_req(&repo, "steer-all"))
            .unwrap()
            .context
            .stream_id;
        let seat = Seat::Orchestrator;
        let agent_id = crate::node::agent_id(&stream_id, seat);
        // Paused so nothing dispatches underneath the assertions.
        pause_dispatch(&state, &stream_id, seat).unwrap();
        enqueue(&state, queue_request(&stream_id, "first"), vec![], false).unwrap();
        enqueue(&state, queue_request(&stream_id, "second"), vec![], false).unwrap();

        let err = steer_all_queued(&state, &stream_id, seat).unwrap_err();
        assert!(err.contains("no turn is running"), "{err}");
        assert_eq!(pending_for_agent(&state, &stream_id, &agent_id).len(), 2);

        state
            .lifecycle
            .lock()
            .unwrap()
            .running_turns
            .insert((stream_id.clone(), seat), 1);
        let steer = steer_all_queued(&state, &stream_id, seat).unwrap();
        assert_eq!(steer.family, STEER_QUEUED);
        let p: serde_json::Value = serde_json::from_str(&steer.payload).unwrap();
        assert_eq!(p["instruction"], "first\n\nsecond");
        let pending = pending_for_agent(&state, &stream_id, &agent_id);
        assert_eq!(pending.len(), 1, "only the merged steer remains");
        assert_eq!(pending[0].event.family, STEER_QUEUED);
        let err = steer_all_queued(&state, &stream_id, seat).unwrap_err();
        assert!(err.contains("nothing is queued"), "{err}");

        // Attachments travel with the steer, and a selected file is named in it.
        enqueue(
            &state,
            queue_request(&stream_id, "with an attachment"),
            vec!["att_1".into()],
            false,
        )
        .unwrap();
        let steer = steer_all_queued(&state, &stream_id, seat).unwrap();
        let p: serde_json::Value = serde_json::from_str(&steer.payload).unwrap();
        assert_eq!(p["attachment_ids"], serde_json::json!(["att_1"]));
        enqueue(
            &state,
            AgentTurnReq {
                file_path: Some("f.txt".into()),
                ..queue_request(&stream_id, "look here")
            },
            vec![],
            false,
        )
        .unwrap();
        let steer = steer_all_queued(&state, &stream_id, seat).unwrap();
        let p: serde_json::Value = serde_json::from_str(&steer.payload).unwrap();
        assert_eq!(p["instruction"], "look here\n(about f.txt)");
    }

    #[test]
    fn pause_preserves_durable_followups_and_resume_dispatches_each_exactly_once() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = open_stream(&state, &open_req(&repo, "composer-queue"))
            .unwrap()
            .context
            .stream_id;

        pause_dispatch(&state, &stream_id, Seat::Orchestrator).unwrap();
        let first = enqueue(
            &state,
            queue_request(&stream_id, "first durable follow-up"),
            vec![],
            false,
        )
        .unwrap();
        let second = enqueue(
            &state,
            queue_request(&stream_id, "second durable follow-up"),
            vec![],
            false,
        )
        .unwrap();
        let agent_id = crate::node::agent_id(&stream_id, Seat::Orchestrator);
        assert!(dispatch_is_paused(&state, &stream_id, &agent_id));
        assert_eq!(pending_for_agent(&state, &stream_id, &agent_id).len(), 2);
        assert_eq!(started_count(&state, &stream_id, &first.event_hash), 0);
        assert_eq!(started_count(&state, &stream_id, &second.event_hash), 0);

        resume_dispatch(&state, &stream_id, Seat::Orchestrator).unwrap();
        wait_for_started(&state, &stream_id, &first.event_hash);
        wait_for_started(&state, &stream_id, &second.event_hash);
        // Give the completion Drop a chance to re-check its queue. A second
        // start would prove a duplicate dispatcher, not a harmless retry.
        std::thread::sleep(Duration::from_millis(80));
        assert_eq!(started_count(&state, &stream_id, &first.event_hash), 1);
        assert_eq!(started_count(&state, &stream_id, &second.event_hash), 1);
        assert!(pending_for_agent(&state, &stream_id, &agent_id).is_empty());
    }

    #[test]
    fn a_paused_dispatch_cannot_start_a_queue_item_in_the_stop_race() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = open_stream(&state, &open_req(&repo, "queue-stop-race"))
            .unwrap()
            .context
            .stream_id;
        let agent_id = crate::node::agent_id(&stream_id, Seat::Orchestrator);

        pause_dispatch(&state, &stream_id, Seat::Orchestrator).unwrap();
        let queued = enqueue(
            &state,
            queue_request(&stream_id, "must remain queued"),
            vec![],
            false,
        )
        .unwrap();
        let result = agent_turn_inner_with_control(
            &state,
            &queue_request(&stream_id, "must remain queued"),
            TurnControl {
                queue_event_hash: Some(queued.event_hash.clone()),
                mode: Some(OperatingMode::Manual),
                attachment_ids: Vec::new(),
            },
        );

        assert!(matches!(result, Err((axum::http::StatusCode::CONFLICT, _))));
        assert_eq!(started_count(&state, &stream_id, &queued.event_hash), 0);
        assert_eq!(pending_for_agent(&state, &stream_id, &agent_id).len(), 1);
    }

    /// A queued message carries nothing of the selector or the mode pill: it
    /// runs on what they show when it starts, changed since it was queued.
    #[test]
    fn a_queued_message_runs_on_the_selector_and_mode_when_it_starts() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = open_stream(&state, &open_req(&repo, "queued-on-selector")).unwrap().context.stream_id;
        let usage = bakery_agent::TurnUsage { tokens_in: Some(1), tokens_out: Some(1) };
        let model = Arc::new(crate::testutil::UsageReportingModel::new("ok", Some(usage)));
        crate::state::seed_seat_model_for_test(&state, &stream_id, Seat::Orchestrator, model);
        pause_dispatch(&state, &stream_id, Seat::Orchestrator).unwrap();
        let queued = enqueue(&state, queue_request(&stream_id, "run after the current work"), vec![], false).unwrap();
        let payload: serde_json::Value = serde_json::from_str(&queued.payload).unwrap();
        for kept_nowhere in ["model_id", "effort", "mode", "native_config"] {
            assert!(payload.get(kept_nowhere).is_none(), "{kept_nowhere} in {payload}");
        }

        // After it was queued, the operator picks a model, an effort and Plan.
        for (category, value) in [(crate::selection::MODEL, "opus"), (crate::selection::EFFORT, "high")] {
            crate::selection::choose(&state, &stream_id, Seat::Orchestrator, "", category, Some(value), None, "human").unwrap();
        }
        set_operating_mode(&state, &stream_id, Seat::Orchestrator, OperatingMode::Plan).unwrap();
        resume_dispatch(&state, &stream_id, Seat::Orchestrator).unwrap();

        let events = |family: &str| state.heart.lock().unwrap().events_of_family(&stream_id, family).unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        while events("agent.turn.usage").is_empty() {
            assert!(Instant::now() < deadline, "the queued message never ran");
            std::thread::sleep(Duration::from_millis(50));
        }
        let ran: serde_json::Value = serde_json::from_str(&events("agent.turn.usage")[0].payload).unwrap();
        assert_eq!((ran["model_id"].as_str(), ran["effort"].as_str()), (Some("opus"), Some("high")), "{ran}");
        let started: serde_json::Value = serde_json::from_str(&events("agent.turn.started")[0].payload).unwrap();
        assert_eq!(started["mode"], serde_json::to_value(OperatingMode::Plan).unwrap(), "{started}");
    }

    /// "i have until it gets sent to hit the up arrow": a waiting message
    /// comes back whole and never sends; once taken back, on its way into the
    /// running turn, or sent, it can't be.
    #[test]
    fn a_waiting_message_can_be_taken_back_until_it_is_sent() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = open_stream(&state, &open_req(&repo, "withdraw")).unwrap().context.stream_id;
        let agent_id = crate::node::agent_id(&stream_id, Seat::Orchestrator);
        let gone = |hash: &str| match withdraw(&state, &stream_id, hash) {
            Err(WithdrawRefused::Gone(why)) => why,
            _ => "not refused as gone",
        };

        pause_dispatch(&state, &stream_id, Seat::Orchestrator).unwrap();
        let queued = enqueue(&state, queue_request(&stream_id, "do the thing"), vec![], false).unwrap();
        let back = withdraw(&state, &stream_id, &queued.event_hash).ok().unwrap();
        assert_eq!(back.instruction, "do the thing");
        assert!(pending_for_agent(&state, &stream_id, &agent_id).is_empty());
        assert_eq!(consume_if_waiting(&state, &stream_id, Some(&queued.event_hash), || ()), None, "its turn never starts");
        assert_eq!(gone(&queued.event_hash), "it was already taken back");
        assert!(matches!(withdraw(&state, &stream_id, "no-such-message"), Err(WithdrawRefused::Unknown)));

        // The live loop owns the dispatcher, as in a running turn.
        state.queue_dispatchers.lock().unwrap().insert(agent_id.clone());
        let steer = enqueue(&state, queue_request(&stream_id, "steer now"), vec![], true).unwrap();
        let (claimed, claim) = claim_next_steer(&state, &stream_id, Seat::Orchestrator).unwrap();
        assert_eq!(claimed.event_hash, steer.event_hash);
        assert_eq!(gone(&steer.event_hash), "it is being delivered right now");
        drop(claim);
        take_next_steer(&state, &stream_id, Seat::Orchestrator, "turn_w", &|_| false, usize::MAX).unwrap().unwrap();
        assert_eq!(gone(&steer.event_hash), "it was already sent");
    }

    #[test]
    fn a_durable_steer_is_consumed_once_at_a_safe_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = open_stream(&state, &open_req(&repo, "composer-steer"))
            .unwrap()
            .context
            .stream_id;
        let agent_id = crate::node::agent_id(&stream_id, Seat::Orchestrator);
        // Simulate the live loop owning the dispatcher guard. The queue
        // remains durable while `take_next_steer` supplies it at a safe
        // boundary, exactly how a real active turn behaves.
        state
            .queue_dispatchers
            .lock()
            .unwrap()
            .insert(agent_id.clone());
        let queued = enqueue(
            &state,
            queue_request(&stream_id, "Explain only; make no edits."),
            vec![],
            true,
        )
        .unwrap();
        let (text, attachments) =
            take_next_steer(&state, &stream_id, Seat::Orchestrator, "turn_test", &|_| false, usize::MAX)
                .unwrap()
                .unwrap();
        assert_eq!(text, "Explain only; make no edits.");
        assert!(attachments.is_empty());
        assert!(
            take_next_steer(&state, &stream_id, Seat::Orchestrator, "turn_test", &|_| false, usize::MAX)
                .unwrap()
                .is_none()
        );
        let events = state.heart.lock().unwrap().read_stream(&stream_id).unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| event.family == STEER_APPLIED
                    && event.causality.contains(&queued.event_hash))
                .count(),
            1,
        );
    }

    /// A steer's attachments reach an HTTP turn resolved exactly as a normal
    /// turn's are: text inline, a screenshot as itself to a model that reads
    /// images. A file that cannot be read never holds the steer back — the
    /// operator said "never gate steering on anything" — it is named instead.
    #[tokio::test]
    async fn a_steer_carries_its_attachments_and_a_missing_one_never_holds_it_back() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = open_stream(&state, &open_req(&repo, "steer-attachments"))
            .unwrap()
            .context
            .stream_id;
        let seat = Seat::Orchestrator;
        let agent_id = crate::node::agent_id(&stream_id, seat);
        state.queue_dispatchers.lock().unwrap().insert(agent_id);
        let upload = serde_json::from_value(serde_json::json!({
            "stream_id": stream_id,
            "seat": "orchestrator",
            "filename": "notes.txt",
            "media_type": "text/plain",
            "data_url": "data:text/plain;base64,cHJpdmF0ZSBub3Rl",
        }))
        .unwrap();
        let attachment = crate::attachments::upload_attachment(
            axum::extract::State(state.clone()),
            axum::Json(upload),
        )
        .await
        .unwrap()
        .0;

        let screenshot = crate::attachments::upload_attachment(
            axum::extract::State(state.clone()),
            axum::Json(
                serde_json::from_value(serde_json::json!({
                    "stream_id": stream_id,
                    "seat": "orchestrator",
                    "filename": "screenshot.png",
                    "media_type": "image/png",
                    "data_url": "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAAB",
                }))
                .unwrap(),
            ),
        )
        .await
        .unwrap()
        .0;

        enqueue(
            &state,
            queue_request(&stream_id, "Use these notes."),
            vec![attachment.id, screenshot.id],
            true,
        )
        .unwrap();
        let reads_png = |media: &str| media == "image/png";
        let (text, attachments) = take_next_steer(&state, &stream_id, seat, "turn_test", &reads_png, usize::MAX)
            .unwrap()
            .unwrap();
        assert_eq!(text, "Use these notes.");
        assert!(attachments.text.contains("private note"), "{}", attachments.text);
        assert!(attachments.text.contains("[Attached image: screenshot.png"), "{}", attachments.text);
        assert_eq!(attachments.native.len(), 1);
        assert_eq!(attachments.native[0].mime_type, "image/png");

        enqueue(&state, queue_request(&stream_id, "And this."), vec!["attach_missing".into()], true)
            .unwrap();
        let (text, attachments) = take_next_steer(&state, &stream_id, seat, "turn_test", &reads_png, usize::MAX)
            .unwrap()
            .expect("a missing file never holds a steer back");
        assert_eq!(text, "And this.");
        assert!(attachments.text.contains("could not be read"), "{}", attachments.text);
        assert!(peek_next_steer(&state, &stream_id, seat).is_none());
    }

    /// A conversation with a plain folder of its own and no repository: the
    /// queue and a turn's start need nothing more.
    fn chat(state: &AppState, title: &str) -> String {
        crate::catalog::create_chat(state, title).unwrap().stream_id
    }

    /// The longest a test lets a send wait for its turn: as long as
    /// `wait_for_started` lets a dispatch take, since under a full suite's
    /// load a dispatch can take seconds to be scheduled at all.
    const LOADED: Duration = Duration::from_secs(60);

    /// A composer send, given that long to see its turn start.
    async fn send(state: &AppState, stream_id: &str, instruction: &str, steer: bool) -> Queued {
        enqueue_awaiting_start_for(state, queue_request(stream_id, instruction), vec![], steer, "human", false, LOADED)
            .await
            .unwrap()
    }

    /// A send that must answer without waiting for a turn that is not coming:
    /// far inside the longest wait, which a send that did wait would use up.
    async fn send_at_once(state: &AppState, stream_id: &str, instruction: &str) -> Queued {
        let began = Instant::now();
        let sent = send(state, stream_id, instruction, false).await;
        assert!(began.elapsed() < LOADED / 2, "{instruction}: it waited for a turn that cannot start");
        sent
    }

    /// The agent's dispatcher has met whatever stops it and gone (under load
    /// that can take seconds): only then does "not started" say anything.
    fn dispatcher_done(state: &AppState, agent_id: &str) {
        let deadline = Instant::now() + LOADED;
        while state.queue_dispatchers.lock().unwrap().contains(agent_id) {
            assert!(Instant::now() < deadline, "the dispatcher never finished");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// A message to an idle agent never reads as waiting: its send answers
    /// once the turn has begun, with that turn's started fact beside the
    /// queued one, so the composer can put it straight into the thread.
    #[tokio::test]
    async fn a_message_to_an_idle_agent_comes_back_with_its_turn_started() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = chat(&state, "idle-send");

        let sent = send(&state, &stream_id, "go", false).await;
        assert_eq!(sent.event.family, FOLLOW_UP_QUEUED);
        let started = sent.started.as_ref().expect("an idle agent's turn has begun when its send answers");
        assert_eq!(started.family, "agent.turn.started");
        assert_eq!(started.causality, [sent.event.event_hash.clone()], "it consumed this very message");
        assert_eq!(started_count(&state, &stream_id, &sent.event.event_hash), 1);
        // On the wire it is the queued event, with the turn's beside it.
        let wire = serde_json::to_value(&sent).unwrap();
        assert_eq!(wire["event_hash"], sent.event.event_hash.as_str());
        assert_eq!(wire["started"]["family"], "agent.turn.started");
    }

    /// Only an idle agent's message waits for its turn. One that has to queue
    /// (a turn running, a paused queue, a message ahead, a change of its
    /// last turn still up for review in Manual) answers as soon as it is
    /// queued, and stays queued.
    #[tokio::test]
    async fn a_message_that_cannot_run_at_once_comes_back_queued_and_promptly() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = chat(&state, "busy-send");
        let seat = Seat::Orchestrator;
        let agent_id = crate::node::agent_id(&stream_id, seat);

        // A turn running: the dispatcher meets it and yields.
        state.lifecycle.lock().unwrap().running_turns.insert((stream_id.clone(), seat), 1);
        let running = send_at_once(&state, &stream_id, "behind the running turn").await;
        dispatcher_done(&state, &agent_id);
        assert!(running.started.is_none());
        assert_eq!(started_count(&state, &stream_id, &running.event.event_hash), 0);
        state.lifecycle.lock().unwrap().running_turns.remove(&(stream_id.clone(), seat));

        // A paused queue holds it, and the message already waiting ahead.
        pause_dispatch(&state, &stream_id, seat).unwrap();
        let paused = send_at_once(&state, &stream_id, "while paused").await;
        assert!(paused.started.is_none());
        assert_eq!(pending_for_agent(&state, &stream_id, &agent_id).len(), 2);
        // Resumed, the ones ahead run first; a new message is behind them.
        state.queue_dispatchers.lock().unwrap().insert(agent_id.clone());
        resume_dispatch(&state, &stream_id, seat).unwrap();
        let behind = send_at_once(&state, &stream_id, "behind the others").await;
        assert!(behind.started.is_none());
        state.queue_dispatchers.lock().unwrap().remove(&agent_id);
        pause_dispatch(&state, &stream_id, seat).unwrap();
        for item in pending_for_agent(&state, &stream_id, &agent_id) {
            withdraw(&state, &stream_id, &item.event.event_hash).ok().unwrap();
        }

        // In Manual, a change still up for review holds the next turn; in
        // Auto it does not.
        resume_dispatch(&state, &stream_id, seat).unwrap();
        let landed = serde_json::json!({ "agent_id": agent_id, "status": "pending", "landed": { "turn_id": "turn_0" } });
        append(&state, &stream_id, "node", "agent.file.changed", landed.to_string(), vec![]).unwrap();
        let reviewing = send_at_once(&state, &stream_id, "while a change waits").await;
        dispatcher_done(&state, &agent_id);
        assert!(reviewing.started.is_none());
        assert_eq!(started_count(&state, &stream_id, &reviewing.event.event_hash), 0);
        set_operating_mode(&state, &stream_id, seat, OperatingMode::Auto).unwrap();
        withdraw(&state, &stream_id, &reviewing.event.event_hash).ok().unwrap();
        let auto = send(&state, &stream_id, "in auto", false).await;
        assert!(auto.started.is_some(), "a change up for review holds only a Manual turn");
    }

    /// A steer joins the running turn, so it never waits for one to start.
    #[tokio::test]
    async fn a_steer_never_waits_for_a_turn_to_start() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = chat(&state, "steer-send");
        // The live loop owns the dispatcher, as in a running turn.
        state.queue_dispatchers.lock().unwrap().insert(crate::node::agent_id(&stream_id, Seat::Orchestrator));

        let began = Instant::now();
        let sent = send(&state, &stream_id, "change course", true).await;
        assert_eq!(sent.event.family, STEER_QUEUED);
        assert!(sent.started.is_none());
        assert!(began.elapsed() < LOADED / 2);
    }

    /// A send that finds out, while it waits, that its message will not be the
    /// next to start answers then, not when its wait runs out (which would
    /// hold the composer on Stop before the queue shows the message): another
    /// turn took the agent first, Stop paused its queue, its dispatcher went
    /// with it still waiting (its admission was refused), or the message was
    /// settled some other way.
    #[tokio::test]
    async fn a_message_that_will_not_start_next_comes_back_queued_when_that_is_known() {
        let seat = Seat::Orchestrator;
        let waits = |name: &'static str| {
            let dir = tempfile::tempdir().unwrap();
            let state = state_with(dir.path(), Arc::new(StubModel));
            let stream_id = chat(&state, name);
            let agent_id = crate::node::agent_id(&stream_id, seat);
            // A live loop owns dispatch, so nothing starts the message by itself.
            state.queue_dispatchers.lock().unwrap().insert(agent_id.clone());
            (dir, state, stream_id, agent_id)
        };
        let later = Duration::from_millis(100);

        // Another sender's turn takes the agent first.
        let (_dir, state, stream_id, agent_id) = waits("rival");
        let rival = async {
            tokio::time::sleep(later).await;
            let started = serde_json::json!({ "agent_id": agent_id, "instruction": "first" }).to_string();
            append(&state, &stream_id, "human", "agent.turn.started", started, vec![]).unwrap();
        };
        let (sent, ()) = tokio::join!(send_at_once(&state, &stream_id, "second"), rival);
        assert!(sent.started.is_none());
        assert_eq!(pending_for_agent(&state, &stream_id, &agent_id).len(), 1, "it stays queued");

        // Stop pauses the queue.
        let (_dir, state, stream_id, _agent_id) = waits("stop");
        let stop = async {
            tokio::time::sleep(later).await;
            pause_dispatch(&state, &stream_id, seat).unwrap();
        };
        let (sent, ()) = tokio::join!(send_at_once(&state, &stream_id, "go"), stop);
        assert!(sent.started.is_none());

        // The dispatcher goes with the message still waiting.
        let (_dir, state, stream_id, agent_id) = waits("refused");
        let gone = async {
            tokio::time::sleep(later).await;
            state.queue_dispatchers.lock().unwrap().remove(&agent_id);
        };
        let (sent, ()) = tokio::join!(send_at_once(&state, &stream_id, "go"), gone);
        assert!(sent.started.is_none());

        // The message is taken back.
        let (_dir, state, stream_id, agent_id) = waits("taken-back");
        let taken_back = async {
            tokio::time::sleep(later).await;
            let waiting = pending_for_agent(&state, &stream_id, &agent_id);
            withdraw(&state, &stream_id, &waiting[0].event.event_hash).ok().unwrap();
        };
        let (sent, ()) = tokio::join!(send_at_once(&state, &stream_id, "go"), taken_back);
        assert!(sent.started.is_none());
    }

    /// A Manual follow-up waits for the agent's last changes to be decided,
    /// and its dispatcher goes quiet: asking again at once would be refused
    /// the same way, over and over, for as long as the review lasts. Deciding
    /// the last change wakes the queue (`snapshots::decided`), and the
    /// follow-up starts.
    #[test]
    fn a_review_wait_leaves_the_dispatcher_quiet_until_the_last_change_is_decided() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = chat(&state, "review-wait");
        let seat = Seat::Orchestrator;
        let agent_id = crate::node::agent_id(&stream_id, seat);
        let landed = serde_json::json!({ "agent_id": agent_id, "status": "pending", "landed": { "turn_id": "turn_0" } });
        let change = append(&state, &stream_id, "node", "agent.file.changed", landed.to_string(), vec![]).unwrap();
        let queued = enqueue(&state, queue_request(&stream_id, "after the review"), vec![], false).unwrap();

        // The dispatcher meets the review wait and goes; sampled, it is not
        // there again: one that restarts itself holds its guard at nearly
        // every instant.
        dispatcher_done(&state, &agent_id);
        let mut seen = 0;
        for _ in 0..50 {
            if state.queue_dispatchers.lock().unwrap().contains(&agent_id) {
                seen += 1;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(seen, 0, "the dispatcher must not restart itself while a review is pending");
        assert_eq!(started_count(&state, &stream_id, &queued.event_hash), 0);
        assert_eq!(pending_for_agent(&state, &stream_id, &agent_id).len(), 1);

        // The change is decided: the queue wakes, and the follow-up starts.
        let accepted = serde_json::json!({ "decided_event": change.event_hash }).to_string();
        append(&state, &stream_id, "human", "agent.edit.accepted", accepted, vec![change.event_hash.clone()]).unwrap();
        crate::snapshots::decided(&state, &stream_id, &serde_json::json!({ "seat": "orchestrator" }));
        wait_for_started(&state, &stream_id, &queued.event_hash);
    }

    /// The turn is on the stream before the workspace is read for review, so
    /// a message reads as a turn while Git is still at work. With the
    /// snapshot made to fail, its notice follows `agent.turn.started`.
    #[test]
    fn a_turn_is_started_before_its_snapshot_is_taken() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = chat(&state, "start-first");
        // A file where the snapshots' folder would go: the snapshot cannot
        // begin, and says so on the stream.
        std::fs::write(dir.path().join("snapshots"), "in the way").unwrap();

        crate::routes::agent::agent_turn_inner(&state, &queue_request(&stream_id, "go")).unwrap();
        let families: Vec<String> = state
            .heart
            .lock()
            .unwrap()
            .read_stream(&stream_id)
            .unwrap()
            .into_iter()
            .map(|e| e.family)
            .collect();
        let at = |family: &str| families.iter().position(|f| f == family).unwrap_or_else(|| panic!("no {family} in {families:?}"));
        assert!(at("agent.turn.started") < at("agent.snapshot.failed"), "{families:?}");
    }
}
