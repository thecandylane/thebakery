<script lang="ts">
  import { onMount } from "svelte";
  import { ask } from "$lib/ask";
  import { node, type StorageArea, type StorageEntry, type StorageFile, type StorageView } from "$lib/node";

  // Settings → Agent storage. Everything any agent writes outside your
  // projects — the Baker, project agents, task workers, whatever runtime — and
  // all of it inside Bakery's own folder. Runtimes are sandboxed so they can't
  // write anywhere else, and never into a project's own agent folders.
  // Credential files are listed but never shown here.

  let view = $state<StorageView | null>(null);
  let loadError = $state("");

  // The open browser, shown right under the area it browses: which area row
  // (the same folder can appear under several agents), its root, and the
  // folder currently shown under it.
  let openKey = $state<string | null>(null);
  let root = $state<StorageArea | null>(null);
  let path = $state("");
  let entries = $state<StorageEntry[]>([]);
  let reading = $state(false);
  let file = $state<{ name: string; data: StorageFile } | null>(null);
  let browseError = $state("");

  async function load() {
    loadError = "";
    try {
      view = await node.agentStorage();
    } catch (e) {
      loadError = `${e}`;
    }
  }

  onMount(() => void load());

  function size(bytes: number): string {
    if (bytes < 1024) return `${bytes} B`;
    const units = ["KB", "MB", "GB"];
    let n = bytes / 1024;
    let i = 0;
    while (n >= 1024 && i < units.length - 1) {
      n /= 1024;
      i++;
    }
    return `${n.toFixed(n >= 10 ? 0 : 1)} ${units[i]}`;
  }

  const when = (iso: string | null) => (iso ? new Date(iso).toLocaleString() : "—");

  async function open(dir: string) {
    browseError = "";
    file = null;
    reading = true;
    try {
      entries = await node.storageList(dir);
      path = dir;
    } catch (e) {
      browseError = `${e}`;
    } finally {
      reading = false;
    }
  }

  function close() {
    openKey = null;
    root = null;
    file = null;
    entries = [];
    browseError = "";
  }

  function browse(area: StorageArea, key: string) {
    if (openKey === key) return close();
    openKey = key;
    root = area;
    path = area.path;
    entries = [];
    void open(area.path);
  }

  async function show(entry: StorageEntry) {
    const target = `${path}/${entry.name}`;
    if (entry.dir) return void open(target);
    if (entry.credential) return;
    browseError = "";
    try {
      file = { name: entry.name, data: await node.storageFile(target) };
    } catch (e) {
      browseError = `${e}`;
    }
  }

  async function remove(target: string, label: string) {
    if (!(await ask(`Delete ${label}? This can't be undone.`))) return;
    browseError = "";
    try {
      await node.storageDelete(target);
      // Deleting the folder on screen (or one holding it) closes the browser.
      if (root && (path === target || path.startsWith(`${target}/`))) close();
      else if (root) await open(path);
      await load();
    } catch (e) {
      if (root) browseError = `${e}`;
      else alert(`${e}`);
    }
  }

  function up() {
    if (!root || path === root.path) return;
    void open(path.slice(0, path.lastIndexOf("/")));
  }

  const GROUPS = [
    { scope: "baker", label: "Baker" },
    { scope: "project", label: "Projects" },
    { scope: "task", label: "Tasks" },
  ] as const;
</script>

