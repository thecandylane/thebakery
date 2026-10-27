<script lang="ts">
  import { goto } from "$app/navigation";
  import { page } from "$app/state";
  import DockviewHost from "$lib/shell/DockviewHost.svelte";
  import { fromHandle, toHandle } from "$lib/handle";
  import { WorkbenchState } from "$lib/workbench.svelte";
  import { studioStore } from "$lib/studioStore.svelte";

  const workbench = new WorkbenchState();
  let handleError = $state("");
  let redirecting = false;

  const label = $derived(
    workbench.socket ? `${workbench.socket.project || workbench.socket.name} · ${workbench.socket.branch}` : "session",
  );

  // `?open=review` deep link (07d-B Phase 4 item 3: the Factory Floor's task
  // rows link straight to a task's Review, not just its conversation).
  // `WidgetId` values only — anything else is ignored rather than passed
  // through to `DockviewHost.addSingleton` unchecked.
  const openPanel = $derived(page.url.searchParams.get("open") === "review" ? "review" : null);
  /** `&change=<hash>` — "Open in Review" on one change in the chat. */
  const focusChange = $derived(page.url.searchParams.get("change"));

  // Re-bind when the handle changes (session tree click reuses this component;
  // onMount alone left the previous stream's history and subscription stuck).
  // The returned cleanup covers the case onMount never did either: navigating
  // clean away from /w/[sid] (not just switching :sid) destroys this effect's
  // scope with no other teardown hook, and without it the event subscription
  // — and the WorkbenchState instance it closes over — would leak forever.
  $effect(() => {
    const raw = page.params.sid ?? "";
    let id: string;
    try {
      id = fromHandle(raw);
    } catch {
      handleError = "That link does not name a session.";
      workbench.unbind();
      return;
    }
    handleError = "";
    workbench.bindTo(id);
    return () => workbench.unbind();
  });

  // 07c-LIVE-STATE: "if the open workbench socket is discarded, navigate to
  // another active socket or the floor — do not leave a dead /w/[sid]." Once
  // the store has loaded and the bound socket resolves as discarded, hop off
  // this route instead of showing a stay-here banner. `redirecting` guards
  // against firing goto() again for the same in-flight navigation if a
  // topology event lands before it resolves; once it lands, the URL (and
  // `workbench.socket`) has moved on, so this naturally does not loop.
  $effect(() => {
    if (!studioStore.data || redirecting) return;
    const sock = workbench.socket;
    if (!sock?.discarded) return;
    redirecting = true;
    const next = studioStore.activeSockets[0];
    void goto(next ? `/w/${toHandle(next.stream_id)}` : "/").finally(() => {
      redirecting = false;
    });
  });
</script>

{#if handleError}
  <div class="err-wrap">
    <p class="slot-hint bad">{handleError}</p>
    <a href="/" class="back-link">Back to the Control Plane</a>
  </div>
{:else}
  <div class="studio" aria-label={label}>
    {#if workbench.histLoading}
      <p class="slot-hint overlay">loading history…</p>
    {:else if workbench.histError}
      <p class="slot-hint bad overlay">{workbench.histError}</p>
    {/if}
    {#if workbench.notice}
      <p class="slot-hint bad overlay notice">{workbench.notice}</p>
    {/if}
    <DockviewHost {workbench} kind={workbench.groupKind} {openPanel} {focusChange} />
  </div>
{/if}

<style>
  .studio {
    height: 100%;
    position: relative;
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }

  .slot-hint {
    padding: 10px 14px;
    font: 400 11.5px/1.4 var(--font-ui);
    color: var(--fg-faint);
    margin: 0;
    flex: 0 0 auto;
  }
  .slot-hint.bad { color: var(--bad); }
  .slot-hint.overlay {
    position: absolute;
    top: 0;
    left: 0;
    right: 0;
    z-index: 30;
    background: var(--bg-panel);
    border-bottom: 1px solid var(--line-chrome);
  }
  .slot-hint.notice { top: auto; bottom: 0; border-bottom: none; border-top: 1px solid var(--line-chrome); }

  .err-wrap {
    padding: 1.25rem;
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }

  .back-link {
    font: 400 12px/1 var(--font-ui);
    color: var(--accent-fg);
  }
</style>
