<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import { page } from "$app/state";
  import { goto } from "$app/navigation";
  import { fromHandle } from "$lib/handle";
  import { studioStore } from "$lib/studioStore.svelte";
  import { WorkbenchState } from "$lib/workbench.svelte";
  import DockviewHost from "$lib/shell/DockviewHost.svelte";
  import { bakerPath } from "$lib/bakerThreads";
  import { bakerChats } from "$lib/conversations";
  import ConversationActions from "$lib/shell/ConversationActions.svelte";
  import { useTitleBarSlot } from "$lib/shell/titleBarSlots.svelte";
  import { node, type BakerHome } from "$lib/node";

  // The Baker studio (docs/slices/BAKER-THREADS-03.md): one Baker, several
  // parallel conversations. `/baker` is its main conversation and
  // `/baker/<handle>` one of its threads — each the SAME Chat widget +
  // WorkbenchState/Dockview studio every socket uses, rebound to that
  // conversation's own stream, never a second chat implementation. The
  // conversation list lives in the sidebar's Code mode; the Baker can also
  // open, message, rename, and archive threads itself through its ops.
  //
  // It draws no bar of its own: the thread title and its actions go into the
  // one title bar. What is working and what needs you is the sidebar's
  // status filter, with node status and each conversation's own status.

  const workbench = new WorkbenchState();
  let bakerHome = $state<BakerHome | null>(null);
  let bakerError = $state("");
  let creating = $state(false);
  let actionError = $state("");

  useTitleBarSlot("crumb", crumb);
  useTitleBarSlot("actions", actions);

  const handle = $derived(page.params.thread ?? null);
  // `?open=review` — a chat's "awaiting review" link lands on its Review.
  const openPanel = $derived(page.url.searchParams.get("open") === "review" ? "review" : null);
  /** `&change=<hash>` — "Open in Review" on one change in the chat. */
  const focusChange = $derived(page.url.searchParams.get("change"));
  /** The stream this page shows: the main Baker until a handle names a thread. */
  const targetStream = $derived.by(() => {
    if (!handle) return bakerHome?.stream_id ?? null;
    try {
      return fromHandle(handle);
    } catch {
      return null;
    }
  });
  const convs = $derived(bakerChats(studioStore.data));
  const current = $derived(
    targetStream === null
      ? null
      : ([...convs.active, ...convs.archived].find((c) => c.streamId === targetStream) ?? null),
  );
  /** A handle that is not (or no longer) one of the Baker's conversations —
   *  a legacy job, another socket, garbage. Never bind the Baker page to it. */
  const missing = $derived(handle !== null && studioStore.data !== null && current === null);

  $effect(() => {
    const sid = targetStream;
    if (sid === null) return;
    // A thread binds once the topology confirms it is a Baker conversation;
    // the main conversation binds as soon as `GET /baker` names it.
    if (handle !== null && current === null) return;
    workbench.bindTo(sid);
  });

  async function newThread() {
    if (creating) return;
    creating = true;
    actionError = "";
    try {
      const res = await node.bakerThreadCreate();
      studioStore.refresh();
      await goto(bakerPath({ streamId: res.thread.stream_id, main: false }));
    } catch (e) {
      actionError = `${e}`;
    } finally {
      creating = false;
    }
  }

  async function restore() {
    if (!current) return;
    actionError = "";
    try {
      await node.bakerThreadArchive(current.streamId, false);
      studioStore.refresh();
    } catch (e) {
      actionError = `${e}`;
    }
  }

  onMount(() => {
    studioStore.ensureStarted();
    void node
      .bakerHome()
      .then((home) => {
        bakerHome = home;
      })
      .catch((e) => {
        bakerError = `${e}`;
      });
  });

  onDestroy(() => {
    workbench.unbind();
  });
</script>

