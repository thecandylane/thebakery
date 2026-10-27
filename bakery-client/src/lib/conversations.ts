// Every conversation the operator can talk to — the Baker's main one, its
// threads, each project chat, each loose chat, each task — as one shape,
// straight off the one topology source (`/studio/state`), plus where each
// one lives in the app. The Baker is its own mode, never part of Chat. The
// same actions (open, stop, restart, archive, delete) apply to all of them;
// only the main Baker is never archived or deleted.
//
// Pure and DOM-free on purpose, so `conversations.check.ts` tests it with
// plain node.

import type { ConversationStatus, SocketInfo, StudioState } from "./node.ts";
import { toHandle } from "./handle.ts";
import { socketLabel } from "./sessionGroups.ts";

export type ConversationKind = "baker" | "project" | "task";

export type Conversation = {
  streamId: string;
  title: string;
  kind: ConversationKind;
  /** The permanent Baker's own conversation — never archived, renamed, or deleted. */
  main: boolean;
  /** One of the Baker's conversations (main or thread) — lives at `/baker…`. */
  bakerThread: boolean;
  /** On the Baker's side of the app — its conversations and its legacy
   *  jobs. Never listed in Chat. */
  baker: boolean;
  /** Where it belongs: "Baker", its project's name, or "" for a loose chat. */
  place: string;
  /** A chat with a folder of its own, which can be made a project. */
  chat: boolean;
  archived: boolean;
  busy: boolean;
  pendingGates: number;
  lastTs: string;
  /** The seat a message is sent to — the server's own default seat. */
  roleId: string;
  status: ConversationStatus;
  /** Why: what it waits on, or how its last turn ended. */
  note: string;
  /** A workspace's orc, beneath the Baker: its workers name it in `openedBy`. */
  workspace: boolean;
  /** The conversation that opened this one as its peer: a worker's orc. */
  openedBy: string | null;
};

/** Each status in words, most urgent first — the order every board uses. */
export const STATUSES: { status: ConversationStatus; label: string; short: string }[] = [
  { status: "needs_input", label: "Needs you", short: "needs you" },
  { status: "failed", label: "Failed", short: "failed" },
  { status: "working", label: "Working", short: "working" },
  { status: "done", label: "Done", short: "done" },
  { status: "idle", label: "Idle", short: "idle" },
];

export function statusLabel(status: ConversationStatus): string {
  return STATUSES.find((s) => s.status === status)?.short ?? status;
}

/** An older node sends no status: derive the little that is knowable. */
function fallbackStatus(s: SocketInfo, busy: boolean): ConversationStatus {
  if (s.pending_gates > 0) return "needs_input";
  return busy ? "working" : "idle";
}

/** A thread's name on screen. The Baker's main thread is "Baker" (the
 *  operator's glossary, 2026-10-09). */
export function threadTitle(s: SocketInfo): string {
  const thread = s.baker_thread ?? null;
  return thread?.main ? "Baker" : thread?.title || s.title || socketLabel(s);
}

function toConversation(s: SocketInfo, factory: boolean, projectName: string): Conversation | null {
  if (s.discarded) return null;
  const thread = s.baker_thread ?? null;
  const busy = s.agents.some((a) => a.busy);
  return {
    streamId: s.stream_id,
    title: threadTitle(s),
    kind: thread ? "baker" : s.state === "task" ? "task" : "project",
    main: thread?.main === true,
    bakerThread: thread !== null,
    baker: factory,
    place: thread || factory ? "Baker" : projectName,
    chat: s.chat ?? false,
    archived: s.archived ?? thread?.archived ?? false,
    busy,
    pendingGates: s.pending_gates,
    lastTs: s.last_ts,
    roleId: factory ? "baker" : "orchestrator",
    status: s.status ?? fallbackStatus(s, busy),
    note: s.status_note ?? "",
    workspace: s.workspace ?? false,
    openedBy: s.opened_by ?? null,
  };
}

/** Main Baker first, then most recently active first. Node timestamps are
 *  RFC 3339 in UTC, so they order lexically. */
function mainThenRecent(a: Conversation, b: Conversation): number {
  if (a.main !== b.main) return a.main ? -1 : 1;
  return a.lastTs < b.lastTs ? 1 : a.lastTs > b.lastTs ? -1 : 0;
}

/** Every live (not deleted) conversation, archived ones included. */
export function allConversations(state: StudioState | null): Conversation[] {
  if (!state) return [];
  const out: Conversation[] = [];
  for (const g of state.projects) {
    for (const s of g.sockets) {
      const c = toConversation(s, false, g.name);
      if (c) out.push(c);
    }
  }
  for (const s of state.ungrouped_sockets) {
    const c = toConversation(s, false, s.project);
    if (c) out.push(c);
  }
  for (const s of state.factory_sockets) {
    const c = toConversation(s, true, "");
    if (c) out.push(c);
  }
  return out.sort(mainThenRecent);
}

export function conversationFor(state: StudioState | null, streamId: string): Conversation | null {
  return allConversations(state).find((c) => c.streamId === streamId) ?? null;
}

function split(all: Conversation[]): { active: Conversation[]; archived: Conversation[] } {
  return { active: all.filter((c) => !c.archived), archived: all.filter((c) => c.archived) };
}

/** Chat's list: project chats and loose chats — never the Baker, and never
 *  tasks (work items, listed with their project and on the Floor). */
export function chats(state: StudioState | null): { active: Conversation[]; archived: Conversation[] } {
  return split(allConversations(state).filter((c) => !c.baker && c.kind !== "task"));
}

