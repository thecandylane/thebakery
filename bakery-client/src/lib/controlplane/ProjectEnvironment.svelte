<script lang="ts">
  import { onMount } from "svelte";
  import { node, type MachineAccessPolicy, type MachineAccessView, type MachineCheck } from "$lib/node";
  import "$lib/styles/machine-access.css";
  import CheckResult from "./CheckResult.svelte";
  import MachineGrants from "./MachineGrants.svelte";
  import * as m from "./machineAccessModel";

  // One project's own environment: tools, folders and variables only its
  // agents and tasks get, on top of the all-projects ones. The same entries
  // as its tab in Settings → Machine access. Every change saves at once,
  // into the policy as it stands then, so another page's edits stay.

  let { projectId }: { projectId: string } = $props();

  let view = $state<MachineAccessView | null>(null);
  let draft = $state<MachineAccessPolicy>({ grants: [], env: [], projects: {}, deny: [], reach: {}, allow_hosts: [] });
  let error = $state("");
  let saving = $state(false);
  let queued = false;
  let checking = $state(false);
  let check = $state<MachineCheck | null>(null);
  const tab = $derived<m.Audience>({ kind: "project", id: projectId });

  function adopt(next: MachineAccessView) {
    view = next;
    draft = structuredClone(next.policy);
    draft.projects ??= {};
  }

  onMount(() => {
    node
      .machineAccess()
      .then(adopt)
      .catch((e) => (error = `${e}`));
  });

  /** Saves this project's own entries; a change made while one is in
   *  flight goes in the next. */
  async function persist() {
    if (saving) {
      queued = true;
      return;
    }
    saving = true;
    error = "";
    try {
      for (;;) {
        queued = false;
        const policy = structuredClone((await node.machineAccess()).policy);
        const own = $state.snapshot(draft.projects?.[projectId]) ?? { grants: [], env: [] };
        policy.projects = { ...(policy.projects ?? {}), [projectId]: own };
        const out = await node.setMachineAccess(policy);
        if (!queued) {
          adopt(out);
          break;
        }
        view = out;
      }
    } catch (e) {
      error = `${e}`;
      if (view) adopt(view);
    } finally {
      saving = false;
    }
  }

  function change(edit: () => void) {
    edit();
    void persist();
  }

  async function runCheck() {
    checking = true;
    error = "";
    try {
      check = await node.checkMachineAccess("agents", projectId);
    } catch (e) {
      error = `${e}`;
    } finally {
      checking = false;
    }
  }
</script>

<details class="env ma" id="environment" open={location.hash === "#environment"} data-testid="project-environment">
  <summary>
    <span class="title">Environment</span>
    {#if view}<span class="n">{m.count(draft, view.presets, tab)}</span>{/if}
    <span class="grow"></span>
    {#if view}
      <span class="state" class:bad={!!error} class:busy={saving} title={error || undefined} data-testid="project-environment-state">
        {#if error}✗ {error}{:else if saving}Saving…{:else}✓ Saved{/if}
      </span>
    {/if}
  </summary>
  {#if !view}
    <p class="muted">{error || "Loading…"}</p>
  {:else}
    <MachineGrants bind:policy={draft} {view} {tab} {change} />
    <div class="bar">
      <button type="button" class="btn" onclick={() => void runCheck()} disabled={checking || saving}>
        {checking ? "Checking…" : "Check"}
      </button>
    </div>
    {#if check}<CheckResult result={check} home={view.home} />{/if}
  {/if}
</details>

<style>
  .env {
    display: flex;
    flex-direction: column;
    margin-top: 22px;
    padding: 12px 14px 14px;
    border: 1px solid var(--line);
    border-radius: var(--r-2xl);
    background: linear-gradient(180deg, var(--bg-raised), var(--bg-panel) 52px);
    box-shadow: inset 0 1px 0 color-mix(in srgb, var(--fg) 4%, transparent), var(--shadow-chip);
  }
  summary {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
    cursor: pointer;
  }
  .title { font: 600 13px/1.3 var(--font-ui); color: var(--fg); }
  .grow { flex: 1 1 auto; }
  .muted { margin: 10px 0 0; font: 400 11.5px/1.5 var(--font-ui); color: var(--fg-faint); }
  .bar { display: flex; gap: 8px; margin: 14px 0 10px; }
</style>
