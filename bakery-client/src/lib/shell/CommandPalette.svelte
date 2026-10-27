<script module lang="ts">
  /** One ⌘⇧P entry. Kept small on purpose: add one when you start wanting it. */
  export type PaletteCommand = { id: string; label: string; hint?: string; run: () => void };
</script>

<script lang="ts">
  import { goto } from "$app/navigation";
  import { page } from "$app/state";
  import type { SocketInfo } from "$lib/node";
  import { studioStore } from "$lib/studioStore.svelte";
  import { chatPath, codePath, modeForPath } from "$lib/conversations";

  let {
    open,
    onclose,
    commands,
    onjump,
  }: {
    open: boolean;
    onclose: () => void;
    /** Set: the palette lists these commands (⌘⇧P). Unset: it jumps to a
     *  chat or thread (⌘K). */
    commands?: PaletteCommand[];
    /** Set: a jump opens the conversation in place (the new shell) instead
     *  of navigating to its old screen. */
    onjump?: (streamId: string, bakerThread: boolean) => void;
  } = $props();

  let query = $state("");
  let activeIdx = $state(0);
  let inputEl = $state<HTMLInputElement | undefined>();
  let dialogEl = $state<HTMLDivElement | undefined>();

  // Retired sockets are excluded — hidden by default everywhere (STUDIO.md),
  // and jumping to one would land on a worktree/room that no longer exists.
  // Archived Baker threads are hidden the same way the sidebar hides them.
  const streams = $derived(studioStore.activeSockets.filter((s) => !s.baker_thread?.archived));

  /** A Baker conversation reads by its live title (a rename never touches
   *  the socket's recorded `name`). */
  function labelOf(s: SocketInfo): string {
    const t = s.baker_thread;
    if (!t) return s.name;
    return t.main ? "Baker" : `Baker · ${t.title}`;
  }

  const results = $derived.by(() => {
    const q = query.trim().toLowerCase();
    if (!q) return streams.slice(0, 8);
    return streams
      .filter(
        (s) =>
          labelOf(s).toLowerCase().includes(q) ||
          s.project.toLowerCase().includes(q) ||
          s.branch.toLowerCase().includes(q),
      )
      .slice(0, 12);
  });
  const commandResults = $derived.by(() => {
    const q = query.trim().toLowerCase();
    return (commands ?? []).filter((c) => !q || c.label.toLowerCase().includes(q));
  });
  const count = $derived(commands ? commandResults.length : results.length);
  const effectiveActiveIdx = $derived(count > 0 ? Math.min(Math.max(activeIdx, 0), count - 1) : -1);
  const activeOptionId = $derived(
    effectiveActiveIdx >= 0 ? `command-palette-option-${effectiveActiveIdx}` : undefined,
  );

  // Focus + reset when palette opens. The shell owns restoration because it
  // knows whether the opener was removed with the compact navigation drawer.
  $effect(() => {
    if (!open) return;
    query = "";
    activeIdx = 0;
    requestAnimationFrame(() => {
      if (open && inputEl?.isConnected) inputEl.focus({ preventScroll: true });
    });
  });

  // Reset active row when the result set changes (query typed or streams updated).
  $effect(() => {
    void count;
    activeIdx = 0;
  });

  function runCommand(c: PaletteCommand) {
    onclose();
    c.run();
  }

  function commit(s: SocketInfo) {
    onclose();
    if (onjump) {
      onjump(s.stream_id, !!s.baker_thread);
      return;
    }
    // A Baker conversation opens in the Baker; in Chat, anything else opens
    // as a chat; anywhere else, in its studio.
    const t = s.baker_thread;
    const c = { streamId: s.stream_id, main: t?.main === true, bakerThread: !!t };
    const chat = !t && modeForPath(page.url.pathname) === "chat" && studioStore.data?.factory_sockets.every((f) => f.stream_id !== s.stream_id);
    goto(chat ? chatPath(c) : codePath(c));
  }

  function focusableElements(): HTMLElement[] {
    if (!dialogEl) return [];
    return Array.from(
      dialogEl.querySelectorAll<HTMLElement>(
        'input:not([disabled]), button:not([disabled]), [href], [tabindex]:not([tabindex="-1"])',
      ),
    ).filter(
      (element) =>
        element.tabIndex >= 0 &&
        !element.hasAttribute("hidden") &&
        element.getAttribute("aria-hidden") !== "true",
    );
  }

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === "Escape") {
      e.stopPropagation();
      onclose();
    } else if (e.key === "Tab") {
      const focusable = focusableElements();
      if (focusable.length === 0) {
        e.preventDefault();
        dialogEl?.focus({ preventScroll: true });
        return;
      }
      const first = focusable[0];
      const last = focusable[focusable.length - 1];
      const active = document.activeElement;
      if (e.shiftKey && (active === first || !dialogEl?.contains(active))) {
        e.preventDefault();
        last.focus({ preventScroll: true });
      } else if (!e.shiftKey && (active === last || !dialogEl?.contains(active))) {
        e.preventDefault();
        first.focus({ preventScroll: true });
      }
    } else if (e.key === "ArrowDown") {
      e.preventDefault();
      activeIdx = Math.min(activeIdx + 1, count - 1);
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      activeIdx = Math.max(activeIdx - 1, 0);
    } else if (e.key === "Enter") {
      if (commands) {
        const c = commandResults[effectiveActiveIdx];
        if (c) runCommand(c);
      } else {
        const s = results[effectiveActiveIdx];
        if (s) commit(s);
      }
    }
  }
