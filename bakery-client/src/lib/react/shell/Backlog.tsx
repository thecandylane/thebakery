// The backlog (Round 12, the operator's B: "just a agent forward to-do"),
// as a board: Queued, Doing, Done and Dropped. Dragging a card across
// changes its status; dragging within Queued puts it in order. The operator
// adds, changes, orders and removes; agents add theirs (marked as theirs,
// their thread linked) and move them along. The Bakery's own, or a
// project's.

import { useEffect, useState } from "react";
import { BotIcon, PlusIcon } from "lucide-react";
import {
  type Backlog,
  backlogAdd,
  backlogEdit,
  backlogList,
  backlogMove,
  backlogRemove,
  backlogStatus,
  type BacklogItem,
  type Status2,
} from "$lib/api.generated";
import { ask } from "$lib/ask";
import { ago } from "$lib/conversations";
import { onEnvelope } from "$lib/events";
import { keep, patchKept, useCached } from "./cache";
import { PLAIN, SOLID } from "./Internet";
import { Chip, Freshness, said, type ToolContext } from "./ui";

const KEY = "backlog";
const DRAG = "application/x-bakery-backlog";
const COLUMNS: [Status2, string, string][] = [
  ["queued", "QUEUED", "var(--warn)"],
  ["doing", "DOING", "var(--ok)"],
  ["done", "DONE", "var(--fg-dim)"],
  ["dropped", "DROPPED", "var(--fg-faint)"],
];
const FIELD = "h-6 min-w-0 rounded-[5px] border border-(--line) bg-(--bg-app) px-2 text-[11px] text-(--fg-body)";

/** An item changed or left, and the whole order: the kept list follows. */
function applyUpdate(b: Backlog, id: string, item: BacklogItem | null, order: string[] | null): Backlog {
  const rest = b.items.filter((i) => i.id !== id);
  const items = item ? [...rest, item] : rest;
  if (!order) return { items: items.sort((x, y) => x.order - y.order) };
  const at = new Map(order.map((oid, n) => [oid, n]));
  return { items: items.map((i) => ({ ...i, order: at.get(i.id) ?? i.order })).sort((x, y) => x.order - y.order) };
}
const upsert = (item: BacklogItem) => patchKept<Backlog>(KEY, (b) => applyUpdate(b, item.id, item, null));

/** Every item the operator reaches, kept like a panel's data and followed
 *  live (backlog.updated). */
export function useBacklog() {
  const c = useCached<Backlog>(KEY, () => backlogList({}));
  useEffect(
    () =>
      onEnvelope("backlog.updated", (e) => {
        const id = typeof e.id === "string" ? e.id : "";
        const order = Array.isArray(e.order) ? (e.order as string[]) : null;
        patchKept<Backlog>(KEY, (b) => applyUpdate(b, id, (e.item as BacklogItem | null | undefined) ?? null, order));
      }),
    [],
  );
  return c;
}

