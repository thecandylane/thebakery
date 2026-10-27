// The GitHub panels' rows (Round 6, G1 and G4): the pull requests and runs
// the node keeps across the owners the operator follows, grouped, kept
// current from the node's github.pr.updated and github.run.updated, and
// where a review comment on a block lands. Pure and DOM-free:
// githubModel.check.ts runs it on node.

import type { GithubPr, GithubRun } from "$lib/api.generated";
import { isFailedRun } from "./sidebarModel.ts";
import type { Hunk } from "./unifiedDiff.ts";

export type PrGroup = "waiting" | "threads" | "failing" | "bots" | "all";
export type RunGroup = "failing" | "running" | "all";

export function inPrGroup(pr: GithubPr, group: PrGroup): boolean {
  switch (group) {
    case "waiting":
      return pr.requested_from_me && !pr.draft;
    case "threads":
      return !!pr.thread_stream_id;
    case "failing":
      return pr.checks.fail > 0;
    case "bots":
      return pr.author_is_bot;
    default:
      return true;
  }
}

export const inRunGroup = (run: GithubRun, group: RunGroup) =>
  group === "all" || (group === "failing" ? isFailedRun(run) : run.status !== "completed");

/** GitHub writes a repository the way it was named ("Acme/Widget"). */
export const sameRepo = (a: string, b: string) => a.toLowerCase() === b.toLowerCase();

const newestFirst = (a: string, b: string) => (a < b ? 1 : a > b ? -1 : 0);

/** The list after a github.pr.updated: the PR replaced, or added when new;
 *  gone when the node says null (closed, merged, or out of scope). */
export function applyPrUpdate(prs: readonly GithubPr[], u: { repository: string; number: number; pr: GithubPr | null }): GithubPr[] {
  const rest = prs.filter((p) => !(p.number === u.number && sameRepo(p.repository, u.repository)));
  return (u.pr ? [...rest, u.pr] : rest).sort((a, b) => newestFirst(a.updated_at, b.updated_at));
}

/** The runs after a github.run.updated: still the latest of each workflow
 *  on each branch. */
export function applyRunUpdate(runs: readonly GithubRun[], run: GithubRun): GithubRun[] {
  const rest = runs.filter((r) => !(sameRepo(r.repository, run.repository) && r.workflow === run.workflow && r.branch === run.branch));
  return [...rest, run].sort((a, b) => newestFirst(a.created_at, b.created_at));
}

/** Where a review comment on a whole block lands: its last line on the new
 *  side, or on the old side for a block that only deletes. */
export function hunkAnchor(hunk: Hunk): { line: number; side: "LEFT" | "RIGHT" } | null {
  const added = hunk.lines.filter((l) => l.tag !== "-").length;
  if (added > 0) return { line: hunk.newStart + added - 1, side: "RIGHT" };
  const removed = hunk.lines.filter((l) => l.tag !== "+").length;
  return removed > 0 ? { line: hunk.oldStart + removed - 1, side: "LEFT" } : null;
}

/** The PR inbox's filters on top of its groups; an empty one lets every
 *  pull request through. `when` is "updated:<days>", "opened:<days>" or
 *  "quiet:<days>" (nothing on it for that long). */
export type PrFilter = { text: string; author: string; when: string; repo: string; state: "" | "ready" | "draft"; review: string };
export const NO_PR_FILTER: PrFilter = { text: "", author: "", when: "", repo: "", state: "", review: "" };
const DAY = 86_400_000;

export function prMatches(pr: GithubPr, f: PrFilter, now = Date.now()): boolean {
  const q = f.text.trim().toLowerCase().replace(/^#/, "");
  if (q && ![pr.title, pr.branch, pr.repository, String(pr.number)].some((s) => s.toLowerCase().includes(q))) return false;
  if (f.author && pr.author !== f.author) return false;
  if (f.repo && pr.repository !== f.repo) return false;
  if (f.state === "ready" && pr.draft) return false;
  if (f.state === "draft" && !pr.draft) return false;
  if (f.review && (pr.review_decision ?? "") !== f.review) return false;
  if (f.when) {
    const [field, days] = f.when.split(":");
    const age = now - Date.parse(field === "opened" ? pr.created_at : pr.updated_at);
    const span = Number(days) * DAY;
    if (field === "quiet" ? age < span : age > span) return false;
  }
  return true;
}
