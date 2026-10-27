// The shell's layout as data: workspaces, each a horizontal strip of columns,
// each column a stack of panels (niri's model, as in jcode desktop), plus the
// Baker's own workspace and the sidebar. A panel is named by an id: a
// conversation's stream id, or "@tool" for a tool panel such as
// "@containers". Each id has at most one panel per workspace.
//
// Pure and DOM-free on purpose: layout.check.ts tests it with plain node, and
// the Baker can later read and write this same JSON. Every operation returns
// the input object itself when nothing changed, so callers can compare by
// reference before re-rendering or saving.

export type Column = {
  id: string;
  width: number;
  panels: string[];
  /** In screen view a column is a group of tabs: the one it shows. */
  active?: string;
};
export type Workspace = {
  id: string;
  name: string;
  /** Set for the default one-per-project workspaces; null when made by hand. */
  projectId: string | null;
  columns: Column[];
  /** The focused panel's stream id. */
  focus: string | null;
  /** Tab groups (the default), or the niri strip of columns. */
  view?: "screen" | "strip";
  /** Panels locked open: they show no close button, and closePanel leaves them. */
  locked?: string[];
  /** An orc's workspace: the orc's stream id. On the Baker's side for the
   *  Bakery's own (`projectId` null), in Bakery mode under its project. */
  orc?: string;
};
export type Layout = {
  version: 1;
  /** Which side the window shows: a Bakery workspace, or the Baker's own. */
  mode: "bakery" | "baker";
  /** The Bakery workspace shown in Bakery mode. */
  active: string | null;
  workspaces: Workspace[];
  /** The Baker's own workspace: its threads and whatever it opens. */
  baker: Workspace;
  /** Every orc's workspace, each with tabs of its own and its workers'
   *  conversations: the Bakery's own on the Baker's side, a project's in
   *  Bakery mode under its project. */
  orcs: Workspace[];
  /** Which workspace Baker mode shows: an orc's, else the Baker's own. */
  bakerShown?: string;
  /** Projects on the Baker's side (the Bakery's own repo): they show in
   *  Baker mode and get no Bakery workspace, which is for everything else. */
  bakerProjects: string[];
  /** `mode` lists a workspace's agents flat, or grouped by worktree;
   *  `rail`: dragged past its narrowest, it is an icon rail (Round 10). */
  sidebar: { open: boolean; width: number; mode?: "agents" | "worktrees"; rail?: boolean };
  /** The bottom panel: the focused agent's terminals. */
  bottom?: { open: boolean; height: number };
};
export type Where = "before" | "after" | "below";

export const COLUMN_MIN = 320;
export const COLUMN_MAX = 1400;
export const COLUMN_DEFAULT = 560;
/** The Baker workspace's id; every operation below takes it like any other. */
export const BAKER = "baker";
/** A Bakery workspace's control plane: its first tab, locked open. On the
 *  Baker's side it is the Baker's home page instead, never a tab. */
export const CONTROL = "@control";
/** An orc's workspace's id: this, then the orc's stream id. */
export const ORC = "orc:";
export const orcWorkspace = (streamId: string) => `${ORC}${streamId}`;

const shortId = () => crypto.randomUUID().slice(0, 8);

/** A Bakery workspace opens on its control plane: first, and locked. */
export function withControl(w: Workspace): Workspace {
  const locked = w.locked?.includes(CONTROL) ? w.locked : [...(w.locked ?? []), CONTROL];
  if (w.columns.some((c) => c.panels.includes(CONTROL))) return locked === w.locked ? w : { ...w, locked };
  const columns = w.columns.length
    ? [{ ...w.columns[0], panels: [CONTROL, ...w.columns[0].panels] }, ...w.columns.slice(1)]
    : [{ id: shortId(), width: COLUMN_DEFAULT, panels: [CONTROL] }];
  return { ...w, columns, focus: w.focus ?? CONTROL, locked };
}

/** The Baker's workspace without a control tab: its control plane is its home
 *  page now, where a layout saved before kept it as a locked first tab. */
