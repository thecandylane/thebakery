// GitHub's panels (Round 6): Pull requests (G1), one inbox of every open PR
// across the owners and repositories the operator follows, and Actions
// (G4), the latest run of each workflow on each branch. The node asks GitHub,
// keeps each answer a minute and sends what changed (github.pr.updated,
// github.run.updated); a two-minute read drives it. A picked PR opens below
// the list, whole: its conversation, its files (Ask its thread about a
// block, or comment on one), its checks with their logs, its commits. Then
// Comment, Request changes, Approve, Merge: each goes out under the
// operator's name, so each asks first.

import { type ComponentProps, type MouseEvent, type ReactNode, useCallback, useContext, useEffect, useMemo, useRef, useState } from "react";
import { BotIcon, ExternalLinkIcon, GitMergeIcon, GitPullRequestIcon, ImageIcon, MessageSquareIcon, RefreshCwIcon, UsersIcon } from "lucide-react";
import { MessageResponse } from "@/lib/ai-elements/message";
import {
  agentTurnQueue,
  type GithubCheck,
  type GithubPr,
  type GithubRun,
  type GithubScope,
  githubPr,
  githubPrComment,
  githubPrMerge,
  githubPrReady,
  githubPrReview,
  githubPrs,
  githubPrsRefresh,
  githubPrThreadResolve,
  githubRun,
  githubRunLog,
  githubRunRerun,
  githubRuns,
  githubScope,
  githubScopeSet,
  type LineComment,
  type PrView as PrData,
  type Prs,
  type ReviewThread,
  type RunLog,
  type Runs,
  type RunView as RunData,
  type Seat,
} from "$lib/api.generated";
import { ask } from "$lib/ask";
import { isTauri } from "$lib/desktop";
import { ago, type Conversation } from "$lib/conversations";
import { onEnvelope } from "$lib/events";
import { bakeryCodePlugin } from "../highlight";
import { SplitDiff } from "../ToolRows";
import { applyPrUpdate, applyRunUpdate, hunkAnchor, inPrGroup, inRunGroup, NO_PR_FILTER, type PrFilter, type PrGroup, prMatches, type RunGroup, sameRepo } from "./githubModel";
import { type Candidate, HunkBlock } from "./MergeReview";
import { PaneContext } from "./PaneHeader";
import { POP, useDismiss } from "./PanelMenu";
import { baseOf } from "./reviewModel";
import { isFailedRun } from "./sidebarModel";
import { patchKept, useCached } from "./cache";
import { Chip, type ChipTone, Freshness, said, StatusDot, type ToolContext, usePoll, useSecretShown } from "./ui";
import { type FileDiff, type Hunk, parseUnifiedDiff } from "./unifiedDiff";

const PR_GROUPS: [PrGroup, string][] = [
  ["waiting", "Waiting on you"],
  ["threads", "From your threads"],
  ["failing", "CI failing"],
  ["bots", "Bots"],
  ["all", "All open"],
];
/** The inbox's time filter: when it was last touched or opened. */
const PR_WHEN: [string, string][] = [
  ["", "Any time"],
  ["updated:1", "Updated in the last day"],
  ["updated:7", "Updated in the last week"],
  ["updated:30", "Updated in the last month"],
  ["opened:7", "Opened in the last week"],
  ["opened:30", "Opened in the last month"],
  ["quiet:30", "Quiet for a month"],
];
const PR_REVIEW: [string, string][] = [
  ["", "Any review"],
  ["REVIEW_REQUIRED", "Review required"],
  ["APPROVED", "Approved"],
  ["CHANGES_REQUESTED", "Changes requested"],
];
const FILTER = "h-6 min-w-0 rounded-[5px] border border-(--line) bg-(--bg-app) px-1.5 text-(--fg) text-[11px]";
const RUN_GROUPS: [RunGroup, string][] = [
  ["failing", "Failing"],
  ["running", "Running"],
  ["all", "All runs"],
];
const REVIEW: Record<string, [ChipTone, string]> = {
  APPROVED: ["teal", "APPROVED"],
  CHANGES_REQUESTED: ["bad", "CHANGES"],
  REVIEW_REQUIRED: ["warn", "REVIEW"],
};
const REVIEW_STATE: Record<string, [ChipTone, string]> = {
  APPROVED: ["teal", "APPROVED"],
  CHANGES_REQUESTED: ["bad", "CHANGES REQUESTED"],
  COMMENTED: ["neutral", "COMMENTED"],
  DISMISSED: ["neutral", "DISMISSED"],
};
const BUCKET: Record<string, [string, string]> = {
  pass: ["✓", "text-(--ok)"],
  fail: ["✕", "text-(--bad)"],
  pending: ["◐", "text-(--warn)"],
  skipping: ["–", "text-(--fg-faint)"],
  cancel: ["⊘", "text-(--fg-faint)"],
};
const pill =
  "flex h-6 shrink-0 items-center gap-1.5 whitespace-nowrap rounded-[5px] border border-(--line) bg-(image:--grad-button) px-2.5 text-(--fg-body) text-[11px] hover:text-(--fg) disabled:opacity-40";
const solid =
  "flex h-6 shrink-0 items-center gap-1.5 rounded-[5px] border border-(--accent-fg) bg-(image:--grad-accent) px-2.5 font-bold text-(--fg-on-solid) text-[11px] hover:brightness-105 disabled:opacity-50";
const area =
  "min-h-14 w-full resize-y rounded-[5px] border border-(--line) bg-(--bg-app) px-2 py-1.5 text-(--fg) text-[12px] outline-none focus:border-(--accent-border)";
const TAB = (on: boolean) =>
  `h-7 rounded-t-md px-3 text-[11px] ${on ? "border border-(--line) border-b-0 bg-(--bg-raised) font-bold text-(--fg)" : "text-(--fg-dim) hover:text-(--fg-2)"}`;
const GROUP_BUTTON = (on: boolean) =>
  `flex h-6 shrink-0 items-center gap-1.5 rounded-[5px] border px-2 text-[11px] ${on ? "border-(--accent-border) bg-(--accent-sel) font-bold text-(--fg)" : "border-transparent text-(--fg-dim) hover:bg-(--bg-chip) hover:text-(--fg-2)"}`;
/** GitHub's writes (review, comment, merge, ready, resolve, re-run, refresh,
 *  the scope) stay off the phone, the Tauri build, until the operator says
 *  otherwise; the node refuses them there too. Reading stays. */
const actsHere = () => !isTauri();
/** A fallback read: the node refreshes the inbox itself (every 2 minutes
 *  while a window is visible) and says so with github.inbox.updated. */
const READ_EVERY = 10 * 60_000;

/** The node read a list again: its problems (GitHub's rate limit, say) at
 *  once, then the list itself, a cheap read of the node's copy. */
function useInboxUpdates(list: "prs" | "runs", key: string, reload: () => Promise<void>) {
  useEffect(
    () =>
      onEnvelope("github.inbox.updated", (e) => {
        if (e.list !== list) return;
        if (Array.isArray(e.problems)) patchKept<{ problems: string[] }>(key, (d) => ({ ...d, problems: (e.problems as unknown[]).filter((p): p is string => typeof p === "string") }));
        void reload();
      }),
    [list, key, reload],
  );
}

const threadOf = (ctx: ToolContext, streamId: string | null | undefined) =>
  streamId ? (ctx.snap.conversations.find((c) => c.streamId === streamId) ?? null) : null;

