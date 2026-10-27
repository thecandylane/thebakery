// A Bakery workspace's control plane (Round 4, A): its first tab, locked
// open. Its project at a glance: every worktree's state beside its action
// (Round 10), repos (CI, worktrees, a new thread in any of them), threads live and past, its containers, and the kinds of action
// it always allows. A workspace with no project yet opens on making one:
// name it, then its first repo, from GitHub or a folder, makes the project.
// The stack, MCP servers, skills, tools and setup come from its files
// (ControlCards.tsx). On the Baker's side, the Bakery's own (project ""):
// the operator's "project control panel, but of the Bakery itself", with
// everything alive in the Bakery.

import { type ReactNode, useCallback, useContext, useEffect, useRef, useState } from "react";
import {
  BoxIcon,
  ChefHatIcon,
  CircleCheckIcon,
  CircleXIcon,
  EyeIcon,
  FolderIcon,
  GitBranchIcon,
  GitMergeIcon,
  GitPullRequestIcon,
  GlobeIcon,
  HistoryIcon,
  MessagesSquareIcon,
  PencilIcon,
  PlusIcon,
  SearchIcon,
  ShieldCheckIcon,
  TerminalIcon,
  Trash2Icon,
  XIcon,
} from "lucide-react";
import {
  agentsAlwaysAllowed,
  agentsAlwaysAllowedRemove,
  githubClone,
  githubRepos,
  projectCreate,
  projectOpen,
  projectRename,
  socketOpen,
  type GrantView,
  type Repo,
} from "$lib/api.generated";
import { ago, STATUSES, statusLabel, type Conversation } from "$lib/conversations";
import { pickFolder } from "$lib/desktop";
import { KIND_WORDS } from "../AgentPanel";
import { BakeryCard } from "./BakeryCard";
import { Card, ProjectControlCards } from "./ControlCards";
import { SecretFilesCard } from "./SecretFiles";
import { DiskCard } from "./DiskCard";
import { ToolchainsCard } from "./Toolchains";
import { ToolkitCard } from "./Toolkit";
import { TrashCard } from "./TrashCard";
import { membersOf } from "./floorModel";
import * as L from "./layout";
import { PaneContext } from "./PaneHeader";
import { DeleteMenu, RemoveProjectButton, ThreadMenu } from "./ThreadMenu";
import { WorktreesCard } from "./Worktrees";
import { AddAgentForm, type NewAgent, POP, useDismiss } from "./PanelMenu";
import { inPrGroup } from "./githubModel";
import { usePrs, useRuns } from "./GitHubPanels";
import { reloadRollup, useRollup } from "./rollup";
import type { ShellProject } from "./shellSource.svelte";
import { ContainerLine, type ContainerRows, readContainerRows } from "./Sidebar";
import { useCached } from "./cache";
import { agentPlace, ciOf, isFailedRun } from "./sidebarModel";
import { Chip, Freshness, said, StatusDot, type ToolContext, usePoll, workspaceColor } from "./ui";

const BUTTON =
  "flex h-6 shrink-0 items-center gap-1.5 whitespace-nowrap rounded-[5px] border border-(--line) bg-(image:--grad-button) px-2.5 text-(--fg-body) text-[11px] hover:text-(--fg) disabled:opacity-40";
const ACCENT =
  "flex h-6 shrink-0 items-center gap-1.5 rounded-[5px] border border-(--accent-fg) bg-(image:--grad-accent) px-2.5 font-bold text-(--fg-on-solid) text-[11px] hover:brightness-105";
const ROW = "flex h-7 min-w-0 items-center gap-2 rounded-[5px] px-1.5 text-left";
/** The floor's faint dot grid behind the cards. */
const DOTS = {
  backgroundImage: "radial-gradient(color-mix(in srgb, var(--fg) 6%, transparent) 1px, transparent 1px)",
  backgroundSize: "18px 18px",
};
const KIND_ICONS: Record<string, ReactNode> = {
  execute: <TerminalIcon className="size-3" />,
  edit: <PencilIcon className="size-3" />,
  delete: <Trash2Icon className="size-3" />,
  read: <EyeIcon className="size-3" />,
  search: <SearchIcon className="size-3" />,
  fetch: <GlobeIcon className="size-3" />,
};
const basename = (p: string) => p.split("/").filter(Boolean).at(-1) ?? p;
/** The project a folder is already a root of: the node gives a folder to one project only. */
const rootOf = (projects: readonly ShellProject[], path: string | null | undefined) =>
  path ? projects.find((p) => p.checkouts.some((c) => (c.repoRoot || c.path) === path)) : undefined;