function withoutControl(w: Workspace): Workspace {
  const open = detach(w, CONTROL);
  return {
    ...open,
    focus: open.focus === CONTROL ? null : open.focus,
    ...(Array.isArray(open.locked) ? { locked: open.locked.filter((id) => id !== CONTROL) } : {}),
  };
}

/** A hand-made workspace becomes the home of the project made from it. The
 *  workspace a sync may already have made for that project goes. */
export function assignProject(l: Layout, wsId: string, projectId: string, name: string): Layout {
  const workspaces = l.workspaces
    .filter((w) => w.id === wsId || w.projectId !== projectId)
    .map((w) => (w.id === wsId ? { ...w, projectId, name } : w));
  return { ...l, workspaces, active: activeOf(l, workspaces) ?? wsId };
}

/** The Bakery workspace shown, while it is still there (a project's orc's
 *  counts), else the first. */
function activeOf(l: Layout, workspaces: readonly Workspace[], orcs: readonly Workspace[] = l.orcs): string | null {
  const still = workspaces.some((w) => w.id === l.active) || orcs.some((w) => w.id === l.active && !!w.projectId);
  return l.active && still ? l.active : (workspaces[0]?.id ?? null);
}
const clampWidth = (w: number) => Math.round(Math.min(COLUMN_MAX, Math.max(COLUMN_MIN, w)));
const bakerWorkspace = (): Workspace => ({ id: BAKER, name: "Baker", projectId: null, columns: [], focus: null });

export function emptyLayout(): Layout {
  return {
    version: 1,
    mode: "bakery",
    active: null,
    workspaces: [],
    baker: bakerWorkspace(),
    orcs: [],
    bakerProjects: [],
    sidebar: { open: true, width: 272 },
  };
}

/** JSON with object keys sorted: the same layout always prints the same,
 *  whatever order the node stored its keys in. */
export function canon(value: unknown): string {
  return JSON.stringify(value, (_key, v) =>
    v && typeof v === "object" && !Array.isArray(v)
      ? Object.fromEntries(Object.entries(v).sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0)))
      : v,
  );
}

const validWorkspace = (w: Workspace | undefined) =>
  typeof w?.id === "string" && Array.isArray(w.columns) && w.columns.every((c) => Array.isArray(c?.panels));

/** A saved layout, or an empty one when the text is missing or not ours. A
 *  layout saved before the Baker had a workspace gets an empty one (its old
 *  `dock` key is ignored); the Baker's loses the control tab it once kept. */
export function parseLayout(raw: string | null): Layout {
  try {
    const l = JSON.parse(raw ?? "null") as Partial<Layout> | null;
    if (l?.version !== 1 || !Array.isArray(l.workspaces)) return emptyLayout();
    if (!l.workspaces.every(validWorkspace) || (l.baker !== undefined && !validWorkspace(l.baker))) return emptyLayout();
    return {
      ...emptyLayout(),
      ...l,
      mode: l.mode === "baker" ? "baker" : "bakery",
      baker: l.baker ? withoutControl({ ...l.baker, id: BAKER }) : bakerWorkspace(),
      orcs: Array.isArray(l.orcs) ? l.orcs.filter((w) => validWorkspace(w) && typeof w.orc === "string") : [],
      bakerShown: typeof l.bakerShown === "string" ? l.bakerShown : undefined,
      bakerProjects: Array.isArray(l.bakerProjects) ? l.bakerProjects.filter((id) => typeof id === "string") : [],
    } as Layout;
  } catch {
    return emptyLayout();
  }
}

/** The workspace the window shows. */
export function shown(l: Layout): Workspace | null {
  if (l.mode === "baker") return l.orcs.find((w) => w.id === l.bakerShown && !w.projectId) ?? l.baker;
  return l.workspaces.find((w) => w.id === l.active) ?? l.orcs.find((w) => w.id === l.active && !!w.projectId) ?? null;
}

/** A workspace by its id, on either side. */
export function workspaceOf(l: Layout, wsId: string): Workspace | null {
  if (wsId === BAKER) return l.baker;
  return (wsId.startsWith(ORC) ? l.orcs : l.workspaces).find((w) => w.id === wsId) ?? null;
}