const GitHubLink = ({ url }: { url: string }) => (
  <a className={pill} href={url} rel="noreferrer" target="_blank" title={url}>
    <ExternalLinkIcon className="size-3" />
    GitHub
  </a>
);

/** GitHub's own hosts. A body's image or link there opens as it is; one
 *  anywhere else is outside, and waits for the operator. */
function onGitHub(url: string): boolean {
  try {
    const { protocol, hostname } = new URL(url);
    return protocol === "https:" && (hostname === "github.com" || hostname.endsWith(".github.com") || hostname.endsWith(".githubusercontent.com"));
  } catch {
    return false;
  }
}

/** An image in a body: GitHub's own load; one from outside shows its host
 *  and loads when clicked, so no one learns the operator read it before
 *  they choose to. Inside a link, the click loads it and goes nowhere. */
function BodyImage({ src, alt }: ComponentProps<"img">) {
  const url = typeof src === "string" ? src : "";
  const [load, setLoad] = useState(() => onGitHub(url) || url.startsWith("data:image/"));
  const [failed, setFailed] = useState(false);
  let host = "";
  try {
    host = new URL(url).hostname;
  } catch {}
  if (load && !failed) {
    return <img alt={alt ?? ""} className="inline max-w-full rounded-[5px]" loading="lazy" onError={() => setFailed(true)} referrerPolicy="no-referrer" src={url} />;
  }
  if (failed || !host) return alt ? <span className="text-(--fg-dim)">{alt}</span> : null;
  const show = (e: MouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
    setLoad(true);
  };
  return (
    <span
      className="inline-flex h-6 max-w-full cursor-pointer items-center gap-1.5 rounded-[5px] border border-(--line) border-dashed bg-(--bg-app) px-2 align-middle text-(--fg-body) text-[11px] hover:border-(--line-strong) hover:text-(--fg)"
      data-testid="body-image"
      onClick={show}
      onKeyDown={(e) => (e.key === "Enter" || e.key === " ") && show(e as unknown as MouseEvent)}
      role="button"
      tabIndex={0}
      title={url}
    >
      <ImageIcon className="size-3 shrink-0 text-(--link)" />
      <span className="min-w-0 truncate">{alt || "Image"}</span>
      <span className="shrink-0 text-(--fg-dim)">{host}</span>
    </span>
  );
}

/** How a body renders: formatted as GitHub shows it, with outside images
 *  waiting for a click (a <picture>'s sources never load) and outside links
 *  showing their address and asking first. Module constants, because
 *  `MessageResponse` re-renders only when its text changes. */
const BODY_COMPONENTS = { img: BodyImage, source: () => null };
const BODY_LINKS = { enabled: true, onLinkCheck: onGitHub };
const BODY_PLUGINS = { code: bakeryCodePlugin };
const BODY_CONTROLS = { code: { copy: true, download: false }, table: false } as const;

/** A body or comment from GitHub, formatted. */
const Said = ({ text }: { text: string }) =>
  text.trim() ? (
    <div className="text-(--fg-body) text-[12px]" data-testid="github-body">
      <MessageResponse
        className="bakery-ai-prose"
        components={BODY_COMPONENTS}
        controls={BODY_CONTROLS}
        lineNumbers={false}
        linkSafety={BODY_LINKS}
        mode="static"
        plugins={BODY_PLUGINS}
      >
        {text}
      </MessageResponse>
    </div>
  ) : null;

/** Sends a thread a message, once: its failing check or run to fix. */
function useHandOff(conv: Conversation | null) {
  const [handed, setHanded] = useState(false);
  const [error, setError] = useState("");
  const hand = async (instruction: string) => {
    if (!conv) return;
    setError("");
    try {
      await agentTurnQueue({ stream_id: conv.streamId, instruction, seat: conv.roleId as Seat });
      setHanded(true);
    } catch (e) {
      setError(said(e));
    }
  };
  return (instruction: string): ReactNode =>
    conv && (
      <>
        <button className={solid} disabled={handed} onClick={() => void hand(instruction)} type="button">
          {handed ? `Handed to ${conv.title}` : `Hand it to ${conv.title}`}
        </button>
        {error && <span className="text-(--bad) text-[11px]">{error}</span>}
      </>
    );
}

// ---- Pull requests -------------------------------------------------------

/** The node's PR list, kept current: the last one at once (kept across
 *  restarts), read again whenever the node refreshed it (and every ten
 *  minutes besides), patched by github.pr.updated in between. `refresh`
 *  asks GitHub again; `reload` asks the node. */
export function usePrs() {
  const c = useCached<Prs>("github:prs", githubPrs);
  const { reload } = c;
  usePoll(useCallback(() => void reload(), [reload]), READ_EVERY);
  useInboxUpdates("prs", "github:prs", reload);
  useEffect(
    () =>
      onEnvelope("github.pr.updated", (e) =>
        patchKept<Prs>("github:prs", (d) => ({ ...d, prs: applyPrUpdate(d.prs, e as unknown as { repository: string; number: number; pr: GithubPr | null }) })),
      ),
    [],
  );
  return { data: c.data, error: c.error, busy: c.busy, at: c.at, reload: () => void reload(), refresh: () => void reload(githubPrsRefresh) };
}

