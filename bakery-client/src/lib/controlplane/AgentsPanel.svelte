<script lang="ts">
  import { onMount } from "svelte";
  import { whileShown } from "$lib/pageShown";
  import {
    node,
    SYSTEM_ACCOUNTS_STREAM_ID,
    type AcpRuntimeSnapshot,
    type AgentAbility,
    type AgentsView,
    type MachineReach,
  } from "$lib/node";
  import Terminal from "$lib/widgets/Terminal.svelte";
  import { studioStore } from "$lib/studioStore.svelte";
  import ConversationActions from "$lib/shell/ConversationActions.svelte";
  import { reportError } from "$lib/clientLog";

  // Settings → Agents. Every agent on every socket — the Baker, project
  // agents, task workers — whatever backs it: Claude, Codex, Cursor, Grok,
  // Antigravity, or an API model. A native runtime's abilities are what it
  // reported about itself when it started, never a per-vendor list.

  let view = $state<AgentsView | null>(null);
  let loadError = $state("");
  let starting = $state<string | null>(null);
  let startError = $state("");

  /** The node's answer, or a failure that says so after 20 s: the page
   *  never waits silently. */
  const LIMIT_MS = 20_000;
  async function load() {
    loadError = "";
    let timer: ReturnType<typeof setTimeout> | undefined;
    try {
      view = await Promise.race([
        node.agents(),
        new Promise<never>((_, reject) => {
          timer = setTimeout(() => reject(new Error("The node didn't answer in 20 seconds.")), LIMIT_MS);
        }),
      ]);
    } catch (e) {
      loadError = e instanceof Error ? e.message : `${e}`;
    } finally {
      clearTimeout(timer);
    }
  }

  onMount(() => {
    studioStore.ensureStarted();
    void load();
    void checkSeatImage();
  });

  async function start(a: AgentAbility) {
    if (!a.model_id) return;
    starting = a.agent_id;
    startError = "";
    try {
      await node.accountCapabilities(a.model_id, a.stream_id, a.seat);
      await load();
    } catch (e) {
      startError = `${a.socket}: ${e}`;
    } finally {
      starting = null;
    }
  }

  // Reach is per conversation: its runtime restarts under it, now or after
  // its turn.
  let reaching = $state<string | null>(null);
  async function setReach(a: AgentAbility, reach: MachineReach) {
    reaching = a.agent_id;
    startError = "";
    try {
      await node.setAgentReach(a.stream_id, reach);
    } catch (e) {
      startError = `${a.socket}: ${e}`;
    } finally {
      reaching = null;
      await load();
    }
  }

  // A container seat's share of the Mac: blank is the default sized from it.
  async function setSize(a: AgentAbility, key: "cpus" | "memory_gib", raw: string) {
    if (!view?.seat_default) return;
    const base = view.seat_default;
    const next = { ...(a.seat_size ?? base), [key]: raw.trim() === "" ? base[key] : Number(raw) };
    const size = next.cpus === base.cpus && next.memory_gib === base.memory_gib ? null : next;
    reaching = a.agent_id;
    startError = "";
    try {
      await node.setSeatSize(a.stream_id, size);
    } catch (e) {
      startError = `${a.socket}: ${e}`;
    } finally {
      reaching = null;
      await load();
    }
  }

  // Containers are off for every agent until the operator turns them on here.
  async function setContainers(a: AgentAbility, on: boolean) {
    reaching = a.agent_id;
    startError = "";
    try {
      await node.setAgentContainers(a.stream_id, on);
    } catch (e) {
      startError = `${a.socket}: ${e}`;
    } finally {
      reaching = null;
      await load();
    }
  }

  // Claude in containers signs in once, in a pane right here.
  let signInTerminal = $state<string | null>(null);
  // The seat image, built from this node's own recipe when it's missing or stale.
  let seatImage = $state("current");
  let imageTerminal = $state<string | null>(null);
  let building = $state(false);
  /** How the last build ended, when not well: cancelled, or failed. */
  let buildNote = $state("");
  async function checkSeatImage() {
    seatImage = (await node.seatImage().catch(() => ({ state: "missing" }))).state;
    // A build already running (started here earlier, or by the Baker) shows its pane.
    const running = (await node.terminalList(SYSTEM_ACCOUNTS_STREAM_ID).catch(() => [])).find(
      (t) => t.live && t.name === "container build",
    );
    building = running !== undefined;
    if (running && imageTerminal !== running.terminal_id) {
      imageTerminal = running.terminal_id;
      buildNote = "";
    }
  }
  // The node builds the image itself when it's needed: while it isn't
  // current, look for a running build now and then, so one it starts shows
  // here as it goes, its progress live in its pane.
  $effect(() => {
    if (seatImage === "current" || imageTerminal) return;
    return whileShown(() => void checkSeatImage(), 5000);
  });
  async function buildSeatImage() {
    startError = "";
    buildNote = "";
    try {
      imageTerminal = (await node.buildSeatImage()).terminal_id;
      building = true;
    } catch (e) {
      startError = `${e}`;
    }
  }
  async function signInForContainers() {
    startError = "";
    try {
      signInTerminal = (await node.claudeContainerSignIn()).terminal_id;
    } catch (e) {
      startError = `${e}`;
    }
  }

  function option(rt: AcpRuntimeSnapshot, category: string): string | null {
    const o = rt.config_options.find((c) => c.category === category);
    if (!o) return null;
    const current = typeof o.current_value === "string" ? o.current_value : JSON.stringify(o.current_value);
    return o.values.find((v) => v.id === current)?.name ?? current;
  }

  const GROUPS: { scope: AgentAbility["scope"]; label: string }[] = [
    { scope: "baker", label: "Baker" },
    { scope: "project", label: "Projects" },
    { scope: "task", label: "Tasks" },
  ];
