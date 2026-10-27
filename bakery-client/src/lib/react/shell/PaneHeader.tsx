// The buttons at the end of a thread's pane header, as the operator's 02
// reference has them: lock the tab open (a lock, Round 10), the worktree's past threads, and a
// new agent of the same project. The shell provides what they act on.

import { createContext, useCallback, useContext, useEffect, useRef, useState } from "react";
import {
  ArrowUpFromLineIcon,
  CloudIcon,
  BookmarkIcon,
  BookOpenIcon,
  CheckIcon,
  ChevronDownIcon,
  CirclePlusIcon,
  GitBranchIcon,
  HistoryIcon,
  LoaderCircleIcon,
  LockIcon,
  LockOpenIcon,
  SmartphoneIcon,
  TerminalIcon,
  XIcon,
} from "lucide-react";
import { agents, grants, grantsGive, grantsRevoke, type PushGrant, type Reach, threadIphone, threadIphoneInstall, threadIphoneRun } from "$lib/api.generated";
import { ask } from "$lib/ask";
import { statusLabel, type Conversation } from "$lib/conversations";
import { isTauri } from "$lib/desktop";
import { onEnvelope, onEvent } from "$lib/events";
import { ThreadNamesContext } from "../AgentPanel";
import { FinishCard } from "./FinishCard";
import { AddAgentForm, type NewAgent, POP, useDismiss } from "./PanelMenu";
import { useRollup } from "./rollup";
import { ThreadMenu } from "./ThreadMenu";
import { type AgentPlace, overlapText } from "./sidebarModel";
import { Chip, said, StatusDot } from "./ui";

/** What a thread's header says of its checkout. */
export type PaneGit = {
  /** The branch it is on; a checkout in place (a Baker thread's) too. */
  branch: string;
  /** The project whose rollup counts a worktree against the branch it
   *  forked from; null for a checkout in place. */
  project: string | null;
  /** Its agent is working: Update waits. */
  working: boolean;
  /** Merges the branch it forked from into the worktree. */
  update(): void;
};

export type PaneActions = {
  locked(wsId: string, streamId: string): boolean;
  toggleLock(wsId: string, streamId: string): void;
  /** The other threads on this thread's worktree, live and archived, newest first. */
  past(streamId: string): readonly Conversation[];
  /** Opens a thread here, restoring it first when it was archived. */
  enter(wsId: string, c: Conversation): void;
  /** How a new thread starts beside this one: a new Baker thread, or an
   *  agent of this thread's project (where it can work); null for neither. */
  starts(streamId: string): "baker" | AgentPlace | null;
  /** Starts it: a Baker thread when `agent` is null. */
  start(wsId: string, streamId: string, agent: NewAgent | null): Promise<void>;
  /** What this thread's agent started (subagents, tasks); null while the
   *  map does not show its agent. */
  started(streamId: string): { count: number; working: boolean } | null;
  /** Opens the workspace's agent map, this thread first. */
  openMap(wsId: string, streamId: string): void;
  /** What a finished thread's card reads, or null while it shows none. */
  finishFor(streamId: string): { projectId: string; branch: string; fallbackBase: string } | null;
  /** Opens Merge review in this workspace on this thread's branch. */
  reviewFrom(wsId: string, streamId: string): void;
  /** The branch of the worktree a thread works in, or null in place. */
  branchOf(streamId: string): string | null;
  /** The checkout a thread works in, as its header shows it; null for a
   *  thread with none. */
  git(streamId: string): PaneGit | null;
  /** Brings the terminal panel forward on a thread's terminals. */
  showTerminals(wsId: string, streamId: string, terminalIds: string[]): void;
};

export const PaneContext = createContext<PaneActions | null>(null);

const ICON = "grid size-6 shrink-0 place-items-center rounded hover:bg-(--bg-chip) disabled:opacity-40";

/** "now", "5m", "3h", "2d" since a time. */
function ago(iso: string): string {
  const minutes = Math.round((Date.now() - Date.parse(iso)) / 60_000);
  if (!Number.isFinite(minutes)) return "";
  if (minutes < 1) return "now";
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.round(minutes / 60);
  return hours < 48 ? `${hours}h` : `${Math.round(hours / 24)}d`;
}

/** A finished thread's card, last in its transcript. */
export function FinishSlot({ wsId, streamId }: { wsId: string; streamId: string }) {
  const ctx = useContext(PaneContext);
  const finish = ctx?.finishFor(streamId);
  if (!ctx || !finish) return null;
  return <FinishCard {...finish} onReview={() => ctx.reviewFrom(wsId, streamId)} streamId={streamId} />;
}