export function PrInbox({ ctx }: { ctx: ToolContext }) {
  const { data, error, busy, at, reload, refresh } = usePrs();
  const prs = data?.prs ?? [];
  const [group, setGroup] = useState<PrGroup | null>(null);
  const [filter, setFilter] = useState<PrFilter>(NO_PR_FILTER);
  const filtering = JSON.stringify(filter) !== JSON.stringify(NO_PR_FILTER);
  const set = (change: Partial<PrFilter>) => setFilter((f) => ({ ...f, ...change }));
  const passing = prs.filter((p) => prMatches(p, filter));
  const shownGroup = group ?? (passing.some((p) => inPrGroup(p, "waiting")) ? "waiting" : passing.some((p) => p.thread_stream_id) ? "threads" : "all");
  const shown = passing.filter((p) => inPrGroup(p, shownGroup));
  /** Who opened them, the most first; the operator as "You". */
  const authors = [...new Set(prs.map((p) => p.author))]
    .map((a) => [a, prs.filter((p) => p.author === a).length] as const)
    .sort((x, y) => y[1] - x[1] || x[0].localeCompare(y[0]));
  const repos = [...new Set(prs.map((p) => p.repository))].sort();
  const [picked, setPicked] = useState<string | null>(null);
  const current = shown.find((p) => p.url === picked) ?? shown[0] ?? null;
  return (
    <div className="flex h-full min-h-0 flex-col bg-(--bg-panel) text-xs" data-testid="pr-inbox">
      <div className="flex h-9 shrink-0 items-center gap-1.5 border-(--line-soft) border-b bg-(--bg-raised) px-2.5">
        {PR_GROUPS.map(([g, label]) => {
          const n = passing.filter((p) => inPrGroup(p, g)).length;
          return (
            <button aria-pressed={g === shownGroup} className={GROUP_BUTTON(g === shownGroup)} key={g} onClick={() => setGroup(g)} type="button">
              {label}
              <Chip tone={g === "failing" && n > 0 ? "bad" : g === "waiting" && n > 0 ? "warn" : g === shownGroup ? "accent" : "neutral"}>{n}</Chip>
            </button>
          );
        })}
        <span className="flex-1" />
        {actsHere() && <ScopePicker onChanged={refresh} />}
        {/* Reload asks GitHub again, a write the phone leaves to the Mac. */}
        <Freshness at={at} busy={busy} error={error} onReload={actsHere() ? refresh : reload} />
      </div>
      {(error || (data?.problems.length ?? 0) > 0) && (
        <p className="border-(--line-soft) border-b px-3 py-1 text-(--warn) text-[11px]" title={[error, ...(data?.problems ?? [])].filter(Boolean).join("\n")}>
          {error || data?.problems[0]}
        </p>
      )}
      {prs.length > 0 && (
        <div className="flex shrink-0 flex-wrap items-center gap-1.5 border-(--line-soft) border-b px-2.5 py-1.5">
          <input aria-label="Find a pull request" className={`${FILTER} w-44`} onChange={(e) => set({ text: e.target.value })} placeholder="Title, branch or #" value={filter.text} />
          <select aria-label="Opened by" className={FILTER} onChange={(e) => set({ author: e.target.value })} value={filter.author}>
            <option value="">Anyone</option>
            {authors.map(([a, n]) => (
              <option key={a} value={a}>
                {a === data?.me ? "You" : a} · {n}
              </option>
            ))}
          </select>
          <select aria-label="When" className={FILTER} onChange={(e) => set({ when: e.target.value })} value={filter.when}>
            {PR_WHEN.map(([v, w]) => (
              <option key={v} value={v}>
                {w}
              </option>
            ))}
          </select>
          {repos.length > 1 && (
            <select aria-label="Repository" className={FILTER} onChange={(e) => set({ repo: e.target.value })} value={filter.repo}>
              <option value="">Every repository</option>
              {repos.map((r) => (
                <option key={r} value={r}>
                  {r}
                </option>
              ))}
            </select>
          )}
          <select aria-label="Drafts" className={FILTER} onChange={(e) => set({ state: e.target.value as PrFilter["state"] })} value={filter.state}>
            <option value="">Drafts and ready</option>
            <option value="ready">Ready</option>
            <option value="draft">Drafts</option>
          </select>
          <select aria-label="Review" className={FILTER} onChange={(e) => set({ review: e.target.value })} value={filter.review}>
            {PR_REVIEW.map(([v, w]) => (
              <option key={v} value={v}>
                {w}
              </option>
            ))}
          </select>
          {filtering && (
            <button className={pill} onClick={() => setFilter(NO_PR_FILTER)} type="button">
              Clear
            </button>
          )}
        </div>
      )}
      {!data && !error && <p className="p-3 text-(--fg-faint)">Asking GitHub…</p>}
      {data && shown.length === 0 && <p className="p-3 text-(--fg-faint)">{filtering ? "No pull requests match these filters." : "No open pull requests here."}</p>}
      {shown.length > 0 && (
        <div className="max-h-[38%] shrink-0 overflow-auto border-(--line) border-b">
          {shown.map((p) => (
            <PrLine ctx={ctx} key={p.url} on={p === current} onPick={() => setPicked(p.url)} pr={p} />
          ))}
        </div>
      )}
      {current && data && <PullView ctx={ctx} key={current.url} me={data.me} onActed={refresh} pr={current} />}
    </div>
  );
}

/** Who and what the operator follows: owners (themselves and their orgs),
 *  and single repositories turned on or off. */
function ScopePicker({ onChanged }: { onChanged: () => void }) {
  const [open, setOpen] = useState(false);
  const [scope, setScope] = useState<GithubScope | null>(null);
  const [error, setError] = useState("");
  const [draft, setDraft] = useState("");
  const box = useRef<HTMLDivElement>(null);
  useDismiss(open, box, () => setOpen(false));
  useEffect(() => {
    if (!open) return;
    githubScope().then(setScope, (e) => setError(said(e)));
  }, [open]);
  const set = (req: Parameters<typeof githubScopeSet>[0]) =>
    githubScopeSet(req).then(
      (s) => {
        setScope(s);
        setError("");
        onChanged();
      },
      (e) => setError(said(e)),
    );
  const add = () => {
    const name = draft.trim();
    if (!name) return;
    void set(name.includes("/") ? { repos: [{ repository: name, on: true }] } : { owners: [{ login: name, on: true }] }).then(() => setDraft(""));
  };
  const on = scope ? scope.owners.filter((o) => o.on).length : null;
  return (
    <div className="relative" ref={box}>
      <button aria-expanded={open} className={pill} onClick={() => setOpen((v) => !v)} type="button">
        <UsersIcon className="size-3" />
        Following{on !== null ? ` · ${on}` : ""}
      </button>
      {open && (
        <div className={`${POP} w-72`} role="dialog">
          {!scope && !error && <p className="p-2 text-(--fg-faint)">Asking GitHub…</p>}
          {scope && (
            <>
              <div className="px-2 pt-1 pb-0.5 font-bold text-(--fg-dim) text-[10px] tracking-[.06em]">OWNERS</div>
              {scope.owners.map((o) => (
                <label className="flex h-7 items-center gap-2 rounded px-2 hover:bg-(--bg-chip)" key={o.login}>
                  <input checked={o.on} className="accent-(--accent)" onChange={(e) => void set({ owners: [{ login: o.login, on: e.target.checked }] })} type="checkbox" />
                  <span className="min-w-0 flex-1 truncate text-(--fg-body)">{o.login}</span>
                  {o.login === scope.me && <Chip tone="accent">YOU</Chip>}
                  {o.kind === "org" && <Chip>ORG</Chip>}
                </label>
              ))}
              {scope.repos.length > 0 && <div className="px-2 pt-2 pb-0.5 font-bold text-(--fg-dim) text-[10px] tracking-[.06em]">REPOSITORIES</div>}
              {scope.repos.map((r) => (
                <label className="flex h-7 items-center gap-2 rounded px-2 hover:bg-(--bg-chip)" key={r.repository}>
                  <input checked={r.on} className="accent-(--accent)" onChange={(e) => void set({ repos: [{ repository: r.repository, on: e.target.checked }] })} type="checkbox" />
                  <span className="min-w-0 flex-1 truncate text-(--fg-body)">{r.repository}</span>
                </label>
              ))}
              <form
                className="flex items-center gap-1.5 p-2"
                onSubmit={(e) => {
                  e.preventDefault();
                  add();
                }}
              >
                <input
                  aria-label="An owner, or owner/repository, to follow"
                  className="h-6 min-w-0 flex-1 rounded-[5px] border border-(--line) bg-(--bg-app) px-2 text-[11px]"
                  onChange={(e) => setDraft(e.target.value)}
                  placeholder="owner, or owner/repository"
                  value={draft}
                />
                <button className={solid} disabled={!draft.trim()} type="submit">
                  Follow
                </button>
              </form>
              {scope.problems.map((p) => (
                <p className="px-2 pb-1 text-(--warn) text-[11px]" key={p}>
                  {p}
                </p>
              ))}
            </>
          )}
          {error && <p className="p-2 text-(--bad) text-[11px]">{error}</p>}
        </div>
      )}
    </div>
  );
}

function ChecksChip({ pr }: { pr: GithubPr }) {
  const c = pr.checks;
  if (c.fail > 0) return <Chip tone="bad">CI ✕{c.fail}</Chip>;
  if (c.pending > 0) return <Chip tone="warn">CI ◐{c.pending}</Chip>;
  if (c.pass > 0) return <Chip tone="ok">CI ✓{c.pass}</Chip>;
  return null;
}

