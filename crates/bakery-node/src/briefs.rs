//! A conversation's brief: standing instructions its agent gets at the start
//! of every turn, pinned under the thread's header. The Baker, or an
//! orchestrator, sets one for another conversation; the operator edits it,
//! and keeps or turns down what an agent set (its recommendation, in effect
//! until turned down); an agent never edits its own (the operator's words:
//! "an orc or baker can set the brief, which i can edit and add or turn down
//! recs, but an agent cant edit its own brief"). Every change is an event on
//! the conversation, so its history shows.

use crate::node::Seat;
use crate::state::AppState;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

/// A brief set: `{text, recommended}`, by its actor.
pub(crate) const SET: &str = "brief.set";
/// The operator kept, or turned down, an agent's brief: `{event_hash, keep}`.
pub(crate) const DECIDED: &str = "brief.decided";

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct BriefQuery {
    pub(crate) stream_id: String,
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct SetBriefReq {
    /// The conversation it is for: never the caller's own.
    pub(crate) stream_id: String,
    /// The whole brief. The operator may clear it with an empty one.
    pub(crate) text: String,
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct DecideBriefReq {
    pub(crate) stream_id: String,
    /// The agent's `brief.set`.
    pub(crate) event_hash: String,
    /// Keep it, or turn it down (the brief before it holds again).
    pub(crate) keep: bool,
}

#[derive(schemars::JsonSchema, Debug, Clone, Serialize)]
pub(crate) struct BriefChange {
    pub(crate) event_hash: String,
    pub(crate) text: String,
    /// "human" (the operator), or the agent that set it.
    pub(crate) by: String,
    pub(crate) at: String,
    /// An agent's: "kept", "turned down", or none yet.
    pub(crate) decision: Option<String>,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct Brief {
    pub(crate) stream_id: String,
    /// What its agent gets every turn: the newest brief not turned down;
    /// empty when there is none.
    pub(crate) text: String,
    /// The change that holds now, if any.
    pub(crate) current: Option<BriefChange>,
    /// An agent's brief the operator has neither kept nor turned down.
    pub(crate) pending: bool,
    /// Every change, newest first.
    pub(crate) history: Vec<BriefChange>,
}

pub(crate) fn brief(state: &AppState, stream_id: &str) -> Brief {
    let events = state.heart.lock().unwrap().read_stream(stream_id).unwrap_or_default();
    let mut history: Vec<BriefChange> = Vec::new();
    for e in &events {
        let Ok(p) = serde_json::from_str::<serde_json::Value>(&e.payload) else {
            continue;
        };
        match e.family.as_str() {
            SET => history.push(BriefChange {
                event_hash: e.event_hash.clone(),
                text: p["text"].as_str().unwrap_or_default().to_string(),
                by: e.actor.clone(),
                at: e.ts.clone(),
                decision: None,
            }),
            DECIDED => {
                let said = if p["keep"] == true { "kept" } else { "turned down" };
                if let Some(change) = history.iter_mut().find(|c| p["event_hash"] == c.event_hash.as_str()) {
                    change.decision = Some(said.to_string());
                }
            }
            _ => {}
        }
    }
    history.reverse();
    let current = history.iter().find(|c| c.decision.as_deref() != Some("turned down")).cloned();
    let pending = current.as_ref().is_some_and(|c| c.by != "human" && c.decision.is_none());
    Brief {
        stream_id: stream_id.to_string(),
        text: current.as_ref().map(|c| c.text.clone()).unwrap_or_default(),
        current,
        pending,
        history,
    }
}

/// The calling agent's own conversation and seat (`agent:<stream>:<seat>`).
fn caller(actor: &str) -> Option<(String, Seat)> {
    let (stream, seat) = actor.strip_prefix("agent:")?.rsplit_once(':')?;
    Some((stream.to_string(), serde_json::from_value(json!(seat)).ok()?))
}

/// Sets a conversation's brief: the operator's holds as theirs; an agent's
/// (the Baker's, or an orchestrator's in the same project) is a
/// recommendation, in effect until the operator turns it down.
pub(crate) fn set(state: &AppState, req: &SetBriefReq) -> Result<Brief, String> {
    crate::state::context_for(state, &req.stream_id).map_err(|_| format!("no conversation {}", req.stream_id))?;
    let actor = crate::actor::attribute("human");
    let text = req.text.trim();
    let recommended = actor != "human";
    if recommended {
        let Some((from, seat)) = caller(&actor) else {
            return Err("only the Baker or an orchestrator sets another conversation's brief".to_string());
        };
        if from == req.stream_id {
            return Err("a conversation's agent never edits its own brief: the operator, the Baker or an orchestrator does".to_string());
        }
        let baker_side = crate::baker_ops::is_baker_scope_stream(state, &from);
        let peers = {
            let heart = state.heart.lock().unwrap();
            let project = |s: &str| crate::state::socket_project(&heart, s).0;
            project(&from).is_some() && project(&from) == project(&req.stream_id)
        };
        if !baker_side && !(peers && seat == Seat::Orchestrator) {
            return Err("only the Baker, or an orchestrator in the same project, sets another conversation's brief".to_string());
        }
        if text.is_empty() {
            return Err("a brief needs text; only the operator clears one".to_string());
        }
    }
    let said = json!({"text": text, "recommended": recommended});
    crate::state::append(state, &req.stream_id, &actor, SET, said.to_string(), vec![])?;
    Ok(brief(state, &req.stream_id))
}

/// The operator keeps, or turns down, an agent's brief.
pub(crate) fn decide(state: &AppState, req: &DecideBriefReq) -> Result<Brief, String> {
    let now = brief(state, &req.stream_id);
    let change = now
        .history
        .iter()
        .find(|c| c.event_hash == req.event_hash)
        .ok_or_else(|| format!("no brief {} on {}", req.event_hash, req.stream_id))?;
    if change.by == "human" {
        return Err("that brief is the operator's own".to_string());
    }
    if change.decision.is_some() {
        return Err("that brief was already decided".to_string());
    }
    let said = json!({"event_hash": req.event_hash, "keep": req.keep});
    crate::state::append(state, &req.stream_id, "human", DECIDED, said.to_string(), vec![req.event_hash.clone()])?;
    Ok(brief(state, &req.stream_id))
}

/// The brief as a turn opens with it, when there is one.
pub(crate) fn prompt_line(state: &AppState, stream_id: &str) -> Option<String> {
    let text = brief(state, stream_id).text;
    (!text.is_empty()).then(|| format!("[This conversation's brief: standing instructions for every turn]\n{text}"))
}

async fn blocking<T: Send + 'static>(work: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<Json<T>, (StatusCode, String)> {
    crate::actor::spawn_blocking(work)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))
}

