//! Git rule 3: an agent brings its base in by a merge (`git_update`), and a
//! rebase takes a stated reason (`git_rebase`, which records it and rebases
//! node-side with hooks off, like every agent git tool).
//!
//! For agent processes only, the Bakery points git at its own hooks folder
//! through the environment ([`for_agents`]): the operator's git, hooks and
//! repo config stay untouched, and nothing is written into a repo. Its
//! pre-rebase refuses an agent's own `git rebase`; every other hook runs the
//! project's own ([`RUNNER`]), so project hooks still run for agents. The
//! Bakery's git tools run node-side with hooks off, so project code never
//! runs outside a room; they run the project's hooks in the agent's room
//! first instead ([`run_project_hook`]). Client hooks are a guardrail, not a
//! lock (`--no-verify`, an agent's own `-c core.hooksPath`), so at each
//! turn's end the reflog is checked for a rebase with no stated reason
//! ([`check_rebases`]).

use crate::state::{append, AppState};
use std::path::{Path, PathBuf};

/// A rebase's stated reason, recorded before `git_rebase` runs it.
pub(crate) const REASONED_FAMILY: &str = "git.rebase.reasoned";
/// Rebases a turn made with no stated reason, found in the reflog.
pub(crate) const UNEXPLAINED_FAMILY: &str = "git.rebase.unexplained";

/// The client hooks git runs; each runs the repo's own of that name.
const HOOKS: &[&str] = &[
    "applypatch-msg",
    "pre-applypatch",
    "post-applypatch",
    "pre-commit",
    "pre-merge-commit",
    "prepare-commit-msg",
    "commit-msg",
    "post-commit",
    "pre-rebase",
    "post-checkout",
    "post-merge",
    "pre-push",
    "post-rewrite",
    "pre-auto-gc",
    "post-index-change",
];

/// The runner for the project's own hooks, in the Bakery's hooks folder.
const RUNNER: &str = "run-project-hook";

/// Runs the project's own hook named `$1`, found the way git would without
/// the Bakery's override (a core.hooksPath in the repo's own config, as
/// husky sets, else the repo's hooks folder), with the rest of the
/// arguments. It runs through the interpreter its first line names: an
/// agent's room runs no program from the workspace directly, only scripts
/// through an interpreter. No such hook, or not executable: it passes.
const RUN_PROJECT_HOOK: &str = r#"#!/bin/sh
# The Bakery's, for agents only: the project's own hook, in the agent's room.
name="$1"; shift
dir="$(GIT_CONFIG_COUNT=0 git config --get core.hooksPath 2>/dev/null)"
if [ -z "$dir" ]; then
  dir="$(git rev-parse --git-common-dir)/hooks"
