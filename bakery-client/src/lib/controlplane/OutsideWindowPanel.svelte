<script lang="ts">
  import { onMount } from "svelte";
  import { outsideWindow, outsideWindowCreate, outsideWindowRevoke, type OutsideWindow } from "$lib/api.generated";
  import { ago } from "$lib/conversations";

  // Settings → Developer: the outside window, the operator's read-only view
  // into the Bakery for a session outside it. Its token shows here once,
  // until Done; the node keeps only its hash.

  let win = $state<OutsideWindow | null>(null);
  let token = $state("");
  let revoking = $state(false);
  let busy = $state(false);
  let copied = $state("");
  let error = $state("");

  async function refresh() {
    try {
      win = await outsideWindow();
    } catch (e) {
      error = `${e}`;
    }
  }

  onMount(() => {
    void refresh();
    // Last used stays live while the page is open.
    const every = setInterval(() => void refresh(), 15_000);
    return () => clearInterval(every);
  });

  async function act(step: () => Promise<void>) {
    busy = true;
    error = "";
    try {
      await step();
    } catch (e) {
      error = `${e}`;
    } finally {
      busy = false;
    }
  }

  const create = () =>
    act(async () => {
      const made = await outsideWindowCreate();
      token = made.token;
      win = made.window;
    });

  const revoke = () =>
    act(async () => {
      win = await outsideWindowRevoke();
      revoking = false;
    });

  const claudeLine = (url: string) =>
    `claude mcp add --transport http --scope user bakery-window ${url} --header "Authorization: Bearer ${token}"`;

  function copy(what: string, text: string) {
    void navigator.clipboard?.writeText(text).then(() => {
      copied = what;
      setTimeout(() => {
        if (copied === what) copied = "";
      }, 1500);
    });
  }

  const when = (ts: string) =>
    new Date(ts).toLocaleString(undefined, { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });
</script>

