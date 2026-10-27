<script lang="ts">
  import { NODE } from "$lib/node";
  import type { ConnStatus } from "$lib/events";

  let {
    status,
  }: {
    status: ConnStatus;
  } = $props();
</script>

<div class="statusbar">
  <span
    class="node-dot"
    class:ok={status === "open"}
    class:warn={status === "connecting"}
    class:bad={status === "closed"}
    aria-hidden="true"
  ></span>
  <span class="seg">node {NODE}</span>
  <span class="spacer"></span>
  <span class="hint">⌘K jump</span>
</div>

<style>
  .statusbar {
    height: var(--statusbar-h, 22px);
    flex: 0 0 auto;
    display: flex;
    align-items: center;
    gap: 5px;
    padding: 0 10px;
    background: var(--bg-rail);
    border-top: 1px solid var(--line-chrome);
    font: 400 10px/1 var(--font-mono);
    color: var(--fg-faint);
    overflow: hidden;
  }

  .node-dot {
    width: 6px;
    height: 6px;
    border-radius: var(--r-pill);
    flex: 0 0 auto;
    background: var(--bad);
  }
  .node-dot.ok   { background: var(--ok); }
  .node-dot.warn { background: var(--warn); }
  .node-dot.bad  { background: var(--bad); }

  .seg { white-space: nowrap; }
  .spacer { flex: 1 1 auto; }
  .hint { color: var(--fg-fainter); white-space: nowrap; }
</style>
