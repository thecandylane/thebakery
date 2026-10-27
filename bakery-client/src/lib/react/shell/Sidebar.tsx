// The shell's sidebar, the operator's control panel (direction A in look C,
// "Crisp studio", picked 2026-10-08). Its top: the Bakery | Baker switch and
// this machine. Below it, colour-coded sections: in Bakery mode the
// workspaces (repos › worktrees › agents), then Containers and GitHub; in
// Baker mode the Baker's threads, the Bakery's own project, its containers
// and its GitHub. Only Settings sits at the bottom left. Every row is live
// data, every list shows all its rows and the sidebar scrolls.

import { type PointerEvent as ReactPointerEvent, type RefObject, useCallback, useRef, useState, type ReactNode } from "react";
import {
  AlignLeftIcon,
  BoxIcon,
  BrainIcon,
  ChefHatIcon,
  ChevronDownIcon,
  ChevronRightIcon,
  CircleCheckIcon,
  CircleXIcon,
  CodeIcon,
  FolderIcon,
  GitBranchIcon,
  GitMergeIcon,
  GitPullRequestIcon,
  GlobeIcon,
  HouseIcon,
  LayoutDashboardIcon,
  ListTodoIcon,
  Maximize2Icon,
  MessageSquareIcon,
  MonitorIcon,
  PlayIcon,
  PlusIcon,
  RotateCwIcon,
  ServerIcon,
  SettingsIcon,
  SquareIcon,
  XIcon,
} from "lucide-react";
import {
  containersList,
  containersLogs,
  containersRestart,
  containersStart,
  containersStop,
  projectOpen,
  workspace as openWorkspace,
  type Backend,
  type ContainerRow,
  type ProjectRollup,
} from "$lib/api.generated";
import { statusLabel, type Conversation } from "$lib/conversations";
import type { AgentMapNode } from "$lib/node";
import { membersOf } from "./floorModel";
import { inPrGroup } from "./githubModel";
import { usePrs, useRuns } from "./GitHubPanels";
import * as L from "./layout";
import { useDismiss } from "./PanelMenu";
import { revealContainer } from "./ContainersPanel";
import { DeleteMenu, ThreadMenu } from "./ThreadMenu";
import { QuickSettings } from "./QuickSettings";
import { baseOf, candidatesOf } from "./reviewModel";
import { type Rollup, useRollup } from "./rollup";
import type { ShellProject, ShellSnapshot, ShellStore } from "./shellSource.svelte";
import { isFailedRun, repoTree } from "./sidebarModel";
import { NewWorktreeForm, NodeBox, useNodeInfo } from "./SidebarParts";
import { useBacklog } from "./Backlog";
import { useMemoryProposals } from "./MemoryWiki";
import { kept, useCached } from "./cache";
import { Chip, PANEL_MIME, said, type SetLayout, StatusDot, usePoll, workspaceColor } from "./ui";

export type SidebarProps = {
  store: ShellStore;
  snap: ShellSnapshot;
  layout: L.Layout;
  nodes: readonly AgentMapNode[];
  setLayout: SetLayout;
  onMode: (mode: L.Layout["mode"]) => void;
  /** Opens a panel (a stream id or a "@tool" id) in a workspace. */
  openIn: (wsId: string, id: string) => void;
};

const URGENCY = ["needs_input", "failed", "working", "done", "idle", "stopped"];
const ROW = "flex h-[26px] w-full min-w-0 items-center gap-2 rounded px-[9px] text-left text-xs hover:bg-(--bg-chip)";
/** The row on screen: filled, with a hairline. */
const PICKED = "bg-(--accent-sel) text-(--fg) shadow-[inset_0_0_0_1px_var(--accent-border)] hover:bg-(--accent-sel)";
const ICON_BUTTON = "grid size-5 shrink-0 place-items-center rounded hover:bg-(--bg-chip)";
const openSettings = (section: string) =>
  window.dispatchEvent(new CustomEvent("bakery:open-settings", { detail: section }));

/** The gear: quick settings above it, and All settings (⌘,) from there. */
function Gear() {
  const [open, setOpen] = useState(false);
  const box = useRef<HTMLDivElement>(null);
  useDismiss(open, box, () => setOpen(false));
  return (
    <div className="relative px-2.5 py-2" ref={box}>
      {open && (
        <div className="absolute bottom-full left-2.5 z-40 mb-0.5">
          <QuickSettings
            onAll={() => {
              setOpen(false);
              openSettings("agents");
            }}
          />
        </div>
      )}
      <button
        aria-expanded={open}
        aria-label="Quick settings"
        className={`grid size-7 place-items-center rounded-md border ${open ? "border-(--accent-border) bg-(--bg-pressed) text-(--accent-fg)" : "border-(--line) bg-(image:--grad-button) text-(--fg-2) hover:text-(--fg)"}`}
        onClick={() => setOpen((v) => !v)}
        title="Quick settings"
        type="button"
      >
        <SettingsIcon className="size-[15px]" />
      </button>
    </div>
  );
}

/** The subagents a conversation's agents launched, most urgent first. */
function subagentsOf(nodes: readonly AgentMapNode[], streamId: string): AgentMapNode[] {
  return nodes
    .filter((n) => n.kind === "subagent" && n.stream_id === streamId)
    .sort((a, b) => URGENCY.indexOf(a.status) - URGENCY.indexOf(b.status) || (a.last_ts < b.last_ts ? 1 : -1));
}

/** One section: a band in its hue with a bar on the left, then its rows. */
const COLLAPSED_KEY = "bakery.sidebar.collapsed";

/** The sections folded in this window, by label. */
function readCollapsed(): Set<string> {
  try {
    const kept = JSON.parse(localStorage.getItem(COLLAPSED_KEY) ?? "[]");
    return new Set(Array.isArray(kept) ? kept.filter((x): x is string => typeof x === "string") : []);
  } catch {
    return new Set();
  }
}

/** Whether a section is folded, and the toggle that remembers it. */
function useCollapsed(label: string): [boolean, () => void] {
  const [closed, setClosed] = useState(() => readCollapsed().has(label));
  const toggle = () =>
    setClosed((was) => {
      const all = readCollapsed();
      if (was) all.delete(label);
      else all.add(label);
      try {
        localStorage.setItem(COLLAPSED_KEY, JSON.stringify([...all]));
      } catch {
        // Not kept: it opens again next time.
      }
      return !was;
    });
  return [closed, toggle];
}

function Section({
  label,
  hue,
  icon,
  chip,
  extra,
  action,
  last,
  children,
}: {
  label: string;
  hue: string;
  icon: ReactNode;
  chip?: ReactNode;
  /** Buttons shown while the pointer is on the band. */
  extra?: ReactNode;
  action?: { label: string; onClick: () => void };
  last?: boolean;
  children: ReactNode;
}) {
  const [closed, toggle] = useCollapsed(label);
  return (
    <section
      aria-label={label}
      className={`flex flex-col ${closed ? "" : "pb-1.5"} ${last ? "" : "border-(--line-chrome) border-b"}`}
    >
      <div
        className="group/head flex h-[30px] shrink-0 items-center gap-[7px] pr-2 pl-1.5"
        style={{ background: `color-mix(in srgb, ${hue} 8%, transparent)`, boxShadow: `inset 3px 0 0 ${hue}`, color: hue }}
      >
        <button
          aria-expanded={!closed}
          aria-label={closed ? `Show ${label}` : `Fold ${label}`}
          className="grid size-4 shrink-0 place-items-center rounded opacity-70 hover:opacity-100"
          onClick={toggle}
          type="button"
        >
          {closed ? <ChevronRightIcon className="size-3" /> : <ChevronDownIcon className="size-3" />}
        </button>
        {icon}
        <h2
          className="m-0 min-w-0 flex-1 cursor-pointer select-none truncate font-bold text-[10.5px] uppercase tracking-[.08em]"
          onClick={toggle}
        >
          {label}
        </h2>
        {extra && <span className="hidden group-focus-within/head:flex group-hover/head:flex">{extra}</span>}
        {chip}
        {action && (
          <button aria-label={action.label} className={ICON_BUTTON} onClick={action.onClick} title={action.label} type="button">
            <PlusIcon className="size-3" />
          </button>
        )}
      </div>
      {!closed && <div className="flex flex-col gap-px px-1.5 pt-1">{children}</div>}
    </section>
  );
}

