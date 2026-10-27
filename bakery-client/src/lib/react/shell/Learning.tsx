// The learning loop's controls (Round 15, the operator's L1): the memory
// wiki's Learning tab. The loop on, paused or held and why, the next nightly
// run, the reviewer model and the projects in it; each part (thread-end
// review, nightly run, reflect, replay, lifting between projects, digest)
// with its switch, triggers, times and limits, its last run and Run now (for
// every project in the loop or just one); and every run, newest first, with
// what it did, the notes and proposals it touched, what its own threads
// asked for and were refused, and the model calls it spent. A change goes to
// the node at once (POST /learning merges it); a value it refuses says why.

import { Fragment, useEffect, useRef, useState } from "react";
import {
  BellIcon,
  BrainIcon,
  ChevronDownIcon,
  FolderIcon,
  LayersIcon,
  MessageSquareIcon,
  MoonIcon,
  PauseIcon,
  PlayIcon,
  RotateCcwIcon,
  SparklesIcon,
  TriangleAlertIcon,
} from "lucide-react";
import {
  type Learning,
  learning,
  learningRun,
  learningRuns,
  type LearningRun,
  type LearningRuns,
  learningSet,
  type LoopModel,
  type LoopSettings,
  type ModelCall,
  type NoteView,
  type Proposal,
} from "$lib/api.generated";
import { ago } from "$lib/conversations";
import { onEnvelope } from "$lib/events";
import { keep, useCached } from "./cache";
import { Switch } from "./EngineRoom";
import { PLAIN, SOLID } from "./Internet";
import { POP, useDismiss } from "./PanelMenu";
import { Chip, type ChipTone, said, type ToolContext } from "./ui";

type Part = "thread_end" | "nightly" | "reflect" | "replay" | "lift" | "digest";
const PARTS: { key: Part; label: string; hue: string; icon: typeof BellIcon }[] = [
  { key: "thread_end", label: "Thread-end review", hue: "var(--link)", icon: MessageSquareIcon },
  { key: "nightly", label: "Nightly run", hue: "var(--hue-github)", icon: MoonIcon },
  { key: "reflect", label: "Reflect", hue: "var(--warn)", icon: SparklesIcon },
  { key: "replay", label: "Replay", hue: "var(--hue-containers)", icon: RotateCcwIcon },
  { key: "lift", label: "Lift between projects", hue: "var(--hue-project)", icon: LayersIcon },
  { key: "digest", label: "Digest", hue: "var(--hue-threads)", icon: BellIcon },
];
const PART: Record<string, (typeof PARTS)[number] | undefined> = Object.fromEntries(PARTS.map((p) => [p.key, p]));
const STATUS: Record<string, [ChipTone, string]> = { done: ["ok", "DONE"], skipped: ["neutral", "SKIPPED"], failed: ["bad", "FAILED"], running: ["accent", "RUNNING"] };
const TRIGGER: Record<string, string> = { archive: "on archive", merge: "on merge", nightly: "nightly", run_now: "run now" };
const ORDERS: [string, string][] = [
  ["newest", "Newest first"],
  ["oldest", "Oldest first"],
  ["most_used", "Most used first"],
  ["least_proven", "Least proven first"],
];
const FIELD = "min-w-0 rounded-[5px] border border-(--line) bg-(--bg-app) px-1.5 text-[11px] text-(--fg) tabular-nums";
const LABEL = "font-bold text-(--fg-dim) text-[10px] tracking-[.08em]";
const DIM = "text-(--fg-dim)";

/** The loop and its runs as the node has them, kept like a panel's data and
 *  read only while the tab shows; any change the node tells of reads both
 *  again. */
export function useLearning(on: boolean) {
  const loop = useCached<Learning>(on ? "learning" : null, () => learning({}));
  // ponytail: the newest 100 runs, filtered here by part; ask with ?part= if one part's history gets buried.
  const runs = useCached<LearningRuns>(on ? "learning:runs" : null, () => learningRuns({}));
  const { reload: reloadLoop } = loop;
  const { reload: reloadRuns } = runs;
  useEffect(() => {
    if (!on) return;
    return onEnvelope("learning.updated", () => {
      void reloadLoop();
      void reloadRuns();
    });
  }, [on, reloadLoop, reloadRuns]);
  const reload = () => {
    void reloadLoop();
    void reloadRuns();
  };
  return { loop, runs, reload };
}

