<script lang="ts">
  import { onMount, untrack } from "svelte";
  import { ask } from "$lib/ask";
  import type { TranscriptItem } from "$lib/transcript";
  import { monaco } from "$lib/monacoSetup";
  import {
    openFile,
    closeTab,
    saveFile,
    reloadFromConflict,
    acceptConflictBaseline,
    getEntry,
    FileModelEntry,
  } from "$lib/editorModels.svelte";

  // Real edit: one Monaco model per (streamId, path), owned by
  // editorModels.svelte.ts — this widget only mounts a view onto whatever
  // model is active. Save is conflict-safe (digest round-trip through
  // /files/write); agent writes still go through the Manual accept path.

  let {
    streamId,
    openPath = "",
    items = [],
    onClose,
  }: {
    streamId: string;
    /** Set from the route's selectedFile — triggers tab open/activate on change. */
    openPath?: string;
    /** TranscriptItems for pending proposal count in footer. */
    items?: TranscriptItem[];
    /** Route clears selectedFile when the closed tab was the selection (reopen via tree works). */
    onClose?: (path: string) => void;
  } = $props();

  let openTabs = $state<string[]>([]);
  let activeTab = $state("");
  let loadingTabs = $state<Record<string, boolean>>({});
  let loadErrors = $state<Record<string, string>>({});

  let containerEl: HTMLDivElement | undefined;
  let editor: monaco.editor.IStandaloneCodeEditor | undefined;
  let currentEntry: FileModelEntry | null = null;

  function tabLabel(path: string): string {
    return path.split("/").pop() ?? path;
  }

  function normalizePath(p: string): string {
    return p.replace(/^\.\//, "").replace(/\\/g, "/");
  }

  function attachEntry(entry: FileModelEntry) {
    if (!editor || currentEntry === entry) return;
    if (currentEntry) currentEntry.viewState = editor.saveViewState();
    editor.setModel(entry.model);
    if (entry.viewState) editor.restoreViewState(entry.viewState);
    editor.focus();
    currentEntry = entry;
  }

  function detachEditor() {
    if (currentEntry && editor) currentEntry.viewState = editor.saveViewState();
    currentEntry = null;
    editor?.setModel(null);
  }

  async function activateTab(path: string) {
    activeTab = path;
    const existing = getEntry(streamId, path);
    if (existing) {
      attachEntry(existing);
      return;
    }
    loadingTabs = { ...loadingTabs, [path]: true };
    loadErrors = { ...loadErrors, [path]: "" };
    try {
      const entry = await openFile(streamId, path);
      // The user may have switched tabs again while this was in flight.
      if (activeTab === path) attachEntry(entry);
    } catch (e) {
      loadErrors = { ...loadErrors, [path]: e instanceof Error ? e.message : String(e) };
    } finally {
      loadingTabs = { ...loadingTabs, [path]: false };
    }
  }

  function openFileTab(path: string) {
    if (!path) return;
    if (!openTabs.includes(path)) openTabs = [...openTabs, path];
    void activateTab(path);
  }

  async function closeFileTab(path: string) {
    const idx = openTabs.indexOf(path);
    if (idx === -1) return;
    const entry = getEntry(streamId, path);
    if (entry?.dirty) {
      const proceed = await ask(
        `${tabLabel(path)} has unsaved changes. Close the tab anyway?\n\nThe edit is kept in memory until you reopen it, save it, or discard this session.`,
      );
      if (!proceed) return;
    }
    const remaining = openTabs.filter((p) => p !== path);
    openTabs = remaining;
    if (lastOpened === path) lastOpened = "";
    if (activeTab === path) {
      const next = remaining.length > 0 ? remaining[Math.max(0, idx - 1)] : "";
      if (next) void activateTab(next);
      else {
        activeTab = "";
        detachEditor();
      }
    }
    closeTab(streamId, path); // no-op if still dirty — registry retains it
    onClose?.(path);
  }

  // Plain latch on openPath (Opus E7): closeFileTab mutates openTabs; without
  // a latch any re-entry re-opens the still-selected path. untrack keeps
  // openTabs off the dependency list; latch is the real close fix.
  let tabsForStream = "";
  let lastOpened = "";
  $effect(() => {
    const id = streamId;
    const path = openPath;
    untrack(() => {
      if (tabsForStream !== id) {
        tabsForStream = id;
        lastOpened = "";
        openTabs = [];
        activeTab = "";
        detachEditor();
      }
      if (!path) {
        lastOpened = "";
        return;
      }
      if (path === lastOpened) return;
      lastOpened = path;
      openFileTab(path);
    });
  });

  async function doSave() {
    if (!currentEntry || currentEntry.saving) return;
    await saveFile(currentEntry);
  }

  onMount(() => {
    if (!containerEl) return;
    editor = monaco.editor.create(containerEl, {
      automaticLayout: true,
      theme: "bakery-dark",
      minimap: { enabled: false },
      fontSize: 12,
      fontFamily: "'JetBrains Mono', ui-monospace, 'Cascadia Mono', Menlo, monospace",
      scrollBeyondLastLine: false,
      renderWhitespace: "selection",
      tabSize: 2,
    });
    editor.addAction({
      id: "bakery.saveFile",
      label: "Save",
      keybindings: [monaco.KeyMod.CtrlCmd | monaco.KeyCode.KeyS],
      run: () => {
        void doSave();
      },
    });
    return () => {
      if (currentEntry && editor) currentEntry.viewState = editor.saveViewState();
      editor?.dispose();
      editor = undefined;
    };
  });

  const active = $derived(activeTab ? getEntry(streamId, activeTab) : undefined);
  const activeLoading = $derived(activeTab ? !!loadingTabs[activeTab] : false);
  const activeLoadError = $derived(activeTab ? loadErrors[activeTab] || "" : "");

  const activeTabPending = $derived.by(() => {
    if (!activeTab || !items.length) return 0;
    const key = normalizePath(activeTab);
    let n = 0;
    for (const item of items) {
      if (
        item.kind === "proposal" &&
        item.verdict === "pending" &&
        normalizePath(item.filePath) === key
      ) {
        n++;
      }
    }
    return n;
  });

  // Conflict diff modal: two throwaway models compared side-by-side; created
  // lazily on conflict and torn down when it closes, on top of the live
  // editor rather than replacing it.
  let diffEl: HTMLDivElement | undefined = $state();
  let diffEditor: monaco.editor.IStandaloneDiffEditor | undefined;
  let diffOriginal: monaco.editor.ITextModel | undefined;
  let diffModified: monaco.editor.ITextModel | undefined;

  function teardownDiff() {
    diffEditor?.dispose();
    diffEditor = undefined;
    diffOriginal?.dispose();
    diffOriginal = undefined;
    diffModified?.dispose();
    diffModified = undefined;
  }

  $effect(() => {
    const entry = active;
    const conflict = entry?.conflict;
    if (!conflict || !diffEl) {
      untrack(teardownDiff);
      return;
    }
    untrack(() => {
      teardownDiff();
      const lang = entry!.model.getLanguageId();
      diffOriginal = monaco.editor.createModel(conflict.currentText ?? "", lang);
      diffModified = monaco.editor.createModel(entry!.model.getValue(), lang);
      diffEditor = monaco.editor.createDiffEditor(diffEl!, {
        theme: "bakery-dark",
        readOnly: true,
        renderSideBySide: true,
        automaticLayout: true,
      });
      diffEditor.setModel({ original: diffOriginal, modified: diffModified });
    });
  });

  function resolveKeepMine() {
    if (!currentEntry) return;
    acceptConflictBaseline(currentEntry);
    void saveFile(currentEntry);
  }

  function resolveDiscardMine() {
    if (!currentEntry) return;
    reloadFromConflict(currentEntry);
  }
</script>

<div class="editor">
  <!-- Tab strip: two sibling <button>s per tab inside a div — avoids nested-button invalid HTML -->
  <div class="tab-strip">
    {#if openTabs.length === 0}
      <span class="no-tabs">Open a file from the tree on the left</span>
    {:else}
      {#each openTabs as tab (tab)}
        {@const entry = getEntry(streamId, tab)}
        <div class="tab" class:active={activeTab === tab}>
          <button
            type="button"
            class="tab-label-btn"
            onclick={() => activateTab(tab)}
            title={tab}
          >
            {#if entry?.dirty}<span class="dirty-dot" title="unsaved changes">●</span>{/if}
            {tabLabel(tab)}
          </button>
          <button
            type="button"
            class="tab-close"
            aria-label="Close {tabLabel(tab)}"
            onclick={() => closeFileTab(tab)}
          >×</button>
        </div>
      {/each}
    {/if}
  </div>

  <!-- File body -->
  <div class="body">
    {#if !activeTab}
      <p class="body-hint">No file open.</p>
    {:else if activeLoading}
      <p class="body-hint">loading…</p>
    {:else if activeLoadError}
      <p class="body-hint bad">{activeLoadError}</p>
    {/if}
    <div class="monaco-host" class:hidden={!activeTab || activeLoading || !!activeLoadError} bind:this={containerEl}></div>

    {#if active?.conflict}
      <div class="conflict-overlay">
        <div class="conflict-panel">
          <div class="conflict-head">
            <strong>{tabLabel(activeTab)} changed on disk</strong>
            <span class="conflict-sub">
              {#if active.conflict.currentText == null}
                The file was deleted since you opened it.
              {:else}
                Someone (or something) else saved this file after you opened it. Review the diff before choosing.
              {/if}
            </span>
          </div>
          <div class="diff-host" bind:this={diffEl}></div>
          <div class="conflict-actions">
            <span class="diff-label left">Current on disk</span>
            <span class="diff-label right">Your edit</span>
            <span class="f-spacer"></span>
            <button type="button" class="conflict-btn" onclick={resolveDiscardMine}>Discard mine — reload from disk</button>
            <button type="button" class="conflict-btn keep" onclick={resolveKeepMine}>Keep mine — overwrite</button>
          </div>
        </div>
      </div>
    {/if}
  </div>

  <!-- Footer -->
  <div class="footer">
    {#if activeTab}
      <span class="stat">{tabLabel(activeTab)}</span>
      {#if activeTabPending > 0}
        <span class="sep">·</span>
        <span class="pending">{activeTabPending} pending proposal{activeTabPending > 1 ? "s" : ""} touch this file</span>
      {/if}
      {#if active?.saveError}
        <span class="sep">·</span>
        <span class="stat bad">{active.saveError}</span>
      {/if}
    {/if}
    <span class="f-spacer"></span>
    <button
      type="button"
      class="save-btn"
      class:dirty={!!active?.dirty}
      disabled={!active || !active.dirty || active.saving || !!active.conflict}
      onclick={doSave}
    >{active?.saving ? "Saving…" : "Save"}</button>
  </div>
</div>

<style>
  .editor {
    display: flex;
    flex-direction: column;
    height: 100%;
    overflow: hidden;
    background: var(--bg-app);
  }

  /* ---- tab strip ---- */

  .tab-strip {
    flex: 0 0 auto;
    display: flex;
    align-items: stretch;
    overflow-x: auto;
    background: var(--bg-panel);
    border-bottom: 1px solid var(--line-chrome);
    scrollbar-width: none;
  }
  .tab-strip::-webkit-scrollbar { display: none; }

  .no-tabs {
    padding: 0 12px;
    font: 400 11px/30px var(--font-mono);
    color: var(--fg-fainter);
    font-style: italic;
    white-space: nowrap;
    display: block;
    line-height: 30px;
  }

  .tab {
    display: flex;
    align-items: stretch;
    border-right: 1px solid var(--line-chrome);
    border-bottom: 2px solid transparent;
    flex-shrink: 0;
  }
  .tab:hover { background: var(--bg-raised); }
  .tab.active {
    background: var(--bg-app);
    border-bottom-color: var(--accent);
  }

  .tab-label-btn {
    background: none;
    border: none;
    cursor: pointer;
    font: 400 11.5px/1 var(--font-mono);
    color: var(--fg-faint);
    padding: 0 6px 0 12px;
    white-space: nowrap;
    max-width: 160px;
    overflow: hidden;
    text-overflow: ellipsis;
    min-height: 30px;
    display: inline-flex;
    align-items: center;
    gap: 5px;
  }
  .tab.active .tab-label-btn { color: var(--fg); }
  .tab-label-btn:hover { color: var(--fg-2); }

  .dirty-dot {
    color: var(--accent);
    font-size: 8px;
  }

  .tab-close {
    background: none;
    border: none;
    cursor: pointer;
    color: var(--fg-ghost);
    font-size: 12px;
    line-height: 1;
    padding: 0 8px 0 2px;
    border-radius: var(--r-sm);
    min-height: 30px;
  }
  .tab-close:hover { color: var(--bad); }

  /* ---- body ---- */

  .body {
    flex: 1 1 auto;
    min-height: 0;
    position: relative;
    background: var(--bg-app);
  }

  .body-hint {
    font: 400 11.5px/1.4 var(--font-ui);
    color: var(--fg-faint);
    font-style: italic;
    padding: 16px 14px;
    margin: 0;
  }
  .body-hint.bad { color: var(--bad); font-style: normal; }

  .monaco-host {
    position: absolute;
    inset: 0;
  }
  .monaco-host.hidden {
    visibility: hidden;
    pointer-events: none;
  }

  /* ---- conflict overlay ---- */

  .conflict-overlay {
    position: absolute;
    inset: 0;
    background: var(--overlay-scrim);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 20;
  }

  .conflict-panel {
    width: min(96%, 1100px);
    height: min(90%, 680px);
    display: flex;
    flex-direction: column;
    background: var(--bg-panel);
    border: 1px solid var(--line-strong);
    border-radius: var(--r-sm);
    overflow: hidden;
  }

  .conflict-head {
    padding: 10px 14px;
    display: flex;
    flex-direction: column;
    gap: 2px;
    border-bottom: 1px solid var(--line-chrome);
    background: var(--bg-raised);
  }
  .conflict-head strong {
    font: 600 12px/1.3 var(--font-ui);
    color: var(--bad);
  }
  .conflict-sub {
    font: 400 11px/1.4 var(--font-ui);
    color: var(--fg-dim);
  }

  .diff-host {
    flex: 1 1 auto;
    min-height: 0;
  }

  .conflict-actions {
    flex: 0 0 auto;
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 12px;
    border-top: 1px solid var(--line-chrome);
    background: var(--bg-raised);
  }

  .diff-label {
    font: 400 10.5px/1 var(--font-mono);
    color: var(--fg-faint);
  }

  .conflict-btn {
    background: none;
    border: 1px solid var(--line);
    border-radius: var(--r-sm);
    padding: 5px 10px;
    font: 400 11px/1 var(--font-ui);
    color: var(--fg-2);
    cursor: pointer;
  }
  .conflict-btn:hover { background: var(--bg-chip); }
  .conflict-btn.keep {
    border-color: var(--accent-border);
    color: var(--accent-fg);
    background: var(--accent-bg);
  }
  .conflict-btn.keep:hover { background: var(--accent-bg-2); }

  /* ---- footer ---- */

  .footer {
    flex: 0 0 24px;
    display: flex;
    align-items: center;
    gap: 4px;
    padding: 0 10px;
    background: var(--bg-panel);
    border-top: 1px solid var(--line-chrome);
  }

  .stat {
    font: 400 10.5px/1 var(--font-mono);
    color: var(--fg-faint);
    flex-shrink: 0;
  }
  .stat.bad { color: var(--bad); }

  .sep {
    color: var(--fg-ghost);
    font: 400 10.5px/1 var(--font-mono);
    flex-shrink: 0;
  }

  .pending {
    font: 400 10.5px/1 var(--font-mono);
    color: var(--warn);
    flex-shrink: 0;
  }

  .f-spacer { flex: 1 1 auto; }

  .save-btn {
    background: none;
    border: 1px solid var(--line);
    border-radius: var(--r-sm);
    padding: 2px 8px;
    font: 400 10.5px/1 var(--font-ui);
    color: var(--fg-faint);
    opacity: 0.4;
    cursor: default;
  }
  .save-btn.dirty:not(:disabled) {
    opacity: 1;
    color: var(--accent-fg);
    border-color: var(--accent-border);
    background: var(--accent-bg);
    cursor: pointer;
  }
  .save-btn.dirty:not(:disabled):hover { background: var(--accent-bg-2); }
</style>
