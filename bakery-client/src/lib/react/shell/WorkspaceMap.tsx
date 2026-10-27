// One workspace's agent map, as the operator's 04 reference has it: each of
// its threads with tree lines to what it started (subagents, tasks), then
// the jobs still running. Opened from a thread's "● N agents" pill.

import { useEffect, useState } from "react";
import { XIcon } from "lucide-react";
import { jobList, type JobView } from "$lib/api.generated";
import type { Conversation } from "$lib/conversations";
import type { AgentMapNode } from "$lib/node";
import { whileShown } from "$lib/pageShown";
import { elapsedText } from "../panelState";
import { agentOf, agentTree } from "./floorModel";
import { StatusDot } from "./ui";

const WORD: Record<string, string> = {
  working: "working",
  needs_input: "needs you",
  failed: "failed",
  done: "done",
  idle: "idle",
  stopped: "stopped",
};

const LINE = "bg-(--line-strong)";

function Card({
  title,
  line,
  status,
  streamId,
  main = false,
  shown = false,
  tip,
  onClick,
}: {
  title: string;
  line: string;
  status: string;
  /** The conversation it is, so its glyph shows its live phase. */
  streamId?: string;
  main?: boolean;
  shown?: boolean;
  tip?: string;
  onClick?: () => void;
}) {
  return (
    <button
      className={`flex w-full min-w-0 flex-col gap-0.5 rounded-md border px-3 py-2 text-left hover:border-(--accent-border) ${shown ? "border-(--accent-border) bg-(--accent-sel)" : "border-(--line) bg-(--bg-raised)"}`}
      disabled={!onClick}
      onClick={onClick}
      title={tip}
      type="button"
    >
      <span className="flex min-w-0 items-center gap-2">
        <StatusDot status={status} streamId={streamId} />
        <span className={`truncate ${main ? "font-bold text-(--fg)" : "text-(--fg-body)"}`}>{title}</span>
      </span>
      {line && <span className="truncate pl-4 text-(--fg-dim) text-[11px]">{line}</span>}
    </button>
  );
}

function childLine(n: AgentMapNode): string {
  const steps = n.steps > 0 ? `${n.steps} ${n.steps === 1 ? "step" : "steps"}` : "";
  return [n.kind === "subagent" ? "" : n.kind, n.detail, steps, WORD[n.status]].filter(Boolean).join(" · ");
}

