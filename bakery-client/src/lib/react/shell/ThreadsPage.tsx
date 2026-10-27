// The Threads page (Round 10, the rail's Threads): every thread on this side
// of the Bakery, by workspace in Bakery mode, the Baker's and its projects'
// in Baker mode; most urgent first, by state and by a search. A thread opens
// in its own workspace.

import { type ReactNode, useEffect, useState } from "react";
import { ChefHatIcon, FolderIcon, GitBranchIcon, LogInIcon, MessageSquareIcon, SearchIcon } from "lucide-react";
import { type ClaudeSession, type Destination, moveInImport, moveInSessions } from "$lib/api.generated";
import { ask } from "$lib/ask";
import { ago, STATUSES, type Conversation } from "$lib/conversations";
import { isTauri } from "$lib/desktop";
import { bytes } from "./containersModel";
import { membersOf } from "./floorModel";
import * as L from "./layout";
import { ThreadMenu } from "./ThreadMenu";
import { Chip, said, StatusDot, type ToolContext, workspaceColor } from "./ui";

type Filter = "all" | "needs" | "working" | "failed" | "done";
const FILTERS: [Filter, string][] = [
  ["all", "All"],
  ["needs", "Needs you"],
  ["working", "Working"],
  ["failed", "Failed"],
  ["done", "Done"],
];
const inFilter = (c: Conversation, f: Filter) =>
  f === "all" ||
  (f === "needs" ? c.status === "needs_input" : f === "working" ? c.status === "working" : f === "failed" ? c.status === "failed" : !["needs_input", "working", "failed"].includes(c.status));
/** Most urgent first, the order every board uses; then the latest. */
const order = (a: Conversation, b: Conversation) =>
  STATUSES.findIndex((s) => s.status === a.status) - STATUSES.findIndex((s) => s.status === b.status) || (a.lastTs < b.lastTs ? 1 : -1);
const GROUP = (on: boolean) =>
  `flex h-6 shrink-0 items-center gap-1.5 rounded-[5px] border px-2 text-[11px] ${on ? "border-(--accent-border) bg-(--accent-sel) font-bold text-(--fg)" : "border-transparent text-(--fg-dim) hover:bg-(--bg-chip) hover:text-(--fg-2)"}`;

const open = (c: Conversation) =>
  window.dispatchEvent(new CustomEvent("bakery:open-conversation", { detail: { streamId: c.streamId, bakerThread: c.bakerThread } }));

/** A folder as the operator reads it: their home as ~. */
const shortPath = (p: string) => p.replace(/^\/Users\/[^/]+/, "~");

/** A session by the name the operator gave it in Claude Code, first. */
const nameOf = (s: ClaudeSession) => s.custom_title || s.title || s.session_id;
/** Where a session can go on, in the operator's words. */
const placeLabel = (d: Destination, suggested: boolean) =>
  `${d.label}${suggested ? " · suggested" : ""}${d.taken_by ? ` · carries on “${d.taken_by}”` : ""}`;
const KIND_ICON: Record<Destination["kind"], ReactNode> = {
  workspace: <FolderIcon className="size-3 shrink-0 text-(--hue-project)" />,
  worker: <GitBranchIcon className="size-3 shrink-0 text-(--violet)" />,
  baker: <ChefHatIcon className="size-3 shrink-0 text-(--baker-solid)" />,
  baker_thread: <MessageSquareIcon className="size-3 shrink-0 text-(--hue-threads)" />,
};
const openMoved = (streamId: string) => window.dispatchEvent(new CustomEvent("bakery:open-conversation", { detail: { streamId, bakerThread: true } }));
const PICK = "h-6 min-w-0 max-w-full rounded-[5px] border border-(--line) bg-(--bg-app) px-1.5 text-(--fg-body)";

/** The operator's Claude Code sessions, to move into the Bakery: each one
 *  goes on where the operator picks, the place it fits best suggested: a
 *  new workspace (its orc), a workspace's orc, the Baker's own
 *  conversation, the conversation already in a folder it worked in, or a
 *  worker or Baker thread made there. It opens once it's in. */
