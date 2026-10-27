//! Jobs: a long command an agent starts and leaves running — a build, a
//! test run, a dev server (step 1's replacement for `oprun.sh`).
//!
//! A job runs where a conversation's `run` would, under the same agent
//! profile (`bakery_exec::agent_room_command`: the room, the credential
//! fence, the operator's grants, and the workspace writable by the same rule,
//! `tools::commands_write_workspace`), but as a studio terminal: it shows
//! live in a pane, its output can be read from any offset, and it can be
//! stopped. When it ends on its own, the conversation it ran in has a Heart
//! fact, and the conversation that started it hears so in a queued message
//! the way an access grant continues a conversation
//! (`machine_access::continue_after_grant`): the Baker's check in a chef's
//! worktree reports to the Baker, not to the chef.

use crate::node::Seat;
use crate::state::{append, context_for, AppState};
use crate::terminals::TerminalHandle;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

pub(crate) const JOB_MODE: &str = "job";
pub(crate) const JOB_ENDED: &str = "agent.job.ended";
/// The most of a job's output one `job_output` call hands back.
const MAX_OUTPUT_BYTES: usize = 32 * 1024;
const TAIL_LINES: usize = 20;
/// Finished jobs kept for reading back; the oldest finished go first.
const KEPT_JOBS: usize = 100;

struct Job {
    id: String,
    /// The conversation whose sandbox runs it.
    stream_id: String,
    /// The conversation that started it, which hears when it ends: the
    /// agent that called job_start, else `stream_id`.
    starter: String,
    command: String,
    cwd: String,
    started_at: String,
    terminal: Arc<TerminalHandle>,
    stopped: AtomicBool,
    /// A proof job (`baker_threads::prove`): its end is recorded, but its
    /// conversation is not woken, or proving would start another batch.
    quiet: bool,
    /// Held from before the child starts until the job is reaped, for one
    /// `cargo test --workspace` at a time. Empty for every other command.
    workspace_test: Mutex<Option<WorkspacePermit>>,
}

/// One full workspace test in the process. Taken before that child starts,
/// dropped when [`ended`] reaps the job.
struct WorkspacePermit;

fn workspace_gate() -> &'static (Mutex<bool>, Condvar) {
    static GATE: OnceLock<(Mutex<bool>, Condvar)> = OnceLock::new();
    GATE.get_or_init(|| (Mutex::new(true), Condvar::new()))
}

impl WorkspacePermit {
    fn acquire() -> Self {
        let (free, wake) = workspace_gate();
        let mut open = free.lock().unwrap_or_else(|e| e.into_inner());
        while !*open {
            open = wake.wait(open).unwrap_or_else(|e| e.into_inner());
        }
        *open = false;
        WorkspacePermit
    }
}

impl Drop for WorkspacePermit {
    fn drop(&mut self) {
        let (free, wake) = workspace_gate();
        let mut open = free.lock().unwrap_or_else(|e| e.into_inner());
        *open = true;
        wake.notify_one();
    }
}

/// A command that runs cargo's whole workspace tests takes the one permit.
fn take_workspace_permit(command: &str) -> Option<WorkspacePermit> {
    if command.contains("cargo") && command.contains("test") && command.contains("--workspace") {
        Some(WorkspacePermit::acquire())
    } else {
        None
    }
}

// ponytail: one registry per process, in memory; a node restart forgets
// finished jobs (their end facts stay in Heart) and kills running ones with
// their terminals.
fn jobs() -> &'static Mutex<HashMap<String, Arc<Job>>> {
    static JOBS: OnceLock<Mutex<HashMap<String, Arc<Job>>>> = OnceLock::new();
    JOBS.get_or_init(Default::default)
}

