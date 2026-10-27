// The control plane's cards for what a project is built from (Round 4, A),
// read from its files: each folder's stack, direct dependencies and tools;
// the MCP servers it defines, beside the Bakery's own; its skills, once each,
// and its instruction files; and the Bakery's own setup for it, with what its
// files suggest to confirm. Every item names the file it came from, and a
// click opens that file in the folder's Code view.

import { type ReactNode, useRef, useState } from "react";
import { FileTextIcon, GlobeIcon, LayersIcon, LockIcon, PlugIcon, PlusIcon, RotateCwIcon, SparklesIcon, TerminalIcon, TriangleAlertIcon, WrenchIcon } from "lucide-react";
import { type ControlPlane as Control, type Folder, type Host, projectControl, projectSetup } from "$lib/api.generated";
import { ago } from "$lib/conversations";
import { sharedTenant } from "../hostsModel";
import { folderOf, openInCode, PLAIN, SHARED_WARNING, SOLID, useHostList } from "./Internet";
import * as L from "./layout";
import { POP, useDismiss } from "./PanelMenu";
import type { ShellProject } from "./shellSource.svelte";
import { patchKept, useCached } from "./cache";
import { Chip, Freshness, said, type ToolContext } from "./ui";

export function Card({
  title,
  hue,
  icon,
  count,
  wide = false,
  action,
  children,
}: {
  title: string;
  hue: string;
  icon: ReactNode;
  count?: number;
  /** Two columns, or the whole row. */
  wide?: boolean | "full";
  action?: ReactNode;
  children: ReactNode;
}) {
  return (
    <section
      aria-label={title}
      className={`flex min-w-0 flex-col rounded-lg border border-(--line) bg-(--bg-raised) ${wide === "full" ? "col-span-full" : wide ? "col-span-2" : ""}`}
    >
      <div
        className="flex h-8 items-center gap-2 rounded-t-lg border-(--line-soft) border-b pr-2.5 pl-3"
        style={{ background: `color-mix(in srgb, ${hue} 8%, transparent)`, boxShadow: `inset 3px 0 0 ${hue}`, color: hue }}
      >
        {icon}
        <h3 className="m-0 min-w-0 flex-1 truncate font-bold text-[10.5px] uppercase tracking-[.08em]">{title}</h3>
        {count !== undefined && <Chip>{count}</Chip>}
        {action}
      </div>
      {count !== 0 && <div className="flex min-w-0 flex-col gap-1 p-2.5">{children}</div>}
    </section>
  );
}

const KIND_HUE: Record<string, string> = {
  language: "var(--link)",
  framework: "var(--violet)",
  platform: "var(--warn)",
  toolchain: "var(--ok)",
  ci: "var(--hue-containers)",
  check: "var(--fg-dim)",
};
const TILE = "flex h-7 min-w-0 items-center gap-1.5 rounded-md border border-(--line-soft) bg-(--bg-app) px-2 text-left hover:border-(--line)";
const SUB = "font-bold text-(--fg-dim) text-[10px] tracking-[.04em]";
const SETUP_WORDS: Record<string, string> = { toolchain: "Toolchain", tasks: "Tasks", containers: "Containers", secrets: "Secrets" };
/** What the node takes for each (`ProjectStacks::validate`), its default first. */
const SETUP_VALUES: Record<string, string[]> = {
  toolchain: ["none", "mise", "proto"],
  tasks: ["none", "moon"],
  containers: ["apple", "orbstack", "colima"],
  secrets: ["bakery", "infisical"],
};
const PICK = "h-[22px] rounded-md border bg-(--bg-app) px-1 font-mono text-[11px]";

