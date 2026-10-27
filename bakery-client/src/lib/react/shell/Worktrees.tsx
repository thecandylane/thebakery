// The control plane's Worktrees card (Round 10, S1): every branch of the
// project with what the sidebar no longer shows (it keeps only what can be
// acted on), each state beside its action: its changes and Commit, how far
// behind its base and Update (a merge, never a rebase), its CI and the runs,
// other threads changing the same files and Compare, then Review.

import { type FormEvent, useContext, useState } from "react";
import { CodeIcon, GitBranchIcon, TriangleAlertIcon } from "lucide-react";
import { gitCommit, gitStage, gitUpdate, type ProjectRollup } from "$lib/api.generated";
import { ask } from "$lib/ask";
import { ago, type Conversation } from "$lib/conversations";
import { Card } from "./ControlCards";
import * as L from "./layout";
import { PaneContext } from "./PaneHeader";
import { reloadRollup } from "./rollup";
import type { ShellProject } from "./shellSource.svelte";
import { ciOf, fetchedText, repoTree } from "./sidebarModel";
import { Chip, said, StatusDot, type ToolContext } from "./ui";

const ACT =
  "flex h-5 shrink-0 items-center gap-1 whitespace-nowrap rounded-[4px] border border-(--line) bg-(image:--grad-button) px-1.5 text-(--fg-body) text-[10.5px] hover:text-(--fg) disabled:opacity-40";
const GO =
  "flex h-5 shrink-0 items-center gap-1 whitespace-nowrap rounded-[4px] border border-(--accent-fg) bg-(image:--grad-accent) px-1.5 font-bold text-(--fg-on-solid) text-[10.5px] hover:brightness-105 disabled:opacity-50";
const CELL = "px-2 py-1.5 align-middle";
const LINE = "flex min-w-0 items-center gap-1.5 whitespace-nowrap";
const HEAD = "px-2 py-1.5 text-left font-bold text-(--fg-dim) text-[10px] tracking-[.06em]";
const CI = { pass: ["ok", "CI ✓"], fail: ["bad", "CI ✕"], pending: ["warn", "CI ◐"] } as const;
const none = <span className="text-(--fg-faint)">—</span>;

type Notice = { bad: boolean; text: string } | null;