fn find(id: &str) -> Result<Arc<Job>, String> {
    jobs()
        .lock()
        .unwrap()
        .get(id)
        .cloned()
        .ok_or_else(|| format!("no job {id:?}"))
}

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct JobStartReq {
    /// The conversation whose sandbox runs it. The conversation that starts
    /// it hears when it ends: yours, when you call this.
    pub(crate) stream_id: String,
    /// A shell command line, run with /bin/sh -c.
    pub(crate) command: String,
    /// Where it starts, inside that conversation's workspace (default: its
    /// root).
    #[serde(default)]
    pub(crate) cwd: Option<String>,
}

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct JobOutputQuery {
    pub(crate) id: String,
    /// The offset to read from: 0, or the `next` of the last call.
    #[serde(default)]
    pub(crate) since: Option<u64>,
}

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct JobStopReq {
    pub(crate) id: String,
}

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct JobListQuery {
    /// Only this conversation's jobs.
    #[serde(default)]
    pub(crate) stream_id: Option<String>,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct JobView {
    pub(crate) id: String,
    pub(crate) stream_id: String,
    pub(crate) command: String,
    pub(crate) cwd: String,
    /// The studio terminal showing it.
    pub(crate) terminal_id: String,
    pub(crate) started_at: String,
    /// running, exited or stopped.
    pub(crate) state: &'static str,
    pub(crate) exit_code: Option<u32>,
}

impl Job {
    fn view(&self) -> JobView {
        JobView {
            id: self.id.clone(),
            stream_id: self.stream_id.clone(),
            command: self.command.clone(),
            cwd: self.cwd.clone(),
            terminal_id: self.terminal.terminal_id.clone(),
            started_at: self.started_at.clone(),
            state: match (self.terminal.is_alive(), self.stopped.load(Ordering::Acquire)) {
                (true, _) => "running",
                (false, true) => "stopped",
                (false, false) => "exited",
            },
            exit_code: self.terminal.exit_code(),
        }
    }
}

/// Terminal output as plain text: escape sequences and carriage returns
/// dropped.
fn plain(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => match chars.next() {
                // CSI: up to a final byte in @..=~.
                Some('[') => {
                    for c in chars.by_ref() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                }
                // OSC: up to BEL or ESC \.
                Some(']') => {
                    while let Some(c) = chars.next() {
                        if c == '\u{7}' || (c == '\u{1b}' && chars.next_if_eq(&'\\').is_some()) {
                            break;
                        }
                    }
                }
                _ => {}
            },
            '\r' => {}
            c => out.push(c),
        }
    }
    out
}

fn tail(job: &Job) -> String {
    let (bytes, _, _) = job.terminal.replay_since(0);
    let text = plain(&String::from_utf8_lossy(&bytes));
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(TAIL_LINES)..].join("\n")
}

/// Starts `command` in `stream_id`'s sandbox and returns at once.
pub(crate) fn start_job(state: &AppState, req: &JobStartReq) -> Result<JobView, String> {
    start(state, req, false)
}

/// A Baker thread's acceptance check, as a quiet job.
pub(crate) fn start_proof(state: &AppState, req: &JobStartReq) -> Result<JobView, String> {
    start(state, req, true)
}

