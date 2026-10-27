<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import { onEvent } from "$lib/events";
  import { node, type HeartStats } from "$lib/node";

  // PANELS.md "Knowledge Heart", built only from GET /heart.
  //
  // The spec's four tiles assume projections exist. They do not: the store is
  // one append-only `events` table, so there is no L0–L4 table here and no
  // "stale projections" tile. Rendering those with placeholder rows would be
  // exactly the silent-staleness defect the panel is supposed to expose, so
  // the absence is stated in one line instead.
  //
  // Verification is a button, never a poll. The node re-hashes every event on
  // ?verify=1, and a result goes stale the moment anything is appended — so a
  // new event clears the last result back to "not checked" rather than leaving
  // a green tick standing over data it never saw.

  let stats = $state<HeartStats | null>(null);
  let loading = $state(true);
  let loadError = $state("");
  let verifying = $state(false);
  let verifyError = $state("");
  /** Cleared whenever the store changes — see note above. */
  let chain = $state<HeartStats["chain"] | null>(null);
  let chainStaleNote = $state("");

  let reloadTimer: ReturnType<typeof setTimeout> | null = null;
  let stopEvents: (() => void) | null = null;

  async function load(opts: { quiet?: boolean } = {}) {
    if (!opts.quiet) {
      loading = true;
      loadError = "";
    }
    try {
      // Never ?verify=1 on a refresh — that would turn a stats poll into a
      // full re-hash of the store.
      stats = await node.heart();
      if (opts.quiet) loadError = "";
    } catch (e) {
      loadError = `${e}`;
    } finally {
      loading = false;
    }
  }

  async function verify() {
    if (verifying) return;
    verifyError = "";
    verifying = true;
    try {
      const fresh = await node.heart(true);
      stats = fresh;
      chain = fresh.chain ?? null;
      chainStaleNote = "";
    } catch (e) {
      verifyError = `${e}`;
    } finally {
      verifying = false;
    }
  }

  function scheduleReload() {
    if (reloadTimer) clearTimeout(reloadTimer);
    reloadTimer = setTimeout(() => {
      reloadTimer = null;
      void load({ quiet: true });
    }, 400);
  }

  onMount(() => {
    void load();
    // Any append changes the counts — and invalidates a previous verify.
    stopEvents = onEvent(null, () => {
      if (chain) {
        chain = null;
        chainStaleNote = "Store changed since the last check — verify again.";
      }
      scheduleReload();
    });
  });

  onDestroy(() => {
    stopEvents?.();
    if (reloadTimer) clearTimeout(reloadTimer);
  });

  const nf = new Intl.NumberFormat("en-US");

  /** Binary units — this is a file on disk, not a marketing figure. */
  function bytes(n: number): string {
    if (n < 1024) return `${n} B`;
    const units = ["KiB", "MiB", "GiB", "TiB"];
    let v = n / 1024;
    let i = 0;
    while (v >= 1024 && i < units.length - 1) {
      v /= 1024;
      i++;
    }
    return `${v < 10 ? v.toFixed(1) : Math.round(v)} ${units[i]}`;
  }

  function when(ts: string | null): string {
    return ts ? ts.slice(0, 19).replace("T", " ") : "—";
  }
</script>

