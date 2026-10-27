// The toolkit every runtime gets (Round 11's T1, a control-plane card):
// skills, MCP servers, hooks, custom tools, Claude Code's slash commands,
// subagents and output styles, plugins and instructions, the Bakery's or
// this project's, and how each runtime gets each one — carried as it is,
// translated by the Bakery, or not yet. The operator adds, changes, turns on
// or off, and removes; agents only read. The VS Code tool port (the
// operator's words) brings in what their own Claude Code setup holds.

import { useEffect, useState } from "react";
import {
  ArrowDownToLineIcon,
  BotIcon,
  BoxIcon,
  FileTextIcon,
  PaletteIcon,
  PlugIcon,
  PlusIcon,
  ShieldIcon,
  SlashIcon,
  SparklesIcon,
  TerminalSquareIcon,
  WrenchIcon,
  XIcon,
} from "lucide-react";
import {
  type Carriage,
  type Found,
  type Imported,
  type Kind3,
  type PluginParts,
  toolkit,
  toolkitEnable,
  toolkitPort,
  toolkitPortSet,
  toolkitRemove,
  toolkitSet,
  type ToolkitView,
} from "$lib/api.generated";
import { ask } from "$lib/ask";
import { patchKept, useCached } from "./cache";
import { Card } from "./ControlCards";
import { PLAIN, SOLID } from "./Internet";
import type { ShellProject } from "./shellSource.svelte";
import { Chip, Freshness, said } from "./ui";

const KINDS: [Kind3, string, typeof PlugIcon, string][] = [
  ["mcp", "MCP servers", PlugIcon, "var(--violet)"],
  ["skill", "Skills", SparklesIcon, "var(--ok)"],
  ["hook", "Hooks", ShieldIcon, "var(--warn)"],
  ["tool", "Custom tools", TerminalSquareIcon, "var(--hue-containers)"],
  ["command", "Slash commands", SlashIcon, "var(--link)"],
  ["agent", "Subagents", BotIcon, "var(--violet)"],
  ["output_style", "Output styles", PaletteIcon, "var(--accent)"],
  ["plugin", "Plugins", BoxIcon, "var(--hue-containers)"],
  ["instructions", "Instructions", FileTextIcon, "var(--accent)"],
];
const KIND_WORD: Record<string, string> = Object.fromEntries(KINDS.map(([k, label]) => [k, label]));
/** Claude Code's own kinds: each one file, held whole. */
const WHOLE = new Set<Kind3>(["command", "agent", "output_style"]);
/** What the port found, in Claude Code's words. */
const FOUND: Record<string, string> = {
  instructions: "instructions",
  rule: "rule",
  skill: "skill",
  mcp_server: "MCP server",
  hook: "hook",
  plugin: "plugin",
  command: "slash command",
  agent: "subagent",
  output_style: "output style",
};
const FOUND_ORDER = Object.keys(FOUND);
const WHEN: [string, string][] = [
  ["pre_tool", "Before a tool runs"],
  ["post_tool", "After a tool runs"],
];
const TYPES = ["string", "number", "integer", "boolean"];
const RUNTIMES: [string, string][] = [
  ["claude", "Claude"],
  ["codex", "Codex"],
  ["grok", "Grok"],
  ["gemini", "Gemini"],
  ["api", "API"],
  ["local", "Local"],
];
/** How a runtime gets an item: its mark and colour. */
const HOW: Record<string, [string, string]> = {
  carried: ["✓", "var(--ok)"],
  translated: ["✓", "var(--accent)"],
  not_yet: ["–", "var(--fg-faint)"],
};
const SEG = (on: boolean) =>
  `h-6 px-2 text-[11px] ${on ? "bg-(--accent-sel) font-bold text-(--fg)" : "text-(--fg-dim) hover:text-(--fg-2)"}`;
const FIELD = "h-6 min-w-0 rounded-[5px] border border-(--line) bg-(--bg-app) px-2 text-[11px] text-(--fg-body)";
const LABEL = "font-bold text-(--fg-dim) text-[10px] tracking-[.08em]";
const lines = (s: string) =>
  s
    .split("\n")
    .map((a) => a.trim())
    .filter(Boolean);