const count = (n: number, word: string) => `${n} ${word}${n === 1 ? "" : "s"}`;
const tokens = (n: number) => (n < 1000 ? String(n) : `${(n / 1000).toFixed(n < 10_000 ? 1 : 0)}k`);
const clock = (ts: string) => new Date(ts).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
const agoWords = (ts: string) => (ago(ts) === "now" ? "just now" : `${ago(ts)} ago`);

/** When the nightly run next starts: its time today or tonight, the day
 *  too past that. */
function nextAt(ts: string): string {
  const d = new Date(ts);
  return d.getTime() - Date.now() < 86_400_000 ? clock(ts) : `${d.toLocaleDateString([], { weekday: "short" })} ${clock(ts)}`;
}

function dayOf(ts: string): string {
  const d = new Date(ts).toDateString();
  const today = new Date();
  if (d === today.toDateString()) return "TODAY";
  today.setDate(today.getDate() - 1);
  if (d === today.toDateString()) return "YESTERDAY";
  return new Date(ts).toLocaleDateString([], { weekday: "short", month: "short", day: "numeric" }).toUpperCase();
}

/** A run's time: when it started, and when it ended if that was later. */
function span(r: LearningRun): string {
  if (!r.started_at) return "";
  const end = r.ended_at && Date.parse(r.ended_at) - Date.parse(r.started_at) >= 60_000 ? `–${clock(r.ended_at)}` : "";
  return clock(r.started_at) + end;
}

/** A part's last run in a few words. */
function lastLine(r: LearningRun, thread: (sid: string) => string): string {
  const bits: string[] = [];
  if (r.part === "thread_end" && r.thread) bits.push(thread(r.thread));
  if (r.proposals?.length) bits.push(count(r.proposals.length, "proposal"));
  else if (r.notes?.length) bits.push(count(r.notes.length, "note"));
  else if (r.model_calls?.length) bits.push(count(r.model_calls.length, "call"));
  if (!bits.length && r.said) bits.push(r.said);
  const when = r.ended_at ?? r.started_at;
  if (when) bits.push(agoWords(when));
  return bits.join(" · ");
}

function Tick({ on, label, onChange }: { on: boolean; label: string; onChange: () => void }) {
  return (
    <button
      aria-pressed={on}
      className={`flex h-[22px] shrink-0 items-center gap-1 whitespace-nowrap rounded-[5px] border px-2 text-[10.5px] ${on ? "text-(--fg-body)" : "border-(--line) text-(--fg-dim)"}`}
      onClick={onChange}
      style={on ? { borderColor: "color-mix(in srgb, var(--ok) 45%, transparent)", background: "var(--ok-bg)" } : undefined}
      type="button"
    >
      {on ? "✓" : "○"} {label}
    </button>
  );
}

/** A number or a time the node keeps: sent once the field is left (or on
 *  Enter); put back when the node refuses it. */
function Field({ label, value, time, onSave }: { label: string; value?: number | string; time?: boolean; onSave: (v: string) => Promise<boolean> }) {
  const shown = value === undefined ? "" : String(value);
  const [draft, setDraft] = useState(shown);
  useEffect(() => setDraft(shown), [shown]);
  const commit = () => {
    if (draft === shown) return;
    if (draft === "") return setDraft(shown);
    void onSave(draft).then((ok) => ok || setDraft(shown));
  };
  return (
    <input
      aria-label={label}
      className={`${FIELD} h-[22px] text-center ${time ? "w-[86px]" : "w-12"}`}
      min={time ? undefined : 1}
      onBlur={commit}
      onChange={(e) => setDraft(e.target.value)}
      onKeyDown={(e) => {
        if (e.key === "Enter") e.currentTarget.blur();
        if (e.key === "Escape") setDraft(shown);
      }}
      type={time ? "time" : "number"}
      value={draft}
    />
  );
}

