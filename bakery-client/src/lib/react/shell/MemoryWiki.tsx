// The memory wiki (Round 13, the operator's M1): every kept note one line
// with how it's used (helped, hurt, stale), most used first, beside the open
// note as a page (summary, body, why, how to apply, evidence, links, its
// versions); the notes agents proposed waiting on top, Keep all keeping
// them; scopes (the Bakery, the Baker, a project, a thread); the timeline
// of what was proposed, kept, edited and retired as its second tab; the
// learning loop's controls and runs as its third (Round 15's L1). The
// operator writes, edits and retires notes; agents propose and use them.

import { useEffect, useMemo, useState } from "react";
import { BrainIcon, PlusIcon } from "lucide-react";
import { MessageResponse } from "@/lib/ai-elements/message";
import {
  memoryEdit,
  memoryKeepAll,
  memoryNote,
  memoryNotes,
  memoryPromote,
  memoryProposals,
  memoryRetire,
  memoryReview,
  memoryTimeline,
  type MemoryNotes,
  type MemoryProposals,
  type MemoryTimeline,
  type Note,
  type NotePage,
  type NoteView,
  type Proposal,
} from "$lib/api.generated";
import { ask } from "$lib/ask";
import { ago } from "$lib/conversations";
import { onEnvelope } from "$lib/events";
import { refresh, useCached } from "./cache";
import { PLAIN, SOLID } from "./Internet";
import { LearningTab, useLearning } from "./Learning";
import { Chip, Freshness, said, type ToolContext } from "./ui";

const TYPES: Record<string, [string, string]> = {
  user: ["USER", "var(--link)"],
  feedback: ["FEEDBACK", "var(--warn)"],
  project: ["PROJECT", "var(--ok)"],
  reference: ["REFERENCE", "var(--violet)"],
  skill: ["SKILL", "var(--hue-containers)"],
  lesson: ["LESSON", "var(--bad)"],
};
const MOMENT: Record<string, string> = {
  proposed: "var(--warn)",
  kept: "var(--ok)",
  edited: "var(--link)",
  written: "var(--violet)",
  scratch: "var(--hue-containers)",
  promoted: "var(--ok)",
  rated: "var(--fg-dim)",
  declined: "var(--fg-dim)",
  retired: "var(--fg-faint)",
};
const FIELD = "min-w-0 rounded-[5px] border border-(--line) bg-(--bg-app) px-2 py-1 text-[11.5px] text-(--fg-body)";
const LABEL = "font-bold text-(--fg-dim) text-[10px] tracking-[.08em]";

function TypeChip({ type }: { type?: string }) {
  const [label, hue] = TYPES[type ?? ""] ?? ["NOTE", "var(--fg-dim)"];
  return (
    <span
      className="inline-flex h-[17px] shrink-0 items-center rounded px-1.5 font-bold text-[9.5px] tracking-[.05em]"
      style={{ color: hue, background: `color-mix(in srgb, ${hue} 14%, transparent)`, boxShadow: `inset 0 0 0 1px color-mix(in srgb, ${hue} 40%, transparent)` }}
    >
      {label}
    </span>
  );
}

/** The notes agents proposed, waiting on the operator: kept like a panel's
 *  data (the Baker's sidebar counts them). */
export const useMemoryProposals = () => useCached<MemoryProposals>("memory:proposals", () => memoryProposals({}));

/** Any change to the memory (memory.updated): every kept read of it reads
 *  again, the open page too. */
function useMemoryLive(open: string | null) {
  useEffect(
    () =>
      onEnvelope("memory.updated", () => {
        void refresh("memory:notes", () => memoryNotes({}));
        void refresh("memory:scratch", () => memoryNotes({ status: "scratch" }));
        void refresh("memory:proposals", () => memoryProposals({}));
        void refresh("memory:timeline", () => memoryTimeline({ limit: 200 }));
        if (open) void refresh(`memory:note:${open}`, () => memoryNote({ id: open }));
      }),
    [open],
  );
}

type Open = { kind: "note"; id: string } | { kind: "scratch"; id: string } | { kind: "proposal"; id: string } | { kind: "new" };
type Tab = "pages" | "timeline" | "learning";
const TAB_WORD: Record<Tab, string> = { pages: "Pages", timeline: "Timeline", learning: "Learning" };
type Fields = { type: string; scope: string; title: string; summary: string; body: string; why: string; how_to_apply: string };

