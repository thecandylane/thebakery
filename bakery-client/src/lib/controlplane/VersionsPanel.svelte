<script lang="ts">
  import { onMount } from "svelte";
  import { ask } from "$lib/ask";
  import { whileShown } from "$lib/pageShown";
  import {
    node,
    type BakeryAction,
    type BakeryControl,
    type DeployedBuild,
    type VersionComponent,
    type VersionsReport,
  } from "$lib/node";

  // Settings → Versions. GET /versions reads what actually runs — this node
  // process, the installed package manifests, each live session's own
  // handshake — never a pinned constant. "Check for updates" asks the public
  // npm registry, only when clicked. Nothing here installs: the Claude
  // native adapter is pinned and patched, so upgrading it is a reviewed change.

  let report = $state<VersionsReport | null>(null);
  let loadError = $state("");
  let checking = $state(false);

  async function load(check = false) {
    loadError = "";
    if (check) checking = true;
    try {
      report = check ? await node.checkVersions() : await node.versions();
    } catch (e) {
      loadError = `${e}`;
    } finally {
      checking = false;
    }
  }

  // Bakery (D1): the app runs from a deployed build, so agent edits change
  // the source only until Deploy. Each button runs scripts/bakery.sh on the
  // node, which stops the node part-way: while an action runs this polls,
  // and a failed poll then means "restarting", not an error.
  let control = $state<BakeryControl | null>(null);
  let controlError = $state("");
  let offline = $state(false);
  // Polls missed in a row; a minute's worth means the node is not coming back by itself.
  let missed = $state(0);
  const busy = $derived(Boolean(control?.last?.running));

  async function loadControl() {
    try {
      const next = await node.bakeryControl();
      // Back after a restart, or done: show what the node runs now.
      if (offline || (control?.last?.running && !next.last?.running)) void load();
      control = next;
      controlError = "";
      offline = false;
      missed = 0;
    } catch (e) {
      if (control?.last?.running) {
        offline = true;
        missed += 1;
      } else controlError = `${e}`;
    }
  }

  $effect(() => {
    if (!busy) return;
    return whileShown(() => void loadControl(), 1500);
  });

  onMount(() => {
    void load();
    void loadControl();
  });

  const INTERRUPTS = "Turns in flight are interrupted; Resume picks them up.";
  // V1: the node upgrades to the release its own update check found, never
  // to a version this page names; the button only appears for that release.
  const adapter = $derived(report?.components.find((c) => c.id === "claude-native-adapter"));

  function confirmText(action: BakeryAction): string {
    switch (action) {
      case "deploy":
        return `Deploy builds the node and the app from the current source (several minutes) and switches to them only if both builds pass. Bakery then restarts on the new build and replaces this window. ${INTERRUPTS} Deploy now?`;
      case "restart":
        return control?.deployed
          ? `Restart the node on the deployed build? The app reconnects by itself. ${INTERRUPTS}`
          : `Nothing is deployed yet, so Restart rebuilds the node from source first. The app reconnects by itself. ${INTERRUPTS} Restart?`;
      case "rollback":
        return `Roll back to ${describe(control?.previous ?? null)}? Bakery restarts on it and replaces this window. ${INTERRUPTS}`;
      case "upgrade-adapter":
        return `Upgrade the Claude native adapter from ${adapter?.installed ?? "the pinned release"} to ${adapter?.latest}? Bakery pins the new release, moves the Bakery patch to it, installs it, checks its handshake, then deploys with the tests (several minutes). Any failure puts everything back. Bakery then restarts on the new build and replaces this window. ${INTERRUPTS}`;
    }
  }

  async function run(action: BakeryAction) {
    if (!(await ask(confirmText(action)))) return;
    controlError = "";
    try {
      control = await node.runBakeryControl(action);
    } catch (e) {
      controlError = `${e}`;
    }
  }

  const ACTION_LABEL: Record<NonNullable<BakeryControl["last"]>["action"], string> = {
    deploy: "Deploy",
    "deploy-ui": "Hot reload",
    restart: "Restart",
    rollback: "Roll back",
    "upgrade-adapter": "The adapter upgrade",
  };

  function describe(build: DeployedBuild | null): string {
    if (!build) return "—";
    const dirty = build.dirty_files ? ` + ${build.dirty_files} uncommitted files` : "";
    return `${build.commit || "—"}${dirty}, built ${when(build.built_at)}`;
  }

  function statusLabel(c: VersionComponent): string {
    switch (c.status) {
      case "ok":
        return c.latest ? "up to date" : "ok";
      case "update_available":
        return "update available";
      case "mismatch":
        return "not what Bakery requires";
      case "missing":
        return "not found";
    }
  }

  const when = (iso: string | null) => (iso ? new Date(iso).toLocaleString() : "—");
</script>

