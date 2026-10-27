// The new shell's side of the Svelte/React boundary — the same subscription
// adapter as `workBoardSource.svelte.ts`: one `$effect.root` rebuilds a frozen
// snapshot from the canonical topology (`studioStore`) and React reads it
// through `useSyncExternalStore`. `attach` hands out the SAME agent panel
// source the studio's Dockview panes use, bound to its own WorkbenchState,
// so a conversation in the strip is never a second implementation.

import { allConversations, conversationFor, reviewPath, type Conversation } from "$lib/conversations";
import { node, type SocketInfo } from "$lib/node";
import { studioStore } from "$lib/studioStore.svelte";
import { WorkbenchState } from "$lib/workbench.svelte";
import { createAgentPanelSource, refreshStudio, type AgentPanelSource } from "../bridge.svelte";

/** Threads off screen that stay warm (history loaded, live), most recent first. */
const WARM_THREADS = 24;

/** One checkout of a project: its main one (`inPlace`, the repository
 *  itself) or an agent's worktree on its own branch. */
export type ShellCheckout = Readonly<{
  streamId: string;
  branch: string;
  inPlace: boolean;
  onDisk: boolean;
  /** Its working folder (the repository itself for the local checkout). */
  path: string;
  repoRoot: string;
  /** Lines added/removed against its baseline; 0/0 also when it has none. */
  added: number;
  removed: number;
  /** An untrusted (burn) session: its terminals run in its room, never on
   *  the host (the node refuses a host one). */
  burn: boolean;
}>;

export type ShellProject = Readonly<{
  id: string;
  name: string;
  streamIds: readonly string[];
  checkouts: readonly ShellCheckout[];
}>;

export type ShellSnapshot = Readonly<{
  /** `/studio/state` has answered at least once. */
  loaded: boolean;
  /** …since this window opened: not just the copy kept from before. */
  fresh: boolean;
  projects: readonly ShellProject[];
  /** The Bakery's own project, the Baker's (id ""): the main checkout, the
   *  permanent Baker's folder, the worktrees its threads work in, and its
   *  workspaces': each orc's and its workers'. */
  bakery: ShellProject;
  /** Every live conversation (archived ones left out), main Baker first. */
  conversations: readonly Conversation[];
  /** The archived ones: a worktree's past threads, to restore and enter. */
  archived: readonly Conversation[];
  /** Conversations whose agent the topology names, i.e. `attach` succeeds. */
  attachable: readonly string[];
  /** Why the Baker could not be reached (`GET /baker`), or "". */
  bakerError: string;
}>;

export type AttachedConversation = { source: AgentPanelSource; dispose(): void };

export type ShellStore = {
  subscribe(onChange: () => void): () => void;
  getSnapshot(): ShellSnapshot;
  /** A live agent panel source for this conversation, or null while the
   *  topology does not name its agent yet. The caller disposes it. */
  attach(streamId: string): AttachedConversation | null;
  /** Starts a Baker thread; resolves to its stream id. */
  newBakerThread(): Promise<string>;
  /** Asks the topology again (after the shell itself changed it). */
  refresh(): void;
  dispose(): void;
};

function agentIdOf(conv: Conversation): string | undefined {
  return studioStore.findSocket(conv.streamId)?.agents.find((a) => a.role_id === conv.roleId)?.agent_id;
}

/** What only the page can do: navigate (React never imports `$app`). */
export type ShellHost = { go(path: string): void };