export function BacklogBoard({ ctx }: { ctx: ToolContext }) {
  const { data, error, busy, at, reload } = useBacklog();
  const [scope, setScope] = useState<string>("all");
  const [adding, setAdding] = useState(false);
  const [open, setOpen] = useState<string | null>(null);
  const [problem, setProblem] = useState("");
  const name = (sid: string) => ctx.snap.conversations.find((c) => c.streamId === sid)?.title ?? ctx.snap.archived.find((c) => c.streamId === sid)?.title ?? "a thread";
  const projectName = (id: string | null | undefined) => (id ? (ctx.snap.projects.find((p) => p.id === id)?.name ?? "a project") : "Bakery");
  const items = (data?.items ?? []).filter((i) => scope === "all" || (scope === "bakery" ? !i.project_id : i.project_id === scope));
  const scopes = [...new Set((data?.items ?? []).map((i) => i.project_id ?? ""))].filter(Boolean);
  const failed = (e: unknown) => setProblem(said(e));

  const drop = (id: string, status: Status2, before: string | null) => {
    const item = data?.items.find((i) => i.id === id);
    if (!item) return;
    setProblem("");
    const moved = () => {
      if (status === "queued" && before !== id) backlogMove({ id, before }).then((b) => keep(KEY, b), failed);
    };
    if (item.status !== status) {
      backlogStatus({ id, status }).then((next) => {
        upsert(next);
        moved();
      }, failed);
    } else moved();
  };
  const remove = async (i: BacklogItem) => {
    if (!(await ask(`Remove "${i.title}" from the backlog?`))) return;
    backlogRemove({ id: i.id }).then((b) => keep(KEY, b), failed);
  };

  return (
    <div className="flex h-full min-h-0 flex-col bg-(--bg-panel) text-xs" data-testid="backlog">
      <div className="flex h-9 shrink-0 items-center gap-1.5 border-(--line-soft) border-b bg-(--bg-raised) px-2.5">
        <span className="flex overflow-hidden rounded-[5px] border border-(--line)">
          {[["all", "All"], ["bakery", "Bakery"], ...scopes.map((id) => [id, projectName(id)])].map(([id, label], n) => (
            <button
              className={`h-6 px-2 text-[11px] ${n > 0 ? "border-(--line) border-l" : ""} ${scope === id ? "bg-(--accent-sel) font-bold text-(--fg)" : "text-(--fg-dim) hover:text-(--fg-2)"}`}
              key={id}
              onClick={() => setScope(id)}
              type="button"
            >
              {label}
            </button>
          ))}
        </span>
        {problem && (
          <span className="min-w-0 truncate text-(--bad) text-[11px]" title={problem}>
            {problem}
          </span>
        )}
        <span className="flex-1" />
        <Freshness at={at} busy={busy} error={error} onReload={() => void reload()} />
        <button className={SOLID} onClick={() => setAdding((a) => !a)} type="button">
          <PlusIcon className="mr-1 size-3" />
          Add
        </button>
      </div>
      {adding && <AddForm ctx={ctx} onDone={() => setAdding(false)} scope={scope} />}
      {!data && !error && <p className="p-3 text-(--fg-faint)">Reading the backlog…</p>}
      <div className="grid min-h-0 flex-1 grid-cols-4">
        {COLUMNS.map(([status, label, hue]) => {
          const cards = items.filter((i) => i.status === status);
          return (
            <section
              aria-label={label}
              className="flex min-h-0 min-w-0 flex-col border-(--line) border-r last:border-r-0"
              key={status}
              onDragOver={(e) => e.dataTransfer.types.includes(DRAG) && e.preventDefault()}
              onDrop={(e) => {
                e.preventDefault();
                drop(e.dataTransfer.getData(DRAG), status, null);
              }}
            >
              <div className="flex h-8 shrink-0 items-center gap-1.5 border-(--line) border-b px-3 font-bold text-[10.5px] tracking-[.08em]" style={{ color: hue, boxShadow: `inset 0 2px 0 ${hue}` }}>
                {label}
                <Chip>{cards.length}</Chip>
              </div>
              <div className="flex min-h-0 flex-1 flex-col gap-1.5 overflow-auto p-2">
                {cards.map((i, n) => (
                  <Card
                    agent={i.added_by === "operator" ? null : name(i.added_by)}
                    expanded={open === i.id}
                    item={i}
                    key={i.id}
                    number={status === "queued" ? n + 1 : null}
                    onDrop={(id) => drop(id, status, i.id)}
                    onRemove={() => void remove(i)}
                    onToggle={() => setOpen((o) => (o === i.id ? null : i.id))}
                    project={projectName(i.project_id)}
                    threadName={name}
                  />
                ))}
              </div>
            </section>
          );
        })}
      </div>
    </div>
  );
}

function AddForm({ ctx, scope, onDone }: { ctx: ToolContext; scope: string; onDone: () => void }) {
  const [title, setTitle] = useState("");
  const [why, setWhy] = useState("");
  const [where, setWhere] = useState(scope === "all" ? "bakery" : scope);
  const [error, setError] = useState("");
  const add = () =>
    backlogAdd({ title: title.trim(), why: why.trim(), scope: where }).then(
      (item) => {
        upsert(item);
        onDone();
      },
      (e) => setError(said(e)),
    );
  return (
    <form
      className="flex flex-wrap items-center gap-1.5 border-(--line-soft) border-b px-2.5 py-1.5"
      onSubmit={(e) => {
        e.preventDefault();
        if (title.trim()) void add();
      }}
    >
      <input aria-label="What we need" autoFocus className={`${FIELD} w-72`} onChange={(e) => setTitle(e.target.value)} placeholder="what we need" value={title} />
      <input aria-label="Why" className={`${FIELD} min-w-48 flex-1`} onChange={(e) => setWhy(e.target.value)} placeholder="why" value={why} />
      <select aria-label="For" className={FIELD} onChange={(e) => setWhere(e.target.value)} value={where}>
        <option value="bakery">Bakery</option>
        {ctx.snap.projects.map((p) => (
          <option key={p.id} value={p.id}>
            {p.name}
          </option>
        ))}
      </select>
      {error && <span className="text-(--bad) text-[11px]">{error}</span>}
      <button className={PLAIN} onClick={onDone} type="button">
        Cancel
      </button>
      <button className={SOLID} disabled={!title.trim()} type="submit">
        Add
      </button>
    </form>
  );
}

