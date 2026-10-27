// The new shell: jcode desktop's model in React. The sidebar (the operator's
// control panel, `Sidebar.tsx`) and a horizontal strip of panel columns for
// the workspace on screen: a Bakery one, or in Baker mode the Baker's own.
// The layout is data (`layout.ts`), kept on the node so the Baker can change
// it, with local storage as this window's cache.
//
// Every panel is a real conversation through the same agent panel source the
// studio uses (`shellSource.svelte.ts`), or a tool panel.

import {
  Activity,
  memo,
  type ReactNode,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  useSyncExternalStore,
  type DragEvent,
} from "react";
import {
  ArrowUpFromLineIcon,
  BotIcon,
  BoxIcon,
  ChefHatIcon,
  CircleCheckIcon,
  CodeIcon,
  GitMergeIcon,
  GitPullRequestIcon,
  GlobeIcon,
  GripVerticalIcon,
  LayoutDashboardIcon,
  LayoutGridIcon,
  LockIcon,
  BrainIcon,
  ListTodoIcon,
  MessageSquareIcon,
  ServerIcon,
  TriangleAlertIcon,
  XIcon,
} from "lucide-react";
import {
  conversationArchive,
  type DeleteTarget,
  filesRead,
  gitMergePreview,
  type Found2 as Found,
  gitFetch,
  gitUpdate,
  node as nodeInfo,
  projectOpen,
  uiLayout,
  uiLayoutSet,
} from "$lib/api.generated";
import { onEnvelope } from "$lib/events";
import { type Conversation, statusLabel } from "$lib/conversations";
import { electronViews } from "$lib/desktop";
import { AgentPanel, type FileChange, ThreadNamesContext } from "../AgentPanel";
import { useAgentMapNodes } from "../AgentMap";
import { holdable } from "../liveSnapshot";
import { CodeView, revealInCode } from "./CodeView";
import { inside } from "./codeModel";
import { ContainersPanel } from "./ContainersPanel";
import { ControlPlane } from "./ControlPlane";
import { DeleteDialog } from "./DeleteDialog";
import { SavedPanel, SaveReply } from "./SavedPanel";
import { useSaved } from "./saved";
import { ThreadsPage } from "./ThreadsPage";
import { DeployPills } from "./DeployPills";
import { ResourcePill } from "./ResourcePill";
import { RemoteHistory, RestartPill, useMergeWaitingIds, WaitingPill } from "./Waiting";
import { Floor } from "./Floor";
import { agentOf, agentTree } from "./floorModel";
import * as L from "./layout";
import { MergeReview, reviewBranch } from "./MergeReview";
import { ActionsPanel, PrInbox } from "./GitHubPanels";
import { useDigestNotices, useFileAskNotices, useHostAskNotices } from "./hostNotices";
import { BacklogBoard } from "./Backlog";
import { MemoryWiki } from "./MemoryWiki";
import { BriefBar, useBrief } from "./Brief";
import { EngineRoom } from "./EngineRoom";
import { InternetSheet, projectOfCheckout } from "./Internet";
import { baseOf } from "./reviewModel";
import type { AttachedConversation, ShellSnapshot, ShellStore } from "./shellSource.svelte";
import { AgentTerminal, showCommand } from "./AgentTerminal";
import { BottomPanel, showTerminals } from "./BottomPanel";
import { terminalPlace } from "./terminalPlace";
import type { CommandAt } from "$lib/terminalSession";
import { type BarCommand, type BarFiles, type BarItem, CommandBar } from "./CommandBar";
import { FinishSlot, type PaneActions, PaneBranch, PaneContext, PaneHeaderActions, StartedPill } from "./PaneHeader";
import { type ThreadActions, ThreadMenuContext } from "./ThreadMenu";
import { type NewAgent, PanelMenu } from "./PanelMenu";
import { useSeedLivePhases } from "./livePhases";
import { peekRollup, reloadRollup, useMergeStates } from "./rollup";
import { mergedFiles } from "./mergeFiles";
import { agentPlace } from "./sidebarModel";
import { type PanelLabel, Screen, TopBar } from "./Screen";
import { Sidebar } from "./Sidebar";
import { PANEL_MIME, said, type SetLayout, StatusDot, startResize, type ThreadStates, ThreadStatesContext, type ToolContext, workspaceColor } from "./ui";
import { shortcutInit } from "./viewModel";
import { closeViewsExcept, WebPanel } from "./WebPanel";
import { WorkspaceMap } from "./WorkspaceMap";
import { revealSubagent } from "../subagentReveal";

const LAYOUT_KEY = "bakery.shell.layout.v1";
/** How many recently shown workspaces stay mounted, hidden, for an instant switch. */
const WARM_WORKSPACES = 8;
type Tool = {
  width: number;
  icon: ReactNode;
  title(snap: ShellSnapshot, arg: string): string;
  render(ctx: ToolContext, arg: string): ReactNode;
};
const titleOf = (snap: ShellSnapshot, streamId: string) =>
  snap.conversations.find((c) => c.streamId === streamId)?.title ?? "gone";
/** Panels that are not conversations. Their layout ids start with "@", as
 *  no stream id does: "@review", or "@code:<stream id>" for one worktree's. */
const TOOLS: Record<string, Tool> = {
  control: {
    width: 900,
    icon: <LayoutDashboardIcon className="size-3 text-(--hue-project)" />,
    title: () => "Control plane",
    render: (ctx) => <ControlPlane ctx={ctx} />,
  },
  review: {
    width: 900,
    icon: <GitMergeIcon className="size-3" />,
    title: () => "Merge review",
    render: (ctx) => <MergeReview ctx={ctx} />,
  },
  prs: {
    width: 900,
    icon: <GitPullRequestIcon className="size-3 text-(--hue-github)" />,
    title: () => "Pull requests",
    render: (ctx) => <PrInbox ctx={ctx} />,
  },
  actions: {
    width: 760,
    icon: <CircleCheckIcon className="size-3 text-(--hue-github)" />,
    title: () => "Actions",
    render: (ctx) => <ActionsPanel ctx={ctx} />,
  },
  // A project's whole internet list, from its control plane card's All ›,
  // on the project's own checkout (so its terminal opens in the repository).
  internet: {
    width: 760,
    icon: <GlobeIcon className="size-3 text-(--link)" />,
    title: (snap, sid) => `Internet · ${projectOfCheckout(snap, sid)?.name ?? "gone"}`,
    render: (ctx, sid) => <InternetSheet ctx={ctx} streamId={sid} />,
  },
  // Everything that left the Bakery, newest first: the waiting list's History.
  remote: {
    width: 900,
    icon: <ArrowUpFromLineIcon className="size-3 text-(--accent)" />,
    title: () => "Remote actions",
    render: (ctx) => <RemoteHistory ctx={ctx} />,
  },
  // Every thread on this side of the Bakery: the rail's Threads (Round 10).
  threads: {
    width: 760,
    icon: <MessageSquareIcon className="size-3 text-(--hue-threads)" />,
    title: () => "Threads",
    render: (ctx) => <ThreadsPage ctx={ctx} />,
  },
  containers: {
    width: 760,
    icon: <BoxIcon className="size-3" />,
    title: () => "Containers",
    render: (ctx) => <ContainersPanel ctx={ctx} />,
  },
  // The memory wiki (Round 13's M1): the Baker sidebar's Memory row.
  memory: {
    width: 1040,
    icon: <BrainIcon className="size-3 text-(--warn)" />,
    title: () => "Memory",
    render: (ctx) => <MemoryWiki ctx={ctx} />,
  },
  // The backlog board (Round 12's B): the Baker sidebar's Backlog rows.
  backlog: {
    width: 960,
    icon: <ListTodoIcon className="size-3 text-(--warn)" />,
    title: () => "Backlog",
    render: (ctx) => <BacklogBoard ctx={ctx} />,
  },
  // Machines (the operator, 10-12): the engine room (Round 8's V1), its
  // engines and this Mac's resources, from the rail, the sidebar's Machines
  // and the resource pill. The other machines and their monitors join it.
  engine: {
    width: 960,
    icon: <ServerIcon className="size-3 text-(--link)" />,
    title: () => "Machines",
    render: (ctx) => <EngineRoom ctx={ctx} />,
  },
  code: {
    width: 900,
    icon: <CodeIcon className="size-3" />,
    title: (snap, sid) => `Code · ${titleOf(snap, sid)}`,
    render: (ctx, sid) => <CodeView ctx={ctx} streamId={sid} />,
  },
  // code-server, Bakery-styled: Code view's "Open in", in a native view.
  editor: {
    width: 900,
    icon: <CodeIcon className="size-3 text-(--hue-containers)" />,
    title: (snap, sid) => `Code editor · ${titleOf(snap, sid)}`,
    render: (ctx, sid) => <WebPanel ctx={ctx} kind="code" streamId={sid} />,
  },
  preview: {
    width: 700,
    icon: <GlobeIcon className="size-3" />,
    title: (snap, sid) => `Preview · ${titleOf(snap, sid)}`,
    render: (ctx, sid) => <WebPanel ctx={ctx} kind="preview" streamId={sid} />,
  },
  // A thread's Agent terminal, read-only, beside the thread: from a command's row.
  agent: {
    width: 640,
    icon: <BotIcon className="size-3 text-(--violet)" />,
    title: (snap, sid) => `Agent · ${titleOf(snap, sid)}`,
    render: (_ctx, sid) => <AgentTerminal streamId={sid} />,
  },
};

