// Heart events → a human transcript.
//
// This is the "quiet tools" rule in code: the stream is an audit log, the
// workpane is a conversation. A tool call becomes one line (`read src/main.rs`),
// a proposal carries its own accept/reject verdict instead of leaving a second
// row for the decision, and node bookkeeping stays out of the default view.
//
// Pure and DOM-free on purpose — this is the part worth testing, and it is
// tested by `transcript.check.ts` without a browser.

import { payloadOf, type BakeryEvent } from "./node.ts";

/** Families that are real, recorded, and simply not conversation. */
const BOOKKEEPING = new Set([
  "stream.ownership.claimed",
  // Gate mode changes are chrome/audit — not chat (show in Events, not Orchestrator).
  "policy.write_mode.set",
  // Seat/model assignment is Studio configuration state, projected live by
  // GET /studio/state — not a chat message (07a).
  "seat.model.assigned",
  // A switch waiting for the turn to end, and one that couldn't land: the
  // seat's picker shows them.
  "seat.model.pending",
  "seat.model.switch_failed",
  // Durable composer controls are status chrome/audit, not turns. The pane
  // projects them separately so a queued instruction does not appear as an
  // empty "note" before the real human/agent conversation begins.
  "agent.follow_up.queued",
  // A steer waiting in the queue shows there; once delivered into the turn
  // (`agent.steer.applied`) it is the operator's message in the conversation.
  "agent.steer.queued",
  "agent.dispatch.paused",
  "agent.dispatch.resumed",
  "agent.operating_mode.set",
  "agent.queue.merged",
  // A waiting message taken back to edit (↑, Edit or ×): it leaves the queue
  // and never ran, so the conversation has nothing to show for it.
  "agent.queue.withdrawn",
  "agent.attachment.uploaded",
  // A thread's finish notice the newer one replaced: a note says so on its
  // row, and a waiting message leaves the queue.
  "message.replaced",
  // The thread told its spawner it finished: the notice shows there.
  "thread.finished",
  // A native compaction's size: folded into its row.
  "agent.context.compaction_size",
  // A Baker tool's allowed call ran: the approval card already shows it.
  "agent.child_tool.used",
  // Folded into its question's card.
  "agent.session.chosen",
]);

/** A proposal's gate state, matching the node's `decide()` exactly.
 *
 *  Accept, reject, and an operator Stop are terminal. `agent.edit.failed` is NOT a verdict:
 *  the node refuses a second decision only when an accept/reject already cites
 *  the proposal, so a decision that failed to land leaves it decidable. It
 *  stays `pending` here and carries `lastFailure` instead — treating it as
 *  terminal would hide a proposal that is still pending on disk and still
 *  retryable, with no way back to it. `GET /gates` lists exactly these. */
export type Verdict = "pending" | "accepted" | "rejected";

/** Parsed `agent.turn.usage` payload, attached to its matching "done" item
 *  by `turn_id` (07d-A3D1 Phase D) — every field optional because every
 *  numeric field is `Option` server-side, and a malformed/legacy payload
 *  must degrade instead of crashing. */
export type TurnUsage = {
  provider?: string;
  modelId?: string;
  effort?: string;
  tokensIn?: number;
  tokensOut?: number;
  contextUsed?: number;
  contextWindow?: number;
  thoughtTokens?: number;
  cacheReadTokens?: number;
  cacheWriteTokens?: number;
  cost?: { amount: number; currency: string };
  durationMs?: number;
};

/** One question an agent asked — one shape for every runtime: an HTTP
 *  seat's `ask_user` and a native runtime's form (Claude's AskUserQuestion). */
export type AskQuestion = {
  question: string;
  /** A short label for the question, shown as its tab. */
  header?: string;
  options: { label: string; description?: string }[];
  multiSelect: boolean;
};

/** One question's answer: the options picked, and/or the human's own words.
 *  Neither means that question was skipped. */
export type AskAnswer = { picks: string[]; other?: string };

function askQuestion(q: any): AskQuestion {
  // Options were plain strings before they carried descriptions.
  const options = Array.isArray(q?.options)
    ? q.options.flatMap((o: any) =>
        typeof o === "string"
          ? [{ label: o }]
          : typeof o?.label === "string"
            ? [typeof o.description === "string" && o.description ? { label: o.label, description: o.description } : { label: o.label }]
            : [],
      )
    : [];
  return {
    question: typeof q?.question === "string" ? q.question : String(q ?? ""),
    ...(typeof q?.header === "string" && q.header ? { header: q.header } : {}),
    options,
    multiSelect: q?.multiSelect === true,
  };
}

function askAnswer(a: any): AskAnswer {
  const picks = Array.isArray(a?.picks) ? a.picks.filter((p: unknown): p is string => typeof p === "string") : [];
  return typeof a?.other === "string" && a.other.trim() ? { picks, other: a.other } : { picks };
}

