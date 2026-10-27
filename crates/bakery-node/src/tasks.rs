//! Task sockets and the completion contract (07d-B Phase 3).
//!
//! Tasks are project work, not factory plumbing: every `task.*` Heart fact
//! lives on the PARENT PROJECT's stream (`bakery-node/src/node.rs`'s
//! "Project catalog" section owns that stream's other facts), while each
//! task also gets its own lightweight socket — a Studio-visible row,
//! chat-capable the instant it exists — whose own stream carries only the
//! pointer-back facts (`socket.kind.recorded` = [`SocketKind::Task`],
//! `socket.project.set`, `socket.task.set`) plus, once the task is actually
//! started, `socket.workspace.set` recording the materialized worktree.
//!
//! Lifecycle, in order (07d-B Phase 3 item 5 — "identity first, workspace
//! at readiness"):
//!
//! > **create** durable task + socket identity → **wait** for
//! > dependencies/capacity (`queued`) → **materialize** workspace and
//! > record baseline → **execute** (Phase 4, not built here).
//!
//! Every op below is a single `pub(crate) fn` taking `(&AppState,
//! <op>Req)` so Stage B's Baker ops can call the exact same inner function
//! an HTTP route calls (item 6) — nothing here is HTTP-shaped beyond the
//! thin `_route` wrappers `main.rs` registers.

use crate::catalog;
use crate::node::{
    mint_id, Seat, SocketKind, SocketKindRecord, SocketProjectSet, SocketTaskSet, WorkspaceSet,
    SOCKET_KIND_FAMILY, SOCKET_PROJECT_FAMILY, SOCKET_TASK_FAMILY, SOCKET_WORKSPACE_FAMILY,
};
use crate::state::{append, request_cancel, AppState};
use axum::{
    extract::{Query, State},
    http::StatusCode,
    Json,
};
use bakery_core::Event;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

pub(crate) const TASK_CREATED_FAMILY: &str = "task.created";
pub(crate) const TASK_STARTED_FAMILY: &str = "task.started";
pub(crate) const TASK_MESSAGE_FAMILY: &str = "task.message";
pub(crate) const TASK_MESSAGE_DELIVERED_FAMILY: &str = "task.message.delivered";
pub(crate) const TASK_STATUS_FAMILY: &str = "task.status.changed";
pub(crate) const TASK_CANCELLED_FAMILY: &str = "task.cancelled";
pub(crate) const TASK_RETIRED_FAMILY: &str = "task.retired";
pub(crate) const TASK_WORKSPACE_MATERIALIZED_FAMILY: &str = "task.workspace.materialized";

// ─── Payloads ────────────────────────────────────────────────────────────

/// A task's completion contract (07d-B Phase 3 item 3) — advisory
/// coordination, not an enforcement primitive (item 6): `owned_files` is
/// declared, never checked against real writes, and `proof` is stored for
/// a human/Baker to run before signing off, never auto-invoked here.
#[derive(schemars::JsonSchema, Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct TaskContract {
    #[serde(default)]
    pub(crate) scope: String,
    /// Advisory globs — coordination only, never enforced (no file-lock
    /// primitive exists or is built here).
    #[serde(default)]
    pub(crate) owned_files: Vec<String>,
    /// Other tasks (by `task_id`, on the same parent project) that must
    /// reach `done` before this one may start.
    #[serde(default)]
    pub(crate) depends_on: Vec<String>,
    /// argv lists a human/Baker runs before signing this task off — stored,
    /// never auto-invoked by anything in this pass.
    #[serde(default)]
    pub(crate) proof: Vec<Vec<String>>,
    #[serde(default)]
    pub(crate) turn_budget: Option<u32>,
}

