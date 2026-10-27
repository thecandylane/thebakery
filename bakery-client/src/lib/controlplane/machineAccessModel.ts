// Settings → Machine access, as the screen reads and changes it: each Grants
// tab (the Baker, all projects, one project) and the Reach list. Changes
// mutate the policy in place; the panel saves it after each one.

import type {
  MachineAccess,
  MachineAccessPolicy,
  MachineAccessPreset,
  MachineEnvVar,
  MachineGrant,
  MachineGrantAccess,
  MachineScope,
  SocketInfo,
  StudioState,
} from "../node";

/** Whose grants a tab shows: the Baker's, every project's, or one project's own. */
export type Audience = { kind: "baker" } | { kind: "all" } | { kind: "project"; id: string };
type Shared = "baker" | "all";

/** A tool's tile in a tab: on, off, or on for every project already. */
export type ToolState = "on" | "off" | "inherited";

const scopeOf = (a: Shared): MachineScope => (a === "baker" ? "baker" : "agents");
const otherOf = (a: Shared): MachineScope => (a === "baker" ? "agents" : "baker");

export const reaches = (scope: MachineScope, a: Shared) => scope === "all_agents" || scope === scopeOf(a);

/** `~/…` for anything under the operator's home. */
export function tilde(path: string, home: string): string {
  if (!home) return path;
  if (path === home) return "~";
  return path.startsWith(`${home}/`) ? `~${path.slice(home.length)}` : path;
}

export function expand(path: string, home: string): string {
  if (path === "~") return home;
  return path.startsWith("~/") ? `${home}${path.slice(1)}` : path;
}

const toolGrant = (presets: MachineAccessPreset[], g: MachineGrant) =>
  presets.some((p) => p.grants.some((x) => x.path === g.path));
const toolVar = (presets: MachineAccessPreset[], v: MachineEnvVar) =>
  presets.some((p) => p.env.some((x) => x.name === v.name));

/** A project's own entries, made when first needed. */
export function ownOf(policy: MachineAccessPolicy, id: string): MachineAccess {
  policy.projects ??= {};
  return (policy.projects[id] ??= { grants: [], env: [] });
}

/** The folders a tab lists: by hand, a tool's own sit on its tile.
 *  `inherited` is what a project gets from all projects. */
export function folders(policy: MachineAccessPolicy, presets: MachineAccessPreset[], a: Audience) {
  const byHand = (g: MachineGrant) => !toolGrant(presets, g);
  if (a.kind !== "project") {
    return { inherited: [], own: policy.grants.filter((g) => byHand(g) && reaches(g.scope, a.kind)) };
  }
  return {
    inherited: policy.grants.filter((g) => byHand(g) && reaches(g.scope, "all")),
    own: (policy.projects?.[a.id]?.grants ?? []).filter(byHand),
  };
}

export function variables(policy: MachineAccessPolicy, presets: MachineAccessPreset[], a: Audience) {
  const byHand = (v: MachineEnvVar) => !toolVar(presets, v);
  if (a.kind !== "project") {
    return { inherited: [], own: policy.env.filter((v) => byHand(v) && reaches(v.scope, a.kind)) };
  }
  return {
    inherited: policy.env.filter((v) => byHand(v) && reaches(v.scope, "all")),
    own: (policy.projects?.[a.id]?.env ?? []).filter(byHand),
  };
}

function toolEntries(list: MachineAccess, p: MachineAccessPreset) {
  return {
    grants: list.grants.filter((d) => p.grants.some((x) => x.path === d.path)),
    env: list.env.filter((d) => p.env.some((x) => x.name === d.name)),
  };
}

export function toolState(policy: MachineAccessPolicy, p: MachineAccessPreset, a: Audience): ToolState {
  if (a.kind === "project") {
    if (toolState(policy, p, { kind: "all" }) === "on") return "inherited";
    const own = policy.projects?.[a.id];
    const mine = own ? toolEntries(own, p) : { grants: [], env: [] };
    return mine.grants.length || mine.env.length ? "on" : "off";
  }
  const kind = a.kind;
  const mine = toolEntries(policy, p);
  return [...mine.grants, ...mine.env].some((d) => reaches(d.scope, kind)) ? "on" : "off";
}

/** Turns a tool on or off for a tab. Shared with the other tab, it stays
 *  theirs when turned off here. */
export function setTool(policy: MachineAccessPolicy, p: MachineAccessPreset, a: Audience, on: boolean) {
  if (a.kind === "project") {
    const own = ownOf(policy, a.id);
    own.grants = own.grants.filter((d) => !p.grants.some((x) => x.path === d.path));
    own.env = own.env.filter((d) => !p.env.some((x) => x.name === d.name));
    if (on) {
      own.grants.push(...p.grants.map((g) => ({ ...g, scope: "agents" as const })));
      own.env.push(...p.env.map((v) => ({ ...v, scope: "agents" as const })));
    }
    return;
  }
  const [mine, theirs] = [scopeOf(a.kind), otherOf(a.kind)];
  if (on) {
    const join = <T extends { scope: MachineScope }>(have: T | undefined, add: () => void) => {
      if (!have) add();
      else if (have.scope === theirs) have.scope = "all_agents";
    };
    for (const g of p.grants) join(policy.grants.find((d) => d.path === g.path), () => policy.grants.push({ ...g, scope: mine }));
    for (const v of p.env) join(policy.env.find((d) => d.name === v.name), () => policy.env.push({ ...v, scope: mine }));
    return;
  }
  const drop = <T extends { scope: MachineScope }>(list: T[], of: (d: T) => boolean) =>
    list.flatMap((d) => (!of(d) || d.scope === theirs ? [d] : d.scope === "all_agents" ? [{ ...d, scope: theirs }] : []));
  policy.grants = drop(policy.grants, (d) => p.grants.some((x) => x.path === d.path));
  policy.env = drop(policy.env, (d) => p.env.some((x) => x.name === d.name));
}

