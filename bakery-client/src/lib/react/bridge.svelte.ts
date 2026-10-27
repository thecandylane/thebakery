// Svelte state → React, via a subscription adapter.
//
// Of the two shapes the slice allows, this is the subscription adapter, not
// controlled props. The reason is structural: Dockview mounts panels
// imperatively, outside any Svelte component tree, so there is no parent to
// re-render and push props down — `DockviewHost` already reaches for live
// getters for exactly this reason. An external store is the honest version
// of that: one `$effect.root` per panel recomputes a frozen snapshot
// whenever its reactive dependencies change while a view shows it
// (`liveSnapshot`), and React consumes it through
// `useSyncExternalStore`. No polling, no timers, and no Svelte proxy ever
// crosses into React — `build()` copies out plain values.
//
// The store owns no server facts of its own. Every field is read from the
// existing `WorkbenchState` / `AgentConversation` / `studioStore`
// projections, and every action calls the operation Chat.svelte already
// called. If a fact is not in one of those, it is not in the snapshot.

import { agentChange, agentSessionChoose, agentTurnQueueWithdraw, agentTurnReview, filesReadDecide, networkHostDecide, type Seat, threadMerge } from "$lib/api.generated";
import { threadTitle } from "$lib/conversations";
import { node } from "$lib/node";
import { studioStore } from "$lib/studioStore.svelte";
import { deriveAvailableModels } from "$lib/studioTarget";
import type { AgentConversation, WorkbenchState } from "$lib/workbench.svelte";
import type { AgentPanelActions, AgentPanelSnapshot, AgentPanelStore, Outcome } from "./contract";
import { liveSnapshot } from "./liveSnapshot";
import { activityAfterTranscript, activityView, deriveComposerControl, deriveInterrupted, derivePendingAsk } from "./panelState";

/** The few things only the Dockview host can do. Kept as callbacks so the
 *  bridge never imports the dock, and the dock never imports React. */
export type PanelHost = {
  /** Open or focus the existing Review pane beside this conversation — on
   *  one change when its hash is given. */
  openReview(hash?: string): void;
  /** A seat assignment landed; refresh the studio projection. */
  onAssigned(): void;
};

const ok = <T,>(value: T): Outcome<T> => ({ status: "ok", value });
const STALE: Outcome<never> = { status: "stale" };

export type AgentPanelSource = AgentPanelStore & { dispose(): void };

