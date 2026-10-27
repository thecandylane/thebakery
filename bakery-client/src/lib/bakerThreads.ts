// The Baker's conversations (docs/slices/BAKER-THREADS-03.md): its main
// conversation plus every parallel thread, derived from the one topology
// source — `/studio/state`'s factory sockets carrying `baker_thread` — never
// a second client-side cache of thread state.
//
// Pure and DOM-free on purpose, so `bakerThreads.check.ts` tests it with
// plain node.

import type { SocketInfo, StudioState } from "./node.ts";
import { toHandle } from "./handle.ts";

export type BakerConversation = {
  streamId: string;
  title: string;
  /** The permanent Baker's own conversation — never archived or renamed. */
  main: boolean;
  archived: boolean;
  /** A turn is running on it right now (live-patched by `studioStore`). */
  busy: boolean;
  /** Undecided proposals in this conversation waiting on the operator. */
  pendingGates: number;
  lastTs: string;
};

function toConversation(s: SocketInfo): BakerConversation | null {
  const t = s.baker_thread;
  if (!t || s.discarded) return null;
  return {
    streamId: s.stream_id,
    title: t.title,
    main: t.main,
    archived: t.archived,
    busy: s.agents.some((a) => a.busy),
    pendingGates: s.pending_gates,
    lastTs: s.last_ts,
  };
}

/** Main first, then most recently active first. Node timestamps are
 *  RFC 3339 in UTC, so they order lexically. */
function mainThenRecent(a: BakerConversation, b: BakerConversation): number {
  if (a.main !== b.main) return a.main ? -1 : 1;
  return a.lastTs < b.lastTs ? 1 : a.lastTs > b.lastTs ? -1 : 0;
}

export function bakerConversations(state: StudioState | null): {
  active: BakerConversation[];
  archived: BakerConversation[];
} {
  const all = (state?.factory_sockets ?? [])
    .map(toConversation)
    .filter((c): c is BakerConversation => c !== null);
  return {
    active: all.filter((c) => !c.archived).sort(mainThenRecent),
    archived: all.filter((c) => c.archived).sort(mainThenRecent),
  };
}

/** Where a conversation lives: `/baker` for the main one, `/baker/<handle>`
 *  for a thread — the same opaque handle `/w/<handle>` uses. */
export function bakerPath(c: { streamId: string; main: boolean }): string {
  return c.main ? "/baker" : `/baker/${toHandle(c.streamId)}`;
}
