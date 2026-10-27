<script lang="ts">
  import { onMount } from "svelte";
  import { node, type NodeInfo } from "$lib/node";
  import { publishNodeInfo, type WriteModeId } from "$lib/writeMode";
  import { ask } from "$lib/ask";

  // PANELS.md "Write mode". Three radio cards over the existing
  // POST /policy/write-mode (node.setWriteMode) — the same endpoint the
  // StatusBar gate cycle uses, with the same confirm-once-on-Auto guard.
  //
  // The auto-accept rules list is NOT rendered from data: NodeInfo does not
  // carry `rules` (the node keeps them in ~/.bakery/node.json and never
  // serializes them over HTTP), so there is nothing real to show. Adding a
  // rules endpoint is W3 — until then this says so rather than printing an
  // invented glob list that would not match what the node actually enforces.

  const MODES: { id: WriteModeId; title: string; blurb: string }[] = [
    {
      id: "manual",
      title: "Manual — every write is gated.",
      blurb:
        "The agent proposes; you read the bytes; the file is written on accept. This is the default and the only mode with no trust surface.",
    },
    {
      id: "rules",
      title: "Rules — narrow the gate by path.",
      blurb:
        "Auto-accept where you have said it is safe. Everything else still asks. Each auto-accept is still an event with your policy cited.",
    },
    {
      id: "auto",
      title: "Auto — accept everything, review after.",
      blurb:
        "For throwaway worktrees only. Writes land immediately; the review queue becomes a history, not a gate.",
    },
  ];

  let info = $state<NodeInfo | null>(null);
  let loading = $state(true);
  let loadError = $state("");
  let saving = $state<WriteModeId | null>(null);
  let saveError = $state("");

  const current = $derived((info?.write_mode ?? "") as string);

  onMount(async () => {
    try {
      info = await node.info();
    } catch (e) {
      loadError = `${e}`;
    } finally {
      loading = false;
    }
  });

  async function choose(mode: WriteModeId) {
    if (saving || mode === current) return;
    // Same guard as the StatusBar gate cycle — elevating to Auto removes the
    // review gate entirely, so it is confirmed once rather than one-click.
    if (mode === "auto") {
      const ok = await ask(
        "Switch to Auto write mode?\n\nEvery agent file proposal will write to disk without review until you switch back to Manual or Rules.",
      );
      if (!ok) return;
    }
    saveError = "";
    saving = mode;
    try {
      // No stream_id: this is the settings surface, not a session. The node
      // persists the mode either way and only skips the per-stream audit event.
      const next = await node.setWriteMode(mode);
      info = next;
      // Chrome (StatusBar / TitleBar) reads write_mode from the layout's copy
      // of NodeInfo; without this the gate chip would lag until a refetch.
      publishNodeInfo(next);
    } catch (e) {
      saveError = `${e}`;
    } finally {
      saving = null;
    }
  }
</script>

