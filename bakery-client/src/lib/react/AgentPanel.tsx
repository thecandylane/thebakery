// The agent conversation panel: one React root, one bound agent.
//
// Everything rendered here is a Bakery fact taken from the snapshot. Where
// Bakery has no fact — a model's price, a reasoning stream, a plan — nothing
// is rendered, because a plausible-looking placeholder is a lie about what
// the agent did.
//
// Rows are memoised on a signature rather than object identity: a new event
// rebuilds the whole transcript array (`toTranscript` is a `$derived` over
// all events), so identity churns even for rows whose content is unchanged.
// The signature keeps that churn from re-rendering every row, and upstream's
// `MessageResponse` is itself memoised on its Markdown string, so Streamdown
// never reparses text that did not change.

import {
  type ComponentProps,
  type CSSProperties,
  type ReactNode,
  createContext,
  memo,
  useCallback,
  useContext,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  useSyncExternalStore,
} from "react";
import { useStickToBottomContext } from "use-stick-to-bottom";
import {
  Confirmation,
  ConfirmationAccepted,
  ConfirmationAction,
  ConfirmationActions,
  ConfirmationRejected,
  ConfirmationTitle,
} from "@/lib/ai-elements/confirmation";
import {
  Conversation,
  ConversationContent,
  ConversationEmptyState,
  ConversationScrollButton,
} from "@/lib/ai-elements/conversation";
import { Message, MessageAction, MessageActions, MessageContent, MessageResponse } from "@/lib/ai-elements/message";
import { Loader } from "@/lib/ai-elements/loader";
import {
  Queue,
  QueueItem,
  QueueItemAction,
  QueueItemActions,
  QueueItemContent,
  QueueItemDescription,
  QueueItemIndicator,
  QueueList,
  QueueSection,
  QueueSectionContent,
  QueueSectionLabel,
  QueueSectionTrigger,
} from "@/lib/ai-elements/queue";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogTitle } from "@/components/ui/dialog";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/components/ui/collapsible";
import { Task, TaskContent, TaskTrigger } from "@/lib/ai-elements/task";
import { onReveal, takeReveal } from "./subagentReveal";
import { patternFor, sharedTenant } from "./hostsModel";
import { useSecretShown } from "./shell/ui";
import { bytes } from "./shell/containersModel";
import { ask } from "$lib/ask";
import { parseUnifiedDiff } from "./shell/unifiedDiff";
import { isTauri } from "$lib/desktop";
import {
  BotIcon,
  BrainIcon,
  TerminalSquareIcon,
  CheckIcon,
  ChevronDownIcon,
  ChevronRightIcon,
  CopyIcon,
  FileIcon,
  FilePenLineIcon,
  FileTextIcon,
  FolderIcon,
  GitMergeIcon,
  GlobeIcon,
  ImageIcon,
  KeyRoundIcon,
  ListTodoIcon,
  MailIcon,
  PencilIcon,
  RotateCcwIcon,
  RotateCwIcon,
  SendIcon,
  ShieldAlertIcon,
  StickyNoteIcon,
  TriangleAlertIcon,
  XIcon,
} from "lucide-react";
import type { ActivityPhase, AgentActivity, ChildToolDecision } from "$lib/node";
import type { CommandAt } from "$lib/terminalStream";
import { type HistoryItem, type MoveInHistory, moveInHistory } from "$lib/api.generated";
import { actorLabel, type Attached, formatUsage, startsTurn, stripAnsi, tokenCount, type TranscriptItem } from "$lib/transcript";
import { countChanges, diffLines } from "$lib/diff";
import type { AgentPanelActions, AgentPanelSnapshot, AgentPanelStore, Outcome } from "./contract";
import {
  activityView,
  boundedHistory,
  changeFor,
  DEFAULT_HISTORY_WINDOW,
  elapsedText,
  liveTokensText,
  pendingTurns,
  type QueuedItem as DurableQueuedItem,
  thoughtFor,
} from "./panelState";
import { firstChangedLine } from "./shell/codeModel";
import { Composer } from "./Composer";
import { AskRecord, QuestionCard } from "./Questions";
import { registerAttachTarget } from "./attachTargets";
import { bakeryCodePlugin } from "./highlight";
import { VISUAL_RENDERERS } from "./VisualBlocks";
import {
  acpStatus,
  AcpToolRow,
  CodeSection,
  DiffView,
  type FileLinks,
  FileLinksContext,
  NativeToolRow,
  ShowTerminalsContext,
  SplitDiff,
  toneForKind,
} from "./ToolRows";

/** How every reply renders its Markdown. Module constants, because
 *  `MessageResponse` re-renders only when its text changes: fences are
 *  coloured by the panel's own highlighter in the Appearance syntax
 *  colours, a fence offers Copy (not a download), tables stay plain, and
 *  the visual fences (./VisualBlocks) draw instead of listing their source. */
const REPLY_PLUGINS = { code: bakeryCodePlugin, renderers: VISUAL_RENDERERS };
const REPLY_CONTROLS = { code: { copy: true, download: false }, table: false } as const;

/** `streaming`: the live row, still being written. Streamdown then hands a
 *  fence that is not closed yet to its visual as incomplete, which shows
 *  the source as it grows and draws only once the fence closes. */
function Reply({ text, streaming = false }: { text: string; streaming?: boolean }) {
  return (
    <MessageResponse
      className="bakery-ai-prose"
      controls={REPLY_CONTROLS}
      isAnimating={streaming}
      lineNumbers={false}
      plugins={REPLY_PLUGINS}
    >
      {text}
    </MessageResponse>
  );
}

type Proposal = Extract<TranscriptItem, { kind: "proposal" }>;

/** Every part of a row that can change AFTER it first appears must be in
 *  here, or the memo below will happily keep rendering a stale row.
 *
 *  Three things change under a stable hash. A proposal gains a verdict. An
 *  ask gains an answer. And a reply gains its usage: `agent.turn.usage` is a
 *  separate Heart event that `toTranscript` folds into the matching `done`
 *  item by `turn_id`, so a reply rendered before its usage arrives keeps the
 *  same hash and would otherwise never show the figures until a remount.
 *  `formatUsage` is used deliberately — it compares exactly the string the
 *  row renders, so the two can never drift apart. */
function rowSignature(it: TranscriptItem): string {
  if (it.kind === "proposal") return `${it.hash}|${it.verdict}|${it.lastFailure ?? ""}`;
  // A session that couldn't resume gains the operator's answer, or a newer session.
  if (it.kind === "not-resumed") return `${it.hash}|${it.chosen ?? ""}|${it.superseded ? 1 : 0}`;
  if (it.kind === "ask") return `${it.hash}|${it.answer ?? " "}|${it.cancelled ? 1 : 0}`;
  if (it.kind === "done") return `${it.hash}|${it.usage ? formatUsage(it.usage) : ""}|${it.segmented ? 1 : 0}`;
  // A native tool row grows as its updates land; a gate gains its decision;
  // a subagent grows as its own steps and words land.
  if (it.kind === "acp_tool") {
    const own = `${it.hash}|${it.updates.length}|${it.status ?? ""}|${it.fullRefs.length}|${it.unfinished ? 1 : 0}|${it.stopped ?? ""}`;
    return it.children ? `${own}|${it.children.map(rowSignature).join(",")}` : own;
  }
  // A reply rejoins its pieces once a subagent's steps move under it.
  if (it.kind === "segment") return `${it.hash}|${it.text.length}`;
  if (it.kind === "gate")
    return `${it.hash}|${it.decision ?? ""}|${it.cancelled ? 1 : 0}|${it.reason ?? ""}|${it.expired ? 1 : 0}`;
  // A folder request or a file ask gains its answer; a host ask, its hosts' answers.
  if (it.kind === "access" || it.kind === "file-ask") return `${it.hash}|${it.allowed ?? ""}`;
  if (it.kind === "hosts") return `${it.hash}|${it.asks.map((a) => `${a.host}:${a.allowed ?? ""}`).join(",")}`;
  // A sent message is taken up, a note read or replaced; a ready-to-merge
  // card merges or is asked again.
  if (it.kind === "message") return `${it.hash}|${it.takenUp ? 1 : 0}|${it.read ? 1 : 0}|${it.replaced ? 1 : 0}`;
  if (it.kind === "merge-waiting") return `${it.hash}|${it.merged?.commit ?? ""}|${it.replaced ? 1 : 0}`;
  // A wait gains tries, then ends.
  if (it.kind === "retrying") return `${it.hash}|${it.attempt}|${it.waiting ? 1 : 0}|${it.reason}`;
  // A native compaction gains its size, or its turn ends without one.
  if (it.kind === "compaction") return `${it.hash}|${it.tokens ?? ""}|${it.pending ? 1 : 0}`;
  return it.hash;
}

const clock = (ts: string) => ts.slice(11, 16);

/** The operator's own clock, e.g. "5:08 PM". */
const localTime = (ts: string) => {
  const d = new Date(ts);
  return Number.isNaN(d.getTime()) ? clock(ts) : d.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
};

// ---- rows -----------------------------------------------------------------

const HumanRow = memo(function HumanRow({ item, actions }: { item: Extract<TranscriptItem, { kind: "human" }>; actions: AgentPanelActions }) {
  const names = useContext(ThreadNamesContext);
  // Sent by the node itself or by another agent: a muted line, not your box.
  if (item.by) {
    const from = item.by.startsWith("agent:") ? `Message from ${actorLabel(item.by, names)}: ` : "";
    return (
      <div className="bakery-peer" data-testid="peer-message" title={localTime(item.at)}>
        <span className="min-w-0 whitespace-pre-wrap break-words" data-testid="message-body">
          {from}
          {item.text}
        </span>
      </div>
    );
  }
  return (
    <Message className="ml-0 max-w-full flex-col items-stretch gap-1" from="user">
      <time className="bakery-human-time" dateTime={item.at}>
        {localTime(item.at)}
      </time>
      <div className="bakery-human">
        {/* Deliberately not Markdown-rendered: this is what the human typed,
            and showing it back transformed would misquote them. */}
        <div className="whitespace-pre-wrap break-words" data-testid="message-body">
          {item.text}
        </div>
        {item.steer && (
          <div className="bakery-human-file" data-testid="steer-tag">
            steered into the running turn
          </div>
        )}
        {item.filePath && (
          <div className="bakery-human-file">
            <FileIcon className="size-3 shrink-0" />
            <span className="truncate font-mono">{item.filePath}</span>
          </div>
        )}
        <SentFiles actions={actions} files={item.attachments} />
      </div>
    </Message>
  );
});

const AgentRow = memo(function AgentRow({ item }: { item: Extract<TranscriptItem, { kind: "done" }> }) {
  const replyActions = useContext(ReplyActionsContext);
  const [copied, setCopied] = useState(false);
  const copy = useCallback(() => {
    void navigator.clipboard?.writeText(item.summary).then(
      () => {
        setCopied(true);
        setTimeout(() => setCopied(false), 1200);
      },
      () => {},
    );
  }, [item.summary]);

  return (
    <Message className="max-w-full" from="assistant">
      {/* A segmented turn's words are already above, between its tool calls;
          this row keeps only the reply's copy/time/usage footer. */}
      {!item.segmented && (
        <MessageContent className="w-full">
          {/* The hook goes on a wrapper, not on `MessageResponse`: that renders
              through Streamdown, which does not forward unknown props to its
              root, so a `data-testid` there never reaches the DOM. */}
          <div data-testid="message-body">
            <Reply text={item.summary} />
          </div>
        </MessageContent>
      )}
      <MessageActions className="-mt-1 opacity-0 transition-opacity focus-within:opacity-100 group-hover:opacity-100">
        <MessageAction label="Copy reply" onClick={copy} size="icon-xs" tooltip={copied ? "Copied" : "Copy"}>
          {copied ? <CheckIcon className="size-3.5" /> : <CopyIcon className="size-3.5" />}
        </MessageAction>
        {replyActions?.(item.hash)}
        <span className="text-muted-foreground text-xs">{localTime(item.at)}</span>
        {item.usage && (
          <span className="text-muted-foreground text-xs" data-testid="usage-line">
            {formatUsage(item.usage)}
          </span>
        )}
      </MessageActions>
    </Message>
  );
});

/** Native assistant text written before, between, or after tool calls —
 *  shown where it was written. Same body hook as a reply. */
const SegmentRow = memo(function SegmentRow({ item }: { item: Extract<TranscriptItem, { kind: "segment" }> }) {
  return (
    <Message className="max-w-full" from="assistant">
      <MessageContent className="w-full">
        <div data-testid="message-body">
          <Reply text={item.text} />
        </div>
      </MessageContent>
    </Message>
  );
});

type Gate = Extract<TranscriptItem, { kind: "gate" }>;

// ---- decisions ------------------------------------------------------------
//
// Both rows that wait on the operator — a permission request and a file
// change — are AI Elements `Confirmation`s, drawn in the callout look. The
// decision is made right here, with the diff in view; Review is one click
// away and never opens by itself.

/** AI Elements' `ConfirmationAction` at the transcript's density (upstream
 *  is 32px tall). Primary action last, as upstream orders them. */
function DecideButton(props: Omit<ComponentProps<typeof ConfirmationAction>, "className" | "size">) {
  return <ConfirmationAction className="h-7 px-2.5" size="xs" {...props} />;
}

/** Guards one decision against a double click: a ref, because two clicks in
 *  the same tick both see the pre-update state. */