export function WorkspaceMap({
  threads,
  nodes,
  onOpen,
  onJob,
  onClose,
}: {
  /** The workspace's threads, the one the map was opened from first. */
  threads: readonly Conversation[];
  nodes: readonly AgentMapNode[];
  /** Shows a thread; for a subagent, its card in its thread too. */
  onOpen: (streamId: string, toolCallId: string | null) => void;
  /** Shows the thread that runs a job, with its terminal. */
  onJob: (streamId: string) => void;
  onClose: () => void;
}) {
  const rows = threads.map((c) => {
    const agent = agentOf(nodes, c.streamId, c.roleId);
    return { c, agent, kids: agent ? agentTree(nodes, agent.id) : [] };
  });
  const started = rows.reduce((n, r) => n + r.kids.length, 0);

  // The jobs still running, refreshed while the map is open.
  const key = threads.map((c) => `${c.streamId}\t${c.title}`).join("\n");
  const [jobs, setJobs] = useState<{ job: JobView; thread: string }[]>([]);
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    let live = true;
    const ids = key ? key.split("\n").map((line) => line.split("\t")) : [];
    const load = () =>
      Promise.all(
        ids.map(([streamId, title]) =>
          jobList({ stream_id: streamId }).then(
            (list) => list.filter((job) => job.state === "running").map((job) => ({ job, thread: title })),
            () => [],
          ),
        ),
      ).then((all) => {
        if (!live) return;
        setJobs(all.flat());
        setNow(Date.now());
      });
    void load();
    const stop = whileShown(load, 5000);
    return () => {
      live = false;
      stop();
    };
  }, [key]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [onClose]);

  return (
    <div
      className="fixed inset-0 z-50 grid place-items-center bg-black/45 p-6"
      onPointerDown={(e) => e.target === e.currentTarget && onClose()}
    >
      <div
        aria-label="Agent map"
        aria-modal="true"
        className="flex max-h-[84vh] w-[min(900px,94vw)] flex-col rounded-[10px] border border-(--line-strong) bg-(--bg-panel) text-xs shadow-(--shadow-menu)"
        data-testid="workspace-map"
        role="dialog"
      >
        <header className="flex items-center gap-2 px-6 pt-5">
          <h2 className="flex-1 font-bold text-(--fg) text-[16px]">Agent map</h2>
          <button
            aria-label="Close"
            className="grid size-7 place-items-center rounded text-(--fg-dim) hover:bg-(--bg-chip) hover:text-(--fg)"
            onClick={onClose}
            type="button"
          >
            <XIcon className="size-4" />
          </button>
        </header>
        <div className="min-h-0 overflow-auto px-6 pb-6">
          <p className="mt-2 mb-4 text-(--fg-dim) text-[12.5px]">
            {started} {started === 1 ? "agent" : "agents"}
          </p>
          <div className="flex flex-col gap-4">
            {rows.map(({ c, agent, kids }, row) => {
              // The trunk joins what the thread started itself; deeper ones hang indented.
              const lastTop = kids.reduce((last, k, i) => (k.depth === 0 ? i : last), -1);
              return (
                <div className="grid grid-cols-[minmax(0,1fr)_40px_minmax(0,1.1fr)] items-center" key={c.streamId}>
                  <Card
                    line={[agent?.detail, WORD[c.status] ?? c.status].filter(Boolean).join(" · ")}
                    main
                    onClick={() => onOpen(c.streamId, null)}
                    shown={row === 0}
                    status={c.status}
                    streamId={c.streamId}
                    tip={c.note || undefined}
                    title={c.title}
                  />
                  {kids.length > 0 && <span aria-hidden className={`h-px ${LINE}`} />}
                  {kids.length > 0 && (
                    <div className="flex flex-col gap-2">
                      {kids.map(({ node: n, depth }, i) => (
                        <div className="relative" key={n.id} style={{ paddingLeft: 16 + depth * 18 }}>
                          {i <= lastTop && (
                            <span
                              aria-hidden
                              className={`absolute left-0 w-px ${LINE}`}
                              style={{ top: i === 0 ? "50%" : 0, bottom: i === lastTop ? "50%" : -8 }}
                            />
                          )}
                          <span aria-hidden className={`absolute top-1/2 h-px w-3 ${LINE}`} style={{ left: depth * 18 }} />
                          <Card
                            line={childLine(n)}
                            onClick={() => onOpen(n.stream_id, n.kind === "subagent" ? n.tool_call_id : null)}
                            status={n.status}
                            streamId={n.kind === "subagent" ? undefined : n.stream_id}
                            tip={n.note ?? n.label}
                            title={n.label}
                          />
                        </div>
                      ))}
                    </div>
                  )}
                </div>
              );
            })}
          </div>
          {jobs.length > 0 && (
            <>
              <p className="mt-6 mb-3 text-(--fg-dim) text-[12.5px]">
                {jobs.length} {jobs.length === 1 ? "job" : "jobs"}
              </p>
              <div className="flex w-[min(460px,100%)] flex-col gap-2">
                {jobs.map(({ job, thread }) => (
                  <Card
                    key={job.id}
                    line={[thread, elapsedText(job.started_at, now)].filter(Boolean).join(" · ")}
                    onClick={() => onJob(job.stream_id)}
                    status="working"
                    tip={job.cwd}
                    title={job.command}
                  />
                ))}
              </div>
            </>
          )}
        </div>
      </div>
    </div>
  );
}
