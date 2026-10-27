<script lang="ts">
  import { onMount } from "svelte";
  import {
    node,
    type AgentAbility,
    type MachineAccessPolicy,
    type MachineAccessView,
    type MachineCheck,
    type MachineReach,
  } from "$lib/node";
  import { studioStore } from "$lib/studioStore.svelte";
  import "$lib/styles/machine-access.css";
  import CheckResult from "./CheckResult.svelte";
  import MachineGrants from "./MachineGrants.svelte";
  import * as m from "./machineAccessModel";

  // Settings → Machine access: where each agent runs, what the Baker, every
  // project and each project get beyond their workspace, what stays blocked
  // and where container seats go. Every change saves at once; one the node
  // refuses goes back to what is saved, with its reason.

  let view = $state<MachineAccessView | null>(null);
  let draft = $state<MachineAccessPolicy>({ grants: [], env: [], projects: {}, deny: [], reach: {}, allow_hosts: [] });
  let loadError = $state("");
  let saving = $state(false);
  let saveError = $state("");
  let busy = $state(0);
  let queued = false;

  function adopt(next: MachineAccessView) {
    view = next;
    draft = structuredClone(next.policy);
    draft.projects ??= {};
    draft.deny ??= [];
    draft.allow_hosts ??= [];
  }

  /** Saves the draft; a change made while one is in flight goes in the next. */
  async function persist() {
    if (saving) {
      queued = true;
      return;
    }
    saving = true;
    saveError = "";
    try {
      for (;;) {
        queued = false;
        const out = await node.setMachineAccess($state.snapshot(draft));
        busy = out.busy.length;
        if (!queued) {
          adopt(out);
          break;
        }
        view = out;
      }
    } catch (e) {
      saveError = `${e}`;
      if (view) adopt(view);
    } finally {
      saving = false;
    }
  }

  function change(edit: () => void) {
    edit();
    void persist();
  }

  const tilde = (path: string) => m.tilde(path, view?.home ?? "");

  let proxyHosts = $state<string[]>([]);
  onMount(() => {
    studioStore.ensureStarted();
    node
      .machineAccess()
      .then(adopt)
      .catch((e) => (loadError = `${e}`));
    node
      .modelProxy()
      .then((p) => (proxyHosts = p.installed ? p.egress_hosts : []))
      .catch(() => {});
  });

  // ─── Reach ──────────────────────────────────────────────────────────────

  const REACHES: { value: MachineReach; label: string }[] = [
    { value: "host_full", label: "Whole Mac" },
    { value: "host_room", label: "Workspace + grants" },
    { value: "container", label: "Container" },
    { value: "locked", label: "Locked" },
  ];

  let agents = $state<AgentAbility[]>([]);
  let reaching = $state<string | null>(null);
  const reachOf = $derived(new Map(agents.map((a) => [a.stream_id, a.reach])));
  const live = $derived(new Set(agents.filter((a) => a.runtime || a.busy).map((a) => a.stream_id)));
  const groups = $derived(studioStore.data ? m.reachGroups(studioStore.data) : []);
  const streamIds = $derived(groups.flatMap((g) => g.rows.map((r) => r.stream_id)).join(","));

  async function loadAgents() {
    try {
      agents = (await node.agents()).agents;
    } catch (e) {
      saveError = `${e}`;
    }
  }
  // Each open conversation's place, read again as conversations open or close.
  $effect(() => {
    void streamIds;
    void loadAgents();
  });

  async function setReach(stream_id: string, reach: MachineReach) {
    reaching = stream_id;
    saveError = "";
    try {
      await node.setAgentReach(stream_id, reach);
    } catch (e) {
      saveError = `${e}`;
    } finally {
      reaching = null;
      await loadAgents();
    }
  }

  // ─── Grants ─────────────────────────────────────────────────────────────

  let tab = $state<m.Audience>({ kind: "baker" });
  const keyOf = (a: m.Audience) => (a.kind === "project" ? `project:${a.id}` : a.kind);
  const tabs = $derived<{ a: m.Audience; label: string }[]>([
    { a: { kind: "baker" }, label: "Baker" },
    { a: { kind: "all" }, label: "All projects" },
    ...(studioStore.data?.projects ?? []).map((p) => ({ a: { kind: "project", id: p.project_id } as m.Audience, label: p.name })),
  ]);

  // ─── Always blocked, container seats ───────────────────────────────────

  let newDeny = $state("");
  function block() {
    const path = newDeny.trim();
    newDeny = "";
    if (path && !draft.deny.includes(path)) change(() => draft.deny.push(path));
  }

  let newHost = $state("");
  function allowHost() {
    const host = newHost.trim().toLowerCase();
    newHost = "";
    if (host && !draft.allow_hosts.includes(host)) change(() => draft.allow_hosts.push(host));
  }

  // ─── Check ──────────────────────────────────────────────────────────────

  let checking = $state<"baker" | "agents" | null>(null);
  let check = $state<{ scope: "baker" | "agents"; result: MachineCheck } | null>(null);
  let checkError = $state("");
  async function runCheck(scope: "baker" | "agents") {
    checking = scope;
    checkError = "";
    try {
      check = { scope, result: await node.checkMachineAccess(scope) };
    } catch (e) {
      checkError = `${e}`;
    } finally {
      checking = null;
    }
  }
