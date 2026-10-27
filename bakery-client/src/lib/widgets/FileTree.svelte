<script lang="ts">
  import { node, type DirEntry } from "$lib/node";
  // Recursion via self-import (the non-deprecated replacement for
  // <svelte:self>) — every level reads/writes the same `store` object, so a
  // toggle three levels down is visible at the root without prop drilling a
  // callback back up.
  import FileTree from "./FileTree.svelte";

  // Lazy directory tree over the worktree. Plain buttons throughout: real,
  // focusable, no ARIA tree-pattern half-measures that svelte-check would
  // flag as incomplete.

  type NodeState = {
    entries: DirEntry[] | null;
    loading: boolean;
    error: string | null;
    expanded: boolean;
  };

  let {
    streamId,
    onOpenFile,
    path = ".",
    depth = 0,
    store,
    selectedPath = "",
    filterQuery = "",
  }: {
    streamId: string;
    onOpenFile: (path: string) => void;
    path?: string;
    depth?: number;
    /** Only the root call omits this — it owns the map every deeper level shares. */
    store?: Record<string, NodeState>;
    /** Highlighted file — passed from the route, threaded down through every level. */
    selectedPath?: string;
    /** Name filter from root — applied client-side; files are hidden on no-match, dirs always shown. */
    filterQuery?: string;
  } = $props();

  let isRoot = $derived(depth === 0);
  // `$state` must be a plain declaration initializer — always create the map,
  // but only the root instance's copy is ever used (deeper instances receive
  // the root's map via `store` and read/write that one instead).
  let ownTree = $state<Record<string, NodeState>>({});
  let tree = $derived(store ?? ownTree);

  // Root-only filter text. Non-root instances declare this but never bind to it;
  // they receive the root's value via the filterQuery prop instead.
  let filterText = $state("");
  let activeFilter = $derived(isRoot ? filterText : filterQuery);

  function nodeOf(p: string): NodeState {
    return tree[p] ?? { entries: null, loading: false, error: null, expanded: p === "." };
  }

  async function ensureLoaded(p: string) {
    const cur = nodeOf(p);
    if (cur.entries || cur.loading) return;
    tree[p] = { ...cur, loading: true, error: null };
    try {
      const entries = await node.filesList(streamId, p);
      tree[p] = { ...tree[p], entries, loading: false };
    } catch (e) {
      tree[p] = { ...tree[p], loading: false, error: `${e}` };
    }
  }

  function toggle(p: string) {
    const cur = nodeOf(p);
    const expanded = !cur.expanded;
    tree[p] = { ...cur, expanded };
    if (expanded) ensureLoaded(p);
  }

  function childPath(name: string): string {
    return path === "." ? name : `${path}/${name}`;
  }

  // Root only: load "." on mount, and reset + reload when the session changes.
  // Plain latch (not $state): this effect both reads and mutates `tree`
  // (Object.keys + delete, ensureLoaded write). Without a non-reactive guard,
  // Svelte re-runs forever → effect_update_depth_exceeded and a node.filesList
  // request storm. Same pattern as SessionTree autoExpanded (D4.1).
  let loadedFor = "";
  $effect(() => {
    if (!isRoot) return;
    const id = streamId;
    if (loadedFor === id) return;
    loadedFor = id;
    for (const k of Object.keys(tree)) delete tree[k];
    ensureLoaded(".");
  });

  let here = $derived(nodeOf(path));

  // Files filtered by name match; dirs always pass through (may have matching children).
  const filteredEntries = $derived.by(() => {
    const entries = here.entries;
    if (!entries) return null;
    const q = activeFilter.trim().toLowerCase();
    if (!q) return entries;
    return entries.filter((e) => e.is_dir || e.name.toLowerCase().includes(q));
  });
</script>