function MoveIn({ ctx }: { ctx: ToolContext }) {
  const [sessions, setSessions] = useState<ClaudeSession[] | null>(null);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState("");
  const [picked, setPicked] = useState<Record<string, string>>({});
  /** A new worker's workspace, by its orc, when it could go under more than one. */
  const [under, setUnder] = useState<Record<string, string>>({});
  const [refused, setRefused] = useState<Record<string, string>>({});
  const read = () =>
    moveInSessions().then(
      (r) => {
        setSessions(r.sessions);
        setError("");
      },
      (e) => setError(said(e)),
    );
  useEffect(() => void read(), []);
  const clear = (id: string) => setRefused(({ [id]: _, ...rest }) => rest);
  /** What a moved-in session gained since, copied into the conversation
   *  that carries it on: the same import, to where it moved. */
  const bringNew = (s: ClaudeSession) => {
    if (!s.suggested_destination) return;
    setBusy(s.session_id);
    clear(s.session_id);
    moveInImport({ transcript: s.path, destination: s.suggested_destination }).then(
      () => {
        setBusy("");
        ctx.store.refresh();
        void read();
      },
      (e) => {
        setBusy("");
        setRefused((m) => ({ ...m, [s.session_id]: said(e) }));
        void read();
      },
    );
  };
  const bring = async (s: ClaudeSession, to: Destination, orc: string | null) => {
    if (!(await ask(`Move “${nameOf(s)}” into ${to.label}?\n\nStop this session in VS Code first; anything it does afterwards stays outside the Bakery.`))) return;
    setBusy(s.session_id);
    clear(s.session_id);
    moveInImport({ transcript: s.path, destination: to.key, orc }).then(
      (r) => {
        setBusy("");
        ctx.store.refresh();
        void read();
        if (to.kind === "workspace" && !to.stream_id) {
          // A new workspace shows at once, opening on its orc.
          ctx.setLayout((l) => L.showNewOrc(l, { streamId: r.stream_id, title: nameOf(s) }));
        } else if (to.kind === "worker" && !to.stream_id && orc && ctx.layout.orcs.some((w) => w.orc === orc)) {
          // A new worker opens in its orc's workspace.
          ctx.setLayout((l) => L.openPanel(l, L.orcWorkspace(orc), r.stream_id));
        } else {
          openMoved(r.stream_id);
        }
      },
      (e) => {
        setBusy("");
        setRefused((m) => ({ ...m, [s.session_id]: said(e) }));
        void read();
      },
    );
  };
  return (
    <section aria-label="Move in from Claude Code" className="border-(--line) border-b" data-testid="move-in">
      <div className="flex h-7 items-center gap-2 bg-(--bg-app) px-3 font-bold text-(--accent) text-[10px] uppercase tracking-[.06em]">
        <LogInIcon className="size-3" />
        Claude Code sessions
        {error && <span className="min-w-0 truncate font-normal text-(--bad) normal-case tracking-normal">{error}</span>}
      </div>
      {sessions === null && !error && <p className="px-3 py-2 text-(--fg-faint)">Reading…</p>}
      <div className="max-h-72 overflow-auto">
        {sessions?.map((s) => {
          const to = s.destinations.find((d) => d.key === (picked[s.session_id] ?? s.suggested_destination));
          // A new worker goes under a workspace: its orc, picked when there's more than one.
          const orcs = to?.kind === "worker" && !to.stream_id ? to.orcs : [];
          const orc = under[s.session_id] ?? (orcs.length === 1 ? orcs[0].stream_id : null);
          const movedTo = s.moved_in ? ctx.snap.conversations.find((c) => c.streamId === s.moved_in) : undefined;
          return (
            <div className="border-(--line-row) border-b px-3" key={s.session_id}>
              <div className="flex h-8 min-w-0 items-center gap-2.5">
                <span className="w-72 min-w-0 shrink-0 truncate text-(--fg)" title={nameOf(s)}>
                  {nameOf(s)}
                </span>
                <span className="flex min-w-0 flex-1 items-center gap-1.5 text-[11px]">
                  {s.moved_in ? (
                    <span className="min-w-0 truncate text-(--fg-2)">{movedTo?.title}</span>
                  ) : s.destinations.length === 0 ? (
                    <span className="min-w-0 truncate font-mono text-(--fg-faint)" title={s.cwd ?? ""}>
                      {s.cwd ? shortPath(s.cwd) : "no folder recorded"}
                    </span>
                  ) : (
                    <>
                      {to && KIND_ICON[to.kind]}
                      {s.destinations.length === 1 && to ? (
                        <span className="min-w-0 truncate text-(--fg-2)" title={to.worktree ?? undefined}>
                          {placeLabel(to, false)}
                        </span>
                      ) : (
                        <select
                          aria-label="Where it goes on"
                          className={PICK}
                          onChange={(e) => {
                            setPicked((m) => ({ ...m, [s.session_id]: e.target.value }));
                            clear(s.session_id);
                          }}
                          title={to?.worktree ?? undefined}
                          value={to?.key ?? ""}
                        >
                          {!to && (
                            <option disabled value="">
                              Pick a place
                            </option>
                          )}
                          {s.destinations.map((d) => (
                            <option key={d.key} value={d.key}>
                              {placeLabel(d, d.key === s.suggested_destination)}
                            </option>
                          ))}
                        </select>
                      )}
                      {orcs.length > 1 && (
                        <select
                          aria-label="Under which workspace"
                          className={PICK}
                          onChange={(e) => {
                            setUnder((m) => ({ ...m, [s.session_id]: e.target.value }));
                            clear(s.session_id);
                          }}
                          value={orc ?? ""}
                        >
                          {!orc && (
                            <option disabled value="">
                              Pick a workspace
                            </option>
                          )}
                          {orcs.map((o) => (
                            <option key={o.stream_id} value={o.stream_id}>
                              {o.title}
                            </option>
                          ))}
                        </select>
                      )}
                    </>
                  )}
                </span>
                <span className="shrink-0 text-(--fg-dim) text-[11px]">
                  {bytes(s.bytes)}
                  {s.compactions > 0 && ` · compacted ${s.compactions}×`}
                </span>
                <span className="w-10 shrink-0 text-right text-(--fg-dim) text-[11px]">{ago(s.modified)}</span>
                {s.moved_in ? (
                  <>
                    {s.can_bring_in && (
                      <button
                        className="h-6 shrink-0 rounded-[5px] border border-(--accent-fg) bg-(image:--grad-accent) px-2 font-bold text-(--fg-on-solid) text-[11px] disabled:opacity-50"
                        disabled={busy !== ""}
                        onClick={() => void bringNew(s)}
                        type="button"
                      >
                        {busy === s.session_id ? "Bringing in…" : "Bring in what's new"}
                      </button>
                    )}
                    <button className="h-6 w-20 shrink-0 rounded-[5px] text-(--ok) text-[11px] hover:underline" onClick={() => openMoved(s.moved_in as string)} type="button">
                      Moved in ›
                    </button>
                  </>
                ) : (
                  to && (
                    <button
                      className="h-6 w-20 shrink-0 rounded-[5px] border border-(--accent-fg) bg-(image:--grad-accent) font-bold text-(--fg-on-solid) text-[11px] disabled:opacity-50"
                      disabled={busy !== "" || !!to.taken_by || (orcs.length > 1 && !orc)}
                      onClick={() => void bring(s, to, orc)}
                      title={to.taken_by ? `${to.label} carries on “${to.taken_by}”: pick another place` : undefined}
                      type="button"
                    >
                      {busy === s.session_id ? "Importing…" : "Import"}
                    </button>
                  )
                )}
              </div>
              {refused[s.session_id] && <p className="pb-1.5 text-(--bad) text-[11px]">{refused[s.session_id]}</p>}
            </div>
          );
        })}
      </div>
    </section>
  );
}

