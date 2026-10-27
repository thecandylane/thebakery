// Small decisions behind the shell's panes: the native Electron views (Code
// and Preview panels) and the terminal panel's splits. Pure, so
// viewModel.check.ts runs them on plain node.

/** Terminals shown side by side, as groups of ids: the kept groups narrowed
 *  to the terminals that still exist (an emptied group closes), then every
 *  other terminal on its own, in list order. */
export function syncGroups(groups: readonly (readonly string[])[], ids: readonly string[]): string[][] {
  const live = new Set(ids);
  const kept = groups.map((g) => g.filter((id) => live.has(id))).filter((g) => g.length > 0);
  const placed = new Set(kept.flat());
  return [...kept, ...ids.filter((id) => !placed.has(id)).map((id) => [id])];
}

/** Moves terminal `id` beside `target`, before or after it, in target's
 *  group; it leaves its own group, which closes once empty. */
export function splitBeside(
  groups: readonly (readonly string[])[],
  id: string,
  target: string,
  side: "before" | "after",
): string[][] {
  if (id === target) return groups.map((g) => [...g]);
  const out = groups.map((g) => g.filter((x) => x !== id)).filter((g) => g.length > 0);
  const at = out.findIndex((g) => g.includes(target));
  if (at < 0) return [...out, [id]];
  const g = [...out[at]];
  g.splice(g.indexOf(target) + (side === "after" ? 1 : 0), 0, id);
  out[at] = g;
  return out;
}

type Rect = { left: number; top: number; right: number; bottom: number };

/** A native view paints above the page and cannot be clipped, so it shows
 *  only while its placeholder sits wholly inside the strip. */
export function fullyInside(r: Rect, box: Rect, slack = 2): boolean {
  return (
    r.right - r.left > 0 &&
    r.bottom - r.top > 0 &&
    r.left >= box.left - slack &&
    r.right <= box.right + slack &&
    r.top >= box.top - slack &&
    r.bottom <= box.bottom + slack
  );
}

/** A shortcut a focused view passed back ("Cmd+Shift+P", "Cmd+Alt+Left",
 *  "Cmd+\\", and "Ctrl+1"–"Ctrl+9" for the workspaces) as the keydown the
 *  page's own handlers already understand. */
export function shortcutInit(shortcut: string): KeyboardEventInit | null {
  const parts = shortcut.split("+");
  const last = parts.pop();
  const cmd = parts.includes("Cmd");
  const workspace = !cmd && parts.includes("Ctrl") && /^[1-9]$/.test(last ?? "");
  if (!last || !(cmd || workspace)) return null;
  const named: Record<string, string> = { Left: "ArrowLeft", Right: "ArrowRight" };
  const key = named[last] ?? (last.length === 1 ? last.toLowerCase() : null);
  if (!key) return null;
  if (workspace) return { key, ctrlKey: true, bubbles: true };
  return { key, metaKey: true, shiftKey: parts.includes("Shift"), altKey: parts.includes("Alt"), bubbles: true };
}
