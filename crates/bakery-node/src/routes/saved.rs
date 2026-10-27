//! Saved replies: the operator keeps an agent's reply to come back to, and
//! ticks its checklist off as they go. Theirs alone. Kept in a file of the
//! Bakery's own (`saved.json`), never in any conversation's stream, so no
//! saved reply reaches an agent's context, a transcript, a compaction or a
//! summary; and every route refuses an agent, the Baker included. Each
//! change goes out live as `saved.updated` `{id, item}` (`item` null: gone),
//! so the Mac and the phone stay in step.

use crate::state::AppState;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use std::sync::{Mutex, OnceLock};

#[derive(schemars::JsonSchema, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct SavedReply {
    pub(crate) id: String,
    pub(crate) stream_id: String,
    /// The reply's `agent.turn.done` event.
    pub(crate) event_hash: String,
    /// The reply as it was when saved: its summary, what its Copy copies.
    pub(crate) text: String,
    pub(crate) saved_at: String,
    /// The checklist items ticked so far, numbered from 0 in the text's
    /// order (see [`checklist`]).
    pub(crate) done: Vec<usize>,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct SavedList {
    /// Newest first.
    pub(crate) items: Vec<SavedReply>,
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct SavedQuery {
    /// One conversation's; every conversation's when absent.
    #[serde(default)]
    pub(crate) stream_id: Option<String>,
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct SaveReq {
    pub(crate) stream_id: String,
    /// The reply's `agent.turn.done` event.
    pub(crate) event_hash: String,
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct SavedId {
    pub(crate) id: String,
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct TickReq {
    pub(crate) id: String,
    /// The checklist item, numbered from 0.
    pub(crate) item: usize,
    pub(crate) done: bool,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct Unsaved {
    pub(crate) id: String,
}

/// A checklist line, `- [ ] `, `* [x] ` or `+ [X] `, indented or not:
/// whether it is written ticked.
fn check_line(line: &str) -> Option<bool> {
    let rest = line.trim_start().strip_prefix(['-', '*', '+'])?.strip_prefix(" [")?;
    let mut chars = rest.chars();
    let mark = chars.next()?;
    if !chars.as_str().starts_with("] ") {
        return None;
    }
    match mark {
        ' ' => Some(false),
        'x' | 'X' => Some(true),
        _ => None,
    }
}

/// A reply's checklist, in order: whether each item is written ticked.
/// Lines inside ``` fences are code, not items. The app numbers them the
/// same way.
pub(crate) fn checklist(text: &str) -> Vec<bool> {
    let mut fenced = false;
    let mut items = Vec::new();
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if !fenced {
            items.extend(check_line(line));
        }
    }
    items
}

/// One change to the file at a time.
fn file_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(Default::default)
}

fn file(state: &AppState) -> std::path::PathBuf {
    state.base.join("saved.json")
}

fn load(state: &AppState) -> Result<Vec<SavedReply>, String> {
    match std::fs::read_to_string(file(state)) {
        Ok(text) => serde_json::from_str(&text).map_err(|e| format!("saved.json can't be read: {e}")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e.to_string()),
    }
}

/// Written whole or not at all, readable by the operator only.
fn store(state: &AppState, items: &[SavedReply]) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let path = file(state);
    let tmp = path.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(items).map_err(|e| e.to_string())?;
    let mut out = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)
        .map_err(|e| e.to_string())?;
    out.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
}

fn tell(state: &AppState, id: &str, item: Option<&SavedReply>) {
    let update = serde_json::json!({"kind": "saved.updated", "id": id, "item": item});
    let _ = state.tx.send(update.to_string());
}

pub(crate) fn list(state: &AppState, stream_id: Option<&str>) -> Result<SavedList, String> {
    let mut items: Vec<SavedReply> = load(state)?
        .into_iter()
        .filter(|s| stream_id.is_none_or(|sid| s.stream_id == sid))
        .collect();
    items.sort_by(|a, b| b.saved_at.cmp(&a.saved_at));
    Ok(SavedList { items })
}

/// Saves a finished reply; saving it again answers the one already kept.
/// Its checklist starts with the items it already wrote ticked.
pub(crate) fn save(state: &AppState, req: &SaveReq) -> Result<SavedReply, String> {
    let _one = file_lock().lock().unwrap();
    let mut items = load(state)?;
    if let Some(kept) = items.iter().find(|s| s.stream_id == req.stream_id && s.event_hash == req.event_hash) {
        return Ok(kept.clone());
    }
    let reply = state
        .heart
        .lock()
        .unwrap()
        .events_of_family(&req.stream_id, "agent.turn.done")
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|e| e.event_hash == req.event_hash)
        .ok_or_else(|| format!("no finished reply {} in {}", req.event_hash, req.stream_id))?;
    let payload: serde_json::Value = serde_json::from_str(&reply.payload).map_err(|e| e.to_string())?;
    let text = payload["summary"].as_str().unwrap_or_default().to_string();
    let done = checklist(&text)
        .iter()
        .enumerate()
        .filter_map(|(at, ticked)| ticked.then_some(at))
        .collect();
    let saved = SavedReply {
        id: crate::node::mint_id("saved"),
        stream_id: req.stream_id.clone(),
        event_hash: req.event_hash.clone(),
        text,
        saved_at: chrono::Utc::now().to_rfc3339(),
        done,
    };
    items.push(saved.clone());
    store(state, &items)?;
    tell(state, &saved.id, Some(&saved));
    Ok(saved)
}

