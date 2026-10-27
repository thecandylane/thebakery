// Pure comparison behind Review.svelte's mandatory fresh-diff check before
// sign-off (07d-B Phase 4 item 2/3 fix). A plain, framework-free module so
// it is unit-testable under `node` without pulling in Svelte or Monaco.
//
// The bug this exists to close: `signOff()` used to go straight to
// `node.taskSignoff()` on whatever `review` the panel happened to be
// showing. Nothing refetches `GET /task/review` except the socket-identity
// `$effect` — accepting a proposal in this very pane (`agent.file.changed`/
// `agent.edit.accepted`) or a terminal/editor writing into the task
// worktree fires no `task.*` event and never re-runs it. The server then
// recomputes its own fresh `compute_review` inside `task_signoff` and
// immediately discards it (`let _ = &review;`) before `git add -A` +
// commit — so the pinned `signoff_commit` can contain changes the human
// reviewing in this pane never saw, including one they accepted seconds
// earlier. `reviewFilesChanged` is the exact-equality check the caller runs
// against a freshly re-fetched `TaskReviewResp` immediately before
// confirming sign-off, so a mismatch can replace the stale panel and refuse
// to sign off until the human re-confirms the new set.
import type { TaskReviewResp } from "./node.ts";

type ReviewFile = TaskReviewResp["files"][number];

function fileKey(f: ReviewFile): string {
  // Full content, not just counts: two edits of equal added/removed size
  // (e.g. a one-line rewrite) must not compare as identical.
  return JSON.stringify([f.path, f.status, f.added, f.removed, f.old_content, f.new_content]);
}

/** True if the file set `a` differs from `b` in path, status, added/removed
 *  counts, or content, in any way (order-independent) — including a
 *  different file count. `null` compares unequal to anything but another
 *  `null`. */
export function reviewFilesChanged(a: TaskReviewResp | null, b: TaskReviewResp | null): boolean {
  if (a === null || b === null) return a !== b;
  if (a.files.length !== b.files.length) return true;
  const aKeys = a.files.map(fileKey).sort();
  const bKeys = b.files.map(fileKey).sort();
  return aKeys.some((k, i) => k !== bKeys[i]);
}