function PrChips({ pr }: { pr: GithubPr }) {
  const review = REVIEW[pr.review_decision ?? ""];
  return (
    <>
      {pr.draft && <Chip>DRAFT</Chip>}
      {pr.mergeable === "CONFLICTING" && <Chip tone="bad">CONFLICTS</Chip>}
      <ChecksChip pr={pr} />
      {review && <Chip tone={review[0]}>{review[1]}</Chip>}
    </>
  );
}

function PrLine({ ctx, pr, on, onPick }: { ctx: ToolContext; pr: GithubPr; on: boolean; onPick: () => void }) {
  const conv = threadOf(ctx, pr.thread_stream_id);
  return (
    <div
      className={`flex h-[46px] cursor-pointer items-center gap-2.5 border-(--line-soft) border-b px-3 ${on ? "bg-(--accent-sel) shadow-[inset_3px_0_0_var(--accent)]" : "hover:bg-(--bg-chip)"}`}
      onClick={onPick}
    >
      {conv ? (
        <StatusDot status={conv.status} streamId={conv.streamId} title={conv.title} />
      ) : pr.author_is_bot ? (
        <BotIcon className="size-3.5 shrink-0 text-(--fg-dim)" />
      ) : (
        <GitPullRequestIcon className={`size-3.5 shrink-0 ${pr.draft ? "text-(--fg-faint)" : "text-(--hue-github)"}`} />
      )}
      <span className="flex min-w-0 flex-1 flex-col">
        <span className="truncate font-bold text-(--fg)">
          <span className="font-normal text-(--fg-dim)">#{pr.number}</span> {pr.title}
        </span>
        <span className="truncate text-(--fg-dim) text-[11px]">
          {pr.repository} · {pr.branch} → {pr.base} · {pr.author}
          {conv ? ` · ${conv.title}` : ""}
        </span>
      </span>
      {pr.requested_from_me && !pr.draft && <Chip tone="warn">YOU</Chip>}
      <PrChips pr={pr} />
      <span className="shrink-0 font-mono text-[11px]">
        <span className="text-(--diff-add-fg)">+{pr.additions}</span> <span className="text-(--diff-del-fg)">−{pr.deletions}</span>
      </span>
      {pr.comments > 0 && (
        <span className="flex shrink-0 items-center gap-1 text-(--fg-dim) text-[11px]">
          <MessageSquareIcon className="size-3" />
          {pr.comments}
        </span>
      )}
      <span className="w-8 shrink-0 text-right text-(--fg-faint) text-[11px]" title={pr.updated_at}>
        {ago(pr.updated_at)}
      </span>
    </div>
  );
}

type Tab = "conversation" | "files" | "checks" | "commits";

/** One PR, whole, and what the operator can do with it. */
function PullView({ ctx, pr, me, onActed }: { ctx: ToolContext; pr: GithubPr; me: string; onActed: () => void }) {
  const pane = useContext(PaneContext);
  const [view, setView] = useState<PrData | null>(null);
  const [error, setError] = useState("");
  const load = useCallback(
    () =>
      githubPr({ repository: pr.repository, number: pr.number }).then(
        (v) => {
          setView(v);
          setError("");
        },
        (e) => setError(said(e)),
      ),
    [pr.repository, pr.number],
  );
  // The list's copy changing (an event, a read) is the PR changing.
  useEffect(() => {
    void load();
  }, [load, pr.updated_at, pr.checks.pass, pr.checks.fail, pr.checks.pending, pr.comments]);
  const [tab, setTab] = useState<Tab>("conversation");
  useSecretShown(tab === "files" && view?.secret === true);
  const [drafts, setDrafts] = useState<LineComment[]>([]);
  const conv = threadOf(ctx, pr.thread_stream_id);
  const acted = () => {
    void load();
    onActed();
  };
  const counts: Record<Tab, number | null> = view
    ? {
        conversation: view.comments.length + view.reviews.filter((r) => r.body.trim() || r.state !== "COMMENTED").length + view.threads.length,
        files: view.files.length,
        checks: view.checks.length,
        commits: view.commits.length,
      }
    : { conversation: null, files: null, checks: null, commits: null };
  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="flex h-9 shrink-0 items-center gap-2 border-(--line-soft) border-b px-3">
        <span className="min-w-0 flex-1 truncate font-bold text-(--fg)">
          #{pr.number} {pr.title}
        </span>
        <PrChips pr={pr} />
        {conv && (
          <button className={pill} onClick={() => pane?.enter(ctx.workspace.id, conv)} title={`Open ${conv.title}`} type="button">
            <StatusDot size={12} status={conv.status} streamId={conv.streamId} />
            {conv.title}
          </button>
        )}
        <GitHubLink url={pr.url} />
      </div>
      <div className="flex h-8 shrink-0 items-end gap-1 border-(--line-soft) border-b px-2.5" role="tablist">
        {(["conversation", "files", "checks", "commits"] as const).map((t) => (
          <button aria-selected={tab === t} className={TAB(tab === t)} key={t} onClick={() => setTab(t)} role="tab" type="button">
            {t[0].toUpperCase() + t.slice(1)}
            {counts[t] !== null ? ` · ${counts[t]}` : ""}
          </button>
        ))}
      </div>
      <div className="flex min-h-0 flex-1 flex-col overflow-hidden">
        {error && <p className="p-3 text-(--bad) text-[11px]">{error}</p>}
        {!view && !error && <p className="p-3 text-(--fg-faint)">Reading #{pr.number}…</p>}
        {view && tab === "conversation" && <ConversationTab me={me} onActed={acted} view={view} />}
        {view && tab === "files" && <FilesTab conv={conv} ctx={ctx} drafts={drafts} setDrafts={setDrafts} view={view} />}
        {view && tab === "checks" && <ChecksTab conv={conv} pr={pr} view={view} />}
        {view && tab === "commits" && <CommitsTab view={view} />}
      </div>
      {view && actsHere() && <ReviewBar drafts={drafts} me={me} onActed={acted} pr={pr} setDrafts={setDrafts} />}
    </div>
  );
}

function ConversationTab({ view, me, onActed }: { view: PrData; me: string; onActed: () => void }) {
  const timeline = useMemo(
    () =>
      [
        ...view.reviews.filter((r) => r.body.trim() || r.state !== "COMMENTED").map((r) => ({ at: r.submitted_at, key: `r${r.id}`, author: r.author, body: r.body, state: r.state })),
        ...view.comments.map((c) => ({ at: c.created_at, key: `c${c.id}`, author: c.author, body: c.body, state: null as string | null })),
      ].sort((a, b) => (a.at < b.at ? -1 : a.at > b.at ? 1 : 0)),
    [view],
  );
  return (
    <div className="flex min-h-0 flex-1 flex-col gap-3 overflow-auto p-3">
      <div className="rounded-md border border-(--line-soft) bg-(--bg-raised) p-3">
        <div className="mb-1.5 text-(--fg-dim) text-[11px]">
          {view.pr.author} opened it {ago(view.pr.created_at)} ago · {view.pr.branch} → {view.pr.base}
        </div>
        <Said text={view.body || "No description."} />
      </div>
      {timeline.map((t) => (
        <div className="rounded-md border border-(--line-soft) px-3 py-2" key={t.key}>
          <div className="mb-1 flex items-center gap-2 text-[11px]">
            <span className="font-bold text-(--fg)">{t.author === me ? "you" : t.author}</span>
            {t.state && REVIEW_STATE[t.state] && <Chip tone={REVIEW_STATE[t.state][0]}>{REVIEW_STATE[t.state][1]}</Chip>}
            <span className="text-(--fg-faint)">{ago(t.at)}</span>
          </div>
          <Said text={t.body} />
        </div>
      ))}
      {view.threads.map((th) => (
        <ThreadCard key={th.id} me={me} onActed={onActed} repository={view.pr.repository} number={view.pr.number} thread={th} />
      ))}
    </div>
  );
}

