//! HTTP surface for a pipe plug's declared child-tool gate (06b-PIPE.md):
//! The operator's allow-once / allow-always / deny on a pending `agent.child_tool.pending`
//! Heart event.

use crate::pipe::{decide_child_tool_gate, read_full_tool_payload, ChildToolGateDecisionKind};
use crate::state::{context_for, AppState};
use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use bakery_core::Event;
use serde::Deserialize;

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct ToolPayloadQuery {
    pub(crate) stream_id: String,
    pub(crate) sha: String,
}

/// `GET /agent/tool-payload` — the complete native tool payload behind one
/// `agent.acp.tool_call` event's `full` reference. Heart keeps only the
/// bounded copy; this is the operator's "show every byte" on demand.
pub(crate) async fn tool_payload(
    State(st): State<AppState>,
    Query(q): Query<ToolPayloadQuery>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    context_for(&st, &q.stream_id).map_err(|e| (StatusCode::NOT_FOUND, e))?;
    let body =
        crate::actor::spawn_blocking(move || read_full_tool_payload(&st, &q.stream_id, &q.sha))
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
            .map_err(|e| (StatusCode::NOT_FOUND, e))?;
    Ok((
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        body,
    ))
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct ChildToolGateDecideReq {
    pub(crate) stream_id: String,
    pub(crate) event_hash: String,
    pub(crate) decision: ChildToolGateDecisionKind,
}

pub(crate) async fn decide_child_tool(
    State(st): State<AppState>,
    Json(req): Json<ChildToolGateDecideReq>,
) -> Result<Json<Event>, (StatusCode, String)> {
    context_for(&st, &req.stream_id).map_err(|e| (StatusCode::NOT_FOUND, e))?;
    decide_child_tool_gate(&st, &req.stream_id, &req.event_hash, req.decision)
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::Seat;
    use crate::pipe::{admit_child_tool_call, ChildToolDecision, ChildToolKind, ChildToolRequest};
    use crate::routes::project::open_stream;
    use crate::testutil::*;
    use bakery_agent::StubModel;
    use std::sync::Arc;

    #[tokio::test]
    async fn decide_child_tool_refuses_an_unknown_stream() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let err = decide_child_tool(
            State(state),
            Json(ChildToolGateDecideReq {
                stream_id: "wt:nope".into(),
                event_hash: "whatever".into(),
                decision: ChildToolGateDecisionKind::Deny,
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn decide_child_tool_allow_always_round_trips_over_http() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = open_stream(&state, &open_req(&repo, "gatehttp"))
            .unwrap()
            .context
            .stream_id;
        // Gates are raised by a live turn; without one this gate is expired.
        let agent_id = crate::node::agent_id(&stream_id, Seat::Worker);
        crate::state::start_agent_activity(&state, &stream_id, &agent_id, "t1");

        assert!(matches!(
            admit_child_tool_call(
                &state,
                &stream_id,
                Seat::Worker,
                "t1",
                &ChildToolRequest {
                    plug: "test".to_string(),
                    kind: ChildToolKind::Execute,
                    title: "run something".into(),
                    diffs: Vec::new(),
                },
            ),
            ChildToolDecision::Deny
        ));
        let events = state.heart.lock().unwrap().read_stream(&stream_id).unwrap();
        let pending = events
            .iter()
            .find(|e| e.family == crate::pipe::CHILD_TOOL_PENDING_FAMILY)
            .unwrap();

        let decided = decide_child_tool(
            State(state.clone()),
            Json(ChildToolGateDecideReq {
                stream_id: stream_id.clone(),
                event_hash: pending.event_hash.clone(),
                decision: ChildToolGateDecisionKind::AllowAlways,
            }),
        )
        .await
        .unwrap();
        assert_eq!(decided.0.family, crate::pipe::CHILD_TOOL_DECIDED_FAMILY);
        let grants = state.node.lock().unwrap().always_grants.clone();
        assert!(grants.iter().any(|g| g.kind == ChildToolKind::Execute), "{grants:?}");
    }
}
