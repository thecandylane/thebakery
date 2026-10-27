//! Write-mode policy (`/policy/write-mode`) and the agent turn / accept /
//! reject routes (`/agent/turn`, `/agent/accept`, `/agent/reject`).

use crate::agent_control::{self, OperatingMode, TurnControl};
use crate::node::WriteMode;
use crate::routes::operator::{gates_inner, node_info_snapshot, NodeInfo};
use crate::state::{
    admit_activity, agent_activity_snapshot, append, append_streaming_text,
    compose_turn_usage, context_for, jail_for, push_completed_tool,
    record_turn_usage, request_cancel, resolve_awaiting_approval, resolve_seat,
    resolve_seat_with_override, set_agent_phase, start_agent_activity, ActiveContext,
    ActivityPhase, AgentActivity, AppState, ApprovalKind, CompletedTool, ToolRunKind,
};
use axum::{
    extract::{Query, State},
    http::StatusCode,
    Json,
};
use bakery_agent::{ChatReply, Model, StopReason, StreamEnd};
use bakery_core::Event;
use serde::Deserialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Heart stream used when the client doesn't cite a session — write-mode is
/// per-node, so gate changes must still leave an event (CONTROL_PLANE silent-state).
pub(crate) const NODE_POLICY_STREAM: &str = "node:policy";

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct WriteModeReq {
    pub(crate) write_mode: WriteMode,
    /// Optional: stream the human was looking at. Always audit; if omitted,
    /// events land on `node:policy` so StatusBar clicks are not silent.
    #[serde(default)]
    pub(crate) stream_id: Option<String>,
}

/// Persist a new write mode to `node.json` and always record
/// `policy.write_mode.set` (named stream or `node:policy`).
pub(crate) async fn set_write_mode(
    State(st): State<AppState>,
    Json(req): Json<WriteModeReq>,
) -> Result<Json<NodeInfo>, (StatusCode, String)> {
    {
        let mut node = st.node.lock().unwrap();
        node.write_mode = req.write_mode;
        node.save(&st.base)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    }
    let audit_stream = req
        .stream_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(NODE_POLICY_STREAM);
    let payload = serde_json::json!({
        "write_mode": req.write_mode,
        "scope": if req.stream_id.as_ref().map(|s| !s.trim().is_empty()).unwrap_or(false) {
            "session"
        } else {
            "node"
        },
    })
    .to_string();
    // Best-effort: mode is already persisted; audit must not undo it.
    let _ = append(
        &st,
        audit_stream,
        "human",
        "policy.write_mode.set",
        payload,
        vec![],
    );
    Ok(Json(node_info_snapshot(&st)))
}

#[derive(schemars::JsonSchema, Debug, Default, Clone, Deserialize)]
pub(crate) struct AgentTurnReq {
    pub(crate) stream_id: String,
    /// Optional now that tools exist: with a path the agent edits that file
    /// directly; without one it explores the worktree first.
    #[serde(default)]
    pub(crate) file_path: Option<String>,
    pub(crate) instruction: String,
    /// Which seat this turn answers as. `None` resolves to `Baker` for a
    /// Baker stream, else `Orchestrator` (06-SEATS.md §2: "Baker stream uses
    /// baker seat").
    #[serde(default)]
    pub(crate) seat: Option<crate::node::Seat>,
    /// The model and effort the turn runs on: what the conversation's
    /// selector shows (`selection`), filled in when the turn starts
    /// (`agent_turn_after_started`). Never taken from a caller: whatever one
    /// puts here is replaced, so nothing sets them out of sight.
    #[serde(skip)]
    pub(crate) model_id: Option<String>,
    #[serde(skip)]
    pub(crate) effort: Option<String>,
    /// Who actually issued this instruction — recorded verbatim as this
    /// turn's `agent.turn.started` actor. `None` (every HTTP caller today:
    /// the human-facing `/agent/turn` route, tests) means "a human typed
    /// this," the historical default. `task_driver.rs` (07d-B Phase 4) is
    /// the one caller that overrides it — to `task-driver` for a turn the
    /// driver itself decided to issue (the opening contract, an autonomous
    /// continuation, a re-queued proof failure), or to the actual actor of
    /// a `task.message` it is relaying (`"human"`, `"agent:baker"`, ...) —
    /// so the task's own transcript never misattributes the driver's
    /// scheduling decisions to the operator who typed none of them.
    #[serde(default)]
    pub(crate) actor: Option<String>,
}

pub(crate) async fn agent_turn(
    State(st): State<AppState>,
    Json(req): Json<AgentTurnReq>,
) -> Result<Json<Event>, (StatusCode, String)> {
    // Model HTTP is pure-sync (ureq). Still run off the async workers so a
    // long Grok call cannot stall the runtime.
    let st2 = st.clone();
    crate::actor::spawn_blocking(move || agent_turn_inner(&st2, &req))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map(Json)
}

/// Composer sends are durable follow-ups, not a request held open until a
/// model finishes.  Keeping this separate from the legacy `/agent/turn`
/// route preserves existing automation while letting a pane close, reload,
/// or submit another instruction during a long turn.
#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct AgentQueueReq {
    #[serde(flatten)]
    pub(crate) turn: AgentTurnReq,
    #[serde(default)]
    pub(crate) attachment_ids: Vec<String>,
    /// Run this one turn read-only (Plan mode: read and search, no edits or
    /// commands), whatever the conversation's mode: an Ask that must not
    /// change files.
    #[serde(default)]
    pub(crate) read_only: bool,
}

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct AgentModeReq {
    pub(crate) stream_id: String,
    pub(crate) seat: crate::node::Seat,
    pub(crate) mode: OperatingMode,
}

fn resolve_composer_seat(
    state: &AppState,
    req: &mut AgentTurnReq,
) -> Result<crate::node::Seat, (StatusCode, String)> {
    let ctx = context_for(state, &req.stream_id).map_err(|e| (StatusCode::NOT_FOUND, e))?;
    let seat = req.seat.unwrap_or_else(|| default_seat_for(state, &ctx));
    let is_factory = is_baker_stream_ctx(state, &ctx);
    if is_factory != (seat == crate::node::Seat::Baker) {
        return Err((
            StatusCode::BAD_REQUEST,
            if is_factory {
                format!(
                    "the {} seat cannot take a turn on the factory socket — only baker can",
                    seat.as_str()
                )
            } else {
                "the baker seat cannot take a turn on a project socket".to_string()
            },
        ));
    }
    req.seat = Some(seat);
    Ok(seat)
}

async fn enqueue_composer_turn(
    State(st): State<AppState>,
    Json(mut req): Json<AgentQueueReq>,
    steer: bool,
) -> Result<agent_control::Queued, (StatusCode, String)> {
    // A steer is never refused. It joins a running turn with its text and
    // attachments; a file it carries applies when it runs as its own turn
    // instead (nothing was running), and is kept for that.
    if req.turn.instruction.trim().is_empty() && req.attachment_ids.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "write an instruction or attach a file".to_string(),
        ));
    }
    // A steer joins the turn that's running, in that turn's mode.
    if req.read_only && steer {
        return Err((
            StatusCode::BAD_REQUEST,
            "a read-only message can't steer a running turn; queue it instead".to_string(),
        ));
    }
    resolve_composer_seat(&st, &mut req.turn)?;
    agent_control::enqueue_awaiting_start(&st, req.turn, req.attachment_ids, steer, "human", req.read_only)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

/// `POST /agent/turn/queue`: the queued message, and, to an idle agent whose
/// turn began at once, that turn's `started` beside it.
pub(crate) async fn agent_turn_queue(
    state: State<AppState>,
    req: Json<AgentQueueReq>,
) -> Result<Json<agent_control::Queued>, (StatusCode, String)> {
    enqueue_composer_turn(state, req, false).await.map(Json)
}

/// `POST /agent/turn/steer`: a steer never waits for a turn to start, so it
/// answers with the queued event alone.
pub(crate) async fn agent_turn_steer(
    state: State<AppState>,
    req: Json<AgentQueueReq>,
) -> Result<Json<Event>, (StatusCode, String)> {
    enqueue_composer_turn(state, req, true).await.map(|queued| Json(queued.event))
}

pub(crate) async fn set_operating_mode(
    State(st): State<AppState>,
    Json(req): Json<AgentModeReq>,
) -> Result<Json<Event>, (StatusCode, String)> {
    let mut turn = AgentTurnReq {
        stream_id: req.stream_id,
        seat: Some(req.seat),
        ..Default::default()
    };
    let seat = resolve_composer_seat(&st, &mut turn)?;
    let st2 = st.clone();
    let stream_id = turn.stream_id.clone();
    crate::actor::spawn_blocking(move || {
        // A mode is a durable Bakery control for every agent.  Only an ACP
        // assignment additionally pushes it into its live session, if one
        // runs (one that starts later takes it before its first prompt); do
        // not make an unassigned/HTTP seat resolve merely to remember its
        // composer preference (that regressed the pre-native behavior).
        let assigned_model =
            crate::state::latest_seat_model_id(&st2.heart.lock().unwrap(), &stream_id, seat)
                .map_err(|_| {
                    (
                        StatusCode::BAD_REQUEST,
                        format!(
                            "the {} seat's latest assignment record is unreadable — reassign it",
                            seat.as_str()
                        ),
                    )
                })?;
        let assignment_is_acp = assigned_model.as_deref().is_some_and(|model_id| {
            model_id.eq_ignore_ascii_case(crate::claude_native::ACCOUNT_ID)
                || model_id.eq_ignore_ascii_case(crate::pipe::ACP_MODEL_ID)
                || model_id.eq_ignore_ascii_case("grok-acp")
                || st2
                    .node
                    .lock()
                    .unwrap()
                    .acp_plugs
                    .iter()
                    .any(|plug| plug.id == model_id)
        });
        if assignment_is_acp {
            let model =
                resolve_seat(&st2, &stream_id, seat).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
            model
                .set_native_operating_mode(Some(req.mode.as_str()))
                .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
        }
        agent_control::set_operating_mode(&st2, &stream_id, seat, req.mode)
            .map(Json)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
}

pub(crate) const NATIVE_CONFIG_ACKNOWLEDGED: &str = "agent.native_config.acknowledged";

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct SelectionReq {
    pub(crate) stream_id: String,
    pub(crate) seat: crate::node::Seat,
    /// The model, as the picker names it; absent or empty for the runtime's own.
    #[serde(default)]
    pub(crate) model: Option<String>,
    /// How the picker shows it, for the thread's "Model set to …" row.
    #[serde(default)]
    pub(crate) model_label: Option<String>,
    /// The effort, as the picker names it; absent or empty for the runtime's own.
    #[serde(default)]
    pub(crate) effort: Option<String>,
    #[serde(default)]
    pub(crate) effort_label: Option<String>,
}

/// Sets a conversation's selector: the model and effort every one of its
/// turns runs on, until changed. On a native seat with a session running
/// they are that session's (`/agent/runtime/config`); before one runs (a new
/// chat) they are kept for it, as thread_create's are.
pub(crate) async fn set_selection(
    State(st): State<AppState>,
    Json(req): Json<SelectionReq>,
) -> Result<Json<crate::selection::Selection>, (StatusCode, String)> {
    context_for(&st, &req.stream_id).map_err(|e| (StatusCode::NOT_FOUND, e))?;
    crate::actor::spawn_blocking(move || {
        let runtime = {
            let heart = st.heart.lock().unwrap();
            crate::state::latest_seat_model_id(&heart, &req.stream_id, req.seat).ok().flatten()
        };
        let runtime = runtime.ok_or((StatusCode::BAD_REQUEST, "no model is assigned to this seat".to_string()))?;
        let model = resolve_seat(&st, &req.stream_id, req.seat).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
        let running = crate::pipe::acp_runtime_snapshot(&st, &req.stream_id, req.seat).is_some();
        if model.transport() == bakery_agent::Transport::Acp && running {
            return Err((
                StatusCode::BAD_REQUEST,
                "a running native session's model and effort are its own: set them with /agent/runtime/config".to_string(),
            ));
        }
        let actor = crate::actor::attribute("human");
        for (category, value, label) in [
            (crate::selection::MODEL, &req.model, &req.model_label),
            (crate::selection::EFFORT, &req.effort, &req.effort_label),
        ] {
            crate::selection::choose(&st, &req.stream_id, req.seat, &runtime, category, value.as_deref(), label.as_deref(), &actor)
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
        }
        Ok(Json(crate::selection::chosen(&st, &req.stream_id, req.seat)))
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
}

#[derive(schemars::JsonSchema, Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum NativeConfigInput {
    Select(String),
    Boolean(bool),
}

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct NativeConfigReq {
    pub(crate) stream_id: String,
    pub(crate) seat: crate::node::Seat,
    pub(crate) generation: u64,
    pub(crate) config_id: String,
    pub(crate) value: NativeConfigInput,
}

/// Apply a session-advertised configuration value and return only the
/// adapter's acknowledged, complete replacement configuration.  The
/// generation is mandatory so a slow picker response can never mutate a
/// respawned/reassigned agent.
pub(crate) async fn set_native_config(
    State(st): State<AppState>,
    Json(req): Json<NativeConfigReq>,
) -> Result<Json<crate::pipe::AcpRuntimeSnapshot>, (StatusCode, String)> {
    context_for(&st, &req.stream_id).map_err(|e| (StatusCode::NOT_FOUND, e))?;
    let st2 = st.clone();
    crate::actor::spawn_blocking(move || {
        let value = match req.value {
            NativeConfigInput::Select(value) => crate::pipe::AcpConfigValue::Select(value),
            NativeConfigInput::Boolean(value) => crate::pipe::AcpConfigValue::Boolean(value),
        };
        let requested = match &value {
            crate::pipe::AcpConfigValue::Select(value) => serde_json::Value::String(value.clone()),
            crate::pipe::AcpConfigValue::Boolean(value) => serde_json::Value::Bool(*value),
        };
        let snapshot = crate::pipe::set_acp_session_config(
            &st2,
            &req.stream_id,
            req.seat,
            req.generation,
            req.config_id.clone(),
            value,
        )
        .map_err(|error| {
            let status = if error.contains("stale ACP session generation") {
                StatusCode::CONFLICT
            } else if error.starts_with(crate::pipe::CONFIG_UNANSWERED) {
                StatusCode::GATEWAY_TIMEOUT
            } else {
                StatusCode::BAD_REQUEST
            };
            (status, error)
        })?;
        let option = snapshot
            .config_options
            .iter()
            .find(|option| option.id == req.config_id);
        let effective = option.map(|option| option.current_value.clone());
        // The names the picker shows, so the transcript can say "Effort set
        // to Max" instead of the event's ids.
        let name = option.map(|option| option.name.clone());
        let value_name = option.and_then(|option| {
            option
                .values
                .iter()
                .find(|value| option.current_value == value.id.as_str())
                .map(|value| value.name.clone())
        });
        let agent_id = crate::node::agent_id(&req.stream_id, req.seat);
        append(
            &st2,
            &req.stream_id,
            "human",
            NATIVE_CONFIG_ACKNOWLEDGED,
            serde_json::json!({
                "agent_id": agent_id,
                "seat": req.seat,
                "account_id": snapshot.account_id,
                "generation": snapshot.generation,
                "config_id": req.config_id,
                "name": name,
                "requested": requested,
                "effective": effective,
                "value_name": value_name,
                "provenance": snapshot.provenance,
            })
            .to_string(),
            vec![],
        )
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error))?;
        Ok(Json(snapshot))
    })
    .await
    .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?
}

/// `POST /agent/turn/queue/steer-all` — every follow-up waiting for this
/// agent becomes one steer for its running turn
/// (`agent_control::steer_all_queued`). 409 when no turn is running or a
/// queued item carries context a steer cannot.
pub(crate) async fn steer_all_queued(
    State(st): State<AppState>,
    Json(req): Json<AgentTurnCancelReq>,
) -> Result<Json<Event>, (StatusCode, String)> {
    context_for(&st, &req.stream_id).map_err(|e| (StatusCode::NOT_FOUND, e))?;
    let mut turn = AgentTurnReq {
        stream_id: req.stream_id,
        seat: Some(req.seat),
        ..Default::default()
    };
    let seat = resolve_composer_seat(&st, &mut turn)?;
    agent_control::steer_all_queued(&st, &turn.stream_id, seat)
        .map(Json)
        .map_err(|e| (StatusCode::CONFLICT, e))
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct WithdrawReq {
    pub(crate) stream_id: String,
    /// The queued message: its queued event's hash.
    pub(crate) id: String,
}

/// `POST /agent/turn/queue/withdraw` — the operator takes back a message
/// still waiting, a follow-up or an undelivered steer, to edit or drop it
/// (`agent_control::withdraw`); its text comes back for the composer. 409
/// once it is sent, merged, or on its way into the running turn.
pub(crate) async fn withdraw_queued(
    State(st): State<AppState>,
    Json(req): Json<WithdrawReq>,
) -> Result<Json<agent_control::Withdrawn>, (StatusCode, String)> {
    let id = req.id.clone();
    crate::actor::spawn_blocking(move || agent_control::withdraw(&st, &req.stream_id, &req.id))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map(Json)
        .map_err(|refused| match refused {
            agent_control::WithdrawRefused::Unknown => (StatusCode::NOT_FOUND, format!("no queued message {id} here")),
            agent_control::WithdrawRefused::Gone(why) => (StatusCode::CONFLICT, why.to_string()),
        })
}

pub(crate) async fn resume_dispatch(
    State(st): State<AppState>,
    Json(req): Json<AgentTurnCancelReq>,
) -> Result<Json<Event>, (StatusCode, String)> {
    context_for(&st, &req.stream_id).map_err(|e| (StatusCode::NOT_FOUND, e))?;
    // The cancellation request already has an explicit seat, but reuse the
    // same factory/project boundary guard as new composer work.
    let mut turn = AgentTurnReq {
        stream_id: req.stream_id,
        seat: Some(req.seat),
        ..Default::default()
    };
    let seat = resolve_composer_seat(&st, &mut turn)?;
    agent_control::resume_dispatch(&st, &turn.stream_id, seat)
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct AgentActivityQuery {
    pub(crate) stream_id: String,
    pub(crate) seat: crate::node::Seat,
}

/// `GET /agent/activity?stream_id=&seat=` (07d-A5) — the bootstrap/reconnect
/// snapshot for the live work-trace strip. Deliberately takes `stream_id`+
/// `seat`, never a raw `agent_id` string, matching every other route's
/// identity-construction convention (`/agent/turn`, `/seat/assign`, ...) —
/// the server reconstructs the opaque `agent_id` itself. `null` when no
/// turn is (or has ever been) tracked for this agent, the same `Option<T>
/// → null` convention `assigned_model_id` etc. already use. Deliberately
/// **not** folded into `/studio/state` — that endpoint is polled
/// continuously by every open Studio view; this route is only ever hit
/// when a Chat pane actually mounts or the event WebSocket reconnects.
pub(crate) async fn agent_activity(
    State(st): State<AppState>,
    Query(q): Query<AgentActivityQuery>,
) -> Result<Json<Option<AgentActivity>>, (StatusCode, String)> {
    context_for(&st, &q.stream_id).map_err(|e| (StatusCode::NOT_FOUND, e))?;
    let agent_id = crate::node::agent_id(&q.stream_id, q.seat);
    Ok(Json(agent_activity_snapshot(&st, &agent_id)))
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct AgentTurnCancelReq {
    pub(crate) stream_id: String,
    pub(crate) seat: crate::node::Seat,
}

/// `POST /agent/turn/cancel` (07d-A5) — flips the shared cancel flag a
/// running turn's `chat_turn_streaming` call (and `agent_tool_turn`'s own
/// loop) polls. Stop always first records a durable per-agent dispatch
/// pause, even when no turn is active; a queued instruction must never be
/// deleted or silently started after the operator pressed Stop. It never
/// appends `agent.turn.cancelled` itself: only the turn loop, once it has
/// actually observed cancellation, can honestly know whether it won the
/// race against a natural finish. An approval wait is the exception to the
/// otherwise invisible boundary: Stop closes that exact pending gate before
/// the loop observes its cancel flag, so a late Accept/Answer cannot revive
/// the stopped operation.
pub(crate) async fn agent_turn_cancel(
    State(st): State<AppState>,
    Json(req): Json<AgentTurnCancelReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    context_for(&st, &req.stream_id).map_err(|e| (StatusCode::NOT_FOUND, e))?;
    stop_turn(&st, &req.stream_id, req.seat)
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

/// Stop's effect, shared by the route and the Baker's `turn_stop` op.
pub(crate) fn stop_turn(
    st: &AppState,
    stream_id: &str,
    seat: crate::node::Seat,
) -> Result<serde_json::Value, String> {
    let agent_id = crate::node::agent_id(stream_id, seat);
    // Stop is a dispatch pause first.  It never clears queued follow-ups or
    // steers; cancellation only reaches the currently active turn.
    let pause = agent_control::pause_dispatch(st, stream_id, seat)?;
    let cancelled = request_cancel(st, &agent_id);
    let approval_cancelled = if cancelled {
        settle_cancelled_approval(st, stream_id, &agent_id)?.is_some()
    } else {
        false
    };
    Ok(serde_json::json!({
        "cancelled": cancelled,
        "approval_cancelled": approval_cancelled,
        "paused": true,
        "pause_event": pause.event_hash,
    }))
}

/// `agent.approval.cancelled` is a terminal decision for any of Bakery's
/// human gates (file review, `ask_user`, or a native child-tool permission).
/// Unlike `agent.turn.cancelled`, it identifies the exact pending event so
/// projections and recovery do not leave an actionable card behind after the
/// operator stopped the operation that created it.
const APPROVAL_CANCELLED_FAMILY: &str = "agent.approval.cancelled";

fn approval_kind_name(kind: ApprovalKind) -> &'static str {
    match kind {
        ApprovalKind::FileEdit => "file_edit",
        ApprovalKind::ChildTool => "child_tool",
        ApprovalKind::Question => "question",
    }
}

pub(crate) fn closes_pending_operation(family: &str) -> bool {
    matches!(
        family,
        "agent.edit.accepted"
            | "agent.edit.rejected"
            | "agent.ask.answered"
            | "agent.child_tool.decided"
            | APPROVAL_CANCELLED_FAMILY
    )
}

/// True only when the *same* operation that produced `pending_payload` is
/// currently stopping. A later turn for this agent must never make an old
/// approval unanswerable, and an old cancellation must never affect a newer
/// turn, hence both the agent identity and minted turn id are required.
pub(crate) fn pending_operation_is_stopping(
    state: &AppState,
    pending_payload: &serde_json::Value,
) -> bool {
    let Some(agent_id) = pending_payload
        .get("agent_id")
        .and_then(serde_json::Value::as_str)
    else {
        return false;
    };
    let Some(turn_id) = pending_payload
        .get("turn_id")
        .and_then(serde_json::Value::as_str)
    else {
        return false;
    };
    agent_activity_snapshot(state, agent_id)
        .is_some_and(|activity| activity.turn_id == turn_id && activity.cancel_requested)
}

/// Mark the exact current approval as stopped. All decision paths share
/// `edit_decision_lock`, including questions and child-tool permissions, so
/// an Accept/Answer that acquires the lock after Stop sees this durable
/// terminal decision and cannot write or resume the old operation.
fn settle_cancelled_approval(
    state: &AppState,
    stream_id: &str,
    agent_id: &str,
) -> Result<Option<Event>, String> {
    let _decision = state.edit_decision_lock.lock().unwrap();
    let Some(activity) = agent_activity_snapshot(state, agent_id) else {
        return Ok(None);
    };
    let ActivityPhase::AwaitingApproval { kind, event_hash } = &activity.phase else {
        return Ok(None);
    };
    if !activity.cancel_requested {
        return Ok(None);
    }

    let event_hash = event_hash.clone();
    let kind = *kind;
    let events = state
        .heart
        .lock()
        .unwrap()
        .read_stream(stream_id)
        .map_err(|e| e.to_string())?;
    let Some(pending) = events.iter().find(|event| event.event_hash == event_hash) else {
        return Ok(None);
    };
    let pending_payload: serde_json::Value =
        serde_json::from_str(&pending.payload).unwrap_or(serde_json::Value::Null);
    let owns_pending = pending_payload
        .get("agent_id")
        .and_then(serde_json::Value::as_str)
        == Some(agent_id)
        && pending_payload
            .get("turn_id")
            .and_then(serde_json::Value::as_str)
            == Some(activity.turn_id.as_str());
    if !owns_pending {
        return Ok(None);
    }
    if events.iter().any(|event| {
        closes_pending_operation(&event.family)
            && event.causality.iter().any(|hash| hash == &event_hash)
    }) {
        return Ok(None);
    }

    let event = append(
        state,
        stream_id,
        "human",
        APPROVAL_CANCELLED_FAMILY,
        serde_json::json!({
            "decided_event": event_hash,
            "status": "cancelled",
            // What it waited to allow never ran.
            "reason": STOPPED_BEFORE_IT_RAN,
            "kind": approval_kind_name(kind),
            "agent_id": agent_id,
            "turn_id": activity.turn_id,
        })
        .to_string(),
        vec![pending.event_hash.clone()],
    )?;
    // Wake a parked HTTP loop without pretending the human rejected the
    // request. `wait_for_decision` maps this distinct outcome to cancellation.
    let _ =
        crate::state::notify_decision(state, &event_hash, crate::state::DecisionOutcome::Cancelled);
    Ok(Some(event))
}

/// One agent turn. With `file_path`, a single-shot edit of that file (works with
/// every provider). Without it, the native tool loop explores first.
///
/// Manual mode either way: nothing is written to disk here — a proposal lands as
/// a PENDING `agent.file.changed` event and waits for Accept.
/// Synchronous → unit-testable without HTTP.
pub(crate) fn agent_turn_inner(
    state: &AppState,
    req: &AgentTurnReq,
) -> Result<Event, (StatusCode, String)> {
    agent_turn_inner_with_control(state, req, TurnControl::default())
}

/// Internal turn entrypoint used by the durable dispatcher.  Direct callers
/// retain `agent_turn_inner`; the extra control data carries the queue
/// causality and mode snapshot without widening the legacy HTTP request.
pub(crate) fn agent_turn_inner_with_control(
    state: &AppState,
    req: &AgentTurnReq,
    control: TurnControl,
) -> Result<Event, (StatusCode, String)> {
    turn_holding(state, req, control, None)
}

/// A turn, on the agent's turn lease its caller already holds when `held`
/// (a resume's claim): kept from that claim to the turn's end, so no queued
/// turn can take the agent in between.
fn turn_holding(
    state: &AppState,
    req: &AgentTurnReq,
    control: TurnControl,
    held: Option<crate::state::TurnLease>,
) -> Result<Event, (StatusCode, String)> {
    let target_path = req.file_path.as_deref().map(str::trim).unwrap_or("");

    // Resolve the stream BEFORE appending anything: `append` creates a stream
    // it has never seen, so a typo'd stream_id would otherwise mint a junk
    // stream out of a failed turn. No event is appended for THIS failure —
    // the turn never began.
    let ctx = context_for(state, &req.stream_id).map_err(|e| (StatusCode::NOT_FOUND, e))?;
    // Held for the whole turn, not just this check: a long Grok call must
    // not let discard rm the worktree out from under it (04c-BURN §4). A
    // discarded (or mid-discard) stream must not still take turns — its
    // worktree may already be gone, or about to be.
    let _activity = admit_activity(state, &req.stream_id)?;

    // Decided once, here, before the turn's own started-event is even
    // written — everything downstream (the event payload, the actor string
    // on every event this turn appends) uses this same value, never
    // re-derives it (06-SEATS audit item 2: "agent.turn.started includes
    // seat"; 07d-A1 §1: actor is the seat alone, never the model).
    let seat = req.seat.unwrap_or_else(|| default_seat_for(state, &ctx));
    let agent_id = crate::node::agent_id(&req.stream_id, seat);
    let selected_operating_mode = control
        .mode
        .or_else(|| agent_control::configured_operating_mode(state, &req.stream_id, &agent_id));
    let operating_mode = selected_operating_mode.unwrap_or_default();
    // The agent's own operating mode is the whole write policy. There is no
    // node-wide gate to defer to, so an agent with no recorded choice is
    // Manual and never auto-writes.
    let mode_allows_auto_edit = operating_mode.allows_auto_edit();

    // A dispatcher can observe "not paused" immediately before Stop lands.
    // Re-check at the last point before a queued item acquires a turn lease
    // or writes `agent.turn.started`; otherwise that tiny race would let a
    // paused follow-up begin after the operator explicitly stopped it.
    // Legacy/direct callers deliberately remain available: Stop pauses the
    // durable dispatch queue, not a later explicit API call.
    if control.queue_event_hash.is_some()
        && agent_control::dispatch_is_paused(state, &req.stream_id, &agent_id)
    {
        return Err((
            StatusCode::CONFLICT,
            "queued dispatch is paused; resume it before starting queued instructions".to_string(),
        ));
    }

    // 07d-A1 §4: Baker is factory-only — a project socket may never take a
    // turn as Baker, and the factory socket may never take one as an
    // ordinary project seat. `default_seat_for` already keeps the default
    // path honest; this also refuses an explicit `req.seat` that crosses
    // the boundary.
    let is_factory = is_baker_stream_ctx(state, &ctx);
    if is_factory != (seat == crate::node::Seat::Baker) {
        let msg = if is_factory {
            format!(
                "the {} seat cannot take a turn on the factory socket — only baker can",
                seat.as_str()
            )
        } else {
            "the baker seat cannot take a turn on a project socket".to_string()
        };
        return Err((StatusCode::BAD_REQUEST, msg));
    }

    // In Manual the agent's next turn waits until its last turn's changes
    // are decided (turn snapshots); a queued instruction stays queued.
    if operating_mode == OperatingMode::Manual {
        if let Some(why) = crate::snapshots::waiting_for_review(state, &req.stream_id, &agent_id) {
            return Err((StatusCode::CONFLICT, why));
        }
    }

    // Atomically admit the first turn for this exact agent; a second
    // request for the same agent is refused here, before `agent.turn.started`
    // is ever appended. The durable dispatcher retries after this lease
    // drops, so queued follow-ups never compete with an active turn. Held
    // for the whole turn (dropped when this function returns, any path) — 07d-A:
    // `/studio/state` projects this agent's `busy` fact from it. Never
    // blocks a different agent: the other configured agent on this socket
    // admits its own lease independently and both proceed (07d-A
    // "Concurrency").
    let _turn = match held {
        Some(lease) => lease,
        None => crate::state::admit_turn(state, &req.stream_id, seat)?,
    };

    // Minted once, here, before anything else — 07d-A5's correlation key
    // for every event and every activity broadcast this turn produces.
    // `start_agent_activity` seeds the live work-trace entry (phase
    // `ContactingModel`) and hands back the shared cancel flag threaded all
    // the way down to `chat_turn_streaming`.
    let turn_id = crate::node::mint_id("turn");
    let cancel = start_agent_activity(state, &req.stream_id, &agent_id, &turn_id);
    crate::state::set_turn_mode(state, &agent_id, operating_mode);

    // The seat's currently declared model, if the assignment record is
    // readable — independent of whether the model actually resolves (a
    // missing API key still has a known assigned id). Snapshotted once and
    // reused for both `agent.turn.started` and, on failure, as the
    // `agent.turn.failed` fallback (07d-A1 §1).
    let assigned_model_id = assigned_model_id_or_none(state, &req.stream_id, seat);

    // The human's words are part of the loop, not just its trigger. The
    // workpane rebuilds its transcript from the stream; without this event a
    // reload shows the agent answering nobody. Appended before any model call
    // so it is first on the wire on BOTH paths (direct `file_path` and the
    // tool loop) and survives a model that errors on the very first call.
    // Full instruction, not a summary: this is the human's own text.
    //
    // Actor here defaults to "human", not `agent:{seat}` — 07d-A1 §1's actor
    // rule is about WHO/WHAT the agent's own turn machinery is once it
    // starts running (tool.called/turn.done/exhausted/file.changed all get
    // `agent:{seat}`, stable across model reassignment). `actor` names the
    // literal initiator of THIS event; `agent_id` in the payload is what
    // owns the conversation regardless of actor. Every human-facing caller
    // leaves `req.actor` unset, so this stays "human" for them unchanged.
    // `task_driver.rs` (07d-B Phase 4) is the one caller that overrides it —
    // see `AgentTurnReq::actor`'s doc comment on why a driver-issued turn
    // must not misattribute itself to the operator.
    let turn_actor = req.actor.as_deref().unwrap_or("human");
    let started = serde_json::json!({
        "instruction": req.instruction,
        "file_path": if target_path.is_empty() { None } else { Some(target_path) },
        "mode": operating_mode,
        "seat": seat,
        "agent_id": agent_id,
        "model_id": assigned_model_id,
        "turn_id": turn_id,
        "attachment_ids": control.attachment_ids,
    })
    .to_string();
    // A queued message starts its turn only if the operator has not taken
    // it back meanwhile (`agent_control::withdraw`).
    let causality: Vec<String> = control.queue_event_hash.clone().into_iter().collect();
    let start = || append(state, &req.stream_id, turn_actor, "agent.turn.started", started, causality);
    match agent_control::consume_if_waiting(state, &req.stream_id, control.queue_event_hash.as_deref(), start) {
        Some(appended) => {
            appended.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
            // A message another conversation sent is taken up now: its
            // sender's conversation says so.
            crate::messages::taken_up(state, &req.stream_id, control.queue_event_hash.as_deref(), &turn_id);
        }
        None => {
            set_agent_phase(state, &agent_id, ActivityPhase::Cancelled);
            return Err((StatusCode::CONFLICT, "the queued message was taken back before its turn began".to_string()));
        }
    };
    // A Baker thread still titled "New thread" is named by its first message
    // (display only; a cheap no-op on every other stream).
    crate::baker_threads::maybe_autotitle(state, &req.stream_id, &req.instruction);
    // The workspace before and after this turn: whatever changes it comes
    // up for review. Taken only now, after the turn is on the stream: Git
    // reading the workspace takes a while, and until `agent.turn.started`
    // the message shows as waiting. Nothing has run in the workspace yet.
    // Declared after the lease above, so it drops first when the turn ends.
    let _snapshot = crate::snapshots::TurnSnapshot::begin(
        state,
        &ctx,
        seat,
        &turn_id,
        assigned_model_id.clone(),
        operating_mode,
    );

    // From here on, `agent.turn.started` is already on the stream — any error
    // must leave a trace of how the turn ended, not just silence.
    agent_turn_after_started(
        state,
        req,
        target_path,
        seat,
        &turn_id,
        &cancel,
        operating_mode,
        mode_allows_auto_edit,
        &control.attachment_ids,
    )
    .map_err(|t| {
        let TurnTerminal {
            phase,
            reason,
            tool_log,
            paths_hint,
            prior_accept_path,
            model_id,
        } = t;
        let returned_reason = reason.clone();
        // A model already resolved before this failure is "the model
        // actually used"; otherwise fall back to the seat's declared
        // assignment, or null when nothing is known (07d-A1 §1).
        let model_id = model_id.or_else(|| assigned_model_id.clone());
        // agent.tool.called events on the stream are authoritative; tools[] here
        // is a one-shot analytics projection: same fields as agent.turn.exhausted
        // minus `steps` (no step limit on failure; tools.len() is the count).
        let payload = serde_json::json!({
            "reason": reason,
            "phase": phase.as_str(),
            "instruction": crate::agent_loop::truncate(&req.instruction, 400),
            "tools": tool_log,
            "paths_hint": paths_hint,
            "prior_accept_path": prior_accept_path,
            "seat": seat,
            "agent_id": agent_id,
            "model_id": model_id,
            "turn_id": turn_id,
        })
        .to_string();
        // Best-effort: the real error below is what the caller sees either way,
        // and a broken Heart must not turn into a panic on top of the failure.
        //
        // Actor is "node", not `agent:{seat}`, for the same reason
        // `agent.turn.started` stays "human": actor names the initiator of
        // THIS event, and here that is the node's own error-handling path,
        // not the agent (a model that errored before producing a
        // structured reply never "authored" this record). `agent_id` in
        // the payload is what ties this failure to its conversation.
        let _ = append(
            state,
            &req.stream_id,
            "node",
            "agent.turn.failed",
            payload,
            vec![],
        );
        set_agent_phase(
            state,
            &agent_id,
            ActivityPhase::Error {
                reason: returned_reason.clone(),
            },
        );
        (StatusCode::INTERNAL_SERVER_ERROR, returned_reason)
    })
}

/// Where in the post-`agent.turn.started` pipeline a turn failed.
#[derive(Clone, Copy)]
pub(crate) enum TurnPhase {
    /// Path resolution / worktree-jail refusal, or the worktree is missing.
    Jail,
    /// The model call itself failed, or the model does not support the tool loop.
    Model,
    /// Failure while running the tool loop, other than the model call.
    Tools,
}

impl TurnPhase {
    fn as_str(self) -> &'static str {
        match self {
            TurnPhase::Jail => "jail",
            TurnPhase::Model => "model",
            TurnPhase::Tools => "tools",
        }
    }
}

/// Terminal failure context carried from `agent_tool_turn` /
/// `agent_turn_after_started` to `agent_turn_inner`.
///
/// `agent.tool.called` events on the stream are authoritative for the full
/// tool record; `tool_log` is a one-shot analytics projection for
/// Sentry/PostHog-class tooling — same shape as `agent.turn.exhausted`.
pub(crate) struct TurnTerminal {
    pub(crate) phase: TurnPhase,
    pub(crate) reason: String,
    /// Tool calls accumulated before failure (empty on pre-loop or direct-path errors).
    pub(crate) tool_log: Vec<serde_json::Value>,
    pub(crate) paths_hint: Vec<String>,
    pub(crate) prior_accept_path: Option<String>,
    /// The model actually used, when a model was resolved before this
    /// failure happened (07d-A1 §1: "every terminal success or failure
    /// records the model actually used"). `None` here means "not resolved
    /// yet" — the caller falls back to the seat's assigned id.
    pub(crate) model_id: Option<String>,
}

impl TurnTerminal {
    /// Pre-loop or direct-path failure: no tool analytics yet.
    fn early(phase: TurnPhase, reason: impl Into<String>) -> Self {
        Self {
            phase,
            reason: reason.into(),
            tool_log: vec![],
            paths_hint: vec![],
            prior_accept_path: None,
            model_id: None,
        }
    }

    /// Mid-loop failure: carry the analytics accumulated so far.
    fn with_analytics(
        phase: TurnPhase,
        reason: impl Into<String>,
        tool_log: &[serde_json::Value],
        paths_hint: &[String],
        prior_accept_path: Option<String>,
    ) -> Self {
        Self {
            phase,
            reason: reason.into(),
            tool_log: tool_log.to_vec(),
            paths_hint: paths_hint.to_vec(),
            prior_accept_path,
            model_id: None,
        }
    }

    /// The model was already resolved when this failure happened — record
    /// which one actually ran, not just the seat's declared assignment.
    fn with_model_id(mut self, model_id: impl Into<String>) -> Self {
        self.model_id = Some(model_id.into());
        self
    }
}

/// Whether this stream carries factory (Baker) authority — a durable server
/// fact (`socket.kind.recorded`), read fresh every call, never inferred
/// from the stream's name/branch/id (07d-A1 correction: any ordinary
/// project a human happened to name `baker-whatever` used to silently
/// inherit the Baker boundary). `baker_assign_inner` (`routes/baker.rs`) is
/// the only place that ever records `Factory`.
pub(crate) fn is_baker_stream_ctx(state: &AppState, ctx: &ActiveContext) -> bool {
    crate::state::socket_kind(&state.heart.lock().unwrap(), &ctx.stream_id)
        == crate::node::SocketKind::Factory
}

/// Measured ops for the **model** (write_mode, stream/gates counts, branch).
/// Never invents spend/email/tokens/cost. Never sends node_id/hostname to the
/// provider — that stays on local Heart audit (`baker.assignment.accepted`).
///
/// `is_permanent_baker` (callers pass the Baker's whole scope —
/// `crate::baker_ops::is_baker_scope_stream`, main conversation or thread)
/// additionally appends the declared op-registry block (07d-B Phase 2 item
/// 6) — never for an old `/baker/assign` branch-job socket, which is
/// `is_baker_stream_ctx` too but is NOT the Baker and gets none of these ops.
///
/// `ops_dispatchable` says whether THIS turn's transport can actually call
/// one of those ops (only `agent_tool_turn`'s native HTTP tool loop can,
/// today — it alone extends `decls` with `baker_op_tool_decls()`). When
/// `false` the registry names the ops but tells the model not to call them
/// (`agent_loop::format_baker_op_registry_unavailable`) — Phase 2 item 2's
/// "a plug that cannot deliver the required transport is honestly marked
/// unable … not silently degraded", applied to ACP and the direct-edit path,
/// neither of which ever reaches `dispatch_baker_op`.
pub(crate) fn baker_ops_for_turn(
    state: &AppState,
    stream_id: &str,
    branch: &str,
    is_permanent_baker: bool,
    ops_dispatchable: bool,
) -> String {
    // This conversation's own operating mode is its whole write policy (there
    // is no node-wide gate), so the Baker is told truthfully whether its edits
    // apply unattended — never a stale node-wide "manual" while it runs Auto.
    let write_mode = crate::agent_control::operating_mode_for(
        state,
        stream_id,
        &crate::node::agent_id(stream_id, crate::node::Seat::Baker),
    )
    .as_str();
    let stream_count = state
        .heart
        .lock()
        .unwrap()
        .list_streams()
        .map(|s| s.len())
        .unwrap_or(0);
    // Pending write proposals across all streams — real gate queue length.
    let pending_gates = gates_inner(state).map(|g| g.len()).unwrap_or(0);
    let mut out =
        crate::agent_loop::format_baker_ops(write_mode, stream_count, pending_gates, branch);
    if is_permanent_baker {
        // WIRE names, not the dotted spec `id`: this text tells the model
        // what it may actually call, and the callable name (`ToolDecl.name`
        // in `baker_op_tool_decls`) is the wire name — see `BakerOpSpec`'s
        // doc comment for why `.` never reaches a tool-calling wire.
        let names: Vec<String> = crate::api_tools::decls().into_iter().map(|d| d.name).collect();
        let op_ids: Vec<&str> = names.iter().map(String::as_str).collect();
        out.push('\n');
        if ops_dispatchable {
            out.push_str(&crate::agent_loop::format_baker_op_registry(&op_ids));
        } else {
            out.push_str(&crate::agent_loop::format_baker_op_registry_unavailable(
                &op_ids,
            ));
        }
    }
    out
}

/// `req.seat`'s default when the caller names none: `Baker` for a Baker
/// stream, else `Orchestrator` (STEERING.md § Baker / 06-SEATS.md §2).
pub(crate) fn default_seat_for(state: &AppState, ctx: &ActiveContext) -> crate::node::Seat {
    if is_baker_stream_ctx(state, ctx) {
        crate::node::Seat::Baker
    } else {
        crate::node::Seat::Orchestrator
    }
}

/// The seat's currently declared model id, or `None` when there is no
/// assignment record or it is unreadable (07d-A1 §1: "records the assigned
/// `model_id` when the assignment is readable"). Deliberately does not
/// distinguish "nothing assigned" from "assigned but unreadable" — both are
/// simply nothing to record here; `resolve_seat` is what fails closed loudly
/// on an unreadable record.
fn assigned_model_id_or_none(
    state: &AppState,
    stream_id: &str,
    seat: crate::node::Seat,
) -> Option<String> {
    crate::state::latest_seat_model_id(&state.heart.lock().unwrap(), stream_id, seat)
        .ok()
        .flatten()
}

/// Decision families carry no `agent_id` of their own — ownership follows
/// causality back to the tagged proposal/pending gate they decide (07d-A1
/// §2), mirroring the client's `eventsForAgent` (`transcript.ts`).
/// `agent.child_tool.decided` (`pipe.rs`) decides an
/// `agent.child_tool.pending` the exact same way `agent.edit.*` decides an
/// `agent.file.changed` proposal.
const DECISION_FAMILIES: &[&str] = &[
    "agent.edit.accepted",
    "agent.edit.rejected",
    "agent.edit.failed",
    "agent.child_tool.decided",
    "agent.ask.answered",
    APPROVAL_CANCELLED_FAMILY,
];

/// Explicit allowlist of families with no per-agent concept at all —
/// socket-wide facts (who this stream is, its config, its lifecycle) that
/// stay visible to every agent. 07d-A1 correction: this used to be the
/// *default* for anything not on a small agent-scoped allowlist, so a new
/// `agent.*` family accidentally left off that list silently leaked into
/// every agent's Chat and prompt instead of failing closed. Now it is the
/// only way in: everything else — including any family this build doesn't
/// know about yet — falls through to the strict, fail-closed `agent_id`
/// check below.
const SOCKET_GLOBAL_FAMILIES: &[&str] = &[
    "project.onboarded",
    "baker.assignment.accepted",
    "seat.model.assigned",
    "socket.kind.recorded",
    "socket.workspace.set",
    "terminal.prepared",
    "stream.lifetime.set",
    "stream.discarded",
    "stream.ownership.claimed",
    "policy.write_mode.set",
    "memory.written",
];

/// Does this event's payload carry exactly `agent_id`? Missing, non-string,
/// or unreadable (malformed JSON) all fail closed to `false` (07d-A1 §2) —
/// there is no production legacy history to grandfather in.
fn event_belongs_to_agent(payload: &str, agent_id: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(payload)
        .ok()
        .and_then(|v| {
            v.get("agent_id")
                .and_then(|a| a.as_str().map(|s| s == agent_id))
        })
        .unwrap_or(false)
}

/// Filter `events` (stream order) down to one agent's own thread (07d-A1
/// §2). A decision event (`DECISION_FAMILIES`) carries no `agent_id` of its
/// own — it belongs to this agent when its causality cites something
/// already owned by this agent, never by direct field match. Explicit
/// socket-global bookkeeping (`SOCKET_GLOBAL_FAMILIES`) passes through
/// unfiltered. Everything else — every agent-scoped family this build
/// knows about, and any future one it doesn't — requires an exact
/// `agent_id` match and fails closed without one: unknown agent families
/// default to excluded, never to leaked (07d-A1 correction).
fn events_owned_by_agent(events: Vec<Event>, agent_id: &str) -> Vec<Event> {
    let mut owned_hashes: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(events.len());
    for ev in events {
        if DECISION_FAMILIES.contains(&ev.family.as_str()) {
            if ev.causality.iter().any(|h| owned_hashes.contains(h)) {
                owned_hashes.insert(ev.event_hash.clone());
                out.push(ev);
            }
            continue;
        }
        if SOCKET_GLOBAL_FAMILIES.contains(&ev.family.as_str()) {
            out.push(ev);
            continue;
        }
        if event_belongs_to_agent(&ev.payload, agent_id) {
            owned_hashes.insert(ev.event_hash.clone());
            out.push(ev);
        }
    }
    out
}

/// Maps a resolved model down to a bare provider bucket for the
/// `agent.turn.usage` event's `provider` field (07d-A3D1 Phase C).
/// Structural, not name-sniffed (review fix): `Model::name()` for a plug is
/// an arbitrary operator-chosen id with no enforced naming convention —
/// `NodeConfig::validate_new_plug_id` only rejects an id that's empty,
/// reserved, or already claimed, never one lacking an `"acp"`/`"local:"`
/// prefix — so an openai-compat plug named e.g. `"acp-relay"` or an ACP
/// plug named `"codex"` would have been mislabeled by a
/// `starts_with("acp")`/`starts_with("local:")` guess. `transport()`
/// reliably tells ACP apart from every HTTP-speaking model; among HTTP
/// models, only the four built-ins have a name this node itself controls —
/// anything else reaching this point over HTTP is an openai-compat plug,
/// the only other way an arbitrary-named HTTP `Model` instance reaches a
/// seat.
fn provider_for(model_name: &str, transport: bakery_agent::Transport) -> &'static str {
    if transport == bakery_agent::Transport::Acp {
        return "acp";
    }
    match model_name {
        "claude" => "claude",
        "grok" => "grok",
        "gemini" => "gemini",
        "claude-code" => "claude-code",
        _ => "openai-compat",
    }
}

/// Best-effort `agent.turn.usage` companion to `agent.turn.done` (07d-A3D1
/// Phase C) — appended only when `model` reported at least one usage field;
/// a missing/failed append never fails an otherwise-successful turn (`let _
/// =`), and nothing here ever touches `agent.turn.failed`/`.cancelled`,
/// only the normal-completion path.
fn append_turn_usage_if_any(
    state: &AppState,
    req: &AgentTurnReq,
    actor: &str,
    agent_id: &str,
    turn_id: &str,
    model: &Arc<dyn Model>,
) {
    let provider = provider_for(model.name(), model.transport());
    // Same blank/whitespace-only-means-"no override" normalization
    // `resolve_seat_with_override` already applies before deciding whether
    // an override actually took effect (07d-A3D1 Phase C review fix) — a
    // raw, untrimmed `req.model_id` here would both miss the catalog cache
    // on a real trimmed id and misreport a blank string as the model that
    // ran in the persisted `agent.turn.usage` event, even though the seat's
    // real base model is what actually executed the turn.
    let model_id_override = crate::state::normalize_override(req.model_id.as_deref());
    let effort_override = crate::state::normalize_override(req.effort.as_deref());
    let detail = model.last_turn_usage_detail();
    // A runtime that reads out its own context keeps that reading; the
    // catalog lookup below only stands in for one that doesn't.
    let runtime_context = detail.as_ref().is_some_and(|d| d.context_window.is_some());
    let Some(mut usage) = compose_turn_usage(
        state,
        provider,
        model_id_override,
        model.last_turn_usage(),
        detail,
    ) else {
        return;
    };
    if let Some(model_id) = model_id_override.filter(|_| !runtime_context) {
        usage.context_window =
            crate::account_adapter::cached_context_window(state, model.name(), provider, model_id);
        usage.context_used = match (usage.tokens_in, usage.tokens_out) {
            (Some(input), Some(output)) => Some(input + output),
            (Some(input), None) => Some(input),
            (None, Some(output)) => Some(output),
            (None, None) => None,
        };
    }
    usage.duration_ms = turn_duration_ms(state, agent_id, turn_id);
    let payload = serde_json::json!({
        "agent_id": agent_id,
        "provider": provider,
        "model_id": model_id_override.map(str::to_string).unwrap_or_else(|| model.name().to_string()),
        "effort": effort_override,
        "tokens_in": usage.tokens_in,
        "tokens_out": usage.tokens_out,
        "context_used": usage.context_used,
        "context_window": usage.context_window,
        "thought_tokens": usage.thought_tokens,
        "cache_read_tokens": usage.cache_read_tokens,
        "cache_write_tokens": usage.cache_write_tokens,
        "cost": usage.cost,
        "duration_ms": usage.duration_ms,
        "turn_id": turn_id,
    })
    .to_string();
    let _ = append(
        state,
        &req.stream_id,
        actor,
        "agent.turn.usage",
        payload,
        vec![],
    );
    record_turn_usage(state, agent_id, usage);
}

/// How long `turn_id` has run, from its live activity's start — `None` when
/// no activity tracks that turn.
fn turn_duration_ms(state: &AppState, agent_id: &str, turn_id: &str) -> Option<u64> {
    let activity = crate::state::agent_activity_snapshot(state, agent_id)
        .filter(|a| a.turn_id == turn_id)?;
    let started = chrono::DateTime::parse_from_rfc3339(&activity.turn_started_at).ok()?;
    u64::try_from((chrono::Utc::now() - started.with_timezone(&chrono::Utc)).num_milliseconds())
        .ok()
}

/// A seat's configured model (optionally overridden for this one call —
/// 07d-A3D1 Phase C), or a Setup-shaped failure — never a node-global
/// fallback (STEERING.md § Models). `seat` is decided once, by
/// `agent_turn_inner`, before `agent.turn.started` is even appended (so the
/// event can record it — 06-SEATS audit item 2), and threaded through from
/// there; this only resolves the model for that already-chosen seat.
fn resolve_model_for_seat(
    state: &AppState,
    stream_id: &str,
    seat: crate::node::Seat,
    model_id_override: Option<&str>,
) -> Result<Arc<dyn Model>, TurnTerminal> {
    resolve_seat_with_override(state, stream_id, seat, model_id_override)
        .map_err(|e| TurnTerminal::early(TurnPhase::Model, e))
}

/// Everything after `agent.turn.started`. Errors carry a `TurnTerminal` so
/// `agent_turn_inner` can append a fully annotated `agent.turn.failed`.
/// What changed around this agent since its previous turn began, one line
/// each: every file the operator saved in the Bakery (E-4), and every other
/// thread on its repository now changing the same files (git rule 5). None
/// before its first turn.
fn operator_edits_note(state: &AppState, stream_id: &str, agent_id: &str, turn_id: &str) -> Option<String> {
    let events = state.heart.lock().unwrap().read_stream(stream_id).ok()?;
    let payload = |e: &Event| serde_json::from_str::<serde_json::Value>(&e.payload).unwrap_or_default();
    // A command's turn carried no notes (it goes as typed), so it is no
    // "last turn" here.
    let previous = events.iter().rposition(|e| {
        let p = payload(e);
        e.family == "agent.turn.started"
            && p["agent_id"] == agent_id
            && p["turn_id"] != turn_id
            && !crate::pipe::is_command(p["instruction"].as_str().unwrap_or_default())
    })?;
    let mut files: Vec<String> = Vec::new();
    for e in events[previous + 1..].iter().filter(|e| e.family == crate::routes::project::OPERATOR_EDIT_FAMILY) {
        if let Some(file) = payload(e)["file_path"].as_str().filter(|f| !files.iter().any(|seen| seen == f)) {
            files.push(file.to_string());
        }
    }
    let mut lines: Vec<String> = files.iter().map(|f| format!("The operator edited {f} since your last turn.")).collect();
    lines.extend(crate::overlaps::note_since(state, &events, previous + 1));
    (!lines.is_empty()).then(|| lines.join("\n"))
}

/// What opens an agent's instruction this turn: [`operator_edits_note`],
/// then, while the operator runs commands themselves, that rule. Plan runs
/// no commands at all. Then the work already waiting to merge that overlaps
/// it, then the notes other conversations left it since its last turn
/// (`messages::reading`), which the caller marks read once the turn is under
/// way. Only a turn whose opening carries it calls this.
fn turn_notes(state: &AppState, stream_id: &str, agent_id: &str, turn_id: &str, _mode: OperatingMode) -> (Option<String>, Option<crate::messages::Reading>) {
    let notes: Vec<String> = operator_edits_note(state, stream_id, agent_id, turn_id)
        .into_iter()
        .chain(crate::baker_threads::already_waiting_note(state, stream_id))
        .collect();
    let notes = (!notes.is_empty()).then(|| notes.join("\n"));
    let left = crate::messages::reading(state, stream_id, agent_id);
    let text = match (notes, left.as_ref()) {
        (Some(notes), Some(left)) => Some(format!("{notes}\n\n{}", left.text)),
        (notes, left) => notes.or(left.map(|l| l.text.clone())),
    };
    (text, left)
}

pub(crate) fn agent_turn_after_started(
    state: &AppState,
    req: &AgentTurnReq,
    target_path: &str,
    seat: crate::node::Seat,
    turn_id: &str,
    cancel: &Arc<AtomicBool>,
    operating_mode: OperatingMode,
    mode_allows_auto_edit: bool,
    attachment_ids: &[String],
) -> Result<Event, TurnTerminal> {
    // Resolved once, here, so every branch below shares the exact same
    // model instance — an ACP plug's `chat()` spawns/reuses its boxed
    // session as a side effect, so resolving twice would be wasteful, and
    // only this one resolution's `transport()` decides dispatch.
    // The model and effort are the selector's. A native session holds its
    // own (the picker sets them there, and a new session restores them), so
    // nothing is sent with the turn; the Bakery's own loop runs on the
    // thread's choice.
    let base = resolve_model_for_seat(state, &req.stream_id, seat, None)?;
    let chosen = match base.transport() {
        bakery_agent::Transport::Acp => crate::selection::Selection::default(),
        _ => crate::selection::chosen(state, &req.stream_id, seat),
    };
    let model = match chosen.model.as_deref() {
        Some(_) => resolve_model_for_seat(state, &req.stream_id, seat, chosen.model.as_deref())?,
        None => base,
    };
    // Called on every turn, Some or None, so a cached model instance never
    // keeps an earlier choice.
    model.set_effort(chosen.effort.as_deref());
    let selected = AgentTurnReq { model_id: chosen.model, effort: chosen.effort, ..req.clone() };
    let req = &selected;
    // The operator's own saves since this agent's last turn open its
    // instruction (E-4), so it neither undoes nor redoes them, and so does
    // the rule while the operator runs commands themselves. A resumed
    // turn's briefing stays as it is.
    let agent_id = crate::node::agent_id(&req.stream_id, seat);
    // A command (`/compact`, a skill) reaches the runtime exactly as typed:
    // nothing opens it and nothing rides after it. The next message opens
    // with all of it, the operator's edits since the last such one among it,
    // and the notes left meanwhile: only a turn that carries them reads them.
    let command = crate::pipe::is_command(&req.instruction);
    // The conversation's brief opens every turn, a resumed one's too.
    let (notes, reading) = match command || req.instruction.starts_with(RESUME_PREFIX) {
        true => (None, None),
        false => turn_notes(state, &req.stream_id, &agent_id, turn_id, operating_mode),
    };
    // The notes are read once the turn's setup is done and nothing stopped
    // it: a turn that ends before its prompt goes out leaves them unread.
    let read_notes = |stream_id: &str| {
        if let Some(reading) = reading {
            if !cancel.load(Ordering::Acquire) {
                reading.mark(state, stream_id, &agent_id, turn_id);
            }
        }
    };
    // A Claude seat reads the toolkit's instructions and skills from its own
    // config (`claude_native::install_toolkit_files`), as Claude Code does.
    let claude_reads_its_own = model.name().eq_ignore_ascii_case(crate::claude_native::ACCOUNT_ID);
    let opening: Vec<String> = match command {
        true => Vec::new(),
        false => crate::briefs::prompt_line(state, &req.stream_id)
            .into_iter()
            .chain((!claude_reads_its_own).then(|| crate::toolkit::prompt_block(state, &req.stream_id)).flatten())
            .chain(notes)
            .collect(),
    };
    // The notes the operator's words match ride on the message; a resumed
    // turn's are on it already.
    let matching = match command || req.instruction.starts_with(RESUME_PREFIX) {
        true => None,
        false => crate::memory_index::matching(state, &req.stream_id, &req.instruction),
    };
    if let Some((_, ids)) = &matching {
        let used = serde_json::json!({"turn_id": turn_id, "ids": ids}).to_string();
        let _ = crate::state::append(state, &req.stream_id, &format!("agent:{agent_id}"), crate::memory_index::USED, used, vec![]);
    }
    let noted;
    let req = if opening.is_empty() && matching.is_none() {
        req
    } else {
        let riding = matching.as_ref().map(|(text, _)| text.as_str());
        let instruction = opening.iter().map(String::as_str).chain([req.instruction.as_str()]).chain(riding).collect::<Vec<_>>();
        noted = AgentTurnReq { instruction: instruction.join("\n\n"), ..req.clone() };
        &noted
    };

    if model.transport() == bakery_agent::Transport::Acp {
        // Native modes are real adapter controls.  Apply the queued mode
        // snapshot and wait for acknowledgement before the prompt; absence
        // means unsupported for this session, never "ACP cannot do it".
        model
            .set_native_operating_mode(Some(operating_mode.as_str()))
            .map_err(|e| TurnTerminal::early(TurnPhase::Model, e).with_model_id(model.name()))?;
        let agent_id = crate::node::agent_id(&req.stream_id, seat);
        let attachments = crate::attachments::native_attachments_for_turn(
            state,
            &req.stream_id,
            &agent_id,
            attachment_ids,
        )
        .map_err(|e| TurnTerminal::early(TurnPhase::Tools, e).with_model_id(model.name()))?;
        model
            .set_native_attachments(attachments)
            .map_err(|e| TurnTerminal::early(TurnPhase::Model, e).with_model_id(model.name()))?;
        read_notes(&req.stream_id);
        // 07d-A3B "Runtime law": ACP already runs its own native tool loop
        // inside the boxed CLI child — never wrap its output as Bakery's
        // 8-step JSON tool-loop protocol. And never fall into the direct
        // `propose_edit` path below either (07d-A3B review #3: any
        // nonempty `file_path` used to bypass ACP dispatch entirely and
        // hit `AcpPipeModel::propose_edit`, which explicitly rejects it —
        // the Studio "+ file" control was incompatible with ACP seats). A
        // selected file becomes part of the turn's domain envelope
        // instead of a different dispatch path.
        return agent_acp_turn(state, req, seat, model, target_path, turn_id, cancel);
    }

    // A selected file is CONTEXT for the normal agent loop, never a
    // dispatch switch (Checkpoint 2 item 1). The legacy single-shot
    // `propose_edit` path that used to live here — no system prompt, no
    // persona, no hot memory, no tools, one blind whole-file rewrite — is
    // gone: ACP has treated selection as envelope context since 07d-A3B,
    // and HTTP now matches. `agent_tool_turn` folds `req.file_path` into
    // the opening message's hint and the model reads/edits it through its
    // ordinary jailed tools. The selection is still jail-checked up front,
    // so an escaping path is refused at the door rather than smuggled into
    // the prompt as a "file" the tools would refuse one step later.
    if !target_path.is_empty() {
        // Rootless sockets (chat-only Baker/task) have no jail to check a
        // selection against — refuse with the same honesty the rootless
        // no-selection path gets, never a raw canonicalize error (review
        // finding A).
        let has_root = context_for(state, &req.stream_id)
            .map(|c| !c.worktree_path.trim().is_empty())
            .unwrap_or(false);
        if !has_root {
            return Err(TurnTerminal::early(
                TurnPhase::Jail,
                format!("a file was selected ({target_path}) but this socket has no workspace root — file tools are not available here"),
            ));
        }
        let jail =
            jail_for(state, &req.stream_id).map_err(|e| TurnTerminal::early(TurnPhase::Jail, e))?;
        jail.resolve(target_path)
            .map_err(|e| TurnTerminal::early(TurnPhase::Jail, e))?;
    }
    read_notes(&req.stream_id);
    agent_tool_turn(
        state,
        req,
        seat,
        model,
        turn_id,
        cancel,
        operating_mode,
        mode_allows_auto_edit,
        attachment_ids,
    )
}

/// A compact per-turn grounding header for ACP prompts (07d-A3B review: "A
/// domain envelope: project, roots, socket, cwd, role, selected file,
/// recent agent history") — the CLI's own prompt has no other way to learn
/// which socket/seat/file this turn is about; Bakery's harness does. Cheap
/// enough to repeat on every prompt, unlike catch-up history, which only
/// needs replaying once per freshly (re)spawned session (see
/// `pipe::acp_catchup_context` — that half lives in pipe.rs because only
/// the session thread knows whether this is a fresh process).
/// The branch the checkout is on NOW. `ctx.branch` is what `project.onboarded`
/// recorded at mint time — for an in-place socket (the Baker's own, bound to
/// the operator's checkout) that goes stale the moment the operator switches
/// branches, and the agent then confidently claims the wrong one.
fn live_branch(ctx: &ActiveContext) -> String {
    if ctx.in_place && !ctx.worktree_path.trim().is_empty() {
        if let Some(branch) = crate::git::inspect(&ctx.worktree_path).current_branch {
            return branch;
        }
    }
    ctx.branch.clone()
}

fn acp_domain_envelope(
    state: &AppState,
    ctx: &ActiveContext,
    seat: crate::node::Seat,
    target_path: &str,
) -> String {
    let (project_id, root_id) =
        crate::state::socket_project(&state.heart.lock().unwrap(), &ctx.stream_id);
    let mut lines = vec![
        format!("project: {}", ctx.project),
        format!("branch: {}", live_branch(ctx)),
        format!("cwd: {}", ctx.worktree_path),
        format!("role: {}", seat.as_str()),
    ];
    if let Some(project_id) = project_id {
        lines.push(format!("project_id: {project_id}"));
    }
    if let Some(root_id) = root_id {
        lines.push(format!("root_id: {root_id}"));
    }
    if !target_path.is_empty() {
        lines.push(format!("selected_file: {target_path}"));
    }
    if let Some(access) = crate::machine_access::session_line(state, &ctx.stream_id) {
        lines.push(access);
    }
    // The memory's index opens a native session once, as it starts
    // (`pipe::session_main`), so it stays the same for the session; the
    // notes a turn's words match ride on each message
    // (`agent_turn_after_started`).
    format!(
        "{}\n[Bakery session]\n{}\n\n",
        crate::agent_loop::persona_for_agent(state, &ctx.stream_id, seat),
        lines.join("\n")
    )
}

/// One turn on an ACP-transport seat (07d-A3B): the boxed CLI child already
/// ran its own native tool loop inside this one prompt/reply — a single
/// `chat()` call, and its raw text becomes the turn's summary directly.
/// Never routed through [`crate::agent_loop::parse_action`]; that JSON
/// tool-loop protocol is an HTTP-seat concern only (STEERING.md § Models:
/// "Do not wrap ACP output as the 8-step JSON protocol"). `target_path` is
/// the Studio "+ file" selection, if any — folded into the domain envelope
/// rather than routed to `propose_edit`, which ACP explicitly rejects.
fn agent_acp_turn(
    state: &AppState,
    req: &AgentTurnReq,
    seat: crate::node::Seat,
    model: Arc<dyn Model>,
    target_path: &str,
    turn_id: &str,
    cancel: &Arc<AtomicBool>,
) -> Result<Event, TurnTerminal> {
    let ctx =
        context_for(state, &req.stream_id).map_err(|e| TurnTerminal::early(TurnPhase::Jail, e))?;
    let actor = format!("agent:{}", seat.as_str());
    let agent_id = crate::node::agent_id(&req.stream_id, seat);

    let mut envelope = acp_domain_envelope(state, &ctx, seat, target_path);
    if is_baker_stream_ctx(state, &ctx) {
        let is_baker_scope = crate::baker_ops::is_baker_scope_stream(state, &req.stream_id);
        // An ACP session on a Baker conversation is spawned with Bakery's
        // own MCP server (`state::mcp_spawn_config_for`, BakerOps scope), and
        // a plug that cannot carry MCP is refused before it gets here — so
        // the declared ops ARE callable on this transport, as
        // `mcp__bakery__<op>`. Telling the model otherwise (the old
        // `ops_dispatchable: false`, S0 row 20) hid its whole toolbox.
        let ops_dispatchable = is_baker_scope
            && crate::mcp::mcp_scope_for_stream(state, &req.stream_id)
                == Some(crate::mcp::McpScope::BakerOps);
        envelope.push_str(&baker_ops_for_turn(
            state,
            &req.stream_id,
            &live_branch(&ctx),
            is_baker_scope,
            ops_dispatchable,
        ));
        if ops_dispatchable {
            if let Some(note) = crate::baker_threads::conversation_note(state, &req.stream_id) {
                envelope.push_str(&note);
            }
        }
        envelope.push('\n');
    }
    // A command (`/compact`, `/context`, a skill) reaches the runtime exactly
    // as typed: nothing goes in front of it.
    let command = crate::pipe::is_command(&req.instruction);
    let mut prompt = if command { req.instruction.clone() } else { format!("{envelope}{}", req.instruction) };

    // ACP already runs its own native tool loop inside the boxed CLI child
    // — a single blocking `chat()` call, with no per-step granularity this
    // side can observe. `ContactingModel` for its whole duration is the
    // most honest phase available here.
    set_agent_phase(state, &agent_id, ActivityPhase::ContactingModel);
    // 07d-A5 review #5: lets a gate/tool-call callback that lands on the
    // ACP session's own background task (outside this call stack) know
    // which turn is current — a no-op for every other transport.
    model.set_current_turn_id(turn_id);

    // 07d-A5 review #4: an ACP turn is one opaque blocking `chat()` call
    // with no per-step checkpoint this side can observe (the boxed CLI
    // child runs its own native tool loop) — the only points cancellation
    // can be honestly acted on are immediately before the call starts and
    // immediately after it returns. This does not forcibly interrupt an
    // already-in-flight call (same documented limit as an already-
    // dispatched HTTP-seat tool subprocess that keeps running after
    // cancel), but it stops `POST /agent/turn/cancel` from being a
    // complete no-op for this transport, and it stops a cancel that landed
    // mid-call from being reported as a successful `agent.turn.done`.
    if cancel.load(Ordering::Acquire) {
        return record_acp_turn_cancelled(state, req, &agent_id, turn_id, model.name());
    }

    let (mut tries, mut crashes) = (0, 0);
    let stream_end = loop {
        let attempt = model.chat_turn_streaming(
            "",
            &[bakery_agent::ChatMsg::user(prompt.clone())],
            &[],
            // The session's notification handler paints the live text itself,
            // in order with tool calls (pipe.rs) — never from this callback.
            &mut |_| {},
            cancel,
        );
        // A passing failure (the network, the provider's load), or the
        // runtime's process dying, is waited out inside the turn.
        let why = match &attempt {
            Err(e) => e.clone(),
            Ok(StreamEnd::Stopped {
                reason: StopReason::StreamDied(reason),
                ..
            }) => format!("ACP stream ended early: {reason}"),
            Ok(_) => break attempt,
        };
        match crate::resume::cut(&why) {
            Some(crate::resume::Cut::Passing) => {}
            Some(crate::resume::Cut::Crashed) if crashes < crate::resume::CRASH_TRIES => crashes += 1,
            Some(crate::resume::Cut::SignedOut) => break Err(crate::resume::SIGNED_OUT.to_string()),
            _ => break attempt,
        }
        let stopped = crate::resume::wait_out(
            state,
            &req.stream_id,
            &agent_id,
            turn_id,
            model.name(),
            &why,
            tries,
            cancel,
        );
        if stopped {
            return record_acp_turn_cancelled(state, req, &agent_id, turn_id, model.name());
        }
        tries += 1;
        // The failure took the runtime's process with it (pipe.rs ends a
        // session on a failed prompt), so the next one is told this very
        // turn from its record, and carries it on.
        let header = format!(
            "{RESUME_PREFIX} a cut-off ({}): this same turn stopped partway, and the Bakery waited it out. Its durable record follows.",
            crate::agent_loop::clip_head(why.trim(), 300)
        );
        let briefing = turn_briefing(state, &req.stream_id, turn_id, &req.instruction, &header)
            .unwrap_or_else(|_| req.instruction.clone());
        prompt = if command { req.instruction.clone() } else { format!("{envelope}{briefing}") };
    }
    .map_err(|e| {
            set_agent_phase(state, &agent_id, ActivityPhase::Error { reason: e.clone() });
            TurnTerminal::early(TurnPhase::Model, e).with_model_id(model.name())
        })?;

    let summary = match stream_end {
        StreamEnd::Completed(ChatReply::Text(text)) => text,
        StreamEnd::Stopped {
            reason: StopReason::Cancelled,
            ..
        } => {
            return record_acp_turn_cancelled(state, req, &agent_id, turn_id, model.name());
        }
        StreamEnd::Stopped {
            reason: StopReason::StreamDied(reason),
            ..
        } => {
            return Err(TurnTerminal::early(
                TurnPhase::Model,
                format!("ACP stream ended early: {reason}"),
            )
            .with_model_id(model.name()));
        }
        StreamEnd::Completed(ChatReply::ToolCalls(_)) => {
            return Err(TurnTerminal::early(
                TurnPhase::Model,
                "ACP native runtime returned an HTTP tool-loop envelope",
            )
            .with_model_id(model.name()));
        }
    };

    if cancel.load(Ordering::Acquire) {
        return record_acp_turn_cancelled(state, req, &agent_id, turn_id, model.name());
    }

    // Durable full text (07d-A3B review #1: "the assistant response is
    // permanently truncated to 1,200 characters") — the HTTP tool loop's
    // `MAX_EVENT_SUMMARY` truncation exists because that protocol asks the
    // model for a short summary by convention; an ACP reply IS the whole
    // answer, kept whole (pipe.rs never cuts a reply), so truncating it here
    // would be pure data loss, not protocol hygiene.
    let payload = serde_json::json!({
        "summary": summary,
        "agent_id": agent_id,
        "model_id": model.name(),
        "turn_id": turn_id,
    })
    .to_string();
    let ev = append(
        state,
        &req.stream_id,
        &actor,
        "agent.turn.done",
        payload,
        vec![],
    )
    .map_err(|e| TurnTerminal::early(TurnPhase::Tools, e).with_model_id(model.name()))?;
    append_turn_usage_if_any(state, req, &actor, &agent_id, turn_id, &model);
    set_agent_phase(state, &agent_id, ActivityPhase::Complete { note: None });
    Ok(ev)
}

/// ACP-transport analogue of [`record_turn_cancelled`] — no step/tool_log/
/// paths_hint/prior_accept_path exist for this transport (a single opaque
/// `chat()` call, not the HTTP tool loop), so the payload is deliberately
/// smaller.
fn record_acp_turn_cancelled(
    state: &AppState,
    req: &AgentTurnReq,
    agent_id: &str,
    turn_id: &str,
    model_name: &str,
) -> Result<Event, TurnTerminal> {
    let payload = serde_json::json!({
        "reason": "cancelled by human",
        "instruction": crate::agent_loop::truncate(&req.instruction, 400),
        "agent_id": agent_id,
        "model_id": model_name,
        "turn_id": turn_id,
    })
    .to_string();
    let ev = append(
        state,
        &req.stream_id,
        "human",
        "agent.turn.cancelled",
        payload,
        vec![],
    )
    .map_err(|e| TurnTerminal::early(TurnPhase::Model, e).with_model_id(model_name))?;
    set_agent_phase(state, agent_id, ActivityPhase::Cancelled);
    Ok(ev)
}

/// Native tool loop: explore until completion, approval, cancellation, error,
/// or an explicitly configured operator budget. Context compacts independently.
/// HTTP-shaped seats only (plain HTTP providers and openai-compat plugs,
/// which speak the same `chat()`-returns-JSON-action contract); an
/// ACP-transport seat never reaches here — see [`agent_acp_turn`].
/// Cheap, honest, cosmetic-only heuristic (07d-A5) — never a claim that
/// tests actually ran; a mislabel only ever affects the live activity
/// strip's wording, nothing load-bearing. Only applies to the shell-out
/// tools (`run`/`bash`); every other tool always reports `ToolRunKind::Tool`.
fn looks_like_test_run(tool: &str, args: &serde_json::Value) -> bool {
    if tool != "run" && tool != "bash" {
        return false;
    }
    let Some(argv) = args.get("argv").and_then(|v| v.as_array()) else {
        return false;
    };
    let joined = argv
        .iter()
        .filter_map(|v| v.as_str())
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    const MARKERS: &[&str] = &[
        "cargo test",
        "pytest",
        "npm test",
        "npm run test",
        "pnpm test",
        "pnpm run test",
        "yarn test",
        "go test",
        "jest",
        "vitest",
        "rspec",
        "phpunit",
        "ctest",
        "mvn test",
        "gradle test",
    ];
    MARKERS.iter().any(|m| joined.contains(m))
}

/// Appends `agent.turn.cancelled` (actor `"human"` — the initiator of the
/// stop, not the machinery) and sets the live activity phase to
/// `Cancelled`. Shared by every point in `agent_tool_turn` where a human's
/// Cancel request is observed: mid-stream (inside `chat_turn_streaming`'s
/// own cancel-aware read loop, surfaced here as `StopReason::Cancelled`) or
/// between steps/tool calls (checked directly in the loop below, since
/// streaming cancellation only covers the model call itself — 07d-A5's
/// documented limitation: an already-dispatched tool subprocess still runs
/// to completion).
fn record_turn_cancelled(
    state: &AppState,
    req: &AgentTurnReq,
    agent_id: &str,
    turn_id: &str,
    model_name: &str,
    step: usize,
    tool_log: &[serde_json::Value],
    paths_hint: &[String],
    prior_accept_path: Option<String>,
) -> Result<Event, TurnTerminal> {
    let payload = serde_json::json!({
        "reason": "cancelled by human",
        "steps": step,
        "instruction": crate::agent_loop::truncate(&req.instruction, 400),
        "tools": tool_log,
        "paths_hint": paths_hint,
        "prior_accept_path": prior_accept_path,
        "agent_id": agent_id,
        "model_id": model_name,
        "turn_id": turn_id,
    })
    .to_string();
    let ev = append(
        state,
        &req.stream_id,
        "human",
        "agent.turn.cancelled",
        payload,
        vec![],
    )
    .map_err(|e| {
        TurnTerminal::with_analytics(
            TurnPhase::Tools,
            e,
            tool_log,
            paths_hint,
            prior_accept_path.clone(),
        )
        .with_model_id(model_name)
    })?;
    set_agent_phase(state, agent_id, ActivityPhase::Cancelled);
    Ok(ev)
}

/// What a call Stop caught before it ran says, in its record and its row.
pub(crate) const STOPPED_BEFORE_IT_RAN: &str = "stopped before it ran";

/// The calls the model asked for that Stop caught before they ran (at the
/// step's boundary, or at a command's Allow card): each is an
/// `agent.tool.called` with `stopped: "before"`, so the thread shows what was
/// asked and that it never ran, never a failure.
#[allow(clippy::too_many_arguments)]
fn record_unrun_calls(
    state: &AppState,
    stream_id: &str,
    actor: &str,
    agent_id: &str,
    model_name: &str,
    turn_id: &str,
    step: usize,
    calls: &[bakery_agent::ToolCall],
) {
    for call in calls {
        let payload = serde_json::json!({
            "tool": call.name,
            "args": call.arguments,
            "ok": false,
            "stopped": "before",
            "step": step,
            "summary": STOPPED_BEFORE_IT_RAN,
            "agent_id": agent_id,
            "model_id": model_name,
            "turn_id": turn_id,
            "call_id": crate::node::mint_id("call"),
        });
        let _ = append(state, stream_id, actor, "agent.tool.called", payload.to_string(), vec![]);
    }
}

/// Attached files, framed the same way for a turn's opening and for a steer:
/// the text the model reads, carrying the files it reads as themselves.
fn attachment_message(attachments: crate::attachments::TurnAttachments) -> bakery_agent::ChatMsg {
    bakery_agent::ChatMsg::user_with_attachments(
        format!("{}{}", crate::attachments::ATTACHED_PREAMBLE, attachments.text),
        attachments.native,
    )
}

/// What a steer injects into a running HTTP turn: the instruction, then its
/// attachments exactly as a normal turn would have opened with them.
fn push_steer(
    messages: &mut Vec<bakery_agent::ChatMsg>,
    steer: &str,
    attachments: crate::attachments::TurnAttachments,
) {
    messages.push(bakery_agent::ChatMsg::user(format!(
        "The human changed the direction of this active turn. Follow this instruction now; do not execute stale planned actions: {steer}"
    )));
    if !attachments.is_empty() {
        messages.push(attachment_message(attachments));
    }
}

pub(crate) fn agent_tool_turn(
    state: &AppState,
    req: &AgentTurnReq,
    seat: crate::node::Seat,
    model: Arc<dyn Model>,
    turn_id: &str,
    cancel: &Arc<AtomicBool>,
    operating_mode: OperatingMode,
    mode_allows_auto_edit: bool,
    attachment_ids: &[String],
) -> Result<Event, TurnTerminal> {
    let ctx =
        context_for(state, &req.stream_id).map_err(|e| TurnTerminal::early(TurnPhase::Jail, e))?;
    let toolchain = crate::catalog::stacks_for_socket(state, &req.stream_id).toolchain;
    // 07d-B Phase 2 item 3: a rootless socket (`worktree_path == ""` — a
    // chat-only Baker, a chat-only task, or any future chat-only project
    // socket) degrades honestly to ops+chat with no file tools, instead of
    // erroring at `TurnPhase::Jail` before the model is ever contacted. The
    // jail is `None` here, never a `Jail::new` call against an empty path.
    let jail: Option<crate::tools::Jail> = if ctx.worktree_path.trim().is_empty() {
        None
    } else {
        Some(
            crate::tools::Jail::new(std::path::Path::new(&ctx.worktree_path))
                .map_err(|e| TurnTerminal::early(TurnPhase::Jail, e))?
                .with_exec(
                    state.clone(),
                    req.stream_id.clone(),
                    crate::state::cache_root(state),
                    toolchain,
                ),
        )
    };
    // Authorization is the Baker's own scope — the permanent Baker or one of
    // its threads — never the kind bit alone (07d-B Phase 2 item 2): an old
    // `/baker/assign` branch-job socket is `is_baker_stream_ctx` too but
    // gets none of these ops.
    let is_baker_scope = crate::baker_ops::is_baker_scope_stream(state, &req.stream_id);
    // Worker-scoped completion tool (Stage B item 4): advertised only on a
    // task socket (`socket.task.set` present), never on the permanent
    // Baker's own stream — the two are mutually exclusive by construction
    // (a task socket is `SocketKind::Task`, the Baker's is `Factory`), but
    // the check is by fact, not by inference.
    let task_id_for_persona =
        crate::state::socket_task_id(&state.heart.lock().unwrap(), &req.stream_id);
    let is_task_socket = task_id_for_persona.is_some();
    let operator_tools = crate::operator_tools::load_from_default_dir().map_err(|e| {
        let home = std::env::var("HOME").unwrap_or_default();
        let msg = if home.len() > 1 {
            e.replace(&home, "~")
        } else {
            e
        };
        TurnTerminal::early(
            TurnPhase::Tools,
            format!("Setup needed: operator tools registry: {msg}"),
        )
    })?;
    if !model.supports_chat() {
        return Err(TurnTerminal::early(
            TurnPhase::Model,
            format!(
                "model {} cannot run the tool loop — give a file path, or pick another model in Setup",
                model.name()
            ),
        )
        .with_model_id(model.name()));
    }
    // The actor is the seat alone — stable across model reassignment
    // (07d-A1 §1: reassigning Grok→Claude must not change the being on the
    // wire). Which model actually answered is payload `model_id`, recorded
    // on every event below, never part of actor identity.
    let actor = format!("agent:{}", seat.as_str());
    let agent_id = crate::node::agent_id(&req.stream_id, seat);
    // Its calls through the model proxy carry its own token: usage per agent.
    let _proxy = bakery_agent::proxy::as_agent(crate::model_proxy::agent_token(state, &agent_id));

    // Hot memory: tail of this stream (exclude current turn's agent.turn.started).
    // +1 so that stripping the just-appended started event still leaves a full
    // HOT_MEMORY_SCAN window for hot_memory_from_events.
    let hot = {
        let events = state
            .heart
            .lock()
            .unwrap()
            .read_stream_tail(&req.stream_id, crate::agent_loop::HOT_MEMORY_SCAN + 1)
            .map_err(|e| TurnTerminal::early(TurnPhase::Tools, e.to_string()))?;
        let prior: Vec<_> = if events
            .last()
            .map(|e| e.family.as_str() == "agent.turn.started")
            .unwrap_or(false)
        {
            events[..events.len().saturating_sub(1)].to_vec()
        } else {
            events
        };
        // 07d-A1 §2: this agent's prompt uses only this agent's own prior
        // turns — never another configured agent's thread on the same
        // socket, and never an untagged/malformed agent-scoped event either
        // (fails closed, no legacy compatibility mapping). Explicit
        // socket-global bookkeeping and edit decisions (via causality) are
        // unaffected — see `events_owned_by_agent`.
        let prior_for_agent: Vec<_> = events_owned_by_agent(prior, &agent_id);
        crate::agent_loop::hot_memory_from_events(
            prior_for_agent
                .iter()
                .map(|e| (e.family.as_str(), e.payload.as_str())),
        )
    };
    // The memory's index, before the instruction: the same from turn to
    // turn until a note changes, so the opening's start caches.
    let memory = crate::memory_index::turn_index(state, &req.stream_id, crate::memory_index::index_tokens(model.name()));
    let hint_owned: Option<String> = req
        .file_path
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .or_else(|| hot.paths.last().cloned());

    // Snapshots from hot memory captured before the loop mutates nothing —
    // used verbatim in agent.turn.exhausted / agent.turn.failed for orc/gepa thrash analysis.
    let paths_hint: Vec<String> = {
        let p = &hot.paths;
        if p.len() > crate::agent_loop::EXHAUSTED_PATHS_CAP {
            p[p.len() - crate::agent_loop::EXHAUSTED_PATHS_CAP..].to_vec()
        } else {
            p.clone()
        }
    };
    let prior_accept_path: Option<String> = hot.prior_accept_path.clone();
    // Recent analytics only; every individual tool event remains in Heart.
    let mut tool_log: Vec<serde_json::Value> = Vec::new();
    // Stall detection (Checkpoint 2 item 5): repeated failing/no-progress
    // calls climb warn → diagnose → halt. This — not a step quota — is
    // what bounds a thrashing loop.
    let mut stall_guard = crate::agent_loop::StallGuard::default();

    // Baker streams only: measured factory ops on system + opening (no invent metrics).
    let baker_ops = if is_baker_stream_ctx(state, &ctx) {
        // This transport extends `decls` with `baker_op_tool_decls()` below
        // — `ops_dispatchable: true`.
        let mut text =
            baker_ops_for_turn(state, &req.stream_id, &live_branch(&ctx), is_baker_scope, true);
        if is_baker_scope {
            if let Some(note) = crate::baker_threads::conversation_note(state, &req.stream_id) {
                text.push_str(&note);
            }
            // The same machine-access line a native Baker gets in its envelope.
            if let Some(access) = crate::machine_access::session_line(state, &req.stream_id) {
                text.push_str(&format!("\n{access}\n"));
            }
        }
        Some(text)
    } else {
        None
    };
    // 07d-B Phase 2 item 3/6: the system prompt's tool contract reflects
    // what this turn can actually dispatch — jailed file tools present only
    // with a real worktree root, `task_complete` named only on a task
    // socket. The permanent Baker's op-registry line already lives inside
    // `baker_ops` (`baker_ops_for_turn` appends it via
    // `format_baker_op_registry`) — `system_with_ops_for_turn` just decides
    // where that block lands, same as `system_with_ops` always did.
    let mut system = crate::agent_loop::system_with_ops_for_turn(
        baker_ops.as_deref(),
        jail.is_some(),
        is_task_socket,
    );
    // Stage C item 5 ("persona injection"): the exact same
    // `agent_loop::persona_preamble` text `pipe::acp_persona_preamble`
    // prepends to an ACP session's first prompt, so the HTTP and ACP
    // transports' instructions can never drift apart.
    system = format!(
        "{}\n{system}",
        crate::agent_loop::persona_for_agent(state, &req.stream_id, seat)
    );

    // Every file reaches the model: one it reads natively (an image; a PDF
    // where the provider takes documents) as itself, the rest as text.
    let native_types = model.native_attachment_types();
    let reads_natively = |media: &str| native_types.contains(&media);
    let attachments = crate::attachments::turn_attachments(
        state,
        &req.stream_id,
        &agent_id,
        attachment_ids,
        &reads_natively,
        crate::attachments::inline_budget(model.name()),
    );
    // A previous turn that did not finish (a network error, a step limit,
    // a restart) is briefed in full: this loop keeps no session between
    // turns, so otherwise "continue" would have only hot memory's one line
    // to go on. Resume's own briefing already carries that turn.
    let prior = crate::agent_loop::agent_prior_turns(state, &req.stream_id, &agent_id, turn_id);
    let mut instruction = if req.instruction.starts_with(RESUME_PREFIX) {
        None
    } else {
        crate::agent_loop::unfinished_previous_turn_note(&prior)
    }
    .map_or_else(
        || req.instruction.clone(),
        |note| format!("{note}\n\n{}", req.instruction),
    );
    // A conversation handed over from another model (the seat switched
    // runtime or model) gets its prior turns in full, as a native runtime's
    // fresh session does: hot memory alone is one line a turn.
    if previous_model(state, &req.stream_id, &agent_id).is_some_and(|m| m != model.name()) {
        let body = crate::agent_loop::render_prior_turns(&prior, 10, 20_000);
        if !body.is_empty() {
            instruction = format!(
                "[Bakery: this conversation comes to you from another model. Its prior turns, \
                 including any that did not finish:]\n\n{body}[End of prior turns — continue from here:]\n\n{instruction}"
            );
        }
    }
    let mut messages = vec![crate::agent_loop::opening_message(
        &instruction,
        &ctx.project,
        &live_branch(&ctx),
        hint_owned.as_deref(),
        Some(&hot),
        memory.as_deref(),
        baker_ops.as_deref(),
    )];
    if !attachments.is_empty() {
        messages.push(attachment_message(attachments));
    }
    let mut last: Option<Event> = None;

    let mut decls = crate::tools::http_tool_decls(jail.is_some(), &operator_tools);
    // Every other agent gets the Bakery's tools too, within its own project
    // and its tools limit (`api_tools::within_reach`), as on native runtimes.
    // A task worker is one of them.
    let agent_scope = !is_baker_scope;
    let limit = crate::api_tools::tools_limit(state, &req.stream_id);
    if is_baker_scope {
        decls.extend(crate::api_tools::baker_decls(limit));
    } else {
        decls.extend(crate::api_tools::agent_decls(limit));
    }
    if is_task_socket {
        decls.push(task_complete_tool_decl());
        if crate::machine_access::containers_on(state, &req.stream_id) {
            decls.extend(crate::containers::worker_decls());
        }
    }
    let custom_tools = crate::toolkit::custom_tools(state, &req.stream_id);
    decls.extend(crate::toolkit::custom_tool_decls(&custom_tools));
    // The visible plan picker and the wire contract must agree. Filtering
    // saves context and the per-call check below is the hard backstop.
    let allowed = |name: &str| operating_mode.allows_tool(name);
    decls.retain(|decl| allowed(&decl.name));
    // Coalescing window for streamed text deltas broadcast into the live
    // activity strip (07d-A5 §4): `bakery_agent::Model::chat_turn_streaming`
    // hands back one callback invocation per raw provider-level fragment,
    // deliberately with no throttling of its own — this node is the only
    // place that knows the broadcast channel's realistic budget, so it owns
    // batching. Flushing at whichever comes first (time or size) keeps a
    // fast model from flooding `state.tx` with one message per token.
    const STREAM_FLUSH_INTERVAL: Duration = Duration::from_millis(90);
    const STREAM_FLUSH_CHARS: usize = 120;
    let max_steps = state.node.lock().unwrap().agent_max_steps.map(|n| n.get());
    let mut step = 0;
    // Capacity-aware budgeting (Checkpoint 2 item 6): the seat's effective
    // model id (per-turn override, else the durable assignment) resolves a
    // live catalog context window when one exists; the loop then triggers
    // compaction from max(provider-reported usage, local estimate) against
    // that window, and falls back to the byte heuristic — saying so —
    // when no reliable window is known.
    let context_window_tokens: Option<u64> = {
        let assigned = {
            let heart = state.heart.lock().unwrap();
            crate::state::latest_seat_model_id(&heart, &req.stream_id, seat)
                .ok()
                .flatten()
        };
        let effective_id = crate::state::normalize_override(req.model_id.as_deref())
            .map(str::to_string)
            .or(assigned);
        effective_id.and_then(|id| {
            crate::account_adapter::cached_context_window(
                state,
                model.name(),
                provider_for(model.name(), model.transport()),
                &id,
            )
        })
    };
    // Provider-reported input tokens from the PREVIOUS model call this
    // turn — the freshest real consumption signal available at the top of
    // each iteration.
    let mut reported_input_tokens: Option<u64> = None;
    'turn_loop: loop {
        if max_steps.is_some_and(|limit| step >= limit) {
            break;
        }
        step += 1;
        if tool_log.len() > 256 {
            tool_log.drain(..tool_log.len() - 256);
        }
        if cancel.load(Ordering::Acquire) {
            return record_turn_cancelled(
                state,
                req,
                &agent_id,
                turn_id,
                model.name(),
                step,
                &tool_log,
                &paths_hint,
                prior_accept_path.clone(),
            );
        }
        // A screenshot the last step took reaches the model after all of that
        // step's tool results (desktop.rs).
        if let Some(png) = crate::desktop::take_screenshot(&agent_id, Some(turn_id)) {
            messages.push(crate::desktop::screenshot_message(png, model.native_attachment_types()));
        }
        set_agent_phase(state, &agent_id, ActivityPhase::ContactingModel);

        if let Some(trigger_basis) = crate::agent_loop::should_compact(
            crate::agent_loop::context_bytes(&messages),
            reported_input_tokens,
            context_window_tokens,
        ) {
            if let Some(cut) = crate::agent_loop::compaction_cut(&messages) {
                let mut prefix = messages[..cut].to_vec();
                prefix.push(bakery_agent::ChatMsg::user(
                    "Summarize the preceding work for continued execution. Preserve the user's objective, \
                     paths, discoveries, errors, accepted/rejected/pending edits, verification, and next steps. \
                     Do not execute tools or claim pending changes were applied. Return only a concise \
                     factual handoff under 2500 characters; no code dumps."
                ));
                append_streaming_text(
                    state,
                    &agent_id,
                    "\nCompacting earlier tool context; continuing this instruction.\n",
                );
                let result = model
                    .chat_turn_streaming(&system, &prefix, &[], &mut |_| {}, cancel)
                    .map_err(|e| {
                        TurnTerminal::early(
                            TurnPhase::Model,
                            format!("context compaction failed: {e}"),
                        )
                        .with_model_id(model.name())
                    })?;
                if cancel.load(Ordering::Acquire) {
                    return record_turn_cancelled(
                        state,
                        req,
                        &agent_id,
                        turn_id,
                        model.name(),
                        step,
                        &tool_log,
                        &paths_hint,
                        prior_accept_path.clone(),
                    );
                }
                let summary = match result {
                    StreamEnd::Completed(ChatReply::Text(text)) => text,
                    _ => {
                        return Err(TurnTerminal::early(
                            TurnPhase::Model,
                            "context compaction did not return a completed text summary",
                        )
                        .with_model_id(model.name()))
                    }
                };
                let next = crate::agent_loop::compacted_messages(&messages, cut, &summary)
                    .map_err(|e| {
                        TurnTerminal::early(TurnPhase::Model, e).with_model_id(model.name())
                    })?;
                // Persist before replacing working context. Conversation events
                // remain immutable; repeated compaction includes the prior summary.
                // The summarizer call's own reported usage is recorded too
                // (Checkpoint 2 item 6: "record compaction usage where
                // reported") — compaction is a real model call with a cost.
                let compaction_usage = model.last_turn_usage();
                append(
                    state,
                    &req.stream_id,
                    &actor,
                    "agent.context.compacted",
                    serde_json::json!({
                    "agent_id": agent_id, "turn_id": turn_id, "model_id": model.name(),
                    "summary": summary, "before_step": step, "replaced_messages": cut - 1,
                    "before_bytes": crate::agent_loop::context_bytes(&messages),
                    "after_bytes": crate::agent_loop::context_bytes(&next),
                    "trigger": trigger_basis,
                    "context_window_tokens": context_window_tokens,
                    "reported_input_tokens": reported_input_tokens,
                    "compaction_tokens_in": compaction_usage.and_then(|u| u.tokens_in),
                    "compaction_tokens_out": compaction_usage.and_then(|u| u.tokens_out),
                    })
                    .to_string(),
                    vec![],
                )
                .map_err(|e| TurnTerminal::early(TurnPhase::Tools, e))?;
                messages = next;
                // The old reported figure described the pre-compaction
                // context — stale by construction now.
                reported_input_tokens = None;
            }
        }

        let mut stream_buf = String::new();
        let mut last_flush = Instant::now();
        // W1-T: a stretch of thinking runs from this call (its last step
        // ended) to its first word, or to its end when it only acts.
        let (mut asked, mut asked_at) = (Instant::now(), chrono::Utc::now().to_rfc3339());
        let mut first_word: Option<Instant> = None;
        let mut tries = 0;
        let stream_end = loop {
            let attempt = model.chat_turn_streaming(
                system.as_ref(),
                &messages,
                &decls,
                &mut |delta: &str| {
                    first_word.get_or_insert_with(Instant::now);
                    stream_buf.push_str(delta);
                    if stream_buf.len() >= STREAM_FLUSH_CHARS
                        || last_flush.elapsed() >= STREAM_FLUSH_INTERVAL
                    {
                        append_streaming_text(state, &agent_id, &stream_buf);
                        stream_buf.clear();
                        last_flush = Instant::now();
                    }
                },
                cancel,
            );
            // A passing failure (the network, the provider's load, the model
            // proxy restarting) is waited out, and the step asked again from
            // the same messages: nothing of this step was kept yet.
            let why = match &attempt {
                Err(e) => e.clone(),
                Ok(StreamEnd::Stopped {
                    reason: StopReason::StreamDied(diag),
                    ..
                }) => format!("model stream ended unexpectedly: {diag}"),
                Ok(_) => break attempt,
            };
            match crate::resume::cut(&why) {
                Some(crate::resume::Cut::Passing) => {}
                Some(crate::resume::Cut::Crashed) if tries < crate::resume::CRASH_TRIES => {}
                Some(crate::resume::Cut::SignedOut) => break Err(crate::resume::SIGNED_OUT.to_string()),
                _ => break attempt,
            }
            let stopped = crate::resume::wait_out(
                state,
                &req.stream_id,
                &agent_id,
                turn_id,
                model.name(),
                &why,
                tries,
                cancel,
            );
            if stopped {
                return record_turn_cancelled(
                    state,
                    req,
                    &agent_id,
                    turn_id,
                    model.name(),
                    step,
                    &tool_log,
                    &paths_hint,
                    prior_accept_path.clone(),
                );
            }
            tries += 1;
            stream_buf.clear();
            first_word = None;
            (asked, asked_at) = (Instant::now(), chrono::Utc::now().to_rfc3339());
        }
        .map_err(|e| {
                TurnTerminal::with_analytics(
                    TurnPhase::Model,
                    e,
                    &tool_log,
                    &paths_hint,
                    prior_accept_path.clone(),
                )
                .with_model_id(model.name())
            })?;
        if !stream_buf.is_empty() {
            append_streaming_text(state, &agent_id, &stream_buf);
        }
        if let Some(thinking) = model.last_turn_thinking() {
            let ms = first_word.unwrap_or_else(Instant::now).duration_since(asked).as_millis();
            let fact = crate::pipe::ThoughtFact {
                started_at: &asked_at,
                ms: u64::try_from(ms).unwrap_or(u64::MAX),
                text: &thinking.text,
                cut: false,
            };
            crate::pipe::record_thought(state, &req.stream_id, seat, model.name(), turn_id, fact);
        }
        // Freshest real consumption signal for the next iteration's
        // capacity check; keep the previous value when this provider
        // reported nothing.
        reported_input_tokens = model
            .last_turn_usage()
            .and_then(|u| u.tokens_in)
            .or(reported_input_tokens);

        let reply = match stream_end {
            StreamEnd::Completed(reply) => reply,
            StreamEnd::Stopped {
                reason: StopReason::Cancelled,
                ..
            } => {
                return record_turn_cancelled(
                    state,
                    req,
                    &agent_id,
                    turn_id,
                    model.name(),
                    step,
                    &tool_log,
                    &paths_hint,
                    prior_accept_path.clone(),
                );
            }
            StreamEnd::Stopped {
                partial_text,
                reason: StopReason::StreamDied(diag),
            } => {
                let preview = crate::agent_loop::truncate(&partial_text, 200);
                return Err(TurnTerminal::with_analytics(
                    TurnPhase::Model,
                    format!("model stream ended unexpectedly: {diag} (partial reply: {preview:?})"),
                    &tool_log,
                    &paths_hint,
                    prior_accept_path,
                )
                .with_model_id(model.name()));
            }
        };
        // A steer that arrived while the model was streaming wins before a
        // completed answer or any queued tool call is acted upon. This is a
        // safe boundary: no side-effecting child process has begun yet.
        if let Some((steer, attachments)) = agent_control::take_next_steer(
            state,
            &req.stream_id,
            seat,
            turn_id,
            &reads_natively,
            crate::attachments::inline_budget(model.name()),
        )
        .map_err(|e| {
            TurnTerminal::with_analytics(
                TurnPhase::Tools,
                e,
                &tool_log,
                &paths_hint,
                prior_accept_path.clone(),
            )
            .with_model_id(model.name())
        })? {
            push_steer(&mut messages, &steer, attachments);
            continue 'turn_loop;
        }
        match reply {
            ChatReply::Text(summary) => {
                let payload = serde_json::json!({
                    "summary": summary,
                    "steps": step,
                    "agent_id": agent_id,
                    "model_id": model.name(),
                    "turn_id": turn_id,
                })
                .to_string();
                let ev = append(
                    state,
                    &req.stream_id,
                    &actor,
                    "agent.turn.done",
                    payload,
                    vec![],
                )
                .map_err(|e| {
                    TurnTerminal::with_analytics(
                        TurnPhase::Tools,
                        e,
                        &tool_log,
                        &paths_hint,
                        prior_accept_path.clone(),
                    )
                    .with_model_id(model.name())
                })?;
                append_turn_usage_if_any(state, req, &actor, &agent_id, turn_id, &model);
                set_agent_phase(state, &agent_id, ActivityPhase::Complete { note: None });
                return Ok(ev);
            }
            ChatReply::ToolCalls(calls) if calls.is_empty() => {
                let payload = serde_json::json!({
                    "summary": "done",
                    "steps": step,
                    "agent_id": agent_id,
                    "model_id": model.name(),
                    "turn_id": turn_id,
                })
                .to_string();
                let ev = append(
                    state,
                    &req.stream_id,
                    &actor,
                    "agent.turn.done",
                    payload,
                    vec![],
                )
                .map_err(|e| {
                    TurnTerminal::with_analytics(
                        TurnPhase::Tools,
                        e,
                        &tool_log,
                        &paths_hint,
                        prior_accept_path.clone(),
                    )
                    .with_model_id(model.name())
                })?;
                append_turn_usage_if_any(state, req, &actor, &agent_id, turn_id, &model);
                set_agent_phase(state, &agent_id, ActivityPhase::Complete { note: None });
                return Ok(ev);
            }
            ChatReply::ToolCalls(calls) => {
                messages.push(bakery_agent::ChatMsg::assistant_tools(calls.clone()));
                let asked = calls.clone();
                for (nth, call) in calls.into_iter().enumerate() {
                    if cancel.load(Ordering::Acquire) {
                        // Stop caught this call, and every one after it,
                        // before it ran.
                        record_unrun_calls(state, &req.stream_id, &actor, &agent_id, model.name(), turn_id, step, &asked[nth..]);
                        return record_turn_cancelled(
                            state,
                            req,
                            &agent_id,
                            turn_id,
                            model.name(),
                            step,
                            &tool_log,
                            &paths_hint,
                            prior_accept_path.clone(),
                        );
                    }
                    // Re-check immediately before dispatch. It is possible
                    // for a steer to land between the post-model safe point
                    // and a later call in a multi-call response.
                    if let Some((steer, attachments)) = agent_control::take_next_steer(
                        state,
                        &req.stream_id,
                        seat,
                        turn_id,
                        &reads_natively,
                        crate::attachments::inline_budget(model.name()),
                    )
                    .map_err(|e| {
                        TurnTerminal::with_analytics(
                            TurnPhase::Tools,
                            e,
                            &tool_log,
                            &paths_hint,
                            prior_accept_path.clone(),
                        )
                        .with_model_id(model.name())
                    })? {
                        push_steer(&mut messages, &steer, attachments);
                        continue 'turn_loop;
                    }
                    let name = call.name.clone();
                    let args = call.arguments.clone();
                    let call_id = crate::node::mint_id("call");
                    if !allowed(&name) {
                        let body = operating_mode.blocked_tool_message(&name);
                        let payload = serde_json::json!({
                            "tool": name,
                            "args": args,
                            "ok": false,
                            "blocked_by_mode": operating_mode.as_str(),
                            "step": step,
                            "summary": body,
                            "agent_id": agent_id,
                            "model_id": model.name(),
                            "turn_id": turn_id,
                            "call_id": call_id.clone(),
                        })
                        .to_string();
                        last = Some(
                            append(
                                state,
                                &req.stream_id,
                                &actor,
                                "agent.tool.blocked",
                                payload,
                                vec![],
                            )
                            .map_err(|e| {
                                TurnTerminal::with_analytics(
                                    TurnPhase::Tools,
                                    e,
                                    &tool_log,
                                    &paths_hint,
                                    prior_accept_path.clone(),
                                )
                                .with_model_id(model.name())
                            })?,
                        );
                        push_completed_tool(
                            state,
                            &agent_id,
                            CompletedTool {
                                tool: name,
                                call_id,
                                ok: false,
                                finished_at: chrono::Utc::now().to_rfc3339(),
                            },
                        );
                        messages.push(crate::agent_loop::native_tool_result(&call, false, &body));
                        continue;
                    }
                    // A command waits for the operator's Allow in Manual, and
                    // never runs against their "never" — the same card a
                    // native runtime's own commands raise.
                    if matches!(name.as_str(), "run" | "bash") {
                        let command = args
                            .get("argv")
                            .and_then(|v| v.as_array())
                            .map(|argv| argv.iter().filter_map(|a| a.as_str()).collect::<Vec<_>>().join(" "))
                            .unwrap_or_default();
                        let title = format!("{name}: {}", crate::agent_loop::clip_head(&command, 300));
                        if let Err(why) = command_admission(
                            state,
                            &req.stream_id,
                            seat,
                            turn_id,
                            model.name(),
                            &title,
                            operating_mode,
                            cancel,
                        ) {
                            // Stopped while its Allow card waited: it never ran.
                            if cancel.load(Ordering::Acquire) {
                                record_unrun_calls(state, &req.stream_id, &actor, &agent_id, model.name(), turn_id, step, std::slice::from_ref(&call));
                            }
                            push_completed_tool(
                                state,
                                &agent_id,
                                CompletedTool {
                                    tool: name,
                                    call_id,
                                    ok: false,
                                    finished_at: chrono::Utc::now().to_rfc3339(),
                                },
                            );
                            messages.push(crate::agent_loop::native_tool_result(
                                &call,
                                false,
                                &format!("not run: {why}"),
                            ));
                            continue;
                        }
                    }
                    let tool_kind = if looks_like_test_run(&name, &args) {
                        ToolRunKind::Testing
                    } else {
                        ToolRunKind::Tool
                    };
                    set_agent_phase(
                        state,
                        &agent_id,
                        ActivityPhase::RunningTool {
                            tool: name.clone(),
                            call_id: call_id.clone(),
                            started_at: chrono::Utc::now().to_rfc3339(),
                            kind: tool_kind,
                            // The legacy provider loop records the tool name;
                            // its argument preview is surfaced through the
                            // ordinary tool transcript row, not the live strip.
                            preview: None,
                            output: None,
                        },
                    );
                    if let Some(reason) = crate::toolkit::run_pre_hooks(state, &req.stream_id, &name, &args) {
                        push_completed_tool(
                            state,
                            &agent_id,
                            CompletedTool {
                                tool: name,
                                call_id,
                                ok: false,
                                finished_at: chrono::Utc::now().to_rfc3339(),
                            },
                        );
                        messages.push(crate::agent_loop::native_tool_result(
                            &call, false, &reason,
                        ));
                        continue;
                    }
                    if name == "write_file" {
                        // A credential, a runtime's own settings or an
                        // always-blocked path is refused like a missing jail:
                        // the tool's answer, not the turn's end. An escape
                        // still ends the turn below.
                        let refused =
                            match (jail.as_ref(), args.get("path").and_then(|p| p.as_str())) {
                                (None, _) => Some(no_jail_message(&name)),
                                (Some(j), Some(p)) if j.resolve(p.trim()).is_ok() => {
                                    j.resolve_write(p.trim()).err()
                                }
                                _ => None,
                            };
                        let (Some(jail), None) = (jail.as_ref(), &refused) else {
                            let msg = refused.clone().unwrap_or_default();
                            tool_log.push(serde_json::json!({ "tool": name, "ok": false, "step": step, "args_summary": name }));
                            let payload = serde_json::json!({
                            "tool": name, "args": args, "ok": false, "step": step,
                            "summary": crate::agent_loop::truncate(&msg, crate::agent_loop::MAX_EVENT_SUMMARY),
                            "agent_id": agent_id, "model_id": model.name(), "turn_id": turn_id, "call_id": call_id.clone(),
                        })
                        .to_string();
                            last = Some(
                                append(
                                    state,
                                    &req.stream_id,
                                    &actor,
                                    "agent.tool.called",
                                    payload,
                                    vec![],
                                )
                                .map_err(|e| {
                                    TurnTerminal::with_analytics(
                                        TurnPhase::Tools,
                                        e,
                                        &tool_log,
                                        &paths_hint,
                                        prior_accept_path.clone(),
                                    )
                                    .with_model_id(model.name())
                                })?,
                            );
                            push_completed_tool(
                                state,
                                &agent_id,
                                CompletedTool {
                                    tool: name,
                                    call_id,
                                    ok: false,
                                    finished_at: chrono::Utc::now().to_rfc3339(),
                                },
                            );
                            // A refusal repeated forever is a stall too
                            // (review finding D).
                            match stall_guard.observe("write_file", &args, false, &msg) {
                                crate::agent_loop::StallAction::Halt(reason) => {
                                    return Err(TurnTerminal::with_analytics(
                                        TurnPhase::Tools,
                                        reason,
                                        &tool_log,
                                        &paths_hint,
                                        prior_accept_path.clone(),
                                    )
                                    .with_model_id(model.name()));
                                }
                                crate::agent_loop::StallAction::Warn(g)
                                | crate::agent_loop::StallAction::Diagnose(g) => {
                                    messages.push(crate::agent_loop::native_tool_result(
                                        &call,
                                        false,
                                        &format!("{msg}\n{g}"),
                                    ));
                                }
                                _ => {
                                    messages.push(crate::agent_loop::native_tool_result(
                                        &call, false, &msg,
                                    ));
                                }
                            }
                            continue;
                        };
                        let file_path = args
                            .get("path")
                            .and_then(|x| x.as_str())
                            .filter(|s| !s.trim().is_empty())
                            .ok_or_else(|| {
                                TurnTerminal::with_analytics(
                                    TurnPhase::Tools,
                                    "write_file needs a non-empty \"path\"",
                                    &tool_log,
                                    &paths_hint,
                                    prior_accept_path.clone(),
                                )
                                .with_model_id(model.name())
                            })?
                            .trim()
                            .to_string();
                        let new_content = args
                            .get("content")
                            .and_then(|x| x.as_str())
                            .ok_or_else(|| {
                                TurnTerminal::with_analytics(
                                    TurnPhase::Tools,
                                    "write_file needs \"content\"",
                                    &tool_log,
                                    &paths_hint,
                                    prior_accept_path.clone(),
                                )
                                .with_model_id(model.name())
                            })?
                            .to_string();
                        let rationale = args
                            .get("rationale")
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string();
                        let target = jail.resolve(&file_path).map_err(|e| {
                            TurnTerminal::with_analytics(
                                TurnPhase::Jail,
                                e,
                                &tool_log,
                                &paths_hint,
                                prior_accept_path.clone(),
                            )
                            .with_model_id(model.name())
                        })?;
                        let old_content = std::fs::read_to_string(&target).unwrap_or_default();
                        let ev = propose_edit_event(
                            state,
                            &req.stream_id,
                            seat,
                            &model,
                            &file_path,
                            &new_content,
                            &rationale,
                            old_content,
                            turn_id,
                            &call_id,
                        )
                        .map_err(|e| {
                            TurnTerminal::with_analytics(
                                TurnPhase::Tools,
                                e,
                                &tool_log,
                                &paths_hint,
                                prior_accept_path.clone(),
                            )
                            .with_model_id(model.name())
                        })?;
                        push_completed_tool(
                            state,
                            &agent_id,
                            CompletedTool {
                                tool: name,
                                call_id,
                                ok: true,
                                finished_at: chrono::Utc::now().to_rfc3339(),
                            },
                        );
                        set_agent_phase(
                            state,
                            &agent_id,
                            ActivityPhase::AwaitingApproval {
                                kind: ApprovalKind::FileEdit,
                                event_hash: ev.event_hash.clone(),
                            },
                        );
                        maybe_auto_accept_for_mode(
                            state,
                            &req.stream_id,
                            &ev,
                            mode_allows_auto_edit,
                        );
                        // Continue-after-decision (Checkpoint 2 item 2): park
                        // until the human (or Auto) decides, then feed the
                        // ACTUAL outcome back as this call's result and keep
                        // working — a proposal no longer ends the turn.
                        last = Some(ev.clone());
                        let outcome = match wait_for_decision(
                            state,
                            &req.stream_id,
                            &ev.event_hash,
                            cancel,
                        ) {
                            Ok(o) => o,
                            Err(()) => {
                                return record_turn_cancelled(
                                    state,
                                    req,
                                    &agent_id,
                                    turn_id,
                                    model.name(),
                                    step,
                                    &tool_log,
                                    &paths_hint,
                                    prior_accept_path.clone(),
                                );
                            }
                        };
                        let (ok, body) = decision_result_body(&outcome, &file_path);
                        set_agent_phase(state, &agent_id, ActivityPhase::ContactingModel);
                        messages.push(crate::agent_loop::native_tool_result(&call, ok, &body));
                        continue;
                    }
                    if name == "edit" {
                        let Some(jail) = jail.as_ref() else {
                            let msg = no_jail_message(&name);
                            tool_log.push(serde_json::json!({ "tool": name, "ok": false, "step": step, "args_summary": "edit" }));
                            let payload = serde_json::json!({
                            "tool": name, "args": args, "ok": false, "step": step,
                            "summary": crate::agent_loop::truncate(&msg, crate::agent_loop::MAX_EVENT_SUMMARY),
                            "agent_id": agent_id, "model_id": model.name(), "turn_id": turn_id, "call_id": call_id.clone(),
                        })
                        .to_string();
                            last = Some(
                                append(
                                    state,
                                    &req.stream_id,
                                    &actor,
                                    "agent.tool.called",
                                    payload,
                                    vec![],
                                )
                                .map_err(|e| {
                                    TurnTerminal::with_analytics(
                                        TurnPhase::Tools,
                                        e,
                                        &tool_log,
                                        &paths_hint,
                                        prior_accept_path.clone(),
                                    )
                                    .with_model_id(model.name())
                                })?,
                            );
                            push_completed_tool(
                                state,
                                &agent_id,
                                CompletedTool {
                                    tool: name,
                                    call_id,
                                    ok: false,
                                    finished_at: chrono::Utc::now().to_rfc3339(),
                                },
                            );
                            match stall_guard.observe("edit", &args, false, &msg) {
                                crate::agent_loop::StallAction::Halt(reason) => {
                                    return Err(TurnTerminal::with_analytics(
                                        TurnPhase::Tools,
                                        reason,
                                        &tool_log,
                                        &paths_hint,
                                        prior_accept_path.clone(),
                                    )
                                    .with_model_id(model.name()));
                                }
                                crate::agent_loop::StallAction::Warn(g)
                                | crate::agent_loop::StallAction::Diagnose(g) => {
                                    messages.push(crate::agent_loop::native_tool_result(
                                        &call,
                                        false,
                                        &format!("{msg}\n{g}"),
                                    ));
                                }
                                _ => {
                                    messages.push(crate::agent_loop::native_tool_result(
                                        &call, false, &msg,
                                    ));
                                }
                            }
                            continue;
                        };
                        match crate::tools::apply_exact_replace(jail, &args) {
                            Ok((file_path, new_content, rationale)) => {
                                let target = jail.resolve(&file_path).map_err(|e| {
                                    TurnTerminal::with_analytics(
                                        TurnPhase::Jail,
                                        e,
                                        &tool_log,
                                        &paths_hint,
                                        prior_accept_path.clone(),
                                    )
                                    .with_model_id(model.name())
                                })?;
                                let old_content =
                                    std::fs::read_to_string(&target).unwrap_or_default();
                                let ev = propose_edit_event(
                                    state,
                                    &req.stream_id,
                                    seat,
                                    &model,
                                    &file_path,
                                    &new_content,
                                    &rationale,
                                    old_content,
                                    turn_id,
                                    &call_id,
                                )
                                .map_err(|e| {
                                    TurnTerminal::with_analytics(
                                        TurnPhase::Tools,
                                        e,
                                        &tool_log,
                                        &paths_hint,
                                        prior_accept_path.clone(),
                                    )
                                    .with_model_id(model.name())
                                })?;
                                push_completed_tool(
                                    state,
                                    &agent_id,
                                    CompletedTool {
                                        tool: name,
                                        call_id,
                                        ok: true,
                                        finished_at: chrono::Utc::now().to_rfc3339(),
                                    },
                                );
                                set_agent_phase(
                                    state,
                                    &agent_id,
                                    ActivityPhase::AwaitingApproval {
                                        kind: ApprovalKind::FileEdit,
                                        event_hash: ev.event_hash.clone(),
                                    },
                                );
                                maybe_auto_accept_for_mode(
                                    state,
                                    &req.stream_id,
                                    &ev,
                                    mode_allows_auto_edit,
                                );
                                // Continue-after-decision — same contract as the
                                // write_file arm above.
                                last = Some(ev.clone());
                                let outcome = match wait_for_decision(
                                    state,
                                    &req.stream_id,
                                    &ev.event_hash,
                                    cancel,
                                ) {
                                    Ok(o) => o,
                                    Err(()) => {
                                        return record_turn_cancelled(
                                            state,
                                            req,
                                            &agent_id,
                                            turn_id,
                                            model.name(),
                                            step,
                                            &tool_log,
                                            &paths_hint,
                                            prior_accept_path.clone(),
                                        );
                                    }
                                };
                                let (ok, body) = decision_result_body(&outcome, &file_path);
                                set_agent_phase(state, &agent_id, ActivityPhase::ContactingModel);
                                messages
                                    .push(crate::agent_loop::native_tool_result(&call, ok, &body));
                                continue;
                            }
                            Err(e) => {
                                // Stall ladder for the classic thrash: repeated
                                // failing exact-replace edits (stale anchor,
                                // invented content). Same warn → diagnose →
                                // halt contract as the generic dispatch arm
                                // (review finding D — with step/turn caps gone
                                // by design, this guard IS the bound).
                                let stall = stall_guard.observe(&name, &args, false, &e);
                                if let crate::agent_loop::StallAction::Halt(reason) = &stall {
                                    let _ = append(
                                        state,
                                        &req.stream_id,
                                        &actor,
                                        "agent.stall.detected",
                                        serde_json::json!({
                                            "tool": name, "action": "halt", "detail": reason,
                                            "agent_id": agent_id, "turn_id": turn_id,
                                        })
                                        .to_string(),
                                        vec![],
                                    );
                                    return Err(TurnTerminal::with_analytics(
                                        TurnPhase::Tools,
                                        reason.clone(),
                                        &tool_log,
                                        &paths_hint,
                                        prior_accept_path.clone(),
                                    )
                                    .with_model_id(model.name()));
                                }
                                let payload = serde_json::json!({
                                "tool": name,
                                "args": args,
                                "ok": false,
                                "step": step,
                                "summary": crate::agent_loop::truncate(&e, crate::agent_loop::MAX_EVENT_SUMMARY),
                                "agent_id": agent_id,
                                "model_id": model.name(),
                                "turn_id": turn_id,
                                "call_id": call_id.clone(),
                            })
                            .to_string();
                                tool_log.push(serde_json::json!({
                                    "tool": name,
                                    "ok": false,
                                    "step": step,
                                    "args_summary": "edit",
                                }));
                                last = Some(
                                    append(
                                        state,
                                        &req.stream_id,
                                        &actor,
                                        "agent.tool.called",
                                        payload,
                                        vec![],
                                    )
                                    .map_err(|e| {
                                        TurnTerminal::with_analytics(
                                            TurnPhase::Tools,
                                            e,
                                            &tool_log,
                                            &paths_hint,
                                            prior_accept_path.clone(),
                                        )
                                        .with_model_id(model.name())
                                    })?,
                                );
                                push_completed_tool(
                                    state,
                                    &agent_id,
                                    CompletedTool {
                                        tool: name,
                                        call_id,
                                        ok: false,
                                        finished_at: chrono::Utc::now().to_rfc3339(),
                                    },
                                );
                                match &stall {
                                    crate::agent_loop::StallAction::Warn(g) => {
                                        messages.push(crate::agent_loop::native_tool_result(
                                            &call,
                                            false,
                                            &format!("{e}\n{g}"),
                                        ));
                                    }
                                    crate::agent_loop::StallAction::Diagnose(g) => {
                                        messages.push(crate::agent_loop::native_tool_result(
                                            &call, false, &e,
                                        ));
                                        messages.push(bakery_agent::ChatMsg::user(format!("STALL CHECK — respond to this before anything else: {g}")));
                                        let _ = append(
                                        state,
                                        &req.stream_id,
                                        &actor,
                                        "agent.stall.detected",
                                        serde_json::json!({
                                            "tool": call.name, "action": "diagnose", "detail": g,
                                            "agent_id": agent_id, "turn_id": turn_id,
                                        })
                                        .to_string(),
                                        vec![],
                                    );
                                    }
                                    _ => {
                                        messages.push(crate::agent_loop::native_tool_result(
                                            &call, false, &e,
                                        ));
                                    }
                                }
                                continue;
                            }
                        }
                    }
                    if name == "ask_user" && crate::learning::scratch(state, &req.stream_id) {
                        let refused = crate::learning::refuse(state, &req.stream_id, "a question for the operator");
                        messages.push(crate::agent_loop::native_tool_result(&call, false, &refused));
                        continue;
                    }
                    if name == "ask_user" {
                        // A malformed ask goes back to the model to fix, the
                        // way any failed tool call does — it never ends the turn.
                        let questions = match crate::ask::questions_from_tool_args(&args) {
                            Ok(questions) => questions,
                            Err(e) => {
                                messages.push(crate::agent_loop::native_tool_result(&call, false, &e));
                                continue;
                            }
                        };
                        let payload = serde_json::json!({
                            "questions": questions,
                            "status": "pending",
                            "agent_id": agent_id,
                            "model_id": model.name(),
                            "turn_id": turn_id,
                            "call_id": call_id.clone(),
                        })
                        .to_string();
                        let ev = append(
                            state,
                            &req.stream_id,
                            &actor,
                            "agent.ask.pending",
                            payload,
                            vec![],
                        )
                        .map_err(|e| {
                            TurnTerminal::with_analytics(
                                TurnPhase::Tools,
                                e,
                                &tool_log,
                                &paths_hint,
                                prior_accept_path.clone(),
                            )
                            .with_model_id(model.name())
                        })?;
                        push_completed_tool(
                            state,
                            &agent_id,
                            CompletedTool {
                                tool: name,
                                call_id,
                                ok: true,
                                finished_at: chrono::Utc::now().to_rfc3339(),
                            },
                        );
                        // Continue-after-decision (Checkpoint 2 item 2): a real
                        // answer route exists now (`POST /agent/answer`), so
                        // "awaiting" is finally the honest phase — the answer
                        // resumes THIS turn as the ask_user call's own result.
                        set_agent_phase(
                            state,
                            &agent_id,
                            ActivityPhase::AwaitingApproval {
                                kind: ApprovalKind::Question,
                                event_hash: ev.event_hash.clone(),
                            },
                        );
                        last = Some(ev.clone());
                        let outcome = match wait_for_decision(
                            state,
                            &req.stream_id,
                            &ev.event_hash,
                            cancel,
                        ) {
                            Ok(o) => o,
                            Err(()) => {
                                return record_turn_cancelled(
                                    state,
                                    req,
                                    &agent_id,
                                    turn_id,
                                    model.name(),
                                    step,
                                    &tool_log,
                                    &paths_hint,
                                    prior_accept_path.clone(),
                                );
                            }
                        };
                        let (ok, body) = decision_result_body(&outcome, "");
                        set_agent_phase(state, &agent_id, ActivityPhase::ContactingModel);
                        messages.push(crate::agent_loop::native_tool_result(&call, ok, &body));
                        continue;
                    }
                    if name == "todo_write" {
                        let outcome = crate::tools::dispatch_http_tool(
                            jail.as_ref(),
                            &operator_tools,
                            &name,
                            &args,
                        );
                        let (ok, body) = match &outcome {
                            Ok(s) => (true, s.clone()),
                            Err(e) => (false, e.clone()),
                        };
                        if ok {
                            let payload = serde_json::json!({
                            "todos": args.get("todos").cloned().unwrap_or(serde_json::Value::Null),
                            "agent_id": agent_id,
                            "model_id": model.name(),
                            "turn_id": turn_id,
                            "call_id": call_id.clone(),
                        })
                        .to_string();
                            append(
                                state,
                                &req.stream_id,
                                &actor,
                                "agent.todos.set",
                                payload,
                                vec![],
                            )
                            .map_err(|e| {
                                TurnTerminal::with_analytics(
                                    TurnPhase::Tools,
                                    e,
                                    &tool_log,
                                    &paths_hint,
                                    prior_accept_path.clone(),
                                )
                                .with_model_id(model.name())
                            })?;
                        }
                        let payload = serde_json::json!({
                        "tool": name,
                        "args": args,
                        "ok": ok,
                        "step": step,
                        "summary": crate::agent_loop::truncate(&body, crate::agent_loop::MAX_EVENT_SUMMARY),
                        "agent_id": agent_id,
                        "model_id": model.name(),
                        "turn_id": turn_id,
                        "call_id": call_id.clone(),
                    })
                    .to_string();
                        tool_log.push(serde_json::json!({
                            "tool": name,
                            "ok": ok,
                            "step": step,
                            "args_summary": "todos",
                        }));
                        last = Some(
                            append(
                                state,
                                &req.stream_id,
                                &actor,
                                "agent.tool.called",
                                payload,
                                vec![],
                            )
                            .map_err(|e| {
                                TurnTerminal::with_analytics(
                                    TurnPhase::Tools,
                                    e,
                                    &tool_log,
                                    &paths_hint,
                                    prior_accept_path.clone(),
                                )
                                .with_model_id(model.name())
                            })?,
                        );
                        push_completed_tool(
                            state,
                            &agent_id,
                            CompletedTool {
                                tool: name,
                                call_id,
                                ok,
                                finished_at: chrono::Utc::now().to_rfc3339(),
                            },
                        );
                        messages.push(crate::agent_loop::native_tool_result(&call, ok, &body));
                        continue;
                    }
                    // Declared Bakery ops (07d-B Phase 2) and the worker-scoped
                    // `task_complete` tool both dispatch here, ahead of the
                    // generic HTTP/operator table — same "one path, one event
                    // shape" this branch already gives every other tool
                    // (`agent.tool.called` below is identical regardless of
                    // which of the three actually ran). Authorization was
                    // already decided when `decls` was built: `is_baker_scope`
                    // gates whether a baker op name is even in `BAKER_OPS`'
                    // advertised set, and `is_task_socket` gates `task_complete`
                    // — this dispatch step trusts that gate, matching
                    // `operator_tools::dispatch_operator_tool`'s own posture.
                    let api_tool = (is_baker_scope && crate::api_tools::is_tool(&name))
                        || (is_task_socket && crate::containers::is_worker_tool(&name))
                        || (agent_scope && crate::api_tools::is_agent_tool(&name));
                    let outcome = if api_tool {
                        // Graded and gated in `api_tools`; an ask blocks this
                        // turn's thread until the operator decides or Stop.
                        let caller = crate::api_tools::Caller {
                            stream_id: req.stream_id.clone(),
                            seat,
                            turn_id: turn_id.to_string(),
                            mode: operating_mode,
                            wait: None,
                        };
                        crate::api_tools::call(state, &caller, &name, &args)
                    } else if is_task_socket && name == "task_complete" {
                        dispatch_task_complete(state, &req.stream_id, &args)
                    } else if let Some(ct) = custom_tools.iter().find(|t| t.name == name) {
                        crate::toolkit::dispatch_custom_tool(state, &req.stream_id, ct, &args)
                    } else {
                        // The web tools reach only what the thread's own
                        // commands may (`network::Wall`). A command's Agent
                        // terminal block carries this call's id, as its row does.
                        crate::actor::with(actor.clone(), || {
                            crate::tools::with_call_id(&call_id, || {
                                crate::network::call(state, &req.stream_id, &name, &args).unwrap_or_else(|| {
                                    crate::tools::dispatch_http_tool(jail.as_ref(), &operator_tools, &name, &args)
                                })
                            })
                        })
                    };
                    // ponytail: post-hooks on generic path only; per-tool builtins
                    // (write_file, edit, ask_user, todo_write) skip — add if needed
                    crate::toolkit::run_post_hooks(
                        state,
                        &req.stream_id,
                        &name,
                        &args,
                        outcome.as_ref().unwrap_or_else(|e| e),
                    );
                    let (ran, body) = match &outcome {
                        Ok(s) => (true, s.clone()),
                        Err(e) => (false, e.clone()),
                    };
                    // A command that ran and exited non-zero failed: the model
                    // is told so, and the transcript shows it red.
                    let ok = ran && !crate::tools::command_failed(&name, &body);
                    // Stall ladder: annotate the model-facing result on Warn,
                    // inject a replanning demand + durable event on Diagnose,
                    // end the turn with a named cause on Halt. The Heart event
                    // below keeps the RAW result summary either way. A command
                    // that ran counts as a call that ran, whatever its exit
                    // code: re-running the tests after an edit is progress, and
                    // a command whose output never changes is still caught as
                    // no progress.
                    let stall = stall_guard.observe(&name, &args, ran, &body);
                    if let crate::agent_loop::StallAction::Halt(reason) = &stall {
                        let _ = append(
                            state,
                            &req.stream_id,
                            &actor,
                            "agent.stall.detected",
                            serde_json::json!({
                                "tool": name,
                                "action": "halt",
                                "detail": reason,
                                "agent_id": agent_id,
                                "turn_id": turn_id,
                            })
                            .to_string(),
                            vec![],
                        );
                        return Err(TurnTerminal::with_analytics(
                            TurnPhase::Tools,
                            reason.clone(),
                            &tool_log,
                            &paths_hint,
                            prior_accept_path.clone(),
                        )
                        .with_model_id(model.name()));
                    }
                    let payload = serde_json::json!({
                    "tool": name,
                    "args": args,
                    "ok": ok,
                    "step": step,
                    "summary": crate::agent_loop::event_summary(&name, &body),
                    // How much of the output the model actually saw, so a
                    // clipped result is visible after the fact.
                    "raw_bytes": body.len(),
                    "shown_bytes": crate::agent_loop::tool_result_body(&name, &body).len(),
                    "agent_id": agent_id,
                    "model_id": model.name(),
                    "turn_id": turn_id,
                    "call_id": call_id.clone(),
                })
                .to_string();
                    let args_str = args.to_string();
                    let args_summary = if args_str.chars().count() <= 120 {
                        args_str
                    } else {
                        format!("{}…", args_str.chars().take(120).collect::<String>())
                    };
                    tool_log.push(serde_json::json!({
                        "tool": name,
                        "ok": ok,
                        "step": step,
                        "args_summary": args_summary,
                    }));
                    last = Some(
                        append(
                            state,
                            &req.stream_id,
                            &actor,
                            "agent.tool.called",
                            payload,
                            vec![],
                        )
                        .map_err(|e| {
                            TurnTerminal::with_analytics(
                                TurnPhase::Tools,
                                e,
                                &tool_log,
                                &paths_hint,
                                prior_accept_path.clone(),
                            )
                            .with_model_id(model.name())
                        })?,
                    );
                    push_completed_tool(
                        state,
                        &agent_id,
                        CompletedTool {
                            tool: name,
                            call_id,
                            ok,
                            finished_at: chrono::Utc::now().to_rfc3339(),
                        },
                    );
                    match &stall {
                        crate::agent_loop::StallAction::Warn(g) => {
                            messages.push(crate::agent_loop::native_tool_result(
                                &call,
                                ok,
                                &format!("{body}\n{g}"),
                            ));
                        }
                        crate::agent_loop::StallAction::Diagnose(g) => {
                            messages.push(crate::agent_loop::native_tool_result(&call, ok, &body));
                            messages.push(bakery_agent::ChatMsg::user(format!(
                                "STALL CHECK — respond to this before anything else: {g}"
                            )));
                            let _ = append(
                                state,
                                &req.stream_id,
                                &actor,
                                "agent.stall.detected",
                                serde_json::json!({
                                    "tool": call.name,
                                    "action": "diagnose",
                                    "detail": g,
                                    "agent_id": agent_id,
                                    "turn_id": turn_id,
                                })
                                .to_string(),
                                vec![],
                            );
                        }
                        _ => {
                            messages.push(crate::agent_loop::native_tool_result(&call, ok, &body));
                        }
                    }
                }
            }
        }
    }

    let payload = serde_json::json!({
        "reason": "step limit reached",
        "steps": step,
        "instruction": crate::agent_loop::truncate(&req.instruction, 400),
        "tools": tool_log.clone(),
        "paths_hint": paths_hint.clone(),
        "prior_accept_path": prior_accept_path.clone(),
        "agent_id": agent_id,
        "model_id": model.name(),
        "turn_id": turn_id,
    })
    .to_string();
    let ev = append(
        state,
        &req.stream_id,
        &actor,
        "agent.turn.exhausted",
        payload,
        vec![],
    )
    .map_err(|e| {
        TurnTerminal::with_analytics(
            TurnPhase::Tools,
            e.to_string(),
            &tool_log,
            &paths_hint,
            prior_accept_path,
        )
        .with_model_id(model.name())
    })?;
    set_agent_phase(
        state,
        &agent_id,
        ActivityPhase::Complete {
            note: Some("stopped at the step limit".to_string()),
        },
    );
    let _ = last; // the exhaustion event is what the caller reports
    Ok(ev)
}

/// Message for a jailed tool called on a rootless socket (07d-B Phase 2
/// item 3) — the tool decls never advertise these names when `jail.is_none()`
/// (`crate::tools::http_tool_decls`'s `has_jail` filter), so reaching this
/// is a model calling something it wasn't offered; refuse honestly rather
/// than panicking on the missing jail.
fn no_jail_message(tool: &str) -> String {
    format!("{tool} needs a workspace root — this socket is rootless, no file tools are available")
}

/// Native tool declaration for the worker-scoped `task_complete` tool
/// (07d-B Stage B item 4) — advertised only on a task socket
/// (`crate::state::socket_task_id`), never to the permanent Baker.
pub(crate) fn task_complete_tool_decl() -> bakery_agent::ToolDecl {
    bakery_agent::ToolDecl {
        name: "task_complete".to_string(),
        description: "Declare this task's scope done. The ONLY signal that moves a task toward proof/review — a completed chat turn alone does not complete the task.".to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": { "summary": { "type": "string" } },
            "required": ["summary"],
        }),
    }
}

/// `task_complete`'s effect (07d-B Stage B item 4): appends
/// `task.status.changed` on the task's PARENT PROJECT stream (tasks are
/// project facts — `crate::tasks`'s own doc comment) carrying a
/// `claimed_done` marker in `reason` for the Stage D driver
/// (`crate::task_driver::drive_one_step`) to consume — proof-running and the
/// `awaiting_review` transition are that driver's job, not this tool's.
/// Status stays `Running`: no code here has actually run proof yet, and
/// asserting a status this pass cannot back up would be exactly the
/// dishonest-event shape `crate::tasks`'s own doc comments forbid.
pub(crate) fn dispatch_task_complete(
    state: &AppState,
    stream_id: &str,
    args: &serde_json::Value,
) -> Result<String, String> {
    let summary = args
        .get("summary")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or("task_complete needs a non-empty \"summary\"")?;
    let (task_id, project_id) = {
        let heart = state.heart.lock().unwrap();
        let task_id = crate::state::socket_task_id(&heart, stream_id)
            .ok_or("this socket is not a task socket — task_complete is not available here")?;
        let (project_id, _root_id) = crate::state::socket_project(&heart, stream_id);
        (
            task_id,
            project_id.ok_or("task socket has no parent project recorded")?,
        )
    };
    // Refuse to resurrect a task the human already terminated. Without this,
    // `tasks_for_project_locked`'s latest-status-wins fold means a stray
    // (or prompt-injected) `task_complete` call always wins the race against
    // a `task.cancel`/retire that already landed, silently flipping a
    // cancelled/done/retired task back to `running` in `GET /task/list`, the
    // Studio projection, and the future Phase-4 driver.
    let task = crate::tasks::find_task(state, &project_id, &task_id)?;
    if task.retired {
        return Err(format!(
            "task {task_id} has been retired — task_complete refused"
        ));
    }
    if matches!(
        task.status,
        crate::tasks::TaskStatus::Cancelled | crate::tasks::TaskStatus::Done
    ) {
        return Err(format!(
            "task {task_id} is already {:?} — task_complete refused, it cannot be resurrected to running",
            task.status
        ));
    }
    let payload = serde_json::to_string(&crate::tasks::TaskStatusChanged {
        task_id,
        status: crate::tasks::TaskStatus::Running,
        reason: Some(format!("claimed_done: {summary}")),
    })
    .unwrap();
    append(
        state,
        &project_id,
        "agent:worker",
        crate::tasks::TASK_STATUS_FAMILY,
        payload,
        vec![],
    )?;
    Ok(summary.to_string())
}

/// Derive a pending operation's decision from durable facts alone: the
/// newest event whose causality cites `event_hash`. This — not the wake
/// channel — is the authority (the driver rule: events wake, Heart
/// decides), which is what makes the auto-accept-inside-the-turn ordering
/// and a notification lost to a race both harmless.
fn derive_decision_from_heart(
    state: &AppState,
    stream_id: &str,
    event_hash: &str,
) -> Option<crate::state::DecisionOutcome> {
    let events = state.heart.lock().unwrap().read_stream(stream_id).ok()?;
    for e in events.iter().rev() {
        if !e.causality.iter().any(|h| h == event_hash) {
            continue;
        }
        let p: serde_json::Value =
            serde_json::from_str(&e.payload).unwrap_or(serde_json::Value::Null);
        match e.family.as_str() {
            "agent.edit.accepted" => {
                let human_edited = p
                    .get("human_edited")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let final_content = p
                    .get("new_content")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
                    .filter(|_| human_edited);
                return Some(crate::state::DecisionOutcome::Accepted {
                    human_edited,
                    final_content,
                });
            }
            "agent.edit.rejected" => return Some(crate::state::DecisionOutcome::Rejected),
            "agent.edit.failed" => {
                let reason = p
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("write failed")
                    .to_string();
                return Some(crate::state::DecisionOutcome::WriteFailed(reason));
            }
            "agent.ask.answered" => {
                let answer = p
                    .get("answer")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                return Some(crate::state::DecisionOutcome::Answered(answer));
            }
            APPROVAL_CANCELLED_FAMILY => return Some(crate::state::DecisionOutcome::Cancelled),
            _ => continue,
        }
    }
    None
}

/// Whether a command call on Bakery's own tool loop may run, or why not.
/// Manual asks — the same Allow/Deny card a native runtime raises for its own
/// commands, so an Allow Always there holds here too — and the operator's
/// "never" for commands holds in every mode, as it does on a native runtime.
/// (Plan and Auto Edit never get here: they cannot run commands at all.)
#[allow(clippy::too_many_arguments)]
fn command_admission(
    state: &AppState,
    stream_id: &str,
    seat: crate::node::Seat,
    turn_id: &str,
    model_id: &str,
    command: &str,
    mode: OperatingMode,
    cancel: &Arc<AtomicBool>,
) -> Result<(), String> {
    use crate::pipe::{ChildToolAdmission, ChildToolGateDecisionKind, ChildToolKind};
    let never = crate::pipe::tool_policy_denies(state, ChildToolKind::Execute);
    if mode != OperatingMode::Manual && !never {
        return Ok(());
    }
    let request = crate::pipe::ChildToolRequest {
        plug: model_id.to_string(),
        kind: ChildToolKind::Execute,
        title: command.to_string(),
        diffs: Vec::new(),
    };
    let pending = match crate::pipe::admit_child_tool_call_detailed(
        state, stream_id, seat, turn_id, &request, false,
    ) {
        ChildToolAdmission::Allow => return Ok(()),
        ChildToolAdmission::Deny if never => {
            return Err("your tool policy never allows commands (Settings → Agents & abilities)".into())
        }
        ChildToolAdmission::Deny => return Err("the command was not allowed".into()),
        ChildToolAdmission::Pending(hash) => hash,
    };
    loop {
        if cancel.load(Ordering::Acquire) {
            crate::pipe::withdraw_pending_approval(state, stream_id, seat, turn_id, &pending, "child_tool");
            return Err("the turn was stopped before the command was allowed".into());
        }
        if let Some(decision) = crate::pipe::child_tool_gate_decision(state, stream_id, &pending) {
            return match decision {
                ChildToolGateDecisionKind::AllowOnce => {
                    // The decision leaves a next-call allowance behind; this
                    // waiting call is that call, so it uses it up.
                    state
                        .child_tool_allow_once
                        .lock()
                        .unwrap()
                        .remove(&(stream_id.to_string(), ChildToolKind::Execute));
                    Ok(())
                }
                ChildToolGateDecisionKind::AllowAlways => Ok(()),
                ChildToolGateDecisionKind::Deny => Err("the operator denied this command".into()),
            };
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// Block this (spawn_blocking) turn thread until the pending operation at
/// `event_hash` is decided, or the turn is cancelled. Continue-after-
/// decision's core (Checkpoint 2 item 2): the SAME frame that made the
/// call receives the outcome, so tool-call/result correspondence is
/// structural and duplicate execution is impossible — there is exactly one
/// call site and it runs exactly once. `Err(())` means cancelled.
///
/// Honest limit (review finding C): a parked HTTP turn pins one tokio
/// blocking-pool thread (default pool 512) until decided or cancelled —
/// fine at single-operator scale, and each parked turn is individually
/// cancellable; an async park (oneshot/watch) is the named follow-up
/// before heavy concurrent multi-agent load. Task-driver turns park their
/// own dedicated per-task thread, not this pool.
fn wait_for_decision(
    state: &AppState,
    stream_id: &str,
    event_hash: &str,
    cancel: &Arc<AtomicBool>,
) -> Result<crate::state::DecisionOutcome, ()> {
    let rx = crate::state::register_decision_waiter(state, event_hash);
    // Heart is checked immediately (a decision that landed BEFORE the
    // waiter registered — auto-accept inside this same frame — is found
    // here), then only every ~1.5s as the missed-notify fallback: the
    // channel is the fast path, and a full-stream read every 150ms per
    // parked turn was a real contention cost (review finding B). Cancel
    // stays at 150ms responsiveness.
    let mut tick: u32 = 0;
    let result = loop {
        // Stop wins over a decision that happened to land in the same polling
        // interval. Otherwise a late answer/accept could resume the parked
        // frame even though the operator had already stopped its turn.
        if cancel.load(Ordering::Acquire) {
            break Err(());
        }
        if tick % 10 == 0 {
            if let Some(outcome) = derive_decision_from_heart(state, stream_id, event_hash) {
                match outcome {
                    crate::state::DecisionOutcome::Cancelled => break Err(()),
                    outcome => break Ok(outcome),
                }
            }
        }
        tick = tick.wrapping_add(1);
        match rx.recv_timeout(std::time::Duration::from_millis(150)) {
            Ok(crate::state::DecisionOutcome::Cancelled) => break Err(()),
            Ok(outcome) => break Ok(outcome),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                // Someone evicted the waiter — keep polling Heart.
                std::thread::sleep(std::time::Duration::from_millis(150));
            }
        }
    };
    crate::state::unregister_decision_waiter(state, event_hash);
    result
}

/// The tool-result text a continued turn sees for its decided proposal —
/// the ACTUAL outcome, including the human's edited bytes when they differ
/// from what the model proposed.
fn decision_result_body(
    outcome: &crate::state::DecisionOutcome,
    file_path: &str,
) -> (bool, String) {
    match outcome {
        crate::state::DecisionOutcome::Accepted { human_edited: false, .. } => {
            (true, format!("accepted: {file_path} was approved and written to disk exactly as proposed. Continue."))
        }
        crate::state::DecisionOutcome::Accepted { human_edited: true, final_content } => {
            let shown = final_content
                .as_deref()
                .map(|c| crate::agent_loop::truncate(c, crate::agent_loop::MAX_TOOL_RESULT.saturating_sub(400)))
                .unwrap_or_else(|| "(content not recorded)".to_string());
            (
                true,
                format!(
                    "accepted WITH HUMAN EDITS: {file_path} was written, but the human changed your proposal first. \
                     The content on disk is now:\n{shown}\nContinue from THIS version — do not re-apply your original."
                ),
            )
        }
        crate::state::DecisionOutcome::Rejected => (
            false,
            format!(
                "rejected: the human declined the change to {file_path}. Do not re-propose the same bytes — adjust the approach, or ask what they want instead."
            ),
        ),
        crate::state::DecisionOutcome::WriteFailed(e) => {
            (false, format!("the human accepted the change to {file_path} but writing it failed: {e}. Treat the file as unchanged."))
        }
        crate::state::DecisionOutcome::Answered(a) => (true, format!("the human answered: {a}")),
        crate::state::DecisionOutcome::Cancelled => {
            (false, "stopped by the operator before a decision was made".to_string())
        }
    }
}

/// Record a proposal as a PENDING edit event (the Manual gate's input).
pub(crate) fn propose_edit_event(
    state: &AppState,
    stream_id: &str,
    seat: crate::node::Seat,
    model: &Arc<dyn Model>,
    file_path: &str,
    new_content: &str,
    rationale: &str,
    old_content: String,
    turn_id: &str,
    call_id: &str,
) -> Result<Event, String> {
    let payload = serde_json::json!({
        "file_path": file_path,
        "status": "pending",
        "rationale": rationale,
        "old_content": old_content,
        "new_content": new_content,
        "agent_id": crate::node::agent_id(stream_id, seat),
        "model_id": model.name(),
        "turn_id": turn_id,
        "call_id": call_id,
    })
    .to_string();
    append(
        state,
        stream_id,
        &format!("agent:{}", seat.as_str()),
        "agent.file.changed",
        payload,
        vec![],
    )
}

/// A native ACP edit that Bakery has translated into its normal Review
/// proposal. The adapter performs the eventual file mutation only after a
/// decision; Bakery never writes this proposal itself. Keeping the original
/// input here lets the permission response preserve provider-specific fields
/// while replacing only the reviewed content.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct NativeReviewOperation {
    pub(crate) tool_call_id: String,
    pub(crate) tool_name: String,
    pub(crate) input: serde_json::Value,
    /// `{sha, bytes}` of the whole old file, kept beside the record (GET
    /// /agent/tool-payload) when it is too large to keep in it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) old_full: Option<serde_json::Value>,
}

/// Record an ACP `Edit`/`Write` request as the same `agent.file.changed`
/// proposal consumed by the existing Review pane. The operation is marked
/// explicitly so `agent_accept` records the human decision without racing the
/// native runtime by writing the file itself.
pub(crate) fn propose_native_edit_event(
    state: &AppState,
    stream_id: &str,
    seat: crate::node::Seat,
    model_id: &str,
    file_path: &str,
    new_content: &str,
    rationale: &str,
    old_content: String,
    turn_id: &str,
    operation: NativeReviewOperation,
) -> Result<Event, String> {
    let payload = serde_json::json!({
        "file_path": file_path,
        "status": "pending",
        "rationale": rationale,
        "old_content": old_content,
        "new_content": new_content,
        "agent_id": crate::node::agent_id(stream_id, seat),
        "model_id": model_id,
        "turn_id": turn_id,
        "call_id": operation.tool_call_id,
        "native_review": operation,
    })
    .to_string();
    append(
        state,
        stream_id,
        &format!("agent:{}", seat.as_str()),
        "agent.file.changed",
        payload,
        vec![],
    )
}

/// Resolve `file_path` through the stream jail and write `content` to disk.
/// Shared by human accept and Auto/Rules policy — single place for future
/// hardening (O_NOFOLLOW, re-canonicalize, etc.). Does not append Heart events.
pub(crate) fn write_through_jail(
    state: &AppState,
    stream_id: &str,
    file_path: &str,
    content: &str,
    by: crate::writers::Writer,
) -> Result<std::path::PathBuf, String> {
    let jail = jail_for(state, stream_id)?;
    let target = jail.resolve(file_path)?;
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(&target, content).map_err(|e| format!("write {}: {e}", target.display()))?;
    if let Ok(rel) = target.strip_prefix(jail.root()) {
        crate::writers::wrote(by, jail.root(), &rel.to_string_lossy());
    }
    Ok(target)
}

/// Write mode's on-propose write. The proposal event itself is untouched —
/// it stays a literal record of what was proposed — and this appends a
/// distinct, causally-linked `agent.edit.accepted` with actor `"policy/auto"`.
/// Heart audit is complete; Review shows that actor as decidedBy (`by policy/auto`).
/// StatusBar shows write_mode so the gate itself is visible.
/// Best-effort: the turn has already succeeded, so write failure becomes
/// `agent.edit.failed` rather than failing the turn — never a silent disk write.
#[allow(dead_code)] // Compatibility seam for non-composer callers and tests.
pub(crate) fn maybe_auto_accept(state: &AppState, stream_id: &str, proposal: &Event) {
    // Compatibility seam for existing non-composer callers and tests. New
    // composer turns call `maybe_auto_accept_for_mode` with their explicit
    // per-turn capability mode.
    maybe_auto_accept_for_mode(state, stream_id, proposal, true);
}

/// The agent's own operating mode is the whole write policy: Auto Edit/Auto
/// auto-apply, Manual/Plan never do. There is no node-wide gate, and the
/// Baker is not exempt — a Baker thread the operator explicitly put in Auto
/// works unattended like any other agent. (Dogfood run one's unreviewed
/// Baker edit came from a *node-wide* Auto silently reaching the Baker; with
/// the choice per agent and Manual by default, that cannot recur.)
pub(crate) fn maybe_auto_accept_for_mode(
    state: &AppState,
    stream_id: &str,
    proposal: &Event,
    mode_allows_auto_edit: bool,
) {
    if !mode_allows_auto_edit {
        return;
    }
    // Rule 1: a checkout its agents only read (the Bakery's main checkout)
    // is never written by policy either; the proposal waits for the
    // operator, as in Manual.
    if !crate::tools::commands_write_workspace(state, stream_id) {
        return;
    }
    let _decision = state.edit_decision_lock.lock().unwrap();
    // A fast human can decide after proposal broadcast but before Auto gets
    // this lock. Never overwrite that decision with the original proposal.
    let already_decided = state
        .heart
        .lock()
        .unwrap()
        .read_stream(stream_id)
        .map(|events| {
            events.iter().any(|e| {
                matches!(
                    e.family.as_str(),
                    "agent.edit.accepted" | "agent.edit.rejected" | APPROVAL_CANCELLED_FAMILY
                ) && e.causality.contains(&proposal.event_hash)
            })
        })
        .unwrap_or(true);
    if already_decided {
        return;
    }
    let p: serde_json::Value = match serde_json::from_str(&proposal.payload) {
        Ok(v) => v,
        Err(_) => return,
    };
    if pending_operation_is_stopping(state, &p) {
        return;
    }
    let (Some(file_path), Some(new_content)) = (
        p.get("file_path").and_then(|v| v.as_str()),
        p.get("new_content").and_then(|v| v.as_str()),
    ) else {
        return;
    };

    // The proposal's own payload already carries `agent_id` (`propose_edit_
    // event` stamps it) — read it back here rather than reconstructing it,
    // since this function only has `stream_id`, not the seat that proposed.
    let agent_id = p
        .get("agent_id")
        .and_then(|v| v.as_str())
        .map(str::to_string);

    let by = crate::writers::Writer::Thread(stream_id.to_string());
    match write_through_jail(state, stream_id, file_path, new_content, by) {
        Ok(_target) => {
            let payload = serde_json::json!({
                "decided_event": proposal.event_hash,
                "file_path": file_path,
                "status": "accepted",
            })
            .to_string();
            let _ = append(
                state,
                stream_id,
                "policy/auto",
                "agent.edit.accepted",
                payload,
                vec![proposal.event_hash.clone()],
            );
            if let Some(agent_id) = &agent_id {
                resolve_awaiting_approval(
                    state,
                    agent_id,
                    &proposal.event_hash,
                    ActivityPhase::Complete { note: None },
                );
            }
        }
        Err(reason) => {
            let payload = serde_json::json!({
                "decided_event": proposal.event_hash,
                "file_path": file_path,
                "decision": "accept",
                "reason": reason.clone(),
            })
            .to_string();
            let _ = append(
                state,
                stream_id,
                "policy/auto",
                "agent.edit.failed",
                payload,
                vec![proposal.event_hash.clone()],
            );
            if let Some(agent_id) = &agent_id {
                resolve_awaiting_approval(
                    state,
                    agent_id,
                    &proposal.event_hash,
                    ActivityPhase::Error { reason },
                );
            }
        }
    }
}

/// Native ACP edits must still leave the same proposal/decision trail as an
/// HTTP edit. The permission callback calls this only when the agent's own
/// operating mode is Auto Edit or Auto, and that per-agent choice is the whole
/// policy — no node-wide gate, and a Baker thread in Auto is not exempt.
/// Record acceptance but leave the actual write to the paused native tool
/// call. `decide_with_content` recognizes `native_review` and therefore never
/// races that tool by writing on Bakery's side.
pub(crate) fn maybe_auto_accept_native_review(state: &AppState, stream_id: &str, proposal: &Event) {
    let is_native_review = serde_json::from_str::<serde_json::Value>(&proposal.payload)
        .ok()
        .is_some_and(|payload| {
            payload
                .get("native_review")
                .is_some_and(serde_json::Value::is_object)
        });
    if !is_native_review {
        return;
    }
    // Recorded as the mode's decision, not the operator's click — the same
    // `policy/auto` actor the HTTP auto-accept path writes.
    let _ = decide_as(
        state,
        &AgentDecisionReq {
            stream_id: stream_id.to_string(),
            event_hash: proposal.event_hash.clone(),
        },
        true,
        None,
        "policy/auto",
    );
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct AgentDecisionReq {
    pub(crate) stream_id: String,
    pub(crate) event_hash: String,
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct AgentAcceptReq {
    #[serde(flatten)]
    decision: AgentDecisionReq,
    /// Human-edited replacement, never a path override. The original proposal
    /// remains immutable; the acceptance records exactly what the human wrote.
    new_content: Option<String>,
}

pub(crate) async fn agent_accept(
    State(st): State<AppState>,
    Json(req): Json<AgentAcceptReq>,
) -> Result<Json<Event>, (StatusCode, String)> {
    decide_with_content(&st, &req.decision, true, req.new_content.as_deref())
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

pub(crate) async fn agent_reject(
    State(st): State<AppState>,
    Json(req): Json<AgentDecisionReq>,
) -> Result<Json<Event>, (StatusCode, String)> {
    decide(&st, &req, false)
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct AgentResumeReq {
    pub(crate) stream_id: String,
    #[serde(default)]
    pub(crate) seat: Option<crate::node::Seat>,
}

/// Durable recovery (Checkpoint 2 item 3), the deepseek-harness
/// `interruptedTurnClosers` shape adapted to Heart: a turn that died with
/// the process left `agent.turn.started` with no terminal event. Resume
/// (a) closes it durably with a synthetic terminal naming the restart,
/// then (b) issues a FRESH turn primed from durable facts — the original
/// objective, the recorded tool/effect trail, the plan (todos), and any
/// still-pending decisions — with the explicit two-tier retry rule: retry
/// only read-only/idempotent work freely; VERIFY externally-visible
/// effects before redoing them. Never a blind replay of the dead frame.
pub(crate) async fn agent_resume(
    State(st): State<AppState>,
    Json(req): Json<AgentResumeReq>,
) -> Result<Json<Event>, (StatusCode, String)> {
    crate::actor::spawn_blocking(move || agent_resume_inner(&st, &req))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))
}

#[derive(schemars::JsonSchema, Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SessionChoice {
    /// Resume the same session again.
    Retry,
    /// A new session, briefed from the thread.
    Fresh,
}

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct SessionChoiceReq {
    pub(crate) stream_id: String,
    #[serde(default)]
    pub(crate) seat: Option<crate::node::Seat>,
    pub(crate) choice: SessionChoice,
}

/// `POST /agent/session/choose` — the operator's answer to a session that
/// didn't resume (`agent.session.not_resumed`, awaiting): recorded as
/// `agent.session.chosen` (the answer), then its stopped turn carries on in
/// the background, on the same session tried again or on a new one.
pub(crate) async fn agent_session_choose(
    State(st): State<AppState>,
    Json(req): Json<SessionChoiceReq>,
) -> Result<Json<Event>, (StatusCode, String)> {
    if crate::actor::agent_calling() {
        return Err((StatusCode::FORBIDDEN, "the operator answers this".to_string()));
    }
    crate::actor::spawn_blocking(move || agent_session_choose_inner(&st, &req))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))
}

pub(crate) fn agent_session_choose_inner(state: &AppState, req: &SessionChoiceReq) -> Result<Event, String> {
    use crate::resume::{NOT_RESUMED, SESSION_CHOSEN, SESSION_OPENED};
    let seat = req.seat.unwrap_or(crate::node::Seat::Orchestrator);
    let agent_id = crate::node::agent_id(&req.stream_id, seat);
    let waiting = {
        let events = state.heart.lock().unwrap().read_stream(&req.stream_id).map_err(|e| e.to_string())?;
        events
            .into_iter()
            .rev()
            .filter(|e| [NOT_RESUMED, SESSION_CHOSEN, SESSION_OPENED].contains(&e.family.as_str()))
            .find(|e| serde_json::from_str::<serde_json::Value>(&e.payload).is_ok_and(|p| p["agent_id"] == agent_id))
            .filter(|e| e.family == NOT_RESUMED)
            .ok_or_else(|| "no session is waiting for a choice here".to_string())?
    };
    let session_id = serde_json::from_str::<serde_json::Value>(&waiting.payload).map_err(|e| e.to_string())?["session_id"].clone();
    let choice = match req.choice {
        SessionChoice::Retry => "retry",
        SessionChoice::Fresh => "fresh",
    };
    let chosen = append(
        state,
        &req.stream_id,
        "human",
        SESSION_CHOSEN,
        serde_json::json!({"agent_id": agent_id, "session_id": session_id, "choice": choice}).to_string(),
        vec![waiting.event_hash],
    )?;
    let (state, resume) = (state.clone(), AgentResumeReq { stream_id: req.stream_id.clone(), seat: Some(seat) });
    std::thread::spawn(move || {
        if let Err(e) = agent_resume_inner(&state, &resume) {
            eprintln!("bakery-node: {} did not carry on after the session choice: {e}", resume.stream_id);
        }
    });
    Ok(chosen)
}

/// Resume's briefing opens with this, which is how a resumed turn is told
/// apart: resuming it again briefs the objective it carried, and the HTTP
/// loop does not brief the unfinished turn a second time.
const RESUME_PREFIX: &str = "RESUME after";
const OBJECTIVE_LABEL: &str = "Original objective: ";
/// The notes the cut turn's opening carried, right after its objective.
const NOTES_READ_LABEL: &str = "The notes from other conversations it had read at its start:";

/// The newest turn on an agent that stopped short of its objective.
pub(crate) struct UnfinishedTurn {
    turn_id: String,
    instruction: String,
    /// It already has a terminal event — Resume's own closer, or its
    /// failure — so Resume appends none.
    closed: bool,
    /// Why it stopped, for a turn that failed or hit its step limit; `None`
    /// for one a restart killed.
    failure: Option<String>,
}

/// The unfinished turn on this agent, if any: the newest
/// `agent.turn.started`, on an agent with no live tracked activity (a live
/// turn — including one parked on a decision — is running, not
/// unfinished), when that turn
/// - has no terminal event (a restart killed it);
/// - has only Resume's own closers (confirmed gap 3: an earlier resume
///   closed it but failed before the replacement began — still resumable,
///   without appending a second closer); or
/// - failed or hit its step limit (a network error, a provider outage). A
///   turn that finished, or that the operator stopped, is not resumable.
pub(crate) fn interrupted_turn(
    state: &AppState,
    stream_id: &str,
    seat: crate::node::Seat,
) -> Option<UnfinishedTurn> {
    let agent_id = crate::node::agent_id(stream_id, seat);
    if let Some(activity) = crate::state::agent_activity_snapshot(state, &agent_id) {
        if !matches!(
            activity.phase,
            ActivityPhase::Complete { .. } | ActivityPhase::Error { .. } | ActivityPhase::Cancelled
        ) {
            return None; // genuinely running (or parked) in THIS process
        }
    }
    let events = state.heart.lock().unwrap().read_stream(stream_id).ok()?;
    let started = events.iter().rev().find(|e| {
        e.family == "agent.turn.started"
            && serde_json::from_str::<serde_json::Value>(&e.payload)
                .ok()
                .and_then(|p| {
                    p.get("agent_id")
                        .and_then(|v| v.as_str())
                        .map(|a| a == agent_id)
                })
                .unwrap_or(false)
    })?;
    let p: serde_json::Value = serde_json::from_str(&started.payload).ok()?;
    let turn_id = p.get("turn_id").and_then(|v| v.as_str())?.to_string();
    // Structured field compare, never substring (review finding E: a
    // model echoing another turn's id inside its own untruncated summary
    // text must not fake a terminal for that turn).
    let terminals: Vec<&Event> = events
        .iter()
        .filter(|e| {
            matches!(
                e.family.as_str(),
                "agent.turn.done"
                    | "agent.turn.failed"
                    | "agent.turn.exhausted"
                    | "agent.turn.cancelled"
            ) && serde_json::from_str::<serde_json::Value>(&e.payload)
                .ok()
                .and_then(|q| {
                    q.get("turn_id")
                        .and_then(|v| v.as_str())
                        .map(|t| t == turn_id)
                })
                .unwrap_or(false)
        })
        .collect();
    let instruction = p
        .get("instruction")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if terminals.is_empty() {
        return Some(UnfinishedTurn {
            turn_id,
            instruction,
            closed: false,
            failure: None,
        });
    }
    let all_resume_closers = terminals.iter().all(|e| {
        serde_json::from_str::<serde_json::Value>(&e.payload)
            .ok()
            .and_then(|q| q.get("closed_by_resume").and_then(|v| v.as_bool()))
            == Some(true)
    });
    if all_resume_closers {
        // Since this `started` is the NEWEST for the agent, no replacement
        // turn ever started — the earlier resume died between closing and
        // continuing. Retryable, closer already in place.
        return Some(UnfinishedTurn {
            turn_id,
            instruction,
            closed: true,
            failure: None,
        });
    }
    if terminals
        .iter()
        .any(|e| matches!(e.family.as_str(), "agent.turn.done" | "agent.turn.cancelled"))
    {
        return None;
    }
    let failure = terminals.iter().rev().find_map(|e| match e.family.as_str() {
        "agent.turn.exhausted" => Some("it hit its step limit".to_string()),
        "agent.turn.failed" => Some(
            serde_json::from_str::<serde_json::Value>(&e.payload)
                .ok()
                .and_then(|q| q.get("reason").and_then(|v| v.as_str()).map(str::to_string))
                .filter(|r| !r.trim().is_empty())
                .unwrap_or_else(|| "it failed".to_string()),
        ),
        _ => None,
    })?;
    Some(UnfinishedTurn {
        turn_id,
        instruction,
        closed: true,
        failure: Some(failure),
    })
}

/// A resumed turn's instruction is Resume's own briefing; resuming it again
/// briefs the objective it carried, not the briefing wrapped in another.
fn original_objective(instruction: &str) -> &str {
    if !instruction.starts_with(RESUME_PREFIX) {
        return instruction;
    }
    let Some(start) = instruction
        .find(OBJECTIVE_LABEL)
        .map(|i| i + OBJECTIVE_LABEL.len())
    else {
        return instruction;
    };
    let rest = &instruction[start..];
    let end = [
        "\nThe notes from other conversations",
        "\nWhat provably happened",
        "\nWhat you had written",
        "\nStill pending:",
        "\nContinue the objective",
    ]
    .iter()
    .filter_map(|marker| rest.find(marker))
    .min()
    .unwrap_or(rest.len());
    &rest[..end]
}

/// A briefing on one turn from its durable record, opening with `header`:
/// its original objective, what provably happened (tools, proposals'
/// outcomes), the end of what it had written, what is still pending, and
/// the rule to verify before redoing anything with side effects.
fn turn_briefing(
    state: &AppState,
    stream_id: &str,
    turn_id: &str,
    instruction: &str,
    header: &str,
) -> Result<String, String> {
    let events = state
        .heart
        .lock()
        .unwrap()
        .read_stream(stream_id)
        .map_err(|e| e.to_string())?;
    let mut trail: Vec<String> = Vec::new();
    let mut pending: Vec<String> = Vec::new();
    // A native runtime's tool call is announced, then updated: its trail
    // line, by toolCallId → (index in `trail`, title, status).
    let mut native: Vec<(String, usize, String, String)> = Vec::new();
    // Calls it made through Bakery's MCP server (also reported as its own
    // tool calls): (tool, index in `trail`), counted once below.
    let mut mcp_lines: Vec<(String, usize)> = Vec::new();
    // What a native turn had written before it stopped.
    let mut written = String::new();
    // The notes its opening carried (`messages::Reading::mark`): the
    // objective below is its instruction from before they were added.
    let mut notes_read = String::new();
    for e in &events {
        let p: serde_json::Value =
            serde_json::from_str(&e.payload).unwrap_or(serde_json::Value::Null);
        if p.get("turn_id").and_then(|v| v.as_str()) != Some(turn_id) {
            continue;
        }
        match e.family.as_str() {
            "agent.acp.text" => {
                written.push_str(p.get("text").and_then(|v| v.as_str()).unwrap_or(""));
            }
            crate::messages::READ => {
                notes_read = p.get("text").and_then(|v| v.as_str()).unwrap_or("").to_string();
            }
            "agent.acp.tool_call" => {
                let detail = |key: &str| {
                    p.get("detail")
                        .and_then(|d| d.get(key))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string()
                };
                let id = detail("toolCallId");
                let at = match native.iter().position(|(known, ..)| *known == id) {
                    Some(at) => at,
                    None => {
                        trail.push(String::new());
                        native.push((id, trail.len() - 1, String::new(), String::new()));
                        native.len() - 1
                    }
                };
                let (title, status) = (detail("title"), detail("status"));
                if !title.is_empty() {
                    native[at].2 = title;
                }
                // What Stop did to it outlasts the failure its teardown
                // reports; only finishing after all says more.
                let stopped = crate::agent_loop::stopped_outcome(p.get("event").and_then(|v| v.as_str()).unwrap_or(""));
                if let Some(stopped) = stopped {
                    native[at].3 = stopped.to_string();
                } else if !status.is_empty() && (!native[at].3.starts_with("stopped") || status == "completed") {
                    native[at].3 = status;
                }
            }
            "agent.tool.called" if p.get("stopped").and_then(|v| v.as_str()) == Some("before") => {
                let tool = p.get("tool").and_then(|v| v.as_str()).unwrap_or("tool");
                trail.push(format!("- {tool} ({STOPPED_BEFORE_IT_RAN})"));
            }
            "agent.tool.called" => {
                let tool = p.get("tool").and_then(|v| v.as_str()).unwrap_or("tool");
                let ok = p.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
                let summary = p.get("summary").and_then(|v| v.as_str()).unwrap_or("");
                trail.push(format!(
                    "- {tool} ({}): {}",
                    if ok { "ok" } else { "failed" },
                    crate::agent_loop::truncate(summary, 200)
                ));
                if p.get("transport").and_then(|v| v.as_str()) == Some("mcp") {
                    mcp_lines.push((tool.to_string(), trail.len() - 1));
                }
            }
            "agent.file.changed" => {
                let path = p.get("file_path").and_then(|v| v.as_str()).unwrap_or("?");
                match derive_decision_from_heart(state, stream_id, &e.event_hash) {
                    Some(crate::state::DecisionOutcome::Accepted { .. }) => {
                        trail.push(format!("- proposed {path}: ACCEPTED and written"))
                    }
                    Some(crate::state::DecisionOutcome::Rejected) => {
                        trail.push(format!("- proposed {path}: REJECTED"))
                    }
                    Some(crate::state::DecisionOutcome::WriteFailed(_)) => {
                        trail.push(format!("- proposed {path}: accept FAILED to write"))
                    }
                    Some(crate::state::DecisionOutcome::Cancelled) => {
                        trail.push(format!("- proposed {path}: STOPPED before approval"))
                    }
                    _ => pending.push(format!(
                        "- proposal for {path} is STILL PENDING human review"
                    )),
                }
            }
            "agent.ask.pending" => {
                match derive_decision_from_heart(state, stream_id, &e.event_hash) {
                    None => pending.push("- an ask_user question is STILL UNANSWERED".to_string()),
                    Some(crate::state::DecisionOutcome::Cancelled) => trail
                        .push("- an ask_user question was STOPPED before an answer".to_string()),
                    Some(_) => {}
                }
            }
            _ => {}
        }
    }
    for (_, at, title, status) in &native {
        trail[*at] = if status.is_empty() {
            format!("- {title}")
        } else {
            format!("- {title} ({status})")
        };
    }
    // One MCP record accounts for one runtime row naming the same tool; a
    // record nothing accounts for stays.
    let mut claimed: Vec<usize> = Vec::new();
    for (tool, at) in &mcp_lines {
        let accounted = native.iter().enumerate().find(|(i, (_, _, title, _))| {
            !claimed.contains(i) && crate::agent_loop::names_tool(title, tool)
        });
        if let Some((i, _)) = accounted {
            claimed.push(i);
            trail[*at].clear();
        }
    }
    trail.retain(|line| !line.is_empty());
    let mut briefing = format!(
        "{header}\n{OBJECTIVE_LABEL}{}\n",
        original_objective(instruction)
    );
    if !notes_read.trim().is_empty() {
        briefing.push_str(&format!("{NOTES_READ_LABEL}\n{}\n", notes_read.trim()));
    }
    if !trail.is_empty() {
        briefing.push_str("What provably happened before it stopped:\n");
        for t in trail.iter().rev().take(30).rev() {
            briefing.push_str(t);
            briefing.push('\n');
        }
    }
    if !written.trim().is_empty() {
        briefing.push_str("What you had written before it stopped (its end):\n");
        briefing.push_str(&crate::agent_loop::clip_tail(written.trim(), 2_000));
        briefing.push('\n');
    }
    if !pending.is_empty() {
        briefing.push_str("Still pending:\n");
        for t in &pending {
            briefing.push_str(t);
            briefing.push('\n');
        }
    }
    briefing.push_str(
        "Continue the objective from this state. Retry freely only what is read-only or idempotent; \
         for anything with side effects whose outcome is uncertain (writes, commands that may have run), \
         VERIFY the current state first — do not blindly replay it.",
    );
    Ok(briefing)
}

/// The model the agent's last ended turn ran on.
fn previous_model(state: &AppState, stream_id: &str, agent_id: &str) -> Option<String> {
    let ends = ["agent.turn.done", "agent.turn.failed", "agent.turn.exhausted", "agent.turn.cancelled"];
    let tail = state.heart.lock().unwrap().read_stream_tail(stream_id, 400).ok()?;
    tail.iter().rev().filter(|e| ends.contains(&e.family.as_str())).find_map(|e| {
        let p: serde_json::Value = serde_json::from_str(&e.payload).ok()?;
        (p["agent_id"] == agent_id).then(|| p["model_id"].as_str().map(str::to_string)).flatten()
    })
}

/// A turn the Bakery's restart cut off carries on by itself
/// (`resume::on_boot`), the way Resume would carry it on.
pub(crate) fn resume_after_restart(
    state: &AppState,
    stream_id: &str,
    seat: crate::node::Seat,
) -> Result<Event, String> {
    let req = AgentResumeReq {
        stream_id: stream_id.to_string(),
        seat: Some(seat),
    };
    resume_turn(state, &req, true)
}

fn agent_resume_inner(state: &AppState, req: &AgentResumeReq) -> Result<Event, String> {
    resume_turn(state, req, false)
}

/// Ends a command's turn that a restart or a failure cut off, carrying
/// nothing on: an `agent.turn.cancelled` from the node, its reason the note
/// to send it again, so neither Resume nor the catch-up takes it up as work
/// left to do. Queued messages follow as after any end.
fn close_unrun_command(state: &AppState, stream_id: &str, agent_id: &str, unfinished: &UnfinishedTurn, after_restart: bool) -> Result<Event, String> {
    let command = unfinished.instruction.split_whitespace().next().unwrap_or_default();
    let reason = match after_restart {
        true => format!("The Bakery restarted while {command} was running, and a command isn't carried on as text: send {command} again to run it."),
        false => format!("{command} is a command, and Resume doesn't carry it on as text: send {command} again to run it."),
    };
    let closer = serde_json::json!({
        "reason": reason,
        "agent_id": agent_id,
        "turn_id": unfinished.turn_id,
        "command": true,
        "after_restart": after_restart,
    });
    append(state, stream_id, "node", "agent.turn.cancelled", closer.to_string(), vec![])
}

/// Resume's work; `after_restart`: the Bakery itself does it at boot, and
/// its closer says so, which is how restarts in a row are counted.
fn resume_turn(state: &AppState, req: &AgentResumeReq, after_restart: bool) -> Result<Event, String> {
    let seat = req.seat.unwrap_or(crate::node::Seat::Orchestrator);
    let agent_id = crate::node::agent_id(&req.stream_id, seat);

    // Atomic claim (confirmed gap 3): the agent-turn lease serializes
    // concurrent resumes — exactly one passes the check-and-close section;
    // the loser gets a clean 409-shaped refusal, never a second closer or
    // a duplicate continuation. The replacement turn runs on this same
    // lease, so nothing (a queued message included) can take the agent in
    // between; if that turn fails, the closer stays marked retryable
    // (`closed_by_resume` + no newer turn), so a later resume picks the
    // work back up instead of losing it.
    let claim = crate::state::admit_turn(state, &req.stream_id, seat)
        .map_err(|(_, m)| format!("cannot resume right now: {m}"))?;
    let Some(unfinished) = interrupted_turn(state, &req.stream_id, seat) else {
        drop(claim);
        return Err(
            "nothing to resume — the last turn either finished or is still running".to_string(),
        );
    };
    // A command (`/compact`, a skill) runs only as sent: carried on as a
    // briefing it would reach the runtime as text, and the runtime's own
    // command would never run. Its turn ends instead, saying to send it again.
    if crate::pipe::is_command(&unfinished.instruction) {
        return close_unrun_command(state, &req.stream_id, &agent_id, &unfinished, after_restart);
    }
    let dead_turn_id = unfinished.turn_id;

    // (a) Synthetic closer — durable, marked as Resume's own so a failed
    // continuation stays retryable; appended at most once per dead turn. A
    // turn that failed is closed already, by its own failure.
    if !unfinished.closed {
        append(
            state,
            &req.stream_id,
            "node",
            "agent.turn.failed",
            serde_json::json!({
                "reason": if after_restart {
                    "the Bakery restarted while this turn was running; a fresh turn carries the work on from durable state"
                } else {
                    "the node restarted while this turn was running — closed by Resume; a fresh turn continues the work from durable state"
                },
                "phase": "tools",
                "agent_id": agent_id,
                "turn_id": dead_turn_id,
                "closed_by_resume": true,
                "after_restart": after_restart,
            })
            .to_string(),
            vec![],
        )?;
    }

    // (b) The continuation's briefing, from durable facts only.
    let header = match &unfinished.failure {
        None => format!(
            "{RESUME_PREFIX} a process restart. Your earlier turn on this objective died mid-work; its durable record follows."
        ),
        Some(why) => format!(
            "{RESUME_PREFIX} your earlier turn on this objective stopped before finishing ({}). Its durable record follows.",
            crate::agent_loop::clip_head(why.trim(), 300)
        ),
    };
    let briefing = turn_briefing(state, &req.stream_id, &dead_turn_id, &unfinished.instruction, &header)?;

    // (c) A fresh, ordinary turn — same machinery, same gates — on the
    // claim itself. Released in between, its drop would wake the queue, and
    // a message queued before the restart could take the agent first: the
    // cut-off work would never carry on. The closer's retryable marker
    // covers a failure of the turn.
    turn_holding(
        state,
        &AgentTurnReq {
            stream_id: req.stream_id.clone(),
            file_path: None,
            instruction: briefing,
            seat: Some(seat),
            model_id: None,
            effort: None,
            actor: Some("human".to_string()),
        },
        TurnControl::default(),
        Some(claim),
    )
    .map_err(|e| format!("resume turn failed: {}", e.1))
}

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct AgentAnswerReq {
    pub(crate) stream_id: String,
    /// The `agent.ask.pending` event being answered.
    pub(crate) event_hash: String,
    /// One answer for all the questions, as text — what older clients send.
    #[serde(default)]
    pub(crate) answer: String,
    /// One answer per question, in order. Wins over `answer` when present.
    #[serde(default)]
    pub(crate) answers: Option<Vec<crate::ask::AskAnswer>>,
}

/// Answer a pending `ask_user` question (Checkpoint 2 item 2). The answer
/// is durable (`agent.ask.answered`, causality → the question) and resumes
/// the parked turn as the ask_user call's own tool result. Idempotent: a
/// second answer to the same question refuses.
pub(crate) async fn agent_answer(
    State(st): State<AppState>,
    Json(req): Json<AgentAnswerReq>,
) -> Result<Json<Event>, (StatusCode, String)> {
    crate::actor::spawn_blocking(move || agent_answer_inner(&st, &req))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))
}

pub(crate) fn agent_answer_inner(state: &AppState, req: &AgentAnswerReq) -> Result<Event, String> {
    if req.answers.is_none() && req.answer.trim().is_empty() {
        return Err("answer must not be empty".to_string());
    }
    // Stop, answers, edit decisions, and child-tool decisions all serialize
    // through this one lock. A queued browser click that gets here after Stop
    // must fail before it can wake the parked operation.
    let _decision = state.edit_decision_lock.lock().unwrap();
    let pending = {
        let events = state
            .heart
            .lock()
            .unwrap()
            .read_stream(&req.stream_id)
            .map_err(|e| e.to_string())?;
        events
            .iter()
            .find(|e| e.event_hash == req.event_hash)
            .cloned()
            .ok_or_else(|| format!("no event {} on stream {}", req.event_hash, req.stream_id))?
    };
    if pending.family != "agent.ask.pending" {
        return Err(format!(
            "event {} is not a pending question",
            req.event_hash
        ));
    }
    let p: serde_json::Value =
        serde_json::from_str(&pending.payload).unwrap_or(serde_json::Value::Null);
    if pending_operation_is_stopping(state, &p) {
        return Err(format!(
            "question {} was stopped before it could be answered",
            req.event_hash
        ));
    }
    if let Some(outcome) = derive_decision_from_heart(state, &req.stream_id, &req.event_hash) {
        return Err(match outcome {
            crate::state::DecisionOutcome::Cancelled => {
                format!(
                    "question {} was stopped before it could be answered",
                    req.event_hash
                )
            }
            _ => format!("question {} was already answered", req.event_hash),
        });
    }
    // Structured answers are checked against the questions they answer, and
    // still recorded as one line of text for HTTP models and older clients.
    let (answer, answers) = match &req.answers {
        Some(answers) => {
            let questions = crate::ask::pending_questions(&p);
            crate::ask::validate_answers(&questions, answers)?;
            (crate::ask::answer_summary(&questions, answers), Some(answers))
        }
        None => (req.answer.trim().to_string(), None),
    };
    let ev = append(
        state,
        &req.stream_id,
        "human",
        "agent.ask.answered",
        serde_json::json!({
            "decided_event": req.event_hash,
            "answer": answer,
            "answers": answers,
            "agent_id": p.get("agent_id"),
            "turn_id": p.get("turn_id"),
        })
        .to_string(),
        vec![req.event_hash.clone()],
    )?;
    let turn_continues = crate::state::notify_decision(
        state,
        &req.event_hash,
        crate::state::DecisionOutcome::Answered(answer),
    );
    if let Some(agent_id) = p.get("agent_id").and_then(|v| v.as_str()) {
        let next_phase = if turn_continues {
            ActivityPhase::ContactingModel
        } else {
            ActivityPhase::Complete { note: None }
        };
        resolve_awaiting_approval(state, agent_id, &req.event_hash, next_phase);
    }
    Ok(ev)
}

/// Accept (write the proposed content, then record acceptance) or reject (record
/// only) a pending `agent.file.changed` event identified by its `event_hash`.
/// The decision event links back to the proposal via the causality DAG.
pub(crate) fn decide(
    state: &AppState,
    req: &AgentDecisionReq,
    accept: bool,
) -> Result<Event, String> {
    decide_with_content(state, req, accept, None)
}

pub(crate) fn decide_with_content(
    state: &AppState,
    req: &AgentDecisionReq,
    accept: bool,
    edited: Option<&str>,
) -> Result<Event, String> {
    decide_as(state, req, accept, edited, "human")
}

/// [`decide_with_content`] recorded as `actor`: `"human"` for an operator's
/// click, `"policy/auto"` when the agent's own Auto/Auto-edit mode decided —
/// so the trail never claims the operator clicked what their mode accepted.
pub(crate) fn decide_as(
    state: &AppState,
    req: &AgentDecisionReq,
    accept: bool,
    edited: Option<&str>,
    actor: &str,
) -> Result<Event, String> {
    // Held for the whole decision, not just this check — same "counted
    // activity" discard already waits on for env's one-shot attaches
    // (04c-BURN §4). A discarded (or mid-discard) stream's worktree may
    // already be gone — an accept trying to write through the jail would
    // fail confusingly instead of with a plain "discarded" (04b-FENCE §4).
    let _activity = admit_activity(state, &req.stream_id).map_err(|(_, msg)| msg)?;
    let _decision = state.edit_decision_lock.lock().unwrap();
    let events = state
        .heart
        .lock()
        .unwrap()
        .read_stream(&req.stream_id)
        .map_err(|e| e.to_string())?;
    let pending = events
        .iter()
        .find(|e| e.event_hash == req.event_hash)
        .ok_or_else(|| format!("no event {} in stream {}", req.event_hash, req.stream_id))?;
    if pending.family != "agent.file.changed" {
        return Err(format!("event {} is not a pending edit", req.event_hash));
    }
    let mut p: serde_json::Value =
        serde_json::from_str(&pending.payload).map_err(|e| e.to_string())?;
    if p.get("status").and_then(|s| s.as_str()) != Some("pending") {
        return Err(format!(
            "event {} is not pending (already decided or invalid)",
            req.event_hash
        ));
    }
    if pending_operation_is_stopping(state, &p) {
        return Err(format!(
            "proposal {} was stopped before it could be decided",
            req.event_hash
        ));
    }
    // Idempotency: Stop has its own clear error, rather than looking like a
    // generic duplicate decision to a Review click that was already in the
    // browser event queue when the operator pressed Stop.
    let stopped = events.iter().any(|e| {
        e.family == APPROVAL_CANCELLED_FAMILY && e.causality.iter().any(|h| h == &req.event_hash)
    });
    if stopped {
        return Err(format!(
            "proposal {} was stopped before it could be decided",
            req.event_hash
        ));
    }
    // Any ordinary terminal decision also refuses a second decision.
    let already = events.iter().any(|e| {
        (e.family == "agent.edit.accepted" || e.family == "agent.edit.rejected")
            && e.causality.iter().any(|h| h == &req.event_hash)
    });
    if already {
        return Err(format!(
            "event {} already has a terminal decision",
            req.event_hash
        ));
    }

    if let Some(content) = edited {
        if !accept {
            return Err("edited content requires acceptance".into());
        }
        p["new_content"] = serde_json::Value::String(content.to_string());
        p["human_edited"] = serde_json::Value::Bool(true);
    }

    // Everything above is a malformed request (bad hash, wrong family, already
    // decided) — no trace value. From here the proposal is confirmed pending,
    // so a failure IS the decision failing and must leave a record.
    decide_confirmed(state, req, accept, &p, actor).map_err(|reason| {
        let mut payload = serde_json::json!({
            "decided_event": req.event_hash,
            "decision": if accept { "accept" } else { "reject" },
            "reason": reason,
        });
        if let Some(fp) = p.get("file_path").and_then(|v| v.as_str()) {
            payload["file_path"] = serde_json::Value::String(fp.to_string());
        }
        let _ = append(
            state,
            &req.stream_id,
            "node",
            "agent.edit.failed",
            payload.to_string(),
            vec![req.event_hash.clone()],
        );
        reason
    })
}

/// The decision itself, once the proposal is confirmed pending: write on
/// accept (through the jail), then record the outcome either way.
pub(crate) fn decide_confirmed(
    state: &AppState,
    req: &AgentDecisionReq,
    accept: bool,
    p: &serde_json::Value,
    actor: &str,
) -> Result<Event, String> {
    let file_path = p
        .get("file_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "pending event missing file_path".to_string())?
        .to_string();
    let native_review = p
        .get("native_review")
        .is_some_and(serde_json::Value::is_object);

    // A landed change (turn snapshots) is already on disk: keeping it writes
    // nothing unless the operator edited it first; undoing it restores the
    // file as it was before the turn.
    let landed = p.get("landed").filter(|l| l.is_object());
    let (family, status) = if accept && landed.is_some() {
        let hidden = landed.is_some_and(|l| l.get("hidden").is_some_and(|h| !h.is_null()));
        if hidden && p.get("human_edited").and_then(|v| v.as_bool()) == Some(true) {
            // Its content never goes into the history, and an edit would.
            return Err(
                "a change to a credential or denied file can only be kept or undone".into(),
            );
        }
        if p.get("human_edited").and_then(|v| v.as_bool()) == Some(true) {
            let edited = p
                .get("new_content")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            write_through_jail(
                state,
                &req.stream_id,
                &file_path,
                edited,
                crate::writers::Writer::Operator,
            )?;
        }
        ("agent.edit.accepted", "accepted")
    } else if accept {
        // A reviewed proposal must not overwrite a newer edit unnoticed.
        // Re-resolve the path, including for user-authored replacement bytes.
        if let Some(expected) = p.get("old_content").and_then(|v| v.as_str()) {
            let target = jail_for(state, &req.stream_id)?.resolve(&file_path)?;
            // A native proposal is compared as it was made: in the view
            // Claude Code edits in (any encoding, `\n` line ends), or only
            // the part it changes, read as the review read it.
            let current = if native_review {
                crate::pipe::native_review_baseline(&p["native_review"], &target, &file_path).ok()
            } else {
                let bytes = match std::fs::read(target) {
                    Ok(bytes) => bytes,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
                    Err(e) => return Err(format!("cannot read current file: {e}")),
                };
                Some(String::from_utf8(bytes).map_err(|e| format!("cannot read current file: {e}"))?)
            };
            if current.as_deref() != Some(expected) {
                return Err("file changed since this proposal was made; request a fresh proposal before accepting".into());
            }
        }
        // An ordinary proposal writes through the jail here. A native ACP
        // proposal is different: its reviewed bytes are returned to the paused
        // tool call, which performs the one real write. That prevents an
        // accepted edit from being written twice and lets edited acceptance
        // reach the operation Claude actually executes.
        let new_content = p
            .get("new_content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "pending event missing new_content".to_string())?;
        if !native_review {
            let by = if p.get("human_edited").and_then(|v| v.as_bool()) == Some(true) {
                crate::writers::Writer::Operator
            } else {
                crate::writers::Writer::Thread(req.stream_id.clone())
            };
            write_through_jail(state, &req.stream_id, &file_path, new_content, by)?;
        }
        ("agent.edit.accepted", "accepted")
    } else {
        if let Some(landed) = landed {
            crate::snapshots::restore(state, &req.stream_id, &file_path, landed).map_err(|why| {
                match crate::routes::project::saved_over(state, &req.stream_id, &req.event_hash) {
                    true => format!("you edited {file_path} since that turn, so it wasn't restored: your edit stays"),
                    false => why,
                }
            })?;
        }
        ("agent.edit.rejected", "rejected")
    };

    let mut payload = serde_json::json!({
        "decided_event": req.event_hash,
        "file_path": file_path,
        "status": status,
    });
    if accept && (native_review || p.get("human_edited").and_then(|v| v.as_bool()) == Some(true)) {
        payload["new_content"] = p["new_content"].clone();
        if p.get("human_edited").and_then(|v| v.as_bool()) == Some(true) {
            payload["human_edited"] = serde_json::Value::Bool(true);
        }
    }
    if native_review {
        payload["native_review"] = serde_json::json!({
            "tool_call_id": p["native_review"].get("tool_call_id"),
            "tool_name": p["native_review"].get("tool_name"),
        });
    }
    if landed.is_some() {
        payload["landed"] = serde_json::Value::Bool(true);
    }

    // Link decision → proposal in the causality DAG.
    let ev = append(
        state,
        &req.stream_id,
        actor,
        family,
        payload.to_string(),
        vec![req.event_hash.clone()],
    )?;
    if let Some(landed) = landed {
        crate::snapshots::decided(state, &req.stream_id, landed);
    }
    // Continue-after-decision: wake the parked turn (if one is alive) with
    // the actual outcome. A live waiter means the turn CONTINUES — its
    // honest next phase is contacting the model again, not Complete.
    let outcome = if accept {
        let human_edited = p.get("human_edited").and_then(|v| v.as_bool()) == Some(true);
        crate::state::DecisionOutcome::Accepted {
            human_edited,
            final_content: human_edited
                .then(|| {
                    p.get("new_content")
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                })
                .flatten(),
        }
    } else {
        crate::state::DecisionOutcome::Rejected
    };
    let turn_continues = crate::state::notify_decision(state, &req.event_hash, outcome);
    // The proposal's own payload already carries `agent_id` — read it back
    // rather than reconstructing it (this function has no `seat`).
    if let Some(agent_id) = p.get("agent_id").and_then(|v| v.as_str()) {
        let next_phase = if native_review || turn_continues {
            ActivityPhase::ContactingModel
        } else {
            ActivityPhase::Complete { note: None }
        };
        resolve_awaiting_approval(state, agent_id, &req.event_hash, next_phase);
    }
    Ok(ev)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::NodeConfig;
    use crate::routes::project::open_stream;
    use crate::testutil::*;
    use bakery_agent::ProposedEdit;
    use std::sync::Arc;

    #[tokio::test]
    async fn human_edited_accept_writes_and_records_exact_bytes_without_mutating_proposal() {
        let (_dir, state, wt, hash, stream_id) = setup_with_pending();
        let req = AgentAcceptReq {
            decision: AgentDecisionReq {
                stream_id: stream_id.clone(),
                event_hash: hash.clone(),
            },
            new_content: Some("human revision\n".into()),
        };
        let ev = agent_accept(State(state.clone()), Json(req))
            .await
            .unwrap()
            .0;
        assert_eq!(
            std::fs::read_to_string(wt.join("f.txt")).unwrap(),
            "human revision\n"
        );
        let p: serde_json::Value = serde_json::from_str(&ev.payload).unwrap();
        assert_eq!(p["new_content"], "human revision\n");
        assert_eq!(p["human_edited"], true);
        assert_eq!(ev.causality, vec![hash.clone()]);
        let events = state.heart.lock().unwrap().read_stream(&stream_id).unwrap();
        let original = events.iter().find(|e| e.event_hash == hash).unwrap();
        assert!(!original.payload.contains("human revision"));
        state.node.lock().unwrap().write_mode = WriteMode::Auto;
        maybe_auto_accept(&state, &stream_id, original);
        assert_eq!(
            std::fs::read_to_string(wt.join("f.txt")).unwrap(),
            "human revision\n"
        );
    }

    #[test]
    fn edited_accept_refuses_stale_file_and_remains_pending() {
        let (_dir, state, wt, hash, stream_id) = setup_with_pending();
        std::fs::write(wt.join("f.txt"), "newer work\n").unwrap();
        let req = AgentDecisionReq {
            stream_id: stream_id.clone(),
            event_hash: hash,
        };
        let err = decide_with_content(&state, &req, true, Some("overwrite")).unwrap_err();
        assert!(err.contains("file changed"), "{err}");
        assert_eq!(
            std::fs::read_to_string(wt.join("f.txt")).unwrap(),
            "newer work\n"
        );
        let events = state.heart.lock().unwrap().read_stream(&stream_id).unwrap();
        assert_eq!(events.last().unwrap().family, "agent.edit.failed");
        assert!(!events.iter().any(|e| e.family == "agent.edit.accepted"));
    }

    #[test]
    fn concurrent_edited_accepts_decide_only_once() {
        let (_dir, state, _wt, hash, stream_id) = setup_with_pending();
        let barrier = std::sync::Barrier::new(2);
        let wins = std::thread::scope(|scope| {
            let handles: Vec<_> = ["first", "second"]
                .into_iter()
                .map(|content| {
                    let (state, barrier, hash, stream_id) = (&state, &barrier, &hash, &stream_id);
                    scope.spawn(move || {
                        barrier.wait();
                        decide_with_content(
                            state,
                            &AgentDecisionReq {
                                stream_id: stream_id.clone(),
                                event_hash: hash.clone(),
                            },
                            true,
                            Some(content),
                        )
                        .is_ok()
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().unwrap() as usize)
                .sum::<usize>()
        });
        assert_eq!(wins, 1);
    }

    struct LongRunModel {
        calls: std::sync::atomic::AtomicUsize,
        compact_calls: std::sync::atomic::AtomicUsize,
        cancel_after: Option<usize>,
    }
    impl Model for LongRunModel {
        fn name(&self) -> &str {
            "long-run-test"
        }
        fn supports_chat(&self) -> bool {
            true
        }
        fn propose_edit(&self, _: &bakery_agent::EditRequest) -> Result<ProposedEdit, String> {
            Err("native tools only".into())
        }
        fn chat_turn_streaming(
            &self,
            system: &str,
            messages: &[bakery_agent::ChatMsg],
            tools: &[bakery_agent::ToolDecl],
            _: &mut dyn FnMut(&str),
            cancel: &AtomicBool,
        ) -> Result<StreamEnd, String> {
            assert!(system.contains("orchestrator agent for this project"));
            assert!(messages[0].content.contains("inspect thoroughly"));
            if tools.is_empty() {
                self.compact_calls.fetch_add(1, Ordering::SeqCst);
                return Ok(StreamEnd::Completed(ChatReply::Text(
                    "Read f.txt. No files changed. Continue inspecting as requested.".into(),
                )));
            }
            let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            if self.cancel_after == Some(n) {
                cancel.store(true, Ordering::Release);
                return Ok(StreamEnd::Stopped {
                    partial_text: String::new(),
                    reason: StopReason::Cancelled,
                });
            }
            if n > 12 {
                return Ok(StreamEnd::Completed(ChatReply::Text(
                    "verified after twelve reads".into(),
                )));
            }
            Ok(StreamEnd::Completed(ChatReply::ToolCalls(vec![
                bakery_agent::ToolCall {
                    id: format!("call-{n}"),
                    name: "read_file".into(),
                    arguments: serde_json::json!({"path":"f.txt","offset": n}),
                },
            ])))
        }
    }

    #[test]
    fn native_turn_continues_past_eight_with_compaction_and_cancellation() {
        for cancel_after in [None, Some(10)] {
            let dir = tempfile::tempdir().unwrap();
            let repo = make_repo(&dir.path().join("repo"));
            let model = Arc::new(LongRunModel {
                calls: 0.into(),
                compact_calls: 0.into(),
                cancel_after,
            });
            let state = state_with(dir.path(), model.clone());
            assert!(state.node.lock().unwrap().agent_max_steps.is_none());
            let ctx = open_stream(&state, &open_req(&repo, "long"))
                .unwrap()
                .context;
            // Many readable lines: exceed working context without hitting the
            // read tool's per-line limit or its result truncation budget.
            std::fs::write(
                std::path::Path::new(&ctx.worktree_path).join("f.txt"),
                "0123456789\n".repeat(950),
            )
            .unwrap();
            let ev = agent_turn_inner(
                &state,
                &AgentTurnReq {
                    stream_id: ctx.stream_id.clone(),
                    instruction: "inspect thoroughly".into(),
                    file_path: None,
                    seat: None,
                    model_id: None,
                    effort: None,
                    actor: None,
                },
            )
            .unwrap();
            assert_eq!(
                ev.family,
                if cancel_after.is_some() {
                    "agent.turn.cancelled"
                } else {
                    "agent.turn.done"
                }
            );
            assert!(model.calls.load(Ordering::SeqCst) > 8);
            assert!(model.compact_calls.load(Ordering::SeqCst) > 0);
            let events = state
                .heart
                .lock()
                .unwrap()
                .read_stream(&ctx.stream_id)
                .unwrap();
            assert!(events.iter().any(|e| e.family == "agent.context.compacted"));
            assert!(!events.iter().any(|e| e.family == "agent.turn.exhausted"));
        }
    }

    #[test]
    fn agent_turn_proposes_pending_edit_without_writing() {
        let (_dir, state, wt, hash, stream_id) = setup_with_pending();
        let target = wt.join("f.txt");
        let events = state.heart.lock().unwrap().read_stream(&stream_id).unwrap();
        let ev = events.iter().find(|e| e.event_hash == hash).unwrap();

        assert_eq!(ev.family, "agent.file.changed");
        assert_eq!(
            ev.actor, "agent:orchestrator",
            "actor is the seat alone, never the model"
        );
        let payload: serde_json::Value = serde_json::from_str(&ev.payload).unwrap();
        assert_eq!(
            payload["model_id"], "scripted",
            "which model ran lives in the payload instead"
        );
        assert_eq!(payload["status"], "pending");
        assert!(payload["new_content"]
            .as_str()
            .unwrap()
            .contains("bakery stub agent: add marker"));
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "line one\n",
            "agent turn must not write the file in Manual mode"
        );
    }

    /// Review finding D: the classic thrash — the SAME failing exact-replace
    /// `edit` over and over — must climb the stall ladder and halt with a
    /// named cause, because with step/turn caps deliberately gone the guard
    /// is the loop's only bound.
    #[test]
    fn repeated_failing_exact_replace_edits_are_halted_by_the_stall_guard() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let bad = r#"{"tool":"edit","args":{"path":"f.txt","old_string":"THIS ANCHOR DOES NOT EXIST","new_string":"x"}}"#;
        let model = Arc::new(ScriptedModel::new(&[
            bad,
            bad,
            bad,
            bad,
            bad,
            bad,
            "unreached",
        ]));
        let state = state_with(dir.path(), model);
        let resp = open_stream(&state, &open_req(&repo, "editstall")).unwrap();

        let err = agent_turn_inner(
            &state,
            &AgentTurnReq {
                model_id: None,
                effort: None,
                actor: None,
                stream_id: resp.context.stream_id.clone(),
                file_path: None,
                instruction: "apply the change".into(),
                seat: None,
            },
        )
        .unwrap_err();
        assert!(format!("{err:?}").contains("stalled"), "{err:?}");

        let stream = state
            .heart
            .lock()
            .unwrap()
            .read_stream(&resp.context.stream_id)
            .unwrap();
        assert!(
            stream
                .iter()
                .any(|e| e.family == "agent.stall.detected" && e.payload.contains("diagnose")),
            "a diagnose step precedes the halt"
        );
        assert!(
            stream
                .iter()
                .any(|e| e.family == "agent.turn.failed" && e.payload.contains("stalled")),
            "the terminal names the stall"
        );
    }

    /// Confirmed gap 4: the permanent Baker's source checkout is
    /// read-scoped by contract — `run` there must NOT be able to write the
    /// workspace (a command write would bypass the always-manual proposal
    /// gate), while an ordinary project socket keeps the writable room.
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn run_cannot_write_the_permanent_bakers_source_but_can_write_a_project_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let repo = bakery_repo_for_test(dir.path());
        let state = state_with(dir.path(), Arc::new(StubModel));
        state.node.lock().unwrap().bakery_source_root = Some(repo.to_string_lossy().into_owned());
        let home = crate::routes::baker::ensure_permanent_baker(&state, None, None).unwrap();
        let cache_root = crate::state::cache_root(&state);

        let baker_ctx = crate::state::context_for(&state, &home.stream_id).unwrap();
        let baker_jail = crate::tools::Jail::new(std::path::Path::new(&baker_ctx.worktree_path))
            .unwrap()
            .with_exec(
                state.clone(),
                home.stream_id.clone(),
                cache_root.clone(),
                "none",
            );
        let out = baker_jail
            .run(
                &[
                    "/bin/sh".into(),
                    "-c".into(),
                    "echo pwn > baker-pwn.txt".into(),
                ],
                None,
            )
            .unwrap();
        assert!(
            !out.starts_with("exit_code: 0"),
            "the write must fail on Baker's checkout: {out}"
        );
        assert!(
            !std::path::Path::new(&baker_ctx.worktree_path)
                .join("baker-pwn.txt")
                .exists(),
            "nothing may be written into the read-scoped Baker source"
        );
        assert!(
            out.contains("read-scoped"),
            "the refusal is explained to the model: {out}"
        );

        // Contrast: an ordinary project socket's room IS writable.
        let proj_repo = make_repo(&dir.path().join("proj"));
        let resp = open_stream(&state, &open_req(&proj_repo, "rwproj")).unwrap();
        let proj_jail = crate::tools::Jail::new(std::path::Path::new(&resp.context.worktree_path))
            .unwrap()
            .with_exec(
                state.clone(),
                resp.context.stream_id.clone(),
                cache_root,
                "none",
            );
        let out = proj_jail
            .run(
                &["/bin/sh".into(), "-c".into(), "echo built > out.txt".into()],
                None,
            )
            .unwrap();
        assert!(out.starts_with("exit_code: 0"), "{out}");
        assert!(std::path::Path::new(&resp.context.worktree_path)
            .join("out.txt")
            .exists());
    }

    /// BAKER-THREADS-03: a thread binds the SAME read-scoped source in place,
    /// so its `run` must be exactly as unable to write it as the main Baker's.
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn run_cannot_write_the_bakery_source_from_a_baker_thread_either() {
        let dir = tempfile::tempdir().unwrap();
        let repo = bakery_repo_for_test(dir.path());
        let state = state_with(dir.path(), Arc::new(StubModel));
        state.node.lock().unwrap().bakery_source_root = Some(repo.to_string_lossy().into_owned());
        crate::routes::baker::ensure_permanent_baker(&state, None, None).unwrap();
        let thread =
            crate::baker_threads::create_thread(&state, &Default::default(), "human", None, None)
                .unwrap()
                .thread
                .stream_id;

        let ctx = crate::state::context_for(&state, &thread).unwrap();
        assert!(
            !ctx.worktree_path.is_empty(),
            "the fixture thread must be rooted"
        );
        let jail = crate::tools::Jail::new(std::path::Path::new(&ctx.worktree_path))
            .unwrap()
            .with_exec(
                state.clone(),
                thread.clone(),
                crate::state::cache_root(&state),
                "none",
            );
        let out = jail
            .run(
                &[
                    "/bin/sh".into(),
                    "-c".into(),
                    "echo pwn > thread-pwn.txt".into(),
                ],
                None,
            )
            .unwrap();
        assert!(
            !out.starts_with("exit_code: 0"),
            "the write must fail: {out}"
        );
        assert!(
            !std::path::Path::new(&ctx.worktree_path)
                .join("thread-pwn.txt")
                .exists(),
            "nothing may be written into the read-scoped source from a thread"
        );
    }

    /// Review finding A: selecting a file on a rootless (chat-only) socket
    /// refuses with the same honesty as the rootless no-selection path —
    /// never a raw canonicalize error.
    #[test]
    fn a_selected_file_on_a_rootless_socket_refuses_honestly() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let project_id = crate::catalog::create_project(&state, "ChatOnly", None)
            .unwrap()
            .0
            .project_id;
        let task = crate::tasks::task_create(
            &state,
            crate::tasks::TaskCreateReq {
                parent_project_id: project_id.clone(),
                title: "T".into(),
                work_order: "wo".into(),
                contract: Default::default(),
            },
        )
        .unwrap();
        let err = agent_turn_inner(
            &state,
            &AgentTurnReq {
                model_id: None,
                effort: None,
                actor: None,
                stream_id: task.task.stream_id.clone(),
                file_path: Some("notes.md".into()),
                instruction: "read the selection".into(),
                seat: None,
            },
        )
        .unwrap_err();
        let msg = format!("{err:?}");
        assert!(
            msg.contains("no workspace root"),
            "honest refusal, not a canonicalize error: {msg}"
        );
        assert!(!msg.contains("No such file or directory"), "{msg}");
    }

    /// Checkpoint 2 item 3: after a restart killed a turn mid-work, Resume
    /// (a) durably closes the dead turn with a synthetic terminal naming
    /// the restart, and (b) issues a fresh turn briefed from durable facts
    /// — objective, recorded effects, still-pending decisions — with the
    /// verify-before-redo rule. No blind replay.
    #[test]
    fn resume_closes_the_dead_turn_and_continues_from_durable_state() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let model = Arc::new(ScriptedModel::new(&["picked the work back up"]));
        let state = state_with(dir.path(), model.clone());
        let resp = open_stream(&state, &open_req(&repo, "resume")).unwrap();
        let sid = resp.context.stream_id.clone();
        let agent_id = crate::node::agent_id(&sid, crate::node::Seat::Orchestrator);

        // A dead turn's durable remains: started + one tool call + one
        // still-pending proposal, no terminal event (the process "died").
        append(&state, &sid, "human", "agent.turn.started",
            serde_json::json!({"instruction": "refactor the parser", "agent_id": agent_id, "turn_id": "turn_dead", "mode": "manual", "seat": "orchestrator"}).to_string(), vec![]).unwrap();
        append(&state, &sid, "agent:orchestrator", "agent.tool.called",
            serde_json::json!({"tool": "read_file", "ok": true, "summary": "read parser.rs", "agent_id": agent_id, "turn_id": "turn_dead", "call_id": "c1"}).to_string(), vec![]).unwrap();
        append(&state, &sid, "agent:orchestrator", "agent.file.changed",
            serde_json::json!({"file_path": "parser.rs", "status": "pending", "rationale": "r", "old_content": "a", "new_content": "b", "agent_id": agent_id, "model_id": "m", "turn_id": "turn_dead", "call_id": "c2"}).to_string(), vec![]).unwrap();

        let ev = agent_resume_inner(
            &state,
            &AgentResumeReq {
                stream_id: sid.clone(),
                seat: None,
            },
        )
        .unwrap();
        assert_eq!(ev.family, "agent.turn.done");

        let events = state.heart.lock().unwrap().read_stream(&sid).unwrap();
        let closer = events
            .iter()
            .find(|e| e.family == "agent.turn.failed" && e.payload.contains("turn_dead"))
            .expect("the dead turn must be durably closed");
        assert_eq!(closer.actor, "node");
        assert!(closer.payload.contains("restarted"), "{}", closer.payload);

        let captured = model.captured.lock().unwrap();
        let (_, opening) = captured.first().expect("the resume turn ran");
        assert!(
            opening.contains("RESUME after a process restart"),
            "{opening}"
        );
        assert!(
            opening.contains("refactor the parser"),
            "the original objective survives: {opening}"
        );
        assert!(
            opening.contains("read_file (ok)"),
            "the durable trail is briefed: {opening}"
        );
        assert!(
            opening.contains("STILL PENDING"),
            "pending decisions are named: {opening}"
        );
        assert!(
            opening.contains("VERIFY"),
            "the no-blind-replay rule is stated: {opening}"
        );

        // Idempotence: the closer exists now, so a second resume refuses.
        let err = agent_resume_inner(
            &state,
            &AgentResumeReq {
                stream_id: sid.clone(),
                seat: None,
            },
        )
        .unwrap_err();
        assert!(err.contains("nothing to resume"), "{err}");
    }

    /// Confirmed gap 3, race half: two concurrent resumes on the same dead
    /// turn produce exactly ONE closer and ONE continuation; the loser gets
    /// a clean refusal, never a duplicate of either.
    #[test]
    fn concurrent_resumes_produce_exactly_one_closer_and_one_continuation() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let model = Arc::new(ScriptedModel::new(&["picked the work back up"]));
        let state = state_with(dir.path(), model);
        let resp = open_stream(&state, &open_req(&repo, "raceresume")).unwrap();
        let sid = resp.context.stream_id.clone();
        let agent_id = crate::node::agent_id(&sid, crate::node::Seat::Orchestrator);
        append(&state, &sid, "human", "agent.turn.started",
            serde_json::json!({"instruction": "long job", "agent_id": agent_id, "turn_id": "turn_race", "mode": "manual", "seat": "orchestrator"}).to_string(), vec![]).unwrap();

        let barrier = Arc::new(std::sync::Barrier::new(2));
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let st = state.clone();
                let sid = sid.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    agent_resume_inner(
                        &st,
                        &AgentResumeReq {
                            stream_id: sid,
                            seat: None,
                        },
                    )
                })
            })
            .collect();
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        let oks = results.iter().filter(|r| r.is_ok()).count();
        assert_eq!(oks, 1, "exactly one resume must win: {results:?}");

        let events = state.heart.lock().unwrap().read_stream(&sid).unwrap();
        let closers = events
            .iter()
            .filter(|e| e.family == "agent.turn.failed" && e.payload.contains("closed_by_resume"))
            .count();
        assert_eq!(closers, 1, "exactly one synthetic closer");
        let continuations = events
            .iter()
            .filter(|e| {
                e.family == "agent.turn.started"
                    && e.payload.contains("RESUME after a process restart")
            })
            .count();
        assert_eq!(continuations, 1, "exactly one continuation turn");
    }

    /// Confirmed gap 3, retry half: an earlier resume that closed the dead
    /// turn but died before its continuation started must NOT strand the
    /// work — the closer is marked as Resume's own, so a later resume picks
    /// it up (without a second closer).
    #[test]
    fn a_resume_that_failed_after_closing_stays_retryable() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let model = Arc::new(ScriptedModel::new(&["continuing at last"]));
        let state = state_with(dir.path(), model);
        let resp = open_stream(&state, &open_req(&repo, "retryresume")).unwrap();
        let sid = resp.context.stream_id.clone();
        let agent_id = crate::node::agent_id(&sid, crate::node::Seat::Orchestrator);
        append(&state, &sid, "human", "agent.turn.started",
            serde_json::json!({"instruction": "stranded job", "agent_id": agent_id, "turn_id": "turn_stranded", "mode": "manual", "seat": "orchestrator"}).to_string(), vec![]).unwrap();
        // The durable trace of a resume that died between closing and
        // continuing: the marked closer, no continuation turn after it.
        append(&state, &sid, "node", "agent.turn.failed",
            serde_json::json!({"reason": "closed by Resume", "phase": "tools", "agent_id": agent_id, "turn_id": "turn_stranded", "closed_by_resume": true}).to_string(), vec![]).unwrap();

        let ev = agent_resume_inner(
            &state,
            &AgentResumeReq {
                stream_id: sid.clone(),
                seat: None,
            },
        )
        .unwrap();
        assert_eq!(ev.family, "agent.turn.done");
        let events = state.heart.lock().unwrap().read_stream(&sid).unwrap();
        let closers = events
            .iter()
            .filter(|e| e.family == "agent.turn.failed" && e.payload.contains("closed_by_resume"))
            .count();
        assert_eq!(closers, 1, "the retry must not append a second closer");
    }

    #[test]
    fn resume_refuses_when_the_last_turn_finished_normally() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let resp = open_stream(&state, &open_req(&repo, "noresume")).unwrap();
        let sid = resp.context.stream_id.clone();
        agent_turn_inner(
            &state,
            &AgentTurnReq {
                model_id: None,
                effort: None,
                actor: None,
                stream_id: sid.clone(),
                file_path: None,
                instruction: "say hi".into(),
                seat: None,
            },
        )
        .unwrap();
        let err = agent_resume_inner(
            &state,
            &AgentResumeReq {
                stream_id: sid,
                seat: None,
            },
        )
        .unwrap_err();
        assert!(err.contains("nothing to resume"), "{err}");
    }

    /// The operator's report: a native turn failed on a network blip and
    /// there was no way to continue it. A failed turn is resumable — no
    /// second closer (its failure already closes it) — and the briefing
    /// carries why it stopped, its native tool calls and its last words. A
    /// briefing that itself failed resumes the objective it carried, not a
    /// briefing nested in a briefing.
    #[test]
    fn a_failed_turn_resumes_with_its_native_trail_and_its_objective() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let model = Arc::new(ScriptedModel::new(&["back on it", "back on it again"]));
        let state = state_with(dir.path(), model.clone());
        let resp = open_stream(&state, &open_req(&repo, "failresume")).unwrap();
        let sid = resp.context.stream_id.clone();
        let agent_id = crate::node::agent_id(&sid, crate::node::Seat::Orchestrator);
        let add = |actor: &str, family: &str, payload: serde_json::Value| {
            append(&state, &sid, actor, family, payload.to_string(), vec![]).unwrap();
        };
        add("human", "agent.turn.started", serde_json::json!({"instruction": "hide the tab strip", "agent_id": agent_id, "turn_id": "turn_net", "seat": "orchestrator"}));
        add("agent:orchestrator", "agent.acp.tool_call", serde_json::json!({"event": "tool_call", "agent_id": agent_id, "turn_id": "turn_net", "detail": {"toolCallId": "c1", "title": "Read DockviewHost.svelte", "status": "pending"}}));
        add("agent:orchestrator", "agent.acp.tool_call", serde_json::json!({"event": "tool_call_update", "agent_id": agent_id, "turn_id": "turn_net", "detail": {"toolCallId": "c1", "status": "completed"}}));
        add("agent:orchestrator", "agent.acp.text", serde_json::json!({"agent_id": agent_id, "turn_id": "turn_net", "text": "Checking which selectors depend on the controls."}));
        add("node", "agent.turn.failed", serde_json::json!({"reason": "ACP session/prompt failed: API Error: Can't reach the API server (ENOTFOUND)", "phase": "model", "agent_id": agent_id, "turn_id": "turn_net"}));

        let ev = agent_resume_inner(&state, &AgentResumeReq { stream_id: sid.clone(), seat: None }).unwrap();
        assert_eq!(ev.family, "agent.turn.done");
        let events = state.heart.lock().unwrap().read_stream(&sid).unwrap();
        assert_eq!(
            events.iter().filter(|e| e.family == "agent.turn.failed").count(),
            1,
            "the failure already closes the turn; Resume adds no closer"
        );
        {
            let captured = model.captured.lock().unwrap();
            let (_, opening) = captured.first().expect("the resume turn ran");
            assert!(opening.contains("stopped before finishing (ACP session/prompt failed"), "{opening}");
            assert!(opening.contains("Original objective: hide the tab strip\n"), "{opening}");
            assert!(opening.contains("- Read DockviewHost.svelte (completed)"), "{opening}");
            assert!(opening.contains("Checking which selectors depend on the controls."), "{opening}");
            assert!(!opening.contains("did not finish. It is here for reference"), "Resume's briefing is not doubled by the unfinished-turn note: {opening}");
        }

        // The resumed turn finished; nothing is left to resume.
        let err = agent_resume_inner(&state, &AgentResumeReq { stream_id: sid.clone(), seat: None }).unwrap_err();
        assert!(err.contains("nothing to resume"), "{err}");

        // A resume briefing that failed in its turn resumes its objective.
        let briefing = format!(
            "{RESUME_PREFIX} a process restart. Your earlier turn died.\n{OBJECTIVE_LABEL}build the parser\nWhat provably happened before it stopped:\n- read_file (ok)\nContinue the objective from this state."
        );
        add("human", "agent.turn.started", serde_json::json!({"instruction": briefing, "agent_id": agent_id, "turn_id": "turn_again", "seat": "orchestrator"}));
        add("node", "agent.turn.failed", serde_json::json!({"reason": "provider outage", "phase": "model", "agent_id": agent_id, "turn_id": "turn_again"}));
        agent_resume_inner(&state, &AgentResumeReq { stream_id: sid, seat: None }).unwrap();
        let captured = model.captured.lock().unwrap();
        let (_, opening) = captured.last().unwrap();
        assert!(opening.contains("Original objective: build the parser\nWhat provably happened"), "{opening}");
        assert!(
            !opening.contains(&format!("{OBJECTIVE_LABEL}{RESUME_PREFIX}")),
            "the objective is not itself a briefing: {opening}"
        );
    }

    /// A Bakery tool a native runtime called is recorded twice — by the
    /// runtime and by Bakery's MCP server. The briefing lists it once, and
    /// still lists an MCP call no runtime row accounts for.
    #[test]
    fn resume_lists_a_bakery_tool_call_once_not_twice() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let model = Arc::new(ScriptedModel::new(&["ok"]));
        let state = state_with(dir.path(), model.clone());
        let resp = open_stream(&state, &open_req(&repo, "mcpdup")).unwrap();
        let sid = resp.context.stream_id.clone();
        let agent_id = crate::node::agent_id(&sid, crate::node::Seat::Orchestrator);
        let add = |actor: &str, family: &str, payload: serde_json::Value| {
            append(&state, &sid, actor, family, payload.to_string(), vec![]).unwrap();
        };
        add("human", "agent.turn.started", serde_json::json!({"instruction": "tidy the memory", "agent_id": agent_id, "turn_id": "turn_mcp", "seat": "orchestrator"}));
        add("agent:orchestrator", "agent.acp.tool_call", serde_json::json!({"event": "tool_call_update", "agent_id": agent_id, "turn_id": "turn_mcp", "detail": {"toolCallId": "c1", "title": "mcp__bakery__memory_read", "status": "completed"}}));
        add("agent:orchestrator", "agent.tool.called", serde_json::json!({"tool": "memory_read", "ok": true, "summary": "[]", "agent_id": agent_id, "turn_id": "turn_mcp", "transport": "mcp"}));
        add("agent:orchestrator", "agent.tool.called", serde_json::json!({"tool": "gates_list", "ok": true, "summary": "none", "agent_id": agent_id, "turn_id": "turn_mcp", "transport": "mcp"}));
        add("node", "agent.turn.failed", serde_json::json!({"reason": "connection reset", "phase": "model", "agent_id": agent_id, "turn_id": "turn_mcp"}));

        agent_resume_inner(&state, &AgentResumeReq { stream_id: sid, seat: None }).unwrap();
        let opening = model.captured.lock().unwrap().first().unwrap().1.clone();
        assert_eq!(opening.matches("memory_read").count(), 1, "{opening}");
        assert!(opening.contains("- mcp__bakery__memory_read (completed)"), "{opening}");
        assert!(opening.contains("- gates_list (ok)"), "an unaccounted MCP call stays: {opening}");
    }

    /// A command (`/compact`) a restart cut off is never carried on as text:
    /// its turn ends with a note to send it again, no turn runs and the
    /// model is never asked; Resume on one that failed says the same; and
    /// neither is resumable after.
    #[test]
    fn a_cut_off_command_is_never_carried_on_as_text() {
        let dir = tempfile::tempdir().unwrap();
        let model = Arc::new(ScriptedModel::new(&["must never be asked"]));
        let state = state_with(dir.path(), model.clone());
        let sid = crate::catalog::create_chat(&state, "resume-command").unwrap().stream_id;
        let seat = crate::node::Seat::Orchestrator;
        let agent_id = crate::node::agent_id(&sid, seat);
        let add = |actor: &str, family: &str, payload: serde_json::Value| {
            append(&state, &sid, actor, family, payload.to_string(), vec![]).unwrap();
        };
        let started = |turn: &str| serde_json::json!({"instruction": "/compact keep the plan", "agent_id": agent_id, "turn_id": turn, "seat": "orchestrator"});
        let turns = || state.heart.lock().unwrap().events_of_family(&sid, "agent.turn.started").unwrap().len();

        add("human", "agent.turn.started", started("turn_cut"));
        let closed = resume_after_restart(&state, &sid, seat).unwrap();
        assert_eq!((closed.family.as_str(), closed.actor.as_str()), ("agent.turn.cancelled", "node"));
        let said: serde_json::Value = serde_json::from_str(&closed.payload).unwrap();
        assert_eq!(said["turn_id"], "turn_cut");
        assert!(said["reason"].as_str().unwrap().contains("send /compact again"), "{said}");
        assert_eq!(turns(), 1, "nothing runs it as text");
        assert!(model.captured.lock().unwrap().is_empty(), "the model was never asked");
        assert!(interrupted_turn(&state, &sid, seat).is_none());

        add("human", "agent.turn.started", started("turn_failed"));
        add("node", "agent.turn.failed", serde_json::json!({"reason": "connection reset", "phase": "model", "agent_id": agent_id, "turn_id": "turn_failed"}));
        let closed = agent_resume_inner(&state, &AgentResumeReq { stream_id: sid.clone(), seat: None }).unwrap();
        let said: serde_json::Value = serde_json::from_str(&closed.payload).unwrap();
        assert!(said["reason"].as_str().unwrap().contains("Resume doesn't carry it on as text"), "{said}");
        assert_eq!(turns(), 2);
        assert!(model.captured.lock().unwrap().is_empty());
        assert!(interrupted_turn(&state, &sid, seat).is_none());
    }

    /// A resumed turn hears what Stop did to the cut turn's calls, as the
    /// catch-up does: never ran, or had started.
    #[test]
    fn a_resume_briefing_says_what_stop_did_to_each_call() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let sid = crate::catalog::create_chat(&state, "stop-briefing").unwrap().stream_id;
        let agent_id = crate::node::agent_id(&sid, crate::node::Seat::Orchestrator);
        let add = |family: &str, payload: serde_json::Value| {
            append(&state, &sid, "agent:orchestrator", family, payload.to_string(), vec![]).unwrap();
        };
        let native = |event: &str, detail: serde_json::Value| {
            add(crate::pipe::ACP_TOOL_EVENT_FAMILY, serde_json::json!({"event": event, "agent_id": agent_id, "turn_id": "turn_x", "detail": detail}));
        };
        add("agent.turn.started", serde_json::json!({"instruction": "build", "agent_id": agent_id, "turn_id": "turn_x"}));
        native("tool_call", serde_json::json!({"toolCallId": "a", "title": "make all", "status": "in_progress"}));
        native(crate::pipe::STOPPED_AFTER, serde_json::json!({"toolCallId": "a"}));
        native(crate::pipe::STOPPED_BEFORE, serde_json::json!({"toolCallId": "b", "title": "rm -rf build"}));
        native("tool_call_update", serde_json::json!({"toolCallId": "b", "status": "failed"}));
        add("agent.tool.called", serde_json::json!({"tool": "run", "ok": false, "stopped": "before", "summary": STOPPED_BEFORE_IT_RAN, "agent_id": agent_id, "turn_id": "turn_x"}));
        let briefing = turn_briefing(&state, &sid, "turn_x", "build", &format!("{RESUME_PREFIX} a stop.")).unwrap();
        for line in ["- make all (stopped after it ran)", "- rm -rf build (stopped before it ran)", "- run (stopped before it ran)"] {
            assert!(briefing.contains(line), "{line} in {briefing}");
        }
    }

    /// A turn that read notes from other conversations and then failed is
    /// resumed with them, after its objective, which stays its instruction
    /// alone. A note left since waits: a resumed turn's briefing stays as it
    /// is, and the next ordinary turn reads it.
    #[test]
    fn a_resumed_turn_is_briefed_with_the_notes_the_cut_turn_had_read() {
        let dir = tempfile::tempdir().unwrap();
        let model = Arc::new(ScriptedModel::new(&["back on it"]));
        let state = state_with(dir.path(), model.clone());
        let sid = crate::catalog::create_chat(&state, "notes-resume").unwrap().stream_id;
        let seat = crate::node::Seat::Orchestrator;
        let agent_id = crate::node::agent_id(&sid, seat);
        let add = |actor: &str, family: &str, payload: serde_json::Value| {
            append(&state, &sid, actor, family, payload.to_string(), vec![]).unwrap();
        };
        add("human", "agent.turn.started", serde_json::json!({"instruction": "build the parser", "agent_id": agent_id, "turn_id": "turn_cut", "seat": "orchestrator"}));
        add("node", crate::messages::READ, serde_json::json!({"agent_id": agent_id, "turn_id": "turn_cut", "text": "[Note from w1 (s1), id msg_1]\nthe grammar moved"}));
        add("node", "agent.turn.failed", serde_json::json!({"reason": "connection reset", "phase": "model", "agent_id": agent_id, "turn_id": "turn_cut"}));
        let later = crate::messages::MessageNote { id: "msg_2".into(), from: "s1".into(), name: "w1".into(), reply_to: None, text: "left since".into() };
        crate::messages::leave(&state, &sid, seat, "agent:s1:orchestrator", later).unwrap();

        agent_resume_inner(&state, &AgentResumeReq { stream_id: sid.clone(), seat: None }).unwrap();
        let opening = model.captured.lock().unwrap().first().unwrap().1.clone();
        assert!(opening.contains(&format!("{OBJECTIVE_LABEL}build the parser\n{NOTES_READ_LABEL}\n[Note from w1 (s1), id msg_1]\nthe grammar moved\n")), "{opening}");
        assert!(!opening.contains("left since"), "{opening}");
        assert_eq!(crate::messages::unread(&state, &sid, &agent_id).len(), 1, "it waits for an ordinary turn");
        let briefing = format!("{RESUME_PREFIX} a process restart.\n{OBJECTIVE_LABEL}build the parser\n{NOTES_READ_LABEL}\n[Note from w1 (s1), id msg_1]\nthe grammar moved\nContinue the objective from this state.");
        assert_eq!(original_objective(&briefing), "build the parser", "resumed again, the objective alone");
    }

    /// W1-T over an API runtime: the thinking a model reports for a call
    /// lands as a stretch, before the turn's end, never in its reply.
    #[test]
    fn an_api_runtimes_thinking_lands_as_a_stretch_before_the_turn_ends() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let model = Arc::new(ScriptedModel::new(&["the answer"]).with_thinking("weighed both ways"));
        let state = state_with(dir.path(), model.clone());
        let sid = open_stream(&state, &open_req(&repo, "apithought")).unwrap().context.stream_id;
        let req = AgentTurnReq { model_id: None, effort: None, actor: None, stream_id: sid.clone(), file_path: None, instruction: "think".into(), seat: None };
        agent_turn_inner(&state, &req).unwrap();

        let events = state.heart.lock().unwrap().read_stream(&sid).unwrap();
        let at = |family: &str| events.iter().position(|e| e.family == family).unwrap_or_else(|| panic!("no {family}"));
        assert!(at(crate::pipe::THOUGHT_FAMILY) < at("agent.turn.done"), "the thinking, then the end");
        let thought: serde_json::Value = serde_json::from_str(&events[at(crate::pipe::THOUGHT_FAMILY)].payload).unwrap();
        assert_eq!((thought["text"].as_str(), thought["shared"].as_bool()), (Some("weighed both ways"), Some(true)));
        let done: serde_json::Value = serde_json::from_str(&events[at("agent.turn.done")].payload).unwrap();
        assert!(!done.to_string().contains("weighed"), "never in the reply: {done}");
    }

    /// An agent's next turn opens with each file the operator saved since its
    /// last turn began, once; its first turn gets no such note.
    #[test]
    fn a_turn_opens_with_the_files_the_operator_saved_since_the_last_one() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let sid = "wt:noted";
        let agent = crate::node::agent_id(sid, crate::node::Seat::Orchestrator);
        let fact = |family: &str, payload: serde_json::Value| {
            append(&state, sid, "human", family, payload.to_string(), vec![]).unwrap();
        };
        let saved = |file: &str| fact(crate::routes::project::OPERATOR_EDIT_FAMILY, serde_json::json!({"file_path": file}));
        saved("early.txt");
        fact("agent.turn.started", serde_json::json!({"agent_id": agent, "turn_id": "t1"}));
        assert_eq!(operator_edits_note(&state, sid, &agent, "t1"), None, "its first turn");
        saved("src/app.ts");
        saved("README.md");
        saved("src/app.ts");
        fact("agent.turn.started", serde_json::json!({"agent_id": agent, "turn_id": "t2"}));
        assert_eq!(
            operator_edits_note(&state, sid, &agent, "t2").as_deref(),
            Some("The operator edited src/app.ts since your last turn.\nThe operator edited README.md since your last turn.")
        );
    }

    /// The other half of "it can't remember what we were doing": an HTTP
    /// runtime keeps no session, so after a failed turn the next one —
    /// "continue", typed — must be told that turn's request in full, not hot
    /// memory's 180-character line.
    #[test]
    fn a_turn_after_a_failed_one_is_briefed_on_it_in_full() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let model = Arc::new(ScriptedModel::new(&["continuing"]));
        let state = state_with(dir.path(), model.clone());
        let resp = open_stream(&state, &open_req(&repo, "httpnote")).unwrap();
        let sid = resp.context.stream_id.clone();
        let agent_id = crate::node::agent_id(&sid, crate::node::Seat::Orchestrator);
        let long_request = format!("hide the tab strip until there are multiple tabs; {}and move the sidebar toggle into the sidebar", "keep every existing shortcut working; ".repeat(8));
        append(&state, &sid, "human", "agent.turn.started",
            serde_json::json!({"instruction": long_request, "agent_id": agent_id, "turn_id": "turn_prev", "seat": "orchestrator"}).to_string(), vec![]).unwrap();
        append(&state, &sid, "agent:orchestrator", "agent.tool.called",
            serde_json::json!({"tool": "read_file", "ok": true, "summary": "read", "agent_id": agent_id, "turn_id": "turn_prev"}).to_string(), vec![]).unwrap();
        append(&state, &sid, "node", "agent.turn.failed",
            serde_json::json!({"reason": "connection reset", "phase": "model", "agent_id": agent_id, "turn_id": "turn_prev"}).to_string(), vec![]).unwrap();

        agent_turn_inner(&state, &AgentTurnReq { model_id: None, effort: None, actor: None, stream_id: sid.clone(), file_path: None, instruction: "continue".into(), seat: None }).unwrap();
        let opening = model.captured.lock().unwrap().first().unwrap().1.clone();
        assert!(opening.contains(&long_request), "the whole request, not a 180-char line: {opening}");
        assert!(opening.contains("did not finish — it failed: connection reset"), "{opening}");
        assert!(opening.contains("Tools it ran: read_file (ok)"), "{opening}");
        assert!(opening.contains("continue"), "{opening}");

        // Once a turn finishes, the next one carries no such note.
        let model2 = Arc::new(ScriptedModel::new(&["ok"]));
        let state2 = state_with(dir.path().join("second").as_path(), model2.clone());
        let resp2 = open_stream(&state2, &open_req(&repo, "httpnote2")).unwrap();
        agent_turn_inner(&state2, &AgentTurnReq { model_id: None, effort: None, actor: None, stream_id: resp2.context.stream_id.clone(), file_path: None, instruction: "hello".into(), seat: None }).unwrap();
        assert!(!model2.captured.lock().unwrap()[0].1.contains("did not finish"));
    }

    /// Checkpoint 2 item 5: a model repeating one failing call does not get
    /// an arbitrary cutoff — it gets warned, then a recorded diagnosis
    /// demand, and finally a HALT whose reason names the stall, with
    /// `agent.stall.detected` events on the stream as the visible trail.
    #[test]
    fn repeated_identical_failing_calls_are_diagnosed_then_halted_not_quota_capped() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let same_bad_call = r#"{"tool":"read_file","args":{"path":"does-not-exist.txt"}}"#;
        let model = Arc::new(ScriptedModel::new(&[
            same_bad_call,
            same_bad_call,
            same_bad_call,
            same_bad_call,
            same_bad_call,
            same_bad_call,
            "unreached",
        ]));
        let state = state_with(dir.path(), model);
        let resp = open_stream(&state, &open_req(&repo, "stall")).unwrap();

        let err = agent_turn_inner(
            &state,
            &AgentTurnReq {
                model_id: None,
                effort: None,
                actor: None,
                stream_id: resp.context.stream_id.clone(),
                file_path: None,
                instruction: "read the file".into(),
                seat: None,
            },
        )
        .unwrap_err();
        let msg = format!("{err:?}");
        assert!(
            msg.contains("stalled"),
            "the failure names the stall, not a step count: {msg}"
        );

        let stream = state
            .heart
            .lock()
            .unwrap()
            .read_stream(&resp.context.stream_id)
            .unwrap();
        let stalls: Vec<_> = stream
            .iter()
            .filter(|e| e.family == "agent.stall.detected")
            .collect();
        assert!(
            stalls.iter().any(|e| e.payload.contains("diagnose")),
            "a diagnose step must be durably recorded before the halt"
        );
        assert!(
            stalls.iter().any(|e| e.payload.contains("halt")),
            "and the halt itself is recorded"
        );
        assert!(
            stream
                .iter()
                .any(|e| e.family == "agent.turn.failed" && e.payload.contains("stalled")),
            "the turn's terminal event carries the stall reason"
        );
    }

    /// Checkpoint 2 item 7: a durable `memory.written` note actually
    /// reaches the model's opening message on the next turn — before this,
    /// L1 memory was written faithfully and never injected anywhere.
    #[test]
    fn a_durable_memory_note_reaches_the_next_turns_opening_message() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let model = Arc::new(ScriptedModel::new(&["noted."]));
        let state = state_with(dir.path(), model.clone());
        let resp = open_stream(&state, &open_req(&repo, "mem")).unwrap();

        crate::routes::memory::memory_write_inner(
            &state,
            &crate::routes::memory::MemoryWriteReq {
                stream_id: resp.context.stream_id.clone(),
                id: Some("decision-http-client".into()),
                layer: None,
                sensitivity: None,
                text: "we use ureq, never reqwest, for blocking calls".into(),
                shape: Default::default(),
            },
        )
        .unwrap();

        agent_turn_inner(
            &state,
            &AgentTurnReq {
                model_id: None,
                effort: None,
                actor: None,
                stream_id: resp.context.stream_id.clone(),
                file_path: None,
                instruction: "which http client do we use?".into(),
                seat: None,
            },
        )
        .unwrap();

        let captured = model.captured.lock().unwrap();
        let (_, opening) = captured.first().expect("model saw a prompt");
        assert!(
            opening.contains("we use ureq, never reqwest"),
            "the durable note must be retrievable in the opening message: {opening}"
        );
        let (index, instruction) = (opening.find("[Memory:").unwrap(), opening.find("Instruction:").unwrap());
        assert!(index < instruction, "the index comes before the instruction, so the start caches: {opening}");
    }

    /// A native session gets the memory's index once, as it starts (see
    /// pipe's first-prompt test), never on every prompt's envelope; the
    /// notes a turn's words match ride on its message, on every transport.
    /// Memory survives a model or transport change because it never lived
    /// in one. The operator's private notes reach no model.
    #[test]
    fn memory_opens_a_session_and_matching_notes_ride_each_message() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let resp = open_stream(&state, &open_req(&repo, "memacp")).unwrap();
        crate::routes::memory::memory_write_inner(
            &state,
            &crate::routes::memory::MemoryWriteReq {
                stream_id: resp.context.stream_id.clone(),
                id: Some("port".into()),
                layer: None,
                sensitivity: None,
                text: "the dev server runs on port 7171".into(),
                shape: Default::default(),
            },
        )
        .unwrap();
        crate::routes::memory::memory_write_inner(
            &state,
            &crate::routes::memory::MemoryWriteReq {
                stream_id: resp.context.stream_id.clone(),
                id: Some("pin".into()),
                layer: None,
                sensitivity: Some("private".into()),
                text: "the operator's own port note".into(),
                shape: Default::default(),
            },
        )
        .unwrap();
        let ctx = crate::state::context_for(&state, &resp.context.stream_id).unwrap();
        let envelope = acp_domain_envelope(&state, &ctx, crate::node::Seat::Orchestrator, "");
        assert!(!envelope.contains("port 7171"), "not on every prompt: {envelope}");
        let index = crate::memory_index::turn_index(&state, &resp.context.stream_id, crate::memory_index::INDEX_TOKENS).unwrap();
        assert!(index.contains("port 7171") && !index.contains("operator's own"), "{index}");
        let matching = crate::memory_index::matching(&state, &resp.context.stream_id, "which port does the dev server use?").unwrap().0;
        assert!(matching.contains("port 7171") && !matching.contains("operator's own"), "{matching}");
        assert!(crate::memory_index::matching(&state, &resp.context.stream_id, "unrelated words entirely").is_none());
    }

    /// Checkpoint 2 item 1, stated directly: a selected file rides the
    /// NORMAL loop — full system prompt (persona + tool table) and the
    /// file folded into the opening message as a hint — never the deleted
    /// single-shot `propose_edit` path (which had no system prompt at all
    /// and exactly one blind whole-file rewrite).
    #[test]
    fn a_selected_file_is_context_for_the_normal_loop_not_a_single_shot_path() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let model = Arc::new(ScriptedModel::new(&[
            r#"{"tool":"read_file","args":{"path":"f.txt"}}"#,
            "read it — one line",
        ]));
        let state = state_with(dir.path(), model.clone());
        let resp = open_stream(&state, &open_req(&repo, "selctx")).unwrap();

        let ev = agent_turn_inner(
            &state,
            &AgentTurnReq {
                model_id: None,
                effort: None,
                actor: None,
                stream_id: resp.context.stream_id.clone(),
                file_path: Some("f.txt".into()),
                instruction: "summarize the selected file".into(),
                seat: None,
            },
        )
        .unwrap();
        assert_eq!(
            ev.family, "agent.turn.done",
            "the loop ran and answered — no forced proposal"
        );

        let captured = model.captured.lock().unwrap();
        let (system, opening) = captured.first().expect("the model saw a real prompt");
        assert!(
            !system.is_empty(),
            "the loop always sends a system prompt (the legacy path sent none)"
        );
        assert!(
            system.contains("read_file"),
            "the tool table is present: {system}"
        );
        assert!(
            opening.contains("f.txt"),
            "the selection is a context hint in the opening message: {opening}"
        );

        // And the model actually used a tool — impossible on the old path.
        let stream = state
            .heart
            .lock()
            .unwrap()
            .read_stream(&resp.context.stream_id)
            .unwrap();
        assert!(stream.iter().any(|e| e.family == "agent.tool.called"));
    }

    /// A selected file that escapes the jail is refused at the door — it
    /// must not become a poisoned "context hint" either.
    #[test]
    fn a_selected_file_outside_the_worktree_is_still_refused() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let resp = open_stream(&state, &open_req(&repo, "selesc")).unwrap();
        let err = agent_turn_inner(
            &state,
            &AgentTurnReq {
                model_id: None,
                effort: None,
                actor: None,
                stream_id: resp.context.stream_id.clone(),
                file_path: Some("../../outside.txt".into()),
                instruction: "read it".into(),
                seat: None,
            },
        )
        .unwrap_err();
        assert!(format!("{err:?}").contains("escapes"), "{err:?}");
    }

    #[test]
    fn auto_mode_turn_writes_file_and_appends_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let model = Arc::new(ScriptedModel::new(&[
            r#"{"edit":{"file_path":"f.txt","new_content":"line one\nbakery stub agent: add marker\n","rationale":"add marker"}}"#,
            "marker added",
        ]));
        // The node-wide write mode no longer gates anything — Manual here
        // proves it. The agent's own Auto is what writes.
        let state = state_with_write_mode(dir.path(), model, WriteMode::Manual);
        let resp = open_stream(&state, &open_req(&repo, "wtAuto")).unwrap();
        agent_control::set_operating_mode(
            &state,
            &resp.context.stream_id,
            crate::node::Seat::Orchestrator,
            agent_control::OperatingMode::Auto,
        )
        .unwrap();
        let wt = std::path::PathBuf::from(&resp.context.worktree_path);

        let ev = agent_turn_inner(
            &state,
            &AgentTurnReq {
                model_id: None,
                effort: None,
                actor: None,
                stream_id: resp.context.stream_id.clone(),
                file_path: Some("f.txt".into()),
                instruction: "add marker".into(),
                seat: None,
            },
        )
        .unwrap();

        // Continue-after-decision: the Auto-decided write feeds back into
        // the same turn, which then finishes normally.
        assert_eq!(ev.family, "agent.turn.done");

        assert!(
            std::fs::read_to_string(wt.join("f.txt"))
                .unwrap()
                .contains("bakery stub agent: add marker"),
            "Auto mode must write the file immediately"
        );

        let events = state
            .heart
            .lock()
            .unwrap()
            .read_stream(&resp.context.stream_id)
            .unwrap();
        let proposal = events
            .iter()
            .find(|e| e.family == "agent.file.changed")
            .expect("proposal recorded");
        let accepted = events
            .iter()
            .find(|e| e.family == "agent.edit.accepted")
            .expect("Auto mode must append agent.edit.accepted — never a silent disk write");
        assert_eq!(
            accepted.actor, "policy/auto",
            "audit must show a policy decided, not a human"
        );
        assert_eq!(
            accepted.causality,
            vec![proposal.event_hash.clone()],
            "accept must cite the proposal it wrote"
        );
    }

    /// An edit the agent's own Auto mode accepted is recorded as the mode's
    /// decision (`policy/auto`), never as the operator's click — the trail and
    /// every "accepted by" label must tell the two apart.
    #[test]
    fn native_auto_accept_is_recorded_as_the_mode_not_the_operator() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with_write_mode(dir.path(), Arc::new(StubModel), WriteMode::Manual);
        let resp = open_stream(&state, &open_req(&repo, "wtNativeAuto")).unwrap();
        let stream_id = resp.context.stream_id.clone();
        let proposal = propose_native_edit_event(
            &state,
            &stream_id,
            crate::node::Seat::Orchestrator,
            "claude-native",
            "f.txt",
            "line one\nauto\n",
            "Edit f.txt",
            "line one\n".to_string(),
            "turn-native-auto",
            NativeReviewOperation {
                tool_call_id: "toolu_auto".into(),
                tool_name: "Edit".into(),
                input: serde_json::json!({ "file_path": "f.txt", "old_string": "line one\n", "new_string": "line one\nauto\n" }),
                old_full: None,
            },
        )
        .unwrap();

        maybe_auto_accept_native_review(&state, &stream_id, &proposal);

        let events = state.heart.lock().unwrap().read_stream(&stream_id).unwrap();
        let accepted = events
            .iter()
            .find(|e| e.family == "agent.edit.accepted")
            .expect("the agent's Auto mode accepts the native edit");
        assert_eq!(accepted.actor, "policy/auto", "the mode decided, not the operator");
        assert_eq!(accepted.causality, vec![proposal.event_hash.clone()]);
    }

    /// Dogfood run one: a node-wide Auto silently reached the Baker and its
    /// first edit landed on disk with no diff shown. The node-wide gate is
    /// gone; only the Baker's own operating mode can auto-apply, and with no
    /// choice recorded it is Manual — so a node-wide Auto still leaves the
    /// permanent Baker's edit a pending proposal.
    #[test]
    fn node_wide_auto_never_bypasses_review_on_the_permanent_baker_stream() {
        let dir = tempfile::tempdir().unwrap();
        let repo = bakery_repo_for_test(dir.path());
        let model = Arc::new(ScriptedModel::new(&[
            r#"{"edit":{"file_path":"f.txt","new_content":"line one\nadd marker\n","rationale":"add marker"}}"#,
        ]));
        let state = state_with_write_mode(dir.path(), model, WriteMode::Auto);
        state.node.lock().unwrap().bakery_source_root = Some(repo.to_string_lossy().into_owned());
        let home = crate::routes::baker::ensure_permanent_baker(&state, None, None).unwrap();

        // The Baker proposal stays pending (never auto-accepted), so the
        // turn parks awaiting the human — issue it on a thread and read the
        // proposal off the stream, the continue-after-decision shape.
        let pending_hash =
            spawn_turn_and_await_pending(&state, &home.stream_id, Some("f.txt"), "add marker");
        let events = state
            .heart
            .lock()
            .unwrap()
            .read_stream(&home.stream_id)
            .unwrap();
        let ev = events
            .iter()
            .find(|e| e.event_hash == pending_hash)
            .unwrap()
            .clone();

        assert_eq!(
            ev.family, "agent.file.changed",
            "the proposal itself still lands"
        );
        assert!(
            !std::fs::read_to_string(repo.join("f.txt"))
                .unwrap()
                .contains("add marker"),
            "Auto mode must NOT write the permanent Baker's proposal to disk"
        );
        let events = state
            .heart
            .lock()
            .unwrap()
            .read_stream(&home.stream_id)
            .unwrap();
        assert!(
            events.iter().all(|e| e.family != "agent.edit.accepted"),
            "no auto-accept may be recorded on the permanent Baker stream — the human decides"
        );
    }

    /// Same for a Baker thread: a node set to Auto does not make the thread
    /// auto-apply; only the thread's own operating mode can.
    #[test]
    fn node_wide_auto_never_bypasses_review_on_a_baker_thread() {
        let dir = tempfile::tempdir().unwrap();
        let repo = bakery_repo_for_test(dir.path());
        let model = Arc::new(ScriptedModel::new(&[
            r#"{"edit":{"file_path":"f.txt","new_content":"line one\nthread marker\n","rationale":"add marker"}}"#,
        ]));
        let state = state_with_write_mode(dir.path(), model, WriteMode::Auto);
        state.node.lock().unwrap().bakery_source_root = Some(repo.to_string_lossy().into_owned());
        crate::routes::baker::ensure_permanent_baker(&state, None, None).unwrap();
        let thread =
            crate::baker_threads::create_thread(&state, &Default::default(), "human", None, None)
                .unwrap()
                .thread
                .stream_id;

        let pending_hash =
            spawn_turn_and_await_pending(&state, &thread, Some("f.txt"), "add marker");
        let events = state.heart.lock().unwrap().read_stream(&thread).unwrap();
        let ev = events
            .iter()
            .find(|e| e.event_hash == pending_hash)
            .unwrap();
        assert_eq!(
            ev.family, "agent.file.changed",
            "the proposal itself still lands"
        );
        assert!(
            !std::fs::read_to_string(repo.join("f.txt"))
                .unwrap()
                .contains("thread marker"),
            "Auto mode must NOT write a Baker thread's proposal to disk"
        );
        assert!(
            events.iter().all(|e| e.family != "agent.edit.accepted"),
            "no auto-accept may be recorded on a Baker thread — the human decides"
        );
    }

    /// Auto re-resolves through the jail even if a proposal somehow carries an
    /// escaping path (defence-in-depth; upstream propose also jails).
    #[test]
    fn auto_mode_refuses_escaping_proposal_path() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with_write_mode(dir.path(), Arc::new(StubModel), WriteMode::Auto);
        let id_wtesc = open_stream(&state, &open_req(&repo, "wtEsc"))
            .unwrap()
            .context
            .stream_id;
        let outside = dir.path().join("outside_secret.txt");
        let _ = std::fs::remove_file(&outside);

        let proposal = append(
            &state,
            &id_wtesc,
            "agent:test",
            "agent.file.changed",
            serde_json::json!({
                "file_path": "../../../etc/x",
                "status": "pending",
                "rationale": "escape",
                "old_content": "",
                "new_content": "pwned",
            })
            .to_string(),
            vec![],
        )
        .unwrap();

        maybe_auto_accept(&state, &id_wtesc, &proposal);

        assert!(
            !outside.exists(),
            "escaping path must not create files outside the worktree"
        );
        let events = state.heart.lock().unwrap().read_stream(&id_wtesc).unwrap();
        assert!(
            events.iter().any(|e| e.family == "agent.edit.failed"),
            "jail refusal must append agent.edit.failed, not leave a silent pending"
        );
        assert!(
            !events.iter().any(|e| e.family == "agent.edit.accepted"),
            "must not accept an escaping proposal"
        );
    }

    /// The node-wide write mode no longer gates anything. With it on Auto and
    /// no per-agent choice recorded, the agent is Manual: its edit stays a
    /// pending proposal, nothing lands on disk, nothing is auto-accepted. This
    /// is the invariant that replaced WriteMode's Auto/Rules gate.
    #[test]
    fn node_write_mode_auto_no_longer_writes_without_a_per_agent_mode() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let model = Arc::new(ScriptedModel::new(&[
            r#"{"edit":{"file_path":"f.txt","new_content":"line one\nadd marker\n","rationale":"add marker"}}"#,
        ]));
        let state = state_with_write_mode(dir.path(), model, WriteMode::Auto);
        let resp = open_stream(&state, &open_req(&repo, "wtNoAgentMode")).unwrap();
        let wt = std::path::PathBuf::from(&resp.context.worktree_path);

        // Manual by default keeps it pending, so the turn parks — thread + stream read.
        let _hash = spawn_turn_and_await_pending(
            &state,
            &resp.context.stream_id,
            Some("f.txt"),
            "add marker",
        );

        assert_eq!(
            std::fs::read_to_string(wt.join("f.txt")).unwrap(),
            "line one\n",
            "a node-wide Auto must not write for an agent that never chose Auto"
        );
        let events = state
            .heart
            .lock()
            .unwrap()
            .read_stream(&resp.context.stream_id)
            .unwrap();
        assert!(!events.iter().any(|e| e.family == "agent.edit.accepted"));
    }

    #[tokio::test]
    async fn set_write_mode_persists_and_round_trips_over_http() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        assert_eq!(node_info_snapshot(&state).write_mode, WriteMode::Manual);

        let resp = set_write_mode(
            State(state.clone()),
            Json(WriteModeReq {
                write_mode: WriteMode::Auto,
                stream_id: None,
            }),
        )
        .await
        .unwrap();
        assert_eq!(
            resp.0.write_mode,
            WriteMode::Auto,
            "the response must reflect the new mode"
        );

        // In-memory state agrees immediately...
        assert_eq!(node_info_snapshot(&state).write_mode, WriteMode::Auto);
        // ...and it survives a fresh load from disk (same node.json `stream_fallback` lives in).
        let reloaded = NodeConfig::load_or_create(&state.base);
        assert_eq!(reloaded.write_mode, WriteMode::Auto);

        // No stream_id → still audit on node:policy (not silent).
        let events = state
            .heart
            .lock()
            .unwrap()
            .read_stream(NODE_POLICY_STREAM)
            .unwrap();
        assert!(
            events.iter().any(|e| e.family == "policy.write_mode.set"),
            "StatusBar-style change without stream_id must leave a Heart record"
        );
    }

    #[tokio::test]
    async fn set_write_mode_with_stream_id_appends_audit_event() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id_wtaudit = open_stream(&state, &open_req(&repo, "wtAudit"))
            .unwrap()
            .context
            .stream_id;

        let _ = set_write_mode(
            State(state.clone()),
            Json(WriteModeReq {
                write_mode: WriteMode::Rules,
                stream_id: Some(id_wtaudit.clone()),
            }),
        )
        .await
        .unwrap();

        let events = state
            .heart
            .lock()
            .unwrap()
            .read_stream(&id_wtaudit)
            .unwrap();
        let set_ev = events
            .iter()
            .find(|e| e.family == "policy.write_mode.set")
            .expect("a cited stream_id must get an audit event");
        assert_eq!(set_ev.actor, "human");
        let p: serde_json::Value = serde_json::from_str(&set_ev.payload).unwrap();
        assert_eq!(p["write_mode"], "rules");
    }

    #[test]
    fn accept_writes_file_and_records_acceptance() {
        let (_dir, state, wt, hash, stream_id) = setup_with_pending();
        let ev = decide(
            &state,
            &AgentDecisionReq {
                stream_id,
                event_hash: hash.clone(),
            },
            true,
        )
        .unwrap();
        assert_eq!(ev.family, "agent.edit.accepted");
        assert_eq!(ev.causality, vec![hash]);
        assert!(std::fs::read_to_string(wt.join("f.txt"))
            .unwrap()
            .contains("bakery stub agent: add marker"));
    }

    #[test]
    fn reject_records_rejection_without_writing() {
        let (_dir, state, wt, hash, stream_id) = setup_with_pending();
        let before = std::fs::read_to_string(wt.join("f.txt")).unwrap();
        let ev = decide(
            &state,
            &AgentDecisionReq {
                stream_id,
                event_hash: hash,
            },
            false,
        )
        .unwrap();
        assert_eq!(ev.family, "agent.edit.rejected");
        assert_eq!(std::fs::read_to_string(wt.join("f.txt")).unwrap(), before);
    }

    #[test]
    fn accept_refuses_a_proposal_pointing_outside_the_worktree() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id_wta = open_stream(&state, &open_req(&repo, "wtA"))
            .unwrap()
            .context
            .stream_id;

        // Forge a pending proposal that escapes the jail (a hostile model reply).
        let payload = serde_json::json!({
            "file_path": "../../escaped.txt",
            "status": "pending",
            "rationale": "hostile",
            "old_content": "",
            "new_content": "owned",
        })
        .to_string();
        let ev = append(
            &state,
            &id_wta,
            "agent:test",
            "agent.file.changed",
            payload,
            vec![],
        )
        .unwrap();

        let err = decide(
            &state,
            &AgentDecisionReq {
                stream_id: id_wta.clone(),
                event_hash: ev.event_hash.clone(),
            },
            true,
        )
        .unwrap_err();
        assert!(err.contains("escapes"), "got: {err}");
        assert!(
            !dir.path().join("escaped.txt").exists(),
            "nothing may be written outside"
        );

        // The decision was confirmed pending before the write was attempted,
        // so the failure must be recorded, linked back to the proposal.
        let stream = state.heart.lock().unwrap().read_stream(&id_wta).unwrap();
        let failed = stream
            .iter()
            .find(|e| e.family == "agent.edit.failed")
            .expect("a failed accept must append agent.edit.failed");
        assert_eq!(failed.actor, "node");
        assert_eq!(failed.causality, vec![ev.event_hash.clone()]);
        let p: serde_json::Value = serde_json::from_str(&failed.payload).unwrap();
        assert_eq!(p["decided_event"], ev.event_hash);
        assert_eq!(p["decision"], "accept");
        assert_eq!(p["file_path"], "../../escaped.txt");
        assert!(p["reason"].as_str().unwrap().contains("escapes"));
    }

    #[test]
    fn decide_pre_checks_do_not_append_edit_failed() {
        // Malformed requests (bad hash, wrong family, already decided) carry
        // no trace value — only a decision that was confirmed pending and then
        // failed gets an `agent.edit.failed` event.
        let (_dir, state, _wt, hash, stream_id) = setup_with_pending();
        let err = decide(
            &state,
            &AgentDecisionReq {
                stream_id: stream_id.clone(),
                event_hash: "not-a-real-hash".into(),
            },
            true,
        )
        .unwrap_err();
        assert!(err.contains("no event"), "got: {err}");

        decide(
            &state,
            &AgentDecisionReq {
                stream_id: stream_id.clone(),
                event_hash: hash.clone(),
            },
            true,
        )
        .unwrap();
        let err = decide(
            &state,
            &AgentDecisionReq {
                stream_id: stream_id.clone(),
                event_hash: hash,
            },
            true,
        )
        .unwrap_err();
        assert!(
            err.contains("already has a terminal decision"),
            "got: {err}"
        );

        let stream = state.heart.lock().unwrap().read_stream(&stream_id).unwrap();
        assert!(
            !stream.iter().any(|e| e.family == "agent.edit.failed"),
            "malformed-request pre-checks must not append agent.edit.failed"
        );
    }

    #[test]
    fn direct_turn_refuses_a_file_path_outside_the_worktree() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id_wta = open_stream(&state, &open_req(&repo, "wtA"))
            .unwrap()
            .context
            .stream_id;

        let err = agent_turn_inner(
            &state,
            &AgentTurnReq {
                model_id: None,
                effort: None,
                actor: None,
                stream_id: id_wta.clone(),
                file_path: Some("../../../etc/hosts".into()),
                instruction: "escape".into(),
                seat: None,
            },
        )
        .unwrap_err()
        .1;
        assert!(err.contains("escapes"), "got: {err}");
    }

    #[test]
    fn turn_failing_inside_the_jail_appends_turn_failed_with_phase_jail() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id_jailfail = open_stream(&state, &open_req(&repo, "jailfail"))
            .unwrap()
            .context
            .stream_id;

        let err = agent_turn_inner(
            &state,
            &AgentTurnReq {
                model_id: None,
                effort: None,
                actor: None,
                stream_id: id_jailfail.clone(),
                file_path: Some("../../../etc/hosts".into()),
                instruction: "escape".into(),
                seat: None,
            },
        )
        .unwrap_err()
        .1;
        // The original error is still what the caller sees.
        assert!(err.contains("escapes"), "got: {err}");

        let stream = state
            .heart
            .lock()
            .unwrap()
            .read_stream(&id_jailfail)
            .unwrap();
        let start_at = stream
            .iter()
            .position(|e| e.family == "agent.turn.started")
            .unwrap();
        let failed_at = stream
            .iter()
            .position(|e| e.family == "agent.turn.failed")
            .expect("a jail failure must append agent.turn.failed");
        assert!(start_at < failed_at, "started must precede failed");
        let failed = &stream[failed_at];
        assert_eq!(failed.actor, "node");
        let p: serde_json::Value = serde_json::from_str(&failed.payload).unwrap();
        assert_eq!(p["phase"], "jail");
        assert!(p["reason"].as_str().unwrap().contains("escapes"));
    }

    /// A model that proposes an edit outside the worktree fails in the jail,
    /// not "in the tool loop" — the phase must say so, because that phase is
    /// what tells a human whether their agent misbehaved or their model did.
    #[test]
    fn model_proposing_an_escaping_edit_is_phase_jail() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let model = Arc::new(ScriptedModel::new(&[
            r#"{"edit":{"file_path":"../../escape.txt","new_content":"x\n","rationale":"nope"}}"#,
        ]));
        let state = state_with(dir.path(), model);
        let id_escedit = open_stream(&state, &open_req(&repo, "escedit"))
            .unwrap()
            .context
            .stream_id;

        let err = agent_turn_inner(
            &state,
            &AgentTurnReq {
                model_id: None,
                effort: None,
                actor: None,
                stream_id: id_escedit.clone(),
                file_path: None,
                instruction: "write outside the tree".into(),
                seat: None,
            },
        )
        .unwrap_err()
        .1;
        assert!(err.contains("escapes"), "got: {err}");

        let stream = state
            .heart
            .lock()
            .unwrap()
            .read_stream(&id_escedit)
            .unwrap();
        let failed = stream
            .iter()
            .find(|e| e.family == "agent.turn.failed")
            .expect("an escaping proposal must append agent.turn.failed");
        let p: serde_json::Value = serde_json::from_str(&failed.payload).unwrap();
        assert_eq!(
            p["phase"], "jail",
            "a refused path is a jail failure wherever it happens"
        );
        assert!(
            !dir.path().join("escape.txt").exists(),
            "nothing may be written outside"
        );
    }

    #[test]
    fn turn_failing_at_the_model_call_appends_turn_failed_with_phase_model() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(FailingChatModel));
        let id_modelfail = open_stream(&state, &open_req(&repo, "modelfail"))
            .unwrap()
            .context
            .stream_id;

        let err = agent_turn_inner(
            &state,
            &AgentTurnReq {
                model_id: None,
                effort: None,
                actor: None,
                stream_id: id_modelfail.clone(),
                file_path: None,
                instruction: "explore".into(),
                seat: None,
            },
        )
        .unwrap_err()
        .1;
        assert!(err.contains("simulated model failure"), "got: {err}");

        let stream = state
            .heart
            .lock()
            .unwrap()
            .read_stream(&id_modelfail)
            .unwrap();
        let failed = stream
            .iter()
            .find(|e| e.family == "agent.turn.failed")
            .expect("a model-call failure must append agent.turn.failed");
        let p: serde_json::Value = serde_json::from_str(&failed.payload).unwrap();
        assert_eq!(p["phase"], "model");
        assert!(p["reason"]
            .as_str()
            .unwrap()
            .contains("simulated model failure"));
    }

    // ─── B: tool loop ────────────────────────────────────────────────────────

    #[test]
    fn tool_loop_explores_then_proposes_one_edit() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let model = Arc::new(ScriptedModel::new(&[
            r#"{"tool":"list_files","args":{"path":"."}}"#,
            r#"{"tool":"read_file","args":{"path":"f.txt"}}"#,
            r#"{"edit":{"file_path":"f.txt","new_content":"line one\nline two\n","rationale":"append"}}"#,
        ]));
        let state = state_with(dir.path(), model);
        let resp = open_stream(&state, &open_req(&repo, "tl")).unwrap();

        // Manual mode: the proposing turn parks awaiting the decision —
        // issue on a thread, assert against the recorded proposal.
        let hash = spawn_turn_and_await_pending(
            &state,
            &resp.context.stream_id,
            None,
            "explain the layout then add a line",
        );
        let ev = {
            let events = state
                .heart
                .lock()
                .unwrap()
                .read_stream(&resp.context.stream_id)
                .unwrap();
            events
                .iter()
                .find(|e| e.event_hash == hash)
                .unwrap()
                .clone()
        };

        assert_eq!(ev.family, "agent.file.changed");
        let p: serde_json::Value = serde_json::from_str(&ev.payload).unwrap();
        assert_eq!(p["status"], "pending");
        assert_eq!(
            p["old_content"], "line one\n",
            "the loop read the real file"
        );

        // Tool calls are on the stream, summarised.
        let stream = state
            .heart
            .lock()
            .unwrap()
            .read_stream(&resp.context.stream_id)
            .unwrap();
        let tools: Vec<_> = stream
            .iter()
            .filter(|e| e.family == "agent.tool.called")
            .collect();
        assert_eq!(tools.len(), 2);
        let first: serde_json::Value = serde_json::from_str(&tools[0].payload).unwrap();
        assert_eq!(first["tool"], "list_files");
        assert_eq!(first["ok"], true);
        assert!(first["summary"].as_str().unwrap().contains("f.txt"));

        // Manual gate intact: disk untouched until Accept.
        let target = std::path::Path::new(&resp.context.worktree_path).join("f.txt");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "line one\n");
    }

    #[test]
    fn tool_loop_auto_mode_writes_the_edit_immediately() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let model = Arc::new(ScriptedModel::new(&[
            r#"{"edit":{"file_path":"f.txt","new_content":"line one\nline two\n","rationale":"append"}}"#,
            "appended the line",
        ]));
        let state = state_with_write_mode(dir.path(), model, WriteMode::Manual);
        let resp = open_stream(&state, &open_req(&repo, "tlauto")).unwrap();
        agent_control::set_operating_mode(
            &state,
            &resp.context.stream_id,
            crate::node::Seat::Orchestrator,
            agent_control::OperatingMode::Auto,
        )
        .unwrap();

        let ev = agent_turn_inner(
            &state,
            &AgentTurnReq {
                model_id: None,
                effort: None,
                actor: None,
                stream_id: resp.context.stream_id.clone(),
                file_path: None,
                instruction: "add a line".into(),
                seat: None,
            },
        )
        .unwrap();
        // Continue-after-decision: the Auto write no longer ends the turn —
        // the model learns the outcome and finishes normally.
        assert_eq!(ev.family, "agent.turn.done");

        let target = std::path::Path::new(&resp.context.worktree_path).join("f.txt");
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "line one\nline two\n",
            "Auto mode must write the tool-loop's proposed edit immediately"
        );

        let stream = state
            .heart
            .lock()
            .unwrap()
            .read_stream(&resp.context.stream_id)
            .unwrap();
        let proposal = stream
            .iter()
            .find(|e| e.family == "agent.file.changed")
            .expect("proposal recorded");
        let accepted = stream
            .iter()
            .find(|e| e.family == "agent.edit.accepted")
            .expect("Auto mode must append agent.edit.accepted for the tool-loop edit path too");
        assert_eq!(accepted.actor, "policy/auto");
        assert_eq!(accepted.causality, vec![proposal.event_hash.clone()]);
    }

    #[test]
    fn tool_loop_can_answer_without_editing() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id_ask = open_stream(&state, &open_req(&repo, "ask"))
            .unwrap()
            .context
            .stream_id;

        let ev = agent_turn_inner(
            &state,
            &AgentTurnReq {
                model_id: None,
                effort: None,
                actor: None,
                stream_id: id_ask.clone(),
                file_path: None,
                instruction: "explain this repo's layout".into(),
                seat: None,
            },
        )
        .unwrap();

        assert_eq!(ev.family, "agent.turn.done");
        let stream = state.heart.lock().unwrap().read_stream(&id_ask).unwrap();
        assert!(stream.iter().any(|e| e.family == "agent.tool.called"));
        assert!(!stream.iter().any(|e| e.family == "agent.file.changed"));
    }

    /// The workpane rebuilds its transcript from the stream. If the human's own
    /// message is not on the stream, a reload shows the agent talking to nobody.
    #[test]
    fn turn_started_records_the_human_instruction() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id_started = open_stream(&state, &open_req(&repo, "started"))
            .unwrap()
            .context
            .stream_id;

        let instruction = "explain this repo's layout";
        agent_turn_inner(
            &state,
            &AgentTurnReq {
                model_id: None,
                effort: None,
                actor: None,
                stream_id: id_started.clone(),
                file_path: None,
                instruction: instruction.into(),
                seat: None,
            },
        )
        .unwrap();

        let stream = state
            .heart
            .lock()
            .unwrap()
            .read_stream(&id_started)
            .unwrap();
        let started = stream
            .iter()
            .find(|e| e.family == "agent.turn.started")
            .expect("the turn recorded no human message");
        assert_eq!(
            started.actor, "human",
            "the human authored it, not the model"
        );
        let p: serde_json::Value = serde_json::from_str(&started.payload).unwrap();
        assert_eq!(
            p["instruction"], instruction,
            "stored in full, not summarised"
        );
        assert!(p["file_path"].is_null(), "tool-loop turn targets no file");
        assert_eq!(p["mode"], "manual");
        assert_eq!(
            p["seat"], "orchestrator",
            "06-SEATS audit item 2: agent.turn.started must record the resolved seat"
        );
    }

    /// 06-SEATS audit item 2, explicit-seat case: the recorded seat is the
    /// one the turn actually resolved against, not always the default.
    #[test]
    fn turn_started_records_an_explicitly_requested_seat() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id_startedseat = open_stream(&state, &open_req(&repo, "startedseat"))
            .unwrap()
            .context
            .stream_id;

        agent_turn_inner(
            &state,
            &AgentTurnReq {
                model_id: None,
                effort: None,
                actor: None,
                stream_id: id_startedseat.clone(),
                file_path: None,
                instruction: "hi".into(),
                seat: Some(crate::node::Seat::Worker),
            },
        )
        .unwrap();

        let stream = state
            .heart
            .lock()
            .unwrap()
            .read_stream(&id_startedseat)
            .unwrap();
        let started = stream
            .iter()
            .find(|e| e.family == "agent.turn.started")
            .unwrap();
        let p: serde_json::Value = serde_json::from_str(&started.payload).unwrap();
        assert_eq!(p["seat"], "worker");
    }

    /// 06b-PIPE.md: "Same stream + seat assignment change = next turn uses
    /// the new plug against the same Heart. That is the point." — reassigns
    /// a live stream's orchestrator seat from a first-party model to the
    /// piped Grok plug mid-conversation and checks the very next turn picks
    /// it up, still on the identical stream. Self-consistent with this
    /// machine's real Grok CLI state (installed/authenticated or not) the
    /// same way the credential-dependent tests in `bakery-agent` are —
    /// either branch proves the reassignment actually took effect.
    #[test]
    fn reassigning_a_seat_to_the_piped_plug_mid_stream_uses_it_on_the_next_turn_same_heart() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id_reassign = open_stream(&state, &open_req(&repo, "reassign"))
            .unwrap()
            .context
            .stream_id;

        let ev1 = agent_turn_inner(
            &state,
            &AgentTurnReq {
                model_id: None,
                effort: None,
                actor: None,
                stream_id: id_reassign.clone(),
                file_path: None,
                instruction: "hi".into(),
                seat: None,
            },
        )
        .unwrap();
        // 07d-A1 §1: actor is the seat alone — the model never leaks into it.
        assert_eq!(ev1.actor, "agent:orchestrator");
        let ev1_p: serde_json::Value = serde_json::from_str(&ev1.payload).unwrap();
        assert_eq!(
            ev1_p["model_id"], "stub",
            "model_id lives in the payload, not the actor"
        );

        crate::state::assign_seat_model(
            &state,
            &id_reassign,
            crate::node::Seat::Orchestrator,
            crate::pipe::ACP_MODEL_ID,
        )
        .unwrap();

        match agent_turn_inner(
            &state,
            &AgentTurnReq {
                model_id: None,
                effort: None,
                actor: None,
                stream_id: id_reassign.clone(),
                file_path: None,
                instruction: "say hi back".into(),
                seat: None,
            },
        ) {
            Ok(ev2) => {
                // Reassigning Grok→ACP must not change the being on the wire.
                assert_eq!(
                    ev2.actor, ev1.actor,
                    "actor must survive a model reassignment unchanged"
                );
                let ev2_p: serde_json::Value = serde_json::from_str(&ev2.payload).unwrap();
                assert_eq!(
                    ev2_p["model_id"], "acp",
                    "the terminal success event names the model actually used"
                );
                let stream = state
                    .heart
                    .lock()
                    .unwrap()
                    .read_stream(&id_reassign)
                    .unwrap();
                assert!(
                    stream.iter().any(|e| e.event_hash == ev1.event_hash),
                    "both turns must land on the same Heart stream"
                );
                assert!(stream.iter().any(|e| e.event_hash == ev2.event_hash));
            }
            Err((_status, e)) => {
                // Every genuine pipe.rs failure shape starts "Setup needed:"
                // or "grok ACP ..." (pipe.rs's own timeout/launch/handshake
                // errors) — `resolve_seat_model`'s generic fallback for an
                // unrecognized model id also happens to contain "grok" (its
                // message lists it as a suggestion), so a substring match on
                // "grok" alone can't tell "the pipe path was never taken"
                // from "the pipe path ran and failed honestly" (06b review
                // finding #9).
                assert!(e.contains("Setup needed") || e.contains("grok ACP"), "{e}");
                assert!(
                    !e.contains("no client for model id"),
                    "must have taken the pipe path, not the generic fallback: {e}"
                );

                // A resolution failure still names the seat's assigned id —
                // the model never resolved, but the assignment was readable.
                let stream = state
                    .heart
                    .lock()
                    .unwrap()
                    .read_stream(&id_reassign)
                    .unwrap();
                let failed = stream
                    .iter()
                    .rev()
                    .find(|e| e.family == "agent.turn.failed")
                    .expect("a resolution failure must append agent.turn.failed");
                let failed_p: serde_json::Value = serde_json::from_str(&failed.payload).unwrap();
                assert_eq!(
                    failed_p["model_id"], "acp",
                    "a resolution failure records the assigned id when known"
                );
            }
        }
    }

    /// 07d-A3B "Runtime law": a turn on a named ACP plug's seat must land as
    /// `agent.turn.done` directly, with the fake agent's raw text as the
    /// summary — never through `agent_loop::parse_action`'s 8-step JSON tool
    /// loop (no `agent.tool.called`, no `agent.turn.exhausted`, and the
    /// summary is not itself a `{"done":{"summary":...}}` envelope).
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn agent_turn_on_a_named_acp_plug_bypasses_the_json_tool_loop() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id_acpturn = open_stream(&state, &open_req(&repo, "acpturn"))
            .unwrap()
            .context
            .stream_id;

        state.node.lock().unwrap().acp_plugs = vec![crate::node::AcpPlugConfig {
            id: "acp:codex".into(),
            label: Some("Codex CLI".into()),
            argv: vec![crate::testutil::fake_acp_agent_binary()
                .to_string_lossy()
                .into_owned()],
            config: None,
            env: Default::default(),
            mcp_transport: crate::node::default_mcp_transport(),
        }];
        crate::state::assign_seat_model(
            &state,
            &id_acpturn,
            crate::node::Seat::Orchestrator,
            "acp:codex",
        )
        .unwrap();

        let ev = agent_turn_inner(
            &state,
            &AgentTurnReq {
                model_id: None,
                effort: None,
                actor: None,
                stream_id: id_acpturn.clone(),
                file_path: None,
                instruction: "hi".into(),
                seat: None,
            },
        )
        .unwrap();

        assert_eq!(ev.family, "agent.turn.done");
        assert_eq!(ev.actor, "agent:orchestrator");
        let p: serde_json::Value = serde_json::from_str(&ev.payload).unwrap();
        assert_eq!(
            p["summary"], "hello from fake agent",
            "raw ACP text, not a JSON action envelope"
        );
        assert_eq!(p["model_id"], "acp:codex");

        let stream = state
            .heart
            .lock()
            .unwrap()
            .read_stream(&id_acpturn)
            .unwrap();
        assert!(
            !stream
                .iter()
                .any(|e| e.family == "agent.tool.called" || e.family == "agent.turn.exhausted"),
            "an ACP-native turn must never run the JSON tool loop"
        );
    }

    /// A command reaches the runtime exactly as typed even where every turn
    /// opens with the toolkit's instructions: they open the next message.
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn a_command_goes_as_typed_past_the_turns_opening() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = open_stream(&state, &open_req(&repo, "acpcommand")).unwrap().context.stream_id;
        let binary = crate::testutil::fake_acp_agent_binary().to_string_lossy().into_owned();
        state.node.lock().unwrap().acp_plugs = vec![crate::node::AcpPlugConfig {
            id: "acp:echo".into(),
            label: None,
            argv: vec![binary, "--env".into(), "BAKERY_FAKE_ACP_ECHO_PROMPT=1".into()],
            config: None,
            env: Default::default(),
            mcp_transport: crate::node::default_mcp_transport(),
        }];
        crate::state::assign_seat_model(&state, &stream_id, crate::node::Seat::Orchestrator, "acp:echo").unwrap();
        let rules = serde_json::json!({"kind": "instructions", "name": "house-rules", "text": "Always test first."});
        crate::toolkit::set(&state, &serde_json::from_value(rules).unwrap()).unwrap();
        let turn = |instruction: &str| {
            let req = AgentTurnReq {
                model_id: None,
                effort: None,
                actor: None,
                stream_id: stream_id.clone(),
                file_path: None,
                instruction: instruction.into(),
                seat: None,
            };
            let done: serde_json::Value = serde_json::from_str(&agent_turn_inner(&state, &req).unwrap().payload).unwrap();
            done["summary"].as_str().unwrap().to_string()
        };
        assert_eq!(turn("/compact keep the plan"), "/compact keep the plan");
        let next = turn("hello");
        assert!(next.contains("Always test first.") && next.ends_with("hello"), "{next}");
    }

    /// Both entry paths, and before the model: a turn whose first model call
    /// fails must still leave the human's message on the stream.
    #[test]
    fn turn_started_precedes_the_agent_on_both_paths() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        // Both turns share one scripted model: turn 1 proposes an edit to
        // the selected file (which now rides the SAME loop — Checkpoint 2
        // item 1); turn 2 makes a tool call, then answers.
        let model = Arc::new(ScriptedModel::new(&[
            r#"{"edit":{"file_path":"f.txt","new_content":"line one\nline two\n","rationale":"append"}}"#,
            r#"{"tool":"read_file","args":{"path":"f.txt"}}"#,
            "the layout is one file",
        ]));
        let state = state_with(dir.path(), model);

        // Selected-file turn: file_path is recorded and the proposal comes
        // after. Manual mode parks the turn at the proposal — thread it.
        let id_direct = open_stream(&state, &open_req(&repo, "direct"))
            .unwrap()
            .context
            .stream_id;
        let _hash =
            spawn_turn_and_await_pending(&state, &id_direct, Some("f.txt"), "append a line");
        let stream = state.heart.lock().unwrap().read_stream(&id_direct).unwrap();
        let start_at = stream
            .iter()
            .position(|e| e.family == "agent.turn.started")
            .unwrap();
        let edit_at = stream
            .iter()
            .position(|e| e.family == "agent.file.changed")
            .unwrap();
        assert!(
            start_at < edit_at,
            "human message must precede the proposal"
        );
        let p: serde_json::Value = serde_json::from_str(&stream[start_at].payload).unwrap();
        assert_eq!(p["file_path"], "f.txt");

        // Tool path: the human message precedes the first tool call.
        let id_toolpath = open_stream(&state, &open_req(&repo, "toolpath"))
            .unwrap()
            .context
            .stream_id;
        agent_turn_inner(
            &state,
            &AgentTurnReq {
                model_id: None,
                effort: None,
                actor: None,
                stream_id: id_toolpath.clone(),
                file_path: None,
                instruction: "explain this repo's layout".into(),
                seat: None,
            },
        )
        .unwrap();
        let stream = state
            .heart
            .lock()
            .unwrap()
            .read_stream(&id_toolpath)
            .unwrap();
        let start_at = stream
            .iter()
            .position(|e| e.family == "agent.turn.started")
            .unwrap();
        let tool_at = stream
            .iter()
            .position(|e| e.family == "agent.tool.called")
            .unwrap();
        assert!(
            start_at < tool_at,
            "human message must precede the first tool call"
        );
    }

    /// 04b-FENCE §4: a discarded stream must not still take turns, and an
    /// accept/reject decision on it must refuse rather than try to write
    /// through a jail whose worktree may already be gone.
    #[tokio::test]
    async fn agent_turn_and_decide_refuse_a_discarded_stream() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id_gone = open_stream(&state, &open_req(&repo, "gone"))
            .unwrap()
            .context
            .stream_id;

        let _ = crate::routes::lifecycle::discard_stream(
            State(state.clone()),
            axum::Json(crate::routes::lifecycle::DiscardReq {
                stream_id: id_gone.clone(),
                confirm: false,
            }),
        )
        .await
        .unwrap();

        let err = agent_turn_inner(
            &state,
            &AgentTurnReq {
                model_id: None,
                effort: None,
                actor: None,
                stream_id: id_gone.clone(),
                file_path: None,
                instruction: "hi".into(),
                seat: None,
            },
        )
        .unwrap_err()
        .1;
        assert!(err.contains("discarded"), "{err}");

        let err = decide(
            &state,
            &AgentDecisionReq {
                stream_id: id_gone.clone(),
                event_hash: "whatever".into(),
            },
            true,
        )
        .unwrap_err();
        assert!(err.contains("discarded"), "{err}");
    }

    /// `append` mints a stream it has never seen. A typo'd stream_id must fail
    /// without leaving a junk stream behind.
    #[test]
    fn turn_on_an_unknown_stream_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));

        let err = agent_turn_inner(
            &state,
            &AgentTurnReq {
                model_id: None,
                effort: None,
                actor: None,
                stream_id: "wt:typo".into(),
                file_path: None,
                instruction: "hello".into(),
                seat: None,
            },
        )
        .unwrap_err()
        .1;
        assert!(err.contains("project.onboarded"), "unexpected error: {err}");

        let stream = state.heart.lock().unwrap().read_stream("wt:typo").unwrap();
        assert!(stream.is_empty(), "a failed turn minted a stream");
    }

    #[test]
    fn tool_loop_is_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(LoopingModel));
        state.node.lock().unwrap().agent_max_steps = std::num::NonZeroUsize::new(8);
        let id_loop = open_stream(&state, &open_req(&repo, "loop"))
            .unwrap()
            .context
            .stream_id;

        let ev = agent_turn_inner(
            &state,
            &AgentTurnReq {
                model_id: None,
                effort: None,
                actor: None,
                stream_id: id_loop.clone(),
                file_path: None,
                instruction: "spin forever".into(),
                seat: None,
            },
        )
        .unwrap();

        assert_eq!(ev.family, "agent.turn.exhausted");
        let stream = state.heart.lock().unwrap().read_stream(&id_loop).unwrap();
        let calls = stream
            .iter()
            .filter(|e| e.family == "agent.tool.called")
            .count();
        assert_eq!(calls, 8, "the explicit operator budget is what stops it");
    }

    #[test]
    fn exhausted_payload_has_tool_log() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(LoopingModel));
        state.node.lock().unwrap().agent_max_steps = std::num::NonZeroUsize::new(8);
        let id_exh = open_stream(&state, &open_req(&repo, "exh"))
            .unwrap()
            .context
            .stream_id;

        let ev = agent_turn_inner(
            &state,
            &AgentTurnReq {
                model_id: None,
                effort: None,
                actor: None,
                stream_id: id_exh.clone(),
                file_path: None,
                instruction: "loop forever".into(),
                seat: None,
            },
        )
        .unwrap();

        assert_eq!(ev.family, "agent.turn.exhausted");
        let p: serde_json::Value = serde_json::from_str(&ev.payload).unwrap();

        let tools = p["tools"].as_array().expect("tools must be a JSON array");
        assert_eq!(tools.len(), 8, "one entry per step taken");
        for (i, t) in tools.iter().enumerate() {
            let step = t["step"].as_u64().expect("step must be a number") as usize;
            assert_eq!(step, i + 1, "steps must be monotonic 1..N");
            assert!(t["tool"].is_string(), "tool must be a string: {:?}", t);
            assert!(t["ok"].is_boolean(), "ok must be boolean: {:?}", t);
            let summary = t["args_summary"]
                .as_str()
                .expect("args_summary must be a string");
            assert!(
                summary.len() <= 130,
                "args_summary must be bounded: {:?}",
                summary
            );
        }
        assert!(
            p["paths_hint"].is_array(),
            "paths_hint must be a JSON array"
        );
        assert!(p["prior_accept_path"].is_null(), "no prior accept → null");
    }

    #[test]
    fn exhausted_payload_prior_accept_path_from_seeded_accept() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(LoopingModel));
        state.node.lock().unwrap().agent_max_steps = std::num::NonZeroUsize::new(8);
        let id_prior = open_stream(&state, &open_req(&repo, "prior"))
            .unwrap()
            .context
            .stream_id;
        let id = &id_prior;
        let agent_id = crate::node::agent_id(id, crate::node::Seat::Orchestrator);

        // Seed a prior proposal this agent owns, then its accepted decision —
        // the same shape `propose_edit_event`/`decide_confirmed` produce for
        // real (07d-A1 §2: a decision's ownership follows causality back to
        // the tagged proposal it decides, not a field of its own).
        let proposal = append(
            &state,
            id,
            "agent:orchestrator",
            "agent.file.changed",
            serde_json::json!({
                "file_path": "seeded.txt",
                "status": "pending",
                "rationale": "seed",
                "old_content": "",
                "new_content": "x",
                "agent_id": agent_id,
                "model_id": "stub",
            })
            .to_string(),
            vec![],
        )
        .unwrap();
        append(
            &state,
            id,
            "human",
            "agent.edit.accepted",
            serde_json::json!({
                "decided_event": proposal.event_hash,
                "file_path": "seeded.txt",
                "status": "accepted",
            })
            .to_string(),
            vec![proposal.event_hash.clone()],
        )
        .unwrap();

        let ev = agent_turn_inner(
            &state,
            &AgentTurnReq {
                model_id: None,
                effort: None,
                actor: None,
                stream_id: id.into(),
                file_path: None,
                instruction: "loop forever".into(),
                seat: None,
            },
        )
        .unwrap();

        assert_eq!(ev.family, "agent.turn.exhausted");
        let p: serde_json::Value = serde_json::from_str(&ev.payload).unwrap();

        assert_eq!(
            p["prior_accept_path"].as_str(),
            Some("seeded.txt"),
            "prior_accept_path must reflect the last agent.edit.accepted in the hot window"
        );
        let paths_hint = p["paths_hint"]
            .as_array()
            .expect("paths_hint must be an array");
        assert!(
            paths_hint.iter().any(|v| v.as_str() == Some("seeded.txt")),
            "seeded.txt must appear in paths_hint: {:?}",
            paths_hint
        );
    }

    #[test]
    fn tool_loop_survives_a_protocol_violation() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let model = Arc::new(ScriptedModel::new(&[
            "I'll take a look at that for you!",
            r#"{"done":{"summary":"recovered"}}"#,
        ]));
        let state = state_with(dir.path(), model);
        let id_bad = open_stream(&state, &open_req(&repo, "bad"))
            .unwrap()
            .context
            .stream_id;

        let ev = agent_turn_inner(
            &state,
            &AgentTurnReq {
                model_id: None,
                effort: None,
                actor: None,
                stream_id: id_bad.clone(),
                file_path: None,
                instruction: "say something unparseable first".into(),
                seat: None,
            },
        )
        .unwrap();
        assert_eq!(ev.family, "agent.turn.done");
    }

    #[test]
    fn tool_loop_reports_a_jail_escape_to_the_model_without_failing_the_turn() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let model = Arc::new(ScriptedModel::new(&[
            r#"{"tool":"read_file","args":{"path":"../../../etc/passwd"}}"#,
            r#"{"done":{"summary":"blocked, stopping"}}"#,
        ]));
        let state = state_with(dir.path(), model);
        let id_jail = open_stream(&state, &open_req(&repo, "jail"))
            .unwrap()
            .context
            .stream_id;

        let ev = agent_turn_inner(
            &state,
            &AgentTurnReq {
                model_id: None,
                effort: None,
                actor: None,
                stream_id: id_jail.clone(),
                file_path: None,
                instruction: "read the password file".into(),
                seat: None,
            },
        )
        .unwrap();

        assert_eq!(ev.family, "agent.turn.done");
        let stream = state.heart.lock().unwrap().read_stream(&id_jail).unwrap();
        let call = stream
            .iter()
            .find(|e| e.family == "agent.tool.called")
            .unwrap();
        let p: serde_json::Value = serde_json::from_str(&call.payload).unwrap();
        assert_eq!(p["ok"], false, "the escape must be refused");
        assert!(p["summary"].as_str().unwrap().contains("escapes"));
    }

    #[test]
    fn tool_loop_refuses_a_model_without_chat_support() {
        struct EditOnly;
        impl Model for EditOnly {
            fn name(&self) -> &str {
                "edit-only"
            }
            fn propose_edit(&self, _r: &bakery_agent::EditRequest) -> Result<ProposedEdit, String> {
                Err("no".into())
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(EditOnly));
        let id_nochat = open_stream(&state, &open_req(&repo, "nochat"))
            .unwrap()
            .context
            .stream_id;

        let err = agent_turn_inner(
            &state,
            &AgentTurnReq {
                model_id: None,
                effort: None,
                actor: None,
                stream_id: id_nochat.clone(),
                file_path: None,
                instruction: "explore".into(),
                seat: None,
            },
        )
        .unwrap_err()
        .1;
        assert!(err.contains("cannot run the tool loop"), "got: {err}");
    }

    // ─── M1c.2: agent.turn.failed analytics parity ───────────────────────────

    /// ScriptedModel serves N tool replies then exhausts on the (N+1)th chat
    /// call. The resulting agent.turn.failed must carry tools[].len() == N with
    /// monotonically increasing steps, matching agent.turn.exhausted's shape.
    #[test]
    fn failed_payload_has_tool_log_after_mid_loop_error() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        // Three tool replies, then script exhausts → Err("script exhausted") on 4th chat.
        let model = Arc::new(ScriptedModel::new(&[
            r#"{"tool":"list_files","args":{"path":"."}}"#,
            r#"{"tool":"list_files","args":{"path":"."}}"#,
            r#"{"tool":"list_files","args":{"path":"."}}"#,
        ]));
        let state = state_with(dir.path(), model);
        let id_midloop = open_stream(&state, &open_req(&repo, "midloop"))
            .unwrap()
            .context
            .stream_id;

        let result = agent_turn_inner(
            &state,
            &AgentTurnReq {
                model_id: None,
                effort: None,
                actor: None,
                stream_id: id_midloop.clone(),
                file_path: None,
                instruction: "do some work".into(),
                seat: None,
            },
        );

        // The turn must fail (not succeed or exhaust).
        assert!(
            result.is_err(),
            "expected Err from script exhaustion, got Ok"
        );

        // Find the agent.turn.failed event on the stream.
        let events = state
            .heart
            .lock()
            .unwrap()
            .read_stream(&id_midloop)
            .unwrap();
        let failed = events
            .iter()
            .find(|e| e.family == "agent.turn.failed")
            .expect("agent.turn.failed must be appended");

        let p: serde_json::Value = serde_json::from_str(&failed.payload).unwrap();

        assert_eq!(p["phase"].as_str(), Some("model"), "phase must be model");
        assert!(
            p["reason"]
                .as_str()
                .unwrap_or("")
                .contains("script exhausted"),
            "reason must name the script exhaustion: {:?}",
            p["reason"]
        );

        let tools = p["tools"].as_array().expect("tools must be an array");
        assert_eq!(
            tools.len(),
            3,
            "tools[] must have one entry per completed step"
        );

        // Steps must be 1, 2, 3 — monotonically increasing, no gaps.
        let steps: Vec<u64> = tools
            .iter()
            .map(|t| {
                t["step"]
                    .as_u64()
                    .expect("each tool entry must have a step")
            })
            .collect();
        assert_eq!(
            steps,
            vec![1, 2, 3],
            "steps must be monotonically increasing: {steps:?}"
        );

        assert!(p["paths_hint"].is_array(), "paths_hint must be an array");
        assert!(p["instruction"].is_string(), "instruction must be present");
    }

    /// FailingChatModel errors on the very first chat call — no tools ran.
    /// The resulting agent.turn.failed must have tools: [], paths_hint: [],
    /// and valid JSON fields matching agent.turn.exhausted's shape.
    #[test]
    fn failed_payload_has_empty_tool_log_on_early_model_error() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(FailingChatModel));
        let id_earlyerr = open_stream(&state, &open_req(&repo, "earlyerr"))
            .unwrap()
            .context
            .stream_id;

        let result = agent_turn_inner(
            &state,
            &AgentTurnReq {
                model_id: None,
                effort: None,
                actor: None,
                stream_id: id_earlyerr.clone(),
                file_path: None,
                instruction: "do something".into(),
                seat: None,
            },
        );

        assert!(result.is_err(), "FailingChatModel must cause an error");

        let events = state
            .heart
            .lock()
            .unwrap()
            .read_stream(&id_earlyerr)
            .unwrap();
        let failed = events
            .iter()
            .find(|e| e.family == "agent.turn.failed")
            .expect("agent.turn.failed must be appended");

        let p: serde_json::Value = serde_json::from_str(&failed.payload).unwrap();

        assert_eq!(p["phase"].as_str(), Some("model"), "phase must be model");

        let tools = p["tools"].as_array().expect("tools must be an array");
        assert!(
            tools.is_empty(),
            "no tools ran — tools[] must be empty: {tools:?}"
        );

        let paths_hint = p["paths_hint"]
            .as_array()
            .expect("paths_hint must be an array");
        assert!(
            paths_hint.is_empty(),
            "no hot memory — paths_hint must be empty: {paths_hint:?}"
        );

        assert!(p["instruction"].is_string(), "instruction must be present");
        // prior_accept_path should be null (no prior accepts in this stream).
        assert!(
            p["prior_accept_path"].is_null(),
            "prior_accept_path must be null with no prior accepts: {:?}",
            p["prior_accept_path"]
        );
    }

    // ─── 06-SEATS: turns route to a seat, not a node-global model ──────────

    /// STEERING.md § Models: "no credentials and no local model → Setup, not
    /// a toy loop" — the same rule for "nobody assigned this seat at all".
    #[test]
    fn tool_loop_turn_fails_closed_when_the_resolved_seat_has_no_configured_model() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with_no_default_model(dir.path());
        let id_noseat = open_stream(&state, &open_req(&repo, "noseat"))
            .unwrap()
            .context
            .stream_id;

        let err = agent_turn_inner(
            &state,
            &AgentTurnReq {
                model_id: None,
                effort: None,
                actor: None,
                stream_id: id_noseat.clone(),
                file_path: None,
                instruction: "hi".into(),
                seat: None,
            },
        )
        .unwrap_err()
        .1;
        assert!(err.contains("no model assigned"), "{err}");
        assert!(
            err.contains("orchestrator"),
            "must name the resolved seat: {err}"
        );

        let events = state.heart.lock().unwrap().read_stream(&id_noseat).unwrap();
        let failed = events
            .iter()
            .find(|e| e.family == "agent.turn.failed")
            .expect("a Setup-shaped failure must still be recorded, not silently swallowed");
        let p: serde_json::Value = serde_json::from_str(&failed.payload).unwrap();
        assert_eq!(p["phase"], "model");
    }

    /// Same fail-closed rule on the direct-edit (`file_path`) turn path, not
    /// just the tool loop — both call `resolve_turn_seat_and_model`.
    #[test]
    fn direct_edit_turn_also_fails_closed_when_the_resolved_seat_has_no_configured_model() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with_no_default_model(dir.path());
        let id_noseatdirect = open_stream(&state, &open_req(&repo, "noseatdirect"))
            .unwrap()
            .context
            .stream_id;

        let err = agent_turn_inner(
            &state,
            &AgentTurnReq {
                model_id: None,
                effort: None,
                actor: None,
                stream_id: id_noseatdirect.clone(),
                file_path: Some("f.txt".into()),
                instruction: "hi".into(),
                seat: None,
            },
        )
        .unwrap_err()
        .1;
        assert!(err.contains("no model assigned"), "{err}");
    }

    /// STEERING.md § Baker: a Baker stream's turns default to the baker
    /// seat, not orchestrator. Orchestrator's only route to an answer here
    /// is `state_with`'s `test_default_model` fallback; Baker is seeded
    /// directly — if the routing picked the wrong seat, the actor on the
    /// resulting event would name the fallback model instead.
    #[test]
    fn a_baker_stream_defaults_to_the_baker_seat_not_orchestrator() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(ScriptedModel::new(&[])));
        let id_baker_turn = open_stream(&state, &open_req(&repo, "baker-turn"))
            .unwrap()
            .context
            .stream_id;
        mark_factory(&state, &id_baker_turn);
        crate::state::seed_seat_model_for_test(
            &state,
            &id_baker_turn,
            crate::node::Seat::Baker,
            Arc::new(StubModel),
        );

        let ev = agent_turn_inner(
            &state,
            &AgentTurnReq {
                model_id: None,
                effort: None,
                actor: None,
                stream_id: id_baker_turn.clone(),
                file_path: None,
                instruction: "hi".into(),
                seat: None,
            },
        )
        .unwrap();

        assert_eq!(
            ev.actor, "agent:baker",
            "a Baker stream must default to the baker seat"
        );
        let p: serde_json::Value = serde_json::from_str(&ev.payload).unwrap();
        assert_eq!(
            p["model_id"], "stub",
            "the baker seat's model still shows up in the payload"
        );
    }

    /// `seat` in the request wins over the default even on an ordinary
    /// (non-Baker) stream.
    #[test]
    fn a_turn_can_explicitly_request_a_seat_other_than_the_default() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(ScriptedModel::new(&[])));
        let id_seatpick = open_stream(&state, &open_req(&repo, "seatpick"))
            .unwrap()
            .context
            .stream_id;
        crate::state::seed_seat_model_for_test(
            &state,
            &id_seatpick,
            crate::node::Seat::Worker,
            Arc::new(StubModel),
        );

        let ev = agent_turn_inner(
            &state,
            &AgentTurnReq {
                model_id: None,
                effort: None,
                actor: None,
                stream_id: id_seatpick.clone(),
                file_path: None,
                instruction: "hi".into(),
                seat: Some(crate::node::Seat::Worker),
            },
        )
        .unwrap();

        assert_eq!(
            ev.actor, "agent:worker",
            "an explicit seat must override the default"
        );
        let p: serde_json::Value = serde_json::from_str(&ev.payload).unwrap();
        assert_eq!(p["model_id"], "stub");
    }

    // ─── 07d-A: agent identity ──────────────────────────────────────────────

    #[test]
    fn agent_id_is_recorded_on_started_tool_called_and_the_proposal() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let model = Arc::new(ScriptedModel::new(&[
            r#"{"tool":"list_files","args":{"path":"."}}"#,
            r#"{"edit":{"file_path":"f.txt","new_content":"x\n","rationale":"r"}}"#,
        ]));
        let state = state_with(dir.path(), model);
        let id_aid = open_stream(&state, &open_req(&repo, "aid"))
            .unwrap()
            .context
            .stream_id;

        // Manual proposal parks the turn — thread it and assert on events.
        let pending_hash = spawn_turn_and_await_pending(&state, &id_aid, None, "do it");

        let expected = format!("{id_aid}:orchestrator");
        let stream = state.heart.lock().unwrap().read_stream(&id_aid).unwrap();
        let ev = stream
            .iter()
            .find(|e| e.event_hash == pending_hash)
            .unwrap()
            .clone();

        let started = stream
            .iter()
            .find(|e| e.family == "agent.turn.started")
            .unwrap();
        let started_p: serde_json::Value = serde_json::from_str(&started.payload).unwrap();
        assert_eq!(started_p["agent_id"], expected);

        let tool_called = stream
            .iter()
            .find(|e| e.family == "agent.tool.called")
            .unwrap();
        let tool_p: serde_json::Value = serde_json::from_str(&tool_called.payload).unwrap();
        assert_eq!(tool_p["agent_id"], expected);
        assert_eq!(tool_p["model_id"], "scripted");

        let changed_p: serde_json::Value = serde_json::from_str(&ev.payload).unwrap();
        assert_eq!(
            changed_p["agent_id"], expected,
            "the proposal itself must carry agent_id"
        );
        assert_eq!(changed_p["model_id"], "scripted");
    }

    #[test]
    fn agent_id_survives_a_model_reassignment() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id_swap = open_stream(&state, &open_req(&repo, "swap"))
            .unwrap()
            .context
            .stream_id;

        crate::state::seed_seat_model_for_test(
            &state,
            &id_swap,
            crate::node::Seat::Orchestrator,
            Arc::new(ScriptedModel::new(&[r#"{"done":{"summary":"first"}}"#])),
        );
        agent_turn_inner(
            &state,
            &AgentTurnReq {
                model_id: None,
                effort: None,
                actor: None,
                stream_id: id_swap.clone(),
                file_path: None,
                instruction: "one".into(),
                seat: Some(crate::node::Seat::Orchestrator),
            },
        )
        .unwrap();

        // Reassign the SAME seat to a different model — only the actor's
        // model portion and `model_id` should change on the next turn;
        // `agent_id` must be identical to the first turn's.
        crate::state::seed_seat_model_for_test(
            &state,
            &id_swap,
            crate::node::Seat::Orchestrator,
            Arc::new(ScriptedModel::new(&[r#"{"done":{"summary":"second"}}"#])),
        );
        agent_turn_inner(
            &state,
            &AgentTurnReq {
                model_id: None,
                effort: None,
                actor: None,
                stream_id: id_swap.clone(),
                file_path: None,
                instruction: "two".into(),
                seat: Some(crate::node::Seat::Orchestrator),
            },
        )
        .unwrap();

        let stream = state.heart.lock().unwrap().read_stream(&id_swap).unwrap();
        let dones: Vec<_> = stream
            .iter()
            .filter(|e| e.family == "agent.turn.done")
            .collect();
        assert_eq!(dones.len(), 2);
        let expected_agent_id = format!("{id_swap}:orchestrator");
        for ev in &dones {
            let p: serde_json::Value = serde_json::from_str(&ev.payload).unwrap();
            assert_eq!(
                p["agent_id"], expected_agent_id,
                "agent_id must survive a model reassignment"
            );
        }
    }

    /// A configured agent's prompt uses only its own prior turns — never
    /// another configured agent's thread on the same socket (07d-A "Do not:
    /// merge two agents' transcripts into one prompt 'for context'").
    #[test]
    fn hot_memory_scan_does_not_mix_two_agents_prior_turns() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id_iso = open_stream(&state, &open_req(&repo, "iso"))
            .unwrap()
            .context
            .stream_id;

        let orch_model = Arc::new(ScriptedModel::new(&[
            r#"{"done":{"summary":"orchestrator finished the setup"}}"#,
        ]));
        crate::state::seed_seat_model_for_test(
            &state,
            &id_iso,
            crate::node::Seat::Orchestrator,
            orch_model.clone(),
        );
        agent_turn_inner(
            &state,
            &AgentTurnReq {
                model_id: None,
                effort: None,
                actor: None,
                stream_id: id_iso.clone(),
                file_path: None,
                instruction: "orchestrator's own first instruction".into(),
                seat: Some(crate::node::Seat::Orchestrator),
            },
        )
        .unwrap();

        let worker_model = Arc::new(ScriptedModel::new(&[
            r#"{"done":{"summary":"worker finished too"}}"#,
        ]));
        crate::state::seed_seat_model_for_test(
            &state,
            &id_iso,
            crate::node::Seat::Worker,
            worker_model.clone(),
        );
        agent_turn_inner(
            &state,
            &AgentTurnReq {
                model_id: None,
                effort: None,
                actor: None,
                stream_id: id_iso.clone(),
                file_path: None,
                instruction: "worker's own instruction".into(),
                seat: Some(crate::node::Seat::Worker),
            },
        )
        .unwrap();

        let worker_first_msg = worker_model.captured.lock().unwrap()[0].1.clone();
        assert!(
            !worker_first_msg.contains("orchestrator finished the setup"),
            "worker's prompt must not contain the orchestrator's prior turn: {worker_first_msg}"
        );
        assert!(!worker_first_msg.contains("orchestrator's own first instruction"));

        // The SAME agent's own second turn must still see its own prior
        // turn — filtering by agent must not also erase same-agent
        // continuity.
        orch_model
            .replies
            .lock()
            .unwrap()
            .push_back(r#"{"done":{"summary":"orchestrator second turn"}}"#.to_string());
        agent_turn_inner(
            &state,
            &AgentTurnReq {
                model_id: None,
                effort: None,
                actor: None,
                stream_id: id_iso.clone(),
                file_path: None,
                instruction: "orchestrator's second instruction".into(),
                seat: Some(crate::node::Seat::Orchestrator),
            },
        )
        .unwrap();
        let orch_second_msg = orch_model.captured.lock().unwrap()[1].1.clone();
        assert!(
            orch_second_msg.contains("orchestrator finished the setup"),
            "orchestrator's own second turn must still see its own prior turn: {orch_second_msg}"
        );
    }

    /// A model whose `chat()` call blocks until the test releases it —
    /// proves two turns on different seats genuinely run concurrently (not
    /// merely "not deadlocking eventually"): each thread must reach its own
    /// `chat()` call while the other is still blocked inside its own.
    struct GatedModel {
        entered: std::sync::mpsc::Sender<()>,
        release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
        reply: String,
    }
    impl Model for GatedModel {
        fn name(&self) -> &str {
            "gated"
        }
        fn propose_edit(&self, _req: &bakery_agent::EditRequest) -> Result<ProposedEdit, String> {
            Err("chat only".into())
        }
        fn supports_chat(&self) -> bool {
            true
        }
        fn chat(
            &self,
            _system: &str,
            _messages: &[bakery_agent::ChatMsg],
        ) -> Result<String, String> {
            self.entered.send(()).ok();
            self.release
                .lock()
                .unwrap()
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("test never released this model call");
            Ok(self.reply.clone())
        }
    }

    #[test]
    fn two_seats_run_turns_concurrently_without_blocking_each_other() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id_concur = open_stream(&state, &open_req(&repo, "concur"))
            .unwrap()
            .context
            .stream_id;

        let (orc_entered_tx, orc_entered_rx) = std::sync::mpsc::channel();
        let (orc_release_tx, orc_release_rx) = std::sync::mpsc::channel();
        let (wrk_entered_tx, wrk_entered_rx) = std::sync::mpsc::channel();
        let (wrk_release_tx, wrk_release_rx) = std::sync::mpsc::channel();

        crate::state::seed_seat_model_for_test(
            &state,
            &id_concur,
            crate::node::Seat::Orchestrator,
            Arc::new(GatedModel {
                entered: orc_entered_tx,
                release: std::sync::Mutex::new(orc_release_rx),
                reply: r#"{"done":{"summary":"orc ok"}}"#.into(),
            }),
        );
        crate::state::seed_seat_model_for_test(
            &state,
            &id_concur,
            crate::node::Seat::Worker,
            Arc::new(GatedModel {
                entered: wrk_entered_tx,
                release: std::sync::Mutex::new(wrk_release_rx),
                reply: r#"{"done":{"summary":"wrk ok"}}"#.into(),
            }),
        );

        let s1 = state.clone();
        let id_concur_t1 = id_concur.clone();
        let t1 = std::thread::spawn(move || {
            agent_turn_inner(
                &s1,
                &AgentTurnReq {
                    model_id: None,
                    effort: None,
                    actor: None,
                    stream_id: id_concur_t1,
                    file_path: None,
                    instruction: "one".into(),
                    seat: Some(crate::node::Seat::Orchestrator),
                },
            )
        });
        let s2 = state.clone();
        let id_concur_t2 = id_concur.clone();
        let t2 = std::thread::spawn(move || {
            agent_turn_inner(
                &s2,
                &AgentTurnReq {
                    model_id: None,
                    effort: None,
                    actor: None,
                    stream_id: id_concur_t2,
                    file_path: None,
                    instruction: "two".into(),
                    seat: Some(crate::node::Seat::Worker),
                },
            )
        });

        // Both must reach their model call within a bounded time. If a
        // stream-wide lock were reintroduced, the second turn would never
        // even reach its `chat()` call while the first sits blocked inside
        // its own — this would time out instead of silently passing.
        orc_entered_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("orchestrator turn never reached its model call");
        wrk_entered_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("worker turn never reached its model call — it was blocked behind the orchestrator's turn");

        // While both are genuinely mid-turn, `/studio/state`'s busy
        // projection must show both agents busy at once (07d-A "Studio:
        // agents expose agent_id, busy").
        assert!(crate::state::is_turn_running(
            &state,
            &id_concur,
            crate::node::Seat::Orchestrator
        ));
        assert!(crate::state::is_turn_running(
            &state,
            &id_concur,
            crate::node::Seat::Worker
        ));

        orc_release_tx.send(()).unwrap();
        wrk_release_tx.send(()).unwrap();
        let r1 = t1.join().unwrap();
        let r2 = t2.join().unwrap();
        assert!(r1.is_ok(), "{:?}", r1.err());
        assert!(r2.is_ok(), "{:?}", r2.err());

        assert!(!crate::state::is_turn_running(
            &state,
            &id_concur,
            crate::node::Seat::Orchestrator
        ));
        assert!(!crate::state::is_turn_running(
            &state,
            &id_concur,
            crate::node::Seat::Worker
        ));
    }

    /// 07d-A1 §5: a second turn for the exact same agent while the first is
    /// still running is refused with 409 — atomically, before the second
    /// `agent.turn.started` would ever be appended — while a different agent
    /// on the same socket keeps running unaffected (no socket-wide mutex).
    #[test]
    fn two_overlapping_turns_on_the_same_agent_the_second_gets_409_with_no_second_started_event() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id_dup = open_stream(&state, &open_req(&repo, "dup"))
            .unwrap()
            .context
            .stream_id;

        let (orc_entered_tx, orc_entered_rx) = std::sync::mpsc::channel();
        let (orc_release_tx, orc_release_rx) = std::sync::mpsc::channel();
        let (wrk_entered_tx, wrk_entered_rx) = std::sync::mpsc::channel();
        let (wrk_release_tx, wrk_release_rx) = std::sync::mpsc::channel();

        crate::state::seed_seat_model_for_test(
            &state,
            &id_dup,
            crate::node::Seat::Orchestrator,
            Arc::new(GatedModel {
                entered: orc_entered_tx,
                release: std::sync::Mutex::new(orc_release_rx),
                reply: r#"{"done":{"summary":"orc ok"}}"#.into(),
            }),
        );
        crate::state::seed_seat_model_for_test(
            &state,
            &id_dup,
            crate::node::Seat::Worker,
            Arc::new(GatedModel {
                entered: wrk_entered_tx,
                release: std::sync::Mutex::new(wrk_release_rx),
                reply: r#"{"done":{"summary":"wrk ok"}}"#.into(),
            }),
        );

        let s1 = state.clone();
        let id_dup_t1 = id_dup.clone();
        let t1 = std::thread::spawn(move || {
            agent_turn_inner(
                &s1,
                &AgentTurnReq {
                    model_id: None,
                    effort: None,
                    actor: None,
                    stream_id: id_dup_t1,
                    file_path: None,
                    instruction: "first".into(),
                    seat: Some(crate::node::Seat::Orchestrator),
                },
            )
        });
        // Wait until the first turn is genuinely mid-flight (past `admit_turn`,
        // blocked inside its own model call) before firing the second.
        orc_entered_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("first orchestrator turn never reached its model call");

        let second = agent_turn_inner(
            &state,
            &AgentTurnReq {
                model_id: None,
                effort: None,
                actor: None,
                stream_id: id_dup.clone(),
                file_path: None,
                instruction: "second, same agent".into(),
                seat: Some(crate::node::Seat::Orchestrator),
            },
        );
        let (status, _) =
            second.expect_err("a second turn for the same still-running agent must be refused");
        assert_eq!(status, StatusCode::CONFLICT);

        let started_count = state
            .heart
            .lock()
            .unwrap()
            .read_stream(&id_dup)
            .unwrap()
            .iter()
            .filter(|e| e.family == "agent.turn.started")
            .count();
        assert_eq!(
            started_count, 1,
            "the refused second attempt must never append its own agent.turn.started"
        );

        // A different agent on the same socket is unaffected by the refusal.
        let s2 = state.clone();
        let t2 = std::thread::spawn(move || {
            agent_turn_inner(
                &s2,
                &AgentTurnReq {
                    model_id: None,
                    effort: None,
                    actor: None,
                    stream_id: id_dup.clone(),
                    file_path: None,
                    instruction: "worker turn".into(),
                    seat: Some(crate::node::Seat::Worker),
                },
            )
        });
        wrk_entered_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect(
                "a different agent must still run concurrently with the refused agent's live turn",
            );

        orc_release_tx.send(()).unwrap();
        wrk_release_tx.send(()).unwrap();
        assert!(t1.join().unwrap().is_ok());
        assert!(t2.join().unwrap().is_ok());
    }

    /// 07d-A1 §4: turning as Baker on a project socket, or as an ordinary
    /// project seat on the factory socket, is refused — mirrors seats.rs's
    /// assignment-time enforcement for the turn path.
    #[test]
    fn a_turn_refuses_a_seat_that_does_not_match_its_socket_kind() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id_notbakerturn = open_stream(&state, &open_req(&repo, "notbakerturn"))
            .unwrap()
            .context
            .stream_id;
        let id_baker_turnkind = open_stream(&state, &open_req(&repo, "baker-turnkind"))
            .unwrap()
            .context
            .stream_id;
        mark_factory(&state, &id_baker_turnkind);

        let (status, msg) = agent_turn_inner(
            &state,
            &AgentTurnReq {
                model_id: None,
                effort: None,
                actor: None,
                stream_id: id_notbakerturn.clone(),
                file_path: None,
                instruction: "hi".into(),
                seat: Some(crate::node::Seat::Baker),
            },
        )
        .unwrap_err();
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(msg.to_ascii_lowercase().contains("project socket"), "{msg}");

        let (status, msg) = agent_turn_inner(
            &state,
            &AgentTurnReq {
                model_id: None,
                effort: None,
                actor: None,
                stream_id: id_baker_turnkind.clone(),
                file_path: None,
                instruction: "hi".into(),
                seat: Some(crate::node::Seat::Worker),
            },
        )
        .unwrap_err();
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(msg.to_ascii_lowercase().contains("factory socket"), "{msg}");

        // Neither refusal leaves a stream event behind — the turn never began.
        let notbaker_events = state
            .heart
            .lock()
            .unwrap()
            .read_stream(&id_notbakerturn)
            .unwrap();
        assert!(!notbaker_events
            .iter()
            .any(|e| e.family == "agent.turn.started"));
    }

    /// 07d-A1 §1: `agent.turn.started` carries the seat's assigned `model_id`
    /// when the assignment record is readable, before the model itself is
    /// ever resolved.
    #[test]
    fn turn_started_records_the_assigned_model_id_when_readable() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id_startedmid = open_stream(&state, &open_req(&repo, "startedmid"))
            .unwrap()
            .context
            .stream_id;
        // An unrecognized model id fails resolution deterministically with
        // no network/credential dependency — `resolve_seat_model` refuses it
        // outright regardless of environment.
        crate::state::assign_seat_model(
            &state,
            &id_startedmid,
            crate::node::Seat::Orchestrator,
            "not-a-real-provider",
        )
        .unwrap();

        // The turn itself fails at resolution — `agent.turn.started` is
        // still written first, and it must already know the declared
        // assignment.
        let _ = agent_turn_inner(
            &state,
            &AgentTurnReq {
                model_id: None,
                effort: None,
                actor: None,
                stream_id: id_startedmid.clone(),
                file_path: None,
                instruction: "hi".into(),
                seat: None,
            },
        );

        let stream = state
            .heart
            .lock()
            .unwrap()
            .read_stream(&id_startedmid)
            .unwrap();
        let started = stream
            .iter()
            .find(|e| e.family == "agent.turn.started")
            .unwrap();
        let p: serde_json::Value = serde_json::from_str(&started.payload).unwrap();
        assert_eq!(p["model_id"], "not-a-real-provider");

        // The resulting `agent.turn.failed` also names the assigned id — a
        // resolution failure with no model ever resolved (07d-A1 §1).
        let failed = stream
            .iter()
            .find(|e| e.family == "agent.turn.failed")
            .unwrap();
        let fp: serde_json::Value = serde_json::from_str(&failed.payload).unwrap();
        assert_eq!(fp["model_id"], "not-a-real-provider");
    }

    // ─── 07d-A1 §2: fail-closed agent ownership ─────────────────────────────

    fn synthetic_event(family: &str, payload: &str, hash: &str, causality: Vec<String>) -> Event {
        Event {
            stream_id: "wt:x".into(),
            seq: 0,
            content_hash: "c".into(),
            prev_hash: None,
            actor: "agent:orchestrator".into(),
            ts: "2020-01-01T00:00:00Z".into(),
            causality,
            layer: None,
            sensitivity: "normal".into(),
            family: family.into(),
            payload: payload.into(),
            event_hash: hash.into(),
        }
    }

    /// An agent-scoped event with no `agent_id`, a malformed `agent_id`, or an
    /// unreadable payload belongs to no agent — there is no production legacy
    /// history to grandfather in. Explicit socket-global bookkeeping (no
    /// `agent_id` concept at all) is unaffected.
    #[test]
    fn events_owned_by_agent_fails_closed_on_untagged_or_malformed_agent_scoped_events() {
        let events = vec![
            synthetic_event(
                "agent.turn.started",
                r#"{"agent_id":"a1","instruction":"hi"}"#,
                "h1",
                vec![],
            ),
            synthetic_event(
                "agent.file.changed",
                r#"{"file_path":"x.txt"}"#,
                "h2",
                vec![],
            ),
            synthetic_event(
                "agent.tool.called",
                r#"{"agent_id":123,"tool":"read_file"}"#,
                "h3",
                vec![],
            ),
            synthetic_event("agent.turn.done", "not json at all", "h4", vec![]),
            synthetic_event(
                "seat.model.assigned",
                r#"{"seat":"orchestrator","model_id":"grok"}"#,
                "h5",
                vec![],
            ),
            synthetic_event("project.onboarded", r#"{"project":"p"}"#, "h6", vec![]),
        ];

        let owned = events_owned_by_agent(events, "a1");
        let hashes: Vec<&str> = owned.iter().map(|e| e.event_hash.as_str()).collect();

        assert!(
            hashes.contains(&"h1"),
            "a correctly tagged event must survive: {hashes:?}"
        );
        assert!(
            !hashes.contains(&"h2"),
            "agent-scoped with no agent_id must fail closed: {hashes:?}"
        );
        assert!(
            !hashes.contains(&"h3"),
            "agent-scoped with a malformed agent_id must fail closed: {hashes:?}"
        );
        assert!(
            !hashes.contains(&"h4"),
            "agent-scoped with an unreadable payload must fail closed: {hashes:?}"
        );
        assert!(
            hashes.contains(&"h5"),
            "socket-global bookkeeping must remain socket-global: {hashes:?}"
        );
        assert!(
            hashes.contains(&"h6"),
            "socket-global bookkeeping must remain socket-global: {hashes:?}"
        );
    }

    /// A decision (`agent.edit.accepted`/`rejected`/`failed`) carries no
    /// `agent_id` of its own — it belongs to this agent only when its
    /// causality cites a proposal this agent already owns.
    #[test]
    fn events_owned_by_agent_follows_decisions_back_to_their_proposal_via_causality() {
        let events = vec![
            synthetic_event(
                "agent.file.changed",
                r#"{"agent_id":"a1","file_path":"mine.txt"}"#,
                "p1",
                vec![],
            ),
            synthetic_event(
                "agent.file.changed",
                r#"{"agent_id":"a2","file_path":"theirs.txt"}"#,
                "p2",
                vec![],
            ),
            synthetic_event(
                "agent.edit.accepted",
                r#"{"decided_event":"p1","status":"accepted"}"#,
                "d1",
                vec!["p1".into()],
            ),
            synthetic_event(
                "agent.edit.rejected",
                r#"{"decided_event":"p2","status":"rejected"}"#,
                "d2",
                vec!["p2".into()],
            ),
        ];

        let owned = events_owned_by_agent(events, "a1");
        let hashes: Vec<&str> = owned.iter().map(|e| e.event_hash.as_str()).collect();

        assert_eq!(
            hashes,
            vec!["p1", "d1"],
            "decisions follow causality to a proposal this agent owns, and only that one"
        );
    }

    /// 07d-A1 correction: a family this build has never heard of — not on
    /// `SOCKET_GLOBAL_FAMILIES`, not a decision family — must default to
    /// agent-scoped and fail closed, exactly like a known agent family
    /// would, never fall through to "pass everyone." Simulates a future
    /// `agent.*` family someone adds without remembering to list it
    /// anywhere: it must still exclude, not leak.
    #[test]
    fn an_unrecognized_family_defaults_to_agent_scoped_fail_closed_not_socket_global() {
        let events = vec![
            synthetic_event("agent.turn.started", r#"{"agent_id":"a1"}"#, "h1", vec![]),
            // A brand-new family nobody has classified yet, tagged for a
            // DIFFERENT agent.
            synthetic_event(
                "agent.some_future_family",
                r#"{"agent_id":"a2"}"#,
                "h2",
                vec![],
            ),
            // The same unrecognized family, correctly tagged for this agent.
            synthetic_event(
                "agent.some_future_family",
                r#"{"agent_id":"a1"}"#,
                "h3",
                vec![],
            ),
            // The same family with no agent_id at all.
            synthetic_event("agent.some_future_family", r#"{}"#, "h4", vec![]),
        ];

        let owned = events_owned_by_agent(events, "a1");
        let hashes: Vec<&str> = owned.iter().map(|e| e.event_hash.as_str()).collect();

        assert_eq!(
            hashes,
            vec!["h1", "h3"],
            "an unrecognized family must fail closed by default — excluded without a matching agent_id, included with one, never passed through unfiltered"
        );
    }

    /// The ACP pipe's own gate family (`pipe.rs`) — a pending child-tool
    /// approval belongs to the seat that raised it, and its decision
    /// follows causality back to that pending gate, the same shape as
    /// `agent.edit.*` deciding `agent.file.changed`.
    #[test]
    fn child_tool_gate_families_are_agent_scoped_and_causality_linked() {
        let events = vec![
            synthetic_event(
                "agent.child_tool.pending",
                r#"{"plug":"grok","kind":"read_file","title":"x","seat":"orchestrator","agent_id":"a1"}"#,
                "pend1",
                vec![],
            ),
            synthetic_event(
                "agent.child_tool.pending",
                r#"{"plug":"grok","kind":"read_file","title":"y","seat":"worker","agent_id":"a2"}"#,
                "pend2",
                vec![],
            ),
            synthetic_event(
                "agent.child_tool.decided",
                r#"{"decided_event":"pend1","decision":"allow_once"}"#,
                "dec1",
                vec!["pend1".into()],
            ),
            synthetic_event(
                "agent.child_tool.decided",
                r#"{"decided_event":"pend2","decision":"deny"}"#,
                "dec2",
                vec!["pend2".into()],
            ),
        ];

        let owned = events_owned_by_agent(events, "a1");
        let hashes: Vec<&str> = owned.iter().map(|e| e.event_hash.as_str()).collect();
        assert_eq!(
            hashes,
            vec!["pend1", "dec1"],
            "a1's own pending gate and its decision, never a2's"
        );
    }

    // ─── 07d-A5: Live Work Trace ─────────────────────────────────────────────

    #[test]
    fn looks_like_test_run_matches_common_runners_and_nothing_else() {
        let argv = |parts: &[&str]| serde_json::json!({ "argv": parts });
        assert!(looks_like_test_run("run", &argv(&["cargo", "test"])));
        assert!(looks_like_test_run("run", &argv(&["pnpm", "test"])));
        assert!(looks_like_test_run(
            "bash",
            &argv(&["sh", "-c", "pytest -k foo"])
        ));
        assert!(!looks_like_test_run("run", &argv(&["ls", "-la"])));
        assert!(
            !looks_like_test_run("read_file", &argv(&["cargo", "test"])),
            "only run/bash ever qualify"
        );
        assert!(
            !looks_like_test_run("run", &serde_json::json!({})),
            "no argv at all is never testing"
        );
    }

    /// Every event a turn appends carries the same `turn_id` — the
    /// correlation key 07d-A5 introduces alongside the pre-existing
    /// `agent_id`. Uses a real tool call (`list_dir`, always available)
    /// followed by a text reply so both `agent.tool.called` and
    /// `agent.turn.done` are exercised in one turn.
    #[test]
    fn every_event_a_turn_appends_carries_the_same_turn_id() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let model = Arc::new(ScriptedModel::new(&[
            r#"{"tool":"list_dir","args":{"path":"."}}"#,
            r#"{"done":{"summary":"explored"}}"#,
        ]));
        let state = state_with(dir.path(), model);
        let id = open_stream(&state, &open_req(&repo, "turnid"))
            .unwrap()
            .context
            .stream_id;

        agent_turn_inner(
            &state,
            &AgentTurnReq {
                model_id: None,
                effort: None,
                actor: None,
                stream_id: id.clone(),
                file_path: None,
                instruction: "explore".into(),
                seat: None,
            },
        )
        .unwrap();

        let events = state.heart.lock().unwrap().read_stream(&id).unwrap();
        let started = events
            .iter()
            .find(|e| e.family == "agent.turn.started")
            .unwrap();
        let started_turn_id = serde_json::from_str::<serde_json::Value>(&started.payload).unwrap()
            ["turn_id"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(started_turn_id.starts_with("turn_"), "{started_turn_id}");

        for family in ["agent.tool.called", "agent.turn.done"] {
            let ev = events
                .iter()
                .find(|e| e.family == family)
                .unwrap_or_else(|| panic!("missing {family}"));
            let p: serde_json::Value = serde_json::from_str(&ev.payload).unwrap();
            assert_eq!(
                p["turn_id"], started_turn_id,
                "{family} must carry the same turn_id as agent.turn.started"
            );
        }

        // A short result reaches the model whole, and the event says so.
        let called = events
            .iter()
            .find(|e| e.family == "agent.tool.called")
            .unwrap();
        let p: serde_json::Value = serde_json::from_str(&called.payload).unwrap();
        assert!(p["raw_bytes"].as_u64().is_some_and(|n| n > 0), "{p}");
        assert_eq!(p["shown_bytes"], p["raw_bytes"]);
    }

    /// `agent.tool.called`'s `call_id` is minted fresh per tool call, never
    /// reused across two calls in the same turn — the correlation key for
    /// "which specific tool call is this."
    #[test]
    fn each_tool_call_gets_a_distinct_call_id() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let model = Arc::new(ScriptedModel::new(&[
            r#"{"tool":"list_dir","args":{"path":"."}}"#,
            r#"{"tool":"list_dir","args":{"path":"."}}"#,
            r#"{"done":{"summary":"done"}}"#,
        ]));
        let state = state_with(dir.path(), model);
        let id = open_stream(&state, &open_req(&repo, "callid"))
            .unwrap()
            .context
            .stream_id;

        agent_turn_inner(
            &state,
            &AgentTurnReq {
                model_id: None,
                effort: None,
                actor: None,
                stream_id: id.clone(),
                file_path: None,
                instruction: "explore twice".into(),
                seat: None,
            },
        )
        .unwrap();

        let events = state.heart.lock().unwrap().read_stream(&id).unwrap();
        let call_ids: Vec<String> = events
            .iter()
            .filter(|e| e.family == "agent.tool.called")
            .map(|e| {
                serde_json::from_str::<serde_json::Value>(&e.payload).unwrap()["call_id"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert_eq!(call_ids.len(), 2);
        assert_ne!(
            call_ids[0], call_ids[1],
            "each tool call must mint its own call_id"
        );
        assert!(call_ids.iter().all(|c| c.starts_with("call_")));
    }

    /// `recent_completed_tools` accumulates in order as the turn's tool
    /// calls actually finish — proving `push_completed_tool` is really
    /// wired into `agent_tool_turn`'s loop, not just unit-tested against
    /// `state.rs` in isolation.
    #[test]
    fn recent_completed_tools_records_each_call_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let model = Arc::new(ScriptedModel::new(&[
            r#"{"tool":"list_dir","args":{"path":"."}}"#,
            r#"{"tool":"glob","args":{"pattern":"*.txt"}}"#,
            r#"{"done":{"summary":"done"}}"#,
        ]));
        let state = state_with(dir.path(), model);
        let id = open_stream(&state, &open_req(&repo, "recenttools"))
            .unwrap()
            .context
            .stream_id;
        let agent_id = crate::node::agent_id(&id, crate::node::Seat::Orchestrator);

        agent_turn_inner(
            &state,
            &AgentTurnReq {
                model_id: None,
                effort: None,
                actor: None,
                stream_id: id.clone(),
                file_path: None,
                instruction: "explore".into(),
                seat: None,
            },
        )
        .unwrap();

        let snapshot = crate::state::agent_activity_snapshot(&state, &agent_id)
            .expect("activity must be tracked");
        let tools: Vec<&str> = snapshot
            .recent_completed_tools
            .iter()
            .map(|t| t.tool.as_str())
            .collect();
        assert_eq!(tools, vec!["list_dir", "glob"]);
        assert!(snapshot.recent_completed_tools.iter().all(|t| t.ok));
        assert!(
            matches!(snapshot.phase, ActivityPhase::Complete { .. }),
            "the turn finished with a text reply"
        );
    }

    /// A file-edit proposal (Manual mode) sets `AwaitingApproval` and it
    /// stays there — the activity object outlives the turn function's own
    /// return, since only a later `/agent/accept`/`/agent/reject` resolves
    /// it. Accepting transitions it to `Complete`.
    #[test]
    fn a_pending_proposal_sets_awaiting_approval_and_accept_resolves_it() {
        let (_dir, state, _wt, hash, stream_id) = setup_with_pending();
        let agent_id = crate::node::agent_id(&stream_id, crate::node::Seat::Orchestrator);

        let snapshot = crate::state::agent_activity_snapshot(&state, &agent_id)
            .expect("activity must be tracked");
        match snapshot.phase {
            ActivityPhase::AwaitingApproval {
                kind: ApprovalKind::FileEdit,
                event_hash,
            } => {
                assert_eq!(event_hash, hash);
            }
            other => panic!("expected AwaitingApproval, got {other:?}"),
        }

        decide(
            &state,
            &AgentDecisionReq {
                stream_id: stream_id.clone(),
                event_hash: hash,
            },
            true,
        )
        .unwrap();
        // Continue-after-decision: accept resolves AwaitingApproval into a
        // CONTINUING turn (contacting_model), which then finishes on its
        // own — poll to the terminal Complete instead of asserting an
        // instant one.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let snapshot = crate::state::agent_activity_snapshot(&state, &agent_id).unwrap();
            match snapshot.phase {
                ActivityPhase::AwaitingApproval { .. } => {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "accept must resolve AwaitingApproval"
                    );
                }
                ActivityPhase::Complete { .. } => break,
                _ => assert!(
                    std::time::Instant::now() < deadline,
                    "turn must finish after the decision"
                ),
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    /// Symmetric with accept: reject also resolves the activity, not just
    /// the Heart record.
    #[test]
    fn rejecting_a_pending_proposal_also_resolves_awaiting_approval() {
        let (_dir, state, _wt, hash, stream_id) = setup_with_pending();
        let agent_id = crate::node::agent_id(&stream_id, crate::node::Seat::Orchestrator);

        decide(
            &state,
            &AgentDecisionReq {
                stream_id: stream_id.clone(),
                event_hash: hash,
            },
            false,
        )
        .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let snapshot = crate::state::agent_activity_snapshot(&state, &agent_id).unwrap();
            match snapshot.phase {
                ActivityPhase::AwaitingApproval { .. } => {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "reject must also resolve AwaitingApproval"
                    );
                }
                ActivityPhase::Complete { .. } => break,
                _ => assert!(
                    std::time::Instant::now() < deadline,
                    "turn must finish after the decision"
                ),
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    #[tokio::test]
    async fn stopping_a_pending_file_review_closes_it_before_a_late_accept_can_write() {
        let (_dir, state, wt, hash, stream_id) = setup_with_pending();
        let before = std::fs::read_to_string(wt.join("f.txt")).unwrap();
        let response = agent_turn_cancel(
            State(state.clone()),
            Json(AgentTurnCancelReq {
                stream_id: stream_id.clone(),
                seat: crate::node::Seat::Orchestrator,
            }),
        )
        .await
        .unwrap();
        assert_eq!(response.0["cancelled"], true);
        assert_eq!(response.0["approval_cancelled"], true);

        let err = decide(
            &state,
            &AgentDecisionReq {
                stream_id: stream_id.clone(),
                event_hash: hash.clone(),
            },
            true,
        )
        .unwrap_err();
        assert!(err.contains("stopped"), "{err}");
        assert_eq!(std::fs::read_to_string(wt.join("f.txt")).unwrap(), before);
        let events = state.heart.lock().unwrap().read_stream(&stream_id).unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| {
                    event.family == APPROVAL_CANCELLED_FAMILY
                        && event.causality.iter().any(|cause| cause == &hash)
                })
                .count(),
            1,
            "the file review has one durable terminal Stop decision"
        );
        assert!(
            !events.iter().any(|event| {
                event.family == "agent.edit.accepted"
                    && event.causality.iter().any(|cause| cause == &hash)
            }),
            "a late accept must never write after Stop"
        );
        assert!(gates_inner(&state)
            .unwrap()
            .into_iter()
            .all(|gate| gate.event_hash != hash));
    }

    #[tokio::test]
    async fn get_agent_activity_is_null_for_an_agent_with_no_tracked_turn() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id = open_stream(&state, &open_req(&repo, "noactivity"))
            .unwrap()
            .context
            .stream_id;

        let resp = agent_activity(
            State(state.clone()),
            Query(AgentActivityQuery {
                stream_id: id,
                seat: crate::node::Seat::Orchestrator,
            }),
        )
        .await
        .unwrap();
        assert!(resp.0.is_none());
    }

    #[tokio::test]
    async fn get_agent_activity_returns_the_live_snapshot_once_a_proposal_is_pending() {
        let (_dir, state, _wt, hash, stream_id) = setup_with_pending();
        let resp = agent_activity(
            State(state.clone()),
            Query(AgentActivityQuery {
                stream_id: stream_id.clone(),
                seat: crate::node::Seat::Orchestrator,
            }),
        )
        .await
        .unwrap();
        let snapshot = resp
            .0
            .expect("a proposal is pending — activity must be tracked");
        assert_eq!(snapshot.stream_id, stream_id);
        match snapshot.phase {
            ActivityPhase::AwaitingApproval { event_hash, .. } => assert_eq!(event_hash, hash),
            other => panic!("expected AwaitingApproval, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn post_agent_turn_cancel_pauses_future_dispatch_even_when_no_turn_is_running() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id = open_stream(&state, &open_req(&repo, "nocancel"))
            .unwrap()
            .context
            .stream_id;

        let response = agent_turn_cancel(
            State(state.clone()),
            Json(AgentTurnCancelReq {
                stream_id: id.clone(),
                seat: crate::node::Seat::Orchestrator,
            }),
        )
        .await
        .unwrap();
        assert_eq!(response.0["cancelled"], false);
        assert_eq!(response.0["paused"], true);
        let agent_id = crate::node::agent_id(&id, crate::node::Seat::Orchestrator);
        assert!(crate::agent_control::dispatch_is_paused(
            &state, &id, &agent_id
        ));
    }

    /// A steer is never refused: whatever it carries is queued, and a paused
    /// queue is resumed, since sending a steer is itself the go-ahead.
    #[tokio::test]
    async fn steering_is_never_refused_and_resumes_a_paused_queue() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id = open_stream(&state, &open_req(&repo, "steer-anything"))
            .unwrap()
            .context
            .stream_id;
        let seat = crate::node::Seat::Orchestrator;
        let agent_id = crate::node::agent_id(&id, seat);
        // The dispatcher guard is held, as by a live loop, so nothing runs
        // underneath the assertions.
        state.queue_dispatchers.lock().unwrap().insert(agent_id.clone());
        crate::agent_control::pause_dispatch(&state, &id, seat).unwrap();

        let queued = agent_turn_steer(
            State(state.clone()),
            Json(AgentQueueReq {
                turn: AgentTurnReq {
                    stream_id: id.clone(),
                    file_path: Some("f.txt".to_string()),
                    instruction: "change direction".to_string(),
                    seat: None,
                    model_id: Some("another-model".to_string()),
                    effort: Some("high".to_string()),
                    actor: None,
                },
                attachment_ids: vec!["att_opaque".to_string()],
                read_only: false,
            }),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(queued.family, crate::agent_control::STEER_QUEUED);
        for kept in ["att_opaque", "f.txt"] {
            assert!(queued.payload.contains(kept), "{kept} missing from {}", queued.payload);
        }
        // A model and effort a caller sends are never kept: the turn runs on
        // the selector.
        assert!(!queued.payload.contains("another-model") && !queued.payload.contains("\"effort\""), "{}", queued.payload);
        assert!(!crate::agent_control::dispatch_is_paused(&state, &id, &agent_id));
    }

    #[test]
    fn plan_mode_explains_without_creating_an_edit_proposal() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        // Deliberately make the model ask for an edit despite Plan having
        // withheld that declaration. The runtime backstop, not model
        // obedience, is what proves Plan cannot create a proposal.
        let model = Arc::new(ScriptedModel::new(&[
            r#"{"tool":"edit","args":{"path":"f.txt","old_string":"line one","new_string":"line two","rationale":"should be blocked"}}"#,
            r#"{"done":{"summary":"I inspected the request. Explanation only; no edits were proposed."}}"#,
        ]));
        let state = state_with(dir.path(), model);
        let id = open_stream(&state, &open_req(&repo, "plan-only"))
            .unwrap()
            .context
            .stream_id;

        let done = agent_turn_inner_with_control(
            &state,
            &AgentTurnReq {
                stream_id: id.clone(),
                file_path: None,
                instruction: "Explain only; make no edits".to_string(),
                seat: None,
                model_id: None,
                effort: None,
                actor: None,
            },
            TurnControl {
                queue_event_hash: None,
                mode: Some(OperatingMode::Plan),
                attachment_ids: Vec::new(),
            },
        )
        .unwrap();
        assert_eq!(done.family, "agent.turn.done");
        let events = state.heart.lock().unwrap().read_stream(&id).unwrap();
        assert!(events
            .iter()
            .any(|event| event.family == "agent.tool.blocked"));
        assert!(!events
            .iter()
            .any(|event| event.family == "agent.file.changed"));
        let payload: serde_json::Value = serde_json::from_str(&done.payload).unwrap();
        assert!(payload["summary"]
            .as_str()
            .unwrap()
            .contains("Explanation only"));
    }

    /// The audit's finding: a failing command was recorded and shown as a
    /// success (only the "exit_code: N" text said otherwise). It is a failed
    /// call now — to the model and in the transcript — with its verdict kept
    /// at the end of the stored summary. Re-running a failing test after a
    /// change is progress, not a stall; a command whose output never changes
    /// still stops the turn.
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn a_failing_command_is_a_failed_call_but_reruns_that_change_are_not_a_stall() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        // Each run counts up in a file, so its output changes — like a test
        // suite rerun after an edit. Eight reruns: past every failure ladder.
        let rerun = r#"{"tool":"run","args":{"argv":["/bin/sh","-c","n=$(cat n 2>/dev/null || echo 0); n=$((n+1)); echo $n > n; echo \"attempt $n\"; echo 'test result: FAILED. 1 failed' >&2; exit 101"]}}"#;
        let mut script = vec![rerun; 8];
        script.push(r#"{"done":{"summary":"still red"}}"#);
        let model = Arc::new(ScriptedModel::new(&script));
        let state = state_with(dir.path(), model);
        let id = open_stream(&state, &open_req(&repo, "red-tests")).unwrap().context.stream_id;
        let turn = |instruction: &str| {
            agent_turn_inner_with_control(
                &state,
                &AgentTurnReq {
                    stream_id: id.clone(),
                    file_path: None,
                    instruction: instruction.to_string(),
                    seat: None,
                    model_id: None,
                    effort: None,
                    actor: None,
                },
                TurnControl {
                    queue_event_hash: None,
                    mode: Some(OperatingMode::Auto),
                    attachment_ids: Vec::new(),
                },
            )
        };
        let done = turn("make the tests pass").expect("reruns that change are progress");
        assert_eq!(done.family, "agent.turn.done");
        let events = state.heart.lock().unwrap().read_stream(&id).unwrap();
        let runs: Vec<serde_json::Value> = events
            .iter()
            .filter(|e| e.family == "agent.tool.called")
            .map(|e| serde_json::from_str(&e.payload).unwrap())
            .collect();
        assert_eq!(runs.len(), 8);
        for run in &runs {
            assert_eq!(run["ok"], false, "exit 101 is a failure: {run}");
            let summary = run["summary"].as_str().unwrap();
            assert!(summary.starts_with("exit_code: 101"), "{summary}");
            assert!(summary.contains("test result: FAILED"), "the verdict is kept: {summary}");
        }

        // The same failing output again and again: no progress, so it stops.
        let stuck = r#"{"tool":"run","args":{"argv":["/bin/sh","-c","echo same; exit 1"]}}"#;
        let model = Arc::new(ScriptedModel::new(&[stuck; 9]));
        let state = state_with(dir.path().join("second").as_path(), model);
        let repo2 = make_repo(&dir.path().join("repo2"));
        let id = open_stream(&state, &open_req(&repo2, "stuck")).unwrap().context.stream_id;
        let err = agent_turn_inner_with_control(
            &state,
            &AgentTurnReq {
                stream_id: id,
                file_path: None,
                instruction: "loop".to_string(),
                seat: None,
                model_id: None,
                effort: None,
                actor: None,
            },
            TurnControl {
                queue_event_hash: None,
                mode: Some(OperatingMode::Auto),
                attachment_ids: Vec::new(),
            },
        )
        .unwrap_err();
        assert!(format!("{err:?}").contains("stalled"), "{err:?}");
    }

    /// The audit's finding: on Bakery's own tool loop, Manual ran commands
    /// without asking (looser than Auto Edit, which may not run them at all),
    /// and the operator's "never" for commands was not checked. A command now
    /// raises the same Allow/Deny card a native runtime's commands do; it runs
    /// on Allow, not on Deny, and "never" refuses it even in Auto.
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn manual_asks_before_a_command_runs_and_never_holds_in_every_mode() {
        use crate::pipe::{ChildToolGateDecisionKind, ChildToolKind};
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let touch = |file: &str| format!(r#"{{"tool":"run","args":{{"argv":["/bin/sh","-c","touch {file}"]}}}}"#);
        let (allowed, denied) = (touch("allowed"), touch("denied"));
        let model = Arc::new(ScriptedModel::new(&[
            allowed.as_str(),
            denied.as_str(),
            r#"{"done":{"summary":"asked twice"}}"#,
        ]));
        let state = state_with(dir.path(), model);
        let opened = open_stream(&state, &open_req(&repo, "manual-commands")).unwrap().context;
        let (id, wt) = (opened.stream_id, std::path::PathBuf::from(opened.worktree_path));
        let turn = |state: &AppState, id: &str, mode: OperatingMode| {
            agent_turn_inner_with_control(
                state,
                &AgentTurnReq {
                    stream_id: id.to_string(),
                    file_path: None,
                    instruction: "run things".to_string(),
                    seat: None,
                    model_id: None,
                    effort: None,
                    actor: None,
                },
                TurnControl {
                    queue_event_hash: None,
                    mode: Some(mode),
                    attachment_ids: Vec::new(),
                },
            )
        };
        let running = {
            let (state, id) = (state.clone(), id.clone());
            std::thread::spawn(move || turn(&state, &id, OperatingMode::Manual))
        };
        // Answer each card as it appears: Allow the first, Deny the second.
        for decision in [ChildToolGateDecisionKind::AllowOnce, ChildToolGateDecisionKind::Deny] {
            let mut waited = 0;
            let pending = loop {
                let events = state.heart.lock().unwrap().read_stream(&id).unwrap();
                let undecided = events.iter().find(|e| {
                    e.family == crate::pipe::CHILD_TOOL_PENDING_FAMILY
                        && !events.iter().any(|d| {
                            d.family == crate::pipe::CHILD_TOOL_DECIDED_FAMILY
                                && d.causality.contains(&e.event_hash)
                        })
                });
                if let Some(e) = undecided {
                    break e.clone();
                }
                assert!(waited < 200, "no approval card appeared");
                waited += 1;
                std::thread::sleep(std::time::Duration::from_millis(50));
            };
            assert!(pending.payload.contains("run: /bin/sh -c touch"), "{}", pending.payload);
            crate::pipe::decide_child_tool_gate(&state, &id, &pending.event_hash, decision).unwrap();
        }
        let done = running.join().unwrap().unwrap();
        assert_eq!(done.family, "agent.turn.done");
        assert!(wt.join("allowed").exists(), "allowed once, so it ran");
        assert!(!wt.join("denied").exists(), "denied, so it never ran");

        // "never" for commands refuses one even in Auto — no card, no run.
        let never = touch("never");
        let model = Arc::new(ScriptedModel::new(&[never.as_str(), r#"{"done":{"summary":"refused"}}"#]));
        let state2 = state_with(dir.path().join("second").as_path(), model);
        state2.node.lock().unwrap().denied_child_tools.push(ChildToolKind::Execute);
        let repo2 = make_repo(&dir.path().join("repo2"));
        let opened2 = open_stream(&state2, &open_req(&repo2, "never-commands")).unwrap().context;
        assert_eq!(turn(&state2, &opened2.stream_id, OperatingMode::Auto).unwrap().family, "agent.turn.done");
        assert!(!std::path::Path::new(&opened2.worktree_path).join("never").exists(), "never means never, even in Auto");
    }

    #[test]
    fn auto_edit_never_dispatches_command_tools() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        // The side-effecting command must never reach the room. The scripted
        // model ignores the filtered declaration deliberately, exercising
        // the runtime backstop just like the Plan-mode regression above.
        let model = Arc::new(ScriptedModel::new(&[
            r#"{"tool":"run","args":{"argv":["sh","-c","touch should-not-exist"]}}"#,
            r#"{"done":{"summary":"The command needs an explicit Auto-mode decision."}}"#,
        ]));
        let state = state_with(dir.path(), model);
        let id = open_stream(&state, &open_req(&repo, "auto-edit-command"))
            .unwrap()
            .context
            .stream_id;

        let done = agent_turn_inner_with_control(
            &state,
            &AgentTurnReq {
                stream_id: id.clone(),
                file_path: None,
                instruction: "Update the code, but do not run commands automatically".to_string(),
                seat: None,
                model_id: None,
                effort: None,
                actor: None,
            },
            TurnControl {
                queue_event_hash: None,
                mode: Some(OperatingMode::AutoEdit),
                attachment_ids: Vec::new(),
            },
        )
        .unwrap();
        assert_eq!(done.family, "agent.turn.done");
        let events = state.heart.lock().unwrap().read_stream(&id).unwrap();
        assert!(events.iter().any(|event| {
            event.family == "agent.tool.blocked"
                && serde_json::from_str::<serde_json::Value>(&event.payload)
                    .ok()
                    .and_then(|p| p["tool"].as_str().map(str::to_string))
                    .as_deref()
                    == Some("run")
        }));
        assert!(!events.iter().any(|event| {
            event.family == "agent.tool.called"
                && serde_json::from_str::<serde_json::Value>(&event.payload)
                    .ok()
                    .and_then(|p| p["tool"].as_str().map(str::to_string))
                    .as_deref()
                    == Some("run")
        }));
        assert!(!repo.join("should-not-exist").exists());
    }

    /// A model whose `chat_turn_streaming` is a REAL override (not the
    /// trait's one-shot default every other test double here falls back
    /// to) still produces an ordinary `agent.turn.done` — proves
    /// `agent_tool_turn`'s new streaming call site handles a normal
    /// `StreamEnd::Completed(Text(..))` outcome correctly, end to end.
    #[test]
    fn a_real_streaming_model_still_produces_an_ordinary_turn_done() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let model = Arc::new(StreamingScriptedModel::new(vec![ChatReply::Text(
            "streamed answer".to_string(),
        )]));
        let state = state_with(dir.path(), model);
        let id = open_stream(&state, &open_req(&repo, "realstream"))
            .unwrap()
            .context
            .stream_id;

        let ev = agent_turn_inner(
            &state,
            &AgentTurnReq {
                model_id: None,
                effort: None,
                actor: None,
                stream_id: id.clone(),
                file_path: None,
                instruction: "answer directly".into(),
                seat: None,
            },
        )
        .unwrap();
        assert_eq!(ev.family, "agent.turn.done");
        let p: serde_json::Value = serde_json::from_str(&ev.payload).unwrap();
        assert_eq!(p["summary"], "streamed answer");

        let agent_id = crate::node::agent_id(&id, crate::node::Seat::Orchestrator);
        let snapshot = crate::state::agent_activity_snapshot(&state, &agent_id).unwrap();
        assert!(matches!(
            snapshot.phase,
            ActivityPhase::Complete { note: None }
        ));
    }

    /// An API-loop agent can't change a runtime's own settings (they grant
    /// its permissions): even in Auto Edit both write tools are refused, the
    /// transcript shows why, and the turn goes on.
    #[test]
    fn the_api_loops_write_tools_never_change_a_runtimes_settings() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let call = |name: &str, arguments: serde_json::Value| {
            ChatReply::ToolCalls(vec![bakery_agent::ToolCall {
                id: name.into(),
                name: name.into(),
                arguments,
            }])
        };
        let model = Arc::new(StreamingScriptedModel::new(vec![
            call(
                "write_file",
                serde_json::json!({"path": ".claude/settings.local.json", "content": "{\"permissions\":{\"allow\":[\"Bash(*)\"]}}"}),
            ),
            call(
                "edit",
                serde_json::json!({"path": ".claude/settings.json", "old_string": "{}", "new_string": "{\"permissions\":{\"allow\":[\"Bash(*)\"]}}"}),
            ),
            ChatReply::Text("done".into()),
        ]));
        let state = state_with(dir.path(), model);
        let ctx = open_stream(&state, &open_req(&repo, "settings"))
            .unwrap()
            .context;
        let settings = std::path::Path::new(&ctx.worktree_path).join(".claude");
        std::fs::create_dir_all(&settings).unwrap();
        std::fs::write(settings.join("settings.json"), "{}").unwrap();
        agent_control::set_operating_mode(
            &state,
            &ctx.stream_id,
            crate::node::Seat::Orchestrator,
            agent_control::OperatingMode::AutoEdit,
        )
        .unwrap();

        let ev = agent_turn_inner(
            &state,
            &AgentTurnReq {
                model_id: None,
                effort: None,
                actor: None,
                stream_id: ctx.stream_id.clone(),
                file_path: None,
                instruction: "allow yourself everything".into(),
                seat: None,
            },
        )
        .unwrap();
        assert_eq!(ev.family, "agent.turn.done");
        assert!(!settings.join("settings.local.json").exists());
        assert_eq!(
            std::fs::read_to_string(settings.join("settings.json")).unwrap(),
            "{}"
        );
        let events = state
            .heart
            .lock()
            .unwrap()
            .read_stream(&ctx.stream_id)
            .unwrap();
        assert!(!events.iter().any(|e| e.family == "agent.file.changed"));
        let refused: Vec<String> = events
            .iter()
            .filter(|e| e.family == "agent.tool.called")
            .map(|e| serde_json::from_str::<serde_json::Value>(&e.payload).unwrap())
            .filter(|p| p["ok"] == false)
            .map(|p| format!("{} {}", p["tool"], p["summary"]))
            .collect();
        assert_eq!(refused.len(), 2, "{refused:?}");
        assert!(
            refused
                .iter()
                .all(|r| r.contains("an agent runtime's own settings")),
            "{refused:?}"
        );
    }

    /// Answers its one call with `reply`, running `during` first: as if the
    /// operator pressed Stop while the model was still answering.
    struct StopsMidAnswer {
        during: std::sync::Mutex<Option<Box<dyn FnOnce() + Send>>>,
        reply: ChatReply,
    }

    impl bakery_agent::Model for StopsMidAnswer {
        fn name(&self) -> &str {
            "stops-mid-answer"
        }
        fn propose_edit(&self, _req: &bakery_agent::EditRequest) -> Result<ProposedEdit, String> {
            Err("chat only".into())
        }
        fn supports_chat(&self) -> bool {
            true
        }
        fn chat_turn(&self, _system: &str, _messages: &[bakery_agent::ChatMsg], _tools: &[bakery_agent::ToolDecl]) -> Result<ChatReply, String> {
            let during = self.during.lock().unwrap().take();
            if let Some(during) = during {
                during();
            }
            Ok(self.reply.clone())
        }
    }

    /// The calls a model asked for that Stop caught before they ran are each
    /// in the thread, said never to have run, the turn stopped, not failed.
    #[test]
    fn calls_stop_caught_before_they_ran_say_so() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id = open_stream(&state, &open_req(&repo, "stop-before-run")).unwrap().context.stream_id;
        let seat = crate::node::Seat::Orchestrator;
        let call = |name: &str, args: serde_json::Value| bakery_agent::ToolCall { id: format!("c-{name}"), name: name.into(), arguments: args };
        let (stopping, stream) = (state.clone(), id.clone());
        let model = Arc::new(StopsMidAnswer {
            during: std::sync::Mutex::new(Some(Box::new(move || {
                stop_turn(&stopping, &stream, seat).unwrap();
            }))),
            reply: ChatReply::ToolCalls(vec![call("read_file", serde_json::json!({"path": "f.txt"})), call("list_dir", serde_json::json!({"path": "."}))]),
        });
        crate::state::seed_seat_model_for_test(&state, &id, seat, model);

        let req = AgentTurnReq { model_id: None, effort: None, actor: None, stream_id: id.clone(), file_path: None, instruction: "look around".into(), seat: None };
        let ended = agent_turn_inner(&state, &req).unwrap();
        assert_eq!(ended.family, "agent.turn.cancelled");
        let calls: Vec<serde_json::Value> = state
            .heart
            .lock()
            .unwrap()
            .events_of_family(&id, "agent.tool.called")
            .unwrap()
            .iter()
            .map(|e| serde_json::from_str(&e.payload).unwrap())
            .collect();
        let named: Vec<(&str, &str, &str)> =
            calls.iter().map(|c| (c["tool"].as_str().unwrap(), c["stopped"].as_str().unwrap_or(""), c["summary"].as_str().unwrap())).collect();
        assert_eq!(named, [("read_file", "before", STOPPED_BEFORE_IT_RAN), ("list_dir", "before", STOPPED_BEFORE_IT_RAN)]);
    }

    /// A command Stop catches while its Allow card waits never ran: its card
    /// closes saying so, and so does its call, and nothing ran it.
    #[test]
    fn a_command_stopped_at_its_allow_card_never_ran() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let model = Arc::new(ScriptedModel::new(&[r#"{"tool":"run","args":{"argv":["touch","ran"]}}"#, "never asked"]));
        let state = state_with(dir.path(), model);
        let opened = open_stream(&state, &open_req(&repo, "stop-at-allow")).unwrap();
        let (id, wt) = (opened.context.stream_id, std::path::PathBuf::from(&opened.context.worktree_path));
        let seat = crate::node::Seat::Orchestrator;
        let running = {
            let (state, id) = (state.clone(), id.clone());
            std::thread::spawn(move || {
                let req = AgentTurnReq { model_id: None, effort: None, actor: None, stream_id: id, file_path: None, instruction: "make a file".into(), seat: None };
                agent_turn_inner(&state, &req)
            })
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let card = loop {
            let pending = state.heart.lock().unwrap().events_of_family(&id, crate::pipe::CHILD_TOOL_PENDING_FAMILY).unwrap();
            if let Some(card) = pending.into_iter().next() {
                break card;
            }
            assert!(std::time::Instant::now() < deadline, "no Allow card");
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        stop_turn(&state, &id, seat).unwrap();
        let ended = running.join().unwrap().unwrap();
        assert_eq!(ended.family, "agent.turn.cancelled");

        let events = state.heart.lock().unwrap().read_stream(&id).unwrap();
        let payload = |e: &Event| serde_json::from_str::<serde_json::Value>(&e.payload).unwrap();
        let closed: Vec<serde_json::Value> =
            events.iter().filter(|e| e.family == APPROVAL_CANCELLED_FAMILY && e.causality.contains(&card.event_hash)).map(payload).collect();
        assert_eq!(closed.len(), 1);
        assert_eq!(closed[0]["reason"], STOPPED_BEFORE_IT_RAN);
        let call: Vec<serde_json::Value> = events.iter().filter(|e| e.family == "agent.tool.called").map(payload).collect();
        assert_eq!(call.len(), 1, "{call:?}");
        assert_eq!((&call[0]["tool"], &call[0]["stopped"]), (&"run".into(), &"before".into()));
        assert!(!wt.join("ran").exists(), "nothing ran it");
    }

    /// The real end-to-end cancellation path: a turn genuinely in flight
    /// (paused via the model double's synchronization hook, not a sleep) is
    /// stopped by a concurrent `/agent/turn/cancel`-equivalent call,
    /// records `agent.turn.cancelled` (never `agent.turn.failed`), and the
    /// activity snapshot ends in `Cancelled`.
    #[test]
    fn cancelling_a_turn_mid_stream_stops_it_and_records_agent_turn_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        // Two one-way channels form a rendezvous: the hook signals it has
        // started (so the main thread never guesses with a sleep), then
        // BLOCKS until the main thread confirms `request_cancel` has
        // already run — without this second handshake, the model double
        // could race past its own `cancel.load()` check before the main
        // thread's cancel had actually landed.
        let (started_tx, started_rx) = std::sync::mpsc::channel::<()>();
        let (proceed_tx, proceed_rx) = std::sync::mpsc::channel::<()>();
        let model = Arc::new(StreamingScriptedModel::with_first_call_hook(
            vec![ChatReply::Text(
                "this reply is abandoned once cancel lands".to_string(),
            )],
            move || {
                let _ = started_tx.send(());
                let _ = proceed_rx.recv();
            },
        ));
        let state = state_with(dir.path(), model);
        let id = open_stream(&state, &open_req(&repo, "cancelme"))
            .unwrap()
            .context
            .stream_id;
        let agent_id = crate::node::agent_id(&id, crate::node::Seat::Orchestrator);

        let state_for_thread = state.clone();
        let id_for_thread = id.clone();
        let handle = std::thread::spawn(move || {
            agent_turn_inner(
                &state_for_thread,
                &AgentTurnReq {
                    model_id: None,
                    effort: None,
                    actor: None,
                    stream_id: id_for_thread,
                    file_path: None,
                    instruction: "start streaming".into(),
                    seat: None,
                },
            )
        });

        started_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("the model double must signal it started");
        assert!(
            request_cancel(&state, &agent_id),
            "a turn is genuinely in flight — cancel must succeed"
        );
        proceed_tx
            .send(())
            .expect("the model double must still be waiting on this channel");

        let ev = handle
            .join()
            .unwrap()
            .expect("a cancelled turn is still Ok, not an HTTP error");
        assert_eq!(ev.family, "agent.turn.cancelled");
        assert_ne!(ev.family, "agent.turn.failed");

        let snapshot = crate::state::agent_activity_snapshot(&state, &agent_id).unwrap();
        assert!(
            matches!(snapshot.phase, ActivityPhase::Cancelled),
            "expected Cancelled, got {:?}",
            snapshot.phase
        );
    }

    #[tokio::test]
    async fn stopping_after_a_question_cancels_the_turn_but_keeps_followups_paused_until_resume() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let model = Arc::new(ScriptedModel::new(&[
            r#"{"tool":"ask_user","args":{"questions":[{"question":"Continue?","options":["yes","no"]}]}}"#,
        ]));
        let state = state_with(dir.path(), model);
        let id = open_stream(&state, &open_req(&repo, "question-stop"))
            .unwrap()
            .context
            .stream_id;
        let agent_id = crate::node::agent_id(&id, crate::node::Seat::Orchestrator);
        let state_for_turn = state.clone();
        let id_for_turn = id.clone();
        let handle = std::thread::spawn(move || {
            agent_turn_inner(
                &state_for_turn,
                &AgentTurnReq {
                    stream_id: id_for_turn,
                    file_path: None,
                    instruction: "ask one question".to_string(),
                    seat: None,
                    model_id: None,
                    effort: None,
                    actor: None,
                },
            )
        });
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if matches!(
                crate::state::agent_activity_snapshot(&state, &agent_id).map(|a| a.phase),
                Some(ActivityPhase::AwaitingApproval {
                    kind: ApprovalKind::Question,
                    ..
                })
            ) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(matches!(
            crate::state::agent_activity_snapshot(&state, &agent_id).map(|a| a.phase),
            Some(ActivityPhase::AwaitingApproval {
                kind: ApprovalKind::Question,
                ..
            })
        ));
        let pending_hash = match crate::state::agent_activity_snapshot(&state, &agent_id)
            .expect("the question turn remains tracked")
            .phase
        {
            ActivityPhase::AwaitingApproval {
                kind: ApprovalKind::Question,
                event_hash,
            } => event_hash,
            phase => panic!("expected question approval, got {phase:?}"),
        };

        let queued = crate::agent_control::enqueue(
            &state,
            AgentTurnReq {
                stream_id: id.clone(),
                file_path: None,
                instruction: "follow up after the question".to_string(),
                seat: Some(crate::node::Seat::Orchestrator),
                model_id: None,
                effort: None,
                actor: None,
            },
            vec![],
            false,
        )
        .unwrap();
        let response = agent_turn_cancel(
            State(state.clone()),
            Json(AgentTurnCancelReq {
                stream_id: id.clone(),
                seat: crate::node::Seat::Orchestrator,
            }),
        )
        .await
        .unwrap();
        assert_eq!(response.0["cancelled"], true);
        assert_eq!(response.0["approval_cancelled"], true);
        assert_eq!(response.0["paused"], true);
        let late_answer = agent_answer_inner(
            &state,
            &AgentAnswerReq {
                stream_id: id.clone(),
                event_hash: pending_hash.clone(),
                answer: "yes".to_string(),
                answers: None,
            },
        )
        .unwrap_err();
        assert!(late_answer.contains("stopped"), "{late_answer}");
        let decisions = state
            .heart
            .lock()
            .unwrap()
            .read_stream(&id)
            .unwrap()
            .into_iter()
            .filter(|event| {
                event.family == APPROVAL_CANCELLED_FAMILY
                    && event.causality.iter().any(|hash| hash == &pending_hash)
            })
            .count();
        assert_eq!(
            decisions, 1,
            "Stop must settle this one pending question exactly once"
        );
        let cancelled = handle.join().unwrap().unwrap();
        assert_eq!(cancelled.family, "agent.turn.cancelled");
        std::thread::sleep(Duration::from_millis(80));
        let starts_before_resume = state
            .heart
            .lock()
            .unwrap()
            .read_stream(&id)
            .unwrap()
            .iter()
            .filter(|event| {
                event.family == "agent.turn.started" && event.causality.contains(&queued.event_hash)
            })
            .count();
        assert_eq!(
            starts_before_resume, 0,
            "Stop must retain—not dispatch or delete—the queued instruction"
        );

        let _ = super::resume_dispatch(
            State(state.clone()),
            Json(AgentTurnCancelReq {
                stream_id: id.clone(),
                seat: crate::node::Seat::Orchestrator,
            }),
        )
        .await
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            let starts = state
                .heart
                .lock()
                .unwrap()
                .read_stream(&id)
                .unwrap()
                .iter()
                .filter(|event| {
                    event.family == "agent.turn.started"
                        && event.causality.contains(&queued.event_hash)
                })
                .count();
            if starts == 1 {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("queued follow-up was not delivered after explicit resume");
    }

    /// An ACP-transport double whose one blocking `chat()` call rendezvous
    /// with the test the same way `StreamingScriptedModel`'s hook does —
    /// proves 07d-A5 review #4's fix: `agent_acp_turn` has no per-step
    /// checkpoint to poll mid-call (the boxed CLI child is opaque from this
    /// side), so its cancel handling can only ever be observed immediately
    /// before and immediately after the one `chat()` call.
    struct AcpDouble {
        started: std::sync::mpsc::Sender<()>,
        proceed: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
        reply: String,
    }
    impl Model for AcpDouble {
        fn name(&self) -> &str {
            "acp:double"
        }
        fn transport(&self) -> bakery_agent::Transport {
            bakery_agent::Transport::Acp
        }
        fn propose_edit(&self, _req: &bakery_agent::EditRequest) -> Result<ProposedEdit, String> {
            Err("acp double never proposes edits".into())
        }
        fn chat(
            &self,
            _system: &str,
            _messages: &[bakery_agent::ChatMsg],
        ) -> Result<String, String> {
            let _ = self.started.send(());
            self.proceed
                .lock()
                .unwrap()
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("test never released this model call");
            Ok(self.reply.clone())
        }
    }

    /// A cancel requested while an ACP turn's one opaque `chat()` call is
    /// still in flight cannot interrupt that call (07d-A5's documented
    /// limit for this transport — same as an already-dispatched HTTP-seat
    /// tool subprocess), but once it returns, the turn must still be
    /// honestly recorded as `agent.turn.cancelled`, never a successful
    /// `agent.turn.done` — and a second, later cancel against the now-
    /// terminal turn must be refused. Before the 07d-A5 review #4 fix,
    /// `agent_acp_turn` never even looked at `cancel`, so this reply would
    /// have been recorded as `agent.turn.done`.
    #[test]
    fn cancelling_an_acp_turn_mid_call_still_records_agent_turn_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let (started_tx, started_rx) = std::sync::mpsc::channel::<()>();
        let (proceed_tx, proceed_rx) = std::sync::mpsc::channel::<()>();
        let model = Arc::new(AcpDouble {
            started: started_tx,
            proceed: std::sync::Mutex::new(proceed_rx),
            reply: "this reply is abandoned once cancel lands".to_string(),
        });
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id = open_stream(&state, &open_req(&repo, "acpcancelme"))
            .unwrap()
            .context
            .stream_id;
        crate::state::seed_seat_model_for_test(&state, &id, crate::node::Seat::Orchestrator, model);
        let agent_id = crate::node::agent_id(&id, crate::node::Seat::Orchestrator);

        let state_for_thread = state.clone();
        let id_for_thread = id.clone();
        let handle = std::thread::spawn(move || {
            agent_turn_inner(
                &state_for_thread,
                &AgentTurnReq {
                    model_id: None,
                    effort: None,
                    actor: None,
                    stream_id: id_for_thread,
                    file_path: None,
                    instruction: "start the acp call".into(),
                    seat: None,
                },
            )
        });

        started_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("the acp double must signal it started");
        assert!(
            request_cancel(&state, &agent_id),
            "a turn is genuinely in flight — cancel must succeed"
        );
        proceed_tx
            .send(())
            .expect("the acp double must still be waiting on this channel");

        let ev = handle
            .join()
            .unwrap()
            .expect("a cancelled turn is still Ok, not an HTTP error");
        assert_eq!(ev.family, "agent.turn.cancelled");
        assert_ne!(ev.family, "agent.turn.done");

        let snapshot = crate::state::agent_activity_snapshot(&state, &agent_id).unwrap();
        assert!(
            matches!(snapshot.phase, ActivityPhase::Cancelled),
            "expected Cancelled, got {:?}",
            snapshot.phase
        );

        // The turn has already ended — a later cancel must be refused, not
        // silently report success (07d-A5 review #8).
        assert!(
            !request_cancel(&state, &agent_id),
            "the turn already finished — a later cancel must be refused"
        );
    }

    // ─── The selector's model and effort + usage ─────────────────────────

    /// Picks `model_id` and `effort` in the conversation's selector (`None`:
    /// the runtime's own), as the composer does, then the turn to send.
    fn turn_req(
        state: &AppState,
        stream_id: &str,
        instruction: &str,
        model_id: Option<&str>,
        effort: Option<&str>,
    ) -> AgentTurnReq {
        let seat = crate::node::Seat::Orchestrator;
        let runtime = crate::state::latest_seat_model_id(&state.heart.lock().unwrap(), stream_id, seat).ok().flatten().unwrap_or_default();
        for (category, value) in [(crate::selection::MODEL, model_id), (crate::selection::EFFORT, effort)] {
            crate::selection::choose(state, stream_id, seat, &runtime, category, value, None, "human").unwrap();
        }
        AgentTurnReq {
            model_id: None,
            effort: None,
            stream_id: stream_id.to_string(),
            file_path: None,
            instruction: instruction.to_string(),
            seat: Some(crate::node::Seat::Orchestrator),
            actor: None,
        }
    }

    /// Review fix (test-coverage gap): every other test that reaches
    /// `compose_turn_usage` supplies a `TurnUsage` with BOTH token counts
    /// `Some`, exercising only the `(Some,Some)` match arm. This directly
    /// drives the two partial-token arms its own doc comment promises are
    /// real ("derived from whichever of the two token counts are actually
    /// present") — a real provider path (Anthropic's `message_start`, which
    /// only reports input tokens) produces exactly this shape.
    #[test]
    fn compose_turn_usage_derives_context_used_from_whichever_token_count_is_present() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));

        let only_in = compose_turn_usage(
            &state,
            "claude",
            None,
            Some(bakery_agent::TurnUsage {
                tokens_in: Some(7),
                tokens_out: None,
            }),
            None,
        )
        .expect("an input-only report is still something reported");
        assert_eq!(
            only_in.context_used,
            Some(7),
            "input-only usage must still derive context_used"
        );

        let only_out = compose_turn_usage(
            &state,
            "claude",
            None,
            Some(bakery_agent::TurnUsage {
                tokens_in: None,
                tokens_out: Some(9),
            }),
            None,
        )
        .expect("an output-only report is still something reported");
        assert_eq!(
            only_out.context_used,
            Some(9),
            "output-only usage must still derive context_used"
        );

        let neither = compose_turn_usage(
            &state,
            "claude",
            None,
            Some(bakery_agent::TurnUsage {
                tokens_in: None,
                tokens_out: None,
            }),
            None,
        );
        assert!(
            neither.is_none(),
            "nothing reported at all must skip the event entirely"
        );
    }

    /// A native runtime's own context reading replaces the token sum (which
    /// leaves out its cached prompt), its extra figures ride along, and
    /// what it did not report stays absent rather than 0.
    #[test]
    fn compose_turn_usage_keeps_a_runtimes_own_context_and_extra_figures() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let usage = compose_turn_usage(
            &state,
            "acp",
            None,
            Some(bakery_agent::TurnUsage {
                tokens_in: Some(12),
                tokens_out: Some(30),
            }),
            Some(bakery_agent::TurnUsageDetail {
                cache_read_tokens: Some(9000),
                cost: Some(bakery_agent::TurnCost {
                    amount: 0.25,
                    currency: "USD".into(),
                }),
                context_used: Some(9042),
                context_window: Some(200_000),
                ..Default::default()
            }),
        )
        .unwrap();
        assert_eq!(usage.context_used, Some(9042));
        assert_eq!(usage.context_window, Some(200_000));
        assert_eq!(usage.cache_read_tokens, Some(9000));
        assert_eq!(usage.cost.as_ref().map(|c| c.amount), Some(0.25));
        assert_eq!(usage.thought_tokens, None);
        assert_eq!(usage.cache_write_tokens, None);

        let cost_only = compose_turn_usage(
            &state,
            "acp",
            None,
            None,
            Some(bakery_agent::TurnUsageDetail {
                cost: Some(bakery_agent::TurnCost {
                    amount: 0.01,
                    currency: "USD".into(),
                }),
                ..Default::default()
            }),
        )
        .expect("a cost alone is still something reported");
        assert_eq!(cost_only.tokens_in, None);
        assert_eq!(cost_only.context_used, None);
    }

    #[test]
    fn a_selector_model_runs_its_own_instance_without_touching_the_persisted_seat_assignment() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id = open_stream(&state, &open_req(&repo, "modeloverride"))
            .unwrap()
            .context
            .stream_id;

        let base = Arc::new(UsageReportingModel::new("base reply", None));
        crate::state::seed_seat_model_for_test(&state, &id, crate::node::Seat::Orchestrator, base);

        let ev = agent_turn_inner(&state, &turn_req(&state, &id, "hi", Some("variant-x"), None)).unwrap();
        let p: serde_json::Value = serde_json::from_str(&ev.payload).unwrap();
        assert_eq!(
            p["summary"], "overridden reply from variant-x",
            "the overridden instance, not the cached base one, must have answered"
        );

        // Picked back to the runtime's own, the next turn goes back to the
        // original cached instance: the variant was never inserted into
        // AppState::seats.
        let ev2 = agent_turn_inner(&state, &turn_req(&state, &id, "hi again", None, None)).unwrap();
        let p2: serde_json::Value = serde_json::from_str(&ev2.payload).unwrap();
        assert_eq!(p2["summary"], "base reply", "the runtime's own model answers again");
    }

    #[test]
    fn the_selectors_effort_runs_every_turn_until_it_is_changed() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id = open_stream(&state, &open_req(&repo, "effortonly"))
            .unwrap()
            .context
            .stream_id;

        let model = Arc::new(UsageReportingModel::new("ok", None));
        crate::state::seed_seat_model_for_test(
            &state,
            &id,
            crate::node::Seat::Orchestrator,
            model.clone(),
        );

        agent_turn_inner(&state, &turn_req(&state, &id, "hi", None, Some("high"))).unwrap();
        assert_eq!(*model.last_effort.lock().unwrap(), Some("high".to_string()));

        // A turn that changes nothing (a queued one, another thread's message)
        // runs on it too.
        *model.last_effort.lock().unwrap() = None;
        let plain = AgentTurnReq { instruction: "and again".into(), ..turn_req_unpicked(&id) };
        agent_turn_inner(&state, &plain).unwrap();
        assert_eq!(*model.last_effort.lock().unwrap(), Some("high".to_string()), "it stays until changed");

        agent_turn_inner(&state, &turn_req(&state, &id, "hi again", None, None)).unwrap();
        assert_eq!(*model.last_effort.lock().unwrap(), None, "picked back to the runtime's own");
    }

    /// The composer's pick on the Bakery's own loop is saved as the thread's
    /// selection, which stays after the send; a native seat with no session
    /// yet (a new chat) keeps its pick for the session to start on.
    #[test]
    fn the_composers_pick_is_saved_and_stays_and_a_native_seat_keeps_it_for_its_session() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id = open_stream(&state, &open_req(&repo, "selection-route")).unwrap().context.stream_id;
        let seat = crate::node::Seat::Orchestrator;
        crate::state::assign_seat_model(&state, &id, seat, "grok").unwrap();
        crate::state::seed_seat_model_for_test(&state, &id, seat, Arc::new(UsageReportingModel::new("ok", None)));
        let rt = tokio::runtime::Runtime::new().unwrap();
        let pick = |model: Option<&str>, effort: Option<&str>| {
            let req = SelectionReq {
                stream_id: id.clone(),
                seat,
                model: model.map(Into::into),
                model_label: None,
                effort: effort.map(Into::into),
                effort_label: Some("High".into()),
            };
            rt.block_on(set_selection(State(state.clone()), Json(req)))
        };
        let saved = pick(Some("grok-4"), Some("high")).unwrap().0;
        assert_eq!((saved.model.as_deref(), saved.effort.as_deref()), (Some("grok-4"), Some("high")));
        agent_turn_inner(&state, &AgentTurnReq { instruction: "go".into(), ..turn_req_unpicked(&id) }).unwrap();
        assert_eq!(crate::selection::chosen(&state, &id, seat), saved, "a send leaves it as it was");

        let native = crate::pipe::AcpPipeModel::new(
            state.clone(),
            id.clone(),
            seat,
            repo.clone(),
            crate::pipe::AcpSpawnConfig {
                binary: crate::testutil::fake_acp_agent_binary(),
                argv_suffix: vec![],
                config_copy: None,
                extra_env: vec![],
                trusted: false,
                mcp: None,
                extra_process_exec_dirs: vec![],
                credential_dirs: vec![],
                read_only: vec![],
            },
            "acp".to_string(),
        );
        crate::state::seed_seat_model_for_test(&state, &id, seat, Arc::new(native));
        let kept = pick(Some("opus"), Some("max")).unwrap().0;
        assert_eq!((kept.model.as_deref(), kept.effort.as_deref()), (Some("opus"), Some("max")));
    }

    /// A turn that picks nothing, as a queued or another thread's message is.
    fn turn_req_unpicked(stream_id: &str) -> AgentTurnReq {
        AgentTurnReq {
            model_id: None,
            effort: None,
            stream_id: stream_id.to_string(),
            file_path: None,
            instruction: String::new(),
            seat: Some(crate::node::Seat::Orchestrator),
            actor: None,
        }
    }

    #[test]
    fn agent_turn_usage_lands_alongside_agent_turn_done_only_when_something_was_reported() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));

        let id_usage = open_stream(&state, &open_req(&repo, "usageyes"))
            .unwrap()
            .context
            .stream_id;
        let model_with_usage = Arc::new(UsageReportingModel::new(
            "ok",
            Some(bakery_agent::TurnUsage {
                tokens_in: Some(10),
                tokens_out: Some(5),
            }),
        ));
        crate::state::seed_seat_model_for_test(
            &state,
            &id_usage,
            crate::node::Seat::Orchestrator,
            model_with_usage,
        );
        agent_turn_inner(&state, &turn_req(&state, &id_usage, "hi", None, None)).unwrap();
        let stream = state.heart.lock().unwrap().read_stream(&id_usage).unwrap();
        let usage_ev = stream
            .iter()
            .find(|e| e.family == "agent.turn.usage")
            .expect("agent.turn.usage must be appended when usage was reported");
        let p: serde_json::Value = serde_json::from_str(&usage_ev.payload).unwrap();
        assert_eq!(p["tokens_in"], 10);
        assert_eq!(p["tokens_out"], 5);
        assert_eq!(p["context_used"], 15);
        assert!(
            p["context_window"].is_null(),
            "no model_id override was given — context_window must stay None"
        );
        assert!(p["duration_ms"].is_u64(), "the turn's wall time: {p}");
        for absent in ["cache_read_tokens", "cache_write_tokens", "thought_tokens", "cost"] {
            assert!(p[absent].is_null(), "{absent} was never reported: {p}");
        }

        // `record_turn_usage`'s own doc comment promises this populates the
        // live activity snapshot too (review fix: previously only the Heart
        // event was ever checked, leaving this half of the function
        // completely unverified).
        let agent_id_usage = crate::node::agent_id(&id_usage, crate::node::Seat::Orchestrator);
        let snapshot = crate::state::agent_activity_snapshot(&state, &agent_id_usage)
            .expect("activity must be tracked");
        let usage = snapshot
            .usage
            .expect("AgentActivity::usage must be populated when usage was reported");
        assert_eq!(usage.tokens_in, Some(10));
        assert_eq!(usage.tokens_out, Some(5));
        assert_eq!(usage.context_used, Some(15));

        let id_nousage = open_stream(&state, &open_req(&repo, "usageno"))
            .unwrap()
            .context
            .stream_id;
        let model_without_usage = Arc::new(UsageReportingModel::new("ok", None));
        crate::state::seed_seat_model_for_test(
            &state,
            &id_nousage,
            crate::node::Seat::Orchestrator,
            model_without_usage,
        );
        let ev = agent_turn_inner(&state, &turn_req(&state, &id_nousage, "hi", None, None)).unwrap();
        assert_eq!(
            ev.family, "agent.turn.done",
            "the turn must still succeed with no usage reported"
        );
        let stream2 = state
            .heart
            .lock()
            .unwrap()
            .read_stream(&id_nousage)
            .unwrap();
        assert!(
            stream2.iter().all(|e| e.family != "agent.turn.usage"),
            "no agent.turn.usage event must be appended when nothing was reported"
        );
    }

    #[test]
    fn a_model_id_override_against_a_seat_with_no_override_support_fails_the_turn_clearly() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        // StubModel (this state's blanket test_default_model) has no
        // per-turn override support — matches ACP/openai-compat/Claude
        // Code's own real default (`Model::with_overridden_model_id`'s
        // safe-no-op-`None` default) exactly.
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id = open_stream(&state, &open_req(&repo, "nooverride"))
            .unwrap()
            .context
            .stream_id;

        let err = agent_turn_inner(&state, &turn_req(&state, &id, "hi", Some("some-variant"), None))
            .map(|_| ())
            .unwrap_err();
        assert!(err.1.contains("no per-turn model override"), "{:?}", err);
    }

    #[test]
    fn context_window_is_populated_only_when_this_turns_model_id_was_overridden() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id = open_stream(&state, &open_req(&repo, "ctxwindow"))
            .unwrap()
            .context
            .stream_id;

        let model = Arc::new(UsageReportingModel::new(
            "ok",
            Some(bakery_agent::TurnUsage {
                tokens_in: Some(1),
                tokens_out: Some(1),
            }),
        ));
        crate::state::seed_seat_model_for_test(&state, &id, crate::node::Seat::Orchestrator, model);

        // `UsageReportingModel::name()` is always "usage-double" and its
        // `transport()` is the `Http` default, which `provider_for` falls
        // through to the "openai-compat" bucket for (it isn't one of the
        // four HTTP built-ins) — seed the catalog under that exact bucket,
        // matching what the real turn code will actually look up.
        let catalog = bakery_agent::ModelCatalog::Available {
            models: vec![bakery_agent::ModelCatalogEntry {
                id: "variant-x".to_string(),
                label: "variant-x".to_string(),
                context_window: Some(999_000),
                effort_levels: Vec::new(),
            }],
        };
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(state.catalog_cache.seed_for_test(
                "openai-compat",
                catalog,
                Duration::from_secs(0),
            ));

        agent_turn_inner(&state, &turn_req(&state, &id, "hi", Some("variant-x"), None)).unwrap();
        let stream = state.heart.lock().unwrap().read_stream(&id).unwrap();
        let usage_ev = stream
            .iter()
            .find(|e| e.family == "agent.turn.usage")
            .unwrap();
        let p: serde_json::Value = serde_json::from_str(&usage_ev.payload).unwrap();
        assert_eq!(p["context_window"], 999_000);
    }

    /// Review fix: `req.effort` used to reach `model.set_effort` with no
    /// trim/empty-string normalization, unlike `model_id_override`, which
    /// `resolve_seat_with_override` already trims and treats a blank value
    /// as "no override". A caller sending `effort: "   "` must be treated
    /// identically — never forwarded verbatim into a real provider request.
    #[test]
    fn a_blank_effort_override_is_treated_as_no_override_not_forwarded_to_the_model() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id = open_stream(&state, &open_req(&repo, "blankeffort"))
            .unwrap()
            .context
            .stream_id;

        let model = Arc::new(UsageReportingModel::new("ok", None));
        crate::state::seed_seat_model_for_test(
            &state,
            &id,
            crate::node::Seat::Orchestrator,
            model.clone(),
        );

        agent_turn_inner(&state, &turn_req(&state, &id, "hi", None, Some("   "))).unwrap();
        assert_eq!(
            *model.last_effort.lock().unwrap(),
            None,
            "a blank/whitespace-only effort must never reach the model's set_effort verbatim"
        );
    }

    /// Review fix: `append_turn_usage_if_any` used to read `req.model_id`
    /// raw for both the catalog-cache lookup key and the persisted
    /// `agent.turn.usage` event's `model_id` field, instead of the same
    /// trim+filter-to-"no override" normalization `resolve_seat_with_override`
    /// already applies before deciding whether an override took effect. A
    /// whitespace-only `model_id` must run the seat's real base model (no
    /// override) and the usage event must report that real model, not the
    /// literal blank string.
    #[test]
    fn a_blank_model_id_override_is_treated_as_no_override_and_never_pollutes_the_usage_event() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id = open_stream(&state, &open_req(&repo, "blankmodelid"))
            .unwrap()
            .context
            .stream_id;

        let base = Arc::new(UsageReportingModel::new(
            "base reply",
            Some(bakery_agent::TurnUsage {
                tokens_in: Some(4),
                tokens_out: Some(2),
            }),
        ));
        crate::state::seed_seat_model_for_test(&state, &id, crate::node::Seat::Orchestrator, base);

        let ev = agent_turn_inner(&state, &turn_req(&state, &id, "hi", Some("   "), None)).unwrap();
        let p: serde_json::Value = serde_json::from_str(&ev.payload).unwrap();
        assert_eq!(
            p["summary"], "base reply",
            "a whitespace-only model_id must be treated as no override"
        );

        let stream = state.heart.lock().unwrap().read_stream(&id).unwrap();
        let usage_ev = stream
            .iter()
            .find(|e| e.family == "agent.turn.usage")
            .unwrap();
        let up: serde_json::Value = serde_json::from_str(&usage_ev.payload).unwrap();
        assert_eq!(
            up["model_id"], "usage-double",
            "the usage event's model_id must fall back to the real model that ran, not the blank override string"
        );
        assert!(
            up["context_window"].is_null(),
            "a blank override must never be treated as a real cache-lookup key"
        );
    }

    /// Review fix (test-coverage gap): `provider_for` had no direct test at
    /// all — the only test reaching it used a model double whose name only
    /// ever hit the fallback arm, so a mistyped/swapped/deleted arm (or a
    /// broken `Transport::Acp` check) would not have been caught. Also
    /// covers the mislabeling bug itself: plug ids have no enforced naming
    /// convention (`NodeConfig::validate_new_plug_id` never requires an
    /// `"acp"`/`"local:"` prefix), so classification must key off
    /// `Transport`, never off a name-prefix guess.
    #[test]
    fn provider_for_uses_transport_not_name_sniffing_for_acp_vs_openai_compat() {
        use bakery_agent::Transport;
        assert_eq!(provider_for("claude", Transport::Http), "claude");
        assert_eq!(provider_for("grok", Transport::Http), "grok");
        assert_eq!(provider_for("gemini", Transport::Http), "gemini");
        assert_eq!(provider_for("claude-code", Transport::Http), "claude-code");
        // An ACP plug whose operator-chosen id has no "acp" prefix must
        // still be bucketed as "acp" — transport, not name, decides.
        assert_eq!(provider_for("codex", Transport::Acp), "acp");
        // An openai-compat plug whose operator-chosen id happens to start
        // with "acp" must NOT be mislabeled as the ACP bucket.
        assert_eq!(provider_for("acp-relay", Transport::Http), "openai-compat");
    }

    // ─── 07d-B Stage B: declared Bakery operator tools ────────────────────

    /// Runs one HTTP tool-loop turn on `stream_id` whose ScriptedModel calls
    /// `project.list` (a Bakery sense op), then returns whether that
    /// `agent.tool.called` event succeeded. Seeds both the baker and
    /// orchestrator seats so this works regardless of which one the stream
    /// defaults to.
    fn baker_op_call_succeeds(state: &AppState, stream_id: &str) -> bool {
        let model = Arc::new(ScriptedModel::new(&[
            r#"{"tool":"project_list","args":{}}"#,
            r#"{"done":{"summary":"ok"}}"#,
        ]));
        crate::state::seed_seat_model_for_test(
            state,
            stream_id,
            crate::node::Seat::Baker,
            model.clone(),
        );
        crate::state::seed_seat_model_for_test(
            state,
            stream_id,
            crate::node::Seat::Orchestrator,
            model,
        );
        agent_turn_inner(
            state,
            &AgentTurnReq {
                model_id: None,
                effort: None,
                actor: None,
                stream_id: stream_id.to_string(),
                file_path: None,
                instruction: "list projects".into(),
                seat: None,
            },
        )
        .expect("the turn itself must still complete even when the op is refused");
        let events = state.heart.lock().unwrap().read_stream(stream_id).unwrap();
        let called = events
            .iter()
            .find(|e| e.family == "agent.tool.called")
            .expect("agent.tool.called must be appended either way");
        let p: serde_json::Value = serde_json::from_str(&called.payload).unwrap();
        p["ok"].as_bool().unwrap()
    }

    /// 07d-B Phase 2 item 2: only the transport that can actually dispatch a
    /// Bakery op (`agent_tool_turn`'s native HTTP tool loop) may tell the
    /// model the op registry is callable. Every other transport (ACP, the
    /// direct single-file-edit path) must get the honest "declared but not
    /// callable here" variant instead — never the plain registry, which
    /// would instruct a model to call tools it has no way to invoke.
    #[test]
    fn baker_ops_for_turn_only_advertises_dispatchable_ops_as_callable() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        state.node.lock().unwrap().baker_stream_id = Some("socket_baker".into());

        let dispatchable = baker_ops_for_turn(&state, "baker-stream", "baker/main", true, true);
        assert!(
            dispatchable.contains("You act through declared operations"),
            "{dispatchable}"
        );
        assert!(dispatchable.contains("task_create"), "{dispatchable}");

        let not_dispatchable = baker_ops_for_turn(&state, "baker-stream", "baker/main", true, false);
        assert!(
            not_dispatchable
                .to_ascii_lowercase()
                .contains("not callable"),
            "{not_dispatchable}"
        );
        assert!(
            not_dispatchable
                .to_ascii_lowercase()
                .contains("do not attempt"),
            "{not_dispatchable}"
        );
        assert!(
            not_dispatchable.contains("task_create"),
            "still names the ops: {not_dispatchable}"
        );
        assert!(
            !not_dispatchable.contains("You act through declared operations"),
            "must not read as if these ops are callable: {not_dispatchable}"
        );

        // A branch-job socket (not the permanent Baker) gets neither block,
        // regardless of `ops_dispatchable`.
        let not_baker = baker_ops_for_turn(&state, "baker-stream", "baker/main", false, true);
        assert!(!not_baker.contains("task_create"), "{not_baker}");
    }

    /// 07d-B Phase 2 item 2: authorization is the permanent Baker identity —
    /// `NodeConfig.baker_stream_id` — never `is_baker_stream_ctx`'s kind bit
    /// alone. A compiled-in Bakery op dispatches on the permanent Baker
    /// stream, and is refused (same "unknown tool" shape as any unregistered
    /// name) on an old `/baker/assign` branch-job socket — also
    /// `SocketKind::Factory`, but not the permanent Baker — and on an
    /// ordinary project socket.
    #[test]
    fn baker_ops_dispatch_only_on_the_permanent_baker_stream() {
        let dir = tempfile::tempdir().unwrap();
        let repo = bakery_repo_for_test(dir.path());
        let state = state_with_bakery_root(dir.path(), &repo);

        let baker_home =
            crate::routes::baker::ensure_permanent_baker(&state, None, None).expect("bootstrap");
        assert!(
            baker_op_call_succeeds(&state, &baker_home.stream_id),
            "the permanent Baker must be able to call a declared op"
        );

        let branch_job = crate::routes::baker::baker_assign_inner(&state, "old job", None, None)
            .expect("assign");
        assert!(
            !baker_op_call_succeeds(&state, &branch_job.context.stream_id),
            "an old /baker/assign branch-job socket (factory-kind, not the permanent Baker) must NOT get Bakery ops"
        );

        let repo2 = make_repo(&dir.path().join("other-repo"));
        let id_proj = open_stream(&state, &open_req(&repo2, "ordinary-project"))
            .unwrap()
            .context
            .stream_id;
        assert!(
            !baker_op_call_succeeds(&state, &id_proj),
            "an ordinary project socket must NOT get Bakery ops"
        );
    }

    /// Checkpoint-1-shaped e2e: a scripted-model turn drives the permanent
    /// Baker calling `task.create` end to end through the real HTTP tool
    /// loop, and the resulting `task.created` domain event attributes the
    /// calling agent.
    #[test]
    fn a_scripted_baker_turn_calls_task_create_end_to_end_through_the_tool_loop() {
        let dir = tempfile::tempdir().unwrap();
        let repo = bakery_repo_for_test(dir.path());
        let state = state_with_bakery_root(dir.path(), &repo);
        let baker_home =
            crate::routes::baker::ensure_permanent_baker(&state, None, None).expect("bootstrap");
        // Creating a task asks the operator in every mode: they allow it.
        crate::agent_control::set_operating_mode(&state, &baker_home.stream_id, crate::node::Seat::Baker, OperatingMode::Auto).unwrap();

        let project_id = crate::catalog::create_project(&state, "Target Project", None)
            .unwrap()
            .0
            .project_id;

        let model = Arc::new(ScriptedModel::new(&[
            &format!(
                r#"{{"tool":"task_create","args":{{"parent_project_id":"{project_id}","title":"Fix workstream A","work_order":"wo-a6"}}}}"#
            ),
            r#"{"done":{"summary":"created the task"}}"#,
        ]));
        crate::state::seed_seat_model_for_test(
            &state,
            &baker_home.stream_id,
            crate::node::Seat::Baker,
            model,
        );

        allowing(&state, &baker_home.stream_id, "task_create", || {
            agent_turn_inner(
                &state,
                &AgentTurnReq {
                    model_id: None,
                    effort: None,
                    actor: None,
                    stream_id: baker_home.stream_id.clone(),
                    file_path: None,
                    instruction: "Decompose the spec into one task.".into(),
                    seat: None,
                },
            )
        })
        .expect("Baker's task.create turn must complete");

        let baker_events = state
            .heart
            .lock()
            .unwrap()
            .read_stream(&baker_home.stream_id)
            .unwrap();
        let called = baker_events
            .iter()
            .find(|e| e.family == "agent.tool.called")
            .expect("agent.tool.called for task.create must be appended");
        assert_eq!(called.actor, "agent:baker");
        let p: serde_json::Value = serde_json::from_str(&called.payload).unwrap();
        assert_eq!(p["tool"], "task_create");
        assert!(
            p["ok"].as_bool().unwrap(),
            "task_create op must succeed: {p}"
        );

        let tasks = crate::tasks::task_list(&state, &project_id);
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].title, "Fix workstream A");

        let project_events = state
            .heart
            .lock()
            .unwrap()
            .read_stream(&project_id)
            .unwrap();
        let created_ev = project_events
            .iter()
            .find(|e| e.family == "task.created")
            .unwrap();
        assert_eq!(
            created_ev.actor, crate::actor::agent(&baker_home.stream_id, crate::node::Seat::Baker),
            "the op-triggered task.created event must attribute the calling agent, not the human route's default"
        );
    }

    /// BAKER-THREADS-03: a Baker thread is the Baker in another conversation
    /// — its turns dispatch the same op registry as the main one — and a
    /// thread that loses membership (its kind re-recorded as project) loses
    /// the ops with it.
    #[test]
    fn baker_ops_dispatch_on_a_baker_thread_and_stop_when_membership_is_lost() {
        let dir = tempfile::tempdir().unwrap();
        let repo = bakery_repo_for_test(dir.path());
        let state = state_with_bakery_root(dir.path(), &repo);
        crate::routes::baker::ensure_permanent_baker(&state, None, None).expect("bootstrap");
        let new_thread = || {
            crate::baker_threads::create_thread(&state, &Default::default(), "human", None, None)
                .expect("thread")
                .thread
                .stream_id
        };

        let thread = new_thread();
        assert!(
            baker_op_call_succeeds(&state, &thread),
            "a Baker thread must be able to call a declared op"
        );

        let lost = new_thread();
        append(
            &state,
            &lost,
            "human",
            crate::node::SOCKET_KIND_FAMILY,
            serde_json::to_string(&crate::node::SocketKindRecord {
                kind: crate::node::SocketKind::Project,
            })
            .unwrap(),
            vec![],
        )
        .unwrap();
        assert!(
            !baker_op_call_succeeds(&state, &lost),
            "a socket that is no longer a Baker thread must NOT get Bakery ops"
        );
    }

    /// The natural-language control path end to end through the real HTTP
    /// tool loop: a thread's turn opens a parallel thread (its first message
    /// queued on the NEW thread's own durable queue, attributed to the
    /// Baker), and is refused when it tries to queue a message onto itself —
    /// the unbounded self-feeding loop `dispatch_baker_op_as` guards against.
    #[test]
    fn a_scripted_thread_turn_opens_a_parallel_thread_and_cannot_message_itself() {
        let dir = tempfile::tempdir().unwrap();
        let repo = bakery_repo_for_test(dir.path());
        let state = state_with_bakery_root(dir.path(), &repo);
        crate::routes::baker::ensure_permanent_baker(&state, None, None).expect("bootstrap");
        let thread =
            crate::baker_threads::create_thread(&state, &Default::default(), "human", None, None)
                .expect("thread")
                .thread
                .stream_id;
        // These ops act; Auto runs them without an approval card, but opening
        // a thread asks the operator in every mode: they allow it.
        crate::agent_control::set_operating_mode(&state, &thread, crate::node::Seat::Baker, OperatingMode::Auto).unwrap();

        let model = Arc::new(ScriptedModel::new(&[
            r#"{"tool":"thread_create","args":{"title":"Audit scopes","instruction":"Audit the MCP scopes"}}"#,
            &format!(
                r#"{{"tool":"thread_message","args":{{"stream_id":"{thread}","text":"keep going forever"}}}}"#
            ),
            r#"{"done":{"summary":"fanned out"}}"#,
        ]));
        crate::state::seed_seat_model_for_test(&state, &thread, crate::node::Seat::Baker, model);
        allowing(&state, &thread, "thread_create", || {
            agent_turn_inner(
                &state,
                &AgentTurnReq {
                    model_id: None,
                    effort: None,
                    actor: None,
                    stream_id: thread.clone(),
                    file_path: None,
                    instruction: "Fan the audit out into its own thread.".into(),
                    seat: None,
                },
            )
        })
        .expect("the thread's turn must complete");

        let events = state.heart.lock().unwrap().read_stream(&thread).unwrap();
        let calls: Vec<serde_json::Value> = events
            .iter()
            .filter(|e| e.family == "agent.tool.called")
            .map(|e| serde_json::from_str(&e.payload).unwrap())
            .collect();
        assert_eq!(calls.len(), 2, "{calls:?}");
        assert_eq!(calls[0]["tool"], "thread_create");
        assert!(calls[0]["ok"].as_bool().unwrap(), "{}", calls[0]);
        assert_eq!(calls[1]["tool"], "thread_message");
        assert!(
            !calls[1]["ok"].as_bool().unwrap(),
            "self-message must be refused: {}",
            calls[1]
        );
        assert!(
            calls[1]["summary"]
                .as_str()
                .unwrap()
                .contains("this conversation"),
            "{}",
            calls[1]
        );

        let spawned = crate::baker_threads::list_threads(&state, false)
            .unwrap()
            .into_iter()
            .find(|t| t.title == "Audit scopes")
            .expect("the thread the Baker opened is listed");
        let spawned_events = state
            .heart
            .lock()
            .unwrap()
            .read_stream(&spawned.stream_id)
            .unwrap();
        let queued = spawned_events
            .iter()
            .find(|e| e.family == crate::agent_control::FOLLOW_UP_QUEUED)
            .expect("its first message is queued on its own durable queue");
        assert_eq!(
            queued.actor, crate::actor::agent(&thread, crate::node::Seat::Baker),
            "a Baker-issued brief is never the human's words"
        );
    }

    /// 07d-B Phase 2 item 3: the HTTP tool loop's unconditional `Jail::new`
    /// used to fail any rootless socket at `TurnPhase::Jail` before the
    /// model was ever contacted. A rootless permanent Baker (no
    /// `bakery_source_root` known) must instead degrade honestly to
    /// ops+chat: the turn completes, and its system prompt neither claims
    /// nor advertises file/exec tools it cannot run.
    #[test]
    fn rootless_permanent_baker_turn_completes_with_no_jail_error_and_no_file_tools_advertised() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let baker_home =
            crate::routes::baker::ensure_permanent_baker(&state, None, None).expect("bootstrap");
        assert!(
            baker_home.root.is_empty(),
            "sanity: no bakery_source_root known — this Baker must be rootless"
        );

        let model = Arc::new(ScriptedModel::new(&[
            r#"{"done":{"summary":"chatted fine, no jail error"}}"#,
        ]));
        crate::state::seed_seat_model_for_test(
            &state,
            &baker_home.stream_id,
            crate::node::Seat::Baker,
            model.clone(),
        );

        let ev = agent_turn_inner(
            &state,
            &AgentTurnReq {
                model_id: None,
                effort: None,
                actor: None,
                stream_id: baker_home.stream_id.clone(),
                file_path: None,
                instruction: "hi".into(),
                seat: None,
            },
        )
        .expect("a rootless Baker turn must not fail at TurnPhase::Jail");
        assert_eq!(ev.family, "agent.turn.done");

        let system_prompt = model.captured.lock().unwrap()[0].0.clone();
        assert!(
            system_prompt.contains("no workspace root"),
            "the prompt must be honest about having no file tools: {system_prompt}"
        );
        for jailed_usage in [
            "\"tool\":\"read_file\"",
            "\"tool\":\"write_file\"",
            "\"tool\":\"edit\"",
            "\"tool\":\"grep\"",
            "\"tool\":\"list_dir\"",
            "\"tool\":\"glob\"",
            "\"tool\":\"run\"",
        ] {
            assert!(
                !system_prompt.contains(jailed_usage),
                "{jailed_usage} must not be advertised on a rootless turn:\n{system_prompt}"
            );
        }
        // The declared op registry is still there — ops, not file tools.
        assert!(system_prompt.contains("declared operations"));
    }

    /// 07d-B Stage B item 4: a task socket's turn is advertised
    /// `task_complete` but never a Baker op; the tool loop refuses a
    /// Baker-only op call there exactly like any unregistered name, while
    /// `task_complete` succeeds and leaves a `claimed_done` marker for the
    /// Stage D (Phase 4) driver to consume.
    #[test]
    fn a_task_socket_gets_task_complete_but_never_baker_ops() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let project_id = crate::catalog::create_project(&state, "P", None)
            .unwrap()
            .0
            .project_id;
        let created = crate::tasks::task_create(
            &state,
            crate::tasks::TaskCreateReq {
                parent_project_id: project_id.clone(),
                title: "Research".into(),
                work_order: "wo-x".into(),
                contract: Default::default(),
            },
        )
        .unwrap();
        crate::tasks::task_start(
            &state,
            crate::tasks::TaskStartReq {
                project_id: project_id.clone(),
                task_id: created.task.task_id.clone(),
            },
        )
        .unwrap();
        let task_stream = created.task.stream_id.clone();

        let model = Arc::new(ScriptedModel::new(&[
            r#"{"tool":"project_list","args":{}}"#,
            r#"{"tool":"task_complete","args":{"summary":"done researching"}}"#,
            r#"{"done":{"summary":"wrapped up"}}"#,
        ]));
        crate::state::seed_seat_model_for_test(
            &state,
            &task_stream,
            crate::node::Seat::Orchestrator,
            model,
        );

        agent_turn_inner(
            &state,
            &AgentTurnReq {
                model_id: None,
                effort: None,
                actor: None,
                stream_id: task_stream.clone(),
                file_path: None,
                instruction: "go".into(),
                seat: None,
            },
        )
        .expect("a task socket's turn must complete");

        let events = state
            .heart
            .lock()
            .unwrap()
            .read_stream(&task_stream)
            .unwrap();
        let calls: Vec<_> = events
            .iter()
            .filter(|e| e.family == "agent.tool.called")
            .collect();
        assert_eq!(calls.len(), 2, "both tool calls must be recorded");
        let first: serde_json::Value = serde_json::from_str(&calls[0].payload).unwrap();
        assert!(
            !first["ok"].as_bool().unwrap(),
            "a task socket must never get Baker ops: {first}"
        );
        let second: serde_json::Value = serde_json::from_str(&calls[1].payload).unwrap();
        assert!(
            second["ok"].as_bool().unwrap(),
            "task_complete must succeed on a task socket: {second}"
        );

        let project_events = state
            .heart
            .lock()
            .unwrap()
            .read_stream(&project_id)
            .unwrap();
        let status_ev = project_events
            .iter()
            .rev()
            .find(|e| e.family == "task.status.changed")
            .expect("task_complete must append task.status.changed");
        let p: serde_json::Value = serde_json::from_str(&status_ev.payload).unwrap();
        assert!(
            p["reason"]
                .as_str()
                .unwrap_or("")
                .starts_with("claimed_done"),
            "task_complete must leave a claimed_done marker for the Phase 4 driver: {p}"
        );
    }

    /// Regression: `task_complete` used to append `task.status.changed{status:
    /// Running}` with no check of the task's current status —
    /// `tasks_for_project_locked`'s latest-status-wins fold meant a worker's
    /// stray/late `task_complete` call always resurrected a task the human
    /// already cancelled. Cancel it first, then run `task_complete` on the
    /// same socket, and prove the task stays `cancelled`.
    #[test]
    fn task_complete_refuses_to_resurrect_a_task_the_human_already_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let project_id = crate::catalog::create_project(&state, "P", None)
            .unwrap()
            .0
            .project_id;
        let created = crate::tasks::task_create(
            &state,
            crate::tasks::TaskCreateReq {
                parent_project_id: project_id.clone(),
                title: "Research".into(),
                work_order: "wo-cancel-race".into(),
                contract: Default::default(),
            },
        )
        .unwrap();
        crate::tasks::task_start(
            &state,
            crate::tasks::TaskStartReq {
                project_id: project_id.clone(),
                task_id: created.task.task_id.clone(),
            },
        )
        .unwrap();
        let task_stream = created.task.stream_id.clone();

        let cancelled = crate::tasks::task_cancel(
            &state,
            crate::tasks::TaskCancelReq {
                project_id: project_id.clone(),
                task_id: created.task.task_id.clone(),
                reason: None,
            },
        )
        .unwrap();
        assert_eq!(cancelled.task.status, crate::tasks::TaskStatus::Cancelled);

        let model = Arc::new(ScriptedModel::new(&[
            r#"{"tool":"task_complete","args":{"summary":"i kept working anyway"}}"#,
            r#"{"done":{"summary":"wrapped up"}}"#,
        ]));
        crate::state::seed_seat_model_for_test(
            &state,
            &task_stream,
            crate::node::Seat::Orchestrator,
            model,
        );

        agent_turn_inner(
            &state,
            &AgentTurnReq {
                model_id: None,
                effort: None,
                actor: None,
                stream_id: task_stream.clone(),
                file_path: None,
                instruction: "go".into(),
                seat: None,
            },
        )
        .expect("the turn itself must still complete even though the tool call is refused");

        let events = state
            .heart
            .lock()
            .unwrap()
            .read_stream(&task_stream)
            .unwrap();
        let called = events
            .iter()
            .find(|e| e.family == "agent.tool.called")
            .unwrap();
        let p: serde_json::Value = serde_json::from_str(&called.payload).unwrap();
        assert!(
            !p["ok"].as_bool().unwrap(),
            "task_complete on a cancelled task must be refused: {p}"
        );

        let listed = crate::tasks::task_list(&state, &project_id);
        let row = listed
            .iter()
            .find(|t| t.task_id == created.task.task_id)
            .unwrap();
        assert_eq!(
            row.status,
            crate::tasks::TaskStatus::Cancelled,
            "a refused task_complete must never resurrect a cancelled task to running"
        );
    }

    /// The mirror case: the permanent Baker never gets `task_complete` — it
    /// is a worker-scoped tool advertised only on a task socket.
    #[test]
    fn the_permanent_baker_never_sees_task_complete() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let baker_home =
            crate::routes::baker::ensure_permanent_baker(&state, None, None).expect("bootstrap");

        let model = Arc::new(ScriptedModel::new(&[
            r#"{"tool":"task_complete","args":{"summary":"nope"}}"#,
            r#"{"done":{"summary":"ok"}}"#,
        ]));
        crate::state::seed_seat_model_for_test(
            &state,
            &baker_home.stream_id,
            crate::node::Seat::Baker,
            model,
        );

        agent_turn_inner(
            &state,
            &AgentTurnReq {
                model_id: None,
                effort: None,
                actor: None,
                stream_id: baker_home.stream_id.clone(),
                file_path: None,
                instruction: "hi".into(),
                seat: None,
            },
        )
        .expect("the turn must still complete");

        let events = state
            .heart
            .lock()
            .unwrap()
            .read_stream(&baker_home.stream_id)
            .unwrap();
        let called = events
            .iter()
            .find(|e| e.family == "agent.tool.called")
            .unwrap();
        let p: serde_json::Value = serde_json::from_str(&called.payload).unwrap();
        assert!(
            !p["ok"].as_bool().unwrap(),
            "the permanent Baker must never be able to call task_complete: {p}"
        );
    }
}
