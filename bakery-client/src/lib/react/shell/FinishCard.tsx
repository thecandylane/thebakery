// A thread's finish card (Round 3): the last thing in a finished thread. Its
// branch against the branch it started from, how much changed, whether it
// merges cleanly, its jobs and CI, and Review, which opens Merge review on
// that branch. A thread is finished at every turn end where its branch has
// new commits beyond its base: the shell offers the card at the turn end, and
// the card shows only once the merge preview finds something to merge.

import { useEffect, useState } from "react";
import { GitBranchIcon, GitMergeIcon } from "lucide-react";
import { gitMergePreview, jobList } from "$lib/api.generated";
import { mergedFiles } from "./mergeFiles";
import { jobSummary } from "./reviewModel";
import { useRollup } from "./rollup";
import { ciOf } from "./sidebarModel";
import { Chip } from "./ui";

export function FinishCard({
  streamId,
  projectId,
  branch,
  fallbackBase,
  onReview,
}: {
  streamId: string;
  projectId: string;
  branch: string;
  /** The project's merge target, for a worktree the node kept no start of. */
  fallbackBase: string;
  onReview: () => void;
}) {
  const rollup = useRollup(projectId);
  const worktree = rollup.data?.worktrees.find((w) => w.stream_id === streamId);
  const base = worktree?.base || fallbackBase;
  const ci = ciOf(rollup.data?.repos.flatMap((r) => r.runs) ?? [], branch);
  /** undefined while the preview runs; null when it failed (the card still shows). */
  const [merge, setMerge] = useState<{ clean: boolean; conflicts: number; files: number } | null | undefined>(undefined);
  const [jobs, setJobs] = useState<ReturnType<typeof jobSummary> | null>(null);
  useEffect(() => {
    let live = true;
    // What merging would change on base: the branch's files, and any clash.
    gitMergePreview({ stream_id: streamId, base }).then(
      (p) => live && setMerge({ clean: p.clean, conflicts: p.conflicts.length, files: mergedFiles(p) }),
      () => live && setMerge(null),
    );
    jobList({ stream_id: streamId }).then(
      (list) => live && setJobs(jobSummary(list)),
      () => live && setJobs(null),
    );
    return () => {
      live = false;
    };
  }, [streamId, base]);

  // ponytail: "new commits beyond base" read as "merging changes base", which
  // misses only commits that net to nothing; a rollup ahead count would be exact.
  if (merge === undefined || merge?.files === 0) return null;
  const ran = jobs ? jobs.passed + jobs.failed + jobs.running : 0;
  const branchName = (name: string) => (
    <span className="flex min-w-0 items-center gap-1.5 font-bold text-(--fg)">
      <GitBranchIcon className="size-3.5 shrink-0 text-(--violet)" />
      <span className="truncate">{name}</span>
    </span>
  );
  return (
    <div
      className="flex flex-col gap-2 rounded-lg border border-(--accent-border) bg-(--bg-raised) px-3 pt-2.5 pb-3 text-xs shadow-[inset_0_2px_0_var(--accent)]"
      data-testid="finish-card"
    >
      <div className="flex min-w-0 items-center gap-2">
        <GitMergeIcon className="size-3.5 shrink-0 text-(--accent)" />
        {branchName(branch)}
        <span className="shrink-0 text-(--fg-dim)">→</span>
        {branchName(base)}
        <span className="flex-1" />
        {merge &&
          (merge.clean ? (
            <Chip tone="teal">CLEAN</Chip>
          ) : (
            <Chip tone="warn">
              {merge.conflicts} {merge.conflicts === 1 ? "CONFLICT" : "CONFLICTS"}
            </Chip>
          ))}
      </div>
      <div className="flex items-center gap-2">
        {merge && (
          <span className="text-(--fg-2)">
            {merge.files} {merge.files === 1 ? "file" : "files"}
          </span>
        )}
        {worktree && worktree.added + worktree.removed > 0 && (
          <span className="font-mono text-[11px]">
            <span className="text-(--diff-add-fg)">+{worktree.added}</span>{" "}
            <span className="text-(--diff-del-fg)">−{worktree.removed}</span>
          </span>
        )}
        <span className="flex-1" />
        {jobs && ran > 0 && (
          <span title="The latest run of each command this agent ran as a job">
            <Chip tone={jobs.failed ? "bad" : jobs.running ? "neutral" : "ok"}>
              ▸ {jobs.failed ? `✗ ${jobs.failed}` : jobs.running ? `◐ ${jobs.running}` : `✓ ${jobs.passed}`}
            </Chip>
          </span>
        )}
        {ci && (
          <span title={`CI on ${branch}`}>
            <Chip tone={ci === "pass" ? "ok" : ci === "fail" ? "bad" : "neutral"}>
              CI {ci === "pass" ? "✓" : ci === "fail" ? "✕" : "◐"}
            </Chip>
          </span>
        )}
      </div>
      <button
        className="flex h-7 items-center gap-1.5 self-start rounded-md border border-(--accent-fg) bg-(image:--grad-accent) px-3 font-bold text-(--fg-on-solid) hover:brightness-105"
        onClick={onReview}
        type="button"
      >
        <GitMergeIcon className="size-3.5" />
        Review
      </button>
    </div>
  );
}