/** The thread's branch, right after its name in the pane header; and only
 *  when they apply, how many commits the worktree is behind the branch it
 *  forked from, with Update, and other threads changing the same files
 *  (git rules 3 and 5). */
export function PaneBranch({ streamId }: { streamId: string }) {
  const ctx = useContext(PaneContext);
  const names = useContext(ThreadNamesContext);
  const git = ctx?.git(streamId) ?? null;
  const rollup = useRollup(git?.project ?? null);
  if (!git) return null;
  const tree = git.project ? rollup.data?.worktrees.find((w) => w.stream_id === streamId) : undefined;
  const overlap = tree ? overlapText(tree.also_changing, (id) => names(id) ?? id) : "";
  return (
    <>
      {git.branch && (
        <span
          className="flex h-5 max-w-80 min-w-0 items-center gap-1 rounded border border-(--line) bg-(--bg-chip) px-1.5 text-(--fg-body) text-[10.5px]"
          data-testid="pane-branch"
          title={git.branch}
        >
          <GitBranchIcon className="size-3 shrink-0 text-(--violet)" />
          <span className="truncate">{git.branch}</span>
        </span>
      )}
      {tree && tree.behind > 0 && (
        <span className="flex shrink-0 items-center gap-1.5 text-(--warn) text-[11px]" data-testid="pane-behind">
          {tree.behind} behind
          <span aria-hidden className="text-(--fg-ghost)">
            ·
          </span>
          <button
            className="rounded px-1 text-(--link) hover:bg-(--bg-chip) disabled:opacity-40"
            disabled={git.working}
            onClick={git.update}
            title={git.working ? "The agent is working here" : "Merge the branch it forked from into it"}
            type="button"
          >
            Update
          </button>
        </span>
      )}
      {overlap && (
        <span className="min-w-0 truncate text-(--warn) text-[11px]" title={overlap}>
          {overlap}
        </span>
      )}
    </>
  );
}

/** "● N agents" in the composer: what this thread's agent started, green
 *  while any of it works. Opens the workspace's agent map. */
export function StartedPill({ wsId, streamId }: { wsId: string; streamId: string }) {
  const ctx = useContext(PaneContext);
  const started = ctx?.started(streamId);
  if (!ctx || !started) return null;
  return (
    <button
      className={`flex h-[22px] shrink-0 items-center gap-1.5 rounded-[5px] border px-2 text-[11px] ${started.working ? "border-(--chip-ok-bg) bg-(--chip-ok-bg)/35 text-(--chip-ok-fg)" : "border-(--line) bg-(--bg-chip) text-(--fg-body)"} hover:text-(--fg)`}
      data-testid="started-pill"
      onClick={() => ctx.openMap(wsId, streamId)}
      title="Agent map"
      type="button"
    >
      <span className={`size-1.5 rounded-full ${started.working ? "bg-(--ok)" : "bg-(--idle)"}`} />
      {started.count} {started.count === 1 ? "agent" : "agents"}
    </button>
  );
}