const inProject = (p: ShellProject) => <span className="shrink-0 text-(--fg-dim) text-[10.5px]">in {p.name}</span>;
const tilde = (p: string) => p.replace(/^\/(Users|home)\/[^/]+(?=\/|$)/, "~");
/** Most urgent first, the order every board uses. */
const rank = (c: Conversation) => STATUSES.findIndex((s) => s.status === c.status);
/** Shown only while its row is hovered or holds the focus. */
const HOVER = "opacity-0 focus-within:opacity-100 group-hover:opacity-100";

/** One thread: its state and name (and where it is, when asked), and its menu. */
function ThreadRow({ c, onOpen, place = false }: { c: Conversation; onOpen: () => void; place?: boolean }) {
  return (
    <div className="group flex min-w-0 items-center">
      <button className={`${ROW} min-w-0 flex-1 hover:bg-(--bg-chip)`} onClick={onOpen} title={c.note || c.title} type="button">
        <StatusDot status={c.status} streamId={c.streamId} title={statusLabel(c.status)} />
        <span className="min-w-0 flex-1 truncate text-(--fg-body)">{c.title}</span>
        {place && c.place && <span className="max-w-[40%] shrink-0 truncate text-(--fg-dim) text-[10.5px]">{c.place}</span>}
        {c.status !== "needs_input" && (
          <span className="shrink-0 text-(--fg-dim) text-[11px]">{c.status === "working" ? ago(c.lastTs) : statusLabel(c.status)}</span>
        )}
      </button>
      <ThreadMenu className={HOVER} streamId={c.streamId} />
    </div>
  );
}

/** A button and the popover it opens, closed by Escape or a press outside;
 *  it hangs left from the button's right edge, or right from its left one. */
function Pop({ label, open, setOpen, width, toRight = false, children }: {
  label: ReactNode;
  open: boolean;
  setOpen: (open: boolean) => void;
  width: string;
  toRight?: boolean;
  children: ReactNode;
}) {
  const box = useRef<HTMLDivElement>(null);
  useDismiss(open, box, () => setOpen(false));
  return (
    <div className="relative" ref={box}>
      <span className="contents" onClick={() => setOpen(!open)}>
        {label}
      </span>
      {open && (
        <div className={`${toRight ? POP.replace("right-0", "left-0") : POP} ${width}`} role="dialog">
          {children}
        </div>
      )}
    </div>
  );
}

export function ControlPlane({ ctx }: { ctx: ToolContext }) {
  const ws = ctx.workspace;
  if (!ws.projectId && (ws.id === L.BAKER || ws.orc)) return <BakeryPlane ctx={ctx} />;
  const project = ctx.snap.projects.find((p) => p.id === ws.projectId);
  if (project) return <ProjectPlane ctx={ctx} project={project} />;
  // Just made: the topology has not named it yet.
  if (ctx.workspace.projectId) return <p className="p-4 text-(--fg-faint) text-xs">Opening…</p>;
  return <NewWorkspace ctx={ctx} />;
}

