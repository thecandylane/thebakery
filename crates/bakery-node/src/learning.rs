//! The memory's learning loop, every part of it the operator's to see and
//! change in the Bakery (`/learning`: hidden from agents, open to the
//! phone). Its parts:
//! - the thread-end review: a thread that ends (archived or merged), or that
//!   worked today, has its turns read by its own model (or one the operator
//!   picks), which proposes what is worth remembering;
//! - the nightly run, once a day when the Mac is idle and cool: the
//!   thread-end sweep, then reflection, replays, lifting and the digest;
//! - reflection: where one kind of work in a project both passed and failed
//!   lately, the reviewer model proposes the lesson in the difference;
//! - replays: a lesson's teaching turn runs again in a fresh copy of the
//!   project, once with the lesson and once without, and the reviewer model
//!   judges whether it helped;
//! - lifting: a lesson seen in two projects is proposed for the whole
//!   Bakery, shared only once the operator keeps it;
//! - the digest: what waits for the operator, what went stale, and the day.
//!
//! Each part on or off, every trigger, time and limit, the models, the
//! projects in the loop, a pause, run now, and the history of every run
//! with the notes it touched and the model calls it spent. Nothing it
//! proposes is kept without the operator. Every model call is an ordinary
//! turn of a scratch thread on a fresh worktree of the project, inside the
//! Bakery's rooms and doors like any thread, then put away; never anything
//! run as the operator.

use crate::agent_control::OperatingMode;
use crate::memory_index::{self as index, Note};
use crate::node::Seat;
use crate::state::AppState;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Local, NaiveDate, NaiveTime, TimeZone};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

/// The loop's own history and settings.
pub(crate) const STREAM: &str = "sys:learning";
/// Who the loop's proposals and ratings are by.
pub(crate) const ACTOR: &str = "learning";
/// A thread the loop opened for one model call: `{run}`, on the thread.
pub(crate) const SCRATCH: &str = "learning.scratch";
/// What a scratch thread asked for and was refused: `{what, why}`.
pub(crate) const REFUSED: &str = "learning.refused";
const SETTINGS_SET: &str = "learning.settings.set";
const RUN_STARTED: &str = "learning.run.started";
const RUN_ENDED: &str = "learning.run.ended";
const PARTS: &[&str] = &["thread_end", "nightly", "reflect", "replay", "lift", "digest"];
/// How often the loop looks whether a part is due.
const LOOK_EVERY: Duration = Duration::from_secs(60);
/// The longest a scratch turn may take before it is stopped.
const TURN_LIMIT: Duration = Duration::from_secs(20 * 60);
/// What a review reads of a thread: its last turns, each cut short.
const REVIEW_TURNS: usize = 20;
const TURN_CHARS: usize = 3000;

#[derive(schemars::JsonSchema, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct ThreadEnd {
    pub(crate) on: bool,
    /// When a thread is archived.
    pub(crate) on_archive: bool,
    /// When a thread's work is merged.
    pub(crate) on_merge: bool,
    /// At the nightly run, for each thread that worked that day.
    pub(crate) in_nightly: bool,
    /// The fewest turns since its last review that make a thread worth one.
    pub(crate) min_turns: u32,
    /// The most notes one review proposes.
    pub(crate) max_proposals: u32,
    /// The model it runs on; the thread's own when absent.
    pub(crate) model: Option<String>,
}

impl Default for ThreadEnd {
    fn default() -> Self {
        ThreadEnd { on: true, on_archive: true, on_merge: true, in_nightly: true, min_turns: 3, max_proposals: 3, model: None }
    }
}

#[derive(schemars::JsonSchema, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Nightly {
    pub(crate) on: bool,
    /// When it starts, local time (HH:MM).
    pub(crate) at: String,
    /// Until when it may still start, retrying while the Mac is busy or hot.
    pub(crate) retry_until: String,
    pub(crate) retry_every_minutes: u32,
    /// It waits while any agent's turn runs.
    pub(crate) wait_while_busy: bool,
    /// It waits while the Mac is hot (agents' new turns wait then too).
    pub(crate) wait_while_hot: bool,
}

impl Default for Nightly {
    fn default() -> Self {
        Nightly {
            on: true,
            at: "03:00".to_string(),
            retry_until: "07:00".to_string(),
            retry_every_minutes: 60,
            wait_while_busy: true,
            wait_while_hot: true,
        }
    }
}

#[derive(schemars::JsonSchema, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Reflect {
    pub(crate) on: bool,
    /// How far back it looks for one kind of work that both passed and
    /// failed.
    pub(crate) window_days: u32,
    /// The most lessons one run proposes.
    pub(crate) max_proposals: u32,
}

impl Default for Reflect {
    fn default() -> Self {
        Reflect { on: true, window_days: 7, max_proposals: 3 }
    }
}

#[derive(schemars::JsonSchema, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Replay {
    pub(crate) on: bool,
    /// The most lessons replayed in one run.
    pub(crate) cap: u32,
    /// Which lessons first: newest, oldest, most_used or least_proven.
    pub(crate) order: String,
}

impl Default for Replay {
    fn default() -> Self {
        Replay { on: true, cap: 5, order: "newest".to_string() }
    }
}

#[derive(schemars::JsonSchema, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Lift {
    pub(crate) on: bool,
    /// In how many projects alike lessons must be before they are proposed
    /// for the whole Bakery.
    pub(crate) min_projects: u32,
}

impl Default for Lift {
    fn default() -> Self {
        Lift { on: true, min_projects: 2 }
    }
}

#[derive(schemars::JsonSchema, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Digest {
    pub(crate) on: bool,
    /// A Mac notification with each digest.
    pub(crate) notify: bool,
}

impl Default for Digest {
    fn default() -> Self {
        Digest { on: true, notify: true }
    }
}

/// Everything about the loop the operator decides.
#[derive(schemars::JsonSchema, Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct LoopSettings {
    /// Nothing runs on its own while paused; run now still runs.
    pub(crate) paused: bool,
    /// The model reflection and replays run on; they wait until one is
    /// picked.
    pub(crate) reviewer_model: Option<String>,
    /// Projects left out of the loop, by id.
    pub(crate) projects_out: Vec<String>,
    pub(crate) thread_end: ThreadEnd,
    pub(crate) nightly: Nightly,
    pub(crate) reflect: Reflect,
    pub(crate) replay: Replay,
    pub(crate) lift: Lift,
    pub(crate) digest: Digest,
}

#[derive(schemars::JsonSchema, Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ModelCall {
    pub(crate) model: String,
    /// What it was asked for.
    pub(crate) purpose: String,
    /// The thread it was about.
    pub(crate) thread: Option<String>,
    pub(crate) tokens_in: Option<u64>,
    pub(crate) tokens_out: Option<u64>,
    pub(crate) ok: bool,
}

/// One run of one part of the loop.
#[derive(schemars::JsonSchema, Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct LearningRun {
    pub(crate) id: String,
    /// thread_end, nightly, reflect, replay, lift or digest.
    pub(crate) part: String,
    /// archive, merge, nightly or run_now.
    pub(crate) trigger: String,
    pub(crate) started_at: String,
    pub(crate) ended_at: Option<String>,
    /// running, done, skipped or failed.
    pub(crate) status: String,
    /// What it did, or why it didn't.
    pub(crate) said: String,
    pub(crate) thread: Option<String>,
    pub(crate) project: Option<String>,
    /// The notes it touched.
    pub(crate) notes: Vec<String>,
    /// The proposals it made.
    pub(crate) proposals: Vec<String>,
    pub(crate) model_calls: Vec<ModelCall>,
    /// What its scratch threads asked for and were refused: a push, a pull
    /// request, a new site, an install, a question for the operator.
    pub(crate) refused: Vec<String>,
}

#[derive(schemars::JsonSchema, Debug, Clone, Serialize)]
pub(crate) struct LoopModel {
    pub(crate) id: String,
    pub(crate) label: String,
    /// http, acp or openai-compat.
    pub(crate) runtime: String,
    pub(crate) available: bool,
    pub(crate) reason: Option<String>,
}