{#snippet treeBody()}
  {#if isRoot || here.expanded}
    {#if here.error}
      <p class="tree-msg bad" style="padding-left: {8 + (depth + 1) * 14}px">{here.error}</p>
    {:else if here.loading && !here.entries}
      <p class="tree-msg" style="padding-left: {8 + (depth + 1) * 14}px">loading…</p>
    {:else if filteredEntries !== null && filteredEntries.length === 0}
      {#if activeFilter.trim()}
        <p class="tree-msg" style="padding-left: {8 + (depth + 1) * 14}px">no match for "{activeFilter.trim()}"</p>
      {:else}
        <p class="tree-msg" style="padding-left: {8 + (depth + 1) * 14}px">empty</p>
      {/if}
    {:else if filteredEntries}
      {#each filteredEntries as entry (entry.name)}
        {#if entry.is_dir}
          <FileTree
            {streamId}
            {onOpenFile}
            path={childPath(entry.name)}
            depth={depth + 1}
            store={tree}
            {selectedPath}
            filterQuery={activeFilter}
          />
        {:else}
          {@const cp = childPath(entry.name)}
          <button
            type="button"
            class="row file"
            class:selected={selectedPath === cp}
            style="padding-left: {8 + (depth + 1) * 14}px"
            onclick={() => onOpenFile(cp)}
          >
            <span class="name">{entry.name}</span>
            <!-- TODO(E6-badges): pending proposal count badge — needs pendingPaths prop -->
          </button>
        {/if}
      {/each}
    {/if}
  {/if}
{/snippet}

{#if isRoot}
  <div class="files-widget">
    <div class="filter-row">
      <input
        class="filter-input"
        type="search"
        placeholder="filter loaded names…"
        autocomplete="off"
        bind:value={filterText}
      />
    </div>
    <div class="tree-scroll">
      {@render treeBody()}
    </div>
  </div>
{:else}
  <button
    type="button"
    class="row dir"
    style="padding-left: {8 + depth * 14}px"
    onclick={() => toggle(path)}
    aria-expanded={here.expanded}
  >
    <span class="chev" aria-hidden="true">{here.expanded ? "▾" : "▸"}</span>
    <span class="name">{path.split("/").pop()}</span>
  </button>
  {@render treeBody()}
{/if}

<style>
  .files-widget {
    display: flex;
    flex-direction: column;
    height: 100%;
    overflow: hidden;
  }

  /* ---- filter input ---- */

  .filter-row {
    flex: 0 0 auto;
    padding: 6px 8px;
    background: var(--bg-panel);
    border-bottom: 1px solid var(--line-chrome);
  }

  .filter-input {
    width: 100%;
    background: var(--bg-app);
    border: 1px solid var(--line);
    border-radius: var(--r-md);
    padding: 4px 8px;
    font: 400 11.5px/1 var(--font-mono);
    color: var(--fg-2);
    outline: none;
    box-sizing: border-box;
  }
  .filter-input:focus { border-color: var(--accent-border); }
  .filter-input::placeholder { color: var(--fg-fainter); }

  /* ---- scrollable tree ---- */

  .tree-scroll {
    flex: 1 1 auto;
    min-height: 0;
    overflow-y: auto;
    padding: 4px 0;
  }

  /* ---- rows ---- */

  .row {
    display: flex;
    align-items: center;
    gap: 5px;
    width: 100%;
    text-align: left;
    background: none;
    border: none;
    cursor: pointer;
    font: 400 11.5px/1.4 var(--font-mono);
    padding-top: 2px;
    padding-bottom: 2px;
    padding-right: 8px;
    border-radius: var(--r-md);
    color: var(--fg-dim);
    white-space: nowrap;
    overflow: hidden;
  }
  .row:hover { background: var(--bg-raised); }

  .row.dir .chev {
    color: var(--fg-ghost);
    font-size: 9px;
    width: 10px;
    text-align: center;
    flex-shrink: 0;
  }

  .row.file { color: var(--fg-dim); }

  .row.selected {
    background: var(--accent-sel);
    color: var(--fg);
  }
  .row.selected:hover { background: var(--accent-sel); }

  .name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    min-width: 0;
  }

  /* ---- status messages ---- */

  .tree-msg {
    font: 400 11px/1.4 var(--font-ui);
    color: var(--fg-faint);
    font-style: italic;
    margin: 2px 0;
    padding-top: 2px;
    padding-bottom: 2px;
    padding-right: 8px;
  }
  .tree-msg.bad { color: var(--bad); font-style: normal; }
</style>