export function PaneHeaderActions({
  wsId,
  streamId,
  saved,
  brief,
}: {
  wsId: string;
  streamId: string;
  /** The thread's Saved panel (Round 10), while the node keeps saved replies. */
  saved?: { open: boolean; count: number; toggle: () => void };
  /** The thread's brief under the header (Round 11): shown, and whether it has one. */
  brief?: { open: boolean; has: boolean; toggle: () => void };
}) {
  const ctx = useContext(PaneContext);
  const [open, setOpen] = useState<"past" | "new" | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const box = useRef<HTMLDivElement>(null);
  useDismiss(open !== null, box, () => setOpen(null));
  if (!ctx) return null;

  const locked = ctx.locked(wsId, streamId);
  const past = ctx.past(streamId);
  const starts = ctx.starts(streamId);
  const startBaker = () => {
    setBusy(true);
    setError("");
    ctx.start(wsId, streamId, null).then(
      () => setBusy(false),
      (e) => {
        setBusy(false);
        setError(said(e));
      },
    );
  };

  return (
    <div className="relative flex items-center gap-0.5" ref={box}>
      <PushGrants branch={ctx.branchOf(streamId)} streamId={streamId} />
      <IphoneInstall streamId={streamId} wsId={wsId} />
      {brief && (
        <button
          aria-label="Brief"
          aria-pressed={brief.open}
          className={`${ICON} ${brief.open ? "bg-(--bg-pressed) text-(--violet)" : brief.has ? "text-(--violet)" : "text-(--fg-dim)"}`}
          onClick={brief.toggle}
          title="Brief"
          type="button"
        >
          <BookOpenIcon className="size-3.5" />
        </button>
      )}
      {saved && (
        <button
          aria-label="Saved"
          aria-pressed={saved.open}
          className={`${ICON} ${saved.open ? "bg-(--bg-pressed) text-(--accent-fg)" : "text-(--accent)"}`}
          onClick={saved.toggle}
          title={saved.count > 0 ? `Saved · ${saved.count}` : "Saved"}
          type="button"
        >
          <BookmarkIcon className={`size-3.5 ${saved.count > 0 ? "fill-current" : ""}`} />
        </button>
      )}
      <button
        aria-label={locked ? "Unlock" : "Lock"}
        aria-pressed={locked}
        className={`${ICON} ${locked ? "bg-(--bg-pressed) text-(--accent-fg)" : "text-(--hue-project)"}`}
        onClick={() => ctx.toggleLock(wsId, streamId)}
        title={locked ? "Unlock" : "Lock"}
        type="button"
      >
        {locked ? <LockIcon className="size-3.5" /> : <LockOpenIcon className="size-3.5" />}
      </button>
      <button
        aria-expanded={open === "past"}
        aria-label="Past threads"
        className={`${ICON} text-(--link)`}
        disabled={past.length === 0}
        onClick={() => setOpen((o) => (o === "past" ? null : "past"))}
        title="Past threads"
        type="button"
      >
        <HistoryIcon className="size-3.5" />
      </button>
      {starts && (
        <button
          aria-expanded={open === "new"}
          aria-label={starts === "baker" ? "New thread" : "Add agent"}
          className={`${ICON} text-(--ok)`}
          disabled={busy}
          onClick={() => (starts === "baker" ? startBaker() : setOpen((o) => (o === "new" ? null : "new")))}
          title={starts === "baker" ? "New thread" : "Add agent"}
          type="button"
        >
          <CirclePlusIcon className="size-3.5" />
        </button>
      )}
      {open === "past" && (
        <div className={`${POP} w-72`} role="menu">
          {past.map((c) => (
            <button
              className="flex h-7 w-full items-center gap-2 rounded px-2 text-left hover:bg-(--bg-chip)"
              key={c.streamId}
              onClick={() => {
                setOpen(null);
                ctx.enter(wsId, c);
              }}
              role="menuitem"
              type="button"
            >
              <StatusDot status={c.status} streamId={c.streamId} title={statusLabel(c.status)} />
              <span className="min-w-0 flex-1 truncate text-(--fg-body)">{c.title}</span>
              {c.archived && <Chip>archived</Chip>}
              <span className="shrink-0 text-(--fg-dim) text-[10.5px]">{ago(c.lastTs)}</span>
            </button>
          ))}
        </div>
      )}
      {open === "new" && starts && starts !== "baker" && (
        <div aria-label="Add agent" className={`${POP} w-80`} role="dialog">
          <AddAgentForm create={(agent) => ctx.start(wsId, streamId, agent)} onDone={() => setOpen(null)} place={starts} />
        </div>
      )}
      {error && (
        <span className="max-w-48 truncate text-(--bad) text-[11px]" title={error}>
          {error}
        </span>
      )}
      <ThreadMenu streamId={streamId} />
    </div>
  );
}

/** Where each thread's agent runs, by its place on the scope dial (absent:
 *  a room on this Mac); read once for the window, again when it's back in
 *  front. */
let reaches: Promise<Map<string, Reach>> | null = null;
const reachOf = (streamId: string) => {
  reaches ??= agents().then(
    (v) => new Map(v.agents.map((a) => [a.stream_id, a.reach])),
    () => new Map<string, Reach>(),
  );
  return reaches.then((m) => m.get(streamId) ?? "host_room");
};

type IphonePanel =
  | { kind: "menu" }
  | { kind: "progress"; steps: string[] }
  | { kind: "result"; bad: boolean; text: string; devices: string[]; again: (device: string) => void }
  | null;
const RUN_WARNING = "This runs the project's build code as you, with your keychain.";

/** Build to my iPhone (worker-2's): a thread on this Mac with an iOS build
 *  gets it signed with the operator's own certificate and installed on
 *  their iPhone, each step showing as the node takes it. Beside it, Run on
 *  my iPhone (as me): after its warning, `flutter run` typed into the
 *  operator's own terminal, brought forward, waiting for their Enter. Shown
 *  only while there's a build to install, and never on the phone; the
 *  result, or the node's refusal, opens under the button, with a button per
 *  iPhone when several are paired. */