/** The models the loop may ask; one that can't run here shows why. */
function ModelSelect({ label, models, value, empty, need, onPick }: { label: string; models: LoopModel[]; value: string | null; empty: string; need?: boolean; onPick: (id: string | null) => void }) {
  return (
    <select
      aria-label={label}
      className={`${FIELD} h-[22px] ${need ? "font-bold" : ""}`}
      onChange={(e) => onPick(e.target.value || null)}
      style={need ? { borderColor: "var(--warn)", color: "var(--warn)", boxShadow: "0 0 0 3px var(--warn-bg)" } : undefined}
      value={value ?? ""}
    >
      <option value="">{empty}</option>
      {models.map((m) => (
        <option disabled={!m.available && m.id !== value} key={m.id} value={m.id}>
          {m.available ? m.label : `${m.label} · ${m.reason || "unavailable"}`}
        </option>
      ))}
      {value && !models.some((m) => m.id === value) && <option value={value}>{value}</option>}
    </select>
  );
}

const Row = ({ children }: { children: React.ReactNode }) => <div className="flex flex-wrap items-center gap-1.5">{children}</div>;

function PartCard({ part, on, last, lastWords, onToggle, run, children }: { part: (typeof PARTS)[number]; on: boolean; last: LearningRun | null; lastWords: string; onToggle: () => void; run: React.ReactNode; children: React.ReactNode }) {
  const [tone, word] = STATUS[last?.status ?? ""] ?? ["neutral", (last?.status ?? "").toUpperCase()];
  return (
    <div className="flex min-w-0 flex-col rounded-[10px] border border-(--line) bg-(--bg-raised)" style={{ boxShadow: `inset 3px 0 0 ${part.hue}` }}>
      <div
        className="flex h-[34px] items-center gap-2 rounded-t-[10px] border-(--line-soft) border-b pr-2.5 pl-3 font-bold text-(--fg) text-[11.5px]"
        style={{ background: `linear-gradient(90deg, color-mix(in srgb, ${part.hue} 12%, transparent), transparent 60%)` }}
      >
        <part.icon className="size-3.5 shrink-0" style={{ color: part.hue }} />
        <span className={on ? "" : "opacity-50"}>{part.label}</span>
        <span className="flex-1" />
        <Switch label={part.label} on={on} onChange={onToggle} />
      </div>
      <div className={`flex flex-1 flex-col gap-1.5 py-2 pr-2.5 pl-3 text-[11px] ${on ? "" : "opacity-50"}`}>{children}</div>
      <div className="flex min-w-0 items-center gap-1.5 border-(--line-soft) border-t py-1.5 pr-2.5 pl-3 text-(--fg-dim) text-[10.5px]">
        <span className={LABEL}>LAST</span>
        {last ? (
          <>
            <Chip tone={tone}>{word}</Chip>
            <span className="min-w-0 flex-1 truncate" title={last.said}>
              {lastWords}
            </span>
          </>
        ) : (
          <span className="flex-1 text-(--fg-faint)">none yet</span>
        )}
        {run}
      </div>
    </div>
  );
}