/** A review thread on a line: its comments, a reply, and Resolve. */
function ThreadCard({ thread, repository, number, me, onActed }: { thread: ReviewThread; repository: string; number: number; me: string; onActed: () => void }) {
  const [reply, setReply] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const run = async (what: string, go: () => Promise<unknown>) => {
    if (!(await ask(what))) return;
    setBusy(true);
    setError("");
    try {
      await go();
      setReply("");
      onActed();
    } catch (e) {
      setError(said(e));
    }
    setBusy(false);
  };
  const first = thread.comments[0];
  return (
    <div className={`rounded-md border px-3 py-2 ${thread.resolved ? "border-(--line-soft) opacity-75" : "border-(--line)"}`}>
      <div className="mb-1.5 flex items-center gap-2 text-[11px]">
        <span className="min-w-0 truncate font-mono text-(--fg-2)">
          {thread.path}
          {thread.line ? `:${thread.line}` : ""}
        </span>
        {thread.resolved && <Chip tone="ok">RESOLVED</Chip>}
        {thread.outdated && <Chip>OUTDATED</Chip>}
        <span className="flex-1" />
        {actsHere() && <button
          className={pill}
          disabled={busy}
          onClick={() =>
            void run(`${thread.resolved ? "Reopen" : "Resolve"} this conversation on #${number} as ${me}?`, () =>
              githubPrThreadResolve({ repository, thread_id: thread.id, resolved: !thread.resolved }),
            )
          }
          type="button"
        >
          {thread.resolved ? "Reopen" : "Resolve"}
        </button>}
      </div>
      {thread.comments.map((c) => (
        <div className="border-(--line-soft) border-t py-1.5 first:border-t-0" key={c.id}>
          <div className="mb-0.5 flex items-center gap-2 text-[11px]">
            <span className="font-bold text-(--fg)">{c.author === me ? "you" : c.author}</span>
            <span className="text-(--fg-faint)">{ago(c.created_at)}</span>
          </div>
          <Said text={c.body} />
        </div>
      ))}
      {first && actsHere() && (
        <div className="mt-1.5 flex items-end gap-2">
          <textarea aria-label="Reply" className={area} onChange={(e) => setReply(e.target.value)} placeholder="Reply…" rows={2} value={reply} />
          <button
            className={solid}
            disabled={busy || !reply.trim()}
            onClick={() =>
              void run(`Post this reply on #${number} as ${me}?`, () => githubPrComment({ repository, number, body: reply.trim(), in_reply_to: first.id }))
            }
            type="button"
          >
            Reply
          </button>
        </div>
      )}
      {error && <p className="mt-1 text-(--bad) text-[11px]">{error}</p>}
    </div>
  );
}

function FilesTab({
  ctx,
  view,
  conv,
  drafts,
  setDrafts,
}: {
  ctx: ToolContext;
  view: PrData;
  conv: Conversation | null;
  drafts: LineComment[];
  setDrafts: (f: (d: LineComment[]) => LineComment[]) => void;
}) {
  const files = useMemo(() => parseUnifiedDiff(view.diff), [view.diff]);
  const [picked, setPicked] = useState<string | null>(null);
  const file = files.find((f) => f.path === picked) ?? files[0] ?? null;
  // A thread's own PR asks its thread about a block, as Merge review does.
  const project = ctx.snap.projects.find((p) => p.checkouts.some((c) => c.streamId === conv?.streamId)) ?? null;
  const checkout = project?.checkouts.find((c) => c.streamId === conv?.streamId) ?? null;
  const candidate: Candidate | null = checkout && project ? { ...checkout, conv, base: view.pr.base || baseOf(project.checkouts)?.branch || "main" } : null;
  if (files.length === 0) {
    return (
      <p className="p-3 text-(--fg-dim)">
        {view.diff_truncated ? "GitHub won't send a diff this large; read it there." : "No changes."} <a className="text-(--link)" href={`${view.pr.url}/files`} rel="noreferrer" target="_blank">Files on GitHub</a>
      </p>
    );
  }
  return (
    <div className="flex min-h-0 flex-1">
      <aside aria-label="Files" className="flex w-[min(280px,34%)] shrink-0 flex-col overflow-auto border-(--line-soft) border-r bg-(--bg-chrome)">
        {view.diff_truncated && <p className="px-3 py-1 text-(--warn) text-[11px]">cut short</p>}
        {files.map((f) => (
          <button
            className={`flex h-8 w-full shrink-0 items-center gap-2 px-3 text-left ${f === file ? "bg-(--accent-sel) shadow-[inset_0_0_0_1px_var(--accent-border)]" : "hover:bg-(--bg-chip)"}`}
            key={`${f.oldPath ?? ""}→${f.path}`}
            onClick={() => setPicked(f.path)}
            title={f.path}
            type="button"
          >
            <span className="min-w-0 flex-1 truncate text-(--fg-body)">{f.path.split("/").pop()}</span>
            {drafts.some((d) => d.path === f.path) && <MessageSquareIcon className="size-3 shrink-0 text-(--accent)" />}
            <span className="shrink-0 font-mono text-[11px]">
              <span className="text-(--diff-add-fg)">+{f.add}</span> <span className="text-(--diff-del-fg)">−{f.del}</span>
            </span>
          </button>
        ))}
      </aside>
      {file && (
        <section aria-label={file.path} className="flex min-w-0 flex-1 flex-col gap-3 overflow-auto p-3" key={file.path}>
          <div className="truncate font-bold text-(--fg)">{file.oldPath ? `${file.oldPath} → ${file.path}` : file.path}</div>
          {file.status === "binary" && <p className="text-(--fg-faint)">Binary file; not shown.</p>}
          {file.hunks.map((h, i) => (
            <PrHunk
              candidate={candidate}
              ctx={ctx}
              drafts={drafts.filter((d) => d.path === file.path && d.line === hunkAnchor(h)?.line)}
              file={file}
              hunk={h}
              key={`${i}:${h.header}`}
              onDraft={(d) => setDrafts((all) => [...all, d])}
              onDrop={(d) => setDrafts((all) => all.filter((x) => x !== d))}
              project={project?.name ?? ""}
              secret={view.secret === true}
            />
          ))}
        </section>
      )}
    </div>
  );
}

/** One block of a PR's change: the thread's own Ask when it has one, and a
 *  comment for the review, on the block's last line. */