function ProjectPlane({ ctx, project }: { ctx: ToolContext; project: ShellProject }) {
  const { snap, workspace: ws, setLayout, store } = ctx;
  const pane = useContext(PaneContext);
  const rollup = useRollup(project.id);
  /** The open popover: "repo", "thread", or a repo's root for its "+ Thread". */
  const [popover, setPopover] = useState<string | null>(null);
  const [pastOpen, setPastOpen] = useState(false);
  const threads = membersOf(ws, snap.projects, snap.conversations).sort((a, b) => rank(a) - rank(b));
  const threadTitle = (id: string | null | undefined) => threads.find((c) => c.streamId === id)?.title;
  const streams = new Set(project.streamIds);
  const past = snap.archived.filter((c) => streams.has(c.streamId));
  const working = threads.filter((c) => c.status === "working").length;
  const needs = threads.filter((c) => c.status === "needs_input").length;
  const place = agentPlace(project.checkouts, "", false);

  const addAgent = async (a: NewAgent) => {
    const opened = await projectOpen({ repo: a.repo, name: a.name, worktree_path: a.shared, project_id: project.id, tools: a.tools });
    store.refresh();
    setLayout((l) => L.openPanel(l, ws.id, opened.context.stream_id));
  };
  const pop = (key: string) => (open: boolean) => setPopover(open ? key : null);
  const threadForm = (repo: string) => (
    <AddAgentForm create={addAgent} onDone={() => setPopover(null)} place={{ ...place, repo }} />
  );

  return (
    <div className="h-full overflow-auto bg-(--bg-app) p-4 text-xs" data-testid="control-plane" style={DOTS}>
      <div className="mb-3 flex items-center gap-3 rounded-[10px] border border-(--line) bg-(image:--grad-chrome) px-3.5 py-3">
        <span
          className="grid size-[30px] shrink-0 place-items-center rounded-[7px] font-bold text-(--fg-on-solid) text-[15px]"
          style={{ background: ctx.color }}
        >
          {(project.name[0] ?? "?").toUpperCase()}
        </span>
        <div className="flex min-w-0 flex-1 flex-wrap items-center gap-2">
          <span className="truncate font-bold text-(--fg) text-[17px]">{project.name}</span>
          <Chip>
            {place.repos.length} {place.repos.length === 1 ? "REPO" : "REPOS"}
          </Chip>
          {working > 0 && <Chip tone="ok">{working} WORKING</Chip>}
          {needs > 0 && <Chip tone="warn">{needs} NEEDS YOU</Chip>}
        </div>
        <Freshness at={rollup.at} busy={rollup.busy} error={rollup.error} onReload={() => reloadRollup(project.id)} />
        <RemoveProjectButton className={BUTTON} project={project} />
        <Pop
          label={
            <button className={BUTTON} type="button">
              <FolderIcon className="size-3 text-(--hue-project)" />
              Add repo
            </button>
          }
          open={popover === "repo"}
          setOpen={pop("repo")}
          width="w-[560px]"
        >
          <AddRepo project={project} projects={snap.projects} store={store} />
        </Pop>
        {place.repos.length > 0 && (
          <Pop
            label={
              <button className={ACCENT} type="button">
                <PlusIcon className="size-3" />
                New thread
              </button>
            }
            open={popover === "thread"}
            setOpen={pop("thread")}
            width="w-72"
          >
            {threadForm(place.repo)}
          </Pop>
        )}
      </div>

      <div className="grid grid-cols-3 items-start gap-3">
        <BakeryCard project={project} />
        <WorktreesCard ctx={ctx} project={project} rollup={rollup.data} threads={threads} />
        <Card count={place.repos.length} hue="var(--hue-project)" icon={<FolderIcon className="size-3.5" />} title="Repos" wide>
          <div className="grid grid-cols-[repeat(auto-fill,minmax(240px,1fr))] gap-2">
            {place.repos.map(({ root, name }) => {
              const here = project.checkouts.filter((c) => (c.repoRoot || c.path) === root);
              const branch = here.find((c) => c.inPlace)?.branch ?? "";
              const trees = here.filter((c) => !c.inPlace && c.onDisk).length;
              const repo = rollup.data?.repos.find((r) => r.path === root);
              const ci = repo && !repo.problem && branch ? ciOf(repo.runs, branch) : null;
              return (
                <div className="flex min-w-0 flex-col gap-[3px] rounded-md border border-(--line-soft) bg-(--bg-app) px-2.5 py-2" key={root}>
                  <span className="flex min-w-0 items-center gap-2" title={root}>
                    <FolderIcon className="size-3.5 shrink-0 text-(--hue-project)" />
                    <span className="truncate font-bold text-(--fg)">{name}</span>
                  </span>
                  <span className="flex min-w-0 items-center gap-1.5 pl-[22px] text-(--fg-dim) text-[11px]">
                    {branch && (
                      <>
                        <GitBranchIcon className="size-3 shrink-0 text-(--violet)" />
                        <span className="truncate">{branch}</span>
                        <span>·</span>
                      </>
                    )}
                    <span className="shrink-0">
                      {trees} {trees === 1 ? "worktree" : "worktrees"}
                    </span>
                  </span>
                  <span className="mt-[3px] flex min-w-0 items-center gap-1.5 pl-[22px]">
                    {ci ? (
                      <Chip tone={ci === "pass" ? "ok" : ci === "fail" ? "bad" : "warn"}>
                        CI {ci === "pass" ? "✓" : ci === "fail" ? "✕" : "◐"}
                      </Chip>
                    ) : repo?.problem ? (
                      <span className="min-w-0 truncate text-(--fg-faint) text-[10.5px]" title={repo.problem}>
                        GitHub unavailable
                      </span>
                    ) : (
                      repo && <Chip>NO RUNS</Chip>
                    )}
                    <span className="flex-1" />
                    <Pop
                      label={
                        <button aria-label={`New thread in ${name}`} className={`${BUTTON} h-5 px-1.5`} type="button">
                          <PlusIcon className="size-3 text-(--ok)" />
                          Thread
                        </button>
                      }
                      open={popover === root}
                      setOpen={pop(root)}
                      toRight
                      width="w-72"
                    >
                      {threadForm(root)}
                    </Pop>
                    <DeleteMenu label={name} target={{ kind: "repo", project_id: project.id, repo: root }} />
                  </span>
                </div>
              );
            })}
          </div>
        </Card>

        <Card count={threads.length} hue="var(--hue-threads)" icon={<MessagesSquareIcon className="size-3.5" />} title="Threads">
          {threads.map((c) => (
            <ThreadRow c={c} key={c.streamId} onOpen={() => pane?.enter(ws.id, c)} />
          ))}
          {past.length > 0 && (
            <button
              aria-expanded={pastOpen}
              className={`${ROW} text-(--fg-dim) text-[11px] hover:text-(--fg-2)`}
              onClick={() => setPastOpen((o) => !o)}
              type="button"
            >
              <HistoryIcon className="size-3 shrink-0" />
              {past.length} past {past.length === 1 ? "thread" : "threads"} to resume
            </button>
          )}
          {pastOpen &&
            past.map((c) => (
              <div className="group flex min-w-0 items-center" key={c.streamId}>
                <button className={`${ROW} min-w-0 flex-1 pl-5 hover:bg-(--bg-chip)`} onClick={() => pane?.enter(ws.id, c)} title="Restore and open" type="button">
                  <span className="min-w-0 flex-1 truncate text-(--fg-2)">{c.title}</span>
                  <span className="shrink-0 text-(--fg-faint) text-[10.5px]">{ago(c.lastTs)}</span>
                </button>
                <ThreadMenu className={HOVER} streamId={c.streamId} />
              </div>
            ))}
        </Card>

        <ToolkitCard project={project} />
        <ProjectControlCards ctx={ctx} project={project} />
        <ToolchainsCard threads={threads} />
        <DiskCard threads={threads} />
        <SecretFilesCard ctx={ctx} project={project} />
        <Containers streams={streams} titleOf={(id) => snap.conversations.find((c) => c.streamId === id)?.title} />
        <AlwaysAllowed
          mine={(g) => (g.scope === "project" ? g.id === project.id : g.scope === "chat" && !!threadTitle(g.id))}
          where={(g) => (g.scope === "chat" ? threadTitle(g.id) : undefined)}
        />
      </div>
    </div>
  );
}

