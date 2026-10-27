<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import { onStatus, type ConnStatus } from "$lib/events";
  import { toHandle } from "$lib/handle";
  import { NODE, node, type NodeInfo } from "$lib/node";
  import { studioStore } from "$lib/studioStore.svelte";

  // PANELS.md "Nodes & streams". Identity comes from GET /node (fallback
  // policy); per-stream ownership comes from studioStore (07c-LIVE-STATE's
  // one client topology store, backed by GET /studio/state) — no shadow
  // /streams fetch here. There is still no multi-node discovery endpoint
  // (NODE_API.md §9 lists GET /nodes and POST /stream/handoff as *needed*,
  // not built), so this renders exactly one card: the node this client is
  // talking to. Peer cards are never invented, replication lag is never
  // shown (nothing measures it), and Hand off is rendered disabled rather
  // than pretending a transfer path exists.
  //
  // This is an ownership-audit surface, not one of 07c's four active-work
  // views (sidebar/floor/baker/workbench), so it deliberately reads
  // studioStore.sockets — ALL sockets, retired included — instead of
  // .activeSockets: a retired stream can still carry a stale ownership
  // claim (see "foreign", below), and hiding it here would hide the exact
  // drift this panel exists to surface. Rows are tagged "retired" so the
  // audit stays honest about state.
  //
  // CONTROL_PLANE.md §149: replication, leases, fencing and handoff are not
  // built. §170: ungraceful handoff is the primary case and is unsolved.
  // The notes at the bottom say both, out loud.

  let info = $state<NodeInfo | null>(null);
  let loading = $state(true);
  let loadError = $state("");
  let status = $state<ConnStatus>("closed");

  const stopStatus = onStatus((s) => {
    status = s;
  });
  onDestroy(stopStatus);

  async function load() {
    loading = true;
    loadError = "";
    try {
      info = await node.info();
    } catch (e) {
      loadError = `${e}`;
    } finally {
      loading = false;
    }
  }

  onMount(() => {
    load();
    studioStore.ensureStarted();
  });

  // Loopback address is real evidence the client and node share a machine —
  // that is the "client + node" role, not a guess.
  const isLoopback = $derived(NODE.startsWith("127.0.0.1") || NODE.startsWith("localhost"));

  const owned = $derived(
    info ? studioStore.sockets.filter((s) => s.owner_node_id === info!.node_id) : [],
  );
  // Claims naming a node_id this client is not talking to. Normally empty —
  // but ~/.bakery/node.json regenerating (corrupt file) mints a new node_id
  // and strands the old claims, so show them rather than silently dropping.
  const foreign = $derived(
    info ? studioStore.sockets.filter((s) => s.owner_node_id && s.owner_node_id !== info!.node_id) : [],
  );
  const unclaimed = $derived(studioStore.sockets.filter((s) => !s.owner_node_id));

  const dotColor = $derived(
    status === "open" ? "var(--ok)" : status === "connecting" ? "var(--warn)" : "var(--bad)",
  );
</script>

