<script lang="ts">
  import type {
    MachineAccessPolicy,
    MachineAccessView,
    MachineEnvVar,
    MachineGrant,
    MachineGrantAccess,
    MachineScope,
  } from "$lib/node";
  import "$lib/styles/machine-access.css";
  import * as m from "./machineAccessModel";

  // One Machine access tab: the tools, folders and variables the Baker,
  // every project or one project gets. Settings → Machine access shows a tab
  // at a time; a project's Environment card shows its own, the same entries.
  // Each edit goes through `change`, which saves it.

  let {
    policy = $bindable(),
    view,
    tab,
    change,
    toAll,
  }: {
    policy: MachineAccessPolicy;
    view: MachineAccessView;
    tab: m.Audience;
    /** Makes an edit and saves it. */
    change: (edit: () => void) => void;
    /** Where a project's inherited entry leads: the all-projects tab, or
     *  Settings → Machine access when absent. */
    toAll?: () => void;
  } = $props();

  const ACCESS: { value: MachineGrantAccess; label: string }[] = [
    { value: "read", label: "Read" },
    { value: "write", label: "Write" },
    { value: "exec", label: "Run" },
    { value: "full", label: "Write + run" },
  ];
  const accessLabel = (g: MachineGrant) =>
    `${ACCESS.find((a) => a.value === g.access)?.label ?? g.access}${g.on_path ? " · PATH" : ""}`;
  const tilde = (path: string) => m.tilde(path, view.home);

  const listed = $derived({ folders: m.folders(policy, view.presets, tab), variables: m.variables(policy, view.presets, tab) });
  /** The other tab a Baker or all-projects entry can be shared with. */
  const shareWith = $derived(tab.kind === "baker" ? "All projects" : "Baker");

  let newPath = $state("");
  let newAccess = $state<MachineGrantAccess>("read");
  let newName = $state("");
  let newValue = $state("");

  function addFolder() {
    if (!newPath.trim()) return;
    const [path, access] = [newPath, newAccess];
    newPath = "";
    change(() => m.addFolder(policy, tab, path, access, view.home));
  }

  function addVariable() {
    if (!newName.trim()) return;
    const [name, value] = [newName, newValue];
    newName = "";
    newValue = "";
    change(() => m.addVariable(policy, tab, name, value));
  }

  function removeFolder(g: MachineGrant) {
    change(() => {
      if (tab.kind === "project") {
        const own = m.ownOf(policy, tab.id);
        own.grants = m.remove(own.grants, g, tab);
      } else policy.grants = m.remove(policy.grants, g, tab);
    });
  }

  function removeVariable(v: MachineEnvVar) {
    change(() => {
      if (tab.kind === "project") {
        const own = m.ownOf(policy, tab.id);
        own.env = m.remove(own.env, v, tab);
      } else policy.env = m.remove(policy.env, v, tab);
    });
  }

  function share(entry: { scope: MachineScope }) {
    if (tab.kind !== "project") {
      const kind = tab.kind;
      change(() => m.toggleShared(entry, kind));
    }
  }
</script>