function PrHunk({
  ctx,
  candidate,
  project,
  file,
  hunk,
  drafts,
  onDraft,
  onDrop,
  secret,
}: {
  ctx: ToolContext;
  candidate: Candidate | null;
  project: string;
  file: FileDiff;
  hunk: Hunk;
  drafts: LineComment[];
  onDraft: (d: LineComment) => void;
  onDrop: (d: LineComment) => void;
  secret: boolean;
}) {
  const anchor = hunkAnchor(hunk);
  const [writing, setWriting] = useState(false);
  const [text, setText] = useState("");
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
  return (
    <div className="flex flex-col gap-1.5">
      {candidate ? (
        <HunkBlock candidate={candidate} ctx={ctx} file={file} hunk={hunk} project={project} secret={secret} />
      ) : (
        <div className="overflow-hidden rounded-md border border-(--line)">
          <div className="border-(--line-soft) border-b bg-(--bg-raised) px-2.5 py-1 font-mono text-(--fg-dim) text-[11px]">{hunk.header}</div>
          <SplitDiff newText={newText} oldText={oldText} />
        </div>
      )}
      {drafts.map((d) => (
        <div className="flex items-start gap-2 rounded-[5px] border border-(--accent-border) bg-(--accent-sel) px-2 py-1.5" key={`${d.line}:${d.body}`}>
          <MessageSquareIcon className="mt-0.5 size-3 shrink-0 text-(--accent)" />
          <span className="min-w-0 flex-1 whitespace-pre-wrap break-words text-(--fg-body) text-[11.5px]">{d.body}</span>
          <button className="text-(--fg-dim) text-[11px] hover:text-(--fg)" onClick={() => onDrop(d)} type="button">
            Remove
          </button>
        </div>
      ))}
      {anchor &&
        actsHere() &&
        (writing ? (
          <div className="flex items-end gap-2">
            <textarea aria-label="Comment on this block" autoFocus className={area} onChange={(e) => setText(e.target.value)} placeholder="Comment on this block…" rows={2} value={text} />
            <button
              className={solid}
              disabled={!text.trim()}
              onClick={() => {
                onDraft({ path: file.path, line: anchor.line, side: anchor.side, body: text.trim() });
                setText("");
                setWriting(false);
              }}
              type="button"
            >
              Add to review
            </button>
            <button className={pill} onClick={() => setWriting(false)} type="button">
              Cancel
            </button>
          </div>
        ) : (
          <button className="flex w-fit items-center gap-1.5 text-(--fg-dim) text-[11px] hover:text-(--fg)" onClick={() => setWriting(true)} type="button">
            <MessageSquareIcon className="size-3" />
            Comment on this block
          </button>
        ))}
    </div>
  );
}

function ChecksTab({ pr, view, conv }: { pr: GithubPr; view: PrData; conv: Conversation | null }) {
  const [open, setOpen] = useState<number | null>(view.checks.find((c) => c.bucket === "fail" && c.job_id)?.job_id ?? null);
  const handOff = useHandOff(conv);
  const failing = view.checks.filter((c) => c.bucket === "fail");
  const rerunIds = [...new Set(failing.flatMap((c) => (c.run_id ? [c.run_id] : [])))];
  const [rerun, setRerun] = useState("");
  return (
    <div className="flex min-h-0 flex-1 flex-col overflow-auto p-2.5">
      {view.checks.length === 0 && <p className="p-1 text-(--fg-faint)">No checks on this PR.</p>}
      {view.checks.map((c) => (
        <CheckRow check={c} key={`${c.workflow}:${c.name}`} onToggle={() => setOpen((o) => (o === c.job_id ? null : (c.job_id ?? null)))} open={!!c.job_id && open === c.job_id} repository={pr.repository} />
      ))}
      {failing.length > 0 && (
        <div className="mt-2 flex flex-wrap items-center gap-2 px-2">
          {rerunIds.length > 0 && actsHere() && (
            <button
              className={pill}
              disabled={rerun !== ""}
              onClick={async () => {
                if (!(await ask(`Re-run the failed jobs of ${failing.map((c) => c.name).join(", ")} on #${pr.number}?`))) return;
                try {
                  for (const id of rerunIds) await githubRunRerun({ repository: pr.repository, run_id: id, failed_only: true });
                  setRerun("Re-running");
                } catch (e) {
                  setRerun(said(e));
                }
              }}
              type="button"
            >
              <RefreshCwIcon className="size-3" />
              {rerun || "Re-run failed"}
            </button>
          )}
          {handOff(
            `On pull request #${pr.number} (${pr.url}), ${failing.length === 1 ? "this check fails" : "these checks fail"}: ${failing.map((c) => `${c.name}${c.link ? ` (${c.link})` : ""}`).join(", ")}. Find out why and fix it.`,
          )}
        </div>
      )}
    </div>
  );
}

function CheckRow({ check, repository, open, onToggle }: { check: GithubCheck; repository: string; open: boolean; onToggle: () => void }) {
  const [glyph, tone] = BUCKET[check.bucket] ?? ["·", "text-(--fg-dim)"];
  return (
    <div className="flex flex-col">
      <div className="flex h-8 min-w-0 items-center gap-2.5 rounded-[5px] px-2 hover:bg-(--bg-chip)">
        <span className={`w-3 shrink-0 text-center ${tone}`}>{glyph}</span>
        {check.job_id ? (
          <button aria-expanded={open} className="min-w-0 truncate text-left text-(--fg-body) hover:text-(--fg)" onClick={onToggle} type="button">
            {check.name}
          </button>
        ) : (
          <span className="min-w-0 truncate text-(--fg-body)">{check.name}</span>
        )}
        {check.workflow && <span className="min-w-0 truncate text-(--fg-dim) text-[11px]">{check.workflow}</span>}
        <span className="flex-1" />
        {check.link && (
          <a aria-label={`${check.name} on GitHub`} className="shrink-0 text-(--fg-dim) hover:text-(--fg)" href={check.link} rel="noreferrer" target="_blank" title={check.link}>
            <ExternalLinkIcon className="size-3" />
          </a>
        )}
      </div>
      {open && check.job_id && (
        <div className="ml-7">
          <JobLog jobId={check.job_id} repository={repository} />
        </div>
      )}
    </div>
  );
}

/** A finished job's log by step, its failing steps open. */
function JobLog({ repository, jobId }: { repository: string; jobId: number }) {
  const [log, setLog] = useState<RunLog | null>(null);
  const [error, setError] = useState("");
  useEffect(() => {
    let live = true;
    githubRunLog({ repository, job_id: jobId }).then(
      (l) => live && setLog(l),
      (e) => live && setError(said(e)),
    );
    return () => {
      live = false;
    };
  }, [repository, jobId]);
  if (error) return <p className="px-2 py-1 text-(--fg-dim) text-[11px]">{error}</p>;
  if (!log) return <p className="px-2 py-1 text-(--fg-faint) text-[11px]">Reading the log…</p>;
  return (
    <div className="mb-2 flex flex-col gap-1">
      {log.steps.map((s) => (
        <details className="rounded-[5px] border border-(--line-soft)" key={s.number} open={s.conclusion === "failure"}>
          <summary className={`cursor-pointer px-2 py-1 text-[11px] ${s.conclusion === "failure" ? "text-(--bad)" : "text-(--fg-2)"}`}>
            {s.number > 0 ? `${s.number}. ` : ""}
            {s.name || "Other lines"}
          </summary>
          <pre className="max-h-72 overflow-auto whitespace-pre-wrap break-all bg-(--bg-app) px-2 py-1.5 font-mono text-(--fg-body) text-[10.5px] leading-snug">{s.log}</pre>
        </details>
      ))}
      {log.truncated && <p className="text-(--fg-faint) text-[11px]">Each step keeps its last 64 KiB.</p>}
    </div>
  );
}