fn start(state: &AppState, req: &JobStartReq, quiet: bool) -> Result<JobView, String> {
    let command = req.command.trim();
    if command.is_empty() {
        return Err("job_start needs a command".to_string());
    }
    let ctx = context_for(state, &req.stream_id)?;
    if ctx.worktree_path.trim().is_empty() {
        return Err("this conversation has no workspace to run a job in".to_string());
    }
    let workspace = std::fs::canonicalize(&ctx.worktree_path).map_err(|e| e.to_string())?;
    let cwd = match req.cwd.as_deref().map(str::trim).filter(|c| !c.is_empty()) {
        Some(cwd) => std::fs::canonicalize(workspace.join(cwd)).map_err(|e| format!("{cwd}: {e}"))?,
        None => workspace.clone(),
    };
    if !cwd.starts_with(&workspace) {
        return Err(format!("{} is outside this conversation's workspace", cwd.display()));
    }

    let toolchain = crate::catalog::stacks_for_socket(state, &req.stream_id).toolchain;
    let env = bakery_exec::room_for_run(&workspace, &crate::state::cache_root(state), &req.stream_id, &toolchain)?;
    let granted = crate::machine_access::for_stream(state, &req.stream_id);
    let write_workspace = crate::tools::commands_write_workspace(state, &req.stream_id);
    // A linked worktree's git dirs, so git works in it, as `run` grants.
    let (mut read, mut write) = match crate::git::linked_worktree_git_dirs(&workspace) {
        Some((shared, own)) if write_workspace => (vec![shared], vec![own]),
        _ => (Vec::new(), Vec::new()),
    };
    read.extend(granted.read.iter().cloned());
    write.extend(granted.write.iter().cloned());
    let argv = vec!["/bin/sh".to_string(), "-c".to_string(), command.to_string()];
    let shell = bakery_exec::agent_room_command(
        &env,
        &workspace,
        &bakery_exec::JobRun {
            argv: &argv,
            write_workspace,
            grants: bakery_exec::ExtraGrants {
                read: &read,
                write: &write,
                exec: &granted.exec,
                deny: &granted.deny,
                whole_machine: granted.whole_machine.as_ref(),
                path: &granted.path,
                env: &granted.env,
            },
        },
    )?;

    // Before the child starts. Dropped here if the spawn fails, and by
    // `ended` once the job is reaped.
    let workspace_test = take_workspace_permit(command);
    let pending = crate::state::admit_pty(state, &req.stream_id, JOB_MODE).map_err(|(_, e)| e)?;
    let name: String = command.chars().take(40).collect();
    let info = crate::terminals::spawn_terminal(
        state,
        crate::terminals::SpawnSpec {
            stream_id: req.stream_id.clone(),
            name: Some(format!("{} · {name}", if quiet { "proof" } else { "job" })),
            mode: JOB_MODE.to_string(),
            program: shell.program,
            args: shell.args,
            cwd: cwd.clone(),
            profile_path: Some(shell.profile_path),
            login_command: None,
            env: None,
        },
        pending,
    )?;
    let terminal = state
        .terminals
        .lock()
        .unwrap()
        .get(&info.terminal_id)
        .ok_or("the job's terminal ended before it could be watched")?;
    // An agent's job reports to that agent's conversation, which is not
    // always the one whose sandbox runs it (the Baker's check in a chef's
    // worktree); the operator's, to the one it runs in.
    let starter = crate::messages::sender(&crate::actor::attribute("human")).unwrap_or_else(|| req.stream_id.clone());
    let job = Arc::new(Job {
        id: crate::node::mint_id("job"),
        stream_id: req.stream_id.clone(),
        starter,
        command: command.to_string(),
        cwd: cwd.to_string_lossy().into_owned(),
        started_at: chrono::Utc::now().to_rfc3339(),
        terminal,
        stopped: AtomicBool::new(false),
        quiet,
        workspace_test: Mutex::new(workspace_test),
    });
    {
        let mut all = jobs().lock().unwrap();
        if all.len() >= KEPT_JOBS {
            let oldest = all
                .values()
                .filter(|j| !j.terminal.is_alive())
                .min_by(|a, b| a.started_at.cmp(&b.started_at))
                .map(|j| j.id.clone());
            if let Some(oldest) = oldest {
                all.remove(&oldest);
            }
        }
        all.insert(job.id.clone(), job.clone());
    }
    let (state, watched) = (state.clone(), job.clone());
    tokio::spawn(async move {
        let mut alive = watched.terminal.watch_alive();
        while *alive.borrow_and_update() {
            if alive.changed().await.is_err() {
                break;
            }
        }
        let _ = crate::actor::spawn_blocking(move || ended(&state, &watched)).await;
    });
    Ok(job.view())
}

