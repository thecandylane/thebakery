<script lang="ts">
  // Read-only monospace viewer with line numbers. No editing, no save — writes
  // stay on the Manual proposal path (agent.file.changed -> accept/reject).
  // Wiring a second, ungoverned write path in here would defeat the whole
  // point of the review gate, so this component cannot mutate a file, full stop.

  let {
    path,
    loading = false,
    text = "",
    error = "",
  }: {
    path: string;
    loading?: boolean;
    text?: string;
    error?: string;
  } = $props();

  let lines = $derived(text.length ? text.split("\n") : []);
</script>

<div class="file-view">
  <div class="file-view-head">
    <code class="path">{path}</code>
  </div>
  {#if loading}
    <p class="muted state">loading…</p>
  {:else if error}
    <p class="notice bad state">{error}</p>
  {:else if lines.length === 0}
    <p class="muted state">empty file</p>
  {:else}
    <div class="code">
      {#each lines as line, i (i)}
        <div class="code-line">
          <span class="ln">{i + 1}</span><span class="txt">{line}</span>
        </div>
      {/each}
    </div>
  {/if}
</div>

<style>
  .file-view {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
    overflow: hidden;
  }
  .file-view-head {
    flex: 0 0 auto;
    padding: 0.3rem 0;
    border-bottom: 1px solid var(--line-soft);
  }
  .state {
    padding: 0.5rem 0.1rem;
  }
  .code {
    flex: 1 1 auto;
    min-height: 0;
    overflow: auto;
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 0.78rem;
    line-height: 1.5;
  }
  .code-line {
    display: flex;
    white-space: pre;
  }
  .ln {
    flex: 0 0 3rem;
    text-align: right;
    padding-right: 0.75rem;
    opacity: 0.45;
    user-select: none;
    position: sticky;
    left: 0;
    background: inherit;
  }
  .txt {
    flex: 1;
    padding-right: 0.75rem;
  }
</style>
