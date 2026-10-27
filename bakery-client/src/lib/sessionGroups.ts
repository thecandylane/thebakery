// Session display helpers, extracted from the sidebar (Sidebar.svelte) so
// they're testable without a DOM.
//
// 07d-A3: grouping and factory/project classification are no longer client
// concerns at all — `GET /studio/state` is the one topology source, already
// bucketed into `projects`/`ungrouped_sockets`/`factory_sockets` off durable
// server facts (`socket.kind.recorded`, `socket.project.set`). The old
// `isBakerStream`/`buildGroups`/`projectLabel` name/branch/repo_root
// heuristics this file used to hold are gone, not adapted — reintroducing
// name-based classification client-side would be exactly the kind of
// inference 07d-A3 §1 forbids ("Nothing may infer behavior from wt:,
// baker/, names, paths, or branches").

import type { StreamRef } from "./node.ts";

/**
 * A branch minted by the old naming scheme. Shown as a quiet "legacy" tag so a
 * `bakery/*` branch under someone else's project reads as this product's old
 * default rather than as evidence the session belongs to The Bakery.
 */
export function isLegacyBakeryBranch(branch: string): boolean {
  return branch.startsWith("bakery/");
}

/** Last non-empty path segment. `/a/b/` and `/a/b` both give `b`. */
export function basename(path: string): string {
  const parts = path.split("/").filter((p) => p.length > 0);
  return parts.length > 0 ? parts[parts.length - 1] : path;
}

/**
 * 07c-UX-SHELL §1.3: chosen name → else branch → else worktree. `name` is
 * currently always server-derived and non-empty (`known_streams` strips the
 * `wt:` prefix off `stream_id`), so this resolves at the first tier today —
 * the chain still exists so a future real "chosen name" concept (or a
 * degenerate empty value) falls through correctly instead of rendering "".
 */
export function socketLabel(s: StreamRef): string {
  return s.name.trim() || s.branch.trim() || basename(s.worktree_path) || s.stream_id;
}

/** "in place" | "" — per-socket secondary fact (spec 1.3: muted, not the
 *  row title). "tree gone" is not repeated here; the row's status dot/text
 *  already carries that fact. */
export function socketTag(s: StreamRef): string {
  return s.exists_on_disk && s.in_place ? "in place" : "";
}
