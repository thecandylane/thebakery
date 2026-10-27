// What waits for the operator to leave the Bakery (Round 9's A, arriving
// with the move-in): a count in the top bar while anything waits, and under
// it the list of every waiting card across the threads, each answered where
// it stands or opened in its thread; then Remote actions, everything that
// left, newest first. The cards are the threads' own (a push, a pull
// request, an issue, a comment): answering one here answers it there.

import { useEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";
import {
  ArrowUpFromLineIcon,
  CheckCircle2Icon,
  CircleDotIcon,
  CloudIcon,
  ExternalLinkIcon,
  GitBranchIcon,
  GitMergeIcon,
  GitPullRequestIcon,
  MessageSquareIcon,
  RotateCwIcon,
} from "lucide-react";
import {
  agentPipeGateDecide,
  controlBakery,
  controlRestartNow,
  type MergeWaiting,
  mergeWaiting,
  remoteHistory,
  remoteWaiting,
  type RemoteAction,
  type RestartWait,
  threadMerge,
  type Waiting,
} from "$lib/api.generated";
import { ask } from "$lib/ask";
import { ago } from "$lib/conversations";
import { isTauri } from "$lib/desktop";
import { onEnvelope, onEvent, onStatus } from "$lib/events";
import { keep, kept, useCached } from "./cache";
import { POP, useDismiss } from "./PanelMenu";
import { Chip, Freshness, said, type ToolContext } from "./ui";

// ---- the list ---------------------------------------------------------------

type State = { items: readonly Waiting[]; merges: readonly MergeWaiting[]; error: string; busy: boolean; at: number | null };
let state: State = { items: [], merges: [], error: "", busy: false, at: null };
const listeners = new Set<() => void>();
const set = (next: Partial<State>) => {
  state = { ...state, ...next };
  for (const listener of listeners) listener();
};
let loading = false;
let again = false;
function load() {
  if (loading) {
    again = true;
    return;
  }
  loading = true;
  set({ busy: true });
  // A node from before merge waiting has no such route: no merges then.
  Promise.all([remoteWaiting(), mergeWaiting().catch(() => ({ items: [] as MergeWaiting[] }))])
    .then(
      ([w, m]) => {
        const at = Date.now();
        // Kept for the next start; a card whose diff shows a credential-shaped
        // file keeps the list in memory only.
        keep("waiting", w.items.some((i) => i.push?.secret) ? { secret: true } : { items: w.items, merges: m.items }, at);
        set({ items: w.items, merges: m.items, error: "", at });
      },
      (e) => set({ error: said(e) }),
    )
    .finally(() => {
      loading = false;
      set({ busy: false });
      if (again) {
        again = false;
        load();
      }
    });
}
/** A card asked, answered, or closed anywhere moves the count; so does a
 *  chef's work asking to merge, or merging. */
const MOVES = new Set(["agent.child_tool.pending", "agent.child_tool.decided", "agent.approval.cancelled", "merge.waiting", "merge.done"]);
let started = false;
function start() {
  if (started) return;
  started = true;
  // What was waiting when the app last ran, at once; then the node's.
  const last = kept("waiting");
  const k = last?.data as { items?: Waiting[]; merges?: MergeWaiting[] } | undefined;
  if (last && k?.items) set({ items: k.items, merges: k.merges ?? [], at: last.at });
  load();
  window.addEventListener("focus", load);
  onEvent(null, (ev) => {
    if (MOVES.has(ev.family)) load();
  });
}
const subscribe = (listener: () => void) => {
  listeners.add(listener);
  return () => listeners.delete(listener);
};
function useWaiting(): State {
  useEffect(start, []);
  return useSyncExternalStore(subscribe, () => state);
}

/** The threads whose work waits to merge, by the node's own list, space
 *  separated: their glyph says ready to merge. A string, so a reload that
 *  lists the same ones redraws nothing. */
export function useMergeWaitingIds(): string {
  useEffect(start, []);
  return useSyncExternalStore(subscribe, () => state.merges.map((m) => m.stream_id).join(" "));
}

// ---- one waiting card -------------------------------------------------------

type Look = { label: string; hue: string; icon: typeof GitPullRequestIcon };
const LOOK: Record<string, Look> = {
  git_push: { label: "PUSH", hue: "var(--accent)", icon: ArrowUpFromLineIcon },
  git_pr: { label: "PULL REQUEST", hue: "var(--violet)", icon: GitPullRequestIcon },
  github_issue_create: { label: "ISSUE", hue: "var(--link)", icon: CircleDotIcon },
  github_issue_comment: { label: "COMMENT", hue: "var(--hue-containers)", icon: MessageSquareIcon },
  google_change: { label: "GOOGLE CLOUD", hue: "var(--link)", icon: CloudIcon },
};
const text = (v: unknown) => (typeof v === "string" ? v : "");

/** What a card would send, from its call's arguments. */
function described(w: Waiting): { title: string; sub: string; body: string; labels: string[] } {
  const a = (w.args && typeof w.args === "object" ? w.args : {}) as Record<string, unknown>;
  switch (w.tool) {
    case "git_push": {
      // The card's own preview when it has one: the same words as its card.
      const p = w.push;
      if (!p) return { title: text(a.branch) || "its branch", sub: text(a.commit) ? `sends ${text(a.commit).slice(0, 7)}` : "", body: "", labels: [] };
      const n = p.commits.length;
      const where = p.remote_head ? `on top of ${p.remote_head.slice(0, 7)}` : `new on the remote, from ${p.base}`;
      return { title: p.branch, sub: `${n} ${n === 1 ? "commit" : "commits"} · ${where} · sends ${p.head.slice(0, 7)}`, body: "", labels: [] };
    }
    case "git_pr":
      return { title: text(a.title) || "A pull request", sub: text(a.base) ? `into ${text(a.base)}` : "", body: text(a.body), labels: [] };
    case "github_issue_create":
      return { title: text(a.title) || "An issue", sub: text(a.repository), body: text(a.body), labels: Array.isArray(a.labels) ? a.labels.map(text).filter(Boolean) : [] };
    case "github_issue_comment":
      return { title: `on #${typeof a.number === "number" ? a.number : "?"}`, sub: text(a.repository), body: text(a.body), labels: [] };
    case "google_change":
      return { title: `${text(a.method)} ${text(a.url).replace(/^https:\/\//, "")}`, sub: `sends ${text(a.sends) || "nothing"}`, body: "", labels: [] };
    default:
      return { title: w.tool, sub: "", body: "", labels: [] };
  }
}

const BUTTON =
  "flex h-6 shrink-0 items-center gap-1.5 whitespace-nowrap rounded-[5px] border border-(--line) bg-(image:--grad-button) px-2.5 text-(--fg-body) text-[11px] hover:text-(--fg) disabled:opacity-40";
const SOLID =
  "flex h-6 shrink-0 items-center gap-1.5 rounded-[5px] border border-(--accent-fg) bg-(image:--grad-accent) px-2.5 font-bold text-(--fg-on-solid) text-[11px] hover:brightness-105 disabled:opacity-50";
const kindChip = (label: string, hue: string, Icon: typeof GitPullRequestIcon, width = "") => (
  <span
    className={`flex h-[18px] shrink-0 items-center gap-1 rounded px-1.5 font-bold text-[9.5px] tracking-[.06em] ${width}`}
    style={{ color: hue, background: `color-mix(in srgb, ${hue} 15%, transparent)`, boxShadow: `inset 0 0 0 1px color-mix(in srgb, ${hue} 42%, transparent)` }}
  >
    <Icon className="size-3 shrink-0" />
    <span className="truncate">{label}</span>
  </span>
);

function WaitingCard({ w, onOpen }: { w: Waiting; onOpen: () => void }) {
  const look = LOOK[w.tool] ?? { label: w.tool.toUpperCase(), hue: "var(--fg-dim)", icon: CircleDotIcon };
  const d = described(w);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const decide = (decision: "allow_once" | "allow_always" | "deny") => {
    setBusy(true);
    setError("");
    agentPipeGateDecide({ stream_id: w.stream_id, event_hash: w.card, decision }).then(
      () => load(),
      (e) => {
        setBusy(false);
        setError(said(e));
      },
    );
  };
  return (
    <div className="relative flex flex-col gap-1.5 border-(--line-soft) border-b py-2.5 pr-3 pl-4 last:border-b-0" data-testid="waiting-card">
      <span aria-hidden className="absolute inset-y-0 left-0 w-[3px]" style={{ background: look.hue }} />
      <div className="flex min-w-0 items-center gap-2">
        {kindChip(look.label, look.hue, look.icon)}
        <span className="min-w-0 flex-1 truncate font-bold text-(--fg)">{d.title}</span>
        <span className="shrink-0 text-(--fg-dim) text-[10.5px]">{ago(w.asked_at)}</span>
      </div>
      <div className="flex min-w-0 items-center gap-1.5 text-(--fg-dim) text-[11px]">
        <span className="truncate text-(--fg-body)">{w.name || "A thread"}</span>
        {d.sub && <span className="min-w-0 truncate">· {d.sub}</span>}
        {d.labels.map((l) => (
          <Chip key={l}>{l}</Chip>
        ))}
        <span className="flex-1" />
        <button className="shrink-0 text-(--link) text-[10.5px] hover:underline" onClick={onOpen} type="button">
          Open in thread ›
        </button>
      </div>
      {d.body && <p className="line-clamp-4 whitespace-pre-wrap break-words border-(--line) border-l-2 pl-2 text-(--fg-body) text-[11px]">{d.body}</p>}
      {w.push && w.push.commits.length > 0 && (
        <div className="max-h-28 overflow-auto rounded-md border border-(--line-soft) bg-(--bg-app) py-0.5 text-[11px]">
          {w.push.commits.map((c) => (
            <div className="flex min-w-0 gap-2 px-2 py-px" key={c.sha}>
              <span className="shrink-0 font-mono text-(--fg-dim)">{c.sha.slice(0, 7)}</span>
              <span className="min-w-0 truncate text-(--fg)">{c.subject}</span>
            </div>
          ))}
        </div>
      )}
      {w.push?.secret && (
        <span className="self-start">
          <Chip tone="bad">ITS DIFF SHOWS A CREDENTIAL-SHAPED FILE</Chip>
        </span>
      )}
      <div className="flex flex-wrap items-center gap-1.5">
        <button className={BUTTON} disabled={busy} onClick={() => decide("deny")} type="button">
          Deny
        </button>
        <button className={SOLID} disabled={busy} onClick={() => decide("allow_once")} type="button">
          Allow once
        </button>
        {/* As the push's own card offers it, and never from the phone. */}
        {w.grant && !isTauri() && (
          <button
            className="flex h-6 shrink-0 items-center gap-1.5 whitespace-nowrap rounded-[5px] border border-(--accent-border) px-2.5 text-(--accent-fg) text-[11px] hover:bg-(--bg-chip) disabled:opacity-40"
            disabled={busy}
            onClick={() => decide("allow_always")}
            title={
              w.grant.kind === "google"
                ? `This change, and every later change to ${w.grant.repository} in Google Cloud, never a delete; take it back from the thread's header`
                : `This push, and every later push of ${w.grant.branch}, never forced; take it back from the thread's header`
            }
            type="button"
          >
            {w.grant.kind === "google"
              ? `Approve, and let this conversation change ${w.grant.repository} in Google Cloud from now on, never deleting`
              : `Approve, and let this conversation push ${w.grant.branch} from now on`}
          </button>
        )}
        {error && (
          <span className="min-w-0 truncate text-(--bad) text-[11px]" title={error}>
            {error}
          </span>
        )}
      </div>
    </div>
  );
}

/** A chef's work waiting to merge: what it merges, its commits; Review opens
 *  it in the Merge review, and its card waits in its spawner's thread. */
function MergeCard({ m, onOpen, onReview }: { m: MergeWaiting; onOpen: () => void; onReview: () => void }) {
  const hue = "var(--ok)";
  const n = m.commits.length;
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const merge = async () => {
    if (!(await ask(`Merge ${m.branch} into ${m.base}?`))) return;
    setBusy(true);
    setError("");
    threadMerge({ stream_id: m.stream_id }).then(
      () => load(),
      (e) => {
        setBusy(false);
        setError(said(e));
      },
    );
  };
  return (
    <div className="relative flex flex-col gap-1.5 border-(--line-soft) border-b py-2.5 pr-3 pl-4 last:border-b-0" data-testid="waiting-merge">
      <span aria-hidden className="absolute inset-y-0 left-0 w-[3px]" style={{ background: hue }} />
      <div className="flex min-w-0 items-center gap-2">
        {kindChip("MERGE", hue, GitMergeIcon)}
        <span className="min-w-0 flex-1 truncate font-bold text-(--fg)">
          {m.branch} into {m.base}
        </span>
        <span className="shrink-0 text-(--fg-dim) text-[10.5px]">{ago(m.asked_at)}</span>
      </div>
      <div className="flex min-w-0 items-center gap-1.5 text-(--fg-dim) text-[11px]">
        <span className="truncate text-(--fg-body)">{m.name || "A thread"}</span>
        <span className="min-w-0 truncate">
          · {n} {n === 1 ? "commit" : "commits"} · <span className="text-(--diff-add-fg)">+{m.adds}</span> <span className="text-(--diff-del-fg)">−{m.dels}</span> ·{" "}
          {m.files.length} {m.files.length === 1 ? "file" : "files"}
        </span>
        <span className="flex-1" />
        <button className="shrink-0 text-(--link) text-[10.5px] hover:underline" onClick={onOpen} type="button">
          Open in Baker ›
        </button>
      </div>
      {m.note && <p className="line-clamp-4 whitespace-pre-wrap break-words border-(--line) border-l-2 pl-2 text-(--fg-body) text-[11px]">{m.note}</p>}
      {n > 0 && (
        <div className="max-h-28 overflow-auto rounded-md border border-(--line-soft) bg-(--bg-app) py-0.5 text-[11px]">
          {m.commits.map((c) => (
            <div className="flex min-w-0 gap-2 px-2 py-px" key={c.sha}>
              <span className="shrink-0 font-mono text-(--fg-dim)">{c.sha.slice(0, 7)}</span>
              <span className="min-w-0 truncate text-(--fg)">{c.subject}</span>
            </div>
          ))}
        </div>
      )}
      <div className="flex flex-wrap items-center gap-1.5">
        <button className={SOLID} onClick={onReview} type="button">
          Review
        </button>
        <button className={BUTTON} disabled={busy} onClick={() => void merge()} type="button">
          Merge
        </button>
        {error && (
          <span className="min-w-0 truncate text-(--bad) text-[11px]" title={error}>
            {error}
          </span>
        )}
      </div>
    </div>
  );
}

/** The count in the top bar, only while something waits; its list opens
 *  under it, merges first. */
export function WaitingPill({
  baker,
  openThread,
  openHistory,
}: {
  /** The Baker's main thread, where merge cards wait. */
  baker: string | null;
  openThread: (streamId: string) => void;
  openHistory: () => void;
}) {
  const { items, merges, error, busy, at } = useWaiting();
  const total = items.length + merges.length;
  const [open, setOpen] = useState(false);
  const box = useRef<HTMLDivElement>(null);
  useDismiss(open, box, () => setOpen(false));
  useEffect(() => {
    if (total === 0) setOpen(false);
  }, [total]);
  if (total === 0) return null;
  return (
    <div className="relative mr-2" ref={box}>
      <button
        aria-expanded={open}
        aria-label={`${total} waiting for you`}
        className={`flex h-6 items-center gap-1.5 rounded-full border border-(--accent-fg) bg-(image:--grad-accent) pr-2.5 pl-2 font-bold text-(--fg-on-solid) text-[11px] ${open ? "shadow-[0_0_0_3px_color-mix(in_srgb,var(--accent)_45%,transparent)]" : "shadow-[0_0_0_3px_color-mix(in_srgb,var(--accent)_14%,transparent)]"}`}
        data-testid="waiting-pill"
        onClick={() => setOpen((o) => !o)}
        title="Waiting for you"
        type="button"
      >
        <ArrowUpFromLineIcon className="size-3.5" />
        {total}
      </button>
      {open && (
        <div aria-label="Waiting for you" className={`${POP} w-[540px] overflow-hidden p-0`} role="dialog">
          <div className="flex h-9 items-center gap-2 border-(--line) border-b bg-(--warn-bg) px-3 font-bold text-(--accent) text-[10.5px] tracking-[.08em]">
            <ArrowUpFromLineIcon className="size-3.5" />
            WAITING FOR YOU
            <Chip tone="warn">{total}</Chip>
            <span className="flex-1" />
            <span className="font-normal tracking-normal">
              <Freshness at={at} busy={busy} error={error} onReload={load} />
            </span>
            <button
              className={`${BUTTON} h-[22px] font-normal tracking-normal`}
              onClick={() => {
                setOpen(false);
                openHistory();
              }}
              type="button"
            >
              History ›
            </button>
          </div>
          <div className="max-h-[70vh] overflow-y-auto">
            {merges.map((m) => (
              <MergeCard
                key={`${m.stream_id}:${m.asked_at}`}
                m={m}
                onOpen={() => {
                  setOpen(false);
                  openThread(m.spawner ?? baker ?? m.stream_id);
                }}
                onReview={() => {
                  setOpen(false);
                  window.dispatchEvent(new CustomEvent("bakery:review-thread", { detail: { streamId: m.stream_id } }));
                }}
              />
            ))}
            {items.map((w) => (
              <WaitingCard
                key={w.card}
                onOpen={() => {
                  setOpen(false);
                  openThread(w.stream_id);
                }}
                w={w}
              />
            ))}
          </div>
          {error && <p className="px-3 py-1.5 text-(--bad) text-[11px]">{error}</p>}
        </div>
      )}
    </div>
  );
}

// ---- a restart waiting for running turns ----------------------------------------

/** A restart (a deploy's, say) waiting for running turns, worker-2's 24/7
 *  work: said in the top bar while it waits, with Restart now; the turns it
 *  cuts off carry on after it. */
export function RestartPill() {
  const [wait, setWait] = useState<RestartWait | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  // The node says when the wait starts, when its turn count changes, and as
  // it goes ahead; a read each time the event socket opens covers what it missed.
  useEffect(() => {
    let live = true;
    const offStatus = onStatus((s) => {
      if (s !== "open") return;
      controlBakery().then(
        (b) => live && setWait(b.restart ?? null),
        () => {},
      );
    });
    const off = onEnvelope("restart-wait", (e) => setWait(e.restarting === true ? null : (e as unknown as RestartWait)));
    return () => {
      live = false;
      offStatus();
      off();
    };
  }, []);
  if (!wait) return null;
  const now = async () => {
    if (!(await ask("Restart now? The turns still running are cut off, and carry on after it."))) return;
    setBusy(true);
    setError("");
    controlRestartNow().then(
      () => {
        // It goes ahead now.
        setBusy(false);
        setWait(null);
      },
      (e) => {
        setBusy(false);
        const said_ = said(e);
        // Nothing waits any more: it went ahead meanwhile.
        if (/no restart is waiting/i.test(said_)) setWait(null);
        else setError(said_);
      },
    );
  };
  return (
    <div
      className="mr-2 flex h-6 min-w-0 items-center gap-2 rounded-full border border-(--warn-border) bg-(--warn-bg) pr-0.5 pl-2.5 text-(--warn) text-[11px]"
      data-testid="restart-wait"
      title={error || undefined}
    >
      <RotateCwIcon className="size-3.5 shrink-0" />
      <span className="min-w-0 truncate font-bold" title={`A restart waits for ${wait.turns} running ${wait.turns === 1 ? "turn" : "turns"} to finish`}>
        Restart waits for {wait.turns} {wait.turns === 1 ? "turn" : "turns"}
      </span>
      <button
        className="h-5 whitespace-nowrap rounded-full border border-(--warn-border) px-2 font-bold hover:bg-(--bg-chip) disabled:opacity-50"
        disabled={busy}
        onClick={() => void now()}
        type="button"
      >
        Restart now
      </button>
    </div>
  );
}

// ---- Remote actions: everything that left -------------------------------------

type Line = Record<string, unknown>;
const KIND: Record<string, [string, string, typeof GitPullRequestIcon, string]> = {
  push: ["PUSH", "var(--accent)", ArrowUpFromLineIcon, "pushes"],
  "pull request opened": ["PULL REQUEST", "var(--violet)", GitPullRequestIcon, "prs"],
  "ready for review": ["READY", "var(--violet)", GitPullRequestIcon, "prs"],
  "issue opened": ["ISSUE", "var(--link)", CircleDotIcon, "issues"],
  "issue comment": ["COMMENT", "var(--hue-containers)", MessageSquareIcon, "issues"],
  comment: ["COMMENT", "var(--hue-containers)", MessageSquareIcon, "issues"],
  review: ["REVIEW", "var(--ok)", CheckCircle2Icon, "reviews"],
  merge: ["MERGE", "var(--ok)", GitMergeIcon, "reviews"],
  "thread resolved": ["RESOLVED", "var(--fg-dim)", MessageSquareIcon, "reviews"],
  "thread reopened": ["REOPENED", "var(--fg-dim)", MessageSquareIcon, "reviews"],
  "re-run": ["RE-RUN", "var(--warn)", RotateCwIcon, "reruns"],
  "Google Cloud change": ["GOOGLE CLOUD", "var(--link)", CloudIcon, "google"],
};
const FILTERS: [string, string][] = [
  ["all", "All"],
  ["pushes", "Pushes"],
  ["prs", "Pull requests"],
  ["issues", "Issues and comments"],
  ["reviews", "Reviews and merges"],
  ["reruns", "Re-runs"],
  ["google", "Google Cloud"],
];
const num = (v: unknown) => (typeof v === "number" ? v : null);

/** What was done, as one line, and where it is on GitHub. */
function told(l: Line): { detail: string; branch: string; url: string } {
  const repo = text(l.repository);
  const n = num(l.number);
  const gh = (path: string) => (repo ? `https://github.com/${repo}/${path}` : "");
  const commit = text(l.commit);
  switch (text(l.what)) {
    case "push":
      return { detail: commit ? `at ${commit.slice(0, 7)}` : "", branch: text(l.branch), url: commit ? gh(`commit/${commit}`) : "" };
    case "pull request opened":
      return { detail: n ? `#${n}` : "", branch: text(l.branch), url: text(l.url) };
    case "issue opened": {
      const last = text(l.url).split("/").pop() ?? "";
      return { detail: /^\d+$/.test(last) ? `#${last}` : "", branch: "", url: text(l.url) };
    }
    case "issue comment":
      return { detail: n ? `on issue #${n}` : "", branch: "", url: text(l.url) };
    case "comment":
      return { detail: n ? `on #${n}` : "", branch: "", url: text(l.url) };
    case "review":
      return { detail: `${text(l.event).toLowerCase().replace(/_/g, " ") || "reviewed"}${n ? ` #${n}` : ""}`, branch: "", url: text(l.url) };
    case "merge":
      return { detail: [n ? `#${n}` : "", text(l.method), l.branch_deleted ? "branch deleted" : ""].filter(Boolean).join(" · "), branch: "", url: n ? gh(`pull/${n}`) : "" };
    case "ready for review":
      return { detail: n ? `#${n} ready for review` : "ready for review", branch: "", url: n ? gh(`pull/${n}`) : "" };
    case "thread resolved":
    case "thread reopened":
      return { detail: "a review thread", branch: "", url: "" };
    case "re-run": {
      const run = num(l.run_id);
      return { detail: `${l.failed_only ? "failed jobs of " : ""}run ${run ?? ""}`.trim(), branch: "", url: run ? gh(`actions/runs/${run}`) : "" };
    }
    case "Google Cloud change":
      return { detail: [`${text(l.method)} ${text(l.url).replace(/^https:\/\//, "")}`, text(l.status)].filter((x) => x.trim()).join(" · "), branch: "", url: "" };
    default:
      return { detail: text(l.what), branch: "", url: text(l.url) };
  }
}
const day = (iso: string) => new Date(iso).toLocaleDateString([], { weekday: "long", month: "short", day: "numeric" });
const time = (iso: string) => new Date(iso).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });

/** The Remote actions page: every remote action done, newest first, by day;
 *  by kind and by repository; who did it, its thread, and GitHub. */
export function RemoteHistory({ ctx }: { ctx: ToolContext }) {
  const { data: rows, error, busy, at, reload } = useCached<RemoteAction[]>("remote:history", () => remoteHistory({ limit: 1000 }));
  const [filter, setFilter] = useState("all");
  const [repo, setRepo] = useState("");
  useEffect(
    () =>
      onEvent(null, (ev) => {
        if (ev.family === "remote.done" && ev.stream_id === "sys:remote") void reload();
      }),
    [reload],
  );
  const lines = useMemo(() => (rows ?? []).map((r) => ({ r, l: (r.line && typeof r.line === "object" ? r.line : {}) as Line })), [rows]);
  const repos = [...new Set(lines.map(({ l }) => text(l.repository)).filter(Boolean))].sort();
  const kindOf = (l: Line) => KIND[text(l.what)]?.[3] ?? "other";
  const shown = lines.filter(({ l }) => (filter === "all" || kindOf(l) === filter) && (!repo || text(l.repository) === repo));
  const conv = (id: string) => ctx.snap.conversations.find((c) => c.streamId === id);
  /** "human" is the operator; an agent's id is its thread's and seat's. */
  const by = (who: string) => (who === "human" ? "you" : (conv(who.split(":").slice(0, -1).join(":"))?.title ?? who));
  const openThread = (id: string) =>
    window.dispatchEvent(new CustomEvent("bakery:open-conversation", { detail: { streamId: id, bakerThread: conv(id)?.bakerThread ?? false } }));
  let lastDay = "";

  return (
    <div className="flex h-full min-h-0 flex-col bg-(--bg-panel) text-xs" data-testid="remote-history">
      <div className="flex h-9 shrink-0 items-center gap-1.5 overflow-x-auto border-(--line-soft) border-b bg-(--bg-raised) px-2.5">
        {FILTERS.map(([f, label]) => (
          <button
            aria-pressed={f === filter}
            className={`flex h-6 shrink-0 items-center gap-1.5 rounded-[5px] border px-2 text-[11px] ${f === filter ? "border-(--accent-border) bg-(--accent-sel) font-bold text-(--fg)" : "border-transparent text-(--fg-dim) hover:bg-(--bg-chip) hover:text-(--fg-2)"}`}
            key={f}
            onClick={() => setFilter(f)}
            type="button"
          >
            {label}
            <Chip tone={f === filter ? "accent" : "neutral"}>{f === "all" ? lines.length : lines.filter(({ l }) => kindOf(l) === f).length}</Chip>
          </button>
        ))}
        <span className="flex-1" />
        <select aria-label="Repository" className="h-6 max-w-56 rounded-[5px] border border-(--line) bg-(--bg-app) px-1.5 text-(--fg-body)" onChange={(e) => setRepo(e.target.value)} value={repo}>
          <option value="">All repositories</option>
          {repos.map((r) => (
            <option key={r} value={r}>
              {r}
            </option>
          ))}
        </select>
        <Freshness at={at} busy={busy} error={error} onReload={() => void reload()} />
      </div>
      {error && <p className="border-(--line-soft) border-b px-3 py-1 text-(--bad) text-[11px]">{error}</p>}
      {rows === null && !error && <p className="p-3 text-(--fg-faint)">Reading…</p>}
      {rows !== null && shown.length === 0 && <p className="p-3 text-(--fg-faint)">Nothing has left the Bakery here yet.</p>}
      <div className="min-h-0 flex-1 overflow-auto">
        {shown.map(({ r, l }, i) => {
          const k = KIND[text(l.what)] ?? [text(l.what).toUpperCase() || "DONE", "var(--fg-dim)", CircleDotIcon, "other"];
          const t = told(l);
          const d = day(r.at);
          const head = d !== lastDay;
          lastDay = d;
          const thread = text(l.stream_id);
          return (
            <div key={`${r.at}:${i}`}>
              {head && <div className="bg-(--bg-app) px-3 py-1 font-bold text-(--fg-dim) text-[10px] uppercase tracking-[.06em]">{d}</div>}
              <div className="flex h-8 min-w-0 items-center gap-2.5 border-(--line-row) border-b px-3" data-testid="remote-action">
                <span className="w-16 shrink-0 text-(--fg-dim) text-[11px]">{time(r.at)}</span>
                {kindChip(k[0], k[1], k[2], "w-[118px]")}
                {t.branch && (
                  <span className="flex shrink-0 items-center gap-1 rounded border border-(--line) bg-(--bg-chip) px-1.5 text-(--fg-body) text-[10.5px]">
                    <GitBranchIcon className="size-3 text-(--violet)" />
                    {t.branch}
                  </span>
                )}
                <span className="min-w-24 flex-1 truncate text-(--fg)">{t.detail}</span>
                <span className="w-56 min-w-0 truncate text-(--fg-dim)">{text(l.repository)}</span>
                <span className="w-36 min-w-0 truncate text-(--fg-body)">{by(r.by)}</span>
                <span className="w-16 shrink-0">
                  {thread && (
                    <button className="text-(--link) text-[10.5px] hover:underline" onClick={() => openThread(thread)} type="button">
                      Thread ›
                    </button>
                  )}
                </span>
                <span className="w-5 shrink-0">
                  {t.url && (
                    <a aria-label="On GitHub" className="text-(--link)" href={t.url} rel="noreferrer" target="_blank" title={t.url}>
                      <ExternalLinkIcon className="size-3" />
                    </a>
                  )}
                </span>
              </div>
            </div>
          );
        })}
      </div>
    </div>
  );
}