export function MemoryWiki({ ctx }: { ctx: ToolContext }) {
  const [tab, setTab] = useState<Tab>("pages");
  // A way in that names a tab (the digest's Mac notification: To keep).
  useEffect(() => {
    const onTool = (e: Event) => {
      const { id, tab: to } = (e as CustomEvent<{ id: string; tab?: Tab }>).detail;
      if (id === "@memory" && to) setTab(to);
    };
    window.addEventListener("bakery:open-tool", onTool);
    return () => window.removeEventListener("bakery:open-tool", onTool);
  }, []);
  const learn = useLearning(tab === "learning");
  const notes = useCached<MemoryNotes>("memory:notes", () => memoryNotes({}));
  // Agents' direct notes, each its own thread's working notes until the
  // operator promotes one; the operator sees every thread's.
  const scratch = useCached<MemoryNotes>("memory:scratch", () => memoryNotes({ status: "scratch" }));
  const proposals = useMemoryProposals();
  const [scope, setScope] = useState("all");
  const [query, setQuery] = useState("");
  const [open, setOpen] = useState<Open | null>(null);
  const [problem, setProblem] = useState("");
  const thread = (sid: string) => ctx.snap.conversations.find((c) => c.streamId === sid)?.title ?? ctx.snap.archived.find((c) => c.streamId === sid)?.title ?? "a thread";
  const who = (by?: string | null) => (!by || by === "operator" ? "you" : thread(by));
  const scopeWord = (s?: string) =>
    !s || s === "bakery"
      ? "Bakery"
      : s === "baker"
        ? "Baker"
        : s.startsWith("project:")
          ? (ctx.snap.projects.find((p) => p.id === s.slice(8))?.name ?? "a project")
          : s.startsWith("workspace:")
            ? thread(s.slice(10))
            : s.startsWith("private:")
              ? `${thread(s.slice(8))}, yours alone`
              : s;
  const all = notes.data?.notes ?? [];
  const pending = (proposals.data?.proposals ?? []).filter((p) => !p.status || p.status === "pending");
  const scopes = [...new Set(all.map((n) => n.scope ?? "bakery"))];
  const q = query.trim().toLowerCase();
  const shown = all
    .filter((n) => (scope === "all" || (n.scope ?? "bakery") === scope) && (!q || [n.title, n.summary, n.body, n.name].some((s) => s?.toLowerCase().includes(q))))
    .sort((a, b) => b.uses.read_week + b.uses.helped - (a.uses.read_week + a.uses.helped));
  const current = open ?? (shown[0]?.id ? { kind: "note" as const, id: shown[0].id } : null);
  useMemoryLive(current?.kind === "note" || current?.kind === "scratch" ? current.id : null);
  const reloadAll = () => {
    void notes.reload();
    void scratch.reload();
    void proposals.reload();
  };
  /** Each thread's working notes, the thread with the newest first. */
  const working = useMemo(() => {
    const by = new Map<string, NoteView[]>();
    for (const n of scratch.data?.notes ?? []) {
      const sid = (n.scope ?? "").startsWith("workspace:") ? (n.scope ?? "").slice(10) : (n.stream_id ?? "");
      if (q && ![n.title, n.summary, n.body, n.name].some((x) => x?.toLowerCase().includes(q))) continue;
      by.set(sid, [...(by.get(sid) ?? []), n]);
    }
    return [...by.entries()];
  }, [scratch.data, q]);
  const keepAll = async () => {
    if (!(await ask(`Keep all ${pending.length} proposed ${pending.length === 1 ? "note" : "notes"} as they are?`))) return;
    setProblem("");
    memoryKeepAll({}).then(reloadAll, (e) => setProblem(said(e)));
  };

  return (
    <div className="flex h-full min-h-0 flex-col bg-(--bg-panel) text-xs" data-testid="memory-wiki">
      <div className="flex h-9 shrink-0 items-center gap-1.5 border-(--line-soft) border-b bg-(--bg-raised) px-2.5">
        <BrainIcon className="size-3.5 text-(--warn)" />
        {(["pages", "timeline", "learning"] as const).map((t) => (
          <button
            aria-pressed={tab === t}
            className={`h-6 rounded-[5px] border px-2 text-[11px] ${tab === t ? "border-(--accent-border) bg-(--accent-sel) font-bold text-(--fg)" : "border-transparent text-(--fg-dim) hover:bg-(--bg-chip)"}`}
            key={t}
            onClick={() => setTab(t)}
            type="button"
          >
            {TAB_WORD[t]}
          </button>
        ))}
        {problem && <span className="min-w-0 truncate text-(--bad) text-[11px]">{problem}</span>}
        <span className="flex-1" />
        {tab === "learning" ? (
          <Freshness at={learn.loop.at} busy={learn.loop.busy || learn.runs.busy} error={learn.loop.error || learn.runs.error} onReload={learn.reload} />
        ) : (
          <Freshness at={notes.at} busy={notes.busy} error={notes.error} onReload={reloadAll} />
        )}
        <button
          className={SOLID}
          onClick={() => {
            setTab("pages");
            setOpen({ kind: "new" });
          }}
          type="button"
        >
          <PlusIcon className="mr-1 size-3" />
          Note
        </button>
      </div>
      {tab === "learning" ? (
        <LearningTab
          ctx={ctx}
          notes={all}
          onOpenNote={(id) => {
            setTab("pages");
            setOpen({ kind: "note", id });
          }}
          onOpenProposal={(id) => {
            setTab("pages");
            setOpen({ kind: "proposal", id });
          }}
          proposals={proposals.data?.proposals ?? []}
          reads={learn}
          thread={thread}
        />
      ) : tab === "timeline" ? (
        <Timeline
          onOpen={(id) => {
            setTab("pages");
            setOpen({ kind: "note", id });
          }}
          who={who}
        />
      ) : (
        <div className="flex min-h-0 flex-1">
          <nav aria-label="Notes" className="flex w-[360px] shrink-0 flex-col gap-1 overflow-auto border-(--line) border-r bg-(--bg-raised) p-2">
            <div className="flex flex-wrap gap-1">
              {["all", ...scopes].map((s) => (
                <button
                  className={`h-6 rounded-[5px] border px-2 text-[11px] ${scope === s ? "border-(--accent-border) bg-(--accent-sel) font-bold text-(--fg)" : "border-(--line) text-(--fg-dim) hover:text-(--fg-2)"}`}
                  key={s}
                  onClick={() => setScope(s)}
                  type="button"
                >
                  {s === "all" ? "All" : scopeWord(s)}
                </button>
              ))}
            </div>
            <input aria-label="Search memory" className={`${FIELD} h-7`} onChange={(e) => setQuery(e.target.value)} placeholder="Search memory" value={query} />
            {pending.length > 0 && (
              <>
                <div className="flex h-7 items-center gap-1.5 px-1">
                  <span className={`${LABEL} text-(--warn)`}>TO KEEP</span>
                  <Chip tone="warn">{pending.length}</Chip>
                  <span className="flex-1" />
                  <button className={SOLID} onClick={() => void keepAll()} type="button">
                    Keep all
                  </button>
                </div>
                {pending.map((p) => (
                  <button
                    className={`flex min-h-7 min-w-0 items-center gap-1.5 rounded-[5px] px-1.5 text-left ${current?.kind === "proposal" && current.id === p.id ? "bg-(--accent-sel)" : "hover:bg-(--bg-chip)"}`}
                    key={p.id}
                    onClick={() => p.id && setOpen({ kind: "proposal", id: p.id })}
                    type="button"
                  >
                    <TypeChip type={p.note?.type} />
                    <span className="min-w-0 flex-1 truncate text-(--fg)">{p.note?.title || "Untitled"}</span>
                    <span className="shrink-0 text-(--violet) text-[10.5px]">{who(p.proposed_by)}</span>
                  </button>
                ))}
              </>
            )}
            <div className="flex h-7 items-center gap-1.5 px-1">
              <span className={LABEL}>NOTES</span>
              <Chip>{shown.length}</Chip>
              <span className="flex-1" />
              <span className="text-(--fg-faint) text-[10px]">most used first</span>
            </div>
            {!notes.data && !notes.error && <p className="px-1 text-(--fg-faint)">Reading the memory…</p>}
            {shown.map((n) => (
              <NoteRow key={n.id} note={n} on={current?.kind === "note" && current.id === n.id} onOpen={() => n.id && setOpen({ kind: "note", id: n.id })} />
            ))}
            {working.length > 0 && (
              <div className="flex h-7 items-center gap-1.5 px-1 pt-2">
                <span className={`${LABEL} text-(--hue-containers)`}>THREADS' WORKING NOTES</span>
                <Chip>{working.reduce((n, [, xs]) => n + xs.length, 0)}</Chip>
              </div>
            )}
            {working.map(([sid, xs]) => (
              <div className="flex flex-col" key={sid}>
                <span className="truncate px-1.5 pt-1 pb-0.5 text-(--fg-dim) text-[10.5px]">{thread(sid)}</span>
                {xs.map((n) => (
                  <NoteRow key={n.id} note={n} on={current?.kind === "scratch" && current.id === n.id} onOpen={() => n.id && setOpen({ kind: "scratch", id: n.id })} />
                ))}
              </div>
            ))}
          </nav>
          <div className="min-h-0 min-w-0 flex-1 overflow-auto">
            {(current?.kind === "note" || current?.kind === "scratch") && (
              <NotePane
                all={all}
                ctx={ctx}
                id={current.id}
                key={current.id}
                onChanged={reloadAll}
                onGone={() => {
                  setOpen(null);
                  reloadAll();
                }}
                onOpen={(id) => setOpen({ kind: "note", id })}
                onPromoted={(id) => {
                  reloadAll();
                  setOpen({ kind: "note", id });
                }}
                scopeWord={scopeWord}
                scopes={scopes}
                scratch={current.kind === "scratch"}
                who={who}
              />
            )}
            {current?.kind === "proposal" &&
              (() => {
                const p = pending.find((x) => x.id === current.id);
                return p ? (
                  <ProposalPane
                    all={all}
                    ctx={ctx}
                    key={p.id}
                    onDone={() => {
                      setOpen(null);
                      reloadAll();
                    }}
                    p={p}
                    scopeWord={scopeWord}
                    scopes={scopes}
                    who={who}
                  />
                ) : null;
              })()}
            {current?.kind === "new" && (
              <div className="p-5">
                <Editor
                  ctx={ctx}
                  initial={{ type: "", scope: "baker", title: "", summary: "", body: "", why: "", how_to_apply: "" }}
                  onCancel={() => setOpen(null)}
                  onSave={(f) =>
                    memoryEdit({ ...f, type: f.type || null }).then((made) => {
                      reloadAll();
                      setOpen(made?.id ? { kind: "note", id: made.id } : null);
                    })
                  }
                  saveLabel="Write it"
                  scopeWord={scopeWord}
                  scopes={scopes}
                />
              </div>
            )}
            {!current && notes.data && <p className="p-5 text-(--fg-faint)">No notes kept yet.</p>}
          </div>
        </div>
      )}
    </div>
  );
}

