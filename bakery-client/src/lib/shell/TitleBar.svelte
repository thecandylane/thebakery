<script lang="ts">
  import { page } from "$app/state";
  import { fromHandle, toHandle } from "$lib/handle";
  import { studioStore } from "$lib/studioStore.svelte";
  import { conversationFor } from "$lib/conversations";
  import { NODE } from "$lib/node";
  import type { ConnStatus } from "$lib/events";
  import { titleBar } from "$lib/shell/titleBarSlots.svelte";

  // The tools of what is open here first, then its name. Where you are —
  // Chat or Baker, and which list — is the sidebar's, and so is the button
  // that hides it: this bar carries that button only while the sidebar is
  // hidden, so there is exactly one of it on screen.

  let {
    sidebarOpen = true,
    status = "open",
    onToggleSidebar,
  }: {
    sidebarOpen?: boolean;
    /** The node connection. Shown only while it is not open. */
    status?: ConnStatus;
    onToggleSidebar?: () => void;
  } = $props();

  const path = $derived(page.url.pathname);

  const chatConversation = $derived.by(() => {
    if (!path.startsWith("/chat/")) return null;
    try {
      return conversationFor(studioStore.data, fromHandle(path.slice("/chat/".length)));
    } catch {
      return null;
    }
  });

  // Full socket set (not just active) — a socket the user just discarded
  // should keep naming the title bar for the instant before navigation moves
  // off it, rather than blanking.
  const currentRef = $derived.by(() => {
    if (!path.startsWith("/w/")) return null;
    const handle = path.slice(3).split("?")[0];
    return studioStore.sockets.find((s) => toHandle(s.stream_id) === handle) ?? null;
  });
</script>

<header class="titlebar" data-tauri-drag-region>
  {#if !sidebarOpen}
    <button
      type="button"
      class="chrome-button"
      data-testid="sidebar-toggle"
      data-tauri-drag-region="false"
      aria-controls="primary-sidebar"
      aria-expanded="false"
      aria-label="Show navigation"
      title="Show navigation (⌘\)"
      onclick={() => onToggleSidebar?.()}
    >
      <svg viewBox="0 0 18 18" aria-hidden="true">
        <rect x="2.25" y="2.75" width="13.5" height="12.5" rx="2.25"></rect>
        <path d="M6.25 3v12"></path>
      </svg>
    </button>
  {/if}

  {#if titleBar.tools}
    <div class="slot" data-tauri-drag-region="false">{@render titleBar.tools()}</div>
  {/if}
  {#if titleBar.actions}
    <div class="slot" data-tauri-drag-region="false">{@render titleBar.actions()}</div>
  {/if}

  <div class="where" aria-label="Current location">
    {#if currentRef}
      <span class="where-name">{currentRef.name}</span>
      {#if currentRef.branch}
        <span class="branch-chip">{currentRef.branch}</span>
      {/if}
      {#if currentRef.in_place}
        <span class="state-tag">local</span>
      {/if}
      {#if !currentRef.exists_on_disk}
        <span class="state-tag warn">folder gone</span>
      {/if}
    {:else if chatConversation}
      <span class="where-name">{chatConversation.title}</span>
    {/if}
    {@render titleBar.crumb?.()}
  </div>

  <span class="spacer"></span>

  {#if status !== "open"}
    <span class="conn" class:warn={status === "connecting"} title="node {NODE}" role="status" data-testid="conn-status">
      {status === "connecting" ? "Connecting…" : "Offline"}
    </span>
  {/if}
</header>

<style>
  /* Visible overflow and its own stacking level so menus opened from a
     slot drop over the page; below the compact drawer (44/45), which must
     still cover it. */
  .titlebar {
    position: relative;
    z-index: 30;
    height: 40px;
    flex: 0 0 auto;
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 0 8px;
    background: var(--bg-app);
    border-bottom: 1px solid var(--line-chrome);
    user-select: none;
  }

  .slot {
    display: flex;
    align-items: center;
    gap: 2px;
    flex: 0 0 auto;
  }
  .slot + .slot {
    margin-left: 4px;
    padding-left: 8px;
    border-left: 1px solid var(--line-soft);
  }

  .conn {
    flex: 0 0 auto;
    padding: 3px 8px;
    border-radius: var(--r-pill);
    background: var(--bad-bg);
    color: var(--bad);
    font: 550 10.5px/1 var(--font-ui);
    white-space: nowrap;
  }
  .conn.warn { background: var(--warn-bg); color: var(--warn); }

  .chrome-button {
    min-width: 28px;
    height: 28px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    padding: 0 6px;
    border: 1px solid transparent;
    border-radius: var(--r-xl);
    background: transparent;
    color: var(--fg-faint);
    cursor: pointer;
    flex: 0 0 auto;
  }
  .chrome-button:hover {
    color: var(--fg-2);
    background: var(--bg-chip);
    border-color: var(--line-soft);
  }
  .chrome-button:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }
  .chrome-button svg {
    width: 16px;
    height: 16px;
    fill: none;
    stroke: currentColor;
    stroke-width: 1.35;
    stroke-linecap: round;
    stroke-linejoin: round;
  }

  /* The name of what is open, after its tools. */
  .where {
    flex: 0 1 auto;
    min-width: 0;
    display: flex;
    align-items: center;
    gap: 7px;
    padding-left: 6px;
    overflow: hidden;
  }
  .where-name {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font: 550 12px/1 var(--font-ui);
    color: var(--fg-2);
  }

  .branch-chip,
  .state-tag {
    padding: 3px 7px;
    border-radius: var(--r-pill);
    background: var(--bg-chip);
    color: var(--fg-faint);
    font: 450 10px/1 var(--font-mono);
    flex: 0 0 auto;
    white-space: nowrap;
  }
  .state-tag.warn { background: var(--warn-bg); color: var(--warn); }

  .spacer { flex: 1 1 auto; }

  @media (max-width: 760px) {
    .titlebar { padding-inline: 6px; }
    .branch-chip,
    .state-tag { display: none; }
  }
</style>
