<script lang="ts">
  import { onDestroy } from "svelte";
  import { onEvent } from "$lib/events";
  import {
    node,
    type ContextView,
    type MemoryView,
  } from "$lib/node";

  // Studio Memory & context widget. Only numbers the node measures:
  //   - prompt_chars (characters, never tokens — node has no tokenizer)
  //   - files_in_scope (jail = worktree)
  //   - refusals, with "showing n of total" when capped
  //   - hot summary from GET /memory
  // No L1–L4 rows, no token bar, no forget button (POST /memory/forget is out of scope).

  let {
    streamId,
  }: {
    streamId: string;
  } = $props();

  let memory = $state<MemoryView | null>(null);
  let context = $state<ContextView | null>(null);
  let loading = $state(true);
  let loadError = $state("");

  let reloadTimer: ReturnType<typeof setTimeout> | null = null;
  let stopEvents: (() => void) | null = null;
  let mountedFor: string | null = null;

  async function load(opts: { quiet?: boolean } = {}) {
    if (!streamId) return;
    if (!opts.quiet) {
      loading = true;
      loadError = "";
    }
    try {
      const [m, c] = await Promise.all([
        node.memory(streamId),
        node.context(streamId),
      ]);
      memory = m;
      context = c;
      if (opts.quiet) loadError = "";
    } catch (e) {
      loadError = `${e}`;
    } finally {
      loading = false;
    }
  }

  function scheduleReload() {
    if (reloadTimer) clearTimeout(reloadTimer);
    reloadTimer = setTimeout(() => {
      reloadTimer = null;
      void load({ quiet: true });
    }, 400);
  }

  // Rebind when the studio stream changes (rare on this route, but safe).
  $effect(() => {
    const id = streamId;
    if (!id) return;
    if (mountedFor === id) return;
    mountedFor = id;
    stopEvents?.();
    memory = null;
    context = null;
    void load();
    stopEvents = onEvent(id, () => scheduleReload());
  });

  onDestroy(() => {
    stopEvents?.();
    if (reloadTimer) clearTimeout(reloadTimer);
  });

  const nf = new Intl.NumberFormat("en-US");

  function timeOf(ts: string): string {
    // ISO → HH:MM when possible; otherwise leave as-is.
    if (ts.length >= 16) return ts.slice(11, 16);
    return ts;
  }

  const refusalCaption = $derived.by(() => {
    if (!context) return "";
    const n = context.refusals.length;
    const total = context.refusals_total;
    if (total > n) return `showing ${n} of ${total}`;
    if (total === 0) return "none yet";
    return `${n} refusal${n === 1 ? "" : "s"}`;
  });
</script>