export type TranscriptItem =
  // The thread's first fact. `forkNote` says why it forked the local main
  // instead of origin's, just fetched (git rule 2); "" when it didn't.
  | { kind: "session"; at: string; hash: string; project: string; branch: string; name: string; forkNote: string }
  | {
      kind: "human";
      at: string;
      hash: string;
      text: string;
      filePath: string | null;
      /** Sent while a turn ran and delivered into it (`agent.steer.applied`). */
      steer?: boolean;
      /** What it was sent with, as each upload was recorded. */
      attachments?: Attached[];
      /** Who sent it when not the operator: the event's actor, e.g. "bakery"
       *  (the node itself) or "agent:baker". */
      by?: string;
    }
  | {
      kind: "tool";
      at: string;
      hash: string;
      label: string;
      ok: boolean;
      summary: string;
      args: string;
      /** The tool's own name (`label` is how it reads). */
      toolName: string;
      /** The native loop's id for the call: a command's Agent terminal
       *  block is marked with it. */
      callId?: string;
      /** Recorded by Bakery's MCP server for a call a native runtime made —
       *  see `withoutMcpDuplicates`. */
      viaMcp?: boolean;
      /** Stop caught it before it ran: the model asked, nothing ran it. */
      stopped?: "before";
    }
  | {
      kind: "proposal";
      at: string;
      hash: string;
      filePath: string;
      /** What of the file its two sides are, when not the whole file: a
       *  notebook's one cell (Claude Code's NotebookEdit), or only the change
       *  of a file too big to show whole. */
      part?: "cell" | "change";
      rationale: string;
      oldContent: string;
      newContent: string;
      verdict: Verdict;
      /** Why the most recent decision attempt did not land. Present only while
       *  `verdict === "pending"` — the proposal is still open and retryable.
       *  Cleared once an accept/reject actually lands. */
      lastFailure?: string;
      /** Who decided — the decision event's actor (e.g. "human", "policy/auto").
       *  Absent while verdict === "pending": nobody has decided yet. */
      decidedBy?: string;
      /** Stop closed this approval before it was accepted or rejected. The
       *  wire verdict remains `rejected` so every existing pending-gate
       *  consumer treats it as terminal, while the UI can state the more
       *  truthful reason. */
      cancelled?: boolean;
      /** The proposing agent/model — Review must name who authored a change
       *  (role/agent_id), never imply "the model" is some singular author. */
      agentId?: string;
      modelId?: string;
      /** A change the agent's turn already made on disk (turn snapshots):
       *  keeping it writes nothing; undoing it restores the file as it was.
       *  `restorable` is false for a file too big to keep a copy of, which
       *  can only be kept; `binary` changes show no text. `hidden` (a
       *  credential or denied file) keeps its content out of the history:
       *  only the operator's `GET /agent/change` shows it. `operatorEdit`:
       *  the operator saved the file while the turn ran, so the change
       *  includes their edit and, like a file too big to copy, is only kept. */
      landed?: { turnId: string; restorable: boolean; binary: boolean; hidden?: string; operatorEdit: boolean };
    }
  | {
      kind: "done";
      at: string;
      hash: string;
      summary: string;
      actor: string;
      /** This turn's `agent.turn.usage` companion, if the provider reported
       *  any — folded in here rather than a second row (see the module doc
       *  above: a proposal already carries its own verdict the same way). */
      usage?: TurnUsage;
      /** The turn's words already appear as `segment` rows between its tool
       *  calls; `summary` is the same text joined, kept for copying. */
      segmented?: boolean;
    }
  // Native assistant text in the order it was written (`agent.acp.text`):
  // the words before a tool call, then the ones after. A subagent's own words
  // (`agent.acp.subagent_text`) carry the call that launched it.
  | { kind: "segment"; at: string; hash: string; text: string; parentId?: string }
  | { kind: "exhausted"; at: string; hash: string; reason: string; steps: number }
  // A human explicitly stopped the turn (07d-A5) — distinct from "exhausted"
  // (hit the step limit) and "failed" (a real error): nobody misbehaved here.
  | { kind: "cancelled"; at: string; hash: string; reason: string }
  // A runtime setting changed (model, effort, …), as one sentence.
  | { kind: "setting"; at: string; hash: string; text: string }
  // A turn waiting out a passing failure (agent.turn.retrying: the provider
  // busy, the Wi-Fi gone, a timeout): what cut it off and its latest try,
  // one line per stretch of waiting; `waiting` while nothing came after it.
  | { kind: "retrying"; at: string; hash: string; turnId: string; reason: string; attempt: number; waiting: boolean }
  // The conversation's context was compacted to fit: how, its size before,
  // and the summary it went on from.
  | {
      kind: "compaction";
      at: string;
      hash: string;
      trigger: string | null;
      /** Its size in tokens before, when reported; null when not. */
      tokens: number | null;
      /** A native one's size comes after it, from the session's own record,
       *  and may still come while its turn runs. */
      pending?: boolean;
      summary: string;
      /** A native runtime's own (Claude Code's /compact or its auto-compact). */
      native?: boolean;
      /** How a native one ended, when not completed, and why. */
      failed?: { status: string; error: string };
    }
  // A waiting message the node could not send: its words, its files, and why.
  | { kind: "queue-failed"; at: string; hash: string; text: string; reason: string; attachments?: Attached[] }
  // A native seat's last session could not resume: a new one took over,
  // briefed from the thread.
  | {
      kind: "not-resumed";
      at: string;
      hash: string;
      reason: string;
      /** The turn stopped to ask: retry it, or start fresh. */
      awaiting: boolean;
      /** The operator's answer, once given. */
      chosen?: "retry" | "fresh";
      /** A newer session took over before an answer: the node takes none now. */
      superseded?: boolean;
    }
  // One stretch of the agent's thinking (agent.thought), for the operator's
  // view only: how long, and its text when the runtime shared it (`cut`
  // when History kept only the first 20,000 characters).
  | { kind: "thought"; at: string; hash: string; ms: number; text: string; cut: boolean; shared: boolean; modelId: string }
  // Commands typed at the thread's terminals for the operator to run (ST-3):
  // each waits for their Enter. `by` is who typed them (the event's actor).
  | { kind: "prepared"; at: string; hash: string; by: string; commands: { terminalId: string; name: string; command: string }[] }
  // A model switch someone decided (the Baker, on the model proxy's quota
  // signal): the seat's old and new model, why, who, and the event that
  // caused it. A plain seat assignment stays Studio state, out of the chat.
  | {
      kind: "model_switch";
      at: string;
      hash: string;
      seat: string;
      /** The model the seat had; null when it had none. */
      from: string | null;
      to: string;
      reason: string;
      /** The causing event's hash (a model.quota_exhausted), when given. */
      because: string | null;
      /** Who decided it: the event's actor. */
      by: string;
    }
  // The node hit jail/model/tools trouble before a turn could produce anything
  // reviewable — the human still sent an instruction, so they need to see it died.
  | { kind: "failed"; at: string; hash: string; reason: string; phase: "jail" | "model" | "tools" }
  // The agent asked the human a question (`ask_user`). The turn is PARKED
  // on it (continue-after-decision) until an answer lands — `answer` is
  // the human's reply once it exists, folded from `agent.ask.answered`.
  | {
      kind: "ask";
      at: string;
      hash: string;
      questions: AskQuestion[];
      /** The answers as one line of text — every answered ask has it. */
      answer?: string;
      /** One answer per question, when the client that answered sent them. */
      answers?: AskAnswer[];
      /** The operator stopped the parked turn before answering. */
      cancelled?: boolean;
    }
  // A native (ACP) tool call: its announcement and every status/result update
  // folded into ONE row by `toolCallId`, carrying everything the runtime
  // reported — the operator can expand it to every byte (and fetch the
  // complete payload when Heart's copy was bounded: `fullRefs`).
  | {
      kind: "acp_tool";
      at: string;
      hash: string;
      toolCallId: string;
      title: string;
      toolKind: string | null;
      status: string | null;
      rawInput: unknown;
      rawOutput: unknown;
      content: unknown[];
      locations: unknown[];
      /** Every raw update that built this row, in stream order; `full`
       *  names the whole of one the Heart's copy cut. */
      updates: { event: string; at: string; detail: unknown; full?: { sha: string; bytes: number } }[];
      fullRefs: { sha: string; bytes: number }[];
      /** Its turn ended before the runtime reported completed/failed — the
       *  call will never finish, so it must not read as "running". */
      unfinished?: boolean;
      /** Stop cut it short: before it ran (held at a card or a review), or
       *  after it started, its result or output so far what it got done
       *  (`stopped_before` / `stopped_after`). `turn`: its turn was stopped
       *  while it was open, from a node that did not say which. */
      stopped?: "before" | "after" | "turn";
      /** A command the runtime ran in its own terminal (`_meta.terminal_*`):
       *  the output so far, in order, and its exit code once it ended. */
      terminal?: { output: string; exitCode: number | null };
      /** The subagent call this one belongs to: the runtime's
       *  `_meta.claudeCode.parentToolUseId`. */
      parentId?: string;
      /** Set when this call launched a subagent (Claude's Agent/Task tool,
       *  or any call another names as its parent). */
      subagent?: { description: string; type: string | null };
      /** That subagent's steps and words, in order — kept here so they never
       *  split this agent's own reply (see `nestSubagents`). */
      children?: TranscriptItem[];
    }
  // A native tool permission request — a paused tool call waiting on the
  // operator. `decision` is folded in from `agent.child_tool.decided`.
  | {
      kind: "gate";
      at: string;
      hash: string;
      title: string;
      toolKind: string;
      plug: string;
      decision?: "allow_once" | "allow_always" | "deny";
      /** Where its "Always allow" holds: a project (by name), the Baker's
       *  conversations, or this chat. Absent from a node that allowed
       *  everywhere. */
      always?: { scope: string; name: string | null };
      decidedBy?: string;
      /** The node's own reason when it refused with nothing to decide. */
      reason?: string;
      cancelled?: boolean;
      /** Its turn ended with no decision — nothing is waiting on it any more
       *  (the node refuses a late answer), so it is not actionable. */
      expired?: boolean;
      /** The exact file changes the runtime says this call makes — shown so
       *  an edit is approved by what it does, not by its title. */
      diffs: { path: string; oldText: string; newText: string }[];
      /** One Baker tool call graded by the node (`api_tools`): allowing it
       *  allows that call only, never a whole kind. */
      bakeryTool?: { name: string; risk: string };
      /** A push of the conversation's own branch, or a change to a Google
       *  Cloud project from its rooms, which the operator may answer with a
       *  grant: it pushes that branch (never forced), or changes that project
       *  (never deleting), without asking from then. */
      grant?: { kind: "push" | "google"; repository: string; branch: string };
      /** A room's change to Google Cloud held at its door: the request, and
       *  the names of the fields it sends (never their values). */
      google?: { method: string; url: string; sends: string };
      /** A push: exactly what goes out, the head an approval sends, its
       *  commits (newest first) and its diff against what the remote has. */
      push?: {
        branch: string;
        head: string;
        /** Null when the branch is new on the remote. */
        remoteHead: string | null;
        base: string;
        commits: { sha: string; subject: string }[];
        diff: string;
        diffTruncated: boolean;
        secret: boolean;
      };
      /** Why it closed without a decision: Stop, or a grant that let it go. */
      cancelReason?: string;
    }
  // A conversation's push grant (git.grant.*): given by the operator for one
  // branch, a push it let go ahead without a card, and its end.
  | {
      kind: "grant";
      at: string;
      hash: string;
      what: "given" | "used" | "ended";
      /** A branch's pushes, or a Google Cloud project's changes. */
      of: "push" | "google";
      branch: string;
      repository: string;
      why: string;
    }
  // The Baker asking for a folder outside its workspace (`access_request`).
  // Unlike a gate it stays open after its turn ends: the Baker ends the turn
  // to wait, and an Allow carries the conversation on.
  | {
      kind: "access";
      at: string;
      hash: string;
      path: string;
      /** "read" | "write" | "exec" | "full" */
      access: string;
      reason: string;
      allowed?: boolean;
      decidedBy?: string;
    }
  // Hosts the thread's sandbox tried that its project's list doesn't hold
  // (`network.host.ask`), the asks that arrive together on one card. Only
  // the operator answers, for the whole project (`network.host.decided`).
  | {
      kind: "hosts";
      at: string;
      hash: string;
      projectId: string;
      asks: { hash: string; host: string; program: string | null; allowed?: boolean }[];
    }
  // A message between conversations: out on the sender's (`message.sent`),
  // with whether a turn of the receiver's took it up yet
  // (`message.taken_up`); in on the receiver's, as the turn it started. A
  // note starts no turn: in on the receiver's where it landed
  // (`message.received`), until a turn there reads it (`message.read`).
  | {
      kind: "message";
      at: string;
      hash: string;
      dir: "in" | "out";
      id: string;
      /** The other conversation, and its name as the node had it. */
      peer: string;
      peerName: string;
      text: string;
      /** Out: a turn of the receiver's took it up, or read the note. */
      takenUp?: boolean;
      /** A note: it started no turn, and the receiver's next turn reads it. */
      note?: boolean;
      /** A note in: a turn here read it, or a newer one from the same
       *  conversation replaced it before any did. */
      read?: boolean;
      replaced?: boolean;
    }
  // A chef's work ready to merge (`merge.waiting`, on its thread and its
  // spawner's), until its merge (`merge.done`) or its next ask replaces it.
  | {
      kind: "merge-waiting";
      at: string;
      hash: string;
      /** The chef's thread. */
      thread: string;
      name: string;
      branch: string;
      base: string;
      commits: { sha: string; subject: string }[];
      files: string[];
      adds: number;
      dels: number;
      /** The conversation it was told to, which reviews it (a chef's
       *  spawner), and its name as the node had it. A workspace's work has
       *  none. */
      spawner?: string;
      spawnerName?: string;
      /** This is the chef's own copy of the card, on its own thread. */
      own: boolean;
      merged?: { into: string; commit: string; uncommitted: boolean };
      replaced?: boolean;
    }
  // The conversation's brief set, changed, cleared, kept or turned down
  // (`brief.set`, `brief.decided`); the bar under the header shows it.
  | { kind: "brief"; at: string; hash: string; by: string; what: "set" | "changed" | "cleared" | "kept" | "turned down" }
  // An agent here added to the backlog (`backlog.added`).
  | { kind: "backlog-added"; at: string; hash: string; title: string }
  // A merge with no ask before it in this conversation.
  | { kind: "merged"; at: string; hash: string; branch: string; into: string; commit: string; uncommitted: boolean }
  // This chef's work overlaps another's waiting to merge (`merge.overlap`):
  // the files both change, none when it's the same branch.
  | { kind: "merge-overlap"; at: string; hash: string; waiting: string; files: string[] }
  // A secret-named file the thread's room was refused (`file.read.ask`), with
  // what the node says it is. Only the operator answers, for the whole
  // project (`file.read.decided`).
  | {
      kind: "file-ask";
      at: string;
      hash: string;
      projectId: string;
      /** From the thread's folder. */
      path: string;
      /** `public_key` | `certificate` | `private_key` | `token_or_env` | `unknown` */
      what: string;
      bytes: number;
      program: string | null;
      allowed?: boolean;
    }
  // Anything the client does not know about yet. Shown, not dropped: a silently
  // invisible event is indistinguishable from an event that never happened.
  // `raw` is the event's whole payload, so nothing about it is hidden.
  | { kind: "note"; at: string; hash: string; family: string; text: string; raw: string };