/** What an agent row says on its right: only that it failed; its dot says the
 *  rest, a thread waiting on the operator included. */
function AgentState({ status }: { status: Conversation["status"] }) {
  if (status === "failed") return <Chip tone="bad">{statusLabel(status).toUpperCase()}</Chip>;
  return null;
}

/** One agent: its status, name and, under it, why it waits or what it
 *  launched. Drag it into the screen; hover for its worktree's Code and Preview. */
function AgentRow({
  conv,
  nodes,
  focused,
  indent,
  title,
  sub,
  onOpen,
  onTool,
}: {
  conv: Conversation;
  nodes: readonly AgentMapNode[];
  focused: boolean;
  indent: number;
  title?: string;
  sub?: string;
  onOpen: () => void;
  onTool?: (kind: "code" | "preview") => void;
}) {
  const subs = subagentsOf(nodes, conv.streamId);
  const second =
    (conv.status === "needs_input" || conv.status === "failed") && conv.note
      ? conv.note
      : subs.length > 0
        ? `${subs.length} subagent${subs.length > 1 ? "s" : ""} · ${subs
            .slice(0, 2)
            .map((n) => n.label)
            .join(", ")}`
        : (sub ?? "");
  const [menu, setMenu] = useState(false);
  // While its buttons show (hover, focus, an open menu), the row's text ends
  // before them rather than under them.
  const room = onTool
    ? `group-hover/agent:pr-[74px] group-focus-within/agent:pr-[74px] ${menu ? "pr-[74px]" : "pr-[9px]"}`
    : `group-hover/agent:pr-[30px] group-focus-within/agent:pr-[30px] ${menu ? "pr-[30px]" : "pr-[9px]"}`;
  return (
    <div
      className="group/agent relative"
      data-testid="shell-agent-card"
      draggable
      onDragStart={(e) => {
        e.dataTransfer.setData(PANEL_MIME, conv.streamId);
        e.dataTransfer.effectAllowed = "move";
      }}
    >
      <button
        aria-current={focused || undefined}
        className={`flex min-h-[26px] w-full min-w-0 flex-col justify-center rounded py-[3px] ${room} text-left text-xs ${focused ? PICKED : "text-(--fg-body) hover:bg-(--bg-chip)"}`}
        onClick={onOpen}
        style={{ paddingLeft: indent }}
        title={conv.note || conv.title}
        type="button"
      >
        <span className="flex w-full min-w-0 items-center gap-2">
          <StatusDot status={conv.status} streamId={conv.streamId} title={statusLabel(conv.status)} />
          <span className={`min-w-0 flex-1 truncate ${focused ? "font-semibold" : ""}`}>{title ?? conv.title}</span>
          <AgentState status={conv.status} />
        </span>
        {second && <span className="w-full truncate pl-4 text-(--fg-dim) text-[11px]">{second}</span>}
      </button>
      <span className={`absolute top-[3px] right-1 gap-0.5 rounded bg-(--bg-chrome) group-focus-within/agent:flex group-hover/agent:flex ${menu ? "flex" : "hidden"}`}>
        {onTool && (
          <>
          <button
            aria-label={`Open ${conv.title} in Code`}
            className={`${ICON_BUTTON} text-(--hue-containers)`}
            onClick={() => onTool("code")}
            title="Code"
            type="button"
          >
            <CodeIcon className="size-3" />
          </button>
          <button
            aria-label={`Preview ${conv.title}`}
            className={`${ICON_BUTTON} text-(--violet)`}
            onClick={() => onTool("preview")}
            title="Preview"
            type="button"
          >
            <GlobeIcon className="size-3" />
          </button>
          </>
        )}
        <ThreadMenu onOpenChange={setMenu} streamId={conv.streamId} />
      </span>
    </div>
  );
}

function Tally({ mine }: { mine: readonly Conversation[] }) {
  const shown = (["needs_input", "failed"] as const)
    .map((s) => [s, mine.filter((c) => c.status === s).length] as const)
    .filter(([, n]) => n > 0);
  if (shown.length === 0) return null;
  return (
    <span className="flex shrink-0 items-center gap-[3px]" title={shown.map(([s, n]) => `${n} ${statusLabel(s)}`).join(", ")}>
      {shown.map(([s]) => (
        <StatusDot key={s} status={s} />
      ))}
    </span>
  );
}

type Tree = ReturnType<typeof repoTree<Conversation>>;
const treeOf = (project: ShellProject, mine: readonly Conversation[], rollup: ProjectRollup | null): Tree =>
  repoTree([...mine], project.checkouts, rollup?.repos.map((r) => r.path) ?? []);

/** A project's repos › worktrees › agents. With one repo, its "new worktree"
 *  sits on the project's own row; with several, on each repo's label. */
function RepoTree({
  p,
  w,
  project,
  tree,
  cutting,
  onCut,
}: {
  p: SidebarProps;
  w: L.Workspace;
  project: ShellProject;
  tree: Tree;
  cutting: string | null;
  onCut: (root: string | null) => void;
}) {
  const open = new Set(w.columns.flatMap((c) => c.panels));
  const agent = (c: Conversation, indent: number) => (
    <AgentRow
      conv={c}
      focused={w.focus === c.streamId}
      indent={indent}
      key={c.streamId}
      nodes={p.nodes}
      onOpen={() => p.openIn(w.id, c.streamId)}
      onTool={(kind) => p.openIn(w.id, `@${kind}:${c.streamId}`)}
    />
  );

  return (
    <>
      {tree.repos.map((r) => (
        <div className="flex flex-col gap-px" key={r.root}>
          {tree.repos.length > 1 && (
            <div className="flex h-6 items-center gap-2 pr-0.5 pl-[9px] text-(--fg-dim) text-[10.5px]" title={r.root}>
              <span className="min-w-0 flex-1 truncate font-bold uppercase tracking-[.04em]">{r.name}</span>
              <button
                aria-label={`New worktree in ${r.name}`}
                className={ICON_BUTTON}
                onClick={() => onCut(r.root)}
                title="New worktree"
                type="button"
              >
                <PlusIcon className="size-3" />
              </button>
              <DeleteMenu label={r.name} target={{ kind: "repo", project_id: project.id, repo: r.root }} />
            </div>
          )}
          {cutting === r.root && (
            <NewWorktreeForm
              create={async (name) => {
                const opened = await projectOpen({ repo: r.root, name, project_id: project.id });
                p.store.refresh();
                p.openIn(w.id, opened.context.stream_id);
              }}
              onDone={() => onCut(null)}
            />
          )}
          {r.worktrees.map((g) => {
            const first = g.members.find((m) => open.has(m.streamId)) ?? g.members[0];
            return (
              <div className="flex flex-col gap-px" key={g.path}>
                <button
                  className={`${ROW} text-(--fg-2)`}
                  onClick={() => first && p.openIn(w.id, first.streamId)}
                  title={g.path}
                  type="button"
                >
                  <GitBranchIcon className="size-[13px] shrink-0 text-(--violet)" />
                  <span className="min-w-0 flex-1 truncate">{g.branch || "detached"}</span>
                </button>
                {g.members.map((c) => agent(c, 29))}
              </div>
            );
          })}
        </div>
      ))}
      {tree.loose.map((c) => agent(c, 9))}
    </>
  );
}

