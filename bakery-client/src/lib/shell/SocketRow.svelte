<script lang="ts">
  import type { Snippet } from "svelte";
  import type { StreamRef } from "$lib/node";

  let {
    s,
    selected,
    dotClass,
    displayName,
    seatChipText,
    seatChipClass = "",
    statusText,
    rowClass = "",
    retiring = false,
    onOpen,
    onCycleLifetime,
    onDiscard,
    actions,
  }: {
    s: StreamRef;
    selected: boolean;
    dotClass: string;
    displayName: string;
    seatChipText: string;
    seatChipClass?: string;
    statusText: string;
    rowClass?: string;
    /** True while a discard() call for this socket is in flight
     *  (studioStore.isRetiring) — disables the discard button and shows a
     *  spinner in place of ×, distinct from `s.discarded` (server truth). */
    retiring?: boolean;
    onOpen: () => void;
    onCycleLifetime: (s: StreamRef) => void;
    onDiscard: (s: StreamRef) => void;
    /** The standard conversation actions menu, in place of the lone discard ×. */
    actions?: Snippet;
  } = $props();
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div
  class="session-row {rowClass}"
  class:selected
  class:retiring
  data-stream-id={s.stream_id}
  role="button"
  tabindex="0"
  onclick={onOpen}
  onkeydown={(e) => {
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      onOpen();
    }
  }}
>
  <div class="sess-line1">
    <span class="status-dot {dotClass}" aria-hidden="true"></span>
    <span class="sess-name">{displayName}</span>
    {#if s.discarded}
      <span class="lifetime-tag retired-tag" title="Discarded — worktree and room removed; history kept">retired</span>
    {:else}
      <button
        type="button"
        class="lifetime-dot lifetime-{s.lifetime}"
        disabled={retiring}
        title={s.lifetime === "living"
          ? "Living — pinned; discard needs confirm. Click to cycle."
          : s.lifetime === "burn"
            ? "Burn — untrusted; no host terminal. Click to cycle."
            : "Slice — default. Click to pin as living."}
        aria-label="Lifetime: {s.lifetime}. Click to cycle."
        onclick={(e) => { e.stopPropagation(); onCycleLifetime(s); }}
      ></button>
      {#if actions}
        <!-- svelte-ignore a11y_click_events_have_key_events -->
        <!-- svelte-ignore a11y_no_static_element_interactions -->
        <span class="row-actions" onclick={(e) => e.stopPropagation()} onkeydown={(e) => e.stopPropagation()}>
          {@render actions()}
        </span>
      {:else}
        <button
          type="button"
          class="discard-btn"
          class:retiring
          disabled={retiring}
          title={retiring ? "Discarding…" : "Discard — removes the worktree and room, keeps history"}
          aria-label={retiring ? `Discarding ${displayName}` : `Discard ${displayName}`}
          onclick={(e) => { e.stopPropagation(); onDiscard(s); }}
        >{#if retiring}<span class="spinner" aria-hidden="true"></span>{:else}×{/if}</button>
      {/if}
    {/if}
  </div>
  <div class="sess-line2">
    <span class="seat-chip {seatChipClass}">{seatChipText}</span>
    <span class="sess-status-str">{statusText}</span>
  </div>
</div>

<style>
  .session-row {
    display: flex;
    flex-direction: column;
    gap: 4px;
    width: calc(100% - 4px);
    margin: 1px 2px;
    padding: 7px 7px 7px 10px;
    background: transparent;
    border: none;
    border-radius: 8px;
    cursor: pointer;
    text-align: left;
    font: inherit;
  }
  .session-row:hover {
    background: var(--bg-chip);
  }
  .session-row.selected {
    background: var(--accent-sel);
    box-shadow: inset 2px 0 0 var(--accent);
  }
  .session-row:focus-visible {
    outline: 2px solid var(--accent-border);
    outline-offset: -2px;
  }
  .session-row.retiring {
    opacity: 0.6;
  }

  .sess-line1 {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .row-actions { display: inline-flex; flex: 0 0 auto; }
  .sess-line2 {
    display: flex;
    align-items: center;
    gap: 5px;
    padding-left: 12px;
  }

  .status-dot {
    width: 6px;
    height: 6px;
    border-radius: var(--r-pill);
    flex: 0 0 auto;
  }
  .status-dot.idle  { background: var(--idle); }
  .status-dot.error { background: var(--bad); }
  .status-dot.baker-dot {
    background: var(--violet);
    box-shadow: 0 0 6px color-mix(in srgb, var(--violet) 45%, transparent);
  }

  .sess-name {
    font: 520 12px/1.1 var(--font-ui);
    color: var(--fg-2);
    flex: 1 1 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .seat-chip {
    padding: 2px 5px;
    border-radius: var(--r-xs);
    background: var(--bg-chip);
    color: var(--fg-dim);
    font: 500 9px/1 var(--font-mono);
    letter-spacing: 0.04em;
  }
  .seat-chip.baker-chip {
    background: var(--violet-bg);
    color: var(--violet);
  }
  .sess-status-str {
    font: 400 10px/1 var(--font-mono);
    color: var(--fg-faint);
    margin-left: auto;
  }

  .lifetime-dot {
    width: 8px;
    height: 8px;
    border-radius: var(--r-pill);
    border: 1px solid var(--line-strong);
    background: transparent;
    padding: 0;
    cursor: pointer;
    flex: 0 0 auto;
    opacity: 0.45;
  }
  .lifetime-dot:hover { opacity: 1; }
  .lifetime-dot.living { background: var(--warn); border-color: transparent; opacity: 1; }
  .lifetime-dot.burn { background: var(--bad); border-color: transparent; opacity: 1; }
  .lifetime-dot:disabled { cursor: default; opacity: 0.3; }

  .discard-btn {
    display: flex;
    align-items: center;
    justify-content: center;
    flex: 0 0 auto;
    width: 14px;
    height: 14px;
    line-height: 1;
    border: none;
    background: transparent;
    color: var(--fg-fainter);
    cursor: pointer;
    padding: 0;
    font: 400 13px/1 var(--font-ui);
    border-radius: var(--r-xs);
  }
  .discard-btn:hover {
    color: var(--bad);
    background: var(--bad-bg);
  }
  .discard-btn:disabled {
    cursor: default;
    color: var(--fg-fainter);
    background: transparent;
  }

  /* Discard in flight — a small spinning ring in place of ×. */
  .spinner {
    display: inline-block;
    width: 8px;
    height: 8px;
    border: 1.5px solid var(--fg-fainter);
    border-top-color: var(--fg-dim);
    border-radius: var(--r-pill);
    animation: spin 700ms linear infinite;
  }
  @keyframes spin {
    to { transform: rotate(360deg); }
  }

  .lifetime-tag {
    padding: 2px 4px;
    border-radius: var(--r-xs);
    font: 400 9.5px/1 var(--font-mono);
    flex: 0 0 auto;
  }
  .retired-tag {
    background: var(--bg-chip);
    color: var(--fg-ghost);
  }
</style>