/// Each part's last run.
#[derive(schemars::JsonSchema, Debug, Default, Serialize)]
pub(crate) struct LastRuns {
    pub(crate) thread_end: Option<LearningRun>,
    pub(crate) nightly: Option<LearningRun>,
    pub(crate) reflect: Option<LearningRun>,
    pub(crate) replay: Option<LearningRun>,
    pub(crate) lift: Option<LearningRun>,
    pub(crate) digest: Option<LearningRun>,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct Learning {
    pub(crate) settings: LoopSettings,
    pub(crate) changed_at: Option<String>,
    /// operator (none: never changed).
    pub(crate) changed_by: Option<String>,
    /// When the nightly run next starts; none while it is off or paused.
    pub(crate) next_nightly: Option<String>,
    /// Why parts won't run now.
    pub(crate) held: Vec<String>,
    /// The models the loop may ask.
    pub(crate) models: Vec<LoopModel>,
    pub(crate) last: LastRuns,
}

/// A thread the loop opened for one model call. It asks no one and
/// reaches nothing outward: no card for the operator, no push, no pull
/// request, no new site, no install, no note or message; only its own work
/// in its own worktree and room.
pub(crate) fn scratch(state: &AppState, stream_id: &str) -> bool {
    state.heart.lock().unwrap().last_of_family(stream_id, SCRATCH).ok().flatten().is_some()
}

/// Refuses what a scratch thread asked for, with no card: recorded on it
/// once, for its run. The answer is what the asker is told.
pub(crate) fn refuse(state: &AppState, stream_id: &str, what: &str) -> String {
    let why = "a learning scratch thread asks no one and reaches nothing outward: refused";
    let said = state.heart.lock().unwrap().events_of_family(stream_id, REFUSED).unwrap_or_default();
    if !said.iter().any(|e| serde_json::from_str::<Value>(&e.payload).is_ok_and(|p| p["what"] == what)) {
        let fact = json!({"what": what, "why": why}).to_string();
        let _ = crate::state::append(state, stream_id, ACTOR, REFUSED, fact, vec![]);
    }
    format!("{what}: {why}")
}

/// The Bakery tools a scratch thread may use besides those that only read:
/// its own commands, jobs and service copies in its own room, all stopped
/// when it is put away. Not previews: they show in the app and go anywhere.
pub(crate) const SCRATCH_TOOLS: &[&str] = &["env_exec", "job_start", "job_stop", "service_copy", "service_copy_stop"];

fn operator_only() -> Result<(), String> {
    match crate::actor::agent_calling() {
        true => Err("only the operator manages the learning loop".to_string()),
        false => Ok(()),
    }
}

/// The settings as last set, with when and by whom.
pub(crate) fn settings(state: &AppState) -> (LoopSettings, Option<String>, Option<String>) {
    let last = state.heart.lock().unwrap().last_of_family(STREAM, SETTINGS_SET).ok().flatten();
    match last {
        Some(e) => {
            let fact: Value = serde_json::from_str(&e.payload).unwrap_or_default();
            let settings = serde_json::from_value(fact["settings"].clone()).unwrap_or_default();
            (settings, Some(e.ts), Some(index::who(&e.actor)))
        }
        None => (LoopSettings::default(), None, None),
    }
}

/// `patch` laid over `base`, object by object.
fn merge(base: &mut Value, patch: &Value) {
    match (base, patch) {
        (Value::Object(base), Value::Object(patch)) => {
            for (key, value) in patch {
                merge(base.entry(key.clone()).or_insert(Value::Null), value);
            }
        }
        (base, patch) => *base = patch.clone(),
    }
}

fn time_of(text: &str) -> Option<NaiveTime> {
    NaiveTime::parse_from_str(text.trim(), "%H:%M").ok()
}

fn check(state: &AppState, s: &LoopSettings) -> Result<(), String> {
    let within = |what: &str, n: u32, low: u32, high: u32| match (low..=high).contains(&n) {
        true => Ok(()),
        false => Err(format!("{what} is {low} to {high}")),
    };
    within("the fewest turns", s.thread_end.min_turns, 1, 1000)?;
    within("a review's most proposals", s.thread_end.max_proposals, 1, 10)?;
    within("the retry gap", s.nightly.retry_every_minutes, 5, 720)?;
    within("reflection's window", s.reflect.window_days, 1, 90)?;
    within("reflection's most proposals", s.reflect.max_proposals, 1, 10)?;
    within("the replay cap", s.replay.cap, 0, 50)?;
    within("lifting's projects", s.lift.min_projects, 2, 10)?;
    for (what, time) in [("the nightly time", &s.nightly.at), ("the retry window's end", &s.nightly.retry_until)] {
        time_of(time).ok_or_else(|| format!("{what} is a time like 03:00"))?;
    }
    if !["newest", "oldest", "most_used", "least_proven"].contains(&s.replay.order.as_str()) {
        return Err("the replay order is newest, oldest, most_used or least_proven".to_string());
    }
    let known: BTreeSet<String> = crate::routes::studio::model_options(state).into_iter().map(|m| m.id).collect();
    for model in s.reviewer_model.iter().chain(&s.thread_end.model) {
        if !known.contains(model) {
            return Err(format!("{model} is no model this node knows"));
        }
    }
    let projects: BTreeSet<String> = crate::catalog::known_projects(state)?.into_iter().map(|p| p.project_id).collect();
    match s.projects_out.iter().find(|p| !projects.contains(*p)) {
        Some(p) => Err(format!("no project {p}")),
        None => Ok(()),
    }
}

/// The operator changes any of the settings: `patch` is laid over them.
pub(crate) fn set(state: &AppState, patch: &Value) -> Result<Learning, String> {
    operator_only()?;
    let mut merged = serde_json::to_value(settings(state).0).map_err(|e| e.to_string())?;
    merge(&mut merged, patch);
    let next: LoopSettings = serde_json::from_value(merged).map_err(|e| format!("not settings the loop has: {e}"))?;
    check(state, &next)?;
    crate::state::append(state, STREAM, "human", SETTINGS_SET, json!({"settings": next}).to_string(), vec![])?;
    tell(state, "settings", None);
    learning(state)
}

fn tell(state: &AppState, what: &str, run: Option<&LearningRun>) {
    let _ = state.tx.send(json!({"kind": "learning.updated", "what": what, "run": run}).to_string());
}

/// Every run, each as it last stood, the oldest first.
fn all_runs(state: &AppState) -> Vec<LearningRun> {
    let events = state.heart.lock().unwrap().read_stream(STREAM).unwrap_or_default();
    let mut runs: Vec<LearningRun> = Vec::new();
    for e in events.iter().filter(|e| e.family == RUN_STARTED || e.family == RUN_ENDED) {
        let fact: Value = serde_json::from_str(&e.payload).unwrap_or_default();
        let Ok(run) = serde_json::from_value::<LearningRun>(fact["run"].clone()) else { continue };
        match runs.iter_mut().find(|r| r.id == run.id) {
            Some(had) => *had = run,
            None => runs.push(run),
        }
    }
    runs
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct RunsQuery {
    /// Only this part's runs.
    #[serde(default)]
    pub(crate) part: Option<String>,
    /// At most this many; 100 when absent.
    #[serde(default)]
    pub(crate) limit: Option<usize>,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct LearningRuns {
    /// The newest first.
    pub(crate) runs: Vec<LearningRun>,
}

pub(crate) fn runs(state: &AppState, q: &RunsQuery) -> Result<LearningRuns, String> {
    operator_only()?;
    let limit = q.limit.unwrap_or(100).clamp(1, 1000);
    let runs = all_runs(state).into_iter().rev().filter(|r| q.part.as_ref().is_none_or(|p| *p == r.part)).take(limit).collect();
    Ok(LearningRuns { runs })
}

/// The nightly window that `day` opens, local time; one that ends before it
/// starts ends the next day.
fn window(s: &Nightly, day: NaiveDate) -> Option<(DateTime<Local>, DateTime<Local>)> {
    let start = Local.from_local_datetime(&day.and_time(time_of(&s.at)?)).earliest()?;
    let mut end = Local.from_local_datetime(&day.and_time(time_of(&s.retry_until)?)).earliest()?;
    if end <= start {
        end += chrono::Duration::days(1);
    }
    Some((start, end))
}

/// The window `now` is in: today's, or yesterday's when it runs past
/// midnight.
fn current_window(s: &Nightly, now: DateTime<Local>) -> Option<(DateTime<Local>, DateTime<Local>)> {
    let today = now.date_naive();
    [today.pred_opt(), Some(today)].into_iter().flatten().filter_map(|day| window(s, day)).find(|(start, end)| *start <= now && now < *end)
}

/// The scheduled nightly run already started in this window.
fn ran_in(runs: &[LearningRun], (start, end): (DateTime<Local>, DateTime<Local>)) -> bool {
    runs.iter().filter(|r| r.part == "nightly" && r.trigger == "nightly").any(|r| {
        DateTime::parse_from_rfc3339(&r.started_at).is_ok_and(|at| {
            let at = at.with_timezone(&Local);
            start <= at && at < end
        })
    })
}

fn next_nightly(s: &LoopSettings, runs: &[LearningRun], now: DateTime<Local>) -> Option<DateTime<Local>> {
    if s.paused || !s.nightly.on {
        return None;
    }
    if let Some(window) = current_window(&s.nightly, now).filter(|w| !ran_in(runs, *w)) {
        return Some(now.max(window.0));
    }
    let today = now.date_naive();
    [Some(today), today.succ_opt()].into_iter().flatten().filter_map(|day| window(&s.nightly, day)).map(|(start, _)| start).find(|start| *start > now)
}

/// The loop as the operator sees it: its settings, what holds parts back,
/// the models it may ask, and each part's last run.
pub(crate) fn learning(state: &AppState) -> Result<Learning, String> {
    operator_only()?;
    let (settings, changed_at, changed_by) = settings(state);
    let runs = all_runs(state);
    let models: Vec<LoopModel> = crate::routes::studio::model_options(state)
        .into_iter()
        .map(|m| LoopModel { id: m.id, label: m.label, runtime: m.transport, available: m.node_available, reason: m.reason })
        .collect();
    let mut held = Vec::new();
    if settings.paused {
        held.push("paused: nothing runs on its own; run now still runs".to_string());
    }
    match settings.reviewer_model.as_ref().map(|id| models.iter().find(|m| &m.id == id)) {
        None => held.push("the reviewer model isn't picked: reflection and replays wait for it".to_string()),
        Some(Some(model)) if !model.available => {
            held.push(format!("the reviewer model can't run now: {}", model.reason.clone().unwrap_or_default()))
        }
        _ => {}
    }
    for (part, on) in [
        ("the thread-end review", settings.thread_end.on),
        ("the nightly run", settings.nightly.on),
        ("reflection", settings.reflect.on),
        ("replays", settings.replay.on),
        ("lifting between projects", settings.lift.on),
        ("the digest", settings.digest.on),
    ] {
        if !on {
            held.push(format!("{part} is off"));
        }
    }
    let last_of = |part: &str| runs.iter().rev().find(|r| r.part == part).cloned();
    let last = LastRuns {
        thread_end: last_of("thread_end"),
        nightly: last_of("nightly"),
        reflect: last_of("reflect"),
        replay: last_of("replay"),
        lift: last_of("lift"),
        digest: last_of("digest"),
    };
    let next_nightly = next_nightly(&settings, &runs, Local::now()).map(|at| at.to_rfc3339());
    Ok(Learning { settings, changed_at, changed_by, next_nightly, held, models, last })
}

/// A run's start, in the Heart and live: for a thread, or for one project
/// (none: every project in the loop).
fn start(state: &AppState, part: &str, trigger: &str, thread: Option<String>, project: Option<String>) -> LearningRun {
    let of_thread = || thread.as_deref().and_then(|t| crate::state::socket_project(&state.heart.lock().unwrap(), t).0);
    let project = project.or_else(of_thread);
    let run = LearningRun {
        id: crate::node::mint_id("learn"),
        part: part.to_string(),
        trigger: trigger.to_string(),
        started_at: chrono::Utc::now().to_rfc3339(),
        status: "running".to_string(),
        thread,
        project,
        ..LearningRun::default()
    };
    let _ = crate::state::append(state, STREAM, ACTOR, RUN_STARTED, json!({"run": run}).to_string(), vec![]);
    tell(state, "run", Some(&run));
    run
}

/// What a part came to.
enum Came {
    Done(String),
    Skipped(String),
}

fn finish(state: &AppState, mut run: LearningRun, came: Result<Came, String>) -> LearningRun {
    (run.status, run.said) = match came {
        Ok(Came::Done(said)) => ("done".to_string(), said),
        Ok(Came::Skipped(why)) => ("skipped".to_string(), why),
        Err(why) => ("failed".to_string(), why),
    };
    run.ended_at = Some(chrono::Utc::now().to_rfc3339());
    let _ = crate::state::append(state, STREAM, ACTOR, RUN_ENDED, json!({"run": run}).to_string(), vec![]);
    tell(state, "run", Some(&run));
    run
}

/// A started run carried out to its end.
fn carry_out(state: &AppState, s: &LoopSettings, mut run: LearningRun) -> LearningRun {
    let came = match run.part.clone().as_str() {
        "thread_end" => match run.thread.clone() {
            Some(thread) => thread_end(state, s, &thread, &mut run),
            None => Err("a thread-end review needs its thread".to_string()),
        },
        "reflect" => reflect(state, s, &mut run),
        "replay" => replay(state, s, &mut run),
        "lift" => lift(state, s, &mut run),
        "digest" => digest(state, s, &mut run),
        "nightly" => nightly(state, s, &mut run),
        other => Err(format!("no part {other}")),
    };
    finish(state, run, came)
}

fn run_part(state: &AppState, s: &LoopSettings, part: &str, trigger: &str, thread: Option<String>, project: Option<String>) -> LearningRun {
    carry_out(state, s, start(state, part, trigger, thread, project))
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct RunReq {
    /// thread_end, nightly, reflect, replay, lift or digest.
    pub(crate) part: String,
    /// The thread a thread-end review reads.
    #[serde(default)]
    pub(crate) thread: Option<String>,
    /// Only this project's threads, kinds of work and lessons; every
    /// project in the loop when absent.
    #[serde(default)]
    pub(crate) project: Option<String>,
}

/// The operator runs a part now, whatever the pause and the time; a part
/// that is off stays off. The run is answered as it starts, and goes on
/// behind the answer.
pub(crate) fn run_now(state: &AppState, req: &RunReq) -> Result<LearningRun, String> {
    operator_only()?;
    let (s, _, _) = settings(state);
    let part = req.part.trim();
    let on = match part {
        "thread_end" => s.thread_end.on,
        "nightly" => s.nightly.on,
        "reflect" => s.reflect.on,
        "replay" => s.replay.on,
        "lift" => s.lift.on,
        "digest" => s.digest.on,
        other => return Err(format!("no part {other}: {}", PARTS.join(", "))),
    };
    if !on {
        return Err(format!("{part} is off: turn it on first"));
    }
    let thread = req.thread.as_deref().map(str::trim).filter(|t| !t.is_empty()).map(str::to_string);
    if part == "thread_end" {
        let thread = thread.as_deref().ok_or("a thread-end review needs its thread")?;
        crate::state::context_for(state, thread).map_err(|_| format!("no thread {thread}"))?;
    }
    let project = req.project.as_deref().map(str::trim).filter(|p| !p.is_empty()).map(str::to_string);
    if let Some(project) = &project {
        if !crate::catalog::known_projects(state)?.iter().any(|p| &p.project_id == project) {
            return Err(format!("no project {project}"));
        }
        if s.projects_out.contains(project) {
            return Err(format!("{project} is out of the loop: bring it back in first"));
        }
        let of_thread = thread.as_deref().and_then(|t| crate::state::socket_project(&state.heart.lock().unwrap(), t).0);
        if part == "thread_end" && of_thread.as_ref() != Some(project) {
            return Err(format!("that thread isn't in {project}"));
        }
    }
    let run = start(state, part, "run_now", thread, project);
    let answer = run.clone();
    let state = state.clone();
    std::thread::spawn(move || carry_out(&state, &s, run));
    Ok(answer)
}

/// The text of a model's answer: the JSON object in it.
fn json_in(text: &str) -> Option<Value> {
    let (from, to) = (text.find('{')?, text.rfind('}')?);
    serde_json::from_str(text.get(from..=to)?).ok()
}

fn cut(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text.to_string(),
    }
}

/// What a scratch turn is: on which model, in which repository, in which
/// mode, asked what, and about which thread.
struct Ask {
    model: String,
    repo: String,
    mode: OperatingMode,
    purpose: &'static str,
    prompt: String,
    about: Option<String>,
}

/// A scratch thread's turn on the asked model, in a fresh worktree of its
/// repository (inside the project's room and door, like any thread), then
/// put away: its answer, with the call in the run's account. An empty model
/// keeps the scratch thread's own (tests).
fn ask(state: &AppState, run: &mut LearningRun, asked: Ask) -> Result<String, String> {
    let short = crate::node::mint_id("x").chars().rev().take(6).collect::<String>();
    let name = format!("learning-{}-{short}", asked.purpose.replace(' ', "-"));
    let req = crate::routes::project::OpenReq { repo: asked.repo.clone(), name: Some(name), ..Default::default() };
    let scratch = crate::routes::project::open_stream(state, &req)?.context;
    let mark = json!({"run": run.id}).to_string();
    crate::state::append(state, &scratch.stream_id, ACTOR, SCRATCH, mark, vec![])?;
    let seat = Seat::Orchestrator;
    let answer = (|| -> Result<String, String> {
        if !asked.model.is_empty() {
            crate::routes::seats::seat_assign_checked(state, &scratch.stream_id, seat, &asked.model).map_err(|(_, why)| why)?;
        }
        crate::agent_control::set_operating_mode(state, &scratch.stream_id, seat, asked.mode)?;
        let turn = crate::routes::agent::AgentTurnReq {
            stream_id: scratch.stream_id.clone(),
            instruction: asked.prompt.clone(),
            seat: Some(seat),
            ..Default::default()
        };
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let turn_state = state.clone();
        std::thread::spawn(move || {
            let _ = done_tx.send(crate::routes::agent::agent_turn_inner(&turn_state, &turn).map_err(|(_, why)| why));
        });
        let done = match done_rx.recv_timeout(TURN_LIMIT) {
            Ok(done) => done,
            Err(_) => {
                crate::state::request_cancel(state, &crate::node::agent_id(&scratch.stream_id, seat));
                done_rx.recv_timeout(Duration::from_secs(60)).unwrap_or_else(|_| Err("stopped: it ran too long".to_string()))
            }
        }?;
        let said: Value = serde_json::from_str(&done.payload).unwrap_or_default();
        Ok(said["summary"].as_str().unwrap_or_default().to_string())
    })();
    let usage = state.heart.lock().unwrap().last_of_family(&scratch.stream_id, "agent.turn.usage").ok().flatten();
    let usage: Value = usage.map(|e| serde_json::from_str(&e.payload).unwrap_or_default()).unwrap_or_default();
    let refused = state.heart.lock().unwrap().events_of_family(&scratch.stream_id, REFUSED).unwrap_or_default();
    run.refused.extend(refused.iter().filter_map(|e| serde_json::from_str::<Value>(&e.payload).ok()?["what"].as_str().map(str::to_string)));
    run.model_calls.push(ModelCall {
        model: if asked.model.is_empty() { "the thread's own".to_string() } else { asked.model.clone() },
        purpose: asked.purpose.to_string(),
        thread: asked.about.clone(),
        tokens_in: usage["tokens_in"].as_u64(),
        tokens_out: usage["tokens_out"].as_u64(),
        ok: answer.is_ok(),
    });
    put_away(state, &scratch.stream_id, &scratch.repo_root, &scratch.branch);
    answer
}

/// A scratch thread put away as any thread is (its session, worktree and
/// room), and its scratch branch with it.
fn put_away(state: &AppState, stream_id: &str, repo_root: &str, branch: &str) {
    let req = crate::routes::lifecycle::DiscardReq { stream_id: stream_id.to_string(), confirm: true };
    let work = crate::routes::lifecycle::discard_stream(State(state.clone()), Json(req));
    if let Ok(runtime) = tokio::runtime::Builder::new_current_thread().enable_all().build() {
        let _ = runtime.block_on(work);
    }
    if branch.starts_with("learning-") {
        let _ = crate::git::run(std::path::Path::new(repo_root), &["branch", "-D", branch]);
    }
}

const RULES: &str = "Propose only what will still matter in six months and is hard to find again from the code, git \
or the conversation. Never: a secret; task progress, a todo or a log of finished work; what the code or its docs \
already say; the story of a one-off task; a setup failure the operator can fix (a missing tool, an unset \
credential); a claim that a tool or feature doesn't work; an error that went away on a retry; attempts that never \
worked. Write the body as plain facts, not orders to yourself; the steps go in how_to_apply, with {variables} where \
they vary. Answer with the JSON alone; use no tools and change nothing.";

/// The thread's last turns, the operator's words and the agent's, oldest
/// first, and the last turn's end (the review's evidence).
fn transcript(state: &AppState, thread: &str) -> (String, Option<String>) {
    let events = state.heart.lock().unwrap().read_stream(thread).unwrap_or_default();
    let payload = |e: &bakery_core::Event| serde_json::from_str::<Value>(&e.payload).unwrap_or_default();
    let asked: BTreeMap<String, String> = events
        .iter()
        .filter(|e| e.family == "agent.turn.started")
        .map(|e| {
            let p = payload(e);
            (p["turn_id"].as_str().unwrap_or_default().to_string(), p["instruction"].as_str().unwrap_or_default().to_string())
        })
        .collect();
    let done: Vec<(String, String, String)> = events
        .iter()
        .filter(|e| e.family == "agent.turn.done")
        .map(|e| {
            let p = payload(e);
            (p["turn_id"].as_str().unwrap_or_default().to_string(), p["summary"].as_str().unwrap_or_default().to_string(), e.event_hash.clone())
        })
        .collect();
    let last = done.last().map(|(_, _, hash)| hash.clone());
    let mut text = String::new();
    for (at, (turn, answer, _)) in done.iter().rev().take(REVIEW_TURNS).rev().enumerate() {
        let question = asked.get(turn).map(String::as_str).unwrap_or_default();
        text.push_str(&format!(
            "[Turn {}] The operator asked:\n{}\n[Turn {}] The agent answered:\n{}\n\n",
            at + 1,
            cut(question, TURN_CHARS),
            at + 1,
            cut(answer, TURN_CHARS)
        ));
    }
    (text, last)
}

/// How many turns a thread finished since its last thread-end review.
fn turns_since_review(state: &AppState, thread: &str) -> usize {
    let reviewed = all_runs(state)
        .into_iter()
        .filter(|r| r.part == "thread_end" && r.status == "done" && r.thread.as_deref() == Some(thread))
        .map(|r| r.started_at)
        .max();
    let done = state.heart.lock().unwrap().events_of_family(thread, "agent.turn.done").unwrap_or_default();
    done.iter().filter(|e| reviewed.as_ref().is_none_or(|at| e.ts > *at)).count()
}

/// A thread's own model: its working seat's.
fn own_model(state: &AppState, thread: &str) -> Option<String> {
    let ctx = crate::state::context_for(state, thread).ok()?;
    let seat = crate::routes::agent::default_seat_for(state, &ctx);
    crate::state::latest_seat_model_id(&state.heart.lock().unwrap(), thread, seat).ok().flatten()
}

fn thread_end(state: &AppState, s: &LoopSettings, thread: &str, run: &mut LearningRun) -> Result<Came, String> {
    let ctx = crate::state::context_for(state, thread).map_err(|_| format!("no thread {thread}"))?;
    if run.project.as_ref().is_some_and(|p| s.projects_out.contains(p)) {
        return Ok(Came::Skipped("its project is out of the loop".to_string()));
    }
    if ctx.repo_root.is_empty() {
        return Ok(Came::Skipped("a chat outside any repository has no copy to review in".to_string()));
    }
    let since = turns_since_review(state, thread);
    if since < s.thread_end.min_turns as usize {
        return Ok(Came::Skipped(format!("{since} turns since its last review; a review takes {}", s.thread_end.min_turns)));
    }
    // A test node's threads have no model of their own: the test's runs.
    let model = match s.thread_end.model.clone().or_else(|| own_model(state, thread)) {
        Some(model) => model,
        None if cfg!(test) => String::new(),
        None => return Ok(Came::Skipped("the thread has no model of its own".to_string())),
    };
    let (turns, last) = transcript(state, thread);
    let prompt = format!(
        "[Bakery memory: a review of a conversation, for what is worth remembering.]\n\nPropose at most {} notes for the \
         Bakery's memory. {RULES}\n\nAnswer: {{\"notes\": [{{\"type\": \"user|feedback|project|reference|skill|lesson\", \"title\": \
         \"…\", \"summary\": \"one line\", \"body\": \"…\", \"why\": \"what happened that taught it\", \"how_to_apply\": \"…\"}}]}}. \
         Nothing worth keeping: {{\"notes\": []}}.\n\nThe conversation, oldest first:\n\n{turns}",
        s.thread_end.max_proposals
    );
    let review = Ask { model, repo: ctx.repo_root.clone(), mode: OperatingMode::Plan, purpose: "thread-end review", prompt, about: Some(thread.to_string()) };
    let answer = ask(state, run, review)?;
    let notes = json_in(&answer).and_then(|v| v["notes"].as_array().cloned()).ok_or("the review's answer held no notes")?;
    let seat = crate::routes::agent::default_seat_for(state, &ctx);
    let (mut proposed, mut repeated) = (0, 0);
    for note in notes.iter().take(s.thread_end.max_proposals as usize) {
        let text = |key: &str| note[key].as_str().unwrap_or_default().to_string();
        let req = crate::memory_review::ProposeReq {
            kind: text("type"),
            title: text("title"),
            summary: text("summary"),
            body: text("body"),
            why: text("why"),
            how_to_apply: text("how_to_apply"),
            evidence: last.iter().map(|hash| json!({"event": hash})).collect(),
            supersedes: None,
            scope: None,
        };
        match crate::actor::with(crate::actor::agent(thread, seat), || crate::memory_review::propose(state, &req)) {
            Ok(p) if p.proposal.is_some() => {
                proposed += 1;
                run.proposals.extend(p.proposal);
                run.notes.extend(p.note);
            }
            Ok(p) => {
                repeated += 1;
                run.notes.extend(p.note);
            }
            Err(_) => repeated += 1,
        }
    }
    Ok(Came::Done(format!("read {since} turns; proposed {proposed}; {repeated} repeated what's kept, waiting or declined, or didn't hold")))
}

/// The kind of work a turn did, from the tools and commands it ran.
fn kind_of(work: &str) -> Option<&'static str> {
    let work = work.to_lowercase();
    [
        ("deploy", &["deploy", "release", "publish"][..]),
        ("tests", &["test", "pytest", "jest", "vitest", "spec"][..]),
        ("build", &["build", "compile", "cargo check", "tsc", "make "][..]),
        ("review", &["review", "gh pr", "pull request"][..]),
    ]
    .into_iter()
    .find(|(_, words)| words.iter().any(|w| work.contains(w)))
    .map(|(kind, _)| kind)
}

/// Each turn's tools and commands on a thread, as one text a turn.
fn work_of(state: &AppState, thread: &str) -> BTreeMap<String, String> {
    let events = state.heart.lock().unwrap().read_stream(thread).unwrap_or_default();
    let mut work: BTreeMap<String, String> = BTreeMap::new();
    for e in events.iter().filter(|e| e.family == "agent.tool.called" || e.family == "agent.acp.tool_call") {
        let p: Value = serde_json::from_str(&e.payload).unwrap_or_default();
        let Some(turn) = p["turn_id"].as_str() else { continue };
        let what = [&p["tool"], &p["summary"], &p["detail"]["title"], &p["detail"]["rawInput"]["command"]]
            .iter()
            .filter_map(|v| v.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        work.entry(turn.to_string()).or_default().push_str(&format!("{what}\n"));
    }
    work
}

/// One turn, the operator's words and the agent's (cut short), and the
/// words alone.
fn turn_of(state: &AppState, thread: &str, turn_id: &str) -> (String, Option<String>) {
    let events = state.heart.lock().unwrap().read_stream(thread).unwrap_or_default();
    let of = |family: &str, key: &str| {
        events.iter().find_map(|e| {
            let p: Value = serde_json::from_str(&e.payload).ok()?;
            (e.family == family && p["turn_id"] == turn_id).then(|| p[key].as_str().unwrap_or_default().to_string())
        })
    };
    let (asked, answered) = (of("agent.turn.started", "instruction"), of("agent.turn.done", "summary"));
    let said = format!("asked: {} / answered: {}", cut(asked.as_deref().unwrap_or_default(), 600), cut(answered.as_deref().unwrap_or_default(), 600));
    (said, asked)
}

/// A project's first git root: where its scratch threads get their copy.
fn project_repo(state: &AppState, project: &str) -> Option<String> {
    let projects = crate::catalog::known_projects(state).ok()?;
    projects.into_iter().find(|p| p.project_id == project)?.roots.into_iter().find(|r| r.is_git).map(|r| r.path)
}

fn reflect(state: &AppState, s: &LoopSettings, run: &mut LearningRun) -> Result<Came, String> {
    let Some(reviewer) = s.reviewer_model.clone() else {
        return Ok(Came::Skipped("the reviewer model isn't picked".to_string()));
    };
    let since = (chrono::Utc::now() - chrono::Duration::days(i64::from(s.reflect.window_days))).to_rfc3339();
    let turns = index::turn_outcomes(state, &since);
    let mut works: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    // (project, kind of work): turns that went well, and badly.
    type Pair<'a> = (Vec<&'a index::TurnOutcome>, Vec<&'a index::TurnOutcome>);
    let mut groups: BTreeMap<(String, &'static str), Pair> = BTreeMap::new();
    for turn in &turns {
        let project = crate::state::socket_project(&state.heart.lock().unwrap(), &turn.thread).0;
        let only = |p: &String| !s.projects_out.contains(p) && run.project.as_ref().is_none_or(|only| only == p);
        let Some(project) = project.filter(only) else { continue };
        let work = works.entry(turn.thread.clone()).or_insert_with(|| work_of(state, &turn.thread));
        let Some(kind) = work.get(&turn.turn_id).and_then(|w| kind_of(w)) else { continue };
        let group = groups.entry((project, kind)).or_default();
        match turn.outcome.as_str() {
            "done" | "accepted" => group.0.push(turn),
            "failure" | "step_cap" | "rejected" | "corrected" => group.1.push(turn),
            _ => {}
        }
    }
    let mixed: Vec<_> = groups.into_iter().filter(|(_, (good, bad))| !good.is_empty() && !bad.is_empty()).collect();
    if mixed.is_empty() {
        return Ok(Came::Skipped(format!("no kind of work both passed and failed in the last {} days", s.reflect.window_days)));
    }
    let most = s.reflect.max_proposals as usize;
    let mut proposed = 0;
    for ((project, kind), (good, bad)) in &mixed {
        if proposed >= most {
            break;
        }
        let Some(repo) = project_repo(state, project) else { continue };
        let show = |turns: &[&index::TurnOutcome]| {
            let lines: Vec<String> = turns.iter().rev().take(5).map(|t| format!("- ({}) {}", t.outcome, turn_of(state, &t.thread, &t.turn_id).0)).collect();
            lines.join("\n")
        };
        let prompt = format!(
            "[Bakery memory: reflection on one kind of work that both passed and failed lately.]\n\nThese turns did {kind} work in one \
             project. Some went well and some badly. What explains the difference? Propose at most {} lessons. {RULES}\n\nAnswer: \
             {{\"lessons\": [{{\"title\": \"…\", \"summary\": \"one line\", \"body\": \"…\", \"why\": \"…\", \"how_to_apply\": \"…\"}}]}}. \
             None: {{\"lessons\": []}}.\n\nWent well:\n{}\n\nWent badly:\n{}\n",
            most - proposed,
            show(good),
            show(bad)
        );
        let answer = ask(state, run, Ask { model: reviewer.clone(), repo, mode: OperatingMode::Plan, purpose: "reflection", prompt, about: None })?;
        let lessons = json_in(&answer).and_then(|v| v["lessons"].as_array().cloned()).unwrap_or_default();
        let evidence: Vec<Value> = good.iter().chain(bad.iter()).map(|t| json!({"turn": t.turn_id, "thread": t.thread})).collect();
        for lesson in lessons.iter().take(most - proposed) {
            let text = |key: &str| lesson[key].as_str().unwrap_or_default().trim().to_string();
            let note = Note {
                name: text("title").to_lowercase().replace(' ', "-"),
                kind: "lesson".to_string(),
                scope: format!("project:{project}"),
                title: text("title"),
                summary: text("summary"),
                body: text("body"),
                why: text("why"),
                how_to_apply: text("how_to_apply"),
                evidence: evidence.clone(),
                trust: "agent".to_string(),
                ..Note::default()
            };
            if note.title.is_empty() || note.body.is_empty() {
                continue;
            }
            if let Ok(p) = crate::memory_review::propose_from_loop(state, note) {
                run.notes.extend(p.note);
                if let Some(id) = p.proposal {
                    run.proposals.push(id);
                    proposed += 1;
                }
            }
        }
    }
    Ok(Came::Done(format!("reflected on {} kinds of work; proposed {proposed} lessons", mixed.len())))
}

/// The project a note's scope names.
fn project_of(note: &Note) -> Option<String> {
    note.scope.strip_prefix("project:").map(str::to_string)
}

fn kept(state: &AppState, kind: &str) -> Vec<Note> {
    let q = index::MemoryNotesQuery { status: None, kind: Some(kind.to_string()), scope: None };
    index::notes(state, &q).map(|n| n.notes.into_iter().map(|v| v.note).collect()).unwrap_or_default()
}

/// The turn that taught a lesson, by its evidence: what the operator asked
/// there, and the repository it worked in.
fn taught(state: &AppState, lesson: &Note) -> Option<(String, String)> {
    lesson.evidence.iter().find_map(|e| {
        let thread = e["thread"].as_str()?;
        let turn = match e["turn"].as_str() {
            Some(turn) => turn.to_string(),
            None => {
                let found = state.heart.lock().unwrap().find(e["event"].as_str()?).ok()??;
                serde_json::from_str::<Value>(&found.payload).ok()?["turn_id"].as_str()?.to_string()
            }
        };
        let asked = turn_of(state, thread, &turn).1?;
        let repo = crate::state::context_for(state, thread).ok().map(|c| c.repo_root).filter(|r| !r.is_empty())?;
        Some((asked, repo))
    })
}

fn replay(state: &AppState, s: &LoopSettings, run: &mut LearningRun) -> Result<Came, String> {
    let Some(reviewer) = s.reviewer_model.clone() else {
        return Ok(Came::Skipped("the reviewer model isn't picked".to_string()));
    };
    let only = |l: &Note| match (project_of(l), &run.project) {
        (Some(p), _) if s.projects_out.contains(&p) => false,
        (lesson, Some(only)) => lesson.as_ref() == Some(only),
        _ => true,
    };
    let mut lessons: Vec<Note> = kept(state, "lesson").into_iter().filter(only).collect();
    let mut replayed: BTreeMap<String, usize> = BTreeMap::new();
    for id in all_runs(state).into_iter().filter(|r| r.part == "replay").flat_map(|r| r.notes) {
        *replayed.entry(id).or_default() += 1;
    }
    match s.replay.order.as_str() {
        "oldest" => lessons.sort_by(|a, b| a.written_at.cmp(&b.written_at)),
        "most_used" => lessons.sort_by_key(|l| std::cmp::Reverse(index::page(state, &l.id).map(|p| p.note.uses.turns).unwrap_or_default())),
        "least_proven" => lessons.sort_by_key(|l| replayed.get(&l.id).copied().unwrap_or_default()),
        _ => lessons.sort_by(|a, b| b.written_at.cmp(&a.written_at)),
    }
    let mut verdicts = Vec::new();
    for lesson in &lessons {
        if verdicts.len() >= s.replay.cap as usize {
            break;
        }
        let Some((task, repo)) = taught(state, lesson) else { continue };
        let with_it = format!("[A lesson from the Bakery's memory: {}: {}\n{}]\n\n{task}", lesson.title, lesson.summary, lesson.how_to_apply);
        let mut answers = Vec::new();
        for (purpose, prompt) in [("replay with the lesson", with_it), ("replay without it", task.clone())] {
            answers.push(ask(state, run, Ask { model: reviewer.clone(), repo: repo.clone(), mode: OperatingMode::Auto, purpose, prompt, about: None })?);
        }
        let judge = format!(
            "[Bakery memory: judging a replay.]\n\nThe same task ran twice in a fresh copy of the project, once with this lesson in mind \
             and once without. Which run handled it better? Answer with the JSON alone, using no tools: {{\"better\": \"with\"|\"without\"|\"same\", \
             \"why\": \"one line\"}}.\n\nThe lesson: {}: {}\n{}\n\nThe task:\n{}\n\nWith the lesson:\n{}\n\nWithout it:\n{}\n",
            lesson.title,
            lesson.summary,
            lesson.how_to_apply,
            cut(&task, TURN_CHARS),
            cut(&answers[0], TURN_CHARS),
            cut(&answers[1], TURN_CHARS)
        );
        let verdict = ask(state, run, Ask { model: reviewer.clone(), repo, mode: OperatingMode::Plan, purpose: "replay judge", prompt: judge, about: None })?;
        let better = json_in(&verdict).and_then(|v| v["better"].as_str().map(str::to_string)).unwrap_or_else(|| "same".to_string());
        let rating = match better.as_str() {
            "with" => Some("helped"),
            "without" => Some("hurt"),
            _ => None,
        };
        if let Some(rating) = rating {
            let fact = json!({"id": lesson.id, "rating": rating});
            crate::state::append(state, index::STREAM, ACTOR, index::RATED, fact.to_string(), vec![])?;
            index::tell(state, "rated", Some(&lesson.id), None);
        }
        run.notes.push(lesson.id.clone());
        verdicts.push(better);
    }
    if verdicts.is_empty() {
        return Ok(Came::Skipped("no lesson with a turn to replay".to_string()));
    }
    let count = |what: &str| verdicts.iter().filter(|v| v.as_str() == what).count();
    Ok(Came::Done(format!(
        "replayed {} lessons: {} helped, {} hurt, {} made no difference",
        verdicts.len(),
        count("with"),
        count("without"),
        count("same")
    )))
}

fn lift(state: &AppState, s: &LoopSettings, run: &mut LearningRun) -> Result<Came, String> {
    let lessons: Vec<Note> = kept(state, "lesson").into_iter().filter(|l| project_of(l).is_some_and(|p| !s.projects_out.contains(&p))).collect();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let (mut proposed, mut groups) = (0, 0);
    for lesson in &lessons {
        if seen.contains(&lesson.id) {
            continue;
        }
        let alike: Vec<&Note> = lessons.iter().filter(|other| crate::memory_review::alike(lesson, other) >= crate::memory_review::CLOSE).collect();
        let projects: BTreeSet<String> = alike.iter().filter_map(|n| project_of(n)).collect();
        if projects.len() < s.lift.min_projects as usize || run.project.as_ref().is_some_and(|only| !projects.contains(only)) {
            continue;
        }
        groups += 1;
        seen.extend(alike.iter().map(|n| n.id.clone()));
        let newest = alike.iter().max_by(|a, b| a.written_at.cmp(&b.written_at)).copied().unwrap_or(lesson);
        let note = Note {
            name: newest.name.clone(),
            kind: "lesson".to_string(),
            scope: "bakery".to_string(),
            title: newest.title.clone(),
            summary: newest.summary.clone(),
            body: newest.body.clone(),
            why: format!("Seen in {} projects. {}", projects.len(), newest.why).trim().to_string(),
            how_to_apply: newest.how_to_apply.clone(),
            evidence: alike.iter().map(|n| json!({"note": n.id})).collect(),
            trust: "agent".to_string(),
            ..Note::default()
        };
        run.notes.extend(alike.iter().map(|n| n.id.clone()));
        if let Ok(p) = crate::memory_review::propose_from_loop(state, note) {
            if let Some(id) = p.proposal {
                run.proposals.push(id);
                proposed += 1;
            }
        }
    }
    match groups {
        0 => Ok(Came::Skipped(format!("no lesson is in {} projects yet", s.lift.min_projects))),
        _ => Ok(Came::Done(format!("{groups} lessons seen in {}+ projects; {proposed} proposed for the whole Bakery", s.lift.min_projects))),
    }
}

fn digest(state: &AppState, s: &LoopSettings, run: &mut LearningRun) -> Result<Came, String> {
    let mut digest = crate::memory_review::digest(state)?;
    if let Some(only) = run.project.as_ref().map(|p| format!("project:{p}")) {
        digest.pending.retain(|p| p.note.scope == only);
        digest.stale.retain(|n| n.scope == only);
        let proposals: BTreeSet<String> = ["kept", "declined"]
            .iter()
            .filter_map(|status| crate::memory_review::proposals(state, &crate::memory_review::ProposalsQuery { status: Some(status.to_string()) }).ok())
            .flat_map(|p| p.proposals)
            .filter(|p| p.note.scope == only)
            .map(|p| p.id)
            .collect();
        digest.day.retain(|m| m.proposal.as_ref().is_some_and(|p| proposals.contains(p)));
    }
    let count = |what: &str| digest.day.iter().filter(|m| m.what == what).count();
    let (kept, declined) = (count("kept"), count("declined"));
    run.notes.extend(digest.stale.iter().map(|n| n.id.clone()));
    if s.digest.notify {
        let told = json!({"kind": "memory.digest", "project": run.project, "pending": digest.pending.len(), "stale": digest.stale.len(), "kept": kept, "declined": declined, "at": chrono::Utc::now().to_rfc3339()});
        let _ = state.tx.send(told.to_string());
    }
    Ok(Came::Done(format!("{} wait for you, {} went stale; today {kept} kept, {declined} declined", digest.pending.len(), digest.stale.len())))
}

/// The threads that finished turns in the last day.
fn worked_today(state: &AppState) -> BTreeSet<String> {
    let since = (chrono::Utc::now() - chrono::Duration::days(1)).to_rfc3339();
    index::turn_outcomes(state, &since).into_iter().map(|t| t.thread).collect()
}

fn nightly(state: &AppState, s: &LoopSettings, run: &mut LearningRun) -> Result<Came, String> {
    let mut parts: Vec<LearningRun> = Vec::new();
    let in_run = |thread: &String| {
        run.project.is_none() || crate::state::socket_project(&state.heart.lock().unwrap(), thread).0 == run.project
    };
    if s.thread_end.on && s.thread_end.in_nightly {
        for thread in worked_today(state).into_iter().filter(in_run) {
            if turns_since_review(state, &thread) >= s.thread_end.min_turns as usize {
                parts.push(run_part(state, s, "thread_end", "nightly", Some(thread), None));
            }
        }
    }
    for (part, on) in [("reflect", s.reflect.on), ("replay", s.replay.on), ("lift", s.lift.on), ("digest", s.digest.on)] {
        if on {
            parts.push(run_part(state, s, part, "nightly", None, run.project.clone()));
        }
    }
    for part in &parts {
        run.notes.extend(part.notes.iter().cloned());
        run.proposals.extend(part.proposals.iter().cloned());
        run.model_calls.extend(part.model_calls.iter().cloned());
        run.refused.extend(part.refused.iter().cloned());
    }
    let said: Vec<String> = parts.iter().map(|p| format!("{} {}", p.part, p.status)).collect();
    Ok(Came::Done(if said.is_empty() { "every part is off".to_string() } else { said.join(", ") }))
}

/// The loop's own clock, looking every minute: a thread that ends (archived,
/// or its work merged) gets its review, and the nightly run starts in its
/// window when the Mac is idle and cool, retrying every so often until the
/// window closes; a window that closes unrun is recorded so.
pub(crate) fn keep_learning(state: AppState) {
    std::thread::spawn(move || {
        let mut read_to = state.heart.lock().unwrap().tip().ok().flatten().map_or(0, |(at, _)| at);
        let mut busy_since: Option<Instant> = None;
        let mut closed: Option<DateTime<Local>> = None;
        loop {
            std::thread::sleep(LOOK_EVERY);
            let (s, _, _) = settings(&state);
            let families = [crate::baker_threads::THREAD_ARCHIVED_FAMILY, crate::baker_threads::MERGED_FAMILY];
            let ends = state.heart.lock().unwrap().events_after(read_to, &families).unwrap_or_default();
            read_to = ends.last().map_or(read_to, |(at, _)| *at);
            for (_, e) in ends {
                let fact: Value = serde_json::from_str(&e.payload).unwrap_or_default();
                let archived = e.family == crate::baker_threads::THREAD_ARCHIVED_FAMILY && fact["archived"] == true && s.thread_end.on_archive;
                let merged = e.family == crate::baker_threads::MERGED_FAMILY && fact["thread"] == e.stream_id.as_str() && s.thread_end.on_merge;
                if !s.paused && s.thread_end.on && (archived || merged) {
                    run_part(&state, &s, "thread_end", if archived { "archive" } else { "merge" }, Some(e.stream_id.clone()), None);
                }
            }
            if s.paused || !s.nightly.on {
                continue;
            }
            let now = Local::now();
            let runs = all_runs(&state);
            match current_window(&s.nightly, now) {
                Some(window) if !ran_in(&runs, window) => {
                    let busy = s.nightly.wait_while_busy && crate::service::running(&state) > 0;
                    let hot = s.nightly.wait_while_hot && crate::this_mac::holding();
                    if busy || hot {
                        busy_since.get_or_insert_with(Instant::now);
                        continue;
                    }
                    let retry = Duration::from_secs(u64::from(s.nightly.retry_every_minutes) * 60);
                    if busy_since.is_some_and(|at| at.elapsed() < retry) {
                        continue;
                    }
                    (busy_since, closed) = (None, Some(window.1));
                    run_part(&state, &s, "nightly", "nightly", None, None);
                }
                Some(_) => {}
                None => {
                    let days = [now.date_naive().pred_opt(), Some(now.date_naive())];
                    let ended = days.into_iter().flatten().filter_map(|day| window(&s.nightly, day)).filter(|(_, end)| *end <= now).last();
                    let unrun = ended.filter(|w| closed != Some(w.1) && !ran_in(&runs, *w) && now - w.1 < chrono::Duration::hours(1));
                    if let Some(window) = unrun {
                        (busy_since, closed) = (None, Some(window.1));
                        let run = start(&state, "nightly", "nightly", None, None);
                        finish(&state, run, Ok(Came::Skipped("the Mac was busy or hot through the whole window".to_string())));
                    }
                }
            }
        }
    });
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

pub(crate) async fn learning_route(State(st): State<AppState>) -> Result<Json<Learning>, (StatusCode, String)> {
    blocking(move || learning(&st)).await
}

pub(crate) async fn set_route(State(st): State<AppState>, Json(patch): Json<Value>) -> Result<Json<Learning>, (StatusCode, String)> {
    blocking(move || set(&st, &patch)).await
}

pub(crate) async fn run_route(State(st): State<AppState>, Json(req): Json<RunReq>) -> Result<Json<LearningRun>, (StatusCode, String)> {
    blocking(move || run_now(&st, &req)).await
}

pub(crate) async fn runs_route(State(st): State<AppState>, Query(q): Query<RunsQuery>) -> Result<Json<LearningRuns>, (StatusCode, String)> {
    blocking(move || runs(&st, &q)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;
    use std::sync::Arc;

    fn project(state: &AppState, dir: &std::path::Path, name: &str) -> (String, String) {
        let root = dir.join(name);
        std::fs::create_dir_all(&root).unwrap();
        let (p, socket) = crate::catalog::create_project(state, name, Some(crate::catalog::DirectoryRootSpec::Attach(root.to_str().unwrap()))).unwrap();
        (p.project_id, socket.stream_id)
    }

    /// Every part of the loop is the operator's to change, one setting at a
    /// time, checked; an agent changes none. The view says what holds
    /// parts back, when the nightly run next starts, and lists the models.
    #[test]
    fn the_operator_manages_every_part_of_the_loop() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(bakery_agent::StubModel));
        let (app, _) = project(&state, dir.path(), "app");
        let view = learning(&state).unwrap();
        assert_eq!(view.settings, LoopSettings::default());
        assert!(view.held.iter().any(|h| h.contains("reviewer model isn't picked")), "{:?}", view.held);
        assert!(view.next_nightly.is_some() && !view.models.is_empty() && view.changed_by.is_none());

        let changed = set(&state, &json!({"paused": true, "replay": {"cap": 2}, "projects_out": [app]})).unwrap();
        assert!(changed.settings.paused && changed.settings.replay.cap == 2, "{:?}", changed.settings);
        assert_eq!(changed.settings.replay.order, "newest", "the rest of a part stays as it was");
        assert_eq!((changed.changed_by.as_deref(), changed.next_nightly), (Some("operator"), None), "paused: no nightly run");
        for wrong in [json!({"nightly": {"at": "3am"}}), json!({"replay": {"order": "random"}}), json!({"reviewer_model": "nope"}), json!({"projects_out": ["ghost"]}), json!({"replay": {"cap": "lots"}})] {
            assert!(set(&state, &wrong).is_err(), "{wrong}");
        }
        let as_agent = crate::actor::with(crate::actor::agent("socket_x", Seat::Orchestrator), || set(&state, &json!({"paused": false})));
        assert!(as_agent.unwrap_err().starts_with("only the operator"));
        assert!(run_now(&state, &RunReq { part: "lift".into(), thread: None, project: None }).is_ok());
        set(&state, &json!({"lift": {"on": false}})).unwrap();
        assert!(run_now(&state, &RunReq { part: "lift".into(), thread: None, project: None }).unwrap_err().contains("off"));
        assert!(run_now(&state, &RunReq { part: "thread_end".into(), thread: None, project: None }).is_err(), "a review names its thread");
    }

    /// A nightly window may run past midnight; one already run isn't run
    /// again, and the next start is the window's.
    #[test]
    fn the_nightly_window_runs_once() {
        let s = Nightly { at: "23:00".into(), retry_until: "02:00".into(), ..Nightly::default() };
        let at = |day: u32, h: u32| Local.with_ymd_and_hms(2026, 10, day, h, 30, 0).unwrap();
        assert_eq!(current_window(&s, at(12, 1)).map(|w| w.0), Some(Local.with_ymd_and_hms(2026, 10, 11, 23, 0, 0).unwrap()));
        assert!(current_window(&s, at(12, 12)).is_none());
        let window = current_window(&s, at(11, 23)).unwrap();
        let run = LearningRun { part: "nightly".into(), trigger: "nightly".into(), started_at: at(11, 23).to_rfc3339(), ..LearningRun::default() };
        assert!(ran_in(std::slice::from_ref(&run), window));
        let settings = LoopSettings { nightly: s, ..LoopSettings::default() };
        assert_eq!(next_nightly(&settings, &[run], at(12, 1)), Some(Local.with_ymd_and_hms(2026, 10, 12, 23, 0, 0).unwrap()), "today's, once it ran");
    }

    /// A lesson seen in two projects is proposed for the whole Bakery, once,
    /// and not while one of them is out of the loop; the digest tells the
    /// operator when asked to. Each run is in the history with what it
    /// touched.
    #[test]
    fn lifting_and_the_digest_need_no_model() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(bakery_agent::StubModel));
        let (a, _) = project(&state, dir.path(), "a");
        let (b, _) = project(&state, dir.path(), "b");
        for (id, p) in [("rerun-a", &a), ("rerun-b", &b)] {
            let lesson = json!({"id": id, "scope": format!("project:{p}"), "type": "lesson", "title": "Rerun flaky tests once",
                                "summary": "A flaky test passes on a second run", "body": "Rerun a flaky test once before chasing it."});
            crate::state::append(&state, index::STREAM, "human", index::WRITTEN, lesson.to_string(), vec![]).unwrap();
        }
        let mut events = state.tx.subscribe();
        let s = settings(&state).0;
        let lifted = run_part(&state, &s, "lift", "run_now", None, None);
        assert_eq!((lifted.status.as_str(), lifted.proposals.len(), lifted.notes.len()), ("done", 1, 2), "{}", lifted.said);
        let waiting = crate::memory_review::proposals(&state, &crate::memory_review::ProposalsQuery { status: None }).unwrap().proposals;
        assert_eq!((waiting[0].note.scope.as_str(), waiting[0].proposed_by.as_str()), ("bakery", ACTOR));
        assert!(run_part(&state, &s, "lift", "run_now", None, None).proposals.is_empty(), "once");
        let out = LoopSettings { projects_out: vec![a.clone()], ..s.clone() };
        assert_eq!(run_part(&state, &out, "lift", "run_now", None, None).status, "skipped");

        let told = run_part(&state, &s, "digest", "run_now", None, None);
        assert!(told.said.starts_with("1 wait for you"), "{}", told.said);
        let digests = std::iter::from_fn(|| events.try_recv().ok()).filter(|e| e.contains("\"memory.digest\"")).count();
        assert_eq!(digests, 1);
        let quiet = LoopSettings { digest: Digest { on: true, notify: false }, ..s.clone() };
        run_part(&state, &quiet, "digest", "run_now", None, None);
        assert_eq!(std::iter::from_fn(|| events.try_recv().ok()).filter(|e| e.contains("\"memory.digest\"")).count(), 0, "no notice when asked not to");

        let history = runs(&state, &RunsQuery { part: Some("lift".into()), limit: None }).unwrap().runs;
        assert_eq!(history.len(), 3);
        assert_eq!(learning(&state).unwrap().last.digest.map(|r| r.status), Some("done".to_string()));
    }

    /// A thread's turns are read by a model in a scratch thread of its own
    /// (a fresh worktree, put away after, its branch too), which proposes
    /// what is worth remembering as the thread; the call is in the run's
    /// account. A thread with too few turns since is skipped.
    #[test]
    fn a_thread_end_review_proposes_what_the_thread_learned() {
        let dir = tempfile::tempdir().unwrap();
        let review = r#"{"notes": [{"type": "lesson", "title": "Build with the lockfile", "summary": "The build needs --locked",
            "body": "The build only passes with cargo build --locked.", "why": "Two builds failed without it.", "how_to_apply": "Run {cmd} --locked."}]}"#;
        let model = Arc::new(ScriptedModel::new(&["fixed one", "fixed two", "fixed three", review]));
        let state = state_with(dir.path(), model);
        let repo = make_repo(&dir.path().join("repo"));
        let thread = crate::routes::project::open_stream(&state, &open_req(&repo, "work")).unwrap().context.stream_id;
        for n in 1..=3 {
            let turn = crate::routes::agent::AgentTurnReq { stream_id: thread.clone(), instruction: format!("fix the build, part {n}"), ..Default::default() };
            crate::routes::agent::agent_turn_inner(&state, &turn).unwrap();
        }
        let s = settings(&state).0;
        let reviewed = run_part(&state, &s, "thread_end", "run_now", Some(thread.clone()), None);
        assert_eq!((reviewed.status.as_str(), reviewed.proposals.len()), ("done", 1), "{}", reviewed.said);
        assert_eq!((reviewed.model_calls[0].purpose.as_str(), reviewed.model_calls[0].ok), ("thread-end review", true));
        let waiting = crate::memory_review::proposals(&state, &crate::memory_review::ProposalsQuery { status: None }).unwrap().proposals;
        assert_eq!((waiting[0].note.title.as_str(), waiting[0].proposed_by.as_str()), ("Build with the lockfile", thread.as_str()));
        let branches = crate::git::run(&repo, &["branch", "--list", "learning-*"]).unwrap();
        assert!(branches.trim().is_empty(), "the scratch branch is gone: {branches}");
        assert_eq!(run_part(&state, &s, "thread_end", "run_now", Some(thread), None).status, "skipped", "no turns since");
    }

    /// The operator runs any part for one project: only what it taught and
    /// what waits for it count. An unknown project, one out of the loop, or a
    /// thread from another project is refused with the reason.
    #[test]
    fn a_part_runs_for_one_project() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(bakery_agent::StubModel));
        let (a, thread_a) = project(&state, dir.path(), "a");
        let (b, _) = project(&state, dir.path(), "b");
        let (c, _) = project(&state, dir.path(), "c");
        for (id, p) in [("rerun-a", &a), ("rerun-b", &b)] {
            let lesson = json!({"id": id, "scope": format!("project:{p}"), "type": "lesson", "title": "Rerun flaky tests once",
                                "summary": "A flaky test passes on a second run", "body": "Rerun a flaky test once before chasing it."});
            crate::state::append(&state, index::STREAM, "human", index::WRITTEN, lesson.to_string(), vec![]).unwrap();
        }
        let s = settings(&state).0;
        assert!(run_part(&state, &s, "lift", "run_now", None, Some(c.clone())).proposals.is_empty(), "c taught nothing");
        let lifted = run_part(&state, &s, "lift", "run_now", None, Some(a.clone()));
        assert_eq!((lifted.project.as_deref(), lifted.proposals.len()), (Some(a.as_str()), 1), "{}", lifted.said);
        let said = |project: Option<String>| run_part(&state, &s, "digest", "run_now", None, project).said;
        assert!(said(None).starts_with("1 wait for you"), "the whole Bakery's");
        assert!(said(Some(a.clone())).starts_with("0 wait for you"), "a Bakery-wide proposal isn't a's");

        let req = |part: &str, project: &str| RunReq { part: part.into(), thread: None, project: Some(project.into()) };
        assert_eq!(run_now(&state, &req("lift", "ghost")).unwrap_err(), "no project ghost");
        set(&state, &json!({"projects_out": [b]})).unwrap();
        assert!(run_now(&state, &req("lift", &b)).unwrap_err().contains("out of the loop"));
        let wrong = RunReq { part: "thread_end".into(), thread: Some(thread_a), project: Some(c) };
        assert!(run_now(&state, &wrong).unwrap_err().contains("isn't in"));
        assert_eq!(run_now(&state, &req("digest", &a)).unwrap().project, Some(a));
    }

    /// A scratch thread asks no one and reaches nothing outward: a push, a
    /// pull request, an issue, a Google change, a note, a message or a
    /// preview is refused outright with no card; so are a question and a
    /// native runtime's gated action. Each refusal is on the thread once,
    /// for its run.
    #[test]
    fn a_scratch_thread_asks_no_one() {
        let dir = tempfile::tempdir().unwrap();
        let question = r#"{"tool":"ask_user","args":{"questions":[{"question":"Push it?","options":["yes","no"]}]}}"#;
        let state = state_with(dir.path(), Arc::new(ScriptedModel::new(&[question, "done"])));
        let repo = make_repo(&dir.path().join("repo"));
        let sid = crate::routes::project::open_stream(&state, &open_req(&repo, "replay")).unwrap().context.stream_id;
        crate::state::append(&state, &sid, ACTOR, SCRATCH, json!({"run": "learn_x"}).to_string(), vec![]).unwrap();
        let caller = crate::api_tools::Caller { stream_id: sid.clone(), seat: Seat::Orchestrator, turn_id: "t".into(), mode: OperatingMode::Auto, wait: Some(Duration::from_millis(200)) };
        let outward = ["git_push", "git_pr", "github_issue_create", "github_issue_comment", "google_change", "memory_propose", "backlog_add", "send_message", "preview_open"];
        for tool in outward.iter().chain(&["git_push"]) {
            let refused = crate::api_tools::call(&state, &caller, tool, &json!({"stream_id": sid})).unwrap_err();
            assert!(refused.ends_with("refused"), "{tool}: {refused}");
        }
        let gated = crate::pipe::ChildToolRequest { plug: "acp:test".into(), kind: crate::pipe::ChildToolKind::Execute, title: "curl example.com".into(), diffs: vec![] };
        let admitted = crate::pipe::admit_child_tool_call_detailed(&state, &sid, Seat::Orchestrator, "t", &gated, false);
        assert!(matches!(admitted, crate::pipe::ChildToolAdmission::Deny));
        let turn = crate::routes::agent::AgentTurnReq { stream_id: sid.clone(), instruction: "replay the task".into(), ..Default::default() };
        crate::routes::agent::agent_turn_inner(&state, &turn).unwrap();

        assert_eq!(crate::remote::waiting(&state).count, 0, "no card waits");
        let heart = state.heart.lock().unwrap();
        let of = |family: &str| heart.events_of_family(&sid, family).unwrap();
        assert!(of(crate::pipe::CHILD_TOOL_PENDING_FAMILY).is_empty() && of("agent.ask.pending").is_empty());
        let refused: Vec<String> = of(REFUSED).iter().map(|e| serde_json::from_str::<Value>(&e.payload).unwrap()["what"].as_str().unwrap().to_string()).collect();
        assert_eq!(refused.len(), outward.len() + 2, "each once: {refused:?}");
        assert!(refused.contains(&"a question for the operator".to_string()), "{refused:?}");
    }

    /// A scratch thread put away leaves nothing running: the job it started
    /// ends with its terminals, and its worktree goes.
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn a_scratch_thread_put_away_leaves_nothing_running() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(bakery_agent::StubModel));
        let repo = make_repo(&dir.path().join("repo"));
        let ctx = crate::routes::project::open_stream(&state, &open_req(&repo, "learning-replay")).unwrap().context;
        crate::state::append(&state, &ctx.stream_id, ACTOR, SCRATCH, json!({"run": "learn_x"}).to_string(), vec![]).unwrap();
        let caller = crate::api_tools::Caller { stream_id: ctx.stream_id.clone(), seat: Seat::Orchestrator, turn_id: "t".into(), mode: OperatingMode::Auto, wait: None };
        let started = crate::api_tools::call(&state, &caller, "job_start", &json!({"stream_id": ctx.stream_id, "command": "echo started; sleep 30"})).unwrap();
        let id = serde_json::from_str::<Value>(&started).unwrap()["id"].as_str().unwrap().to_string();
        let job = || crate::jobs::job_output(&id, 0).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while !job().output.contains("started") {
            assert!(std::time::Instant::now() < deadline, "it never started: {}", job().output);
            std::thread::sleep(Duration::from_millis(50));
        }
        let (st, sid, root, branch) = (state.clone(), ctx.stream_id.clone(), ctx.repo_root.clone(), ctx.branch.clone());
        std::thread::spawn(move || put_away(&st, &sid, &root, &branch)).join().unwrap();
        assert_ne!(job().job.state, "running", "the job ended with its thread");
        assert!(!std::path::Path::new(&ctx.worktree_path).exists(), "the worktree is gone");
    }
}