/** One workspace of the Bakery: its row, then (open) its project's tree. */
function WorkspaceRow({
  p,
  w,
  i,
  rollup,
  naming,
  cutting,
  onCut,
  onRename,
}: {
  p: SidebarProps;
  w: L.Workspace;
  i: number;
  rollup: Rollup;
  naming: ReactNode | null;
  cutting: string | null;
  onCut: (root: string | null) => void;
  onRename: () => void;
}) {
  const { snap, layout, setLayout } = p;
  /** The operator's own expand/collapse; otherwise only the active one is open. */
  const [choice, setChoice] = useState<boolean | null>(null);
  const active = w.id === layout.active;
  // It opens while it, or one of its project's orc workspaces, shows.
  const holdsShown = !!w.projectId && layout.orcs.some((o) => o.projectId === w.projectId && o.id === layout.active);
  const expanded = choice ?? (active || holdsShown);
  const mine = membersOf(w, snap.projects, snap.conversations);
  const project = w.projectId ? snap.projects.find((x) => x.id === w.projectId) : undefined;
  const data = active ? rollup.data : null;
  const tree = project ? treeOf(project, mine, data) : null;
  const single = tree?.repos.length === 1 ? tree.repos[0].root : null;

  return (
    <div className="flex flex-col gap-px">
      {naming ?? (
        <div className="group flex items-center gap-0.5">
          <button
            aria-expanded={expanded}
            aria-label={`${expanded ? "Collapse" : "Expand"} ${w.name}`}
            className={`${ICON_BUTTON} text-(--fg-dim) hover:text-(--fg-2)`}
            onClick={() => setChoice(!expanded)}
            type="button"
          >
            {expanded ? <ChevronDownIcon className="size-3" /> : <ChevronRightIcon className="size-3" />}
          </button>
          <button
            className={`${ROW} h-7 flex-1 pl-1 ${active ? "text-(--fg)" : "text-(--fg-body)"}`}
            data-testid="shell-workspace"
            onClick={() => setLayout((l) => L.setActive(l, w.id))}
            onDoubleClick={() => w.projectId === null && onRename()}
            title={w.name}
            type="button"
          >
            <span
              className="grid size-4 shrink-0 place-items-center rounded font-bold text-(--fg-on-solid) text-[10px]"
              style={{ background: workspaceColor(i) }}
            >
              {(w.name[0] ?? "?").toUpperCase()}
            </span>
            <span className={`min-w-0 flex-1 truncate ${active ? "font-semibold" : ""}`}>{w.name}</span>
            <Tally mine={mine} />
            {i < 9 && <span className="hidden text-(--fg-dim) text-[10px] group-hover:inline">⌃{i + 1}</span>}
          </button>
          {w.projectId === null && active && (
            <button
              aria-label={`Remove workspace ${w.name}`}
              className={`${ICON_BUTTON} text-(--fg-dim) opacity-0 hover:text-(--bad) focus:opacity-100 group-hover:opacity-100`}
              onClick={() => setLayout((l) => L.removeWorkspace(l, w.id))}
              title="Remove workspace"
              type="button"
            >
              <XIcon className="size-3" />
            </button>
          )}
          {w.projectId !== null && (
            <button
              aria-label={`Move ${w.name} to the Baker`}
              className={`${ICON_BUTTON} text-(--fg-dim) opacity-0 hover:text-(--fg-2) focus:opacity-100 group-hover:opacity-100`}
              onClick={() => w.projectId && setLayout((l) => L.moveProject(l, w.projectId as string, "baker"))}
              title="Move to the Baker"
              type="button"
            >
              <ChefHatIcon className="size-3" />
            </button>
          )}
          {single && (
            <button
              aria-label={`New worktree in ${w.name}`}
              className={`${ICON_BUTTON} text-(--hue-project) ${expanded ? "" : "opacity-0 focus:opacity-100 group-hover:opacity-100"}`}
              onClick={() => {
                setChoice(true);
                onCut(single);
              }}
              title="New worktree"
              type="button"
            >
              <PlusIcon className="size-3" />
            </button>
          )}
          {project && (
            <DeleteMenu
              className="opacity-0 focus-within:opacity-100 group-hover:opacity-100"
              label={w.name}
              target={{ kind: "project", project_id: project.id }}
            />
          )}
        </div>
      )}
      {expanded && (
        <div className="flex flex-col gap-px pl-3">
          {project && tree ? (
            <RepoTree cutting={cutting} onCut={onCut} p={p} project={project} tree={tree} w={w} />
          ) : (
            mine.map((c) => (
              <AgentRow
                conv={c}
                focused={w.focus === c.streamId}
                indent={9}
                key={c.streamId}
                nodes={p.nodes}
                onOpen={() => p.openIn(w.id, c.streamId)}
                onTool={(kind) => p.openIn(w.id, `@${kind}:${c.streamId}`)}
                sub={c.place}
              />
            ))
          )}
          {/* The project's workspaces: each orc's, with its workers under it. */}
          {w.projectId &&
            layout.orcs
              .filter((o) => o.projectId === w.projectId)
              .map((o, j) => <OrcWorkspaceRow i={j} key={o.id} p={p} w={o} />)}
        </div>
      )}
    </div>
  );
}

function Workspaces({ p, rollup }: { p: SidebarProps; rollup: Rollup }) {
  const { layout, setLayout } = p;
  const [naming, setNaming] = useState<{ id: string | null; text: string } | null>(null);
  const [cutting, setCutting] = useState<string | null>(null);
  const [error, setError] = useState("");
  /** One commit per name typed: Enter and the blur after it make one. */
  const named = useRef(false);
  // A new workspace is the project's on screen (the node's: an orc on a
  // worktree of its repository); with none, a hand-made one.
  const projectId = L.shown(layout)?.projectId ?? null;

  function commitName() {
    if (!naming || named.current) return;
    named.current = true;
    const text = naming.text.trim();
    setNaming(null);
    if (!text) return;
    if (naming.id) return setLayout((l) => L.renameWorkspace(l, naming.id as string, text));
    if (!projectId) return setLayout((l) => L.addWorkspace(l, text));
    setError("");
    openWorkspace({ title: text, project_id: projectId }).then(
      (w) => {
        p.store.refresh();
        setLayout((l) => L.showNewOrc(l, { streamId: w.stream_id, title: w.title, projectId }));
      },
      (e) => setError(said(e)),
    );
  }
  const name = (id: string | null, text: string) => {
    named.current = false;
    setNaming({ id, text });
  };
  const nameInput = (
    <div className="px-1 py-1">
      <input
        aria-label="Workspace name"
        autoFocus
        className="w-full rounded border border-(--line) bg-(--bg-app) px-1.5 py-0.5 text-xs outline-none focus:border-(--accent)"
        onBlur={commitName}
        onChange={(e) => setNaming((n) => n && { ...n, text: e.target.value })}
        onKeyDown={(e) => {
          if (e.key === "Enter") commitName();
          if (e.key === "Escape") {
            named.current = true;
            setNaming(null);
          }
        }}
        placeholder="Workspace name"
        value={naming?.text ?? ""}
      />
    </div>
  );

  return (
    <Section
      action={{ label: "New workspace", onClick: () => name(null, "") }}
      hue="var(--hue-project)"
      icon={<FolderIcon className="size-[13px] shrink-0" />}
      label="Workspaces"
    >
      {layout.workspaces.map((w, i) => (
        <WorkspaceRow
          cutting={cutting}
          i={i}
          key={w.id}
          naming={naming?.id === w.id ? nameInput : null}
          onCut={setCutting}
          onRename={() => name(w.id, w.name)}
          p={p}
          rollup={rollup}
          w={w}
        />
      ))}
      {naming?.id === null && nameInput}
      {error && <p className="px-[9px] text-(--bad) text-[11px]">{error}</p>}
    </Section>
  );
}