export function ProjectControlCards({ ctx, project }: { ctx: ToolContext; project: ShellProject }) {
  // The project's files as last read, at once (kept across restarts), then
  // read again in the background.
  const key = `control:${project.id}`;
  const { data: control, error: readError, busy, at, reload } = useCached<Control>(key, () => projectControl({ project_id: project.id }));
  const [setupError, setSetupError] = useState("");
  const error = setupError || readError;
  const load = () => {
    setSetupError("");
    void reload();
  };

  /** Opens a folder's file in that folder's Code view. */
  const open = (folder: string, file: string) => openInCode(ctx, project, folder, file);
  const refresh = <Freshness at={at} busy={busy} error={readError} onReload={load} />;
  if (!control) {
    return (
      <Card action={refresh} hue="var(--link)" icon={<LayersIcon className="size-3.5" />} title="Stack" wide>
        <p className="px-1 text-(--fg-faint)">{error || "Reading the project's files…"}</p>
      </Card>
    );
  }
  const tools = control.folders.flatMap((f) => f.tools.map((t) => ({ ...t, folder: f })));
  const setup = control.setup;
  /** The operator's pick for one field of the setup, the rest kept: shown
   *  once the node takes it, its files read again behind it. */
  const choose = (field: string, value: string) =>
    void projectSetup({ project_id: project.id, setup: { ...setup.current, [field]: value } }).then(
      (current) => {
        patchKept<Control>(key, (c) => ({ ...c, setup: { ...c.setup, current } }));
        load();
      },
      (e) => setSetupError(said(e)),
    );
  return (
    <>
      <Card action={refresh} hue="var(--link)" icon={<LayersIcon className="size-3.5" />} title="Stack" wide>
        {error && <p className="px-1 text-(--bad) text-[11px]">{error}</p>}
        {control.folders.map((f) => (
          <FolderStack folder={f} key={f.path} open={open} />
        ))}
      </Card>

      <Card count={control.mcp.length} hue="var(--violet)" icon={<PlugIcon className="size-3.5" />} title="MCP servers">
        {control.mcp.map((m) => (
          <button
            className="flex min-h-7 min-w-0 flex-col items-start gap-0.5 rounded-[5px] px-1.5 py-1 text-left hover:bg-(--bg-chip) disabled:hover:bg-transparent"
            disabled={!m.source || !m.folder}
            key={`${m.folder ?? ""}:${m.name}`}
            onClick={() => m.source && m.folder && open(m.folder, m.source)}
            title={m.source ? `From ${m.source}` : "The Bakery's own, for every agent"}
            type="button"
          >
            <span className="flex w-full min-w-0 items-center gap-2">
              <span className="min-w-0 flex-1 truncate text-(--fg-body)">{m.name}</span>
              {!m.source && <Chip tone="accent">BAKERY</Chip>}
            </span>
            <span className="w-full truncate font-mono text-(--fg-dim) text-[10.5px]">{m.url ?? [m.command, ...m.args].filter(Boolean).join(" ")}</span>
            {(m.env.length > 0 || m.headers.length > 0) && (
              <span className="flex flex-wrap gap-1">
                {[...m.env, ...m.headers].map((n) => (
                  <Chip key={n}>{n}</Chip>
                ))}
              </span>
            )}
          </button>
        ))}
      </Card>

      <Card count={control.skills.length + control.instructions.length} hue="var(--ok)" icon={<SparklesIcon className="size-3.5" />} title="Skills">
        {control.skills.map((s) => (
          <div className="flex h-7 min-w-0 items-center gap-2 px-1.5" key={s.name} title={s.description ?? s.files.join("\n")}>
            <SparklesIcon className="size-3 shrink-0 text-(--ok)" />
            <span className="min-w-0 flex-1 truncate text-(--fg-body)">{s.name}</span>
            {s.dirs.map((d) => (
              <Chip key={d}>{d.startsWith(".claude") ? "CLAUDE" : "AGENTS"}</Chip>
            ))}
          </div>
        ))}
        {control.instructions.length > 0 && <div className={`${SUB} px-1.5 pt-1.5`}>INSTRUCTIONS</div>}
        {control.instructions.map((i) => (
          <button
            className="flex h-7 min-w-0 items-center gap-2 rounded-[5px] px-1.5 text-left hover:bg-(--bg-chip)"
            key={`${i.folder}/${i.file}`}
            onClick={() => open(i.folder, i.file)}
            title={`${i.folder}/${i.file}`}
            type="button"
          >
            <FileTextIcon className="size-3 shrink-0 text-(--fg-dim)" />
            <span className="min-w-0 flex-1 truncate text-(--fg-body)">{i.file}</span>
            <span className="shrink-0 text-(--fg-faint) text-[10.5px]">{i.folder.split("/").pop()}</span>
          </button>
        ))}
      </Card>

      <Card count={tools.length} hue="var(--fg-2)" icon={<WrenchIcon className="size-3.5" />} title="Tools">
        <div className="flex flex-wrap gap-1.5">
          {tools.map((t) => (
            <button className={TILE} key={`${t.folder.path}:${t.name}`} onClick={() => open(t.folder.path, t.source)} title={`${t.folder.name} · ${t.source}`} type="button">
              <TerminalIcon className="size-3 shrink-0 text-(--ok)" />
              <span className="truncate text-(--fg-body)">{t.name}</span>
            </button>
          ))}
        </div>
      </Card>

      <Card hue="var(--accent)" icon={<LayersIcon className="size-3.5" />} title="Setup">
        <div className="flex flex-wrap gap-1.5">
          {(Object.keys(SETUP_VALUES) as ("toolchain" | "tasks" | "containers" | "secrets")[]).map((k) => {
            const values = SETUP_VALUES[k];
            const now = setup.current[k] ?? values[0];
            return (
              <label className="flex items-center gap-1.5 text-[11px]" key={k}>
                <span className="text-(--fg-dim)">{SETUP_WORDS[k]}</span>
                <select
                  className={`${PICK} ${now === values[0] ? "border-(--line-soft) text-(--fg-body) hover:border-(--line)" : "border-(--accent-border) font-bold text-(--fg)"}`}
                  onChange={(e) => choose(k, e.target.value)}
                  value={now}
                >
                  {(values.includes(now) ? values : [...values, now]).map((v) => (
                    <option key={v} value={v}>
                      {v}
                    </option>
                  ))}
                </select>
              </label>
            );
          })}
        </div>
        {setup.suggestions.map((s) => (
          <div className="flex min-w-0 items-center gap-2 rounded-[5px] border border-(--line-soft) bg-(--bg-app) px-2 py-1.5" key={`${s.field}:${s.value}`}>
            <span className="min-w-0 flex-1 text-[11px]">
              <span className="text-(--fg-body)">
                {SETUP_WORDS[s.field] ?? s.field}: <b>{s.value}</b>
              </span>
              <span className="block truncate text-(--fg-dim) text-[10.5px]" title={s.because}>
                from {s.because.split("/").slice(-2).join("/")}
              </span>
            </span>
            <button
              className="flex h-6 shrink-0 items-center rounded-[5px] border border-(--accent-fg) bg-(image:--grad-accent) px-2.5 font-bold text-(--fg-on-solid) text-[11px] hover:brightness-105"
              onClick={() => choose(s.field, s.value)}
              type="button"
            >
              Use it
            </button>
          </div>
        ))}
      </Card>

      <InternetCard
        ctx={ctx}
        openFile={(file) => {
          const folder = folderOf(control, file);
          return folder ? () => open(folder, file) : null;
        }}
        project={project}
      />
    </>
  );
}