function NoteRow({ note, on, onOpen }: { note: NoteView; on: boolean; onOpen: () => void }) {
  return (
    <button
      className={`flex min-h-7 min-w-0 items-center gap-1.5 rounded-[5px] px-1.5 text-left ${on ? "bg-(--accent-sel) shadow-[inset_0_0_0_1px_var(--accent-border)]" : "hover:bg-(--bg-chip)"}`}
      onClick={onOpen}
      type="button"
    >
      <TypeChip type={note.type} />
      <span className="min-w-0 flex-1 truncate text-(--fg)" title={note.summary}>
        {note.title || note.name}
      </span>
      {note.stale && <Chip tone="warn">STALE</Chip>}
      <span className="shrink-0 text-(--ok) text-[10.5px] tabular-nums">▲{note.uses.helped}</span>
      <span className={`shrink-0 text-[10.5px] tabular-nums ${note.uses.hurt ? "text-(--bad)" : "text-(--fg-faint)"}`}>▼{note.uses.hurt}</span>
    </button>
  );
}

/** What a note rests on: a Heart fact by hash, or a file with its hash. */
function evidenceWord(e: unknown): string {
  if (typeof e === "string") return `fact ${e.slice(0, 7)}`;
  if (e && typeof e === "object") {
    const o = e as { path?: unknown; hash?: unknown };
    if (typeof o.path === "string") return o.path;
    if (typeof o.hash === "string") return `fact ${o.hash.slice(0, 7)}`;
  }
  return JSON.stringify(e);
}

