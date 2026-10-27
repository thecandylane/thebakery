<script module lang="ts">
  let closeGuard: (() => Promise<boolean>) | null = null;
  /** A page with unsaved work can hold the popup open: it is asked first
   *  when ✕, Escape, or a click outside would close Settings. */
  export function guardSettingsClose(guard: (() => Promise<boolean>) | null) {
    closeGuard = guard;
  }
</script>

<script lang="ts">
  import "$lib/styles/settings.css";
  import { onMount, tick, untrack, type Snippet } from "svelte";
  import { goto, replaceState } from "$app/navigation";
  import { page } from "$app/state";
  import { navMode } from "$lib/navMode.svelte";
  import {
    claimCompactOverlay,
    onCompactOverlayClaim,
  } from "$lib/shell/compactOverlay";

  // Settings is a window over the whole app, laid out the way desktop AI
  // harnesses lay out theirs: a searchable nav column on the left, a close
  // button top-right, and one page per concern in a scrolling column. Every
  // /settings/<section> address is unchanged; closing (✕ or Escape) returns
  // to the last place outside Settings (navMode.returnPath).
  //
  // Stacking: above the shell (title bar 30, primary sidebar 45 / its scrim
  // 44), below whatever floats over the app — Radix popovers (60) and the
  // command palette (9000). The compact primary drawer can still be summoned
  // (⌘\) over Settings; the window steps under it while it is open.
  //
  // The page rows share one vocabulary (`$lib/styles/settings.css`).

  type SettingsKey =
    | "agents"
    | "approvals"
    | "machine-access"
    | "memory"
    | "providers"
    | "versions"
    | "appearance"
    | "machines"
    | "developer";

  /** `section` set: Settings is a popup over the page you are on (a shallow
   *  route, so that page stays mounted). Unset: it is the /settings route. */
  let { children, section }: { children: Snippet; section?: string } = $props();
  const overlay = $derived(section !== undefined);
  let compact = $state(false);
  let navOpen = $state(false);
  let query = $state("");
  let windowEl = $state<HTMLElement>();
  let mainEl = $state<HTMLElement>();
  let navToggle = $state<HTMLButtonElement>();
  let navDrawer = $state<HTMLElement>();

  // One page per concern, no two pages editing the same thing.
  const SETTINGS: { key: SettingsKey; label: string; icon: string; href: string; group: string }[] = [
    { key: "agents", label: "Agents", icon: "agents", href: "/settings/agents", group: "Agents" },
    { key: "approvals", label: "Approvals", icon: "write-mode", href: "/settings/approvals", group: "Agents" },
    { key: "machine-access", label: "Machine access", icon: "machine-access", href: "/settings/machine-access", group: "Agents" },
    { key: "providers", label: "Provider accounts", icon: "providers", href: "/settings/providers", group: "Accounts" },
    { key: "memory", label: "Memory & storage", icon: "memory", href: "/settings/memory", group: "Data" },
    { key: "appearance", label: "Appearance", icon: "appearance", href: "/settings/appearance", group: "App" },
    { key: "machines", label: "Machines", icon: "machines", href: "/settings/machines", group: "App" },
    { key: "versions", label: "Versions", icon: "versions", href: "/settings/versions", group: "App" },
    { key: "developer", label: "Developer", icon: "developer", href: "/settings/developer", group: "App" },
  ];
  const GROUPS = [...new Set(SETTINGS.map((item) => item.group))];

  const pathname = $derived(page.url.pathname);
  const activeKey = $derived(
    overlay ? (section ?? "") : pathname.startsWith("/settings/") ? pathname.slice("/settings/".length).split("/")[0] : "",
  );
  const activeItem = $derived(SETTINGS.find((item) => item.key === activeKey));
  const activeLabel = $derived(activeItem?.label ?? "Settings");

  // The search narrows the nav by label; Enter opens the first match.
  const matches = $derived.by(() => {
    const q = query.trim().toLocaleLowerCase();
    return q ? SETTINGS.filter((item) => item.label.toLocaleLowerCase().includes(q)) : SETTINGS;
  });

  async function close() {
    if (overlay) {
      // On the route, AppearancePanel's own navigation guard does this.
      if (closeGuard && !(await closeGuard())) return;
      history.back();
    } else {
      void goto(navMode.returnPath());
    }
  }

  function closeNav(options: { restoreFocus?: boolean } = {}) {
    navOpen = false;
    if (options.restoreFocus) navToggle?.focus();
  }

  async function toggleNav() {
    if (navOpen) {
      closeNav();
      return;
    }
    claimCompactOverlay("settings-navigation");
    navOpen = true;
    await tick();
    const target =
      navDrawer?.querySelector<HTMLElement>('[aria-current="page"]') ??
      navDrawer?.querySelector<HTMLElement>("a[href]") ??
      navDrawer?.querySelector<HTMLElement>("input");
    target?.focus();
  }

  function onSearchKeydown(event: KeyboardEvent) {
    if (event.key === "Enter") {
      const first = matches[0];
      if (!first) return;
      event.preventDefault();
      if (compact) closeNav({ restoreFocus: true });
      if (overlay) replaceState(first.href, { settings: first.key });
      else void goto(first.href, { keepFocus: true });
    } else if (event.key === "Escape" && query) {
      // The first Escape clears the search; the next one closes the window.
      event.preventDefault();
      query = "";
    }
  }

  // Surfaces that float above the window own their own keyboard: the
  // command palette (or any other dialog), Radix popovers, and the compact
  // primary drawer with its scrim.
  const ABOVE =
    '[role="dialog"], [role="alertdialog"], [data-radix-popper-content-wrapper], #primary-sidebar.sidebar-overlay, [data-testid="sidebar-scrim"]';

  function ownedElsewhere(target: EventTarget | null): boolean {
    if (!(target instanceof Element) || !windowEl || windowEl.contains(target)) return false;
    return target.closest(ABOVE) !== null;
  }

  function focusableWithin(container: HTMLElement | undefined) {
    if (!container) return [];
    return Array.from(
      container.querySelectorAll<HTMLElement>(
        'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), summary, [tabindex]:not([tabindex="-1"])',
      ),
    ).filter(
      (element) =>
        element.tabIndex >= 0 &&
        element.getClientRects().length > 0 &&
        getComputedStyle(element).visibility === "visible",
    );
  }

  function trapTab(event: KeyboardEvent, container: HTMLElement | undefined) {
    if (!container) return;
    const focusable = focusableWithin(container);
    if (focusable.length === 0) {
      event.preventDefault();
      return;
    }
    const first = focusable[0];
    const last = focusable[focusable.length - 1];
    const active = document.activeElement;
    const outside = !container.contains(active);
    if (event.shiftKey && (active === first || active === container || outside)) {
      event.preventDefault();
      last.focus();
    } else if (!event.shiftKey && (active === last || outside)) {
      event.preventDefault();
      first.focus();
    }
  }

  function handleKeydown(event: KeyboardEvent) {
    if (event.isComposing || ownedElsewhere(event.target)) return;
    if (event.key === "Escape") {
      if (event.defaultPrevented) return; // e.g. the search just cleared itself
      if (compact && navOpen) {
        event.preventDefault();
        closeNav({ restoreFocus: true });
        return;
      }
      // A page may use Escape for itself (Appearance stops a live preview)
      // and marks it handled, so decide only once every listener has run.
      setTimeout(() => {
        if (!event.defaultPrevented && windowEl?.isConnected) close();
      });
      return;
    }
    if (event.key === "Tab") trapTab(event, compact && navOpen ? navDrawer : windowEl);
  }

  // Focus stays in the window while it is open: a route change resets focus
  // to <body>, and the shell may restore focus to a control this window
  // covers (the title-bar toggle, once its drawer or the palette closes).
  function keepFocusInside(event: FocusEvent) {
    const target = event.target;
    if (!windowEl || !(target instanceof Element)) return;
    if (windowEl.contains(target) || ownedElsewhere(target)) return;
    windowEl.focus({ preventScroll: true });
  }

  onMount(() => {
    const media = window.matchMedia("(max-width: 900px)");
    const syncCompact = () => {
      compact = media.matches;
      if (!compact) navOpen = false;
    };
    syncCompact();
    media.addEventListener("change", syncCompact);
    const stopOverlayClaims = onCompactOverlayClaim((owner) => {
      if (compact && owner !== "settings-navigation") closeNav();
    });
    document.addEventListener("focusin", keepFocusInside);
    // Opening moves focus into the window itself, so nothing shows a focus
    // ring before the operator does anything; Tab starts at the search.
    if (!windowEl?.contains(document.activeElement)) windowEl?.focus({ preventScroll: true });
    return () => {
      media.removeEventListener("change", syncCompact);
      stopOverlayClaims();
      document.removeEventListener("focusin", keepFocusInside);
    };
  });

  // The window persists between sections; each section starts at its top.
  $effect(() => {
    void activeKey;
    untrack(() => mainEl?.scrollTo({ top: 0 }));
  });
