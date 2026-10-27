//! Baker threads — parallel conversations beneath the one permanent Baker
//! (docs/slices/BAKER-THREADS-03.md).
//!
//! A thread is its own freshly minted factory socket, so everything this
//! node keys by `(stream_id, seat)` — transcript, durable queue, busy state,
//! native session — is separate per thread by construction, and threads run
//! in parallel. What stays singular is the Baker itself: one persona, one op
//! registry, one set of protections, and one durable memory, all reached
//! through [`crate::baker_ops::is_baker_scope_stream`].
//!
//! **Membership is a write-once owner fact.** `baker.thread.opened` names the
//! permanent Baker a thread belongs to. It is written only by
//! [`create_thread`], only onto a stream minted in that same call, and it is
//! read with `first_of_family` — a later append can never re-own a thread,
//! and nothing re-parents an existing socket into one. A socket whose owner
//! fact names anything but the current `baker_stream_id` (a re-minted Baker,
//! a malformed payload) is an ordinary, unprivileged factory socket.
//!
//! **A thread reports to its spawner.** The same record names the Baker
//! conversation that opened it: what it says is ready to merge, and what is
//! merged of it, goes to that conversation, which may merge it too
//! ([`reports_to`]). The permanent Baker stays the fallback, and sees any
//! thread's work in [`merge_waiting`] when it looks.

use crate::agent_control::OperatingMode;
use crate::node::{Seat, SocketKind, SocketKindRecord, SOCKET_KIND_FAMILY};
use crate::state::{append, context_for, AppState};
use axum::{
    extract::{Query, State},
    http::StatusCode,
    Json,
};
use bakery_core::Event;
use bakery_heart::Heart;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};

pub(crate) const THREAD_OPENED_FAMILY: &str = "baker.thread.opened";
pub(crate) const THREAD_RENAMED_FAMILY: &str = "baker.thread.renamed";
pub(crate) const THREAD_ARCHIVED_FAMILY: &str = "baker.thread.archived";
/// The proof jobs one batch of a thread's work got ([`prove`]).
pub(crate) const THREAD_PROOF_FAMILY: &str = "baker.thread.proof";
/// Which conversation opened a peer ([`create_peer`]), and its title.
pub(crate) const PEER_OPENED_FAMILY: &str = "agent.peer.opened";

/// What a thread is called until a rename or its first message names it.
pub(crate) const DEFAULT_THREAD_TITLE: &str = "New thread";
/// Display title of the permanent Baker's own conversation.
pub(crate) const MAIN_THREAD_TITLE: &str = "Baker";
const MAX_TITLE_CHARS: usize = 80;
/// Every open thread can hold its own native engine process, so the number
/// of simultaneously open (non-archived) threads is bounded. Archiving one
/// frees a slot; nothing is ever deleted to make room.
pub(crate) const MAX_OPEN_THREADS: usize = 24;
/// The only scope this pass mints. Project-scoped Baker contexts (S0's
/// `ProjectOps`) need a narrower op set and are deliberately not built here;
/// a `scope` this build does not know never passes membership.
const FACTORY_SCOPE: &str = "factory";

#[derive(Debug, Serialize, Deserialize)]
struct ThreadOpened {
    baker_stream_id: String,
    scope: String,
    title: String,
    /// The Baker conversation that opened it, the one it reports to: the
    /// permanent Baker when the operator opened it from the app. A thread
    /// opened before this was recorded has none and reports to the permanent
    /// Baker ([`reports_to`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    spawner: Option<String>,
    /// What its result must satisfy, reported back with every result.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    acceptance: Vec<Criterion>,
}

/// One thing a thread's result must satisfy: text the Baker judges itself,
/// or a criterion with a check that proves it.
#[derive(schemars::JsonSchema, Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum Criterion {
    Text(String),
    Checked {
        criterion: String,
        /// A shell command, run as a proof job in the thread's workspace when
        /// the thread goes idle after work; exit 0 passes.
        check: String,
    },
}

impl Criterion {
    fn text(&self) -> &str {
        match self {
            Criterion::Text(text) | Criterion::Checked { criterion: text, .. } => text,
        }
    }

    fn check(&self) -> Option<&str> {
        match self {
            Criterion::Checked { check, .. } => Some(check.trim()).filter(|c| !c.is_empty()),
            Criterion::Text(_) => None,
        }
    }
}