/** The fields every note page shows, a proposal's too. */
function Body({ n, all, onOpen }: { n: Note; all: NoteView[]; onOpen?: (id: string) => void }) {
  const named = (to: string) => all.find((x) => x.name === to || x.id === to);
  const from = all.filter((x) => x.id !== n.id && (x.links ?? []).some((l) => l.to === n.name || l.to === n.id));
  const section = (label: string, text?: string) =>
    text?.trim() ? (
      <div className="flex flex-col gap-1">
        <span className={LABEL}>{label}</span>
        <div className="text-[12.5px] text-(--fg-body) leading-relaxed">
          <MessageResponse className="bakery-ai-prose">{text}</MessageResponse>
        </div>
      </div>
    ) : null;
  return (
    <>
      {n.summary && <p className="text-(--accent-fg) text-[12.5px]">{n.summary}</p>}
      {section("BODY", n.body)}
      {section("WHY", n.why)}
      {section("HOW TO APPLY", n.how_to_apply)}
      {(n.evidence ?? []).length > 0 && (
        <div className="flex flex-col gap-1">
          <span className={LABEL}>EVIDENCE</span>
          <div className="flex flex-wrap gap-1">
            {(n.evidence ?? []).map((e, i) => (
              <span className="inline-flex h-[19px] items-center rounded-full border border-(--line) px-2 font-mono text-(--link) text-[10.5px]" key={`${i}:${evidenceWord(e)}`}>
                {evidenceWord(e)}
              </span>
            ))}
          </div>
        </div>
      )}
      {((n.links ?? []).length > 0 || from.length > 0) && (
        <div className="flex flex-col gap-1">
          <span className={LABEL}>LINKS</span>
          <div className="flex flex-wrap items-center gap-1">
            {(n.links ?? []).map((l) => {
              const to = named(l.to);
              return (
                <button
                  className="inline-flex h-[19px] items-center rounded-full border border-(--line) px-2 text-(--link) text-[10.5px] disabled:text-(--fg-dim)"
                  disabled={!to?.id || !onOpen}
                  key={`${l.kind}:${l.to}`}
                  onClick={() => to?.id && onOpen?.(to.id)}
                  type="button"
                >
                  {to?.title || l.to}
                </button>
              );
            })}
            {from.length > 0 && <span className="px-1 text-(--fg-dim) text-[10.5px]">linked from</span>}
            {from.map((x) => (
              <button className="inline-flex h-[19px] items-center rounded-full border border-(--line) px-2 text-(--link) text-[10.5px]" key={x.id} onClick={() => x.id && onOpen?.(x.id)} type="button">
                {x.title || x.name}
              </button>
            ))}
          </div>
        </div>
      )}
    </>
  );
}

