// The work board's side of the Svelte/React boundary — the same subscription
// adapter as `bridge.svelte.ts`: one `$effect.root` rebuilds a frozen
// snapshot from the canonical topology (`studioStore`) and notifies React,
// which reads it through `useSyncExternalStore`. No second conversation list,
// no polling, no Svelte proxy crossing over.

import { studioStore } from "$lib/studioStore.svelte";
import {
  allConversations,
  chats,
  conversationActions,
  type Conversation,
  type ConversationAction,
  type ConversationActionId,
} from "$lib/conversations";

/** Which conversations a board shows. */
export type WorkScope =
  | { kind: "chats" }
  | { kind: "all" }
  | { kind: "archived" }
  | { kind: "project"; projectId: string };

export type WorkItem = Readonly<{ conv: Conversation; actions: readonly ConversationAction[] }>;

export type WorkBoardSnapshot = Readonly<{
  /** `/studio/state` has answered at least once. */
  loaded: boolean;
  items: readonly WorkItem[];
}>;

export type WorkBoardStore = {
  subscribe(onChange: () => void): () => void;
  getSnapshot(): WorkBoardSnapshot;
  /** Runs one standard action; resolves to a sentence worth showing. */
  run(id: ConversationActionId, streamId: string): Promise<string | void>;
  dispose(): void;
};

export type WorkBoardHost = {
  /** The current route — actions for the page you are on are not offered. */
  here(): string;
  run(id: ConversationActionId, conv: Conversation): Promise<string | void>;
};

export function createWorkBoardSource(scope: WorkScope, host: WorkBoardHost): WorkBoardStore {
  const listeners = new Set<() => void>();

  function members(): Conversation[] {
    const data = studioStore.data;
    if (scope.kind === "chats") return chats(data).active;
    if (scope.kind === "archived") return allConversations(data).filter((c) => c.archived);
    const live = allConversations(data).filter((c) => !c.archived);
    if (scope.kind === "all") return live;
    const ids = new Set(
      data?.projects.find((p) => p.project_id === scope.projectId)?.sockets.map((s) => s.stream_id) ?? [],
    );
    return live.filter((c) => ids.has(c.streamId));
  }

  function build(): WorkBoardSnapshot {
    const here = host.here();
    return Object.freeze({
      loaded: studioStore.data !== null,
      items: Object.freeze(
        members().map((conv) =>
          Object.freeze({ conv, actions: conversationActions(conv, here, studioStore.isRetiring(conv.streamId)) }),
        ),
      ),
    });
  }

  let snapshot = build();
  const stopRoot = $effect.root(() => {
    $effect(() => {
      snapshot = build();
      for (const notify of listeners) notify();
    });
  });

  return {
    subscribe(onChange) {
      listeners.add(onChange);
      return () => listeners.delete(onChange);
    },
    getSnapshot: () => snapshot,
    async run(id, streamId) {
      const conv = snapshot.items.find((i) => i.conv.streamId === streamId)?.conv;
      if (conv) return host.run(id, conv);
    },
    dispose() {
      stopRoot();
      listeners.clear();
    },
  };
}