/// The checks one batch of a thread's work got, recorded against its last
/// turn.
#[derive(Debug, Serialize, Deserialize)]
struct ThreadProof {
    turn_id: String,
    proofs: Vec<Proof>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Proof {
    criterion: String,
    check: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    job_id: Option<String>,
    /// Why its job could not start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ThreadRenamed {
    title: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct ThreadArchived {
    archived: bool,
}

// ─── Membership ─────────────────────────────────────────────────────────────

/// The recorded permanent Baker stream id, or `None` when absent or blank —
/// an empty pointer never matches anything (S0 "empty pointer" row).
pub(crate) fn baker_pointer(state: &AppState) -> Option<String> {
    state
        .node
        .lock()
        .unwrap()
        .baker_stream_id
        .clone()
        .filter(|id| !id.trim().is_empty())
}

fn opened_record(heart: &Heart, stream_id: &str) -> Option<ThreadOpened> {
    let ev = heart
        .first_of_family(stream_id, THREAD_OPENED_FAMILY)
        .ok()
        .flatten()?;
    serde_json::from_str(&ev.payload).ok()
}

/// Heart-locked membership test. Takes the Baker pointer as an argument so a
/// caller already holding `heart` never has to lock `node` underneath it.
pub(crate) fn is_thread_locked(heart: &Heart, baker: Option<&str>, stream_id: &str) -> bool {
    let Some(baker) = baker.filter(|b| !b.trim().is_empty()) else {
        return false;
    };
    if stream_id.trim().is_empty() || stream_id == baker {
        return false;
    }
    let Some(opened) = opened_record(heart, stream_id) else {
        return false;
    };
    opened.baker_stream_id == baker
        && opened.scope == FACTORY_SCOPE
        && crate::state::socket_kind(heart, stream_id) == SocketKind::Factory
        && !crate::state::discarded_of(heart, stream_id)
}

/// Whether `stream_id` is a live thread of the CURRENT permanent Baker. Read
/// fresh every call, never cached — same posture as
/// [`crate::baker_ops::is_permanent_baker_stream`].
pub(crate) fn is_baker_thread_stream(state: &AppState, stream_id: &str) -> bool {
    let baker = baker_pointer(state);
    let heart = state.heart.lock().unwrap();
    is_thread_locked(&heart, baker.as_deref(), stream_id)
}

// ─── Spawner ────────────────────────────────────────────────────────────────

/// The Baker conversation opening a thread, for the record: the one `actor`
/// names (`agent:<stream>:baker`, also behind the route's "human" inside an
/// agent's call) when it is one of the Baker's, else the permanent Baker
/// `home`: the operator opening one from the app, or a caller with no
/// conversation to name.
fn spawner_for(state: &AppState, actor: &str, home: &str) -> String {
    crate::messages::sender(&crate::actor::attribute(actor))
        .filter(|stream| crate::baker_ops::is_baker_scope_stream(state, stream))
        .unwrap_or_else(|| home.to_string())
}

/// The spawner a thread's opening record names; none for a thread opened
/// before it was recorded.
pub(crate) fn spawner_locked(heart: &Heart, thread: &str) -> Option<String> {
    opened_record(heart, thread)?.spawner
}

/// The Baker conversation a chef reports to: its spawner, the permanent Baker
/// when that one is archived, gone, or never recorded. What a chef says is
/// ready to merge, and its merge, are told there and nowhere else.
pub(crate) fn reports_to(state: &AppState, thread: &str) -> Result<String, String> {
    let home = baker_pointer(state).ok_or("there is no Baker to merge into")?;
    let heart = state.heart.lock().unwrap();
    let live = |stream: &String| *stream == home || (is_thread_locked(&heart, Some(&home), stream) && !archived_locked(&heart, stream));
    Ok(spawner_locked(&heart, thread).filter(live).unwrap_or(home))
}

/// Whether `caller` may merge `thread`'s work: the permanent Baker, or the
/// Baker conversation that opened the thread.
pub(crate) fn may_merge(state: &AppState, caller: &str, thread: &str) -> bool {
    if caller.trim().is_empty() {
        return false;
    }
    baker_pointer(state).as_deref() == Some(caller) || spawner_locked(&state.heart.lock().unwrap(), thread).as_deref() == Some(caller)
}

/// Where a Baker conversation's durable (L1) memory lives: every thread reads
/// the permanent Baker's own notes — one Baker, one memory — and every other
/// stream reads its own.
pub(crate) fn memory_stream_for(state: &AppState, stream_id: &str) -> String {
    if is_baker_thread_stream(state, stream_id) {
        if let Some(baker) = baker_pointer(state) {
            return baker;
        }
    }
    stream_id.to_string()
}

/// One prompt line telling a Baker turn which of its conversations it is in
/// and how to reach the others — appended only where the Baker's ops are
/// actually dispatchable, so it never advertises a tool the turn lacks.
pub(crate) fn conversation_note(state: &AppState, stream_id: &str) -> Option<String> {
    let baker = baker_pointer(state);
    let info = {
        let heart = state.heart.lock().unwrap();
        thread_info_locked(&heart, baker.as_deref(), stream_id)?
    };
    Some(if info.main {
        format!(
            "Conversation: your main conversation (stream_id {stream_id}). You can run work in \
             parallel: thread_create opens another conversation of yours (on its own runtime, \
             model, effort, mode and worktree if you give them, with an optional first instruction \
             it starts on immediately), thread_wait waits for its result, thread_read shows its \
             work, thread_message steers one, thread_list shows them all.\n"
        )
    } else {
        format!(
            "Conversation: your parallel thread \u{201c}{}\u{201d} (stream_id {stream_id}). Your main \
             conversation and other threads run independently of this one; coordinate with \
             thread_list / thread_message. Durable notes (memory_write) are shared by every \
             conversation of yours.\n",
            info.title
        )
    })
}

// ─── Projection ─────────────────────────────────────────────────────────────

fn title_locked(heart: &Heart, stream_id: &str, opened: &ThreadOpened) -> String {
    heart
        .last_of_family(stream_id, THREAD_RENAMED_FAMILY)
        .ok()
        .flatten()
        .and_then(|ev| serde_json::from_str::<ThreadRenamed>(&ev.payload).ok())
        .map(|r| r.title)
        .unwrap_or_else(|| opened.title.clone())
}

/// Latest archive fact. An unreadable newest record reads as NOT archived:
/// archive is display-only (it grants and removes no authority), and hiding a
/// thread on a fact nobody can read would drop it from the operator's view.
pub(crate) fn archived_locked(heart: &Heart, stream_id: &str) -> bool {
    heart
        .last_of_family(stream_id, THREAD_ARCHIVED_FAMILY)
        .ok()
        .flatten()
        .and_then(|ev| serde_json::from_str::<ThreadArchived>(&ev.payload).ok())
        .is_some_and(|r| r.archived)
}

/// `/studio/state`'s marker on a factory socket that is a Baker conversation.
#[derive(schemars::JsonSchema, Debug, Clone, Serialize)]
pub(crate) struct BakerThreadInfo {
    /// The permanent Baker's own conversation — never archived or renamed.
    pub(crate) main: bool,
    pub(crate) title: String,
    pub(crate) archived: bool,
}

pub(crate) fn thread_info_locked(
    heart: &Heart,
    baker: Option<&str>,
    stream_id: &str,
) -> Option<BakerThreadInfo> {
    let baker = baker.filter(|b| !b.trim().is_empty())?;
    if stream_id == baker {
        return Some(BakerThreadInfo {
            main: true,
            title: MAIN_THREAD_TITLE.to_string(),
            archived: false,
        });
    }
    if !is_thread_locked(heart, Some(baker), stream_id) {
        return None;
    }
    let opened = opened_record(heart, stream_id)?;
    Some(BakerThreadInfo {
        main: false,
        title: title_locked(heart, stream_id, &opened),
        archived: archived_locked(heart, stream_id),
    })
}

/// One Baker conversation as the op registry and `GET /baker/threads` report
/// it. Deliberately carries no `owner_node_id` (the no-hostname rule every
/// route result keeps, `api_tools`).
#[derive(schemars::JsonSchema, Debug, Clone, Serialize)]
pub(crate) struct BakerThreadView {
    pub(crate) stream_id: String,
    pub(crate) agent_id: String,
    pub(crate) main: bool,
    pub(crate) title: String,
    pub(crate) archived: bool,
    /// A turn is running on this conversation right now.
    pub(crate) busy: bool,
    pub(crate) last_ts: String,
    pub(crate) model_id: Option<String>,
}

/// Streams with a running Baker-seat turn — read from `lifecycle` BEFORE any
/// `heart` lock is taken, never nested inside one (`admit_activity` locks
/// lifecycle then heart; the reverse order here would invert it).
fn busy_streams(state: &AppState) -> HashSet<String> {
    state
        .lifecycle
        .lock()
        .unwrap()
        .running_turns
        .iter()
        .filter(|((_, seat), &count)| *seat == Seat::Baker && count > 0)
        .map(|((stream_id, _), _)| stream_id.clone())
        .collect()
}

/// The permanent Baker's conversation first, then its threads, most recently
/// active first. Archived threads only when asked for.
pub(crate) fn list_threads(
    state: &AppState,
    include_archived: bool,
) -> Result<Vec<BakerThreadView>, String> {
    let baker = baker_pointer(state);
    let busy = busy_streams(state);
    let streams = crate::state::known_streams(state)?;
    let heart = state.heart.lock().unwrap();
    let mut out = Vec::new();
    for s in streams {
        if s.discarded {
            continue;
        }
        let Some(info) = thread_info_locked(&heart, baker.as_deref(), &s.stream_id) else {
            continue;
        };
        if info.archived && !include_archived {
            continue;
        }
        out.push(BakerThreadView {
            agent_id: crate::node::agent_id(&s.stream_id, Seat::Baker),
            main: info.main,
            title: info.title,
            archived: info.archived,
            busy: busy.contains(&s.stream_id),
            last_ts: s.last_ts,
            model_id: crate::state::latest_seat_model_id(&heart, &s.stream_id, Seat::Baker)
                .ok()
                .flatten(),
            stream_id: s.stream_id,
        });
    }
    out.sort_by(|a, b| b.main.cmp(&a.main).then_with(|| b.last_ts.cmp(&a.last_ts)));
    Ok(out)
}

fn thread_view(state: &AppState, stream_id: &str) -> Result<BakerThreadView, String> {
    list_threads(state, true)?
        .into_iter()
        .find(|t| t.stream_id == stream_id)
        .ok_or_else(|| format!("{stream_id} is not a Baker conversation"))
}

// ─── Lifecycle ──────────────────────────────────────────────────────────────

/// First non-blank line, trimmed and clipped — a title, never a paragraph.
pub(crate) fn clean_title(raw: &str) -> Option<String> {
    let line = raw.lines().map(str::trim).find(|l| !l.is_empty())?;
    let clipped: String = line.chars().take(MAX_TITLE_CHARS).collect();
    let clipped = clipped.trim().to_string();
    (!clipped.is_empty()).then_some(clipped)
}

#[derive(schemars::JsonSchema, Debug, Default, Deserialize)]
pub(crate) struct CreateThreadReq {
    #[serde(default)]
    pub(crate) title: Option<String>,
    /// Optional first message, queued on the new thread's own durable queue
    /// so it starts working immediately, in parallel with every other thread.
    #[serde(default)]
    pub(crate) instruction: Option<String>,
    /// The runtime it runs on, a model_options id such as claude-native or
    /// grok. Unset, the Baker's own. A Baker thread only: a peer runs on its
    /// creator's runtime, model and effort.
    #[serde(default)]
    pub(crate) runtime: Option<String>,
    /// The model, as the runtime's model picker names it. Unset, the
    /// runtime's current one. A Baker thread only.
    #[serde(default)]
    pub(crate) model: Option<String>,
    /// Reasoning effort, as the runtime names it (e.g. max). Unset, the
    /// runtime's current one. A Baker thread only.
    #[serde(default)]
    pub(crate) effort: Option<String>,
    /// Its operating mode. Unset, the mode of the conversation that opens it.
    /// A peer's is at most its creator's.
    #[serde(default)]
    pub(crate) mode: Option<crate::agent_control::OperatingMode>,
    /// A Baker thread works in its own git worktree, on a new baker/ branch
    /// cut from its project's main, freshly fetched. A peer always gets its
    /// own, from its creator's current HEAD.
    #[serde(default)]
    pub(crate) worktree: bool,
    /// An existing worktree of the Bakery's repo for it to work in, on the
    /// branch it has: one the operator already keeps (never the main
    /// checkout), adopted as the thread's own, so its agents write it. The
    /// operator's alone to give; not with worktree. Discarding the thread
    /// leaves the worktree where it was.
    #[serde(default)]
    pub(crate) adopt: Option<String>,
    /// What its result must satisfy: text you judge, or {criterion, check},
    /// where check is a shell command that proves it, run as a job when the
    /// thread goes idle (exit 0 passes). Every result reports a verdict for
    /// each. A Baker thread only.
    #[serde(default)]
    pub(crate) acceptance: Vec<Criterion>,
    /// What it may use of the Bakery's tools: all (the default), read or
    /// none.
    #[serde(default)]
    pub(crate) tools: Option<crate::api_tools::ToolsLimit>,
    /// The conversation opening it, when that is an agent other than the
    /// Baker (filled in for one): the new conversation is its peer, in its
    /// project, with at most its mode and Tools. The Baker names one to open
    /// a peer of it: a workspace's worker, or a project's.
    #[serde(default)]
    pub(crate) stream_id: Option<String>,
}

/// Its first message, and its title: the one asked for, else that message's
/// first line.
fn instruction_and_title(req: &CreateThreadReq) -> (Option<&str>, String) {
    let instruction = req.instruction.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let title = req
        .title
        .as_deref()
        .and_then(clean_title)
        .or_else(|| instruction.and_then(clean_title))
        .unwrap_or_else(|| DEFAULT_THREAD_TITLE.to_string());
    (instruction, title)
}

fn setting(value: &Option<String>) -> Option<String> {
    value.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

/// Refuses a model or effort the thread's runtime does not offer, naming
/// the ones it does: the picker's own catalog (`account_adapter`). A runtime
/// with no list to check against is taken at its word. Gives back how the
/// picker names the model and effort chosen, where it knows.
fn check_settings(state: &AppState, baker: &str, req: &CreateThreadReq) -> Result<(Option<String>, Option<String>), String> {
    let (model, effort) = (setting(&req.model), setting(&req.effort));
    if model.is_none() && effort.is_none() {
        return Ok((None, None));
    }
    let bakers = crate::state::latest_seat_model_id(&state.heart.lock().unwrap(), baker, Seat::Baker)
        .ok()
        .flatten();
    let Some(runtime) = setting(&req.runtime).or_else(|| bakers.clone()) else {
        return Ok((None, None));
    };
    let catalog = {
        let (state, baker, own) = (state.clone(), baker.to_string(), bakers.as_ref() == Some(&runtime));
        let runtime = runtime.clone();
        crate::api_tools::block_on(async move {
            let caps = if own {
                crate::account_adapter::capabilities_for_agent(&state, &runtime, &baker, Seat::Baker).await
            } else {
                crate::account_adapter::capabilities(&state, &runtime).await
            };
            caps.catalog
        })?
    };
    let bakery_agent::ModelCatalog::Available { models } = catalog else {
        return Ok((None, None));
    };
    let chosen = match &model {
        Some(model) => Some(models.iter().find(|m| m.id == *model).ok_or_else(|| {
            let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
            format!("{runtime} offers no model {model:?}; choose one of: {}", ids.join(", "))
        })?),
        None => None,
    };
    if let Some(effort) = &effort {
        let mut levels: Vec<&str> = chosen
            .map(|m| vec![m])
            .unwrap_or_else(|| models.iter().collect())
            .into_iter()
            .flat_map(|m| m.effort_levels.iter().map(|l| l.id.as_str()))
            .collect();
        levels.dedup();
        if !levels.is_empty() && !levels.contains(&effort.as_str()) {
            let whose = chosen.map_or(runtime.as_str(), |m| m.id.as_str());
            return Err(format!(
                "{whose} offers no effort {effort:?}; choose one of: {}",
                levels.join(", ")
            ));
        }
    }
    let effort_label = effort.as_ref().and_then(|effort| {
        let mut levels = chosen.map(|m| vec![m]).unwrap_or_else(|| models.iter().collect()).into_iter().flat_map(|m| m.effort_levels.iter());
        levels.find(|l| l.id == *effort).map(|l| l.label.clone())
    });
    Ok((chosen.map(|m| m.label.clone()), effort_label))
}

/// A new `baker/<title>-<id>` branch at `repo_root`'s newer main, freshly
/// fetched (rule 2: never the Baker's own unfinished work), checked out
/// under Bakery's own worktrees folder, and why it isn't origin's main. Cut
/// before any fact is written, so a failure leaves nothing behind.
fn cut_worktree(
    state: &AppState,
    repo_root: &str,
    title: &str,
    stream_id: &str,
) -> Result<(String, String, Option<String>), String> {
    if repo_root.trim().is_empty() {
        return Err("the Baker has no project folder to cut a worktree from".to_string());
    }
    let slug: String = crate::git::sanitize_name(&title.to_lowercase()).chars().take(40).collect();
    let name = format!("{slug}-{}", &stream_id[stream_id.len().saturating_sub(6)..]);
    let branch = format!("baker/{name}");
    let path = state.base.join("worktrees").join(&name);
    std::fs::create_dir_all(state.base.join("worktrees")).map_err(|e| e.to_string())?;
    let info = crate::git::inspect(repo_root);
    let fork = crate::git::fresh_main(std::path::Path::new(repo_root));
    crate::routes::project::add_worktree(repo_root, &path, &branch, Some(&fork.base), &info.local_branches)?;
    Ok((path.to_string_lossy().into_owned(), branch, fork.stale))
}

/// A chef's work merged into main: on its thread and on the conversation it
/// reports to (`{thread, branch, into, commit}`).
pub(crate) const MERGED_FAMILY: &str = "merge.done";

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct ThreadMerged {
    pub(crate) thread: String,
    pub(crate) branch: String,
    /// The main checkout's branch it went into.
    pub(crate) into: String,
    /// The merge commit.
    pub(crate) commit: String,
    /// Changes the thread had not committed, which did not merge: they went
    /// to the Trash with its worktree (`cleanup`), or stayed in it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) uncommitted: Option<String>,
    /// What became of a chef's thread, worktree and branch once its work was
    /// in main; none for a workspace's merge, which leaves them as they are.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) cleanup: Option<MergeCleanup>,
}

/// A merged chef's thread archived and its worktree and branch removed, as a
/// Delete removes them. Only the answer: `merge.done` says nothing of it.
#[derive(schemars::JsonSchema, Debug, Default, PartialEq, Serialize)]
pub(crate) struct MergeCleanup {
    /// The thread is archived: out of the lists, its history kept.
    pub(crate) archived: bool,
    /// Its worktree is removed.
    pub(crate) worktree_removed: bool,
    /// Its branch is deleted: its commits are in main.
    pub(crate) branch_removed: bool,
    /// What it held that is in no main (changes not committed above all) was
    /// kept in git first: the Trash lists it, and Put back brings it back.
    pub(crate) kept_in_trash: bool,
    /// Each step that didn't go, and why. What it concerns stays.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) errors: Vec<String>,
}

/// Merges a chef thread's own branch into the main checkout's branch, as
/// the conversation that opened it reviews, tests and merges chefs' work
/// when the operator says to (the permanent Baker may merge any): no
/// fast-forward, with the project's hooks, into a clean main checkout only,
/// and a conflict merges nothing. The merge is told to the conversation the
/// chef reports to, and the chef's thread is archived with its worktree and
/// branch removed ([`clean_up_chef`]). The one place an agent's git writes
/// the main checkout, which rule 1 otherwise keeps read; pushing and
/// deploying stay the operator's.
pub(crate) fn merge_thread(state: &AppState, thread: &str, message: Option<&str>, actor: &str) -> Result<ThreadMerged, String> {
    let (chef, into) = merge_target(state, thread)?;
    let reviewed = into.orc.is_some() || crate::workspaces::is_workspace(state, thread);
    if reviewed && actor != "human" {
        return Err("a workspace's work goes where it goes only when the operator approves it in the merge review".to_string());
    }
    let dir = into.dir.as_path();
    let dirty = crate::git::run(dir, &["status", "--porcelain"])?;
    if !dirty.trim().is_empty() {
        return Err(match into.orc {
            Some(_) => "the workspace has changes not committed: nothing was merged".to_string(),
            None => "the main checkout has changes of its own, the operator's: nothing was merged".to_string(),
        });
    }
    let branch = crate::git::run(dir, &["rev-parse", "--abbrev-ref", "HEAD"])?.trim().to_string();
    if reviewed {
        // The operator approves what the review showed: the same commits,
        // into the same branch.
        let shown = merge_waiting(state).items.into_iter().find(|w| w.stream_id == thread);
        let shown = shown.ok_or("nothing of this conversation waits in the merge review: propose it first")?;
        let tip = crate::git::run(dir, &["rev-parse", &format!("refs/heads/{}", chef.branch)])?;
        if shown.commits.first().map(|c| c.sha.as_str()) != Some(tip.trim()) {
            return Err(format!("{} changed since the review showed it: nothing was merged; propose it again", chef.branch));
        }
        if branch != shown.base {
            return Err(format!("what it goes into is on {branch} now, not {}: nothing was merged", shown.base));
        }
    }
    // Who hears of it, found before anything is merged.
    let home = baker_pointer(state).ok_or("there is no Baker to merge into")?;
    let mut told = vec![thread.to_string()];
    if reviewed {
        // A workspace's merge is told where its checkout is, and to the Baker.
        told.extend(into.stream_id.clone());
        told.push(home);
    } else {
        // A chef's is told to the conversation it reports to, and closes the
        // card where its ask put it, should that be another.
        told.push(reports_to(state, thread)?);
        told.extend(card_holder(state, thread));
    }
    told.dedup();
    let title = conversation_title(state, thread);
    let message = message
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("Merge {}: {title}", chef.branch));
    let args = ["merge", "--no-ff", "--no-edit", "-m", message.as_str(), chef.branch.as_str()];
    let landing = crate::snapshots::landing(state, dir);
    crate::routes::git::merged_or_aborted(state, into.stream_id.as_deref().unwrap_or(thread), dir, &args, &chef.branch)?;
    landing.landed();
    let commit = crate::git::run(dir, &["rev-parse", "HEAD"])?.trim().to_string();
    let left = crate::git::run(std::path::Path::new(&chef.worktree_path), &["status", "--porcelain"]).unwrap_or_default();
    let mut merged = ThreadMerged {
        thread: thread.to_string(),
        branch: chef.branch.clone(),
        into: branch,
        commit,
        uncommitted: (!left.trim().is_empty()).then(|| left.trim().to_string()),
        cleanup: None,
    };
    let fact = serde_json::to_string(&merged).map_err(|e| e.to_string())?;
    for stream in told {
        append(state, &stream, actor, MERGED_FAMILY, fact.clone(), vec![])?;
    }
    if !reviewed {
        merged.cleanup = Some(clean_up_chef(state, thread, &chef, actor));
    }
    Ok(merged)
}

/// A merged chef's thread is archived and its worktree and branch removed the
/// way a Delete removes them: what they hold that is in no main, changes not
/// committed above all, is kept in git first (`crate::git_trash`, the
/// Trash); a thread with a turn running, a worktree the Bakery did not make
/// (the operator's own, with its branch), or what can't be kept stays, said.
fn clean_up_chef(state: &AppState, thread: &str, chef: &crate::state::ActiveContext, actor: &str) -> MergeCleanup {
    let mut out = MergeCleanup::default();
    match set_thread_archived(state, thread, true, actor) {
        Ok(_) => out.archived = true,
        Err(e) => {
            out.errors.push(format!("{e}; its worktree and branch stay"));
            return out;
        }
    }
    if !crate::routes::lifecycle::is_bakery_owned_worktree(chef, &state.worktree_base) {
        return out;
    }
    let (repo, at) = (std::path::Path::new(&chef.repo_root), std::path::Path::new(&chef.worktree_path));
    if let Some(why) = crate::routes::lifecycle::kept_for_project(state, at) {
        out.errors.push(why);
        return out;
    }
    // No shell, runtime or preview is left in a folder that goes.
    let fenced = {
        let (state, thread) = (state.clone(), thread.to_string());
        crate::api_tools::block_on(async move {
            crate::terminals::fence_active_ptys(&state, &thread, None).await
                && crate::state::fence_acp_with_deadline(&state, &thread, std::time::Duration::from_secs(5)).await
        })
    };
    if fenced != Ok(true) {
        out.errors.push("a terminal or runtime of the thread didn't end in time; its worktree and branch stay".to_string());
        return out;
    }
    crate::views::stop_all(thread);
    crate::services::stop_copies(thread);

    let stamp = crate::git_trash::stamp_now();
    if at.is_dir() {
        let name = crate::git_trash::name_of(&chef.branch, at);
        match crate::git_trash::keep(repo, &name, &chef.branch, Some(at), &stamp).and_then(|kept| crate::routes::project::remove_worktree(state, &chef.repo_root, at).map(|()| kept)) {
            Ok(kept) => (out.worktree_removed, out.kept_in_trash) = (true, kept),
            Err(e) => {
                out.errors.push(format!("{}: {e}", at.display()));
                return out;
            }
        }
        // Its turn history goes with it, as a Delete's does.
        if let Err(e) = crate::snapshots::forget(&state.base, at) {
            out.errors.push(format!("{}'s turn history: {e}", at.display()));
        }
    }
    // Its branch once its worktree is gone, its work kept first.
    if !chef.branch.is_empty() && crate::git::inspect(&chef.repo_root).local_branches.contains(&chef.branch) {
        match crate::git_trash::keep(repo, &chef.branch, &chef.branch, None, &stamp).and_then(|kept| crate::git_trash::drop_branch(repo, &chef.branch).map(|()| kept)) {
            Ok(kept) => (out.branch_removed, out.kept_in_trash) = (true, out.kept_in_trash || kept),
            Err(e) => out.errors.push(format!("{} in {}: {e}", chef.branch, chef.repo_root)),
        }
    }
    out
}

/// The conversation the chef's latest ask put its card in.
fn card_holder(state: &AppState, thread: &str) -> Option<String> {
    let ask = state.heart.lock().unwrap().last_of_family(thread, MERGE_WAITING_FAMILY).ok().flatten()?;
    serde_json::from_str::<MergeWaiting>(&ask.payload).ok()?.spawner
}

/// What a conversation is called: a Baker thread's title, else the name
/// the operator or its opener gave it, else its own.
fn conversation_title(state: &AppState, stream_id: &str) -> String {
    if let Ok(thread) = thread_view(state, stream_id) {
        return thread.title;
    }
    let named = {
        let heart = state.heart.lock().unwrap();
        let fact = heart.last_of_family(stream_id, crate::state::CONVERSATION_RENAMED_FAMILY).ok().flatten();
        fact.and_then(|e| serde_json::from_str::<serde_json::Value>(&e.payload).ok()?["title"].as_str().map(str::to_string))
    };
    named.or_else(|| context_for(state, stream_id).ok().map(|c| c.name)).unwrap_or_default()
}

/// Where a conversation's own branch merges.
struct Target {
    /// The conversation whose checkout takes it: a worker's orc, the
    /// permanent Baker's (the Bakery's main checkout), or the one working in
    /// place on a project's main checkout; none when no one does.
    stream_id: Option<String>,
    /// That checkout, and the branch it is on.
    dir: std::path::PathBuf,
    branch: String,
    /// For a worker's merge into its workspace, the orc.
    orc: Option<String>,
}

/// A conversation's checkout and where its branch merges: a workspace
/// worker's into its orc's workspace; a Baker thread's into the Bakery's
/// main checkout; a workspace's into its repository's.
fn merge_target(state: &AppState, thread: &str) -> Result<(crate::state::ActiveContext, Target), String> {
    let chef = context_for(state, thread)?;
    let orc = opened_by(state, thread).filter(|orc| crate::workspaces::is_workspace(state, orc));
    let target = match orc {
        Some(orc) => {
            let ctx = context_for(state, &orc)?;
            Target { stream_id: Some(orc.clone()), dir: ctx.worktree_path.into(), branch: ctx.branch, orc: Some(orc) }
        }
        None if is_baker_thread_stream(state, thread) => bakery_main(state)?,
        None if crate::workspaces::is_workspace(state, thread) => main_of(state, &chef)?,
        None => return Err("only a Baker thread's, a workspace's or a workspace worker's work merges this way".to_string()),
    };
    if !crate::tools::own_checkout(state, &chef) || chef.branch == target.branch {
        return Err("this conversation works in place: it has no branch of its own to merge".to_string());
    }
    Ok((chef, target))
}

/// The Bakery's main checkout: the permanent Baker's.
fn bakery_main(state: &AppState) -> Result<Target, String> {
    let home = baker_pointer(state).ok_or("there is no Baker to merge into")?;
    let ctx = context_for(state, &home)?;
    Ok(Target { stream_id: Some(home), dir: ctx.worktree_path.into(), branch: ctx.branch, orc: None })
}

/// The main checkout of a workspace's repository: the Bakery's, else its
/// project's, with the conversation working in place there when there is
/// one.
fn main_of(state: &AppState, workspace: &crate::state::ActiveContext) -> Result<Target, String> {
    let bakery = bakery_main(state)?;
    if crate::git::same_repository(&bakery.dir, std::path::Path::new(&workspace.worktree_path)) {
        return Ok(bakery);
    }
    let dir = std::path::PathBuf::from(&workspace.repo_root);
    let branch = crate::git::run(&dir, &["rev-parse", "--abbrev-ref", "HEAD"])?.trim().to_string();
    let streams = crate::state::known_streams(state)?;
    let in_place = streams
        .into_iter()
        .find(|s| !s.discarded && s.stream_id != workspace.stream_id && crate::state::same_path(&s.worktree_path, &workspace.repo_root));
    Ok(Target { stream_id: in_place.map(|s| s.stream_id), dir, branch, orc: None })
}

/// A chef's work waiting to merge: on its thread and on the conversation it
/// reports to (a card in each), closed by its `merge.done`.
pub(crate) const MERGE_WAITING_FAMILY: &str = "merge.waiting";
/// A chef told its work overlaps another's waiting to merge (once each).
pub(crate) const MERGE_OVERLAP_FAMILY: &str = "merge.overlap";

#[derive(schemars::JsonSchema, Debug, Clone, Serialize, Deserialize)]
pub(crate) struct MergeWaiting {
    /// The chef's thread, the workspace, or the workspace's worker.
    pub(crate) stream_id: String,
    pub(crate) name: String,
    pub(crate) project_id: Option<String>,
    pub(crate) branch: String,
    /// What it merges into: the main checkout's branch, or a worker's
    /// workspace's.
    pub(crate) base: String,
    /// The conversation whose checkout it merges into: the permanent
    /// Baker's (the main checkout), or a worker's orc.
    #[serde(default)]
    pub(crate) into_stream_id: String,
    /// A worker's orc, which proposed it: it merges into the workspace
    /// when the operator approves it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) orc: Option<String>,
    /// The conversation told it is ready, which reviews it and brings the
    /// operator the merge review: the chef's spawner, the permanent Baker
    /// when that one is archived or gone. A workspace's work has none: the
    /// operator approves it in the merge review.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) spawner: Option<String>,
    /// Its name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) spawner_name: Option<String>,
    /// Newest first; at most 50.
    pub(crate) commits: Vec<crate::routes::git::PushCommit>,
    pub(crate) files: Vec<String>,
    pub(crate) adds: u64,
    pub(crate) dels: u64,
    /// What the chef said about it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) note: Option<String>,
    pub(crate) asked_at: String,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct MergeWaitingList {
    pub(crate) count: usize,
    /// Newest first.
    pub(crate) items: Vec<MergeWaiting>,
}

/// A chef's own branch and the one it merges into: its context and that
/// branch.
fn chef_and_base(state: &AppState, thread: &str) -> Result<(crate::state::ActiveContext, String), String> {
    merge_target(state, thread).map(|(chef, target)| (chef, target.branch))
}

/// The files a chef's branch changes since `base`, with lines added and
/// removed.
fn changed(dir: &std::path::Path, base: &str, head: &str) -> Result<(Vec<String>, u64, u64), String> {
    let numstat = crate::git::run(dir, &["diff", "--numstat", &format!("{base}...{head}")])?;
    let (mut files, mut adds, mut dels) = (Vec::new(), 0, 0);
    for line in numstat.lines() {
        let mut parts = line.splitn(3, '\t');
        let (a, d, path) = (parts.next(), parts.next(), parts.next());
        let Some(path) = path else {
            continue;
        };
        adds += a.and_then(|n| n.parse::<u64>().ok()).unwrap_or(0);
        dels += d.and_then(|n| n.parse::<u64>().ok()).unwrap_or(0);
        files.push(path.to_string());
    }
    Ok((files, adds, dels))
}

/// A chef says its work is ready: committed on its own branch. A card in
/// its own conversation and in the one it reports to, an item in the waiting
/// list, and a message to the one it reports to, its spawner (the permanent
/// Baker when that is archived or gone), to review, test and audit it and
/// bring the operator the merge review. The permanent Baker is told nothing
/// of other conversations' chefs; the waiting list is where it looks. Asking
/// again replaces the item; its merge closes it.
pub(crate) fn ready_to_merge(state: &AppState, thread: &str, note: Option<&str>, actor: &str) -> Result<MergeWaiting, String> {
    let (chef, target) = merge_target(state, thread)?;
    let base = target.branch.clone();
    let dir = std::path::Path::new(&chef.worktree_path);
    let dirty = crate::git::run(dir, &["status", "--porcelain"])?;
    if !dirty.trim().is_empty() {
        return Err(format!("commit your work first: {} files have changes not committed", dirty.lines().count()));
    }
    // What the review shows is what a merge takes: its own branch, never
    // whatever is checked out.
    let head = format!("refs/heads/{}", chef.branch);
    let log = crate::git::run(dir, &["log", "-n", "50", "--format=%H%x1f%s", &format!("{base}..{head}")])?;
    let commits: Vec<crate::routes::git::PushCommit> = log
        .lines()
        .filter_map(|line| line.split_once('\u{1f}'))
        .map(|(sha, subject)| crate::routes::git::PushCommit { sha: sha.to_string(), subject: subject.to_string() })
        .collect();
    if commits.is_empty() {
        return Err(format!("nothing to merge: {} has no commits beyond {base}", chef.branch));
    }
    let (files, adds, dels) = changed(dir, &base, &head)?;
    let home = baker_pointer(state).ok_or("there is no Baker to merge into")?;
    // A workspace's work is the operator's to approve in the merge review; a
    // chef's is reported to the conversation that opened it.
    let reviewed = target.orc.is_some() || crate::workspaces::is_workspace(state, thread);
    let spawner = if reviewed { None } else { Some(reports_to(state, thread)?) };
    // On its own line: inside the struct below, the Heart's guard would live
    // to the statement's end, and the titles after it lock the Heart again.
    let project_id = crate::state::socket_project(&state.heart.lock().unwrap(), thread).0;
    let waiting = MergeWaiting {
        stream_id: thread.to_string(),
        name: conversation_title(state, thread),
        project_id,
        branch: chef.branch.clone(),
        base: base.clone(),
        into_stream_id: target.stream_id.clone().unwrap_or_default(),
        orc: target.orc.clone(),
        spawner_name: spawner.as_deref().map(|s| conversation_title(state, s)),
        spawner: spawner.clone(),
        commits,
        files,
        adds,
        dels,
        note: note.map(str::trim).filter(|n| !n.is_empty()).map(str::to_string),
        asked_at: chrono::Utc::now().to_rfc3339(),
    };
    let fact = serde_json::to_string(&waiting).map_err(|e| e.to_string())?;
    let mut told = vec![thread.to_string()];
    match &spawner {
        Some(spawner) => told.push(spawner.clone()),
        None => {
            told.extend(target.stream_id.clone());
            told.push(home);
        }
    }
    told.dedup();
    for stream in told {
        append(state, &stream, actor, MERGE_WAITING_FAMILY, fact.clone(), vec![])?;
    }
    let Some(spawner) = spawner else {
        return Ok(waiting);
    };
    let said = waiting.note.as_deref().map(|n| format!("\n{}", crate::messages::guarded(n))).unwrap_or_default();
    let message = crate::messages::MessageNote {
        id: crate::node::mint_id("msg"),
        from: thread.to_string(),
        name: waiting.name.clone(),
        reply_to: None,
        text: waiting.note.clone().unwrap_or_default(),
    };
    let instruction = format!(
        "[Ready to merge, from {} ({thread})] {} into {base}: {} commits, {} files (+{} −{}).{said}\n\nReview, test and audit it, then bring the operator the merge review and wait. Merge it with thread_merge {{\"stream_id\": \"{thread}\"}} only when the operator has told you to for this case.",
        crate::messages::one_line(&waiting.name),
        waiting.branch,
        waiting.commits.len(),
        waiting.files.len(),
        waiting.adds,
        waiting.dels
    );
    let turn = crate::routes::agent::AgentTurnReq {
        stream_id: spawner,
        file_path: None,
        instruction,
        seat: Some(Seat::Baker),
        model_id: None,
        effort: None,
        actor: None,
    };
    crate::agent_control::enqueue_message(state, turn, actor, message)?;
    Ok(waiting)
}

/// Every chef's work waiting to merge: each thread's newest ask that no
/// merge has closed since.
pub(crate) fn merge_waiting(state: &AppState) -> MergeWaitingList {
    let Some(home) = baker_pointer(state) else {
        return MergeWaitingList { count: 0, items: Vec::new() };
    };
    // Every ask and every merge, wherever it was told: the permanent Baker's
    // own conversation holds only those of the chefs that report to it, but
    // each chef's has its own, so the list is whole whoever got the card.
    let events = state.heart.lock().unwrap().events_after(0, &[MERGE_WAITING_FAMILY, MERGED_FAMILY]).unwrap_or_default();
    let mut open: Vec<MergeWaiting> = Vec::new();
    for (_, e) in events {
        match e.family.as_str() {
            MERGE_WAITING_FAMILY => {
                if let Ok(mut item) = serde_json::from_str::<MergeWaiting>(&e.payload) {
                    if item.into_stream_id.is_empty() {
                        item.into_stream_id = home.clone();
                    }
                    open.retain(|o| o.stream_id != item.stream_id);
                    open.push(item);
                }
            }
            MERGED_FAMILY => {
                if let Ok(done) = serde_json::from_str::<serde_json::Value>(&e.payload) {
                    open.retain(|o| Some(o.stream_id.as_str()) != done["thread"].as_str());
                }
            }
            _ => {}
        }
    }
    // A deleted conversation's work waits no more.
    {
        let heart = state.heart.lock().unwrap();
        open.retain(|item| !crate::state::discarded_of(&heart, &item.stream_id));
    }
    // Merged some other way (the operator's Merge review, say): its head is
    // already in what it waits to merge into, in its own repository.
    if let Ok(main) = context_for(state, &home) {
        open.retain(|item| {
            let dir = context_for(state, &item.stream_id).map_or_else(|_| main.worktree_path.clone(), |c| c.worktree_path);
            let head = item.commits.first().map(|c| c.sha.as_str()).unwrap_or_default();
            crate::git::run(std::path::Path::new(&dir), &["merge-base", "--is-ancestor", head, &item.base]).is_err()
        });
    }
    open.reverse();
    MergeWaitingList { count: open.len(), items: open }
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct MergeThreadReq {
    pub(crate) stream_id: String,
    #[serde(default)]
    pub(crate) message: Option<String>,
}

/// `POST /thread/merge`: the operator merges a chef's work into main, as
/// its spawner's thread_merge does (the Merge button on its card).
pub(crate) async fn merge_thread_route(
    State(st): State<AppState>,
    Json(req): Json<MergeThreadReq>,
) -> Result<Json<ThreadMerged>, (StatusCode, String)> {
    if crate::actor::agent_calling() {
        return Err((StatusCode::FORBIDDEN, "the Baker merges with thread_merge".to_string()));
    }
    crate::actor::spawn_blocking(move || merge_thread(&st, &req.stream_id, req.message.as_deref(), "human"))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct MergeReadyReq {
    /// The conversation asking.
    pub(crate) stream_id: String,
    /// A worker it opened, whose branch it proposes for its workspace; its
    /// own branch, for main, when absent.
    #[serde(default)]
    pub(crate) worker: Option<String>,
    /// What it is, and how it was tested.
    #[serde(default)]
    pub(crate) note: Option<String>,
}

/// `POST /merge/ready`: a workspace's orc proposes a worker's branch for
/// the workspace, or its own for main; either waits in the merge review.
/// A worker's own goes through its orc.
pub(crate) async fn merge_ready_route(
    State(st): State<AppState>,
    Json(req): Json<MergeReadyReq>,
) -> Result<Json<MergeWaiting>, (StatusCode, String)> {
    crate::actor::spawn_blocking(move || {
        let thread = proposed(&st, &req.stream_id, req.worker.as_deref())?;
        ready_to_merge(&st, &thread, req.note.as_deref(), &crate::actor::attribute("human"))
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map(Json)
    .map_err(|e| (StatusCode::BAD_REQUEST, e))
}

/// Whose branch `asking` proposes: a worker's it opened as a workspace's
/// orc, else its own, unless it is a workspace's worker.
fn proposed(state: &AppState, asking: &str, worker: Option<&str>) -> Result<String, String> {
    let orc_of = |stream: &str| opened_by(state, stream).filter(|orc| crate::workspaces::is_workspace(state, orc));
    match worker {
        Some(worker) if orc_of(worker).as_deref() == Some(asking) => Ok(worker.to_string()),
        Some(_) => Err("that is not a worker this workspace opened".to_string()),
        None if orc_of(asking).is_some() => Err("your orc proposes your work for its workspace: tell it yours is ready".to_string()),
        None => Ok(asking.to_string()),
    }
}

/// For a chef whose work overlaps another's waiting to merge (the same
/// branch, or files both change): a line for its turn, and once per such
/// ask a `merge.overlap` on its thread, so it builds on that work rather
/// than doing it again.
pub(crate) fn already_waiting_note(state: &AppState, thread: &str) -> Option<String> {
    // Only a chef has work to overlap, which spares every other turn start
    // a look through the whole list.
    let (chef, base) = chef_and_base(state, thread).ok()?;
    let others: Vec<MergeWaiting> = merge_waiting(state).items.into_iter().filter(|w| w.stream_id != thread).collect();
    if others.is_empty() {
        return None;
    }
    let dir = std::path::Path::new(&chef.worktree_path);
    let mut mine = changed(dir, &base, "HEAD").map(|(files, ..)| files).unwrap_or_default();
    let dirty = crate::git::run(dir, &["status", "--porcelain"]).unwrap_or_default();
    mine.extend(dirty.lines().filter_map(|l| l.get(3..)).map(|p| p.trim().to_string()));
    let told: Vec<String> = state
        .heart
        .lock()
        .unwrap()
        .events_of_family(thread, MERGE_OVERLAP_FAMILY)
        .unwrap_or_default()
        .into_iter()
        .map(|e| e.payload)
        .collect();
    let mut lines = Vec::new();
    for other in others {
        let shared: Vec<&String> = other.files.iter().filter(|f| mine.contains(f)).collect();
        if shared.is_empty() && other.branch != chef.branch {
            continue;
        }
        let files: Vec<&str> = shared.iter().take(5).map(|f| f.as_str()).collect();
        lines.push(format!(
            "Already waiting to merge: {} ({}, {}) also changes {}. Build on it once it merges, or coordinate with send_message to \"{}\".",
            other.name,
            other.branch,
            other.stream_id,
            if files.is_empty() { "this branch".to_string() } else { files.join(", ") },
            other.stream_id
        ));
        let fact = serde_json::json!({"waiting": other.stream_id, "asked_at": other.asked_at, "files": files});
        if !told.iter().any(|t| t.contains(&other.asked_at) && t.contains(&other.stream_id)) {
            let _ = append(state, thread, "node", MERGE_OVERLAP_FAMILY, fact.to_string(), vec![]);
        }
    }
    (!lines.is_empty()).then(|| lines.join("\n"))
}

pub(crate) async fn merge_waiting_route(State(st): State<AppState>) -> Result<Json<MergeWaitingList>, (StatusCode, String)> {
    crate::actor::spawn_blocking(move || Json(merge_waiting(&st)))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// An existing linked worktree of the Bakery's repo, adopted by a new
/// thread as its own: its path and the branch it has. The operator's to
/// give; never the main checkout, nor the tree this node runs from.
fn adopt_worktree(state: &AppState, repo_root: &str, path: &str, actor: &str) -> Result<(String, String, Option<String>), String> {
    if actor != "human" {
        return Err("only the operator gives a thread a worktree that already exists".to_string());
    }
    let dir = std::fs::canonicalize(path).map_err(|e| format!("{path}: {e}"))?;
    if crate::git::linked_worktree_git_dirs(&dir).is_none() {
        return Err(format!("{path} is not a linked worktree: the main checkout stays only read"));
    }
    if !crate::git::same_repository(std::path::Path::new(repo_root), &dir) {
        return Err(format!("{path} is not a worktree of the Bakery's repository"));
    }
    let running = crate::versions::source_root(state).is_some_and(|root| std::fs::canonicalize(root).is_ok_and(|r| r == dir));
    if running {
        return Err(format!("{path} is the tree this Bakery runs from: it stays only read"));
    }
    let branch = crate::git::run(&dir, &["rev-parse", "--abbrev-ref", "HEAD"])?.trim().to_string();
    Ok((dir.to_string_lossy().into_owned(), branch, None))
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct CreateThreadResp {
    pub(crate) thread: BakerThreadView,
    /// The first message's durable queue record, when one was given.
    #[schemars(with = "Option<crate::api_catalog::EventShape>")]
    pub(crate) queued: Option<Event>,
    /// A caution about how it was opened.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) note: Option<String>,
}

/// Mint a new Baker thread. `actor` is who asked (`"human"` for the route,
/// `agent:<stream>:baker` for the op) and is recorded on every fact written
/// here; the conversation it names is the thread's spawner.
pub(crate) fn create_thread(
    state: &AppState,
    req: &CreateThreadReq,
    actor: &str,
    env_root: Option<&str>,
    detect_from: Option<&std::path::Path>,
) -> Result<CreateThreadResp, String> {
    let (instruction, title) = instruction_and_title(req);
    let home = crate::routes::baker::ensure_permanent_baker(state, env_root, detect_from)?;
    let open = list_threads(state, false)?
        .iter()
        .filter(|t| !t.main)
        .count();
    if open >= MAX_OPEN_THREADS {
        return Err(format!(
            "{open} Baker threads are already open (limit {MAX_OPEN_THREADS}) — archive one first"
        ));
    }
    // The exact factory scope the permanent Baker has — its recorded root,
    // in place and read-scoped, or none — never a new or wider grant.
    let scope = context_for(state, &home.stream_id)?;
    let labels = check_settings(state, &home.stream_id, req)?;

    let stream_id = crate::node::mint_id("socket");
    let (worktree_path, branch, fork_note) = match (&req.adopt, req.worktree) {
        (Some(_), true) => return Err("a thread either adopts a worktree or gets a new one, not both".to_string()),
        (Some(path), false) => adopt_worktree(state, &scope.repo_root, path, actor)?,
        (None, true) => cut_worktree(state, &scope.repo_root, &title, &stream_id)?,
        (None, false) => (scope.worktree_path.clone(), scope.branch.clone(), None),
    };
    let onboarded = append(
        state,
        &stream_id,
        actor,
        "project.onboarded",
        serde_json::json!({
            "repo": scope.repo_root,
            "repo_root": scope.repo_root,
            "project": "Baker",
            // Never `baker-…`: that prefix is how legacy `/baker/assign`
            // branch jobs are recognized (`baker_streams_for_repo`).
            "name": format!("Baker · {title}"),
            "worktree_path": worktree_path,
            "branch": branch,
            "opened_by_node": state.node.lock().unwrap().node_id.clone(),
            // Its own worktree is Bakery's to remove on discard.
            "created_by_bakery": req.worktree,
            "fork_note": fork_note,
        })
        .to_string(),
        vec![],
    )?;
    let kind = append(
        state,
        &stream_id,
        actor,
        SOCKET_KIND_FAMILY,
        serde_json::to_string(&SocketKindRecord {
            kind: SocketKind::Factory,
        })
        .unwrap(),
        vec![onboarded.event_hash.clone()],
    )?;
    append(
        state,
        &stream_id,
        actor,
        THREAD_OPENED_FAMILY,
        serde_json::to_string(&ThreadOpened {
            baker_stream_id: home.stream_id.clone(),
            scope: FACTORY_SCOPE.to_string(),
            title,
            spawner: Some(spawner_for(state, actor, &home.stream_id)),
            acceptance: req.acceptance.clone(),
        })
        .unwrap(),
        vec![kind.event_hash.clone()],
    )?;
    crate::state::claim_ownership(state, &stream_id)?;
    inherit_seat(state, (&home.stream_id, Seat::Baker), (&stream_id, Seat::Baker));
    // Its whole selector is in place before the first message is queued:
    // the runtime, the mode, the model and the effort.
    if let Some(runtime) = setting(&req.runtime) {
        crate::routes::seats::seat_assign_checked(state, &stream_id, Seat::Baker, &runtime)
            .map_err(|(_, e)| e)?;
    }
    if let Some(mode) = req.mode {
        crate::agent_control::set_operating_mode(state, &stream_id, Seat::Baker, mode)?;
    }
    // Its model and effort are its selector's from the start, shown in the
    // thread as any pick is, and every turn runs on them.
    let runtime = crate::state::latest_seat_model_id(&state.heart.lock().unwrap(), &stream_id, Seat::Baker).ok().flatten().unwrap_or_default();
    let chosen = [(crate::selection::MODEL, setting(&req.model), labels.0), (crate::selection::EFFORT, setting(&req.effort), labels.1)];
    for (category, value, label) in chosen.iter().filter(|(_, value, _)| value.is_some()) {
        crate::selection::choose(state, &stream_id, Seat::Baker, &runtime, category, value.as_deref(), label.as_deref(), actor)?;
    }
    crate::api_tools::set_tools_at_creation(state, &stream_id, req.tools)?;

    let queued = match instruction {
        Some(text) => Some(message_thread(state, &stream_id, text, actor)?),
        None => None,
    };
    // Checks are not refused in place: a read-only one (test -d, grep) works.
    let in_place_checks = !req.worktree && req.adopt.is_none() && req.acceptance.iter().any(|c| c.check().is_some());
    Ok(CreateThreadResp {
        thread: thread_view(state, &stream_id)?,
        queued,
        note: in_place_checks.then(|| {
            "this thread works in place, so its workspace is read-only; checks that build or test \
             will fail. Use worktree: true."
                .to_string()
        }),
    })
}

/// How freely a mode lets an agent act, least first.
fn freedom(mode: OperatingMode) -> u8 {
    match mode {
        OperatingMode::Plan => 0,
        OperatingMode::Manual => 1,
        OperatingMode::AutoEdit => 2,
        OperatingMode::Auto => 3,
    }
}

/// How many open peers agents may open in one project, unless the operator
/// chose otherwise (`/agents/peer-limit`).
pub(crate) const DEFAULT_PEER_LIMIT: usize = 24;

pub(crate) fn peer_limit(state: &AppState) -> usize {
    state.node.lock().unwrap().peer_limit.unwrap_or(DEFAULT_PEER_LIMIT)
}

/// The conversation that opened `stream_id` as its peer: a worker's orc.
pub(crate) fn opened_by_locked(heart: &Heart, stream_id: &str) -> Option<String> {
    let ev = heart.first_of_family(stream_id, PEER_OPENED_FAMILY).ok().flatten()?;
    let opened: serde_json::Value = serde_json::from_str(&ev.payload).ok()?;
    opened["opened_by"].as_str().map(str::to_string)
}

pub(crate) fn opened_by(state: &AppState, stream_id: &str) -> Option<String> {
    opened_by_locked(&state.heart.lock().unwrap(), stream_id)
}

/// The open peers (neither archived nor discarded) `creator` opened itself,
/// most recently active first: a workspace's workers, for its orc.
pub(crate) fn peers_of(state: &AppState, creator: &str) -> Result<Vec<crate::state::StreamRef>, String> {
    let streams = crate::state::known_streams(state)?;
    let heart = state.heart.lock().unwrap();
    Ok(streams
        .into_iter()
        .filter(|s| !s.discarded && !crate::agents::socket_archived_locked(&heart, &s.stream_id))
        .filter(|s| opened_by_locked(&heart, &s.stream_id).as_deref() == Some(creator))
        .collect())
}

/// The open peers (neither archived nor discarded) that count toward the
/// operator's limit for one `creator` opens: every peer in its project, or
/// in its workspace (the orc and everything under it), so peers opening
/// peers stay bounded.
fn open_peers(state: &AppState, creator: &str) -> Result<usize, String> {
    let streams = crate::state::known_streams(state)?;
    let heart = state.heart.lock().unwrap();
    let project = |s: &str| crate::state::socket_project(&heart, s).0;
    let workspace = |s: &str| crate::workspaces::workspace_of_locked(&heart, s);
    let (mine, ours) = (project(creator), workspace(creator));
    Ok(streams
        .iter()
        .filter(|s| !s.discarded && !crate::agents::socket_archived_locked(&heart, &s.stream_id))
        .filter(|s| opened_by_locked(&heart, &s.stream_id).is_some())
        .filter(|s| match &mine {
            Some(_) => project(&s.stream_id) == mine,
            None => ours.is_some() && workspace(&s.stream_id) == ours,
        })
        .count())
}

/// A peer that an agent other than the Baker opens: a new conversation in
/// its project (a workspace's orc has none: its workers are the Bakery's),
/// on its own worktree of its repository and on its runtime, in at most
/// its mode: a freer one is refused before anything is made. A worktree
/// that already exists is adopted only when the operator gives it.
pub(crate) fn create_peer(
    state: &AppState,
    req: &CreateThreadReq,
    creator: &str,
    actor: &str,
) -> Result<CreateThreadResp, String> {
    let project_id = crate::state::socket_project(&state.heart.lock().unwrap(), creator).0;
    if project_id.is_none() && !crate::workspaces::is_workspace(state, creator) {
        return Err("a chat with no project opens no peers".to_string());
    }
    let ctx = context_for(state, creator)?;
    let seat = crate::routes::agent::default_seat_for(state, &ctx);
    let own_mode = crate::agent_control::operating_mode_for(state, creator, &crate::node::agent_id(creator, seat));
    let mode = req.mode.unwrap_or(own_mode);
    // Its Tools need no cap: only an agent with all of them may open a peer.
    let tools = req.tools.unwrap_or_else(|| crate::api_tools::tools_limit(state, creator));
    if freedom(mode) > freedom(own_mode) {
        return Err(format!("a peer's mode is at most yours ({})", own_mode.as_str()));
    }
    if [&req.runtime, &req.model, &req.effort].into_iter().any(|s| setting(s).is_some()) || !req.acceptance.is_empty() {
        return Err("a peer runs on your runtime, model and effort, and acceptance checks are the Baker's: leave them out".to_string());
    }
    let limit = peer_limit(state);
    if open_peers(state, creator)? >= limit {
        let whole = if project_id.is_some() { "project" } else { "workspace" };
        return Err(format!("this {whole} is at the operator's limit of {limit} open peers: archive one first"));
    }
    let (instruction, title) = instruction_and_title(req);
    let adopted = match req.adopt.as_deref() {
        Some(_) if crate::actor::agent_calling() => {
            return Err("only the operator gives a peer a worktree that already exists".to_string());
        }
        Some(path) => Some(adopt_worktree(state, &ctx.repo_root, path, "human")?),
        None => None,
    };
    // A new one starts from its creator's current HEAD: the branch checked
    // out there now, or that commit when detached.
    let base = match (&adopted, crate::git::inspect(&ctx.worktree_path).current_branch) {
        (Some(_), _) => None,
        (None, Some(branch)) => Some(branch),
        (None, None) => Some(crate::git::run(std::path::Path::new(&ctx.worktree_path), &["rev-parse", "HEAD"])?.trim().to_string()),
    };

    let id = crate::node::mint_id("peer");
    let slug: String = crate::git::sanitize_name(&title.to_lowercase()).chars().take(40).collect();
    let opened = crate::routes::project::open_stream(
        state,
        &crate::routes::project::OpenReq {
            repo: ctx.repo_root.clone(),
            name: Some(format!("{slug}-{}", &id[id.len().saturating_sub(6)..])),
            branch: adopted.as_ref().map(|(_, branch, _)| branch.clone()),
            base_branch: base,
            worktree_path: adopted.map(|(path, ..)| path),
            project_id: project_id.clone(),
            tools: Some(tools),
        },
    )?;
    if opened.attached {
        return Err("a conversation by that name is already open: try again".to_string());
    }
    let peer = opened.context.stream_id.clone();
    if let Some(project_id) = &project_id {
        crate::catalog::link_socket_to_project(state, &peer, project_id, &opened.context.worktree_path, true)?;
    }
    append(
        state,
        &peer,
        actor,
        PEER_OPENED_FAMILY,
        serde_json::json!({ "opened_by": creator, "title": title }).to_string(),
        vec![opened.event.event_hash.clone()],
    )?;
    let named = serde_json::json!({ "title": title }).to_string();
    append(state, &peer, actor, crate::state::CONVERSATION_RENAMED_FAMILY, named, vec![])?;
    inherit_seat(state, (creator, seat), (&peer, Seat::Orchestrator));
    crate::agent_control::set_operating_mode(state, &peer, Seat::Orchestrator, mode)?;
    let queued = match instruction {
        Some(text) => Some(crate::agent_control::enqueue_as(
            state,
            crate::routes::agent::AgentTurnReq {
                stream_id: peer.clone(),
                file_path: None,
                instruction: text.to_string(),
                seat: Some(Seat::Orchestrator),
                model_id: None,
                effort: None,
                actor: None,
            },
            vec![],
            false,
            actor,
        )?),
        None => None,
    };
    let model_id = crate::state::latest_seat_model_id(&state.heart.lock().unwrap(), &peer, Seat::Orchestrator)
        .ok()
        .flatten();
    Ok(CreateThreadResp {
        thread: BakerThreadView {
            agent_id: crate::node::agent_id(&peer, Seat::Orchestrator),
            main: false,
            title,
            archived: false,
            busy: false,
            last_ts: opened.event.ts.clone(),
            model_id,
            stream_id: peer,
        },
        queued,
        note: None,
    })
}

/// A new conversation answers on whatever its opener currently runs on (a
/// Baker thread on the Baker's, a peer on its creator's) — the same
/// assignment record a human's `/seat/assign` writes — and starts from the
/// opener's acknowledged native preferences (model/effort). Those are desired
/// values, not capabilities: every new native session re-validates each one
/// against its own setup response before applying it. Best effort: a thread
/// that could not inherit simply asks for a model like any new socket.
pub(crate) fn inherit_seat(state: &AppState, (from, from_seat): (&str, Seat), (to, to_seat): (&str, Seat)) {
    let model = {
        let heart = state.heart.lock().unwrap();
        crate::state::latest_seat_model_id(&heart, from, from_seat)
    };
    if let Ok(Some(model_id)) = model {
        if let Err((_, e)) =
            crate::routes::seats::seat_assign_checked(state, to, to_seat, &model_id)
        {
            eprintln!("bakery-node: {to} could not inherit model {model_id}: {e}");
        }
    }

    let from_agent = crate::node::agent_id(from, from_seat);
    let to_agent = crate::node::agent_id(to, to_seat);
    let acknowledged = state
        .heart
        .lock()
        .unwrap()
        .events_of_family(from, crate::routes::agent::NATIVE_CONFIG_ACKNOWLEDGED)
        .unwrap_or_default();
    let mut latest: BTreeMap<(String, String), serde_json::Value> = BTreeMap::new();
    for ev in acknowledged.iter().rev() {
        let Ok(payload) = serde_json::from_str::<serde_json::Value>(&ev.payload) else {
            continue;
        };
        if payload.get("agent_id").and_then(|v| v.as_str()) != Some(from_agent.as_str()) {
            continue;
        }
        let (Some(account), Some(config)) = (
            payload.get("account_id").and_then(|v| v.as_str()),
            payload.get("config_id").and_then(|v| v.as_str()),
        ) else {
            continue;
        };
        latest
            .entry((account.to_string(), config.to_string()))
            .or_insert(payload);
    }
    for (_, mut payload) in latest {
        payload["agent_id"] = serde_json::Value::String(to_agent.clone());
        payload["inherited_from"] = serde_json::Value::String(from_agent.clone());
        let _ = append(
            state,
            to,
            "node",
            crate::routes::agent::NATIVE_CONFIG_ACKNOWLEDGED,
            payload.to_string(),
            vec![],
        );
    }
}

/// Resolve a Baker conversation for a mutating op, refusing anything else.
fn require_thread(state: &AppState, stream_id: &str) -> Result<(), String> {
    if crate::baker_ops::is_permanent_baker_stream(state, stream_id) {
        return Err("the main Baker conversation cannot be renamed or archived".to_string());
    }
    if !is_baker_thread_stream(state, stream_id) {
        return Err(format!("{stream_id} is not a Baker thread"));
    }
    Ok(())
}

pub(crate) fn rename_thread(
    state: &AppState,
    stream_id: &str,
    title: &str,
    actor: &str,
) -> Result<BakerThreadView, String> {
    require_thread(state, stream_id)?;
    let title = clean_title(title).ok_or_else(|| "title must not be empty".to_string())?;
    append(
        state,
        stream_id,
        actor,
        THREAD_RENAMED_FAMILY,
        serde_json::to_string(&ThreadRenamed { title }).unwrap(),
        vec![],
    )?;
    thread_view(state, stream_id)
}

/// Archive (hide) or restore a thread. Display-only and reversible: history,
/// root, membership, and model are untouched, so — like every other
/// reversible op — no approval gate is needed. A thread with a turn in flight
/// must be stopped first rather than hidden mid-work. Permanent deletion is
/// not this: it stays the human-only `POST /stream/discard`.
pub(crate) fn set_thread_archived(
    state: &AppState,
    stream_id: &str,
    archived: bool,
    actor: &str,
) -> Result<BakerThreadView, String> {
    require_thread(state, stream_id)?;
    if archived && busy_streams(state).contains(stream_id) {
        return Err("this thread has a turn running — stop it before archiving".to_string());
    }
    let current = {
        let heart = state.heart.lock().unwrap();
        archived_locked(&heart, stream_id)
    };
    if current == archived {
        return thread_view(state, stream_id);
    }
    if !archived {
        let open = list_threads(state, false)?
            .iter()
            .filter(|t| !t.main)
            .count();
        if open >= MAX_OPEN_THREADS {
            return Err(format!(
                "{open} Baker threads are already open (limit {MAX_OPEN_THREADS}) — archive one first"
            ));
        }
    }
    append(
        state,
        stream_id,
        actor,
        THREAD_ARCHIVED_FAMILY,
        serde_json::to_string(&ThreadArchived { archived }).unwrap(),
        vec![],
    )?;
    thread_view(state, stream_id)
}

/// Queue `text` as the next message on a Baker conversation — the same
/// durable queue the composer's follow-up uses, so Stop, pause, and restart
/// recovery apply unchanged. `actor` is recorded on the queue fact and on the
/// turn it starts: a Baker-spawned brief never reads as the human's words.
pub(crate) fn message_thread(
    state: &AppState,
    stream_id: &str,
    text: &str,
    actor: &str,
) -> Result<Event, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("message must not be empty".to_string());
    }
    if !crate::baker_ops::is_baker_scope_stream(state, stream_id) {
        return Err(format!("{stream_id} is not a Baker conversation"));
    }
    let archived = archived_locked(&state.heart.lock().unwrap(), stream_id);
    if archived {
        return Err("that thread is archived — restore it before messaging it".to_string());
    }
    crate::agent_control::enqueue_as(
        state,
        crate::routes::agent::AgentTurnReq {
            stream_id: stream_id.to_string(),
            file_path: None,
            instruction: text.to_string(),
            seat: Some(Seat::Baker),
            model_id: None,
            effort: None,
            actor: None,
        },
        vec![],
        false,
        actor,
    )
}

/// A thread still carrying the placeholder title takes the first line of the
/// first message it is sent. Display only; never touches identity. Called
/// right after `agent.turn.started` lands, so it runs once per turn at most
/// and is a cheap no-op for every non-thread stream.
pub(crate) fn maybe_autotitle(state: &AppState, stream_id: &str, instruction: &str) {
    let baker = baker_pointer(state);
    let rename = {
        let heart = state.heart.lock().unwrap();
        if !is_thread_locked(&heart, baker.as_deref(), stream_id) {
            return;
        }
        let renamed = heart
            .last_of_family(stream_id, THREAD_RENAMED_FAMILY)
            .ok()
            .flatten()
            .is_some();
        let placeholder =
            opened_record(&heart, stream_id).is_some_and(|o| o.title == DEFAULT_THREAD_TITLE);
        !renamed && placeholder
    };
    if !rename {
        return;
    }
    if let Some(title) = clean_title(instruction) {
        let _ = append(
            state,
            stream_id,
            "node",
            THREAD_RENAMED_FAMILY,
            serde_json::to_string(&ThreadRenamed { title }).unwrap(),
            vec![],
        );
    }
}

// ─── HTTP ───────────────────────────────────────────────────────────────────

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct ListThreadsQuery {
    #[serde(default)]
    pub(crate) include_archived: bool,
}

/// `GET /baker/threads` — the op registry's `thread_list`, over HTTP. The
/// client's sidebar reads `/studio/state` instead (one topology source).
pub(crate) async fn threads_list(
    State(st): State<AppState>,
    Query(q): Query<ListThreadsQuery>,
) -> Result<Json<Vec<BakerThreadView>>, (StatusCode, String)> {
    crate::actor::spawn_blocking(move || list_threads(&st, q.include_archived))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))
}

/// `POST /baker/threads` — a human-created thread.
pub(crate) async fn thread_create(
    State(st): State<AppState>,
    Json(req): Json<CreateThreadReq>,
) -> Result<Json<CreateThreadResp>, (StatusCode, String)> {
    let detect = std::env::current_dir().ok();
    let env_root = crate::routes::baker::bakery_source_root_from_env();
    crate::actor::spawn_blocking(move || match req.stream_id.as_deref() {
        Some(creator) if !crate::baker_ops::is_baker_scope_stream(&st, creator) => {
            create_peer(&st, &req, creator, "human")
        }
        _ => create_thread(&st, &req, "human", env_root.as_deref(), detect.as_deref()),
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map(Json)
    .map_err(|e| (StatusCode::BAD_REQUEST, e))
}

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct RenameThreadReq {
    pub(crate) stream_id: String,
    pub(crate) title: String,
}

pub(crate) async fn thread_rename(
    State(st): State<AppState>,
    Json(req): Json<RenameThreadReq>,
) -> Result<Json<BakerThreadView>, (StatusCode, String)> {
    crate::actor::spawn_blocking(move || rename_thread(&st, &req.stream_id, &req.title, "human"))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))
}

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct ArchiveThreadReq {
    pub(crate) stream_id: String,
    pub(crate) archived: bool,
}

pub(crate) async fn thread_archive(
    State(st): State<AppState>,
    Json(req): Json<ArchiveThreadReq>,
) -> Result<Json<BakerThreadView>, (StatusCode, String)> {
    crate::actor::spawn_blocking(move || {
        set_thread_archived(&st, &req.stream_id, req.archived, "human")
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map(Json)
    .map_err(|e| (StatusCode::BAD_REQUEST, e))
}

// ─── Reading and waiting for a thread's work ────────────────────────────────

/// The facts that end a turn.
pub(crate) const TURN_ENDS: [&str; 4] = [
    "agent.turn.done",
    "agent.turn.failed",
    "agent.turn.cancelled",
    "agent.turn.exhausted",
];
/// The longest one `thread_wait` call waits: a runtime may give up on a
/// longer tool call. The Baker calls again while the thread still works.
const MAX_WAIT_SECS: u64 = 50;

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct ThreadReadQuery {
    pub(crate) stream_id: String,
    /// How many of the latest messages to return (default 20, at most 100).
    #[serde(default)]
    pub(crate) limit: Option<usize>,
}

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct ThreadWaitReq {
    pub(crate) stream_id: String,
    /// The most seconds to wait (default and most 50).
    #[serde(default)]
    pub(crate) timeout_secs: Option<u64>,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct ThreadMessage {
    /// operator, baker or agent.
    pub(crate) from: String,
    pub(crate) text: String,
    pub(crate) at: String,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct Tokens {
    pub(crate) input: Option<u64>,
    pub(crate) output: Option<u64>,
    pub(crate) total: Option<u64>,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct Acceptance {
    pub(crate) criterion: String,
    /// passed or failed by its check's exit code; pending until the thread
    /// goes idle and running while its proof job runs; unverified when it has
    /// no check or its check could not finish (`detail` says why). Never
    /// silently passed.
    pub(crate) verdict: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) check: Option<String>,
    /// Its proof job: job_output reads all of its output.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) job_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) exit_code: Option<u32>,
    /// A failed check's last output, or why it went unverified.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) detail: Option<String>,
}

impl Acceptance {
    /// The verdict on `criterion` for the turn `turn_id` ended (`completed`
    /// or not), from its stream's proof and job-end facts.
    fn of(criterion: &Criterion, events: &[Event], stream_id: &str, turn_id: &str, completed: bool) -> Self {
        let mut out = Acceptance {
            criterion: criterion.text().to_string(),
            verdict: "unverified",
            check: criterion.check().map(str::to_string),
            job_id: None,
            exit_code: None,
            detail: None,
        };
        let Some(check) = criterion.check() else {
            return out;
        };
        let proof = events
            .iter()
            .rev()
            .filter(|e| e.family == THREAD_PROOF_FAMILY)
            .filter_map(|e| serde_json::from_str::<ThreadProof>(&e.payload).ok())
            .find(|p| p.turn_id == turn_id)
            .and_then(|p| p.proofs.into_iter().find(|p| p.criterion == out.criterion && p.check == check));
        let Some(proof) = proof else {
            if completed {
                out.verdict = "pending";
            } else {
                out.detail = Some("its turn did not complete".to_string());
            }
            return out;
        };
        let Some(job_id) = proof.job_id else {
            out.detail = proof.error;
            return out;
        };
        let ended = events
            .iter()
            .rev()
            .filter(|e| e.family == crate::jobs::JOB_ENDED)
            .filter_map(|e| serde_json::from_str::<serde_json::Value>(&e.payload).ok())
            .find(|p| p["job_id"] == job_id.as_str());
        match ended {
            Some(end) if end["stopped"] == true => out.detail = Some("its proof job was stopped".to_string()),
            Some(end) => {
                out.exit_code = end["exit_code"].as_u64().and_then(|c| u32::try_from(c).ok());
                if out.exit_code == Some(0) {
                    out.verdict = "passed";
                } else {
                    out.verdict = "failed";
                    out.detail = end["tail"].as_str().map(str::to_string);
                }
            }
            // This node knows the job but its end fact isn't among the events
            // read: it is running, or it ended a moment after they were read
            // and its fact is on its way. Only a job the node has forgotten
            // (a restart) ended unrecorded.
            None if crate::jobs::list_jobs(Some(stream_id)).iter().any(|j| j.id == job_id) => {
                out.verdict = "running";
            }
            None => out.detail = Some("its proof job ended unrecorded (the node restarted)".to_string()),
        }
        out.job_id = Some(job_id);
        out
    }
}

/// A finished turn, after deer-flow's subagent status contract: status,
/// stop reason, result or error, model, tokens and acceptance.
#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct TurnResult {
    /// completed, failed or cancelled.
    pub(crate) status: &'static str,
    /// turn_capped when its step limit ended it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) stop_reason: Option<&'static str>,
    pub(crate) turn_id: String,
    pub(crate) result: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) tokens: Option<Tokens>,
    pub(crate) acceptance: Vec<Acceptance>,
    pub(crate) finished_at: String,
}

/// One conversation's work, as another agent reading it sees it.
#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct ThreadRead {
    pub(crate) stream_id: String,
    pub(crate) title: String,
    /// running (a turn is under way or waiting to start), paused, or idle.
    pub(crate) state: &'static str,
    pub(crate) runtime: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) effort: Option<String>,
    pub(crate) mode: crate::agent_control::OperatingMode,
    pub(crate) worktree: String,
    pub(crate) branch: String,
    pub(crate) queued: usize,
    pub(crate) messages: Vec<ThreadMessage>,
    /// Its latest finished turn; `None` until one has finished.
    pub(crate) result: Option<TurnResult>,
}

fn bounded(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    format!("{}…", text.chars().take(max).collect::<String>())
}

/// Who a message came from: the operator, the Baker (any of its
/// conversations), or the agent that sent it, named by its thread.
fn who(actor: &str) -> String {
    match actor {
        "human" => "operator".to_string(),
        baker if baker.starts_with("agent:") && baker.ends_with(":baker") => "baker".to_string(),
        other => other.to_string(),
    }
}

/// Reads the conversation's own facts back: settings, messages, the last
/// finished turn as a status contract, and whether it is still working.
pub(crate) fn read_thread(state: &AppState, stream_id: &str, limit: usize) -> Result<ThreadRead, String> {
    let ctx = context_for(state, stream_id)?;
    let seat = crate::routes::agent::default_seat_for(state, &ctx);
    let agent_id = crate::node::agent_id(stream_id, seat);
    let title = match crate::baker_ops::is_baker_scope_stream(state, stream_id) {
        true => thread_view(state, stream_id)?.title,
        false => ctx.name.clone(),
    };
    let queued = crate::agent_control::pending_count(state, stream_id, seat);
    let paused = crate::agent_control::dispatch_is_paused(state, stream_id, &agent_id);
    let mode = crate::agent_control::operating_mode_for(state, stream_id, &agent_id);
    let live = crate::pipe::acp_runtime_snapshot(state, stream_id, seat);
    let current = |category: &str| {
        live.as_ref()?
            .config_options
            .iter()
            .find(|o| o.category.as_deref() == Some(category))?
            .current_value
            .as_str()
            .map(str::to_string)
    };
    let heart = state.heart.lock().unwrap();
    let opened = opened_record(&heart, stream_id);
    let runtime = crate::state::latest_seat_model_id(&heart, stream_id, seat).ok().flatten();
    let chosen = crate::selection::chosen_locked(&heart, stream_id, seat, runtime.as_deref().unwrap_or_default());
    let events = heart.read_stream(stream_id).map_err(|e| e.to_string())?;
    drop(heart);

    let payload = |e: &Event| serde_json::from_str::<serde_json::Value>(&e.payload).unwrap_or_default();
    let ours: Vec<(&Event, serde_json::Value)> = events
        .iter()
        .map(|e| (e, payload(e)))
        .filter(|(_, p)| p["agent_id"] == agent_id.as_str())
        .collect();
    let mut messages: Vec<ThreadMessage> = ours
        .iter()
        .filter_map(|(e, p)| {
            let text = |key: &str| p[key].as_str().unwrap_or_default().to_string();
            let (from, text) = match e.family.as_str() {
                "agent.turn.started" | "agent.steer.applied" => (who(&e.actor), text("instruction")),
                "agent.turn.done" => ("agent".to_string(), text("summary")),
                "agent.turn.failed" => ("agent".to_string(), format!("(failed) {}", text("reason"))),
                "agent.turn.cancelled" => ("agent".to_string(), format!("(stopped) {}", text("reason"))),
                "agent.turn.exhausted" => ("agent".to_string(), format!("(step limit) {}", text("reason"))),
                _ => return None,
            };
            Some(ThreadMessage { from, text: bounded(&text, 2000), at: e.ts.clone() })
        })
        .collect();
    messages.drain(..messages.len().saturating_sub(limit.clamp(1, 100)));

    let last_started = ours.iter().rev().find(|(e, _)| e.family == "agent.turn.started");
    let last_end = ours.iter().rev().find(|(e, _)| TURN_ENDS.contains(&e.family.as_str()));
    let running = last_started.is_some_and(|(_, started)| {
        !ours.iter().any(|(e, p)| TURN_ENDS.contains(&e.family.as_str()) && p["turn_id"] == started["turn_id"])
    });
    let acceptance = opened.as_ref().map(|o| o.acceptance.clone()).unwrap_or_default();
    let result = last_end.map(|(end, p)| {
        let turn_id = p["turn_id"].as_str().unwrap_or_default().to_string();
        let reason = p["reason"].as_str().map(str::to_string);
        let (status, stop_reason, error) = match end.family.as_str() {
            "agent.turn.done" => ("completed", None, None),
            "agent.turn.cancelled" => ("cancelled", None, reason),
            "agent.turn.exhausted" => ("failed", Some("turn_capped"), reason),
            _ => ("failed", None, reason),
        };
        let usage = ours
            .iter()
            .rev()
            .find(|(e, u)| e.family == "agent.turn.usage" && u["turn_id"] == turn_id.as_str())
            .map(|(_, u)| u);
        let count = |u: &serde_json::Value, key: &str| u[key].as_u64();
        TurnResult {
            status,
            stop_reason,
            result: bounded(p["summary"].as_str().unwrap_or_default(), 8000),
            error,
            model: usage
                .and_then(|u| u["model_id"].as_str())
                .or_else(|| p["model_id"].as_str())
                .map(str::to_string),
            tokens: usage.map(|u| Tokens {
                input: count(u, "tokens_in"),
                output: count(u, "tokens_out"),
                total: count(u, "tokens_in").zip(count(u, "tokens_out")).map(|(i, o)| i + o),
            }),
            acceptance: acceptance
                .iter()
                .map(|criterion| Acceptance::of(criterion, &events, stream_id, &turn_id, status == "completed"))
                .collect(),
            finished_at: end.ts.clone(),
            turn_id,
        }
    });
    Ok(ThreadRead {
        stream_id: stream_id.to_string(),
        title,
        state: match (running, queued > 0, paused) {
            (true, _, _) | (false, true, false) => "running",
            (false, true, true) => "paused",
            (false, false, _) => "idle",
        },
        runtime,
        model: current("model").or(chosen.model),
        effort: current("thought_level").or(chosen.effort),
        mode,
        worktree: ctx.worktree_path,
        branch: ctx.branch,
        queued,
        messages,
        result,
    })
}

pub(crate) async fn thread_read_route(
    State(st): State<AppState>,
    Query(q): Query<ThreadReadQuery>,
) -> Result<Json<ThreadRead>, (StatusCode, String)> {
    crate::actor::spawn_blocking(move || read_thread(&st, &q.stream_id, q.limit.unwrap_or(20)))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct ThreadWaited {
    /// False when the wait ended with the thread still working: call
    /// thread_wait again.
    pub(crate) done: bool,
    #[serde(flatten)]
    pub(crate) thread: ThreadRead,
}

/// Runs a Baker thread's checks once a batch of its work is done: when it is
/// idle (no unfinished turn, nothing queued) after a completed turn that has
/// no proof yet. Each check is a quiet proof job; one `baker.thread.proof`
/// fact records them against that turn. Called as its queue drains
/// (`agent_control::schedule_dispatch`), and by [`wait_thread`] should a node
/// restart have come in between.
pub(crate) fn prove(state: &AppState, stream_id: &str) -> Result<(), String> {
    // ponytail: one lock for every thread, so two callers never prove one
    // batch twice; per-thread locks if proofs ever queue behind each other.
    static PROVING: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let checks: Vec<(String, String)> = {
        let heart = state.heart.lock().unwrap();
        let Some(opened) = opened_record(&heart, stream_id) else {
            return Ok(());
        };
        opened
            .acceptance
            .iter()
            .filter_map(|c| Some((c.text().to_string(), c.check()?.to_string())))
            .collect()
    };
    if checks.is_empty() {
        return Ok(());
    }
    let _one = PROVING.lock().unwrap_or_else(|e| e.into_inner());
    let read = read_thread(state, stream_id, 1)?;
    let idle = read.state == "idle";
    let Some(turn) = read.result.filter(|r| idle && r.acceptance.iter().any(|a| a.verdict == "pending")) else {
        return Ok(());
    };
    let proofs: Vec<Proof> = checks
        .into_iter()
        .map(|(criterion, check)| {
            let req = crate::jobs::JobStartReq { stream_id: stream_id.to_string(), command: check.clone(), cwd: None };
            let state = state.clone();
            // A job's watcher needs a runtime, and a dispatcher thread has none.
            match crate::api_tools::block_on(async move { crate::jobs::start_proof(&state, &req) }).and_then(|r| r) {
                Ok(job) => Proof { criterion, check, job_id: Some(job.id), error: None },
                Err(e) => Proof { criterion, check, job_id: None, error: Some(e) },
            }
        })
        .collect();
    let proof = ThreadProof { turn_id: turn.turn_id, proofs };
    append(state, stream_id, "node", THREAD_PROOF_FAMILY, serde_json::to_string(&proof).unwrap(), vec![])?;
    Ok(())
}

/// Waits until the conversation has nothing left to run (or its queue is
/// paused) and its acceptance checks are done, or the timeout passes, then
/// reads it. Woken by its own turn ends, proofs and job ends, never by
/// polling.
pub(crate) async fn wait_thread(state: AppState, stream_id: String, timeout: std::time::Duration) -> Result<ThreadWaited, String> {
    // Subscribed before the first look, so no turn end slips in between.
    let mut events = state.tx.subscribe();
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let read = {
            let (state, stream_id) = (state.clone(), stream_id.clone());
            crate::actor::spawn_blocking(move || read_thread(&state, &stream_id, 20))
                .await
                .map_err(|e| e.to_string())??
        };
        let proving = read
            .result
            .as_ref()
            .is_some_and(|r| r.acceptance.iter().any(|a| matches!(a.verdict, "pending" | "running")));
        if read.state == "paused" || (read.state == "idle" && !proving) {
            return Ok(ThreadWaited { done: true, thread: read });
        }
        if read.state == "idle" {
            // Done but not proved yet: its dispatcher is starting the checks
            // now, unless a node restart came in between. Either way, once.
            let (state, stream_id) = (state.clone(), stream_id.clone());
            crate::actor::spawn_blocking(move || prove(&state, &stream_id))
                .await
                .map_err(|e| e.to_string())??;
        }
        loop {
            match tokio::time::timeout_at(deadline, events.recv()).await {
                Err(_) => return Ok(ThreadWaited { done: false, thread: read }),
                Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => {
                    return Err("the node is shutting down".to_string())
                }
                // Missed some events: look again.
                Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => break,
                Ok(Ok(raw)) => {
                    let ev: serde_json::Value = serde_json::from_str(&raw).unwrap_or_default();
                    if ev["stream_id"] == stream_id.as_str()
                        && ev["family"].as_str().is_some_and(|f| {
                            TURN_ENDS.contains(&f) || f == THREAD_PROOF_FAMILY || f == crate::jobs::JOB_ENDED
                        })
                    {
                        break;
                    }
                }
            }
        }
    }
}

pub(crate) async fn thread_wait_route(
    State(st): State<AppState>,
    Json(req): Json<ThreadWaitReq>,
) -> Result<Json<ThreadWaited>, (StatusCode, String)> {
    let secs = req.timeout_secs.unwrap_or(MAX_WAIT_SECS).min(MAX_WAIT_SECS);
    wait_thread(st, req.stream_id, std::time::Duration::from_secs(secs))
        .await
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;
    use bakery_agent::StubModel;
    use std::sync::Arc;

    fn baker(state: &AppState) -> String {
        crate::routes::baker::ensure_permanent_baker(state, None, None)
            .expect("bootstrap")
            .stream_id
    }

    fn new_thread(state: &AppState, title: Option<&str>) -> BakerThreadView {
        create_thread(
            state,
            &CreateThreadReq {
                title: title.map(str::to_string),
                instruction: None,
                ..Default::default()
            },
            "human",
            None,
            None,
        )
        .expect("create thread")
        .thread
    }

    fn onboard_bare(state: &AppState, sid: &str) {
        append(
            state,
            sid,
            "human",
            "project.onboarded",
            serde_json::json!({"worktree_path": "", "repo_root": "", "name": "x", "project": "x"})
                .to_string(),
            vec![],
        )
        .unwrap();
    }

    #[test]
    fn a_created_thread_is_a_member_of_the_permanent_baker_and_lists_after_it() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let baker_sid = baker(&state);

        let t = new_thread(&state, Some("Refactor the queue"));
        assert!(is_baker_thread_stream(&state, &t.stream_id));
        assert!(crate::baker_ops::is_baker_scope_stream(
            &state,
            &t.stream_id
        ));
        assert!(!crate::baker_ops::is_permanent_baker_stream(
            &state,
            &t.stream_id
        ));
        assert_eq!(t.title, "Refactor the queue");
        assert_eq!(t.agent_id, format!("{}:baker", t.stream_id));
        assert!(!t.main && !t.archived);

        let listed = list_threads(&state, false).unwrap();
        assert_eq!(listed[0].stream_id, baker_sid, "main conversation first");
        assert!(listed[0].main);
        assert!(listed.iter().any(|v| v.stream_id == t.stream_id));
        let heart = state.heart.lock().unwrap();
        assert_eq!(
            crate::state::socket_kind(&heart, &t.stream_id),
            SocketKind::Factory
        );
    }

    #[test]
    fn a_thread_shares_the_permanent_bakers_exact_scope_and_is_never_bakery_owned() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("bakery"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        state.node.lock().unwrap().bakery_source_root = Some(repo.to_string_lossy().into_owned());
        let baker_sid = baker(&state);

        let t = new_thread(&state, None);
        let b = context_for(&state, &baker_sid).unwrap();
        let c = context_for(&state, &t.stream_id).unwrap();
        assert!(!b.worktree_path.is_empty(), "fixture Baker must be rooted");
        assert_eq!(c.worktree_path, b.worktree_path);
        assert_eq!(c.repo_root, b.repo_root);
        assert!(
            c.in_place,
            "bound in place on the factory root, like the Baker"
        );
        assert!(
            !c.created_by_bakery,
            "discard must never be able to remove the factory checkout"
        );
        assert!(
            !c.name.starts_with("baker-"),
            "must not read as a legacy branch job"
        );
    }

    #[test]
    fn an_owner_fact_naming_another_baker_or_a_later_reowning_append_grants_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let baker_sid = baker(&state);
        let real = new_thread(&state, Some("mine")).stream_id;

        let impostor = crate::node::mint_id("socket");
        onboard_bare(&state, &impostor);
        append(
            &state,
            &impostor,
            "human",
            SOCKET_KIND_FAMILY,
            serde_json::to_string(&SocketKindRecord {
                kind: SocketKind::Factory,
            })
            .unwrap(),
            vec![],
        )
        .unwrap();
        for owner in ["socket_someone_else", baker_sid.as_str()] {
            // The FIRST owner fact names a different Baker; the later,
            // correct-looking one must never re-own it.
            append(
                &state,
                &impostor,
                "human",
                THREAD_OPENED_FAMILY,
                serde_json::json!({"baker_stream_id": owner, "scope": "factory", "title": "x"})
                    .to_string(),
                vec![],
            )
            .unwrap();
        }
        assert!(!is_baker_thread_stream(&state, &impostor));
        assert!(!crate::baker_ops::is_baker_scope_stream(&state, &impostor));
        // Just an agent's tools, never the Baker's.
        assert_eq!(crate::mcp::mcp_scope_for_stream(&state, &impostor), Some(crate::mcp::McpScope::Agent));

        // A re-minted Baker pointer orphans existing threads — unprivileged,
        // never silently re-owned by the new Baker.
        state.node.lock().unwrap().baker_stream_id = Some("socket_reminted".into());
        assert!(!is_baker_thread_stream(&state, &real));
        // An empty pointer matches nothing.
        state.node.lock().unwrap().baker_stream_id = Some(String::new());
        assert!(!is_baker_thread_stream(&state, &real));
    }

    #[test]
    fn an_unknown_scope_or_a_non_factory_kind_is_not_a_thread() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let baker_sid = baker(&state);

        // Owner fact present but no Factory kind recorded: a project socket.
        let sid = crate::node::mint_id("socket");
        onboard_bare(&state, &sid);
        append(
            &state,
            &sid,
            "human",
            THREAD_OPENED_FAMILY,
            serde_json::json!({"baker_stream_id": baker_sid, "scope": "factory", "title": "x"})
                .to_string(),
            vec![],
        )
        .unwrap();
        assert!(!is_baker_thread_stream(&state, &sid));

        // A scope this build does not know never passes.
        let scoped = crate::node::mint_id("socket");
        onboard_bare(&state, &scoped);
        append(
            &state,
            &scoped,
            "human",
            SOCKET_KIND_FAMILY,
            serde_json::to_string(&SocketKindRecord {
                kind: SocketKind::Factory,
            })
            .unwrap(),
            vec![],
        )
        .unwrap();
        append(
            &state,
            &scoped,
            "human",
            THREAD_OPENED_FAMILY,
            serde_json::json!({"baker_stream_id": baker_sid, "scope": "project", "title": "x"})
                .to_string(),
            vec![],
        )
        .unwrap();
        assert!(!is_baker_thread_stream(&state, &scoped));

        // A real thread re-kinded to project loses membership.
        let t = new_thread(&state, None).stream_id;
        append(
            &state,
            &t,
            "human",
            SOCKET_KIND_FAMILY,
            serde_json::to_string(&SocketKindRecord {
                kind: SocketKind::Project,
            })
            .unwrap(),
            vec![],
        )
        .unwrap();
        assert!(!is_baker_thread_stream(&state, &t));
    }

    #[test]
    fn rename_archive_and_restore_round_trip_and_the_main_conversation_is_protected() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let baker_sid = baker(&state);
        let t = new_thread(&state, None).stream_id;

        let renamed =
            rename_thread(&state, &t, "  Ship the sidebar \nsecond line", "human").unwrap();
        assert_eq!(renamed.title, "Ship the sidebar");
        assert!(rename_thread(&state, &t, "   ", "human").is_err());

        assert!(
            set_thread_archived(&state, &t, true, "human")
                .unwrap()
                .archived
        );
        assert!(!list_threads(&state, false)
            .unwrap()
            .iter()
            .any(|v| v.stream_id == t));
        assert!(list_threads(&state, true)
            .unwrap()
            .iter()
            .any(|v| v.stream_id == t && v.archived));
        // Archive hides; it never costs membership, so restore is lossless.
        assert!(is_baker_thread_stream(&state, &t));
        assert!(
            !set_thread_archived(&state, &t, false, "human")
                .unwrap()
                .archived
        );

        let err = set_thread_archived(&state, &baker_sid, true, "human").unwrap_err();
        assert!(err.contains("main Baker"), "{err}");
        assert!(rename_thread(&state, &baker_sid, "x", "human").is_err());
        let err = set_thread_archived(&state, "socket_not_a_thread", true, "human").unwrap_err();
        assert!(err.contains("not a Baker thread"), "{err}");
    }

    #[test]
    fn archiving_a_busy_thread_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        baker(&state);
        let t = new_thread(&state, None).stream_id;
        state
            .lifecycle
            .lock()
            .unwrap()
            .running_turns
            .insert((t.clone(), Seat::Baker), 1);
        let err = set_thread_archived(&state, &t, true, "human").unwrap_err();
        assert!(err.contains("stop it"), "{err}");
    }