export function createAgentPanelSource(
  workbench: WorkbenchState,
  agentId: string,
  roleId: string,
  host: PanelHost,
): AgentPanelSource {
  const conversation: AgentConversation = workbench.conversationFor(agentId, roleId);
  let disposed = false;
  /** Some view shows this thread: only then does the effect below rebuild. */
  let watched = $state(false);
  // Each read off the thread's own events, not redone per activity push.
  const composer = $derived(deriveComposerControl(conversation.events, agentId, conversation.sending));
  const pendingAsk = $derived(derivePendingAsk(conversation.items));

  /** Read every reactive dependency and copy out a plain, frozen value.
   *  Called inside the root `$effect` below, so Svelte tracks exactly the
   *  reads that happen here and nothing else. */
  function build(): AgentPanelSnapshot {
    const socket = workbench.socket;
    const agent = socket?.agents.find((a) => a.role_id === roleId) ?? null;
    const items = conversation.items;
    const rawActivity = conversation.activity;
    const activity = activityAfterTranscript(rawActivity, items);
    const { show } = activityView(activity);
    const canSend = agent?.socket_ready === true;

    return Object.freeze({
      streamId: workbench.streamId,
      agentId,
      roleId,
      roleLabel: agent?.role_label ?? roleId,
      title: socket ? threadTitle(socket) : "",

      // `items` is a `$derived` over `toTranscript`, i.e. already a fresh
      // array of plain objects rebuilt only when events change — never the
      // `$state` proxy itself. Handing it over keeps reference identity
      // stable across activity pushes, which is what lets React skip
      // re-rendering memoised rows on every streamed token.
      items,
      busy: rawActivity && !activity ? false : conversation.busy,
      activity,
      notice: conversation.notice,
      reconnects: conversation.reconnects,

      disabled: workbench.streamId == null,
      canSend,
      unavailableReason: !canSend && agent?.reason ? agent.reason : null,

      historyLoading: workbench.histLoading,
      historyError: workbench.histError,

      assignedModelId: agent?.assigned_model_id ?? null,
      assignedProvider: agent?.assigned_provider ?? null,
      selection: { model: agent?.selection?.model ?? "", effort: agent?.selection?.effort ?? "" },
      switching: agent?.switching ? { modelId: agent.switching.model_id, provider: agent.switching.provider ?? null } : null,
      switchFailed: agent?.switch_failed
        ? {
            id: agent.switch_failed.id,
            modelId: agent.switch_failed.to.model_id,
            provider: agent.switch_failed.to.provider ?? null,
            error: agent.switch_failed.error,
          }
        : null,
      assignedNative:
        workbench.modelOptions.find((m) => m.id === agent?.assigned_model_id)?.transport === "acp",
      socketReady: agent?.socket_ready === true,
      availableModels: deriveAvailableModels(socket, workbench.modelOptions).map((m) => ({
        id: m.id,
        label: m.label,
      })),

      worktreePath: socket?.worktree_path ?? "",
      worktreeOnDisk: socket?.exists_on_disk === true,
      selectedFile: workbench.selectedFile,
      operatingMode: composer.operatingMode,
      queuedInstructions: composer.queuedInstructions,
      queuedItems: composer.queuedItems,
      dispatchPaused: composer.dispatchPaused,

      pendingProposals: socket?.pending_gates ?? 0,
      // Only a materialized task socket has a baseline; every other socket
      // reports 0/0, which is absence, not "no changes". Conflating the two
      // would imply a clean worktree for a socket that has no baseline at all.
      taskBaseline:
        socket?.state === "task" ? { added: socket.changed_added, removed: socket.changed_removed } : null,

      interrupted: deriveInterrupted(items, show),
      pendingAsk,
    });
  }

  // A warm thread nobody shows rebuilds nothing on its live updates; the
  // next view to subscribe reads it fresh (liveSnapshot).
  const live = liveSnapshot(
    build,
    () => watched,
    (now) => {
      watched = now;
    },
  );

  const stopRoot = $effect.root(() => {
    $effect(() => live.track());
  });

  /** Wrap one existing action. Captures the bound stream up front and
   *  reports `stale` if the panel was disposed or rebound while the request
   *  was in flight, so a late answer is dropped instead of being applied to
   *  a panel that now shows someone else's conversation. */
  async function guarded<T>(run: (streamId: string) => Promise<T>): Promise<Outcome<T>> {
    const sid = workbench.streamId;
    if (sid == null) return { status: "error", error: "No thread selected" };
    const boundAgentIsCurrent = () =>
      workbench.socket?.agents.some((agent) => agent.agent_id === agentId && agent.role_id === roleId) === true;
    try {
      const value = await run(sid);
      if (disposed || workbench.streamId !== sid || !boundAgentIsCurrent()) return STALE;
      return ok(value);
    } catch (e) {
      if (disposed || workbench.streamId !== sid || !boundAgentIsCurrent()) return STALE;
      return { status: "error", error: `${e}` };
    }
  }

  /** Capability responses are additionally scoped to the assignment that
   * requested them. A catalog for yesterday's provider is not valid merely
   * because the same stream and agent still exist. */
  async function guardedAssignment<T>(
    modelId: string,
    run: (streamId: string) => Promise<T>,
  ): Promise<Outcome<T>> {
    const sid = workbench.streamId;
    if (sid == null) return { status: "error", error: "No thread selected" };
    const assignmentIsCurrent = () =>
      workbench.socket?.agents.some(
        (agent) =>
          agent.agent_id === agentId && agent.role_id === roleId && agent.assigned_model_id === modelId,
      ) === true;
    if (!assignmentIsCurrent()) return STALE;
    try {
      const value = await run(sid);
      if (disposed || workbench.streamId !== sid || !assignmentIsCurrent()) return STALE;
      return ok(value);
    } catch (e) {
      if (disposed || workbench.streamId !== sid || !assignmentIsCurrent()) return STALE;
      return { status: "error", error: `${e}` };
    }
  }

  const actions: AgentPanelActions = {
    async send(text, opts) {
      const sid = workbench.streamId;
      if (sid == null) return { status: "error", error: "No thread selected" };
      // The existing per-agent send: same route, same seat, same busy
      // bookkeeping. Its outcome decides whether the composer clears.
      const outcome = await conversation.send(text, opts.filePath, opts.attachmentIds, opts.steer);
      // A disposed React projection is not the same thing as a rejected
      // instruction. Once this exact request was acknowledged by the node,
      // the durable queue owns it and the composer's staged draft/one-shot
      // settings must be consumed even if Dockview closed the pane while the
      // response was in flight. A real stream rebind remains ambiguous and
      // therefore stale. Refusals after disposal remain stale as well, so the
      // staged draft is restored for the next projection.
      if (workbench.streamId !== sid) return STALE;
      if (outcome.ok) return ok(undefined as void);
      if (disposed) return STALE;
      return { status: "error", error: outcome.error };
    },

    // `AgentConversation` owns the optimistic “stopping…” snapshot. Keep
    // the usual stream/disposal fence around it so a pane rebound cannot
    // apply a late cancel outcome to another agent.
    cancel: () => guarded((sid) => conversation.requestStop(sid)),

    async resumeDispatch() {
      const out = await guarded((sid) => node.resumeDispatch(sid, roleId));
      if (out.status === "ok") {
        workbench.add(out.value);
        return ok(undefined as void);
      }
      return out;
    },

    async setOperatingMode(mode) {
      const out = await guarded((sid) => node.setOperatingMode(sid, roleId, mode));
      if (out.status === "ok") {
        workbench.add(out.value);
        return ok(undefined as void);
      }
      return out;
    },

    uploadAttachment: (file) => guarded((sid) => node.uploadAttachment(sid, roleId, file)),

    answer: (hash, answers) => guarded(async (sid) => void (await node.answer(sid, hash, answers))),

    async decideChildTool(hash, decision) {
      const out = await guarded((sid) => node.decideChildTool(sid, hash, decision));
      if (out.status === "ok") {
        workbench.add(out.value);
        return ok(undefined as void);
      }
      return out;
    },

    async decideAccessRequest(hash, allow) {
      const out = await guarded((sid) => node.decideAccessRequest(sid, hash, allow));
      if (out.status === "ok") {
        workbench.add(out.value);
        return ok(undefined as void);
      }
      return out;
    },

    decideHost: (projectId, host, allow) => guarded(async () => void (await networkHostDecide({ project_id: projectId, host, allow }))),
    decideFile: (projectId, path, allow, sure) => guarded(async () => void (await filesReadDecide({ project_id: projectId, path, allow, sure }))),
    mergeThread: (streamId) => guarded(async () => void (await threadMerge({ stream_id: streamId }))),

    async decideProposal(hash, accept) {
      const out = await guarded((sid) => node.decide(sid, hash, accept));
      if (out.status === "ok") {
        workbench.add(out.value);
        return ok(undefined as void);
      }
      return out;
    },

    reviewTurn(turnId, keep) {
      return guarded((sid) => agentTurnReview({ stream_id: sid, turn_id: turnId, accept: keep }));
    },

    fetchChange(hash) {
      return guarded(async (sid) => {
        const change = await agentChange({ stream_id: sid, event_hash: hash });
        return { before: change.before ?? null, after: change.after ?? null };
      });
    },

    fetchToolPayload: (sha) => guarded((sid) => node.toolPayload(sid, sha)),

    async steerAllQueued() {
      const out = await guarded((sid) => node.steerAllQueued(sid, roleId));
      if (out.status === "ok") {
        workbench.add(out.value);
        return ok(undefined as void);
      }
      return out;
    },

    withdrawQueued: (id) =>
      guarded(async (sid) => {
        const back = await agentTurnQueueWithdraw({ stream_id: sid, id });
        return { instruction: back.instruction, filePath: back.file_path ?? null, attachmentIds: back.attachment_ids };
      }),

    resume: () => guarded(async (sid) => void (await node.resume(sid, roleId))),
    fetchAttachment: (id) => guarded((sid) => node.attachment(sid, id)),
    chooseSession: (choice) => guarded(async (sid) => void (await agentSessionChoose({ stream_id: sid, seat: roleId as Seat, choice }))),

    async assignModel(modelId, provider) {
      const out = await guarded(async (sid) => void (await node.seatAssign(sid, roleId, modelId, provider)));
      if (out.status === "ok") host.onAssigned();
      return out;
    },
    async select(choice) {
      const out = await guarded(async (sid) => void (await node.select(sid, roleId, choice)));
      if (out.status === "ok") host.onAssigned();
      return out;
    },

    setSelectedFile(path) {
      workbench.selectedFile = path;
    },

    openReview(hash) {
      host.openReview(hash);
    },

    fetchModelCatalog: (modelId) => guardedAssignment(modelId, () => node.modelCatalog(modelId)),

    fetchAccountCapabilities: (modelId) =>
      guardedAssignment(modelId, (sid) => node.accountCapabilities(modelId, sid, roleId)),

    setNativeConfig: (generation, configId, value) =>
      guardedAssignment(live.getSnapshot().assignedModelId ?? "", (sid) =>
        node.setNativeConfig(sid, roleId, generation, configId, value),
      ),
    restartRuntime: () => guarded((sid) => node.restartRuntime(sid)),

    listDir: (path) => guarded((sid) => node.filesList(sid, path)),
    readFile: (path) => guarded((sid) => node.filesRead(sid, path)),
  };

  return {
    subscribe: live.subscribe,
    getSnapshot: live.getSnapshot,
    actions,
    dispose() {
      disposed = true;
      live.clear();
      // Releases the `$effect` above. The `AgentConversation` is NOT
      // disposed here: it outlives every panel bound to it, so closing a
      // pane must not stop a running turn or drop its history —
      // `WorkbenchState.bindTo`/`unbind` own that lifetime.
      stopRoot();
    },
  };
}

/** `studioStore.refresh()` as a `PanelHost` hook, for callers with no other
 *  opinion. Exported so `DockviewHost` states its intent in its own file
 *  rather than the store being reached into from a React module. */
export const refreshStudio = () => studioStore.refresh();
