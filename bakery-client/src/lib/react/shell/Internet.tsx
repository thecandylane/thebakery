// A project's internet list (Round 7, L1): the hosts its threads' sandboxes
// reach, where each comes from, and the ones waiting on the operator. The
// control plane's card shows it at a glance; its "All ›" opens this sheet,
// every host on a row, filtered and searched. Only the operator answers.

import { useCallback, useMemo, useState } from "react";
import { FileTextIcon, GlobeIcon, LockIcon, PlusIcon, RotateCwIcon, TriangleAlertIcon } from "lucide-react";
import { type Ask, type ControlPlane as Control, type Host, type Hosts, networkHostDecide, networkHosts, projectControl } from "$lib/api.generated";
import { ago } from "$lib/conversations";
import { sharedTenant } from "../hostsModel";
import { revealInCode } from "./CodeView";
import * as L from "./layout";
import type { ShellProject, ShellSnapshot } from "./shellSource.svelte";
import { keep, useCached } from "./cache";
import { Chip, type ChipTone, Freshness, said, type ToolContext, usePoll } from "./ui";

export const SHARED_WARNING = "Anyone can keep a bucket or a site there: an agent could send data to a stranger's.";
export const SOLID =
  "flex h-6 shrink-0 items-center rounded-[5px] border border-(--accent-fg) bg-(image:--grad-accent) px-2.5 font-bold text-(--fg-on-solid) text-[11px] hover:brightness-105 disabled:opacity-50";
export const PLAIN =
  "flex h-6 shrink-0 items-center gap-1 rounded-[5px] border border-(--line) bg-(image:--grad-button) px-2.5 text-(--fg-body) text-[11px] hover:text-(--fg) disabled:opacity-50";

/** A project's list, the last one at once (kept across restarts), read
 *  every ten seconds, and the operator's answers: a host (or `*.domain`)
 *  allowed, denied, or forgotten so it asks again. `busy` is an answer on
 *  its way; `loading`, a read. */