function Editor({
  ctx,
  initial,
  scopes,
  scopeWord,
  saveLabel,
  onSave,
  onCancel,
}: {
  ctx: ToolContext;
  initial: Fields;
  scopes: string[];
  scopeWord: (s?: string) => string;
  saveLabel: string;
  onSave: (f: Fields) => Promise<unknown>;
  onCancel: () => void;
}) {
  const [f, setF] = useState(initial);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const set = (next: Partial<Fields>) => setF((x) => ({ ...x, ...next }));
  const options = [...new Set(["bakery", "baker", ...ctx.snap.projects.map((p) => `project:${p.id}`), ...scopes, initial.scope].filter(Boolean))];
  const save = () => {
    setBusy(true);
    setError("");
    onSave(f).then(
      () => setBusy(false),
      (e) => {
        setBusy(false);
        setError(said(e));
      },
    );
  };
  return (
    <form
      className="flex max-w-[760px] flex-col gap-2"
      data-testid="memory-editor"
      onSubmit={(e) => {
        e.preventDefault();
        if (f.title.trim()) save();
      }}
    >
      <div className="flex flex-wrap gap-1.5">
        <select aria-label="Type" className={`${FIELD} h-7`} onChange={(e) => set({ type: e.target.value })} value={f.type}>
          <option value="">plain note</option>
          {Object.keys(TYPES).map((t) => (
            <option key={t} value={t}>
              {t}
            </option>
          ))}
        </select>
        <select aria-label="Scope" className={`${FIELD} h-7`} onChange={(e) => set({ scope: e.target.value })} value={f.scope}>
          {options.map((s) => (
            <option key={s} value={s}>
              {scopeWord(s)}
            </option>
          ))}
        </select>
        <input aria-label="Title" autoFocus className={`${FIELD} h-7 min-w-64 flex-1`} onChange={(e) => set({ title: e.target.value })} placeholder="title" value={f.title} />
      </div>
      <input aria-label="Summary" className={`${FIELD} h-7`} onChange={(e) => set({ summary: e.target.value })} placeholder="one line" value={f.summary} />
      <textarea aria-label="Body" className={`${FIELD} h-28`} onChange={(e) => set({ body: e.target.value })} placeholder="body" value={f.body} />
      <textarea aria-label="Why" className={`${FIELD} h-16`} onChange={(e) => set({ why: e.target.value })} placeholder="why" value={f.why} />
      <textarea aria-label="How to apply" className={`${FIELD} h-16`} onChange={(e) => set({ how_to_apply: e.target.value })} placeholder="how to apply" value={f.how_to_apply} />
      <div className="flex items-center gap-1.5">
        {error && <span className="min-w-0 flex-1 truncate text-(--bad) text-[11px]">{error}</span>}
        <span className="flex-1" />
        <button className={PLAIN} onClick={onCancel} type="button">
          Cancel
        </button>
        <button className={SOLID} disabled={busy || !f.title.trim()} type="submit">
          {saveLabel}
        </button>
      </div>
    </form>
  );
}