function toolOf(id: string): { tool: Tool; arg: string } | null {
  if (!id.startsWith("@")) return null;
  const at = id.indexOf(":");
  const tool = TOOLS[at < 0 ? id.slice(1) : id.slice(1, at)];
  return tool ? { tool, arg: at < 0 ? "" : id.slice(at + 1) } : null;
}

/** Shows or hides the bottom (terminal) panel. */
const toggleBottom = (l: L.Layout): L.Layout => ({
  ...l,
  bottom: { open: !l.bottom?.open, height: l.bottom?.height ?? 240 },
});

/** A native view's panel, and back: code-server's view "code:<sid>" sits in
 *  the panel "@editor:<sid>", a preview's "preview:<sid>" in "@preview:<sid>". */
const panelOfView = (viewId: string) => `@${viewId.startsWith("code:") ? `editor:${viewId.slice(5)}` : viewId}`;
const viewOfPanel = (id: string) =>
  id.startsWith("@editor:") ? `code:${id.slice(8)}` : id.startsWith("@preview:") ? id.slice(1) : null;

/** Opens a panel by its layout id beside the focused one, at its tool's
 *  width when it is a tool. */
const openIn = (wsId: string, id: string) => (l: L.Layout) => L.openPanel(l, wsId, id, toolOf(id)?.tool.width);
function readSaved(): string | null {
  try {
    return localStorage.getItem(LAYOUT_KEY);
  } catch {
    return null;
  }
}

function save(l: L.Layout) {
  try {
    localStorage.setItem(LAYOUT_KEY, JSON.stringify(l));
  } catch {
    // Storage full or blocked: the layout just isn't kept across launches.
  }
}

const smooth = (): ScrollBehavior =>
  matchMedia("(prefers-reduced-motion: reduce)").matches ? "auto" : "smooth";

/** One live thread. Memoised on its props, so the shell's own re-renders
 *  (topology updates, drags) never re-render a transcript. Its source is
 *  released, not cleared, when it hides: the store keeps it warm, so showing
 *  it again hands back the same source with nothing to load. */

/** Opens an edit row's file in its thread's Code at the changed line. Set
 *  by the shell (one per window), so the memoized panes reach it without
 *  re-rendering whenever the shell does. */
let openFileHandler: (wsId: string, streamId: string, change: FileChange) => void = () => {};

/** Brings the terminal panel forward on a thread's terminals; with none
 *  named, a command's row: the thread's Agent tab, on that command. Set by
 *  the shell, like `openFileHandler`. */
let showTerminalsHandler: (wsId: string, streamId: string, terminalIds: string[], at?: CommandAt) => void = () => {};

/** A thread's name and side, for a reply saved there (the Saved panel's All
 *  threads). Set by the shell, like `openFileHandler`. */
let threadInfoHandler: (streamId: string) => { title: string; bakerThread: boolean } | null = () => null;

/** Threads whose repository this run already fetched as they opened (git rule 2). */
const fetchedOnce = new Set<string>();

const ConversationBody = memo(function ConversationBody({
  store,
  streamId,
  attachable,
  making,
  wsId,
  held = false,
}: {
  store: ShellStore;
  streamId: string;
  attachable: boolean;
  /** A new workspace's orc whose thread the node has not listed yet. */
  making: boolean;
  /** The workspace it shows in, for its header's lock and new thread. */
  wsId: string;
  /** Scrolled well out of sight in the strip: it holds still (`holdable`). */
  held?: boolean;
}) {
  const [attached, setAttached] = useState<AttachedConversation | null>(null);
  const source = useMemo(() => (attached ? holdable(attached.source) : null), [attached]);
  useEffect(() => source?.hold(held), [source, held]);
  // Saved (Round 10): the operator's own, never an agent's; out of sight
  // until the node has its routes.
  const saved = useSaved();
  const [savedOpen, setSavedOpen] = useState(false);
  const savedHere = saved.items.filter((i) => i.stream_id === streamId).length;
  // The brief (Round 11): shown while there is one, or once asked for.
  const brief = useBrief(streamId);
  const [briefShown, setBriefShown] = useState<boolean | null>(null);
  const showBrief = brief.available && (briefShown ?? brief.has);
  const replyActions = useCallback((hash: string) => <SaveReply hash={hash} streamId={streamId} />, [streamId]);
  useEffect(() => {
    if (!attachable) return;
    const a = store.attach(streamId);
    setAttached(a);
    return () => a?.dispose();
  }, [store, streamId, attachable]);
  const openFile = useCallback((change: FileChange) => openFileHandler(wsId, streamId, change), [wsId, streamId]);
  const terminals = useCallback((ids: string[], at?: CommandAt) => showTerminalsHandler(wsId, streamId, ids, at), [wsId, streamId]);
  if (!attachable || !source) {
    return (
      <div className="grid h-full place-items-center p-4 text-(--fg-faint) text-xs">{attachable || making ? "Opening…" : "Gone"}</div>
    );
  }
  return (
    <AgentPanel
      afterTitle={<PaneBranch streamId={streamId} />}
      agents={<StartedPill streamId={streamId} wsId={wsId} />}
      finish={<FinishSlot streamId={streamId} wsId={wsId} />}
      headerActions={
        <PaneHeaderActions
          brief={brief.available ? { open: showBrief, has: brief.has, toggle: () => setBriefShown(!showBrief) } : undefined}
          saved={saved.available ? { open: savedOpen, count: savedHere, toggle: () => setSavedOpen((o) => !o) } : undefined}
          streamId={streamId}
          wsId={wsId}
        />
      }
      pinned={showBrief ? <BriefBar b={brief} onClose={() => setBriefShown(false)} streamId={streamId} /> : undefined}
      openFile={openFile}
      replyActions={saved.available ? replyActions : undefined}
      showTerminals={terminals}
      side={
        saved.available && savedOpen ? (
          <SavedPanel onClose={() => setSavedOpen(false)} streamId={streamId} threadOf={(id) => threadInfoHandler(id)} />
        ) : undefined
      }
      store={source}
    />
  );
});