export function useHostList(projectId: string) {
  const key = `hosts:${projectId}`;
  const c = useCached<Hosts>(key, () => networkHosts({ project_id: projectId }));
  const { reload } = c;
  usePoll(useCallback(() => void reload(), [reload]), 10_000);
  const [answerError, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const decide = (host: string, allow: boolean | null) => {
    setBusy(true);
    return networkHostDecide({ project_id: projectId, host, allow }).then(
      (h) => {
        keep(key, h);
        setError("");
        setBusy(false);
        return true;
      },
      (e) => {
        setError(said(e));
        setBusy(false);
        return false;
      },
    );
  };
  return { list: c.data, error: answerError || c.error, busy, decide, at: c.at, loading: c.busy, reload: () => void reload() };
}

/** Opens a file of one of a project's folders in that folder's Code view:
 *  its repository's own checkout, or the worktree that is the folder (the
 *  Bakery's own worktrees are folders of it). */
export function openInCode(ctx: ToolContext, project: ShellProject, folder: string, file: string) {
  const own = project.checkouts.find((c) => c.inPlace && (c.path === folder || c.repoRoot === folder));
  const sid = (own ?? project.checkouts.find((c) => c.path === folder))?.streamId;
  if (!sid) return;
  revealInCode(sid, file, 1);
  ctx.setLayout((l) => L.openPanel(l, ctx.workspace.id, `@code:${sid}`, 900));
}

/** The folder whose detection found `file`, by the project's control read. */
export const folderOf = (control: Control | null, file: string) =>
  control?.folders.find((f) => [...f.stack, ...f.tools, ...f.dependencies].some((x) => x.source === file))?.path ?? null;

type Filter = "all" | "allowed" | "asking" | "denied";
type Row = { key: string; host: string; state: Exclude<Filter, "all">; source: string; when: string | null; ask?: Ask; entry?: Host };

const FILTERS: [Filter, string][] = [
  ["all", "All"],
  ["allowed", "Allowed"],
  ["asking", "Asking"],
  ["denied", "Denied"],
];
const STATE: Record<Row["state"], [ChipTone, string]> = {
  asking: ["warn", "ASKING"],
  allowed: ["ok", "ALLOWED"],
  denied: ["bad", "DENIED"],
};
const CELL = "border-(--line-soft) border-b px-3 py-1.5";

/** The project whose checkout `streamId` is, the Bakery's own included. */
export const projectOfCheckout = (snap: ShellSnapshot, streamId: string) =>
  [...snap.projects, snap.bakery].find((p) => p.checkouts.some((c) => c.streamId === streamId)) ?? null;

/** The sheet for the project whose checkout `streamId` is. */
export function InternetSheet({ ctx, streamId }: { ctx: ToolContext; streamId: string }) {
  const project = projectOfCheckout(ctx.snap, streamId);
  return project ? <Sheet ctx={ctx} project={project} /> : <p className="p-3 text-(--fg-faint)">This project is gone.</p>;
}

function Sheet({ ctx, project }: { ctx: ToolContext; project: ShellProject }) {
  const projectId = project.id;
  const { list, error, busy, decide, at, loading, reload } = useHostList(projectId);
  // Which folder found a file: the control plane's own read of the project.
  const { data: control } = useCached<Control>(`control:${projectId}`, () => projectControl({ project_id: projectId }));
  const [filter, setFilter] = useState<Filter>("all");
  const [query, setQuery] = useState("");
  const [adding, setAdding] = useState(false);
  const [draft, setDraft] = useState("");
  const conversations = ctx.snap.conversations;
  const rows = useMemo((): Row[] => {
    const threadOf = (sid: string) => conversations.find((c) => c.streamId === sid)?.title ?? "a thread";
    return [
      ...(list?.asks ?? []).map((a) => ({
        key: `ask:${a.event_hash}`,
        host: a.host,
        state: "asking" as const,
        source: `${threadOf(a.stream_id)}${a.program ? ` · ${a.program}` : ""}`,
        when: a.at,
        ask: a,
      })),
      ...(list?.hosts ?? []).map((h) => ({
        key: `host:${h.host}`,
        host: h.host,
        state: h.allow ? ("allowed" as const) : ("denied" as const),
        source: h.source === "default" ? "default" : h.source === "project" ? (h.because ?? "the project's files") : "you",
        when: h.at ?? null,
        entry: h,
      })),
    ];
  }, [list, conversations]);
  const q = query.trim().toLowerCase();
  const shown = rows.filter((r) => (filter === "all" || r.state === filter) && (!q || r.host.toLowerCase().includes(q)));
  const add = () => {
    const host = draft.trim();
    if (host) void decide(host, true).then((done) => done && (setDraft(""), setAdding(false)));
  };
  return (
    <div className="flex h-full min-h-0 flex-col bg-(--bg-panel) text-xs" data-testid="internet-sheet">
      <div className="flex h-9 shrink-0 items-center gap-1.5 border-(--line-soft) border-b bg-(--bg-raised) px-2.5">
        {FILTERS.map(([f, label]) => {
          const n = f === "all" ? rows.length : rows.filter((r) => r.state === f).length;
          const on = f === filter;
          return (
            <button
              aria-pressed={on}
              className={`flex h-6 shrink-0 items-center gap-1.5 rounded-[5px] border px-2 text-[11px] ${on ? "border-(--accent-border) bg-(--accent-sel) font-bold text-(--fg)" : "border-transparent text-(--fg-dim) hover:bg-(--bg-chip) hover:text-(--fg-2)"}`}
              key={f}
              onClick={() => setFilter(f)}
              type="button"
            >
              {label}
              <Chip tone={f === "asking" && n > 0 ? "warn" : on ? "accent" : "neutral"}>{n}</Chip>
            </button>
          );
        })}
        <input
          aria-label="Filter hosts"
          className="h-6 w-40 min-w-0 rounded-[5px] border border-(--line) bg-(--bg-app) px-2 font-mono text-[11px]"
          onChange={(e) => setQuery(e.target.value)}
          placeholder="Filter hosts"
          value={query}
        />
        <span className="flex-1" />
        <Freshness at={at} busy={loading} error={error} onReload={reload} />
        <button className={PLAIN} onClick={() => setAdding((a) => !a)} type="button">
          <PlusIcon className="size-3" />
          Add host
        </button>
      </div>
      {adding && (
        <form
          className="flex items-center gap-2 border-(--line-soft) border-b px-3 py-1.5"
          onSubmit={(e) => {
            e.preventDefault();
            add();
          }}
        >
          <input
            aria-label="Host to allow"
            autoFocus
            className="h-6 min-w-0 flex-1 rounded-[5px] border border-(--line) bg-(--bg-app) px-2 font-mono text-[11px]"
            onChange={(e) => setDraft(e.target.value)}
            onKeyDown={(e) => e.key === "Escape" && setAdding(false)}
            placeholder="host, or *.domain"
            value={draft}
          />
          <button className={SOLID} disabled={busy || !draft.trim()} type="submit">
            Allow
          </button>
        </form>
      )}
      {error && <p className="border-(--line-soft) border-b px-3 py-1 text-(--bad) text-[11px]">{error}</p>}
      {!list && !error && <p className="p-3 text-(--fg-faint)">Reading the list…</p>}
      {list && (
        <div className="min-h-0 flex-1 overflow-auto">
          <table className="w-full border-separate border-spacing-0 text-[11.5px]">
            <thead className="sticky top-0 bg-(--bg-raised)">
              <tr className="text-left text-(--fg-dim) text-[10px] tracking-[.06em]">
                <th className={`${CELL} font-bold`}>HOST</th>
                <th className={`${CELL} font-bold`}>STATE</th>
                <th className={`${CELL} font-bold`}>SOURCE</th>
                <th className={`${CELL} font-bold`}>WHEN</th>
                <th className={CELL} />
              </tr>
            </thead>
            <tbody>
              {shown.map((r) => {
                const file = r.entry?.source === "project" && r.entry.because ? r.entry.because : null;
                const folder = file ? folderOf(control, file) : null;
                return (
                  <tr className={`hover:bg-(--bg-chip) ${r.ask ? "bg-(--warn-bg)" : ""}`} key={r.key}>
                    <td className={CELL}>
                      <span className="flex min-w-0 items-center gap-1.5">
                        {r.ask && <GlobeIcon className="size-3 shrink-0 text-(--warn)" />}
                        <span className={`truncate font-mono ${r.state === "denied" ? "text-(--bad)" : "text-(--fg)"}`}>{r.host}</span>
                        {sharedTenant(r.host) && <Chip tone="bad" title={SHARED_WARNING}>SHARED</Chip>}
                      </span>
                    </td>
                    <td className={CELL}>
                      <Chip tone={STATE[r.state][0]}>{STATE[r.state][1]}</Chip>
                    </td>
                    <td className={`${CELL} max-w-56 truncate text-(--fg-dim)`}>{r.source}</td>
                    <td className={`${CELL} text-(--fg-faint)`}>{r.when ? ago(r.when) : ""}</td>
                    <td className={CELL}>
                      <span className="flex justify-end gap-1.5">
                        {r.ask ? (
                          <>
                            <button className={SOLID} disabled={busy} onClick={() => void decide(r.host, true)} type="button">
                              Allow
                            </button>
                            <button className={PLAIN} disabled={busy} onClick={() => void decide(r.host, false)} type="button">
                              Deny
                            </button>
                          </>
                        ) : (
                          r.entry && (
                            <>
                              {r.entry.source === "operator" && (
                                <button
                                  className={PLAIN}
                                  disabled={busy}
                                  onClick={() => void decide(r.host, null)}
                                  title={projectId ? "Forget: ask next time" : undefined}
                                  type="button"
                                >
                                  <RotateCwIcon className="size-3" />
                                  Forget
                                </button>
                              )}
                              {r.entry.allow ? (
                                <button className={PLAIN} disabled={busy} onClick={() => void decide(r.host, false)} type="button">
                                  Deny
                                </button>
                              ) : (
                                <button className={PLAIN} disabled={busy} onClick={() => void decide(r.host, true)} type="button">
                                  Allow
                                </button>
                              )}
                              {file && folder && (
                                <button className={PLAIN} onClick={() => openInCode(ctx, project, folder, file)} title={`Open ${file}`} type="button">
                                  <FileTextIcon className="size-3" />
                                  {file.split("/").pop()}
                                </button>
                              )}
                            </>
                          )
                        )}
                      </span>
                    </td>
                  </tr>
                );
              })}
              {(filter === "all" || filter === "denied") && !q && (
                <tr>
                  <td className={`${CELL} text-(--fg-dim)`}>private · LAN · tailnet addresses</td>
                  <td className={CELL}>
                    <Chip tone="bad">NEVER</Chip>
                  </td>
                  <td className={`${CELL} text-(--fg-dim)`}>the gateway</td>
                  <td className={CELL} />
                  <td className={CELL}>
                    <span className="flex justify-end">
                      <LockIcon className="size-3 text-(--fg-faint)" />
                    </span>
                  </td>
                </tr>
              )}
            </tbody>
          </table>
          {rows.some((r) => r.ask && sharedTenant(r.host)) && (
            <p className="flex items-start gap-1.5 px-3 py-2 text-(--bad) text-[11px]">
              <TriangleAlertIcon className="mt-px size-3 shrink-0" />
              {SHARED_WARNING}
            </p>
          )}
        </div>
      )}
    </div>
  );
}