/** The Bakery's own control plane (project ""), on the Baker's side: what a
 *  project's has of its own (its deploys, toolkit, commands, and what its
 *  main checkout and worktrees are built from, its setup and internet), and
 *  everything alive in the Bakery: every project, thread and container, its
 *  GitHub, and every "Always allow". */
function BakeryPlane({ ctx }: { ctx: ToolContext }) {
  const { snap, store, setLayout } = ctx;
  const [error, setError] = useState("");
  const threads = [...snap.conversations].sort((a, b) => rank(a) - rank(b));
  const working = threads.filter((c) => c.status === "working").length;
  const needs = threads.filter((c) => c.status === "needs_input").length;
  const titleOf = (id: string | null | undefined) => snap.conversations.find((c) => c.streamId === id)?.title;
  /** In its own workspace, as everywhere a thread opens from. */
  const open = (c: Conversation) =>
    window.dispatchEvent(new CustomEvent("bakery:open-conversation", { detail: { streamId: c.streamId, bakerThread: c.bakerThread } }));
  const newThread = () => {
    setError("");
    store.newBakerThread().then(
      (id) => setLayout((l) => L.openPanel(l, L.BAKER, id)),
      (e) => setError(said(e)),
    );
  };

  return (
    <div className="h-full overflow-auto bg-(--bg-app) p-4 text-xs" data-testid="control-plane-bakery" style={DOTS}>
      <div className="mb-3 flex items-center gap-3 rounded-[10px] border border-(--line) bg-(image:--grad-chrome) px-3.5 py-3">
        <span className="grid size-[30px] shrink-0 place-items-center rounded-[7px] text-(--fg-on-solid)" style={{ background: "var(--baker-solid)" }}>
          <ChefHatIcon className="size-4" />
        </span>
        <div className="flex min-w-0 flex-1 flex-wrap items-center gap-2">
          <span className="truncate font-bold text-(--fg) text-[17px]">Bakery</span>
          <Chip>
            {snap.projects.length} {snap.projects.length === 1 ? "PROJECT" : "PROJECTS"}
          </Chip>
          <Chip>
            {threads.length} {threads.length === 1 ? "THREAD" : "THREADS"}
          </Chip>
          {working > 0 && <Chip tone="ok">{working} WORKING</Chip>}
          {needs > 0 && <Chip tone="warn">{needs} NEEDS YOU</Chip>}
        </div>
        {error && (
          <span className="max-w-48 truncate text-(--bad) text-[11px]" title={error}>
            {error}
          </span>
        )}
        <button className={ACCENT} onClick={newThread} type="button">
          <PlusIcon className="size-3" />
          New thread
        </button>
      </div>

      <div className="grid grid-cols-3 items-start gap-3">
        <BakeryCard project={snap.bakery} />
        <ProjectsCard ctx={ctx} />
        <Card count={threads.length} hue="var(--hue-threads)" icon={<MessagesSquareIcon className="size-3.5" />} title="Threads">
          {threads.map((c) => (
            <ThreadRow c={c} key={c.streamId} onOpen={() => open(c)} place />
          ))}
        </Card>
        <GitHubCard ctx={ctx} />
        <Containers streams={null} titleOf={titleOf} />
        <TrashCard />
        <AlwaysAllowed
          mine={() => true}
          where={(g) => (g.scope === "baker" ? "Baker" : g.scope === "project" ? (g.name ?? g.id ?? "") : (titleOf(g.id) ?? g.id ?? ""))}
        />
        <ToolkitCard project={snap.bakery} />
        <ProjectControlCards ctx={ctx} project={snap.bakery} />
      </div>
    </div>
  );
}