const HOST_CHIP =
  "flex h-[22px] min-w-0 items-center gap-1.5 rounded-md border border-(--line-soft) bg-(--bg-app) px-2 font-mono text-(--fg-body) text-[11px] hover:border-(--line)";
const MENU_ITEM = "flex h-7 w-full items-center gap-2 rounded px-2 text-left text-(--fg-body) hover:bg-(--bg-chip)";
const GROUP_LABEL = "w-24 shrink-0 pt-[3px] font-bold text-[10px] tracking-[.06em]";

/** The project's internet (Round 7, L1): the hosts its threads' sandboxes
 *  reach, by where each comes from, and the ones waiting on you. Only you
 *  answer, for the whole project; each host's menu offers what applies. */
function InternetCard({
  ctx,
  project,
  openFile,
}: {
  ctx: ToolContext;
  project: ShellProject;
  /** The file a project host comes from, opened in its folder's Code view. */
  openFile: (file: string) => (() => void) | null;
}) {
  const { list, error, busy, decide } = useHostList(project.id);
  /** The project's own checkout, which the whole list opens on. */
  const home = project.checkouts.find((c) => c.inPlace)?.streamId ?? null;
  const [adding, setAdding] = useState(false);
  const [draft, setDraft] = useState("");
  const add = () => {
    const host = draft.trim();
    if (host) void decide(host, true).then((done) => done && (setDraft(""), setAdding(false)));
  };
  const hosts = list?.hosts ?? [];
  const asks = list?.asks ?? [];
  const allowed = hosts.filter((h) => h.allow).length;
  const denied = hosts.filter((h) => !h.allow);
  const threadOf = (sid: string) => ctx.snap.conversations.find((c) => c.streamId === sid)?.title ?? "a thread";
  const group = (label: string, rows: Host[], tone = "text-(--fg-dim)") =>
    rows.length > 0 && (
      <div className="flex min-w-0 items-start gap-2.5 px-1">
        <span className={`${GROUP_LABEL} ${tone}`}>{label}</span>
        <div className="flex min-w-0 flex-wrap gap-1.5">
          {rows.map((h) => (
            <HostChip bakery={!project.id} busy={busy} decide={decide} host={h} key={h.host} open={h.because ? openFile(h.because) : null} />
          ))}
        </div>
      </div>
    );
  return (
    <Card
      action={
        <>
          {list && <Chip tone="ok">{allowed} ALLOWED</Chip>}
          {asks.length > 0 && <Chip tone="warn">{asks.length} ASKING</Chip>}
          {denied.length > 0 && <Chip>{denied.length} DENIED</Chip>}
          <button
            className="flex h-5 items-center gap-1 rounded px-1.5 text-(--fg-dim) text-[11px] hover:bg-(--bg-chip) hover:text-(--fg)"
            onClick={() => setAdding((a) => !a)}
            type="button"
          >
            <PlusIcon className="size-3" />
            Add host
          </button>
          {home && (
            <button
              className="flex h-5 items-center rounded px-1.5 text-(--fg-dim) text-[11px] hover:bg-(--bg-chip) hover:text-(--fg)"
              onClick={() => ctx.setLayout((l) => L.openPanel(l, ctx.workspace.id, `@internet:${home}`, 760))}
              title="The whole list"
              type="button"
            >
              All ›
            </button>
          )}
        </>
      }
      hue="var(--link)"
      icon={<GlobeIcon className="size-3.5" />}
      title="Internet"
      wide
    >
      {error && <p className="px-1 text-(--bad) text-[11px]">{error}</p>}
      {!list && !error && <p className="px-1 text-(--fg-faint)">Reading the list…</p>}
      {adding && (
        <form
          className="flex items-center gap-2 px-1"
          onSubmit={(e) => {
            e.preventDefault();
            add();
          }}
        >
          <input
            aria-label="Host to allow"
            autoFocus
            className="h-6 min-w-0 flex-1 rounded-[5px] border border-(--line) bg-(--bg-app) px-2 font-mono text-[11px]"
            onChange={(e) => setDraft(e.target.value)}
            onKeyDown={(e) => e.key === "Escape" && setAdding(false)}
            placeholder="host, or *.domain"
            value={draft}
          />
          <button className={SOLID} disabled={busy || !draft.trim()} type="submit">
            Allow
          </button>
        </form>
      )}
      {asks.map((a) => (
        <div className="flex min-h-7 min-w-0 flex-wrap items-center gap-2 rounded-[5px] bg-(--warn-bg) px-2 py-1 shadow-[inset_3px_0_0_var(--warn)]" key={a.event_hash}>
          <GlobeIcon className="size-3.5 shrink-0 text-(--warn)" />
          <span className="font-bold font-mono text-(--fg) text-[11.5px]">{a.host}</span>
          {sharedTenant(a.host) && <Chip tone="bad">SHARED</Chip>}
          <span className="min-w-0 flex-1 truncate text-(--fg-dim) text-[11px]">
            {threadOf(a.stream_id)}
            {a.program ? ` · ${a.program}` : ""} · {ago(a.at)}
          </span>
          <button className={SOLID} disabled={busy} onClick={() => void decide(a.host, true)} type="button">
            Allow
          </button>
          <button className={PLAIN} disabled={busy} onClick={() => void decide(a.host, false)} type="button">
            Deny
          </button>
        </div>
      ))}
      {asks.some((a) => sharedTenant(a.host)) && (
        <p className="flex items-start gap-1.5 px-1 text-(--bad) text-[11px]">
          <TriangleAlertIcon className="mt-px size-3 shrink-0" />
          {SHARED_WARNING}
        </p>
      )}
      {group("DEFAULTS", hosts.filter((h) => h.source === "default"))}
      {group("FROM FILES", hosts.filter((h) => h.source === "project"))}
      {group("BY YOU", hosts.filter((h) => h.source === "operator" && h.allow))}
      {group("DENIED", denied, "text-(--bad)")}
      {list && (
        <div className="flex min-w-0 items-start gap-2.5 px-1">
          <span className={`${GROUP_LABEL} flex items-center gap-1 text-(--fg-dim)`}>
            NEVER
            <LockIcon className="size-2.5" />
          </span>
          <span className={`${HOST_CHIP} border-dashed text-(--fg-dim) hover:border-(--line-soft)`}>private · LAN · tailnet addresses</span>
        </div>
      )}
    </Card>
  );
}

