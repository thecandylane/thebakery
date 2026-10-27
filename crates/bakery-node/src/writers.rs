//! The concurrent-writer guard (F1-8), for threads that share one folder (on
//! the Bakery's own repo every writing thread has a worktree of its own). A
//! write that goes through the Bakery is refused when the file changed since
//! the thread's agents last read it, naming who changed it when the Bakery
//! knows; a file they never read goes through unless someone else wrote it
//! during their current turn. What lands outside the Bakery (shell
//! commands, a runtime writing directly) meets at the turn's end instead
//! (`snapshots`). A thread is the reader, not each agent in it: threads are
//! what share a folder. Kept in memory: after a restart every file counts as
//! never read.

use crate::state::AppState;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

/// Who wrote a file the Bakery saw written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Writer {
    /// A thread's agents, by the thread's stream.
    Thread(String),
    Operator,
}

#[derive(Debug, Clone)]
struct Wrote {
    by: Writer,
    at: Instant,
}

#[derive(Default)]
struct Book {
    /// What a thread last saw of a file, and when: its content's hash,
    /// `None` when it wasn't there. By (folder, file, thread).
    reads: HashMap<(PathBuf, String, String), (Option<String>, Instant)>,
    /// The last write the Bakery saw of each file, by (folder, file).
    writes: HashMap<(PathBuf, String), Wrote>,
    /// When each thread's current turn began.
    turns: HashMap<String, Instant>,
}

fn book() -> &'static Mutex<Book> {
    static BOOK: OnceLock<Mutex<Book>> = OnceLock::new();
    BOOK.get_or_init(Default::default)
}

fn folder(workspace: &Path) -> PathBuf {
    workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf())
}

/// A file's content, as the guard compares it; `None` when it isn't there.
fn content(path: &Path) -> Option<String> {
    std::fs::read(path)
        .ok()
        .map(|bytes| blake3::hash(&bytes).to_hex().to_string())
}

/// A thread's turn began.
pub(crate) fn turn_began(stream_id: &str) {
    let mut book = book().lock().unwrap();
    book.turns.insert(stream_id.to_string(), Instant::now());
}

/// A thread's agents read `rel`: they have seen it as it is now.
pub(crate) fn read(stream_id: &str, workspace: &Path, rel: &str) {
    let at = folder(workspace);
    let seen = content(&at.join(rel));
    let mut book = book().lock().unwrap();
    book.reads.insert(
        (at, rel.to_string(), stream_id.to_string()),
        (seen, Instant::now()),
    );
}

/// `by` wrote `rel`. A thread that wrote it knows it as it is now.
pub(crate) fn wrote(by: Writer, workspace: &Path, rel: &str) {
    let at = folder(workspace);
    let now = content(&at.join(rel));
    let mut book = book().lock().unwrap();
    if let Writer::Thread(stream) = &by {
        book.reads.insert(
            (at.clone(), rel.to_string(), stream.clone()),
            (now, Instant::now()),
        );
    }
    book.writes.insert(
        (at, rel.to_string()),
        Wrote {
            by,
            at: Instant::now(),
        },
    );
}

/// Whether a thread's agents may write `rel` now. Refused when it changed
/// since they last read it, or, never read, when someone else wrote it
/// during their current turn; the refusal says who, when the Bakery knows.
pub(crate) fn check(
    state: &AppState,
    stream_id: &str,
    workspace: &Path,
    rel: &str,
) -> Result<(), String> {
    let at = folder(workspace);
    let now = content(&at.join(rel));
    let (changed, by) = {
        let book = book().lock().unwrap();
        let last = book
            .writes
            .get(&(at.clone(), rel.to_string()))
            .filter(|w| w.by != Writer::Thread(stream_id.to_string()))
            .cloned();
        match book
            .reads
            .get(&(at, rel.to_string(), stream_id.to_string()))
        {
            // Named only when it wrote after the read.
            Some((seen, read_at)) => (*seen != now, last.filter(|w| w.at > *read_at)),
            None => {
                let began = book.turns.get(stream_id).copied();
                let during = last.filter(|w| began.is_some_and(|b| w.at > b));
                (during.is_some(), during)
            }
        }
    };
    if !changed {
        return Ok(());
    }
    let by = match by.map(|w| w.by) {
        Some(Writer::Operator) => " by the operator".to_string(),
        Some(Writer::Thread(other)) => format!(" by the agent in {}", thread(state, &other)),
        None => String::new(),
    };
    Err(format!(
        "{rel} changed since you last read it{by}: read it again, then make your change"
    ))
}

/// Who last wrote `rel`, in words for the operator; `None` when the Bakery
/// saw no write of it.
pub(crate) fn changed_by(state: &AppState, workspace: &Path, rel: &str) -> Option<String> {
    let at = folder(workspace);
    let last = book()
        .lock()
        .unwrap()
        .writes
        .get(&(at, rel.to_string()))
        .cloned()?;
    match last.by {
        Writer::Operator => Some("you".to_string()),
        Writer::Thread(stream) => Some(format!("the agent in {}", thread(state, &stream))),
    }
}

