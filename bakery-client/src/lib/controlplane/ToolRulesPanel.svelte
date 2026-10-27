<script lang="ts">
  import { onMount } from "svelte";
  import { node, type KindPolicy, type ToolPolicyValue } from "$lib/node";

  // The rules every native runtime's own actions follow — Claude, Codex,
  // Cursor, Grok, Antigravity alike. Never outranks every operating mode.

  let rules = $state<KindPolicy[] | null>(null);
  let error = $state("");

  onMount(async () => {
    try {
      rules = await node.toolPolicy();
    } catch (e) {
      error = `${e}`;
    }
  });

  const KIND_LABEL: Record<string, string> = {
    read: "Read files",
    search: "Search",
    think: "Think",
    fetch: "Fetch from the web",
    edit: "Edit files",
    delete: "Delete files",
    move: "Move files",
    execute: "Run commands",
    switch_mode: "Switch its own mode",
    other: "Other (MCP tools, anything unclassified)",
  };

  const choices = (p: KindPolicy): ToolPolicyValue[] => (p.read_only ? ["always", "never"] : ["always", "ask", "never"]);

  async function setPolicy(p: KindPolicy, policy: ToolPolicyValue) {
    if (p.policy === policy) return;
    error = "";
    try {
      rules = await node.setToolPolicy(p.kind, policy);
    } catch (e) {
      error = `${e}`;
    }
  }
</script>

<!-- One row per kind of action, its rule as a segmented choice on the right
     (settings.css). An active "Never" stays red: it is a hard stop. -->
<section class="set-section" data-testid="tool-rules">
  <h2 class="set-section-h">Native runtimes' own actions</h2>
  <p class="set-section-desc">
    For every native runtime at once. Never outranks every mode, even Auto; Always and Ask apply in Manual mode.
    Edits, deletes and moves on a project socket still go through Bakery review; on a task socket they follow these
    rules.
  </p>
  {#if error}<p class="set-error">{error}</p>{/if}
  {#if !rules && !error}
    <p class="set-muted">Reading tool rules…</p>
  {:else if rules}
    {#each rules as p (p.kind)}
      <div class="set-row">
        <span class="set-row-text"><span class="set-row-title">{KIND_LABEL[p.kind] ?? p.kind}</span></span>
        <div class="set-seg" role="group" aria-label="Rule for {KIND_LABEL[p.kind] ?? p.kind}">
          {#each choices(p) as value (value)}
            <button
              type="button"
              class="set-seg-btn"
              class:on={p.policy === value}
              class:bad={value === "never"}
              aria-pressed={p.policy === value}
              onclick={() => void setPolicy(p, value)}
              data-testid="policy-{p.kind}-{value}"
            >
              {value === "always" ? "Always" : value === "ask" ? "Ask" : "Never"}
            </button>
          {/each}
        </div>
      </div>
    {/each}
  {/if}
</section>