<div class="heart">
  <header class="h-hd">
    <h1>Knowledge Heart</h1>
    <p class="subhead">
      Append-only. Physical worktrees are disposable; these streams are not.
    </p>
  </header>

  {#if loading}
    <p class="muted">Reading store stats…</p>
  {:else if loadError && !stats}
    <p class="bad">{loadError}</p>
  {:else if stats}
    {#if loadError}
      <p class="bad">Counts may be stale — refresh failed: {loadError}</p>
    {/if}

    <div class="tiles">
      <div class="tile">
        <span class="t-label">Streams</span>
        <span class="t-value">{nf.format(stats.streams)}</span>
        <span class="t-note">
          {stats.streams === 0 ? "nothing onboarded yet" : "with at least one event"}
        </span>
      </div>

      <div class="tile">
        <span class="t-label">Events</span>
        <span class="t-value">{nf.format(stats.events)}</span>
        <span class="t-note">append-only · never rewritten</span>
      </div>

      <div class="tile">
        <span class="t-label">On disk</span>
        <span class="t-value" class:unknown={stats.db_bytes === null}>
          {stats.db_bytes === null ? "—" : bytes(stats.db_bytes)}
        </span>
        <span class="t-note">
          {stats.db_bytes === null
            ? "not on disk — in-memory store"
            : "SQLite + WAL sidecars"}
        </span>
      </div>

      <div class="tile">
        <span class="t-label">Hash chain</span>
        {#if chain}
          <span class="t-value" class:ok={chain.verified} class:bad-v={!chain.verified}>
            {chain.verified ? "verified" : `${chain.broken.length} broken`}
          </span>
          <span class="t-note">{nf.format(chain.streams_checked)} streams checked</span>
        {:else}
          <span class="t-value unknown">not checked</span>
          <span class="t-note">{chainStaleNote || "verification is on demand"}</span>
        {/if}
      </div>
    </div>

    <div class="verify-row">
      <button type="button" class="btn" disabled={verifying} onclick={verify}>
        {verifying ? "Verifying…" : "Verify hash chain"}
      </button>
      <span class="verify-note">
        Re-reads and re-hashes every event, so it is not run on a refresh.
      </span>
    </div>
    {#if verifyError}<p class="bad">{verifyError}</p>{/if}

    {#if chain && !chain.verified}
      <ul class="broken">
        {#each chain.broken as b (b.stream_id)}
          <li>
            <code class="sid">{b.stream_id}</code>
            <span class="reason">{b.reason}</span>
          </li>
        {/each}
      </ul>
    {/if}

    <p class="span">
      {#if stats.first_ts && stats.last_ts}
        Oldest event <code>{when(stats.first_ts)}</code> · newest
        <code>{when(stats.last_ts)}</code>
      {:else}
        No events recorded yet.
      {/if}
    </p>

    <p class="note">
      Projections are not recorded yet — the store is a single append-only
      <code>events</code> table, so there are no L0–L4 layers to show freshness for. This panel
      lists nothing it cannot measure; when layers exist, their staleness will be shown here
      rather than assumed.
    </p>
  {/if}
</div>

<style>
  .heart {
    display: flex;
    flex-direction: column;
    gap: 15px;
    min-width: 0;
  }

  .h-hd { display: flex; flex-direction: column; gap: 5px; }
  .h-hd h1 {
    margin: 0;
    font: 600 17px/1.3 var(--font-ui);
    letter-spacing: -0.015em;
    color: var(--fg);
  }
  .subhead {
    margin: 0;
    font: 400 12px/1.55 var(--font-ui);
    color: var(--fg-faint);
  }

  .muted { color: var(--fg-faint); font: 400 12px/1.5 var(--font-ui); margin: 0; }
  .bad { color: var(--bad); font: 400 12px/1.5 var(--font-ui); margin: 0; }

  .tiles {
    display: grid;
    grid-template-columns: repeat(4, minmax(0, 1fr));
    gap: 12px;
  }

  .tile {
    display: flex;
    flex-direction: column;
    justify-content: center;
    gap: 6px;
    min-width: 0;
    min-height: 106px;
    box-sizing: border-box;
    padding: 16px;
    border: 1px solid var(--line-soft);
    border-radius: var(--r-4xl);
    background: var(--bg-panel);
    box-shadow: var(--shadow-chip);
  }
  .t-label {
    font: 600 9.5px/1 var(--font-ui);
    text-transform: uppercase;
    letter-spacing: 0.07em;
    color: var(--fg-fainter);
  }
  .t-value {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font: 600 21px/1.1 var(--font-mono);
    letter-spacing: -0.025em;
    color: var(--fg);
    font-variant-numeric: tabular-nums;
  }
  .t-value.unknown { color: var(--fg-faint); }
  .t-value.ok { color: var(--ok); }
  .t-value.bad-v { color: var(--bad); }
  .t-note {
    font: 400 10px/1.4 var(--font-ui);
    color: var(--fg-faint);
  }

  .verify-row {
    display: flex;
    align-items: center;
    gap: 10px;
    flex-wrap: wrap;
    padding: 2px 0;
  }
  .btn {
    flex: 0 0 auto;
    min-height: 34px;
    padding: 7px 12px;
    border-radius: var(--r-xl);
    border: 1px solid var(--line);
    background: var(--bg-panel);
    color: var(--fg-2);
    font: 500 11.5px/1 var(--font-ui);
    cursor: pointer;
  }
  .btn:hover:not(:disabled) { background: var(--bg-chip); }
  .btn:focus-visible {
    outline: 2px solid var(--accent-border);
    outline-offset: 2px;
  }
  .btn:disabled { opacity: 0.45; cursor: default; }
  .verify-note {
    font: 400 10.5px/1.4 var(--font-ui);
    color: var(--fg-fainter);
  }

  .broken {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .broken li {
    display: flex;
    gap: 8px;
    align-items: baseline;
    flex-wrap: wrap;
    padding: 8px 10px;
    border: 1px solid var(--bad-border);
    border-radius: var(--r-lg);
    background: var(--bad-bg);
  }
  .sid { font: 400 11px/1.3 var(--font-mono); color: var(--fg-2); }
  .reason { font: 400 11px/1.4 var(--font-ui); color: var(--bad); }

  .span {
    margin: 0;
    font: 400 10.5px/1.6 var(--font-mono);
    color: var(--fg-fainter);
    overflow-wrap: anywhere;
  }
  .span code { color: var(--fg-dim); }

  .note {
    margin: 0;
    padding: 13px 15px;
    border: 1px dashed var(--line-strong);
    border-radius: var(--r-2xl);
    background: var(--bg-panel);
    font: 400 11.5px/1.55 var(--font-ui);
    color: var(--fg-dim);
    max-width: 760px;
  }
  .note code { font-style: normal; color: var(--fg-2); }

  @media (max-width: 900px) {
    .tiles { grid-template-columns: repeat(2, minmax(0, 1fr)); }
  }

  @media (max-width: 480px) {
    .tiles { grid-template-columns: minmax(0, 1fr); }
    .tile { min-height: 92px; }
  }
</style>