/** Every project in the Bakery, each opening on its control plane in its
 *  workspace; a Baker-side one has none. */
function ProjectsCard({ ctx }: { ctx: ToolContext }) {
  const { snap, layout, setLayout } = ctx;
  const show = (wsId: string) => setLayout((l) => L.showControl(l, wsId));
  return (
    <Card count={snap.projects.length} hue="var(--hue-project)" icon={<FolderIcon className="size-3.5" />} title="Projects" wide>
      <div className="grid grid-cols-[repeat(auto-fill,minmax(240px,1fr))] gap-2">
        {snap.projects.map((p) => {
          const i = layout.workspaces.findIndex((w) => w.projectId === p.id);
          const home = layout.workspaces[i];
          const mine = snap.conversations.filter((c) => p.streamIds.includes(c.streamId));
          const working = mine.filter((c) => c.status === "working").length;
          const needs = mine.filter((c) => c.status === "needs_input").length;
          const repos = new Set(p.checkouts.map((c) => c.repoRoot || c.path).filter(Boolean)).size;
          const body = (
            <>
              <span
                className="grid size-5 shrink-0 place-items-center rounded font-bold text-(--fg-on-solid) text-[10px]"
                style={{ background: home ? workspaceColor(i) : "var(--baker-solid)" }}
              >
                {(p.name[0] ?? "?").toUpperCase()}
              </span>
              <span className="flex min-w-0 flex-1 flex-col">
                <span className="truncate font-bold text-(--fg)">{p.name}</span>
                <span className="truncate text-(--fg-dim) text-[10.5px]">
                  {mine.length} {mine.length === 1 ? "thread" : "threads"} · {repos} {repos === 1 ? "repo" : "repos"}
                </span>
              </span>
            </>
          );
          return (
            <div className="group flex min-w-0 items-center gap-1.5 rounded-md border border-(--line-soft) bg-(--bg-app) px-2 py-1.5" key={p.id} title={p.id}>
              {home ? (
                <button className="flex min-w-0 flex-1 items-center gap-2 text-left hover:text-(--fg)" onClick={() => show(home.id)} type="button">
                  {body}
                </button>
              ) : (
                <span className="flex min-w-0 flex-1 items-center gap-2">{body}</span>
              )}
              {needs > 0 && <Chip tone="warn">{needs} NEEDS YOU</Chip>}
              {working > 0 && <Chip tone="ok">{working} WORKING</Chip>}
              {!home && <Chip>BAKER SIDE</Chip>}
              <DeleteMenu className={HOVER} label={p.name} target={{ kind: "project", project_id: p.id }} />
            </div>
          );
        })}
      </div>
    </Card>
  );
}

/** GitHub across the Bakery: the pull requests waiting on the operator and
 *  the runs failing, each opening its page here, as the review does. */