/** One host on the list, and its menu: what applies to it, no more. Forget
 *  only undoes an answer of yours. */
function HostChip({
  host,
  bakery,
  busy,
  decide,
  open,
}: {
  host: Host;
  /** The Bakery's own list: no project, and its threads never ask about
   *  a host nobody answered. */
  bakery: boolean;
  busy: boolean;
  decide: (host: string, allow: boolean | null) => Promise<boolean>;
  /** Opens the file that points to it, when it can be found. */
  open: (() => void) | null;
}) {
  const [shown, setShown] = useState(false);
  const box = useRef<HTMLSpanElement>(null);
  useDismiss(shown, box, () => setShown(false));
  const act = (allow: boolean | null) => () => {
    setShown(false);
    void decide(host.host, allow);
  };
  const note = host.source === "project" ? host.because : host.source === "operator" && host.at ? ago(host.at) : null;
  return (
    <span className="relative" ref={box}>
      <button
        aria-expanded={shown}
        aria-haspopup="menu"
        className={`${HOST_CHIP} ${host.allow ? "" : "border-(--bad-border) text-(--bad)"}`}
        onClick={() => setShown((s) => !s)}
        title={host.because ? `From ${host.because}` : undefined}
        type="button"
      >
        {host.host}
        {sharedTenant(host.host) && <Chip tone="bad">SHARED</Chip>}
        {note && <span className="font-sans text-(--fg-faint) text-[10px]">{note}</span>}
      </button>
      {shown && (
        <div className={`${POP} right-auto left-0 w-60`} role="menu">
          {host.source === "operator" && (
            <button className={MENU_ITEM} disabled={busy} onClick={act(null)} role="menuitem" type="button">
              <RotateCwIcon className="size-3 text-(--fg-dim)" />
              {bakery ? "Forget" : "Forget: ask next time"}
            </button>
          )}
          {host.allow ? (
            <button className={MENU_ITEM} disabled={busy} onClick={act(false)} role="menuitem" type="button">
              <span className="w-3 text-center text-(--bad)">✕</span>
              {bakery ? "Deny" : "Deny for this project"}
            </button>
          ) : (
            <button className={MENU_ITEM} disabled={busy} onClick={act(true)} role="menuitem" type="button">
              <span className="w-3 text-center text-(--ok)">✓</span>
              {bakery ? "Allow" : "Allow for this project"}
            </button>
          )}
          {open && (
            <button
              className={MENU_ITEM}
              onClick={() => {
                setShown(false);
                open();
              }}
              role="menuitem"
              type="button"
            >
              <FileTextIcon className="size-3 text-(--fg-dim)" />
              Open {host.because}
            </button>
          )}
        </div>
      )}
    </span>
  );
}