function useDecision<A extends unknown[]>(run: (...args: A) => Promise<Outcome<void>>) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const inFlight = useRef(false);
  const decide = useCallback(
    async (...args: A) => {
      if (inFlight.current) return;
      inFlight.current = true;
      setBusy(true);
      setError("");
      const out = await run(...args);
      // `stale`: the panel rebound mid-request — nothing to report.
      if (out.status === "error") setError(out.error);
      setBusy(false);
      inFlight.current = false;
    },
    [run],
  );
  return { busy, error, decide };
}

/** A model's name by its id, for a runtime that thought without sharing it. */
const ModelLabelsContext = createContext<(modelId: string) => string>((id) => id || "This runtime");

/** One stretch of thinking: how long, and its text on a click (W1-T). */
const ThoughtRow = memo(function ThoughtRow({ item }: { item: Extract<TranscriptItem, { kind: "thought" }> }) {
  const label = useContext(ModelLabelsContext);
  return (
    <Collapsible className="bakery-activity" data-testid="thought">
      <CollapsibleTrigger className="bakery-activity-line">
        <BrainIcon className="size-3.5 shrink-0 text-(--fg-dim)" />
        <span className="bakery-activity-title">Thought for {thoughtFor(item.ms)}</span>
      </CollapsibleTrigger>
      <CollapsibleContent>
        {item.shared ? (
          <div className="space-y-1 px-2 pb-1.5 text-xs">
            <div className="whitespace-pre-wrap break-words text-(--fg-2)" data-testid="thought-text">
              {item.text}
            </div>
            {item.cut && <div className="text-(--fg-faint) text-[11px]">History kept the first 20,000 characters</div>}
          </div>
        ) : (
          <div className="px-2 pb-1.5 text-(--fg-dim) text-xs">{label(item.modelId)} doesn't share its thinking</div>
        )}
      </CollapsibleContent>
    </Collapsible>
  );
});

/** What the shell adds beside a reply's Copy (Save, Round 10), by the reply's
 *  agent.turn.done; nothing outside the shell. */
const ReplyActionsContext = createContext<((hash: string) => ReactNode) | null>(null);

/** Commands typed for you to run (ST-3): "Typed `npm run dev` and `npm run
 *  server` in 2 terminals: press Enter in each to run". */
const PreparedRow = memo(function PreparedRow({ item }: { item: Extract<TranscriptItem, { kind: "prepared" }> }) {
  const show = useContext(ShowTerminalsContext);
  const names = useContext(ThreadNamesContext);
  const n = item.commands.length;
  // From another thread (agent:<thread>:<seat>), or by the operator, it says who.
  const who = item.by === "human" ? "You typed" : item.by.split(":").length > 2 ? `${actorLabel(item.by, names)} typed` : "Typed";
  return (
    <div className="bakery-notice" data-callout="quiet" data-testid="prepared">
      <TerminalSquareIcon className="size-3.5 shrink-0 text-(--ok)" />
      <span className="min-w-0 flex-1">
        {who}{" "}
        {item.commands.map((c, i) => (
          <span key={c.terminalId}>
            {i > 0 && (i === n - 1 ? " and " : ", ")}
            <code className="font-mono text-(--fg)">{c.command}</code>
          </span>
        ))}{" "}
        in {n === 1 ? "a terminal: press Enter to run" : `${n} terminals: press Enter in each to run`}
      </span>
      {show && n > 0 && (
        <button
          className="shrink-0 rounded px-1.5 text-(--link) text-xs hover:bg-(--bg-chip)"
          onClick={() => show(item.commands.map((c) => c.terminalId))}
          type="button"
        >
          {n === 1 ? "Show terminal" : "Show terminals"}
        </button>
      )}
    </div>
  );
});

/** Seconds since a moment, counting while shown. */
function Since({ since }: { since: string }) {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const t = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(t);
  }, []);
  return <>{elapsedText(since, now)}</>;
}

/** An edit's file as its link opens it: the path, the first line the edit
 *  changed, and the text it left (null when the change shows no text). */
export type FileChange = { path: string; line: number; after: string | null };

/** A thread's name by its stream id, for an actor that names the thread it
 *  acted from (`agent:<stream id>:<seat>`); the shell provides it. */
export const ThreadNamesContext = createContext<(streamId: string) => string | undefined>(() => undefined);

/** Each kind of native action, in the operator's words for "Always allow". */
export const KIND_WORDS: Record<string, string> = {
  read: "Read",
  search: "Search",
  think: "Think",
  fetch: "Fetch",
  edit: "Edit",
  delete: "Delete",
  move: "Move",
  execute: "Run commands",
  switch_mode: "Switch its own mode",
  other: "Other",
};

/** Where an "Always allow" holds, as the card says it: the project by name,
 *  the Baker, or this chat; null from a node that still allows everywhere. */
function alwaysWhere(always: Gate["always"]): string | null {
  if (!always) return null;
  if (always.scope === "baker") return "the Baker";
  if (always.scope === "chat") return "this chat";
  return always.name || "this project";
}

/** Allow once / Deny / Always allow for one paused native tool call. */
function GateActions({
  hash,
  toolKind,
  always,
  actions,
  oneCall,
  grant,
}: {
  hash: string;
  toolKind: string;
  always: Gate["always"];
  actions: AgentPanelActions;
  /** A Baker tool call: it can only be allowed once. */
  oneCall: boolean;
  /** A push of its own branch: the operator may let it push that branch
   *  from now on (never from the phone, until the operator says). */
  grant: Gate["grant"];
}) {
  const run = useCallback((decision: ChildToolDecision) => actions.decideChildTool(hash, decision), [actions, hash]);
  const { busy, error, decide } = useDecision(run);
  const words = KIND_WORDS[toolKind] ?? toolKind;
  const where = alwaysWhere(always);
  return (
    <>
      <ConfirmationActions className="flex-wrap">
        {!oneCall && (
          <DecideButton
            disabled={busy}
            onClick={() => void decide("allow_always")}
            title={
              where
                ? `Every future "${words}" call in ${where} runs without asking`
                : `Every future "${toolKind}" call, in every thread, runs without asking`
            }
            variant="ghost"
          >
            {where ? `Always allow · ${words} · ${where}` : `Always allow “${toolKind}”`}
          </DecideButton>
        )}
        <DecideButton data-testid="gate-deny" disabled={busy} onClick={() => void decide("deny")} variant="outline">
          Deny
        </DecideButton>
        {grant && !isTauri() && (
          <DecideButton
            data-testid="gate-grant"
            disabled={busy}
            onClick={() => void decide("allow_always")}
            title={
              grant.kind === "google"
                ? `This change, and every later change to ${grant.repository} in Google Cloud, never a delete; take it back from the thread's header`
                : `This push, and every later push of ${grant.branch}, never forced; take it back from the thread's header`
            }
            variant="outline"
          >
            {grant.kind === "google"
              ? `Approve, and let this conversation change ${grant.repository} in Google Cloud from now on, never deleting`
              : `Approve, and let this conversation push ${grant.branch} from now on`}
          </DecideButton>
        )}
        <DecideButton data-testid="gate-allow" disabled={busy} onClick={() => void decide("allow_once")}>
          Allow once
        </DecideButton>
      </ConfirmationActions>
      {error && <div className="text-destructive text-xs">{error}</div>}
    </>
  );
}

const GATE_OUTCOME: Record<NonNullable<Gate["decision"]>, string> = {
  allow_once: "allowed once",
  allow_always: "allowed always, everywhere",
  deny: "denied",
};

/** A decided card's outcome; an "Always allow" says where it holds, and a
 *  push grant what it lets the conversation do. */
function gateOutcome(item: Gate, decision: NonNullable<Gate["decision"]>): string {
  if (decision === "allow_always" && item.grant) {
    return item.grant.kind === "google"
      ? `allowed, and this conversation may change ${item.grant.repository} in Google Cloud from now on, never deleting`
      : `allowed, and this conversation may push ${item.grant.branch} from now on`;
  }
  const where = alwaysWhere(item.always);
  return decision === "allow_always" && where ? `allowed always in ${where}` : GATE_OUTCOME[decision];
}

/** The reason a card closes when a push grant let its call go ahead. */
const BY_GRANT = "a grant let this call go ahead";

/** What a push sends, exactly: the head an approval sends, its commits
 *  (newest first) and its diff against what the remote has. A diff showing
 *  a secret file counts for the screenshot guard while it's open. */
function PushPreview({ push }: { push: NonNullable<Gate["push"]> }) {
  const [open, setOpen] = useState(false);
  useSecretShown(open && push.secret);
  const files = useMemo(() => parseUnifiedDiff(push.diff), [push.diff]);
  const add = files.reduce((n, f) => n + f.add, 0);
  const del = files.reduce((n, f) => n + f.del, 0);
  const n = push.commits.length;
  return (
    <div className="flex flex-col gap-1.5 text-xs" data-testid="push-preview">
      <div className="text-foreground">
        {n} {n === 1 ? "commit" : "commits"} to <span className="font-mono font-semibold">{push.branch}</span>
        <span className="text-muted-foreground">
          {" "}
          · {push.remoteHead ? `on top of ${push.remoteHead.slice(0, 7)}` : `new on the remote, from ${push.base}`} · sends {push.head.slice(0, 7)}
        </span>
      </div>
      {n > 0 && (
        <div className="max-h-28 overflow-auto rounded-md border border-border/60">
          {push.commits.map((c) => (
            <div className="flex min-w-0 gap-2 px-2 py-0.5" key={c.sha}>
              <span className="shrink-0 font-mono text-muted-foreground">{c.sha.slice(0, 7)}</span>
              <span className="min-w-0 truncate text-foreground">{c.subject}</span>
            </div>
          ))}
        </div>
      )}
      {files.length > 0 && (
        <details onToggle={(e) => setOpen(e.currentTarget.open)}>
          <summary className="cursor-pointer text-muted-foreground">
            {files.length} {files.length === 1 ? "file" : "files"} · <span className="text-(--diff-add-fg)">+{add}</span>{" "}
            <span className="text-(--diff-del-fg)">−{del}</span>
            {push.diffTruncated ? " · cut short" : ""}
          </summary>
          {open && (
            <div className="mt-1.5 flex flex-col gap-1.5">
              {files.flatMap((f) =>
                f.hunks.map((h, i) => (
                  <DiffView
                    key={`${f.path}:${i}`}
                    newText={h.lines
                      .filter((l) => l.tag !== "-")
                      .map((l) => l.text)
                      .join("\n")}
                    oldText={h.lines
                      .filter((l) => l.tag !== "+")
                      .map((l) => l.text)
                      .join("\n")}
                    path={i === 0 ? f.path : undefined}
                    testId="push-diff"
                  />
                )),
              )}
            </div>
          )}
        </details>
      )}
    </div>
  );
}

const GateRow = memo(function GateRow({ item, actions }: { item: Gate; actions: AgentPanelActions }) {
  const open = !item.decision && !item.cancelled && !item.reason && !item.expired;
  const byGrant = item.cancelled === true && item.cancelReason === BY_GRANT;
  const allowed = item.decision === "allow_once" || item.decision === "allow_always" || byGrant;
  const names = useContext(ThreadNamesContext);
  const by = item.decidedBy ? ` by ${actorLabel(item.decidedBy, names)}` : "";
  return (
    <Confirmation
      approval={open ? { id: item.hash } : { id: item.hash, approved: allowed }}
      className="bakery-callout"
      data-callout={open ? "warn" : allowed ? "ok" : "quiet"}
      data-testid="gate"
      role={open ? "alert" : "group"}
      state={open ? "approval-requested" : "approval-responded"}
    >
      <div className="bakery-callout-head">
        <ShieldAlertIcon className="bakery-callout-icon size-3.5" />
        <ConfirmationTitle className="bakery-callout-title">{open ? "Approval requested" : "Approval"}</ConfirmationTitle>
        <span className="bakery-chip" data-tone={toneForKind(item.toolKind)}>
          {item.bakeryTool?.risk || item.toolKind}
        </span>
        {item.plug && <span>{item.plug}</span>}
      </div>
      <div className="whitespace-pre-wrap break-words font-mono text-foreground text-xs" data-testid="gate-title">
        {item.title || "(no description)"}
      </div>
      {item.diffs.map((d) => (
        <DiffView key={d.path} newText={d.newText} oldText={d.oldText} path={d.path} testId="gate-diff" />
      ))}
      {item.push && <PushPreview push={item.push} />}
      {item.google && (
        <div className="flex min-w-0 flex-col gap-0.5 text-xs" data-testid="gate-google">
          <span className="min-w-0 break-all font-mono text-foreground">
            <b>{item.google.method}</b> {item.google.url}
          </span>
          <span className="text-muted-foreground">sends {item.google.sends || "nothing"}</span>
        </div>
      )}
      {open && (
        <GateActions
          actions={actions}
          always={item.always}
          grant={item.grant}
          hash={item.hash}
          oneCall={!!item.bakeryTool}
          toolKind={item.toolKind}
        />
      )}
      <ConfirmationAccepted>
        <div className="bakery-callout-outcome" data-testid="gate-outcome">
          {byGrant ? "went ahead: this conversation may push its branch" : item.decision ? gateOutcome(item, item.decision) : "allowed"}
          {byGrant ? "" : by}
        </div>
      </ConfirmationAccepted>
      <ConfirmationRejected>
        <div className="bakery-callout-outcome" data-testid="gate-outcome">
          {item.cancelled
            ? // Stop's own reason, from before it said so, reads as it is now.
              item.cancelReason && item.cancelReason !== "stopped by operator"
              ? item.cancelReason
              : "stopped before it ran"
            : item.expired
              ? "expired — its turn ended first, so nothing was allowed"
              : item.decision
                ? `${gateOutcome(item, item.decision)}${by}`
                : `refused: ${item.reason}`}
        </div>
      </ConfirmationRejected>
    </Confirmation>
  );
});