export const ENGINES: { id: Backend; label: string }[] = [
  { id: "apple", label: "Apple container" },
  { id: "docker", label: "Docker (Colima)" },
  { id: "orbstack", label: "OrbStack" },
];
/** OrbStack counts once the engine room's kept read says it's installed. */
const installed = (id: Backend) => id !== "orbstack" || !!(kept("engine")?.data as { orbstack?: unknown } | undefined)?.orbstack;

export type ContainerRows = { backend: Backend; c: ContainerRow }[];
/** Every engine's containers in one list, kept for the sidebar and the
 *  control plane (`containers:rows`); an engine that can't be read adds none. */
export const readContainerRows = (): Promise<ContainerRows> =>
  Promise.all(
    ENGINES.filter(({ id }) => installed(id)).map(({ id }) =>
      containersList({ backend: id, kind: "containers" }).then(
        (l) => l.containers.map((c) => ({ backend: id, c })),
        () => [],
      ),
    ),
  ).then((all) => all.flat());

const SEG = "grid h-5 w-[22px] place-items-center bg-(image:--grad-button) hover:brightness-110 disabled:opacity-40";

/** One container with its actions on the row — no workspace needed: stop
 *  (or start), and its log, which opens right under it. */
export function ContainerLine({
  c,
  backend,
  tag,
  onChanged,
}: {
  c: ContainerRow;
  backend: Backend;
  tag: ReactNode;
  onChanged: () => void;
}) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [logs, setLogs] = useState<string | null>(null);
  const act = (run: () => Promise<unknown>) => {
    setBusy(true);
    setError("");
    run()
      .then(onChanged, (e) => setError(said(e)))
      .finally(() => setBusy(false));
  };
  const toggleLogs = () => {
    if (logs !== null) return setLogs(null);
    setLogs("");
    containersLogs({ backend, id: c.id, tail: 200 }).then(
      (res) => setLogs(String((res as { text?: unknown } | null)?.text ?? "")),
      (e) => {
        setLogs(null);
        setError(said(e));
      },
    );
  };
  return (
    <div className="flex flex-col">
      <div className={`${ROW} h-7 pr-1 text-(--fg-body)`} title={`${c.image} · ${c.status}`}>
        <span className={`size-[7px] shrink-0 rounded-full ${c.running ? "bg-(--ok)" : "bg-(--idle)"}`} />
        <span className={`min-w-0 flex-1 truncate ${c.running ? "" : "text-(--fg-dim)"}`}>{c.name}</span>
        {tag}
        <span className="flex shrink-0 overflow-hidden rounded-[5px] border border-(--line)">
          {c.running ? (
            <button
              aria-label={`Stop ${c.name}`}
              className={`${SEG} text-(--bad)`}
              disabled={busy}
              onClick={() => act(() => containersStop({ backend, id: c.id }))}
              title="Stop"
              type="button"
            >
              <SquareIcon className="size-2.5 fill-current" />
            </button>
          ) : (
            <button
              aria-label={`Start ${c.name}`}
              className={`${SEG} text-(--hue-containers)`}
              disabled={busy}
              onClick={() => act(() => containersStart({ backend, id: c.id }))}
              title="Start"
              type="button"
            >
              <PlayIcon className="size-2.5 fill-current" />
            </button>
          )}
          {c.running && (
            <button
              aria-label={`Restart ${c.name}`}
              className={`${SEG} border-(--line) border-l text-(--hue-containers)`}
              disabled={busy}
              onClick={() => act(() => containersRestart({ backend, kind: "containers", id: c.id }))}
              title="Restart"
              type="button"
            >
              <RotateCwIcon className="size-2.5" />
            </button>
          )}
          <button
            aria-expanded={logs !== null}
            aria-label={`Logs of ${c.name}`}
            className={`${SEG} border-(--line) border-l text-(--fg-2)`}
            onClick={toggleLogs}
            title="Logs"
            type="button"
          >
            <AlignLeftIcon className="size-3" />
          </button>
        </span>
      </div>
      {error && <p className="line-clamp-2 px-[9px] text-(--bad) text-[11px]">{error}</p>}
      {logs !== null && (
        <pre className="mx-1.5 mb-1 max-h-40 overflow-auto whitespace-pre-wrap break-all rounded border border-(--line-row) bg-(--bg-app) p-1.5 text-(--fg-2) text-[10.5px] leading-snug">
          {logs || "…"}
        </pre>
      )}
    </div>
  );
}

function Containers({ onOpen }: { onOpen: (() => void) | null }) {
  // As last read, at once (kept across restarts); then every 15 s.
  const { data, reload } = useCached<ContainerRows>("containers:rows", readContainerRows);
  const load = useCallback(() => void reload(), [reload]);
  usePoll(load, 15_000);
  /** An engine's containers, running ones first. */
  const all = (b: Backend) =>
    (data ?? [])
      .filter((r) => r.backend === b)
      .map((r) => r.c)
      .sort((x, y) => Number(y.running) - Number(x.running));

  return (
    <Section
      extra={
        onOpen && (
          <button aria-label="Open Containers" className={ICON_BUTTON} onClick={onOpen} title="Open Containers" type="button">
            <Maximize2Icon className="size-3" />
          </button>
        )
      }
      hue="var(--hue-containers)"
      icon={<BoxIcon className="size-[13px] shrink-0" />}
      label="Containers"
    >
      {ENGINES.filter((e) => all(e.id).length > 0).map((e) => (
        <div className="flex flex-col gap-px" key={e.id}>
          <div className="flex h-6 items-center pl-[9px] font-bold text-(--fg-dim) text-[10.5px] uppercase tracking-[.04em]">{e.label}</div>
          {/* Just the name: the row opens the container on its page. */}
          {all(e.id).map((c) => (
            <button
              className={`${ROW} h-7 text-(--fg-body) disabled:cursor-default`}
              disabled={!onOpen}
              key={c.id}
              onClick={() => {
                revealContainer(e.id, c);
                onOpen?.();
              }}
              title={`${c.name} · ${c.image} · ${c.status}`}
              type="button"
            >
              <span className={`size-[7px] shrink-0 rounded-full ${c.running ? "bg-(--ok)" : "bg-(--idle)"}`} />
              <span className={`min-w-0 flex-1 truncate text-left ${c.running ? "" : "text-(--fg-dim)"}`}>{c.name}</span>
            </button>
          ))}
        </div>
      ))}
    </Section>
  );
}