<div class="wm">
  <header class="wm-hd">
    <h2>Bakery's own file writes</h2>
    <p class="subhead">
      Manual mode is the write gate. An agent proposes; the file is written only when you accept.
      Auto-accept is a narrowing of that gate, never a removal of the event.
    </p>
  </header>

  {#if loading}
    <p class="muted">Reading the node's write mode…</p>
  {:else if loadError}
    <p class="bad">{loadError}</p>
  {:else}
    {#if saveError}<p class="bad">{saveError}</p>{/if}

    <div class="cards" role="radiogroup" aria-label="Write mode">
      {#each MODES as m (m.id)}
        {@const selected = current === m.id}
        <button
          type="button"
          class="card"
          class:selected
          role="radio"
          aria-checked={selected}
          disabled={saving !== null}
          onclick={() => choose(m.id)}
        >
          <span class="radio" class:on={selected} aria-hidden="true"></span>
          <span class="card-text">
            <span class="card-title">{m.title}</span>
            <span class="card-blurb">{m.blurb}</span>
          </span>
          {#if saving === m.id}<span class="saving">saving…</span>{/if}
        </button>
      {/each}
    </div>

    <section class="rules">
      <h2>Auto-accept rules</h2>
      {#if current === "rules"}
        <p class="note warn-note">
          Rules mode is active, but this screen cannot show you which globs are in force — the node
          does not expose its rule list over HTTP (<code>GET /node</code> carries the mode, not the
          rules). They are read from <code>~/.bakery/node.json</code> at each proposal. A proposal
          that matches no rule stays Manual.
        </p>
      {:else}
        <p class="note">
          Not recorded yet — the node keeps its rule list in
          <code>~/.bakery/node.json</code> and does not serialize it over HTTP, so there is nothing
          here to render. Editing rules from this panel needs an endpoint that does not exist yet.
        </p>
      {/if}
    </section>

    <p class="footnote">
      This is a per-node setting, not per-session: it applies to every stream this node owns.
      Changing it from a session also appends <code>policy.write_mode.set</code> to that stream;
      changed here, it is persisted without a stream to cite.
    </p>
  {/if}
</div>

<style>
  .wm {
    display: flex;
    flex-direction: column;
    gap: 12px;
  }

  .wm-hd { display: flex; flex-direction: column; gap: 4px; }
  .wm-hd h2 {
    margin: 0;
    font: 600 10.5px/1 var(--font-ui);
    letter-spacing: 0.07em;
    text-transform: uppercase;
    color: var(--fg-fainter);
  }
  .subhead {
    margin: 0;
    font: 400 11.5px/1.5 var(--font-ui);
    color: var(--fg-faint);
  }

  .muted { color: var(--fg-faint); font: 400 12px/1.5 var(--font-ui); margin: 0; }
  .bad { color: var(--bad); font: 400 12px/1.5 var(--font-ui); margin: 0; }

  .cards {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }

  .card {
    display: flex;
    align-items: flex-start;
    gap: 10px;
    width: 100%;
    text-align: left;
    padding: 12px 14px;
    border-radius: var(--r-2xl);
    border: 1px solid var(--line);
    background: var(--bg-panel);
    color: inherit;
    font: inherit;
    cursor: pointer;
  }
  .card:hover:not(:disabled):not(.selected) { border-color: var(--line-strong); }
  .card:disabled { cursor: default; }
  .card.selected {
    border-color: var(--accent-border);
    background: var(--accent-sel);
  }

  .radio {
    flex: 0 0 auto;
    width: 15px;
    height: 15px;
    margin-top: 1px;
    border-radius: var(--r-pill);
    border: 1px solid var(--line-strong);
    background: var(--bg-app);
    display: block;
    position: relative;
  }
  .radio.on {
    border-color: var(--accent);
  }
  .radio.on::after {
    content: "";
    position: absolute;
    inset: 3px;
    border-radius: var(--r-pill);
    background: var(--accent);
  }

  .card-text {
    flex: 1 1 auto;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 3px;
  }
  .card-title {
    font: 600 12.5px/1.35 var(--font-ui);
    color: var(--fg);
  }
  .card-blurb {
    font: 400 11.5px/1.55 var(--font-ui);
    color: var(--fg-dim);
  }
  .saving {
    flex: 0 0 auto;
    font: 400 10.5px/1 var(--font-mono);
    color: var(--fg-faint);
    align-self: center;
  }

  .rules {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding-top: 10px;
    border-top: 1px solid var(--line-soft);
  }
  .rules h2 {
    margin: 0;
    font: 600 11px/1 var(--font-ui);
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--fg-dim);
  }

  .note {
    margin: 0;
    padding: 10px 12px;
    border: 1px dashed var(--line-strong);
    border-radius: var(--r-lg);
    background: var(--bg-panel);
    font: 400 11.5px/1.55 var(--font-ui);
    color: var(--fg-dim);
    font-style: italic;
  }
  .note code { font-style: normal; color: var(--fg-2); }
  .warn-note {
    border-color: var(--warn-border);
    color: var(--warn);
  }
  .warn-note code { color: var(--warn); }

  .footnote {
    margin: 0;
    font: 400 11px/1.55 var(--font-ui);
    color: var(--fg-fainter);
  }
  .footnote code { color: var(--fg-dim); }
</style>