const ACCESS_LEVEL: Record<string, string> = {
  read: "read",
  write: "read + write",
  exec: "read + run",
  full: "read + write + run",
};

/** The Baker asking for a folder outside its workspace. Allow adds it to
 *  Settings → Machine access (removable there) and the conversation carries
 *  on with the folder reachable. It stays answerable after its turn ends —
 *  the Baker ends the turn to wait for it. */
const AccessRow = memo(function AccessRow({
  item,
  actions,
}: {
  item: Extract<TranscriptItem, { kind: "access" }>;
  actions: AgentPanelActions;
}) {
  const open = item.allowed === undefined;
  const run = useCallback((allow: boolean) => actions.decideAccessRequest(item.hash, allow), [actions, item.hash]);
  const { busy, error, decide } = useDecision(run);
  const names = useContext(ThreadNamesContext);
  const by = item.decidedBy ? ` by ${actorLabel(item.decidedBy, names)}` : "";
  return (
    <Confirmation
      approval={open ? { id: item.hash } : { id: item.hash, approved: item.allowed }}
      className="bakery-callout"
      data-callout={open ? "warn" : item.allowed ? "ok" : "quiet"}
      data-testid="access-request"
      role={open ? "alert" : "group"}
      state={open ? "approval-requested" : "approval-responded"}
    >
      <div className="bakery-callout-head">
        <FolderIcon className="bakery-callout-icon size-3.5" />
        <ConfirmationTitle className="bakery-callout-title">
          {open ? "Folder access requested" : "Folder access"}
        </ConfirmationTitle>
        <span className="bakery-chip">{ACCESS_LEVEL[item.access] ?? item.access}</span>
      </div>
      <div className="break-all font-mono text-foreground text-xs" data-testid="access-path">
        {item.path}
      </div>
      {item.reason && <div className="text-foreground">{item.reason}</div>}
      {open && (
        <>
          <ConfirmationActions className="flex-wrap">
            <DecideButton data-testid="access-deny" disabled={busy} onClick={() => void decide(false)} variant="outline">
              Deny
            </DecideButton>
            <DecideButton
              data-testid="access-allow"
              disabled={busy}
              onClick={() => void decide(true)}
              title="Added to Settings → Machine access — remove it there any time"
            >
              Allow
            </DecideButton>
          </ConfirmationActions>
          {error && <div className="text-destructive text-xs">{error}</div>}
        </>
      )}
      <ConfirmationAccepted>
        <div className="bakery-callout-outcome" data-testid="access-outcome">
          allowed{by} — listed in Settings → Machine access
        </div>
      </ConfirmationAccepted>
      <ConfirmationRejected>
        <div className="bakery-callout-outcome" data-testid="access-outcome">
          denied{by}
        </div>
      </ConfirmationRejected>
    </Confirmation>
  );
});

const SHARED_CHIP = { "--tone": "var(--bad)" } as CSSProperties;

/** Hosts the thread's sandbox tried that its project's list doesn't hold:
 *  the asks that arrive together on one card, each answered by the
 *  operator for the whole project. A shared domain says what allowing it
 *  means; one `*.domain` can answer several at once. */
const HostsRow = memo(function HostsRow({
  item,
  actions,
}: {
  item: Extract<TranscriptItem, { kind: "hosts" }>;
  actions: AgentPanelActions;
}) {
  const run = useCallback(
    async (hosts: string[], allow: boolean): Promise<Outcome<void>> => {
      for (const host of hosts) {
        const out = await actions.decideHost(item.projectId, host, allow);
        if (out.status !== "ok") return out;
      }
      return { status: "ok", value: undefined };
    },
    [actions, item.projectId],
  );
  const { busy, error, decide } = useDecision(run);
  const open = item.asks.filter((a) => a.allowed === undefined).map((a) => a.host);
  const pattern = open.length > 1 ? patternFor(open) : null;
  const programs = [...new Set(item.asks.flatMap((a) => (a.program ? [a.program] : [])))];
  const shared = open.some(sharedTenant) || (pattern !== null && sharedTenant(pattern.pattern));
  const one = item.asks.length === 1 ? item.asks[0] : null;
  const outcome = (allowed: boolean | undefined) =>
    allowed === undefined ? null : (
      <span className="bakery-callout-outcome">{allowed ? "allowed for this project" : "denied"}</span>
    );
  return (
    <div
      className="bakery-callout"
      data-callout={open.length > 0 ? "warn" : item.asks.every((a) => a.allowed) ? "ok" : "quiet"}
      data-testid="host-ask"
      role={open.length > 0 ? "alert" : "group"}
    >
      <div className="bakery-callout-head">
        <GlobeIcon className="bakery-callout-icon size-3.5" />
        <span>{open.length > 0 ? "Wants to reach" : "Asked to reach"}</span>
        <span className="bakery-callout-title font-mono">{one ? one.host : `${item.asks.length} hosts`}</span>
        {one && sharedTenant(one.host) && (
          <span className="bakery-chip" style={SHARED_CHIP}>
            SHARED
          </span>
        )}
        {programs.length > 0 && <span>from {programs.join(", ")}</span>}
      </div>
      {!one && (
        <div className="flex flex-col gap-1" data-testid="host-ask-hosts">
          {item.asks.map((a) => (
            <div className="flex min-h-7 min-w-0 flex-wrap items-center gap-2" key={a.hash}>
              <span className="min-w-0 truncate font-mono text-foreground text-xs">{a.host}</span>
              {sharedTenant(a.host) && (
                <span className="bakery-chip" style={SHARED_CHIP}>
                  SHARED
                </span>
              )}
              <span className="flex-1" />
              {a.allowed === undefined ? (
                <>
                  <DecideButton disabled={busy} onClick={() => void decide([a.host], false)} variant="outline">
                    Deny
                  </DecideButton>
                  <DecideButton disabled={busy} onClick={() => void decide([a.host], true)} variant="outline">
                    Allow
                  </DecideButton>
                </>
              ) : (
                outcome(a.allowed)
              )}
            </div>
          ))}
        </div>
      )}
      {pattern && (
        <div className="flex flex-wrap items-center gap-2 text-xs">
          <span className="text-muted-foreground">or</span>
          <DecideButton data-testid="host-ask-pattern" disabled={busy} onClick={() => void decide([pattern.pattern], true)} variant="outline">
            Allow {pattern.pattern}
          </DecideButton>
          <span className="text-muted-foreground">covers {pattern.covers.length} of them, and any other host under it</span>
        </div>
      )}
      {shared && (
        <div className="flex items-start gap-1.5 text-xs" data-testid="host-ask-shared" style={{ color: "var(--bad)" }}>
          <TriangleAlertIcon className="mt-px size-3.5 shrink-0" />
          Anyone can keep a bucket or a site there: an agent could send data to a stranger's.
        </div>
      )}
      {open.length > 0 && (one || open.length > 1) && (
        <div className="flex flex-wrap items-center justify-end gap-2">
          <DecideButton data-testid="host-ask-deny" disabled={busy} onClick={() => void decide(open, false)} variant="outline">
            {one ? "Deny" : "Deny all"}
          </DecideButton>
          <DecideButton data-testid="host-ask-allow" disabled={busy} onClick={() => void decide(open, true)}>
            {one ? "Allow for this project" : `Allow all ${open.length} for this project`}
          </DecideButton>
        </div>
      )}
      {one && outcome(one.allowed)}
      {error && <div className="text-destructive text-xs">{error}</div>}
    </div>
  );
});

const MESSAGE_BAR = { boxShadow: "inset 3px 0 0 var(--callout)" } as CSSProperties;

/** Where a message or a note stands, and whether that is done: a message
 *  sent here is taken up by a turn there; a note, either way, is read by
 *  the receiver's next turn. A message in started its turn: nothing to say. */
function messageStatus(item: Extract<TranscriptItem, { kind: "message" }>, name: string): { text: string; done: boolean } | null {
  if (item.dir === "in") {
    if (!item.note) return null;
    if (item.read) return { text: "read", done: true };
    return { text: item.replaced ? "replaced by a newer note" : "unread · the next turn here reads it", done: false };
  }
  if (item.takenUp) return { text: item.note ? "read" : "taken up", done: true };
  return { text: item.note ? `left · ${name}'s next turn reads it` : `delivered · ${name}'s next turn takes it up`, done: false };
}

/** A message between conversations: in from another, in its own words; out
 *  to another, with whether a turn there took it up yet. A note, which
 *  started no turn, is quieter and says whether a turn read it. Either opens
 *  the other conversation. */
const MessageRow = memo(function MessageRow({ item }: { item: Extract<TranscriptItem, { kind: "message" }> }) {
  const names = useContext(ThreadNamesContext);
  const name = names(item.peer) ?? item.peerName;
  const inbound = item.dir === "in";
  const status = messageStatus(item, name);
  const testId = `${item.note ? "note" : "message"}-${inbound ? "in" : "out"}`;
  return (
    <div className="bakery-callout" data-callout={inbound && !item.note ? "ask" : "quiet"} data-testid={testId} style={MESSAGE_BAR}>
      <div className="bakery-callout-head">
        {item.note ? (
          <StickyNoteIcon className="bakery-callout-icon size-3.5" />
        ) : inbound ? (
          <MailIcon className="bakery-callout-icon size-3.5" />
        ) : (
          <SendIcon className="bakery-callout-icon size-3.5" />
        )}
        <span>{item.note ? (inbound ? "note from" : "note to") : inbound ? "from" : "sent to"}</span>
        <span className="bakery-callout-title">{name}</span>
        <span>· {localTime(item.at)}</span>
        {status && (
          <span className="ml-auto flex items-center gap-1" data-testid="message-status">
            {status.done && <CheckIcon className="size-3" style={{ color: "var(--ok)" }} />}
            {status.text}
          </span>
        )}
      </div>
      {item.text && <Reply text={item.text} />}
      <div className="flex justify-end">
        <DecideButton onClick={() => openConversation(item.peer)} variant="outline">
          Open {name} ›
        </DecideButton>
      </div>
    </div>
  );
});

/** The brief changing, as one line: who did what to it. */
const BriefNotice = memo(function BriefNotice({ item }: { item: Extract<TranscriptItem, { kind: "brief" }> }) {
  const names = useContext(ThreadNamesContext);
  return <NoticeRow badge="brief" text={`${actorLabel(item.by, names)} ${item.what} the brief`} tone="quiet" />;
});

const openConversation = (streamId: string) => window.dispatchEvent(new CustomEvent("bakery:open-conversation", { detail: { streamId } }));
const reviewThread = (streamId: string) => window.dispatchEvent(new CustomEvent("bakery:review-thread", { detail: { streamId } }));
const LABEL = "font-bold text-[10.5px] tracking-[.08em]";

/** A chef's work ready to merge, on its thread and its spawner's: what it
 *  merges and its commits and files; Review opens it in the Merge review.
 *  Open goes to the other conversation: the spawner from the chef's own
 *  copy, the chef from the spawner's. Its merge, or its next ask, closes it;
 *  merged, it is one line. */
const MergeWaitingRow = memo(function MergeWaitingRow({
  item,
  actions,
}: {
  item: Extract<TranscriptItem, { kind: "merge-waiting" }>;
  actions: AgentPanelActions;
}) {
  const names = useContext(ThreadNamesContext);
  const name = names(item.thread) ?? item.name;
  const open = !item.merged && !item.replaced;
  const n = item.commits.length;
  const run = useCallback(async (): Promise<Outcome<void>> => {
    if (!(await ask(`Merge ${item.branch} into ${item.base}?`))) return { status: "ok", value: undefined };
    return actions.mergeThread(item.thread);
  }, [actions, item.branch, item.base, item.thread]);
  const { busy, error, decide } = useDecision(run);
  const spawner = item.own ? item.spawner : undefined;
  const other = spawner ?? item.thread;
  const otherName = spawner === undefined ? name : (names(spawner) ?? item.spawnerName ?? "");
  if (item.merged) {
    return (
      <div className="bakery-notice" data-callout="quiet" data-testid="merge-waiting">
        <GitMergeIcon className="size-3.5 shrink-0 self-center" style={{ color: "var(--callout)" }} />
        <span className="min-w-0 break-words">
          merged into {item.merged.into} at <span className="font-mono">{item.merged.commit.slice(0, 7)}</span>
        </span>
      </div>
    );
  }
  return (
    <div className="bakery-callout" data-callout={open ? "ok" : "quiet"} data-testid="merge-waiting" style={MESSAGE_BAR}>
      <div className="bakery-callout-head">
        <GitMergeIcon className="bakery-callout-icon size-3.5" />
        <span className={LABEL} style={{ color: "var(--callout)" }}>
          READY TO MERGE
        </span>
        {open && (
          <span className="bakery-chip ml-auto" style={{ "--tone": "var(--ok)" } as CSSProperties}>
            WAITING
          </span>
        )}
      </div>
      <div className="flex min-w-0 flex-wrap items-center gap-x-1.5 gap-y-0.5 text-muted-foreground text-xs">
        <span className="font-semibold text-foreground">{name}</span>
        <span className="font-mono text-foreground">{item.branch}</span>
        into
        <span className="font-mono text-foreground">{item.base}</span>
        <span>
          · {n} {n === 1 ? "commit" : "commits"} · <span style={{ color: "var(--diff-add-fg)" }}>+{item.adds}</span>{" "}
          <span style={{ color: "var(--diff-del-fg)" }}>−{item.dels}</span> · {item.files.length} {item.files.length === 1 ? "file" : "files"}
        </span>
      </div>
      {n > 0 && (
        <div className="flex max-h-24 flex-col overflow-auto text-[11.5px]">
          {item.commits.map((c) => (
            <span className="truncate" key={c.sha}>
              <span className="font-mono text-muted-foreground">{c.sha.slice(0, 7)}</span> {c.subject}
            </span>
          ))}
        </div>
      )}
      {item.files.length > 0 && (
        <div className="flex max-h-14 flex-wrap gap-1 overflow-auto">
          {item.files.map((f) => (
            <span className="bakery-chip font-mono" key={f}>
              {f}
            </span>
          ))}
        </div>
      )}
      <div className="flex flex-wrap items-center justify-end gap-2">
        {!open && <span className="bakery-callout-outcome mr-auto">asked again since</span>}
        {open && (
          <>
            <DecideButton onClick={() => reviewThread(item.thread)}>Review</DecideButton>
            <DecideButton data-testid="merge-waiting-merge" disabled={busy} onClick={() => void decide()} variant="outline">
              Merge
            </DecideButton>
          </>
        )}
        <DecideButton onClick={() => openConversation(other)} variant="outline">
          Open {otherName} ›
        </DecideButton>
      </div>
      {error && <div className="text-destructive text-xs">{error}</div>}
    </div>
  );
});