/// A fresh start's: only `stream_id`'s saved replies stay, each by its
/// reply's new hash in the new Heart.
pub(crate) fn keep_only(state: &AppState, stream_id: &str, renamed: &std::collections::HashMap<String, String>) -> Result<(), String> {
    let _one = file_lock().lock().unwrap();
    // The other threads' saved replies stay on disk, as their history does
    // in the Heart set aside.
    let file = state.base.join("saved.json");
    if file.exists() {
        let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
        std::fs::copy(&file, state.base.join(format!("saved-{stamp}.json"))).map_err(|e| format!("saved.json couldn't be kept: {e}"))?;
    }
    let kept: Vec<SavedReply> = load(state)?
        .into_iter()
        .filter(|s| s.stream_id == stream_id)
        .filter_map(|s| Some(SavedReply { event_hash: renamed.get(&s.event_hash)?.clone(), ..s }))
        .collect();
    store(state, &kept)
}

pub(crate) fn unsave(state: &AppState, id: &str) -> Result<Unsaved, String> {
    let _one = file_lock().lock().unwrap();
    let mut items = load(state)?;
    let before = items.len();
    items.retain(|s| s.id != id);
    if items.len() == before {
        return Err(format!("no saved reply {id}"));
    }
    store(state, &items)?;
    tell(state, id, None);
    Ok(Unsaved { id: id.to_string() })
}

pub(crate) fn tick(state: &AppState, req: &TickReq) -> Result<SavedReply, String> {
    let _one = file_lock().lock().unwrap();
    let mut items = load(state)?;
    let saved = items
        .iter_mut()
        .find(|s| s.id == req.id)
        .ok_or_else(|| format!("no saved reply {}", req.id))?;
    let count = checklist(&saved.text).len();
    if req.item >= count {
        return Err(format!("its checklist has {count} items"));
    }
    saved.done.retain(|&at| at != req.item);
    if req.done {
        saved.done.push(req.item);
        saved.done.sort_unstable();
    }
    let saved = saved.clone();
    store(state, &items)?;
    tell(state, &saved.id, Some(&saved));
    Ok(saved)
}

fn operator_only() -> Result<(), (StatusCode, String)> {
    if crate::actor::agent_calling() {
        return Err((StatusCode::FORBIDDEN, "saved replies are the operator's own".to_string()));
    }
    Ok(())
}

async fn run<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<Json<T>, (StatusCode, String)> {
    operator_only()?;
    crate::actor::spawn_blocking(work)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))
}