function GitHubCard({ ctx }: { ctx: ToolContext }) {
  const prs = usePrs();
  const runs = useRuns();
  const open = (id: string) => ctx.setLayout((l) => L.openPanel(l, ctx.workspace.id, id));
  const waiting = prs.data?.prs.filter((pr) => inPrGroup(pr, "waiting")) ?? [];
  const failing = runs.data?.runs.filter(isFailedRun) ?? [];
  const problem = [!prs.data && prs.error, !runs.data && runs.error].filter(Boolean).join(" · ");
  const page = (id: string, icon: ReactNode, label: string) => (
    <button className={`${BUTTON} h-5 px-1.5`} onClick={() => open(id)} type="button">
      {icon}
      {label}
    </button>
  );
  return (
    <Card
      action={
        <span className="flex items-center gap-1.5 font-normal normal-case tracking-normal">
          {page("@review", <GitMergeIcon className="size-3 text-(--violet)" />, "Review & merge")}
          {page("@prs", <GitPullRequestIcon className="size-3 text-(--violet)" />, "Pull requests")}
          {page("@actions", <CircleCheckIcon className="size-3 text-(--violet)" />, "Actions")}
          <Freshness
            at={prs.at}
            busy={prs.busy || runs.busy}
            error={prs.error || runs.error}
            onReload={() => {
              prs.reload();
              runs.reload();
            }}
          />
        </span>
      }
      count={problem ? undefined : waiting.length + failing.length}
      hue="var(--hue-github)"
      icon={<GitPullRequestIcon className="size-3.5" />}
      title="GitHub"
      wide
    >
      {problem && (
        <p className="px-1.5 text-(--warn) text-[11px]" title={problem}>
          {problem}
        </p>
      )}
      {waiting.map((pr) => (
        <button className={`${ROW} hover:bg-(--bg-chip)`} key={`${pr.repository}#${pr.number}`} onClick={() => open("@prs")} title={pr.url} type="button">
          <GitPullRequestIcon className="size-3 shrink-0 text-(--violet)" />
          <span className="min-w-0 flex-1 truncate text-(--fg-body)">{pr.title}</span>
          <span className="shrink-0 font-mono text-(--fg-dim) text-[10.5px]">
            {pr.repository}#{pr.number}
          </span>
          <Chip tone="warn">WAITING ON YOU</Chip>
        </button>
      ))}
      {failing.map((r) => (
        <button className={`${ROW} hover:bg-(--bg-chip)`} key={`${r.repository}:${r.run_id}`} onClick={() => open("@actions")} title={r.url} type="button">
          <CircleXIcon className="size-3 shrink-0 text-(--bad)" />
          <span className="min-w-0 flex-1 truncate text-(--fg-body)">
            {r.workflow} · {r.branch}
          </span>
          <span className="shrink-0 font-mono text-(--fg-dim) text-[10.5px]">{r.repository}</span>
          <Chip tone="bad">{r.conclusion.toUpperCase()}</Chip>
        </button>
      ))}
    </Card>
  );
}

/** The containers that serve a project's threads (their sandboxes and
 *  Code), or with no `streams` every one on this Mac's engines. */
function Containers({ streams, titleOf }: { streams: ReadonlySet<string> | null; titleOf: (id: string) => string | undefined }) {
  // Every engine's containers as last read, at once; then every 15 s.
  const { data, reload } = useCached<ContainerRows>("containers:rows", readContainerRows);
  const load = useCallback(() => void reload(), [reload]);
  usePoll(load, 15_000);
  const mine = (data ?? [])
    .filter(({ c }) => !streams || (c.stream_id && streams.has(c.stream_id)))
    .sort((a, b) => Number(b.c.running) - Number(a.c.running));
  return (
    <Card count={mine.length} hue="var(--hue-containers)" icon={<BoxIcon className="size-3.5" />} title="Containers">
      {mine.map(({ backend, c }) => (
        <ContainerLine
          backend={backend}
          c={c}
          key={`${backend}:${c.id}`}
          onChanged={load}
          tag={c.role === "editor" ? <Chip tone="teal">Code</Chip> : <Chip>{titleOf(c.stream_id ?? "") ?? c.role ?? backend}</Chip>}
        />
      ))}
    </Card>
  );
}

/** What agents do without asking, each where it holds (`where`): a
 *  project's own and its threads', or on the Bakery's every one. */