const fieldsOf = (n: Note): Fields => ({
  type: n.type ?? "",
  scope: n.scope ?? "baker",
  title: n.title ?? "",
  summary: n.summary ?? "",
  body: n.body ?? "",
  why: n.why ?? "",
  how_to_apply: n.how_to_apply ?? "",
});

function NotePane({
  ctx,
  id,
  all,
  scopes,
  who,
  scopeWord,
  onOpen,
  onChanged,
  onGone,
  scratch = false,
  onPromoted,
}: {
  ctx: ToolContext;
  id: string;
  all: NoteView[];
  scopes: string[];
  who: (by?: string | null) => string;
  scopeWord: (s?: string) => string;
  onOpen: (id: string) => void;
  onChanged: () => void;
  onGone: () => void;
  /** A thread's working note: Delete and Promote rather than Retire. */
  scratch?: boolean;
  onPromoted?: (id: string) => void;
}) {
  const page = useCached<NotePage>(`memory:note:${id}`, () => memoryNote({ id }));
  const [editing, setEditing] = useState(false);
  const [promoting, setPromoting] = useState(false);
  const [error, setError] = useState("");
  const n = page.data;
  if (!n) return <p className="p-5 text-(--fg-faint)">{page.error || "Reading…"}</p>;
  const retire = async () => {
    const question = scratch
      ? `Delete the working note "${n.title}"? It leaves search for good; its history stays.`
      : `Retire "${n.title}"? It leaves the wiki and every search; its history stays.`;
    if (!(await ask(question))) return;
    memoryRetire({ id }).then(onGone, (e) => setError(said(e)));
  };
  if (editing) {
    return (
      <div className="p-5">
        <Editor
          ctx={ctx}
          initial={fieldsOf(n)}
          onCancel={() => setEditing(false)}
          onSave={(f) =>
            memoryEdit({ id, ...f, type: f.type || null }).then(() => {
              setEditing(false);
              void page.reload();
              onChanged();
            })
          }
          saveLabel="Save"
          scopeWord={scopeWord}
          scopes={scopes}
        />
      </div>
    );
  }
  const u = n.uses;
  return (
    <article className="flex max-w-[780px] flex-col gap-3 p-5" data-testid="memory-page">
      <div className="flex items-center gap-2">
        <TypeChip type={n.type} />
        <h2 className="m-0 min-w-0 flex-1 truncate font-bold text-(--fg) text-[16px]">{n.title || n.name}</h2>
        {scratch && <Chip>WORKING NOTE</Chip>}
        {n.stale && <Chip tone="warn">STALE</Chip>}
        <button className={PLAIN} onClick={() => setEditing(true)} type="button">
          Edit
        </button>
        {scratch && (
          <button className={SOLID} onClick={() => setPromoting((x) => !x)} type="button">
            Promote
          </button>
        )}
        <button className={PLAIN} onClick={() => void retire()} type="button">
          {scratch ? "Delete" : "Retire"}
        </button>
      </div>
      {scratch && promoting && (
        <Promote
          ctx={ctx}
          note={n}
          onDone={(next) => onPromoted?.(next)}
          scopeWord={scopeWord}
          scopes={scopes}
        />
      )}
      <span className="-mt-2 text-(--fg-dim) text-[11px]">
        {n.name} · {scopeWord(n.scope)}
        {n.source ? ` · ${n.source}` : ""}
        {n.proposed_by ? ` · kept from ${who(n.proposed_by)}'s proposal` : n.trust === "operator" && !n.source ? " · written by you" : ""}
      </span>
      <div className="flex flex-wrap items-center gap-3.5 rounded-lg border border-(--line) bg-(--bg-raised) px-3 py-2 text-(--fg-dim) text-[11px]">
        <span>
          <b className="text-(--ok) text-[13px] tabular-nums">{u.helped}</b> helped
        </span>
        <span>
          <b className={`text-[13px] tabular-nums ${u.hurt ? "text-(--bad)" : "text-(--fg)"}`}>{u.hurt}</b> hurt
        </span>
        <span>
          <b className="text-(--fg) text-[13px] tabular-nums">{u.read_week}</b> reads this week · {u.read_total} ever
        </span>
        {u.last_used && (
          <span>
            last used {ago(u.last_used)} ago{u.last_used_by ? ` by ${who(u.last_used_by)}` : ""}
          </span>
        )}
      </div>
      <Body all={all} n={n} onOpen={onOpen} />
      {n.versions.length > 0 && (
        <div className="flex flex-col gap-1">
          <span className={LABEL}>HISTORY</span>
          {n.versions.map((v) => (
            <span className="flex gap-2 text-(--fg-dim) text-[11px]" key={v.event_hash ?? v.version}>
              <span className="min-w-0 flex-1 truncate">
                v{v.version} · {v.written_by === "operator" ? "you" : who(v.written_by)}
                {v.proposed_by && v.version === 1 ? `, kept from ${who(v.proposed_by)}'s proposal` : ""}
              </span>
              {v.written_at && <span className="shrink-0">{ago(v.written_at)}</span>}
            </span>
          ))}
        </div>
      )}
      {error && <p className="text-(--bad) text-[11px]">{error}</p>}
    </article>
  );
}