<!-- Settings rows (settings.css): each component its facts on the left and
     its status on the right; the update check sits under the page's lead. -->

<!-- The last button action, shown where it was started: under the Bakery
     buttons, or under the adapter row for an upgrade. -->
{#snippet lastAction(last: NonNullable<BakeryControl["last"]>)}
  <p class="set-status" aria-live="polite" data-testid="bakery-last-action">
    {#if last.running && (offline || last.action === "restart" || last.action === "rollback")}
      Restarting… the app disconnects for a moment and reconnects by itself.
      {#if missed >= 40}
        It has not come back after a minute: ./scripts/bakery.sh status in a terminal says why,
        and .bakery-dev-logs/node.log has the node's output.
      {/if}
    {:else if last.running && last.action === "deploy"}
      Deploying: building the node and the app. Bakery switches to the new build only if both
      builds pass, then restarts on it and replaces this window.
    {:else if last.running && last.action === "deploy-ui"}
      Hot reload: building the UI. The open app reloads on it; nothing restarts.
    {:else if last.running}
      Upgrading the Claude adapter: pinning the new release, installing it with the Bakery patch,
      checking its handshake, then deploying with the tests. Bakery switches only if every step
      passes, then restarts on the new build and replaces this window.
    {:else if last.ok}
      {ACTION_LABEL[last.action]} finished (started {when(last.started_at)}).
    {:else if last.action === "upgrade-adapter" && last.exit_code === 3}
      The Bakery patch does not apply to the new adapter release, so it needs porting before this
      upgrade; the Baker can port it. Nothing changed: the pin, lockfile and patch were put back.
    {:else if last.ok === false}
      {ACTION_LABEL[last.action]} failed (started {when(last.started_at)}). The log below says why
      and what was left running.
    {:else}
      {ACTION_LABEL[last.action]} (started {when(last.started_at)}) ended without reporting a result.
    {/if}
  </p>
  {#if last.log_tail.length}
    <pre class="log">{last.log_tail.join("\n")}</pre>
  {/if}
{/snippet}

<div class="set-panel" data-testid="versions-panel">
  <div class="head">
    <p class="set-lead">
      What this node actually runs, read from the running process and the installed packages —
      not from what Bakery pins. Updates are only looked up when you ask.
    </p>
    <div class="set-actions">
      <button type="button" class="set-btn" onclick={() => void load()} disabled={checking}>Refresh</button>
      <button
        type="button"
        class="set-btn primary"
        onclick={() => void load(true)}
        disabled={checking}
        data-testid="versions-check"
      >
        {checking ? "Checking npm…" : "Check for updates"}
      </button>
      {#if report?.checked_at}
        <span class="set-muted">checked {when(report.checked_at)}</span>
      {/if}
    </div>
    {#if report?.check_error}
      <p class="set-error">Update check failed: {report.check_error}</p>
    {/if}
  </div>

  {#if loadError}
    <p class="set-error">{loadError}</p>
  {:else if !report}
    <p class="set-muted">Reading versions…</p>
  {:else}
    <section class="set-section">
      <h2 class="set-section-h">Bakery</h2>
      <div class="set-row top">
        <span class="set-row-text">
          <span class="set-row-title">Node process</span>
          <span class="set-row-desc">
            started {when(report.build.started_at)}
            {#if report.build.branch}· {report.build.branch}{/if}
            {#if report.build.commit}@ <code>{report.build.commit}</code>{/if}
            {#if report.build.uncommitted_files}· {report.build.uncommitted_files} uncommitted files{/if}
          </span>
          <!-- A deployed node never picks source up on restart; the deployed
               build row below says what is not in it yet. -->
          {#if report.build.changed_since_start && !control?.running_deployed}
            <span class="set-row-desc" data-testid="versions-restart">
              <code>{report.build.changed_since_start}</code> changed after this node started, so it
              isn't running yet. Deploy builds it into the app; <code>./scripts/bakery.sh dev</code>
              runs it from source while you work on Bakery itself.
            </span>
          {/if}
          {#if report.build.source_root}
            <span class="set-mono">{report.build.source_root}</span>
          {/if}
        </span>
        {#if control?.running_deployed}
          <span class="set-chip ok">deployed build</span>
        {:else if report.build.changed_since_start}
          <span class="set-chip warn">older than source</span>
        {:else}
          <span class="set-chip ok">current</span>
        {/if}
      </div>

      <div class="set-row top" data-testid="bakery-deployed">
        <span class="set-row-text">
          <span class="set-row-title">Deployed build</span>
          {#if !control}
            <span class="set-row-desc">{controlError || "Reading the deployed build…"}</span>
          {:else if control.deployed}
            <span class="set-row-desc">
              <code>{control.deployed.commit || "—"}</code>
              {#if control.deployed.dirty_files}+ {control.deployed.dirty_files} uncommitted files{/if}
              · built {when(control.deployed.built_at)}
              {#if control.previous}· Roll back returns to <code>{control.previous.commit || "—"}</code>{/if}
            </span>
            {#if control.undeployed_count}
              <span class="set-row-desc">
                Edits since then — yours and the agents' — are in the source only. Deploy builds them into
                the app.
              </span>
              <span class="set-mono">
                {control.undeployed.join(" · ")}{#if control.undeployed_count > control.undeployed.length} · …{/if}
              </span>
            {/if}
          {:else}
            <span class="set-row-desc">
              Nothing is deployed yet, so the app runs from source and agent edits to it reload this
              window. Deploy builds the node and the app from the source; after that the app changes
              only when you deploy.
            </span>
          {/if}
        </span>
        {#if control?.deployed}
          {#if control.undeployed_count}
            <span class="set-chip warn">
              {control.undeployed_count}
              {control.undeployed_count === 1 ? "change" : "changes"} not deployed
            </span>
          {:else}
            <span class="set-chip ok">up to date</span>
          {/if}
        {:else if control}
          <span class="set-chip warn">not deployed</span>
        {/if}
      </div>

      <div class="set-actions">
        <button
          type="button"
          class="set-btn primary"
          disabled={!control || busy}
          onclick={() => void run("deploy")}
          data-testid="bakery-deploy"
        >
          Deploy
        </button>
        <button type="button" class="set-btn" disabled={!control || busy} onclick={() => void run("restart")}>
          Restart
        </button>
        <button
          type="button"
          class="set-btn"
          disabled={!control?.previous || busy}
          onclick={() => void run("rollback")}
        >
          Roll back
        </button>
      </div>
      {#if control && controlError}
        <p class="set-error">{controlError}</p>
      {/if}
      {#if control?.last && control.last.action !== "upgrade-adapter"}
        {@render lastAction(control.last)}
      {/if}
    </section>

    <section class="set-section">
      <h2 class="set-section-h">Claude native runtime</h2>
      {#each report.components as c (c.id)}
        <div class="set-row top" data-testid="version-row">
          <span class="set-row-text">
            <span class="set-row-title">{c.label}</span>
            <span class="set-row-desc">
              installed <code>{c.installed ?? "—"}</code>
              {#if c.required}· requires <code>{c.required}</code>{/if}
              · latest <code>{c.latest ?? (report.checked_at ? "—" : "not checked")}</code>
            </span>
            {#if c.status !== "ok"}
              <span class="set-row-desc">{c.update}</span>
            {/if}
            <span class="set-mono">{c.source}</span>
            {#if c.id === "claude-native-adapter" && c.status === "update_available" && c.latest}
              <div class="set-actions upgrade">
                <button
                  type="button"
                  class="set-btn primary sm"
                  disabled={!control || busy}
                  onclick={() => void run("upgrade-adapter")}
                  data-testid="adapter-upgrade"
                >
                  Upgrade to {c.latest}
                </button>
              </div>
            {/if}
          </span>
          <span
            class="set-chip"
            class:ok={c.status === "ok"}
            class:warn={c.status === "update_available"}
            class:bad={c.status === "mismatch" || c.status === "missing"}
          >
            {statusLabel(c)}
          </span>
          {#if c.id === "claude-native-adapter" && control?.last && control.last.action === "upgrade-adapter"}
            <div class="set-row-more">{@render lastAction(control.last)}</div>
          {/if}
        </div>
      {/each}
    </section>

    <section class="set-section">
      <h2 class="set-section-h">Live sessions</h2>
      {#if report.sessions.length === 0}
        <p class="set-muted">No native session is running. Each one reports its own runtime when it starts.</p>
      {:else}
        {#each report.sessions as s (`${s.stream_id}:${s.seat}`)}
          <div class="set-row top">
            <span class="set-row-text">
              <span class="set-row-title">{s.runtime ?? s.account_id} <code>{s.version ?? ""}</code></span>
              <span class="set-row-desc">
                {s.account_id} · {s.seat}{#if s.model} · {s.model}{/if}
              </span>
              <span class="set-mono">{s.stream_id}</span>
            </span>
            {#if s.outdated}
              <span class="set-chip warn">older than installed</span>
            {/if}
          </div>
        {/each}
      {/if}
    </section>
  {/if}
</div>

<style>
  /* The update check belongs to the page's lead, not to a section below. */
  .head {
    display: flex;
    flex-direction: column;
    gap: 2px;
    margin-top: -20px;
  }
  .head .set-actions { padding: 8px 0 0; }
  .set-actions.upgrade { padding: 6px 0 0; }
  /* The last action's output: bounded, scrolls, wraps long paths. */
  .log {
    margin: 6px 0 0;
    max-height: 280px;
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
</style>