<div class="nodes">
  <header class="n-hd">
    <h1>Nodes &amp; streams</h1>
    <p class="subhead">
      Exactly one owning node per stream. Handoff is graceful-only today — an ungraceful drop still
      needs lease + fence.
    </p>
  </header>

  {#if loading}
    <p class="muted">Reading node identity and stream ownership…</p>
  {:else if loadError}
    <p class="bad">{loadError}</p>
  {:else if info}
    <article class="node-card">
      <div class="nc-hd">
        <span class="dot" style="background: {dotColor}" aria-hidden="true"></span>
        <strong class="host">This node</strong>
        {#if isLoopback}
          <span class="chip" title="The client reaches this node on a loopback address, so they share a machine.">
            client + node
          </span>
        {/if}
        <span class="spacer"></span>
        <span class="owner-ct" style="color: {dotColor}">
          owner of {owned.length}
        </span>
      </div>

      <p class="nc-meta">
        <code>{info.node_id}</code> · {NODE} · fallback <code>{info.stream_fallback}</code> ·
        worktrees under <code>{info.worktree_base}</code>
      </p>

      {#if owned.length}
        <ul class="stream-rows">
          {#each owned as s (s.stream_id)}
            <li class="stream-row">
              <code class="sid">{s.stream_id}</code>
              {#if s.discarded}
                <span class="tag">retired</span>
              {/if}
              {#if s.in_place}
                <span class="tag">in place</span>
              {/if}
              {#if !s.exists_on_disk}
                <span class="tag bad-tag">tree gone</span>
              {/if}
              <span class="spacer"></span>
              <a class="btn ghost" href={`/w/${toHandle(s.stream_id)}`}>Open</a>
              <button
                type="button"
                class="btn ghost"
                disabled
                title="No transfer path exists — POST /stream/handoff is not built (NODE_API.md §9)."
              >
                Hand off
              </button>
            </li>
          {/each}
        </ul>
      {:else}
        <p class="muted row-empty">This node owns no streams yet.</p>
      {/if}
    </article>

    {#if foreign.length}
      <article class="node-card stale">
        <div class="nc-hd">
          <span class="dot" style="background: var(--warn)" aria-hidden="true"></span>
          <strong class="host">Claimed by another node id</strong>
          <span class="spacer"></span>
          <span class="owner-ct" style="color: var(--warn)">{foreign.length}</span>
        </div>
        <p class="nc-meta warn-line">
          These streams carry an ownership claim naming a node id this client is not talking to.
          With one node running, that usually means <code>~/.bakery/node.json</code> was
          regenerated and the old claims are stranded — not that a peer is live. Reconcile is not
          built (CONTROL_PLANE.md §170).
        </p>
        <ul class="stream-rows">
          {#each foreign as s (s.stream_id)}
            <li class="stream-row">
              <code class="sid">{s.stream_id}</code>
              {#if s.discarded}
                <span class="tag">retired</span>
              {/if}
              <span class="spacer"></span>
              <code class="claim">{s.owner_node_id}</code>
            </li>
          {/each}
        </ul>
      </article>
    {/if}

    {#if unclaimed.length}
      <p class="muted">
        {unclaimed.length}
        {unclaimed.length === 1 ? "stream carries" : "streams carry"} no ownership claim.
      </p>
    {/if}

    <div class="notes">
      <p>
        <strong>Single node today.</strong> Peer nodes are not discovered — no topology endpoint
        exists (NODE_API.md §9), so this panel shows the one node this client is connected to and
        nothing else. An empty peer list here is the absence of a feature, not the absence of peers.
      </p>
      <p class="warn-line">
        <strong>lease + fence not built.</strong> Replication, leases, fencing and handoff are
        specified but unimplemented (CONTROL_PLANE.md §149). Ungraceful handoff — lid-close, sleep,
        or a network drop mid-write — is the primary case and is <em>not</em> solved (§170). Hand
        off is disabled above for that reason; nothing in this UI transfers a stream.
      </p>
    </div>
  {/if}
</div>

<style>
  .nodes {
    display: flex;
    flex-direction: column;
    gap: 14px;
    min-width: 0;
  }

  .n-hd { display: flex; flex-direction: column; gap: 5px; }
  .n-hd h1 {
    margin: 0;
    font: 600 17px/1.3 var(--font-ui);
    letter-spacing: -0.015em;
    color: var(--fg);
  }
  .subhead {
    margin: 0;
    font: 400 12px/1.55 var(--font-ui);
    color: var(--fg-faint);
    max-width: 720px;
  }

  .muted { color: var(--fg-faint); font: 400 12px/1.5 var(--font-ui); margin: 0; }
  .bad { color: var(--bad); font: 400 12px/1.5 var(--font-ui); margin: 0; }

  .node-card {
    min-width: 0;
    border: 1px solid var(--line-soft);
    border-radius: var(--r-4xl);
    background: var(--bg-panel);
    padding: 16px 18px;
    display: flex;
    flex-direction: column;
    gap: 11px;
    box-shadow: var(--shadow-chip);
  }
  .node-card.stale { border-color: var(--warn-border); }

  .nc-hd {
    display: flex;
    align-items: center;
    gap: 7px;
    min-width: 0;
  }
  .dot {
    flex: 0 0 auto;
    width: 8px;
    height: 8px;
    border-radius: var(--r-pill);
  }
  .host {
    min-width: 0;
    font: 600 13px/1.2 var(--font-ui);
    color: var(--fg);
  }
  .chip {
    padding: 2px 6px;
    border-radius: var(--r-sm);
    background: var(--bg-chip);
    color: var(--fg-dim);
    font: 400 10px/1 var(--font-mono);
  }
  .spacer { flex: 1 1 auto; }
  .owner-ct {
    font: 500 11px/1 var(--font-mono);
    flex-shrink: 0;
  }

  .nc-meta {
    margin: 0;
    padding: 9px 11px;
    border-radius: var(--r-xl);
    background: var(--bg-app);
    font: 400 10.5px/1.65 var(--font-mono);
    color: var(--fg-faint);
    overflow-wrap: anywhere;
  }
  .nc-meta code { color: var(--fg-dim); }
  .warn-line { color: var(--warn); font-family: var(--font-ui); font-size: 11px; line-height: 1.55; }
  .warn-line code { color: var(--warn); }

  .stream-rows {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
  }
  .stream-row {
    display: flex;
    align-items: center;
    gap: 7px;
    min-width: 0;
    padding: 9px 0;
    border-top: 1px solid var(--line-chrome);
    flex-wrap: wrap;
  }
  .sid {
    min-width: 0;
    overflow-wrap: anywhere;
    font: 400 11px/1.3 var(--font-mono);
    color: var(--fg-2);
  }
  .claim {
    min-width: 0;
    max-width: 55%;
    overflow-wrap: anywhere;
    font: 400 10px/1.3 var(--font-mono);
    color: var(--warn);
  }
  .tag {
    padding: 1px 5px;
    border-radius: var(--r-sm);
    background: var(--bg-chip);
    color: var(--fg-faint);
    font: 400 9.5px/1.4 var(--font-mono);
  }
  .tag.bad-tag { background: var(--bad-bg); color: var(--bad); }

  .row-empty { padding-top: 4px; font-style: italic; }

  .btn {
    flex-shrink: 0;
    min-height: 30px;
    padding: 6px 11px;
    border-radius: var(--r-lg);
    font: 500 11px/1 var(--font-ui);
    cursor: pointer;
    text-decoration: none;
    background: none;
    border: 1px solid var(--line);
    color: var(--fg-2);
  }
  .btn.ghost:hover:not(:disabled) { background: var(--bg-chip); text-decoration: none; }
  .btn:focus-visible {
    outline: 2px solid var(--accent-border);
    outline-offset: 2px;
  }
  .btn:disabled { opacity: 0.4; cursor: default; }

  .notes {
    margin-top: 2px;
    padding: 14px 16px;
    border: 1px dashed var(--line-strong);
    border-radius: var(--r-2xl);
    background: var(--bg-panel);
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 16px;
    max-width: 880px;
  }
  .notes p {
    margin: 0;
    font: 400 11px/1.55 var(--font-ui);
    color: var(--fg-fainter);
  }
  .notes strong { color: var(--fg-dim); font-weight: 600; }
  .notes .warn-line strong { color: var(--warn); }

  @media (max-width: 720px) {
    .node-card { padding: 14px; }
    .nc-hd { flex-wrap: wrap; }
    .notes { grid-template-columns: minmax(0, 1fr); gap: 10px; }
    .stream-row .spacer { display: none; }
    .stream-row a.btn { margin-left: auto; }
    .claim { flex-basis: 100%; max-width: 100%; }
  }

  @media (max-width: 480px) {
    .stream-row .sid { flex-basis: 100%; }
    .stream-row a.btn { margin-left: 0; }
  }
</style>