/** The machines, each opening the Machines panel (its engines and
 *  resources): this Mac for now; other machines join it later. */
function Machines({ onOpen }: { onOpen: (() => void) | null }) {
  const { data: info } = useNodeInfo();
  return (
    <Section
      extra={
        onOpen && (
          <button aria-label="Open Machines" className={ICON_BUTTON} onClick={onOpen} title="Open Machines" type="button">
            <Maximize2Icon className="size-3" />
          </button>
        )
      }
      hue="var(--link)"
      icon={<ServerIcon className="size-[13px] shrink-0" />}
      label="Machines"
    >
      <button className={`${ROW} text-(--fg-body) disabled:cursor-default`} disabled={!onOpen} onClick={() => onOpen?.()} type="button">
        <MonitorIcon className="size-[13px] shrink-0 text-(--link)" />
        <span className="min-w-0 flex-1 truncate">{info?.hostname || "This Mac"}</span>
      </button>
    </Section>
  );
}

/** The backlog's Doing and Queued, each opening the board; nothing at zero
 *  but the way in. */
function BacklogRows({ onOpen, onMemory }: { onOpen: () => void; onMemory: () => void }) {
  const items = useBacklog().data?.items ?? [];
  const toKeep = (useMemoryProposals().data?.proposals ?? []).filter((x) => !x.status || x.status === "pending").length;
  const doing = items.filter((i) => i.status === "doing").length;
  const queued = items.filter((i) => i.status === "queued").length;
  return (
    <Section hue="var(--warn)" icon={<ListTodoIcon className="size-[13px] shrink-0" />} label="Backlog">
      <button className={`${ROW} text-(--fg-body)`} onClick={onOpen} type="button">
        <span className="size-2 shrink-0 rounded-full bg-(--ok)" />
        <span className="min-w-0 flex-1 truncate">Doing</span>
        {doing > 0 && <Chip tone="ok">{doing}</Chip>}
      </button>
      <button className={`${ROW} text-(--fg-body)`} onClick={onOpen} type="button">
        <ListTodoIcon className="size-[13px] shrink-0 text-(--warn)" />
        <span className="min-w-0 flex-1 truncate">Queued</span>
        {queued > 0 && <Chip tone="warn">{queued}</Chip>}
      </button>
      <button className={`${ROW} text-(--fg-body)`} onClick={onMemory} type="button">
        <BrainIcon className="size-[13px] shrink-0 text-(--warn)" />
        <span className="min-w-0 flex-1 truncate">Memory</span>
        {toKeep > 0 && <Chip tone="warn">{toKeep} TO KEEP</Chip>}
      </button>
    </Section>
  );
}

function GitHub({
  project,
  onReview,
  onPrs,
  onRuns,
}: {
  project: ShellProject;
  onReview: () => void;
  onPrs: () => void;
  onRuns: () => void;
}) {
  // Counted from the inbox, every owner the operator follows: what waits on
  // them and what fails. Nothing shows while nothing does.
  const prs = usePrs();
  const runs = useRuns();
  const waiting = prs.data?.prs.filter((pr) => inPrGroup(pr, "waiting")).length ?? 0;
  const failing = runs.data?.runs.filter(isFailedRun).length ?? 0;
  const base = baseOf(project.checkouts);
  const branches = base ? candidatesOf(project.checkouts, base).length : 0;
  const unavailable = (error: string) => (
    <span className="shrink-0 text-(--fg-dim) text-[11px]" title={error}>
      unavailable
    </span>
  );

  return (
    <Section
      hue="var(--hue-github)"
      icon={<GitPullRequestIcon className="size-[13px] shrink-0" />}
      label="GitHub"
      last
    >
      <button className={`${ROW} text-(--fg-body)`} onClick={onReview} type="button">
        <GitMergeIcon className="size-[13px] shrink-0 text-(--violet)" />
        <span className="min-w-0 flex-1 truncate">Review &amp; merge</span>
        {branches > 0 && <Chip tone="accent">{branches}</Chip>}
      </button>
      <button className={`${ROW} text-(--fg-body)`} onClick={onPrs} type="button">
        <GitPullRequestIcon className="size-[13px] shrink-0 text-(--violet)" />
        <span className="min-w-0 flex-1 truncate">Pull requests</span>
        {waiting > 0 ? <Chip tone="warn">{waiting} WAITING ON YOU</Chip> : !prs.data && prs.error && unavailable(prs.error)}
      </button>
      <button className={`${ROW} text-(--fg-body)`} onClick={onRuns} type="button">
        {failing > 0 ? (
          <CircleXIcon className="size-[13px] shrink-0 text-(--bad)" />
        ) : (
          <CircleCheckIcon className="size-[13px] shrink-0 text-(--violet)" />
        )}
        <span className="min-w-0 flex-1 truncate">Actions</span>
        {failing > 0 ? <Chip tone="bad">{failing} FAILING</Chip> : !runs.data && runs.error && unavailable(runs.error)}
      </button>
    </Section>
  );
}

function BakerySide({ p }: { p: SidebarProps }) {
  // The workspace on screen: a project's, a hand-made one, or a project orc's.
  const active = p.layout.mode === "bakery" ? L.shown(p.layout) : null;
  const project = active?.projectId ? (p.snap.projects.find((x) => x.id === active.projectId) ?? null) : null;
  const rollup = useRollup(project?.id ?? null);
  return (
    <>
      <Workspaces p={p} rollup={rollup} />
      <Containers onOpen={active ? () => p.openIn(active.id, "@containers") : null} />
      <Machines onOpen={active ? () => p.openIn(active.id, "@engine") : null} />
      {active && project && (
        <GitHub
          onPrs={() => p.openIn(active.id, "@prs")}
          onReview={() => p.openIn(active.id, "@review")}
          onRuns={() => p.openIn(active.id, "@actions")}
          project={project}
        />
      )}
    </>
  );
}

/** The Bakery's own project in Baker mode: a section of its own. */
function OwnProject({ p, project, rollup }: { p: SidebarProps; project: ShellProject; rollup: ProjectRollup | null }) {
  const { snap, layout, setLayout } = p;
  const [cutting, setCutting] = useState<string | null>(null);
  const mine = snap.conversations.filter((c) => !c.bakerThread && project.streamIds.includes(c.streamId));
  const tree = treeOf(project, mine, rollup);
  const single = tree.repos.length === 1 ? tree.repos[0].root : null;
  return (
    <Section
      action={single ? { label: "New worktree", onClick: () => setCutting(single) } : undefined}
      chip={<DeleteMenu label={project.name} target={{ kind: "project", project_id: project.id }} />}
      extra={
        <button
          aria-label={`Move ${project.name} to the Bakery`}
          className={ICON_BUTTON}
          onClick={() => setLayout((l) => L.syncProjects(L.moveProject(l, project.id, "bakery"), snap.projects))}
          title="Move to the Bakery"
          type="button"
        >
          <HouseIcon className="size-3" />
        </button>
      }
      hue="var(--hue-project)"
      icon={<FolderIcon className="size-[13px] shrink-0" />}
      label={project.name}
    >
      <RepoTree cutting={cutting} onCut={setCutting} p={p} project={project} tree={tree} w={layout.baker} />
    </Section>
  );
}

