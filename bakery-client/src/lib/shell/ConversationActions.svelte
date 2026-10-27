<script lang="ts">
  import { page } from "$app/state";
  import { studioStore } from "$lib/studioStore.svelte";
  import { conversationActions, conversationFor, type ConversationAction } from "$lib/conversations";
  import { runConversationAction } from "$lib/conversationActions";

  // The standard actions menu for one conversation (the Baker's main one, a
  // thread, a project chat, a task). What applies and what each action does
  // live in `conversationActions.ts`, shared with the React work board.

  let {
    streamId,
    onNavigate,
    label = "Conversation actions",
    align = "end",
  }: {
    streamId: string;
    /** Called before any navigation this menu starts (closes a compact drawer). */
    onNavigate?: () => void;
    label?: string;
    /** Which edge of the button the menu lines up with: "start" for a
     *  button near the left of the window, so the menu opens rightward. */
    align?: "start" | "end";
  } = $props();

  const conv = $derived(conversationFor(studioStore.data, streamId));
  const here = $derived(page.url.pathname);
  const actions = $derived(conv ? conversationActions(conv, here, studioStore.isRetiring(conv.streamId)) : []);
  let open = $state(false);
  let working = $state(false);
  let note = $state("");
  let failed = $state(false);
  let root: HTMLElement | undefined = $state();

  $effect(() => {
    if (!open) return;
    const away = (e: PointerEvent) => {
      if (root && !root.contains(e.target as Node)) open = false;
    };
    const esc = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        open = false;
      }
    };
    window.addEventListener("pointerdown", away);
    window.addEventListener("keydown", esc, true);
    return () => {
      window.removeEventListener("pointerdown", away);
      window.removeEventListener("keydown", esc, true);
    };
  });

  function toggle() {
    open = !open;
    note = "";
    failed = false;
  }

  async function run(a: ConversationAction) {
    const c = conv;
    if (!c) return;
    if (a.id === "chat" || a.id === "code" || a.id === "baker") onNavigate?.();
    working = true;
    note = "";
    failed = false;
    try {
      const said = await runConversationAction(a.id, c, here);
      if (said) note = said;
      else open = false;
    } catch (e) {
      failed = true;
      note = `${e}`.replace(/^Error:\s*/, "");
    } finally {
      working = false;
    }
  }
</script>

{#if conv}
  <div class="ca" bind:this={root}>
    <button
      type="button"
      class="ca-trigger"
      aria-haspopup="menu"
      aria-expanded={open}
      aria-label="{label}: {conv.title}"
      title={label}
      data-testid="conversation-actions"
      onclick={toggle}
    >⋯</button>
    {#if open}
      <div class="ca-menu" class:start={align === "start"} role="menu" aria-label="{label}: {conv.title}">
        {#each actions as a, i (a.id)}
          {#if i > 0 && actions[i - 1].group !== a.group}<span class="ca-sep" role="separator"></span>{/if}
          <button
            type="button"
            role="menuitem"
            class:danger={a.danger}
            disabled={working || a.disabled}
            title={a.hint}
            data-testid="conversation-{a.id}"
            onclick={() => void run(a)}
          >{a.label}</button>
        {/each}
        {#if note}
          <p class="ca-note" class:bad={failed} role="status">{note}</p>
        {/if}
      </div>
    {/if}
  </div>
{/if}

<style>
  .ca {
    position: relative;
    flex: 0 0 auto;
  }
  .ca-trigger {
    display: grid;
    place-items: center;
    width: 22px;
    height: 22px;
    padding: 0;
    border: 0;
    border-radius: var(--r-sm);
    background: transparent;
    color: var(--fg-faint);
    font: 600 13px/1 var(--font-ui);
    cursor: pointer;
  }
  .ca-trigger:hover,
  .ca-trigger[aria-expanded="true"] {
    background: var(--bg-chip);
    color: var(--fg);
  }
  .ca-trigger:focus-visible { outline: 2px solid var(--accent); outline-offset: 1px; }
  .ca-menu {
    position: absolute;
    top: 100%;
    right: 0;
    z-index: 60;
    display: flex;
    flex-direction: column;
    min-width: 180px;
    max-width: 280px;
    margin-top: 3px;
    padding: 4px;
    background: var(--bg-raised);
    border: 1px solid var(--line-strong);
    border-radius: var(--r-lg);
    box-shadow: var(--shadow-menu);
  }
  .ca-menu.start { right: auto; left: 0; }
  .ca-menu button {
    display: block;
    width: 100%;
    padding: 6px 9px;
    border: 0;
    border-radius: var(--r-sm);
    background: transparent;
    color: var(--fg-2);
    font: 400 12px/1.3 var(--font-ui);
    text-align: left;
    cursor: pointer;
  }
  .ca-menu button:hover:not(:disabled) { background: var(--bg-chip); color: var(--fg); }
  .ca-menu button:disabled { opacity: 0.45; cursor: default; }
  .ca-menu button.danger { color: var(--bad); }
  .ca-menu button.danger:hover:not(:disabled) { background: var(--bad-bg); }
  .ca-sep { height: 1px; margin: 3px 4px; background: var(--line-soft); }
  .ca-note {
    margin: 4px 2px 2px;
    padding: 6px 8px;
    border-radius: var(--r-sm);
    background: var(--bg-chip);
    color: var(--fg-2);
    font: 400 11.5px/1.4 var(--font-ui);
    overflow-wrap: anywhere;
  }
  .ca-note.bad { background: var(--bad-bg); color: var(--bad); }
</style>