/** The Baker's own list: its main conversation, then its threads. */
export function bakerChats(state: StudioState | null): { active: Conversation[]; archived: Conversation[] } {
  return split(allConversations(state).filter((c) => c.bakerThread));
}

/** A conversation on its own, full width (never a Baker conversation). */
export function chatPath(c: { streamId: string }): string {
  return `/chat/${toHandle(c.streamId)}`;
}

/** A conversation's studio — chat beside files, review, terminal. A Baker
 *  conversation's is the Baker's own page. */
export function codePath(c: { streamId: string; main: boolean; bakerThread: boolean }): string {
  if (c.main) return "/baker";
  return c.bakerThread ? `/baker/${toHandle(c.streamId)}` : `/w/${toHandle(c.streamId)}`;
}

/** A conversation's studio with Review open — on one change when given. */
export function reviewPath(c: { streamId: string; main: boolean; bakerThread: boolean }, change?: string): string {
  return `${codePath(c)}?open=review${change ? `&change=${encodeURIComponent(change)}` : ""}`;
}

/** Where a conversation opens by default: the Baker's in the Baker, every
 *  other one in Chat. */
export function openPath(c: { streamId: string; main: boolean; bakerThread: boolean; baker: boolean }): string {
  return c.baker ? codePath(c) : chatPath(c);
}

export type ConversationActionId =
  | "chat"
  | "code"
  | "baker"
  | "rename"
  | "stop"
  | "restart"
  | "archive"
  | "restore"
  | "project"
  | "project-folder"
  | "delete";

export type ConversationAction = {
  id: ConversationActionId;
  label: string;
  /** Menus draw a separator between groups. */
  group: "open" | "run" | "keep";
  danger?: boolean;
  disabled: boolean;
  hint: string;
};

/** What can be done to `c` from the page at `here` — the one list every
 *  actions menu draws (`conversationActions.ts` runs them). The main Baker is
 *  never archived, renamed, or deleted; a task is closed from its own review.
 *  `retiring`: a delete for it is already in flight. */
export function conversationActions(c: Conversation, here: string, retiring = false): ConversationAction[] {
  const out: ConversationAction[] = [];
  if (c.bakerThread) {
    if (here !== codePath(c)) {
      out.push({ id: "baker", label: "Open in Baker", group: "open", disabled: false, hint: "Open this Baker thread" });
    }
  } else {
    if (!c.baker && here !== chatPath(c)) {
      out.push({ id: "chat", label: "Open on its own", group: "open", disabled: false, hint: "Open it on its own" });
    }
    if (here !== codePath(c)) {
      out.push({
        id: "code",
        label: "Open workspace",
        group: "open",
        disabled: false,
        hint: "Open beside its files, review, and terminal",
      });
    }
  }
  if (!c.main) {
    out.push({ id: "rename", label: "Rename…", group: "open", disabled: false, hint: "Rename this thread" });
  }
  if (c.busy) {
    out.push({ id: "stop", label: "Stop its turn", group: "run", disabled: false, hint: "Stop the turn that is running" });
  }
  out.push({
    id: "restart",
    label: "Restart runtime",
    group: "run",
    disabled: c.busy,
    hint: c.busy ? "Stop its turn first" : "Stop its runtime; its next message starts a fresh one",
  });
  if (!c.main) {
    out.push(
      c.archived
        ? { id: "restore", label: "Restore", group: "keep", disabled: false, hint: "Show it in the lists again" }
        : {
            id: "archive",
            label: c.status === "done" ? "Close — archive it" : "Archive",
            group: "keep",
            disabled: c.busy,
            hint: c.busy ? "Stop its turn first" : "Hide it — history kept, restore any time",
          },
    );
  }
  if (c.chat && !c.baker && c.kind !== "task") {
    const hint = c.busy ? "Stop its turn first" : "Its folder moves into a project as a Git repository; it carries on there";
    out.push(
      { id: "project", label: "Make this a project", group: "keep", disabled: c.busy, hint },
      { id: "project-folder", label: "Make this a project in a folder…", group: "keep", disabled: c.busy, hint },
    );
  }
  if (!c.main && c.kind !== "task") {
    out.push({
      id: "delete",
      label: "Delete…",
      group: "keep",
      danger: true,
      disabled: c.busy || retiring,
      hint: c.busy ? "Stop its turn first" : "Delete it permanently",
    });
  }
  return out;
}

/** The two tabs: the Baker, and everything else (Chat). */
export type NavMode = "chat" | "baker";

/** Which tab a route belongs to; `null` for Settings, which belongs to
 *  neither. Derived from the path so the tabs and the sidebar can never
 *  disagree with where you actually are. */
export function modeForPath(pathname: string): NavMode | null {
  if (pathname === "/baker" || pathname.startsWith("/baker/")) return "baker";
  if (pathname.startsWith("/settings")) return null;
  return "chat";
}

/** "3m", "5h", "2d" — or "" for a timestamp that does not parse. */
export function ago(ts: string, now = Date.now()): string {
  const then = Date.parse(ts);
  if (Number.isNaN(then)) return "";
  const secs = Math.max(0, Math.round((now - then) / 1000));
  if (secs < 60) return "now";
  if (secs < 3600) return `${Math.round(secs / 60)}m`;
  if (secs < 86400) return `${Math.round(secs / 3600)}h`;
  return `${Math.round(secs / 86400)}d`;
}