/** "Effort set to Max". Events from before the node recorded the picker's
 *  names carry only ids, so those read "Effort set to max". */
function settingText(p: Record<string, any>): string {
  const name = typeof p.name === "string" && p.name ? p.name : String(p.config_id ?? "setting").replace(/[_-]+/g, " ");
  const value = p.value_name ?? p.effective ?? p.requested;
  const shown = typeof value === "boolean" ? (value ? "on" : "off") : String(value ?? "?");
  return `${name.charAt(0).toUpperCase()}${name.slice(1)} set to ${shown}`;
}

/** An argv as one shell line, as the node's Agent terminal prints it: an
 *  argument that is not a plain word is single-quoted, so `sh -c 'a && b'`
 *  reads as it ran. */
export function shellLine(argv: readonly unknown[]): string {
  return argv
    .filter((a): a is string => typeof a === "string")
    .map((a) => (/^[\w./=:,@%+-]+$/.test(a) ? a : `'${a.replace(/'/g, "'\\''")}'`))
    .join(" ");
}

/** The exit code a `run` result reports: its first line, `exit_code: N`,
 *  exactly as the node writes it; null for any other text. */
export function runExitCode(result: string): number | null {
  const first = /^exit_code: (-?\d+)\s*(?:\n|$)/.exec(result);
  return first ? Number(first[1]) : null;
}

/** One line for a tool call — what it touched, not what it returned. */
export function toolLabel(tool: string, args: Record<string, any>): string {
  const path = typeof args?.path === "string" ? args.path : "";
  switch (tool) {
    case "list_files":
    case "list_dir":
      return `list ${path || "."}`;
    case "glob": {
      const pattern = typeof args?.pattern === "string" ? args.pattern : "";
      return `glob ${pattern || "?"}`;
    }
    case "read_file":
      return `read ${path || "?"}`;
    case "grep": {
      const pattern = typeof args?.pattern === "string" ? args.pattern : "";
      return `grep "${pattern}"${path ? ` in ${path}` : ""}`;
    }
    default: {
      const rest = args && Object.keys(args).length ? ` ${JSON.stringify(args)}` : "";
      return `${tool}${rest}`.slice(0, 120);
    }
  }
}

/** One quiet meta line for a turn's reported usage (07d-A3D1 Phase D):
 *  "provider · model_id · effort · Nin/Mout tokens", then the cache share
 *  and cost a native runtime reports and the turn's wall time, built from
 *  only whichever fields actually showed up — a turn without an override never
 *  has model_id/effort/context_window (Phase C: context_window is
 *  populated only when the turn's model_id was overridden), and not every
 *  provider reports token counts at all. */
/** Adds the `incoming` events not yet in `seen` to `existing`, in stream
 *  order, in one pass — returns `null` when nothing is new. Building a long
 *  history event by event copied the whole list once per event (quadratic),
 *  which froze long chats on open. A live event can land before a fetch's
 *  older ones, so an out-of-order result is sorted by `seq`. */
export function mergeEvents(
  existing: readonly BakeryEvent[],
  incoming: readonly BakeryEvent[],
  seen: Set<string>,
): BakeryEvent[] | null {
  const fresh: BakeryEvent[] = [];
  for (const ev of incoming) {
    if (seen.has(ev.event_hash)) continue;
    seen.add(ev.event_hash);
    fresh.push(ev);
  }
  if (fresh.length === 0) return null;
  const merged = existing.length === 0 ? fresh : [...existing, ...fresh];
  for (let i = 1; i < merged.length; i++) {
    if (merged[i].seq < merged[i - 1].seq) return merged.sort((a, b) => a.seq - b.seq);
  }
  return merged;
}

/** An event's actor in words, for "accepted by …" and "Message from …".
 *  The operator's own click reads "you"; `policy/auto` (the agent's own
 *  Auto / Edit automatically mode) reads "auto mode", so a mode's decision
 *  never looks like the operator clicked. An agent acting through the
 *  Bakery's tools is `agent:<stream id>:<seat>` and reads as its thread's
 *  name (split at the last ":", as a stream id can hold one); `agent:<seat>`
 *  alone, on the agent's own thread or from before, reads "@seat". Other
 *  actors ("node", …) pass through. */
export function actorLabel(actor: string, threadName: (streamId: string) => string | undefined = () => undefined): string {
  if (actor === "human") return "you";
  if (actor === "policy/auto") return "auto mode";
  if (!actor.startsWith("agent:")) return actor;
  const rest = actor.slice("agent:".length);
  const cut = rest.lastIndexOf(":");
  return cut < 0 ? `@${rest}` : (threadName(rest.slice(0, cut)) ?? `@${rest.slice(cut + 1)}`);
}

/** A turn's whole prompt. ACP counts input tokens apart from cache reads and
 *  writes, so it is all three together; an HTTP provider reports neither. */
export function promptTokens(usage: TurnUsage): number {
  return (usage.tokensIn ?? 0) + (usage.cacheReadTokens ?? 0) + (usage.cacheWriteTokens ?? 0);
}

/** A count of tokens as the thread reads it: whole under a thousand, then
 *  in thousands (to a tenth under ten thousand), then millions. A small
 *  count never reads "0k". */
export function tokenCount(n: number): string {
  const short = (value: number) => value.toFixed(1).replace(/\.0$/, "");
  if (n < 1_000) return String(Math.round(n));
  if (n < 10_000) return `${short(n / 1_000)}k`;
  if (n < 999_500) return `${Math.round(n / 1_000)}k`;
  return `${short(n / 1_000_000)}M`;
}

export function formatUsage(usage: TurnUsage): string {
  const parts: string[] = [];
  if (usage.provider) parts.push(usage.provider);
  if (usage.modelId) parts.push(usage.modelId);
  if (usage.effort) parts.push(usage.effort);
  const prompt = promptTokens(usage);
  // A count of zero was not reported (a `/compact` turn's): never "0in/0out".
  const tokens: string[] = [];
  if (typeof usage.tokensIn === "number" && prompt > 0) tokens.push(`${prompt}in`);
  if (typeof usage.tokensOut === "number" && usage.tokensOut > 0) tokens.push(`${usage.tokensOut}out`);
  if (typeof usage.thoughtTokens === "number" && usage.thoughtTokens > 0) tokens.push(`${usage.thoughtTokens}thought`);
  if (tokens.length) parts.push(`${tokens.join("/")} tokens`);
  // The share of the prompt read from cache.
  if (typeof usage.cacheReadTokens === "number" && prompt > 0) {
    parts.push(`${Math.round((100 * usage.cacheReadTokens) / prompt)}% cached`);
  }
  if (usage.cost) {
    const amount = usage.cost.amount.toFixed(usage.cost.amount < 0.1 ? 3 : 2);
    parts.push(usage.cost.currency === "USD" ? `$${amount}` : `${amount} ${usage.cost.currency}`);
  }
  if (typeof usage.durationMs === "number") {
    const seconds = usage.durationMs / 1000;
    parts.push(seconds < 60 ? `${seconds.toFixed(1)}s` : `${Math.floor(seconds / 60)}m ${Math.round(seconds % 60)}s`);
  }
  return parts.join(" · ");
}

/** Terminal output as plain text: ANSI escape sequences (colours, cursor
 *  moves, titles) removed, carriage-return progress lines left as their last
 *  state. */
