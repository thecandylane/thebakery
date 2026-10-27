//! The fresh start, the operator's `scripts/bakery.sh fresh-start`: the
//! node starts again on a new, clean Heart that carries one Baker thread
//! (the operator's "local models") with its whole history, under a
//! permanent Baker minted fresh, and with the Always-allow grants cleared.
//! The old Heart is set aside whole and closed, for the operator to delete
//! when they choose. Nothing else is deleted: the worktrees the other
//! conversations worked in stay on disk, listed.
//!
//! The command asks the running node for the plan (`GET /fresh-start/plan`):
//! what it keeps, what it leaves behind, and whether a turn runs, when it
//! refuses. On the operator's yes it writes the marker and restarts the
//! node, which sets the old Heart aside before it opens one (`set_aside`)
//! and carries the thread over once it runs (`carry`).

use crate::state::AppState;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    Json,
};
use bakery_core::{Event, NewEvent};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The armed fresh start, `{keep}`, beside the Heart.
pub(crate) const MARKER: &str = "fresh-start.json";
/// What the last fresh start did, `{kept, home, closed}`, for the command.
pub(crate) const DONE: &str = "fresh-start.done.json";
/// A fresh start that could not run, with why; the Heart stayed as it was.
const FAILED: &str = "fresh-start.failed.json";

#[derive(Serialize, Deserialize)]
struct Marker {
    keep: String,
}