function mapWorkspace(l: Layout, wsId: string, f: (w: Workspace) => Workspace): Layout {
  if (wsId === BAKER) {
    const next = f(l.baker);
    return next === l.baker ? l : { ...l, baker: next };
  }
  if (wsId.startsWith(ORC)) {
    const i = l.orcs.findIndex((w) => w.id === wsId);
    const next = i < 0 ? null : f(l.orcs[i]);
    if (!next || next === l.orcs[i]) return l;
    const orcs = l.orcs.slice();
    orcs[i] = next;
    return { ...l, orcs };
  }
  const i = l.workspaces.findIndex((w) => w.id === wsId);
  if (i < 0) return l;
  const next = f(l.workspaces[i]);
  if (next === l.workspaces[i]) return l;
  const workspaces = l.workspaces.slice();
  workspaces[i] = next;
  return { ...l, workspaces };
}

const columnOf = (w: Workspace, streamId: string) => w.columns.findIndex((c) => c.panels.includes(streamId));

/** The tab a group shows: its own pick, else the focus when it is there,
 *  else its first. */
export function tabOf(c: Column, focus: string | null): string {
  if (c.active && c.panels.includes(c.active)) return c.active;
  return focus && c.panels.includes(focus) ? focus : c.panels[0];
}

/** Focuses a panel that is open here, and shows it in its group. */
function focusIn(w: Workspace, streamId: string): Workspace {
  const ci = columnOf(w, streamId);
  if (ci < 0 || (w.focus === streamId && tabOf(w.columns[ci], w.focus) === streamId)) return w;
  const columns = w.columns.slice();
  columns[ci] = { ...columns[ci], active: streamId };
  return { ...w, columns, focus: streamId };
}

/** One workspace per project, added (never removed) as projects appear;
 *  a project workspace keeps its project's name. The Baker's projects get
 *  none. */
/** The workspace a thread belongs in: the Baker's for its threads and for
 *  the Bakery's own projects, else its project's workspace. Null for a thread
 *  with no project, or one whose project has no workspace. */
/** An orc's workspace, home to the orc and to its workers, when it has one. */
export function orcHome(l: Layout, orc: string | null): string | null {
  return orc && l.orcs.some((w) => w.orc === orc) ? orcWorkspace(orc) : null;
}

/** Gives every orc a workspace, named as the orc's workspace and opening on
 *  the orc's own conversation: the Bakery's own on the Baker's side, a
 *  project's (`projectId`) in Bakery mode. A new one goes last. An orc no
 *  longer listed takes its workspace with it, so only a loaded list may be
 *  given. */
export function syncOrcs(l: Layout, orcs: readonly { streamId: string; title: string; projectId?: string | null }[]): Layout {
  const wanted = new Map(orcs.map((o) => [orcWorkspace(o.streamId), o]));
  const kept = l.orcs
    .filter((w) => wanted.has(w.id))
    .map((w) => {
      const title = wanted.get(w.id)?.title || w.name;
      return w.name === title ? w : { ...w, name: title };
    });
  const added = orcs
    .filter((o) => !l.orcs.some((w) => w.id === orcWorkspace(o.streamId)))
    .map((o): Workspace => ({
      id: orcWorkspace(o.streamId),
      name: o.title,
      projectId: o.projectId ?? null,
      orc: o.streamId,
      columns: [{ id: shortId(), width: COLUMN_DEFAULT, panels: [o.streamId] }],
      focus: o.streamId,
    }));
  const next = [...kept, ...added];
  if (next.length === l.orcs.length && next.every((w, i) => w === l.orcs[i])) return l;
  const bakerShown = next.some((w) => w.id === l.bakerShown && !w.projectId) ? l.bakerShown : undefined;
  return { ...l, orcs: next, bakerShown, active: activeOf(l, l.workspaces, next) };
}