function AlwaysAllowed({ mine, where }: { mine: (g: GrantView) => boolean; where: (g: GrantView) => string | undefined }) {
  const [grants, setGrants] = useState<GrantView[]>([]);
  const [error, setError] = useState("");
  useEffect(() => {
    agentsAlwaysAllowed().then(setGrants, (e) => setError(said(e)));
  }, []);
  const shown = grants.filter(mine);
  return (
    <Card count={shown.length} hue="var(--warn)" icon={<ShieldCheckIcon className="size-3.5" />} title="Always allow">
      {shown.map((g) => {
        const words = KIND_WORDS[g.kind] ?? g.kind;
        const at = where(g);
        return (
          <div className={ROW} key={`${g.scope}:${g.id}:${g.kind}`}>
            <span className="shrink-0 text-(--warn)">{KIND_ICONS[g.kind] ?? <ShieldCheckIcon className="size-3" />}</span>
            <span className="min-w-0 flex-1 truncate text-(--fg-body)">{words}</span>
            {at && <span className="min-w-0 truncate text-(--fg-dim) text-[10.5px]">{at}</span>}
            <button
              aria-label={`Ask again before ${words}`}
              className="grid size-5 shrink-0 place-items-center rounded text-(--fg-dim) hover:bg-(--bg-chip) hover:text-(--bad)"
              onClick={() =>
                void agentsAlwaysAllowedRemove({ scope: g.scope, id: g.id, kind: g.kind }).then(setGrants, (e) => setError(said(e)))
              }
              title="Ask again"
              type="button"
            >
              <XIcon className="size-3" />
            </button>
          </div>
        );
      })}
      {error && <p className="px-1.5 text-(--bad) text-[11px]">{error}</p>}
    </Card>
  );
}

/** One job at a time, named by `key` (a repository, or "folder"), with its error. */
function useJob(done: () => void) {
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState("");
  const run = (key: string, job: () => Promise<unknown>) => {
    setBusy(key);
    setError("");
    job()
      .then(done, (e) => setError(said(e)))
      .finally(() => setBusy(null));
  };
  return { busy, error, run };
}

/** The operator's GitHub repositories, searchable, each with what it can do
 *  here; `side` sits beside the search (the folder picker). */
function GitHubRepos({ action, side }: { action: (r: Repo) => ReactNode; side: ReactNode }) {
  const [repos, setRepos] = useState<Repo[] | null>(null);
  const [problem, setProblem] = useState("");
  const [query, setQuery] = useState("");
  useEffect(() => {
    githubRepos({}).then(
      (r) => {
        setRepos(r.repos.filter((x) => !x.archived));
        if (r.problems.length) setProblem(`GitHub could not list ${r.problems.join(", ")}`);
      },
      (e) => setProblem(said(e)),
    );
  }, []);
  const q = query.trim().toLowerCase();
  const shown = (repos ?? []).filter((r) => !q || r.repository.toLowerCase().includes(q));
  return (
    <>
      <div className="flex items-center gap-2">
        <label className="flex h-7 min-w-0 flex-1 items-center gap-2 rounded-md border border-(--line) bg-(--bg-app) px-2 text-(--fg-dim)">
          <SearchIcon className="size-3 shrink-0" />
          <input
            aria-label="Search your GitHub repositories"
            autoFocus
            className="min-w-0 flex-1 bg-transparent text-(--fg-body) text-xs outline-none"
            onChange={(e) => setQuery(e.target.value)}
            value={query}
          />
        </label>
        {side}
      </div>
      <div className="max-h-80 overflow-auto">
        {repos === null && !problem && <p className="p-1.5 text-(--fg-faint)">Reading GitHub…</p>}
        {shown.map((r) => (
          <div
            className="flex h-9 min-w-0 items-center gap-2 border-(--line-soft) border-b px-1 last:border-b-0"
            key={r.repository}
            title={r.description || r.repository}
          >
            <span className="min-w-0 flex-1 truncate text-(--fg-body)">{r.repository}</span>
            <Chip>{r.visibility.toUpperCase()}</Chip>
            {r.cloned_at && <span className="max-w-[30%] shrink truncate text-(--fg-dim) text-[10.5px]">{tilde(r.cloned_at)}</span>}
            {action(r)}
          </div>
        ))}
      </div>
      {problem && <p className="px-0.5 text-(--warn) text-[11px]">{problem}</p>}
    </>
  );
}

function FolderButton({ disabled, onClick }: { disabled: boolean; onClick: () => void }) {
  return (
    <button className={BUTTON} disabled={disabled} onClick={onClick} type="button">
      <FolderIcon className="size-3 text-(--hue-project)" />
      Folder on this Mac…
    </button>
  );
}

/** One more repo for an existing project: a GitHub one (added if it is on
 *  this Mac, cloned into the project if not) or any folder. */