/** This chef's work overlaps another's waiting to merge: whose, and the
 *  files both change. */
const MergeOverlapRow = memo(function MergeOverlapRow({ item }: { item: Extract<TranscriptItem, { kind: "merge-overlap" }> }) {
  const names = useContext(ThreadNamesContext);
  const name = names(item.waiting) ?? "Another thread";
  return (
    <div className="bakery-callout" data-callout="ok" data-testid="merge-overlap" style={MESSAGE_BAR}>
      <div className="bakery-callout-head">
        <GitMergeIcon className="bakery-callout-icon size-3.5" />
        <span className={LABEL} style={{ color: "var(--callout)" }}>
          ALREADY WAITING TO MERGE
        </span>
      </div>
      <div className="flex min-w-0 flex-wrap items-center gap-1.5 text-muted-foreground text-xs">
        <span className="font-semibold text-foreground">{name}</span>
        {item.files.length > 0 ? "changes the same files:" : "works on this branch"}
        {item.files.map((f) => (
          <span className="bakery-chip font-mono" key={f}>
            {f}
          </span>
        ))}
      </div>
      <div className="flex flex-wrap items-center justify-end gap-2">
        <DecideButton onClick={() => reviewThread(item.waiting)} variant="outline">
          See its diff
        </DecideButton>
        <DecideButton onClick={() => openConversation(item.waiting)} variant="outline">
          Open {name} ›
        </DecideButton>
      </div>
    </div>
  );
});

const FILE_KIND: Record<string, string> = {
  public_key: "PUBLIC KEY",
  certificate: "CERTIFICATE",
  private_key: "PRIVATE KEY",
  token_or_env: "TOKEN OR ENV",
  unknown: "UNKNOWN",
};

/** A secret-named file the thread's room was refused, with what the node
 *  says it is: Look opens it in the thread's Code; the operator's answer
 *  holds for the whole project, read only. A private key takes a second yes. */
const FileAskRow = memo(function FileAskRow({
  item,
  actions,
}: {
  item: Extract<TranscriptItem, { kind: "file-ask" }>;
  actions: AgentPanelActions;
}) {
  const links = useContext(FileLinksContext);
  const privateKey = item.what === "private_key";
  const run = useCallback(
    async (allow: boolean): Promise<Outcome<void>> => {
      if (allow && privateKey && !(await ask(`Let this project's builds read ${item.path}?\n\nThe node says it's a private key.`))) {
        return { status: "ok", value: undefined };
      }
      return actions.decideFile(item.projectId, item.path, allow, allow && privateKey);
    },
    [actions, item.projectId, item.path, privateKey],
  );
  const { busy, error, decide } = useDecision(run);
  const open = item.allowed === undefined;
  return (
    <div className="bakery-callout" data-callout={open ? "warn" : item.allowed ? "ok" : "quiet"} data-testid="file-ask" role={open ? "alert" : "group"}>
      <div className="bakery-callout-head">
        <KeyRoundIcon className="bakery-callout-icon size-3.5" />
        <span>{open ? "Wants to read" : "Asked to read"}</span>
        <span className="bakery-callout-title font-mono">{item.path}</span>
        <span className="bakery-chip" style={privateKey ? SHARED_CHIP : undefined}>
          {FILE_KIND[item.what] ?? FILE_KIND.unknown}
        </span>
        <span>{bytes(item.bytes)}</span>
        {item.program && <span>from {item.program}</span>}
      </div>
      {open && privateKey && (
        <div className="flex items-start gap-1.5 text-xs" style={{ color: "var(--bad)" }}>
          <TriangleAlertIcon className="mt-px size-3.5 shrink-0" />
          Every room of this project could read this private key.
        </div>
      )}
      <div className="flex flex-wrap items-center justify-end gap-2">
        {!open && <span className="bakery-callout-outcome mr-auto">{item.allowed ? "allowed for this project" : "denied"}</span>}
        {links && (
          <DecideButton data-testid="file-ask-look" onClick={() => links.look(item.path)} variant="outline">
            Look
          </DecideButton>
        )}
        {open && (
          <>
            <DecideButton data-testid="file-ask-deny" disabled={busy} onClick={() => void decide(false)} variant="outline">
              Deny
            </DecideButton>
            <DecideButton data-testid="file-ask-allow" disabled={busy} onClick={() => void decide(true)}>
              Allow for this project
            </DecideButton>
          </>
        )}
      </div>
      {error && <div className="text-destructive text-xs">{error}</div>}
    </div>
  );
});

/** A fact this client has no dedicated row for: its family, its summary, and
 *  — one click away — its entire payload, so nothing is hidden. */
const NoteRow = memo(function NoteRow({ item }: { item: Extract<TranscriptItem, { kind: "note" }> }) {
  return (
    <details className="text-xs" data-testid="note-row">
      <summary className="flex cursor-pointer items-baseline gap-2">
        <span className="bakery-chip shrink-0">{item.family}</span>
        {item.text && <span className="min-w-0 break-words text-muted-foreground">{item.text}</span>}
      </summary>
      <div className="mt-2">
        <CodeSection label="Payload" lang="json" text={item.raw} />
      </div>
    </details>
  );
});

/** One file change. Accept and Reject here call the same node route as
 *  Review — which checks the file has not changed underneath — so one
 *  verdict updates both. Editing before accepting needs the editor, so that
 *  is "Open in Review".
 *
 *  A change that already landed (turn snapshots) is decided after the fact:
 *  Keep writes nothing, Undo restores the file — and a file too big to keep
 *  a copy of can only be kept. A credential or denied file's content stays
 *  out of the history; the operator can ask Bakery's own copy for it. */
const ProposalRow = memo(function ProposalRow({ item, actions }: { item: Proposal; actions: AgentPanelActions }) {
  const pending = item.verdict === "pending";
  const accepted = item.verdict === "accepted";
  const landed = item.landed;
  // Open while it waits — decide on what it does, not on its name. A
  // decided one starts folded to its line.
  const [showChange, setShowChange] = useState(pending);
  const [revealed, setRevealed] = useState<{ before: string; after: string } | null>(null);
  const [revealError, setRevealError] = useState("");
  const oldText = revealed?.before ?? item.oldContent;
  const newText = revealed?.after ?? item.newContent;
  const hasText = !landed?.binary && (!landed?.hidden || revealed !== null);
  const { add, del } = useMemo(() => countChanges(diffLines(oldText, newText)), [oldText, newText]);
  const run = useCallback((accept: boolean) => actions.decideProposal(item.hash, accept), [actions, item.hash]);
  const { busy, error, decide } = useDecision(run);
  const links = useContext(FileLinksContext);
  const names = useContext(ThreadNamesContext);
  const by = item.decidedBy ? ` by ${actorLabel(item.decidedBy, names)}` : "";
  const reveal = async () => {
    setRevealError("");
    const out = await actions.fetchChange(item.hash);
    if (out.status === "ok") {
      setRevealed({ before: out.value.before ?? "", after: out.value.after ?? "" });
      setShowChange(true);
    } else if (out.status === "error") setRevealError(out.error);
  };
  return (
    <Confirmation
      approval={pending ? { id: item.hash } : { id: item.hash, approved: accepted }}
      className="bakery-callout"
      data-callout={pending ? "warn" : accepted ? "ok" : "quiet"}
      data-testid="proposal"
      role={pending ? "alert" : "group"}
      state={pending ? "approval-requested" : "approval-responded"}
    >
      <div className="bakery-callout-head">
        <FilePenLineIcon className="bakery-callout-icon size-3.5" />
        <ConfirmationTitle className="bakery-callout-title min-w-0 break-all font-mono">
          {links ? (
            <button
              className="break-all text-left text-(--link) underline decoration-(--link)/40 underline-offset-2 hover:decoration-(--link)"
              onClick={() => links.open(item.hash)}
              title={`Open ${item.filePath} in Code`}
              type="button"
            >
              {item.filePath}
            </button>
          ) : (
            item.filePath
          )}
        </ConfirmationTitle>
        {item.part && <span className="bakery-chip">{item.part === "cell" ? "cell" : "the change"}</span>}
        {hasText ? (
          <button
            aria-expanded={showChange}
            className="rounded-[var(--r-xs)] px-1 font-mono hover:bg-[var(--bg-chip)]"
            onClick={() => setShowChange((v) => !v)}
            title={showChange ? "Hide the change" : "Show the change"}
            type="button"
          >
            <span className="text-[var(--diff-add-fg)]">+{add}</span>{" "}
            <span className="text-[var(--diff-del-fg)]">−{del}</span>
          </button>
        ) : landed?.binary ? (
          <span className="bakery-chip">binary</span>
        ) : (
          <>
            <span className="bakery-chip" data-testid="proposal-hidden">
              contents hidden
            </span>
            <button
              className="rounded-[var(--r-xs)] px-1 text-(--link) text-xs hover:bg-[var(--bg-chip)]"
              data-testid="proposal-reveal"
              onClick={() => void reveal()}
              type="button"
            >
              Show
            </button>
          </>
        )}
        {landed?.operatorEdit && (
          <span className="bakery-chip" data-testid="proposal-operator-edit" data-tone="edit">
            includes your edit
          </span>
        )}
      </div>
      {item.rationale && <div className="text-foreground">{item.rationale}</div>}
      {item.lastFailure && (
        <div className="text-destructive text-xs">
          The last decision did not land: {item.lastFailure}. It is still open and can be retried.
        </div>
      )}
      {revealError && <div className="text-destructive text-xs">{revealError}</div>}
      {showChange && hasText && <SplitDiff newText={newText} oldText={oldText} testId="proposal-diff" />}
      <ConfirmationActions className="flex-wrap">
        {landed ? (
          <>
            {landed.restorable && (
              <DecideButton data-testid="proposal-reject" disabled={busy} onClick={() => void decide(false)} variant="outline">
                Undo
              </DecideButton>
            )}
            <DecideButton data-testid="proposal-accept" disabled={busy} onClick={() => void decide(true)}>
              Keep
            </DecideButton>
          </>
        ) : (
          <>
            <DecideButton onClick={() => actions.openReview(item.hash)} title="Edit it before accepting" variant="ghost">
              Open in Review
            </DecideButton>
            <DecideButton data-testid="proposal-reject" disabled={busy} onClick={() => void decide(false)} variant="outline">
              Reject
            </DecideButton>
            <DecideButton data-testid="proposal-accept" disabled={busy} onClick={() => void decide(true)}>
              Accept
            </DecideButton>
          </>
        )}
      </ConfirmationActions>
      {error && <div className="text-destructive text-xs">{error}</div>}
      <ConfirmationAccepted>
        <div className="bakery-callout-outcome">
          {landed ? "kept" : "accepted"}
          {by}
        </div>
      </ConfirmationAccepted>
      <ConfirmationRejected>
        <div className="bakery-callout-outcome">
          {item.cancelled ? "stopped before it ran" : landed ? "undone" : "rejected"}
          {by}
        </div>
      </ConfirmationRejected>
    </Confirmation>
  );
});

/** Keep all or Undo all of one turn's landed changes at once. */
function TurnReview({
  turnId,
  count,
  undoable,
  actions,
}: {
  turnId: string;
  count: number;
  undoable: boolean;
  actions: AgentPanelActions;
}) {
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState("");
  const review = async (keep: boolean) => {
    setBusy(true);
    setNote("");
    const out = await actions.reviewTurn(turnId, keep);
    if (out.status === "error") setNote(out.error);
    if (out.status === "ok" && out.value.refused.length > 0) setNote(out.value.refused.join("; "));
    setBusy(false);
  };
  return (
    <div className="flex flex-wrap items-center gap-2 text-xs" data-testid="turn-review">
      <span className="text-(--fg-dim)">{count} changes</span>
      {undoable && (
        <Button className="h-6 px-2 text-xs" disabled={busy} onClick={() => void review(false)} size="sm" variant="outline">
          Undo all
        </Button>
      )}
      <Button className="h-6 px-2 text-xs" disabled={busy} onClick={() => void review(true)} size="sm">
        Keep all
      </Button>
      {note && <span className="text-destructive">{note}</span>}
    </div>
  );
}