/** A working note made shared: the operator says where it goes (and as what);
 *  it keeps its id and versions, kept and written by the operator. */
function Promote({
  ctx,
  note,
  scopes,
  scopeWord,
  onDone,
}: {
  ctx: ToolContext;
  note: Note;
  scopes: string[];
  scopeWord: (s?: string) => string;
  onDone: (id: string) => void;
}) {
  const own = note.scope ?? "";
  const options = [...new Set(["baker", "bakery", ...ctx.snap.projects.map((p) => `project:${p.id}`), ...scopes, own].filter(Boolean))];
  const [scope, setScope] = useState(options.includes("baker") ? "baker" : (options[0] ?? "baker"));
  const [type, setType] = useState(note.type || "project");
  const [error, setError] = useState("");
  const go = () =>
    memoryPromote({ id: note.id ?? "", scope, type }).then(
      (made) => onDone(made.id ?? note.id ?? ""),
      (e) => setError(said(e)),
    );
  return (
    <div className="flex flex-wrap items-center gap-1.5 rounded-lg border border-(--accent-border) bg-(--bg-raised) p-2" data-testid="memory-promote">
      <span className={LABEL}>PROMOTE TO</span>
      <select aria-label="Scope" className={`${FIELD} h-7`} onChange={(e) => setScope(e.target.value)} value={scope}>
        {options.map((s) => (
          <option key={s} value={s}>
            {s === own ? `${scopeWord(s)} (its thread, shared)` : scopeWord(s)}
          </option>
        ))}
      </select>
      <select aria-label="As" className={`${FIELD} h-7`} onChange={(e) => setType(e.target.value)} value={type}>
        {Object.keys(TYPES).map((t) => (
          <option key={t} value={t}>
            {t}
          </option>
        ))}
      </select>
      <button className={SOLID} onClick={() => void go()} type="button">
        Promote
      </button>
      {error && <span className="min-w-0 flex-1 truncate text-(--bad) text-[11px]">{error}</span>}
    </div>
  );
}