function AddRepo({ project, projects, store }: { project: ShellProject; projects: readonly ShellProject[]; store: ToolContext["store"] }) {
  const { busy, error, run } = useJob(() => store.refresh());
  const attach = (path: string) => socketOpen({ project_id: project.id, name: basename(path), root: { mode: "attach", path } });
  return (
    <div className="flex flex-col gap-1.5 p-1.5">
      <GitHubRepos
        action={(r) => {
          const owner = rootOf(projects, r.cloned_at);
          if (owner) return owner.id === project.id ? <Chip tone="ok">✓ ADDED</Chip> : inProject(owner);
          return (
            <button
              className={BUTTON}
              disabled={busy !== null}
              onClick={() =>
                run(r.repository, () =>
                  r.cloned_at ? attach(r.cloned_at) : githubClone({ repository: r.repository, project_id: project.id }),
                )
              }
              type="button"
            >
              {busy === r.repository ? (r.cloned_at ? "Adding…" : "Cloning…") : r.cloned_at ? "Add" : "Clone into this project"}
            </button>
          );
        }}
        side={
          <FolderButton
            disabled={busy !== null}
            onClick={() =>
              run("folder", async () => {
                const path = await pickFolder(`A repo for ${project.name}`);
                if (path) await attach(path);
              })
            }
          />
        }
      />
      {error && <p className="px-0.5 text-(--bad) text-[11px]">{error}</p>}
    </div>
  );
}

/** A workspace with no project yet: name it, then its first repo makes the
 *  project (from GitHub, cloned if need be, or from a folder). */
function NewWorkspace({ ctx }: { ctx: ToolContext }) {
  const { workspace: ws, setLayout, store } = ctx;
  const [name, setName] = useState(ws.name);
  const named = name.trim() || ws.name;
  const { busy, error, run } = useJob(() => store.refresh());
  /** This workspace becomes the project's home. Asked again first, so no
   *  answer from before the project was made can land after and drop it. */
  const bind = (projectId: string) => {
    store.refresh();
    setLayout((l) => L.assignProject(l, ws.id, projectId, named));
  };
  const fromPath = async (path: string) => bind((await projectCreate({ name: named, root: { mode: "attach", path } })).project.project_id);
  const cloned = async (r: Repo) => {
    const made = (await githubClone({ repository: r.repository })).project;
    bind(made.project_id);
    if (made.name !== named) await projectRename({ project_id: made.project_id, name: named });
  };

  return (
    <div className="h-full overflow-auto bg-(--bg-app) p-4 text-xs" data-testid="control-plane-new" style={DOTS}>
      <div className="mb-3 flex items-center gap-3 rounded-[10px] border border-(--line) bg-(image:--grad-chrome) px-3.5 py-3">
        <span
          className="grid size-[30px] shrink-0 place-items-center rounded-[7px] font-bold text-(--fg-on-solid) text-[15px]"
          style={{ background: ctx.color }}
        >
          {(named[0] ?? "?").toUpperCase()}
        </span>
        <input
          aria-label="Workspace name"
          className="h-7 w-80 min-w-0 rounded-md border border-(--accent) bg-(--bg-app) px-2.5 font-bold text-(--fg) text-[15px] shadow-[0_0_0_2px_color-mix(in_srgb,var(--accent)_20%,transparent)] outline-none"
          onBlur={() => setLayout((l) => L.renameWorkspace(l, ws.id, named))}
          onChange={(e) => setName(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && e.currentTarget.blur()}
          value={name}
        />
      </div>
      <div className="max-w-[760px]">
        <Card hue="var(--hue-project)" icon={<FolderIcon className="size-3.5" />} title="Add its first repo">
          <GitHubRepos
            action={(r) => {
              const owner = rootOf(ctx.snap.projects, r.cloned_at);
              if (owner) return inProject(owner);
              return (
                <button
                  className={BUTTON}
                  disabled={busy !== null}
                  onClick={() => run(r.repository, () => (r.cloned_at ? fromPath(r.cloned_at) : cloned(r)))}
                  type="button"
                >
                  {busy === r.repository ? (r.cloned_at ? "Adding…" : "Cloning…") : r.cloned_at ? "Add" : "Clone and add"}
                </button>
              );
            }}
            side={
              <FolderButton
                disabled={busy !== null}
                onClick={() =>
                  run("folder", async () => {
                    const path = await pickFolder(`The first repo of ${named}`);
                    if (path) await fromPath(path);
                  })
                }
              />
            }
          />
          {error && <p className="px-0.5 text-(--bad) text-[11px]">{error}</p>}
        </Card>
      </div>
    </div>
  );
}