export function WorktreesCard({
  ctx,
  project,
  threads,
  rollup,
}: {
  ctx: ToolContext;
  project: ShellProject;
  /** The workspace's threads: each worktree's agents. */
  threads: readonly Conversation[];
  rollup: ProjectRollup | null;
}) {
  const pane = useContext(PaneContext);
  const ws = ctx.workspace;
  const [busy, setBusy] = useState<string | null>(null);
  const [notice, setNotice] = useState<Notice>(null);
  /** The worktree (its first thread) whose commit message is being written. */
  const [committing, setCommitting] = useState<string | null>(null);
  const [message, setMessage] = useState("");
  const tree = repoTree([...threads], project.checkouts, rollup?.repos.map((r) => r.path) ?? []);
  const counted = new Map(rollup?.worktrees.map((t) => [t.stream_id, t]) ?? []);
  const checkout = new Map(project.checkouts.map((c) => [c.streamId, c]));
  const runs = rollup?.repos.flatMap((r) => r.runs) ?? [];
  const titled = (id: string) => ctx.snap.conversations.find((c) => c.streamId === id)?.title ?? id;
  const open = (id: string) => ctx.setLayout((l) => L.openPanel(l, ws.id, id));
  const worktrees = tree.repos.reduce((n, r) => n + r.worktrees.filter((g) => g.path).length, 0);

  /** One action at a time; what it says (or why it failed) shows under the table. */
  const run = async (key: string, job: () => Promise<Notice>) => {
    setBusy(key);
    setNotice(null);
    try {
      setNotice(await job());
      reloadRollup(project.id);
    } catch (e) {
      setNotice({ bad: true, text: said(e) });
    }
    setBusy(null);
  };
  const update = (streamId: string, branch: string, base: string) =>
    run(`update:${streamId}`, async () => {
      if (!(await ask(`Bring ${branch} up to ${base} with a merge?`))) return null;
      const u = await gitUpdate({ stream_id: streamId });
      if (u.conflicts.length > 0) return { bad: true, text: `${u.base} merged into ${branch} with conflicts in ${u.conflicts.join(", ")}: resolve them there, then commit.` };
      return { bad: false, text: u.merged ? `${branch} is up to date with ${u.base}.` : `${branch} already had everything from ${u.base}.` };
    });
  const commit = (e: FormEvent, streamId: string, branch: string) => {
    e.preventDefault();
    const text = message.trim();
    if (!text) return;
    void run(`commit:${streamId}`, async () => {
      await gitStage({ stream_id: streamId, all: true });
      await gitCommit({ stream_id: streamId, message: text });
      setCommitting(null);
      setMessage("");
      return { bad: false, text: `Committed on ${branch}.` };
    });
  };

  return (
    <Card count={worktrees} hue="var(--violet)" icon={<GitBranchIcon className="size-3.5" />} title="Worktrees" wide="full">
      <table className="w-full border-collapse text-[11.5px]" data-testid="worktrees">
        <thead>
          <tr className="border-(--line-soft) border-b">
            <th className={HEAD}>WORKTREE</th>
            <th className={HEAD}>THREAD</th>
            <th className={HEAD}>CHANGES</th>
            <th className={HEAD}>AGAINST ITS BASE</th>
            <th className={HEAD}>CI</th>
            <th className={HEAD} />
          </tr>
        </thead>
        <tbody>
          {tree.repos.map((r) => [
            <tr className="bg-(--bg-app)" key={r.root}>
              <td className="px-2 py-1 font-bold text-(--fg-dim) text-[10px] uppercase tracking-[.06em]" colSpan={6} title={r.root}>
                {r.name}
              </td>
            </tr>,
            ...r.worktrees
              .filter((g) => g.path)
              .flatMap((g) => {
                const first = g.members[0];
                const n = counted.get(first.streamId);
                const co = checkout.get(first.streamId);
                const added = n?.added ?? co?.added ?? 0;
                const removed = n?.removed ?? co?.removed ?? 0;
                const branch = g.branch || "detached";
                const base = n?.base ?? null;
                const ci = g.branch ? ciOf(runs, g.branch) : null;
                const fetched = n ? fetchedText(n.fetched, ago) : undefined;
                const rows = [
                  <tr className="border-(--line-row) border-b" key={g.path}>
                    <td className={CELL} title={g.path}>
                      <span className={LINE}>
                        <GitBranchIcon className="size-3 shrink-0 text-(--violet)" />
                        <span className="truncate text-(--fg)">{branch}</span>
                        {g.local && <Chip>IN PLACE</Chip>}
                      </span>
                    </td>
                    <td className={CELL}>
                      <span className="flex flex-wrap items-center gap-x-2.5 gap-y-1">
                        {g.members.map((c) => (
                          <button className={`${LINE} hover:text-(--fg)`} key={c.streamId} onClick={() => pane?.enter(ws.id, c)} title={c.note || c.title} type="button">
                            <StatusDot status={c.status} streamId={c.streamId} />
                            <span className="truncate text-(--fg-body)">{c.title}</span>
                          </button>
                        ))}
                      </span>
                    </td>
                    <td className={CELL}>
                      <span className={LINE}>
                        {(added > 0 || removed > 0) && (
                          <span title={base ? `Against ${base}` : undefined}>
                            <span className="text-(--diff-add-fg)">+{added}</span> <span className="text-(--diff-del-fg)">−{removed}</span>
                          </span>
                        )}
                        {n?.dirty ? (
                          <>
                            <Chip tone="warn">UNCOMMITTED</Chip>
                            <button
                              className={ACT}
                              disabled={busy !== null}
                              onClick={() => {
                                setCommitting(committing === first.streamId ? null : first.streamId);
                                setMessage("");
                              }}
                              type="button"
                            >
                              Commit…
                            </button>
                          </>
                        ) : (
                          added === 0 && removed === 0 && <span className="text-(--fg-dim)">clean</span>
                        )}
                      </span>
                    </td>
                    <td className={CELL}>
                      <span className={LINE}>
                        {n && n.behind > 0 ? (
                          <>
                            <Chip title={fetched} tone="warn">
                              {n.behind} BEHIND {base ?? "ITS BASE"}
                            </Chip>
                            <button className={ACT} disabled={busy !== null} onClick={() => void update(first.streamId, branch, base ?? "its base")} type="button">
                              {busy === `update:${first.streamId}` ? "Updating…" : "Update"}
                            </button>
                          </>
                        ) : base ? (
                          <span className="text-(--fg-dim)" title={fetched}>
                            up to date
                          </span>
                        ) : (
                          none
                        )}
                      </span>
                    </td>
                    <td className={CELL}>
                      <span className={LINE}>
                        {ci ? (
                          <>
                            <Chip tone={CI[ci][0]}>{CI[ci][1]}</Chip>
                            <button className={ACT} onClick={() => open("@actions")} type="button">
                              Run ›
                            </button>
                          </>
                        ) : (
                          none
                        )}
                      </span>
                    </td>
                    <td className={`${CELL} text-right`}>
                      <span className={`${LINE} justify-end`}>
                        {g.local ? (
                          <button className={ACT} onClick={() => open(`@code:${first.streamId}`)} type="button">
                            <CodeIcon className="size-3 text-(--hue-containers)" />
                            Code
                          </button>
                        ) : (
                          (added > 0 || removed > 0) && (
                            <button className={GO} onClick={() => pane?.reviewFrom(ws.id, first.streamId)} type="button">
                              Review
                            </button>
                          )
                        )}
                      </span>
                    </td>
                  </tr>,
                ];
                if (committing === first.streamId) {
                  rows.push(
                    <tr className="border-(--line-row) border-b bg-(--bg-app)" key={`${g.path}:commit`}>
                      <td className={CELL} colSpan={6}>
                        <form className="flex items-center gap-1.5" onSubmit={(e) => commit(e, first.streamId, branch)}>
                          <input
                            aria-label={`Commit message for ${branch}`}
                            autoFocus
                            className="h-6 min-w-0 flex-1 rounded-[5px] border border-(--line) bg-(--bg-raised) px-2 text-(--fg) text-xs outline-none focus:border-(--accent-border)"
                            onChange={(e) => setMessage(e.target.value)}
                            onKeyDown={(e) => e.key === "Escape" && setCommitting(null)}
                            placeholder={`What changed on ${branch}`}
                            value={message}
                          />
                          <button className={GO} disabled={busy !== null || !message.trim()} type="submit">
                            {busy === `commit:${first.streamId}` ? "Committing…" : "Commit all"}
                          </button>
                          <button className={ACT} onClick={() => setCommitting(null)} type="button">
                            Cancel
                          </button>
                        </form>
                      </td>
                    </tr>,
                  );
                }
                if (n && n.also_changing.length > 0) {
                  const both = n.also_changing.map((o) => `${branch} and ${titled(o.stream_id)} both change ${o.files.join(", ")}`).join(" · ");
                  rows.push(
                    <tr className="border-(--line-row) border-b bg-(--warn-bg)" key={`${g.path}:overlap`}>
                      <td className={CELL} colSpan={6}>
                        <span className="flex items-center gap-1.5 text-(--warn)">
                          <TriangleAlertIcon className="size-3 shrink-0" />
                          <span className="min-w-0 flex-1 truncate" title={both}>
                            {both}
                          </span>
                          <button className={ACT} onClick={() => pane?.reviewFrom(ws.id, first.streamId)} type="button">
                            Compare
                          </button>
                        </span>
                      </td>
                    </tr>,
                  );
                }
                return rows;
              }),
          ])}
        </tbody>
      </table>
      {notice && <p className={`px-1 text-[11px] ${notice.bad ? "text-(--bad)" : "text-(--ok)"}`}>{notice.text}</p>}
    </Card>
  );
}