#[derive(schemars::JsonSchema, Debug, Clone, Serialize)]
pub(crate) struct Thread {
    pub(crate) stream_id: String,
    pub(crate) title: String,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct Plan {
    /// The Baker thread it carries over: the one named, else the one called
    /// "local models".
    pub(crate) keep: Option<Thread>,
    /// The Baker threads there are, to name one when it can't tell.
    pub(crate) threads: Vec<Thread>,
    /// The conversations it leaves behind in the old Heart, the Baker's own
    /// among them.
    pub(crate) left_behind: usize,
    /// Turns running now: it never cuts one off.
    pub(crate) turns_running: usize,
    /// The Always-allow kinds and grants it clears.
    pub(crate) always_allow: usize,
    /// Worktrees the conversations it leaves behind worked in, left on disk.
    pub(crate) worktrees: Vec<String>,
    /// Why it can't go ahead now.
    pub(crate) refused: Option<String>,
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct PlanQuery {
    /// The Baker thread to keep, by stream_id; the one called "local
    /// models" when absent.
    #[serde(default)]
    pub(crate) keep: Option<String>,
}

/// What a fresh start would keep, leave behind and clear, now.
pub(crate) fn plan(state: &AppState, keep: Option<&str>) -> Plan {
    let threads: Vec<Thread> = crate::baker_threads::list_threads(state, true)
        .unwrap_or_default()
        .into_iter()
        .filter(|t| !t.main)
        .map(|t| Thread { stream_id: t.stream_id, title: t.title })
        .collect();
    let chosen: Vec<&Thread> = match keep.map(str::trim).filter(|k| !k.is_empty()) {
        Some(id) => threads.iter().filter(|t| t.stream_id == id).collect(),
        None => threads.iter().filter(|t| t.title.to_lowercase().contains("local models")).collect(),
    };
    let kept = match chosen.as_slice() {
        [one] => Some((*one).clone()),
        _ => None,
    };
    let turns_running = state.lifecycle.lock().unwrap().running_turns.values().filter(|n| **n > 0).count();
    let always_allow = {
        let node = state.node.lock().unwrap();
        node.always_kinds.len() + node.always_grants.len()
    };
    let kept_id = kept.as_ref().map(|t| t.stream_id.as_str());
    let streams = crate::state::known_streams(state).unwrap_or_default();
    let others: Vec<_> = streams.iter().filter(|s| !s.discarded && Some(s.stream_id.as_str()) != kept_id).collect();
    // The Bakery's own folder stays the new Baker's; the kept thread's stays its own.
    let baker = crate::baker_threads::baker_pointer(state).and_then(|home| crate::state::context_for(state, &home).ok());
    let in_use = |path: &str| {
        baker.as_ref().is_some_and(|b| crate::state::same_path(path, &b.worktree_path))
            || streams.iter().any(|s| Some(s.stream_id.as_str()) == kept_id && crate::state::same_path(path, &s.worktree_path))
    };
    let mut worktrees: Vec<String> = others
        .iter()
        .filter(|s| s.exists_on_disk && !s.in_place && !s.worktree_path.trim().is_empty() && !in_use(&s.worktree_path))
        .map(|s| s.worktree_path.clone())
        .collect();
    worktrees.sort();
    worktrees.dedup();
    let refused = if turns_running > 0 {
        Some(format!("{turns_running} turns are running: let them finish, or stop them, then run it again"))
    } else if kept.is_none() && chosen.is_empty() {
        Some("no Baker thread is called \"local models\": name the one to keep with --keep <stream_id>".to_string())
    } else if kept.is_none() {
        Some("more than one Baker thread is called \"local models\": name the one to keep with --keep <stream_id>".to_string())
    } else {
        None
    };
    Plan { keep: kept, threads, left_behind: others.len(), turns_running, always_allow, worktrees, refused }
}

/// `GET /fresh-start/plan`: the operator's own.
pub(crate) async fn plan_route(State(st): State<AppState>, Query(q): Query<PlanQuery>) -> Result<Json<Plan>, (StatusCode, String)> {
    if crate::actor::agent_calling() {
        return Err((StatusCode::FORBIDDEN, "a fresh start is the operator's alone".to_string()));
    }
    crate::actor::spawn_blocking(move || Json(plan(&st, q.keep.as_deref())))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// What the node carries into its new Heart, read before the old one is
/// set aside.
pub(crate) struct Pending {
    keep: String,
    events: Vec<Event>,
    closed: PathBuf,
}

/// Before the node opens its Heart: when the operator armed a fresh start,
/// read the thread it keeps, then set the old Heart aside whole (its
/// write-ahead log folded in) and closed, under a name with the time. Any
/// failure before the Heart moves leaves it as it was, the start disarmed
/// with why.
pub(crate) fn set_aside(base: &Path) -> Option<Pending> {
    let marker = base.join(MARKER);
    let text = std::fs::read_to_string(&marker).ok()?;
    match set_aside_armed(base, &text) {
        Ok(pending) => Some(pending),
        Err(why) => {
            eprintln!("bakery-node: the fresh start did not run, and the Heart is as it was: {why}");
            let failed = serde_json::json!({ "why": why, "armed": text });
            let _ = std::fs::write(base.join(FAILED), failed.to_string());
            let _ = std::fs::remove_file(&marker);
            None
        }
    }
}

fn set_aside_armed(base: &Path, armed: &str) -> Result<Pending, String> {
    let armed: Marker = serde_json::from_str(armed).map_err(|e| format!("{MARKER} can't be read: {e}"))?;
    let heart = base.join("heart.db");
    let events = {
        let old = bakery_heart::Heart::open(&heart.to_string_lossy()).map_err(|e| e.to_string())?;
        old.read_stream(&armed.keep).map_err(|e| e.to_string())?
    };
    let opened = events.iter().any(|e| e.family == crate::baker_threads::THREAD_OPENED_FAMILY);
    if !opened {
        return Err(format!("{} is not a Baker thread in this Heart", armed.keep));
    }
    let fold = rusqlite::Connection::open(&heart).map_err(|e| e.to_string())?;
    fold.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(())).map_err(|e| e.to_string())?;
    drop(fold);
    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    let closed = base.join(format!("heart-{stamp}.db"));
    std::fs::rename(&heart, &closed).map_err(|e| e.to_string())?;
    for side in ["-wal", "-shm"] {
        let from = base.join(format!("heart.db{side}"));
        if from.exists() {
            let _ = std::fs::rename(&from, base.join(format!("heart-{stamp}.db{side}")));
        }
    }
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(&closed, std::fs::Permissions::from_mode(0o400));
    // node.json as it was, for `roll_back`: the carry changes it.
    // It holds the operator credential: 0600, and on the rooms' deny list.
    if std::fs::copy(base.join("node.json"), base.join(NODE_BEFORE)).is_ok() {
        let _ = std::fs::set_permissions(base.join(NODE_BEFORE), std::fs::Permissions::from_mode(0o600));
    }
    let _ = std::fs::remove_file(base.join(MARKER));
    Ok(Pending { keep: armed.keep, events, closed })
}

/// node.json as it was before a fresh start changed it, until it's done.
const NODE_BEFORE: &str = "node.json.before-fresh-start";

/// A fresh start that failed after the old Heart moved: the new Heart goes
/// aside (`heart-failed-<time>.db*`), the old one back as heart.db (0600),
/// node.json as it was, and why is said for the command. The node then
/// stops: it starts again on the Heart it had.
pub(crate) fn roll_back(base: &Path, pending: &Pending, why: &str) {
    use std::os::unix::fs::PermissionsExt;
    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    let closed = pending.closed.to_string_lossy().into_owned();
    for side in ["", "-wal", "-shm"] {
        let new = base.join(format!("heart.db{side}"));
        if new.exists() {
            let _ = std::fs::rename(&new, base.join(format!("heart-failed-{stamp}.db{side}")));
        }
        let old = PathBuf::from(format!("{closed}{side}"));
        if old.exists() {
            let _ = std::fs::rename(&old, &new);
        }
    }
    let _ = std::fs::set_permissions(base.join("heart.db"), std::fs::Permissions::from_mode(0o600));
    if base.join(NODE_BEFORE).exists() {
        let _ = std::fs::rename(base.join(NODE_BEFORE), base.join("node.json"));
    }
    let why = format!("it failed after the old Heart was set aside, and everything was put back: {why}");
    eprintln!("bakery-node: the fresh start did not run: {why}");
    let _ = std::fs::write(base.join(FAILED), serde_json::json!({ "why": why }).to_string());
}

/// A fresh start that went through: node.json's copy goes.
pub(crate) fn finished(base: &Path) {
    let _ = std::fs::remove_file(base.join(NODE_BEFORE));
}

/// Event hashes a payload names, carried over to their new ones: each run
/// of 64 hex digits that is one.
fn renamed_in(payload: &str, renamed: &HashMap<String, String>) -> String {
    let mut out = String::with_capacity(payload.len());
    let mut run = String::new();
    let flush = |run: &mut String, out: &mut String| {
        out.push_str(renamed.get(run.as_str()).map_or(run.as_str(), String::as_str));
        run.clear();
    };
    for c in payload.chars() {
        if c.is_ascii_hexdigit() {
            run.push(c);
            continue;
        }
        if run.len() == 64 {
            flush(&mut run, &mut out);
        } else {
            out.push_str(&run);
            run.clear();
        }
        out.push(c);
    }
    if run.len() == 64 {
        flush(&mut run, &mut out);
    } else {
        out.push_str(&run);
    }
    out
}

/// Once the node runs on its new Heart: the Always-allow grants cleared,
/// the permanent Baker minted fresh, and the kept thread's history copied
/// in as it was, each fact in order with its time, its causes and the
/// facts it names carried over, its owner now the new Baker; a thread with
/// no folder of its own works in the Baker's. Its saved replies stay,
/// every other's goes.
pub(crate) fn carry(state: &AppState, pending: &Pending) -> Result<(), String> {
    {
        let mut node = state.node.lock().unwrap();
        node.always_kinds.clear();
        node.always_grants.clear();
        node.baker_stream_id = None;
        node.save(&state.base)?;
    }
    let env_root = crate::routes::baker::bakery_source_root_from_env();
    let home = crate::routes::baker::ensure_permanent_baker(state, env_root.as_deref(), None)?.stream_id;
    let mut renamed: HashMap<String, String> = HashMap::new();
    {
        let mut heart = state.heart.lock().unwrap();
        for event in &pending.events {
            let mut payload = renamed_in(&event.payload, &renamed);
            if event.family == crate::baker_threads::THREAD_OPENED_FAMILY {
                let mut opened: serde_json::Value = serde_json::from_str(&payload).map_err(|e| e.to_string())?;
                opened["baker_stream_id"] = serde_json::json!(home);
                payload = opened.to_string();
            }
            let carried = heart
                .append(NewEvent {
                    stream_id: event.stream_id.clone(),
                    actor: event.actor.clone(),
                    ts: event.ts.clone(),
                    causality: event.causality.iter().filter_map(|c| renamed.get(c).cloned()).collect(),
                    layer: event.layer.clone(),
                    sensitivity: event.sensitivity.clone(),
                    family: event.family.clone(),
                    payload,
                })
                .map_err(|e| e.to_string())?;
            renamed.insert(event.event_hash.clone(), carried.event_hash);
        }
    }
    let ctx = crate::state::context_for(state, &pending.keep)?;
    if ctx.worktree_path.trim().is_empty() {
        let main = crate::state::context_for(state, &home)?;
        let folder = crate::node::WorkspaceSet {
            worktree_path: main.worktree_path,
            branch: main.branch,
            repo_root: main.repo_root,
            created_by_bakery: false,
            fork_note: None,
        };
        let fact = serde_json::to_string(&folder).map_err(|e| e.to_string())?;
        crate::state::append(state, &pending.keep, "human", crate::node::SOCKET_WORKSPACE_FAMILY, fact, vec![])?;
    }
    crate::routes::saved::keep_only(state, &pending.keep, &renamed)?;
    let done = serde_json::json!({ "kept": pending.keep, "home": home, "closed": pending.closed });
    std::fs::write(state.base.join(DONE), done.to_string()).map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::Seat;
    use crate::testutil::*;

    /// The plan names the thread called "local models", what it leaves and
    /// clears, and refuses while a turn runs or when it can't tell which
    /// thread to keep.
    #[test]
    fn the_plan_keeps_the_local_models_thread_and_refuses_while_a_turn_runs() {
        let dir = tempfile::tempdir().unwrap();
        let repo = bakery_repo_for_test(dir.path());
        let state = state_with_bakery_root(dir.path(), &repo);
        let home = crate::routes::baker::ensure_permanent_baker(&state, None, None).unwrap().stream_id;
        let open = |title: &str| {
            let req = crate::baker_threads::CreateThreadReq { title: Some(title.into()), ..Default::default() };
            crate::baker_threads::create_thread(&state, &req, "human", None, None).unwrap().thread.stream_id
        };
        let (local, other) = (open("Local models"), open("Styling"));
        state.node.lock().unwrap().always_kinds = vec![serde_json::from_value(serde_json::json!("read")).unwrap()];

        let planned = plan(&state, None);
        assert_eq!(planned.keep.as_ref().map(|t| t.stream_id.as_str()), Some(local.as_str()));
        assert_eq!((planned.left_behind, planned.always_allow, planned.refused.as_deref()), (2, 1, None), "the Baker and the other thread");
        assert_eq!(plan(&state, Some(&other)).keep.unwrap().title, "Styling", "the one named");
        open("local models, again");
        assert!(plan(&state, None).refused.unwrap().contains("more than one"));
        state.lifecycle.lock().unwrap().running_turns.insert((home, Seat::Baker), 1);
        assert!(plan(&state, Some(&local)).refused.unwrap().contains("1 turns are running"));
    }

    /// The old Heart goes aside whole and closed, under a name with the
    /// time, and the new one carries the kept thread with its history as
    /// it was, under a Baker minted fresh, its causes and saved replies
    /// carried over, the Always-allow grants cleared. An unknown thread
    /// leaves the Heart as it was.
    #[test]
    fn a_fresh_start_sets_the_old_heart_aside_and_carries_one_thread() {
        let dir = tempfile::tempdir().unwrap();
        let repo = bakery_repo_for_test(dir.path());
        let old = state_with_bakery_root(dir.path(), &repo);
        let old_home = crate::routes::baker::ensure_permanent_baker(&old, None, None).unwrap().stream_id;
        let req = crate::baker_threads::CreateThreadReq { title: Some("Local models".into()), ..Default::default() };
        let kept = crate::baker_threads::create_thread(&old, &req, "human", None, None).unwrap().thread.stream_id;
        let asked = crate::state::append(&old, &kept, "human", "agent.turn.started", r#"{"turn_id":"t1","instruction":"compare"}"#.into(), vec![]).unwrap();
        let said = format!(r#"{{"turn_id":"t1","answering":"{}"}}"#, asked.event_hash);
        let done = crate::state::append(&old, &kept, "agent:baker", "agent.turn.done", said, vec![asked.event_hash.clone()]).unwrap();
        let saved = crate::routes::saved::SaveReq { stream_id: kept.clone(), event_hash: done.event_hash.clone() };
        crate::routes::saved::save(&old, &saved).unwrap();

        // The old Heart as a file, as the node keeps it.
        let base = dir.path().join("base");
        std::fs::create_dir_all(&base).unwrap();
        {
            let mut file = bakery_heart::Heart::open(&base.join("heart.db").to_string_lossy()).unwrap();
            let heart = old.heart.lock().unwrap();
            for stream in [old_home.as_str(), kept.as_str()] {
                for e in heart.read_stream(stream).unwrap() {
                    let new = NewEvent { stream_id: e.stream_id, actor: e.actor, ts: e.ts, causality: vec![], layer: e.layer, sensitivity: e.sensitivity, family: e.family, payload: e.payload };
                    file.append(new).unwrap();
                }
            }
        }
        std::fs::write(base.join(MARKER), r#"{"keep":"no-such-thread"}"#).unwrap();
        assert!(set_aside(&base).is_none());
        assert!(base.join("heart.db").exists() && base.join(FAILED).exists() && !base.join(MARKER).exists(), "as it was, and why");

        std::fs::write(base.join(MARKER), serde_json::json!({ "keep": kept }).to_string()).unwrap();
        let pending = set_aside(&base).unwrap();
        assert!(!base.join("heart.db").exists() && !base.join(MARKER).exists());
        let closed = pending.closed.clone();
        assert!(closed.file_name().unwrap().to_string_lossy().starts_with("heart-") && closed.exists(), "kept aside");
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&closed).unwrap().permissions().mode() & 0o777, 0o400, "closed");

        // The node's new start, on a new Heart, with the old one's thread
        // events (their own hashes, as the old Heart has them).
        let again = tempfile::tempdir().unwrap();
        let fresh = state_with_bakery_root(again.path(), &repo);
        std::fs::copy(old.base.join("saved.json"), fresh.base.join("saved.json")).unwrap();
        fresh.node.lock().unwrap().always_kinds = vec![serde_json::from_value(serde_json::json!("read")).unwrap()];
        let events = old.heart.lock().unwrap().read_stream(&kept).unwrap();
        carry(&fresh, &Pending { keep: kept.clone(), events, closed }).unwrap();

        let home = crate::baker_threads::baker_pointer(&fresh).unwrap();
        assert_ne!(home, old_home, "a Baker minted fresh");
        assert!(crate::baker_threads::is_baker_thread_stream(&fresh, &kept), "the kept thread is the new Baker's");
        let carried = fresh.heart.lock().unwrap().read_stream(&kept).unwrap();
        assert_eq!(carried.len(), old.heart.lock().unwrap().read_stream(&kept).unwrap().len(), "its whole history");
        let finished = carried.iter().find(|e| e.family == "agent.turn.done").unwrap();
        let started = carried.iter().find(|e| e.family == "agent.turn.started").unwrap();
        assert_eq!(finished.causality, [started.event_hash.clone()], "its causes carried over");
        assert!(finished.payload.contains(&started.event_hash), "and the facts it names");
        let saved = crate::routes::saved::list(&fresh, None).unwrap().items;
        assert_eq!(saved.iter().map(|s| s.event_hash.as_str()).collect::<Vec<_>>(), [finished.event_hash.as_str()]);
        assert!(fresh.node.lock().unwrap().always_kinds.is_empty(), "the Always-allow grants cleared");
        let copies = std::fs::read_dir(&*fresh.base).unwrap().flatten().filter(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.starts_with("saved-") && name.ends_with(".json")
        });
        assert_eq!(copies.count(), 1, "every saved reply kept on disk before the others go");
        let done: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(fresh.base.join(DONE)).unwrap()).unwrap();
        assert_eq!((done["kept"].as_str(), done["home"].as_str()), (Some(kept.as_str()), Some(home.as_str())));
    }

    /// A fresh start that fails after the old Heart moved puts everything
    /// back: the old Heart as heart.db (0600), node.json as it was, the new
    /// Heart aside, and why.
    #[test]
    fn a_failed_fresh_start_puts_everything_back() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        let closed = base.join("heart-20261013-120000.db");
        std::fs::write(&closed, "old heart").unwrap();
        std::fs::write(base.join("heart.db"), "new heart").unwrap();
        std::fs::write(base.join("node.json"), "changed").unwrap();
        std::fs::write(base.join(NODE_BEFORE), "as it was").unwrap();
        roll_back(base, &Pending { keep: "wt:kept".into(), events: vec![], closed: closed.clone() }, "carry failed");
        assert_eq!(std::fs::read_to_string(base.join("heart.db")).unwrap(), "old heart");
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(base.join("heart.db")).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::read_to_string(base.join("node.json")).unwrap(), "as it was");
        assert!(!closed.exists());
        let aside: Vec<_> = std::fs::read_dir(base).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().starts_with("heart-failed-")).collect();
        assert_eq!(aside.len(), 1);
        assert_eq!(std::fs::read_to_string(aside[0].path()).unwrap(), "new heart");
        assert!(std::fs::read_to_string(base.join(FAILED)).unwrap().contains("carry failed"));
    }

    #[test]
    fn only_whole_hashes_carry_over() {
        let old = "a".repeat(64);
        let renamed = HashMap::from([(old.clone(), "b".repeat(64))]);
        let text = format!(r#"{{"x":"{old}","y":"{old}0","z":"ab"}}"#);
        assert_eq!(renamed_in(&text, &renamed), format!(r#"{{"x":"{}","y":"{old}0","z":"ab"}}"#, "b".repeat(64)));
    }
}