function RuntimeChip({ c, label }: { c: Carriage | undefined; label: string }) {
  const [mark, hue] = HOW[c?.how ?? "not_yet"] ?? HOW.not_yet;
  return (
    <span
      className="inline-flex h-[18px] shrink-0 items-center gap-1 rounded px-1.5 text-[10px]"
      style={{ color: hue, background: `color-mix(in srgb, ${hue} 12%, transparent)`, boxShadow: `inset 0 0 0 1px color-mix(in srgb, ${hue} 35%, transparent)` }}
      title={c?.note || undefined}
    >
      {label} {mark}
    </span>
  );
}

/** A custom tool's parameters as rows; what a row doesn't edit (a
 *  description, an enum) rides along untouched. */
type Param = { name: string; type: string; required: boolean; rest: Record<string, unknown> };
function paramsOf(schema: unknown): Param[] {
  const s = (schema ?? {}) as { properties?: Record<string, Record<string, unknown>>; required?: string[] };
  const required = new Set(s.required ?? []);
  return Object.entries(s.properties ?? {}).map(([name, p]) => {
    const { type, ...rest } = p ?? {};
    return { name, type: typeof type === "string" ? type : "string", required: required.has(name), rest };
  });
}
function schemaOf(rows: Param[]): Record<string, unknown> | null {
  const named = rows.filter((r) => r.name.trim());
  if (named.length === 0) return null;
  return {
    type: "object",
    properties: Object.fromEntries(named.map((r) => [r.name.trim(), { ...r.rest, type: r.type }])),
    required: named.filter((r) => r.required).map((r) => r.name.trim()),
  };
}
const partsLine = (p: PluginParts) =>
  [
    p.skills.length ? `${p.skills.length} skills` : "",
    p.commands.length ? `${p.commands.length} commands` : "",
    p.agents.length ? `${p.agents.length} subagents` : "",
    p.hooks.length ? `hooks on ${p.hooks.join(", ")}` : "",
    p.mcp_servers.length ? `${p.mcp_servers.length} MCP servers` : "",
  ]
    .filter(Boolean)
    .join(" · ");

type Draft = {
  id?: string;
  kind: Kind3;
  name: string;
  description: string;
  mine: boolean;
  enabled: boolean;
  text: string;
  how: "command" | "url";
  command: string;
  args: string;
  url: string;
  event: string;
  matcher: string;
  params: Param[];
  parts: PluginParts | null;
};
const blank = (kind: Kind3): Draft => ({
  kind,
  name: "",
  description: "",
  mine: true,
  enabled: true,
  text: "",
  how: "command",
  command: "",
  args: "",
  url: "",
  event: "pre_tool",
  matcher: "",
  params: [],
  parts: null,
});
const draftOf = (i: ToolkitView): Draft => ({
  id: i.id,
  kind: i.kind,
  name: i.name,
  description: i.description ?? "",
  mine: !!i.project_id,
  enabled: i.enabled,
  text: i.text ?? "",
  how: i.url ? "url" : "command",
  command: i.command ?? "",
  args: (i.args ?? []).join("\n"),
  url: i.url ?? "",
  // A hook with no when is inert: it stays so until the operator picks one.
  event: i.event ?? "",
  matcher: i.matcher ?? "",
  params: paramsOf(i.params),
  parts: i.plugin?.parts ?? null,
});

/** Adding or changing one item: what it needs depends on its kind. The
 *  Bakery's own project (id "") holds only Bakery-wide ones. */