<div class="mem-w">
  {#if loading && !context && !memory}
    <p class="muted">Loading memory &amp; context…</p>
  {:else if loadError && !context && !memory}
    <p class="bad">{loadError}</p>
  {:else}
    {#if loadError}
      <p class="bad">Refresh failed — showing last good data: {loadError}</p>
    {/if}

    <!-- Prompt composition: characters only. -->
    <section class="sec">
      <h2 class="sec-h">Prompt composition</h2>
      {#if context}
        <div class="chars">
          <div class="c-row">
            <span class="c-label">System</span>
            <span class="c-val">{nf.format(context.prompt_chars.system)}</span>
            <span class="c-unit">chars</span>
          </div>
          <div class="c-row">
            <span class="c-label">Hot memory</span>
            <span class="c-val">{nf.format(context.prompt_chars.memory)}</span>
            <span class="c-unit">chars</span>
          </div>
          <div class="c-row total">
            <span class="c-label">Total measured</span>
            <span class="c-val">{nf.format(context.prompt_chars.total)}</span>
            <span class="c-unit">chars</span>
          </div>
        </div>
        <p class="honest">
          Character counts of assembled pieces — not tokens. The node has no tokenizer and no
          model window size, so there is no used/total bar.
        </p>
        <p class="limits">
          Limits · {context.limits.max_tool_steps === null ? "no fixed step cap" : `max steps ${context.limits.max_tool_steps}`}
          · hot scan {context.limits.hot_memory_scan_events}
          · hot max lines {context.limits.hot_memory_max_lines}
        </p>
      {:else}
        <p class="empty">Context not loaded.</p>
      {/if}
    </section>

    <!-- Jail scope + refusals (the high-value part). -->
    <section class="sec">
      <h2 class="sec-h">Files in scope · jail = worktree</h2>
      {#if context}
        <ul class="scope">
          {#each context.files_in_scope as f (f.glob)}
            <li class="ok-row">
              <span class="mark">✓</span>
              <code class="glob">{f.glob}</code>
              <span class="mode">{f.mode}</span>
            </li>
          {/each}
        </ul>

        <div class="ref-hd">
          <span>Refusals</span>
          <span class="ref-cap">{refusalCaption}</span>
        </div>
        {#if context.refusals.length === 0}
          <p class="empty">No refusals on this stream yet — the jail has not blocked anything here.</p>
        {:else}
          <ul class="refusals">
            {#each context.refusals as r, i (`${r.at}-${r.tool}-${i}`)}
              <li>
                <span class="mark bad-m">✕</span>
                <span class="r-body">
                  <code class="r-path">{r.path ?? r.tool}</code>
                  <span class="r-reason">{r.reason}</span>
                </span>
                <span class="r-meta">{r.tool} · {timeOf(r.at)}</span>
              </li>
            {/each}
          </ul>
        {/if}
      {:else}
        <p class="empty">Context not loaded.</p>
      {/if}
    </section>

    <!-- Hot summary from GET /memory. -->
    <section class="sec">
      <h2 class="sec-h">Hot memory</h2>
      {#if memory}
        <p class="hot-meta">
          {memory.hot.lines.length}/{memory.hot.max_lines} lines
          · scan {memory.hot.scan_events}
          · {memory.hot.paths.length} path{memory.hot.paths.length === 1 ? "" : "s"}
          {#if memory.hot.prior_accept_path}
            · prior accept <code>{memory.hot.prior_accept_path}</code>
          {/if}
        </p>
        {#if memory.hot.lines.length === 0}
          <p class="empty">Nothing injected this turn — empty prior-session projection.</p>
        {:else}
          <ul class="hot-lines">
            {#each memory.hot.lines as line, i (i)}
              <li>{line}</li>
            {/each}
          </ul>
        {/if}
        {#if memory.entries.length === 0}
          <p class="entries-note">
            Durable L1–L4 entries: none on the wire. Not invented.
          </p>
        {/if}
      {:else}
        <p class="empty">Memory not loaded.</p>
      {/if}
    </section>
  {/if}
</div>

<style>
  .mem-w {
    position: absolute;
    inset: 0;
    overflow: auto;
    padding: 12px;
    display: flex;
    flex-direction: column;
    gap: 14px;
    background: var(--bg-app);
  }

  .muted { margin: 0; font: 400 11.5px/1.4 var(--font-ui); color: var(--fg-faint); }
  .bad { margin: 0; font: 400 11.5px/1.4 var(--font-ui); color: var(--bad); }

  .sec { display: flex; flex-direction: column; gap: 6px; }
  .sec-h {
    margin: 0;
    font: 600 10.5px/1 var(--font-ui);
    letter-spacing: 0.07em;
    text-transform: uppercase;
    color: var(--fg-fainter);
  }

  .chars {
    display: flex;
    flex-direction: column;
    gap: 2px;
    border: 1px solid var(--line);
    border-radius: var(--r-md);
    background: var(--bg-panel);
    padding: 6px 0;
  }
  .c-row {
    display: flex;
    align-items: baseline;
    gap: 8px;
    padding: 4px 10px;
  }
  .c-row.total {
    border-top: 1px solid var(--line-soft);
    margin-top: 2px;
    padding-top: 6px;
  }
  .c-label {
    flex: 1 1 auto;
    font: 400 11.5px/1.3 var(--font-ui);
    color: var(--fg-dim);
  }
  .c-val {
    font: 500 12px/1.2 var(--font-mono);
    color: var(--fg);
    font-variant-numeric: tabular-nums;
  }
  .c-row.total .c-val { color: var(--accent-fg); }
  .c-unit {
    font: 400 10px/1.2 var(--font-mono);
    color: var(--fg-fainter);
    min-width: 2.5em;
  }

  .honest, .limits {
    margin: 0;
    font: 400 10.5px/1.4 var(--font-ui);
    color: var(--fg-fainter);
  }
  .limits { font-family: var(--font-mono); font-size: 10px; }

  .scope {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .ok-row {
    display: flex;
    align-items: baseline;
    gap: 6px;
    font: 400 11px/1.9 var(--font-mono);
    color: var(--fg-dim);
  }
  .mark { color: var(--ok); flex: 0 0 auto; }
  .mark.bad-m { color: var(--bad); }
  .glob {
    color: var(--fg-body);
    word-break: break-all;
  }
  .mode { color: var(--fg-faint); }

  .ref-hd {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    margin-top: 4px;
    font: 500 11px/1.3 var(--font-ui);
    color: var(--fg-2);
  }
  .ref-cap {
    font: 400 10.5px/1.2 var(--font-mono);
    color: var(--fg-faint);
  }

  .refusals {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .refusals li {
    display: grid;
    grid-template-columns: auto 1fr auto;
    gap: 6px 8px;
    align-items: start;
    padding: 6px 8px;
    border: 1px solid var(--bad-border);
    border-radius: var(--r-md);
    background: var(--bad-bg);
    font: 400 11px/1.4 var(--font-mono);
  }
  .r-body {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }
  .r-path { color: var(--fg-body); word-break: break-all; }
  .r-reason { color: var(--bad); font-family: var(--font-ui); font-size: 11px; }
  .r-meta {
    color: var(--fg-fainter);
    font-size: 10px;
    white-space: nowrap;
  }

  .hot-meta {
    margin: 0;
    font: 400 10.5px/1.4 var(--font-mono);
    color: var(--fg-faint);
  }
  .hot-meta code { color: var(--fg-dim); }

  .hot-lines {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 3px;
  }
  .hot-lines li {
    padding: 5px 8px;
    border: 1px solid var(--line-soft);
    border-radius: var(--r-sm);
    background: var(--bg-panel);
    font: 400 11px/1.45 var(--font-mono);
    color: var(--fg-body);
    white-space: pre-wrap;
    word-break: break-word;
  }

  .empty {
    margin: 0;
    padding: 8px 10px;
    border: 1px dashed var(--line-strong);
    border-radius: var(--r-md);
    font: 400 11px/1.45 var(--font-ui);
    color: var(--fg-dim);
    font-style: italic;
  }
  .entries-note {
    margin: 2px 0 0;
    font: 400 10.5px/1.4 var(--font-ui);
    color: var(--fg-fainter);
    font-style: italic;
  }
</style>
