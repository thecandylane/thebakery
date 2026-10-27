//! Who a Heart fact is attributed to when an agent calls a route.
//!
//! Route handlers record `"human"`: until the Baker could call them, only
//! the operator's UI did. A Baker tool call runs the same handler inside
//! [`scope`], and [`attribute`] turns that `"human"` into the calling
//! agent's actor. Handlers hand their work to blocking threads through
//! [`spawn_blocking`], which carries the scope along.

use std::cell::RefCell;
use std::future::Future;

tokio::task_local! {
    static ACTOR: String;
}

thread_local! {
    static THREAD_ACTOR: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// The actor an agent's call is recorded as, wherever its facts land:
/// `agent:<its thread>:<seat>`, so History names the thread that acted.
/// What an agent's turn records on its own thread says `agent:<seat>`: the
/// thread is the one the fact is on.
pub(crate) fn agent(stream_id: &str, seat: crate::node::Seat) -> String {
    format!("agent:{}", crate::node::agent_id(stream_id, seat))
}

/// Runs `fut` with its `"human"` facts attributed to `actor`.
pub(crate) async fn scope<F: Future>(actor: String, fut: F) -> F::Output {
    ACTOR.scope(actor, fut).await
}

fn current() -> Option<String> {
    ACTOR
        .try_with(Clone::clone)
        .ok()
        .or_else(|| THREAD_ACTOR.with(|a| a.borrow().clone()))
}

/// The actor to record: `actor` itself, unless it is `"human"` inside an
/// agent's call [`scope`].
pub(crate) fn attribute(actor: &str) -> String {
    match current() {
        Some(scoped) if actor == "human" => scoped,
        _ => actor.to_string(),
    }
}

/// An agent is making this call: not the operator, in the app or on one of
/// their devices.
pub(crate) fn agent_calling() -> bool {
    attribute("human").starts_with("agent:")
}

/// Restores the thread's previous actor when the closure ends, even by
/// panic: blocking-pool threads are reused, and a stale actor would
/// misattribute the next, unrelated job.
struct Restore(Option<String>);

impl Drop for Restore {
    fn drop(&mut self) {
        THREAD_ACTOR.with(|a| *a.borrow_mut() = self.0.take());
    }
}

/// Runs `f` on this thread with its `"human"` facts attributed to `actor`:
/// the sync counterpart of [`scope`], for an op that calls the inner
/// functions a route would.
pub(crate) fn with<R>(actor: String, f: impl FnOnce() -> R) -> R {
    let _restore = Restore(THREAD_ACTOR.with(|a| a.replace(Some(actor))));
    f()
}

/// `tokio::task::spawn_blocking` that keeps the caller's actor scope.
pub(crate) fn spawn_blocking<F, R>(f: F) -> tokio::task::JoinHandle<R>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    let actor = current();
    tokio::task::spawn_blocking(move || {
        let _restore = Restore(THREAD_ACTOR.with(|a| a.replace(actor)));
        f()
    })
}

/// Who calls a route: the operator, or an agent's thread and its project.
pub(crate) struct Caller {
    /// The calling agent's thread; none for the operator.
    pub(crate) thread: Option<String>,
    pub(crate) project: Option<String>,
    /// The operator and the Baker's side reach everything.
    pub(crate) everywhere: bool,
    /// Reads the Baker's own notes: the Baker's side, and the Bakery's own
    /// workspaces (never a catalog project's), however deep a thread was
    /// opened under one. Reads only: what it writes keeps its own scope.
    pub(crate) baker_notes: bool,
    /// The operator's own private notes: theirs alone.
    pub(crate) private_notes: bool,
}

impl Caller {
    pub(crate) fn of(state: &crate::state::AppState) -> Caller {
        let actor = attribute("human");
        if actor == crate::outside_window::ACTOR {
            return Caller::outside_window();
        }
        match crate::messages::sender(&actor) {
            Some(thread) => Caller::of_thread(state, &thread),
            None => Caller::operator(),
        }
    }

    /// The operator, who reaches everything.
    pub(crate) fn operator() -> Caller {
        Caller { thread: None, project: None, everywhere: true, baker_notes: true, private_notes: true }
    }

    /// The outside window: what the Baker reads, on no thread of its own,
    /// so nothing it reads is logged anywhere.
    pub(crate) fn outside_window() -> Caller {
        Caller { thread: None, project: None, everywhere: true, baker_notes: true, private_notes: false }
    }

    /// The agent working on `thread`.
    pub(crate) fn of_thread(state: &crate::state::AppState, thread: &str) -> Caller {
        let (project, ours) = {
            let heart = state.heart.lock().unwrap();
            let project = crate::state::socket_project(&heart, thread).0;
            let ours = project.is_none() && crate::workspaces::in_bakery_workspace_locked(&heart, thread);
            (project, ours)
        };
        let everywhere = crate::baker_ops::is_baker_scope_stream(state, thread);
        Caller { thread: Some(thread.to_string()), project, everywhere, baker_notes: everywhere || ours, private_notes: false }
    }

    /// Whether it reaches what belongs to `project` (none: the Bakery's).
    pub(crate) fn reaches(&self, project: Option<&str>) -> bool {
        self.everywhere || project.is_none() || project == self.project.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scoped_call_attributes_human_facts_to_the_agent_even_on_blocking_threads() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let (inline, blocking, kept) = rt.block_on(scope("agent:baker".into(), async {
            let blocking = spawn_blocking(|| attribute("human")).await.unwrap();
            (attribute("human"), blocking, attribute("node"))
        }));
        assert_eq!(inline, "agent:baker");
        assert_eq!(blocking, "agent:baker");
        assert_eq!(kept, "node", "only \"human\" is the caller");
        // Outside any scope, and on a reused blocking thread, nothing leaks.
        let after = rt.block_on(async { spawn_blocking(|| attribute("human")).await.unwrap() });
        assert_eq!(after, "human");
        assert_eq!(attribute("human"), "human");
    }

    /// The operator on one of their devices is never taken for an agent.
    #[test]
    fn only_an_agents_call_is_an_agent_calling() {
        assert!(!agent_calling());
        assert!(with("agent:baker".into(), agent_calling));
        assert!(!with("device:phone".into(), agent_calling));
    }
}