/** One line for the terminal states that are not a reply: the turn ended
 *  without producing one, and the human needs to see why. */
const NoticeRow = memo(function NoticeRow({
  badge,
  tone,
  text,
}: {
  badge: string;
  tone: "bad" | "warn" | "quiet";
  text: string;
}) {
  return (
    <div className="bakery-notice" data-callout={tone}>
      <span className="bakery-notice-tag">{badge}</span>
      <span className="min-w-0 break-words">{text}</span>
    </div>
  );
});

/** The summary a compacted conversation went on from: folded, above its divider. */
function SummaryFold({ text }: { text: string }) {
  return (
    <Collapsible className="text-xs">
      <CollapsibleTrigger className="flex items-center gap-1 text-muted-foreground hover:text-foreground">
        <ChevronRightIcon className="size-3" />
        Summary it went on from
      </CollapsibleTrigger>
      <CollapsibleContent className="pt-1.5">
        <Reply text={text} />
      </CollapsibleContent>
    </Collapsible>
  );
}

/** Where a conversation was compacted: how, and its size before, or that
 *  none was reported. A native one's size may still be on its way. */
function CompactedLine({ trigger, tokens, native, pending }: { trigger: string | null; tokens: number | null; native?: boolean; pending?: boolean }) {
  const size = pending ? "" : typeof tokens === "number" && tokens > 0 ? ` · ${tokenCount(tokens)} tokens` : " · tokens not reported";
  return (
    <div className="flex items-center gap-2 text-muted-foreground text-xs" data-testid="compacted">
      <span className="h-px flex-1 bg-border" />
      {native ? "Claude compacted" : "Compacted"}
      {trigger ? ` (${trigger})` : ""}
      {size}
      <span className="h-px flex-1 bg-border" />
    </div>
  );
}

/** The files a message was sent with: each by its kind, name and size. */
function SentFiles({ files, actions }: { files: readonly Attached[] | undefined; actions: AgentPanelActions }) {
  return files?.map((a) => <SentFile actions={actions} file={a} key={a.id} />);
}

/** One sent file, read back as it was sent. An image shows small and opens
 *  whole; a text file opens to read; anything else saves. */
/** Shown in the app up to these sizes; a bigger file saves instead. */
const SHOW_IMAGE_BYTES = 16 * 1024 * 1024;
const SHOW_TEXT_BYTES = 2 * 1024 * 1024;

function SentFile({ file, actions }: { file: Attached; actions: AgentPanelActions }) {
  // Only what the page draws well, and only up to a size; a HEIC, a TIFF or
  // a huge file saves (an image that fails to draw falls back to saving).
  const [broken, setBroken] = useState(false);
  const image = /^image\/(png|jpe?g|gif|webp|avif|bmp|svg\+xml)$/.test(file.mediaType) && file.bytes <= SHOW_IMAGE_BYTES && !broken;
  const text = file.mediaType.startsWith("text/") && file.bytes <= SHOW_TEXT_BYTES;
  const [url, setUrl] = useState<string | null>(null);
  const [body, setBody] = useState<string | null>(null);
  const [open, setOpen] = useState(false);
  const [error, setError] = useState("");
  const read = useCallback(async () => {
    const got = await actions.fetchAttachment(file.id);
    if (got.status === "error") {
      setError(got.error);
      return null;
    }
    return got.status === "ok" ? got.value : null;
  }, [actions, file.id]);
  // An image's thumbnail is read as soon as it shows.
  useEffect(() => {
    if (!image) return;
    let live = true;
    let made = "";
    void read().then((blob) => {
      if (!live || !blob) return;
      made = URL.createObjectURL(blob);
      setUrl(made);
    });
    return () => {
      live = false;
      if (made) URL.revokeObjectURL(made);
    };
  }, [image, read]);
  const openIt = async () => {
    if (image) return url && setOpen(true);
    const blob = await read();
    if (!blob) return;
    if (text) {
      setBody(await blob.text());
      setOpen(true);
      return;
    }
    // Saved as bare bytes: where a webview ignores `download` and opens the
    // blob instead (the phone), an HTML or SVG file must never run as a page
    // in the app's own origin.
    const href = URL.createObjectURL(new Blob([blob], { type: "application/octet-stream" }));
    const save = document.createElement("a");
    save.href = href;
    save.download = file.filename;
    save.click();
    setTimeout(() => URL.revokeObjectURL(href), 60_000);
  };
  return (
    <>
      <button
        className="bakery-human-file cursor-pointer text-left hover:underline"
        data-testid="sent-attachment"
        onClick={() => void openIt()}
        title={error || file.mediaType || undefined}
        type="button"
      >
        {image && url ? (
          <img alt="" className="size-8 shrink-0 rounded object-cover" onError={() => setBroken(true)} src={url} />
        ) : file.mediaType.startsWith("image/") ? (
          <ImageIcon className="size-3 shrink-0" />
        ) : file.mediaType.startsWith("text/") ? (
          <FileTextIcon className="size-3 shrink-0" />
        ) : (
          <FileIcon className="size-3 shrink-0" />
        )}
        <span className="truncate">{file.filename}</span>
        {file.bytes > 0 && <span className="shrink-0 opacity-70">{bytes(file.bytes)}</span>}
        {error && <span className="shrink-0 text-destructive">{error}</span>}
      </button>
      <Dialog onOpenChange={setOpen} open={open}>
        <DialogContent className="max-h-[90vh] max-w-[90vw] overflow-auto">
          <DialogTitle className="truncate font-mono text-sm">{file.filename}</DialogTitle>
          {image && url ? (
            <img alt={file.filename} className="max-h-[80vh] max-w-full object-contain" src={url} />
          ) : (
            <pre className="whitespace-pre-wrap break-words font-mono text-xs">{body}</pre>
          )}
        </DialogContent>
      </Dialog>
    </>
  );
}

/** A native session that couldn't resume. The turn stopped to ask: Retry
 *  resumes it again, Start fresh opens a new one briefed from the thread.
 *  Answered, it says which; left behind by a newer session, it says so. */
function NotResumedRow({ item, actions }: { item: Extract<TranscriptItem, { kind: "not-resumed" }>; actions: AgentPanelActions }) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const why = item.reason ? ` (${item.reason})` : "";
  if (!item.awaiting || item.chosen || item.superseded) {
    const text = !item.awaiting
      ? `The last session didn't resume${why}. This turn starts a new one, briefed from this thread.`
      : item.chosen === "retry"
        ? `The last session didn't resume${why}. You chose to retry it.`
        : item.chosen === "fresh"
          ? `The last session didn't resume${why}. You chose a fresh one, briefed from this thread.`
          : `The last session didn't resume${why}. A newer session took over.`;
    return <NoticeRow badge="session" text={text} tone="quiet" />;
  }
  const choose = (choice: "retry" | "fresh") => {
    setBusy(true);
    setError("");
    void actions.chooseSession(choice).then((out) => {
      setBusy(false);
      if (out.status === "error") setError(out.error);
    });
  };
  return (
    <div className="bakery-notice flex-wrap" data-callout="warn" data-testid="not-resumed">
      <span className="bakery-notice-tag">session</span>
      <span className="min-w-0 flex-1 break-words">The last session didn't resume{why}.</span>
      <span className="flex shrink-0 gap-1.5">
        <Button disabled={busy} onClick={() => choose("retry")} size="xs" variant="outline">
          Retry
        </Button>
        <Button disabled={busy} onClick={() => choose("fresh")} size="xs" variant="outline">
          Start fresh
        </Button>
      </span>
      {error && <span className="w-full text-destructive text-xs">{error}</span>}
    </div>
  );
}

/** A waiting message the node could not send: the words and files as they
 *  were, and why. */
function QueueFailedRow({ item, actions }: { item: Extract<TranscriptItem, { kind: "queue-failed" }>; actions: AgentPanelActions }) {
  return (
    <Message className="ml-0 max-w-full flex-col items-stretch gap-1" data-testid="queue-failed" from="user">
      <div className="bakery-human opacity-70">
        <div className="whitespace-pre-wrap break-words">{item.text}</div>
        <SentFiles actions={actions} files={item.attachments} />
      </div>
      <NoticeRow badge="not sent" text={item.reason} tone="bad" />
    </Message>
  );
}

/** More rows above, as the reader nears the top of the scroller: `more`
 *  runs once for each `key` (a new page, a wider window) while `enabled`,
 *  and calls `hold` just before its rows land, so they keep the reader
 *  where they were. Returns the marker that goes above the rows. */
function useMoreAbove(enabled: boolean, key: unknown, more: (hold: () => void) => void) {
  const { scrollRef } = useStickToBottomContext();
  const marker = useRef<HTMLDivElement>(null);
  const held = useRef<number | null>(null);
  const asked = useRef<unknown>(marker);
  const latest = useRef(more);
  latest.current = more;
  useEffect(() => {
    const root = scrollRef.current;
    const el = marker.current;
    if (!enabled || !root || !el) return;
    const observer = new IntersectionObserver(
      (seen) => {
        if (asked.current === key || !seen.some((e) => e.isIntersecting)) return;
        asked.current = key;
        latest.current(() => {
          held.current = root.scrollHeight - root.scrollTop;
        });
      },
      { root, rootMargin: "600px 0px 0px 0px" },
    );
    observer.observe(el);
    return () => observer.disconnect();
  }, [enabled, key, scrollRef]);
  useLayoutEffect(() => {
    const root = scrollRef.current;
    if (root && held.current !== null) root.scrollTop = root.scrollHeight - held.current;
    held.current = null;
  }, [key, scrollRef]);
  return marker;
}

/** The thread's own earlier events: the window widens as the reader nears
 *  the top, until every event shows. */
function EarlierEvents({ hidden, size, widen }: { hidden: number; size: number; widen: () => void }) {
  const marker = useMoreAbove(hidden > 0, size, (hold) => {
    hold();
    widen();
  });
  return <div ref={marker} />;
}

/** A moved-in thread's old conversation, as it came in from Claude Code:
 *  read only, above the thread's own events. It reads a page at a time,
 *  newest first; the next older page loads as the reader nears the top,
 *  keeping their place, until there is none. Nothing for a thread that
 *  never moved in. It shows once the thread's own events all show. */
function MovedIn({ streamId, shown }: { streamId: string; shown: boolean }) {
  const [pages, setPages] = useState<MoveInHistory[]>([]);
  const [error, setError] = useState("");
  useEffect(() => {
    let live = true;
    setPages([]);
    setError("");
    moveInHistory({ stream_id: streamId }).then(
      (h) => live && setPages([h]),
      (e) => live && setError(e instanceof Error ? e.message : String(e)),
    );
    return () => {
      live = false;
    };
  }, [streamId]);
  const earlier = pages.at(-1)?.earlier ?? null;
  const moved = !!pages[0]?.session_id;
  const top = useMoreAbove(shown && moved && earlier != null && !error, earlier, (hold) => {
    if (earlier == null) return;
    moveInHistory({ stream_id: streamId, before: earlier }).then(
      (h) => {
        hold();
        setPages((p) => [...p, h]);
      },
      (e) => setError(e instanceof Error ? e.message : String(e)),
    );
  });
  if (!shown) return null;
  if (!moved) return error ? <NoticeRow badge="moved in" text={error} tone="quiet" /> : null;
  // Pages come newest first, each newest first: the oldest reads at the top.
  const items = pages.flatMap((p) => p.items).reverse();
  return (
    <section aria-label="Moved in from Claude Code" className="flex flex-col gap-3" data-testid="moved-in">
      <div ref={top} />
      {error && <NoticeRow badge="moved in" text={error} tone="quiet" />}
      {items.map((item) => (
        <MovedInRow item={item} key={item.id} />
      ))}
      <div className="flex items-center gap-2 text-muted-foreground text-xs">
        <span className="h-px flex-1 bg-border" />
        Moved in from Claude Code{pages[0].title ? ` · ${pages[0].title}` : ""}
        <span className="h-px flex-1 bg-border" />
      </div>
    </section>
  );
}

function MovedInRow({ item }: { item: HistoryItem }) {
  const cut = item.cut ? "…" : "";
  switch (item.kind) {
    case "user":
      return (
        <Message className="ml-0 max-w-full flex-col items-stretch gap-1" from="user">
          {item.at && (
            <time className="bakery-human-time" dateTime={item.at}>
              {localTime(item.at)}
            </time>
          )}
          <div className="bakery-human">
            <div className="whitespace-pre-wrap break-words">
              {item.text}
              {cut}
            </div>
          </div>
        </Message>
      );
    case "assistant":
      return (
        <Message className="max-w-full" from="assistant">
          <MessageContent className="w-full">
            <Reply text={item.text + cut} />
          </MessageContent>
        </Message>
      );
    case "tool":
      return (
        <div className="flex min-w-0 items-center gap-2 text-muted-foreground text-xs">
          <TerminalSquareIcon className="size-3.5 shrink-0" />
          <span className="shrink-0 font-mono">{item.name}</span>
          <span className="min-w-0 truncate font-mono">{item.text}</span>
        </div>
      );
    case "summary":
      return <SummaryFold text={item.text + cut} />;
    case "compaction":
      return <CompactedLine tokens={item.tokens ?? null} trigger={item.name ?? null} />;
    default:
      return <NoticeRow badge="unreadable" text={item.text || "A line of the transcript could not be read."} tone="quiet" />;
  }
}