pub(crate) async fn list_route(
    State(st): State<AppState>,
    Query(q): Query<SavedQuery>,
) -> Result<Json<SavedList>, (StatusCode, String)> {
    run(move || list(&st, q.stream_id.as_deref())).await
}

pub(crate) async fn save_route(
    State(st): State<AppState>,
    Json(req): Json<SaveReq>,
) -> Result<Json<SavedReply>, (StatusCode, String)> {
    run(move || save(&st, &req)).await
}

pub(crate) async fn unsave_route(
    State(st): State<AppState>,
    Json(req): Json<SavedId>,
) -> Result<Json<Unsaved>, (StatusCode, String)> {
    run(move || unsave(&st, &req.id)).await
}

pub(crate) async fn tick_route(
    State(st): State<AppState>,
    Json(req): Json<TickReq>,
) -> Result<Json<SavedReply>, (StatusCode, String)> {
    run(move || tick(&st, &req)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::project::open_stream;
    use crate::testutil::*;
    use std::sync::Arc;

    #[test]
    fn a_checklist_is_its_item_lines_outside_code() {
        let text = "Plan:\n- [ ] one\n  * [x] two\n+ [X] three\n```\n- [ ] not an item\n```\n- [] nor this\n-[ ] nor this\n- [ ] four";
        assert_eq!(checklist(text), vec![false, true, true, false]);
    }

    /// The operator saves a finished reply, ticks its checklist, finds it in
    /// its thread's list and in every thread's, and unsaves it. Saving it
    /// twice keeps one. Nothing of it lands in the conversation's stream.
    #[test]
    fn a_saved_reply_is_the_operators_and_stays_out_of_the_stream() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let sid = open_stream(&state, &open_req(&repo, "saved")).unwrap().context.stream_id;
        let reply = serde_json::json!({"summary": "Do:\n- [x] read\n- [ ] write\n- [ ] ship", "agent_id": "a", "turn_id": "t"});
        let done = crate::state::append(&state, &sid, "agent:orchestrator", "agent.turn.done", reply.to_string(), vec![]).unwrap();
        let before = state.heart.lock().unwrap().read_stream(&sid).unwrap().len();
        let mut live = state.tx.subscribe();
        let req = SaveReq { stream_id: sid.clone(), event_hash: done.event_hash.clone() };

        let saved = save(&state, &req).unwrap();
        assert_eq!((saved.text.as_str(), saved.done.clone()), ("Do:\n- [x] read\n- [ ] write\n- [ ] ship", vec![0]));
        assert_eq!(save(&state, &req).unwrap(), saved, "the same reply, kept once");
        let ticked = tick(&state, &TickReq { id: saved.id.clone(), item: 2, done: true }).unwrap();
        assert_eq!(ticked.done, vec![0, 2]);
        let unticked = tick(&state, &TickReq { id: saved.id.clone(), item: 0, done: false }).unwrap();
        assert_eq!(unticked.done, vec![2]);
        assert!(tick(&state, &TickReq { id: saved.id.clone(), item: 3, done: true }).is_err());
        assert_eq!(list(&state, Some(&sid)).unwrap().items, vec![unticked.clone()]);
        assert_eq!(list(&state, None).unwrap().items.len(), 1);
        assert!(list(&state, Some("wt:other")).unwrap().items.is_empty());
        let update: serde_json::Value = serde_json::from_str(&live.try_recv().unwrap()).unwrap();
        assert_eq!((update["kind"].as_str(), update["id"].as_str()), (Some("saved.updated"), Some(saved.id.as_str())));

        unsave(&state, &saved.id).unwrap();
        assert!(list(&state, None).unwrap().items.is_empty());
        assert_eq!(state.heart.lock().unwrap().read_stream(&sid).unwrap().len(), before, "nothing in the stream");
    }

    /// Every saved-reply route refuses an agent, the Baker included.
    #[test]
    fn an_agent_never_reaches_saved_replies() {
        let refused = crate::actor::with("agent:wt_x:baker".to_string(), operator_only);
        assert_eq!(refused.unwrap_err().0, StatusCode::FORBIDDEN);
        assert!(operator_only().is_ok(), "the operator's own call");
    }
}
