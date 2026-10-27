// What the factory floor (the zoomed-out overview) and the sidebar show for
// each workspace. Pure and DOM-free: floorModel.check.ts runs it on node.

import type { Workspace } from "./layout.ts";

type Conv = { streamId: string; bakerThread: boolean; openedBy?: string | null; workspace?: boolean };
type Project = { id: string; streamIds: readonly string[] };

/** A workspace's agents: its project's conversations, then anything else
 *  open in it. An orc and its workers live in the orc's workspace (the orc
 *  first), not their project's. The Baker's own threads live in the
 *  Baker's workspace. */
export function membersOf<C extends Conv>(
  w: Workspace,
  projects: readonly Project[],
  conversations: readonly C[],
): C[] {
  const open = new Set(w.columns.flatMap((c) => c.panels));
  const orcs = new Set(conversations.filter((c) => c.workspace).map((c) => c.streamId));
  const project = new Set(w.orc ? [] : (projects.find((p) => p.id === w.projectId)?.streamIds ?? []));
  const own = (c: C) =>
    w.orc ? c.streamId === w.orc || c.openedBy === w.orc : project.has(c.streamId) && !c.workspace && !(c.openedBy && orcs.has(c.openedBy));
  const mine = conversations.filter((c) => !c.bakerThread && (open.has(c.streamId) || own(c)));
  return w.orc ? [...mine.filter((c) => c.streamId === w.orc), ...mine.filter((c) => c.streamId !== w.orc)] : mine;
}

/** The floor's cards for one workspace: its panels in layout order, then
 *  its agents that have no panel yet. */
export function floorCards(w: Workspace, memberIds: readonly string[]): { id: string; open: boolean }[] {
  const open = w.columns.flatMap((c) => c.panels);
  const shown = new Set(open);
  return [
    ...open.map((id) => ({ id, open: true })),
    ...memberIds.filter((id) => !shown.has(id)).map((id) => ({ id, open: false })),
  ];
}

/** The conversation whose folder a Code or Preview panel shows for
 *  `streamId`: its own; for a Baker conversation with no folder of its own,
 *  the permanent Baker's (the Bakery's main checkout). */
export function folderStream(
  conversations: readonly { streamId: string; bakerThread: boolean; main: boolean }[],
  checkouts: readonly { streamId: string; path: string }[],
  streamId: string,
): string {
  if (checkouts.some((c) => c.streamId === streamId && c.path)) return streamId;
  const baker = conversations.find((c) => c.streamId === streamId)?.bakerThread;
  return (baker && conversations.find((c) => c.main)?.streamId) || streamId;
}

type CheckoutLike = { streamId: string; branch: string; inPlace: boolean; path: string };
export type WorktreeGroup<C> = {
  /** The worktree's folder; "" for conversations with no checkout. */
  path: string;
  branch: string;
  /** The repository's own checkout, not a worktree. */
  local: boolean;
  members: C[];
};

/** A workspace's agents grouped by the checkout they work in, as jcode's
 *  Worktrees sidebar does: the local checkout first, then each worktree
 *  by branch name, then any conversation without a checkout. */
export function worktreeGroups<C extends { streamId: string }>(
  members: readonly C[],
  checkouts: readonly CheckoutLike[],
): WorktreeGroup<C>[] {
  const byStream = new Map(checkouts.map((c) => [c.streamId, c]));
  const groups = new Map<string, WorktreeGroup<C>>();
  for (const m of members) {
    const co = byStream.get(m.streamId);
    const path = co?.path ?? "";
    let g = groups.get(path);
    if (!g) {
      g = { path, branch: co?.branch ?? "", local: false, members: [] };
      groups.set(path, g);
    }
    g.local ||= co?.inPlace ?? false;
    g.members.push(m);
  }
  const rank = (g: WorktreeGroup<C>) => (g.local ? 0 : g.path ? 1 : 2);
  return [...groups.values()].sort((a, b) => rank(a) - rank(b) || a.branch.localeCompare(b.branch));
}

type MapNode = { id: string; kind: string; parent: string | null; stream_id: string; last_ts: string };

/** A conversation's own agent on the map: the one in its seat, else its first. */
export function agentOf<N extends MapNode & { seat: string }>(nodes: readonly N[], streamId: string, seat: string): N | null {
  const mine = nodes.filter((n) => n.kind !== "subagent" && n.stream_id === streamId);
  return mine.find((n) => n.seat === seat) ?? mine[0] ?? null;
}

/** Everything an agent started (subagents, tasks, theirs in turn), as a tree
 *  in launch order: each with its depth under it (0 = started by it). */
export function agentTree<N extends MapNode>(nodes: readonly N[], rootId: string): { node: N; depth: number }[] {
  const children = new Map<string, N[]>();
  for (const n of nodes) if (n.parent) children.set(n.parent, [...(children.get(n.parent) ?? []), n]);
  const out: { node: N; depth: number }[] = [];
  const seen = new Set([rootId]);
  const walk = (id: string, depth: number) => {
    const kids = [...(children.get(id) ?? [])].sort((a, b) => (a.last_ts < b.last_ts ? -1 : 1));
    for (const n of kids) {
      if (seen.has(n.id)) continue; // a cycle in bad data must not hang the map
      seen.add(n.id);
      out.push({ node: n, depth });
      walk(n.id, depth + 1);
    }
  };
  walk(rootId, 0);
  return out;
}

/** The subagents a conversation's agents launched, as a tree in launch
 *  order: each with its depth under the seat agent (0 = launched by it). */
export function subagentTree<N extends MapNode>(nodes: readonly N[], streamId: string): { node: N; depth: number }[] {
  const subs = nodes.filter((n) => n.kind === "subagent" && n.stream_id === streamId);
  const ids = new Set(subs.map((n) => n.id));
  const children = new Map<string | null, N[]>();
  for (const n of subs) {
    // A parent outside this conversation's subagents is the seat agent itself.
    const key = n.parent && ids.has(n.parent) ? n.parent : null;
    children.set(key, [...(children.get(key) ?? []), n]);
  }
  const out: { node: N; depth: number }[] = [];
  const seen = new Set<string>();
  const walk = (parent: string | null, depth: number) => {
    const kids = [...(children.get(parent) ?? [])].sort((a, b) => (a.last_ts < b.last_ts ? -1 : 1));
    for (const n of kids) {
      if (seen.has(n.id)) continue; // a cycle in bad data must not hang the floor
      seen.add(n.id);
      out.push({ node: n, depth });
      walk(n.id, depth + 1);
    }
  };
  walk(null, 0);
  // A cycle has no way in from the top; its members stay visible on their own.
  for (const n of subs) if (!seen.has(n.id)) out.push({ node: n, depth: 0 });
  return out;
}
