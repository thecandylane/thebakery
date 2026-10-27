<script lang="ts">
  import { untrack } from "svelte";
  import { page } from "$app/state";
  import { goto } from "$app/navigation";
  import { fromHandle } from "$lib/handle";
  import { node } from "$lib/node";
  import { studioStore } from "$lib/studioStore.svelte";
  import { WorkbenchState } from "$lib/workbench.svelte";
  import { createAgentPanelSource, refreshStudio } from "$lib/react/bridge.svelte";
  import { mountAgentPanel } from "$lib/react/mount";
  import { codePath, conversationFor, reviewPath } from "$lib/conversations";
  import { navMode } from "$lib/navMode.svelte";
  import ConversationActions from "$lib/shell/ConversationActions.svelte";
  import { useTitleBarSlot } from "$lib/shell/titleBarSlots.svelte";

  // One conversation on its own, full width — the Baker, a thread, a project
  // chat, or a task. It is the SAME conversation its studio shows (one
  // `AgentConversation` per agent, drawn by the same React panel), never a
  // second chat; its files, review, and terminal are one click away in Code.
  //
  // No bar of its own: the title bar already names it, the sidebar row shows
  // its status, and its actions go into the title bar.

  const workbench = new WorkbenchState();
  useTitleBarSlot("actions", actions);
  let host: HTMLDivElement | undefined = $state();
  let error = $state("");

  const streamId = $derived.by(() => {
    try {
      return fromHandle(page.params.handle ?? "");
    } catch {
      return null;
    }
  });
  const conv = $derived(streamId ? conversationFor(studioStore.data, streamId) : null);
  const roleId = $derived(conv?.roleId ?? null);
  const agentId = $derived(
    roleId ? (workbench.socket?.agents.find((a) => a.role_id === roleId)?.agent_id ?? null) : null,
  );
  /** Not (or no longer) a conversation — deleted, or a bad link. */
  const missing = $derived(
    streamId === null || (studioStore.data !== null && !studioStore.loading && conv === null),
  );

  // The Baker is its own mode: its conversations never open in Chat. Once
  // handed off, this link is no Chat place for the Chat tab to return to.
  $effect(() => {
    if (!conv?.baker) return;
    const from = page.url.pathname;
    void goto(codePath(conv), { replaceState: true }).then(() => navMode.forget(from));
  });

  $effect(() => {
    const sid = streamId;
    if (!sid) return;
    workbench.bindTo(sid);
    return () => workbench.unbind();
  });

  // Mount the panel once per agent. Only the element and the two ids are
  // dependencies — the panel keeps itself live through its own snapshot
  // subscription, so nothing it reads may re-mount it.
  $effect(() => {
    const el = host;
    const id = agentId;
    const role = roleId;
    if (!el || !id || !role) return;
    return untrack(() => {
      const source = createAgentPanelSource(workbench, id, role, {
        openReview: (hash) => {
          if (conv) void goto(reviewPath(conv, hash));
        },
        onAssigned: refreshStudio,
      });
      return mountAgentPanel(el, source);
    });
  });

  async function restore() {
    if (!streamId) return;
    error = "";
    try {
      await node.archiveConversation(streamId, false);
      studioStore.refresh();
    } catch (e) {
      error = `${e}`;
    }
  }
</script>

{#snippet actions()}
  {#if conv && !missing}
    {#if conv.pendingGates > 0}
      <a class="tb-btn warn" href={reviewPath(conv)} title="Review {conv.pendingGates} {conv.pendingGates === 1 ? 'change' : 'changes'}">
        Review <span class="n">{conv.pendingGates}</span>
      </a>
    {/if}
    <a class="tb-btn" href={codePath(conv)} title="Beside its files, review, and terminal">Studio</a>
  {/if}
  {#if streamId && !missing}<ConversationActions {streamId} align="start" />{/if}
{/snippet}

<div class="chat" data-testid="chat-page">
  {#if missing}
    <div class="gone">
      <p>This conversation doesn’t exist any more — it was deleted, or the link is wrong.</p>
      <a href="/chat">Back to Chat</a>
    </div>
  {:else}
    {#if conv?.archived}
      <p class="banner">
        Archived — hidden from the lists, history kept.
        <button type="button" class="link" onclick={() => void restore()}>Restore</button>
      </p>
    {/if}
    {#if error}<p class="banner bad" role="alert">{error}</p>{/if}
    <div class="body">
      <div class="col" bind:this={host}></div>
      {#if !agentId}<p class="hint">Connecting…</p>{/if}
    </div>
  {/if}
</div>

<style>
  .chat {
    height: 100%;
    display: flex;
    flex-direction: column;
    min-height: 0;
    background: var(--bg-app);
  }
  /* Rendered in the title bar (see the snippet above); scoped here. */
  .tb-btn {
    flex: 0 0 auto;
    display: inline-flex;
    align-items: center;
    gap: 5px;
    height: 26px;
    padding: 0 9px;
    border: 1px solid var(--line-soft);
    border-radius: var(--r-lg);
    color: var(--fg-2);
    font: 550 11.5px/1 var(--font-ui);
    text-decoration: none;
    white-space: nowrap;
  }
  .tb-btn:hover { background: var(--bg-chip); color: var(--fg); text-decoration: none; }
  .tb-btn:focus-visible { outline: 2px solid var(--accent); outline-offset: 1px; }
  .tb-btn.warn { color: var(--warn); background: var(--warn-bg); border-color: var(--warn-border); }
  .tb-btn .n { font: 600 10.5px/1 var(--font-mono); }
  .banner {
    flex: 0 0 auto;
    margin: 0;
    padding: 7px 16px;
    background: var(--bg-panel);
    border-bottom: 1px solid var(--line-chrome);
    font: 400 12px/1.4 var(--font-ui);
    color: var(--fg-dim);
  }
  .banner.bad { color: var(--bad); background: var(--bad-bg); }
  .link {
    padding: 0;
    border: 0;
    background: none;
    color: var(--accent-fg);
    font: inherit;
    cursor: pointer;
  }
  .body {
    position: relative;
    flex: 1 1 auto;
    min-height: 0;
  }
  /* A readable column, centered — the conversation, not a pane. */
  .col {
    position: absolute;
    inset: 0;
    margin: 0 auto;
    max-width: 920px;
    overflow: hidden;
  }
  .hint {
    position: absolute;
    inset: 40% 0 auto;
    margin: 0;
    text-align: center;
    font: 400 12px/1.4 var(--font-ui);
    color: var(--fg-faint);
  }
  .gone {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 8px;
    padding: 18vh 24px 0;
    font: 400 13px/1.5 var(--font-ui);
    color: var(--fg-dim);
  }
  .gone p { margin: 0; }
  .gone a { color: var(--accent-fg); }
</style>
