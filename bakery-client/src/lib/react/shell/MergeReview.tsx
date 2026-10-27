// Merge review: the operator's last look before agents' work merges. Every
// agent branch of the project, finished ones first, each read against the
// branch it started from (`/git/diff` with that base): its files on the
// left, the picked file's blocks side by side on the right. "Ask" on any
// block puts the question to the agent that wrote it, on whatever runtime it
// runs, or to the Baker, and the answer shows under the block. Merge one
// branch, or every finished one in order; a conflict stops the run and
// changes nothing.

import { memo, useContext, useEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";
import { GitMergeIcon, MessageCircleQuestionIcon, RefreshCwIcon, XIcon } from "lucide-react";
import {
  agentTurnQueue,
  gitDiff,
  gitMerge,
  gitMergePreview,
  gitMergePreviewChain,
  gitPrChecks,
  gitProvenance,
  jobList,
  mergeWaiting,
  threadMerge,
  type HunkSource,
  type MergePreview,
  type MergeWaiting,
  type PrCheck,
  type Seat,
} from "$lib/api.generated";
import { ask } from "$lib/ask";
import { ago, statusLabel, type Conversation } from "$lib/conversations";
import { actorLabel } from "$lib/transcript";
import { ThreadNamesContext } from "../AgentPanel";
import type { AgentPanelSource } from "../bridge.svelte";
import { SplitDiff } from "../ToolRows";
import * as L from "./layout";
import { answerAfter, baseOf, candidatesOf, jobSummary, questionMarker } from "./reviewModel";
import { useRollup } from "./rollup";
import type { AttachedConversation, ShellCheckout, ShellStore } from "./shellSource.svelte";
import { Chip, said, StatusDot, type ToolContext, useSecretShown } from "./ui";
import { hunkText, parseUnifiedDiff, type FileDiff, type Hunk } from "./unifiedDiff";

/** A thread's worktree up for review: its checkout, its conversation, and
 *  the branch it is measured against. One its agent asked to merge (the
 *  Bakery's own, and a workspace's orc or worker anywhere) is `asked`: it
 *  merges where the node recorded it (`into`), and `said` is the ask. */
export type Candidate = ShellCheckout & { conv: Conversation | null; base: string; asked?: boolean; into?: string; said?: string | null };

/** The project select's value for the Bakery's own merges. */
const BAKERY = "";

/** Work waiting on the operator, as the node recorded it, for the project
 *  picked (`BAKERY` for the Bakery's own): a chef's or an orc's into main,
 *  a worker's into its workspace. */
function useWaiting(projectId: string, version: number): { items: MergeWaiting[] | null; error: string } {
  // Kept with the project it was read for: another project's list never
  // shows (or merges) while this one's is still on its way.
  const [got, setGot] = useState<{ of: string; items: MergeWaiting[] | null; error: string }>({ of: projectId, items: null, error: "" });
  useEffect(() => {
    let live = true;
    mergeWaiting().then(
      (r) => live && setGot({ of: projectId, items: r.items.filter((w) => (w.project_id ?? BAKERY) === projectId), error: "" }),
      (e) => live && setGot((g) => ({ of: projectId, items: g.of === projectId ? g.items : null, error: said(e) })),
    );
    return () => {
      live = false;
    };
  }, [projectId, version]);
  return got.of === projectId ? got : { items: null, error: "" };
}

/** Finished first, then what still needs someone, then the rest. */
const ORDER = ["done", "needs_input", "failed", "working", "idle"];

const pill =
  "flex h-6 shrink-0 items-center gap-1.5 rounded-[5px] border border-(--line) bg-(image:--grad-button) px-2.5 text-(--fg-body) text-[11px] hover:text-(--fg) disabled:opacity-40";

/** A branch the shell asked to show (a thread's finish card): taken at
 *  once by an open review, or by the next one to open. */
let wanted: string | null = null;
const wanting = new Set<() => void>();

export function reviewBranch(streamId: string) {
  wanted = streamId;
  for (const take of wanting) take();
}

const basename = (p: string) => p.split("/").pop() || p;
const dirname = (p: string) => p.split("/").slice(0, -1).join("/");

export function MergeReview({ ctx }: { ctx: ToolContext }) {
  const { snap } = ctx;
  // The Baker's side reviews the Bakery's own merges; a project's workspace, its project's.
  const bakerSide = ctx.workspace.id === L.BAKER || (!!ctx.workspace.orc && !ctx.workspace.projectId);
  const [projectId, setProjectId] = useState<string>(bakerSide ? BAKERY : (ctx.workspace.projectId ?? snap.projects[0]?.id ?? BAKERY));
  const bakery = projectId === BAKERY;
  const project = bakery ? null : (snap.projects.find((p) => p.id === projectId) ?? null);
  const base = project ? baseOf(project.checkouts) : null;
  const rollup = useRollup(bakery ? null : projectId);
  const [version, setVersion] = useState(0);
  const waiting = useWaiting(projectId, version);
  const candidates = useMemo((): Candidate[] => {
    const byId = new Map(snap.conversations.map((c) => [c.streamId, c]));
    // Each asked card is the branch as the node recorded it, measured against where it lands.
    const checkouts = [...snap.projects, snap.bakery].flatMap((p) => p.checkouts);
    const asked = (waiting.items ?? []).map((w): Candidate => {
      const co = checkouts.find((c) => c.streamId === w.stream_id);
      return {
        ...(co ?? { streamId: w.stream_id, inPlace: false, onDisk: true, path: "", repoRoot: "", added: w.adds, removed: w.dels, burn: false }),
        branch: w.branch,
        conv: byId.get(w.stream_id) ?? null,
        base: w.base,
        asked: true,
        into: w.orc ? `${byId.get(w.orc)?.title ?? "its workspace"} · ${w.base}` : w.base,
        said: w.note,
      };
    });
    if (bakery) return asked;
    if (!project || !base) return asked;
    // A workspace's orc and its workers merge only on their ask, where the node records it.
    const orcs = new Set(snap.conversations.filter((c) => c.workspace).map((c) => c.streamId));
    const inWorkspace = (sid: string) => {
      const c = byId.get(sid);
      return !!c && (c.workspace || (!!c.openedBy && orcs.has(c.openedBy)));
    };
    // The branch each started from, as the node recorded it; else the target.
    const started = new Map((rollup.data?.worktrees ?? []).map((w) => [w.stream_id, w.base ?? ""]));
    const rank = (c: Candidate) => {
      const i = ORDER.indexOf(c.conv?.status ?? "idle");
      return i < 0 ? ORDER.length : i;
    };
    const plain = candidatesOf(project.checkouts, base)
      .filter((c) => !inWorkspace(c.streamId))
      .map((c) => ({ ...c, conv: byId.get(c.streamId) ?? null, base: started.get(c.streamId) || base.branch }))
      .sort((a, b) => rank(a) - rank(b));
    return [...asked, ...plain];
  }, [bakery, waiting.items, project, base, snap.conversations, snap.projects, snap.bakery, rollup.data]);
  const [selected, setSelected] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState<{ bad: boolean; text: string } | null>(null);
  // An asked card is ready by its ask; any other, once its thread is done.
  const ready = candidates.filter((c) => c.asked || c.conv?.status === "done");
  const current = candidates.find((c) => c.streamId === selected) ?? candidates[0] ?? null;

  const latest = useRef(snap);
  latest.current = snap;
  useEffect(() => {
    const take = () => {
      if (!wanted) return;
      const streamId = wanted;
      wanted = null;
      const owner = latest.current.projects.find((p) => p.checkouts.some((c) => c.streamId === streamId));
      if (owner) setProjectId(owner.id);
      else if (latest.current.bakery.checkouts.some((c) => c.streamId === streamId)) setProjectId(BAKERY);
      setSelected(streamId);
    };
    take();
    wanting.add(take);
    return () => {
      wanting.delete(take);
    };
  }, []);

  /** Merges, on one yes, in order, stopping at the first that fails:
   *  asked work where the node recorded it (a refusal as the node words
   *  it), then other branches into the project's base. */
  async function merge(all: Candidate[]) {
    const asked = all.filter((c) => c.asked);
    const list = base ? all.filter((c) => !c.asked) : [];
    if (asked.length + list.length === 0) return;
    // The chained preview merges each branch onto base and every branch
    // before it, so two branches that only clash with each other show too.
    // Its query sends the branches comma-joined, so a comma in a name skips it.
    let warning = "";
    if (base && list.length) {
      if (list.some((c) => c.branch.includes(","))) {
        warning = "\n\nNo preview: a branch name has a comma in it.";
      } else {
        try {
          const chain = await gitMergePreviewChain({
            stream_id: base.streamId,
            base: base.branch,
            branches: list.map((c) => c.branch),
          });
          const clashing = chain.steps.filter((s) => !s.clean);
          if (clashing.length) {
            warning = `\n\nIn this order: ${clashing.map((s) => `${s.branch} conflicts in ${s.conflicts.join(", ")}`).join("; ")}. The run stops at the first conflict and changes nothing.`;
          }
        } catch {
          // No preview (a node without the route): ask without one.
        }
      }
    }
    const where = [...asked.map((c) => `${c.branch} into ${c.into ?? c.base}`), ...list.map((c) => `${c.branch} into ${base?.branch}`)].join(", ");
    if (!(await ask(`Merge ${where}?${warning}`))) return;
    setBusy(true);
    setNotice(null);
    const merged: string[] = [];
    let failure = "";
    for (const c of asked) {
      try {
        const r = await threadMerge({ stream_id: c.streamId });
        merged.push(`${r.branch} into ${r.into}`);
      } catch (e) {
        failure = said(e);
        break;
      }
    }
    for (const c of failure ? [] : list) {
      if (!base) break;
      try {
        await gitMerge({ stream_id: base.streamId, branch: c.branch, message: null });
        merged.push(`${c.branch} into ${base.branch}`);
      } catch (e) {
        failure = `${c.branch} did not merge: ${e}`;
        break;
      }
    }
    setBusy(false);
    setVersion((v) => v + 1);
    const done = merged.length ? `Merged ${merged.join(", ")}.` : "";
    setNotice({ bad: failure !== "", text: [done, failure].filter(Boolean).join(" ") });
  }

  return (
    <div className="flex h-full min-h-0 flex-col bg-(--bg-panel) text-xs" data-testid="merge-review">
      <div className="flex h-9 shrink-0 items-center gap-2 border-(--line-soft) border-b bg-(--bg-raised) px-2.5">
        <select
          aria-label="Project"
          className="h-6 rounded-[5px] border border-(--line) bg-(--bg-app) px-1.5 font-bold text-(--hue-project)"
          onChange={(e) => {
            setProjectId(e.target.value);
            setSelected(null);
          }}
          value={projectId}
        >
          <option value={BAKERY}>Bakery</option>
          {snap.projects.map((p) => (
            <option key={p.id} value={p.id}>
              {p.name}
            </option>
          ))}
        </select>
        <span className="flex-1" />
        <button className={pill} disabled={busy || ready.length === 0} onClick={() => merge(ready)} type="button">
          <GitMergeIcon className="size-3 text-(--accent)" />
          {bakery ? `Merge all (${ready.length}), in order` : `Merge all finished (${ready.length}), in order`}
        </button>
        <button aria-label="Refresh" className={`${pill} px-1.5`} onClick={() => setVersion((v) => v + 1)} title="Refresh" type="button">
          <RefreshCwIcon className="size-3" />
        </button>
      </div>

      {notice && (
        <p
          className={`flex items-start gap-2 border-b px-2.5 py-1 ${notice.bad ? "border-(--bad-border) bg-(--bad-bg) text-(--bad)" : "border-(--ok-border) bg-(--ok-bg) text-(--ok)"}`}
          role={notice.bad ? "alert" : "status"}
        >
          <span className="min-w-0 flex-1 break-words">{notice.text}</span>
          <button aria-label="Dismiss" onClick={() => setNotice(null)} type="button">
            <XIcon className="size-3" />
          </button>
        </p>
      )}

      {waiting.error && <p className="p-3 text-(--bad)">{waiting.error}</p>}
      {bakery && waiting.items?.length === 0 && <p className="p-3 text-(--fg-faint)">Nothing waiting to merge.</p>}
      {!bakery && !project && <p className="p-3 text-(--fg-faint)">No projects yet.</p>}
      {project && !base && <p className="p-3 text-(--fg-faint)">{project.name} has no repository checkout to merge into.</p>}
      {base && candidates.length === 0 && <p className="p-3 text-(--fg-faint)">No agent branches in {project?.name}: nothing to review.</p>}

      {candidates.length > 0 && (
        <div className="max-h-[38%] shrink-0 overflow-auto border-(--line) border-b">
          {candidates.map((c) => {
            const on = current?.streamId === c.streamId;
            return (
              <div
                className={`flex h-[46px] cursor-pointer items-center gap-2.5 border-(--line-soft) border-b px-3 ${on ? "bg-(--accent-sel) shadow-[inset_3px_0_0_var(--accent)]" : "hover:bg-(--bg-chip)"}`}
                key={c.streamId}
                onClick={() => setSelected(c.streamId)}
              >
                <StatusDot status={c.conv?.status ?? "idle"} streamId={c.streamId} title={c.conv ? statusLabel(c.conv.status) : ""} />
                <span className="flex min-w-0 flex-1 flex-col">
                  <span className="truncate font-bold text-(--fg)">{c.conv?.title ?? c.branch}</span>
                  <span className="truncate text-(--fg-dim) text-[11px]">
                    {c.branch} → {c.into ?? c.base} · {c.conv ? statusLabel(c.conv.status) : "no thread"}
                    {c.said ? ` · ${c.said}` : c.conv?.note ? ` · ${c.conv.note}` : ""}
                  </span>
                </span>
                {/* Only a task's worktree has a baseline; 0/0 elsewhere means "not counted". */}
                {c.added + c.removed > 0 && (
                  <span className="shrink-0 font-mono text-[11px]">
                    <span className="text-(--diff-add-fg)">+{c.added}</span> <span className="text-(--diff-del-fg)">−{c.removed}</span>
                  </span>
                )}
                <Jobs streamId={c.streamId} version={version} />
                <Checks streamId={c.streamId} version={version} />
                <MergeCheck base={c.base} streamId={c.streamId} version={version} />
                <button
                  className={pill}
                  disabled={busy}
                  onClick={(e) => {
                    e.stopPropagation();
                    void merge([c]);
                  }}
                  title={`Merge ${c.branch} into ${c.into ?? base?.branch}`}
                  type="button"
                >
                  Merge
                </button>
              </div>
            );
          })}
        </div>
      )}

      {current && (project || bakery) && (
        <BranchDiff candidate={current} ctx={ctx} key={current.streamId} project={project?.name ?? "the Bakery"} version={version} />
      )}
    </div>
  );
}

/** The latest run of each job the agent ran (tests, checks): passed, failed, running. */
function Jobs({ streamId, version }: { streamId: string; version: number }) {
  const [sum, setSum] = useState<ReturnType<typeof jobSummary> | null>(null);
  useEffect(() => {
    let live = true;
    jobList({ stream_id: streamId }).then(
      (jobs) => live && setSum(jobSummary(jobs)),
      () => live && setSum(null),
    );
    return () => {
      live = false;
    };
  }, [streamId, version]);
  if (!sum || sum.passed + sum.failed + sum.running === 0) return null;
  return (
    <span className="shrink-0 text-[11px]" title="The latest run of each command this agent ran as a job">
      {sum.passed > 0 && <span className="text-(--ok)">✓{sum.passed} </span>}
      {sum.failed > 0 && <span className="text-(--bad)">✗{sum.failed} </span>}
      {sum.running > 0 && <span className="text-(--fg-dim)">◐{sum.running}</span>}
    </span>
  );
}

/** Would it merge into base cleanly? git merge-tree on the node; nothing moves. */
function MergeCheck({ streamId, base, version }: { streamId: string; base: string; version: number }) {
  const [preview, setPreview] = useState<MergePreview | null>(null);
  useEffect(() => {
    let live = true;
    gitMergePreview({ stream_id: streamId, base }).then(
      (p) => live && setPreview(p),
      () => live && setPreview(null),
    );
    return () => {
      live = false;
    };
  }, [streamId, base, version]);
  if (!preview) return null;
  return preview.clean ? (
    <span title={`Merges into ${base} without a conflict`}>
      <Chip tone="teal">CLEAN</Chip>
    </span>
  ) : (
    <span title={`Would conflict with ${base} in:\n${preview.conflicts.join("\n")}`}>
      <Chip tone="warn">
        {preview.conflicts.length} {preview.conflicts.length === 1 ? "CONFLICT" : "CONFLICTS"}
      </Chip>
    </span>
  );
}

/** The branch's pull request checks on GitHub, when it has a PR. */
function Checks({ streamId, version }: { streamId: string; version: number }) {
  const [checks, setChecks] = useState<PrCheck[] | null>(null);
  useEffect(() => {
    let live = true;
    gitPrChecks({ stream_id: streamId }).then(
      (r) => live && setChecks(r.checks),
      () => live && setChecks(null), // no PR (or no gh): no badge
    );
    return () => {
      live = false;
    };
  }, [streamId, version]);
  if (!checks || checks.length === 0) return null;
  const count = (b: string) => checks.filter((c) => (c.bucket ?? c.state.toLowerCase()) === b).length;
  const [pass, fail, pending] = [count("pass"), count("fail"), count("pending")];
  return (
    <span className="shrink-0 text-[11px]" title={checks.map((c) => `${c.bucket ?? c.state} · ${c.name}`).join("\n")}>
      <span className="text-(--fg-faint)">CI </span>
      {pass > 0 && <span className="text-(--ok)">✓{pass} </span>}
      {fail > 0 && <span className="text-(--bad)">✗{fail} </span>}
      {pending > 0 && <span className="text-(--fg-dim)">◐{pending}</span>}
    </span>
  );
}

/** The branch against the branch it started from: its files, then the
 *  picked file's blocks side by side. */
export function BranchDiff({
  ctx,
  candidate,
  project,
  version,
}: {
  ctx: ToolContext;
  candidate: Candidate;
  project: string;
  version: number;
}) {
  const [diff, setDiff] = useState<{ files: FileDiff[]; truncated: boolean; secret: boolean } | null>(null);
  useSecretShown(diff?.secret === true);
  const [error, setError] = useState("");
  const [picked, setPicked] = useState<string | null>(null);
  /** Which recorded write put each hunk there, by "file:new_start". */
  const [sources, setSources] = useState<ReadonlyMap<string, HunkSource>>(new Map());
  const base = candidate.base;
  useEffect(() => {
    let live = true;
    gitDiff({ stream_id: candidate.streamId, base }).then(
      (d) => {
        if (!live) return;
        setDiff({ files: parseUnifiedDiff(d.diff), truncated: d.truncated, secret: d.secret });
        setError("");
      },
      (e) => live && setError(`${e}`),
    );
    // The chips are extra: without them the review still works.
    gitProvenance({ stream_id: candidate.streamId, base }).then(
      (p) => live && setSources(new Map(p.hunks.map((h) => [`${h.file}:${h.new_start}`, h]))),
      () => live && setSources(new Map()),
    );
    return () => {
      live = false;
    };
  }, [candidate.streamId, base, version]);

  if (error) return <p className="p-3 text-(--warn)">{error}</p>;
  if (!diff) return <p className="p-3 text-(--fg-faint)">Reading {candidate.branch}…</p>;
  if (diff.files.length === 0) {
    return (
      <p className="p-3 text-(--fg-dim)">
        Nothing on {candidate.branch} that {base} doesn't already have.
      </p>
    );
  }
  const add = diff.files.reduce((n, f) => n + f.add, 0);
  const del = diff.files.reduce((n, f) => n + f.del, 0);
  const file = diff.files.find((f) => f.path === picked) ?? diff.files[0];
  return (
    <div className="flex min-h-0 flex-1" data-testid="merge-review-diff">
      <aside aria-label="Files" className="flex w-[min(300px,34%)] shrink-0 flex-col border-(--line-soft) border-r bg-(--bg-chrome)">
        <div className="flex h-8 shrink-0 items-center gap-2 border-(--line-soft) border-b px-3">
          <span className="font-bold text-(--fg)">
            {diff.files.length} {diff.files.length === 1 ? "file" : "files"}
          </span>
          <span className="font-mono text-[11px]">
            <span className="text-(--diff-add-fg)">+{add}</span> <span className="text-(--diff-del-fg)">−{del}</span>
          </span>
          {diff.truncated && (
            <span className="text-(--warn) text-[11px]" title="The whole diff is too large to show">
              cut short
            </span>
          )}
        </div>
        <div className="min-h-0 flex-1 overflow-auto">
          {diff.files.map((f) => {
            const on = f.path === file.path;
            return (
              <button
                className={`flex h-8 w-full items-center gap-2 px-3 text-left ${on ? "bg-(--accent-sel) shadow-[inset_0_0_0_1px_var(--accent-border)]" : "hover:bg-(--bg-chip)"}`}
                key={`${f.oldPath ?? ""}→${f.path}`}
                onClick={() => setPicked(f.path)}
                title={f.oldPath ? `${f.oldPath} → ${f.path}` : f.path}
                type="button"
              >
                <span className="flex min-w-0 flex-1 flex-col leading-tight">
                  <span className="truncate font-semibold text-(--fg-body)">{basename(f.path)}</span>
                  <span className="truncate text-(--fg-faint) text-[10px]">{dirname(f.path)}</span>
                </span>
                {f.status !== "modified" && <Chip>{f.status.toUpperCase()}</Chip>}
                <span className="shrink-0 font-mono text-[11px]">
                  <span className="text-(--diff-add-fg)">+{f.add}</span> <span className="text-(--diff-del-fg)">−{f.del}</span>
                </span>
              </button>
            );
          })}
        </div>
      </aside>
      <FileView candidate={candidate} ctx={ctx} file={file} key={file.path} project={project} secret={diff?.secret === true} sources={sources} />
    </div>
  );
}

function FileView({
  sources,
  ...props
}: {
  ctx: ToolContext;
  candidate: Candidate;
  project: string;
  file: FileDiff;
  secret: boolean;
  sources: ReadonlyMap<string, HunkSource>;
}) {
  const { file } = props;
  return (
    <section aria-label={file.path} className="flex min-w-0 flex-1 flex-col gap-3 overflow-auto p-3">
      <div className="flex items-center gap-2.5">
        <span className="min-w-0 truncate font-bold text-(--fg)">{file.oldPath ? `${file.oldPath} → ${file.path}` : file.path}</span>
        <span className="shrink-0 font-mono text-[11px]">
          <span className="text-(--diff-add-fg)">+{file.add}</span> <span className="text-(--diff-del-fg)">−{file.del}</span>
        </span>
      </div>
      {file.status === "binary" && <p className="text-(--fg-faint)">Binary file; not shown.</p>}
      {file.hunks.map((h, i) => (
        <HunkBlock {...props} hunk={h} key={`${i}:${h.header}`} source={sources.get(`${file.path}:${h.newStart}`)} />
      ))}
    </section>
  );
}

/** One block of a change, side by side, with who wrote it and Ask. */
export function HunkBlock({
  ctx,
  candidate,
  project,
  file,
  hunk,
  source,
  secret,
}: {
  ctx: ToolContext;
  candidate: Candidate;
  project: string;
  file: FileDiff;
  hunk: Hunk;
  /** The diff holds a secret file: its blocks are never sent to an agent. */
  secret: boolean;
  /** Undefined until provenance answered (or for a hunk it doesn't list). */
  source?: HunkSource;
}) {
  const [asking, setAsking] = useState(false);
  const [question, setQuestion] = useState("");
  const [asked, setAsked] = useState<{ streamId: string; marker: string; question: string; to: string }[]>([]);
  const [error, setError] = useState("");
  const agentReady = candidate.conv !== null && ctx.snap.attachable.includes(candidate.streamId);
  const baker = ctx.snap.conversations.find((c) => c.main) ?? null;
  const base = candidate.base;
  const [oldText, newText] = useMemo(
    () => [
      hunk.lines
        .filter((l) => l.tag !== "+")
        .map((l) => l.text)
        .join("\n"),
      hunk.lines
        .filter((l) => l.tag !== "-")
        .map((l) => l.text)
        .join("\n"),
    ],
    [hunk],
  );

  async function send(toBaker: boolean) {
    const target = toBaker ? baker : candidate.conv;
    const q = question.trim();
    if (!target || !q) return;
    if (secret) {
      setError("This change holds a secret file, so its blocks aren't sent to an agent.");
      return;
    }
    const marker = questionMarker();
    const block = `\`\`\`diff\n${hunkText(file, hunk)}\n\`\`\``;
    const written = source?.source
      ? ` It was written in the turn that began "${(source.source.instruction ?? "").slice(0, 200)}" (turn ${source.source.turn_id}, ${source.source.at}).`
      : "";
    const instruction = toBaker
      ? `Review question (${marker}). The operator is reviewing branch ${candidate.branch} of project ${project} before it merges into ${base}. ` +
        `It is the work of the conversation "${candidate.conv?.title ?? candidate.branch}" (stream ${candidate.streamId}); read it if you need to.${written} ` +
        `Answer the question about this block; change no files.\n\n${block}\n\n${q}`
      : `Review question (${marker}) about this block of your change, before it merges into ${base}.${written} ` +
        `Say what it does and why you wrote it this way; change no files.\n\n${block}\n\n${q}`;
    setError("");
    try {
      // read_only: the turn runs in Plan on every runtime, so a question about
      // a block can never change files.
      await agentTurnQueue({ stream_id: target.streamId, instruction, seat: target.roleId as Seat, read_only: true });
      setAsked((a) => [...a, { streamId: target.streamId, marker, question: q, to: toBaker ? "Baker" : target.title }]);
      setQuestion("");
      setAsking(false);
    } catch (e) {
      setError(`${e}`);
    }
  }

  return (
    <div className="overflow-hidden rounded-md border border-(--line) bg-(--bg-app)">
      <div className="flex h-7 items-center gap-2 border-(--line-soft) border-b bg-(--bg-raised) pr-2 pl-2.5 text-[11px]">
        <span className="min-w-0 shrink truncate text-(--fg-dim)">{hunk.header}</span>
        <Provenance source={source} />
        <span className="flex-1" />
        <button
          aria-expanded={asking}
          className={`flex h-5 shrink-0 items-center gap-1 rounded px-1.5 ${asking ? "bg-(--accent-sel) text-(--accent-fg) shadow-[inset_0_0_0_1px_var(--accent-border)]" : "text-(--fg-2) hover:bg-(--bg-chip) hover:text-(--fg)"}`}
          onClick={() => setAsking((v) => !v)}
          title="Ask about this block"
          type="button"
        >
          <MessageCircleQuestionIcon className="size-3" /> Ask
        </button>
      </div>
      <SplitDiff newText={newText} oldText={oldText} />
      {asked.map((a) => (
        <div className="space-y-1 border-(--line-soft) border-t bg-(--bg-panel) px-3 py-1.5" key={a.marker}>
          <p className="text-(--fg-2)">
            <span className="text-(--fg-faint)">you → {a.to}: </span>
            {a.question}
          </p>
          <Answer marker={a.marker} store={ctx.store} streamId={a.streamId} />
        </div>
      ))}
      {asking && (
        <form
          className="flex flex-wrap items-center gap-1.5 border-(--line-soft) border-t bg-(--bg-raised) px-2 py-1.5"
          onSubmit={(e) => {
            e.preventDefault();
            void send(!agentReady);
          }}
        >
          <input
            aria-label="Your question about this block"
            autoFocus
            className="min-w-48 flex-1 rounded-md border border-(--line) bg-(--bg-app) px-1.5 py-0.5 outline-none focus:border-(--accent-border)"
            onChange={(e) => setQuestion(e.target.value)}
            placeholder="What does this do, and why this way?"
            value={question}
          />
          {agentReady && (
            <button className={pill} disabled={!question.trim()} type="submit">
              Ask {candidate.conv?.title}
            </button>
          )}
          <button
            className={pill}
            disabled={!question.trim() || !baker}
            onClick={() => void send(true)}
            title={agentReady ? "Ask the Baker instead" : "The agent that wrote this can't answer here; the Baker reads its work"}
            type="button"
          >
            Ask the Baker
          </button>
          {error && <span className="w-full text-(--bad)">{error}</span>}
        </form>
      )}
    </div>
  );
}

/** Who wrote a block: the recorded write (agent, when, what it was asked),
 *  "partly" when the block mixes writes, or none (a shell command, a hand
 *  edit, or code from the base branch). */
export function Provenance({ source }: { source?: HunkSource }) {
  const names = useContext(ThreadNamesContext);
  if (!source) return null;
  const s = source.source;
  const chip = "min-w-0 shrink truncate rounded-full bg-(--bg-chip) px-2 text-[10px] leading-[17px]";
  if (!s) {
    return (
      <span className={`${chip} text-(--fg-ghost)`} title="No recorded write: a shell command, a hand edit, or code from the base branch">
        no recorded write
      </span>
    );
  }
  const partly = s.lines_matched < source.added;
  const detail = [
    s.instruction ? `Asked: ${s.instruction}` : "",
    `${s.actor} · ${new Date(s.at).toLocaleString()} · ${s.family}`,
    partly ? `${s.lines_matched} of ${source.added} added lines came from this write` : "",
  ].filter(Boolean);
  return (
    <span className={`${chip} text-(--fg-2)`} data-testid="provenance-chip" title={detail.join("\n")}>
      {actorLabel(s.actor, names)} · {ago(s.at)}
      {s.instruction ? ` · “${s.instruction.slice(0, 48)}${s.instruction.length > 48 ? "…" : ""}”` : ""}
      {partly ? " · partly" : ""}
    </span>
  );
}

/** The answer, live, as the agent writes it. */
const Answer = memo(function Answer({ store, streamId, marker }: { store: ShellStore; streamId: string; marker: string }) {
  const [attached, setAttached] = useState<AttachedConversation | null>(null);
  useEffect(() => {
    const a = store.attach(streamId);
    setAttached(a);
    return () => a?.dispose();
  }, [store, streamId]);
  if (!attached) return <p className="text-(--fg-faint)">Asked. The answer appears in that thread.</p>;
  return <AnswerText marker={marker} source={attached.source} />;
});

function AnswerText({ source, marker }: { source: AgentPanelSource; marker: string }) {
  const snap = useSyncExternalStore(source.subscribe, source.getSnapshot, source.getSnapshot);
  const text = useMemo(() => answerAfter(snap.items, marker), [snap.items, marker]);
  if (text) return <p className="whitespace-pre-wrap text-(--fg-body)">{text}</p>;
  return <p className="text-(--fg-faint)">{snap.busy ? "Thinking…" : "Waiting for an answer…"}</p>;
}