function ProposalPane({
  ctx,
  p,
  all,
  scopes,
  who,
  scopeWord,
  onDone,
}: {
  ctx: ToolContext;
  p: Proposal;
  all: NoteView[];
  scopes: string[];
  who: (by?: string | null) => string;
  scopeWord: (s?: string) => string;
  onDone: () => void;
}) {
  const [editing, setEditing] = useState(false);
  const [declining, setDeclining] = useState(false);
  const [why, setWhy] = useState("");
  const [error, setError] = useState("");
  const n = p.note ?? {};
  const target = all.find((x) => x.id === p.target);
  const review = (decision: "keep" | "decline", extra: { edits?: Partial<Fields>; why?: string } = {}) =>
    memoryReview({ proposal: p.id ?? "", decision, ...extra }).then(onDone, (e) => {
      setError(said(e));
      throw e;
    });
  if (editing) {
    return (
      <div className="p-5">
        <Editor
          ctx={ctx}
          initial={fieldsOf(n)}
          onCancel={() => setEditing(false)}
          onSave={(f) => review("keep", { edits: { ...f, type: f.type || undefined } })}
          saveLabel="Keep as edited"
          scopeWord={scopeWord}
          scopes={scopes}
        />
      </div>
    );
  }
  return (
    <article className="flex max-w-[780px] flex-col gap-3 p-5" data-testid="memory-proposal">
      <div className="flex items-center gap-2">
        <Chip tone="warn">TO KEEP</Chip>
        <TypeChip type={n.type} />
        <h2 className="m-0 min-w-0 flex-1 truncate font-bold text-(--fg) text-[16px]">{n.title || "Untitled"}</h2>
      </div>
      <span className="-mt-2 text-(--fg-dim) text-[11px]">
        proposed by {who(p.proposed_by)}
        {p.proposed_at ? ` · ${ago(p.proposed_at)} ago` : ""} · {scopeWord(n.scope)} ·{" "}
        {p.action === "update" ? `a new version of "${target?.title ?? "a note"}"` : p.action === "supersede" ? `replacing "${target?.title ?? "a note"}"` : "a new note"}
      </span>
      <Body all={all} n={n} />
      <div className="flex flex-wrap items-center gap-1.5">
        <button className={SOLID} onClick={() => void review("keep").catch(() => {})} type="button">
          Keep
        </button>
        <button className={PLAIN} onClick={() => setEditing(true)} type="button">
          Edit and keep
        </button>
        {declining ? (
          <>
            <input aria-label="Why not" autoFocus className={`${FIELD} h-7 min-w-48 flex-1`} onChange={(e) => setWhy(e.target.value)} placeholder="why not" value={why} />
            <button className={PLAIN} onClick={() => void review("decline", { why: why.trim() || undefined }).catch(() => {})} type="button">
              Decline
            </button>
          </>
        ) : (
          <button className={PLAIN} onClick={() => setDeclining(true)} type="button">
            Decline
          </button>
        )}
      </div>
      {error && <p className="text-(--bad) text-[11px]">{error}</p>}
    </article>
  );
}

function Timeline({ who, onOpen }: { who: (by?: string | null) => string; onOpen: (noteId: string) => void }) {
  const t = useCached<MemoryTimeline>("memory:timeline", () => memoryTimeline({ limit: 200 }));
  const days = useMemo(() => {
    const out: [string, MemoryTimeline["moments"]][] = [];
    for (const m of t.data?.moments ?? []) {
      const day = new Date(m.at).toLocaleDateString([], { weekday: "long", month: "short", day: "numeric" });
      const last = out.at(-1);
      if (last && last[0] === day) last[1].push(m);
      else out.push([day, [m]]);
    }
    return out;
  }, [t.data]);
  return (
    <div className="min-h-0 flex-1 overflow-auto p-4" data-testid="memory-timeline">
      <div className="flex max-w-[760px] flex-col gap-1">
        <div className="flex justify-end">
          <Freshness at={t.at} busy={t.busy} error={t.error} onReload={() => void t.reload()} />
        </div>
        {!t.data && !t.error && <p className="text-(--fg-faint)">Reading…</p>}
        {days.map(([day, moments]) => (
          <section className="flex flex-col gap-1" key={day}>
            <div className="pt-2.5 pb-1 font-bold text-(--fg-dim) text-[10px] uppercase tracking-[.08em]">{day}</div>
            {moments.map((m, i) => (
              <button
                className="flex min-w-0 items-center gap-2.5 rounded-lg border border-(--line) bg-(--bg-raised) px-2.5 py-1.5 text-left disabled:cursor-default"
                disabled={!m.note}
                key={`${m.at}:${i}`}
                onClick={() => m.note && onOpen(m.note)}
                type="button"
              >
                <span className="w-16 shrink-0 font-bold text-[9.5px] uppercase tracking-[.06em]" style={{ color: MOMENT[m.what] ?? "var(--fg-dim)" }}>
                  {m.what}
                </span>
                <span className="min-w-0 flex-1 truncate text-(--fg)">{m.title}</span>
                <span className="shrink-0 text-(--fg-dim) text-[10.5px]">{who(m.by)}</span>
                <span className="w-16 shrink-0 text-right text-(--fg-faint) text-[10.5px]">{new Date(m.at).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" })}</span>
              </button>
            ))}
          </section>
        ))}
      </div>
    </div>
  );
}