else
  case "$dir" in /*) ;; *) dir="$(git rev-parse --show-toplevel)/$dir" ;; esac
fi
hook="$dir/$name"
# Run only if executable, as git does: asked of the file's mode, since in a
# room -x answers whether the room may run it, never true in a workspace.
[ -f "$hook" ] && [ -n "$(find "$hook" -prune -perm -u+x 2>/dev/null)" ] || exit 0
first="$(head -n 1 "$hook")"
case "$first" in
  '#!'*) set -- ${first#??} "$hook" "$@" ;;
  *) set -- /bin/sh "$hook" "$@" ;;
esac
exec "$@"
"#;

const RUN_THE_PROJECTS: &str = "exec /bin/sh \"$(dirname \"$0\")/run-project-hook\" \"$(basename \"$0\")\" \"$@\"\n";

const REFUSE_REBASE: &str = "echo \"A rebase needs a stated reason: use the git_rebase tool (onto, reason), or merge the base in with git_update.\" >&2\nexit 1\n";

pub(crate) fn hooks_dir(state: &AppState) -> PathBuf {
    state.base.join("git-hooks")
}

/// Writes the hooks where they differ; agents only read and run them.
fn install(state: &AppState) -> Result<PathBuf, String> {
    use std::os::unix::fs::PermissionsExt;
    let dir = hooks_dir(state);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let files = HOOKS.iter().map(|name| {
        let body = match *name {
            "pre-rebase" => format!("#!/bin/sh\n# The Bakery's, for agents only.\n{REFUSE_REBASE}"),
            _ => format!("#!/bin/sh\n# The Bakery's, for agents only.\n{RUN_THE_PROJECTS}"),
        };
        (*name, body)
    });
    for (name, body) in files.chain([(RUNNER, RUN_PROJECT_HOOK.to_string())]) {
        let path = dir.join(name);
        if std::fs::read_to_string(&path).ok().as_deref() != Some(body.as_str()) {
            std::fs::write(&path, &body).map_err(|e| format!("{}: {e}", path.display()))?;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                .map_err(|e| format!("{}: {e}", path.display()))?;
        }
    }
    Ok(dir)
}

/// Runs the project's own `hook` in the agent's room, as git would have: the
/// Bakery's git tools work node-side with hooks off, so project code never
/// runs outside a room. Nothing for the operator's own call: their commits
/// through the app run no project hooks. A missing hook passes; a failing
/// one stops the action, with what it printed.
pub(crate) fn run_project_hook(state: &AppState, stream_id: &str, hook: &str, args: &[&str]) -> Result<(), String> {
    if !crate::actor::agent_calling() {
        return Ok(());
    }
    let ctx = crate::state::context_for(state, stream_id)?;
    let root = Path::new(&ctx.worktree_path).canonicalize().map_err(|e| e.to_string())?;
    let runner = install(state)?.join(RUNNER);
    let toolchain = crate::catalog::stacks_for_socket(state, stream_id).toolchain;
    let env = bakery_exec::room_for_run(&root, &crate::state::cache_root(state), stream_id, &toolchain)?;
    let granted = crate::machine_access::for_stream(state, stream_id);
    // A hook re-adding files (lint-staged) makes new objects, and only that:
    // no existing one changes, and the shared refs and config stay read-only.
    let (mut read, mut write, objects) = match crate::git::linked_worktree_git_dirs(&root) {
        Some((shared, own)) => (vec![shared.clone()], vec![own], vec![shared.join("objects")]),
        None => (Vec::new(), Vec::new(), Vec::new()),
    };
    read.extend(granted.read.iter().cloned());
    write.extend(granted.write.iter().cloned());
    let mut argv = vec!["/bin/sh".to_string(), runner.display().to_string(), hook.to_string()];
    argv.extend(args.iter().map(|a| a.to_string()));
    let run = bakery_exec::WorkspaceRun {
        argv: &argv,
        extra_deny_read: &granted.deny,
        timeout: bakery_exec::DEFAULT_WORKSPACE_RUN_TIMEOUT,
        extra_read: &read,
        extra_write: &write,
        extra_exec: &granted.exec,
        whole_machine: granted.whole_machine.as_ref(),
        path: &granted.path,
        env: &granted.env,
        append_only: &objects,
    };
    let out = bakery_exec::attach_exec_workspace_rw(&env, &root, &run)?;
    if out.exit_code == 0 {
        return Ok(());
    }
    // Without the xcrun shim's noise about its cache, which a room keeps
    // out of the host's temp folder.
    let said: Vec<&str> = out.stdout.lines().chain(out.stderr.lines())
        .filter(|l| !l.contains("couldn't create cache file") && !l.contains("DVTFilePathFSEvents"))
        .collect();
    Err(format!("the project's {hook} hook stopped it:\n{}", said.join("\n")).trim_end().to_string())
}

/// What an agent's room adds for git: the Bakery's hooks folder, to read and
/// run, and git's setting pointing there.
pub(crate) fn for_agents(state: &AppState) -> Option<(PathBuf, Vec<(String, String)>)> {
    let dir = install(state).ok()?;
    Some((dir.clone(), vec![("core.hooksPath".to_string(), dir.display().to_string())]))
}

/// git settings as its environment (`GIT_CONFIG_COUNT`, `GIT_CONFIG_KEY_n`,
/// `GIT_CONFIG_VALUE_n`), above every config file.
pub(crate) fn config_env(settings: &[(String, String)]) -> Vec<(String, String)> {
    let mut env = vec![("GIT_CONFIG_COUNT".to_string(), settings.len().to_string())];
    for (n, (key, value)) in settings.iter().enumerate() {
        env.push((format!("GIT_CONFIG_KEY_{n}"), key.clone()));
        env.push((format!("GIT_CONFIG_VALUE_{n}"), value.clone()));
    }
    env
}

/// At a turn's end: each rebase it started (a "rebase (start)" in the
/// worktree's reflog since `since`, unix seconds) beyond the reasons it
/// stated is recorded on its thread, so History and the review say "rebased
/// without a stated reason".
pub(crate) fn check_rebases(state: &AppState, stream_id: &str, agent_id: &str, turn_id: &str, since: i64) {
    let Ok(ctx) = crate::state::context_for(state, stream_id) else { return };
    if ctx.worktree_path.trim().is_empty() {
        return;
    }
    let started = rebases_since(Path::new(&ctx.worktree_path), since);
    if started == 0 {
        return;
    }
    let reasoned = {
        let heart = state.heart.lock().unwrap();
        let events = heart.read_stream(stream_id).unwrap_or_default();
        events
            .iter()
            .filter(|e| e.family == REASONED_FAMILY)
            .filter(|e| chrono::DateTime::parse_from_rfc3339(&e.ts).is_ok_and(|t| t.timestamp() >= since))
            .count()
    };
    if started > reasoned {
        let payload = serde_json::json!({ "agent_id": agent_id, "turn_id": turn_id, "count": started - reasoned });
        let _ = append(state, stream_id, "node", UNEXPLAINED_FAMILY, payload.to_string(), vec![]);
    }
}

/// How many rebases began in `dir`'s worktree since `since` (unix seconds).
fn rebases_since(dir: &Path, since: i64) -> usize {
    let Ok(log) = crate::git::run(dir, &["reflog", "--date=unix", "--format=%gd %gs", "HEAD"]) else {
        return 0;
    };
    log.lines()
        .filter_map(|line| {
            let (selector, subject) = line.split_once(' ')?;
            let at: i64 = selector.split_once("@{")?.1.trim_end_matches('}').parse().ok()?;
            (at >= since && subject.starts_with("rebase (start)")).then_some(())
        })
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;
    use bakery_agent::StubModel;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Arc;

    fn commit(dir: &Path, file: &str) {
        std::fs::write(dir.join(file), format!("{file}\n")).unwrap();
        run_git(dir, &["add", "."]);
        run_git(dir, &["commit", "-qm", file]);
    }

    /// An agent's own `git rebase` meets the Bakery's pre-rebase and is
    /// refused; its other hooks run the repo's own; and a turn that rebased
    /// anyway, without a reason, is recorded.
    #[test]
    fn an_agents_own_rebase_needs_a_reason_and_one_without_is_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let (hooks, settings) = for_agents(&state).unwrap();
        assert_eq!(hooks, hooks_dir(&state));
        let env = config_env(&settings);
        let agent_git = |args: &[&str]| {
            let out = std::process::Command::new("git").current_dir(&repo).envs(env.iter().cloned()).args(args).output().unwrap();
            (out.status.success(), String::from_utf8_lossy(&out.stderr).into_owned())
        };
        let main = String::from_utf8(std::process::Command::new("git").current_dir(&repo).args(["branch", "--show-current"]).output().unwrap().stdout).unwrap();
        let main = main.trim().to_string();
        run_git(&repo, &["checkout", "-qb", "side"]);
        commit(&repo, "side.txt");
        run_git(&repo, &["checkout", "-q", &main]);
        commit(&repo, "main.txt");
        run_git(&repo, &["checkout", "-q", "side"]);
        let (ok, why) = agent_git(&["rebase", &main]);
        assert!(!ok && why.contains("git_rebase"), "refused: {why}");

        // The repo's own hook still runs for an agent.
        let own = repo.join(".git/hooks/pre-commit");
        std::fs::write(&own, "#!/bin/sh\necho repo-hook-ran >&2\nexit 1\n").unwrap();
        std::fs::set_permissions(&own, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(repo.join("side.txt"), "more\n").unwrap();
        run_git(&repo, &["add", "."]);
        let (ok, why) = agent_git(&["commit", "-qm", "blocked by the repo's hook"]);
        assert!(!ok && why.contains("repo-hook-ran"), "{why}");
        std::fs::remove_file(&own).unwrap();
        // A hooks folder the repo's own config names, as husky sets.
        let husky = repo.join(".husky/_");
        std::fs::create_dir_all(&husky).unwrap();
        std::fs::write(husky.join("pre-commit"), "#!/bin/sh\necho husky-ran >&2\nexit 1\n").unwrap();
        std::fs::set_permissions(husky.join("pre-commit"), std::fs::Permissions::from_mode(0o755)).unwrap();
        run_git(&repo, &["config", "core.hooksPath", ".husky/_"]);
        let (ok, why) = agent_git(&["commit", "-qm", "blocked by husky"]);
        assert!(!ok && why.contains("husky-ran"), "{why}");
        run_git(&repo, &["config", "--unset", "core.hooksPath"]);
        run_git(&repo, &["reset", "-q", "--hard"]);

        // A thread rebased around the hook: found at the turn's end.
        let since = chrono::Utc::now().timestamp() - 1;
        let sid = crate::routes::project::open_stream(&state, &open_req(&repo, "rebase-check")).unwrap().context.stream_id;
        let ctx = crate::state::context_for(&state, &sid).unwrap();
        let wt = std::path::PathBuf::from(&ctx.worktree_path);
        commit(&wt, "thread.txt");
        run_git(&repo, &["checkout", "-q", &main]);
        commit(&repo, "later.txt");
        run_git(&wt, &["rebase", "--no-verify", "-q", &main]);
        assert_eq!(rebases_since(&wt, since), 1);
        check_rebases(&state, &sid, "a1", "t1", since);
        let found = state.heart.lock().unwrap().last_of_family(&sid, UNEXPLAINED_FAMILY).unwrap();
        assert!(found.is_some_and(|e| e.payload.contains("\"count\":1")));
    }
}