</script>

{#snippet lockIcon()}
  <svg class="ic" viewBox="0 0 16 16" aria-hidden="true">
    <rect x="3" y="7" width="10" height="7" rx="1.5" fill="none" stroke="currentColor" stroke-width="1.3" />
    <path d="M5.5 7V5a2.5 2.5 0 0 1 5 0v2" fill="none" stroke="currentColor" stroke-width="1.3" />
  </svg>
{/snippet}

<div class="set-panel ma" data-testid="machine-access-panel">
  {#if view}
    <span
      class="state"
      class:bad={!!saveError}
      class:busy={saving || reaching !== null}
      title={saveError || undefined}
      data-testid="machine-access-state"
    >
      {#if saveError}✗ {saveError}{:else if saving || reaching !== null}Saving…{:else}✓ Saved{busy ? ` · ${busy} after their turn` : ""}{/if}
    </span>
  {/if}

  {#if loadError}
    <p class="set-error">{loadError}</p>
  {:else if !view}
    <p class="set-muted">Loading…</p>
  {:else}
    <section class="set-section" data-testid="machine-access-reach">
      <h2 class="set-section-h">Reach</h2>
      <div class="card">
        {#each groups as g (g.key)}
          {@const rows = g.rows.filter((r) => reachOf.has(r.stream_id))}
          {#if rows.length}
            <div class="grp">{#if g.baker}<span class="tag baker">{g.label}</span>{:else}{g.label}{/if}</div>
            {#each rows as r (r.stream_id)}
              {@const at = reachOf.get(r.stream_id)}
              <div class="row" style:padding-left="{14 + r.depth * 18}px" data-testid="ma-reach-row">
                <span class="who">
                  <i class="dot" class:live={live.has(r.stream_id)}></i>
                  <span class="name" title={r.label}>{r.label}</span>
                  {#if r.orc}<span class="tag orc">orc</span>{/if}
                </span>
                <span class="seg" role="group" aria-label="Where {r.label} runs">
                  {#each REACHES as opt (opt.value)}
                    <button
                      type="button"
                      data-v={opt.value}
                      class:on={at === opt.value}
                      aria-pressed={at === opt.value}
                      disabled={reaching !== null}
                      onclick={() => at !== opt.value && void setReach(r.stream_id, opt.value)}>{opt.label}</button
                    >
                  {/each}
                </span>
              </div>
            {/each}
          {/if}
        {/each}
      </div>
    </section>

    <section class="set-section" data-testid="machine-access-grants">
      <h2 class="set-section-h">Grants</h2>
      <div class="tabs" role="tablist">
        {#each tabs as t (keyOf(t.a))}
          <button
            type="button"
            role="tab"
            class:on={keyOf(t.a) === keyOf(tab)}
            aria-selected={keyOf(t.a) === keyOf(tab)}
            onclick={() => (tab = t.a)}
            data-testid="ma-tab-{keyOf(t.a)}"
          >
            {t.label} <span class="n">{m.count(draft, view.presets, t.a)}</span>
          </button>
        {/each}
      </div>

      <MachineGrants bind:policy={draft} {view} {tab} {change} toAll={() => (tab = { kind: "all" })} />
    </section>

    <section class="set-section" data-testid="machine-access-blocked">
      <h2 class="set-section-h">Always blocked</h2>
      <div class="card">
        <div class="grp">Yours</div>
        {#if draft.deny.length}
          <div class="chips">
            {#each draft.deny as p, i (p)}
              <span class="chip lock own" title={p}>
                {@render lockIcon()}{tilde(p)}
                <button type="button" class="cx" aria-label="Stop blocking {p}" onclick={() => change(() => draft.deny.splice(i, 1))}>×</button>
              </span>
            {/each}
          </div>
        {/if}
        <div class="add">
          <input
            class="m grow"
            bind:value={newDeny}
            placeholder="~/path/to/secrets"
            aria-label="Folder or file to block"
            onkeydown={(e) => e.key === "Enter" && block()}
          />
          <button type="button" class="btn" onclick={block} disabled={!newDeny.trim()}>Block</button>
        </div>
        <div class="grp">Built in</div>
        <div class="chips">
          {#each view.always_denied as p (p)}<span class="chip lock" title={p}>{tilde(p)}</span>{/each}
        </div>
      </div>
    </section>

    <section class="set-section" data-testid="machine-access-seat-hosts">
      <h2 class="set-section-h">Container seats reach</h2>
      <div class="card">
        <div class="grp">Yours</div>
        {#if draft.allow_hosts.length}
          <div class="chips">
            {#each draft.allow_hosts as h, i (h)}
              <span class="chip host own">
                {h}
                <button type="button" class="cx" aria-label="Stop allowing {h}" onclick={() => change(() => draft.allow_hosts.splice(i, 1))}>×</button>
              </span>
            {/each}
          </div>
        {/if}
        <div class="add">
          <input
            class="m grow"
            bind:value={newHost}
            placeholder="registry.example.com"
            aria-label="Host container seats may reach"
            onkeydown={(e) => e.key === "Enter" && allowHost()}
          />
          <button type="button" class="btn" onclick={allowHost} disabled={!newHost.trim()}>Allow</button>
        </div>
        <div class="grp">Built in</div>
        <div class="chips">
          {#each view.seat_hosts as h (h)}<span class="chip host">{h}</span>{/each}
        </div>
        {#if proxyHosts.length}
          <div class="grp">Model proxy</div>
          <div class="chips">
            {#each proxyHosts as h (h)}<span class="chip proxy">{h}</span>{/each}
          </div>
        {/if}
      </div>
    </section>

    <section class="set-section" data-testid="machine-access-check">
      <h2 class="set-section-h">Check</h2>
      <div class="bar">
        <button type="button" class="btn" class:p={check?.scope !== "agents"} onclick={() => void runCheck("baker")} disabled={checking !== null || saving}>
          {checking === "baker" ? "Checking…" : "As the Baker"}
        </button>
        <button type="button" class="btn" class:p={check?.scope === "agents"} onclick={() => void runCheck("agents")} disabled={checking !== null || saving}>
          {checking === "agents" ? "Checking…" : "As a project agent"}
        </button>
      </div>
      {#if checkError}<p class="set-error">{checkError}</p>{/if}
      {#if check}<CheckResult result={check.result} home={view.home} />{/if}
    </section>
  {/if}
</div>

<!-- The shared look is machine-access.css; only what is this page's own is here. -->
<style>
  .ma { position: relative; }
  .ma .set-section { gap: 0; }

  /* The save state, on the page title's line. */
  .ma .state { position: absolute; top: -52px; right: 0; }

  .tabs { display: flex; flex-wrap: wrap; gap: 6px; margin: 2px 0 4px; }
  .tabs button {
    display: inline-flex;
    align-items: center;
    gap: 7px;
    padding: 7px 12px;
    border: 1px solid var(--line);
    border-radius: var(--r-xl);
    background: var(--bg-panel);
    color: var(--fg-dim);
    font: 500 12.5px/1 var(--font-ui);
    cursor: pointer;
  }
  .tabs button:hover:not(.on) { color: var(--fg-2); background: var(--bg-raised); }
  .tabs button:focus-visible { outline: 2px solid var(--accent); outline-offset: 1px; }
  .tabs button.on {
    color: var(--fg);
    border-color: var(--accent-border);
    background: linear-gradient(180deg, color-mix(in srgb, var(--accent) 22%, transparent), color-mix(in srgb, var(--accent) 5%, transparent));
    box-shadow: 0 0 0 3px color-mix(in srgb, var(--accent) 8%, transparent);
  }
  .tabs button.on .n { background: var(--accent-solid); color: var(--fg-on-solid); }

  .bar { display: flex; gap: 8px; margin: 2px 0 10px; }

  @container settings (max-width: 560px) {
    .ma .state { position: static; align-self: flex-start; max-width: 100%; }
  }
</style>