/** A conversation on the Baker's side: one of its threads, an orc, or an orc's worker. */
const onBakerSide = (l: L.Layout, c: Conversation) => {
  const home = L.orcHome(l, c.workspace ? c.streamId : c.openedBy);
  return c.bakerThread || (home !== null && !L.workspaceOf(l, home)?.projectId);
};

/** One orc's workspace on the Baker's side: its row shows the workspace
 *  (and opens it), and under it the orc, then the workers it opened. */
function OrcWorkspaceRow({ p, w, i }: { p: SidebarProps; w: L.Workspace; i: number }) {
  const { snap, layout, setLayout } = p;
  const [choice, setChoice] = useState<boolean | null>(null);
  const active = L.shown(layout)?.id === w.id;
  const expanded = choice ?? active;
  const mine = membersOf(w, snap.projects, snap.conversations);
  return (
    <div className="flex flex-col gap-px">
      <div className="group flex items-center gap-0.5">
        <button
          aria-expanded={expanded}
          aria-label={`${expanded ? "Collapse" : "Expand"} ${w.name}`}
          className={`${ICON_BUTTON} text-(--fg-dim) hover:text-(--fg-2)`}
          onClick={() => setChoice(!expanded)}
          type="button"
        >
          {expanded ? <ChevronDownIcon className="size-3" /> : <ChevronRightIcon className="size-3" />}
        </button>
        <button
          className={`${ROW} h-7 flex-1 pl-1 ${active ? "text-(--fg)" : "text-(--fg-body)"}`}
          data-testid="shell-orc-workspace"
          onClick={() => setLayout((l) => L.setActive(l, w.id))}
          title={w.name}
          type="button"
        >
          <span className="grid size-4 shrink-0 place-items-center rounded font-bold text-(--fg-on-solid) text-[10px]" style={{ background: workspaceColor(i) }}>
            {(w.name[0] ?? "?").toUpperCase()}
          </span>
          <span className={`min-w-0 flex-1 truncate ${active ? "font-semibold" : ""}`}>{w.name}</span>
          <Tally mine={mine} />
        </button>
        {w.orc && (
          <DeleteMenu
            className="opacity-0 focus-within:opacity-100 group-hover:opacity-100"
            label={w.name}
            target={{ kind: "workspace", stream_id: w.orc }}
          />
        )}
      </div>
      {expanded &&
        mine.map((c) => (
          <AgentRow
            conv={c}
            focused={active && w.focus === c.streamId}
            indent={c.streamId === w.orc ? 22 : 34}
            key={c.streamId}
            nodes={p.nodes}
            onOpen={() => p.openIn(w.id, c.streamId)}
            title={c.streamId === w.orc ? "orc" : undefined}
          />
        ))}
    </div>
  );
}

/** The Baker's side's workspaces: each orc's, with its workers under it;
 *  a new one is the Bakery's own, made by the node and shown at once. */
function OrcWorkspaces({ p }: { p: SidebarProps }) {
  const [naming, setNaming] = useState<string | null>(null);
  const [error, setError] = useState("");
  /** One workspace per name typed: Enter and the blur after it make one. */
  const made = useRef(false);
  const create = () => {
    if (made.current) return;
    made.current = true;
    const title = naming?.trim();
    setNaming(null);
    if (!title) return;
    setError("");
    openWorkspace({ title }).then(
      (w) => {
        p.store.refresh();
        p.setLayout((l) => L.showNewOrc(l, { streamId: w.stream_id, title: w.title }));
      },
      (e) => setError(said(e)),
    );
  };
  return (
    <Section
      action={{
        label: "New workspace",
        onClick: () => {
          made.current = false;
          setNaming("");
        },
      }}
      hue="var(--hue-project)"
      icon={<FolderIcon className="size-[13px] shrink-0" />}
      label="Workspaces"
    >
      {p.layout.orcs
        .filter((w) => !w.projectId)
        .map((w, i) => (
          <OrcWorkspaceRow i={i} key={w.id} p={p} w={w} />
        ))}
      {naming !== null && (
        <div className="px-1 py-1">
          <input
            aria-label="Workspace name"
            autoFocus
            className="w-full rounded border border-(--line) bg-(--bg-app) px-1.5 py-0.5 text-xs outline-none focus:border-(--accent)"
            onBlur={create}
            onChange={(e) => setNaming(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") create();
              if (e.key === "Escape") {
                made.current = true;
                setNaming(null);
              }
            }}
            placeholder="Workspace name"
            value={naming}
          />
        </div>
      )}
      {error && <p className="px-[9px] text-(--bad) text-[11px]">{error}</p>}
    </Section>
  );
}

/** The Baker's threads and its own projects: the top of Baker mode, and
 *  the rail's Threads peek there. */
function BakerTop({ p, rollup }: { p: SidebarProps; rollup: Rollup }) {
  const { snap, layout } = p;
  const [error, setError] = useState("");
  const threads = snap.conversations.filter((c) => c.bakerThread);
  const own = snap.projects.filter((x) => layout.bakerProjects.includes(x.id));
  return (
    <>
      <Section
        action={{
          label: "New thread",
          onClick: () => {
            setError("");
            p.store.newBakerThread().then(
              (id) => p.openIn(L.BAKER, id),
              (e) => setError(said(e)),
            );
          },
        }}
        hue="var(--hue-threads)"
        icon={<MessageSquareIcon className="size-[13px] shrink-0" />}
        label="Threads"
      >
        {threads.map((c) => (
          <AgentRow
            conv={c}
            focused={layout.baker.focus === c.streamId}
            indent={9}
            key={c.streamId}
            nodes={p.nodes}
            onOpen={() => p.openIn(L.BAKER, c.streamId)}
          />
        ))}
        {snap.bakerError && <p className="px-[9px] text-(--bad) text-[11px]">{snap.bakerError}</p>}
        {error && <p className="px-[9px] text-(--bad) text-[11px]">{error}</p>}
      </Section>
      <OrcWorkspaces p={p} />
      {own.map((x) => (
        <OwnProject key={x.id} p={p} project={x} rollup={x.id === own[0]?.id ? rollup.data : null} />
      ))}
    </>
  );
}

/** Whether the Bakery's own control plane is what the window shows: the
 *  Baker's home page. */
const controlShown = (l: L.Layout) => L.isHome(L.shown(l));

/** The Bakery's own control plane: the Baker's home page, as a project's is
 *  the first tab of its workspace in Bakery mode. */
function BakeryControlRow({ p }: { p: SidebarProps }) {
  const on = controlShown(p.layout);
  return (
    <div className="border-(--line-chrome) border-b p-1.5">
      <button
        aria-current={on || undefined}
        className={`${ROW} h-7 ${on ? PICKED : "text-(--fg-body)"}`}
        data-testid="shell-bakery-control"
        onClick={() => p.setLayout((l) => L.showControl(l, L.BAKER))}
        type="button"
      >
        <LayoutDashboardIcon className="size-[13px] shrink-0 text-(--baker-solid)" />
        <span className={`min-w-0 flex-1 truncate ${on ? "font-semibold" : ""}`}>Bakery control plane</span>
        <Tally mine={p.snap.conversations} />
      </button>
    </div>
  );
}

/** Baker mode: the Bakery's own control plane, the Baker's threads, then
 *  the Bakery's own project (the operator works on the Bakery with the
 *  Baker; Bakery mode is for everything else), its containers and its
 *  GitHub. */