{#snippet copyButton(what: string, text: string)}
  <button type="button" class="set-btn sm" onclick={() => copy(what, text)} data-testid="outside-window-copy-{what}">
    <svg viewBox="0 0 24 24" width="13" height="13" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
      <rect x="9" y="9" width="12" height="12" rx="2" /><path d="M5 15V5a2 2 0 0 1 2-2h10" />
    </svg>
    {copied === what ? "Copied" : "Copy"}
  </button>
{/snippet}

<section class="set-section" data-testid="outside-window">
  <h2 class="set-section-h">Outside window</h2>
  {#if error}<p class="set-error">{error}</p>{/if}
  {#if win}
    {@const live = !!win.created_at && !token}
    <ul class="set-rows">
      <li class="set-row">
        <span class="set-row-lead">
          <span class="glyph" class:fresh={!!token} class:live={live && !revoking} class:gone={revoking} aria-hidden="true">
            <svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
              <circle cx="7.5" cy="15.5" r="4.5" /><path d="m10.7 12.3 9.8-9.8M17 6l3 3M14.5 8.5l2.5 2.5" />
            </svg>
            {#if live && !revoking && win.last_used}<span class="dot"></span>{/if}
          </span>
          <span class="set-row-text">
            <span class="set-row-title title">
              Read-only token
              {#if token}<span class="set-chip warn">Shown once</span>{/if}
            </span>
            <span class="set-row-desc">
              {#if win.created_at}
                Created {when(win.created_at)}{#if live && win.last_used}&nbsp;·&nbsp;Last used {ago(win.last_used)}{/if}
              {:else}
                None
              {/if}
            </span>
          </span>
        </span>
        <span class="set-row-control">
          {#if token}
            <button type="button" class="set-btn" onclick={() => (token = "")}>Done</button>
          {:else if !win.created_at}
            <button type="button" class="set-btn primary" disabled={busy} onclick={create} data-testid="outside-window-create">
              Create token
            </button>
          {:else if revoking}
            <button type="button" class="set-btn" onclick={() => (revoking = false)}>Cancel</button>
            <button type="button" class="set-btn destructive" disabled={busy} onclick={revoke} data-testid="outside-window-revoke">
              Revoke token
            </button>
          {:else}
            <button type="button" class="set-btn danger" onclick={() => (revoking = true)}>Revoke</button>
          {/if}
        </span>
        {#if token}
          <div class="set-row-more">
            <div class="reveal">
              <div class="field">
                <span class="label">Token</span>
                <code class="box">{token}</code>
                {@render copyButton("token", token)}
              </div>
              <div class="field">
                <span class="label">Claude Code</span>
                <code class="box"><span class="k">claude mcp add</span> --transport http --scope user bakery-window {win.url} --header <span class="s">"Authorization: Bearer {token}"</span></code>
                {@render copyButton("line", claudeLine(win.url))}
              </div>
            </div>
          </div>
        {/if}
      </li>
      {#if live}
        <li class="set-row">
          <span class="set-row-text indent"><span class="set-row-title">Address</span></span>
          <span class="set-row-control">
            <span class="set-value">{win.url}</span>
            {@render copyButton("url", win.url)}
          </span>
        </li>
        <li class="set-row top">
          <span class="set-row-text indent"><span class="set-row-title">Tools</span></span>
          <span class="set-row-control tools">
            {#each win.tools as name (name)}<span class="tool">{name}</span>{/each}
          </span>
        </li>
      {/if}
    </ul>
  {/if}
</section>

<style>
  .title { display: flex; align-items: center; gap: 8px; }
  .indent { padding-left: 44px; }

  .glyph {
    position: relative;
    flex: 0 0 32px;
    height: 32px;
    display: grid;
    place-items: center;
    border: 1px solid var(--line);
    border-radius: var(--r-lg);
    background: linear-gradient(180deg, var(--bg-raised), var(--bg-panel));
    color: var(--fg-dim);
  }
  .glyph.fresh { border-color: var(--accent-border); color: var(--accent-fg); background: linear-gradient(180deg, var(--accent-bg-2), var(--bg-panel)); }
  .glyph.live { border-color: var(--ok-border); color: var(--ok); background: linear-gradient(180deg, var(--ok-bg), var(--bg-panel)); }
  .glyph.gone { border-color: var(--bad-border); color: var(--bad); background: linear-gradient(180deg, var(--bad-bg), var(--bg-panel)); }
  .dot {
    position: absolute;
    top: -3px;
    right: -3px;
    width: 9px;
    height: 9px;
    border: 2px solid var(--bg-app);
    border-radius: 50%;
    background: var(--ok);
    box-shadow: var(--glow-live);
  }

  .reveal {
    margin: 4px 0 0 44px;
    padding: 14px;
    display: flex;
    flex-direction: column;
    gap: 10px;
    border: 1px solid var(--accent-border);
    border-radius: var(--r-xl);
    background: linear-gradient(160deg, var(--accent-bg-2), var(--accent-sel) 55%, var(--bg-panel));
  }
  .field {
    display: grid;
    grid-template-columns: 96px minmax(0, 1fr) auto;
    align-items: center;
    gap: 10px;
  }
  .label { font: var(--set-font-desc); color: var(--fg-dim); }
  .box {
    min-width: 0;
    padding: 8px 10px;
    border: 1px solid var(--line);
    border-radius: var(--r-lg);
    background: var(--bg-app);
    font: var(--set-font-mono);
    color: var(--fg);
    white-space: pre-wrap;
    word-break: break-all;
    user-select: all;
  }
  .box .k { color: var(--syntax-keyword); }
  .box .s { color: var(--syntax-string); }

  .tools { max-width: 540px; gap: 6px; }
  .tool {
    display: inline-flex;
    align-items: center;
    height: 22px;
    padding: 0 8px;
    border: 1px solid var(--accent-border);
    border-radius: var(--r-sm);
    background: var(--accent-bg);
    color: var(--accent-fg);
    font: 400 11px/1 var(--font-mono);
  }
</style>