/// A thread, as a message names it: “its name”.
fn thread(state: &AppState, stream_id: &str) -> String {
    let name = crate::state::context_for(state, stream_id)
        .map(|c| c.name)
        .unwrap_or_else(|_| stream_id.to_string());
    format!("“{name}”")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn a_write_waits_for_a_fresh_read_and_says_who_changed_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let state = crate::testutil::state_with(dir.path(), Arc::new(bakery_agent::StubModel));
        let shared = tempfile::tempdir().unwrap();
        let ws = shared.path();
        std::fs::write(ws.join("f.txt"), "one\n").unwrap();
        let (a, b) = ("wt:guard-a", "wt:guard-b");

        // A reads; B writes; A's write is refused, naming B's thread.
        turn_began(a);
        read(a, ws, "f.txt");
        assert!(check(&state, a, ws, "f.txt").is_ok(), "nothing changed yet");
        std::fs::write(ws.join("f.txt"), "two\n").unwrap();
        wrote(Writer::Thread(b.to_string()), ws, "f.txt");
        let refused = check(&state, a, ws, "f.txt").unwrap_err();
        assert!(
            refused.contains("by the agent in “wt:guard-b”"),
            "{refused}"
        );
        assert!(
            check(&state, b, ws, "f.txt").is_ok(),
            "B wrote it: B knows it"
        );

        // A reads again: its write goes through; its own write keeps it fresh.
        read(a, ws, "f.txt");
        assert!(check(&state, a, ws, "f.txt").is_ok());
        std::fs::write(ws.join("f.txt"), "three\n").unwrap();
        wrote(Writer::Thread(a.to_string()), ws, "f.txt");
        assert!(check(&state, a, ws, "f.txt").is_ok());

        // The operator saves: named so; a change no one told the Bakery of
        // is refused without a name.
        std::fs::write(ws.join("f.txt"), "mine\n").unwrap();
        wrote(Writer::Operator, ws, "f.txt");
        assert!(check(&state, a, ws, "f.txt")
            .unwrap_err()
            .contains("by the operator"));
        read(a, ws, "f.txt");
        std::fs::write(ws.join("f.txt"), "a command's\n").unwrap();
        let unnamed = check(&state, a, ws, "f.txt").unwrap_err();
        assert!(
            unnamed.starts_with("f.txt changed since you last read it:"),
            "{unnamed}"
        );
    }

    /// Two threads working in one folder meet at the guard: a read through
    /// one's jail, a change landed for the other, and the first's write
    /// waits for a fresh read.
    #[test]
    fn two_threads_in_one_folder_meet_at_the_guard() {
        let (dir, work) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let state = crate::testutil::state_with(dir.path(), Arc::new(bakery_agent::StubModel));
        let shared = work.path().join("shared");
        std::fs::create_dir_all(&shared).unwrap();
        std::fs::write(shared.join("f.txt"), "one\n").unwrap();
        let root = || crate::catalog::DirectoryRootSpec::Attach(shared.to_str().unwrap());
        let (project, a) = crate::catalog::create_project(&state, "shared", Some(root())).unwrap();
        let b =
            crate::catalog::attach_directory_root(&state, &project.project_id, "Fix login", root())
                .unwrap();
        assert_eq!(a.worktree_path, b.worktree_path, "one folder, two threads");
        let jail = |stream: &str| {
            crate::tools::Jail::new(&shared).unwrap().with_exec(
                state.clone(),
                stream.to_string(),
                dir.path().join("cache"),
                "none",
            )
        };
        let read_args = serde_json::json!({ "path": "f.txt" });
        jail(&a.stream_id).call("read_file", &read_args).unwrap();
        crate::routes::agent::write_through_jail(
            &state,
            &b.stream_id,
            "f.txt",
            "two\n",
            Writer::Thread(b.stream_id.clone()),
        )
        .unwrap();
        let refused = jail(&a.stream_id).resolve_write("f.txt").unwrap_err();
        assert!(refused.contains("by the agent in “Fix login”"), "{refused}");
        jail(&a.stream_id).call("read_file", &read_args).unwrap();
        assert!(jail(&a.stream_id).resolve_write("f.txt").is_ok());
    }

    #[test]
    fn a_file_never_read_goes_through_unless_written_during_the_turn() {
        let dir = tempfile::tempdir().unwrap();
        let state = crate::testutil::state_with(dir.path(), Arc::new(bakery_agent::StubModel));
        let shared = tempfile::tempdir().unwrap();
        let ws = shared.path();
        let (a, b) = ("wt:fresh-a", "wt:fresh-b");
        std::fs::write(ws.join("g.txt"), "x\n").unwrap();
        wrote(Writer::Thread(b.to_string()), ws, "g.txt");
        turn_began(a);
        assert!(
            check(&state, a, ws, "g.txt").is_ok(),
            "written before A's turn"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
        wrote(Writer::Thread(b.to_string()), ws, "g.txt");
        assert!(
            check(&state, a, ws, "g.txt").is_err(),
            "written during A's turn"
        );
        assert!(check(&state, a, ws, "new.txt").is_ok(), "a new file");
    }
}