/** A new orc's workspace, shown at once, before the node lists the orc. */
export function showNewOrc(l: Layout, orc: { streamId: string; title: string; projectId?: string | null }): Layout {
  const known = l.orcs.filter((w) => w.orc && w.orc !== orc.streamId).map((w) => ({ streamId: w.orc as string, title: w.name, projectId: w.projectId }));
  return setActive(syncOrcs(l, [...known, orc]), orcWorkspace(orc.streamId));
}

/** The workspace of an orc still being made: shown (showNewOrc) before the
 *  node lists its thread. `listed` is the node's last list, null before the
 *  first: a thread it named that is missing now is gone, not being made. */
export function orcBeingMade(l: Layout, streamId: string, listed: ReadonlySet<string> | null): Workspace | null {
  if (!listed || listed.has(streamId)) return null;
  return l.orcs.find((w) => w.orc === streamId) ?? null;
}

export function homeOf(l: Layout, projectId: string | null, bakerThread: boolean): string | null {
  if (bakerThread) return BAKER;
  if (!projectId) return null;
  if (l.bakerProjects.includes(projectId)) return BAKER;
  return l.workspaces.find((w) => w.projectId === projectId)?.id ?? null;
}

/** Gives every project a workspace. `own`, the Bakery's own project, goes to
 *  the Baker's side instead when it first appears; once it has a workspace,
 *  where it sits is the operator's to change. */
export function syncProjects(l: Layout, projects: readonly { id: string; name: string }[], own: string | null = null): Layout {
  // Every Bakery workspace keeps its control plane, first and locked.
  const controlled = l.workspaces.map(withControl);
  let workspaces = controlled.some((w, i) => w !== l.workspaces[i]) ? controlled : l.workspaces;
  let bakerProjects = l.bakerProjects;
  for (const p of projects) {
    if (bakerProjects.includes(p.id)) continue;
    const i = workspaces.findIndex((w) => w.projectId === p.id);
    if (i < 0 && p.id === own) {
      bakerProjects = [...bakerProjects, p.id];
    } else if (i < 0) {
      workspaces = [...workspaces, withControl({ id: `project:${p.id}`, name: p.name, projectId: p.id, columns: [], focus: null })];
    } else if (workspaces[i].name !== p.name) {
      workspaces = workspaces.slice();
      workspaces[i] = { ...workspaces[i], name: p.name };
    }
  }
  const active = activeOf(l, workspaces);
  return workspaces === l.workspaces && active === l.active && bakerProjects === l.bakerProjects
    ? l
    : { ...l, workspaces, active, bakerProjects };
}

/** Puts a project on the Baker's side (the Bakery's own repo, worked on with
 *  the Baker), taking its Bakery workspace away; or back, where the next
 *  `syncProjects` gives it a workspace again. */
export function moveProject(l: Layout, projectId: string, side: "baker" | "bakery"): Layout {
  const has = l.bakerProjects.includes(projectId);
  if (side === "bakery") return has ? { ...l, bakerProjects: l.bakerProjects.filter((id) => id !== projectId) } : l;
  if (has) return l;
  const workspaces = l.workspaces.filter((w) => w.projectId !== projectId);
  return { ...l, bakerProjects: [...l.bakerProjects, projectId], workspaces, active: activeOf(l, workspaces) };
}

/** A project removed from the Bakery: its workspace goes (syncProjects only
 *  ever adds them), and the Baker no longer counts it as its own. */
export function dropProject(l: Layout, projectId: string): Layout {
  const workspaces = l.workspaces.filter((w) => w.projectId !== projectId);
  const orcs = l.orcs.filter((w) => w.projectId !== projectId);
  const bakerProjects = l.bakerProjects.filter((id) => id !== projectId);
  if (workspaces.length === l.workspaces.length && orcs.length === l.orcs.length && bakerProjects.length === l.bakerProjects.length) return l;
  return { ...l, workspaces, orcs, bakerProjects, active: activeOf(l, workspaces, orcs) };
}

/** Every project the node no longer lists (a fresh start, a delete, another
 *  window's) dropped as `dropProject` drops one. Only the node's own list
 *  may be given, never a copy kept from before. */
