<script lang="ts">
  import type { TranscriptItem } from "$lib/transcript";
  import FileTree from "./FileTree.svelte";
  import Editor from "./Editor.svelte";

  // Files + Editor unified into one Code panel: a collapsible tree on the
  // left, Monaco + tabs on the right, both reading/writing the same
  // `openPath` (workbench.selectedFile) the route already threads through —
  // no second source of truth for "which file is open" is introduced here.

  let {
    streamId,
    openPath = "",
    items = [],
    onOpenFile,
    onCloseFile,
  }: {
    streamId: string;
    openPath?: string;
    items?: TranscriptItem[];
    onOpenFile: (path: string) => void;
    onCloseFile?: (path: string) => void;
  } = $props();

  let treeCollapsed = $state(false);
</script>

<div class="code-panel">
  {#if !treeCollapsed}
    <div class="tree-pane">
      <FileTree {streamId} {onOpenFile} selectedPath={openPath} />
    </div>
  {/if}
  <button
    type="button"
    class="tree-toggle"
    onclick={() => { treeCollapsed = !treeCollapsed; }}
    aria-expanded={!treeCollapsed}
    aria-label={treeCollapsed ? "Show file tree" : "Hide file tree"}
    title={treeCollapsed ? "Show file tree" : "Hide file tree"}
  >{treeCollapsed ? "▸" : "◂"}</button>
  <div class="editor-pane">
    <Editor {streamId} {openPath} {items} onClose={onCloseFile} />
  </div>
</div>

<style>
  .code-panel {
    display: flex;
    height: 100%;
    overflow: hidden;
    background: var(--bg-app);
  }

  .tree-pane {
    flex: 0 0 200px;
    min-width: 0;
    overflow: hidden;
    background: var(--bg-panel);
  }

  .tree-toggle {
    flex: 0 0 auto;
    width: 14px;
    border: none;
    border-left: 1px solid var(--line-chrome);
    border-right: 1px solid var(--line-chrome);
    background: var(--bg-panel);
    color: var(--fg-ghost);
    cursor: pointer;
    font: 400 10px/1 var(--font-ui);
  }
  .tree-toggle:hover { background: var(--bg-raised); color: var(--fg-2); }

  .editor-pane {
    flex: 1 1 auto;
    min-width: 0;
    overflow: hidden;
  }
</style>