    #[test]
    fn a_new_thread_inherits_the_bakers_model_and_acknowledged_native_preferences() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let baker_sid = baker(&state);
        crate::routes::seats::seat_assign_checked(&state, &baker_sid, Seat::Baker, "stub").unwrap();
        append(
            &state,
            &baker_sid,
            "human",
            crate::routes::agent::NATIVE_CONFIG_ACKNOWLEDGED,
            serde_json::json!({
                "agent_id": format!("{baker_sid}:baker"), "seat": "baker",
                "account_id": "claude-native", "generation": 3, "config_id": "model",
                "requested": "sonnet", "effective": "sonnet",
            })
            .to_string(),
            vec![],
        )
        .unwrap();

        let t = new_thread(&state, None);
        assert_eq!(t.model_id.as_deref(), Some("stub"));
        let copied = state
            .heart
            .lock()
            .unwrap()
            .events_of_family(
                &t.stream_id,
                crate::routes::agent::NATIVE_CONFIG_ACKNOWLEDGED,
            )
            .unwrap();
        assert_eq!(copied.len(), 1);
        let p: serde_json::Value = serde_json::from_str(&copied[0].payload).unwrap();
        assert_eq!(p["agent_id"], format!("{}:baker", t.stream_id));
        assert_eq!(p["effective"], "sonnet");
        assert_eq!(p["inherited_from"], format!("{baker_sid}:baker"));
    }

    #[test]
    fn a_first_instruction_is_queued_on_the_new_thread_attributed_to_its_actor_and_titles_it() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        baker(&state);
        let resp = create_thread(
            &state,
            &CreateThreadReq {
                title: None,
                instruction: Some("Audit the MCP scopes\nthen report".into()),
                ..Default::default()
            },
            "agent:baker",
            None,
            None,
        )
        .unwrap();
        assert_eq!(resp.thread.title, "Audit the MCP scopes");
        let queued = resp.queued.expect("queued first message");
        assert_eq!(queued.family, crate::agent_control::FOLLOW_UP_QUEUED);
        assert_eq!(queued.actor, "agent:baker");
        let p: serde_json::Value = serde_json::from_str(&queued.payload).unwrap();
        assert_eq!(p["seat"], "baker");
        assert_eq!(p["actor"], "agent:baker");
    }

    #[test]
    fn a_placeholder_title_is_replaced_once_by_the_first_message_only() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        baker(&state);
        let t = new_thread(&state, None).stream_id;
        maybe_autotitle(&state, &t, "\n  Wire the approval badges  \nmore");
        maybe_autotitle(&state, &t, "second message must not retitle");
        assert_eq!(
            thread_view(&state, &t).unwrap().title,
            "Wire the approval badges"
        );

        let named = new_thread(&state, Some("Named")).stream_id;
        maybe_autotitle(&state, &named, "ignored");
        assert_eq!(thread_view(&state, &named).unwrap().title, "Named");
    }

    #[test]
    fn messaging_refuses_archived_threads_and_non_baker_streams() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        baker(&state);
        let t = new_thread(&state, None).stream_id;
        set_thread_archived(&state, &t, true, "human").unwrap();
        assert!(message_thread(&state, &t, "hi", "agent:baker")
            .unwrap_err()
            .contains("archived"));
        let project_socket = crate::catalog::create_project(&state, "P", None)
            .unwrap()
            .1
            .stream_id;
        assert!(message_thread(&state, &project_socket, "hi", "agent:baker")
            .unwrap_err()
            .contains("not a Baker conversation"));
    }

    #[test]
    fn the_open_thread_limit_is_enforced_and_archiving_frees_a_slot() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        baker(&state);
        let mut first = None;
        for i in 0..MAX_OPEN_THREADS {
            let t = new_thread(&state, Some(&format!("t{i}")));
            first.get_or_insert(t.stream_id);
        }
        let err =
            create_thread(&state, &CreateThreadReq::default(), "human", None, None).unwrap_err();
        assert!(err.contains("limit"), "{err}");
        set_thread_archived(&state, first.as_deref().unwrap(), true, "human").unwrap();
        assert!(create_thread(&state, &CreateThreadReq::default(), "human", None, None).is_ok());
    }

    fn baker_caller(stream_id: &str, mode: crate::agent_control::OperatingMode) -> crate::api_tools::Caller {
        crate::api_tools::Caller {
            stream_id: stream_id.to_string(),
            seat: Seat::Baker,
            turn_id: String::new(),
            mode,
            wait: None,
        }
    }

    fn call(state: &AppState, caller: &crate::api_tools::Caller, tool: &str, args: serde_json::Value) -> serde_json::Value {
        let out = crate::api_tools::call(state, caller, tool, &args).unwrap_or_else(|e| panic!("{tool}: {e}"));
        serde_json::from_str(&out).unwrap()
    }

    /// Step 1's done-criterion: a worker thread opened on model X, effort Y
    /// and mode auto runs its first turn on exactly those, on a native
    /// runtime, and thread_wait hands back its result.
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn a_worker_thread_runs_its_first_turn_as_opened_and_thread_wait_returns_its_result() {
        use crate::agent_control::OperatingMode;
        let dir = tempfile::tempdir().unwrap();
        let repo = bakery_repo_for_test(dir.path());
        let state = state_with_bakery_root(dir.path(), &repo);
        let home = baker(&state);
        // A native runtime that reports the settings each prompt runs with.
        state.node.lock().unwrap().acp_plugs = vec![crate::node::AcpPlugConfig {
            id: "acp:fake".into(),
            label: None,
            argv: vec![
                fake_acp_agent_binary().to_string_lossy().into_owned(),
                "--env".into(),
                "BAKERY_FAKE_ACP_CAPABILITIES=1".into(),
                "--env".into(),
                "BAKERY_FAKE_ACP_REPORT_CONFIG=1".into(),
            ],
            config: None,
            env: Default::default(),
            mcp_transport: crate::node::default_mcp_transport(),
        }];
        crate::state::assign_seat_model(&state, &home, Seat::Baker, "acp:fake").unwrap();
        let baker = baker_caller(&home, OperatingMode::Auto);
        // Opening a thread asks the operator even in Auto; they allow each.
        let open = |args| allowing(&state, &home, "thread_create", || crate::api_tools::call(&state, &baker, "thread_create", &args));

        // fake-sonnet would fall back to effort "high" on its own; "low"
        // proves the effort was applied, not defaulted.
        let created: serde_json::Value = serde_json::from_str(&open(serde_json::json!({
            "title": "Worker A",
            "instruction": "report your settings",
            "model": "fake-sonnet",
            "effort": "low",
            "mode": "auto",
            "acceptance": [
                "names its model",
                {"criterion": "runs in its workspace", "check": "test -d ."},
                {"criterion": "fails on purpose", "check": "echo nope; exit 3"},
            ],
        })).unwrap()).unwrap();
        let thread = created["thread"]["stream_id"].as_str().unwrap().to_string();

        // A model or effort the runtime does not offer is refused up front,
        // naming the choices.
        let bad = open(serde_json::json!({"model": "fake-gpt"})).unwrap_err();
        assert!(bad.contains("fake-gpt") && bad.contains("fake-sonnet"), "{bad}");
        // Effort levels are known for the runtime's current model only.
        let bad = open(serde_json::json!({"effort": "ultra"})).unwrap_err();
        assert!(bad.contains("ultra") && bad.contains("choose one of"), "{bad}");

        let waited = call(&state, &baker, "thread_wait", serde_json::json!({"stream_id": thread}));
        assert_eq!(waited["done"], true, "{waited}");
        assert_eq!(waited["state"], "idle", "{waited}");
        assert_eq!(waited["mode"], "auto", "{waited}");
        let result = &waited["result"];
        assert_eq!(result["status"], "completed", "{waited}");
        let reported: serde_json::Value = serde_json::from_str(result["result"].as_str().unwrap()).unwrap();
        assert_eq!(reported["model"], "fake-sonnet", "{reported}");
        assert_eq!(reported["effort"], "low", "{reported}");
        assert_eq!(result["acceptance"][0]["verdict"], "unverified", "{result}");
        // Its checks ran as proof jobs once it went idle, and thread_wait
        // held for them.
        assert_eq!(result["acceptance"][1]["verdict"], "passed", "{result}");
        assert_eq!(result["acceptance"][2]["verdict"], "failed", "{result}");
        assert_eq!(result["acceptance"][2]["exit_code"], 3, "{result}");
        assert!(result["acceptance"][2]["detail"].as_str().unwrap().contains("nope"), "{result}");
        // A proof job's end wakes nothing: no follow-up, no second turn.
        std::thread::sleep(std::time::Duration::from_millis(300));
        let events = state.heart.lock().unwrap().read_stream(&thread).unwrap();
        let count = |family: &str| events.iter().filter(|e| e.family == family).count();
        assert_eq!(count(THREAD_PROOF_FAMILY), 1, "one proof for one batch");
        assert_eq!((count("agent.follow_up.queued"), count("agent.turn.started")), (1, 1));

        let started = events.iter().find(|e| e.family == "agent.turn.started").unwrap();
        let started: serde_json::Value = serde_json::from_str(&started.payload).unwrap();
        assert_eq!(started["mode"], "auto", "the first turn ran in the mode it was opened with");
        let read = call(&state, &baker, "thread_read", serde_json::json!({"stream_id": thread}));
        assert_eq!(read["messages"][0]["from"], "baker", "{read}");
        assert_eq!(read["messages"][0]["text"], "report your settings", "{read}");
    }

    #[test]
    fn a_thread_can_work_in_its_own_worktree_and_inherits_the_callers_mode() {
        use crate::agent_control::{operating_mode_for, set_operating_mode, OperatingMode};
        let dir = tempfile::tempdir().unwrap();
        let repo = bakery_repo_for_test(dir.path());
        let state = state_with_bakery_root(dir.path(), &repo);
        let home = baker(&state);
        set_operating_mode(&state, &home, Seat::Baker, OperatingMode::Auto).unwrap();
        let baker = baker_caller(&home, OperatingMode::Auto);

        // Opening a thread asks the operator even in Auto; they allow it.
        let created = allowing(&state, &home, "thread_create", || {
            call(&state, &baker, "thread_create", serde_json::json!({"title": "Fix A", "worktree": true}))
        });
        let thread = created["thread"]["stream_id"].as_str().unwrap().to_string();
        let ctx = context_for(&state, &thread).unwrap();
        assert!(ctx.branch.starts_with("baker/fix-a-"), "{}", ctx.branch);
        assert!(ctx.created_by_bakery, "its worktree is Bakery's to remove");
        assert_eq!(
            crate::git::run(std::path::Path::new(&ctx.worktree_path), &["rev-parse", "HEAD"]).unwrap(),
            crate::git::run(&repo, &["rev-parse", "HEAD"]).unwrap(),
            "cut from the project's current HEAD"
        );
        assert_ne!(ctx.worktree_path, context_for(&state, &home).unwrap().worktree_path);
        let agent = crate::node::agent_id(&thread, Seat::Baker);
        assert_eq!(operating_mode_for(&state, &thread, &agent), OperatingMode::Auto, "unset, it takes the caller's mode");
    }

    #[test]
    fn commands_may_write_a_worktree_threads_checkout_but_never_the_bakers_source() {
        let dir = tempfile::tempdir().unwrap();
        let repo = bakery_repo_for_test(dir.path());
        let state = state_with_bakery_root(dir.path(), &repo);
        let home = baker(&state);
        let writes = |stream: &str| crate::tools::commands_write_workspace(&state, stream);
        let in_place = create_thread(&state, &CreateThreadReq::default(), "human", None, None)
            .unwrap()
            .thread
            .stream_id;
        let req = CreateThreadReq { title: Some("Own".into()), worktree: true, ..Default::default() };
        let own = create_thread(&state, &req, "human", None, None).unwrap().thread.stream_id;
        assert!(!writes(&home), "the main Baker's source stays read-only");
        assert!(!writes(&in_place), "a thread sharing that source in place too");
        assert!(writes(&own), "a thread in its own worktree may build and test");
        let project = crate::routes::project::open_stream(&state, &open_req(&make_repo(&dir.path().join("p")), "p"))
            .unwrap()
            .context
            .stream_id;
        assert!(writes(&project), "every other socket keeps its writable room");
    }

    /// Rule 1, strict (the operator's G-1b answer): any conversation in place
    /// on the Bakery's own repo only reads it, in every runtime's room; an
    /// agent's git changes there are refused, the operator's own go through,
    /// and a thread in its own worktree of that repo still writes.
    #[test]
    fn in_place_on_the_bakerys_repo_every_agent_only_reads() {
        let dir = tempfile::tempdir().unwrap();
        let repo = bakery_repo_for_test(dir.path());
        let state = state_with_bakery_root(dir.path(), &repo);
        let root = crate::catalog::DirectoryRootSpec::Attach(&repo.to_string_lossy());
        let in_place = crate::catalog::create_project(&state, "Bakery", Some(root)).unwrap().1.stream_id;
        let own = crate::routes::project::open_stream(&state, &open_req(&repo, "own")).unwrap().context.stream_id;
        assert!(!crate::tools::commands_write_workspace(&state, &in_place), "an ordinary thread in place only reads");
        assert!(crate::tools::commands_write_workspace(&state, &own), "its own worktree of the same repo writes");

        let stage = |stream: &str| {
            let req = crate::routes::git::StageReq { stream_id: stream.to_string(), paths: vec![], all: true, unstage: false };
            crate::routes::git::stage(&state, &req)
        };
        let refused = crate::actor::with("agent:orchestrator".into(), || stage(&in_place)).unwrap_err();
        assert!(refused.contains("only reads"), "{refused}");
        assert!(stage(&in_place).is_ok(), "the operator's own change goes through");
        assert!(crate::actor::with("agent:orchestrator".into(), || stage(&own)).is_ok());
    }

    /// The operator's answer: a worktree of the Bakery's repo a conversation
    /// adopted is its own, and its agents write it; only the main checkout
    /// (and the tree this node runs from) stays read. A thread adopts one
    /// only when the operator gives it, and never the main checkout.
    #[test]
    fn an_adopted_worktree_of_the_bakerys_repo_is_written_by_its_own_agents() {
        let dir = tempfile::tempdir().unwrap();
        let repo = bakery_repo_for_test(dir.path());
        let state = state_with_bakery_root(dir.path(), &repo);
        let side = dir.path().join("w1");
        run_git(&repo, &["worktree", "add", "-q", "-b", "w1/baker-hands", side.to_str().unwrap()]);
        let writes = |stream: &str| crate::tools::commands_write_workspace(&state, stream);

        let attached = crate::catalog::DirectoryRootSpec::Attach(side.to_str().unwrap());
        let project = crate::catalog::create_project(&state, "Bakery w1", Some(attached)).unwrap().1.stream_id;
        assert!(writes(&project), "a conversation in an adopted worktree writes it");
        let main = crate::catalog::DirectoryRootSpec::Attach(repo.to_str().unwrap());
        let in_place = crate::catalog::create_project(&state, "Bakery", Some(main)).unwrap().1.stream_id;
        assert!(!writes(&in_place), "the main checkout stays read");

        let adopt = |path: &std::path::Path, actor: &str| {
            let req = CreateThreadReq { title: Some("Worker 1".into()), adopt: Some(path.display().to_string()), ..Default::default() };
            create_thread(&state, &req, actor, None, None)
        };
        let chef = adopt(&side, "human").unwrap().thread.stream_id;
        assert!(writes(&chef), "a chef in its adopted worktree builds and tests there");
        let ctx = context_for(&state, &chef).unwrap();
        assert_eq!((ctx.branch.as_str(), ctx.created_by_bakery), ("w1/baker-hands", false), "its branch, and never removed on discard");
        assert!(adopt(&repo, "human").unwrap_err().contains("not a linked worktree"));
        assert!(adopt(&side, "agent:baker").unwrap_err().contains("only the operator"));
    }

    /// Rule 1 holds for an edit the policy would accept in Auto too: one to
    /// the Bakery's main checkout waits for the operator, and is never
    /// written; one in a conversation's own worktree goes through.
    #[test]
    fn an_auto_accepted_edit_never_writes_the_bakerys_main_checkout() {
        let dir = tempfile::tempdir().unwrap();
        let repo = bakery_repo_for_test(dir.path());
        let state = state_with_bakery_root(dir.path(), &repo);
        let main = crate::catalog::DirectoryRootSpec::Attach(repo.to_str().unwrap());
        let in_place = crate::catalog::create_project(&state, "Bakery", Some(main)).unwrap().1.stream_id;
        let own = crate::routes::project::open_stream(&state, &open_req(&repo, "own")).unwrap().context.stream_id;
        let propose = |stream: &str| {
            let agent_id = crate::node::agent_id(stream, Seat::Orchestrator);
            let p = serde_json::json!({"file_path": "VISION.md", "status": "pending", "rationale": "r", "old_content": "# The Bakery\n", "new_content": "rewritten\n", "agent_id": agent_id, "turn_id": "t", "call_id": "c"});
            let proposal = append(&state, stream, "agent:orchestrator", "agent.file.changed", p.to_string(), vec![]).unwrap();
            crate::routes::agent::maybe_auto_accept_for_mode(&state, stream, &proposal, true);
            state.heart.lock().unwrap().events_of_family(stream, "agent.edit.accepted").unwrap().len()
        };
        assert_eq!(propose(&in_place), 0, "it waits for the operator");
        assert_eq!(std::fs::read_to_string(repo.join("VISION.md")).unwrap(), "# The Bakery\n", "the main checkout as it was");
        assert_eq!(propose(&own), 1, "its own worktree is written");
        let own_dir = std::path::PathBuf::from(context_for(&state, &own).unwrap().worktree_path);
        assert_eq!(std::fs::read_to_string(own_dir.join("VISION.md")).unwrap(), "rewritten\n");
    }

    /// The Baker reviews, tests and merges a chef's work into main (the
    /// operator's answer): only the permanent Baker, only a thread's own
    /// branch, only into a clean main checkout, and a conflict merges
    /// nothing.
    #[test]
    fn the_baker_merges_a_chefs_branch_into_main() {
        let dir = tempfile::tempdir().unwrap();
        let repo = bakery_repo_for_test(dir.path());
        run_git(&repo, &["add", "."]);
        run_git(&repo, &["commit", "-qm", "the bakery"]);
        let state = state_with_bakery_root(dir.path(), &repo);
        let home = baker(&state);
        let merge = |caller: &str, thread: &str| {
            let args = serde_json::json!({"stream_id": thread});
            crate::baker_ops::dispatch_baker_op_as(&state, caller, "thread_merge", &args)
        };
        let chef_with = |title: &str, text: &str| {
            let req = CreateThreadReq { title: Some(title.into()), worktree: true, ..Default::default() };
            let chef = create_thread(&state, &req, "human", None, None).unwrap().thread.stream_id;
            let work = std::path::PathBuf::from(context_for(&state, &chef).unwrap().worktree_path);
            std::fs::write(work.join("sidebar.md"), text).unwrap();
            run_git(&work, &["add", "."]);
            run_git(&work, &["commit", "-qm", title]);
            chef
        };
        let chef = chef_with("Fix the sidebar", "fixed\n");
        let clash = chef_with("Clash", "another fix\n");

        assert!(merge(&chef, &chef).unwrap_err().contains("only the permanent Baker"));
        std::fs::write(repo.join("VISION.md"), "the operator's own edit\n").unwrap();
        assert!(merge(&home, &chef).unwrap_err().contains("changes of its own"));
        run_git(&repo, &["checkout", "--", "VISION.md"]);

        let merged: serde_json::Value = serde_json::from_str(&merge(&home, &chef).unwrap()).unwrap();
        assert_eq!(std::fs::read_to_string(repo.join("sidebar.md")).unwrap(), "fixed\n");
        let head = crate::git::run(&repo, &["rev-parse", "HEAD"]).unwrap();
        assert_eq!(merged["commit"].as_str(), Some(head.trim()));
        let parents = crate::git::run(&repo, &["rev-list", "--parents", "-n", "1", "HEAD"]).unwrap();
        assert_eq!(parents.split_whitespace().count(), 3, "a merge commit, never a fast-forward");
        for stream in [&chef, &home] {
            let facts = state.heart.lock().unwrap().events_of_family(stream, MERGED_FAMILY).unwrap();
            assert_eq!(facts.len(), 1, "on {stream}");
        }

        let in_place = create_thread(&state, &CreateThreadReq::default(), "human", None, None).unwrap().thread.stream_id;
        assert!(merge(&home, &in_place).unwrap_err().contains("no branch of its own"));

        let refused = merge(&home, &clash).unwrap_err();
        assert!(refused.contains("conflicts in sidebar.md") && refused.contains("nothing changed"), "{refused}");
        assert_eq!(crate::git::run(&repo, &["rev-parse", "HEAD"]).unwrap(), head, "main as it was");
    }

    /// A chef's committed work, said ready, is a card and a message in the
    /// Baker's conversation and an item in the waiting list; a second chef
    /// changing the same files is told it already waits, once; the merge
    /// closes it. Uncommitted work, and the Baker itself, are refused.
    #[test]
    fn a_chefs_ready_work_waits_for_the_baker_and_a_second_chef_is_told() {
        let dir = tempfile::tempdir().unwrap();
        let repo = bakery_repo_for_test(dir.path());
        run_git(&repo, &["add", "."]);
        run_git(&repo, &["commit", "-qm", "the bakery"]);
        let state = state_with_bakery_root(dir.path(), &repo);
        let home = baker(&state);
        let chef = |title: &str| {
            let req = CreateThreadReq { title: Some(title.into()), worktree: true, ..Default::default() };
            let stream = create_thread(&state, &req, "human", None, None).unwrap().thread.stream_id;
            let work = std::path::PathBuf::from(context_for(&state, &stream).unwrap().worktree_path);
            (stream, work)
        };
        let (a, a_work) = chef("Fix the sidebar");
        let (b, b_work) = chef("Sidebar colors");
        let ready = |caller: &str| {
            let args = serde_json::json!({"note": "tested with cargo test"});
            crate::baker_ops::dispatch_baker_op_as(&state, caller, "merge_ready", &args)
        };
        std::fs::write(a_work.join("sidebar.md"), "fixed\n").unwrap();
        assert!(ready(&a).unwrap_err().contains("commit your work first"));
        run_git(&a_work, &["add", "."]);
        run_git(&a_work, &["commit", "-qm", "fix the sidebar"]);
        assert!(ready(&home).unwrap_err().contains("Baker thread"));

        ready(&a).unwrap();
        let list = merge_waiting(&state);
        assert_eq!(list.count, 1);
        let item = &list.items[0];
        assert_eq!((item.stream_id.as_str(), item.files.clone(), item.adds), (a.as_str(), vec!["sidebar.md".to_string()], 1));
        assert_eq!((item.commits.len(), item.commits[0].subject.as_str()), (1, "fix the sidebar"));
        let cards = state.heart.lock().unwrap().events_of_family(&home, MERGE_WAITING_FAMILY).unwrap();
        assert_eq!(cards.len(), 1, "a card in the Baker's conversation");
        let queued = state.heart.lock().unwrap().events_of_family(&home, crate::agent_control::FOLLOW_UP_QUEUED).unwrap();
        let message: serde_json::Value = serde_json::from_str(&queued.last().unwrap().payload).unwrap();
        assert_eq!(message["message"]["from"].as_str(), Some(a.as_str()));
        assert!(message["instruction"].as_str().unwrap().contains("thread_merge"));

        std::fs::write(b_work.join("sidebar.md"), "blue\n").unwrap();
        let told = already_waiting_note(&state, &b).expect("told it already waits");
        assert!(told.contains("Already waiting to merge") && told.contains("sidebar.md"), "{told}");
        already_waiting_note(&state, &b).unwrap();
        let overlaps = state.heart.lock().unwrap().events_of_family(&b, MERGE_OVERLAP_FAMILY).unwrap();
        assert_eq!(overlaps.len(), 1, "noted once");
        assert!(already_waiting_note(&state, &a).is_none(), "its own work is not another's");

        let args = serde_json::json!({"stream_id": a});
        crate::baker_ops::dispatch_baker_op_as(&state, &home, "thread_merge", &args).unwrap();
        assert_eq!(merge_waiting(&state).count, 0, "merged: no longer waiting");

        // Merged some other way, the item leaves the list too.
        std::fs::remove_file(b_work.join("sidebar.md")).unwrap();
        std::fs::write(b_work.join("colors.md"), "blue\n").unwrap();
        run_git(&b_work, &["add", "."]);
        run_git(&b_work, &["commit", "-qm", "colors"]);
        ready(&b).unwrap();
        assert_eq!(merge_waiting(&state).count, 1);
        let branch = context_for(&state, &b).unwrap().branch;
        run_git(&repo, &["merge", "-q", "--no-ff", "--no-edit", &branch]);
        assert_eq!(merge_waiting(&state).count, 0, "its head is in main already");
    }

    /// Whoever calls thread_create is the new thread's spawner, on the record
    /// from the start: the permanent Baker when the operator opens it from the
    /// app or the Baker itself does. A chef reports to its spawner, and to the
    /// permanent Baker when that one is archived, deleted, or was never
    /// recorded (a thread from before). Only the spawner and the permanent
    /// Baker may merge its work.
    #[test]
    fn a_thread_reports_to_the_conversation_that_opened_it_and_to_the_permanent_baker_otherwise() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let home = baker(&state);
        let open_as = |caller: &str, title: &str| -> String {
            let args = serde_json::json!({"title": title});
            let made = crate::baker_ops::dispatch_baker_op_as(&state, caller, "thread_create", &args).unwrap();
            let made: serde_json::Value = serde_json::from_str(&made).unwrap();
            made["thread"]["stream_id"].as_str().unwrap().to_string()
        };
        let spawner = |thread: &str| spawner_locked(&state.heart.lock().unwrap(), thread);

        let a = new_thread(&state, Some("A")).stream_id;
        let b = open_as(&a, "B");
        let c = open_as(&home, "C");
        let d = crate::actor::with(crate::actor::agent(&a, Seat::Baker), || {
            let req = CreateThreadReq { title: Some("D".into()), ..Default::default() };
            create_thread(&state, &req, "human", None, None)
        })
        .unwrap()
        .thread
        .stream_id;
        assert_eq!(spawner(&a).as_deref(), Some(home.as_str()), "the operator, from the app");
        assert_eq!(spawner(&b).as_deref(), Some(a.as_str()), "a thread's call");
        assert_eq!(spawner(&c).as_deref(), Some(home.as_str()), "the permanent Baker's call");
        assert_eq!(spawner(&d).as_deref(), Some(a.as_str()), "the route, called by a thread, names it too");
        assert_eq!(reports_to(&state, &b).unwrap(), a);
        assert_eq!(reports_to(&state, &a).unwrap(), home);

        assert!(may_merge(&state, &a, &b) && may_merge(&state, &home, &b), "its spawner, and the permanent Baker");
        assert!(!may_merge(&state, &c, &b) && !may_merge(&state, &b, &b) && !may_merge(&state, "", &b), "no one else");

        // Archived, it is not told; restored, it is again.
        set_thread_archived(&state, &a, true, "human").unwrap();
        assert_eq!(reports_to(&state, &b).unwrap(), home);
        set_thread_archived(&state, &a, false, "human").unwrap();
        assert_eq!(reports_to(&state, &b).unwrap(), a);

        // Deleted, likewise.
        let gone = new_thread(&state, Some("Gone")).stream_id;
        let child = open_as(&gone, "Child");
        assert_eq!(reports_to(&state, &child).unwrap(), gone);
        append(&state, &gone, "human", crate::node::DISCARD_FAMILY, "not json".to_string(), vec![]).unwrap();
        assert_eq!(reports_to(&state, &child).unwrap(), home);

        // A thread opened before spawners were recorded has none.
        let old = crate::node::mint_id("socket");
        onboard_bare(&state, &old);
        let kind = serde_json::to_string(&SocketKindRecord { kind: SocketKind::Factory }).unwrap();
        append(&state, &old, "human", SOCKET_KIND_FAMILY, kind, vec![]).unwrap();
        let opened = serde_json::json!({"baker_stream_id": home, "scope": "factory", "title": "Old"});
        append(&state, &old, "human", THREAD_OPENED_FAMILY, opened.to_string(), vec![]).unwrap();
        assert!(is_baker_thread_stream(&state, &old));
        assert_eq!((spawner(&old), reports_to(&state, &old).unwrap()), (None, home));
    }

    /// A chef's card and the review turn go to the conversation that opened
    /// it, which is named on the card, and to the chef's own thread; the
    /// permanent Baker gets neither, yet the waiting list still has it, until
    /// the spawner is archived: then the card and the turn are the permanent
    /// Baker's.
    #[test]
    fn a_chefs_card_and_review_go_to_its_spawner_and_the_permanent_baker_gets_neither() {
        let dir = tempfile::tempdir().unwrap();
        let repo = bakery_repo_for_test(dir.path());
        run_git(&repo, &["add", "."]);
        run_git(&repo, &["commit", "-qm", "the bakery"]);
        let state = state_with_bakery_root(dir.path(), &repo);
        let home = baker(&state);
        let req = CreateThreadReq {
            title: Some("Audit the sidebar".into()),
            model: Some("opus".into()),
            effort: Some("max".into()),
            ..Default::default()
        };
        let spawner = create_thread(&state, &req, "human", None, None).unwrap().thread.stream_id;
        let op = |caller: &str, name: &str, args: serde_json::Value| -> Result<serde_json::Value, String> {
            crate::baker_ops::dispatch_baker_op_as(&state, caller, name, &args).map(|out| serde_json::from_str(&out).unwrap())
        };
        let chef = |spawner: &str, title: &str| -> String {
            let made = op(spawner, "thread_create", serde_json::json!({"title": title, "worktree": true})).unwrap();
            let stream = made["thread"]["stream_id"].as_str().unwrap().to_string();
            let work = std::path::PathBuf::from(context_for(&state, &stream).unwrap().worktree_path);
            std::fs::write(work.join(format!("{title}.md")), "done\n").unwrap();
            run_git(&work, &["add", "."]);
            run_git(&work, &["commit", "-qm", title]);
            stream
        };
        let facts = |stream: &str, family: &str| state.heart.lock().unwrap().events_of_family(stream, family).unwrap();
        let queued = |stream: &str| facts(stream, crate::agent_control::FOLLOW_UP_QUEUED);

        let (sidebar, colors) = (chef(&spawner, "sidebar"), chef(&spawner, "colors"));
        let before = queued(&home).len();
        op(&sidebar, "merge_ready", serde_json::json!({"note": "tested with cargo test"})).unwrap();
        let cards = |stream: &str| facts(stream, MERGE_WAITING_FAMILY).len();
        assert_eq!((cards(&sidebar), cards(&spawner), cards(&home)), (1, 1, 0), "the chef's own, its spawner's, not the permanent Baker's");
        assert_eq!(queued(&home).len(), before, "the permanent Baker is told nothing");
        let turns = queued(&spawner);
        assert_eq!(turns.len(), 1);
        let turn: serde_json::Value = serde_json::from_str(&turns[0].payload).unwrap();
        let said = turn["instruction"].as_str().unwrap();
        for words in ["Review, test and audit it", "bring the operator the merge review and wait", "only when the operator has told you to for this case"] {
            assert!(said.contains(words), "{words}: {said}");
        }
        assert!(said.contains(&format!("thread_merge {{\"stream_id\": \"{sidebar}\"}}")), "{said}");
        assert_eq!(turn["message"]["from"].as_str(), Some(sidebar.as_str()));
        assert!(turn.get("model_id").is_none() && turn.get("effort").is_none(), "it carries none: {turn}");
        let selected = crate::selection::chosen(&state, &spawner, Seat::Baker);
        assert_eq!((selected.model.as_deref(), selected.effort.as_deref()), (Some("opus"), Some("max")), "it runs on its spawner's selector");

        // The list has it all the same, and says who it went to.
        let list = merge_waiting(&state);
        assert_eq!(list.count, 1);
        let item = &list.items[0];
        assert_eq!(item.stream_id, sidebar);
        assert_eq!((item.spawner.as_deref(), item.spawner_name.as_deref()), (Some(spawner.as_str()), Some("Audit the sidebar")));
        let own: MergeWaiting = serde_json::from_str(&facts(&sidebar, MERGE_WAITING_FAMILY)[0].payload).unwrap();
        assert_eq!(own.spawner.as_deref(), Some(spawner.as_str()), "the chef's copy points to it");

        // An archived spawner is not told: the permanent Baker is. (Archived by
        // its fact: its stub turn from the review above may still be running.)
        let archived = serde_json::to_string(&ThreadArchived { archived: true }).unwrap();
        append(&state, &spawner, "human", THREAD_ARCHIVED_FAMILY, archived, vec![]).unwrap();
        op(&colors, "merge_ready", serde_json::json!({})).unwrap();
        assert_eq!((cards(&colors), cards(&spawner), cards(&home)), (1, 1, 1));
        assert_eq!(queued(&spawner).len(), 1, "nothing more for the archived spawner");
        assert_eq!(queued(&home).len(), before + 1);
        let fallen = merge_waiting(&state).items.into_iter().find(|w| w.stream_id == colors).unwrap();
        assert_eq!(fallen.spawner.as_deref(), Some(home.as_str()));
        assert_eq!(merge_waiting(&state).count, 2);

        // A deleted chef waits no more.
        append(&state, &colors, "human", crate::node::DISCARD_FAMILY, "not json".to_string(), vec![]).unwrap();
        assert_eq!(merge_waiting(&state).items.iter().map(|w| w.stream_id.as_str()).collect::<Vec<_>>(), [sidebar.as_str()]);
    }

    /// The spawner merges its own chef's work, the permanent Baker any, and
    /// no one else. The merged notice goes to the chef and its spawner, not
    /// the permanent Baker, and the chef's thread is archived with its
    /// worktree and branch gone, what it had not committed in the Trash.
    #[test]
    fn the_spawner_merges_its_chefs_work_and_the_chef_is_archived_and_cleared_away() {
        let dir = tempfile::tempdir().unwrap();
        let repo = bakery_repo_for_test(dir.path());
        run_git(&repo, &["add", "."]);
        run_git(&repo, &["commit", "-qm", "the bakery"]);
        let state = state_with_bakery_root(dir.path(), &repo);
        let home = baker(&state);
        let (spawner, other) = (new_thread(&state, Some("Spawner")).stream_id, new_thread(&state, Some("Other")).stream_id);
        let op = |caller: &str, name: &str, args: serde_json::Value| -> Result<serde_json::Value, String> {
            crate::baker_ops::dispatch_baker_op_as(&state, caller, name, &args).map(|out| serde_json::from_str(&out).unwrap())
        };
        let made = op(&spawner, "thread_create", serde_json::json!({"title": "Sidebar", "worktree": true})).unwrap();
        let chef = made["thread"]["stream_id"].as_str().unwrap().to_string();
        let ctx = context_for(&state, &chef).unwrap();
        let work = std::path::PathBuf::from(&ctx.worktree_path);
        std::fs::write(work.join("sidebar.md"), "fixed\n").unwrap();
        run_git(&work, &["add", "."]);
        run_git(&work, &["commit", "-qm", "fix the sidebar"]);
        op(&chef, "merge_ready", serde_json::json!({"note": "tested"})).unwrap();
        // Something it did not commit since it asked: it does not merge, and is kept.
        std::fs::write(work.join("scratch.md"), "not committed\n").unwrap();

        let merge = |caller: &str| op(caller, "thread_merge", serde_json::json!({"stream_id": chef}));
        for caller in [other.as_str(), chef.as_str()] {
            let refused = merge(caller).unwrap_err();
            assert!(refused.contains("or the conversation that opened the thread"), "{refused}");
        }
        assert!(work.is_dir() && list_threads(&state, false).unwrap().iter().any(|t| t.stream_id == chef), "refused, so as it was");

        let merged = merge(&spawner).unwrap();
        assert_eq!(std::fs::read_to_string(repo.join("sidebar.md")).unwrap(), "fixed\n");
        assert_eq!(merged["commit"].as_str(), Some(crate::git::run(&repo, &["rev-parse", "HEAD"]).unwrap().trim()));
        assert!(merged["uncommitted"].as_str().unwrap().contains("scratch.md"), "{merged}");
        let facts = |stream: &str| state.heart.lock().unwrap().events_of_family(stream, MERGED_FAMILY).unwrap().len();
        assert_eq!((facts(&chef), facts(&spawner), facts(&home), facts(&other)), (1, 1, 0, 0), "told to the chef and its spawner only");
        assert_eq!(merge_waiting(&state).count, 0, "merged: no longer waiting");

        // Archived, its worktree and branch gone, what it held in the Trash.
        assert_eq!(
            merged["cleanup"],
            serde_json::json!({"archived": true, "worktree_removed": true, "branch_removed": true, "kept_in_trash": true})
        );
        assert!(state.heart.lock().unwrap().last_of_family(&chef, THREAD_ARCHIVED_FAMILY).unwrap().is_some());
        assert!(!list_threads(&state, false).unwrap().iter().any(|t| t.stream_id == chef));
        assert!(!work.exists(), "its worktree is removed");
        assert!(!crate::git::inspect(&repo.to_string_lossy()).local_branches.contains(&ctx.branch), "and its branch");
        let trash = crate::git::run(&repo, &["for-each-ref", "refs/bakery/trash", "--format=%(refname)"]).unwrap();
        let entry = format!("refs/bakery/trash/{}/", ctx.branch);
        assert!(trash.lines().any(|r| r.starts_with(&entry) && r.ends_with("/changes")), "{trash}");
        assert!(crate::git::run(&repo, &["show", "HEAD:sidebar.md"]).is_ok(), "its work is in main, not only in the Trash");
    }

    /// The cleanup after a merge stops where it must, and needs no git to
    /// say so: a thread with a turn running is not archived, and a folder the
    /// Bakery did not make is left, only its thread archived.
    #[test]
    fn the_cleanup_after_a_merge_leaves_a_busy_thread_and_a_folder_the_bakery_did_not_make() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        baker(&state);
        let thread = new_thread(&state, Some("Done")).stream_id;
        let ctx = context_for(&state, &thread).unwrap();
        state.lifecycle.lock().unwrap().running_turns.insert((thread.clone(), Seat::Baker), 1);
        let busy = clean_up_chef(&state, &thread, &ctx, "human");
        assert!(!busy.archived && busy.errors.len() == 1 && busy.errors[0].contains("turn running"), "{busy:?}");
        assert!(list_threads(&state, false).unwrap().iter().any(|t| t.stream_id == thread), "still listed");

        state.lifecycle.lock().unwrap().running_turns.clear();
        let idle = clean_up_chef(&state, &thread, &ctx, "human");
        assert_eq!(idle, MergeCleanup { archived: true, ..Default::default() }, "its folder is not the Bakery's to remove");
        assert!(!list_threads(&state, false).unwrap().iter().any(|t| t.stream_id == thread));
    }

    /// The waiting list reads each chef's own thread, so it has every chef
    /// whoever got the card, though the permanent Baker holds none; a merge
    /// closes it from there, and a deleted chef is not on it.
    #[test]
    fn the_waiting_list_has_every_chef_whoever_got_the_card() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let home = baker(&state);
        let (spawner, chef) = (new_thread(&state, Some("Spawner")).stream_id, new_thread(&state, Some("Chef")).stream_id);
        let card = serde_json::json!({
            "stream_id": chef, "name": "Chef", "branch": "baker/chef", "base": "main", "into_stream_id": home,
            "spawner": spawner, "spawner_name": "Spawner", "commits": [{"sha": "abc1234", "subject": "work"}],
            "files": ["a.md"], "adds": 1, "dels": 0, "asked_at": "2026-10-15T09:00:00Z",
        })
        .to_string();
        // Told to the chef and its spawner, as ready_to_merge does.
        for stream in [&chef, &spawner] {
            append(&state, stream, "agent:chef:baker", MERGE_WAITING_FAMILY, card.clone(), vec![]).unwrap();
        }
        assert!(state.heart.lock().unwrap().events_of_family(&home, MERGE_WAITING_FAMILY).unwrap().is_empty());
        let list = merge_waiting(&state);
        assert_eq!(list.count, 1);
        assert_eq!((list.items[0].stream_id.as_str(), list.items[0].spawner.as_deref()), (chef.as_str(), Some(spawner.as_str())));

        // Its merge, told to the chef alone here, closes it.
        let done = serde_json::json!({"thread": chef, "branch": "baker/chef", "into": "main", "commit": "def5678"});
        append(&state, &chef, "human", MERGED_FAMILY, done.to_string(), vec![]).unwrap();
        assert_eq!(merge_waiting(&state).count, 0);

        // Asked again, then deleted: it waits no more.
        append(&state, &chef, "agent:chef:baker", MERGE_WAITING_FAMILY, card, vec![]).unwrap();
        assert_eq!(merge_waiting(&state).count, 1);
        append(&state, &chef, "human", crate::node::DISCARD_FAMILY, "not json".to_string(), vec![]).unwrap();
        assert_eq!(merge_waiting(&state).count, 0);
    }

    /// The operator's own merge does the same, but a chef with a turn running
    /// stays as it is (only the merge is done), and a worktree the operator
    /// gave a thread, with its branch, stays: only its thread is archived.
    #[test]
    fn a_chef_with_a_turn_running_or_the_operators_own_worktree_is_not_cleared_away() {
        let dir = tempfile::tempdir().unwrap();
        let repo = bakery_repo_for_test(dir.path());
        run_git(&repo, &["add", "."]);
        run_git(&repo, &["commit", "-qm", "the bakery"]);
        let state = state_with_bakery_root(dir.path(), &repo);
        let home = baker(&state);
        let side = dir.path().join("w1");
        run_git(&repo, &["worktree", "add", "-q", "-b", "w1/baker-hands", side.to_str().unwrap()]);
        let commit = |work: &std::path::Path, file: &str| {
            std::fs::write(work.join(file), "done\n").unwrap();
            run_git(work, &["add", "."]);
            run_git(work, &["commit", "-qm", file]);
        };
        let ready = |chef: &str| {
            crate::baker_ops::dispatch_baker_op_as(&state, chef, "merge_ready", &serde_json::json!({})).unwrap();
        };
        let branches = || crate::git::inspect(&repo.to_string_lossy()).local_branches;

        let req = CreateThreadReq { title: Some("Busy".into()), worktree: true, ..Default::default() };
        let busy = create_thread(&state, &req, "human", None, None).unwrap().thread.stream_id;
        let ctx = context_for(&state, &busy).unwrap();
        commit(std::path::Path::new(&ctx.worktree_path), "busy.md");
        ready(&busy);
        state.lifecycle.lock().unwrap().running_turns.insert((busy.clone(), Seat::Baker), 1);
        let merged = merge_thread(&state, &busy, None, "human").unwrap();
        let cleanup = merged.cleanup.unwrap();
        assert!(!cleanup.archived && !cleanup.worktree_removed && !cleanup.branch_removed, "{cleanup:?}");
        assert!(cleanup.errors[0].contains("turn running"), "{cleanup:?}");
        assert!(std::path::Path::new(&ctx.worktree_path).is_dir() && branches().contains(&ctx.branch), "it stays");
        assert!(repo.join("busy.md").exists(), "the merge itself is done");
        assert_eq!(state.heart.lock().unwrap().events_of_family(&home, MERGED_FAMILY).unwrap().len(), 1, "told to the permanent Baker, its spawner");

        let req = CreateThreadReq { title: Some("Worker 1".into()), adopt: Some(side.display().to_string()), ..Default::default() };
        let adopted = create_thread(&state, &req, "human", None, None).unwrap().thread.stream_id;
        commit(&side, "hands.md");
        ready(&adopted);
        let merged = merge_thread(&state, &adopted, None, "human").unwrap();
        let cleanup = merged.cleanup.unwrap();
        assert_eq!(cleanup, MergeCleanup { archived: true, ..Default::default() }, "only archived");
        assert!(side.is_dir() && branches().contains(&"w1/baker-hands".to_string()), "the operator's own worktree and branch");
        assert!(!list_threads(&state, false).unwrap().iter().any(|t| t.stream_id == adopted));
    }

    #[test]
    fn thread_read_reports_a_status_contract_and_thread_wait_returns_on_idle_or_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        baker(&state);
        let req = CreateThreadReq {
            title: Some("Audit".into()),
            model: Some("opus".into()),
            effort: Some("max".into()),
            acceptance: vec![Criterion::Text("lists every scope".into())],
            ..Default::default()
        };
        let thread = create_thread(&state, &req, "agent:baker", None, None).unwrap().thread.stream_id;
        let agent = crate::node::agent_id(&thread, Seat::Baker);
        let fact = |family: &str, payload: serde_json::Value| {
            append(&state, &thread, "agent:baker", family, payload.to_string(), vec![]).unwrap();
        };
        fact("agent.turn.started", serde_json::json!({"agent_id": agent, "turn_id": "t1", "instruction": "audit it", "mode": "auto"}));
        fact("agent.turn.usage", serde_json::json!({"agent_id": agent, "turn_id": "t1", "model_id": "opus", "tokens_in": 120, "tokens_out": 30}));
        fact("agent.turn.done", serde_json::json!({"agent_id": agent, "turn_id": "t1", "summary": "all scopes listed", "model_id": "acp"}));

        let read = read_thread(&state, &thread, 20).unwrap();
        assert_eq!(read.state, "idle");
        assert_eq!((read.model.as_deref(), read.effort.as_deref()), (Some("opus"), Some("max")));
        let result = read.result.unwrap();
        assert_eq!((result.status, result.result.as_str()), ("completed", "all scopes listed"));
        assert_eq!(result.tokens.unwrap().total, Some(150));
        assert_eq!(result.acceptance[0].verdict, "unverified");

        let rt = tokio::runtime::Runtime::new().unwrap();
        let wait = |secs| rt.block_on(wait_thread(state.clone(), thread.clone(), std::time::Duration::from_secs(secs))).unwrap();
        assert!(wait(5).done, "an idle thread answers at once");

        fact("agent.turn.started", serde_json::json!({"agent_id": agent, "turn_id": "t2", "instruction": "go on"}));
        let started = std::time::Instant::now();
        assert!(!wait(1).done, "a running thread is still working at the timeout");
        assert!(started.elapsed() >= std::time::Duration::from_secs(1));

        // Its turn ending wakes the wait.
        let ender = {
            let (state, thread, agent) = (state.clone(), thread.clone(), agent.clone());
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(300));
                let payload = serde_json::json!({"agent_id": agent, "turn_id": "t2", "reason": "hit its step limit"});
                append(&state, &thread, "agent:baker", "agent.turn.exhausted", payload.to_string(), vec![]).unwrap();
            })
        };
        let waited = wait(20);
        ender.join().unwrap();
        assert!(waited.done);
        let result = waited.thread.result.unwrap();
        assert_eq!((result.status, result.stop_reason), ("failed", Some("turn_capped")));

        // A Baker message to it carries no model or effort: it runs on the
        // thread's selector, which its opening set, as two rows its thread shows.
        message_thread(&state, &thread, "next step", "agent:baker").unwrap();
        let events = state.heart.lock().unwrap().read_stream(&thread).unwrap();
        let queued = events.iter().rev().find(|e| e.family == "agent.follow_up.queued").unwrap();
        let queued: serde_json::Value = serde_json::from_str(&queued.payload).unwrap();
        assert!(queued.get("model_id").is_none() && queued.get("effort").is_none(), "{queued}");
        let rows: Vec<(String, String)> = events
            .iter()
            .filter(|e| e.family == crate::routes::agent::NATIVE_CONFIG_ACKNOWLEDGED)
            .map(|e| serde_json::from_str::<serde_json::Value>(&e.payload).unwrap())
            .map(|p| (p["name"].as_str().unwrap().to_string(), p["value_name"].as_str().unwrap().to_string()))
            .collect();
        assert_eq!(rows, [("Model".to_string(), "opus".to_string()), ("Effort".to_string(), "max".to_string())]);
    }

    #[test]
    fn checked_criteria_report_their_proof_jobs_verdicts() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        baker(&state);
        let checked = |criterion: &str, check: &str| Criterion::Checked { criterion: criterion.into(), check: check.into() };
        let req = CreateThreadReq {
            acceptance: vec![
                Criterion::Text("reads well".into()),
                checked("builds", "cargo build"),
                checked("tests pass", "cargo test"),
                checked("lints", "cargo clippy"),
                checked("documents", "cargo doc"),
                checked("formats", "cargo fmt --check"),
            ],
            ..Default::default()
        };
        let created = create_thread(&state, &req, "agent:baker", None, None).unwrap();
        let note = created.note.unwrap_or_default();
        assert!(note.contains("read-only") && note.contains("worktree: true"), "checks in place are warned about: {note}");
        let plain = CreateThreadReq { acceptance: vec![Criterion::Text("reads well".into())], ..Default::default() };
        assert!(create_thread(&state, &plain, "agent:baker", None, None).unwrap().note.is_none());
        let thread = created.thread.stream_id;
        let agent = crate::node::agent_id(&thread, Seat::Baker);
        let fact = |family: &str, payload: serde_json::Value| {
            append(&state, &thread, "node", family, payload.to_string(), vec![]).unwrap();
        };
        let verdicts = || -> Vec<(&'static str, Option<String>, Option<u32>)> {
            let result = read_thread(&state, &thread, 20).unwrap().result.unwrap();
            result.acceptance.into_iter().map(|a| (a.verdict, a.detail, a.exit_code)).collect()
        };
        fact("agent.turn.started", serde_json::json!({"agent_id": agent, "turn_id": "t1", "instruction": "build it"}));
        fact("agent.turn.done", serde_json::json!({"agent_id": agent, "turn_id": "t1", "summary": "built"}));
        let v = verdicts();
        assert_eq!(v[0], ("unverified", None, None), "text is the Baker's to judge");
        assert!(v[1..].iter().all(|(verdict, ..)| *verdict == "pending"), "done, not proved yet: {v:?}");

        fact(THREAD_PROOF_FAMILY, serde_json::json!({"turn_id": "t1", "proofs": [
            {"criterion": "builds", "check": "cargo build", "job_id": "job-a"},
            {"criterion": "tests pass", "check": "cargo test", "job_id": "job-b"},
            {"criterion": "lints", "check": "cargo clippy", "job_id": "job-c"},
            {"criterion": "documents", "check": "cargo doc", "job_id": "job-d"},
            {"criterion": "formats", "check": "cargo fmt --check", "error": "no workspace to run a job in"},
        ]}));
        let ended = |job: &str, exit_code: serde_json::Value, stopped: bool, tail: &str| {
            let payload = serde_json::json!({"job_id": job, "exit_code": exit_code, "stopped": stopped, "tail": tail});
            fact(crate::jobs::JOB_ENDED, payload);
        };
        ended("job-a", 0.into(), false, "ok");
        ended("job-b", 101.into(), false, "1 test failed");
        ended("job-c", serde_json::Value::Null, true, "");
        let v = verdicts();
        assert_eq!(v[1], ("passed", None, Some(0)));
        assert_eq!(v[2], ("failed", Some("1 test failed".into()), Some(101)));
        assert_eq!(v[3], ("unverified", Some("its proof job was stopped".into()), None));
        assert_eq!(v[4].0, "unverified", "a job with no end that is not running was lost: {:?}", v[4]);
        assert_eq!(v[5], ("unverified", Some("no workspace to run a job in".into()), None));

        // A later turn that did not complete proves nothing.
        fact("agent.turn.started", serde_json::json!({"agent_id": agent, "turn_id": "t2", "instruction": "go on"}));
        fact("agent.turn.failed", serde_json::json!({"agent_id": agent, "turn_id": "t2", "reason": "boom"}));
        assert_eq!(verdicts()[1], ("unverified", Some("its turn did not complete".into()), None));
    }

    /// A workspace worker's branch waits in the merge review once its orc
    /// proposes it, never on the worker's own word, and goes into the
    /// workspace's branch only when the operator approves it, not the Baker,
    /// and exactly as the review showed it: a commit after, or a workspace
    /// on another branch, merges nothing. The workspace's own branch then
    /// goes to main the same way, the Baker told nothing.
    #[test]
    fn a_workers_branch_goes_into_its_workspace_when_the_operator_approves_the_orcs_proposal() {
        let dir = tempfile::tempdir().unwrap();
        let repo = bakery_repo_for_test(dir.path());
        run_git(&repo, &["add", "."]);
        run_git(&repo, &["commit", "-qm", "the bakery"]);
        let state = state_with_bakery_root(dir.path(), &repo);
        let home = crate::routes::baker::ensure_permanent_baker(&state, None, None).unwrap().stream_id;
        let workspace = crate::workspaces::create_workspace(&state, "Bakery", None, "human").unwrap();
        let req = CreateThreadReq { title: Some("styling".into()), ..Default::default() };
        let worker = create_peer(&state, &req, &workspace.stream_id, "human").unwrap().thread.stream_id;
        let theirs = std::path::PathBuf::from(context_for(&state, &worker).unwrap().worktree_path);
        std::fs::write(theirs.join("styling.md"), "styled\n").unwrap();
        run_git(&theirs, &["add", "."]);
        run_git(&theirs, &["commit", "-qm", "the styling"]);

        assert!(proposed(&state, &worker, None).unwrap_err().contains("your orc proposes"));
        assert!(proposed(&state, &home, Some(&worker)).unwrap_err().contains("not a worker this workspace opened"));
        let asked = proposed(&state, &workspace.stream_id, Some(&worker)).unwrap();
        let waiting = ready_to_merge(&state, &asked, Some("looked at it"), "agent:orchestrator").unwrap();
        assert_eq!((waiting.orc.as_deref(), waiting.into_stream_id.as_str()), (Some(workspace.stream_id.as_str()), workspace.stream_id.as_str()));
        assert_eq!(waiting.base, workspace.branch);
        assert_eq!(merge_waiting(&state).items.iter().map(|w| w.stream_id.as_str()).collect::<Vec<_>>(), [worker.as_str()]);

        assert!(merge_thread(&state, &worker, None, "agent:baker").unwrap_err().contains("operator approves"));
        std::fs::write(theirs.join("more.md"), "more\n").unwrap();
        run_git(&theirs, &["add", "."]);
        run_git(&theirs, &["commit", "-qm", "more"]);
        let unseen = merge_thread(&state, &worker, None, "human").unwrap_err();
        assert!(unseen.contains("changed since the review showed it"), "{unseen}");
        ready_to_merge(&state, &asked, None, "agent:orchestrator").unwrap();
        let orcs = std::path::Path::new(&workspace.worktree_path);
        run_git(orcs, &["checkout", "-q", "-b", "scratch"]);
        let elsewhere = merge_thread(&state, &worker, None, "human").unwrap_err();
        assert!(elsewhere.contains("is on scratch now"), "{elsewhere}");
        run_git(orcs, &["checkout", "-q", &workspace.branch]);
        let merged = merge_thread(&state, &worker, None, "human").unwrap();
        assert_eq!(merged.into, workspace.branch);
        assert!(orcs.join("styling.md").exists() && orcs.join("more.md").exists(), "in the workspace, as shown");
        assert!(!repo.join("styling.md").exists(), "not in main");
        assert!(merge_waiting(&state).items.is_empty(), "its merge closed it");

        let told = || state.heart.lock().unwrap().events_of_family(&home, crate::agent_control::FOLLOW_UP_QUEUED).unwrap().len();
        let before = told();
        let up = ready_to_merge(&state, &proposed(&state, &workspace.stream_id, None).unwrap(), None, "agent:orchestrator").unwrap();
        assert_eq!((up.orc, up.into_stream_id.as_str()), (None, home.as_str()));
        assert_eq!(told(), before, "the Baker is told nothing: it waits for the operator");
        assert!(merge_thread(&state, &workspace.stream_id, None, "agent:baker").unwrap_err().contains("operator approves"), "never the Baker's own merge");
        assert_eq!(merge_thread(&state, &workspace.stream_id, None, "human").unwrap().into, "main");
        assert!(repo.join("styling.md").exists(), "the workspace's branch reached main");
    }
}