/** Run a thread-end review now, on the thread the operator picks. */
function ThreadRun({ ctx, disabled, onRun }: { ctx: ToolContext; disabled: boolean; onRun: (streamId: string) => void }) {
  const [open, setOpen] = useState(false);
  const [q, setQ] = useState("");
  const box = useRef<HTMLDivElement>(null);
  useDismiss(open, box, () => setOpen(false));
  const words = q.trim().toLowerCase();
  const threads = [...ctx.snap.conversations, ...ctx.snap.archived].filter((c) => !words || `${c.title} ${c.place}`.toLowerCase().includes(words));
  return (
    <div className="relative shrink-0" ref={box}>
      <button aria-expanded={open} className={PLAIN} disabled={disabled} onClick={() => setOpen((o) => !o)} type="button">
        <PlayIcon className="size-3" />
        Run on a thread
        <ChevronDownIcon className="size-3" />
      </button>
      {open && (
        <div aria-label="Run on a thread" className={`${POP} w-[340px]`} role="dialog">
          <input aria-label="Find a thread" autoFocus className={`${FIELD} mb-1 h-7 w-full text-left`} onChange={(e) => setQ(e.target.value)} placeholder="Find a thread" value={q} />
          <div className="flex max-h-[260px] flex-col overflow-auto">
            {threads.map((c) => (
              <button
                className="flex min-h-7 w-full items-center gap-2 rounded-[5px] px-2 text-left hover:bg-(--bg-chip)"
                key={c.streamId}
                onClick={() => {
                  setOpen(false);
                  onRun(c.streamId);
                }}
                type="button"
              >
                <MessageSquareIcon className="size-3 shrink-0 text-(--link)" />
                <span className="min-w-0 flex-1 truncate text-(--fg)">{c.title}</span>
                <span className="shrink-0 text-(--fg-faint) text-[10px]">{[c.place, c.archived ? "archived" : "", ago(c.lastTs)].filter(Boolean).join(" · ")}</span>
              </button>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}

const MENU_ROW = "flex min-h-7 w-full items-center gap-2 rounded-[5px] px-2 text-left text-(--fg) hover:bg-(--bg-chip)";

/** Run a part now for every project in the loop, or (its chevron) for one. */
function RunNow({ disabled, projects, onRun }: { disabled: boolean; projects: { id: string; name: string }[]; onRun: (project?: string) => void }) {
  const [open, setOpen] = useState(false);
  const box = useRef<HTMLDivElement>(null);
  useDismiss(open, box, () => setOpen(false));
  const pick = (project?: string) => {
    setOpen(false);
    onRun(project);
  };
  const one = projects.length > 1;
  return (
    <div className="relative flex shrink-0" ref={box}>
      <button className={`${PLAIN} ${one ? "rounded-r-none" : ""}`} disabled={disabled} onClick={() => onRun()} type="button">
        <PlayIcon className="size-3" />
        Run now
      </button>
      {one && (
        <button aria-expanded={open} aria-label="Run now for one project" className={`${PLAIN} -ml-px rounded-l-none px-1`} disabled={disabled} onClick={() => setOpen((o) => !o)} type="button">
          <ChevronDownIcon className="size-3" />
        </button>
      )}
      {open && (
        <div aria-label="Run now for" className={`${POP} w-[220px]`} role="menu">
          <button className={MENU_ROW} onClick={() => pick()} role="menuitem" type="button">
            <PlayIcon className="size-3 shrink-0 text-(--fg-dim)" />
            Every project
          </button>
          {projects.map((p) => (
            <button className={MENU_ROW} key={p.id} onClick={() => pick(p.id)} role="menuitem" type="button">
              <FolderIcon className="size-3 shrink-0 text-(--hue-project)" />
              <span className="truncate">{p.name}</span>
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

const openThread = (streamId: string) => window.dispatchEvent(new CustomEvent("bakery:open-conversation", { detail: { streamId } }));
const LINK = "inline-flex h-5 max-w-full items-center gap-1 truncate rounded-[5px] bg-(--accent-bg) px-1.5 text-(--link) text-[10.5px] hover:brightness-110";

function Calls({ calls, model, thread }: { calls: ModelCall[]; model: (id: string) => string; thread: (sid: string) => string }) {
  const grid = "grid grid-cols-[minmax(0,90px)_minmax(0,1fr)_40px_36px_12px] items-center gap-2 px-2.5";
  return (
    <div className="flex flex-col overflow-hidden rounded-[7px] border border-(--line-soft) bg-(--bg-app) text-[10.5px] tabular-nums">
      <div className={`${grid} h-[21px] bg-(--bg-raised) ${LABEL} text-[9.5px]`}>
        <span>MODEL</span>
        <span>FOR</span>
        <span>IN</span>
        <span>OUT</span>
        <span />
      </div>
      {calls.map((c, i) => (
        <div className={`${grid} h-6 border-(--line-soft) border-t text-(--fg-body)`} key={`${c.model}:${i}`}>
          <span className="truncate">{model(c.model)}</span>
          <span className="truncate">{[c.purpose, c.thread ? thread(c.thread) : ""].filter(Boolean).join(" · ")}</span>
          <span>{c.tokens_in == null ? "—" : tokens(c.tokens_in)}</span>
          <span>{c.tokens_out == null ? "—" : tokens(c.tokens_out)}</span>
          <span className={c.ok ? "text-(--ok)" : "text-(--bad)"}>{c.ok ? "✓" : "✗"}</span>
        </div>
      ))}
    </div>
  );
}

function RunCard({
  r,
  model,
  thread,
  project,
  notes,
  proposals,
  onOpenNote,
  onOpenProposal,
}: {
  r: LearningRun;
  model: (id: string) => string;
  thread: (sid: string) => string;
  project: (id: string) => string;
  notes: NoteView[];
  proposals: Proposal[];
  onOpenNote: (id: string) => void;
  onOpenProposal: (id: string) => void;
}) {
  const [open, setOpen] = useState(false);
  const p = PART[r.part ?? ""];
  const [tone, word] = STATUS[r.status ?? ""] ?? ["neutral", (r.status ?? "").toUpperCase()];
  const calls = r.model_calls ?? [];
  const made = (r.proposals ?? []).flatMap((id) => proposals.filter((x) => x.id === id));
  const touched = (r.notes ?? []).flatMap((id) => notes.filter((n) => n.id === id));
  const failed = calls.filter((c) => !c.ok).length;
  const meta = [
    r.proposals?.length ? count(r.proposals.length, "proposal") : "",
    r.notes?.length ? count(r.notes.length, "note") : "",
    calls.length ? count(calls.length, "call") : "no model calls",
    calls.length ? `${tokens(calls.reduce((n, c) => n + (c.tokens_in ?? 0), 0))} in` : "",
    calls.length ? `${tokens(calls.reduce((n, c) => n + (c.tokens_out ?? 0), 0))} out` : "",
  ].filter(Boolean);
  return (
    <div className="flex flex-col gap-1 rounded-[9px] border border-(--line) bg-(--bg-raised) py-2 pr-2.5 pl-3" style={{ boxShadow: `inset 3px 0 0 ${p?.hue ?? "var(--line)"}` }}>
      <div className="flex items-center gap-1.5 whitespace-nowrap text-(--fg-dim) text-[10.5px]">
        {p && <p.icon className="size-3 shrink-0" style={{ color: p.hue }} />}
        <b className="text-[11px]" style={{ color: p?.hue }}>
          {p?.label ?? r.part}
        </b>
        <Chip tone={tone}>{word}</Chip>
        <span>{TRIGGER[r.trigger ?? ""] ?? r.trigger}</span>
        {r.project && <span className="min-w-0 truncate">· {project(r.project)}</span>}
        <span className="flex-1" />
        {r.status === "running" && <span className="size-3 shrink-0 animate-spin rounded-full border-2 border-(--line) border-t-(--accent)" />}
        <span className="tabular-nums">{span(r)}</span>
      </div>
      {(r.thread || r.said) && (
        <div className="flex flex-wrap items-center gap-1.5 text-(--fg-body) text-[11px] leading-relaxed">
          {r.thread && (
            <button className={LINK} onClick={() => r.thread && openThread(r.thread)} type="button">
              <MessageSquareIcon className="size-2.5 shrink-0" />
              {thread(r.thread)}
            </button>
          )}
          <span className="min-w-0">{r.said}</span>
        </div>
      )}
      {(r.refused?.length ?? 0) > 0 && (
        <div className="flex flex-wrap items-center gap-1">
          <span className={`${LABEL} text-(--bad) text-[9.5px]`}>REFUSED</span>
          {r.refused?.map((x) => (
            <span className="inline-flex h-5 items-center rounded-[5px] bg-(--bad-bg) px-1.5 text-(--fg-body) text-[10.5px]" key={x} style={{ boxShadow: "inset 0 0 0 1px var(--bad-border)" }}>
              {x}
            </span>
          ))}
        </div>
      )}
      {made.length + touched.length > 0 && (
        <div className="flex flex-wrap items-center gap-1">
          {made.map((x) => {
            const kept = x.status === "kept";
            const pending = !x.status || x.status === "pending";
            const go = pending ? () => x.id && onOpenProposal(x.id) : kept && x.kept_as ? () => x.kept_as && onOpenNote(x.kept_as) : undefined;
            return (
              <button className={`${LINK} disabled:opacity-60`} disabled={!go} key={x.id} onClick={go} type="button">
                <Chip tone={pending ? "warn" : kept ? "ok" : "neutral"}>{pending ? "TO KEEP" : kept ? "KEPT" : "DECLINED"}</Chip>
                <span className="truncate">{x.note?.title || "Untitled"}</span>
              </button>
            );
          })}
          {touched.map((n) => (
            <button className={LINK} key={n.id} onClick={() => n.id && onOpenNote(n.id)} type="button">
              <span className="truncate">{n.title || n.name}</span>
            </button>
          ))}
        </div>
      )}
      {calls.length > 0 ? (
        <button aria-expanded={open} className="self-start text-(--fg-faint) text-[10.5px] tabular-nums hover:text-(--fg-dim)" onClick={() => setOpen((o) => !o)} type="button">
          {meta.join(" · ")}
          {failed > 0 && <span className="text-(--bad)"> · {count(failed, "call")} failed</span>}
        </button>
      ) : (
        <span className="text-(--fg-faint) text-[10.5px]">{meta.join(" · ")}</span>
      )}
      {open && <Calls calls={calls} model={model} thread={thread} />}
    </div>
  );
}

export function LearningTab({
  ctx,
  reads,
  notes,
  proposals,
  thread,
  onOpenNote,
  onOpenProposal,
}: {
  ctx: ToolContext;
  reads: ReturnType<typeof useLearning>;
  notes: NoteView[];
  proposals: Proposal[];
  thread: (sid: string) => string;
  onOpenNote: (id: string) => void;
  onOpenProposal: (id: string) => void;
}) {
  const { loop, runs } = reads;
  const [problem, setProblem] = useState("");
  const [only, setOnly] = useState("");
  const l = loop.data;
  if (!l) return <p className="p-5 text-(--fg-faint)">{loop.error || "Reading the learning loop…"}</p>;

  const s = l.settings;
  const paused = s.paused === true;
  const out = s.projects_out ?? [];
  const save = (patch: LoopSettings) => {
    setProblem("");
    return learningSet(patch).then(
      (next) => {
        keep("learning", next);
        return true;
      },
      (e) => {
        setProblem(said(e));
        return false;
      },
    );
  };
  // A part's settings go whole, so a merge one level deep or a shallow one
  // keeps the rest of it.
  const setPart = (k: Part, change: Record<string, unknown>) => save({ [k]: { ...s[k], ...change } } as LoopSettings);
  const num = (k: Part, field: string) => (v: string) => setPart(k, { [field]: Number(v) });
  const runNow = (part: Part, { thread, project }: { thread?: string; project?: string } = {}) => {
    setProblem("");
    learningRun({ part, thread: thread ?? null, project: project ?? null }).then(reads.reload, (e) => setProblem(said(e)));
  };
  const loopProjects = ctx.snap.projects.filter((p) => !out.includes(p.id));
  const model = (id: string) => l.models.find((m) => m.id === id)?.label ?? id;
  const project = (id: string) => ctx.snap.projects.find((p) => p.id === id)?.name ?? id;
  const te = s.thread_end ?? {};
  const ni = s.nightly ?? {};
  const re = s.reflect ?? {};
  const rp = s.replay ?? {};
  const knobs: Record<Part, React.ReactNode> = {
    thread_end: (
      <>
        <Row>
          <Tick label="on archive" on={te.on_archive ?? true} onChange={() => void setPart("thread_end", { on_archive: !(te.on_archive ?? true) })} />
          <Tick label="on merge" on={te.on_merge ?? true} onChange={() => void setPart("thread_end", { on_merge: !(te.on_merge ?? true) })} />
          <Tick label="nightly" on={te.in_nightly ?? true} onChange={() => void setPart("thread_end", { in_nightly: !(te.in_nightly ?? true) })} />
        </Row>
        <Row>
          <span className={DIM}>at least</span>
          <Field label="Fewest turns since its last review" onSave={num("thread_end", "min_turns")} value={te.min_turns} />
          <span className={DIM}>turns · up to</span>
          <Field label="Most proposals in one review" onSave={num("thread_end", "max_proposals")} value={te.max_proposals} />
          <span className={DIM}>proposals</span>
        </Row>
        <Row>
          <span className={DIM}>model</span>
          <ModelSelect empty="The thread's own" label="Thread-end review model" models={l.models} onPick={(id) => void setPart("thread_end", { model: id })} value={te.model ?? null} />
        </Row>
      </>
    ),
    nightly: (
      <>
        <Row>
          <span className={DIM}>at</span>
          <Field label="Nightly run time" onSave={(v) => setPart("nightly", { at: v })} time value={ni.at} />
          <span className={DIM}>· busy or hot: again every</span>
          <Field label="Minutes between tries" onSave={num("nightly", "retry_every_minutes")} value={ni.retry_every_minutes} />
          <span className={DIM}>min until</span>
          <Field label="Last time it may start" onSave={(v) => setPart("nightly", { retry_until: v })} time value={ni.retry_until} />
        </Row>
        <Row>
          <Tick label="wait while agents work" on={ni.wait_while_busy ?? true} onChange={() => void setPart("nightly", { wait_while_busy: !(ni.wait_while_busy ?? true) })} />
          <Tick label="wait while hot" on={ni.wait_while_hot ?? true} onChange={() => void setPart("nightly", { wait_while_hot: !(ni.wait_while_hot ?? true) })} />
        </Row>
      </>
    ),
    reflect: (
      <Row>
        <span className={DIM}>the last</span>
        <Field label="Days it looks back" onSave={num("reflect", "window_days")} value={re.window_days} />
        <span className={DIM}>days · up to</span>
        <Field label="Most lessons in one run" onSave={num("reflect", "max_proposals")} value={re.max_proposals} />
        <span className={DIM}>proposals</span>
      </Row>
    ),
    replay: (
      <Row>
        <span className={DIM}>up to</span>
        <Field label="Most lessons replayed in one run" onSave={num("replay", "cap")} value={rp.cap} />
        <span className={DIM}>notes ·</span>
        <select aria-label="Which lessons first" className={`${FIELD} h-[22px]`} onChange={(e) => void setPart("replay", { order: e.target.value })} value={rp.order ?? "newest"}>
          {ORDERS.map(([v, w]) => (
            <option key={v} value={v}>
              {w}
            </option>
          ))}
        </select>
      </Row>
    ),
    lift: (
      <Row>
        <span className={DIM}>Bakery-wide after</span>
        <Field label="Projects a lesson must be in" onSave={num("lift", "min_projects")} value={s.lift?.min_projects} />
        <span className={DIM}>projects</span>
      </Row>
    ),
    digest: (
      <Row>
        <Tick label="Mac notification" on={s.digest?.notify ?? true} onChange={() => void setPart("digest", { notify: !(s.digest?.notify ?? true) })} />
      </Row>
    ),
  };
  const all = runs.data?.runs ?? [];
  const shown = only ? all.filter((r) => r.part === only) : all;
  const days: [string, LearningRun[]][] = [];
  for (const r of shown) {
    const day = r.started_at ? dayOf(r.started_at) : "";
    if (days.at(-1)?.[0] === day) days.at(-1)?.[1].push(r);
    else days.push([day, [r]]);
  }

  return (
    <div className="min-h-0 flex-1 overflow-auto p-3">
      <div className="flex flex-wrap items-start gap-3.5">
        <div className="flex min-w-[320px] flex-[1_1_560px] flex-col gap-2.5">
          <div className="flex flex-col gap-2 rounded-[10px] border border-(--line) bg-(--bg-raised) px-3 py-2.5 shadow-(--shadow-chip)">
            <div className="flex items-center gap-2">
              <BrainIcon className="size-3.5 shrink-0 text-(--warn)" />
              <b className="text-(--fg) text-[12px]">Learning loop</b>
              <Chip tone={paused || l.held.length ? "warn" : "ok"}>{paused ? "PAUSED" : l.held.length ? "HELD" : "ON"}</Chip>
              <span className="flex-1" />
              {l.changed_at && (
                <span className="text-(--fg-faint) text-[10.5px]">
                  changed by {l.changed_by === "operator" ? "you" : (l.changed_by ?? "the node")} · {agoWords(l.changed_at)}
                </span>
              )}
              {paused ? (
                <button className={SOLID} onClick={() => void save({ paused: false })} type="button">
                  <PlayIcon className="mr-1 size-3" />
                  Resume
                </button>
              ) : (
                <button className={PLAIN} onClick={() => void save({ paused: true })} type="button">
                  <PauseIcon className="size-3" />
                  Pause
                </button>
              )}
            </div>
            <div className="flex flex-wrap items-center gap-2">
              <span className={DIM}>next nightly</span>
              <b className="text-(--fg)">{l.next_nightly ? nextAt(l.next_nightly) : paused ? "paused" : "off"}</b>
              <span className="h-4 w-px bg-(--line)" />
              <span className={DIM}>reviewer</span>
              <ModelSelect empty="Pick a model" label="Reviewer model" models={l.models} need={!s.reviewer_model} onPick={(id) => void save({ reviewer_model: id })} value={s.reviewer_model ?? null} />
            </div>
          </div>
          {l.held.map((h) => (
            <div className="flex items-center gap-2 rounded-[9px] border border-(--warn-border) bg-(--warn-bg) px-3 py-1.5 text-(--warn) text-[11px]" key={h}>
              <TriangleAlertIcon className="size-3.5 shrink-0" />
              {h}
            </div>
          ))}
          {problem && <p className="text-(--bad) text-[11px]">{problem}</p>}
          <div className="flex flex-wrap items-center gap-1.5">
            <span className={`${LABEL} w-[74px]`}>PROJECTS</span>
            {ctx.snap.projects.map((p) => {
              const inLoop = !out.includes(p.id);
              return <Tick key={p.id} label={p.name} on={inLoop} onChange={() => void save({ projects_out: inLoop ? [...out, p.id] : out.filter((x) => x !== p.id) })} />;
            })}
          </div>
          <div className="grid grid-cols-[repeat(auto-fill,minmax(300px,1fr))] gap-2.5">
            {PARTS.map((p) => {
              const on = s[p.key]?.on ?? true;
              const last = l.last[p.key] ?? null;
              return (
                <PartCard
                  key={p.key}
                  last={last}
                  lastWords={last ? lastLine(last, thread) : ""}
                  on={on}
                  onToggle={() => void setPart(p.key, { on: !on })}
                  part={p}
                  run={
                    p.key === "thread_end" ? (
                      <ThreadRun ctx={ctx} disabled={!on} onRun={(sid) => runNow("thread_end", { thread: sid })} />
                    ) : (
                      <RunNow disabled={!on} onRun={(project) => runNow(p.key, { project })} projects={loopProjects} />
                    )
                  }
                >
                  {knobs[p.key]}
                </PartCard>
              );
            })}
          </div>
        </div>
        <div className="flex min-w-[300px] flex-[1_1_380px] flex-col gap-1.5 xl:max-w-[460px]">
          <div className="flex h-6 items-center gap-2">
            <span className={`${LABEL} text-(--fg)`}>RUNS</span>
            <span className="flex-1" />
            <select aria-label="Runs of" className={`${FIELD} h-[22px]`} onChange={(e) => setOnly(e.target.value)} value={only}>
              <option value="">Every part</option>
              {PARTS.map((p) => (
                <option key={p.key} value={p.key}>
                  {p.label}
                </option>
              ))}
            </select>
          </div>
          {runs.error && <p className="text-(--bad) text-[11px]">{runs.error}</p>}
          {days.map(([day, rs]) => (
            <Fragment key={`${day}:${rs[0]?.id}`}>
              <div className={`${LABEL} px-0.5 pt-1 text-(--fg-faint) text-[9.5px]`}>{day}</div>
              {rs.map((r) => (
                <RunCard
                  key={r.id}
                  model={model}
                  notes={notes}
                  onOpenNote={onOpenNote}
                  onOpenProposal={onOpenProposal}
                  project={project}
                  proposals={proposals}
                  r={r}
                  thread={thread}
                />
              ))}
            </Fragment>
          ))}
          {runs.data && !shown.length && <p className="text-(--fg-faint)">No runs yet.</p>}
        </div>
      </div>
    </div>
  );
}
