<script lang="ts">
  import { onMount } from "svelte";
  import { fromHandle } from "$lib/handle";
  import { node, type MemoryView } from "$lib/node";
  import { loadLastSessionHandle } from "$lib/paneState";
  import { studioStore } from "$lib/studioStore.svelte";
  import { allConversations, bakerChats } from "$lib/conversations";
  import { memoryImport, node as nodeInfo, type MemoryImported } from "$lib/api.generated";
  import { isTauri } from "$lib/desktop";

  // Settings → Memory & storage. L0 Heart immutable; L1 from memory.written;
  // hot = agent projection. L2–L4 never invented (BRAIN_MAP).
  //
  // studioStore (07c-LIVE-STATE, GET /studio/state) is the only socket
  // inventory read here — no shadow /streams fetch. Retired sessions are
  // excluded from the "newest session" fallback below (a retired stream is
  // never a good default target to browse memory for), and the display
  // lookup goes through studioStore.findSocket rather than a manual .find.

  let memory = $state<MemoryView | null>(null);
  let streamId = $state<string | null>(null);
  let loading = $state(true);
  let loadError = $state("");
  let noStreamNote = $state("");
  let initialized = false;

  const streamMeta = $derived(streamId ? studioStore.findSocket(streamId) : null);
  /** Any conversation's memory can be read — not just the last one opened. */
  const choices = $derived(allConversations(studioStore.data));

  function resolveStreamId(): string | null {
    const handle = loadLastSessionHandle();
    if (handle) {
      try {
        return fromHandle(handle);
      } catch {
        /* garbage handle — fall through to recents */
      }
    }
    if (studioStore.activeSockets.length === 0) return null;
    // Newest activity first — last_ts is ISO and string-sortable.
    const sorted = [...studioStore.activeSockets].sort((a, b) => b.last_ts.localeCompare(a.last_ts));
    return sorted[0]?.stream_id ?? null;
  }

  async function load(chosen?: string) {
    loading = true;
    loadError = "";
    noStreamNote = "";
    memory = null;
    try {
      const id = chosen ?? resolveStreamId();
      streamId = id;
      if (!id) {
        noStreamNote =
          "No session selected. Open a studio session, then return here to inspect its hot memory.";
        return;
      }
      memory = await node.memory(id);
    } catch (e) {
      loadError = `${e}`;
    } finally {
      loading = false;
    }
  }

  onMount(() => {
    studioStore.ensureStarted();
  });

  // Import Claude Code memory (the move-in): a folder of Claude Code memory
  // notes into the Baker's one memory, through its main conversation;
  // again later, only what changed. Prefilled with Claude Code's folder for
  // the Bakery's own repo, from the node's home and source root. Never on
  // the phone.
  const phone = isTauri();
  let importFolder = $state("");
  let importing = $state(false);
  let imported = $state<MemoryImported | null>(null);
  let importError = $state("");
  const bakerMain = $derived.by(() => {
    const active = bakerChats(studioStore.data).active;
    return active.find((c) => c.main) ?? active[0] ?? null;
  });
  onMount(() => {
    if (phone) return;
    nodeInfo().then(
      (info) => {
        const root = info.bakery_source_root ?? "";
        const home = info.worktree_base.includes("/.bakery") ? info.worktree_base.split("/.bakery")[0] : "";
        if (!importFolder && root && home) importFolder = `${home}/.claude/projects/${root.replace(/[^A-Za-z0-9]/g, "-")}/memory`;
      },
      () => {},
    );
  });
  async function runImport() {
    if (!bakerMain) return;
    importing = true;
    importError = "";
    imported = null;
    try {
      imported = await memoryImport({ stream_id: bakerMain.streamId, folder: importFolder.trim() });
    } catch (e) {
      importError = e instanceof Error ? e.message : String(e);
    } finally {
      importing = false;
    }
  }

  // Wait for the store's own fetch to settle before the first load() so
  // resolveStreamId's no-saved-handle fallback reads real topology instead
  // of whatever was there (nothing) before /studio/state's first response.
  // ensureStarted() is called again here (idempotent) rather than trusting
  // effect-registration order against the onMount above — it flips
  // studioStore.loading true synchronously (set inside pump(), before any
  // await), so this always observes a real in-flight fetch on the very
  // first mount anywhere in the app, not the store's unstarted default.
  $effect(() => {
    if (initialized) return;
    studioStore.ensureStarted();
    if (studioStore.loading) return;
    initialized = true;
    void load();
  });