function Strip({
  store,
  snap,
  layout,
  workspace,
  setLayout,
  color,
  place,
  listed,
}: {
  store: ShellStore;
  snap: ShellSnapshot;
  layout: L.Layout;
  workspace: L.Workspace;
  setLayout: SetLayout;
  color: string;
  /** The workspace a dropped thread belongs in, when not this one. */
  place: (l: L.Layout, id: string) => string | null;
  /** The threads the node's last list named (L.orcBeingMade). */
  listed: ReadonlySet<string> | null;
}) {
  const panels = useRef(new Map<string, HTMLElement>());
  const columns = useRef(new Map<string, HTMLElement>());
  const [drop, setDrop] = useState<{ target: string; where: L.Where } | null>(null);
  const byId = useMemo(() => new Map(snap.conversations.map((c) => [c.streamId, c])), [snap.conversations]);
  const attachable = useMemo(() => new Set(snap.attachable), [snap.attachable]);
  const wsId = workspace.id;
  const strip = useRef<HTMLDivElement>(null);

  // A thread more than a strip's width out of sight holds still; it is let
  // go well before it scrolls in, and the focused one never holds (the
  // camera slides it in).
  const [away, setAway] = useState<ReadonlySet<string>>(() => new Set());
  const ids = workspace.columns.flatMap((c) => c.panels).join(" ");
  useEffect(() => {
    const root = strip.current;
    if (!root || typeof IntersectionObserver === "undefined") return;
    const idOf = new Map([...panels.current].map(([id, el]) => [el, id]));
    const watch = new IntersectionObserver(
      (entries) =>
        setAway((was) => {
          const next = new Set(was);
          for (const e of entries) {
            const id = idOf.get(e.target as HTMLElement);
            if (id === undefined) continue;
            if (e.isIntersecting) next.delete(id);
            else next.add(id);
          }
          return next.size === was.size && [...next].every((id) => was.has(id)) ? was : next;
        }),
      { root, rootMargin: "0px 100% 0px 100%" },
    );
    for (const el of idOf.keys()) watch.observe(el);
    return () => watch.disconnect();
  }, [ids]);

  // The camera: the focused panel slides into view.
  useEffect(() => {
    if (!workspace.focus) return;
    panels.current.get(workspace.focus)?.scrollIntoView({ behavior: smooth(), block: "nearest", inline: "nearest" });
  }, [workspace.focus, wsId]);

  useEffect(() => {
    const clear = () => setDrop(null);
    document.addEventListener("dragend", clear);
    return () => document.removeEventListener("dragend", clear);
  }, []);

  function zone(e: DragEvent<HTMLElement>): L.Where {
    const r = e.currentTarget.getBoundingClientRect();
    const x = (e.clientX - r.left) / r.width;
    return x < 0.3 ? "before" : x > 0.7 ? "after" : "below";
  }

  function onDrop(e: DragEvent<HTMLElement>, target: string | null) {
    const id = e.dataTransfer.getData(PANEL_MIME);
    setDrop(null);
    if (!id) return;
    e.preventDefault();
    const where = target ? zone(e) : null;
    setLayout((l) => {
      // A thread from another project opens in its own workspace instead.
      const home = place(l, id);
      if (home && home !== wsId) return L.openPanel(L.setActive(l, home), home, id);
      const opened = L.openPanel(l, wsId, id);
      return target && where ? L.movePanel(opened, wsId, id, target, where) : opened;
    });
  }

  if (workspace.columns.length === 0) {
    return (
      <div
        aria-label={workspace.name}
        className="flex-1"
        data-testid="shell-strip-empty"
        onDragOver={(e) => e.dataTransfer.types.includes(PANEL_MIME) && e.preventDefault()}
        onDrop={(e) => onDrop(e, null)}
        role="region"
      />
    );
  }

  return (
    <div
      className="flex min-h-0 flex-1 gap-2 overflow-x-auto p-2 [justify-content:safe_center] [scrollbar-width:thin]"
      data-shell-clip
      data-testid="shell-strip"
      ref={strip}
    >
      {workspace.columns.map((col) => (
        <div
          className="relative flex shrink-0 flex-col gap-2"
          key={col.id}
          ref={(node) => {
            if (node) columns.current.set(col.id, node);
            else columns.current.delete(col.id);
          }}
          // Never wider than the strip itself, so a wide panel stays readable
          // on a small window.
          style={{ width: col.width, maxWidth: "100%" }}
        >
          {col.panels.map((id) => {
            const conv = byId.get(id);
            const found = toolOf(id);
            const making = found || conv ? null : L.orcBeingMade(layout, id, listed);
            const title = found ? found.tool.title(snap, found.arg) : (conv?.title ?? making?.name ?? "Gone");
            const status = conv?.status ?? (making ? "working" : null);
            const focused = workspace.focus === id;
            const hint = drop?.target === id ? drop.where : null;
            return (
              <section
                aria-label={title}
                className={`flex min-h-0 flex-1 flex-col overflow-hidden rounded-xl border bg-(--bg-panel) ${focused ? "border-(--accent-border)" : "border-(--line-soft)"}`}
                data-testid="shell-panel"
                key={id}
                onDragLeave={(e) => {
                  if (!e.currentTarget.contains(e.relatedTarget as Node | null)) setDrop(null);
                }}
                onDragOver={(e) => {
                  if (!e.dataTransfer.types.includes(PANEL_MIME)) return;
                  e.preventDefault();
                  const where = zone(e);
                  setDrop((d) => (d?.target === id && d.where === where ? d : { target: id, where }));
                }}
                onDrop={(e) => onDrop(e, id)}
                onPointerDownCapture={() => setLayout((l) => L.focusPanel(l, wsId, id))}
                ref={(node) => {
                  if (node) panels.current.set(id, node);
                  else panels.current.delete(id);
                }}
                style={{
                  boxShadow:
                    hint === "before"
                      ? "inset 3px 0 0 var(--accent)"
                      : hint === "after"
                        ? "inset -3px 0 0 var(--accent)"
                        : hint === "below"
                          ? "inset 0 -3px 0 var(--accent)"
                          : undefined,
                }}
              >
                <header
                  className="flex h-7 shrink-0 cursor-grab items-center gap-1.5 border-(--line-soft) border-b bg-(--bg-raised) px-2 text-xs active:cursor-grabbing"
                  draggable
                  onDragStart={(e) => {
                    e.dataTransfer.setData(PANEL_MIME, id);
                    e.dataTransfer.effectAllowed = "move";
                  }}
                >
                  <GripVerticalIcon className="size-3 shrink-0 text-(--fg-ghost)" />
                  {!found && status && <StatusDot status={status} streamId={id} title={statusLabel(status)} />}
                  {found?.tool.icon}
                  <span className={`truncate ${focused ? "text-(--fg)" : "text-(--fg-dim)"}`}>{title}</span>
                  {!found && conv?.place && conv.place !== conv.title && (
                    <span className="truncate text-(--fg-ghost) text-[10px]">{conv.place}</span>
                  )}
                  <span className="flex-1" />
                  {!found && conv && (
                    <>
                      <button
                        aria-label={`Open ${conv.title} in Code`}
                        className="text-(--hue-containers) hover:text-(--fg-2)"
                        onClick={() => setLayout(openIn(wsId, `@code:${id}`))}
                        title="Code"
                        type="button"
                      >
                        <CodeIcon className="size-3" />
                      </button>
                      <button
                        aria-label={`Preview ${conv.title}`}
                        className="text-(--violet) hover:text-(--fg-2)"
                        onClick={() => setLayout(openIn(wsId, `@preview:${id}`))}
                        title="Preview"
                        type="button"
                      >
                        <GlobeIcon className="size-3" />
                      </button>
                    </>
                  )}
                  {L.isLocked(workspace, id) ? (
                    <LockIcon aria-label="Locked" className="size-3 text-(--fg-faint)" />
                  ) : (
                    <button
                      aria-label="Close panel"
                      className="text-(--fg-faint) hover:text-(--fg-2)"
                      onClick={() => setLayout((l) => L.closePanel(l, wsId, id))}
                      title="Close"
                      type="button"
                    >
                      <XIcon className="size-3" />
                    </button>
                  )}
                </header>
                <div className="min-h-0 flex-1">
                  {found ? (
                    found.tool.render({ store, snap, layout, workspace, setLayout, color }, found.arg)
                  ) : (
                    <ConversationBody
                      attachable={attachable.has(id)}
                      held={!focused && away.has(id)}
                      making={!!making}
                      store={store}
                      streamId={id}
                      wsId={wsId}
                    />
                  )}
                </div>
              </section>
            );
          })}
          <div
            aria-hidden
            className="absolute inset-y-0 -right-1.5 w-2 cursor-col-resize"
            onPointerDown={(e) =>
              startResize(e, columns.current.get(col.id) ?? null, 1, [L.COLUMN_MIN, L.COLUMN_MAX], (width) =>
                setLayout((l) => L.resizeColumn(l, wsId, col.id, width)),
              )
            }
          />
        </div>
      ))}
    </div>
  );
}

function Minimap({ snap, workspace, setLayout }: { snap: ShellSnapshot; workspace: L.Workspace; setLayout: SetLayout }) {
  const status = useMemo(() => new Map(snap.conversations.map((c) => [c.streamId, c.status])), [snap.conversations]);
  if (workspace.columns.length < 2) return null;
  return (
    <div className="flex h-5 shrink-0 items-center justify-center gap-1" data-testid="shell-minimap">
      {workspace.columns.map((col) => {
        const focused = col.panels.includes(workspace.focus ?? "");
        const live = col.panels.some((p) => status.get(p) === "working" || status.get(p) === "needs_input");
        return (
          <button
            aria-label="Focus this column"
            className="h-2 rounded-sm"
            key={col.id}
            onClick={() => setLayout((l) => L.focusPanel(l, workspace.id, col.panels[0]))}
            style={{
              width: Math.max(8, col.width / 24),
              background: focused ? "var(--accent)" : live ? "var(--fg-ghost)" : "var(--bg-chip)",
            }}
            type="button"
          />
        );
      })}
    </div>
  );
}

type KeptProps = {
  shown: boolean;
  render: () => ReactNode;
  pane: PaneActions;
  names: (streamId: string) => string | undefined;
  states: ThreadStates;
  menu: ThreadActions;
};

/** A warm workspace. Hidden, it keeps its last render: the shell's redraws
 *  (a topology answer, an agent map reload) skip it, and it renders current
 *  the moment it shows. The shell's context values are held with it, so a
 *  change to those skips it too. */