{#snippet folderIcon()}
  <svg class="ic" viewBox="0 0 16 16" aria-hidden="true">
    <path d="M1.5 4.5a1 1 0 0 1 1-1h3.6l1.4 1.5h5.9a1 1 0 0 1 1 1v6.5a1 1 0 0 1-1 1h-10.9a1 1 0 0 1-1-1z" fill="none" stroke="currentColor" stroke-width="1.3" />
  </svg>
{/snippet}

{#snippet accessSeg(value: MachineGrantAccess, pick: (next: MachineGrantAccess) => void, what: string)}
  <span class="seg acc" role="group" aria-label="Access for {what}">
    {#each ACCESS as a (a.value)}
      <button type="button" class:on={value === a.value} aria-pressed={value === a.value} onclick={() => pick(a.value)}>{a.label}</button>
    {/each}
  </span>
{/snippet}

{#snippet fromAll()}
  {#if toAll}
    <button type="button" class="from" onclick={toAll}>All projects</button>
  {:else}
    <a class="from" href="/settings/machine-access">All projects</a>
  {/if}
{/snippet}

<div class="ma grants">
  <h3 class="sub-h">Tools</h3>
  <div class="tools">
    {#each view.presets as p (p.id)}
      {@const tool = m.toolState(policy, p, tab)}
      <div class="tool" class:on={!p.unavailable && tool !== "off"} class:off-mac={!!p.unavailable} data-testid="preset-{p.id}">
        <span class="mono-ic" aria-hidden="true">{p.label.slice(0, 2)}</span>
        <span class="t" title={[p.detail, ...p.grants.map((g) => g.path)].filter(Boolean).join("\n")}>
          <b>{p.label}</b>
          <i>{p.grants.map((g) => tilde(g.path)).join(" · ") || p.detail}</i>
        </span>
        {#if p.unavailable}
          <span class="dim">{p.unavailable}</span>
        {:else if tool === "inherited"}
          {@render fromAll()}
        {:else}
          <button
            type="button"
            class="sw"
            class:on={tool === "on"}
            role="switch"
            aria-checked={tool === "on"}
            aria-label={p.label}
            onclick={() => change(() => m.setTool(policy, p, tab, tool !== "on"))}
          ></button>
        {/if}
      </div>
    {/each}
  </div>

  <h3 class="sub-h">Folders</h3>
  <div class="card">
    {#each listed.folders.inherited as g (g.path + "|" + g.scope)}
      <div class="row inherit">
        {@render folderIcon()}
        <span class="path" title={g.path}>{tilde(g.path)}</span>
        <span class="lvl">{accessLabel(g)}</span>
        {@render fromAll()}
      </div>
    {/each}
    {#each listed.folders.own as g (g.path + "|" + g.scope)}
      {@const gone = view.missing.includes(g.path)}
      {@const blocked = view.blocked[g.path]}
      <div class="row" class:gone={gone || !!blocked}>
        {@render folderIcon()}
        <span class="path" title={g.path}>{tilde(g.path)}</span>
        {#if gone}<span class="missing" data-testid="grant-missing">Missing</span>{/if}
        {#if blocked}<span class="missing" title={blocked} data-testid="grant-blocked">Blocked</span>{/if}
        {#if g.access === "exec"}
          <span class="pathsw">
            <button
              type="button"
              class="sw"
              class:on={g.on_path}
              role="switch"
              aria-checked={g.on_path}
              aria-label="{g.path} on PATH"
              onclick={() => change(() => (g.on_path = !g.on_path))}
            ></button>PATH
          </span>
        {/if}
        {@render accessSeg(g.access, (next) => change(() => m.setAccess(g, next)), g.path)}
        {#if tab.kind !== "project"}
          <button type="button" class="share" class:bk={tab.kind === "all"} class:on={m.shared(g)} aria-pressed={m.shared(g)} onclick={() => share(g)}>{shareWith}</button>
        {/if}
        <button type="button" class="x" aria-label="Remove {g.path}" onclick={() => removeFolder(g)}>×</button>
      </div>
    {/each}
    <div class="add">
      <input
        class="m grow"
        bind:value={newPath}
        placeholder="~/path/to/folder"
        aria-label="Folder to add"
        onkeydown={(e) => e.key === "Enter" && addFolder()}
      />
      {@render accessSeg(newAccess, (next) => (newAccess = next), "the new folder")}
      <button type="button" class="btn p" onclick={addFolder} disabled={!newPath.trim()}>Add</button>
    </div>
  </div>

  <h3 class="sub-h">Variables</h3>
  <div class="card">
    {#each listed.variables.inherited as v (v.name)}
      <div class="row inherit">
        <span class="var">{v.name}</span><span class="eq">=</span>
        <span class="path">{v.value}</span>
        {@render fromAll()}
      </div>
    {/each}
    {#each listed.variables.own as v (v.name)}
      <div class="row">
        <span class="var">{v.name}</span><span class="eq">=</span>
        <input
          class="grow"
          value={v.value}
          aria-label="Value of {v.name}"
          onchange={(e) => {
            const value = e.currentTarget.value;
            change(() => (v.value = value));
          }}
        />
        {#if tab.kind !== "project"}
          <button type="button" class="share" class:bk={tab.kind === "all"} class:on={m.shared(v)} aria-pressed={m.shared(v)} onclick={() => share(v)}>{shareWith}</button>
        {/if}
        <button type="button" class="x" aria-label="Remove {v.name}" onclick={() => removeVariable(v)}>×</button>
      </div>
    {/each}
    <div class="add">
      <input class="m name-in" bind:value={newName} placeholder="NAME" aria-label="Variable name" />
      <input
        class="grow"
        bind:value={newValue}
        placeholder="value"
        aria-label="Variable value"
        onkeydown={(e) => e.key === "Enter" && addVariable()}
      />
      <button type="button" class="btn p" onclick={addVariable} disabled={!newName.trim()}>Add</button>
    </div>
  </div>
</div>

<style>
  .grants { display: flex; flex-direction: column; min-width: 0; }
</style>