pub(crate) async fn brief_route(State(st): State<AppState>, Query(q): Query<BriefQuery>) -> Result<Json<Brief>, (StatusCode, String)> {
    blocking(move || Ok(brief(&st, &q.stream_id))).await
}

pub(crate) async fn set_route(State(st): State<AppState>, Json(req): Json<SetBriefReq>) -> Result<Json<Brief>, (StatusCode, String)> {
    blocking(move || set(&st, &req)).await
}

pub(crate) async fn decide_route(State(st): State<AppState>, Json(req): Json<DecideBriefReq>) -> Result<Json<Brief>, (StatusCode, String)> {
    if crate::actor::agent_calling() {
        return Err((StatusCode::FORBIDDEN, "only the operator keeps or turns down a brief".to_string()));
    }
    blocking(move || decide(&st, &req)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;
    use std::sync::Arc;

    /// An orchestrator in the project sets a peer's brief, which holds as a
    /// recommendation until the operator turns it down; the operator's own
    /// edit holds; an agent never sets its own, nor one outside its project.
    #[test]
    fn a_brief_is_set_by_an_orchestrator_and_decided_by_the_operator() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(bakery_agent::StubModel));
        let app = dir.path().join("app");
        std::fs::create_dir_all(&app).unwrap();
        let root = || crate::catalog::DirectoryRootSpec::Attach(app.to_str().unwrap());
        let (project, orc) = crate::catalog::create_project(&state, "app", Some(root())).unwrap();
        let chef = crate::catalog::attach_directory_root(&state, &project.project_id, "Chef", root()).unwrap().stream_id;
        let other = dir.path().join("other");
        std::fs::create_dir_all(&other).unwrap();
        let elsewhere = crate::catalog::DirectoryRootSpec::Attach(other.to_str().unwrap());
        let (_, outside) = crate::catalog::create_project(&state, "other", Some(elsewhere)).unwrap();
        let set_as = |who: Option<&str>, stream: &str, text: &str| {
            let req = SetBriefReq { stream_id: stream.to_string(), text: text.to_string() };
            match who {
                Some(from) => crate::actor::with(crate::actor::agent(from, Seat::Orchestrator), || set(&state, &req)),
                None => set(&state, &req),
            }
        };

        assert!(set_as(Some(&chef), &chef, "do as I like").unwrap_err().contains("never edits its own"));
        assert!(set_as(Some(&outside.stream_id), &chef, "hi").unwrap_err().contains("same project"));
        set_as(None, &chef, "Own the sidebar.").unwrap();
        let recommended = set_as(Some(&orc.stream_id), &chef, "Own the sidebar; never touch the API.").unwrap();
        assert!(recommended.pending && recommended.text.contains("never touch the API"));
        let line = prompt_line(&state, &chef).unwrap();
        assert!(line.contains("never touch the API"), "in effect until turned down: {line}");

        let hash = recommended.current.unwrap().event_hash;
        let turned_down = decide(&state, &DecideBriefReq { stream_id: chef.clone(), event_hash: hash.clone(), keep: false }).unwrap();
        assert_eq!(turned_down.text, "Own the sidebar.", "the operator's own holds again");
        assert!(!turned_down.pending);
        assert!(decide(&state, &DecideBriefReq { stream_id: chef.clone(), event_hash: hash, keep: true }).is_err());
        assert_eq!(turned_down.history.len(), 2);
        assert_eq!(turned_down.history[0].decision.as_deref(), Some("turned down"));
    }

    /// The brief opens the conversation's turns, whatever the runtime.
    #[test]
    fn every_turn_opens_with_the_brief() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let model = Arc::new(ScriptedModel::new(&["done"]));
        let state = state_with(dir.path(), model.clone());
        let sid = crate::routes::project::open_stream(&state, &open_req(&repo, "briefed")).unwrap().context.stream_id;
        set(&state, &SetBriefReq { stream_id: sid.clone(), text: "Answer in one line.".into() }).unwrap();
        let req = crate::routes::agent::AgentTurnReq {
            stream_id: sid,
            file_path: None,
            instruction: "what changed?".into(),
            seat: None,
            model_id: None,
            effort: None,
            actor: None,
        };
        crate::routes::agent::agent_turn_inner(&state, &req).unwrap();
        let opening = model.captured.lock().unwrap()[0].1.clone();
        assert!(opening.contains("brief: standing instructions") && opening.contains("Answer in one line."), "{opening}");
    }
}