function BakerSide({ p }: { p: SidebarProps }) {
  const own = p.snap.projects.filter((x) => p.layout.bakerProjects.includes(x.id));
  const rollup = useRollup(own[0]?.id ?? null);
  // Tools open in the Baker's side's workspace on screen: the Baker's own, or an orc's.
  const here = L.shown(p.layout)?.id ?? L.BAKER;
  return (
    <>
      <BakeryControlRow p={p} />
      <BakerTop p={p} rollup={rollup} />
      <BacklogRows onOpen={() => p.openIn(here, "@backlog")} onMemory={() => p.openIn(here, "@memory")} />
      <Containers onOpen={() => p.openIn(here, "@containers")} />
      <Machines onOpen={() => p.openIn(here, "@engine")} />
      {own[0] && (
        <GitHub
          onPrs={() => p.openIn(here, "@prs")}
          onReview={() => p.openIn(here, "@review")}
          onRuns={() => p.openIn(here, "@actions")}
          project={own[0]}
        />
      )}
    </>
  );
}

const MODES = [
  { id: "bakery", label: "Bakery", hue: "var(--hue-project)", icon: <HouseIcon className="size-3.5" /> },
  { id: "baker", label: "Baker", hue: "var(--hue-threads)", icon: <ChefHatIcon className="size-3.5" /> },
] as const;

/** The rail's width, and how narrow a drag goes before the sidebar folds
 *  into it (or how wide before the rail opens back out). */
const RAIL = 52;
const FOLD = 150;

/** The sidebar's edge, dragged: 220–460 wide, and past its narrowest it
 *  folds to the rail (Round 10); the rail's edge dragged out past that
 *  opens the sidebar again. It switches the moment the drag crosses that
 *  line, not on release (the operator); the page body holds the pointer,
 *  since the switch swaps the aside, and its edge, under it. */
function dragEdge(e: ReactPointerEvent, el: RefObject<HTMLElement | null>, commit: (s: { rail: boolean; width?: number }) => void) {
  const at = el.current;
  if (!at) return;
  e.preventDefault();
  const start = e.clientX;
  const from = at.getBoundingClientRect().width;
  const widthAt = (x: number) => from + x - start;
  const clamp = (w: number) => Math.round(Math.min(460, Math.max(220, w)));
  let folded = from < FOLD;
  const body = document.body;
  body.setPointerCapture(e.pointerId);
  const cursor = body.style.cursor;
  body.style.cursor = "col-resize";
  const move = (ev: PointerEvent) => {
    const w = widthAt(ev.clientX);
    if (w < FOLD !== folded) {
      folded = w < FOLD;
      commit(folded ? { rail: true } : { rail: false, width: clamp(w) });
    } else if (!folded && el.current) el.current.style.width = `${clamp(w)}px`;
  };
  const up = (ev: PointerEvent) => {
    body.removeEventListener("pointermove", move);
    body.removeEventListener("pointerup", up);
    body.removeEventListener("pointercancel", up);
    body.style.cursor = cursor;
    const w = widthAt(ev.clientX);
    commit(w < FOLD ? { rail: true } : { rail: false, width: clamp(w) });
  };
  body.addEventListener("pointermove", move);
  body.addEventListener("pointerup", up);
  body.addEventListener("pointercancel", up);
}

const RAIL_BUTTON =
  "relative grid size-9 shrink-0 place-items-center rounded-[9px] border border-transparent hover:bg-(--bg-chip) disabled:opacity-40";
/** A count on a rail icon, only while something waits on the operator. */
const railCount = (n: number) =>
  n > 0 && (
    <span className="absolute -top-1 -right-1.5 grid h-4 min-w-4 place-items-center rounded-full border-2 border-(--bg-chrome) bg-(--warn) px-0.5 font-bold text-(--fg-on-solid) text-[9.5px] leading-none">
      {n}
    </span>
  );
/** A dot on a rail icon: something there needs the operator, or failed. */
const railDot = (on: boolean, color: string) =>
  on && <span className="absolute -right-0.5 -bottom-0.5 size-2.5 rounded-full border-2 border-(--bg-chrome)" style={{ background: color }} />;

/** One of the rail's sections: its icon opens its page, and hovering it
 *  peeks at the section's list beside the rail. */
function RailPeek({
  label,
  icon,
  marks,
  onOpen,
  children,
}: {
  label: string;
  icon: ReactNode;
  marks?: ReactNode;
  onOpen: (() => void) | null;
  children: ReactNode;
}) {
  const [peek, setPeek] = useState(false);
  return (
    <div className="relative" onMouseEnter={() => setPeek(true)} onMouseLeave={() => setPeek(false)}>
      <button
        aria-label={label}
        className={RAIL_BUTTON}
        disabled={!onOpen}
        onClick={() => {
          setPeek(false);
          onOpen?.();
        }}
        type="button"
      >
        {icon}
        {marks}
      </button>
      {peek && (
        <div className="absolute top-0 left-full z-50 pl-1.5">
          <div
            aria-label={label}
            className="max-h-[70vh] w-[280px] overflow-y-auto rounded-r-xl border border-(--line-strong) bg-(--bg-chrome) shadow-(--shadow-menu)"
            role="dialog"
          >
            {children}
          </div>
        </div>
      )}
    </div>
  );
}

/** The sidebar folded (Round 10, "R3, but everything else from R1 and R2"):
 *  Bakery and Baker, the workspaces as tiles, then Threads, Containers and
 *  GitHub, each opening its page and peeking at its list on hover; counts
 *  only while something waits on the operator. */