type AcpToolItem = Extract<TranscriptItem, { kind: "acp_tool" }>;

/** "1m 12s" / "8s" between two timestamps. */
function took(from: string, to: string): string {
  const seconds = Math.round((Date.parse(to) - Date.parse(from)) / 1000);
  if (!Number.isFinite(seconds) || seconds < 0) return "";
  return seconds >= 60 ? `${Math.floor(seconds / 60)}m ${seconds % 60}s` : `${seconds}s`;
}

/** What a finished subagent handed back to the agent that launched it. */
function handedBack(item: AcpToolItem): string {
  const texts = item.content.flatMap((block) => {
    const b = block as { type?: unknown; content?: { type?: unknown; text?: unknown } } | null;
    return b?.type === "content" && b.content?.type === "text" && typeof b.content.text === "string"
      ? [b.content.text]
      : [];
  });
  if (texts.length > 0) return texts.join("\n\n");
  return typeof item.rawOutput === "string" ? item.rawOutput : "";
}

const SUBAGENT_STATUS = {
  running: "working",
  done: "done",
  failed: "failed",
  stopped: "stopped",
  "stopped-before": "stopped before it ran",
  "stopped-after": "stopped after it ran",
} as const;

/** A subagent at work, the way Claude Code shows one: what it was asked to
 *  do, its steps and words as they land (open while it works, folded once it
 *  is done), and what it handed back — which the agent that launched it then
 *  works from in its own reply, below. */
const SubagentRow = memo(function SubagentRow({ item, actions }: { item: AcpToolItem; actions: AgentPanelActions }) {
  // As a native call stands (`acpStatus`); a subagent left open by a turn
  // that ended otherwise stopped working all the same.
  const call = acpStatus(item);
  const status: keyof typeof SUBAGENT_STATUS =
    call === "done" || call === "failed" || call === "running" || call === "stopped-before" || call === "stopped-after" ? call : "stopped";
  const children = item.children ?? [];
  const steps = children.filter((child) => child.kind === "acp_tool").length;
  const duration =
    status === "running" ? "" : took(item.updates[0]?.at ?? item.at, item.updates[item.updates.length - 1]?.at ?? item.at);
  const result = status === "done" ? handedBack(item) : "";
  const [open, setOpen] = useState(status === "running");
  const chosen = useRef(false);
  const card = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!chosen.current) setOpen(status === "running");
  }, [status]);
  // The Agent map asked to see this one.
  useEffect(() => {
    const reveal = () => {
      if (!takeReveal(item.toolCallId)) return;
      chosen.current = true;
      setOpen(true);
      requestAnimationFrame(() => card.current?.scrollIntoView({ block: "center" }));
    };
    reveal();
    return onReveal(reveal);
  }, [item.toolCallId]);
  return (
    <Task
      className="bakery-subagent"
      data-status={status}
      data-testid="subagent"
      ref={card}
      onOpenChange={(next) => {
        chosen.current = true;
        setOpen(next);
      }}
      open={open}
    >
      <TaskTrigger title={item.subagent?.description ?? "Subagent"}>
        <button className="bakery-subagent-head" type="button">
          <BotIcon className="size-3.5 shrink-0" />
          <span className="font-medium text-foreground">Agent</span>
          <span className="min-w-0 truncate" data-testid="subagent-task">
            {item.subagent?.description}
          </span>
          {item.subagent?.type && <span className="bakery-chip">{item.subagent.type}</span>}
          <span className="bakery-subagent-meta" data-testid="subagent-meta">
            {SUBAGENT_STATUS[status]} · {steps} {steps === 1 ? "step" : "steps"}
            {duration && ` · ${duration}`}
          </span>
          <ChevronDownIcon className="size-3.5 shrink-0 transition-transform group-data-[state=open]:rotate-180" />
        </button>
      </TaskTrigger>
      <TaskContent>
        {children.map((child) => (
          <TranscriptRow actions={actions} item={child} key={child.hash} />
        ))}
        {result && (
          <div data-testid="subagent-result">
            <div className="text-muted-foreground text-xs">Handed back</div>
            <Reply text={result} />
          </div>
        )}
      </TaskContent>
    </Task>
  );
});

/** A turn waiting out a passing failure (the provider busy, the Wi-Fi gone):
 *  a quiet line, never an error. While it waits the arrows turn slowly;
 *  once it's on its way again, the line stays as a record. */
function RetryRow({ item }: { item: Extract<TranscriptItem, { kind: "retrying" }> }) {
  const tries = item.waiting ? `try ${item.attempt}` : `${item.attempt} ${item.attempt === 1 ? "try" : "tries"}`;
  return (
    <div className="flex min-w-0 items-center gap-1.5 text-muted-foreground text-xs" data-testid="retrying" title={item.reason}>
      <RotateCwIcon className={`size-3 shrink-0 ${item.waiting ? "motion-safe:animate-spin [animation-duration:2.5s]" : ""}`} />
      <span className="shrink-0">{item.waiting ? "Waiting for the model, trying again" : "Waited for the model"}</span>
      <span className="shrink-0">· {tries}</span>
      {item.reason && <span className="min-w-0 truncate">· {item.reason}</span>}
    </div>
  );
}

const TranscriptRow = memo(
  function TranscriptRow({ item, actions }: { item: TranscriptItem; actions: AgentPanelActions }) {
    switch (item.kind) {
      // The pane header already names project and branch; a second strip
      // inside the log would just repeat it. Only why it forked the local
      // main, when it had to, shows.
      case "session":
        return item.forkNote ? <NoticeRow badge="forked" text={item.forkNote} tone="warn" /> : null;
      case "human":
        return <HumanRow actions={actions} item={item} />;
      case "done":
        return <AgentRow item={item} />;
      case "segment":
        return <SegmentRow item={item} />;
      case "tool":
        return <NativeToolRow item={item} />;
      case "acp_tool":
        return item.subagent ? (
          <SubagentRow actions={actions} item={item} />
        ) : (
          <AcpToolRow actions={actions} item={item} />
        );
      case "gate":
        return <GateRow actions={actions} item={item} />;
      case "access":
        return <AccessRow actions={actions} item={item} />;
      case "hosts":
        return <HostsRow actions={actions} item={item} />;
      case "file-ask":
        return <FileAskRow actions={actions} item={item} />;
      case "message":
        return <MessageRow item={item} />;
      case "merge-waiting":
        return <MergeWaitingRow actions={actions} item={item} />;
      case "merge-overlap":
        return <MergeOverlapRow item={item} />;
      case "brief":
        return <BriefNotice item={item} />;
      case "backlog-added":
        return (
          <div className="bakery-notice" data-callout="quiet" data-testid="backlog-added">
            <span className="bakery-notice-tag">backlog</span>
            <span className="min-w-0 flex-1 break-words">added “{item.title}” to the backlog</span>
            <button
              className="shrink-0 text-(--link) text-[11px] hover:underline"
              onClick={() => window.dispatchEvent(new CustomEvent("bakery:open-tool", { detail: { id: "@backlog" } }))}
              type="button"
            >
              Backlog ›
            </button>
          </div>
        );
      case "merged":
        return (
          <NoticeRow
            badge="merged"
            text={`${item.branch} into ${item.into} at ${item.commit.slice(0, 7)}${item.uncommitted ? "; changes not committed were not merged" : ""}`}
            tone="quiet"
          />
        );
      case "proposal":
        return <ProposalRow actions={actions} item={item} />;
      case "ask":
        return <AskRecord item={item} />;
      case "exhausted":
        return <NoticeRow badge="step limit" text={`${item.reason} (${item.steps} steps)`} tone="warn" />;
      case "cancelled":
        return <NoticeRow badge="cancelled" text={item.reason} tone="quiet" />;
      case "setting":
        return <NoticeRow badge="settings" text={item.text} tone="quiet" />;
      case "retrying":
        return <RetryRow item={item} />;
      case "compaction":
        return (
          <div className="flex flex-col gap-1.5" data-testid="compaction">
            {item.failed ? (
              <NoticeRow badge="compaction" text={`${item.failed.status}${item.failed.error ? `: ${item.failed.error}` : ""}`} tone="warn" />
            ) : (
              <>
                {item.summary && <SummaryFold text={item.summary} />}
                <CompactedLine native={item.native} pending={item.pending} tokens={item.tokens} trigger={item.trigger} />
              </>
            )}
          </div>
        );
      case "queue-failed":
        return <QueueFailedRow actions={actions} item={item} />;
      case "not-resumed":
        return <NotResumedRow actions={actions} item={item} />;
      case "thought":
        return <ThoughtRow item={item} />;
      case "prepared":
        return <PreparedRow item={item} />;
      case "grant":
        return item.of === "google" ? (
          <NoticeRow
            badge="google"
            text={
              item.what === "given"
                ? `may change ${item.repository} in Google Cloud without asking, never deleting, given by you`
                : item.what === "used"
                  ? `changed ${item.repository} in Google Cloud under its grant`
                  : `may no longer change ${item.repository || "its project"} in Google Cloud: ${item.why}`
            }
            tone="quiet"
          />
        ) : (
          <NoticeRow
            badge="push"
            text={
              item.what === "given"
                ? `may push ${item.branch} without asking, given by you`
                : item.what === "used"
                  ? `pushed ${item.branch} under its grant`
                  : `may no longer push ${item.branch || "its branch"}: ${item.why}`
            }
            tone="quiet"
          />
        );
      case "failed":
        return <NoticeRow badge="turn failed" text={`${item.reason} (${item.phase})`} tone="bad" />;
      case "note":
        return <NoteRow item={item} />;
    }
  },
  (a, b) => rowSignature(a.item) === rowSignature(b.item) && a.actions === b.actions,
);

// ---- live work trace ------------------------------------------------------

/** The tool name stays in its own element rather than being interpolated
 *  into a sentence: "which tool is running" is a distinct fact, and the
 *  proofs assert on it directly. */
function PhaseBody({ phase, since }: { phase: ActivityPhase; since?: string }) {
  switch (phase.phase) {
    case "contacting_model":
      return since ? (
        <>
          Thinking… <Since since={since} />
        </>
      ) : (
        <>thinking…</>
      );
    case "streaming":
      return <>streaming response…</>;
    case "running_tool":
      return phase.kind === "testing" ? (
        <>testing…</>
      ) : (
        <>
          running{" "}
          <code className="font-mono" data-testid="activity-tool">
            {phase.tool}
          </code>
          {phase.preview ? (
            <>
              {" — "}
              <code className="min-w-0 truncate font-mono opacity-80" data-testid="activity-preview">
                {phase.preview}
              </code>
            </>
          ) : (
            "…"
          )}
          {phase.output && (
            <>
              {" · "}
              <code className="min-w-0 truncate font-mono opacity-60" data-testid="activity-output">
                {stripAnsi(phase.output)}
              </code>
            </>
          )}
        </>
      );
    case "awaiting_approval":
      return (
        <>
          {phase.kind === "file_edit"
            ? "a change to accept or reject"
            : phase.kind === "question"
              ? "an answer to its question"
              : "an approval"}
        </>
      );
    default:
      return null;
  }
}

/** Self-guarding: renders nothing unless there is something to report.
 *
 *  The `busy`-without-a-phase case is not a fallback for tidiness — it is
 *  the only signal a human gets when the server's per-agent projection says
 *  a turn is running but no work-trace snapshot has arrived yet (a turn
 *  another client started, or the moment before the activity bootstrap
 *  resolves). Dropping it would show an idle-looking panel for a busy agent. */
function ActivityStrip({ snap }: { snap: AgentPanelSnapshot }) {
  const { show } = activityView(snap.activity);
  const phase = show ? (snap.activity?.phase ?? null) : null;
  if (!phase && !snap.busy) return null;
  const stopping = snap.activity?.cancel_requested === true;
  // Counts the whole turn, so it follows the phase text through every phase.
  const tokens = liveTokensText(snap.activity?.live_tokens);

  // Stream text is assistant output, not status chrome. It is deliberately
  // ephemeral: once Heart records the completed reply the live phase goes
  // terminal and the ordinary transcript row replaces this one. Streamdown
  // is fed the incomplete Markdown directly so code fences/lists remain
  // usable while the provider is still writing them.
  const recent = snap.activity?.recent_completed_tools ?? [];
  if (phase?.phase === "streaming" && phase.text !== "" && !stopping) {
    return (
      <>
        <Message className="max-w-full" data-testid="activity" from="assistant">
          <MessageContent className="w-full">
            <div data-testid="activity-stream">
              <Reply streaming text={phase.text} />
            </div>
          </MessageContent>
        </Message>
        {tokens && (
          <div className="px-1 text-(--fg-dim) text-[11px]" data-testid="activity-tokens">
            {tokens}
          </div>
        )}
        {recent.length > 0 && (
          <Collapsible className="bakery-activity" data-testid="activity-work">
            <CollapsibleTrigger className="bakery-activity-line">
              <span className="bakery-activity-title">Recent work</span>
              <RecentCount count={recent.length} />
            </CollapsibleTrigger>
            <CollapsibleContent>
              <RecentTools tools={recent} />
            </CollapsibleContent>
          </Collapsible>
        )}
      </>
    );
  }

  const waiting = phase?.phase === "awaiting_approval" && !stopping;
  const title = stopping ? "Stopping current work" : waiting ? "Needs you" : "Current work";
  // One line — what is happening now — that opens onto the tools this turn
  // has already finished, when there are any.
  return (
    <Collapsible className="bakery-activity" data-testid="activity" data-waiting={waiting ? "" : undefined}>
      <CollapsibleTrigger className="bakery-activity-line" disabled={recent.length === 0}>
        <Loader className="shrink-0 text-[var(--accent-fg)]" size={13} />
        <span className="bakery-activity-title">{title}</span>
        <span className="bakery-activity-now">
          {stopping ? "stopping…" : phase ? <PhaseBody phase={phase} since={snap.activity?.phase_since} /> : "working…"}
          {tokens && (
            <span data-testid="activity-tokens">
              {" · "}
              {tokens}
            </span>
          )}
        </span>
        {recent.length > 0 && <RecentCount count={recent.length} />}
      </CollapsibleTrigger>
      <CollapsibleContent>
        <RecentTools tools={recent} />
      </CollapsibleContent>
    </Collapsible>
  );
}