function Card({
  item,
  number,
  agent,
  project,
  expanded,
  threadName,
  onToggle,
  onDrop,
  onRemove,
}: {
  item: BacklogItem;
  number: number | null;
  /** The thread of the agent that added it; null for the operator's. */
  agent: string | null;
  project: string;
  expanded: boolean;
  threadName: (sid: string) => string;
  onToggle: () => void;
  onDrop: (id: string) => void;
  onRemove: () => void;
}) {
  const [editing, setEditing] = useState(false);
  const [title, setTitle] = useState(item.title);
  const [why, setWhy] = useState(item.why);
  const [error, setError] = useState("");
  const quiet = item.status === "done" || item.status === "dropped";
  const save = () =>
    backlogEdit({ id: item.id, title: title.trim(), why: why.trim() }).then(
      (next) => {
        upsert(next);
        setEditing(false);
      },
      (e) => setError(said(e)),
    );
  const open = (sid: string) => window.dispatchEvent(new CustomEvent("bakery:open-conversation", { detail: { streamId: sid } }));
  return (
    <div
      className={`flex flex-col gap-1.5 rounded-lg border border-(--line) bg-(--bg-raised) p-2 ${quiet ? "opacity-70" : ""} ${expanded ? "shadow-[inset_3px_0_0_var(--accent)]" : ""}`}
      data-testid="backlog-card"
      draggable={!editing}
      onDragOver={(e) => e.dataTransfer.types.includes(DRAG) && e.preventDefault()}
      onDragStart={(e) => e.dataTransfer.setData(DRAG, item.id)}
      onDrop={(e) => {
        e.preventDefault();
        e.stopPropagation();
        const id = e.dataTransfer.getData(DRAG);
        if (id && id !== item.id) onDrop(id);
      }}
    >
      {editing ? (
        <>
          <input aria-label="What we need" className={FIELD} onChange={(e) => setTitle(e.target.value)} value={title} />
          <textarea aria-label="Why" className={`${FIELD} h-14 py-1`} onChange={(e) => setWhy(e.target.value)} value={why} />
          <div className="flex items-center gap-1.5">
            {error && <span className="min-w-0 flex-1 truncate text-(--bad) text-[11px]">{error}</span>}
            <span className="flex-1" />
            <button className={PLAIN} onClick={() => setEditing(false)} type="button">
              Cancel
            </button>
            <button className={SOLID} disabled={!title.trim()} onClick={() => void save()} type="button">
              Save
            </button>
          </div>
        </>
      ) : (
        <button className="flex items-start gap-1.5 text-left" onClick={onToggle} type="button">
          {number !== null && <span className="w-4 shrink-0 text-right font-bold text-(--fg-dim)">{number}</span>}
          <span className="min-w-0 flex-1 font-semibold text-(--fg) leading-snug">{item.title}</span>
        </button>
      )}
      {!editing && item.why && <p className={`text-(--fg-dim) text-[11px] leading-snug ${expanded ? "" : "line-clamp-2"}`}>{item.why}</p>}
      <div className="flex flex-wrap items-center gap-1">
        {agent ? (
          <span className="inline-flex h-[18px] items-center gap-1 rounded px-1.5 font-bold text-(--violet) text-[10px]" style={{ background: "color-mix(in srgb, var(--violet) 12%, transparent)" }} title="Added by an agent">
            <BotIcon className="size-3" />
            {agent}
          </span>
        ) : (
          <span className="inline-flex h-[18px] items-center rounded bg-(--accent-sel) px-1.5 font-bold text-(--accent-fg) text-[10px]">you</span>
        )}
        <Chip>{project}</Chip>
        {item.threads
          .filter((t) => t !== item.added_by)
          .map((t) => (
            <button className="text-(--link) text-[10.5px] hover:underline" key={t} onClick={() => open(t)} type="button">
              {threadName(t)} ›
            </button>
          ))}
        {agent && (
          <button className="text-(--link) text-[10.5px] hover:underline" onClick={() => open(item.added_by)} type="button">
            its thread ›
          </button>
        )}
      </div>
      {expanded && !editing && (
        <>
          <ol className="flex flex-col gap-0.5 border-(--line-soft) border-t pt-1.5 text-[10.5px] text-(--fg-dim)">
            {item.history.map((h, n) => (
              <li className="flex gap-1.5" key={`${h.at}:${n}`}>
                <span className="min-w-0 flex-1 truncate">
                  {h.what} by {h.by === "operator" ? "you" : threadName(h.by)}
                </span>
                <span className="shrink-0">{ago(h.at)}</span>
              </li>
            ))}
          </ol>
          <div className="flex justify-end gap-1.5">
            <button className={PLAIN} onClick={() => setEditing(true)} type="button">
              Edit
            </button>
            <button className={PLAIN} onClick={onRemove} type="button">
              Remove
            </button>
          </div>
        </>
      )}
    </div>
  );
}
