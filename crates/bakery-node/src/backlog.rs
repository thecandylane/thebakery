//! The backlog: what is to be done next, for the Bakery itself or for one
//! project, in the operator's order. The operator adds, edits, orders and
//! removes items, and moves them along (queued, doing, done, dropped). An
//! agent reads the queue (`backlog_list`), adds to its end (`backlog_add`:
//! marked as its own, with a line in its thread linking to the item), and
//! edits an item or moves it along; ordering and removing are the
//! operator's only. An agent outside the Baker's side reaches its project's
//! items and the Bakery's. Kept in the Heart (`sys:backlog`); each change
//! goes out live as `backlog.updated`.

use crate::actor::Caller;
use crate::state::AppState;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// The whole Bakery's backlog.
pub(crate) const STREAM: &str = "sys:backlog";
/// `{id, title, why, project_id}`.
const ADDED: &str = "backlog.item.added";
/// The item's words, scope and threads as they now are: `{id, title, why,
/// project_id, threads}`.
const EDITED: &str = "backlog.item.edited";
/// `{id, status}`.
const STATUS: &str = "backlog.item.status";
/// `{id, before}`: just above `before`, or last when it is null.
const MOVED: &str = "backlog.item.moved";
/// `{id}`.
const REMOVED: &str = "backlog.item.removed";
/// On an agent's own thread: it added `{id, title}` to the backlog.
pub(crate) const NOTED: &str = "backlog.added";

#[derive(schemars::JsonSchema, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Status {
    Queued,
    Doing,
    Done,
    Dropped,
}

#[derive(schemars::JsonSchema, Debug, Clone, PartialEq, Serialize)]
pub(crate) struct BacklogItem {
    pub(crate) id: String,
    pub(crate) title: String,
    /// Why it is worth doing.
    pub(crate) why: String,
    /// The project it is for; the Bakery itself when absent.
    pub(crate) project_id: Option<String>,
    pub(crate) status: Status,
    /// Its place in the backlog, from 0 at the top.
    pub(crate) order: usize,
    /// `operator`, or the thread of the agent that added it.
    pub(crate) added_by: String,
    pub(crate) added_at: String,
    /// The threads it concerns: the agent's that added it, any agent's that
    /// changed it, and any the operator linked.
    pub(crate) threads: Vec<String>,
    /// Each change, oldest first.
    pub(crate) history: Vec<Change>,
}