const KeptWorkspace = memo(
  function KeptWorkspace({ render, pane, names, states, menu }: KeptProps) {
    return (
      <PaneContext.Provider value={pane}>
        <ThreadNamesContext.Provider value={names}>
          <ThreadStatesContext.Provider value={states}>
            <ThreadMenuContext.Provider value={menu}>{render()}</ThreadMenuContext.Provider>
          </ThreadStatesContext.Provider>
        </ThreadNamesContext.Provider>
      </PaneContext.Provider>
    );
  },
  (_, next) => !next.shown,
);

export function Shell({ store }: { store: ShellStore }) {
  const snap = useSyncExternalStore(store.subscribe, store.getSnapshot, store.getSnapshot);
  const [layout, setLayout] = useState(() => L.parseLayout(readSaved()));
  const { nodes } = useAgentMapNodes();
  /** The factory floor (zoomed out) instead of the strip. Never saved:
   *  the app always opens on the work itself. */
  const [floor, setFloor] = useState(false);
  /** The search and command bar (⌘K, or the top bar's middle). */
  /** The ⌘K bar: everything, or only the text in files (⌘⇧F). */
  const [bar, setBar] = useState<"all" | "files" | null>(null);
  /** The agent map, opened from a thread's "● N agents" in a workspace. */
  const [mapAt, setMapAt] = useState<{ wsId: string; streamId: string } | null>(null);
  /** A short note over the tab bars (where Code views never cover it). */
  const [notice, setNotice] = useState<{ bad: boolean; text: string } | null>(null);
  /** The delete whose plan is open (any row's menu). */
  const [deleting, setDeleting] = useState<DeleteTarget | null>(null);
  useEffect(() => {
    if (!notice) return;
    const timer = setTimeout(() => setNotice(null), 6000);
    return () => clearTimeout(timer);
  }, [notice]);
  const main = useRef<HTMLElement>(null);
  const latest = useRef(snap);
  latest.current = snap;

  /** Bakery or Baker mode. The Baker's workspace with nothing open shows its
   *  home page. */
  const switchMode = (mode: L.Layout["mode"] | "toggle") => {
    setFloor(false);
    setLayout((l) => L.setMode(l, mode === "toggle" ? (l.mode === "baker" ? "bakery" : "baker") : mode));
  };
  /** The id of the workspace on screen. */
  const shownId = (l: L.Layout) => L.shown(l)?.id ?? null;
  /** The workspace a thread (or a tool on its worktree) belongs in. */
  const homeFor = (l: L.Layout, id: string): string | null => {
    const found = toolOf(id);
    const streamId = found ? found.arg : id;
    if (!streamId) return null;
    const s = latest.current;
    const c = s.conversations.find((x) => x.streamId === streamId);
    // An orc and its workers live in the orc's workspace on the Baker's side.
    const orc = L.orcHome(l, c?.workspace ? c.streamId : (c?.openedBy ?? null));
    if (orc) return orc;
    const project = s.projects.find((p) => p.streamIds.includes(streamId))?.id ?? null;
    return L.homeOf(l, project, c?.bakerThread ?? false);
  };

  // The checkout of the Bakery's own source, as the node names it: its
  // project goes to the Baker's side the first time it appears. Null until
  // the node answers, so no project is placed before that.
  const [sourceRoot, setSourceRoot] = useState<string | null>(null);
  useEffect(() => {
    nodeInfo().then(
      (info) => setSourceRoot(info.bakery_source_root ?? ""),
      () => setSourceRoot(""),
    );
  }, []);
  const own =
    (sourceRoot && snap.projects.find((p) => p.checkouts.some((c) => c.repoRoot === sourceRoot || c.path === sourceRoot))?.id) ||
    null;
  /** Bumped each time a saved layout lands (from the node, or another window). */
  const [landed, setLanded] = useState(0);
  // What a gone project had goes with it, once the node itself says so (a
  // saved layout can name one a fresh start or a delete took).
  useEffect(() => {
    if (snap.loaded && sourceRoot !== null)
      setLayout((l) => L.syncProjects(snap.fresh ? L.dropGone(l, snap.projects) : l, snap.projects, own));
  }, [snap.loaded, snap.fresh, snap.projects, sourceRoot, own, landed]);
  // A thread that isn't live any more (deleted by the Baker's tool or from
  // another window, or archived: a merge archives its chef) takes its tabs
  // with it, a locked one too: an archived thread's tab only reads "Gone".
  // Gone means it was in the last list and isn't in this one; on the first,
  // any tab a saved layout kept for a thread already gone. A new thread's
  // tab opens before the list names it, and is never taken for gone.
  const listed = useRef<Set<string> | null>(null);
  useEffect(() => {
    if (!snap.loaded || !snap.fresh) return;
    const known = new Set(snap.conversations.map((c) => c.streamId));
    const before = listed.current;
    listed.current = known;
    setLayout((l) => L.closeGone(l, (id) => !known.has(id) && (before === null || before.has(id))));
  }, [snap.loaded, snap.fresh, snap.conversations, snap.archived, landed]);
  // Every orc gets its workspace, named as its workspace: the Bakery's own
  // (and a Baker-side project's) on the Baker's side, a project's in Bakery
  // mode under its project.
  const orcList = snap.conversations
    .filter((c) => c.workspace)
    .map((c) => {
      const project = snap.projects.find((p) => p.streamIds.includes(c.streamId))?.id ?? "";
      return `${c.streamId}\t${c.title}\t${layout.bakerProjects.includes(project) ? "" : project}`;
    })
    .join("\n");
  useEffect(() => {
    if (!snap.loaded) return;
    const orcs = orcList
      ? orcList.split("\n").map((row) => {
          const [streamId, title, projectId] = row.split("\t");
          return { streamId, title: title ?? "", projectId: projectId || null };
        })
      : [];
    setLayout((l) => L.syncOrcs(l, orcs));
    // Again once a saved layout lands later, so it can't strand an orc. Not
    // on every change of the layout's orcs: a just-made one (showNewOrc)
    // would be pruned before the node lists it.
  }, [snap.loaded, orcList, landed]);
  // The node keeps the layout, so the Baker can change it and a new window
  // or app origin starts from it; localStorage is only this window's cache.
  // `sent` is what the node has (canonical JSON), null until it was read:
  // nothing goes back before that, and nothing echoes back.
  const sent = useRef<string | null>(null);
  useEffect(() => {
    let alive = true;
    uiLayout().then(
      (saved) => {
        if (!alive) return;
        if (saved) {
          const l = L.parseLayout(JSON.stringify(saved));
          sent.current = L.canon(l);
          setLayout(l);
          setLanded((n) => n + 1);
        } else {
          sent.current = "";
          setLayout((l) => ({ ...l })); // seed the node with this window's layout
        }
      },
      () => {
        if (alive) sent.current = ""; // node unreachable: keep working here
      },
    );
    const off = onEnvelope("ui-layout", (envelope) => {
      if (!envelope.layout) return;
      const l = L.parseLayout(JSON.stringify(envelope.layout));
      const key = L.canon(l);
      if (key === sent.current) return;
      sent.current = key;
      setLayout(l);
      setLanded((n) => n + 1);
    });
    return () => {
      alive = false;
      off();
    };
  }, []);
  useEffect(() => {
    save(layout);
    if (sent.current === null) return;
    const key = L.canon(layout);
    if (key === sent.current) return;
    const t = setTimeout(() => {
      sent.current = key;
      void uiLayoutSet({ layout }).catch(() => {});
    }, 500);
    return () => clearTimeout(t);
  }, [layout]);

  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") setFloor(false);
      if ((e.metaKey || e.ctrlKey) && e.shiftKey && e.key.toLowerCase() === "p") {
        e.preventDefault();
        setBar("all");
        return;
      }
      if ((e.metaKey || e.ctrlKey) && e.shiftKey && e.key.toLowerCase() === "f") {
        e.preventDefault();
        setBar("files");
        return;
      }
      if (!(e.metaKey || e.ctrlKey) || e.shiftKey) return;
      let handled = true;
      if (!e.altKey && e.key.toLowerCase() === "k") {
        setBar("all");
      } else if (e.metaKey && !e.altKey && (e.key === "1" || e.key === "2")) {
        // ⌘1 the Bakery, ⌘2 the Baker.
        switchMode(e.key === "1" ? "bakery" : "baker");
      } else if (e.ctrlKey && !e.metaKey && !e.altKey && /^[1-9]$/.test(e.key)) {
        // ⌃1–⌃9 a Bakery workspace (⌘1/⌘2 are the modes).
        const n = Number(e.key) - 1;
        setLayout((l) => (l.workspaces[n] ? L.setActive(l, l.workspaces[n].id) : l));
        setFloor(false);
      } else if (!e.altKey && e.key === "0") {
        setFloor((v) => !v);
      } else if (e.altKey && (e.key === "ArrowLeft" || e.key === "ArrowRight")) {
        const dir = e.key === "ArrowLeft" ? -1 : 1;
        setLayout((l) => {
          const id = shownId(l);
          return id ? L.focusNeighbour(l, id, dir) : l;
        });
      } else if (!e.altKey && e.key.toLowerCase() === "j") {
        // ⌘J is the terminal panel everywhere, as in VS Code.
        setLayout(toggleBottom);
      } else if (e.ctrlKey && !e.metaKey && e.key === "`") {
        setLayout(toggleBottom);
      } else if (!e.altKey && e.key === "\\") {
        setLayout((l) => ({ ...l, sidebar: { ...l.sidebar, open: !l.sidebar.open } }));
      } else if (!e.altKey && e.key === ",") {
        // ⌘, All settings, as on every Mac app; the gear opens quick settings.
        window.dispatchEvent(new CustomEvent("bakery:open-settings", { detail: "agents" }));
      } else handled = false;
      if (handled) e.preventDefault();
    }
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, []);

  /** Opens a thread in its own workspace, which it then shows: a Baker
   *  thread in the Baker's, a project's thread in its project's. A thread
   *  with no project opens in the workspace on screen. */
  const openThread = (streamId: string, bakerThread: boolean) => {
    setFloor(false);
    setLayout((l) => {
      const ws = homeFor(l, streamId) ?? (bakerThread ? L.BAKER : shownId(l));
      return ws ? L.openPanel(L.setActive(l, ws), ws, streamId) : l;
    });
  };
  // A host a sandbox asked for, on the Mac's notification while the app is
  // behind; its body opens the thread that asked.
  useHostAskNotices(snap, (streamId) =>
    openThread(streamId, snap.conversations.find((c) => c.streamId === streamId)?.bakerThread ?? false),
  );
  // Likewise a secret-named file a build was refused: it opens the card.
  useFileAskNotices(snap, (streamId) =>
    openThread(streamId, snap.conversations.find((c) => c.streamId === streamId)?.bakerThread ?? false),
  );
  // The learning loop's nightly digest: it opens the wiki's To keep.
  useDigestNotices(snap);
  // The old screens' palette jumps here the same way.
  useEffect(() => {
    const onOpen = (e: Event) => {
      const { streamId, bakerThread } = (e as CustomEvent<{ streamId: string; bakerThread?: boolean }>).detail;
      openThread(streamId, bakerThread ?? latest.current.conversations.some((c) => c.streamId === streamId && c.bakerThread));
    };
    // A thread's branch in the Merge review, where the operator is (a
    // ready-to-merge card's Review).
    const onReview = (e: Event) => {
      const { streamId } = (e as CustomEvent<{ streamId: string }>).detail;
      setFloor(false);
      reviewBranch(streamId);
      setLayout((l) => {
        const ws = shownId(l);
        return ws ? openIn(ws, "@review")(l) : l;
      });
    };
    // A tool page where the operator is (a thread's "Backlog ›").
    const onTool = (e: Event) => {
      const { id } = (e as CustomEvent<{ id: string }>).detail;
      setFloor(false);
      setLayout((l) => {
        const ws = shownId(l);
        return ws ? openIn(ws, id)(l) : l;
      });
    };
    window.addEventListener("bakery:open-conversation", onOpen);
    window.addEventListener("bakery:review-thread", onReview);
    window.addEventListener("bakery:open-tool", onTool);
    return () => {
      window.removeEventListener("bakery:open-conversation", onOpen);
      window.removeEventListener("bakery:review-thread", onReview);
      window.removeEventListener("bakery:open-tool", onTool);
    };
  }, []);

  // A focused native view (Code, Preview) keeps the keyboard: Electron hands
  // the shell's own shortcuts back, replayed here as ordinary keydowns, and
  // says which view the operator clicked into, so its panel takes focus.
  useEffect(() => {
    const views = electronViews();
    if (!views) return;
    const offs = [
      views.onShortcut((key) => {
        const init = shortcutInit(key);
        if (init) document.dispatchEvent(new KeyboardEvent("keydown", init));
      }),
      views.onFocus((viewId) => {
        setLayout((l) => {
          const id = shownId(l);
          return id ? L.focusPanel(l, id, panelOfView(viewId)) : l;
        });
      }),
    ];
    return () => offs.forEach((off) => off());
  }, []);

  // Pinch: fingers together zooms out to the floor, apart dives back in.
  // Chromium (Electron) sends a trackpad pinch as ctrl+wheel; WebKit sends
  // gesture events. Either way the page itself never zooms.
  useEffect(() => {
    const el = main.current;
    if (!el) return;
    let total = 0;
    let last = 0;
    const onWheel = (e: WheelEvent) => {
      if (!e.ctrlKey) return;
      e.preventDefault();
      if (e.timeStamp - last > 300) total = 0;
      last = e.timeStamp;
      total += e.deltaY;
      if (total > 40) setFloor(true);
      if (total < -40) setFloor(false);
    };
    const onGesture = (e: Event) => {
      e.preventDefault();
      const scale = (e as Event & { scale?: number }).scale ?? 1;
      if (scale < 0.8) setFloor(true);
      if (scale > 1.25) setFloor(false);
    };
    el.addEventListener("wheel", onWheel, { passive: false });
    el.addEventListener("gesturechange", onGesture);
    return () => {
      el.removeEventListener("wheel", onWheel);
      el.removeEventListener("gesturechange", onGesture);
    };
  }, []);

  const workspace = L.shown(layout);
  /** The panel the top bar names: the focused one, else the Baker's home page. */
  const named = L.isHome(workspace) ? L.CONTROL : workspace?.focus;
  /** Nothing open on the Bakery's side: the floor is its page. */
  const home = !workspace && snap.loaded;
  // There the floor is already the page: ⌘0 or a pinch leaves no floor on
  // for the workspace that shows next.
  useEffect(() => {
    if (home && floor) setFloor(false);
  }, [home, floor]);
  const byId = useMemo(() => new Map(snap.conversations.map((c) => [c.streamId, c])), [snap.conversations]);
  /** A thread's name, live or archived, for actors that name the thread they acted from. */
  const threadName = useCallback(
    (id: string) => byId.get(id)?.title ?? snap.archived.find((c) => c.streamId === id)?.title,
    [byId, snap.archived],
  );
  const attachable = useMemo(() => new Set(snap.attachable), [snap.attachable]);

  // Back and forward: where the window has been (workspace and focus),
  // newest last; stepping through it does not add to it.
  const here = L.placeOf(layout);
  const [hist, setHist] = useState<{ stack: string[]; at: number }>({ stack: [], at: -1 });
  const stepping = useRef(false);
  useEffect(() => {
    if (stepping.current) {
      stepping.current = false;
      return;
    }
    setHist((h) => {
      if (h.stack[h.at] === here) return h;
      const stack = [...h.stack.slice(0, h.at + 1), here].slice(-50);
      return { stack, at: stack.length - 1 };
    });
  }, [here]);
  const near = (dir: -1 | 1) => L.nearPlace(layout, hist.stack, hist.at, dir);
  const step = (dir: -1 | 1) => {
    const at = near(dir);
    if (at < 0) return;
    stepping.current = true;
    setHist((h) => ({ ...h, at }));
    setFloor(false);
    setLayout((l) => L.toPlace(l, hist.stack[at]));
  };

  /** A new workspace's orc before its thread is listed: its tab reads the
   *  name typed, live, and its panel Opening…. */
  const beingMade = (id: string) => (byId.has(id) ? null : L.orcBeingMade(layout, id, listed.current));
  const label = (id: string): PanelLabel => {
    const found = toolOf(id);
    if (found) return { title: found.tool.title(snap, found.arg), icon: found.tool.icon, status: null };
    const making = beingMade(id);
    if (making) return { title: making.name, icon: null, status: "working" };
    const conv = byId.get(id);
    return { title: conv?.title ?? "Gone", icon: null, status: conv?.status ?? "idle" };
  };
  const focusStream = (() => {
    const id = workspace?.focus ?? "";
    const found = toolOf(id);
    return found ? found.arg : id;
  })();
  // A Baker conversation's checkout is the Bakery's own.
  const checkout = [...snap.projects, snap.bakery].flatMap((p) => p.checkouts).find((c) => c.streamId === focusStream);
  // The terminal panel follows the focused thread; on a panel with no thread
  // (the control plane) it opens in the workspace's repository.
  const terminalProject = workspace?.projectId ? (snap.projects.find((p) => p.id === workspace.projectId) ?? null) : null;
  const focusConv = byId.get(focusStream);

  /** Merges the branch a worktree forked from into it (git rule 3), the
   *  notice saying how that went; its project's counts are read again. */
  const updateFrom = (streamId: string, projectId: string, branch: string) =>
    gitUpdate({ stream_id: streamId })
      .then(
        (u) =>
          setNotice(
            u.merged
              ? { bad: false, text: `${u.base} is merged into ${branch || "the thread"}` }
              : { bad: true, text: `Merging ${u.base} stopped on conflicts in ${u.conflicts.join(", ")}: resolve them and commit, or abort the merge` },
          ),
        (e) => setNotice({ bad: true, text: said(e) }),
      )
      .finally(() => reloadRollup(projectId));

  // Git rule 2: a thread's repository is fetched as the thread opens (once a
  // run), so its "N behind" counts what origin has now.
  const openThreads = [...layout.workspaces, layout.baker, ...layout.orcs]
    .flatMap((w) => w.columns.flatMap((c) => c.panels))
    .filter((id) => !id.startsWith("@"))
    .join(" ");
  useEffect(() => {
    for (const id of openThreads.split(" ")) {
      if (!id || fetchedOnce.has(id)) continue;
      const project = snap.projects.find((p) => p.checkouts.some((c) => c.streamId === id && c.path));
      if (!project) continue;
      fetchedOnce.add(id);
      void gitFetch({ stream_id: id })
        .catch(() => {})
        .finally(() => reloadRollup(project.id));
    }
  }, [openThreads, snap.projects]);
  // Every thread's glyph shows its live phase (thinking, writing or working)
  // from the node's activity frames, whether or not it is open (livePhases);
  // a window that opens mid-turn asks the working threads where they stand.
  const working = useMemo(
    () => snap.conversations.filter((c) => c.status === "working").map((c) => ({ streamId: c.streamId, roleId: c.roleId })),
    [snap.conversations],
  );
  useSeedLivePhases(working);
  const view = workspace?.view ?? "screen";

  // Which finished threads' branches have something to merge, and cleanly:
  // the finish card's rule, behind the "ready to merge" glyph.
  const finished = useMemo(
    () =>
      snap.projects.flatMap((p) => {
        const target = baseOf(p.checkouts)?.branch;
        return p.checkouts.flatMap((c) => {
          const conv = byId.get(c.streamId);
          if (c.inPlace || !c.onDisk || !c.branch || conv?.status !== "done") return [];
          const base = peekRollup(p.id)?.worktrees.find((w) => w.stream_id === c.streamId)?.base || target;
          return base ? [{ streamId: c.streamId, base, stamp: `${conv.lastTs}:${c.added}:${c.removed}` }] : [];
        });
      }),
    [snap.projects, byId],
  );
  const merges = useMergeStates(finished, (sid, base) =>
    gitMergePreview({ stream_id: sid, base }).then((p) => ({ clean: p.clean, files: mergedFiles(p) })),
  );
  const mergeKey = [...merges].map(([id, m]) => `${id}:${m ?? ""}`).join(" ");
  // Work the node lists as waiting to merge (a chef's, a workspace's, a
  // worker's: the Bakery's own threads, which `finished` above never holds)
  // is ready to merge too, unless its preview says it clashes.
  const asking = useMergeWaitingIds();
  // The thread and project menus' way to the node's answer and the layout.
  const threadActions = useMemo<ThreadActions>(() => ({ snap, refresh: () => store.refresh(), setLayout, remove: setDeleting }), [snap, store]);
  const threadStates = useMemo<ThreadStates>(
    () => {
      const waiting = new Set(asking.split(" ").filter(Boolean));
      return { merge: (id) => merges.get(id) ?? (waiting.has(id) ? "ready" : null) };
    },
    // `mergeKey` carries what `merges` says.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [mergeKey, asking],
  );

  // Workspaces stay warm: every one shown recently stays mounted, hidden by
  // <Activity> (its DOM and state kept, its updates deferred, its effects
  // paused — so its threads go back to the store's warm cache). Switching
  // back is then instant, with nothing to load.
  // ponytail: the WARM_WORKSPACES most recent stay mounted; raise it if
  // switching among more workspaces than that ever waits.
  const [recent, setRecent] = useState<string[]>([]);
  useEffect(() => {
    const id = workspace?.id;
    if (id) setRecent((r) => [...r.filter((x) => x !== id), id].slice(-WARM_WORKSPACES));
  }, [workspace?.id]);
  const warm = [...new Set([...recent, workspace?.id ?? ""])]
    .map((id) => L.workspaceOf(layout, id))
    .filter((w): w is L.Workspace => !!w);
  // Code and Preview views stay alive (hidden) while their panel is in a warm
  // workspace, and close once it is in none: a closed tab, an evicted one.
  const warmViews = warm
    .flatMap((ws) => ws.columns.flatMap((c) => c.panels))
    .flatMap((id) => viewOfPanel(id) ?? [])
    .sort()
    .join("\n");
  useEffect(() => {
    closeViewsExcept(new Set(warmViews ? warmViews.split("\n") : []));
  }, [warmViews]);

  /** What the command bar offers, built when it opens: the Bakery's commands,
   *  every thread elsewhere, and what is live in the workspace on screen. */
  /** Text search in the bar: "This workspace" is the focused thread's folder,
   *  else the repo of the project on screen; a match opens its thread's Code. */
  const barFiles = (only: boolean): BarFiles => {
    const checkouts = snap.projects.flatMap((p) => p.checkouts);
    const streamId = checkout?.path
      ? focusStream
      : ((workspace && projectFor(workspace, focusStream)?.checkouts.find((c) => c.inPlace && c.path)?.streamId) ?? null);
    const open = (m: Found, from: string | null) => {
      const sid = from ?? checkouts.find((c) => c.path === m.folder)?.streamId;
      if (!sid) return;
      revealInCode(sid, m.path, m.line ?? 1);
      setFloor(false);
      setLayout((l) => {
        const ws = homeFor(l, sid) ?? shownId(l);
        return ws ? openIn(ws, `@code:${sid}`)(L.setActive(l, ws)) : l;
      });
    };
    return { streamId, open, only };
  };

  const barLists = () => {
    const here = workspace?.id ?? null;
    const go = (id: string) => {
      setFloor(false);
      setLayout((l) => L.setActive(l, id));
    };
    const settings = (section: string) => window.dispatchEvent(new CustomEvent("bakery:open-settings", { detail: section }));
    const commands: BarCommand[] = [
      layout.mode === "baker"
        ? { id: "mode", label: "Switch to Bakery", hint: "⌘1", run: () => switchMode("bakery") }
        : { id: "mode", label: "Switch to Baker", hint: "⌘2", run: () => switchMode("baker") },
      ...layout.workspaces.map((w, i) => ({
        id: `go:${w.id}`,
        label: `Go to ${w.name}`,
        hint: i < 9 ? `⌃${i + 1}` : undefined,
        run: () => go(w.id),
      })),
      { id: "new-thread", label: "New Baker thread", run: () => void store.newBakerThread().then((id) => openThread(id, true)) },
    ];
    if (here) {
      commands.push(
        { id: "containers", label: "Open Containers", run: () => setLayout(openIn(here, "@containers")) },
        { id: "review", label: "Open Review & merge", run: () => setLayout(openIn(here, "@review")) },
        { id: "prs", label: "Open Pull requests", run: () => setLayout(openIn(here, "@prs")) },
        { id: "actions", label: "Open Actions", run: () => setLayout(openIn(here, "@actions")) },
      );
      if (focusConv) {
        commands.push(
          { id: "code", label: `Open ${focusConv.title} in Code`, run: () => setLayout(openIn(here, `@code:${focusConv.streamId}`)) },
          { id: "preview", label: `Preview ${focusConv.title}`, run: () => setLayout(openIn(here, `@preview:${focusConv.streamId}`)) },
        );
      }
      commands.push({
        id: "view",
        label: view === "screen" ? "Show as columns" : "Show as tabs",
        run: () => setLayout((l) => L.setView(l, here, view === "screen" ? "strip" : "screen")),
      });
    }
    commands.push(
      {
        id: "sidebar",
        label: layout.sidebar.open ? "Hide the sidebar" : "Show the sidebar",
        hint: "⌘\\",
        run: () => setLayout((l) => ({ ...l, sidebar: { ...l.sidebar, open: !l.sidebar.open } })),
      },
      { id: "terminal", label: layout.bottom?.open ? "Hide the terminal" : "Show the terminal", hint: "⌃`", run: () => setLayout(toggleBottom) },
      { id: "settings", label: "Settings", run: () => settings("agents") },
      { id: "machines", label: "Machines", run: () => settings("machines") },
    );
    // Live here: the workspace's panels, then its project's other threads.
    const open = new Set(workspace?.columns.flatMap((c) => c.panels) ?? []);
    const hereItems: BarItem[] = [...open].map((id) => {
      const lab = label(id);
      return {
        id: `panel:${id}`,
        title: lab.title,
        place: "",
        status: lab.status,
        streamId: lab.status ? id : undefined,
        icon: lab.icon,
        run: () => here && setLayout((l) => L.focusPanel(l, here, id)),
      };
    });
    const homeHere = (c: Conversation) => here !== null && homeFor(layout, c.streamId) === here;
    const thread = (c: Conversation): BarItem => ({
      id: `thread:${c.streamId}`,
      title: c.title,
      place: c.place,
      status: c.status,
      streamId: c.streamId,
      run: () => openThread(c.streamId, c.bakerThread),
    });
    for (const c of snap.conversations) if (homeHere(c) && !open.has(c.streamId)) hereItems.push(thread(c));
    return {
      commands,
      threads: snap.conversations.filter((c) => !homeHere(c)).map(thread),
      here: hereItems,
      hereName: workspace?.name ?? "",
    };
  };

  /** What a thread's pane header acts on: its lock, its worktree's past
   *  threads, a new thread beside it (PaneHeader). */
  const checkouts = snap.projects.flatMap((p) => p.checkouts);
  const paneActions: PaneActions = {
    locked: (wsId, id) => {
      const w = L.workspaceOf(layout, wsId);
      return !!w && L.isLocked(w, id);
    },
    toggleLock: (wsId, id) => setLayout((l) => L.toggleLock(l, wsId, id)),
    past: (id) => {
      const path = checkouts.find((c) => c.streamId === id)?.path;
      if (!path) return [];
      const same = new Set(checkouts.filter((c) => c.path === path && c.streamId !== id).map((c) => c.streamId));
      return [...snap.conversations, ...snap.archived]
        .filter((c) => same.has(c.streamId))
        .sort((a, b) => (a.lastTs < b.lastTs ? 1 : a.lastTs > b.lastTs ? -1 : 0));
    },
    enter: (wsId, c) => {
      const show = () => setLayout((l) => L.openPanel(L.setActive(l, wsId), wsId, c.streamId));
      if (!c.archived) return show();
      void conversationArchive({ stream_id: c.streamId, archived: false }).then(() => {
        store.refresh();
        show();
      });
    },
    starts: (id) => {
      if (byId.get(id)?.bakerThread) return "baker";
      const project = snap.projects.find((p) => p.checkouts.some((c) => c.streamId === id && c.path));
      return project ? agentPlace(project.checkouts, id, project.id !== own) : null;
    },
    start: async (wsId, id, agent) => {
      if (!agent) {
        const created = await store.newBakerThread();
        setLayout((l) => L.openPanel(l, wsId, created));
        return;
      }
      const project = snap.projects.find((p) => p.checkouts.some((c) => c.streamId === id));
      if (project) await addAgent(wsId, project.id, agent);
    },
    started: (id) => {
      const c = byId.get(id);
      const agent = c && nodes ? agentOf(nodes, id, c.roleId) : null;
      if (!agent || !nodes) return null;
      const kids = agentTree(nodes, agent.id);
      return { count: kids.length, working: kids.some((k) => k.node.status === "working") };
    },
    openMap: (wsId, id) => setMapAt({ wsId, streamId: id }),
    finishFor: (id) => {
      const project = snap.projects.find((p) => p.checkouts.some((c) => c.streamId === id));
      const co = project?.checkouts.find((c) => c.streamId === id);
      const base = project ? baseOf(project.checkouts) : null;
      // Every turn end offers it; the card itself stays away while the
      // branch has nothing beyond its base.
      if (!project || !co || co.inPlace || !co.branch || !base || byId.get(id)?.status !== "done") return null;
      return { projectId: project.id, branch: co.branch, fallbackBase: base.branch };
    },
    reviewFrom: (wsId, id) => {
      reviewBranch(id);
      setLayout(openIn(wsId, "@review"));
    },
    branchOf: (id) => {
      const co = snap.projects.flatMap((p) => p.checkouts).find((c) => c.streamId === id);
      return co && !co.inPlace && co.branch ? co.branch : null;
    },
    git: (id) => {
      // A Baker conversation's checkout is the Bakery's own, in place: it
      // reads its branch (main) and is behind nothing.
      const co = [...snap.projects, snap.bakery].flatMap((p) => p.checkouts).find((c) => c.streamId === id);
      if (!co) return null;
      const project = co.inPlace ? null : (snap.projects.find((p) => p.streamIds.includes(id))?.id ?? null);
      return {
        branch: co.branch,
        project,
        working: byId.get(id)?.status === "working",
        update: () => {
          if (project) void updateFrom(id, project, co.branch);
        },
      };
    },
    showTerminals: (wsId, id, ids) => showTerminalsHandler(wsId, id, ids),
  };

  useEffect(() => {
    threadInfoHandler = (id) => {
      const c = snap.conversations.find((x) => x.streamId === id) ?? snap.archived.find((x) => x.streamId === id);
      return c ? { title: c.title, bakerThread: c.bakerThread } : null;
    };
  });

  // A prepared row: the terminal panel opens on this thread's terminals, in
  // its room. A command's row: the thread's Agent terminal opens as a tab
  // beside the thread, on that command; the panel is left as it is.
  useEffect(() => {
    showTerminalsHandler = (wsId, id, ids, at) => {
      if (ids.length === 0) {
        const tab = `@agent:${id}`;
        setLayout((l) => {
          const had = L.workspaceOf(l, wsId)?.columns.some((c) => c.panels.includes(tab));
          const opened = openIn(wsId, tab)(l);
          return had ? opened : L.movePanel(opened, wsId, tab, id, "after");
        });
        if (at) showCommand(id, at);
        return;
      }
      setLayout((l) => L.focusPanel({ ...l, bottom: { open: true, height: l.bottom?.height ?? 240 } }, wsId, id));
      showTerminals(id, ids);
    };
  });

  // An edit row's file: the thread's Code view opens on it at the first line
  // the edit changed. A file that changed since that edit says so.
  useEffect(() => {
    openFileHandler = (wsId, id, change) => {
      const path = inside([...snap.projects, snap.bakery].flatMap((p) => p.checkouts).find((c) => c.streamId === id)?.path ?? "", change.path);
      revealInCode(id, path, change.line);
      setLayout(openIn(wsId, `@code:${id}`));
      if (change.after === null) return;
      filesRead({ stream_id: id, path }).then(
        (f) => {
          if (f.text !== change.after) setNotice({ bad: false, text: `${path.split("/").pop()} has changed since this edit; this is how it reads now.` });
        },
        () => {},
      );
    };
  });

  /** A workspace's threads for its agent map, the one it opened from first. */
  const mapThreads = (at: { wsId: string; streamId: string }) => {
    const here = snap.conversations.filter((c) => c.streamId === at.streamId || homeFor(layout, c.streamId) === at.wsId);
    return [...here.filter((c) => c.streamId === at.streamId), ...here.filter((c) => c.streamId !== at.streamId)];
  };

  /** A new agent of a project, opened in this workspace: in a worktree of
   *  its own, unless the operator chose to share one. */
  const addAgent = async (wsId: string, projectId: string, a: NewAgent) => {
    const opened = await projectOpen({ repo: a.repo, name: a.name, worktree_path: a.shared, project_id: projectId, tools: a.tools });
    store.refresh();
    setLayout((l) => L.openPanel(l, wsId, opened.context.stream_id));
  };

  /** The project a workspace adds agents to: its own, or in Baker mode the
   *  shown thread's (else the first) of the Baker's projects. */
  const projectFor = (ws: L.Workspace, shown: string) => {
    const ids = ws.id === L.BAKER ? layout.bakerProjects : ws.projectId ? [ws.projectId] : [];
    const mine = snap.projects.filter((p) => ids.includes(p.id));
    return mine.find((p) => p.checkouts.some((c) => c.streamId === shown)) ?? mine[0] ?? null;
  };

  /** A workspace's colour: its mark in the switcher and the top bar. */
  const colorOf = (ws: L.Workspace) =>
    ws.id === L.BAKER
      ? "var(--baker-solid)"
      : workspaceColor(Math.max(0, (ws.orc ? layout.orcs : layout.workspaces).findIndex((w) => w.id === ws.id)));

  /** One workspace's tab groups (or its strip of columns). The Baker's home
   *  page keeps its tab strip, so it shows on the tab groups in either view. */
  const renderWorkspace = (ws: L.Workspace) =>
    (ws.view ?? "screen") === "strip" && !L.isHome(ws) ? (
      <>
        <Strip color={colorOf(ws)} layout={layout} listed={listed.current} place={homeFor} setLayout={setLayout} snap={snap} store={store} workspace={ws} />
        <Minimap setLayout={setLayout} snap={snap} workspace={ws} />
      </>
    ) : (
      <Screen
        actions={(id) => {
          const project = projectFor(ws, id);
          return (
            <span className="flex items-center gap-1.5">
              {byId.has(id) && (
                <span className="flex overflow-hidden rounded-[5px] border border-(--line) text-[11px]">
                  <button
                    aria-label={`Open ${label(id).title} in Code`}
                    className="flex h-[22px] items-center gap-[5px] bg-(image:--grad-button) px-2 text-(--fg-2) hover:text-(--fg)"
                    onClick={() => setLayout(openIn(ws.id, `@code:${id}`))}
                    type="button"
                  >
                    <CodeIcon className="size-3 text-(--hue-containers)" />
                    Code
                  </button>
                  <button
                    aria-label={`Preview ${label(id).title}`}
                    className="flex h-[22px] items-center gap-[5px] border-(--line) border-l bg-(image:--grad-button) px-2 text-(--fg-2) hover:text-(--fg)"
                    onClick={() => setLayout(openIn(ws.id, `@preview:${id}`))}
                    type="button"
                  >
                    <GlobeIcon className="size-3 text-(--violet)" />
                    Preview
                  </button>
                </span>
              )}
              <PanelMenu
                addAgent={project && { place: agentPlace(project.checkouts, id, project.id !== own), create: (a) => addAgent(ws.id, project.id, a) }}
                onOpen={(kind) => setLayout(openIn(ws.id, `@${kind}`))}
              />
            </span>
          );
        }}
        label={label}
        place={homeFor}
        render={(id) => {
          const found = toolOf(id);
          return found ? (
            found.tool.render({ store, snap, layout, workspace: ws, setLayout, color: colorOf(ws) }, found.arg)
          ) : (
            <ConversationBody attachable={attachable.has(id)} making={!!beingMade(id)} store={store} streamId={id} wsId={ws.id} />
          );
        }}
        setLayout={setLayout}
        workspace={ws}
      />
    );

  return (
    <PaneContext.Provider value={paneActions}>
    <ThreadNamesContext.Provider value={threadName}>
    <ThreadStatesContext.Provider value={threadStates}>
    <ThreadMenuContext.Provider value={threadActions}>
    {/* `bakery-ai` opts the whole shell into the island's scoped reset (ai-panel.css). */}
    <div className="bakery-ai flex h-dvh w-full overflow-hidden bg-(--bg-chrome) text-(--fg-body)" data-testid="shell">
      {layout.sidebar.open && (
        <Sidebar
          layout={layout}
          nodes={nodes ?? []}
          onMode={switchMode}
          openIn={(wsId, id) => {
            setFloor(false);
            setLayout(openIn(wsId, id));
          }}
          setLayout={setLayout}
          snap={snap}
          store={store}
        />
      )}

      <main className="flex min-w-0 flex-1 flex-col bg-(--bg-chrome)" ref={main}>
        {/* The top bar stays over the floor too, so its floor button turns it
            back off; with nothing open, it keeps only what works there. */}
        {(workspace || home) && (
          <TopBar
            badge={
              !workspace
                ? { color: "var(--hue-project)", mark: <LayoutGridIcon className="size-2.5" /> }
                : workspace.id === L.BAKER
                  ? { color: "var(--baker-solid)", mark: <ChefHatIcon className="size-2.5" /> }
                  : { color: colorOf(workspace), mark: (workspace.name[0] ?? "?").toUpperCase() }
            }
            canBack={near(-1) >= 0}
            canForward={near(1) >= 0}
            floor={floor}
            onBack={() => step(-1)}
            onFloor={workspace ? () => setFloor((v) => !v) : undefined}
            onForward={() => step(1)}
            onSearch={() => setBar("all")}
            bottomOpen={layout.bottom?.open ?? false}
            onBottom={workspace ? () => setLayout(toggleBottom) : undefined}
            onSidebar={() => setLayout((l) => ({ ...l, sidebar: { ...l.sidebar, open: !l.sidebar.open } }))}
            onView={workspace ? () => setLayout((l) => L.setView(l, workspace.id, view === "screen" ? "strip" : "screen")) : undefined}
            sidebarOpen={layout.sidebar.open}
            sub={named ? label(named).title : ""}
            title={workspace?.name ?? "Floor"}
            view={view}
            waiting={
              <>
                <RestartPill />
                <ResourcePill />
                <DeployPills />
                <WaitingPill
                baker={snap.conversations.find((c) => c.bakerThread && c.main)?.streamId ?? null}
                openHistory={() => {
                  const ws = shownId(layout);
                  if (ws) setLayout(openIn(ws, "@remote"));
                }}
                openThread={(id) => openThread(id, byId.get(id)?.bakerThread ?? false)}
                />
              </>
            }
          />
        )}
        {(floor || home) && (
          <Floor
            home={home}
            layout={layout}
            nodes={nodes ?? []}
            onClose={() => setFloor(false)}
            setLayout={setLayout}
            snap={snap}
            toolLabel={(id) => {
              const found = toolOf(id);
              return found ? { title: found.tool.title(snap, found.arg), icon: found.tool.icon } : null;
            }}
          />
        )}
        {workspace ? (
          <Activity mode={floor ? "hidden" : "visible"}>
            <div className="flex min-h-0 flex-1 flex-col">
              {warm.map((ws) => (
                <Activity key={ws.id} mode={ws.id === workspace.id ? "visible" : "hidden"}>
                  <KeptWorkspace
                    menu={threadActions}
                    names={threadName}
                    pane={paneActions}
                    render={() => renderWorkspace(ws)}
                    shown={!floor && ws.id === workspace.id}
                    states={threadStates}
                  />
                </Activity>
              ))}
            </div>
            {layout.bottom?.open && (
              <BottomPanel
                height={layout.bottom.height}
                onClose={() => setLayout(toggleBottom)}
                onHeight={(height) => setLayout((l) => ({ ...l, bottom: { open: true, height } }))}
                place={terminalPlace({
                  workspace,
                  thread: focusConv ? { streamId: focusConv.streamId, title: focusConv.title } : null,
                  project: terminalProject,
                })}
                tile={{ color: colorOf(workspace), mark: (workspace.name[0] ?? "?").toUpperCase() }}
              />
            )}
          </Activity>
        ) : (
          !snap.loaded && <div className="grid flex-1 place-items-center text-(--fg-faint) text-xs">Connecting…</div>
        )}
      </main>
      {bar && <CommandBar {...barLists()} files={barFiles(bar === "files")} onClose={() => setBar(null)} />}
      {deleting && (
        <DeleteDialog
          onClose={() => setDeleting(null)}
          onDone={(done) => {
            // A gone project's workspace goes with the node's next list (dropGone).
            store.refresh();
            setLayout((l) => done.removed.threads.reduce(L.closeEverywhere, l));
          }}
          target={deleting}
        />
      )}
      {notice && (
        <div
          className={`fixed top-[44px] left-1/2 z-50 flex max-w-[min(560px,90vw)] -translate-x-1/2 items-center gap-2 rounded-md border bg-(--bg-raised) px-3 py-1.5 text-xs shadow-(--shadow-menu) ${notice.bad ? "border-(--chip-bad-bg) text-(--bad)" : "border-(--chip-warn-bg) text-(--fg-body)"}`}
          data-testid="shell-notice"
          role="status"
        >
          <TriangleAlertIcon className={`size-3.5 shrink-0 ${notice.bad ? "text-(--bad)" : "text-(--warn)"}`} />
          <span className="min-w-0 truncate" title={notice.text}>
            {notice.text}
          </span>
          <button
            aria-label="Dismiss"
            className="grid size-5 shrink-0 place-items-center rounded text-(--fg-dim) hover:bg-(--bg-chip) hover:text-(--fg)"
            onClick={() => setNotice(null)}
            type="button"
          >
            <XIcon className="size-3" />
          </button>
        </div>
      )}
      {mapAt && nodes && (
        <WorkspaceMap
          nodes={nodes}
          onClose={() => setMapAt(null)}
          onJob={(sid) => {
            setMapAt(null);
            openThread(sid, byId.get(sid)?.bakerThread ?? false);
            setLayout((l) => (l.bottom?.open ? l : toggleBottom(l)));
          }}
          onOpen={(sid, call) => {
            setMapAt(null);
            if (call) revealSubagent(call);
            openThread(sid, byId.get(sid)?.bakerThread ?? false);
          }}
          threads={mapThreads(mapAt)}
        />
      )}
    </div>
    </ThreadMenuContext.Provider>
    </ThreadStatesContext.Provider>
    </ThreadNamesContext.Provider>
    </PaneContext.Provider>
  );
}