/** Whether the live strip has anything to report (the same test it makes). */
function activityVisible(snap: AgentPanelSnapshot): boolean {
  const { show } = activityView(snap.activity);
  return (show && !!snap.activity?.phase) || snap.busy;
}

function RecentCount({ count }: { count: number }) {
  return (
    <span className="bakery-activity-count">
      {count} done
      <ChevronRightIcon aria-hidden className="bakery-activity-chevron size-3.5" />
    </span>
  );
}

/** The tools a live turn has already finished, in the order they ended. */
function RecentTools({ tools }: { tools: AgentActivity["recent_completed_tools"] }) {
  return (
    <div className="bakery-activity-body">
      {tools.map((tool) => (
        <div className="bakery-activity-item" data-failed={tool.ok ? undefined : ""} key={tool.call_id}>
          <code>{tool.tool}</code>
          <span>{tool.ok ? "completed" : "failed"}</span>
        </div>
      ))}
    </div>
  );
}

/** A conversation shows its newest output on the very first frame. The
 *  scroller's own pin lands a frame late — after the browser has already
 *  painted the top of the thread, which was the flash. A layout effect runs
 *  after the DOM is in place but before paint: on open, and again when a
 *  cold history first arrives.
 *
 *  It also keeps a reader at the bottom through every size change, and only
 *  one at the bottom: anyone else's thread never moves. The room kept for
 *  the dock grows (the queue opens, the composer grows a line) and the
 *  library watches only the content. */