export function ThreadsPage({ ctx }: { ctx: ToolContext }) {
  const { snap, layout } = ctx;
  const [filter, setFilter] = useState<Filter>("all");
  const [query, setQuery] = useState("");
  const [moving, setMoving] = useState(false);
  const branchOf = new Map(snap.projects.flatMap((p) => p.checkouts.map((c) => [c.streamId, c.branch] as const)));
  const groups: { id: string; name: string; color: string; threads: Conversation[] }[] =
    layout.mode === "baker"
      ? [
          { id: L.BAKER, name: "Baker", color: "var(--baker-solid)", threads: snap.conversations.filter((c) => c.bakerThread) },
          ...layout.orcs
            .filter((w) => !w.projectId)
            .map((w, i) => ({ id: w.id, name: w.name, color: workspaceColor(i), threads: membersOf(w, snap.projects, snap.conversations) })),
          ...snap.projects
            .filter((p) => layout.bakerProjects.includes(p.id))
            .map((p) => ({ id: p.id, name: p.name, color: "var(--hue-project)", threads: snap.conversations.filter((c) => !c.bakerThread && p.streamIds.includes(c.streamId)) })),
        ]
      : [
          ...layout.workspaces.map((w, i) => ({ id: w.id, name: w.name, color: workspaceColor(i), threads: membersOf(w, snap.projects, snap.conversations) })),
          ...layout.orcs
            .filter((w) => !!w.projectId)
            .map((w, i) => ({ id: w.id, name: w.name, color: workspaceColor(i), threads: membersOf(w, snap.projects, snap.conversations) })),
        ];
  const all = groups.flatMap((g) => g.threads);
  const q = query.trim().toLowerCase();
  const shown = (c: Conversation) => inFilter(c, filter) && (!q || c.title.toLowerCase().includes(q) || (branchOf.get(c.streamId) ?? "").toLowerCase().includes(q));

  return (
    <div className="flex h-full min-h-0 flex-col bg-(--bg-panel) text-xs" data-testid="threads-page">
      <div className="flex h-9 shrink-0 items-center gap-1.5 border-(--line-soft) border-b bg-(--bg-raised) px-2.5">
        {FILTERS.map(([f, label]) => (
          <button aria-pressed={f === filter} className={GROUP(f === filter)} key={f} onClick={() => setFilter(f)} type="button">
            {label}
            <Chip tone={f === "needs" && all.some((c) => inFilter(c, f)) ? "warn" : f === filter ? "accent" : "neutral"}>{all.filter((c) => inFilter(c, f)).length}</Chip>
          </button>
        ))}
        <label className="ml-1.5 flex h-6 w-56 items-center gap-1.5 rounded-[5px] border border-(--line) bg-(--bg-app) px-2 text-(--fg-dim)">
          <SearchIcon className="size-3 shrink-0" />
          <input aria-label="Find a thread" className="min-w-0 flex-1 bg-transparent text-(--fg-body) outline-none" onChange={(e) => setQuery(e.target.value)} value={query} />
        </label>
        <span className="flex-1" />
        {/* The Mac's alone: the sessions and folders are this Mac's. */}
        {!isTauri() && (
          <button aria-pressed={moving} className={GROUP(moving)} data-testid="move-in-toggle" onClick={() => setMoving((m) => !m)} type="button">
            <LogInIcon className="size-3" />
            Move in from Claude Code
          </button>
        )}
      </div>
      {moving && <MoveIn ctx={ctx} />}
      <div className="min-h-0 flex-1 overflow-auto">
        {groups.map((g) => {
          const rows = g.threads.filter(shown).sort(order);
          if (rows.length === 0) return null;
          return (
            <section aria-label={g.name} key={g.id}>
              <div className="flex h-7 items-center gap-2 bg-(--bg-app) px-3 font-bold text-(--fg-dim) text-[10px] uppercase tracking-[.06em]">
                <span className="size-2 rounded-sm" style={{ background: g.color }} />
                {g.name}
              </div>
              {rows.map((c) => {
                const branch = branchOf.get(c.streamId);
                return (
                  <div className="group flex h-8 min-w-0 items-center border-(--line-row) border-b pr-2 hover:bg-(--bg-chip)" key={c.streamId}>
                    <button
                      className="flex h-full min-w-0 flex-1 items-center gap-2.5 pl-3 text-left"
                      onClick={() => open(c)}
                      title={c.note || c.title}
                      type="button"
                    >
                      <StatusDot status={c.status} streamId={c.streamId} />
                      <span className="w-56 min-w-0 shrink-0 truncate text-(--fg)">{c.title}</span>
                      {branch && (
                        <span className="flex min-w-0 items-center gap-1 truncate text-(--fg-2) text-[11px]">
                          <GitBranchIcon className="size-3 shrink-0 text-(--violet)" />
                          {branch}
                        </span>
                      )}
                      <span className="flex-1" />
                      {c.status === "failed" && <Chip tone="bad">FAILED</Chip>}
                      <span className="w-10 shrink-0 text-right text-(--fg-dim) text-[11px]">{ago(c.lastTs)}</span>
                    </button>
                    <ThreadMenu className="opacity-0 focus-within:opacity-100 group-hover:opacity-100" streamId={c.streamId} />
                  </div>
                );
              })}
            </section>
          );
        })}
      </div>
    </div>
  );
}
