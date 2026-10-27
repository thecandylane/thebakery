// Pane/layout persistence for the workbench.
//
// Scoped per session (keyed by stream id) so switching sessions does not
// bleed one workbench's layout into another. localStorage is guarded because
// private browsing / disabled storage / a webview without it must degrade to
// sane defaults, never throw and take the workbench down with it.

// ---- Studio dock layout (07c-STUDIO-SHELL) ----------------------------------
//
// Dockview owns the layout grammar (`SerializedDockview` from the `dockview`
// package) — this module only persists/restores its own serialized JSON per
// socket, the same per-stream key the old fixed a/b/c layout used. A value
// saved by that earlier shape (`{a,b,c,leftW,topH}`) fails the shape check
// below and is treated as absent — a one-time, harmless layout reset, not a
// migration worth writing for a pre-release local app. `LAYOUT_KEY_VERSION`
// below is the same one-time-reset idea applied a second time (07c-UX-SHELL).

// 07d-A-AGENT-IDENTITY: "orchestrator" is no longer a fixed singleton widget
// id — chat panels are multi-instance now, one per configured agent
// (`chat:{role_id}`, handled in DockviewHost.svelte the same way "terminal"
// already is), not a member of this closed set.
export const WIDGET_IDS = [
  "review", "events",
  "memory", "terminal", "code", "agents",
] as const;

export type WidgetId = (typeof WIDGET_IDS)[number];

// 07c-UX-SHELL: the first-use default changed from chat+code (or
// chat+review+events) to chat-alone. A layout saved under the old default
// would otherwise mask the new correction forever, so the key is versioned
// once — any pre-existing saved layout (generated default or genuine human
// rearrangement) is treated as absent exactly one time. Do not bump this
// again for ordinary product changes; only for another such one-time reset.
//
// 07d-A0: bumped again, v2 → v3. A v2 layout can serialize a panel with
// `component`/`id: "orchestrator"` — the fixed singleton chat panel from
// before 07d-A-AGENT-IDENTITY. That component no longer exists (chat is
// per-agent now, `chat:{role_id}`); restoring one would have hit
// DockviewHost's unknown-component fallback for every returning user's
// saved layout, not just the rare edge case it exists for. One more
// version bump routes every such socket back through the fresh, correct
// default layout instead.
const LAYOUT_KEY_VERSION = 3;
export const layoutKey = (id: string) => `bakery.layout.v${LAYOUT_KEY_VERSION}.${id}`;

/** Explicit escape hatch alongside the version bump above — a human who
 *  hits a broken/stuck layout (this version, a future one, or anything
 *  neither of those two mechanisms anticipated) can clear just their own
 *  socket's saved arrangement and fall back to the default layout, without
 *  needing to know this is a localStorage key at all. */
export function resetDockLayout(sessionId: string): void {
  try {
    localStorage.removeItem(layoutKey(sessionId));
  } catch {
    /* storage unavailable */
  }
}

/** Loosely-typed here on purpose: the real shape is `SerializedDockview`
 *  (from `dockview`), which this module doesn't need to import just to
 *  shuttle JSON in and out of storage. `DockviewHost.svelte` is the only
 *  caller and casts it back. */
function looksLikeSerializedDockview(v: unknown): v is Record<string, unknown> {
  if (typeof v !== "object" || v === null) return false;
  const o = v as Record<string, unknown>;
  return typeof o.grid === "object" && o.grid !== null && typeof o.panels === "object" && o.panels !== null;
}

export function loadDockLayout(sessionId: string): Record<string, unknown> | null {
  try {
    const raw = localStorage.getItem(layoutKey(sessionId));
    if (!raw) return null;
    const parsed = JSON.parse(raw);
    return looksLikeSerializedDockview(parsed) ? parsed : null;
  } catch {
    return null;
  }
}

export function saveDockLayout(sessionId: string, data: unknown): void {
  try {
    localStorage.setItem(layoutKey(sessionId), JSON.stringify(data));
  } catch {
    /* storage unavailable */
  }
}

// ---- Tree expansion (global, not per-stream) --------------------------------
// 07c-UX-SHELL §1.2: sidebar is flattened to Project → Socket — there is no
// branch level left to expand, so this only tracks which projects are open.

export type TreeExpansion = {
  projects: Set<string>;
  /**
   * False until the user toggles once. Distinguishes "never set" (apply
   * design default: expand all current projects) from "user collapsed
   * everything" (empty set, leave closed).
   */
  touched: boolean;
};

const TREE_PROJECTS_KEY = "bakery.tree.projects";
const TREE_TOUCHED_KEY = "bakery.tree.touched";

function loadSet(key: string): Set<string> {
  try {
    const raw = localStorage.getItem(key);
    if (!raw) return new Set();
    const arr = JSON.parse(raw);
    if (!Array.isArray(arr)) return new Set();
    return new Set(arr.filter((x): x is string => typeof x === "string"));
  } catch {
    return new Set();
  }
}

function loadTouched(): boolean {
  try {
    return localStorage.getItem(TREE_TOUCHED_KEY) === "1";
  } catch {
    return false;
  }
}

export function loadTreeExpansion(): TreeExpansion {
  return {
    projects: loadSet(TREE_PROJECTS_KEY),
    touched: loadTouched(),
  };
}

export function saveTreeExpansion(expansion: TreeExpansion): void {
  try {
    localStorage.setItem(TREE_PROJECTS_KEY, JSON.stringify([...expansion.projects]));
    // Any explicit save is a user interaction — never re-apply open-all default.
    localStorage.setItem(TREE_TOUCHED_KEY, "1");
  } catch {
    /* storage unavailable */
  }
}

// ---- Last-opened session (opaque handle, for rail Sessions target) ----------

const LAST_SESSION_KEY = "bakery.last.session";

/** Handle from `/w/<handle>` — not the raw stream_id. */
export function loadLastSessionHandle(): string | null {
  try {
    const h = localStorage.getItem(LAST_SESSION_KEY);
    return h && h.length > 0 ? h : null;
  } catch {
    return null;
  }
}

export function saveLastSessionHandle(handle: string): void {
  try {
    if (!handle) return;
    localStorage.setItem(LAST_SESSION_KEY, handle);
  } catch {
    /* storage unavailable */
  }
}