#[derive(schemars::JsonSchema, Debug, Clone, PartialEq, Serialize)]
pub(crate) struct Change {
    pub(crate) at: String,
    /// `operator`, or the thread of the agent that made it.
    pub(crate) by: String,
    /// added, edited, moved, or the status it was put in.
    pub(crate) what: String,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct Backlog {
    /// In the backlog's order.
    pub(crate) items: Vec<BacklogItem>,
}

/// `operator`, or the thread of the agent an actor is.
fn who(actor: &str) -> String {
    crate::messages::sender(actor).unwrap_or_else(|| "operator".to_string())
}

fn words(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

/// Every item as it stands, in the backlog's order.
pub(crate) fn items(state: &AppState) -> Vec<BacklogItem> {
    let events = state.heart.lock().unwrap().read_stream(STREAM).unwrap_or_default();
    let mut items: Vec<BacklogItem> = Vec::new();
    for e in events {
        let Ok(fact) = serde_json::from_str::<Value>(&e.payload) else { continue };
        let id = words(&fact["id"]);
        let by = who(&e.actor);
        if e.family == ADDED {
            items.push(BacklogItem {
                id: id.clone(),
                title: words(&fact["title"]),
                why: words(&fact["why"]),
                project_id: fact["project_id"].as_str().map(str::to_string),
                status: Status::Queued,
                order: 0,
                added_by: by.clone(),
                added_at: e.ts.clone(),
                threads: Vec::new(),
                history: Vec::new(),
            });
        }
        let Some(at) = items.iter().position(|i| i.id == id) else { continue };
        let what = match e.family.as_str() {
            ADDED => "added".to_string(),
            EDITED => {
                let item = &mut items[at];
                item.title = words(&fact["title"]);
                item.why = words(&fact["why"]);
                item.project_id = fact["project_id"].as_str().map(str::to_string);
                item.threads = serde_json::from_value(fact["threads"].clone()).unwrap_or_default();
                "edited".to_string()
            }
            STATUS => match serde_json::from_value::<Status>(fact["status"].clone()) {
                Ok(status) => {
                    items[at].status = status;
                    words(&fact["status"])
                }
                Err(_) => continue,
            },
            MOVED => {
                let item = items.remove(at);
                let to = fact["before"].as_str().and_then(|before| items.iter().position(|i| i.id == before));
                items.insert(to.unwrap_or(items.len()), item);
                "moved".to_string()
            }
            REMOVED => {
                items.remove(at);
                continue;
            }
            _ => continue,
        };
        let Some(item) = items.iter_mut().find(|i| i.id == id) else { continue };
        if by != "operator" && !item.threads.contains(&by) {
            item.threads.push(by.clone());
        }
        item.history.push(Change { at: e.ts.clone(), by, what });
    }
    for (order, item) in items.iter_mut().enumerate() {
        item.order = order;
    }
    items
}

/// An item the caller reaches.
fn reached(state: &AppState, caller: &Caller, id: &str) -> Result<BacklogItem, String> {
    items(state)
        .into_iter()
        .find(|i| i.id == id && caller.reaches(i.project_id.as_deref()))
        .ok_or_else(|| format!("no backlog item {id} here"))
}

/// The project a scope names: none for the Bakery.
fn project_of(state: &AppState, scope: &str) -> Result<Option<String>, String> {
    match scope.trim() {
        "bakery" => Ok(None),
        project => {
            let known = crate::catalog::known_projects(state)?.iter().any(|p| p.project_id == project);
            match known {
                true => Ok(Some(project.to_string())),
                false => Err(format!("no project {project}: a scope is bakery or a project's id")),
            }
        }
    }
}

fn operator_only(what: &str) -> Result<(), String> {
    match crate::actor::agent_calling() {
        true => Err(format!("only the operator {what} the backlog")),
        false => Ok(()),
    }
}

fn record(state: &AppState, family: &str, fact: Value) -> Result<String, String> {
    let actor = crate::actor::attribute("human");
    Ok(crate::state::append(state, STREAM, &actor, family, fact.to_string(), vec![])?.event_hash)
}

/// Sends an item as it now stands (null once removed), with the backlog's
/// order, and answers the item.
fn tell(state: &AppState, id: &str) -> Option<BacklogItem> {
    let items = items(state);
    let item = items.iter().find(|i| i.id == id).cloned();
    let order: Vec<&str> = items.iter().map(|i| i.id.as_str()).collect();
    let update = json!({"kind": "backlog.updated", "id": id, "item": item, "order": order});
    let _ = state.tx.send(update.to_string());
    item
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct BacklogQuery {
    /// `bakery`, or a project's id: only its items. Every item you reach
    /// when absent.
    #[serde(default)]
    pub(crate) scope: Option<String>,
}

pub(crate) fn list(state: &AppState, scope: Option<&str>) -> Result<Backlog, String> {
    let caller = Caller::of(state);
    let only = scope.map(|scope| project_of(state, scope)).transpose()?;
    let items = items(state)
        .into_iter()
        .filter(|i| caller.reaches(i.project_id.as_deref()) && only.as_ref().is_none_or(|p| *p == i.project_id))
        .collect();
    Ok(Backlog { items })
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct AddReq {
    pub(crate) title: String,
    /// Why it is worth doing: what it brings. An agent always says.
    #[serde(default)]
    pub(crate) why: String,
    /// `bakery`, or a project's id. Absent: an agent's own project (the
    /// Bakery when it has none); the Bakery for the operator.
    #[serde(default)]
    pub(crate) scope: Option<String>,
}

/// Adds an item to the end of the backlog.
pub(crate) fn add(state: &AppState, req: &AddReq) -> Result<BacklogItem, String> {
    let caller = Caller::of(state);
    let (title, why) = (req.title.trim(), req.why.trim());
    if title.is_empty() {
        return Err("an item needs a title".to_string());
    }
    if why.is_empty() && caller.thread.is_some() {
        return Err("say why it is worth doing: what it brings".to_string());
    }
    let project_id = match &req.scope {
        Some(scope) => project_of(state, scope)?,
        None => caller.project.clone(),
    };
    if !caller.reaches(project_id.as_deref()) {
        return Err("that project is not yours: add to your project's backlog, or the Bakery's".to_string());
    }
    let id = crate::node::mint_id("backlog");
    let added = record(state, ADDED, json!({"id": id, "title": title, "why": why, "project_id": project_id}))?;
    if let Some(thread) = &caller.thread {
        let line = json!({"id": id, "title": title}).to_string();
        crate::state::append(state, thread, &crate::actor::attribute("human"), NOTED, line, vec![added])?;
    }
    tell(state, &id).ok_or_else(|| format!("backlog item {id} went missing"))
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct EditReq {
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) title: Option<String>,
    #[serde(default)]
    pub(crate) why: Option<String>,
    /// `bakery`, or a project's id.
    #[serde(default)]
    pub(crate) scope: Option<String>,
    /// The threads it concerns, by stream id: these replace the ones linked.
    #[serde(default)]
    pub(crate) threads: Option<Vec<String>>,
}

pub(crate) fn edit(state: &AppState, req: &EditReq) -> Result<BacklogItem, String> {
    let caller = Caller::of(state);
    let item = reached(state, &caller, &req.id)?;
    let title = req.title.as_deref().map_or(item.title.as_str(), str::trim);
    if title.is_empty() {
        return Err("an item needs a title".to_string());
    }
    let why = req.why.as_deref().map_or(item.why.as_str(), str::trim);
    let project_id = match &req.scope {
        Some(scope) => project_of(state, scope)?,
        None => item.project_id.clone(),
    };
    if !caller.reaches(project_id.as_deref()) {
        return Err("that project is not yours: keep it in your project's backlog, or the Bakery's".to_string());
    }
    let threads = req.threads.clone().unwrap_or(item.threads.clone());
    for thread in &threads {
        crate::state::context_for(state, thread).map_err(|_| format!("no thread {thread}"))?;
    }
    let fact = json!({"id": item.id, "title": title, "why": why, "project_id": project_id, "threads": threads});
    record(state, EDITED, fact)?;
    tell(state, &item.id).ok_or_else(|| format!("backlog item {} went missing", item.id))
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct StatusReq {
    pub(crate) id: String,
    pub(crate) status: Status,
}

pub(crate) fn set_status(state: &AppState, req: &StatusReq) -> Result<BacklogItem, String> {
    let item = reached(state, &Caller::of(state), &req.id)?;
    if item.status == req.status {
        return Ok(item);
    }
    record(state, STATUS, json!({"id": item.id, "status": req.status}))?;
    tell(state, &item.id).ok_or_else(|| format!("backlog item {} went missing", item.id))
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct MoveReq {
    pub(crate) id: String,
    /// The item it goes just above; the end of the backlog when absent.
    #[serde(default)]
    pub(crate) before: Option<String>,
}

/// The operator puts an item in its place.
pub(crate) fn move_item(state: &AppState, req: &MoveReq) -> Result<Backlog, String> {
    operator_only("orders")?;
    let items = items(state);
    let known = |id: &str| items.iter().any(|i| i.id == id);
    if !known(&req.id) {
        return Err(format!("no backlog item {}", req.id));
    }
    if let Some(before) = &req.before {
        if *before == req.id || !known(before) {
            return Err(format!("{before} is no other backlog item to go above"));
        }
    }
    record(state, MOVED, json!({"id": req.id, "before": req.before}))?;
    tell(state, &req.id);
    list(state, None)
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct IdReq {
    pub(crate) id: String,
}

pub(crate) fn remove(state: &AppState, id: &str) -> Result<Backlog, String> {
    operator_only("removes from")?;
    if !items(state).iter().any(|i| i.id == id) {
        return Err(format!("no backlog item {id}"));
    }
    record(state, REMOVED, json!({"id": id}))?;
    tell(state, id);
    list(state, None)
}

async fn blocking<T: Send + 'static>(work: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<Json<T>, (StatusCode, String)> {
    crate::actor::spawn_blocking(work)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map(Json)
        .map_err(|e| match e.starts_with("only the operator") {
            true => (StatusCode::FORBIDDEN, e),
            false => (StatusCode::BAD_REQUEST, e),
        })
}

pub(crate) async fn list_route(State(st): State<AppState>, Query(q): Query<BacklogQuery>) -> Result<Json<Backlog>, (StatusCode, String)> {
    blocking(move || list(&st, q.scope.as_deref())).await
}

pub(crate) async fn add_route(State(st): State<AppState>, Json(req): Json<AddReq>) -> Result<Json<BacklogItem>, (StatusCode, String)> {
    blocking(move || add(&st, &req)).await
}

pub(crate) async fn edit_route(State(st): State<AppState>, Json(req): Json<EditReq>) -> Result<Json<BacklogItem>, (StatusCode, String)> {
    blocking(move || edit(&st, &req)).await
}

pub(crate) async fn status_route(State(st): State<AppState>, Json(req): Json<StatusReq>) -> Result<Json<BacklogItem>, (StatusCode, String)> {
    blocking(move || set_status(&st, &req)).await
}

pub(crate) async fn move_route(State(st): State<AppState>, Json(req): Json<MoveReq>) -> Result<Json<Backlog>, (StatusCode, String)> {
    blocking(move || move_item(&st, &req)).await
}

pub(crate) async fn remove_route(State(st): State<AppState>, Json(req): Json<IdReq>) -> Result<Json<Backlog>, (StatusCode, String)> {
    blocking(move || remove(&st, &req.id)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::Seat;
    use crate::testutil::*;
    use std::sync::Arc;

    /// The operator and agents keep one backlog: an agent's item goes last,
    /// marked as its own with a line in its thread; it reads, edits and
    /// moves along its project's items and the Bakery's, never another
    /// project's; ordering and removing are the operator's. Each change goes
    /// out live and into the item's history.
    #[test]
    fn the_operator_and_agents_keep_one_backlog() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(bakery_agent::StubModel));
        let project = |name: &str| {
            let root = dir.path().join(name);
            std::fs::create_dir_all(&root).unwrap();
            crate::catalog::create_project(&state, name, Some(crate::catalog::DirectoryRootSpec::Attach(root.to_str().unwrap()))).unwrap()
        };
        let (app, chef) = project("app");
        let (other, outsider) = project("other");
        let mut events = state.tx.subscribe();
        let as_agent = |thread: &str, f: &dyn Fn() -> Result<Backlog, String>| crate::actor::with(crate::actor::agent(thread, Seat::Orchestrator), f);
        let add_as = |thread: &str, title: &str, scope: Option<&str>| {
            let req = AddReq { title: title.into(), why: format!("{title} matters"), scope: scope.map(str::to_string) };
            crate::actor::with(crate::actor::agent(thread, Seat::Orchestrator), || add(&state, &req))
        };
        let titles = |b: Backlog| b.items.into_iter().map(|i| i.title).collect::<Vec<_>>();

        let first = add(&state, &AddReq { title: "Faster boot".into(), why: String::new(), scope: None }).unwrap();
        assert_eq!((first.added_by.as_str(), first.project_id.as_deref()), ("operator", None), "the operator's, for the Bakery");
        let theirs = add_as(&chef.stream_id, "Retry uploads", None).unwrap();
        assert_eq!((theirs.order, theirs.project_id.as_deref()), (1, Some(app.project_id.as_str())), "last, in the agent's project");
        assert_eq!((theirs.added_by.as_str(), theirs.threads.clone()), (chef.stream_id.as_str(), vec![chef.stream_id.clone()]));
        let line = state.heart.lock().unwrap().events_of_family(&chef.stream_id, NOTED).unwrap();
        assert_eq!(serde_json::from_str::<Value>(&line[0].payload).unwrap()["id"], theirs.id.as_str(), "a line in its thread");
        add_as(&outsider.stream_id, "Their thing", None).unwrap();
        let no_why = AddReq { title: "x".into(), why: " ".into(), scope: None };
        assert!(crate::actor::with(crate::actor::agent(&chef.stream_id, Seat::Orchestrator), || add(&state, &no_why)).is_err(), "an agent says why");
        assert!(add_as(&chef.stream_id, "Theirs too", Some(&other.project_id)).unwrap_err().contains("not yours"));

        assert_eq!(titles(as_agent(&chef.stream_id, &|| list(&state, None)).unwrap()), ["Faster boot", "Retry uploads"], "its project's and the Bakery's");
        assert_eq!(titles(list(&state, Some(&other.project_id)).unwrap()), ["Their thing"]);
        assert!(!titles(as_agent(&outsider.stream_id, &|| list(&state, None)).unwrap()).contains(&"Retry uploads".to_string()));

        let moved = |thread: &str, status| {
            let req = StatusReq { id: theirs.id.clone(), status };
            crate::actor::with(crate::actor::agent(thread, Seat::Orchestrator), || set_status(&state, &req))
        };
        assert!(moved(&outsider.stream_id, Status::Doing).is_err(), "another project's item");
        assert_eq!(moved(&chef.stream_id, Status::Doing).unwrap().status, Status::Doing);
        let edit_req = EditReq { id: theirs.id.clone(), title: Some("Retry uploads twice".into()), why: None, scope: None, threads: None };
        let edited = edit(&state, &edit_req).unwrap();
        assert_eq!((edited.title.as_str(), edited.why.as_str()), ("Retry uploads twice", "Retry uploads matters"));

        let to_top = MoveReq { id: theirs.id.clone(), before: Some(first.id.clone()) };
        assert!(as_agent(&chef.stream_id, &|| move_item(&state, &to_top)).unwrap_err().contains("only the operator"));
        assert!(as_agent(&chef.stream_id, &|| remove(&state, &first.id)).unwrap_err().contains("only the operator"));
        assert_eq!(titles(move_item(&state, &to_top).unwrap())[..2], ["Retry uploads twice", "Faster boot"]);
        let last = move_item(&state, &MoveReq { id: theirs.id.clone(), before: None }).unwrap();
        let item = last.items.last().unwrap();
        assert_eq!(item.id, theirs.id);
        let history: Vec<_> = item.history.iter().map(|c| (c.by.as_str(), c.what.as_str())).collect();
        let agent = chef.stream_id.as_str();
        assert_eq!(history, [(agent, "added"), (agent, "doing"), ("operator", "edited"), ("operator", "moved"), ("operator", "moved")]);

        assert_eq!(titles(remove(&state, &first.id).unwrap()), ["Their thing", "Retry uploads twice"]);
        let sent: Vec<Value> = std::iter::from_fn(|| events.try_recv().ok())
            .filter_map(|e| serde_json::from_str::<Value>(&e).ok())
            .filter(|e| e["kind"] == "backlog.updated")
            .collect();
        let gone = sent.last().unwrap();
        assert!(gone["item"].is_null() && gone["id"] == first.id.as_str() && gone["order"].as_array().unwrap().len() == 2, "{gone}");
    }
}
