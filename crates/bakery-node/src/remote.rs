//! Everything that touches a remote, in one place: what is waiting for the
//! operator across every conversation (its count goes in the app's top bar),
//! and a history line for each remote action done, on its conversation and
//! in one list for the whole Bakery.

use crate::state::AppState;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;

/// The whole Bakery's history of remote actions.
pub(crate) const STREAM: &str = "sys:remote";
/// One remote action done: `{what, repository, …, stream_id?}`.
pub(crate) const DONE: &str = "remote.done";
/// The Bakery tools that touch a remote, whose cards the list gathers.
const TOOLS: &[&str] = &["git_push", "git_pr", "github_issue_create", "github_issue_comment", "google_change"];

/// Records one remote action: on its conversation, when it has one, and in
/// the whole Bakery's list. `line` says what: `{what, repository, …}`.
pub(crate) fn done(state: &AppState, stream_id: Option<&str>, mut line: Value) {
    if let Some(stream_id) = stream_id {
        let _ = crate::state::append(state, stream_id, "human", DONE, line.to_string(), vec![]);
        line["stream_id"] = json!(stream_id);
    }
    let _ = crate::state::append(state, STREAM, "human", DONE, line.to_string(), vec![]);
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct Waiting {
    pub(crate) stream_id: String,
    /// The conversation's name.
    pub(crate) name: String,
    pub(crate) project_id: Option<String>,
    /// The card, as decided through /agent/pipe/gate/decide.
    pub(crate) card: String,
    /// git_push, git_pr, github_issue_create, github_issue_comment or
    /// google_change (a room's change to Google Cloud, held at its door).
    pub(crate) tool: String,
    pub(crate) args: Value,
    pub(crate) asked_at: String,
    /// What a push sends, as its card shows it, less the diff text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) push: Option<PushSummary>,
    /// The card's offer to let the conversation push that branch, or change
    /// that Google Cloud project, from now on, taken by deciding the card
    /// AllowAlways.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) grant: Option<GrantOffer>,
}

/// A push card's preview without its diff (`routes::git::PushPreview`).
#[derive(schemars::JsonSchema, Debug, Serialize, Deserialize)]
pub(crate) struct PushSummary {
    pub(crate) branch: String,
    /// The commit it sends.
    pub(crate) head: String,
    /// The remote's branch as last fetched; none when it is new there.
    pub(crate) remote_head: Option<String>,
    pub(crate) base: String,
    /// The commits it adds, newest first; at most 50.
    pub(crate) commits: Vec<crate::routes::git::PushCommit>,
    pub(crate) diff_truncated: bool,
    /// The diff shows a credential-shaped file.
    pub(crate) secret: bool,
}

/// A card's offer (`grants::offer`): this branch, or this Google Cloud
/// project (`repository`, with no branch), from now on.
#[derive(schemars::JsonSchema, Debug, Serialize, Deserialize)]
pub(crate) struct GrantOffer {
    /// push or google
    pub(crate) kind: String,
    pub(crate) repository: String,
    pub(crate) branch: String,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct WaitingList {
    pub(crate) count: usize,
    /// Newest first.
    pub(crate) items: Vec<Waiting>,
}

/// Every remote action's card still waiting for the operator, across all
/// conversations.
pub(crate) fn waiting(state: &AppState) -> WaitingList {
    let names: std::collections::HashMap<String, String> = crate::state::known_streams(state)
        .unwrap_or_default()
        .into_iter()
        .map(|s| (s.stream_id, s.title.unwrap_or(s.name)))
        .collect();
    let heart = state.heart.lock().unwrap();
    let mut items = Vec::new();
    for stream in heart.list_streams().unwrap_or_default() {
        let sid = stream.stream_id;
        let facts = |family| heart.events_of_family(&sid, family).unwrap_or_default();
        let closed: HashSet<String> = [crate::pipe::CHILD_TOOL_DECIDED_FAMILY, "agent.approval.cancelled"]
            .into_iter()
            .flat_map(facts)
            .flat_map(|e| e.causality)
            .collect();
        for card in facts(crate::pipe::CHILD_TOOL_PENDING_FAMILY) {
            let payload: Value = serde_json::from_str(&card.payload).unwrap_or_default();
            let tool = payload["bakery_tool"]["name"].as_str().unwrap_or_default();
            if !TOOLS.contains(&tool) || closed.contains(&card.event_hash) {
                continue;
            }
            items.push(Waiting {
                name: names.get(&sid).cloned().unwrap_or_default(),
                project_id: crate::state::socket_project(&heart, &sid).0,
                card: card.event_hash,
                tool: tool.to_string(),
                args: payload["bakery_tool"]["args"].clone(),
                asked_at: card.ts,
                push: serde_json::from_value(payload["push"].clone()).ok(),
                grant: serde_json::from_value(payload["grant"].clone()).ok(),
                stream_id: sid.clone(),
            });
        }
    }
    items.sort_by(|a, b| b.asked_at.cmp(&a.asked_at));
    WaitingList { count: items.len(), items }
}

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct HistoryQuery {
    /// At most this many, newest first: 100 by default, 1000 at most.
    #[serde(default)]
    pub(crate) limit: Option<usize>,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct RemoteAction {
    pub(crate) at: String,
    /// Who did it: "human" (the operator), or the agent whose call the
    /// operator approved or granted.
    pub(crate) by: String,
    /// What was done: `{what, repository, …, stream_id?}`.
    pub(crate) line: Value,
}

/// The whole Bakery's remote actions, newest first.
pub(crate) fn history(state: &AppState, q: &HistoryQuery) -> Vec<RemoteAction> {
    let limit = q.limit.unwrap_or(100).clamp(1, 1000);
    let events = state.heart.lock().unwrap().events_of_family(STREAM, DONE).unwrap_or_default();
    events
        .into_iter()
        .rev()
        .take(limit)
        .map(|e| RemoteAction { at: e.ts, by: e.actor, line: serde_json::from_str(&e.payload).unwrap_or_default() })
        .collect()
}

pub(crate) async fn waiting_route(State(st): State<AppState>) -> Result<Json<WaitingList>, (StatusCode, String)> {
    crate::actor::spawn_blocking(move || Json(waiting(&st)))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

pub(crate) async fn history_route(
    State(st): State<AppState>,
    Query(q): Query<HistoryQuery>,
) -> Result<Json<Vec<RemoteAction>>, (StatusCode, String)> {
    crate::actor::spawn_blocking(move || Json(history(&st, &q)))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_control::OperatingMode;
    use crate::api_tools::{call, Caller, WAITING};
    use crate::pipe::ChildToolGateDecisionKind;
    use crate::routes::project::open_stream;
    use crate::testutil::*;
    use bakery_agent::StubModel;
    use std::sync::Arc;

    #[test]
    fn the_list_holds_each_waiting_remote_card_and_history_each_push() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let local = crate::git::tests::behind_its_origin(dir.path());
        let ctx = open_stream(&state, &open_req(&local, "remote")).unwrap().context;
        let sid = ctx.stream_id.clone();
        state.node.lock().unwrap().baker_stream_id = Some(sid.clone());
        let wt = std::path::Path::new(&ctx.worktree_path);
        std::fs::write(wt.join("one.txt"), "x\n").unwrap();
        run_git(wt, &["add", "."]);
        run_git(wt, &["commit", "-qm", "one"]);
        let manual = Caller {
            stream_id: sid.clone(),
            seat: crate::node::Seat::Baker,
            turn_id: String::new(),
            mode: OperatingMode::Manual,
            wait: Some(std::time::Duration::from_millis(200)),
        };
        let push = json!({ "stream_id": sid });
        assert_eq!(call(&state, &manual, "git_push", &push).unwrap_err(), WAITING);
        let mode = json!({ "stream_id": sid, "seat": "orchestrator", "mode": "plan" });
        assert_eq!(call(&state, &manual, "agent_mode", &mode).unwrap_err(), WAITING, "a card, but not a remote one");

        let list = waiting(&state);
        assert_eq!(list.count, 1, "{list:?}");
        let card = &list.items[0];
        assert_eq!((card.tool.as_str(), card.stream_id.as_str(), card.name.as_str()), ("git_push", sid.as_str(), "remote"));
        let preview = card.push.as_ref().expect("what the push sends");
        assert_eq!((preview.branch.as_str(), preview.commits.len()), (ctx.branch.as_str(), 1));
        let listed = serde_json::to_value(&list).unwrap();
        assert!(listed["items"][0]["push"].get("diff").is_none(), "the diff stays on the card");
        let offer = card.grant.as_ref().expect("push from now on");
        assert_eq!((offer.kind.as_str(), offer.branch.as_str()), ("push", ctx.branch.as_str()));
        crate::pipe::decide_child_tool_gate(&state, &sid, &card.card, ChildToolGateDecisionKind::AllowOnce).unwrap();
        assert_eq!(waiting(&state).count, 0, "decided: no longer waiting");

        call(&state, &manual, "git_push", &push).unwrap();
        let head = crate::git::run(wt, &["rev-parse", "HEAD"]).unwrap().trim().to_string();
        let done = history(&state, &HistoryQuery { limit: None });
        assert_eq!(done.len(), 1, "{done:?}");
        assert_eq!((done[0].line["what"].as_str(), done[0].line["branch"].as_str()), (Some("push"), Some(ctx.branch.as_str())));
        assert_eq!((done[0].line["commit"].as_str(), done[0].line["stream_id"].as_str()), (Some(head.as_str()), Some(sid.as_str())));
        assert!(done[0].by.starts_with("agent:"), "the agent whose push the operator approved: {}", done[0].by);
        let on_its_thread = state.heart.lock().unwrap().events_of_family(&sid, DONE).unwrap().len();
        assert_eq!(on_its_thread, 1);
    }
}
