// The merge review's decisions, kept pure so reviewModel.check.ts runs them
// on plain node: which checkout is the merge target, which branches are up
// for review, and what an agent answered to a review question.

import type { TranscriptItem } from "../../transcript.ts";

export type Checkout = {
  streamId: string;
  branch: string;
  inPlace: boolean;
  onDisk: boolean;
};

/** The merge target: the project's own checkout (the repository itself,
 *  not a worktree). A chat-only project has none. */
export function baseOf<C extends Checkout>(checkouts: readonly C[]): C | null {
  return checkouts.find((c) => c.inPlace && c.branch) ?? null;
}

/** Every agent worktree on a branch of its own, still on disk. */
export function candidatesOf<C extends Checkout>(checkouts: readonly C[], base: C): C[] {
  return checkouts.filter((c) => !c.inPlace && c.onDisk && c.branch && c.branch !== base.branch);
}

export type JobLike = { command: string; started_at: string; state: string; exit_code?: number | null };

/** The latest run of each distinct command a conversation ran as a job:
 *  how many passed, failed (or were stopped), and are still running. */
export function jobSummary(jobs: readonly JobLike[]): { passed: number; failed: number; running: number } {
  const latest = new Map<string, JobLike>();
  for (const j of jobs) {
    const prev = latest.get(j.command);
    if (!prev || j.started_at > prev.started_at) latest.set(j.command, j);
  }
  let passed = 0;
  let failed = 0;
  let running = 0;
  for (const j of latest.values()) {
    if (j.state === "running") running++;
    else if (j.exit_code === 0) passed++;
    else failed++;
  }
  return { passed, failed, running };
}

/** A tag no ordinary message carries, so the answer can be found again. */
export function questionMarker(): string {
  return `review-${crypto.randomUUID().slice(0, 8)}`;
}

/** What the agent said after the question tagged `marker`: its own words
 *  (not a subagent's), or a turn's summary when its runtime has no segments,
 *  up to the next human message. Empty until something arrives. */
export function answerAfter(items: readonly TranscriptItem[], marker: string): string {
  let at = -1;
  for (let i = items.length - 1; i >= 0; i--) {
    const it = items[i];
    if (it.kind === "human" && it.text.includes(marker)) {
      at = i;
      break;
    }
  }
  if (at < 0) return "";
  const words: string[] = [];
  for (const it of items.slice(at + 1)) {
    if (it.kind === "human") break;
    if (it.kind === "segment" && !it.parentId) words.push(it.text);
    else if (it.kind === "done" && !it.segmented && it.summary) words.push(it.summary);
    else if (it.kind === "failed") words.push(`(the turn failed: ${it.reason})`);
    else if (it.kind === "cancelled") words.push(`(stopped: ${it.reason})`);
  }
  return words.join("\n\n").trim();
}