export function stripAnsi(text: string): string {
  return text
    .replace(/\x1b\[[0-?]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b[@-Z\\-_]/g, "")
    .replace(/[^\n]*\r(?!\n)/g, "");
}

// Accept, reject, and `agent.approval.cancelled` close a proposal.
// `agent.edit.failed` records an
// attempt that did not land: the node's decide() refuses a re-decide only
// on a terminal decision, so the proposal is still open and still acceptable.
// It
// is kept `pending` with the failure attached, never shown as decided — a
// card that reads terminal would strand a retryable write. Module-scoped so
// `eventsForAgent` (07d-A) and `toTranscript` never drift apart on what a
// "decision" event is.
/** A landed change that took in the operator's save made during its turn. */
const operatorEdited = (landed: Record<string, unknown>) =>
  Array.isArray(landed.includes_operator_edit) && landed.includes_operator_edit.length > 0;

/** A seat assignment someone decided carries its reason, and is shown; a
 *  plain one is bookkeeping. */
const decidedSwitch = (ev: BakeryEvent) =>
  ev.family === "seat.model.assigned" && typeof payloadOf(ev).reason === "string";

const DECISION_FAMILIES = new Set([
  "agent.edit.accepted",
  "agent.edit.rejected",
  "agent.edit.failed",
  // The ACP pipe's own gate (pipe.rs): decides a pending
  // `agent.child_tool.pending`, the exact same causality-linked shape as
  // `agent.edit.*` deciding `agent.file.changed`.
  "agent.child_tool.decided",
  // Continue-after-decision (Checkpoint 2 item 2): the human's answer to
  // an `agent.ask.pending` question — folded into the question's own row,
  // never a second one.
  "agent.ask.answered",
  // Stop settles the exact pending operation so it no longer appears as an
  // actionable Review card or question after the active turn terminates.
  "agent.approval.cancelled",
  // The operator's answer to the Baker's ask for a folder.
  "agent.machine_access.decided",
]);

/** Explicit allowlist of families with no per-agent concept at all —
 *  socket-wide facts that stay visible to every agent, unaffected by
 *  `eventsForAgent`'s filter. 07d-A1 correction: this used to be inverted
 *  — a small agent-scoped allowlist, with everything else (including any
 *  future `agent.*` family someone forgets to add here) defaulting to
 *  socket-global and leaking into every agent's Chat. Now it is the only
 *  way IN to "visible to everyone"; everything else requires an exact
 *  `agent_id` match and fails closed without one. Mirrors the node's own
 *  `SOCKET_GLOBAL_FAMILIES` (`routes/agent.rs`). */
const SOCKET_GLOBAL_FAMILIES = new Set([
  "project.onboarded",
  "baker.assignment.accepted",
  "seat.model.assigned",
  "socket.kind.recorded",
  "socket.workspace.set",
  "terminal.prepared",
  "stream.lifetime.set",
  "stream.discarded",
  "stream.ownership.claimed",
  "policy.write_mode.set",
  "memory.written",
  // The thread's sandbox asked to reach a host, and the operator's answer:
  // the sandbox's, whichever agent ran the program.
  "network.host.ask",
  "network.host.decided",
  // Likewise a secret-named file the sandbox was refused, and the answer.
  "file.read.ask",
  "file.read.decided",
  // The conversation's push grants: the conversation's, whichever agent pushes.
  "git.grant.given",
  "git.grant.used",
  "git.grant.ended",
  // The conversation's messages to others: the conversation's, whichever agent sent them.
  "message.sent",
  "message.taken_up",
  // Its work waiting to merge, merged, or overlapping another's.
  "merge.waiting",
  "merge.done",
  "merge.overlap",
  // Its brief, the conversation's whichever agent works in it.
  "brief.set",
  "brief.decided",
  // What its agents added to the backlog.
  "backlog.added",
]);

/** Whether a message is a command (`/compact`, a skill): it reaches the
 *  runtime exactly as typed and runs only when sent. One that opens with a
 *  path (`/Users/…`) is not. The node's `pipe::is_command`, the same rule. */
export function isCommand(text: string): boolean {
  const first = text.trim().split(/\s+/)[0] ?? "";
  return first.length > 1 && first.startsWith("/") && !first.slice(1).includes("/");
}

/** What starts a turn: the operator's (or an agent's) instruction, or
 *  another conversation's message. A note starts none. */
export const startsTurn = (it: TranscriptItem): boolean => it.kind === "human" || (it.kind === "message" && it.dir === "in" && !it.note);

/** A message's own words, out of the instruction the node wraps them in. */
export function messageText(instruction: string): string {
  return instruction
    .replace(/^\[Message from [^\n]*\]\n/, "")
    .replace(/\n\n\[To answer: [^\n]*\]$/, "")
    // A chef's ready-to-merge: its card says the rest; its note stays.
    .replace(/^\[Ready to merge, from [^\n]*\n?/, "")
    .replace(/\n*(?:Review and test it, then merge it with thread_merge|Review, test and audit it, then bring the operator the merge review) [^\n]*$/, "");
}

const strings = (v: unknown): string[] => (Array.isArray(v) ? v.filter((x): x is string => typeof x === "string") : []);
/** A token count someone reported: a positive number. Zero, or anything
 *  else, is none. */
const counted = (v: unknown): number | null => (typeof v === "number" && v > 0 ? v : null);
type MergeCard = Extract<TranscriptItem, { kind: "merge-waiting" }>;

/**
 * Build the conversation. Events arrive in stream order (seq-ascending) and
 * that order is preserved; only decisions move, folding into the proposal they
 * cite so the human sees one reviewable thing per proposed change.
 */
/** Asks this close to the first one share its card. */
const HOSTS_TOGETHER_MS = 10_000;

/** A file a message was sent with: its upload's id, name, type and size. */
export type Attached = { id: string; filename: string; mediaType: string; bytes: number };

export function toTranscript(events: BakeryEvent[]): TranscriptItem[] {
  // Pass 1: collect verdicts, keyed by the proposal they decided. The node
  // records the link twice — `decided_event` in the payload and a causality
  // edge — so read the payload first and fall back to causality.
  const verdicts = new Map<string, Verdict>();
  const lastFailures = new Map<string, string>();
  const decidedBy = new Map<string, string>();
  const acceptedContent = new Map<string, string>();
  const answers = new Map<string, string>();
  const structuredAnswers = new Map<string, AskAnswer[]>();
  const cancelled = new Set<string>();
  const gateDecisions = new Map<string, "allow_once" | "allow_always" | "deny">();
  const gateReasons = new Map<string, string>();
  const accessDecisions = new Map<string, boolean>();
  const askDecisions = new Map<string, boolean>();
  const cancelReasons = new Map<string, string>();
  /** Each grant's branch and repository, for the rows that only name it. */
  const grantsById = new Map<string, { branch: string; repository: string; of: "push" | "google" }>();
  // A native runtime's question tool call (Claude's AskUserQuestion): its
  // questions row already says everything, so the tool row is not repeated.
  const askToolCalls = new Set<string>();
  /** Other conversations' messages, by the queued event that carried each. */
  const messageNotes = new Map<string, { id: string; from: string; name: string; text: string | null }>();
  /** The messages sent from here that a turn of the receiver's took up, by id. */
  const takenUp = new Set<string>();
  /** The notes that landed here which a turn read, and those a newer one
   *  replaced, by the fact each landed in. */
  const readNotes = new Set<string>();
  const replacedNotes = new Set<string>();
  /** Each native compaction's size from the session's own record, by its id:
   *  null when the record holds none. */
  const compactionSizes = new Map<string, number | null>();
  /** Each chef thread's newest ready-to-merge card, until its merge. */
  const mergeCards = new Map<string, MergeCard>();
  for (const ev of events) {
    if (ev.family === "network.host.decided" || ev.family === "file.read.decided") {
      const allow = payloadOf(ev).allow;
      const cited = ev.causality?.[0];
      if (cited && typeof allow === "boolean") askDecisions.set(cited, allow);
      continue;
    }
    if (ev.family === "agent.follow_up.queued") {
      const m = payloadOf(ev).message;
      if (m && typeof m.id === "string" && typeof m.from === "string") {
        messageNotes.set(ev.event_hash, {
          id: m.id,
          from: m.from,
          name: typeof m.name === "string" && m.name ? m.name : m.from,
          // The sender's own words; a node from before it says them only
          // inside its wrapper.
          text: typeof m.text === "string" ? m.text : null,
        });
      }
      continue;
    }
    if (ev.family === "message.taken_up") {
      const id = payloadOf(ev).id;
      if (typeof id === "string") takenUp.add(id);
      continue;
    }
    if (ev.family === "message.read" || ev.family === "message.replaced") {
      for (const cited of ev.causality ?? []) (ev.family === "message.read" ? readNotes : replacedNotes).add(cited);
      continue;
    }
    if (ev.family === "agent.context.compaction_size") {
      const p = payloadOf(ev);
      if (typeof p.compaction_id === "string") compactionSizes.set(p.compaction_id, counted(p.tokens));
      continue;
    }
    if (ev.family === "agent.ask.pending") {
      const id = payloadOf(ev).tool_call_id;
      if (typeof id === "string" && id) askToolCalls.add(id);
      continue;
    }
    if (!DECISION_FAMILIES.has(ev.family)) continue;
    const p = payloadOf(ev);
    const cited = p.decided_event ?? ev.causality?.[0];
    if (typeof cited === "string" && cited) {
      if (ev.family === "agent.child_tool.decided") {
        const d = p.decision;
        if (d === "allow_once" || d === "allow_always" || d === "deny") gateDecisions.set(cited, d);
        if (typeof p.reason === "string") gateReasons.set(cited, p.reason);
        if (ev.actor) decidedBy.set(cited, ev.actor);
        continue;
      }
      if (ev.family === "agent.machine_access.decided") {
        accessDecisions.set(cited, p.allow === true);
        if (ev.actor) decidedBy.set(cited, ev.actor);
        continue;
      }
      if (ev.family === "agent.ask.answered") {
        if (typeof p.answer === "string") answers.set(cited, p.answer);
        if (Array.isArray(p.answers)) structuredAnswers.set(cited, p.answers.map(askAnswer));
        cancelled.delete(cited);
        continue;
      }
      if (ev.family === "agent.approval.cancelled") {
        // A stopped file approval is operationally a closed/denied gate,
        // but keep the richer marker for the view. The next (malformed or
        // migrated) terminal decision may supersede it in stream order.
        cancelled.add(cited);
        if (typeof p.reason === "string" && p.reason) cancelReasons.set(cited, p.reason);
        verdicts.set(cited, "rejected");
        lastFailures.delete(cited);
        if (ev.actor) decidedBy.set(cited, ev.actor);
        continue;
      }
      if (ev.family === "agent.edit.failed") {
        // Latest attempt wins; the proposal's verdict is untouched.
        lastFailures.set(cited, typeof p.reason === "string" ? p.reason : "the decision did not land");
      } else {
        cancelled.delete(cited);
        verdicts.set(cited, ev.family === "agent.edit.accepted" ? "accepted" : "rejected");
        // A landed decision retires any earlier failed attempt — the retry
        // worked, so the old reason is history, not a live warning.
        lastFailures.delete(cited);
        if (ev.actor) decidedBy.set(cited, ev.actor);
        if (ev.family === "agent.edit.accepted" && typeof p.new_content === "string") {
          acceptedContent.set(cited, p.new_content);
        }
      }
    }
  }

  // Pass 1b: collect this turn's `agent.turn.usage` companion, keyed by
  // `turn_id` — every per-turn Heart event (agent.turn.started/.done/.usage/…)
  // carries the same `turn_id`, minted once in routes/agent.rs's
  // agent_turn_inner. `agent.turn.usage` is no longer in BOOKKEEPING
  // (07d-A3D1 Phase D) — it needs this per-event read to populate the map —
  // but it must still never become its own visible row: the main loop below
  // skips it unconditionally, exactly like DECISION_FAMILIES folds a
  // decision into its proposal instead of leaving a second row for it.
  const usageByTurn = new Map<string, TurnUsage>();
  for (const ev of events) {
    if (ev.family !== "agent.turn.usage") continue;
    const p = payloadOf(ev);
    const turnId = typeof p.turn_id === "string" ? p.turn_id : null;
    if (!turnId) continue;
    usageByTurn.set(turnId, {
      provider: typeof p.provider === "string" ? p.provider : undefined,
      modelId: typeof p.model_id === "string" ? p.model_id : undefined,
      effort: typeof p.effort === "string" ? p.effort : undefined,
      tokensIn: typeof p.tokens_in === "number" ? p.tokens_in : undefined,
      tokensOut: typeof p.tokens_out === "number" ? p.tokens_out : undefined,
      contextUsed: typeof p.context_used === "number" ? p.context_used : undefined,
      contextWindow: typeof p.context_window === "number" ? p.context_window : undefined,
      thoughtTokens: typeof p.thought_tokens === "number" ? p.thought_tokens : undefined,
      cacheReadTokens: typeof p.cache_read_tokens === "number" ? p.cache_read_tokens : undefined,
      cacheWriteTokens: typeof p.cache_write_tokens === "number" ? p.cache_write_tokens : undefined,
      cost:
        typeof p.cost?.amount === "number" && typeof p.cost?.currency === "string"
          ? { amount: p.cost.amount, currency: p.cost.currency }
          : undefined,
      durationMs: typeof p.duration_ms === "number" ? p.duration_ms : undefined,
    });
  }

  // Turns that reached a terminal fact: a permission request or native tool
  // call from one of these can no longer be answered or finish. Those Stop
  // ended are kept apart: a call they cut short was stopped, not broken.
  const endedTurns = new Set<string>();
  const stoppedTurns = new Set<string>();
  const segmentedTurns = new Set<string>();
  for (const ev of events) {
    if (ev.family === "agent.acp.text") {
      const turnId = payloadOf(ev).turn_id;
      if (typeof turnId === "string" && turnId) segmentedTurns.add(turnId);
      continue;
    }
    if (
      ev.family !== "agent.turn.done" &&
      ev.family !== "agent.turn.failed" &&
      ev.family !== "agent.turn.cancelled" &&
      ev.family !== "agent.turn.exhausted"
    ) {
      continue;
    }
    const turnId = payloadOf(ev).turn_id;
    if (typeof turnId === "string" && turnId) endedTurns.add(turnId);
    if (typeof turnId === "string" && turnId && ev.family === "agent.turn.cancelled") stoppedTurns.add(turnId);
  }
  const turnEnded = (p: Record<string, any>) => typeof p.turn_id === "string" && endedTurns.has(p.turn_id);
  const turnStopped = (p: Record<string, any>) => typeof p.turn_id === "string" && stoppedTurns.has(p.turn_id);

  // Each upload as recorded, for the messages that name it.
  const uploads = new Map<string, Attached>();
  for (const ev of events) {
    if (ev.family !== "agent.attachment.uploaded") continue;
    const p = payloadOf(ev);
    if (typeof p.id !== "string") continue;
    uploads.set(p.id, {
      id: p.id,
      filename: typeof p.filename === "string" ? p.filename : p.id,
      mediaType: typeof p.media_type === "string" ? p.media_type : "",
      bytes: typeof p.bytes === "number" ? p.bytes : 0,
    });
  }
  // The operator's answer to a session that couldn't resume, by its question,
  // and each agent's newest session fact (a question, an answer, a session
  // opened): the node takes an answer only to a question that is still newest.
  const chosen = new Map<string, "retry" | "fresh">();
  const newestSession = new Map<string, string>();
  for (const ev of events) {
    if (!["agent.session.not_resumed", "agent.session.chosen", "agent.session.opened"].includes(ev.family)) continue;
    const p = payloadOf(ev);
    if (typeof p.agent_id === "string") newestSession.set(p.agent_id, ev.event_hash);
    if (ev.family === "agent.session.chosen" && (p.choice === "retry" || p.choice === "fresh")) chosen.set(ev.causality?.[0] ?? "", p.choice);
  }
  // Each waiting message, by its queued event, for a failure that names it.
  const queued = new Map<string, Record<string, any>>();
  for (const ev of events) {
    if (ev.family === "agent.follow_up.queued" || ev.family === "agent.steer.queued") queued.set(ev.event_hash, payloadOf(ev));
  }
  /** A message's attachments, by the ids it was sent with. */
  const attachedTo = (p: Record<string, any>): { attachments?: Attached[] } => {
    const ids: unknown[] = Array.isArray(p.attachment_ids) ? p.attachment_ids : [];
    const list = ids.filter((id): id is string => typeof id === "string").map((id) => uploads.get(id) ?? { id, filename: id, mediaType: "", bytes: 0 });
    return list.length ? { attachments: list } : {};
  };

  const items: TranscriptItem[] = [];
  const acpRows = new Map<string, Extract<TranscriptItem, { kind: "acp_tool" }>>();
  for (const ev of events) {
    if (BOOKKEEPING.has(ev.family) && !decidedSwitch(ev)) continue;
    if (DECISION_FAMILIES.has(ev.family)) continue;
    // Never its own row — folded into the matching "done" item via
    // usageByTurn (see the pre-pass above).
    if (ev.family === "agent.turn.usage") continue;

    const p = payloadOf(ev);
    const at = ev.ts;
    const hash = ev.event_hash;

    switch (ev.family) {
      case "project.onboarded":
        items.push({
          kind: "session",
          at,
          hash,
          project: p.project ?? "",
          branch: p.branch ?? "",
          name: p.name ?? "",
          forkNote: typeof p.fork_note === "string" ? p.fork_note : "",
        });
        break;
      // A task's thread gets its folder here, and says why when it had to
      // fork the local main. A chat moving into a project writes it too,
      // and shows nothing.
      case "socket.workspace.set":
        if (typeof p.fork_note === "string" && p.fork_note) {
          items.push({ kind: "session", at, hash, project: "", branch: typeof p.branch === "string" ? p.branch : "", name: "", forkNote: p.fork_note });
        }
        break;
      case "agent.turn.started": {
        // A turn another conversation's message started: that message.
        const note = messageNotes.get(ev.causality?.[0] ?? "");
        if (note) {
          const text = note.text ?? messageText(typeof p.instruction === "string" ? p.instruction : "");
          items.push({ kind: "message", at, hash, dir: "in", id: note.id, peer: note.from, peerName: note.name, text });
          break;
        }
        items.push({
          kind: "human",
          at,
          hash,
          text: p.instruction ?? "",
          filePath: typeof p.file_path === "string" ? p.file_path : null,
          ...attachedTo(p),
          ...(ev.actor && ev.actor !== "human" ? { by: ev.actor } : {}),
        });
        break;
      }
      case "message.sent": {
        const id = typeof p.id === "string" ? p.id : "";
        const to = typeof p.to === "string" ? p.to : "";
        items.push({
          kind: "message",
          at,
          hash,
          dir: "out",
          id,
          peer: to,
          peerName: typeof p.to_name === "string" && p.to_name ? p.to_name : to,
          text: typeof p.text === "string" ? p.text : "",
          takenUp: takenUp.has(id),
          ...(p.note === true ? { note: true } : {}),
        });
        break;
      }
      case "message.received": {
        const m = p.message && typeof p.message === "object" ? p.message : {};
        const from = typeof m.from === "string" ? m.from : "";
        const read = readNotes.has(hash);
        items.push({
          kind: "message",
          at,
          hash,
          dir: "in",
          id: typeof m.id === "string" ? m.id : "",
          peer: from,
          peerName: typeof m.name === "string" && m.name ? m.name : from,
          text: typeof m.text === "string" ? m.text : "",
          note: true,
          read,
          ...(!read && replacedNotes.has(hash) ? { replaced: true } : {}),
        });
        break;
      }
      // Folded into its message, or the notes it read, above.
      case "message.taken_up":
      case "message.read":
        break;
      case "merge.waiting": {
        const thread = typeof p.stream_id === "string" ? p.stream_id : "";
        const prior = mergeCards.get(thread);
        if (prior) prior.replaced = true;
        const card: MergeCard = {
          kind: "merge-waiting",
          at,
          hash,
          thread,
          name: typeof p.name === "string" ? p.name : "",
          branch: typeof p.branch === "string" ? p.branch : "",
          base: typeof p.base === "string" ? p.base : "",
          commits: (Array.isArray(p.commits) ? p.commits : [])
            .filter((c: { sha?: unknown }) => c && typeof c.sha === "string")
            .map((c: { sha: string; subject?: unknown }) => ({ sha: c.sha, subject: typeof c.subject === "string" ? c.subject : "" })),
          files: strings(p.files),
          adds: typeof p.adds === "number" ? p.adds : 0,
          dels: typeof p.dels === "number" ? p.dels : 0,
          spawner: typeof p.spawner === "string" && p.spawner ? p.spawner : undefined,
          spawnerName: typeof p.spawner_name === "string" && p.spawner_name ? p.spawner_name : undefined,
          own: ev.stream_id === thread,
        };
        items.push(card);
        mergeCards.set(thread, card);
        break;
      }
      case "merge.done": {
        const thread = typeof p.thread === "string" ? p.thread : "";
        const merged = {
          into: typeof p.into === "string" ? p.into : "",
          commit: typeof p.commit === "string" ? p.commit : "",
          uncommitted: typeof p.uncommitted === "string" && p.uncommitted !== "",
        };
        const card = mergeCards.get(thread);
        if (card) {
          card.merged = merged;
          mergeCards.delete(thread);
        } else items.push({ kind: "merged", at, hash, branch: typeof p.branch === "string" ? p.branch : "", ...merged });
        break;
      }
      case "backlog.added":
        items.push({ kind: "backlog-added", at, hash, title: typeof p.title === "string" ? p.title : "" });
        break;
      case "brief.set":
        items.push({
          kind: "brief",
          at,
          hash,
          by: ev.actor ?? "human",
          what: p.recommended === true ? "set" : typeof p.text === "string" && p.text.trim() === "" ? "cleared" : "changed",
        });
        break;
      case "brief.decided":
        items.push({ kind: "brief", at, hash, by: ev.actor ?? "human", what: p.keep === true ? "kept" : "turned down" });
        break;
      case "merge.overlap":
        items.push({ kind: "merge-overlap", at, hash, waiting: typeof p.waiting === "string" ? p.waiting : "", files: strings(p.files) });
        break;
      case "terminal.prepared":
        items.push({
          kind: "prepared",
          at,
          hash,
          by: ev.actor ?? "",
          commands: (Array.isArray(p.commands) ? p.commands : [])
            .filter((c: { terminal_id?: unknown }) => c && typeof c.terminal_id === "string")
            .map((c: { terminal_id: string; name?: unknown; command?: unknown }) => ({
              terminalId: c.terminal_id,
              name: typeof c.name === "string" ? c.name : "",
              command: typeof c.command === "string" ? c.command : "",
            })),
        });
        break;
      case "agent.thought":
        items.push({
          kind: "thought",
          at,
          hash,
          ms: typeof p.ms === "number" ? p.ms : 0,
          text: typeof p.text === "string" ? p.text : "",
          cut: p.cut === true,
          shared: p.shared !== false && typeof p.text === "string" && p.text !== "",
          modelId: typeof p.model_id === "string" ? p.model_id : "",
        });
        break;
      case "agent.session.not_resumed":
        items.push({
          kind: "not-resumed",
          at,
          hash,
          reason: typeof p.reason === "string" ? p.reason : "",
          awaiting: p.awaiting === true,
          ...(chosen.has(hash) ? { chosen: chosen.get(hash) } : {}),
          ...(p.awaiting === true && !chosen.has(hash) && newestSession.get(p.agent_id) !== hash ? { superseded: true } : {}),
        });
        break;
      case "agent.context.compacted": {
        // A native one's size is beside it once the session's record is
        // read (`agent.context.compaction_size`); an API runtime's is on it.
        const sized = typeof p.compaction_id === "string" ? compactionSizes.get(p.compaction_id) : undefined;
        const tokens = counted(p.reported_input_tokens) ?? sized ?? null;
        items.push({
          kind: "compaction",
          at,
          hash,
          trigger: typeof p.trigger === "string" && p.trigger ? p.trigger : null,
          tokens,
          ...(tokens === null && p.native === true && sized === undefined && !turnEnded(p) ? { pending: true } : {}),
          summary: typeof p.summary === "string" ? p.summary : "",
          ...(p.native === true ? { native: true } : {}),
          ...(p.native === true && typeof p.status === "string" && p.status !== "completed"
            ? { failed: { status: p.status, error: typeof p.error === "string" ? p.error : "" } }
            : {}),
        });
        break;
      }
      case "agent.queue.failed": {
        const message = queued.get(ev.causality?.[0] ?? "") ?? {};
        items.push({
          kind: "queue-failed",
          at,
          hash,
          text: typeof message.instruction === "string" ? message.instruction : "",
          reason: typeof p.reason === "string" && p.reason ? p.reason : typeof p.status === "string" ? p.status : "",
          ...attachedTo(message),
        });
        break;
      }
      case "agent.steer.applied":
        items.push({ kind: "human", at, hash, text: p.instruction ?? "", filePath: null, steer: true, ...attachedTo(p) });
        break;
      case "agent.acp.subagent_text":
        items.push({
          kind: "segment",
          at,
          hash,
          text: typeof p.text === "string" ? p.text : "",
          ...(typeof p.parent_tool_call_id === "string" ? { parentId: p.parent_tool_call_id } : {}),
        });
        break;
      case "agent.tool.called":
        items.push({
          kind: "tool",
          at,
          hash,
          label: toolLabel(p.tool ?? "tool", p.args ?? {}),
          ok: p.ok !== false,
          summary: p.summary ?? "",
          args: JSON.stringify(p.args ?? {}, null, 2),
          toolName: typeof p.tool === "string" ? p.tool : "tool",
          ...(typeof p.call_id === "string" ? { callId: p.call_id } : {}),
          ...(p.transport === "mcp" ? { viaMcp: true } : {}),
          ...(p.stopped === "before" ? { stopped: "before" as const } : {}),
        });
        break;
      case "agent.acp.tool_call": {
        const detail = p.detail && typeof p.detail === "object" ? p.detail : {};
        const id = typeof detail.toolCallId === "string" && detail.toolCallId ? detail.toolCallId : hash;
        if (askToolCalls.has(id)) break;
        let row = acpRows.get(id);
        if (!row) {
          row = {
            kind: "acp_tool",
            at,
            hash,
            toolCallId: id,
            title: "",
            toolKind: null,
            status: null,
            rawInput: undefined,
            rawOutput: undefined,
            content: [],
            locations: [],
            updates: [],
            fullRefs: [],
          };
          acpRows.set(id, row);
          items.push(row);
        }
        foldAcpDetail(row, detail);
        const full = p.full && typeof p.full.sha === "string" ? { sha: p.full.sha as string, bytes: typeof p.full.bytes === "number" ? p.full.bytes : 0 } : undefined;
        row.updates.push({ event: typeof p.event === "string" ? p.event : "update", at, detail, ...(full ? { full } : {}) });
        // What Stop did to it: never ran is final; started holds unless it
        // turned out it never ran.
        if (p.event === "stopped_before") row.stopped = "before";
        else if (p.event === "stopped_after" && row.stopped !== "before") row.stopped = "after";
        // A command that exited finished, whatever its turn did after.
        row.unfinished = turnEnded(p) && row.status !== "completed" && row.status !== "failed" && (row.terminal?.exitCode ?? null) === null;
        // A call still open when Stop ended its turn, from a node that did not
        // say how far it got: stopped all the same.
        if (row.unfinished && !row.stopped && turnStopped(p)) row.stopped = "turn";
        if (full) row.fullRefs.push(full);
        break;
      }
      case "network.host.ask": {
        const ask = { hash, host: typeof p.host === "string" ? p.host : "", program: typeof p.program === "string" && p.program ? p.program : null, allowed: askDecisions.get(hash) };
        const last = items.at(-1);
        // A program reaching several hosts at once asks for them together.
        if (last?.kind === "hosts" && Date.parse(at) - Date.parse(last.at) <= HOSTS_TOGETHER_MS) last.asks.push(ask);
        else items.push({ kind: "hosts", at, hash, projectId: typeof p.project_id === "string" ? p.project_id : "", asks: [ask] });
        break;
      }
      case "file.read.ask":
        items.push({
          kind: "file-ask",
          at,
          hash,
          projectId: typeof p.project_id === "string" ? p.project_id : "",
          path: typeof p.path === "string" ? p.path : "",
          what: typeof p.kind === "string" ? p.kind : "unknown",
          bytes: typeof p.bytes === "number" ? p.bytes : 0,
          program: typeof p.program === "string" && p.program ? p.program : null,
          allowed: askDecisions.get(hash),
        });
        break;
      // Folded into its ask's card above.
      case "network.host.decided":
      case "file.read.decided":
        break;
      case "git.grant.given":
      case "git.grant.used":
      case "git.grant.ended": {
        const id = typeof p.grant_id === "string" ? p.grant_id : "";
        if (ev.family === "git.grant.given" && id) {
          grantsById.set(id, {
            branch: typeof p.branch === "string" ? p.branch : "",
            repository: typeof p.repository === "string" ? p.repository : "",
            of: p.kind === "google" ? "google" : "push",
          });
        }
        const known = grantsById.get(id);
        items.push({
          kind: "grant",
          at,
          hash,
          what: ev.family === "git.grant.given" ? "given" : ev.family === "git.grant.used" ? "used" : "ended",
          of: p.kind === "google" || p.kind === "push" ? p.kind : (known?.of ?? "push"),
          branch: typeof p.branch === "string" ? p.branch : (known?.branch ?? ""),
          repository: typeof p.repository === "string" ? p.repository : (known?.repository ?? ""),
          why: typeof p.why === "string" ? p.why : "",
        });
        break;
      }
      case "agent.machine_access.requested":
        items.push({
          kind: "access",
          at,
          hash,
          path: typeof p.path === "string" ? p.path : "",
          access: typeof p.access === "string" ? p.access : "read",
          reason: typeof p.reason === "string" ? p.reason : "",
          allowed: accessDecisions.get(hash),
          decidedBy: decidedBy.get(hash),
        });
        break;
      case "agent.child_tool.pending":
        items.push({
          kind: "gate",
          at,
          hash,
          title: typeof p.title === "string" ? p.title : "",
          toolKind: typeof p.kind === "string" ? p.kind : "other",
          plug: typeof p.plug === "string" ? p.plug : "",
          decision: gateDecisions.get(hash),
          decidedBy: decidedBy.get(hash),
          reason: gateReasons.get(hash),
          cancelled: cancelled.has(hash),
          expired: !gateDecisions.has(hash) && !cancelled.has(hash) && turnEnded(p),
          ...(typeof p.always?.scope === "string"
            ? { always: { scope: p.always.scope, name: typeof p.always.name === "string" ? p.always.name : null } }
            : {}),
          ...(typeof p.bakery_tool?.name === "string"
            ? { bakeryTool: { name: p.bakery_tool.name, risk: String(p.bakery_tool.risk ?? "") } }
            : {}),
          ...((p.grant?.kind === "push" || p.grant?.kind === "google") && typeof p.grant.branch === "string"
            ? { grant: { kind: p.grant.kind, repository: typeof p.grant.repository === "string" ? p.grant.repository : "", branch: p.grant.branch } }
            : {}),
          ...(p.bakery_tool?.name === "google_change" && p.bakery_tool.args && typeof p.bakery_tool.args === "object"
            ? {
                google: {
                  method: String(p.bakery_tool.args.method ?? ""),
                  url: String(p.bakery_tool.args.url ?? ""),
                  sends: String(p.bakery_tool.args.sends ?? ""),
                },
              }
            : {}),
          ...(cancelReasons.has(hash) ? { cancelReason: cancelReasons.get(hash) } : {}),
          ...(p.push && typeof p.push === "object" && typeof p.push.head === "string"
            ? {
                push: {
                  branch: typeof p.push.branch === "string" ? p.push.branch : "",
                  head: p.push.head,
                  remoteHead: typeof p.push.remote_head === "string" ? p.push.remote_head : null,
                  base: typeof p.push.base === "string" ? p.push.base : "",
                  commits: Array.isArray(p.push.commits)
                    ? p.push.commits.flatMap((c: unknown) =>
                        c && typeof c === "object" && typeof (c as { sha?: unknown }).sha === "string"
                          ? [{ sha: (c as { sha: string }).sha, subject: String((c as { subject?: unknown }).subject ?? "") }]
                          : [],
                      )
                    : [],
                  diff: typeof p.push.diff === "string" ? p.push.diff : "",
                  diffTruncated: p.push.diff_truncated === true,
                  secret: p.push.secret === true,
                },
              }
            : {}),
          diffs: Array.isArray(p.diffs)
            ? p.diffs
                .filter((d: unknown): d is Record<string, unknown> => !!d && typeof d === "object")
                .map((d) => ({
                  path: typeof d.path === "string" ? d.path : "",
                  oldText: typeof d.old_text === "string" ? d.old_text : "",
                  newText: typeof d.new_text === "string" ? d.new_text : "",
                }))
            : [],
        });
        break;
      case "agent.file.changed": {
        // A decision event wins; otherwise fall back to the proposal's own
        // status, and only for a value that is actually a terminal verdict —
        // anything else (including a stray "failed") is still open.
        const own: Verdict =
          p.status === "accepted" ? "accepted" : p.status === "rejected" ? "rejected" : "pending";
        const verdict: Verdict = verdicts.get(hash) ?? own;
        items.push({
          kind: "proposal",
          at,
          hash,
          filePath: p.file_path ?? "",
          ...(p.native_review?.tool_name === "NotebookEdit" ? { part: "cell" as const } : p.native_review?.old_full ? { part: "change" as const } : {}),
          rationale: p.rationale ?? "",
          oldContent: p.old_content ?? "",
          newContent: acceptedContent.get(hash) ?? p.new_content ?? "",
          // A proposal the node already recorded as decided stays decided even
          // if its decision event is missing from this slice of the stream.
          verdict,
          // Only meaningful while still open — see Verdict.
          lastFailure: verdict === "pending" ? lastFailures.get(hash) : undefined,
          decidedBy: decidedBy.get(hash),
          cancelled: cancelled.has(hash),
          agentId: typeof p.agent_id === "string" ? p.agent_id : undefined,
          modelId: typeof p.model_id === "string" ? p.model_id : undefined,
          landed:
            p.landed && typeof p.landed === "object"
              ? {
                  turnId: typeof p.landed.turn_id === "string" ? p.landed.turn_id : "",
                  restorable: p.landed.restorable === true && !operatorEdited(p.landed),
                  binary: p.landed.binary === true,
                  hidden: typeof p.landed.hidden === "string" ? p.landed.hidden : undefined,
                  operatorEdit: operatorEdited(p.landed),
                }
              : undefined,
        });
        break;
      }
      case "agent.turn.done": {
        const turnId = typeof p.turn_id === "string" ? p.turn_id : null;
        items.push({
          kind: "done",
          at,
          hash,
          summary: p.summary ?? "",
          actor: ev.actor,
          usage: turnId ? usageByTurn.get(turnId) : undefined,
          segmented: turnId !== null && segmentedTurns.has(turnId),
        });
        break;
      }
      case "agent.acp.text":
        items.push({ kind: "segment", at, hash, text: typeof p.text === "string" ? p.text : "" });
        break;
      case "agent.turn.retrying": {
        const turnId = typeof p.turn_id === "string" ? p.turn_id : "";
        const reason = typeof p.reason === "string" ? p.reason : "";
        const attempt = typeof p.attempt === "number" ? p.attempt : 1;
        const last = items.at(-1);
        // Tries in a row fold into one line that keeps the latest.
        if (last?.kind === "retrying" && last.turnId === turnId) items[items.length - 1] = { ...last, at, reason, attempt };
        else items.push({ kind: "retrying", at, hash, turnId, reason, attempt, waiting: true });
        break;
      }
      case "agent.turn.exhausted":
        items.push({
          kind: "exhausted",
          at,
          hash,
          reason: p.reason ?? "step limit reached",
          steps: typeof p.steps === "number" ? p.steps : 0,
        });
        break;
      case "agent.turn.failed":
        items.push({
          kind: "failed",
          at,
          hash,
          reason: p.reason ?? "the turn failed",
          phase: p.phase === "jail" || p.phase === "model" || p.phase === "tools" ? p.phase : "model",
        });
        break;
      case "agent.turn.cancelled":
        items.push({ kind: "cancelled", at, hash, reason: p.reason ?? "cancelled by human" });
        break;
      case "agent.native_config.acknowledged":
        items.push({ kind: "setting", at, hash, text: settingText(p) });
        break;
      case "seat.model.assigned":
        items.push({
          kind: "model_switch",
          at,
          hash,
          seat: typeof p.seat === "string" ? p.seat : "",
          from: typeof p.from === "string" ? p.from : null,
          to: typeof p.model_id === "string" ? p.model_id : "",
          reason: p.reason,
          because: ev.causality?.[0] ?? null,
          by: ev.actor,
        });
        break;
      case "agent.ask.pending": {
        const questions = Array.isArray(p.questions) ? p.questions.map(askQuestion) : [];
        items.push({
          kind: "ask",
          at,
          hash,
          questions,
          answer: answers.get(hash),
          answers: structuredAnswers.get(hash),
          cancelled: cancelled.has(hash),
        });
        break;
      }
      default:
        items.push({
          kind: "note",
          at,
          hash,
          family: ev.family,
          text: typeof p.summary === "string" ? p.summary : "",
          raw: JSON.stringify(p, null, 2),
        });
    }
  }
  // Waiting is over for any stretch something came after.
  for (let i = 0; i < items.length - 1; i++) {
    const it = items[i];
    if (it.kind === "retrying" && it.waiting) items[i] = { ...it, waiting: false };
  }
  return nestSubagents(withoutMcpDuplicates(items));
}

type AcpToolItem = Extract<TranscriptItem, { kind: "acp_tool" }>;

/** A subagent's steps and words move under the call that launched it, the
 *  way Claude Code shows an agent at work; the parent's own reply, no longer
 *  cut by them, reads as one block again. A call another call names as its
 *  parent launched a subagent even when the runtime did not mark it one. */
function nestSubagents(items: TranscriptItem[]): TranscriptItem[] {
  const parentOf = (item: TranscriptItem) =>
    item.kind === "acp_tool" || item.kind === "segment" ? item.parentId : undefined;
  const calls = new Map<string, AcpToolItem>();
  for (const item of items) if (item.kind === "acp_tool") calls.set(item.toolCallId, item);
  if (!items.some((item) => parentOf(item) || (item.kind === "acp_tool" && item.subagent))) return items;
  for (const item of items) {
    const parent = calls.get(parentOf(item) ?? "");
    if (parent) parent.subagent ??= { description: "", type: null };
  }
  for (const call of calls.values()) {
    if (!call.subagent) continue;
    const input = call.rawInput && typeof call.rawInput === "object" ? (call.rawInput as Record<string, unknown>) : {};
    const description = typeof input.description === "string" && input.description.trim() ? input.description.trim() : "";
    call.subagent = {
      description: description || call.title || "Subagent",
      type: typeof input.subagent_type === "string" && input.subagent_type ? input.subagent_type : null,
    };
  }
  const top: TranscriptItem[] = [];
  for (const item of items) {
    const parent = calls.get(parentOf(item) ?? "");
    if (parent && parent !== item) {
      (parent.children ??= []).push(item);
      continue;
    }
    // Two pieces of the reply that only a subagent's steps had separated.
    const last = top[top.length - 1];
    if (item.kind === "segment" && last?.kind === "segment") {
      top[top.length - 1] = { ...last, text: last.text + item.text };
      continue;
    }
    top.push(item);
  }
  return top;
}

/** Whether a native runtime's tool-call title names `tool`: the title read
 *  as words and `__` / `.` / `:` / `/` parts (`mcp__bakery__thread_list`,
 *  `bakery.thread_list`, "Calling thread_list"), never as a substring, so
 *  `list` is not `thread_list`. */
export function titleNamesTool(title: string, tool: string): boolean {
  const wanted = tool.toLowerCase();
  return (
    wanted !== "" &&
    title
      .toLowerCase()
      .split(/[\s.:/]+/)
      .flatMap((word) => word.split("__"))
      .includes(wanted)
  );
}

type AcpTool = Extract<TranscriptItem, { kind: "acp_tool" }>;

/** How a call stands. Stop is no failure: a call it cut short never ran
 *  (`stopped-before`), or had started (`stopped-after`); `stopped` is one a
 *  node from before said neither of. */
export type CallStanding = "running" | "done" | "failed" | "unfinished" | "stopped-before" | "stopped-after" | "stopped";

/** How a native call stands: the runtime's own word that it finished first,
 *  then what Stop did to it (its teardown's failure says nothing more), a
 *  failure, a command's exit code, and a turn that ended with none of them. */
export function callStanding(item: Pick<AcpTool, "status" | "stopped" | "unfinished" | "terminal">): CallStanding {
  const exit = item.terminal?.exitCode ?? null;
  if (item.status === "completed") return "done";
  if (item.stopped === "before") return "stopped-before";
  if (item.stopped === "after") return "stopped-after";
  if (item.status === "failed") return "failed";
  if (exit !== null) return exit === 0 ? "done" : "failed";
  if (item.stopped === "turn") return "stopped";
  return item.unfinished ? "unfinished" : "running";
}

/** One update of a native tool call folded into its row. An update carries
 *  only the fields that changed: the newest value of each wins, except that
 *  a finished call stays finished (no later update takes back completed or
 *  failed). A command run in the runtime's own terminal sends its output in
 *  order across updates, and its exit code on the last. */
export function foldAcpDetail(row: AcpTool, detail: Record<string, any>): void {
  if (typeof detail.title === "string" && detail.title) row.title = detail.title;
  if (typeof detail.kind === "string") row.toolKind = detail.kind;
  if (typeof detail.status === "string" && row.status !== "completed" && row.status !== "failed") row.status = detail.status;
  if ("rawInput" in detail) row.rawInput = detail.rawInput;
  if ("rawOutput" in detail) row.rawOutput = detail.rawOutput;
  if (Array.isArray(detail.content)) row.content = detail.content;
  if (Array.isArray(detail.locations)) row.locations = detail.locations;
  const meta = detail._meta && typeof detail._meta === "object" ? detail._meta : {};
  // Claude's adapter names the subagent a call belongs to, and marks the
  // call that launches one (Agent/Task).
  const claude = meta.claudeCode && typeof meta.claudeCode === "object" ? meta.claudeCode : {};
  if (!row.parentId && typeof claude.parentToolUseId === "string" && claude.parentToolUseId) {
    row.parentId = claude.parentToolUseId;
  }
  if (claude.toolName === "Agent" || claude.toolName === "Task" || claude.subagent === true) {
    row.subagent ??= { description: "", type: null };
  }
  const chunk = meta.terminal_output_delta ?? meta.terminal_output;
  if (meta.terminal_info || chunk || meta.terminal_exit) {
    row.terminal ??= { output: "", exitCode: null };
    if (typeof chunk?.data === "string") row.terminal.output += chunk.data;
    if (typeof meta.terminal_exit?.exit_code === "number") row.terminal.exitCode = meta.terminal_exit.exit_code;
  }
}

/** A native tool call with each cut update replaced by its whole payload
 *  (by `full.sha`, as GET /agent/tool-payload answers it), replayed in
 *  order: its input, output, content and terminal output whole. */
export function withWholePayloads(row: AcpTool, whole: ReadonlyMap<string, unknown>): AcpTool {
  const next: AcpTool = { ...row, title: "", toolKind: null, status: null, rawInput: undefined, rawOutput: undefined, content: [], locations: [], terminal: undefined };
  for (const u of row.updates) {
    const detail = u.full && whole.has(u.full.sha) ? whole.get(u.full.sha) : u.detail;
    foldAcpDetail(next, detail && typeof detail === "object" ? (detail as Record<string, any>) : {});
  }
  // A call already finished (by the runtime, or by its MCP record) stays so.
  if ((row.status === "completed" || row.status === "failed") && next.status !== "completed" && next.status !== "failed") next.status = row.status;
  return next;
}

/** Bakery's own MCP server records every call a native runtime makes through
 *  it (`agent.tool.called`, `transport: "mcp"`), and the runtime reports the
 *  same call as a tool-call row of its own — so each call showed twice. The
 *  runtime's row carries the arguments and the output, and the MCP record
 *  stays in Heart for audit. One record cancels one runtime row naming the
 *  same tool, the nearest unclaimed one; a record no runtime row accounts
 *  for (a runtime that does not report MCP calls) stays visible. */
function withoutMcpDuplicates(items: TranscriptItem[]): TranscriptItem[] {
  if (!items.some((item) => item.kind === "tool" && item.viaMcp)) return items;
  const claimed = new Set<number>();
  const hidden = new Set<number>();
  /** A runtime row its MCP record finished: the record says the call ended,
   *  and how, where the runtime never reported it. */
  const finished = new Map<number, TranscriptItem>();
  items.forEach((item, at) => {
    if (item.kind !== "tool" || !item.viaMcp) return;
    let best = -1;
    items.forEach((other, i) => {
      if (other.kind !== "acp_tool" || claimed.has(i) || !titleNamesTool(other.title, item.toolName)) return;
      if (best < 0 || Math.abs(i - at) < Math.abs(best - at)) best = i;
    });
    if (best >= 0) {
      claimed.add(best);
      hidden.add(at);
      const row = items[best];
      if (row.kind === "acp_tool" && row.status !== "completed" && row.status !== "failed") {
        finished.set(best, { ...row, status: item.ok ? "completed" : "failed", unfinished: false });
      }
    }
  });
  if (hidden.size === 0) return items;
  return items.flatMap((item, i) => (hidden.has(i) ? [] : [finished.get(i) ?? item]));
}

/**
 * One configured agent's own thread on a stream that may carry other
 * agents' turns too (07d-A1 §2: "Filter history by `agent_id` for that
 * conversation" — never merge two agents' transcripts into one view). A
 * decision (`DECISION_FAMILIES`) has no `agent_id` of its own — it belongs
 * to whichever agent's proposal/pending gate it cites via causality, the
 * same link `toTranscript` already uses to fold it into that proposal's
 * verdict. Explicit socket-global bookkeeping (`SOCKET_GLOBAL_FAMILIES`)
 * stays visible to every agent. Everything else — every agent-scoped
 * family this client knows about, and any future one it doesn't — belongs
 * to no agent without an exact `agent_id` match on its own payload: fail
 * closed by default (07d-A1 correction), never leak by default. There is
 * no production legacy history to grandfather in.
 */
export function eventsForAgent(events: BakeryEvent[], agentId: string): BakeryEvent[] {
  const owned = new Set<string>();
  const out: BakeryEvent[] = [];
  for (const ev of events) {
    if (DECISION_FAMILIES.has(ev.family)) {
      if (ev.causality.some((h) => owned.has(h))) {
        out.push(ev);
        owned.add(ev.event_hash);
      }
      continue;
    }
    if (SOCKET_GLOBAL_FAMILIES.has(ev.family)) {
      out.push(ev);
      continue;
    }
    const p = payloadOf(ev);
    const evAgentId = typeof p.agent_id === "string" ? p.agent_id : null;
    if (evAgentId === agentId) {
      out.push(ev);
      owned.add(ev.event_hash);
    }
  }
  return out;
}

export type Progress = {
  turns: number;
  tools: number;
  toolFailures: number;
  pending: number;
  accepted: number;
  rejected: number;
  /** Still-open proposals whose last decision attempt misfired. A health
   *  signal, not a gate state: these are ALSO counted in `pending`, because
   *  that is what they are — the write never landed and can be retried. */
  failed: number;
  /** Whole turns that died before producing anything reviewable. */
  turnFailures: number;
  lastAt: string | null;
};

/** Quiet progress: counts a human can read, no hashes. */
export function summarize(items: TranscriptItem[]): Progress {
  const out: Progress = {
    turns: 0,
    tools: 0,
    toolFailures: 0,
    pending: 0,
    accepted: 0,
    rejected: 0,
    failed: 0,
    turnFailures: 0,
    lastAt: null,
  };
  for (const it of items) {
    out.lastAt = it.at || out.lastAt;
    if (startsTurn(it)) out.turns++;
    else if (it.kind === "tool") {
      out.tools++;
      // One Stop caught before it ran did not fail.
      if (!it.ok && !it.stopped) out.toolFailures++;
    } else if (it.kind === "proposal") {
      if (it.verdict === "pending") {
        out.pending++;
        // Overlaps `pending` on purpose — see Progress.failed.
        if (it.lastFailure) out.failed++;
      } else if (it.verdict === "accepted") out.accepted++;
      else out.rejected++;
    } else if (it.kind === "failed") {
      out.turnFailures++;
    }
  }
  return out;
}

/** The proposals still waiting on a human. */
export function pendingProposals(items: TranscriptItem[]) {
  return items.filter((i) => i.kind === "proposal" && i.verdict === "pending");
}