export function createShellSource(host: ShellHost): ShellStore {
  const listeners = new Set<() => void>();
  let bakerError = $state("");
  // `GET /baker` names (and on a fresh node first creates) the main Baker
  // conversation, as the Baker page does on mount; the topology then lists it.
  node.bakerHome().then(
    () => studioStore.refresh(),
    (e) => {
      bakerError = `${e}`;
    },
  );

  const projectOf = (id: string, name: string, sockets: readonly SocketInfo[]): ShellProject =>
    Object.freeze({
      id,
      name,
      streamIds: Object.freeze(sockets.map((s) => s.stream_id)),
      checkouts: Object.freeze(
        sockets
          .filter((s) => !s.discarded)
          .map((s) =>
            Object.freeze({
              streamId: s.stream_id,
              branch: s.branch,
              inPlace: s.in_place,
              onDisk: s.exists_on_disk,
              path: s.worktree_path,
              repoRoot: s.repo_root,
              added: s.changed_added,
              removed: s.changed_removed,
              burn: s.lifetime === "burn",
            }),
          ),
      ),
    });

  function build(): ShellSnapshot {
    const data = studioStore.data;
    const all = allConversations(data);
    const conversations = all.filter((c) => !c.archived);
    // Orcs and their workers have no catalog project: they come ungrouped.
    const loose = data?.ungrouped_sockets ?? [];
    const orcs = new Set(loose.filter((s) => s.workspace).map((s) => s.stream_id));
    const workspaces = loose.filter((s) => s.workspace || (!!s.opened_by && orcs.has(s.opened_by)));
    return Object.freeze({
      loaded: data !== null,
      fresh: studioStore.fresh,
      projects: Object.freeze((data?.projects ?? []).map((p) => projectOf(p.project_id, p.name, p.sockets))),
      bakery: projectOf("", "Bakery", [...(data?.factory_sockets ?? []), ...workspaces]),
      conversations: Object.freeze(conversations),
      archived: Object.freeze(all.filter((c) => c.archived)),
      attachable: Object.freeze(conversations.filter((c) => agentIdOf(c)).map((c) => c.streamId)),
      bakerError,
    });
  }

  // Threads stay warm after their pane hides: the source keeps its loaded
  // history and live updates, so showing the thread again (a workspace
  // switch, a tab, the floor) loads nothing. Every thread on screen is held;
  // of the rest, the most recently shown WARM_THREADS stay.
  // ponytail: a count, not a memory budget — measure and resize if a long
  // history makes the cache heavy.
  const warm = new Map<string, { source: AgentPanelSource; users: number; at: number; close(): void }>();
  function trimWarm() {
    const idle = [...warm.entries()].filter(([, h]) => h.users <= 0).sort((a, b) => b[1].at - a[1].at);
    for (const [key, held] of idle.slice(WARM_THREADS)) {
      held.close();
      warm.delete(key);
    }
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
    attach(streamId) {
      const conv = conversationFor(studioStore.data, streamId);
      const agentId = conv && agentIdOf(conv);
      if (!conv || !agentId) return null;
      const key = `${streamId}:${agentId}`;
      let held = warm.get(key);
      if (!held) {
        // Its own root, as DockviewHost does for a teammate pane, so nothing
        // running when React mounts can own (and later tear down) its state.
        let bench!: WorkbenchState;
        const stopBench = $effect.root(() => {
          bench = new WorkbenchState();
        });
        bench.bindTo(streamId);
        const source = createAgentPanelSource(bench, agentId, conv.roleId, {
          // Review has no panel in the new shell yet: it opens in the studio.
          openReview: (hash) => host.go(reviewPath(conv, hash)),
          onAssigned: refreshStudio,
        });
        held = {
          source,
          users: 0,
          at: 0,
          close() {
            source.dispose();
            bench.unbind();
            stopBench();
          },
        };
        warm.set(key, held);
      }
      const entry = held;
      entry.users++;
      entry.at = Date.now();
      let released = false;
      return {
        source: entry.source,
        dispose() {
          if (released) return;
          released = true;
          entry.users--;
          entry.at = Date.now();
          trimWarm();
        },
      };
    },
    async newBakerThread() {
      const res = await node.bakerThreadCreate();
      studioStore.refresh();
      return res.thread.stream_id;
    },
    refresh: () => studioStore.refresh(),
    dispose() {
      for (const held of warm.values()) held.close();
      warm.clear();
      stopRoot();
      listeners.clear();
    },
  };
}