function PinOnOpen({ hasHistory }: { hasHistory: boolean }) {
  const { scrollRef, contentRef } = useStickToBottomContext();
  useLayoutEffect(() => {
    const el = scrollRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [hasHistory, scrollRef]);
  useEffect(() => {
    const scroller = scrollRef.current;
    if (!scroller) return;
    const below = () => scroller.scrollHeight - scroller.clientHeight - scroller.scrollTop;
    // At the bottom as of the last scroll (the library's own pin stops a
    // pixel short): growing moves no scroll, so that is where the reader was
    // just before. Not the library's lock: any scroll down re-engages that,
    // however far up, and the browser pulling the scroll back as the
    // composer shrinks releases it.
    let atBottom = below() < 2;
    const scrolled = () => {
      atBottom = below() < 2;
    };
    let frame = 0;
    const settle = () => {
      cancelAnimationFrame(frame);
      // After layout has settled, so a fold and a new row that land in
      // separate frames are measured together.
      frame = requestAnimationFrame(() => {
        // A reader who scrolled up stays where they are, and one holding the
        // mouse down in the thread (selecting) too.
        if (!atBottom || scroller.matches(":active")) return;
        if (below() > 1) scroller.scrollTop = scroller.scrollHeight;
      });
    };
    scroller.addEventListener("scroll", scrolled, { passive: true });
    const observer = new ResizeObserver(settle);
    observer.observe(scroller);
    // Its border box: that room is the content's padding, which the
    // library's own observer (its content box) never sees change.
    if (contentRef.current) observer.observe(contentRef.current, { box: "border-box" });
    return () => {
      cancelAnimationFrame(frame);
      scroller.removeEventListener("scroll", scrolled);
      observer.disconnect();
    };
  }, [scrollRef, contentRef]);
  return null;
}

/** The dock floats over the thread, so its height lives on its parent as
 *  `--bakery-dock-h`: the transcript keeps that much room at its foot and
 *  the scroll button sits above it. Set before the first paint, and again
 *  whenever the dock grows (a queue, a question, a second line typed). */
function measureDock(dock: HTMLDivElement | null) {
  const stage = dock?.parentElement;
  if (!dock || !stage) return;
  const set = () => stage.style.setProperty("--bakery-dock-h", `${dock.offsetHeight}px`);
  set();
  const observer = new ResizeObserver(set);
  observer.observe(dock);
  return () => observer.disconnect();
}

/** Sending means "show me what I just sent": re-engage the stick-to-bottom
 *  lock even after a scroll, a click, or a collapsing row released it, so the
 *  new message and the reply under it stay in view. */
function ScrollOnSend({ sent }: { sent: number }) {
  const { scrollToBottom } = useStickToBottomContext();
  const handled = useRef(sent);
  useEffect(() => {
    if (handled.current === sent) return;
    handled.current = sent;
    void scrollToBottom("instant");
  }, [sent, scrollToBottom]);
  return null;
}

// ---- durable follow-up queue --------------------------------------------

/** Heart's queue, as it stands. A waiting message can be taken back (the
 * node refuses once it is on its way): Edit puts it in the composer the way
 * ↑ does, × drops it. There is no reordering: Bakery owns exactly-once
 * dispatch, in order. */
function QueuedInstructions({
  items,
  paused,
  busy,
  actions,
  onEdit,
}: {
  items: readonly DurableQueuedItem[];
  paused: boolean;
  busy: boolean;
  actions: AgentPanelActions;
  onEdit: (item: DurableQueuedItem) => void;
}) {
  const [steering, setSteering] = useState(false);
  const [error, setError] = useState("");
  const steerAll = useCallback(async () => {
    setSteering(true);
    setError("");
    const out = await actions.steerAllQueued();
    if (out.status === "error") setError(out.error);
    setSteering(false);
  }, [actions]);
  const drop = useCallback(
    async (id: string) => {
      setError("");
      const out = await actions.withdrawQueued(id);
      if (out.status === "error") setError(out.error);
    },
    [actions],
  );
  if (items.length === 0) return null;
  // Only follow-ups wait for the turn to end; steers already join it.
  const canSteerAll = busy && items.some((item) => item.kind === "follow_up");
  return (
    <div className="bakery-ai-control-lane" data-testid="queue">
      <Queue className="bakery-ai-surface border-border bg-card py-1">
        <QueueSection defaultOpen>
          <QueueSectionTrigger data-testid="queue-toggle">
            <QueueSectionLabel
              count={items.length}
              icon={<ListTodoIcon className="size-3.5" />}
              label={paused ? "queued · paused" : "queued"}
            />
          </QueueSectionTrigger>
          <QueueSectionContent>
            {(canSteerAll || error) && (
              <div className="flex flex-wrap items-center gap-2 px-2 pb-1">
                {canSteerAll && (
                  <Button
                    className="h-6 px-2 text-xs"
                    data-testid="queue-steer-all"
                    disabled={steering}
                    onClick={() => void steerAll()}
                    size="sm"
                    title="Send every queued follow-up into the running turn now, as one steer"
                    variant="outline"
                  >
                    {steering ? "Steering…" : "Steer all now"}
                  </Button>
                )}
                {error && <span className="text-destructive text-xs">{error}</span>}
              </div>
            )}
            <QueueList>
              {items.map((item, i) => {
                const facts = [
                  item.kind === "steer" ? "Steer" : "Follow-up",
                  item.readOnly ? "Plan" : null,
                  item.filePath ? `file: ${item.filePath}` : null,
                  item.attachmentCount > 0
                    ? `${item.attachmentCount} attachment${item.attachmentCount === 1 ? "" : "s"}`
                    : null,
                ].filter((fact): fact is string => fact !== null);
                const shown = item.message ? `from ${item.message.from}: ${item.message.text}` : item.instruction || "Attachment-only instruction";
                return (
                  <QueueItem data-testid="queue-item" key={item.id}>
                    <div className="flex min-w-0 items-start gap-2">
                      <QueueItemIndicator />
                      <QueueItemContent title={shown}>{shown}</QueueItemContent>
                      <QueueItemActions>
                        <QueueItemAction
                          aria-label="Edit this message"
                          className="focus-visible:opacity-100"
                          data-testid="queue-edit"
                          onClick={() => onEdit(item)}
                          title={i === items.length - 1 ? "Edit (↑)" : "Edit"}
                        >
                          <PencilIcon className="size-3" />
                        </QueueItemAction>
                        <QueueItemAction
                          aria-label="Remove this message"
                          className="focus-visible:opacity-100"
                          data-testid="queue-remove"
                          onClick={() => void drop(item.id)}
                          title="Remove"
                        >
                          <XIcon className="size-3" />
                        </QueueItemAction>
                      </QueueItemActions>
                    </div>
                    <QueueItemDescription>{facts.join(" · ")}</QueueItemDescription>
                  </QueueItem>
                );
              })}
            </QueueList>
          </QueueSectionContent>
        </QueueSection>
      </Queue>
    </div>
  );
}

// ---- header ---------------------------------------------------------------

/** The pane's name, and what stands between it and work. The seat's runtime
 *  and the model are the composer's two pills. */
function PanelHeader({
  snap,
  actions,
  extra,
  afterTitle,
}: {
  snap: AgentPanelSnapshot;
  actions: AgentPanelActions;
  /** The host's own buttons for this pane, at the header's end. */
  extra?: ReactNode;
  /** The host's own note right after the name. */
  afterTitle?: ReactNode;
}) {
  return (
    <header className="flex min-h-8 shrink-0 flex-wrap items-center gap-x-2 gap-y-1 border-(--line-row) border-b bg-(--bg-raised) px-4 py-1">
      <span className="font-bold text-(--fg) text-xs" data-testid="agent-role">
        {snap.title || snap.roleLabel}
      </span>
      {afterTitle}
      {snap.unavailableReason && (
        <span
          className="flex min-w-0 items-center gap-1 text-destructive text-xs"
          data-testid="agent-unavailable"
          title={snap.unavailableReason}
        >
          <TriangleAlertIcon className="size-3 shrink-0" />
          <span className="truncate">{snap.unavailableReason}</span>
        </span>
      )}
      <span className="flex-1" />
      {snap.pendingProposals > 0 && (
        <Button className="h-6 px-2 text-xs" onClick={() => actions.openReview()} size="sm" variant="ghost">
          Review {snap.pendingProposals}
        </Button>
      )}
      {snap.taskBaseline && (
        // Deliberately a separate control from the pending count above, and
        // never added to it: this compares the worktree to the task's
        // materialized baseline, which is a different snapshot and says
        // nothing about whether anything is awaiting approval.
        <Button
          className="h-6 px-2 text-xs"
          onClick={() => actions.openReview()}
          size="sm"
          title="Changed since the task started"
          variant="ghost"
        >
          <span className="font-mono">
            +{snap.taskBaseline.added} −{snap.taskBaseline.removed}
          </span>
          <span className="text-muted-foreground"> since the task started</span>
        </Button>
      )}
      {extra}
    </header>
  );
}

// ---- empty / unconfigured -------------------------------------------------

function ConfigureCard({ snap, actions }: { snap: AgentPanelSnapshot; actions: AgentPanelActions }) {
  const [model, setModel] = useState("");
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState("");
  const noModels = snap.availableModels.length === 0;

  const inFlight = useRef(false);

  const assign = useCallback(async () => {
    if (model === "" || inFlight.current) return;
    inFlight.current = true;
    setBusy(true);
    setMessage("");
    const out = await actions.assignModel(model);
    if (out.status === "error") setMessage(out.error);
    if (out.status === "ok") setMessage(`${snap.roleId} → ${model}`);
    setBusy(false);
    inFlight.current = false;
  }, [actions, model, snap.roleId]);

  return (
    <div className="flex flex-1 flex-col items-center justify-center gap-3 p-6 text-center" data-testid="configure-card">
      <p className="text-muted-foreground text-sm">No agent configured for {snap.title || snap.roleLabel} yet.</p>
      {/* The control is always here, disabled when there is genuinely
          nothing to pick — not hidden. Hiding it would read as "this agent
          cannot be configured", which is a different and wrong claim from
          "no model is ready on this socket yet". */}
      <div className="flex items-center gap-2" data-testid="seat-row">
        <select
          aria-label="Model to assign"
          className="h-8 rounded-md border border-border bg-background px-2 text-sm disabled:opacity-50"
          data-testid="assign-model-select"
          disabled={noModels || busy}
          onChange={(e) => setModel(e.currentTarget.value)}
          value={model}
        >
          <option value="">— pick model —</option>
          {snap.availableModels.map((m) => (
            <option key={m.id} value={m.id}>
              {m.label}
            </option>
          ))}
        </select>
        <Button data-testid="assign" disabled={noModels || busy || model === ""} onClick={() => void assign()} size="sm">
          Assign
        </Button>
      </div>
      {noModels && <p className="text-muted-foreground text-xs">No models are ready for this thread yet.</p>}
      {message && <p className="text-muted-foreground text-xs">{message}</p>}
    </div>
  );
}

// ---- panel ----------------------------------------------------------------

/** The transcript as blocks. What you send is a box of its own; everything
 *  the agent does between two of yours is one timeline, a dot per step. */
type Block = { key: string; items: TranscriptItem[]; timeline: boolean };

function transcriptBlocks(items: readonly TranscriptItem[]): Block[] {
  const out: Block[] = [];
  for (const item of items) {
    // Renders nothing (see `TranscriptRow`), so it must not split a run.
    if (item.kind === "session" && !item.forkNote) continue;
    const timeline = !startsTurn(item);
    const last = out[out.length - 1];
    if (timeline && last?.timeline) last.items.push(item);
    else out.push({ key: item.hash, items: [item], timeline });
  }
  return out;
}

/** A step's dot: green done, red failed, amber waiting on you, grey words. */
function dotOf(item: TranscriptItem): "ok" | "bad" | "warn" | "live" | "idle" {
  switch (item.kind) {
    // A call Stop cut short is no failure.
    case "tool":
      return item.stopped ? "idle" : item.ok ? "ok" : "bad";
    case "acp_tool": {
      const status = acpStatus(item);
      return status === "failed" ? "bad" : status === "done" ? "ok" : status === "running" ? "live" : "idle";
    }
    case "gate":
      if (!item.decision && !item.cancelled && !item.reason && !item.expired) return "warn";
      return item.decision === "allow_once" || item.decision === "allow_always" ? "ok" : "idle";
    case "proposal":
      return item.verdict === "pending" ? "warn" : item.verdict === "accepted" ? "ok" : "idle";
    case "access":
    case "file-ask":
      return item.allowed === undefined ? "warn" : item.allowed ? "ok" : "idle";
    case "hosts":
      return item.asks.some((a) => a.allowed === undefined) ? "warn" : item.asks.every((a) => a.allowed) ? "ok" : "idle";
    case "failed":
      return "bad";
    case "exhausted":
      return "warn";
    default:
      return "idle";
  }
}

export function AgentPanel({
  store,
  headerActions,
  afterTitle,
  agents,
  openFile,
  finish,
  showTerminals,
  replyActions,
  side,
  pinned,
}: {
  store: AgentPanelStore;
  headerActions?: ReactNode;
  /** Right after the pane's name in its header, from the shell (its branch). */
  afterTitle?: ReactNode;
  /** Pinned under the header, above the transcript, from the shell (the brief). */
  pinned?: ReactNode;
  /** Beside each reply's Copy, from the shell (Save). */
  replyActions?: (hash: string) => ReactNode;
  /** A column beside the transcript and composer, from the shell (Saved). */
  side?: ReactNode;
  /** The composer's "● N agents" pill, from the shell. */
  agents?: ReactNode;
  /** The thread's finish card, from the shell: last in the transcript. */
  finish?: ReactNode;
  /** Opens a recorded change's file in this thread's Code at the line it
   *  changed (the shell); without it, file names stay plain text. */
  openFile?: (change: FileChange) => void;
  /** Brings the terminal panel forward on a prepared row's terminals, or
   *  (none named) a command's row: this thread's Agent terminal, beside the
   *  thread, on that command (`at`). */
  showTerminals?: (terminalIds: string[], at?: CommandAt) => void;
}) {
  const snap = useSyncExternalStore(store.subscribe, store.getSnapshot, store.getSnapshot);
  const fileLinks = useMemo<FileLinks | null>(
    () =>
      openFile
        ? {
            open: (hash) => {
              const it = snap.items.find((i) => i.kind === "proposal" && i.hash === hash);
              if (it?.kind !== "proposal") return;
              // A hidden or binary change carries no text to find its line in.
              const blind = !!it.landed?.hidden || !!it.landed?.binary;
              openFile({ path: it.filePath, line: blind ? 1 : firstChangedLine(it.oldContent, it.newContent), after: blind ? null : it.newContent });
            },
            changeFor: (path, at) => changeFor(snap.items, path, at),
            look: (path) => openFile({ path, line: 1, after: null }),
          }
        : null,
    [openFile, snap.items],
  );
  const modelLabel = useCallback(
    (id: string) => snap.availableModels.find((m) => m.id === id)?.label ?? (id || "This runtime"),
    [snap.availableModels],
  );
  const [historyWindow, setHistoryWindow] = useState(DEFAULT_HISTORY_WINDOW);
  const [resumeError, setResumeError] = useState("");
  const [resuming, setResuming] = useState(false);
  const [sent, setSent] = useState(0);
  const onSent = useCallback(() => setSent((n) => n + 1), []);
  // Files dropped or pasted anywhere in the app attach here while this is
  // the chat in use — the one last clicked or typed in (see attachTargets).
  const attachRef = useRef<((files: File[]) => void) | null>(null);
  const takeBackRef = useRef<((item: DurableQueuedItem) => void) | null>(null);
  const [dropping, setDropping] = useState(false);
  const attachTarget = useRef<ReturnType<typeof registerAttachTarget> | null>(null);
  useEffect(() => {
    const target = registerAttachTarget({
      add: (files) => {
        const add = attachRef.current;
        if (!add) return false;
        add(files);
        return true;
      },
      setDropping,
    });
    attachTarget.current = target;
    return () => target.dispose();
  }, []);

  // Rows that arrive while a conversation is open widen the window instead of
  // sliding it: a slide drops the oldest row above someone reading, and the
  // scroller has no anchoring to hold their place.
  // ponytail: rows added while open are never trimmed; reopening re-windows.
  const conversationKey = `${snap.streamId ?? ""}:${snap.agentId}`;
  const [opened, setOpened] = useState<{ key: string; length: number } | null>(null);
  if (!snap.historyLoading && opened?.key !== conversationKey) {
    setOpened({ key: conversationKey, length: snap.items.length });
  }
  const arrived = opened?.key === conversationKey ? Math.max(0, snap.items.length - opened.length) : 0;
  const { shown, hidden } = useMemo(
    () => boundedHistory(snap.items, historyWindow + arrived),
    [snap.items, historyWindow, arrived],
  );
  const blocks = useMemo(() => transcriptBlocks(shown), [shown]);
  const hasVisibleHistory = useMemo(() => snap.items.some((i) => i.kind !== "session" || i.forkNote), [snap.items]);
  const activity = activityView(snap.activity);

  // The compact configure card is for a genuinely empty, unconfigured
  // conversation only. An agent that was configured and later broke keeps
  // its history and gets a notice — it must not vanish behind this card.
  const showConfigure = !snap.canSend && !hasVisibleHistory && snap.assignedModelId == null && !snap.disabled;

  const resumeInFlight = useRef(false);
  const resume = useCallback(async () => {
    if (resumeInFlight.current) return;
    resumeInFlight.current = true;
    setResuming(true);
    setResumeError("");
    const out = await store.actions.resume();
    if (out.status === "error") setResumeError(out.error);
    setResuming(false);
    resumeInFlight.current = false;
  }, [store.actions]);

  if (showConfigure) {
    return (
      <div className="bakery-ai bakery-ai-panel flex h-full min-h-0 flex-col bg-background text-foreground" data-testid="agent-panel">
        <PanelHeader actions={store.actions} afterTitle={afterTitle} extra={headerActions} snap={snap} />
        <ConfigureCard actions={store.actions} snap={snap} />
      </div>
    );
  }

  return (
    <ShowTerminalsContext.Provider value={showTerminals ?? null}>
    <ReplyActionsContext.Provider value={replyActions ?? null}>
    <ModelLabelsContext.Provider value={modelLabel}>
    <FileLinksContext.Provider value={fileLinks}>
    <div
      className="bakery-ai bakery-ai-panel relative flex h-full min-h-0 flex-col bg-background text-foreground"
      data-testid="agent-panel"
      onFocusCapture={() => attachTarget.current?.activate()}
      onPointerDownCapture={() => attachTarget.current?.activate()}
    >
      {dropping && (
        <div
          className="pointer-events-none absolute inset-2 z-20 flex items-center justify-center rounded-xl border-2 border-primary/60 border-dashed bg-background/70 text-foreground text-sm"
          data-testid="drop-overlay"
        >
          Drop to attach
        </div>
      )}
      <PanelHeader actions={store.actions} afterTitle={afterTitle} extra={headerActions} snap={snap} />
      {pinned}

      <div className="flex min-h-0 flex-1">
      <div className="relative flex min-h-0 min-w-0 flex-1 flex-col">
      {/* Opens at the newest output and stays pinned there: no animated
          scroll ever (history and late-rendered markdown snap, not glide),
          and a fresh scroller per conversation, so switching threads never
          starts from the top or from the last thread's position. Isolated,
          so nothing raised inside the thread rises over the dock. */}
      <Conversation
        className="isolate min-h-0 flex-1"
        initial="instant"
        resize="instant"
        key={conversationKey}
      >
        <ConversationContent className="bakery-ai-transcript-lane gap-3 pt-3" data-testid="transcript">
          {snap.streamId && <MovedIn shown={hidden === 0} streamId={snap.streamId} />}
          <EarlierEvents hidden={hidden} size={historyWindow} widen={() => setHistoryWindow((w) => w + DEFAULT_HISTORY_WINDOW)} />

          {snap.historyLoading && !hasVisibleHistory && (
            <div className="flex items-center gap-2 text-muted-foreground text-xs">
              <Loader size={14} /> loading history…
            </div>
          )}
          {snap.historyError && <NoticeRow badge="history" text={snap.historyError} tone="bad" />}

          {!snap.historyLoading && !hasVisibleHistory && !snap.historyError && (
            <ConversationEmptyState description="" title={`Nothing from ${snap.title || snap.roleLabel} yet`} />
          )}

          {blocks.map((block) =>
            block.timeline ? (
              <div className="bakery-timeline" key={block.key}>
                {block.items.map((item) => (
                  <div className="bakery-step" data-dot={dotOf(item)} key={item.hash}>
                    <TranscriptRow actions={store.actions} item={item} />
                  </div>
                ))}
                {pendingTurns(block.items).map((t) => (
                  <div className="bakery-step" data-dot="warn" key={`turn:${t.turnId}`}>
                    <TurnReview actions={store.actions} count={t.count} turnId={t.turnId} undoable={t.undoable} />
                  </div>
                ))}
              </div>
            ) : (
              <TranscriptRow actions={store.actions} item={block.items[0]} key={block.key} />
            ),
          )}

          {snap.interrupted && (
            <div className="bakery-callout" data-callout="bad" data-testid="resume-banner">
              <div className="bakery-callout-head">
                <TriangleAlertIcon className="bakery-callout-icon size-3.5" />
                {snap.interrupted === "failed" ? (
                  <>
                    <span className="bakery-callout-title">Stopped early</span>
                    <span>The last turn did not finish. Resume picks it up where it stopped.</span>
                  </>
                ) : (
                  <>
                    <span className="bakery-callout-title">Interrupted</span>
                    <span>The last turn died with a node restart.</span>
                  </>
                )}
              </div>
              <div className="flex flex-wrap items-center gap-1.5">
                <Button data-testid="resume" disabled={resuming} onClick={() => void resume()} size="xs" variant="outline">
                  <RotateCcwIcon />
                  {resuming ? "Resuming…" : "Resume"}
                </Button>
                {resumeError && <span className="text-destructive text-xs">{resumeError}</span>}
              </div>
            </div>
          )}

          {/* The live step, on a timeline of its own while it has something to say. */}
          {activityVisible(snap) && (
            <div className="bakery-timeline">
              <div className="bakery-step" data-dot="live">
                <ActivityStrip snap={snap} />
              </div>
            </div>
          )}
          {finish}
          <ScrollOnSend sent={sent} />
          <PinOnOpen hasHistory={hasVisibleHistory} />
        </ConversationContent>
        <ConversationScrollButton className="bakery-ai-scroll-end" />
      </Conversation>

      {/* Floats over the thread's foot (ai-panel.css): the queue, the
          agent's questions and the composer. */}
      <div className="bakery-ai-dock" ref={measureDock}>
        <QueuedInstructions
          actions={store.actions}
          busy={snap.busy}
          items={snap.queuedItems}
          onEdit={(item) => takeBackRef.current?.(item)}
          paused={snap.dispatchPaused}
        />

        {/* Keyed on the question, so a new ask starts on its first question. */}
        {snap.pendingAsk && (
          <QuestionCard
            actions={store.actions}
            hash={snap.pendingAsk.hash}
            key={snap.pendingAsk.hash}
            questions={snap.pendingAsk.questions}
          />
        )}

        {/* Keyed on the bound identity. Today this is belt-and-braces, not a
            bug fix: Dockview disposes the panel on a rebind before the
            composer's debounced draft write can fire, which was verified by
            removing this key and watching the socket-switch proof still pass.
            It stays because that ordering is Dockview's to change, and without
            the key a rebind that DID outlive the debounce would persist one
            socket's text under another's draft key. */}
        <Composer
          actions={store.actions}
          agents={agents}
          // The server's per-agent busy projection can arrive before the
          // optional activity snapshot (or survive a reconnect without one).
          // Stop must remain available in that honest busy-without-phase gap.
          attachRef={attachRef}
          cancellable={activity.cancellable || snap.busy}
          key={`${snap.streamId ?? "-"}::${snap.agentId}`}
          onSent={onSent}
          snap={snap}
          takeBackRef={takeBackRef}
        />
      </div>
      </div>
      {side}
      </div>
    </div>
    </FileLinksContext.Provider>
    </ModelLabelsContext.Provider>
    </ReplyActionsContext.Provider>
    </ShowTerminalsContext.Provider>
  );
}
