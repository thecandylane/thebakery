//! Git rule 5: early warning when two open threads on one repository change
//! the same files, "so that agents can know and collaborate". Checked at
//! every turn's end (`snapshots`): each thread's overlaps are a fact on its
//! own stream, appended when they change; the sidebar shows them
//! (`routes::rollup`) and an agent's next turn opens with them
//! (`routes::agent`).

use crate::state::{append, context_for, AppState};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::Path;

pub(crate) const OVERLAP_FAMILY: &str = "git.overlap";

/// Another thread changing some of the same files.
#[derive(schemars::JsonSchema, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Overlap {
    pub(crate) stream_id: String,
    /// The files both have changed since they forked, committed or not.
    pub(crate) files: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct Overlaps {
    with: Vec<Overlap>,
}

/// A thread's overlaps as last recorded.
pub(crate) fn current(state: &AppState, stream_id: &str) -> Vec<Overlap> {
    let last = state.heart.lock().unwrap().last_of_family(stream_id, OVERLAP_FAMILY);
    last.ok()
        .flatten()
        .and_then(|e| serde_json::from_str::<Overlaps>(&e.payload).ok())
        .map(|o| o.with)
        .unwrap_or_default()
}

/// What a thread changed since it forked: committed or not, untracked too.
fn changed(state: &AppState, stream_id: &str, dir: &Path) -> BTreeSet<String> {
    let base = crate::routes::git::fork_base(state, stream_id, dir);
    let Ok(fork) = crate::git::run(dir, &["merge-base", "HEAD", &base]) else {
        return BTreeSet::new();
    };
    let tracked = crate::git::run(dir, &["diff", "--name-only", fork.trim()]).unwrap_or_default();
    let untracked = crate::git::run(dir, &["ls-files", "--others", "--exclude-standard"]).unwrap_or_default();
    tracked.lines().chain(untracked.lines()).map(str::to_string).collect()
}

/// Checks every open thread on `stream_id`'s repository and records each
/// one's overlaps where they changed.
// ponytail: a few git commands per open thread on the repository at each
// turn's end; batch or cache by HEAD if a repository holds many threads.
pub(crate) fn check(state: &AppState, stream_id: &str) {
    let Ok(ctx) = context_for(state, stream_id) else { return };
    let Ok(streams) = crate::state::known_streams(state) else { return };
    let canonical = |p: &str| std::fs::canonicalize(p).ok();
    let repo = canonical(&ctx.repo_root);
    if repo.is_none() {
        return;
    }
    let open: Vec<_> = {
        let heart = state.heart.lock().unwrap();
        streams
            .into_iter()
            .filter(|s| !s.discarded && s.exists_on_disk && canonical(&s.repo_root) == repo)
            .filter(|s| !crate::agents::socket_archived_locked(&heart, &s.stream_id))
            .filter(|s| !crate::baker_threads::archived_locked(&heart, &s.stream_id))
            .collect()
    };
    let threads: Vec<(String, BTreeSet<String>)> = open
        .iter()
        .map(|s| (s.stream_id.clone(), changed(state, &s.stream_id, Path::new(&s.worktree_path))))
        .collect();
    for (sid, mine) in &threads {
        let with: Vec<Overlap> = threads
            .iter()
            .filter(|(other, _)| other != sid)
            .filter_map(|(other, theirs)| {
                let files: Vec<String> = mine.intersection(theirs).cloned().collect();
                (!files.is_empty()).then(|| Overlap { stream_id: other.clone(), files })
            })
            .collect();
        if with != current(state, sid) {
            let payload = serde_json::to_string(&Overlaps { with }).unwrap();
            let _ = append(state, sid, "node", OVERLAP_FAMILY, payload, vec![]);
        }
    }
}

/// One line per thread overlapping this one, when that changed after
/// `since` (an index into its stream): the opening of an agent's next turn.
pub(crate) fn note_since(state: &AppState, events: &[bakery_core::Event], since: usize) -> Vec<String> {
    let Some(last) = events[since..].iter().rev().find(|e| e.family == OVERLAP_FAMILY) else {
        return Vec::new();
    };
    let with = serde_json::from_str::<Overlaps>(&last.payload).map(|o| o.with).unwrap_or_default();
    with.iter()
        .map(|o| {
            let name = context_for(state, &o.stream_id).map(|c| c.name).unwrap_or_default();
            format!(
                "Thread {name} ({}) is also changing {}: coordinate with it before you change those.",
                o.stream_id,
                o.files.join(", ")
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::project::open_stream;
    use crate::testutil::*;
    use bakery_agent::StubModel;
    use std::sync::Arc;

    /// Two open threads on one repository changing the same file each learn
    /// of the other, and their agents are told; a thread changing something
    /// else doesn't, nor does an archived one.
    #[test]
    fn threads_changing_the_same_files_learn_of_each_other() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let open = |name: &str| open_stream(&state, &open_req(&repo, name)).unwrap().context;
        let (a, b, c) = (open("a"), open("b"), open("c"));
        std::fs::write(Path::new(&a.worktree_path).join("f.txt"), "a\n").unwrap();
        std::fs::write(Path::new(&b.worktree_path).join("f.txt"), "b\n").unwrap();
        std::fs::write(Path::new(&c.worktree_path).join("other.txt"), "c\n").unwrap();

        check(&state, &a.stream_id);
        let with = |other: &str| vec![Overlap { stream_id: other.to_string(), files: vec!["f.txt".to_string()] }];
        assert_eq!(current(&state, &a.stream_id), with(&b.stream_id));
        assert_eq!(current(&state, &b.stream_id), with(&a.stream_id));
        assert!(current(&state, &c.stream_id).is_empty());
        let events = state.heart.lock().unwrap().read_stream(&a.stream_id).unwrap();
        let note = note_since(&state, &events, 0);
        assert!(note.len() == 1 && note[0].starts_with("Thread b (") && note[0].contains("f.txt"), "{note:?}");

        let facts = |s: &str| state.heart.lock().unwrap().events_of_family(s, OVERLAP_FAMILY).unwrap().len();
        let before = facts(&a.stream_id);
        check(&state, &b.stream_id);
        assert_eq!(facts(&a.stream_id), before, "nothing new, nothing recorded");

        crate::agents::set_archived(&state, &b.stream_id, true, "human").unwrap();
        check(&state, &a.stream_id);
        assert!(current(&state, &a.stream_id).is_empty(), "an archived thread isn't open");
    }
}