#[derive(schemars::JsonSchema, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TaskStatus {
    Queued,
    Running,
    Blocked,
    AwaitingReview,
    Done,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct TaskCreated {
    pub(crate) task_id: String,
    pub(crate) title: String,
    /// Derived from `title` at create time — the idempotency key alongside
    /// `work_order` (07d-B Phase 3 item 5: "idempotent per (work_order,
    /// task slug)").
    pub(crate) slug: String,
    pub(crate) parent_project_id: String,
    pub(crate) work_order: String,
    pub(crate) contract: TaskContract,
    /// The task socket's own stream id — filled in because the socket is
    /// always minted before this event is appended ("once the socket
    /// exists").
    pub(crate) stream_id: String,
    /// `owned_files` overlaps with other non-dependent tasks, recorded here
    /// (never blocking) so the row and Baker's own proposal can both show
    /// it without re-deriving it later.
    #[serde(default)]
    pub(crate) warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct TaskStatusChanged {
    pub(crate) task_id: String,
    pub(crate) status: TaskStatus,
    #[serde(default)]
    pub(crate) reason: Option<String>,
}

/// Records only that the task socket transitioned to `running` — it does
/// NOT claim a seat model was assigned or an instruction was issued.
/// Neither happens anywhere in this pass (Phase 4/background execution's
/// job); a prior revision recorded `seat`/`instruction` fields here that no
/// code path ever enacted, which is exactly the dishonest-event shape this
/// crate's event-sourcing rules forbid. Restore those fields only once a
/// writer actually assigns the seat and delivers the instruction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct TaskStarted {
    pub(crate) task_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct TaskMessage {
    pub(crate) task_id: String,
    pub(crate) text: String,
}

/// Acknowledges that the driver (Stage D) has consumed one `task.message`
/// event as a turn's instruction — `message_hash` is that event's own
/// `event_hash`, the same identity `agent.edit.accepted`/`rejected` use to
/// cite the proposal they decide. `crate::task_driver::next_undelivered_message`
/// is the only reader: the oldest `task.message` for a task whose hash never
/// appears here is "still in the inbox."
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct TaskMessageDelivered {
    pub(crate) task_id: String,
    pub(crate) message_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct TaskWorkspaceMaterialized {
    pub(crate) task_id: String,
    pub(crate) base_commit: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct TaskCancelled {
    pub(crate) task_id: String,
    #[serde(default)]
    pub(crate) reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct TaskRetired {
    pub(crate) task_id: String,
}

fn broadcast(state: &AppState, ev: &Event) {
    let _ = state.tx.send(serde_json::to_string(ev).unwrap_or_default());
}

// ─── Read side: fold a project's task.* facts ───────────────────────────

/// One task, folded from its parent project's stream. `status` always
/// reflects the latest `task.status.changed` — `task.created` itself
/// implies `queued`, so `task_create` always appends one right after
/// (single source of status truth, same "one channel, newest wins" shape
/// every other Heart-projected fact in this crate uses).
#[derive(Debug, Clone)]
pub(crate) struct TaskRecord {
    pub(crate) task_id: String,
    pub(crate) title: String,
    pub(crate) slug: String,
    pub(crate) parent_project_id: String,
    pub(crate) work_order: String,
    pub(crate) contract: TaskContract,
    pub(crate) stream_id: String,
    pub(crate) warnings: Vec<String>,
    pub(crate) status: TaskStatus,
    pub(crate) status_reason: Option<String>,
    pub(crate) retired: bool,
}

/// Fold every `task.*` fact on `project_stream_id` into today's task list,
/// in creation order. A malformed `task.created` is skipped (it never
/// became a real task); a malformed `task.status.changed`/`task.retired`
/// is likewise skipped rather than failing the whole read closed — unlike
/// a Project's root history, one bad status record for one task must not
/// blind every other task on the same project to its own facts.
fn tasks_for_project_locked(
    heart: &bakery_heart::Heart,
    project_stream_id: &str,
) -> Vec<TaskRecord> {
    let events = match heart.read_stream(project_stream_id) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };
    let mut order: Vec<String> = Vec::new();
    let mut by_id: HashMap<String, TaskRecord> = HashMap::new();
    for ev in &events {
        match ev.family.as_str() {
            TASK_CREATED_FAMILY => {
                if let Ok(c) = serde_json::from_str::<TaskCreated>(&ev.payload) {
                    order.push(c.task_id.clone());
                    by_id.insert(
                        c.task_id.clone(),
                        TaskRecord {
                            task_id: c.task_id,
                            title: c.title,
                            slug: c.slug,
                            parent_project_id: c.parent_project_id,
                            work_order: c.work_order,
                            contract: c.contract,
                            stream_id: c.stream_id,
                            warnings: c.warnings,
                            status: TaskStatus::Queued,
                            status_reason: None,
                            retired: false,
                        },
                    );
                }
            }
            TASK_STATUS_FAMILY => {
                if let Ok(s) = serde_json::from_str::<TaskStatusChanged>(&ev.payload) {
                    if let Some(t) = by_id.get_mut(&s.task_id) {
                        t.status = s.status;
                        t.status_reason = s.reason;
                    }
                }
            }
            TASK_RETIRED_FAMILY => {
                if let Ok(r) = serde_json::from_str::<TaskRetired>(&ev.payload) {
                    if let Some(t) = by_id.get_mut(&r.task_id) {
                        t.retired = true;
                    }
                }
            }
            _ => {}
        }
    }
    order
        .into_iter()
        .filter_map(|id| by_id.remove(&id))
        .collect()
}

fn tasks_for_project(state: &AppState, project_id: &str) -> Vec<TaskRecord> {
    let heart = state.heart.lock().unwrap();
    tasks_for_project_locked(&heart, project_id)
}

pub(crate) fn find_task(
    state: &AppState,
    project_id: &str,
    task_id: &str,
) -> Result<TaskRecord, String> {
    tasks_for_project(state, project_id)
        .into_iter()
        .find(|t| t.task_id == task_id)
        .ok_or_else(|| format!("unknown task {task_id} on project {project_id}"))
}

/// `crate::routes::baker::baker_slug`-shaped: first line, clipped, sanitized —
/// deliberately the same shape a factory-side slug uses, so both read as
/// "boring, filesystem/branch-safe identifier" to a human scanning rows.
fn task_slug(title: &str) -> String {
    let line = title.lines().next().unwrap_or(title).trim();
    let clipped: String = line.chars().take(48).collect();
    crate::git::sanitize_name(&clipped)
}

// ─── Validation: cycles and overlap warnings ────────────────────────────

/// Whether following `edges` (task_id -> its own `depends_on` list)
/// starting from `start`'s OWN dependency edges ever leads back to
/// `start`. Kept as a pure graph function, independent of `task_create`,
/// so it is directly unit-testable against a hand-built graph.
///
/// Through the real create path a genuine cycle can never actually form:
/// `depends_on` may only name a task that already exists on this project's
/// stream, and task ids are minted here — never client-supplied — so
/// nothing already recorded can name a task that does not exist yet. This
/// check still runs on every create (defense in depth, and the same
/// validation path a future batch-create with pre-minted ids would need).
fn depends_on_has_cycle(edges: &HashMap<String, Vec<String>>, start: &str) -> bool {
    fn dfs(
        edges: &HashMap<String, Vec<String>>,
        node: &str,
        start: &str,
        seen: &mut HashSet<String>,
    ) -> bool {
        let Some(deps) = edges.get(node) else {
            return false;
        };
        for d in deps {
            if d == start {
                return true;
            }
            if seen.insert(d.clone()) && dfs(edges, d, start, seen) {
                return true;
            }
        }
        false
    }
    let mut seen = HashSet::new();
    dfs(edges, start, start, &mut seen)
}

/// Transitive `depends_on` closure of `start` over `existing` — two tasks
/// connected by this closure never run concurrently (worktree isolation
/// plus dependency gating already guarantees it), so an `owned_files`
/// overlap between them is never worth a warning (07d-B Phase 3 item 6).
fn depends_on_closure(existing: &[TaskRecord], start: &[String]) -> HashSet<String> {
    let mut closure: HashSet<String> = HashSet::new();
    let mut stack: Vec<String> = start.to_vec();
    while let Some(id) = stack.pop() {
        if !closure.insert(id.clone()) {
            continue;
        }
        if let Some(t) = existing.iter().find(|t| t.task_id == id) {
            stack.extend(t.contract.depends_on.iter().cloned());
        }
    }
    closure
}

/// Whether two `owned_files` glob patterns could describe overlapping
/// paths. Advisory-only heuristic (ownership is declared coordination,
/// never enforced) — segment-wise comparison split on `/`, where a bare
/// `*` matches exactly one path segment and `**` matches the rest of the
/// path unconditionally (in either pattern). This is NOT full glob
/// semantics: no character classes, no brace expansion, and a `*` embedded
/// inside a literal segment (`*.rs`) is compared literally rather than as
/// a wildcard. A false negative here only costs a missed warning — overlap
/// is never enforced — and `**` is treated generously (may over-warn in
/// rare cases) rather than under-warn for the common prefix-style
/// ownership globs this feature targets.
fn globs_may_overlap(a: &str, b: &str) -> bool {
    let a_segs: Vec<&str> = a.split('/').collect();
    let b_segs: Vec<&str> = b.split('/').collect();
    let mut i = 0;
    loop {
        match (a_segs.get(i), b_segs.get(i)) {
            (None, None) => return true,
            (Some(&"**"), _) | (_, Some(&"**")) => return true,
            (None, Some(_)) | (Some(_), None) => return false,
            (Some(x), Some(y)) => {
                if x != y && *x != "*" && *y != "*" {
                    return false;
                }
            }
        }
        i += 1;
    }
}

fn overlap_warnings(
    existing: &[TaskRecord],
    new_owned: &[String],
    new_depends_on: &[String],
) -> Vec<String> {
    if new_owned.is_empty() {
        return Vec::new();
    }
    let related = depends_on_closure(existing, new_depends_on);
    let mut warnings = Vec::new();
    for t in existing {
        if t.retired || related.contains(&t.task_id) {
            continue;
        }
        for og in new_owned {
            for tg in &t.contract.owned_files {
                if globs_may_overlap(og, tg) {
                    warnings.push(format!(
                        "owned_files {og:?} may overlap task {} ({:?})'s {tg:?} — advisory only, not enforced",
                        t.task_id, t.title
                    ));
                }
            }
        }
    }
    warnings
}

// ─── Wire views ──────────────────────────────────────────────────────────

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct TaskView {
    pub(crate) task_id: String,
    pub(crate) title: String,
    pub(crate) slug: String,
    pub(crate) parent_project_id: String,
    pub(crate) work_order: String,
    pub(crate) contract: TaskContract,
    pub(crate) stream_id: String,
    pub(crate) status: TaskStatus,
    pub(crate) status_reason: Option<String>,
    pub(crate) warnings: Vec<String>,
    /// Terminal: `task.retired` was appended for this task. `status` alone
    /// cannot express this (there is no `TaskStatus::Retired` variant, and
    /// retire does not rewrite the status that was current at the time), so
    /// every consumer of this view — `GET /task/list`, `task_retire`'s own
    /// response — must check `retired` rather than trust `status` to have
    /// gone terminal on its own.
    pub(crate) retired: bool,
}

impl From<TaskRecord> for TaskView {
    fn from(t: TaskRecord) -> Self {
        TaskView {
            task_id: t.task_id,
            title: t.title,
            slug: t.slug,
            parent_project_id: t.parent_project_id,
            work_order: t.work_order,
            contract: t.contract,
            stream_id: t.stream_id,
            status: t.status,
            status_reason: t.status_reason,
            warnings: t.warnings,
            retired: t.retired,
        }
    }
}

// ─── task.create ─────────────────────────────────────────────────────────

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct TaskCreateReq {
    pub(crate) parent_project_id: String,
    pub(crate) title: String,
    /// Names the Baker work-order source (for A6: the spec path plus the
    /// plan event hash) — free-form, never parsed beyond the idempotency
    /// key it forms with the derived slug.
    pub(crate) work_order: String,
    #[serde(default)]
    pub(crate) contract: TaskContract,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct TaskCreateResp {
    pub(crate) task: TaskView,
    /// `false` when this call matched an existing `(work_order, slug)` and
    /// returned it unchanged instead of minting a new one.
    pub(crate) created: bool,
}

/// Mints a task's durable identity ONLY (07d-B Phase 3 item 5): the
/// `task.created` contract event + `task.status.changed(queued)` on the
/// parent project's stream, and a chat-only-style task socket (no
/// worktree, no root binding, no baseline yet — that is `task_start`'s
/// job). Idempotent per `(work_order, slug)`: a retry that lands after a
/// partial crash finds the existing task and mints nothing new.
pub(crate) fn task_create(state: &AppState, req: TaskCreateReq) -> Result<TaskCreateResp, String> {
    task_create_as(state, req, "human")
}

/// [`task_create`]'s real body, parameterized on the actor every appended
/// event carries (07d-B Stage B item 1: a Baker-op-triggered effect records
/// `actor: "agent:baker"`, never the human route's hardcoded `"human"` —
/// same function, same events, different initiator). `task_create` is the
/// thin human-route default (`actor = "human"`); `crate::baker_ops` is the
/// only other caller, with `actor = "agent:baker"`.
pub(crate) fn task_create_as(
    state: &AppState,
    req: TaskCreateReq,
    actor: &str,
) -> Result<TaskCreateResp, String> {
    let title = req.title.trim().to_string();
    if title.is_empty() {
        return Err("task title must not be empty".to_string());
    }
    let work_order = req.work_order.trim().to_string();
    if work_order.is_empty() {
        return Err("work_order must not be empty".to_string());
    }
    let slug = task_slug(&title);

    let result = (|| -> Result<(TaskRecord, bool, Vec<Event>), String> {
        let mut heart = state.heart.lock().unwrap();

        if catalog::is_retired(&heart, &req.parent_project_id) {
            return Err(format!("project {} is retired", req.parent_project_id));
        }
        let project = catalog::project_record_locked(&heart, &req.parent_project_id)
            .ok_or_else(|| format!("unknown project {}", req.parent_project_id))?;

        let existing_tasks = tasks_for_project_locked(&heart, &req.parent_project_id);

        if let Some(found) = existing_tasks
            .iter()
            .find(|t| !t.retired && t.work_order == work_order && t.slug == slug)
        {
            // `slug` clips at 48 chars, so two genuinely different titles
            // can collapse onto the same (work_order, slug) key. Matching
            // on slug alone would silently hand the caller someone else's
            // task — compare the untruncated title too, and refuse rather
            // than pretend they're the same task when they aren't.
            if found.title == title {
                return Ok((found.clone(), false, Vec::new()));
            }
            return Err(format!(
                "task {:?} already exists for (work_order={work_order:?}, slug={slug:?}); \
                 title {:?} collides with it after 48-char slug truncation — \
                 use a more distinct title or a different work_order",
                found.title, title
            ));
        }

        for dep in &req.contract.depends_on {
            if !existing_tasks.iter().any(|t| &t.task_id == dep) {
                return Err(format!(
                    "depends_on references unknown task {dep} on this project"
                ));
            }
        }
        let mut edges: HashMap<String, Vec<String>> = existing_tasks
            .iter()
            .map(|t| (t.task_id.clone(), t.contract.depends_on.clone()))
            .collect();
        const PENDING: &str = "\0pending-task\0";
        edges.insert(PENDING.to_string(), req.contract.depends_on.clone());
        if depends_on_has_cycle(&edges, PENDING) {
            return Err("depends_on creates a dependency cycle".to_string());
        }

        let warnings = overlap_warnings(
            &existing_tasks,
            &req.contract.owned_files,
            &req.contract.depends_on,
        );

        let task_id = mint_id("task");
        let socket_id = mint_id("socket");

        // Chat-only-style mint (07d-A3's `create_project_with` None-root
        // branch): empty workspace fields now, bound later by `task_start`
        // via `socket.workspace.set` — never a second `project.onboarded`.
        let onboarded_payload = serde_json::json!({
            "repo": "",
            "repo_root": "",
            "project": project.name,
            "name": title,
            "worktree_path": "",
            "branch": "",
            "opened_by_node": state.node.lock().unwrap().node_id.clone(),
            "created_by_bakery": false,
        })
        .to_string();
        let onboarded_ev = catalog::append_locked(
            &mut heart,
            &socket_id,
            actor,
            "project.onboarded",
            onboarded_payload,
            vec![],
        )?;
        let kind_ev = catalog::append_locked(
            &mut heart,
            &socket_id,
            actor,
            SOCKET_KIND_FAMILY,
            serde_json::to_string(&SocketKindRecord {
                kind: SocketKind::Task,
            })
            .unwrap(),
            vec![onboarded_ev.event_hash.clone()],
        )?;
        let project_set_ev = catalog::append_locked(
            &mut heart,
            &socket_id,
            actor,
            SOCKET_PROJECT_FAMILY,
            serde_json::to_string(&SocketProjectSet {
                project_id: Some(req.parent_project_id.clone()),
                root_id: None,
            })
            .unwrap(),
            vec![kind_ev.event_hash.clone()],
        )?;
        let task_set_ev = catalog::append_locked(
            &mut heart,
            &socket_id,
            actor,
            SOCKET_TASK_FAMILY,
            serde_json::to_string(&SocketTaskSet {
                task_id: task_id.clone(),
                work_order: work_order.clone(),
            })
            .unwrap(),
            vec![project_set_ev.event_hash.clone()],
        )?;

        let created = TaskCreated {
            task_id: task_id.clone(),
            title: title.clone(),
            slug: slug.clone(),
            parent_project_id: req.parent_project_id.clone(),
            work_order: work_order.clone(),
            contract: req.contract.clone(),
            stream_id: socket_id.clone(),
            warnings: warnings.clone(),
        };
        let created_ev = catalog::append_locked(
            &mut heart,
            &req.parent_project_id,
            actor,
            TASK_CREATED_FAMILY,
            serde_json::to_string(&created).unwrap(),
            vec![task_set_ev.event_hash.clone()],
        )?;
        let status_ev = catalog::append_locked(
            &mut heart,
            &req.parent_project_id,
            actor,
            TASK_STATUS_FAMILY,
            serde_json::to_string(&TaskStatusChanged {
                task_id: task_id.clone(),
                status: TaskStatus::Queued,
                reason: Some("created".to_string()),
            })
            .unwrap(),
            vec![created_ev.event_hash.clone()],
        )?;

        let events = vec![
            onboarded_ev,
            kind_ev,
            project_set_ev,
            task_set_ev,
            created_ev,
            status_ev,
        ];
        drop(heart);
        crate::state::claim_ownership(state, &socket_id)?;

        let record = TaskRecord {
            task_id,
            title,
            slug,
            parent_project_id: req.parent_project_id.clone(),
            work_order,
            contract: req.contract.clone(),
            stream_id: socket_id,
            warnings,
            status: TaskStatus::Queued,
            status_reason: Some("created".to_string()),
            retired: false,
        };
        Ok((record, true, events))
    })();

    match result {
        Ok((record, created, events)) => {
            for ev in &events {
                broadcast(state, ev);
            }
            Ok(TaskCreateResp {
                task: record.into(),
                created,
            })
        }
        Err(e) => Err(e),
    }
}

// ─── task.start (materialization) ───────────────────────────────────────

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct TaskStartReq {
    pub(crate) project_id: String,
    pub(crate) task_id: String,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct TaskStartResp {
    pub(crate) task: TaskView,
    pub(crate) materialized: bool,
    pub(crate) worktree_path: Option<String>,
    pub(crate) branch: Option<String>,
    pub(crate) base_commit: Option<String>,
}

fn resolve_commit(repo_root: &str, refname: &str) -> Result<String, String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .arg("rev-parse")
        .arg(refname)
        .output()
        .map_err(|e| format!("failed to run git: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git rev-parse {refname} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Materializes a task's workspace as the first act of starting it (07d-B
/// Phase 3 item 5), then flips it to `running`. Refuses (stays `queued`,
/// honest reason recorded) while any `depends_on` task is not yet `done` —
/// dependents' worktrees must be cut from a base that already contains
/// every integrated predecessor.
///
/// Idempotent once running: calling this again on an already-`running`
/// task is a no-op that returns the current view, never a second
/// materialization (07d-B Phase 3 item 5: "re-approving an already-launched
/// plan is a no-op, not a second launch" generalizes to task start too).
///
/// A task reaches `done` only via `task_review::task_signoff` (07d-B Stage
/// D) — proof (`task_driver`) plus a human sign-off, never automatically —
/// so a dependent's `depends_on` gate above is live end-to-end: it stays
/// `queued` until every predecessor is genuinely signed off, not merely
/// `awaiting_review`.
///
/// The ancestry gate (Checkpoint 1 item 3) is live at materialization:
/// beyond `status == Done`, every predecessor's `signoff_commit` must be a
/// real ancestor (`git merge-base --is-ancestor`) of the base this task is
/// about to branch from — fetched-and-rechecked once, in case integration
/// happened remotely — or the task stays `queued` with the integration
/// instruction in its reason. A chat-only predecessor (no commit) has
/// nothing to integrate and passes on status alone.
pub(crate) fn task_start(state: &AppState, req: TaskStartReq) -> Result<TaskStartResp, String> {
    task_start_as(state, req, "human")
}

/// [`task_start`]'s real body — see [`task_create_as`]'s doc comment on why
/// `actor` is a parameter here rather than a second implementation.
pub(crate) fn task_start_as(
    state: &AppState,
    req: TaskStartReq,
    actor: &str,
) -> Result<TaskStartResp, String> {
    let task = find_task(state, &req.project_id, &req.task_id)?;
    if task.retired {
        return Err(format!("task {} has been retired", task.task_id));
    }
    if task.status == TaskStatus::Running {
        return Ok(TaskStartResp {
            task: task.into(),
            materialized: false,
            worktree_path: None,
            branch: None,
            base_commit: None,
        });
    }
    if matches!(task.status, TaskStatus::Done | TaskStatus::Cancelled) {
        return Err(format!(
            "task {} is already {:?} — cannot start",
            task.task_id, task.status
        ));
    }

    let all_tasks = tasks_for_project(state, &req.project_id);
    for dep in &task.contract.depends_on {
        match all_tasks.iter().find(|t| &t.task_id == dep) {
            None => {
                return Err(format!(
                    "depends_on task {dep} no longer exists on this project"
                ))
            }
            Some(d) if d.status != TaskStatus::Done => {
                let reason = format!("waiting on dependency {dep} ({:?})", d.status);
                append(
                    state,
                    &req.project_id,
                    actor,
                    TASK_STATUS_FAMILY,
                    serde_json::to_string(&TaskStatusChanged {
                        task_id: task.task_id.clone(),
                        status: TaskStatus::Queued,
                        reason: Some(reason.clone()),
                    })
                    .unwrap(),
                    vec![],
                )?;
                return Err(format!("task {} stays queued — {reason}", task.task_id));
            }
            Some(_) => {}
        }
    }

    let project = catalog::project_by_id(state, &req.project_id)
        .ok_or_else(|| format!("unknown project {}", req.project_id))?;

    let (materialized, worktree_path, branch, base_commit) = if project.roots.is_empty() {
        // Chat-only task: no root, no materialization step, no baseline.
        (false, None, None, None)
    } else {
        // 07d-A3 ships a single-root UI; a task's parent may in principle
        // carry more than one attached root, but nothing yet declares which
        // one a given task targets — pick the first deterministically,
        // same simplification `attach_directory_root`'s single-root
        // callers already make.
        let root = &project.roots[0];
        if !root.is_git {
            return Err(
                "in-place task workspaces are not built yet — this lands with plain-dir snapshot support".to_string(),
            );
        }
        let repo_root = root.path.clone();
        let info = crate::git::inspect(&repo_root);
        if !info.is_git {
            return Err(format!(
                "project root {repo_root} is no longer a git repository"
            ));
        }
        // Keyed on the task id (never the slug alone): two distinct tasks
        // can share a title (same project, different work_order, or two
        // projects entirely), and a slug-only key made them silently share
        // one worktree/branch — the exact isolation primitive the spec
        // names as *the* per-task guarantee. `task_id` is server-minted and
        // unique per task, so appending it makes collision structurally
        // impossible rather than merely unlikely.
        let id_suffix = task.task_id.strip_prefix("task_").unwrap_or(&task.task_id);
        let branch = format!("task/{}-{id_suffix}", task.slug);
        let wt_path = state
            .worktree_base
            .join(format!("task-{}-{id_suffix}", task.slug));
        // Rule 2: the newer main, just fetched; offline, the local main.
        let fork = crate::git::fresh_main(std::path::Path::new(&repo_root));
        let base_ref = Some(fork.base.clone());
        let mut base_commit = resolve_commit(&repo_root, base_ref.as_deref().unwrap_or("HEAD"))?;

        // Integration is proven, not assumed: a predecessor being marked
        // done is insufficient — its signed-off revision must actually be
        // present in the base this task branches from, or the dependent
        // would build against code that lacks the approved changes. Fetch
        // once and re-resolve when the check fails (integration may have
        // landed remotely), then refuse honestly.
        let repo_path_for_deps = std::path::PathBuf::from(&repo_root);
        for dep in &task.contract.depends_on {
            let Some(signoff) =
                crate::task_review::latest_signoff_commit(state, &req.project_id, dep)
            else {
                continue; // chat-only predecessor — nothing to integrate
            };
            let mut integrated =
                crate::git::is_ancestor(&repo_path_for_deps, &signoff, &base_commit)
                    .unwrap_or(false);
            if !integrated {
                let _ = crate::git::fetch_all(&repo_path_for_deps);
                base_commit = resolve_commit(&repo_root, base_ref.as_deref().unwrap_or("HEAD"))?;
                integrated = crate::git::is_ancestor(&repo_path_for_deps, &signoff, &base_commit)
                    .unwrap_or(false);
            }
            if !integrated {
                let reason = format!(
                    "waiting on dependency {dep}: its signed-off revision {} is not in this task's base {} — integrate it (local task/integrate, or push + merge the PR) before this task can start",
                    &signoff[..12.min(signoff.len())],
                    &base_commit[..12.min(base_commit.len())]
                );
                append(
                    state,
                    &req.project_id,
                    actor,
                    TASK_STATUS_FAMILY,
                    serde_json::to_string(&TaskStatusChanged {
                        task_id: task.task_id.clone(),
                        status: TaskStatus::Queued,
                        reason: Some(reason.clone()),
                    })
                    .unwrap(),
                    vec![],
                )?;
                return Err(format!("task {} stays queued — {reason}", task.task_id));
            }
        }

        let created_by_bakery = if wt_path.is_dir() {
            // Adopt a worktree a crashed earlier attempt already created —
            // same "adopt, don't re-`git worktree add`" rule
            // `routes::project::open_stream` uses — rather than failing on
            // a path/branch that already exists. Refuse the adoption
            // outright unless the directory is actually a worktree of THIS
            // project's own repo, on THIS task's own branch: adopting a
            // directory that resolves into a different repository would
            // point the task's jail at the wrong tree, and `git worktree
            // remove` on retire would run against the wrong `-C` root.
            let existing = crate::git::inspect(&wt_path.to_string_lossy());
            let same_repo = existing.root.as_deref().is_some_and(|r| {
                crate::state::same_path(r, info.root.as_deref().unwrap_or(&repo_root))
            });
            if !existing.is_git
                || existing.current_branch.as_deref() != Some(branch.as_str())
                || !same_repo
            {
                return Err(format!(
                    "{} exists but is not a {branch} worktree of {repo_root} — remove it or retry once the conflict is resolved",
                    wt_path.display()
                ));
            }
            // We didn't run `git worktree add` for it, so we cannot prove
            // Bakery created it — `discard_stream`'s "only remove what we
            // can prove we created" gate depends on this being honest.
            false
        } else {
            // Because the branch name is now unique per task (it embeds the
            // task id), an existing local branch of that exact name can only
            // mean a genuine naming conflict, never "this task's own retired
            // predecessor" — refuse rather than letting `add_worktree` check
            // it out and silently start the new task on old, possibly
            // unrelated commits.
            if info.local_branches.iter().any(|b| b == &branch) {
                return Err(format!(
                    "branch {branch} already exists in {repo_root} — refusing to reuse it for a new task worktree"
                ));
            }
            crate::routes::project::add_worktree(
                &repo_root,
                &wt_path,
                &branch,
                base_ref.as_deref(),
                &info.local_branches,
            )?;
            // Belt-and-suspenders: a freshly cut worktree must sit exactly
            // on the base commit we are about to record as its baseline —
            // if it doesn't, `task.workspace.materialized.base_commit`
            // would be recording a baseline the worktree never actually
            // started from.
            let head = resolve_commit(&wt_path.to_string_lossy(), "HEAD")?;
            if head != base_commit {
                return Err(format!(
                    "materialized worktree HEAD {head} does not match recorded base_commit {base_commit} — refusing to record a false baseline"
                ));
            }
            true
        };

        append(
            state,
            &req.project_id,
            actor,
            TASK_WORKSPACE_MATERIALIZED_FAMILY,
            serde_json::to_string(&TaskWorkspaceMaterialized {
                task_id: task.task_id.clone(),
                base_commit: base_commit.clone(),
            })
            .unwrap(),
            vec![],
        )?;
        append(
            state,
            &task.stream_id,
            actor,
            SOCKET_WORKSPACE_FAMILY,
            serde_json::to_string(&WorkspaceSet {
                worktree_path: wt_path.to_string_lossy().into_owned(),
                branch: branch.clone(),
                repo_root: repo_root.clone(),
                created_by_bakery,
                fork_note: fork.stale.clone(),
            })
            .unwrap(),
            vec![],
        )?;
        (
            true,
            Some(wt_path.to_string_lossy().into_owned()),
            Some(branch),
            Some(base_commit),
        )
    };

    // `seat`/`instruction` are deliberately NOT recorded on `task.started`
    // (07d-B Phase 3 item 3 names them, but nothing in this pass assigns a
    // seat model or delivers an instruction — that is Phase 4/background
    // execution territory). Claiming them here without any code path that
    // enacts them would be exactly the silent/dishonest-event shape the
    // Heart's event-sourcing rules forbid: this fact only asserts that the
    // task socket transitioned to `running`, nothing about what is running
    // on it.
    append(
        state,
        &req.project_id,
        actor,
        TASK_STARTED_FAMILY,
        serde_json::to_string(&TaskStarted {
            task_id: task.task_id.clone(),
        })
        .unwrap(),
        vec![],
    )?;
    append(
        state,
        &req.project_id,
        actor,
        TASK_STATUS_FAMILY,
        serde_json::to_string(&TaskStatusChanged {
            task_id: task.task_id.clone(),
            status: TaskStatus::Running,
            reason: Some("started".to_string()),
        })
        .unwrap(),
        vec![],
    )?;

    let updated = find_task(state, &req.project_id, &req.task_id)?;
    Ok(TaskStartResp {
        task: updated.into(),
        materialized,
        worktree_path,
        branch,
        base_commit,
    })
}

// ─── task.message ────────────────────────────────────────────────────────

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct TaskMessageReq {
    pub(crate) project_id: String,
    pub(crate) task_id: String,
    pub(crate) text: String,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct TaskMessageResp {
    #[schemars(with = "crate::api_catalog::EventShape")]
    pub(crate) event: Event,
}

/// Appends to the task's persisted inbox (07d-B Phase 3 item 4). Delivery
/// consumption — the driver reading this at the next safe boundary and
/// acknowledging it with `task.message.delivered` — is
/// `crate::task_driver::next_undelivered_message`'s job (Stage D). A message
/// that lands while the task is `Blocked` also wakes it back to `Running`
/// (`crate::task_driver::deliver_message_and_maybe_resume`) — the one path
/// through which an `ask_user`/turn-budget block or a restart-recovered
/// block ever resumes.
pub(crate) fn task_message(
    state: &AppState,
    req: TaskMessageReq,
) -> Result<TaskMessageResp, String> {
    task_message_as(state, req, "human")
}

/// [`task_message`]'s real body — see [`task_create_as`]'s doc comment on
/// why `actor` is a parameter here rather than a second implementation.
pub(crate) fn task_message_as(
    state: &AppState,
    req: TaskMessageReq,
    actor: &str,
) -> Result<TaskMessageResp, String> {
    let text = req.text.trim();
    if text.is_empty() {
        return Err("message text must not be empty".to_string());
    }
    let task = find_task(state, &req.project_id, &req.task_id)?;
    if task.retired {
        return Err(format!("task {} has been retired", task.task_id));
    }
    let event = append(
        state,
        &req.project_id,
        actor,
        TASK_MESSAGE_FAMILY,
        serde_json::to_string(&TaskMessage {
            task_id: task.task_id.clone(),
            text: text.to_string(),
        })
        .unwrap(),
        vec![],
    )?;
    crate::task_driver::deliver_message_and_maybe_resume(state, &req.project_id, &task.task_id);
    Ok(TaskMessageResp { event })
}

// ─── task.cancel ─────────────────────────────────────────────────────────

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct TaskCancelReq {
    pub(crate) project_id: String,
    pub(crate) task_id: String,
    #[serde(default)]
    pub(crate) reason: Option<String>,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct TaskCancelResp {
    pub(crate) task: TaskView,
    /// Whether an in-flight worker-seat turn's cancel flag was actually
    /// flipped — `false` just means no turn was running, not that the
    /// cancel failed: the task moves to `cancelled` either way.
    pub(crate) turn_cancel_requested: bool,
}

/// Flips the agent cancel flag for the task's worker seat (reusing
/// `POST /agent/turn/cancel`'s own `request_cancel`) and records the task
/// as terminally cancelled.
pub(crate) fn task_cancel(state: &AppState, req: TaskCancelReq) -> Result<TaskCancelResp, String> {
    task_cancel_as(state, req, "human")
}

/// [`task_cancel`]'s real body — see [`task_create_as`]'s doc comment on why
/// `actor` is a parameter here rather than a second implementation.
pub(crate) fn task_cancel_as(
    state: &AppState,
    req: TaskCancelReq,
    actor: &str,
) -> Result<TaskCancelResp, String> {
    let task = find_task(state, &req.project_id, &req.task_id)?;
    if task.retired {
        return Err(format!("task {} has been retired", task.task_id));
    }
    if matches!(task.status, TaskStatus::Done | TaskStatus::Cancelled) {
        return Err(format!(
            "task {} is already {:?}",
            task.task_id, task.status
        ));
    }

    // A task socket is not a Baker stream (`SocketKind::Task`, never
    // `Factory`), so `routes::agent::default_seat_for` runs its turns under
    // `Seat::Orchestrator` — `Worker` is "not yet auto-selected by anything
    // in this pass" (`node.rs`). Hard-coding `Worker` here meant a running
    // task turn was never actually reachable by cancel: `request_cancel`
    // found no `agent_activity` entry and silently returned `false`.
    // Request cancel for every seat a task socket's turns could plausibly be
    // running under instead of guessing one — an unstarted/absent seat's
    // `request_cancel` is a harmless no-op (`false`), so this is never worse
    // than the single-seat version and is correct regardless of which seat
    // actually picked up the turn, now or after a future seat reassignment.
    let turn_cancel_requested = [Seat::Orchestrator, Seat::Worker, Seat::Baker]
        .into_iter()
        .map(|seat| request_cancel(state, &crate::node::agent_id(&task.stream_id, seat)))
        .fold(false, |any, requested| any || requested);

    append(
        state,
        &req.project_id,
        actor,
        TASK_CANCELLED_FAMILY,
        serde_json::to_string(&TaskCancelled {
            task_id: task.task_id.clone(),
            reason: req.reason.clone(),
        })
        .unwrap(),
        vec![],
    )?;
    append(
        state,
        &req.project_id,
        actor,
        TASK_STATUS_FAMILY,
        serde_json::to_string(&TaskStatusChanged {
            task_id: task.task_id.clone(),
            status: TaskStatus::Cancelled,
            reason: req.reason,
        })
        .unwrap(),
        vec![],
    )?;

    let updated = find_task(state, &req.project_id, &req.task_id)?;
    Ok(TaskCancelResp {
        task: updated.into(),
        turn_cancel_requested,
    })
}

// ─── task.retire ─────────────────────────────────────────────────────────

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct TaskRetireReq {
    pub(crate) project_id: String,
    pub(crate) task_id: String,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct TaskRetireResp {
    pub(crate) task: TaskView,
    pub(crate) discard: crate::routes::lifecycle::DiscardResp,
}

/// Discards the task socket via the existing discard path
/// (`routes::lifecycle::discard_stream` — worktree/room release, PTY/ACP
/// fencing, all of it) and marks the task itself terminally retired.
pub(crate) async fn task_retire(
    state: &AppState,
    req: TaskRetireReq,
) -> Result<TaskRetireResp, String> {
    task_retire_as(state, req, "human").await
}

/// [`task_retire`]'s real body — see [`task_create_as`]'s doc comment on why
/// `actor` is a parameter here rather than a second implementation. The
/// delegated `discard_stream` call keeps its own actor semantics unchanged
/// (out of scope here — see 07d-B Stage B's `remaining_work`).
pub(crate) async fn task_retire_as(
    state: &AppState,
    req: TaskRetireReq,
    actor: &str,
) -> Result<TaskRetireResp, String> {
    let task = find_task(state, &req.project_id, &req.task_id)?;
    if task.retired {
        return Err(format!("task {} has already been retired", task.task_id));
    }

    let discard = crate::routes::lifecycle::discard_stream(
        State(state.clone()),
        Json(crate::routes::lifecycle::DiscardReq {
            stream_id: task.stream_id.clone(),
            confirm: true,
        }),
    )
    .await
    .map_err(|(_, msg)| msg)?
    .0;

    append(
        state,
        &req.project_id,
        actor,
        TASK_RETIRED_FAMILY,
        serde_json::to_string(&TaskRetired {
            task_id: task.task_id.clone(),
        })
        .unwrap(),
        vec![],
    )?;

    let updated = find_task(state, &req.project_id, &req.task_id)?;
    Ok(TaskRetireResp {
        task: updated.into(),
        discard,
    })
}

// ─── task.list ───────────────────────────────────────────────────────────

pub(crate) fn task_list(state: &AppState, project_id: &str) -> Vec<TaskView> {
    tasks_for_project(state, project_id)
        .into_iter()
        .map(Into::into)
        .collect()
}

// ─── HTTP routes ─────────────────────────────────────────────────────────

pub(crate) async fn task_create_route(
    State(st): State<AppState>,
    Json(req): Json<TaskCreateReq>,
) -> Result<Json<TaskCreateResp>, (StatusCode, String)> {
    task_create(&st, req)
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))
}

pub(crate) async fn task_start_route(
    State(st): State<AppState>,
    Json(req): Json<TaskStartReq>,
) -> Result<Json<TaskStartResp>, (StatusCode, String)> {
    // `git worktree add` is blocking; stay off the async workers (same
    // discipline `baker_assign` uses).
    let st2 = st.clone();
    let resp = crate::actor::spawn_blocking(move || task_start(&st2, req))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    // Kick the Stage D driver off (or onto the capacity queue) now that the
    // task is materialized and `Running` — a no-op if a driver is already
    // admitted for it (idempotent re-`task_start` calls, `task_started_event_*`
    // tests that never touch the driver at all).
    crate::task_driver::start(&st, &resp.task.parent_project_id, &resp.task.task_id);
    Ok(Json(resp))
}

pub(crate) async fn task_message_route(
    State(st): State<AppState>,
    Json(req): Json<TaskMessageReq>,
) -> Result<Json<TaskMessageResp>, (StatusCode, String)> {
    task_message(&st, req)
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))
}

pub(crate) async fn task_cancel_route(
    State(st): State<AppState>,
    Json(req): Json<TaskCancelReq>,
) -> Result<Json<TaskCancelResp>, (StatusCode, String)> {
    task_cancel(&st, req)
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))
}

pub(crate) async fn task_retire_route(
    State(st): State<AppState>,
    Json(req): Json<TaskRetireReq>,
) -> Result<Json<TaskRetireResp>, (StatusCode, String)> {
    task_retire(&st, req)
        .await
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))
}

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct TaskListQuery {
    pub(crate) project_id: String,
}

pub(crate) async fn task_list_route(
    State(st): State<AppState>,
    Query(q): Query<TaskListQuery>,
) -> Json<Vec<TaskView>> {
    Json(task_list(&st, &q.project_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;
    use bakery_agent::StubModel;
    use std::sync::Arc;

    fn project_with_git_root(state: &AppState, dir: &std::path::Path) -> String {
        let repo = make_repo(&dir.join("repo"));
        let created = catalog::create_project(
            state,
            "P",
            Some(catalog::DirectoryRootSpec::Attach(&repo.to_string_lossy())),
        )
        .expect("create project");
        created.0.project_id
    }

    fn chat_only_project(state: &AppState) -> String {
        catalog::create_project(state, "Chat P", None)
            .expect("create project")
            .0
            .project_id
    }

    fn base_req(project_id: &str, title: &str, work_order: &str) -> TaskCreateReq {
        TaskCreateReq {
            parent_project_id: project_id.to_string(),
            title: title.to_string(),
            work_order: work_order.to_string(),
            contract: TaskContract::default(),
        }
    }

    #[tokio::test]
    async fn full_lifecycle_round_trips_from_create_through_retire() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let project_id = project_with_git_root(&state, dir.path());

        let created =
            task_create(&state, base_req(&project_id, "Do the thing", "wo-1")).expect("create");
        assert!(created.created);
        assert_eq!(created.task.status, TaskStatus::Queued);
        let task_id = created.task.task_id.clone();

        let started = task_start(
            &state,
            TaskStartReq {
                project_id: project_id.clone(),
                task_id: task_id.clone(),
            },
        )
        .expect("start");
        assert!(started.materialized);
        assert_eq!(started.task.status, TaskStatus::Running);
        let wt_path = std::path::PathBuf::from(started.worktree_path.clone().unwrap());
        assert!(wt_path.is_dir(), "worktree must exist on disk");
        assert!(started.branch.as_deref().unwrap().starts_with("task/"));
        assert!(!started.base_commit.clone().unwrap().is_empty());

        let msg = task_message(
            &state,
            TaskMessageReq {
                project_id: project_id.clone(),
                task_id: task_id.clone(),
                text: "steer this way".into(),
            },
        )
        .expect("message");
        assert_eq!(msg.event.family, TASK_MESSAGE_FAMILY);

        let cancelled = task_cancel(
            &state,
            TaskCancelReq {
                project_id: project_id.clone(),
                task_id: task_id.clone(),
                reason: Some("changed mind".into()),
            },
        )
        .expect("cancel");
        assert_eq!(cancelled.task.status, TaskStatus::Cancelled);

        let retired = task_retire(
            &state,
            TaskRetireReq {
                project_id: project_id.clone(),
                task_id: task_id.clone(),
            },
        )
        .await
        .expect("retire");
        assert!(
            retired.discard.worktree_removed,
            "retire must release the materialized worktree"
        );
        assert!(
            !wt_path.exists(),
            "worktree directory must be gone after retire"
        );

        let listed = task_list(&state, &project_id);
        let row = listed.iter().find(|t| t.task_id == task_id).unwrap();
        assert_eq!(row.status, TaskStatus::Cancelled);
    }

    /// Regression: a task socket is never a Baker/Factory-kind stream, so
    /// `routes::agent::default_seat_for` runs its turns under
    /// `Seat::Orchestrator` — `Worker` is "not yet auto-selected by anything
    /// in this pass" (`node.rs`). `task_cancel_as` used to hard-code
    /// `request_cancel` at the task's `Worker` seat, which had no
    /// `agent_activity` entry for a real task-socket turn, so cancel was
    /// dead on arrival. Simulate a genuinely in-flight turn the way the real
    /// tool loop would (`state::start_agent_activity`) and prove cancel
    /// actually reaches it.
    #[test]
    fn cancel_reaches_the_orchestrator_seat_a_task_sockets_turns_actually_run_under() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let project_id = project_with_git_root(&state, dir.path());
        let created = task_create(&state, base_req(&project_id, "Long task", "wo-cancel")).unwrap();
        task_start(
            &state,
            TaskStartReq {
                project_id: project_id.clone(),
                task_id: created.task.task_id.clone(),
            },
        )
        .unwrap();

        let agent_id = crate::node::agent_id(&created.task.stream_id, Seat::Orchestrator);
        let _cancel_flag = crate::state::start_agent_activity(
            &state,
            &created.task.stream_id,
            &agent_id,
            "turn_1",
        );

        let cancelled = task_cancel_as(
            &state,
            TaskCancelReq {
                project_id: project_id.clone(),
                task_id: created.task.task_id.clone(),
                reason: None,
            },
            "human",
        )
        .expect("cancel");
        assert!(
            cancelled.turn_cancel_requested,
            "task_cancel_as must reach the seat a task socket's turns actually run under (Orchestrator), \
             not a hard-coded Worker seat nothing runs on"
        );
    }

    #[test]
    fn create_is_idempotent_per_work_order_and_slug() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let project_id = chat_only_project(&state);

        let first = task_create(&state, base_req(&project_id, "Ship the feature", "wo-a")).unwrap();
        assert!(first.created);
        let second =
            task_create(&state, base_req(&project_id, "Ship the feature", "wo-a")).unwrap();
        assert!(!second.created);
        assert_eq!(first.task.task_id, second.task.task_id);

        let listed = task_list(&state, &project_id);
        assert_eq!(
            listed
                .iter()
                .filter(|t| t.title == "Ship the feature")
                .count(),
            1,
            "must never mint a second task for the same (work_order, slug)"
        );
    }

    #[test]
    fn create_refuses_a_dependency_cycle() {
        // Pure graph check, independent of the id-minting order that makes
        // a real cycle unreachable through the HTTP/inner-fn path itself —
        // see `depends_on_has_cycle`'s doc comment.
        let mut edges: HashMap<String, Vec<String>> = HashMap::new();
        edges.insert("A".to_string(), vec!["B".to_string()]);
        edges.insert("B".to_string(), vec!["C".to_string()]);
        edges.insert("C".to_string(), vec!["A".to_string()]);
        assert!(
            depends_on_has_cycle(&edges, "A"),
            "A -> B -> C -> A must be detected as a cycle"
        );

        let mut acyclic: HashMap<String, Vec<String>> = HashMap::new();
        acyclic.insert("A".to_string(), vec!["B".to_string()]);
        acyclic.insert("B".to_string(), vec![]);
        assert!(
            !depends_on_has_cycle(&acyclic, "A"),
            "A -> B with no path back must not be flagged"
        );
    }

    #[test]
    fn create_refuses_when_a_depends_on_id_is_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let project_id = chat_only_project(&state);

        let mut req = base_req(&project_id, "Second task", "wo-b");
        req.contract.depends_on = vec!["task_does_not_exist".into()];
        let err = task_create(&state, req).unwrap_err();
        assert!(err.contains("unknown task"), "got: {err}");
    }

    #[test]
    fn create_records_an_overlap_warning_for_non_dependent_tasks_sharing_owned_files() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let project_id = chat_only_project(&state);

        let mut req_a = base_req(&project_id, "Task A", "wo-c");
        req_a.contract.owned_files = vec!["src/foo/**".into()];
        task_create(&state, req_a).unwrap();

        let mut req_b = base_req(&project_id, "Task B", "wo-c");
        req_b.contract.owned_files = vec!["src/foo/bar.rs".into()];
        let created_b = task_create(&state, req_b).unwrap();
        assert_eq!(
            created_b.task.warnings.len(),
            1,
            "non-dependent overlapping owned_files must warn exactly once"
        );
        assert!(created_b.task.warnings[0].contains("overlap"));
    }

    #[test]
    fn create_does_not_warn_when_the_overlapping_task_is_a_dependency() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let project_id = chat_only_project(&state);

        let mut req_a = base_req(&project_id, "Task A", "wo-d");
        req_a.contract.owned_files = vec!["src/foo/**".into()];
        let a = task_create(&state, req_a).unwrap();

        let mut req_b = base_req(&project_id, "Task B", "wo-d");
        req_b.contract.owned_files = vec!["src/foo/bar.rs".into()];
        req_b.contract.depends_on = vec![a.task.task_id];
        let created_b = task_create(&state, req_b).unwrap();
        assert!(
            created_b.task.warnings.is_empty(),
            "a dependent task's overlap with its own dependency must not warn"
        );
    }

    #[test]
    fn start_refuses_while_a_dependency_is_not_yet_done() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let project_id = chat_only_project(&state);

        let a = task_create(&state, base_req(&project_id, "Predecessor", "wo-e")).unwrap();
        let mut req_b = base_req(&project_id, "Successor", "wo-e");
        req_b.contract.depends_on = vec![a.task.task_id.clone()];
        let b = task_create(&state, req_b).unwrap();

        let err = task_start(
            &state,
            TaskStartReq {
                project_id: project_id.clone(),
                task_id: b.task.task_id.clone(),
            },
        )
        .unwrap_err();
        assert!(err.contains("stays queued"), "got: {err}");

        let listed = task_list(&state, &project_id);
        let row = listed.iter().find(|t| t.task_id == b.task.task_id).unwrap();
        assert_eq!(
            row.status,
            TaskStatus::Queued,
            "a gated task must stay queued, not silently start"
        );
    }

    #[test]
    fn chat_only_task_skips_materialization_and_goes_straight_to_running() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let project_id = chat_only_project(&state);

        let created =
            task_create(&state, base_req(&project_id, "Research the thing", "wo-f")).unwrap();
        let started = task_start(
            &state,
            TaskStartReq {
                project_id: project_id.clone(),
                task_id: created.task.task_id,
            },
        )
        .unwrap();
        assert!(!started.materialized);
        assert!(started.worktree_path.is_none());
        assert_eq!(started.task.status, TaskStatus::Running);
    }

    #[tokio::test]
    async fn studio_projection_buckets_a_task_socket_under_its_parent_project_and_states_task() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let project_id = chat_only_project(&state);
        let created = task_create(&state, base_req(&project_id, "Studio row", "wo-g")).unwrap();

        let studio_state = crate::routes::studio::studio_state(axum::extract::State(state.clone()))
            .await
            .unwrap()
            .0;
        let group = studio_state
            .projects
            .iter()
            .find(|p| p.project_id == project_id)
            .expect("project group present");
        let row = group
            .sockets
            .iter()
            .find(|s| s.stream_id == created.task.stream_id)
            .expect("task socket bucketed under its project");
        assert_eq!(row.state, "task");
    }

    // ─── Regression: per-task worktree/branch isolation ──────────────────

    #[test]
    fn two_tasks_sharing_a_title_get_isolated_worktrees_that_coexist_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let project_id = project_with_git_root(&state, dir.path());

        let a = task_create(&state, base_req(&project_id, "Fix login", "wo-1")).unwrap();
        assert!(a.created);
        let b = task_create(&state, base_req(&project_id, "Fix login", "wo-2")).unwrap();
        assert!(
            b.created,
            "two distinct work orders naming the same title are two distinct tasks"
        );
        assert_ne!(a.task.task_id, b.task.task_id);

        let started_a = task_start(
            &state,
            TaskStartReq {
                project_id: project_id.clone(),
                task_id: a.task.task_id.clone(),
            },
        )
        .unwrap();
        let started_b = task_start(
            &state,
            TaskStartReq {
                project_id: project_id.clone(),
                task_id: b.task.task_id.clone(),
            },
        )
        .unwrap();

        let wt_a = started_a.worktree_path.unwrap();
        let wt_b = started_b.worktree_path.unwrap();
        assert_ne!(
            wt_a, wt_b,
            "two live tasks must never be handed the same worktree path"
        );
        assert_ne!(
            started_a.branch, started_b.branch,
            "two live tasks must never share a branch"
        );
        assert!(
            std::path::Path::new(&wt_a).is_dir(),
            "task A's worktree must exist"
        );
        assert!(
            std::path::Path::new(&wt_b).is_dir(),
            "task B's worktree must coexist alongside task A's"
        );
    }

    #[test]
    fn start_refuses_to_adopt_a_worktree_that_resolves_into_a_different_repository() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let project_id = project_with_git_root(&state, dir.path());

        let created = task_create(&state, base_req(&project_id, "Fix login", "wo-1")).unwrap();
        let task_id = created.task.task_id.clone();
        let id_suffix = task_id
            .strip_prefix("task_")
            .unwrap_or(&task_id)
            .to_string();
        let branch = format!("task/{}-{id_suffix}", created.task.slug);
        let wt_path = state
            .worktree_base
            .join(format!("task-{}-{id_suffix}", created.task.slug));

        // A completely unrelated repository already occupies the exact path
        // (and, by coincidence, the exact branch name) this task would use.
        let other_repo = make_repo(&dir.path().join("other-repo"));
        run_git(
            &other_repo,
            &["worktree", "add", wt_path.to_str().unwrap(), "-b", &branch],
        );
        assert!(wt_path.is_dir(), "sanity: the foreign worktree exists");

        let err = task_start(
            &state,
            TaskStartReq {
                project_id: project_id.clone(),
                task_id: task_id.clone(),
            },
        )
        .unwrap_err();
        assert!(err.contains("is not a"), "got: {err}");

        let row = task_list(&state, &project_id)
            .into_iter()
            .find(|t| t.task_id == task_id)
            .unwrap();
        assert_eq!(
            row.status,
            TaskStatus::Queued,
            "a refused cross-repo adoption must not flip the task to running"
        );
    }

    #[test]
    fn start_refuses_when_the_intended_branch_name_already_exists() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let project_id = project_with_git_root(&state, dir.path());
        let repo = dir.path().join("repo");

        let created = task_create(&state, base_req(&project_id, "Fix login", "wo-1")).unwrap();
        let task_id = created.task.task_id.clone();
        let id_suffix = task_id
            .strip_prefix("task_")
            .unwrap_or(&task_id)
            .to_string();
        let branch = format!("task/{}-{id_suffix}", created.task.slug);

        // A branch with this exact name already exists locally (e.g. left
        // behind after a worktree — but not its branch — was removed).
        run_git(&repo, &["branch", &branch]);

        let err = task_start(
            &state,
            TaskStartReq {
                project_id: project_id.clone(),
                task_id: task_id.clone(),
            },
        )
        .unwrap_err();
        assert!(err.contains("already exists"), "got: {err}");
    }

    #[tokio::test]
    async fn retire_never_force_deletes_an_adopted_worktree_bakery_did_not_create() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let project_id = project_with_git_root(&state, dir.path());
        let repo = dir.path().join("repo");

        let created = task_create(&state, base_req(&project_id, "Fix login", "wo-1")).unwrap();
        let task_id = created.task.task_id.clone();
        let id_suffix = task_id
            .strip_prefix("task_")
            .unwrap_or(&task_id)
            .to_string();
        let branch = format!("task/{}-{id_suffix}", created.task.slug);
        let wt_path = state
            .worktree_base
            .join(format!("task-{}-{id_suffix}", created.task.slug));

        // A human (or a crashed earlier attempt) already created this exact
        // worktree/branch by hand and left uncommitted work in it.
        run_git(
            &repo,
            &["worktree", "add", wt_path.to_str().unwrap(), "-b", &branch],
        );
        std::fs::write(wt_path.join("precious.txt"), "do not delete").unwrap();

        let started = task_start(
            &state,
            TaskStartReq {
                project_id: project_id.clone(),
                task_id: task_id.clone(),
            },
        )
        .unwrap();
        assert!(
            started.materialized,
            "an adopted directory still counts as materialized"
        );

        let ctx = crate::state::context_for(&state, &started.task.stream_id).unwrap();
        assert!(
            !ctx.created_by_bakery,
            "an adopted worktree must never be recorded as Bakery-created"
        );

        let retired = task_retire(
            &state,
            TaskRetireReq {
                project_id: project_id.clone(),
                task_id: task_id.clone(),
            },
        )
        .await
        .unwrap();
        assert!(
            !retired.discard.worktree_removed,
            "a worktree Bakery cannot prove it created must never be force-removed"
        );
        assert!(
            wt_path.join("precious.txt").exists(),
            "the human's uncommitted file must survive retire"
        );
    }

    #[test]
    fn a_freshly_materialized_task_worktree_is_recorded_as_bakery_created() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let project_id = project_with_git_root(&state, dir.path());

        let created = task_create(&state, base_req(&project_id, "Fresh", "wo-k")).unwrap();
        let started = task_start(
            &state,
            TaskStartReq {
                project_id: project_id.clone(),
                task_id: created.task.task_id.clone(),
            },
        )
        .unwrap();

        let ctx = crate::state::context_for(&state, &started.task.stream_id).unwrap();
        assert!(ctx.created_by_bakery, "a worktree task_start actually ran `git worktree add` for must be provably Bakery-owned");

        // Rule 2's fork note lands on the task's own thread, as a thread's does.
        let heart = state.heart.lock().unwrap();
        let set = heart.last_of_family(&started.task.stream_id, SOCKET_WORKSPACE_FAMILY).unwrap().unwrap();
        let set: WorkspaceSet = serde_json::from_str(&set.payload).unwrap();
        assert_eq!(set.fork_note.as_deref(), Some("the repository has no remote"));
    }

    // ─── Regression: terminal `retired` visibility ────────────────────────

    #[tokio::test]
    async fn retired_task_reports_retired_true_in_list_and_in_its_own_retire_response() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let project_id = chat_only_project(&state);

        let created = task_create(&state, base_req(&project_id, "Throwaway", "wo-h")).unwrap();
        assert!(!created.task.retired);
        let task_id = created.task.task_id.clone();

        let retired = task_retire(
            &state,
            TaskRetireReq {
                project_id: project_id.clone(),
                task_id: task_id.clone(),
            },
        )
        .await
        .unwrap();
        assert!(
            retired.task.retired,
            "task_retire's own response must mark the task retired"
        );

        let listed = task_list(&state, &project_id);
        let row = listed.iter().find(|t| t.task_id == task_id).unwrap();
        assert!(
            row.retired,
            "GET /task/list must surface the terminal retired flag"
        );
    }

    // ─── Regression: idempotency must not collapse distinct titles ───────

    #[test]
    fn create_refuses_a_same_slug_match_whose_untruncated_title_actually_differs() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let project_id = chat_only_project(&state);

        let long_a = "Wire the Baker plan approval flow into the studio sidebar";
        let long_b = "Wire the Baker plan approval flow into the studio topbar";
        assert_eq!(
            task_slug(long_a),
            task_slug(long_b),
            "sanity: both titles clip to the same 48-char slug"
        );

        let a = task_create(&state, base_req(&project_id, long_a, "wo-i")).unwrap();
        assert!(a.created);

        let err = task_create(&state, base_req(&project_id, long_b, "wo-i")).unwrap_err();
        assert!(err.contains("collides"), "got: {err}");

        let listed = task_list(&state, &project_id);
        assert_eq!(
            listed.len(),
            1,
            "the second, genuinely distinct task must not have been silently minted as the first"
        );
    }

    // ─── Regression: task.started must not assert an unassigned seat ─────

    #[test]
    fn task_started_event_does_not_claim_a_seat_or_instruction_that_never_happened() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let project_id = chat_only_project(&state);
        let created = task_create(&state, base_req(&project_id, "Research", "wo-j")).unwrap();
        task_start(
            &state,
            TaskStartReq {
                project_id: project_id.clone(),
                task_id: created.task.task_id.clone(),
            },
        )
        .unwrap();

        let events = {
            state
                .heart
                .lock()
                .unwrap()
                .read_stream(&project_id)
                .unwrap()
        };
        let started_ev = events
            .iter()
            .find(|e| e.family == TASK_STARTED_FAMILY)
            .expect("task.started must have been appended");
        let payload: serde_json::Value = serde_json::from_str(&started_ev.payload).unwrap();
        assert!(
            payload.get("seat").is_none(),
            "task.started must not claim a seat was assigned when none was"
        );
        assert!(
            payload.get("instruction").is_none(),
            "task.started must not claim an instruction was issued when none was"
        );
    }
}
