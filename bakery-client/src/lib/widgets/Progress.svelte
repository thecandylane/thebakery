<script lang="ts">
  import { payloadOf, type BakeryEvent } from "$lib/node";
  import type { Progress } from "$lib/transcript";

  // Event queue. Every BakeryEvent from the stream is shown; filter chips narrow
  // by family prefix client-side. Unknown families stay visible under "all" so
  // nothing is silently hidden.

  const FILTERS = ["all", "file.changed", "tool.called", "turn.*", "memory.*", "admin"] as const;
  type Filter = (typeof FILTERS)[number];

  let {
    events,
    progress,
  }: {
    events: BakeryEvent[];
    progress: Progress;
  } = $props();

  let activeFilter = $state<Filter>("all");

  const filtered = $derived(
    activeFilter === "all"
      ? events
      : events.filter((ev) => matchesFilter(activeFilter, ev.family)),
  );

  function matchesFilter(filter: Filter, family: string): boolean {
    switch (filter) {
      case "file.changed": return family === "agent.file.changed";
      case "tool.called":  return family === "agent.tool.called";
      case "turn.*":       return family.startsWith("agent.turn.");
      case "memory.*":     return family.startsWith("memory.");
      case "admin":        return !family.startsWith("agent.") && !family.startsWith("memory.");
      default:             return true;
    }
  }

  function familyColor(family: string): string {
    if (family.startsWith("agent.file.")) return "var(--ok)";
    if (family.startsWith("agent.tool.")) return "var(--fg-dim)";
    if (family.startsWith("agent.turn.")) return "var(--accent)";
    if (family.startsWith("memory."))     return "var(--violet)";
    return "var(--fg-ghost)";
  }

  function rowDetail(ev: BakeryEvent): string {
    const p = payloadOf(ev);
    if (ev.family === "policy.write_mode.set") {
      const m = typeof p.write_mode === "string" ? p.write_mode : typeof p.mode === "string" ? p.mode : "";
      return m ? `write mode → ${m}` : "write mode changed";
    }
    if (typeof p.file_path === "string" && p.file_path) return p.file_path;
    if (typeof p.tool === "string" && p.tool) return p.tool;
    if (typeof p.instruction === "string" && p.instruction) return String(p.instruction).slice(0, 120);
    if (typeof p.summary === "string" && p.summary) return String(p.summary).slice(0, 120);
    if (typeof p.reason === "string" && p.reason) return String(p.reason).slice(0, 120);
    return "";
  }

  function tsShort(ts: string): string {
    return ts.slice(11, 19);
  }

  function hashShort(hash: string): string {
    return hash.slice(0, 8);
  }
</script>

<div class="evt-widget">
  <div class="filter-row">
    {#each FILTERS as f}
      <button
        type="button"
        class="chip"
        class:active={activeFilter === f}
        onclick={() => { activeFilter = f; }}
      >{f}</button>
    {/each}
    <span class="spacer"></span>
    <span class="stats-note">
      {events.length} events{#if progress.turns} · {progress.turns} turns{/if}{#if progress.pending > 0}<span
          class="pending-ct"> · {progress.pending} pending</span>{/if}
    </span>
    <!-- TODO(H): hash-chain audit endpoint not yet built — verification not available -->
    <span class="chain-note" title="chain verification not yet wired">— chain</span>
  </div>

  <div class="tbl-wrap">
    {#if events.length === 0}
      <p class="empty">Waiting for events…</p>
    {:else if filtered.length === 0}
      <p class="empty">No events match this filter.</p>
    {:else}
      <div class="tbl">
        <div class="row hdr">
          <span class="cell seq">seq</span>
          <span class="cell ts">time</span>
          <span class="cell family">family</span>
          <span class="cell actor">actor</span>
          <span class="cell detail">detail</span>
          <span class="cell hash">hash</span>
        </div>
        {#each filtered as ev (ev.event_hash)}
          <div class="row">
            <span class="cell seq">{ev.seq}</span>
            <span class="cell ts">{tsShort(ev.ts)}</span>
            <span class="cell family" style="color:{familyColor(ev.family)}">{ev.family}</span>
            <span class="cell actor">{ev.actor}</span>
            <span class="cell detail">{rowDetail(ev)}</span>
            <span class="cell hash">{hashShort(ev.event_hash)}</span>
          </div>
        {/each}
      </div>
    {/if}
  </div>
</div>

<style>
  .evt-widget {
    display: flex;
    flex-direction: column;
    height: 100%;
    overflow: hidden;
  }

  /* ---- filter + stats row ---- */

  .filter-row {
    flex: 0 0 30px;
    display: flex;
    align-items: center;
    gap: 3px;
    padding: 0 10px;
    background: var(--bg-panel);
    border-bottom: 1px solid var(--line-chrome);
    overflow: hidden;
  }

  .chip {
    background: none;
    border: 1px solid transparent;
    cursor: pointer;
    font: 400 10px/1 var(--font-mono);
    padding: 2px 7px;
    border-radius: var(--r-pill);
    color: var(--fg-faint);
    white-space: nowrap;
    flex-shrink: 0;
  }
  .chip:hover { color: var(--fg-2); background: var(--bg-chip); }
  .chip.active {
    color: var(--accent-fg);
    background: var(--accent-bg);
    border-color: var(--accent-border);
  }

  .spacer { flex: 1 1 auto; }

  .stats-note {
    font: 400 10px/1 var(--font-mono);
    color: var(--fg-fainter);
    white-space: nowrap;
    flex-shrink: 0;
  }

  .pending-ct { color: var(--warn); }

  .chain-note {
    font: 400 10px/1 var(--font-mono);
    color: var(--fg-ghost);
    padding-left: 8px;
    flex-shrink: 0;
  }

  /* ---- table ---- */

  .tbl-wrap {
    flex: 1 1 auto;
    min-height: 0;
    overflow-y: auto;
    overflow-x: auto;
  }

  .empty {
    font: 400 11.5px/1.4 var(--font-ui);
    color: var(--fg-faint);
    font-style: italic;
    padding: 20px 14px;
    margin: 0;
  }

  .tbl {
    display: flex;
    flex-direction: column;
    min-width: 580px;
  }

  .row {
    display: flex;
    align-items: center;
    border-bottom: 1px solid var(--line-row);
    min-height: 22px;
  }

  .row:hover:not(.hdr) { background: var(--bg-raised); }

  .row.hdr {
    background: var(--bg-panel);
    position: sticky;
    top: 0;
    z-index: 1;
    border-bottom: 1px solid var(--line-strong);
  }

  .cell {
    display: block;
    padding: 3px 6px;
    font: 400 11px/1 var(--font-mono);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    min-width: 0;
    flex-shrink: 0;
    color: var(--fg-dim);
  }

  .row.hdr .cell {
    font: 500 9px/1 var(--font-ui);
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--fg-faint);
  }

  .cell.seq    { width: 4ch;  text-align: right; color: var(--fg-linenum); }
  .cell.ts     { width: 9ch; color: var(--fg-faint); }
  .cell.family { width: 22ch; }
  .cell.actor  { width: 14ch; color: var(--fg-dim); }
  .cell.detail { flex: 1 1 auto; min-width: 0; color: var(--fg-dim); }
  .cell.hash   { width: 9ch;  color: var(--fg-linenum); }
</style>