function IphoneInstall({ wsId, streamId }: { wsId: string; streamId: string }) {
  const ctx = useContext(PaneContext);
  const [shown, setShown] = useState(false);
  const [busy, setBusy] = useState(false);
  const [panel, setPanel] = useState<IphonePanel>(null);
  const box = useRef<HTMLDivElement>(null);
  useDismiss(panel !== null, box, () => setPanel(null));
  useEffect(() => {
    if (isTauri()) return;
    let live = true;
    const check = () => {
      void reachOf(streamId)
        .then((r) => (r === "host_room" || r === "host_full" ? threadIphone({ stream_id: streamId }).then((b) => !!b.build, () => false) : false))
        .then((has) => live && setShown(has));
    };
    const again = () => {
      reaches = null;
      check();
    };
    check();
    window.addEventListener("focus", again);
    // A turn that ends may have built it.
    const off = onEvent(streamId, (ev) => {
      if (ev.family === "agent.turn.done") check();
    });
    return () => {
      live = false;
      window.removeEventListener("focus", again);
      off();
    };
  }, [streamId]);
  // Each step of an install, as the node takes it.
  useEffect(() => {
    if (!busy) return;
    return onEnvelope("iphone-progress", (e) => {
      const step = e.step;
      if (e.stream_id !== streamId || typeof step !== "string") return;
      setPanel((p) => (p?.kind === "progress" ? { kind: "progress", steps: [...p.steps, step] } : p));
    });
  }, [busy, streamId]);
  if (!shown) return null;

  /** The node's refusal; when several iPhones are paired ("say which: A,
   *  B."), a button for each. */
  const refused = (e: unknown, again: (device: string) => void) => {
    const text = said(e);
    const several = /say which: (.+?)\.?$/.exec(text);
    setPanel({ kind: "result", bad: true, text, devices: several ? several[1].split(", ") : [], again });
  };
  const install = (device: string | null) => {
    setBusy(true);
    setPanel({ kind: "progress", steps: [] });
    threadIphoneInstall({ stream_id: streamId, device })
      .then(
        (r) => {
          const when = ago(r.built);
          setPanel({ kind: "result", bad: false, text: `On ${r.device}: ${r.bundle_id}, built ${when === "now" ? "just now" : `${when} ago`}, signed for ${r.profile}.`, devices: [], again: install });
        },
        (e) => refused(e, install),
      )
      .finally(() => setBusy(false));
  };
  /** Its warning first; a device picked after a refusal was already warned. */
  const run = async (device: string | null) => {
    setPanel(null);
    if (device === null && !(await ask(`Run on my iPhone (as me)?\n\n${RUN_WARNING}`))) return;
    setBusy(true);
    threadIphoneRun({ stream_id: streamId, device })
      .then(
        (r) => {
          ctx?.showTerminals(wsId, streamId, [r.terminal_id]);
          setPanel({ kind: "result", bad: false, text: `In your terminal for ${r.device}, waiting for your Enter: ${r.command}`, devices: [], again: (d) => void run(d) });
        },
        (e) => refused(e, (d) => void run(d)),
      )
      .finally(() => setBusy(false));
  };
  const item = "flex h-7 w-full items-center gap-2 rounded px-2 text-left text-(--fg-body) hover:bg-(--bg-chip)";
  return (
    <div className="relative flex items-center" ref={box}>
      <button
        aria-label="Build to my iPhone"
        className={`${ICON} text-(--link)`}
        disabled={busy}
        onClick={() => install(null)}
        title={busy ? "Working on your iPhone…" : "Build to my iPhone"}
        type="button"
      >
        {busy ? <LoaderCircleIcon className="size-3.5 motion-safe:animate-spin" /> : <SmartphoneIcon className="size-3.5" />}
      </button>
      <button
        aria-expanded={panel?.kind === "menu"}
        aria-label="More for my iPhone"
        className="grid h-6 w-3.5 place-items-center rounded text-(--fg-dim) hover:bg-(--bg-chip) hover:text-(--fg-2) disabled:opacity-40"
        disabled={busy}
        onClick={() => setPanel((p) => (p?.kind === "menu" ? null : { kind: "menu" }))}
        title="More for my iPhone"
        type="button"
      >
        <ChevronDownIcon className="size-3" />
      </button>
      {panel?.kind === "menu" && (
        <div className={`${POP} w-72 p-1`} role="menu">
          <button className={item} onClick={() => install(null)} role="menuitem" type="button">
            <SmartphoneIcon className="size-3.5 shrink-0 text-(--link)" />
            Build to my iPhone
          </button>
          <button className={item} onClick={() => void run(null)} role="menuitem" title={RUN_WARNING} type="button">
            <TerminalIcon className="size-3.5 shrink-0 text-(--warn)" />
            Run on my iPhone (as me)
          </button>
        </div>
      )}
      {panel?.kind === "progress" && (
        <div className={`${POP} w-80 p-2.5`} data-testid="iphone-progress" role="status">
          {panel.steps.length === 0 && <p className="text-(--fg-dim) text-[11.5px]">Starting…</p>}
          {panel.steps.map((s, i) => (
            <p className="flex items-center gap-1.5 text-[11.5px] leading-relaxed" key={`${i}:${s}`}>
              {i === panel.steps.length - 1 ? (
                <LoaderCircleIcon className="size-3 shrink-0 text-(--link) motion-safe:animate-spin" />
              ) : (
                <CheckIcon className="size-3 shrink-0 text-(--ok)" />
              )}
              <span className={i === panel.steps.length - 1 ? "text-(--fg)" : "text-(--fg-dim)"}>{s}</span>
            </p>
          ))}
        </div>
      )}
      {panel?.kind === "result" && (
        <div className={`${POP} w-80 p-2.5`} data-testid="iphone-result" role="status">
          <p className={`break-words text-[11.5px] leading-snug ${panel.bad ? "text-(--bad)" : "text-(--ok)"}`}>{panel.text}</p>
          {panel.devices.length > 0 && (
            <div className="mt-2 flex flex-wrap gap-1.5">
              {panel.devices.map((d) => (
                <button
                  className="flex h-6 items-center gap-1.5 rounded-[5px] border border-(--line) bg-(image:--grad-button) px-2 text-(--fg-body) text-[11px] hover:text-(--fg)"
                  key={d}
                  onClick={() => panel.again(d)}
                  type="button"
                >
                  <SmartphoneIcon className="size-3" />
                  {d}
                </button>
              ))}
            </div>
          )}
        </div>
      )}
    </div>
  );
}

