// The Agent map's layout, kept apart from its React view so a plain check
// can run it (agentMapLayout.check.ts).

import type { AgentMapNode } from "../node.ts";

export const CARD_W = 210;
export const CARD_H = 58;
const GAP_X = 18;
const GAP_Y = 46;
const DAY_MS = 24 * 60 * 60 * 1000;

export type Placed = { node: AgentMapNode; x: number; y: number };

export const live = (n: AgentMapNode) => n.status === "working" || n.status === "needs_input";

/** What "Active" shows: anything working or waiting on you, anything active
 *  in the last day, the Baker — and every node on the way to one of those,
 *  so no edge is cut. */
export function visibleNodes(nodes: AgentMapNode[], all: boolean, now = Date.now()): AgentMapNode[] {
  if (all) return nodes;
  const byId = new Map(nodes.map((n) => [n.id, n]));
  const keep = new Set<string>();
  for (const n of nodes) {
    const recent = now - Date.parse(n.last_ts) < DAY_MS;
    if (!(n.kind === "baker" || live(n) || recent)) continue;
    for (let at: AgentMapNode | undefined = n; at && !keep.has(at.id); at = at.parent ? byId.get(at.parent) : undefined) {
      keep.add(at.id);
    }
  }
  return nodes.filter((n) => keep.has(n.id));
}

/** A tidy top-down tree: each subtree as wide as its leaves, every parent
 *  centred over its children (`x` is a card's centre, `y` its top). The
 *  Baker leads; then whatever needs you or is working; then the most recent. */
export function layout(nodes: AgentMapNode[]): { placed: Placed[]; width: number; height: number } {
  const byId = new Map(nodes.map((n) => [n.id, n]));
  const children = new Map<string, AgentMapNode[]>();
  const roots: AgentMapNode[] = [];
  for (const n of nodes) {
    if (n.parent && byId.has(n.parent)) {
      const list = children.get(n.parent) ?? [];
      list.push(n);
      children.set(n.parent, list);
    } else roots.push(n);
  }
  const rank = (n: AgentMapNode) => (n.kind === "baker" ? 0 : live(n) ? 1 : 2);
  roots.sort((a, b) => rank(a) - rank(b) || b.last_ts.localeCompare(a.last_ts));
  const placed: Placed[] = [];
  const seen = new Set<string>();
  let cursor = CARD_W / 2;
  let deepest = 0;
  const place = (n: AgentMapNode, depth: number): number => {
    seen.add(n.id);
    deepest = Math.max(deepest, depth);
    const kids = (children.get(n.id) ?? []).filter((k) => !seen.has(k.id));
    let x: number;
    if (kids.length === 0) {
      x = cursor;
      cursor += CARD_W + GAP_X;
    } else {
      const centres = kids.map((k) => place(k, depth + 1));
      x = (centres[0] + centres[centres.length - 1]) / 2;
    }
    placed.push({ node: n, x, y: depth * (CARD_H + GAP_Y) });
    return x;
  };
  for (const root of roots) place(root, 0);
  // A node only reachable through a cycle of "started by" still gets drawn.
  for (const n of nodes) if (!seen.has(n.id)) place(n, 0);
  return {
    placed,
    // The last card's right edge: half a card past its centre.
    width: Math.max(cursor - GAP_X - CARD_W / 2, CARD_W),
    height: (deepest + 1) * (CARD_H + GAP_Y) - GAP_Y,
  };
}