function ItemForm({ project, draft, onDone }: { project: ShellProject; draft: Draft; onDone: (changed: boolean) => void }) {
  const [d, setD] = useState(project.id ? draft : { ...draft, mine: false });
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const set = (next: Partial<Draft>) => setD((x) => ({ ...x, ...next }));
  const setParam = (i: number, next: Partial<Param>) => set({ params: d.params.map((p, j) => (j === i ? { ...p, ...next } : p)) });
  const save = () => {
    setBusy(true);
    setError("");
    const runs = (d.kind === "mcp" && d.how === "command") || d.kind === "tool";
    toolkitSet({
      id: d.id ?? null,
      kind: d.kind,
      name: d.name.trim(),
      description: d.description.trim(),
      project_id: d.mine ? project.id : null,
      enabled: d.enabled,
      text: d.kind === "mcp" || d.kind === "tool" || d.kind === "plugin" ? undefined : d.text,
      command: runs ? d.command.trim() : null,
      args: runs ? lines(d.args) : [],
      url: d.kind === "mcp" && d.how === "url" ? d.url.trim() : null,
      event: d.kind === "hook" ? d.event || null : null,
      matcher: d.kind === "hook" ? d.matcher.trim() || null : null,
      params: d.kind === "tool" ? schemaOf(d.params) : null,
    }).then(
      () => onDone(true),
      (e) => {
        setBusy(false);
        setError(said(e));
      },
    );
  };
  const remove = async () => {
    if (!d.id || !(await ask(`Remove ${d.name}? No runtime gets it after this.`))) return;
    setBusy(true);
    toolkitRemove({ id: d.id }).then(
      () => onDone(true),
      (e) => {
        setBusy(false);
        setError(said(e));
      },
    );
  };
  return (
    <form
      className="flex flex-col gap-1.5 rounded-md border border-(--line) bg-(--bg-app) p-2"
      data-testid="toolkit-form"
      onSubmit={(e) => {
        e.preventDefault();
        save();
      }}
    >
      <div className="flex flex-wrap items-center gap-1.5">
        {/* A plugin comes only through the port, whole. */}
        <select aria-label="Kind" className={FIELD} disabled={!!d.id} onChange={(e) => set({ kind: e.target.value as Kind3 })} value={d.kind}>
          {KINDS.filter(([k]) => k !== "plugin" || d.id).map(([k, label]) => (
            <option key={k} value={k}>
              {label}
            </option>
          ))}
        </select>
        <input
          aria-label="Name"
          autoFocus
          className={`${FIELD} w-48 font-mono`}
          disabled={d.kind === "plugin"}
          onChange={(e) => set({ name: e.target.value })}
          placeholder={d.kind === "command" ? "name, or folder:name" : "name"}
          value={d.name}
        />
        {!!project.id && (
          <select aria-label="For" className={FIELD} onChange={(e) => set({ mine: e.target.value === "project" })} value={d.mine ? "project" : "bakery"}>
            <option value="project">{project.name}</option>
            <option value="bakery">Bakery-wide</option>
          </select>
        )}
        <input aria-label="What it's for" className={`${FIELD} flex-1`} onChange={(e) => set({ description: e.target.value })} placeholder="what it's for, and when to use it" value={d.description} />
      </div>
      {d.kind === "mcp" && (
        <div className="flex flex-wrap items-start gap-1.5">
          <select aria-label="Runs as" className={FIELD} onChange={(e) => set({ how: e.target.value as Draft["how"] })} value={d.how}>
            <option value="command">a program</option>
            <option value="url">an address</option>
          </select>
          {d.how === "command" ? (
            <>
              <input aria-label="Command" className={`${FIELD} w-56 font-mono`} onChange={(e) => set({ command: e.target.value })} placeholder="npx" value={d.command} />
              <textarea
                aria-label="Arguments, one per line"
                className={`${FIELD} h-14 min-w-48 flex-1 py-1 font-mono`}
                onChange={(e) => set({ args: e.target.value })}
                placeholder={"arguments, one per line"}
                value={d.args}
              />
            </>
          ) : (
            <input aria-label="Address" className={`${FIELD} flex-1 font-mono`} onChange={(e) => set({ url: e.target.value })} placeholder="https://…" value={d.url} />
          )}
        </div>
      )}
      {d.kind === "tool" && (
        <>
          <div className="flex flex-wrap items-start gap-1.5">
            <input aria-label="Command" className={`${FIELD} w-56 font-mono`} onChange={(e) => set({ command: e.target.value })} placeholder="./scripts/deploy.sh" value={d.command} />
            <textarea
              aria-label="Arguments, one per line"
              className={`${FIELD} h-14 min-w-48 flex-1 py-1 font-mono`}
              onChange={(e) => set({ args: e.target.value })}
              placeholder={"arguments, one per line"}
              value={d.args}
            />
          </div>
          <div className="flex flex-col gap-1">
            <span className={LABEL}>PARAMS</span>
            {d.params.map((p, i) => (
              <div className="flex items-center gap-1.5" key={i}>
                <input aria-label="Param name" className={`${FIELD} w-40 font-mono`} onChange={(e) => setParam(i, { name: e.target.value })} placeholder="name" value={p.name} />
                <select aria-label="Its type" className={FIELD} onChange={(e) => setParam(i, { type: e.target.value })} value={p.type}>
                  {TYPES.map((t) => (
                    <option key={t} value={t}>
                      {t}
                    </option>
                  ))}
                </select>
                <label className="flex items-center gap-1 text-(--fg-dim) text-[11px]">
                  <input checked={p.required} onChange={(e) => setParam(i, { required: e.target.checked })} type="checkbox" />
                  required
                </label>
                <button aria-label={`Remove ${p.name || "this param"}`} className="text-(--fg-faint) hover:text-(--bad)" onClick={() => set({ params: d.params.filter((_, j) => j !== i) })} type="button">
                  <XIcon className="size-3" />
                </button>
              </div>
            ))}
            <button className={`${PLAIN} self-start`} onClick={() => set({ params: [...d.params, { name: "", type: "string", required: false, rest: {} }] })} type="button">
              <PlusIcon className="size-3" />
              Param
            </button>
          </div>
        </>
      )}
      {d.kind === "hook" && (
        <div className="flex flex-wrap items-center gap-1.5">
          <select aria-label="When" className={FIELD} onChange={(e) => set({ event: e.target.value })} value={d.event}>
            {d.event === "" && <option value="">Pick when</option>}
            {WHEN.map(([v, w]) => (
              <option key={v} value={v}>
                {w}
              </option>
            ))}
          </select>
          <input
            aria-label="Which tools"
            className={`${FIELD} w-64 font-mono`}
            onChange={(e) => set({ matcher: e.target.value })}
            placeholder="every tool, or Edit|Write"
            title="Claude's tool names: one, several with |, or none for every tool"
            value={d.matcher}
          />
        </div>
      )}
      {d.kind === "plugin" && d.parts && <p className="text-(--fg-dim) text-[11px]">Carried whole into Claude agents: {partsLine(d.parts) || "nothing listed"}</p>}
      {d.kind !== "mcp" && d.kind !== "tool" && d.kind !== "plugin" && (
        <textarea
          aria-label={d.kind === "hook" ? "Command" : WHOLE.has(d.kind) ? "The whole file" : "Text"}
          className={`${FIELD} ${WHOLE.has(d.kind) ? "h-40" : "h-24"} py-1 font-mono`}
          onChange={(e) => set({ text: e.target.value })}
          placeholder={
            d.kind === "hook"
              ? "the command it runs"
              : WHOLE.has(d.kind)
                ? "the whole file, its --- header and all, as Claude Code keeps it"
                : d.kind === "skill"
                  ? "the skill, in Markdown"
                  : "the instructions, in Markdown"
          }
          value={d.text}
        />
      )}
      <div className="flex items-center gap-1.5">
        {d.id && (
          <button className={PLAIN} disabled={busy} onClick={() => void remove()} type="button">
            Remove
          </button>
        )}
        {error && (
          <span className="min-w-0 flex-1 truncate text-(--bad) text-[11px]" title={error}>
            {error}
          </span>
        )}
        <span className="flex-1" />
        <button className={PLAIN} onClick={() => onDone(false)} type="button">
          Cancel
        </button>
        <button className={SOLID} disabled={busy || !d.name.trim()} type="submit">
          {d.id ? "Save" : "Add"}
        </button>
      </div>
    </form>
  );
}