export function dropGone(l: Layout, projects: readonly { id: string }[]): Layout {
  const known = new Set(projects.map((p) => p.id));
  const named = [...l.workspaces, ...l.orcs].map((w) => w.projectId).concat(l.bakerProjects);
  return [...new Set(named)].reduce((at, id) => (id === null || known.has(id) ? at : dropProject(at, id)), l);
}

export function setMode(l: Layout, mode: Layout["mode"]): Layout {
  return l.mode === mode ? l : { ...l, mode };
}

/** Shows a workspace: a Bakery one (in Bakery mode: a project's, a
 *  hand-made one, a project orc's), or one of the Baker's side (in Baker
 *  mode): the Baker's own, or the Bakery's own orc's. */
export function setActive(l: Layout, wsId: string): Layout {
  if (wsId === BAKER || wsId.startsWith(ORC)) {
    const orc = l.orcs.find((w) => w.id === wsId);
    if (wsId !== BAKER && !orc) return l;
    if (orc?.projectId) return l.mode === "bakery" && l.active === wsId ? l : { ...l, mode: "bakery", active: wsId };
    const bakerShown = wsId === BAKER ? undefined : wsId;
    return l.mode === "baker" && l.bakerShown === bakerShown ? l : { ...l, mode: "baker", bakerShown };
  }
  if (!l.workspaces.some((w) => w.id === wsId)) return l;
  return l.active === wsId ? setMode(l, "bakery") : { ...l, active: wsId, mode: "bakery" };
}

/** Whether a workspace shows the Baker's home page, its control plane: the
 *  Baker's own, with nothing focused. */
export const isHome = (w: Workspace | null) => w?.id === BAKER && !w.focus;

/** A workspace's control plane, shown: a Bakery workspace's first tab, or the
 *  Baker's home page, which is no tab and leaves its tabs as they were. */
export function showControl(l: Layout, wsId: string): Layout {
  if (wsId === BAKER) return setActive(mapWorkspace(l, BAKER, (w) => (w.focus ? { ...w, focus: null } : w)), BAKER);
  return setActive(focusPanel(mapWorkspace(l, wsId, withControl), wsId, CONTROL), wsId);
}

/** Where the window is, for back and forward: the workspace shown and its
 *  focus, as one string (the Baker's home page has none). */
export function placeOf(l: Layout): string {
  const w = shown(l);
  return JSON.stringify([w?.id ?? null, w?.focus ?? null]);
}

/** Shows a place again; nothing open is the Bakery's floor. */
export function toPlace(l: Layout, place: string): Layout {
  const [wsId, focus] = JSON.parse(place) as [string | null, string | null];
  if (!wsId) return setMode(l, "bakery");
  if (wsId === BAKER && !focus) return showControl(l, BAKER);
  const shownThere = setActive(l, wsId);
  return focus ? focusPanel(shownThere, wsId, focus) : shownThere;
}

/** The nearest place in `stack` back (-1) or forward (1) of `at` that would
 *  change what shows, or -1. One whose workspace is gone (a fresh start, a
 *  delete) is passed over, so back and forward are never on with nowhere to go. */
export function nearPlace(l: Layout, stack: readonly string[], at: number, dir: -1 | 1): number {
  const here = placeOf(l);
  for (let i = at + dir; i >= 0 && i < stack.length; i += dir) {
    if (placeOf(toPlace(l, stack[i])) !== here) return i;
  }
  return -1;
}

export function addWorkspace(l: Layout, name: string): Layout {
  const id = `ws:${shortId()}`;
  return {
    ...l,
    mode: "bakery",
    active: id,
    workspaces: [...l.workspaces, withControl({ id, name, projectId: null, columns: [], focus: null })],
  };
}

export function renameWorkspace(l: Layout, wsId: string, name: string): Layout {
  return mapWorkspace(l, wsId, (w) => (w.name === name || !name.trim() ? w : { ...w, name: name.trim() }));
}