</script>

<!-- Rows (settings.css): the agent on the left, its state and actions on the
     right; what it can do opens underneath. -->
<div class="set-panel" data-testid="agents-panel">
  <p class="set-lead">
    Every agent on every socket, whatever runtime or model backs it. A native runtime's abilities are what it
    reported about itself when it started.
  </p>

  {#if loadError}
    <p class="set-error">{loadError}</p>
    <div class="set-actions">
      <button type="button" class="set-btn" onclick={() => void load()}>Try again</button>
    </div>
  {:else if !view}
    <p class="set-muted">Reading agents…</p>
  {:else}
    <p class="set-lead">
      What they may do without asking is set in <a href="/settings/approvals">Approvals</a>; folders, tools and what
      stays blocked, in <a href="/settings/machine-access">Machine access</a>. Reach, per agent below.
    </p>

    <div class="set-actions">
      <button type="button" class="set-btn" onclick={() => void signInForContainers()} disabled={signInTerminal !== null}>
        Sign in for containers
      </button>
      <span class="set-muted">Claude seats in a container use their own sign-in, never your ~/.claude.</span>
    </div>
    {#if seatImage !== "current"}
      <div class="set-actions">
        <button type="button" class="set-btn" onclick={() => void buildSeatImage()} disabled={building}>
          Build the seat image
        </button>
        <span class="set-muted">Container seats need it ({seatImage}).</span>
      </div>
    {/if}
    {#if buildNote}<p class="set-muted">{buildNote}</p>{/if}
    {#if imageTerminal}
      <div class="sign-in-term">
        <Terminal
          streamId={SYSTEM_ACCOUNTS_STREAM_ID}
          terminalId={imageTerminal}
          name="container build"
          closeLabel="Cancel build"
          onExited={(code) => {
            // A failed build keeps its pane, so the operator can read why.
            if (code === 0) imageTerminal = null;
            buildNote = code === 0 ? "" : "The seat image build failed; its pane says why.";
            building = false;
            void checkSeatImage();
          }}
          onClosed={() => {
            // Cancelled while it ran, or a failed one's pane dismissed: the
            // pane goes, and the image's state is read again.
            buildNote = building ? "Build cancelled." : "";
            imageTerminal = null;
            building = false;
            void checkSeatImage();
          }}
        />
      </div>
    {/if}
    {#if signInTerminal}
      <div class="sign-in-term">
        <Terminal
          streamId={SYSTEM_ACCOUNTS_STREAM_ID}
          terminalId={signInTerminal}
          name="claude-container-login"
          onExited={() => {
            signInTerminal = null;
            void load();
          }}
          onClosed={() => {
            signInTerminal = null;
            void load();
          }}
        />
      </div>
    {/if}

    {#if startError}<p class="set-error">{startError}</p>{/if}

    <svelte:boundary onerror={(e) => reportError("agents-panel", e)}>
    {#each GROUPS as group (group.scope)}
      {@const members = view.agents.filter(
        // Each agent once, whatever the node lists twice.
        (a, i, all) => a.scope === group.scope && all.findIndex((b) => b.agent_id === a.agent_id) === i,
      )}
      {#if members.length}
        <section class="set-section">
          <h2 class="set-section-h">{group.label}</h2>
          {#each members as a (a.agent_id)}
            <div class="set-row" data-testid="agent-card">
              <div class="set-row-text">
                <span class="set-row-title">{a.socket} · {a.seat}</span>
                <span class="set-row-desc">
                  {a.model_id ?? "no model assigned"}
                  {#if a.runtime?.runtime}
                    · {a.runtime.runtime.title ?? a.runtime.runtime.name} <code>{a.runtime.runtime.version}</code>
                  {/if}
                </span>
                {#if a.transport === "native" && a.runtime}
                  {@const rt = a.runtime}
                  <span class="set-row-desc">
                    {#if rt.auth_status}
                      signed in: {rt.auth_status.label ?? rt.auth_status.kind}{rt.auth_status.plan
                        ? ` (${rt.auth_status.plan})`
                        : ""} ·
                    {/if}
                    model {option(rt, "model") ?? rt.effective_model ?? "—"}
                    {#if option(rt, "thought_level")}· effort {option(rt, "thought_level")}{/if}
                    {#if typeof rt.usage?.used === "number" && typeof rt.usage?.size === "number"}· context {rt.usage.used.toLocaleString()} / {rt.usage.size.toLocaleString()}{/if}
                    {#if rt.current_mode}· mode {rt.modes.find((m) => m.id === rt.current_mode)?.name ?? rt.current_mode}{/if}
                  </span>
                {:else if a.transport === "native"}
                  <span class="set-row-desc">Not running — its runtime reports its abilities when it starts.</span>
                {/if}
                {#if a.model_usage}
                  {@const u = a.model_usage}
                  <span class="set-row-desc" data-testid="agent-model-usage">
                    Through the model proxy: {u.calls.toLocaleString()} calls{u.failed ? ` (${u.failed} failed)` : ""} ·
                    {u.tokens_in.toLocaleString()} tokens in · {u.tokens_out.toLocaleString()} out{u.pin_mismatches
                      ? ` · ${u.pin_mismatches} answered by another model`
                      : ""}
                  </span>
                {/if}
              </div>

              <div class="set-row-control">
                <span class="set-chip">
                  {a.transport === "native" ? "native runtime" : a.transport === "api" ? "API model" : "no model"}
                </span>
                {#if a.busy}<span class="set-chip warn">working</span>{/if}
                {#if a.runtime}<span class="set-chip ok">running</span>{/if}
                {#if a.transport !== "none"}
                  <select
                    class="set-select"
                    value={a.reach}
                    onchange={(e) => void setReach(a, e.currentTarget.value as MachineReach)}
                    disabled={reaching !== null}
                    aria-label="What {a.socket} can reach"
                    title="Whole Mac: everything except what Machine access always blocks. Containers only through Bakery, when on."
                    data-testid="agent-reach"
                  >
                    <option value="host_full">Whole Mac</option>
                    <option value="host_room">Workspace + grants</option>
                    <option value="container">Container</option>
                    <option value="locked">Locked</option>
                  </select>
                {/if}
                {#if view && (a.reach === "container" || a.reach === "locked")}
                  <span class="seat-size" title="This seat's share of the Mac. Blank is the default.">
                    <input
                      type="number"
                      min="1"
                      class="set-input seat-num"
                      aria-label="CPUs for {a.socket}"
                      placeholder={String(view.seat_default?.cpus ?? "")}
                      value={a.seat_size?.cpus ?? ""}
                      disabled={reaching !== null}
                      onchange={(e) => void setSize(a, "cpus", e.currentTarget.value)}
                    />
                    CPUs
                    <input
                      type="number"
                      min="1"
                      class="set-input seat-num"
                      aria-label="Memory in GB for {a.socket}"
                      placeholder={String(view.seat_default?.memory_gib ?? "")}
                      value={a.seat_size?.memory_gib ?? ""}
                      disabled={reaching !== null}
                      onchange={(e) => void setSize(a, "memory_gib", e.currentTarget.value)}
                    />
                    GB
                  </span>
                {/if}
                {#if a.transport !== "none" && (a.scope === "baker" || a.scope === "task")}
                  <label
                    class="containers-switch"
                    title="Run and manage containers through Bakery's container tools; a mount reaches only what this agent reaches. Docker's socket and Apple's container CLI stay blocked."
                  >
                    <input
                      type="checkbox"
                      checked={a.containers}
                      disabled={reaching !== null}
                      onchange={(e) => void setContainers(a, e.currentTarget.checked)}
                      data-testid="agent-containers"
                    />
                    Containers
                  </label>
                {/if}
                {#if a.transport === "native" && !a.runtime}
                  <button type="button" class="set-btn sm" onclick={() => void start(a)} disabled={starting !== null}>
                    {starting === a.agent_id ? "Starting…" : "Start runtime"}
                  </button>
                {/if}
                <ConversationActions streamId={a.stream_id} label="Conversation actions" />
              </div>

              {#if (a.transport === "native" && a.runtime && (a.runtime.commands.length || a.runtime.modes.length)) || a.bakery_tools.length || a.sandbox}
                <div class="set-row-more">
                  {#if a.transport === "native" && a.runtime}
                    {@const rt = a.runtime}
                    {#if rt.commands.length}
                      <details class="set-disclosure">
                        <summary>{rt.commands.length} slash commands</summary>
                        <ul class="set-list">
                          {#each rt.commands as c}
                            <li>/{c.name} — {c.description}</li>
                          {/each}
                        </ul>
                      </details>
                    {/if}
                    {#if rt.modes.length}
                      <details class="set-disclosure">
                        <summary>{rt.modes.length} modes</summary>
                        <ul class="set-list">
                          {#each rt.modes as m}
                            <li>{m.name}{m.description ? ` — ${m.description}` : ""}</li>
                          {/each}
                        </ul>
                      </details>
                    {/if}
                  {/if}

                  {#if a.bakery_tools.length}
                    <details class="set-disclosure">
                      <summary>
                        {a.transport === "native" ? "Bakery tools on top of its own" : "Tools (Bakery runs them)"} ·
                        {a.bakery_tools.length}
                      </summary>
                      <ul class="set-list">
                        {#each a.bakery_tools as t}
                          <li>{t.name}{t.read_only ? " · read-only" : ""} — {t.summary}</li>
                        {/each}
                      </ul>
                    </details>
                  {/if}

                  {#if a.sandbox && a.reach === "host_full"}
                    <details class="set-disclosure">
                      <summary>Sandbox — the whole Mac</summary>
                      <p class="set-note sandbox-note">
                        Everything you can reach, except what Machine access always blocks. Docker's socket, Apple
                        container's CLI, the Keychain, and your shell, login and git settings stay out of reach;
                        containers only through Bakery, when Containers is on.
                      </p>
                    </details>
                  {:else if a.sandbox}
                    <details class="set-disclosure">
                      <summary>Sandbox — where it can write</summary>
                      <ul class="set-list">
                        {#each a.sandbox.writes as p}<li>write · {p}</li>{/each}
                        {#each a.sandbox.reads as p}<li>read · {p}</li>{/each}
                        {#each a.sandbox.runs as p}<li>run · {p}</li>{/each}
                      </ul>
                      <p class="set-note sandbox-note">
                        Never writes the project's {a.sandbox.never_writes.join(", ")} folders, and nothing else on
                        this machine.
                      </p>
                    </details>
                  {/if}
                </div>
              {/if}
            </div>
          {/each}
        </section>
      {/if}
    {/each}
    {#snippet failed(error, reset)}
      <p class="set-error">The agents list failed to show: {error instanceof Error ? error.message : String(error)}</p>
      <div class="set-actions">
        <button type="button" class="set-btn" onclick={reset}>Try again</button>
      </div>
    {/snippet}
    </svelte:boundary>
  {/if}
</div>

<style>
  /* Everything else comes from the shared settings rows. */
  .set-disclosure .sandbox-note { margin: 4px 0 2px 16px; }
  .sign-in-term { height: 260px; margin: 8px 0; }
  .containers-switch { display: inline-flex; align-items: center; gap: 4px; font-size: 12px; }
  .seat-size { display: inline-flex; align-items: center; gap: 4px; font-size: 12px; }
  .seat-num { width: 4.5em; }
</style>