/** One folder's stack, then its direct dependencies, runtime before dev. */
function FolderStack({ folder, open }: { folder: Folder; open: (folder: string, file: string) => void }) {
  const runtime = folder.dependencies.filter((d) => !d.dev);
  const dev = folder.dependencies.filter((d) => d.dev);
  const dep = (d: (typeof folder.dependencies)[number]) => (
    <button
      className={`rounded px-1.5 py-px text-[10.5px] hover:bg-(--bg-chip) ${d.dev ? "text-(--fg-dim)" : "text-(--fg-body)"}`}
      key={`${d.name}:${d.dev}`}
      onClick={() => open(folder.path, d.source)}
      title={`${d.source}${d.dev ? " · for development" : ""}`}
      type="button"
    >
      {d.name}
      {d.version && <span className="text-(--fg-faint)"> {d.version}</span>}
    </button>
  );
  return (
    <div className="flex min-w-0 flex-col gap-1.5 rounded-md border border-(--line-soft) bg-(--bg-panel) p-2">
      <span className="truncate font-bold text-(--fg) text-[11.5px]" title={folder.path}>
        {folder.name}
      </span>
      {folder.stack.length > 0 && (
        <div className="flex flex-wrap gap-1.5">
          {folder.stack.map((s) => (
            <button className={TILE} key={`${s.kind}:${s.name}`} onClick={() => open(folder.path, s.source)} title={`${s.kind} · ${s.source}`} type="button">
              <span className="size-2 shrink-0 rounded-sm" style={{ background: KIND_HUE[s.kind] ?? "var(--fg-dim)" }} />
              <span className="truncate text-(--fg-body)">{s.name}</span>
              {s.version && <span className="shrink-0 text-(--fg-dim) text-[10.5px]">{s.version}</span>}
            </button>
          ))}
        </div>
      )}
      {runtime.length > 0 && (
        <div className="flex max-h-28 flex-wrap items-center gap-x-0.5 overflow-y-auto">
          <span className={`${SUB} mr-1`}>DEPENDS ON</span>
          {runtime.map(dep)}
        </div>
      )}
      {dev.length > 0 && (
        <div className="flex max-h-20 flex-wrap items-center gap-x-0.5 overflow-y-auto">
          <span className={`${SUB} mr-1`}>FOR DEVELOPMENT</span>
          {dev.map(dep)}
        </div>
      )}
    </div>
  );
}