/** Only hand-made workspaces go; a project's comes back with its project. */
export function removeWorkspace(l: Layout, wsId: string): Layout {
  const w = l.workspaces.find((x) => x.id === wsId);
  if (!w || w.projectId !== null) return l;
  const workspaces = l.workspaces.filter((x) => x.id !== wsId);
  return { ...l, workspaces, active: l.active === wsId ? (workspaces[0]?.id ?? null) : l.active };
}

export function focusPanel(l: Layout, wsId: string, streamId: string): Layout {
  return mapWorkspace(l, wsId, (w) => focusIn(w, streamId));
}

export function setView(l: Layout, wsId: string, view: "screen" | "strip"): Layout {
  return mapWorkspace(l, wsId, (w) => ((w.view ?? "screen") === view ? w : { ...w, view }));
}

/** Panels a workspace has one of, whichever worktree they look at: Code,
 *  Preview and the Code editor. */
const ONE_EACH = ["@code:", "@preview:", "@editor:"];

/** The panel `from` looking at `to` instead, where it stands. */
function retarget(w: Workspace, from: string, to: string): Workspace {
  const swap = (id: string) => (id === from ? to : id);
  return {
    ...w,
    columns: w.columns.map((c) => (c.panels.includes(from) ? { ...c, panels: c.panels.map(swap), active: c.active && swap(c.active) } : c)),
    focus: w.focus && swap(w.focus),
    ...(w.locked ? { locked: w.locked.map(swap) } : {}),
  };
}

/** Focus the panel if it is open here. Otherwise open it: in screen view a
 *  tab beside the focused one (with nothing focused, the Baker's home page:
 *  last in the first group), in the strip a new column right after it.
 *  A Code, Preview or Code editor panel the workspace already has turns to
 *  it instead.
 *  Either way the workspace becomes active. */
export function openPanel(l: Layout, wsId: string, streamId: string, width = COLUMN_DEFAULT): Layout {
  const opened = mapWorkspace(l, wsId, (w) => {
    if (columnOf(w, streamId) >= 0) return focusIn(w, streamId);
    const kind = ONE_EACH.find((k) => streamId.startsWith(k));
    const had = kind && w.columns.flatMap((c) => c.panels).find((p) => p.startsWith(kind));
    if (had) return focusIn(retarget(w, had, streamId), streamId);
    const fi = w.focus ? columnOf(w, w.focus) : -1;
    const columns = w.columns.slice();
    const gi = Math.max(fi, 0);
    if ((w.view ?? "screen") === "screen" && columns[gi]) {
      const panels = columns[gi].panels.slice();
      panels.splice(fi >= 0 ? panels.indexOf(w.focus as string) + 1 : panels.length, 0, streamId);
      columns[gi] = { ...columns[gi], panels, active: streamId };
    } else {
      columns.splice(fi >= 0 ? fi + 1 : columns.length, 0, { id: shortId(), width: clampWidth(width), panels: [streamId] });
    }
    return { ...w, columns, focus: streamId };
  });
  return setActive(opened, wsId);
}

/** Takes the panel out of its column (dropping the column once empty). */
function detach(w: Workspace, streamId: string): Workspace {
  const ci = columnOf(w, streamId);
  if (ci < 0) return w;
  const col = w.columns[ci];
  const panels = col.panels.filter((p) => p !== streamId);
  const columns = w.columns.slice();
  if (panels.length) columns[ci] = { ...col, panels, active: col.active === streamId ? undefined : col.active };
  else columns.splice(ci, 1);
  return { ...w, columns };
}

/** Whether a panel is locked open in a workspace. */
export const isLocked = (w: Workspace, id: string) => Array.isArray(w.locked) && w.locked.includes(id);

/** Locks a panel open so it can't be closed by accident, or unlocks it. */
export function toggleLock(l: Layout, wsId: string, id: string): Layout {
  return mapWorkspace(l, wsId, (w) => {
    const locked = Array.isArray(w.locked) ? w.locked : [];
    return { ...w, locked: locked.includes(id) ? locked.filter((x) => x !== id) : [...locked, id] };
  });
}