/** One line of what an item does, beside its name. */
function detailOf(i: ToolkitView): string {
  if (i.kind === "hook") return `${WHEN.find(([v]) => v === i.event)?.[1] ?? "Never (no when set)"} · ${i.matcher?.trim() || "every tool"}`;
  if (i.kind === "tool") {
    const names = paramsOf(i.params).map((p) => p.name);
    return `${i.command ?? ""}${names.length ? ` (${names.join(", ")})` : ""}`;
  }
  if (i.kind === "plugin" && i.plugin) return partsLine(i.plugin.parts);
  return i.description ?? "";
}

/** The VS Code tool port: what the operator's Claude Code setup holds, to
 *  tick into the Toolkit, this project's or Bakery-wide. Nothing is ticked
 *  at first; output styles never by "Tick the new ones". */
function PortPanel({ project, onPorted, onClose }: { project: ShellProject; onPorted: () => void; onClose: () => void }) {
  const [mine, setMine] = useState(!!project.id);
  const projectId = mine ? project.id : null;
  const [found, setFound] = useState<Found[] | null>(null);
  const [error, setError] = useState("");
  const [picked, setPicked] = useState<Set<string>>(new Set());
  const [busy, setBusy] = useState(false);
  const [results, setResults] = useState<Map<string, Imported>>(new Map());
  const read = (scope: string | null) => toolkitPort({ project_id: scope }).then((p) => setFound(p.found), (e) => setError(said(e)));
  useEffect(() => {
    setFound(null);
    setError("");
    setResults(new Map());
    void read(projectId);
  }, [projectId]);
  const fresh = (found ?? []).filter((f) => f.kind && f.kind !== "output_style" && !f.in_toolkit);
  const flip = (key: string) =>
    setPicked((p) => {
      const next = new Set(p);
      if (!next.delete(key)) next.add(key);
      return next;
    });
  const port = () => {
    setBusy(true);
    setError("");
    toolkitPortSet({ keys: [...picked], project_id: projectId }).then(
      (r) => {
        setResults(new Map(r.imported.map((i) => [i.key, i])));
        setPicked(new Set());
        setBusy(false);
        onPorted();
        void read(projectId);
      },
      (e) => {
        setError(said(e));
        setBusy(false);
      },
    );
  };
  const done = [...results.values()];
  return (
    <div className="flex flex-col gap-1.5 rounded-md border border-(--accent-border) bg-(--bg-app) p-2" data-testid="toolkit-port">
      <div className="flex flex-wrap items-center gap-1.5">
        <ArrowDownToLineIcon className="size-3.5 text-(--accent)" />
        <b className="text-(--fg)">VS Code tool port</b>
        {project.id ? (
          <span className="flex overflow-hidden rounded-[5px] border border-(--line)">
            <button className={SEG(mine)} onClick={() => setMine(true)} type="button">
              {project.name}
            </button>
            <button className={`${SEG(!mine)} border-(--line) border-l`} onClick={() => setMine(false)} type="button">
              Bakery-wide
            </button>
          </span>
        ) : (
          <Chip>BAKERY-WIDE</Chip>
        )}
        {done.length > 0 && (
          <span className="text-(--fg-dim) text-[11px]">
            {done.filter((r) => !r.error).length} ported{done.some((r) => r.error) ? `, ${done.filter((r) => r.error).length} not` : ""}
          </span>
        )}
        <span className="flex-1" />
        <button className={PLAIN} disabled={fresh.length === 0} onClick={() => setPicked(new Set(fresh.map((f) => f.key)))} type="button">
          Tick the new ones
        </button>
        <button className={SOLID} disabled={busy || picked.size === 0} onClick={port} type="button">
          {busy ? "Porting…" : `Port ${picked.size || ""}`.trim()}
        </button>
        <button aria-label="Close the port" className={PLAIN} onClick={onClose} type="button">
          <XIcon className="size-3" />
        </button>
      </div>
      {error && <p className="text-(--bad) text-[11px]">{error}</p>}
      {!found && !error && <p className="text-(--fg-faint)">Reading your Claude Code setup…</p>}
      {found && found.length === 0 && <p className="text-(--fg-faint)">Nothing in your Claude Code setup.</p>}
      {FOUND_ORDER.map((what) => {
        const rows = (found ?? []).filter((f) => f.found === what);
        if (rows.length === 0) return null;
        return (
          <section aria-label={FOUND[what]} className="flex flex-col" key={what}>
            <div className={`${LABEL} px-1 pt-1`}>{FOUND[what].toUpperCase()}</div>
            {rows.map((f) => {
              const result = results.get(f.key);
              // A plugin whose options or keys stay behind needs its own settings again.
              const needsSettings = f.found === "plugin" && f.left_out.length > 0;
              return (
                <label className={`flex items-start gap-2 rounded-[5px] px-1 py-1 ${f.kind ? "cursor-pointer hover:bg-(--bg-chip)" : "opacity-75"}`} key={f.key}>
                  <input checked={picked.has(f.key)} className="mt-0.5" disabled={!f.kind || busy} onChange={() => flip(f.key)} type="checkbox" />
                  <span className="flex min-w-0 flex-1 flex-col gap-0.5">
                    <span className="flex min-w-0 flex-wrap items-center gap-1.5">
                      <span className="font-mono text-(--fg)">{f.name}</span>
                      <span className="text-(--fg-dim) text-[10.5px]">
                        {FOUND[f.found] ?? f.found} → {f.kind ? KIND_WORD[f.kind] : "stays for now"}
                      </span>
                      {f.in_toolkit && (
                        <span title="Porting it again changes that item">
                          <Chip>IN THE TOOLKIT</Chip>
                        </span>
                      )}
                      {needsSettings && <Chip tone="warn">NEEDS ITS SETTINGS</Chip>}
                      {result && <Chip tone={result.error ? "bad" : "ok"}>{result.error ? "NOT PORTED" : "PORTED"}</Chip>}
                      <span className="min-w-0 flex-1" />
                      <span className="truncate text-(--fg-faint) text-[10px]">{f.source}</span>
                    </span>
                    {f.description && <span className="truncate text-(--fg-dim) text-[11px]">{f.description}</span>}
                    {f.reason && <span className="text-(--fg-faint) text-[10.5px]">Can't come yet: {f.reason}</span>}
                    {f.parts && <span className="text-(--fg-dim) text-[10.5px]">{partsLine(f.parts)}</span>}
                    {f.warnings.map((w) => (
                      <span className="text-(--warn) text-[10.5px]" key={w}>
                        ⚠ {w}
                      </span>
                    ))}
                    {f.left_out.length > 0 && <span className="text-(--fg-dim) text-[10.5px]">Left out: {f.left_out.join(" · ")}</span>}
                    {result?.error && <span className="text-(--bad) text-[10.5px]">{result.error}</span>}
                  </span>
                </label>
              );
            })}
          </section>
        );
      })}
    </div>
  );
}

export function ToolkitCard({ project }: { project: ShellProject }) {
  const key = `toolkit:${project.id}`;
  const { data, error, busy, at, reload } = useCached(key, () => toolkit({ project_id: project.id }));
  // The Bakery's own project ("") holds only the Bakery-wide ones.
  const items = (data?.items ?? []).filter((i) => !!project.id || !i.project_id);
  const [kind, setKind] = useState<Kind3 | null>(null);
  /** The item open for a change, "new" for one being added, or nothing. */
  const [editing, setEditing] = useState<string | null>(null);
  const [porting, setPorting] = useState(false);
  const [toggleError, setToggleError] = useState("");
  const done = (changed: boolean) => {
    setEditing(null);
    if (changed) void reload();
  };
  const toggle = (i: ToolkitView) => {
    setToggleError("");
    toolkitEnable({ id: i.id, enabled: !i.enabled }).then(
      (next) => patchKept<{ items: ToolkitView[] }>(key, (t) => ({ ...t, items: t.items.map((x) => (x.id === next.id ? next : x)) })),
      (e) => setToggleError(said(e)),
    );
  };
  const shownKinds = KINDS.filter(([k]) => !kind || k === kind);
  return (
    <Card
      action={
        <span className="flex items-center gap-1.5 font-normal normal-case tracking-normal">
          <select aria-label="Which kind" className={FIELD} onChange={(e) => setKind((e.target.value || null) as Kind3 | null)} value={kind ?? ""}>
            <option value="">Every kind</option>
            {KINDS.map(([k, label]) => (
              <option key={k} value={k}>
                {label} · {items.filter((i) => i.kind === k).length}
              </option>
            ))}
          </select>
          <Freshness at={at} busy={busy} error={error} onReload={() => void reload()} />
          <button aria-pressed={porting} className={PLAIN} onClick={() => setPorting((p) => !p)} type="button">
            <ArrowDownToLineIcon className="size-3" />
            VS Code tool port
          </button>
          <button className={PLAIN} onClick={() => setEditing("new")} type="button">
            <PlusIcon className="size-3" />
            Add
          </button>
        </span>
      }
      // An empty toolkit still opens its port and its new item.
      count={items.length || undefined}
      hue="var(--violet)"
      icon={<WrenchIcon className="size-3.5" />}
      title="Toolkit"
      wide="full"
    >
      <div className="flex flex-col gap-1 p-2" data-testid="toolkit">
        <div className="flex flex-wrap items-center gap-2 px-1 text-(--fg-dim) text-[10.5px]">
          {Object.entries({ carried: "carried as it is", translated: "translated by the Bakery", not_yet: "not yet" }).map(([how, words]) => (
            <span className="flex items-center gap-1" key={how}>
              <span style={{ color: HOW[how][1] }}>{HOW[how][0]}</span>
              {words}
            </span>
          ))}
        </div>
        {porting && <PortPanel onClose={() => setPorting(false)} onPorted={() => void reload()} project={project} />}
        {editing === "new" && <ItemForm draft={blank(kind && kind !== "plugin" ? kind : "skill")} onDone={done} project={project} />}
        {toggleError && <p className="px-1 text-(--bad) text-[11px]">{toggleError}</p>}
        {!data && !error && <p className="px-1 text-(--fg-faint)">Reading the toolkit…</p>}
        {shownKinds.map(([k, label, Icon, hue]) => {
          const rows = items.filter((i) => i.kind === k);
          if (rows.length === 0) return null;
          return (
            <section aria-label={label} className="flex flex-col" key={k}>
              <div className="flex h-6 items-center gap-1.5 px-1 font-bold text-[10px] uppercase tracking-[.06em]" style={{ color: hue }}>
                <Icon className="size-3" />
                {label}
              </div>
              {rows.map((i) =>
                editing === i.id ? (
                  <ItemForm draft={draftOf(i)} key={i.id} onDone={done} project={project} />
                ) : (
                  <div className="flex min-h-7 min-w-0 items-center gap-2.5 rounded-[5px] px-1 hover:bg-(--bg-chip)" key={i.id}>
                    <button className="w-48 min-w-0 shrink-0 truncate text-left font-mono text-(--fg)" onClick={() => setEditing(i.id)} title={i.description || i.name} type="button">
                      {i.name}
                    </button>
                    <span className="w-32 shrink-0 truncate text-(--fg-dim) text-[10.5px]">{i.project_id ? project.name : "Bakery-wide"}</span>
                    <span className="min-w-0 flex-1 truncate text-(--fg-dim) text-[10.5px]" title={detailOf(i)}>
                      {detailOf(i)}
                    </span>
                    <span className="flex min-w-0 shrink-0 flex-wrap gap-1">
                      {RUNTIMES.map(([r, rl]) => (
                        <RuntimeChip c={i.runtimes.find((c) => c.runtime === r)} key={r} label={rl} />
                      ))}
                    </span>
                    <button
                      aria-checked={i.enabled}
                      aria-label={`${i.name} on`}
                      className={`relative h-[18px] w-8 shrink-0 rounded-full ${i.enabled ? "bg-(--accent-solid)" : "bg-(--bg-chip)"}`}
                      onClick={() => toggle(i)}
                      role="switch"
                      type="button"
                    >
                      <span className={`absolute top-0.5 size-3.5 rounded-full bg-(--bg-app) ${i.enabled ? "left-4" : "left-0.5"}`} />
                    </button>
                  </div>
                ),
              )}
            </section>
          );
        })}
        {data && items.length === 0 && editing !== "new" && !porting && <p className="px-1 text-(--fg-faint)">Nothing in the toolkit yet.</p>}
      </div>
    </Card>
  );
}