function CommitsTab({ view }: { view: PrData }) {
  return (
    <div className="flex min-h-0 flex-1 flex-col overflow-auto p-2.5">
      {view.commits.map((c) => (
        <div className="flex h-7 min-w-0 items-center gap-2.5 rounded-[5px] px-2 hover:bg-(--bg-chip)" key={c.sha}>
          <span className="shrink-0 font-mono text-(--fg-dim) text-[11px]">{c.sha.slice(0, 7)}</span>
          <span className="min-w-0 flex-1 truncate text-(--fg-body)">{c.subject}</span>
          <span className="shrink-0 text-(--fg-dim) text-[11px]">{c.author}</span>
          <span className="w-8 shrink-0 text-right text-(--fg-faint) text-[11px]">{ago(c.committed_at)}</span>
        </div>
      ))}
    </div>
  );
}

/** What the operator says back: a comment, a review with the block comments
 *  gathered so far, a merge, or ready for review. Each asks first. */
function ReviewBar({
  pr,
  me,
  drafts,
  setDrafts,
  onActed,
}: {
  pr: GithubPr;
  me: string;
  drafts: LineComment[];
  setDrafts: (f: (d: LineComment[]) => LineComment[]) => void;
  onActed: () => void;
}) {
  const [body, setBody] = useState("");
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState<{ bad: boolean; text: string } | null>(null);
  const [method, setMethod] = useState<"merge" | "squash">("merge");
  // Where the repository's own setting says, as GitHub's merge does; the
  // operator can still choose either way for this merge.
  const [deleteBranch, setDeleteBranch] = useState(pr.delete_branch_on_merge);
  const lines = drafts.length > 0 ? ` with ${drafts.length} block comment${drafts.length === 1 ? "" : "s"}` : "";
  const act = async <T,>(question: string, go: () => Promise<T>, done: string | ((result: T) => string)) => {
    if (!(await ask(question))) return;
    setBusy(true);
    setNotice(null);
    try {
      const result = await go();
      setBody("");
      setDrafts(() => []);
      setNotice({ bad: false, text: typeof done === "string" ? done : done(result) });
      onActed();
    } catch (e) {
      setNotice({ bad: true, text: said(e) });
    }
    setBusy(false);
  };
  const review = (event: "COMMENT" | "APPROVE" | "REQUEST_CHANGES", label: string, done: string) =>
    act(`${label} #${pr.number} as ${me}${lines}?`, () => githubPrReview({ repository: pr.repository, number: pr.number, event, body: body.trim(), comments: drafts }), done);
  return (
    <div className="flex shrink-0 flex-col gap-2 border-(--line) border-t bg-(--bg-raised) p-2.5">
      <textarea aria-label="Your comment" className={area} onChange={(e) => setBody(e.target.value)} placeholder={`Comment on #${pr.number}…`} rows={2} value={body} />
      <div className="flex flex-wrap items-center gap-1.5">
        {drafts.length > 0 && <Chip tone="accent">{drafts.length} ON BLOCKS</Chip>}
        <button
          className={pill}
          disabled={busy || (!body.trim() && drafts.length === 0)}
          onClick={() =>
            void (drafts.length > 0
              ? review("COMMENT", "Comment on", "Review comments sent.")
              : act(`Post this comment on #${pr.number} as ${me}?`, () => githubPrComment({ repository: pr.repository, number: pr.number, body: body.trim() }), "Comment posted."))
          }
          type="button"
        >
          Comment
        </button>
        <button className={pill} disabled={busy || (!body.trim() && drafts.length === 0)} onClick={() => void review("REQUEST_CHANGES", "Request changes on", "Changes requested.")} type="button">
          Request changes
        </button>
        <button className={pill} disabled={busy || pr.author === me} onClick={() => void review("APPROVE", "Approve", "Approved.")} title={pr.author === me ? "GitHub won't let you approve your own PR" : undefined} type="button">
          Approve
        </button>
        <span className="flex-1" />
        {pr.draft ? (
          <button className={solid} disabled={busy} onClick={() => void act(`Mark #${pr.number} ready for review as ${me}?`, () => githubPrReady({ repository: pr.repository, number: pr.number }), "Ready for review.")} type="button">
            Ready for review
          </button>
        ) : (
          <>
            <select aria-label="How to merge" className="h-6 rounded-[5px] border border-(--line) bg-(--bg-app) px-1.5 text-[11px]" onChange={(e) => setMethod(e.target.value as "merge" | "squash")} value={method}>
              <option value="merge">Merge commit</option>
              <option value="squash">Squash</option>
            </select>
            <label className="flex items-center gap-1 text-(--fg-dim) text-[11px]">
              <input checked={deleteBranch} className="accent-(--accent)" onChange={(e) => setDeleteBranch(e.target.checked)} type="checkbox" />
              delete branch
            </label>
            <button
              className={solid}
              disabled={busy}
              onClick={() =>
                void act(
                  `${method === "squash" ? "Squash and merge" : "Merge"} #${pr.number} into ${pr.base} as ${me}${deleteBranch ? `, then delete ${pr.branch}` : ""}?`,
                  () => githubPrMerge({ repository: pr.repository, number: pr.number, method, delete_branch: deleteBranch }),
                  (merged) => `Merged into ${pr.base}; ${pr.branch} ${merged.branch_deleted ? "deleted" : "kept"}.`,
                )
              }
              type="button"
            >
              <GitMergeIcon className="size-3" />
              Merge
            </button>
          </>
        )}
      </div>
      {notice && <p className={`text-[11px] ${notice.bad ? "text-(--bad)" : "text-(--ok)"}`}>{notice.text}</p>}
    </div>
  );
}

// ---- Actions -------------------------------------------------------------

/** The node's Actions runs, kept like the PR list. */
export function useRuns() {
  const c = useCached<Runs>("github:runs", githubRuns);
  const { reload } = c;
  usePoll(useCallback(() => void reload(), [reload]), READ_EVERY);
  useInboxUpdates("runs", "github:runs", reload);
  useEffect(() => onEnvelope("github.run.updated", (e) => patchKept<Runs>("github:runs", (d) => ({ ...d, runs: applyRunUpdate(d.runs, (e as { run: GithubRun }).run) }))), []);
  return { data: c.data, error: c.error, busy: c.busy, at: c.at, reload: () => void reload() };
}