/** Whether a Baker or all-projects entry reaches the other tab too. */
export const shared = (entry: { scope: MachineScope }) => entry.scope === "all_agents";

export function toggleShared(entry: { scope: MachineScope }, a: Shared) {
  entry.scope = shared(entry) ? scopeOf(a) : "all_agents";
}

/** Removes an entry from a tab: shared, it stays the other tab's. */
export function remove<T extends { scope: MachineScope }>(list: T[], entry: T, a: Audience): T[] {
  if (a.kind !== "project" && shared(entry)) {
    entry.scope = otherOf(a.kind);
    return list;
  }
  return list.filter((d) => d !== entry);
}

/** Leaving Run takes it off PATH (only a run grant goes there); coming to
 *  Run puts it on, as a new run grant is. */
export function setAccess(g: MachineGrant, access: MachineGrantAccess) {
  if (access === g.access) return;
  g.on_path = access === "exec";
  g.access = access;
}

export function addFolder(policy: MachineAccessPolicy, a: Audience, raw: string, access: MachineGrantAccess, home: string) {
  const path = raw.trim();
  if (!path) return;
  const full = expand(path, home);
  const fresh = (scope: MachineScope): MachineGrant => ({ path, access, scope, on_path: access === "exec", preset: null });
  if (a.kind === "project") {
    const own = ownOf(policy, a.id);
    if (!own.grants.some((g) => g.path === full)) own.grants.push(fresh("agents"));
    return;
  }
  const have = policy.grants.find((g) => g.path === full);
  if (!have) policy.grants.push(fresh(scopeOf(a.kind)));
  else if (!reaches(have.scope, a.kind)) have.scope = "all_agents";
}

/** A name is set once per list: one the other tab has becomes shared. */
export function addVariable(policy: MachineAccessPolicy, a: Audience, rawName: string, value: string) {
  const name = rawName.trim();
  if (!name) return;
  const list = a.kind === "project" ? ownOf(policy, a.id).env : policy.env;
  const have = list.find((v) => v.name === name);
  if (!have) {
    list.push({ name, value, scope: a.kind === "project" ? "agents" : scopeOf(a.kind), preset: null });
    return;
  }
  have.value = value;
  if (a.kind !== "project" && !reaches(have.scope, a.kind)) have.scope = "all_agents";
}

/** The tab's own count: its tools on, its folders and its variables. */
export function count(policy: MachineAccessPolicy, presets: MachineAccessPreset[], a: Audience): number {
  const tools = presets.filter((p) => !p.unavailable && toolState(policy, p, a) === "on").length;
  return tools + folders(policy, presets, a).own.length + variables(policy, presets, a).own.length;
}

// ─── Reach ──────────────────────────────────────────────────────────────────

export type ReachRow = { stream_id: string; label: string; depth: number; orc: boolean };
export type ReachGroup = { key: string; label: string; baker: boolean; rows: ReachRow[] };

/** Every open conversation by where it belongs: the Baker's, the Bakery's
 *  own workspaces, each project's, then chats; each opened one under the
 *  one that opened it. */
export function reachGroups(studio: StudioState): ReachGroup[] {
  const seen = new Set<string>();
  const open: SocketInfo[] = [];
  for (const s of [...studio.factory_sockets, ...studio.projects.flatMap((p) => p.sockets), ...studio.ungrouped_sockets]) {
    if (seen.has(s.stream_id) || s.discarded || s.archived || s.baker_thread?.archived) continue;
    seen.add(s.stream_id);
    open.push(s);
  }
  const ids = new Set(open.map((s) => s.stream_id));
  const children = new Map<string, SocketInfo[]>();
  for (const s of open) {
    if (s.opened_by && ids.has(s.opened_by)) children.set(s.opened_by, [...(children.get(s.opened_by) ?? []), s]);
  }
  const label = (s: SocketInfo) =>
    s.baker_thread ? (s.baker_thread.main ? "Baker" : s.baker_thread.title) : (s.title ?? s.name);
  const rows = (s: SocketInfo, depth: number): ReachRow[] => [
    { stream_id: s.stream_id, label: label(s), depth, orc: !!s.workspace },
    ...(children.get(s.stream_id) ?? []).flatMap((c) => rows(c, depth + 1)),
  ];

  const roots = open.filter((s) => !(s.opened_by && ids.has(s.opened_by)));
  const baker = roots
    .filter((s) => s.baker_thread)
    .sort((x, y) => Number(!!y.baker_thread?.main) - Number(!!x.baker_thread?.main));
  const ours = roots.filter((s) => !s.baker_thread && s.workspace && !s.project_id);
  const groups: ReachGroup[] = [
    { key: "baker", label: "Baker", baker: true, rows: baker.flatMap((s) => rows(s, 0)) },
    { key: "workspaces", label: "Bakery workspaces", baker: false, rows: ours.flatMap((s) => rows(s, 0)) },
  ];
  for (const p of studio.projects) {
    const theirs = roots.filter((s) => !s.baker_thread && s.project_id === p.project_id);
    groups.push({ key: `project:${p.project_id}`, label: p.name, baker: false, rows: theirs.flatMap((s) => rows(s, 0)) });
  }
  const placed = new Set(groups.flatMap((g) => g.rows.map((r) => r.stream_id)));
  const chats = roots.filter((s) => !placed.has(s.stream_id));
  groups.push({ key: "chats", label: "Chats", baker: false, rows: chats.flatMap((s) => rows(s, 0)) });
  return groups.filter((g) => g.rows.length);
}