{#snippet browser(r: StorageArea)}
  <div class="browser" data-testid="storage-browser">
    <div class="browser-hd">
      <span class="crumb" title={path}>{r.label}{path.slice(r.path.length) || ""}</span>
      <span class="grow"></span>
      <button type="button" class="set-btn sm" onclick={up} disabled={path === r.path || reading}>Up</button>
      <button type="button" class="set-btn sm" onclick={close}>Close</button>
    </div>
    {#if browseError}<p class="set-error">{browseError}</p>{/if}
    {#if reading}
      <p class="set-muted">Reading…</p>
    {:else if entries.length === 0 && !browseError}
      <p class="set-muted">Empty.</p>
    {/if}
    {#each entries as e (e.name)}
      <div class="entry">
        <button type="button" class="linkish" onclick={() => void show(e)} disabled={e.credential}>
          {e.dir ? "▸ " : ""}{e.name}
        </button>
        <span class="set-row-desc">
          {e.credential ? "credential — never shown" : e.dir ? "folder" : size(e.bytes)} · {when(e.modified)}
        </span>
        {#if !e.credential}
          <button type="button" class="set-btn sm" onclick={() => void remove(`${path}/${e.name}`, e.name)}>Delete</button>
        {/if}
      </div>
    {/each}
    {#if file}
      <div class="file">
        <div class="browser-hd">
          <span class="set-row-title">{file.name} · {size(file.data.bytes)}{file.data.truncated ? " (first 256 KB)" : ""}</span>
          <span class="grow"></span>
          <button type="button" class="set-btn sm" onclick={() => (file = null)}>Close file</button>
        </div>
        {#if file.data.binary}
          <p class="set-muted">Binary file — not shown.</p>
        {:else}
          <pre>{file.data.text}</pre>
        {/if}
      </div>
    {/if}
  </div>
{/snippet}

{#snippet areaRow(a: StorageArea, running: boolean, key: string)}
  <div class="set-row top area">
    <span class="set-row-text">
      <span class="set-row-title">{a.label}</span>
      <span class="set-row-desc">
        {a.exists
          ? `${size(a.bytes)} · ${a.files.toLocaleString()} files${a.truncated ? "+" : ""} · last write ${when(a.newest)}`
          : "empty"}
      </span>
      <span class="set-mono">{a.path}</span>
    </span>
    {#if a.exists}
      <span class="set-row-control">
        <button type="button" class="set-btn sm" onclick={() => browse(a, key)} aria-expanded={openKey === key}>
          {openKey === key ? "Close" : "Browse"}
        </button>
        {#if a.clearable && a.label === "Temp"}
          <button
            type="button"
            class="set-btn sm"
            disabled={running}
            onclick={() => void remove(a.path, "this agent's temp files")}
          >
            Clear temp
          </button>
        {:else if a.clearable && a.label === "Private home"}
          <button
            type="button"
            class="set-btn sm danger"
            disabled={running}
            onclick={() => void remove(a.path, "this agent's whole private home")}
          >
            Reset home
          </button>
        {/if}
      </span>
    {/if}
    {#if openKey === key && root}<div class="set-row-more">{@render browser(root)}</div>{/if}
  </div>
{/snippet}

<!-- Rendered under Bakery's memory on the Memory & storage page: one
     section with the agents grouped under it, each storage area a settings
     row (settings.css) that opens its file browser underneath. -->
<div class="set-panel" data-testid="agent-storage-panel">
  <section class="set-section">
    <h2 class="set-section-h">Agent files</h2>
    <p class="set-section-desc">
      Everything agents write outside your projects — scratch files, temp files, runtime state, transcripts — and
      all of it inside Bakery's own folder. Runtimes are sandboxed: they can't write anywhere else.
    </p>

    {#if loadError}
      <p class="set-error">{loadError}</p>
    {:else if !view}
      <p class="set-muted">Measuring agent storage…</p>
    {:else}
      <h3 class="set-subhead">Never written in your projects</h3>
      <p class="set-note">
        No runtime can create or change these folders in a project: {view.never_in_projects.join(", ")}. If one
        already exists, it's yours — agents can read it.
      </p>

      {#each GROUPS as group (group.scope)}
        {@const members = view.agents.filter((a) => a.scope === group.scope)}
        {#if members.length}
          <h3 class="set-subhead">{group.label}</h3>
          {#each members as a (`${a.stream_id}:${a.seat}`)}
            <div class="agent" data-testid="storage-agent">
              <div class="agent-hd">
                <span class="set-row-title">{a.socket} · {a.seat}</span>
                {#if a.model_id}<span class="set-chip mono">{a.model_id}</span>{/if}
                {#if a.running}<span class="set-chip ok">running — stop it to clear</span>{/if}
              </div>
              {#each a.areas as area (area.path)}
                {@render areaRow(area, a.running, `${a.stream_id}:${a.seat}:${area.label}`)}
              {/each}
              {#if a.project_files.length}
                <p class="set-muted project-files">In the project (yours, read-only to agents): {a.project_files.join(", ")}</p>
              {/if}
            </div>
          {/each}
        {/if}
      {/each}

      <h3 class="set-subhead">Shared by every agent on one account</h3>
      {#each view.shared as area (area.path)}
        {@render areaRow(area, false, `shared:${area.label}`)}
      {/each}

      <h3 class="set-subhead">Outside the sandbox</h3>
      {#each view.host_side as h (h.what)}
        <p class="set-note"><strong>{h.what}</strong> {h.detail}</p>
      {/each}
    {/if}
  </section>
</div>

<style>
  /* Rows and controls come from the shared settings vocabulary; the agent
     groups and the inline file browser are this page's own. */
  .agent { padding: 6px 0 4px; }
  .agent + .agent { border-top: 1px solid var(--line-soft); }
  .agent-hd {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
    padding: 6px 0 2px;
  }
  .agent .area { margin-left: 14px; }
  .agent .project-files { margin-left: 14px; padding: 2px 0 8px; }
  .set-note strong { color: var(--fg-2); font-weight: 600; }

  .browser {
    display: flex;
    flex-direction: column;
    gap: 6px;
    min-width: 0;
    padding: 10px 12px;
    border-radius: var(--r-lg);
    background: var(--bg-panel);
  }
  .browser-hd { display: flex; flex-wrap: wrap; align-items: center; gap: 8px; }
  .crumb { font: 500 12px/1.4 var(--font-mono); color: var(--fg-2); overflow-wrap: anywhere; }
  .grow { flex: 1 1 auto; }
  .entry {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 4px 10px;
    padding: 6px 0;
    border-top: 1px solid var(--line-soft);
  }
  .entry .set-row-desc { flex: 1 1 auto; }
  .linkish {
    background: none;
    border: none;
    padding: 0;
    font: 400 12px/1.4 var(--font-mono);
    color: var(--accent-fg);
    cursor: pointer;
    text-align: left;
    overflow-wrap: anywhere;
  }
  .linkish:disabled { color: var(--fg-faint); cursor: default; }
  .linkish:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; border-radius: var(--r-xs); }
  .file { display: flex; flex-direction: column; gap: 6px; padding-top: 6px; border-top: 1px solid var(--line-soft); }
  pre {
    margin: 0;
    max-height: 360px;
    overflow: auto;
    padding: 8px 10px;
    background: var(--bg-app);
    border: 1px solid var(--line-soft);
    border-radius: var(--r-lg);
    font: 400 11px/1.5 var(--font-mono);
    color: var(--fg-2);
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }

  @container settings (max-width: 520px) {
    .agent .area,
    .agent .project-files { margin-left: 0; }
  }
</style>