/** The conversation's push grants (the operator's "push for me"): each a chip
 *  naming its branch, taken back with ×; on a branch of its own with none
 *  yet, a way to give one. Giving and taking back stay off the phone until
 *  the operator says otherwise; the node refuses there too. */
function PushGrants({ streamId, branch }: { streamId: string; branch: string | null }) {
  const [list, setList] = useState<PushGrant[]>([]);
  const [error, setError] = useState("");
  const load = useCallback(() => {
    grants({ stream_id: streamId }).then(setList, () => setList([]));
  }, [streamId]);
  useEffect(() => {
    load();
    return onEvent(streamId, (ev) => {
      if (ev.family.startsWith("git.grant.")) load();
    });
  }, [streamId, load]);
  const phone = isTauri();
  const done = (left: PushGrant[]) => {
    setList(left);
    setError("");
  };
  return (
    <>
      {list.map((g) => (
        <span
          className="flex h-5 shrink-0 items-center gap-1 rounded-[5px] border border-(--ok-border) bg-(--ok-bg) pr-0.5 pl-1.5 text-(--ok) text-[10.5px]"
          key={g.grant_id}
          title={
            g.kind === "google"
              ? `Changes ${g.repository} in Google Cloud without asking, never deleting`
              : `Pushes ${g.branch} to ${g.repository} without asking, never forced`
          }
        >
          {g.kind === "google" ? <CloudIcon className="size-3" /> : <ArrowUpFromLineIcon className="size-3" />}
          {g.kind === "google" ? g.repository : g.branch}
          {!phone && (
            <button
              aria-label={g.kind === "google" ? `Take back changing ${g.repository} in Google Cloud` : `Take back pushing ${g.branch}`}
              className="grid size-4 place-items-center rounded hover:bg-(--bg-chip)"
              onClick={() => grantsRevoke({ stream_id: streamId, grant_id: g.grant_id }).then(done, (e) => setError(said(e)))}
              title="Take it back"
              type="button"
            >
              <XIcon className="size-2.5" />
            </button>
          )}
        </span>
      ))}
      {!list.some((g) => g.kind !== "google") && branch && !phone && (
        <button
          aria-label={`Let this conversation push ${branch} without asking`}
          className={`${ICON} text-(--ok)`}
          onClick={() => grantsGive({ stream_id: streamId, kind: "push" }).then((g) => done([g]), (e) => setError(said(e)))}
          title={`Let this conversation push ${branch} without asking`}
          type="button"
        >
          <ArrowUpFromLineIcon className="size-3.5" />
        </button>
      )}
      {error && (
        <span className="max-w-48 truncate text-(--bad) text-[11px]" title={error}>
          {error}
        </span>
      )}
    </>
  );
}