export function ActionsPanel({ ctx }: { ctx: ToolContext }) {
  const { data, error, busy, at, reload } = useRuns();
  const runs = data?.runs ?? [];
  const [repo, setRepo] = useState("");
  const inRepo = runs.filter((r) => !repo || sameRepo(r.repository, repo));
  const [group, setGroup] = useState<RunGroup | null>(null);
  const shownGroup = group ?? (inRepo.some((r) => isFailedRun(r)) ? "failing" : "all");
  const shown = inRepo.filter((r) => inRunGroup(r, shownGroup));
  const repos = [...new Set(runs.map((r) => r.repository))].sort();
  const [picked, setPicked] = useState<number | null>(null);
  const current = shown.find((r) => r.run_id === picked) ?? null;
  return (
    <div className="flex h-full min-h-0 flex-col bg-(--bg-panel) text-xs" data-testid="actions-panel">
      <div className="flex h-9 shrink-0 items-center gap-1.5 border-(--line-soft) border-b bg-(--bg-raised) px-2.5">
        {RUN_GROUPS.map(([g, label]) => {
          const n = inRepo.filter((r) => inRunGroup(r, g)).length;
          return (
            <button aria-pressed={g === shownGroup} className={GROUP_BUTTON(g === shownGroup)} key={g} onClick={() => setGroup(g)} type="button">
              {label}
              <Chip tone={g === "failing" && n > 0 ? "bad" : g === shownGroup ? "accent" : "neutral"}>{n}</Chip>
            </button>
          );
        })}
        <span className="flex-1" />
        <select aria-label="Repository" className="h-6 max-w-56 rounded-[5px] border border-(--line) bg-(--bg-app) px-1.5 font-bold text-(--hue-github)" onChange={(e) => setRepo(e.target.value)} value={repo}>
          <option value="">All repositories</option>
          {repos.map((r) => (
            <option key={r} value={r}>
              {r}
            </option>
          ))}
        </select>
        <Freshness at={at} busy={busy} error={error} onReload={reload} />
      </div>
      {(error || (data?.problems.length ?? 0) > 0) && <p className="border-(--line-soft) border-b px-3 py-1 text-(--warn) text-[11px]">{error || data?.problems[0]}</p>}
      {!data && !error && <p className="p-3 text-(--fg-faint)">Asking GitHub…</p>}
      {data && shown.length === 0 && <p className="p-3 text-(--fg-faint)">No runs here.</p>}
      <div className={`${current ? "max-h-[38%] shrink-0 border-(--line) border-b" : "min-h-0 flex-1"} overflow-auto`}>
        {shown.map((r) => (
          <RunLine ctx={ctx} key={`${r.repository}:${r.run_id}`} on={r === current} onPick={() => setPicked((p) => (p === r.run_id ? null : r.run_id))} run={r} />
        ))}
      </div>
      {current && <RunPanel ctx={ctx} key={current.run_id} run={current} />}
    </div>
  );
}

/** A run's state as a mark: still going, passed, failed, or neither. */
function runMark(run: GithubRun): [string, string] {
  if (run.status !== "completed") return ["◐", "text-(--warn)"];
  if (run.conclusion === "success") return ["✓", "text-(--ok)"];
  if (isFailedRun(run)) return ["✕", "text-(--bad)"];
  return ["–", "text-(--fg-faint)"];
}

function RunLine({ ctx, run, on, onPick }: { ctx: ToolContext; run: GithubRun; on: boolean; onPick: () => void }) {
  const conv = threadOf(ctx, run.thread_stream_id);
  const [mark, tone] = runMark(run);
  return (
    <div
      className={`flex h-[46px] cursor-pointer items-center gap-2.5 border-(--line-soft) border-b px-3 ${on ? "bg-(--accent-sel) shadow-[inset_3px_0_0_var(--accent)]" : "hover:bg-(--bg-chip)"}`}
      onClick={onPick}
    >
      <span className={`w-3 shrink-0 text-center ${tone}`} title={run.conclusion || run.status}>
        {mark}
      </span>
      <span className="flex min-w-0 flex-1 flex-col">
        <span className="truncate font-bold text-(--fg)" title={run.title}>
          {run.workflow} <span className="font-normal text-(--fg-dim)">· {run.title}</span>
        </span>
        <span className="flex min-w-0 items-center gap-1.5 truncate text-(--fg-dim) text-[11px]">
          {run.repository} · {run.branch} · {run.event}
          {conv && (
            <>
              <span>·</span>
              <StatusDot size={11} status={conv.status} streamId={conv.streamId} />
              {conv.title}
            </>
          )}
        </span>
      </span>
      <GitHubLink url={run.url} />
      <span className="w-8 shrink-0 text-right text-(--fg-faint) text-[11px]" title={run.created_at}>
        {ago(run.created_at)}
      </span>
    </div>
  );
}

/** A picked run: its jobs and their steps, a job's log, re-running what
 *  failed, and handing a failure to its thread with the log's tail. */
function RunPanel({ ctx, run }: { ctx: ToolContext; run: GithubRun }) {
  const [view, setView] = useState<RunData | null>(null);
  const [error, setError] = useState("");
  useEffect(() => {
    let live = true;
    githubRun({ repository: run.repository, run_id: run.run_id }).then(
      (v) => live && setView(v),
      (e) => live && setError(said(e)),
    );
    return () => {
      live = false;
    };
  }, [run.repository, run.run_id, run.updated_at]);
  const failedJobs = view?.jobs.filter((j) => isFailedRun({ status: j.status, conclusion: j.conclusion, branch: run.branch })) ?? [];
  const [job, setJob] = useState<number | null>(null);
  const shownJob = job ?? failedJobs[0]?.job_id ?? view?.jobs[0]?.job_id ?? null;
  const conv = threadOf(ctx, run.thread_stream_id);
  const handOff = useHandOff(conv);
  const [rerun, setRerun] = useState("");
  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="flex h-9 shrink-0 items-center gap-2 border-(--line-soft) border-b px-3">
        <span className={runMark(run)[1]}>{runMark(run)[0]}</span>
        <span className="min-w-0 flex-1 truncate font-bold text-(--fg)">
          {run.workflow} · {run.branch}
        </span>
        {isFailedRun(run) && actsHere() && (
          <button
            className={pill}
            disabled={rerun !== ""}
            onClick={async () => {
              if (!(await ask(`Re-run the failed jobs of ${run.workflow} on ${run.branch}?`))) return;
              try {
                await githubRunRerun({ repository: run.repository, run_id: run.run_id, failed_only: true });
                setRerun("Re-running");
              } catch (e) {
                setRerun(said(e));
              }
            }}
            type="button"
          >
            <RefreshCwIcon className="size-3" />
            {rerun || "Re-run failed"}
          </button>
        )}
        {isFailedRun(run) && handOff(`The ${run.workflow} run on ${run.branch} failed (${run.url})${failedJobs.length ? `: ${failedJobs.map((j) => j.name).join(", ")}` : ""}. Find out why and fix it.`)}
      </div>
      {error && <p className="p-3 text-(--bad) text-[11px]">{error}</p>}
      {view && (
        <div className="flex min-h-0 flex-1">
          <aside className="flex w-[min(260px,34%)] shrink-0 flex-col overflow-auto border-(--line-soft) border-r bg-(--bg-chrome)">
            {view.jobs.map((j) => {
              const [m, t] = runMark({ ...run, status: j.status, conclusion: j.conclusion });
              return (
                <button
                  className={`flex h-8 w-full shrink-0 items-center gap-2 px-3 text-left ${j.job_id === shownJob ? "bg-(--accent-sel) shadow-[inset_0_0_0_1px_var(--accent-border)]" : "hover:bg-(--bg-chip)"}`}
                  key={j.job_id}
                  onClick={() => setJob(j.job_id)}
                  type="button"
                >
                  <span className={`w-3 shrink-0 text-center ${t}`}>{m}</span>
                  <span className="min-w-0 flex-1 truncate text-(--fg-body)">{j.name}</span>
                </button>
              );
            })}
          </aside>
          <section className="min-w-0 flex-1 overflow-auto p-2.5">{shownJob && <JobLog jobId={shownJob} key={shownJob} repository={run.repository} />}</section>
        </div>
      )}
    </div>
  );
}