/// Records the end on the conversation it ran in, and tells the one that
/// started it, unless someone stopped it on purpose or it is a quiet proof
/// job.
fn ended(state: &AppState, job: &Job) {
    drop(job.workspace_test.lock().unwrap_or_else(|e| e.into_inner()).take());
    let stopped = job.stopped.load(Ordering::Acquire);
    let exit_code = job.terminal.exit_code();
    let tail = tail(job);
    let _ = append(
        state,
        &job.stream_id,
        "node",
        JOB_ENDED,
        serde_json::json!({
            "job_id": job.id,
            "command": job.command,
            "terminal_id": job.terminal.terminal_id,
            "exit_code": exit_code,
            "stopped": stopped,
            "tail": tail,
        })
        .to_string(),
        vec![],
    );
    if stopped || job.quiet {
        return;
    }
    let how = exit_code.map_or("ended".to_string(), |code| format!("exited {code}"));
    let text = format!(
        "Job {} {how}: `{}`.\nLast output:\n{tail}\nRead all of it with job_output (id {}).",
        job.id, job.command, job.id
    );
    // A Baker thread's message carries the model and effort it was opened
    // with; any other conversation gets a plain follow-up.
    let _ = if crate::baker_ops::is_baker_scope_stream(state, &job.starter) {
        crate::baker_threads::message_thread(state, &job.starter, &text, "bakery").map(drop)
    } else {
        crate::agent_control::enqueue_as(
            state,
            crate::routes::agent::AgentTurnReq {
                stream_id: job.starter.clone(),
                file_path: None,
                instruction: text,
                seat: Some(Seat::Orchestrator),
                model_id: None,
                effort: None,
                actor: None,
            },
            vec![],
            false,
            "bakery",
        )
        .map(drop)
    };
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct JobOutput {
    #[serde(flatten)]
    pub(crate) job: JobView,
    pub(crate) output: String,
    /// Pass as `since` to read on from here.
    pub(crate) next: u64,
    /// Output before `since` that the job no longer keeps.
    pub(crate) truncated: bool,
}

pub(crate) fn job_output(id: &str, since: u64) -> Result<JobOutput, String> {
    let job = find(id)?;
    let (bytes, truncated, end) = job.terminal.replay_since(since);
    let start = end - bytes.len() as u64;
    let taken = &bytes[..bytes.len().min(MAX_OUTPUT_BYTES)];
    Ok(JobOutput {
        job: job.view(),
        output: plain(&String::from_utf8_lossy(taken)),
        next: start + taken.len() as u64,
        truncated,
    })
}

pub(crate) async fn stop_job(state: &AppState, id: &str) -> Result<JobView, String> {
    let job = find(id)?;
    if job.terminal.is_alive() {
        job.stopped.store(true, Ordering::Release);
        // A job that ended on its own meanwhile has nothing left to kill.
        let _ = crate::terminals::kill_terminal(state, &job.terminal.terminal_id).await;
    }
    Ok(job.view())
}

pub(crate) fn list_jobs(stream_id: Option<&str>) -> Vec<JobView> {
    let mut views: Vec<JobView> = jobs()
        .lock()
        .unwrap()
        .values()
        .filter(|j| stream_id.is_none_or(|s| j.stream_id == s))
        .map(|j| j.view())
        .collect();
    views.sort_by(|a, b| b.started_at.cmp(&a.started_at));
    views
}

fn bad(e: String) -> (StatusCode, String) {
    (StatusCode::BAD_REQUEST, e)
}

pub(crate) async fn job_start_route(
    State(st): State<AppState>,
    Json(req): Json<JobStartReq>,
) -> Result<Json<JobView>, (StatusCode, String)> {
    crate::actor::spawn_blocking(move || start_job(&st, &req))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map(Json)
        .map_err(bad)
}

pub(crate) async fn job_output_route(
    Query(q): Query<JobOutputQuery>,
) -> Result<Json<JobOutput>, (StatusCode, String)> {
    job_output(&q.id, q.since.unwrap_or(0)).map(Json).map_err(bad)
}

pub(crate) async fn job_stop_route(
    State(st): State<AppState>,
    Json(req): Json<JobStopReq>,
) -> Result<Json<JobView>, (StatusCode, String)> {
    stop_job(&st, &req.id).await.map(Json).map_err(bad)
}

pub(crate) async fn job_list_route(Query(q): Query<JobListQuery>) -> Json<Vec<JobView>> {
    Json(list_jobs(q.stream_id.as_deref()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::project::open_stream;
    use crate::testutil::*;
    use bakery_agent::StubModel;
    use std::time::{Duration, Instant};

    fn until(what: &str, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !done() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    #[test]
    fn plain_drops_terminal_escapes() {
        assert_eq!(plain("\u{1b}[31mFAILED\u{1b}[0m\r\nok\u{1b}]0;title\u{7}"), "FAILED\nok");
    }

    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn a_job_runs_in_the_agent_sandbox_streams_its_output_and_wakes_its_conversation() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = open_stream(&state, &open_req(&repo, "jobs")).unwrap().context.stream_id;
        let rt = tokio::runtime::Runtime::new().unwrap();
        let _ctx = rt.enter();

        // A planted, fake credential: the agent profile's fence keeps it
        // unreadable, where the operator's own room terminal could read it.
        let workspace = context_for(&state, &stream_id).unwrap().worktree_path;
        std::fs::write(std::path::Path::new(&workspace).join(".env"), "PLANTED=1\n").unwrap();
        let job = start_job(
            &state,
            &JobStartReq {
                stream_id: stream_id.clone(),
                command: "echo built; cat .env >/dev/null 2>&1 && echo leaked || echo fenced; exit 3".into(),
                cwd: None,
            },
        )
        .unwrap();
        let panes = state.terminals.lock().unwrap().list_for_stream(&stream_id);
        assert!(
            panes.iter().any(|t| t.terminal_id == job.terminal_id && t.mode == JOB_MODE),
            "a studio pane shows it"
        );

        let events = || state.heart.lock().unwrap().read_stream(&stream_id).unwrap();
        until("the end fact", || events().iter().any(|e| e.family == JOB_ENDED));
        let out = job_output(&job.id, 0).unwrap();
        assert_eq!((out.job.state, out.job.exit_code), ("exited", Some(3)), "{}", out.output);
        assert!(out.output.contains("built") && out.output.contains("fenced"), "{}", out.output);
        assert_eq!(job_output(&job.id, out.next).unwrap().output, "", "nothing new after next");

        let ended = events().into_iter().find(|e| e.family == JOB_ENDED).unwrap();
        let ended: serde_json::Value = serde_json::from_str(&ended.payload).unwrap();
        assert_eq!(ended["exit_code"], 3);
        assert!(ended["tail"].as_str().unwrap().contains("built"));
        until("the wake-up", || {
            events()
                .iter()
                .any(|e| e.family == "agent.follow_up.queued" && e.payload.contains(&job.id))
        });
        assert!(list_jobs(Some(&stream_id)).iter().any(|j| j.id == job.id));
    }

    /// A check the permanent Baker runs in a chef's worktree reports to the
    /// Baker that started it, not to the chef whose sandbox ran it; the end
    /// is still recorded on the chef's own thread.
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn a_job_reports_its_end_to_the_conversation_that_started_it_not_the_one_it_ran_in() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("chef");
        std::fs::create_dir_all(&folder).unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let home = crate::routes::baker::ensure_permanent_baker(&state, None, None).unwrap().stream_id;
        let root = crate::catalog::DirectoryRootSpec::Attach(folder.to_str().unwrap());
        let chef = crate::catalog::create_project(&state, "chef", Some(root)).unwrap().1.stream_id;
        let rt = tokio::runtime::Runtime::new().unwrap();
        let _ctx = rt.enter();

        let job = crate::actor::with(crate::actor::agent(&home, Seat::Baker), || {
            start_job(&state, &JobStartReq { stream_id: chef.clone(), command: "echo checked; exit 2".into(), cwd: None })
        })
        .unwrap();
        assert_eq!(job.stream_id, chef, "it runs in the chef's sandbox");
        let events = |stream: &str| state.heart.lock().unwrap().read_stream(stream).unwrap();
        until("the end fact on the chef's thread", || events(&chef).iter().any(|e| e.family == JOB_ENDED));
        until("the Baker's wake-up", || {
            events(&home).iter().any(|e| e.family == "agent.follow_up.queued" && e.payload.contains(&job.id))
        });
        std::thread::sleep(Duration::from_millis(300));
        assert!(
            !events(&chef).iter().any(|e| e.family == "agent.follow_up.queued"),
            "the chef whose sandbox ran it is not woken"
        );
    }

    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn a_stopped_job_ends_without_waking_anyone_and_cwd_stays_inside_the_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = open_stream(&state, &open_req(&repo, "jobstop"))
            .unwrap()
            .context
            .stream_id;
        let rt = tokio::runtime::Runtime::new().unwrap();
        let _ctx = rt.enter();
        let start = |command: &str, cwd: Option<&str>| {
            start_job(
                &state,
                &JobStartReq {
                    stream_id: stream_id.clone(),
                    command: command.into(),
                    cwd: cwd.map(str::to_string),
                },
            )
        };
        let outside = start("true", Some("..")).unwrap_err();
        assert!(outside.contains("outside"), "{outside}");

        let job = start("echo started; sleep 30", None).unwrap();
        until("its first line", || job_output(&job.id, 0).unwrap().output.contains("started"));
        let stopped = rt.block_on(stop_job(&state, &job.id)).unwrap();
        assert_eq!(stopped.state, "stopped");
        let events = || state.heart.lock().unwrap().read_stream(&stream_id).unwrap();
        until("the end fact", || events().iter().any(|e| e.family == JOB_ENDED));
        std::thread::sleep(Duration::from_millis(300));
        assert!(
            !events().iter().any(|e| e.family == "agent.follow_up.queued"),
            "a stop on purpose wakes no one"
        );
    }
}
