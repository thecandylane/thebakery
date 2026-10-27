// What the sidebar's control panel shows: a workspace's repos, the worktrees
// of each and the agents in them; a branch's CI from GitHub's runs. Pure and
// DOM-free: sidebarModel.check.ts runs it on node.

import { worktreeGroups, type WorktreeGroup } from "./floorModel.ts";

type CheckoutLike = { streamId: string; branch: string; inPlace: boolean; path: string; repoRoot: string };
export type RepoGroup<C> = { root: string; name: string; worktrees: WorktreeGroup<C>[] };

/** A workspace's agents as repo › worktree › agents. `roots` adds repos no
 *  agent works in yet (from the rollup), so each can still get a worktree.
 *  Agents without a checkout (chats) come back as `loose`. */
export function repoTree<C extends { streamId: string }>(
  members: readonly C[],
  checkouts: readonly CheckoutLike[],
  roots: readonly string[] = [],
): { repos: RepoGroup<C>[]; loose: C[] } {
  const rootOf = new Map(checkouts.map((c) => [c.streamId, c.repoRoot]));
  const byRoot = new Map<string, C[]>(roots.map((r) => [r, []]));
  const loose: C[] = [];
  for (const m of members) {
    const root = rootOf.get(m.streamId);
    if (root) byRoot.set(root, [...(byRoot.get(root) ?? []), m]);
    else loose.push(m);
  }
  const repos = [...byRoot].map(([root, ms]) => ({
    root,
    name: root.split("/").filter(Boolean).at(-1) ?? root,
    worktrees: worktreeGroups(ms, checkouts),
  }));
  return { repos: repos.sort((a, b) => a.name.localeCompare(b.name)), loose };
}

/** Where a new agent of a project can work. */
export type AgentPlace = {
  /** The project's repositories: each can give the agent its own worktree. */
  repos: { root: string; name: string }[];
  /** The repository picked first: the shown thread's. */
  repo: string;
  /** The shown thread's folder, to share instead; null when it has none or
   *  sharing is not offered. */
  shared: { path: string; branch: string } | null;
};

/** Where a new agent of the project with `checkouts` can work, the shown
 *  thread's repository first; that thread's folder is the one it could
 *  share, unless `share` is off (the Bakery's own repo: always its own). */
export function agentPlace(checkouts: readonly CheckoutLike[], shown: string, share: boolean): AgentPlace {
  const roots = [...new Set(checkouts.map((c) => c.repoRoot || c.path).filter(Boolean))];
  const co = checkouts.find((c) => c.streamId === shown && c.path);
  return {
    repos: roots
      .map((root) => ({ root, name: root.split("/").filter(Boolean).at(-1) ?? root }))
      .sort((a, b) => a.name.localeCompare(b.name)),
    repo: co ? co.repoRoot || co.path : (roots[0] ?? ""),
    shared: co && share ? { path: co.path, branch: co.branch } : null,
  };
}

type RunLike = { branch: string; status: string; conclusion: string };
const FAILED = new Set(["failure", "timed_out", "startup_failure", "action_required"]);

export const isFailedRun = (r: RunLike) => r.status === "completed" && FAILED.has(r.conclusion);

/** A branch's CI badge: the worst of its latest runs (a failure, then one
 *  still going, then all passed), or null when it has none. */
export function ciOf(runs: readonly RunLike[], branch: string): "fail" | "pending" | "pass" | null {
  const mine = runs.filter((r) => r.branch === branch);
  if (mine.length === 0) return null;
  if (mine.some(isFailedRun)) return "fail";
  return mine.some((r) => r.status !== "completed") ? "pending" : "pass";
}

type FetchedLike = { at: string; error?: string | null } | null | undefined;

/** How fresh a worktree's "N behind" is (git rule 3): when its repository
 *  was last fetched, or why the last fetch failed. `ago` words a time. */
export function fetchedText(fetched: FetchedLike, ago: (ts: string) => string): string {
  if (!fetched) return "not fetched yet";
  const when = ago(fetched.at);
  const last = when === "now" ? "just now" : `${when} ago`;
  // git's own message can run over several lines; it shows on one.
  return fetched.error ? `couldn't fetch: ${fetched.error.replace(/\s+/g, " ").trim()}, last fetched ${last}` : `fetched ${last}`;
}

/** Other open threads changing some of the same files (git rule 5), in words. */
export function overlapText(
  overlaps: readonly { stream_id: string; files: readonly string[] }[],
  name: (streamId: string) => string,
): string {
  return overlaps.map((o) => `also changing ${o.files.join(", ")}: ${name(o.stream_id)}`).join(" · ");
}