{#snippet crumb()}
  {#if current && !current.main}
    <span class="thread-title" title={current.title} data-testid="baker-thread-title">{current.title}</span>
  {/if}
{/snippet}

{#snippet actions()}
  {#if current}
    <ConversationActions streamId={current.streamId} align="start" />
  {/if}
  <button
    type="button"
    class="new-thread"
    onclick={newThread}
    disabled={creating}
    data-testid="baker-page-new-thread"
    aria-label="New thread"
    title="New Baker thread"
  >
    <svg viewBox="0 0 14 14" aria-hidden="true"><path d="M7 2.5v9M2.5 7h9" /></svg>
  </button>
{/snippet}

<div class="baker">
  {#if actionError}
    <p class="notice bad" role="alert">
      {actionError}
      <button type="button" class="dismiss" onclick={() => { actionError = ""; }} aria-label="Dismiss">×</button>
    </p>
  {/if}

  {#if current?.archived}
    <p class="notice" data-testid="baker-archived-banner">
      Archived.
      <button type="button" class="notice-btn" onclick={restore}>Restore</button>
    </p>
  {/if}

  <section class="main-panel">
    {#if missing}
      <div class="slot-hint">
        <p>That Baker thread doesn’t exist on this node.</p>
        <a href="/baker">Back to the Baker</a>
      </div>
    {:else if bakerError && !handle}
      <p class="slot-hint bad">{bakerError}</p>
    {:else if targetStream !== null && (handle === null || current !== null)}
      <div class="chat-slot">
        <DockviewHost {workbench} kind="factory" {openPanel} {focusChange} />
      </div>
    {:else}
      <p class="slot-hint">Connecting to Baker…</p>
    {/if}
  </section>
</div>

<style>
  .baker {
    height: 100%;
    display: flex;
    flex-direction: column;
    min-height: 0;
    background: var(--bg-app);
  }

  /* Rendered in the title bar (see the snippets above); scoped here. */
  .thread-title {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font: 550 12px/1 var(--font-ui);
    color: var(--fg-2);
  }

  .new-thread {
    flex: 0 0 auto;
    display: grid;
    place-items: center;
    width: 28px;
    height: 28px;
    padding: 0;
    border: 1px solid transparent;
    border-radius: var(--r-xl);
    background: transparent;
    color: var(--fg-faint);
    cursor: pointer;
  }
  .new-thread:hover:not(:disabled) { background: var(--bg-chip); border-color: var(--line-soft); color: var(--fg-2); }
  .new-thread:disabled { opacity: 0.55; cursor: default; }
  .new-thread:focus-visible { outline: 2px solid var(--accent); outline-offset: 1px; }
  .new-thread svg {
    width: 14px;
    height: 14px;
    fill: none;
    stroke: currentColor;
    stroke-width: 1.5;
    stroke-linecap: round;
  }

  .notice {
    flex: 0 0 auto;
    display: flex;
    align-items: center;
    gap: 10px;
    margin: 0;
    padding: 7px 12px;
    border-bottom: 1px solid var(--line-chrome);
    background: var(--bg-panel);
    color: var(--fg-dim);
    font: 400 12px/1.4 var(--font-ui);
  }
  .notice.bad { background: var(--bad-bg); color: var(--bad); }
  .notice-btn {
    padding: 3px 9px;
    border: 1px solid var(--line);
    border-radius: var(--r-sm);
    background: var(--bg-app);
    color: var(--fg-2);
    font: 550 11px/1 var(--font-ui);
    cursor: pointer;
  }
  .notice-btn:hover { border-color: var(--line-strong); color: var(--fg); }
  .dismiss {
    margin-left: auto;
    border: none;
    background: transparent;
    color: inherit;
    font: 600 13px/1 var(--font-ui);
    cursor: pointer;
  }

  .main-panel {
    flex: 1 1 auto;
    min-height: 0;
    min-width: 0;
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }
  /* Chat's root fills its slot absolutely (widget convention), so the slot
     supplies the positioning context the panel itself used to. */
  .chat-slot {
    flex: 1 1 auto;
    min-height: 0;
    position: relative;
    overflow: hidden;
  }
  .slot-hint {
    margin: 0;
    padding: 40px 20px;
    text-align: center;
    font: 400 12.5px/1.5 var(--font-ui);
    color: var(--fg-faint);
  }
  .slot-hint.bad { color: var(--bad); }
  .slot-hint a { color: var(--violet); }
</style>