</script>

<!-- Settings rows (settings.css): one section per memory layer, each fact a
     row with its value on the right. Agent files follow on the same page. -->
<div class="set-panel">
  <p class="set-lead">
    What gets remembered, at which layer, and who can read it. Ground truth is immutable —
    retention applies to projections and sidecars, never to the mutation log. Below it, every file an
    agent keeps outside your projects.
  </p>

  {#if !phone}
    <section class="set-section" data-testid="memory-import">
      <h2 class="set-section-h">Import Claude Code memory</h2>
      <div class="set-row">
        <span class="set-row-text">
          <label class="set-row-title" for="memory-import-folder">Folder</label>
          <span class="set-row-desc">into the Baker's memory</span>
        </span>
      </div>
      <div class="import-row">
        <input id="memory-import-folder" class="set-input folder" bind:value={importFolder} spellcheck="false" />
        <button
          type="button"
          class="set-btn primary"
          disabled={importing || !importFolder.trim() || !bakerMain}
          onclick={() => void runImport()}
        >{importing ? "Importing…" : "Import"}</button>
      </div>
      {#if imported}
        <p class="set-status" data-testid="memory-imported">
          {imported.imported.length} new or changed · {imported.unchanged.length} already the same{imported.skipped.length ? ` · ${imported.skipped.length} skipped` : ""}
        </p>
        {#if imported.skipped.length > 0}
          <ul class="set-list">
            {#each imported.skipped as why (why)}
              <li>{why}</li>
            {/each}
          </ul>
        {/if}
      {/if}
      {#if importError}<p class="set-error">{importError}</p>{/if}
      {#if !bakerMain}<p class="set-muted">The Baker has no conversation yet.</p>{/if}
    </section>
  {/if}

  {#if loading}
    <p class="set-muted">Reading memory…</p>
  {:else if loadError}
    <p class="set-error">{loadError}</p>
  {:else}
    <!-- L0 / Heart — the only durable layer that exists. -->
    <section class="set-section">
      <h2 class="set-section-h">L0 · Mutation log</h2>
      <div class="set-row">
        <span class="set-row-lead">
          <span class="set-chip mono">L0</span>
          <span class="set-row-text">
            <span class="set-row-title">Heart (events)</span>
            <span class="set-row-desc">
              Ground truth. Append-only, never pruned from this UI. Export and chain verify live
              on Knowledge Heart — not here.
            </span>
          </span>
        </span>
        <span class="set-value">forever · immutable</span>
      </div>
    </section>

    <section class="set-section">
      <h2 class="set-section-h">Hot memory · session projection</h2>
      {#if choices.length > 0}
        <div class="set-row">
          <span class="set-row-text"><label class="set-row-title" for="memory-conversation">Conversation</label></span>
          <select
            id="memory-conversation"
            class="set-select conversation"
            value={streamId ?? ""}
            onchange={(e) => void load(e.currentTarget.value)}
            data-testid="memory-conversation"
          >
            {#if !streamId}<option value="" disabled>Choose one…</option>{/if}
            {#each choices as c (c.streamId)}
              <option value={c.streamId}>{c.main ? "Baker — main conversation" : c.place ? `${c.title} · ${c.place}` : c.title}{c.archived ? " (archived)" : ""}</option>
            {/each}
          </select>
        </div>
      {/if}
      {#if noStreamNote}
        <p class="set-muted">{choices.length ? "Choose a conversation above to read its memory." : noStreamNote}</p>
      {:else if memory}
        <div class="set-row">
          <span class="set-row-text">
            <span class="set-row-title">Stream</span>
            <span class="set-mono">{memory.stream_id}</span>
          </span>
          <code class="set-value">{streamMeta ? `${streamMeta.project || streamMeta.name} · ${streamMeta.branch}` : memory.stream_id}</code>
        </div>
        <div class="set-row">
          <span class="set-row-text"><span class="set-row-title">Scan window</span><span class="set-row-desc">events looked back</span></span>
          <span class="set-value big">{memory.hot.scan_events}</span>
        </div>
        <div class="set-row">
          <span class="set-row-text"><span class="set-row-title">Max lines</span><span class="set-row-desc">injected cap</span></span>
          <span class="set-value big">{memory.hot.max_lines}</span>
        </div>
        <div class="set-row">
          <span class="set-row-text"><span class="set-row-title">Lines now</span><span class="set-row-desc">in the projection</span></span>
          <span class="set-value big">{memory.hot.lines.length}</span>
        </div>
        <div class="set-row">
          <span class="set-row-text"><span class="set-row-title">Paths</span><span class="set-row-desc">recently touched</span></span>
          <span class="set-value big">{memory.hot.paths.length}</span>
        </div>
        {#if memory.hot.prior_accept_path}
          <div class="set-row">
            <span class="set-row-text"><span class="set-row-title">Prior accept path</span></span>
            <code class="set-value">{memory.hot.prior_accept_path}</code>
          </div>
        {/if}

        {#if memory.hot.lines.length === 0}
          <p class="set-muted">No hot lines yet — the agent has nothing prior-session to inject.</p>
        {:else}
          <ul class="lines">
            {#each memory.hot.lines as line, i (i)}
              <li>{line}</li>
            {/each}
          </ul>
        {/if}

        {#if memory.hot.paths.length > 0}
          <p class="set-subhead">Recent paths</p>
          <ul class="set-list paths">
            {#each memory.hot.paths as p (p)}
              <li>{p}</li>
            {/each}
          </ul>
        {/if}
      {/if}
    </section>

    <section class="set-section">
      <h2 class="set-section-h">Durable · L1 notes</h2>
      {#if !memory}
        <p class="set-muted">Open a session to list L1 notes.</p>
      {:else if memory.entries.length === 0}
        <p class="set-muted">
          No L1 notes on this stream yet. Write via
          <code>POST /memory/write</code> (six-month filter — BRAIN_MAP). L2–L4 not built.
        </p>
      {:else}
        <ul class="set-rows">
          {#each memory.entries as e (e.id)}
            <li class="set-row top entry">
              <span class="set-row-lead">
                <span class="set-chip mono">L1</span>
                <span class="set-row-text">
                  <span class="set-row-title">{e.text}</span>
                  <span class="set-row-desc">
                    <code>{e.id}</code> · {e.by} · {e.written_at.slice(0, 19).replace("T", " ")}
                  </span>
                </span>
              </span>
              <span class="set-value">{e.sensitivity}</span>
            </li>
          {/each}
        </ul>
      {/if}
    </section>
  {/if}
</div>

<style>
  /* Rows and text come from the shared settings vocabulary. */
  .conversation { width: min(420px, 100%); }
  .import-row { display: flex; gap: 8px; align-items: center; padding: 4px 0 6px; }
  .import-row .folder { flex: 1; min-width: 0; font-family: var(--font-mono); font-size: 11.5px; }
  .lines {
    list-style: none;
    margin: 4px 0 0;
    padding: 0;
    max-height: 240px;
    overflow: auto;
    border-top: 1px solid var(--line-soft);
  }
  .lines li {
    padding: 7px 0;
    border-bottom: 1px solid var(--line-soft);
    font: 400 11.5px/1.5 var(--font-mono);
    color: var(--fg-body);
    white-space: pre-wrap;
    word-break: break-word;
  }
  .set-section .paths { margin-left: 0; }
  .entry .set-row-lead { align-items: flex-start; }
</style>