export function closePanel(l: Layout, wsId: string, streamId: string): Layout {
  return mapWorkspace(l, wsId, (w) => {
    const ci = columnOf(w, streamId);
    if (ci < 0 || isLocked(w, streamId)) return w;
    const next = detach(w, streamId);
    if (w.focus !== streamId) return next;
    // Focus moves to the panel that took its place in the same column (the
    // tab to its right, or the last), else the column that now sits where
    // this one was, else the one before it.
    const at = w.columns[ci].panels.indexOf(streamId);
    const sameCol = next.columns.find((c) => c.id === w.columns[ci].id);
    const neighbour =
      sameCol?.panels[Math.min(at, sameCol.panels.length - 1)] ??
      (next.columns[ci] ?? next.columns[ci - 1])?.panels[0] ??
      null;
    return neighbour ? focusIn({ ...next, focus: null }, neighbour) : { ...next, focus: null };
  });
}

/** A conversation that's gone, closed everywhere: its panels and those of
 *  tools on its worktree (`@code:<id>` and the like), locked ones too. */
export function closeEverywhere(l: Layout, streamId: string): Layout {
  const its = (id: string) => id === streamId || (id.startsWith("@") && id.slice(id.indexOf(":") + 1) === streamId);
  return [l.baker, ...l.workspaces, ...l.orcs].reduce((at, w) => {
    const ids = w.columns.flatMap((c) => c.panels).filter(its);
    if (ids.length === 0) return at;
    const free = mapWorkspace(at, w.id, (x) => ({ ...x, locked: x.locked?.filter((id) => !its(id)) }));
    return ids.reduce((acc, id) => closePanel(acc, w.id, id), free);
  }, l);
}

/** Every conversation panel `isGone` says is gone, closed everywhere, locked
 *  ones too: a thread deleted by anything but this window's own Delete (the
 *  Baker's tool, another window) must not leave a "Gone" tab that can't be
 *  closed. Tool panels (`@…`) are not conversations. */
export function closeGone(l: Layout, isGone: (streamId: string) => boolean): Layout {
  const gone = new Set(
    [l.baker, ...l.workspaces, ...l.orcs].flatMap((w) => w.columns.flatMap((c) => c.panels)).filter((id) => !id.startsWith("@") && isGone(id)),
  );
  return [...gone].reduce(closeEverywhere, l);
}

/** Drops `streamId` beside `target`: a new column before or after the
 *  target's column, or stacked below the target inside its column. */
export function movePanel(l: Layout, wsId: string, streamId: string, target: string, where: Where): Layout {
  if (streamId === target) return l;
  return mapWorkspace(l, wsId, (w) => {
    const from = columnOf(w, streamId);
    if (from < 0 || columnOf(w, target) < 0) return w;
    const width = w.columns[from].width;
    const rest = detach(w, streamId);
    const ti = columnOf(rest, target);
    const columns = rest.columns.slice();
    if (where === "below") {
      const panels = columns[ti].panels.slice();
      panels.splice(panels.indexOf(target) + 1, 0, streamId);
      columns[ti] = { ...columns[ti], panels, active: streamId };
    } else {
      columns.splice(where === "before" ? ti : ti + 1, 0, { id: shortId(), width, panels: [streamId] });
    }
    return { ...rest, columns, focus: streamId };
  });
}

export function resizeColumn(l: Layout, wsId: string, columnId: string, width: number): Layout {
  return mapWorkspace(l, wsId, (w) => {
    const i = w.columns.findIndex((c) => c.id === columnId);
    if (i < 0 || w.columns[i].width === clampWidth(width)) return w;
    const columns = w.columns.slice();
    columns[i] = { ...columns[i], width: clampWidth(width) };
    return { ...w, columns };
  });
}

/** Focus the first panel of the column left (-1) or right (+1) of the focus. */
export function focusNeighbour(l: Layout, wsId: string, dir: -1 | 1): Layout {
  return mapWorkspace(l, wsId, (w) => {
    const ci = w.focus ? columnOf(w, w.focus) : -1;
    const next = w.columns[ci < 0 ? 0 : ci + dir];
    return next && next.panels[0] !== w.focus ? { ...w, focus: next.panels[0] } : w;
  });
}