</script>

{#if open}
  <!-- svelte-ignore a11y_click_events_have_key_events -->
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div class="pal-overlay" onclick={onclose}>
    <!-- svelte-ignore a11y_click_events_have_key_events -->
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div
      bind:this={dialogEl}
      class="pal-dialog"
      role="dialog"
      aria-modal="true"
      aria-label="Command palette"
      tabindex="-1"
      onclick={(e) => e.stopPropagation()}
      onkeydown={handleKeydown}
    >
      <input
        bind:this={inputEl}
        bind:value={query}
        class="pal-input"
        placeholder={commands ? "Type a command" : "Jump to a thread"}
        type="text"
        role="combobox"
        aria-label={commands ? "Search commands" : "Search chats and threads"}
        aria-autocomplete="list"
        aria-expanded="true"
        aria-controls="command-palette-results"
        aria-activedescendant={activeOptionId}
        autocomplete="off"
        spellcheck="false"
      />

      <div
        class="pal-results"
        id="command-palette-results"
        role="listbox"
        aria-label={commands ? "Commands" : "Chats and threads"}
      >
        {#if commands}
          {#if commandResults.length === 0}
            <p class="pal-empty">No command matches "{query}"</p>
          {:else}
            {#each commandResults as c, i (c.id)}
              <button
                type="button"
                id={`command-palette-option-${i}`}
                class="pal-row"
                class:active={i === effectiveActiveIdx}
                role="option"
                aria-selected={i === effectiveActiveIdx}
                tabindex="-1"
                onclick={() => runCommand(c)}
                onmousemove={() => { activeIdx = i; }}
              >
                <span class="pal-name">{c.label}</span>
                {#if c.hint}<span class="pal-hint">{c.hint}</span>{/if}
              </button>
            {/each}
          {/if}
        {:else if streams.length === 0}
          <p class="pal-empty">No threads</p>
        {:else if results.length === 0}
          <p class="pal-empty">Nothing matches "{query}"</p>
        {:else}
          <div class="pal-group-lbl" role="presentation">THREADS</div>
          {#each results as s, i (s.stream_id)}
            <button
              type="button"
              id={`command-palette-option-${i}`}
              class="pal-row"
              class:active={i === effectiveActiveIdx}
              role="option"
              aria-selected={i === effectiveActiveIdx}
              tabindex="-1"
              onclick={() => commit(s)}
              onmousemove={() => { activeIdx = i; }}
            >
              <span class="pal-name">{labelOf(s)}</span>
              <span class="pal-branch">{s.branch}</span>
              <span class="pal-hint">⏎ open</span>
            </button>
          {/each}
        {/if}
      </div>
    </div>
  </div>
{/if}

<style>
  .pal-overlay {
    position: fixed;
    inset: 0;
    background: var(--overlay-scrim);
    display: flex;
    align-items: flex-start;
    justify-content: center;
    padding-top: 80px;
    z-index: 9000;
  }

  .pal-dialog {
    width: 520px;
    max-width: calc(100vw - 32px);
    background: var(--bg-panel);
    border: 1px solid var(--line-strong);
    border-radius: var(--r-modal);
    box-shadow: var(--shadow-modal);
    overflow: hidden;
    display: flex;
    flex-direction: column;
  }

  .pal-input {
    width: 100%;
    padding: 13px 16px;
    background: transparent;
    border: none;
    border-bottom: 1px solid var(--line-soft);
    font: 400 14px/1 var(--font-ui);
    color: var(--fg);
    outline: none;
    box-sizing: border-box;
  }
  .pal-input::placeholder {
    color: var(--fg-fainter);
    opacity: 1;
  }

  .pal-results {
    max-height: 360px;
    overflow-y: auto;
    padding: 4px 0;
  }

  .pal-group-lbl {
    padding: 6px 14px 3px;
    font: 600 9px/1 var(--font-ui);
    letter-spacing: 0.08em;
    color: var(--fg-fainter);
    text-transform: uppercase;
  }

  .pal-row {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    padding: 7px 14px;
    background: transparent;
    border: none;
    cursor: pointer;
    text-align: left;
    font: inherit;
    color: var(--fg-2);
    transition: background 120ms ease-out;
  }
  .pal-row:hover,
  .pal-row.active {
    background: var(--accent-bg);
    color: var(--fg);
  }

  .pal-name {
    font: 500 12.5px/1 var(--font-ui);
    flex: 1 1 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .pal-branch {
    font: 400 11px/1 var(--font-mono);
    color: var(--fg-dim);
    flex-shrink: 0;
    white-space: nowrap;
  }
  .pal-hint {
    font: 400 10px/1 var(--font-mono);
    color: var(--fg-fainter);
    flex-shrink: 0;
  }

  .pal-empty {
    padding: 12px 14px;
    font: 400 11.5px/1 var(--font-ui);
    color: var(--fg-faint);
    margin: 0;
  }
</style>