function Rail({ p }: { p: SidebarProps }) {
  const { snap, layout, setLayout } = p;
  const baker = layout.mode === "baker";
  const active = baker ? null : L.shown(layout);
  const own = snap.projects.filter((x) => layout.bakerProjects.includes(x.id));
  const project = baker ? (own[0] ?? null) : active?.projectId ? (snap.projects.find((x) => x.id === active.projectId) ?? null) : null;
  const rollup = useRollup(project?.id ?? null);
  const wsId = L.shown(layout)?.id ?? null;
  const go = (id: string) => (wsId ? () => p.openIn(wsId, id) : null);
  const prs = usePrs();
  const runs = useRuns();
  const waitingPrs = prs.data?.prs.filter((pr) => inPrGroup(pr, "waiting")).length ?? 0;
  const failing = runs.data?.runs.some(isFailedRun) ?? false;
  const needs = (bakerSide: boolean) => snap.conversations.filter((c) => onBakerSide(layout, c) === bakerSide && c.status === "needs_input").length;
  const rule = <span className="my-0.5 h-px w-6 shrink-0 bg-(--line-chrome)" />;
  const controlOn = controlShown(layout);

  return (
    <>
      {MODES.map((m) => {
        const on = layout.mode === m.id;
        return (
          <button
            aria-label={m.label}
            aria-pressed={on}
            className={`${RAIL_BUTTON} ${on ? "border-(--accent-fg) bg-(image:--grad-accent) text-(--fg-on-solid) hover:bg-(image:--grad-accent)" : ""}`}
            data-testid={`shell-mode-${m.id}`}
            key={m.id}
            onClick={() => p.onMode(m.id)}
            title={`${m.label} (${m.id === "baker" ? "⌘2" : "⌘1"})`}
            type="button"
          >
            <span style={on ? undefined : { color: m.hue }}>{m.icon}</span>
            {railDot(!on && needs(m.id === "baker") > 0, "var(--warn)")}
          </button>
        );
      })}
      {rule}
      {baker && (
        <>
          {/* The Bakery's own control plane, as a project's tile opens its own. */}
          <div className="relative">
            {controlOn && <span className="absolute top-1/2 -left-1 h-5 w-[3px] -translate-y-1/2 rounded-r-full bg-(--fg)" />}
            <button
              aria-label="Bakery control plane"
              aria-pressed={controlOn}
              className={`${RAIL_BUTTON} text-(--fg-on-solid) ${controlOn ? "" : "opacity-75 hover:opacity-100"}`}
              data-testid="shell-rail-bakery-control"
              onClick={() => setLayout((l) => L.showControl(l, L.BAKER))}
              style={{
                background: "var(--baker-solid)",
                boxShadow: controlOn ? "inset 0 0 0 2px color-mix(in srgb, var(--fg) 60%, transparent)" : undefined,
              }}
              title="Bakery control plane"
              type="button"
            >
              <LayoutDashboardIcon className="size-4" />
            </button>
          </div>
          {rule}
        </>
      )}
      {!baker && (
        <>
          <div className="flex min-h-0 shrink flex-col items-center gap-1.5 overflow-y-auto px-1 py-0.5">
            {layout.workspaces.map((w, i) => {
              const on = w.id === layout.active;
              const color = workspaceColor(i);
              return (
                <div className="relative" key={w.id}>
                  {/* The one shown: a bar at the rail's edge and a ring inside
                      the tile, so nothing reaches past the rail. */}
                  {on && <span className="absolute top-1/2 -left-1 h-5 w-[3px] -translate-y-1/2 rounded-r-full bg-(--fg)" />}
                  <button
                    aria-label={w.name}
                    aria-pressed={on}
                    className={`${RAIL_BUTTON} font-bold text-(--fg-on-solid) text-[13px] ${on ? "" : "opacity-75 hover:opacity-100"}`}
                    data-testid="shell-rail-workspace"
                    onClick={() => setLayout((l) => L.setActive(l, w.id))}
                    style={{ background: color, boxShadow: on ? "inset 0 0 0 2px color-mix(in srgb, var(--fg) 60%, transparent)" : undefined }}
                    title={w.name}
                    type="button"
                  >
                    {(w.name[0] ?? "?").toUpperCase()}
                    {railDot(membersOf(w, snap.projects, snap.conversations).some((c) => c.status === "needs_input"), "var(--warn)")}
                  </button>
                </div>
              );
            })}
            <button
              aria-label="New workspace"
              className={`${RAIL_BUTTON} border-(--line) border-dashed text-(--fg-dim) hover:text-(--fg-2)`}
              onClick={() => setLayout((l) => L.addWorkspace(l, "New workspace"))}
              title="New workspace"
              type="button"
            >
              <PlusIcon className="size-3.5" />
            </button>
          </div>
          {rule}
        </>
      )}
      <RailPeek
        icon={<MessageSquareIcon className="size-4 text-(--hue-threads)" />}
        label="Threads"
        marks={railCount(needs(baker))}
        onOpen={go("@threads")}
      >
        {baker ? <BakerTop p={p} rollup={rollup} /> : <Workspaces p={p} rollup={rollup} />}
      </RailPeek>
      <RailPeek icon={<BoxIcon className="size-4 text-(--hue-containers)" />} label="Containers" onOpen={go("@containers")}>
        <Containers onOpen={go("@containers")} />
      </RailPeek>
      <RailPeek icon={<ServerIcon className="size-4 text-(--link)" />} label="Machines" onOpen={go("@engine")}>
        <Machines onOpen={go("@engine")} />
      </RailPeek>
      {project && wsId && (
        <RailPeek
          icon={<GitPullRequestIcon className="size-4 text-(--hue-github)" />}
          label="GitHub"
          marks={
            <>
              {railCount(waitingPrs)}
              {railDot(failing, "var(--bad)")}
            </>
          }
          onOpen={go("@prs")}
        >
          <GitHub
            onPrs={() => p.openIn(wsId, "@prs")}
            onReview={() => p.openIn(wsId, "@review")}
            onRuns={() => p.openIn(wsId, "@actions")}
            project={project}
          />
        </RailPeek>
      )}
      <span className="flex-1" />
      <Gear />
    </>
  );
}

export function Sidebar(p: SidebarProps) {
  const { snap, layout, setLayout } = p;
  const el = useRef<HTMLElement>(null);
  const baker = layout.mode === "baker";
  const waiting = (bakerSide: boolean) =>
    snap.conversations.some((c) => onBakerSide(layout, c) === bakerSide && c.status === "needs_input");
  const edge = (
    <div
      aria-hidden
      className="absolute inset-y-0 -right-1 z-10 w-2 cursor-col-resize"
      onPointerDown={(e) => dragEdge(e, el, (next) => setLayout((l) => ({ ...l, sidebar: { ...l.sidebar, ...next } })))}
    />
  );

  if (layout.sidebar.rail) {
    return (
      <aside
        className="relative flex shrink-0 flex-col items-center gap-1.5 border-(--line-chrome) border-r bg-(--bg-chrome) pt-2.5"
        data-testid="shell-rail"
        ref={el}
        style={{ width: RAIL }}
      >
        <Rail p={p} />
        {edge}
      </aside>
    );
  }

  return (
    <aside
      className="relative flex shrink-0 flex-col border-(--line-chrome) border-r bg-(--bg-chrome)"
      data-testid="shell-sidebar"
      ref={el}
      style={{ width: layout.sidebar.width }}
    >
      <div className="flex flex-col gap-2 border-(--line-chrome) border-b bg-(image:--grad-chrome) p-2.5">
        <div aria-label="Mode" className="flex overflow-hidden rounded-[7px] border border-(--line)" role="group">
          {MODES.map((m, i) => {
            const on = layout.mode === m.id;
            return (
              <button
                aria-pressed={on}
                className={`flex h-7 flex-1 items-center justify-center gap-[7px] text-xs ${i > 0 ? "border-(--line) border-l" : ""} ${on ? "bg-(image:--grad-accent) font-bold text-(--fg-on-solid)" : "bg-(--bg-chrome) text-(--fg-dim) hover:text-(--fg-2)"}`}
                data-testid={`shell-mode-${m.id}`}
                key={m.id}
                onClick={() => p.onMode(m.id)}
                title={`${m.label} (${m.id === "baker" ? "⌘2" : "⌘1"})`}
                type="button"
              >
                <span style={on ? undefined : { color: m.hue }}>{m.icon}</span>
                {m.label}
                {!on && waiting(m.id === "baker") && <span className="size-1.5 rounded-full bg-(--warn)" title="Needs you" />}
              </button>
            );
          })}
        </div>
        <NodeBox
          onEngine={() => {
            const ws = L.shown(layout)?.id;
            if (ws) p.openIn(ws, "@engine");
          }}
          onSettings={() => openSettings("machines")}
        />
      </div>

      <nav aria-label={baker ? "Baker" : "Bakery"} className="flex min-h-0 flex-1 flex-col overflow-y-auto">
        {baker ? <BakerSide p={p} /> : <BakerySide p={p} />}
      </nav>

      {/* No footer band: only the gear, at the bottom left. */}
      <Gear />

      {edge}
    </aside>
  );
}