</script>

<svelte:window onkeydown={handleKeydown} />

{#snippet icon(kind: string)}
  {#if kind === "providers"}
    <svg viewBox="0 0 14 14" width="16" height="16" fill="none" stroke="currentColor" stroke-width="1.2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
      <rect x="1" y="3" width="12" height="8" rx="1.5" />
      <circle cx="4.5" cy="7" r="1.2" />
      <line x1="7.2" y1="5.5" x2="11" y2="5.5" />
      <line x1="7.2" y1="8.5" x2="10" y2="8.5" />
    </svg>
  {:else if kind === "write-mode"}
    <svg viewBox="0 0 14 14" width="16" height="16" fill="none" stroke="currentColor" stroke-width="1.2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
      <rect x="3" y="6.5" width="8" height="6" rx="1" />
      <path d="M5 6.5V4.5a2 2 0 0 1 4 0v2" />
    </svg>
  {:else if kind === "appearance"}
    <svg viewBox="0 0 14 14" width="16" height="16" aria-hidden="true">
      <circle cx="7" cy="7" r="5.5" fill="none" stroke="currentColor" stroke-width="1.2" />
      <path d="M7 1.5a5.5 5.5 0 0 0 0 11z" fill="currentColor" />
    </svg>
  {:else if kind === "memory"}
    <svg viewBox="0 0 14 14" width="16" height="16" fill="none" stroke="currentColor" stroke-width="1.2" aria-hidden="true">
      <ellipse cx="7" cy="3.2" rx="5.2" ry="1.8" />
      <path d="M1.8 3.2v7.6c0 1 2.3 1.8 5.2 1.8s5.2-.8 5.2-1.8V3.2" />
      <path d="M1.8 7c0 1 2.3 1.8 5.2 1.8S12.2 8 12.2 7" />
    </svg>
  {:else if kind === "agents"}
    <svg viewBox="0 0 14 14" width="16" height="16" fill="none" stroke="currentColor" stroke-width="1.2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
      <rect x="2.5" y="4" width="9" height="7.5" rx="1.5" />
      <path d="M7 1.8V4M5 7.3h.01M9 7.3h.01M5.5 9.6h3" />
    </svg>
  {:else if kind === "machine-access"}
    <svg viewBox="0 0 14 14" width="16" height="16" fill="none" stroke="currentColor" stroke-width="1.2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
      <rect x="1.5" y="2" width="11" height="7.5" rx="1" />
      <path d="M5 12.2h4M7 9.5v2.7" />
    </svg>
  {:else if kind === "machines"}
    <svg viewBox="0 0 14 14" width="16" height="16" fill="none" stroke="currentColor" stroke-width="1.2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
      <rect x="1" y="2" width="8" height="6" rx="1" />
      <path d="M3.5 10.5h3M5 8v2.5" />
      <rect x="10" y="4.5" width="3" height="7" rx=".8" />
    </svg>
  {:else if kind === "versions"}
    <svg viewBox="0 0 14 14" width="16" height="16" fill="none" stroke="currentColor" stroke-width="1.2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
      <path d="M1.8 7.6V2.4c0-.3.3-.6.6-.6h5.2l4.6 4.6-5.8 5.8z" />
      <circle cx="4.6" cy="4.6" r="1" />
    </svg>
  {:else if kind === "developer"}
    <svg viewBox="0 0 14 14" width="16" height="16" fill="none" stroke="currentColor" stroke-width="1.2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
      <path d="M4.5 4 1.8 7l2.7 3M9.5 4l2.7 3-2.7 3M8 2.5 6 11.5" />
    </svg>
  {/if}
{/snippet}

{#if overlay}
  <button
    type="button"
    class="settings-backdrop"
    aria-label="Close settings"
    tabindex={-1}
    onclick={() => void close()}
    data-testid="settings-backdrop"
  ></button>
{/if}

<div
  bind:this={windowEl}
  class="settings-window"
  class:popup={overlay}
  role="dialog"
  aria-modal="true"
  aria-labelledby="settings-window-title"
  tabindex="-1"
  data-testid="settings-window"
>
  <span id="settings-window-title" class="sr-only">Settings</span>

  <div class="compact-nav-bar">
    <button
      bind:this={navToggle}
      type="button"
      class="compact-nav-toggle"
      aria-controls="settings-subnav"
      aria-expanded={navOpen}
      onclick={() => void toggleNav()}
      data-testid="settings-nav-toggle"
    >
      <svg viewBox="0 0 16 16" width="16" height="16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" aria-hidden="true">
        <path d="M2.5 4h11M2.5 8h11M2.5 12h11" />
      </svg>
      <span>{activeLabel}</span>
    </button>
  </div>

  <button
    type="button"
    class="nav-scrim"
    class:open={navOpen}
    aria-label="Close settings navigation"
    aria-hidden={!navOpen}
    tabindex={-1}
    onclick={() => closeNav({ restoreFocus: true })}
    data-testid="settings-nav-scrim"
  ></button>

  <!-- Switching sections keeps focus where it is (the nav persists). -->
  <nav
    bind:this={navDrawer}
    id="settings-subnav"
    class="settings-nav"
    class:open={navOpen}
    aria-label="Settings"
    data-testid="settings-nav-drawer"
    data-sveltekit-keepfocus
  >
    <div class="nav-search">
      <svg viewBox="0 0 16 16" width="14" height="14" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" aria-hidden="true">
        <circle cx="7" cy="7" r="4.25" />
        <path d="m10.25 10.25 3 3" />
      </svg>
      <input
        bind:value={query}
        type="search"
        placeholder="Search"
        aria-label="Search settings"
        autocomplete="off"
        spellcheck="false"
        onkeydown={onSearchKeydown}
      />
    </div>

    {#each GROUPS as group (group)}
      {@const items = matches.filter((item) => item.group === group)}
      {#if items.length}
        <div class="nav-group">
          <div class="nav-hd">{group}</div>
          {#each items as item (item.key)}
            <a
              class="nav-item"
              class:active={activeKey === item.key}
              href={item.href}
              aria-current={activeKey === item.key ? "page" : undefined}
              onclick={() => closeNav({ restoreFocus: compact })}
            >
              <span class="nav-icon">{@render icon(item.icon)}</span>
              <span class="nav-label">{item.label}</span>
            </a>
          {/each}
        </div>
      {/if}
    {/each}
    {#if matches.length === 0}
      <p class="nav-empty">No settings match “{query.trim()}”.</p>
    {/if}
  </nav>

  <div bind:this={mainEl} class="settings-main">
    <div class="settings-page">
      {#if activeItem}
        <h1 class="settings-title">{activeItem.label}</h1>
      {/if}
      {@render children()}
    </div>
  </div>

  <button
    type="button"
    class="settings-close"
    aria-label="Close settings"
    title="Close (Esc)"
    onclick={() => void close()}
    data-testid="settings-close"
  >
    <svg viewBox="0 0 16 16" width="16" height="16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" aria-hidden="true">
      <path d="M4 4l8 8M12 4l-8 8" />
    </svg>
  </button>
</div>

<style>
  .settings-window {
    position: fixed;
    inset: 0;
    z-index: 50;
    display: flex;
    flex-direction: row;
    overflow: hidden;
    background: var(--bg-app);
    color: var(--fg);
    outline: none;
    animation: settings-in 140ms ease-out;
  }
  /* The compact primary drawer (and its scrim) cover the window while open. */
  :global(body:has(#primary-sidebar.sidebar-overlay)) .settings-window {
    z-index: 43;
  }
  :global(body:has(#primary-sidebar.sidebar-overlay)) .settings-backdrop {
    z-index: 42;
  }

  /* A popup over the page you are on, the way desktop AI apps show their
     settings: that page stays mounted behind a scrim. Narrow windows keep
     the full-screen layout (see the compact rules below). */
  .settings-backdrop {
    position: fixed;
    inset: 0;
    z-index: 49;
    padding: 0;
    border: 0;
    background: var(--overlay-scrim);
    cursor: default;
    animation: settings-scrim-in 140ms ease-out;
  }
  .settings-window.popup {
    margin: auto;
    width: min(1120px, calc(100vw - 64px));
    height: min(820px, calc(100vh - 64px));
    border: 1px solid var(--line-soft);
    border-radius: var(--r-modal);
    box-shadow: 0 24px 80px rgb(0 0 0 / 0.45);
  }

  .sr-only {
    position: absolute;
    width: 1px;
    height: 1px;
    margin: -1px;
    padding: 0;
    overflow: hidden;
    clip: rect(0 0 0 0);
    white-space: nowrap;
    border: 0;
  }

  .compact-nav-bar,
  .nav-scrim {
    display: none;
  }

  /* ---- nav column -------------------------------------------------------- */

  .settings-nav {
    flex: 0 0 240px;
    width: 240px;
    height: 100%;
    overflow-y: auto;
    padding: 14px 10px 24px;
    box-sizing: border-box;
  }

  .nav-search {
    position: relative;
    display: flex;
    align-items: center;
    margin-bottom: 18px;
  }
  .nav-search svg {
    position: absolute;
    left: 10px;
    color: var(--fg-faint);
    pointer-events: none;
  }
  .nav-search input {
    width: 100%;
    height: 32px;
    padding: 0 10px 0 30px;
    border: 1px solid var(--line-soft);
    border-radius: var(--r-lg);
    background: var(--bg-panel);
    color: var(--fg);
    font: 400 13px/1 var(--font-ui);
    appearance: none;
  }
  .nav-search input::-webkit-search-cancel-button,
  .nav-search input::-webkit-search-decoration {
    appearance: none;
  }
  .nav-search input::placeholder { color: var(--fg-faint); }
  .nav-search input:focus-visible {
    outline: none;
    border-color: var(--accent);
    box-shadow: 0 0 0 3px var(--accent-bg);
  }

  .nav-group + .nav-group { margin-top: 18px; }
  .nav-hd {
    padding: 0 10px 6px;
    font: 500 12px/1.3 var(--font-ui);
    color: var(--fg-faint);
  }

  .nav-item {
    display: flex;
    align-items: center;
    gap: 10px;
    min-height: 32px;
    padding: 0 10px;
    border-radius: var(--r-lg);
    font: 400 13px/1.3 var(--font-ui);
    color: var(--fg-2);
    text-decoration: none;
  }
  .nav-item + .nav-item { margin-top: 1px; }
  .nav-item:hover {
    background: color-mix(in srgb, var(--bg-chip) 55%, transparent);
    color: var(--fg);
    text-decoration: none;
  }
  .nav-item.active {
    background: var(--bg-chip);
    color: var(--fg);
  }
  .nav-item:focus-visible { outline: 2px solid var(--accent); outline-offset: -2px; }

  .nav-icon {
    flex: 0 0 auto;
    width: 16px;
    height: 16px;
    display: grid;
    place-items: center;
    color: var(--fg-dim);
  }
  .nav-item.active .nav-icon { color: var(--fg); }
  .nav-label {
    flex: 1 1 auto;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .nav-empty {
    margin: 0 10px;
    font: 400 12px/1.5 var(--font-ui);
    color: var(--fg-faint);
    overflow-wrap: anywhere;
  }

  /* ---- content ------------------------------------------------------------ */

  .settings-main {
    flex: 1 1 auto;
    min-width: 0;
    height: 100%;
    overflow-x: hidden;
    overflow-y: auto;
  }

  /* The content column: left-aligned, 760px at most. Panels size themselves
     against it (`@container settings`), not against the window. */
  .settings-page {
    container: settings / inline-size;
    box-sizing: content-box;
    max-width: 760px;
    padding: 44px 48px 80px;
  }

  .settings-title {
    margin: 0 0 28px;
    font: var(--set-font-title);
    color: var(--fg);
    overflow-wrap: anywhere;
  }

  .settings-close {
    position: absolute;
    top: 12px;
    right: 14px;
    z-index: 2;
    width: 32px;
    height: 32px;
    display: grid;
    place-items: center;
    padding: 0;
    border: 0;
    border-radius: var(--r-lg);
    background: transparent;
    color: var(--fg-dim);
    cursor: pointer;
  }
  .settings-close:hover { background: var(--bg-chip); color: var(--fg); }
  .settings-close:focus-visible { outline: 2px solid var(--accent); outline-offset: 1px; }

  @keyframes settings-in {
    from { opacity: 0; transform: scale(0.992); }
    to { opacity: 1; transform: none; }
  }
  @keyframes settings-scrim-in {
    from { opacity: 0; }
    to { opacity: 1; }
  }

  /* ---- compact: the nav becomes a drawer under a slim bar ---------------- */

  @media (max-width: 900px) {
    .settings-window {
      flex-direction: column;
    }
    .settings-window.popup {
      width: 100%;
      height: 100%;
      border: 0;
      border-radius: 0;
      box-shadow: none;
    }
    .settings-backdrop {
      display: none;
    }

    .compact-nav-bar {
      flex: 0 0 48px;
      display: flex;
      align-items: center;
      min-width: 0;
      padding: 0 56px 0 10px;
      border-bottom: 1px solid var(--line-soft);
    }

    .compact-nav-toggle {
      min-width: 0;
      height: 32px;
      display: inline-flex;
      align-items: center;
      gap: 8px;
      padding: 0 10px;
      border: 1px solid var(--line-soft);
      border-radius: var(--r-lg);
      background: transparent;
      color: var(--fg-2);
      font: 500 13px/1 var(--font-ui);
      cursor: pointer;
    }
    .compact-nav-toggle:hover,
    .compact-nav-toggle[aria-expanded="true"] {
      border-color: var(--line);
      background: var(--bg-chip);
      color: var(--fg);
    }
    .compact-nav-toggle:focus-visible { outline: 2px solid var(--accent); outline-offset: 1px; }
    .compact-nav-toggle svg { flex: 0 0 auto; }
    .compact-nav-toggle span {
      overflow: hidden;
      text-overflow: ellipsis;
      white-space: nowrap;
    }

    .settings-close {
      top: 8px;
      right: 10px;
    }

    .nav-scrim {
      position: absolute;
      inset: 48px 0 0;
      z-index: 20;
      display: block;
      border: 0;
      padding: 0;
      background: var(--overlay-scrim);
      opacity: 0;
      pointer-events: none;
      transition: opacity 140ms ease;
    }
    .nav-scrim.open {
      opacity: 1;
      pointer-events: auto;
    }

    .settings-nav {
      position: absolute;
      z-index: 22;
      top: 48px;
      bottom: 0;
      left: 0;
      width: min(280px, calc(100% - 44px));
      height: auto;
      background: var(--bg-chrome);
      border-right: 1px solid var(--line);
      box-shadow: var(--shadow-menu);
      transform: translateX(-102%);
      visibility: hidden;
      transition:
        transform 160ms ease,
        visibility 0s linear 160ms;
    }
    .settings-nav.open {
      transform: translateX(0);
      visibility: visible;
      transition-delay: 0s;
    }

    .settings-main {
      flex: 1 1 auto;
      height: auto;
      min-height: 0;
    }

    .settings-page {
      padding: 24px 20px 64px;
    }
  }

  @media (max-width: 480px) {
    .settings-page {
      padding: 20px 16px 56px;
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .settings-window,
    .settings-backdrop { animation: none; }
    .nav-scrim,
    .settings-nav {
      transition: none;
    }
  }
</style>
