<script lang="ts">
  import "../app.css";
  import { onDestroy, onMount } from "svelte";
  import { afterNavigate, beforeNavigate, goto, pushState, replaceState } from "$app/navigation";
  import { reportClient } from "$lib/clientLog";
  import { page } from "$app/state";
  import SettingsFrame from "$lib/controlplane/SettingsFrame.svelte";
  import SettingsContent, { MOVED } from "$lib/controlplane/SettingsContent.svelte";
  import { onStatus, onEvent, type ConnStatus } from "$lib/events";
  import { connectToNode, node, type ConnectFailure, type NodeInfo } from "$lib/node";
  import { electronShell } from "$lib/desktop";
  import { saveLastSessionHandle } from "$lib/paneState";
  import { NODE_INFO_EVENT } from "$lib/writeMode";
  import { studioStore } from "$lib/studioStore.svelte";
  import { navMode } from "$lib/navMode.svelte";
  import { startAppearanceSync, startCodeAppearance, syncAppearanceWithNode } from "$lib/theme";
  import Sidebar from "$lib/shell/Sidebar.svelte";
  import Resizer from "$lib/shell/Resizer.svelte";
  import TitleBar from "$lib/shell/TitleBar.svelte";
  import CommandPalette, { type PaletteCommand } from "$lib/shell/CommandPalette.svelte";
  import ConnectionScreen from "$lib/shell/ConnectionScreen.svelte";
  import {
    claimCompactOverlay,
    onCompactOverlayClaim,
  } from "$lib/shell/compactOverlay";

  let { children } = $props();

  // Remember last studio handle so the Sessions rail can return without ejecting.
  $effect(() => {
    const p = page.url.pathname;
    if (p.startsWith("/w/") && p.length > 3) {
      saveLastSessionHandle(p.slice(3));
    }
  });

  // Each mode's last location, for the sidebar's Chat / Baker switch and for
  // closing Settings. Here, because the sidebar unmounts while it is hidden.
  $effect(() => {
    navMode.remember(page.url.pathname, page.url.search);
  });

  // ---- Connection + node info -------------------------------------------------
  // The chrome owns the socket so it outlives navigation and every route
  // agrees on connection state without opening its own WebSocket.
  let status = $state<ConnStatus>("closed");
  let info = $state<NodeInfo | null>(null);

  async function refreshInfo() {
    try {
      info = await node.info();
    } catch {
      /* node down — keep last known or null */
    }
  }

  // The socket opening is the only reliable signal that the node is answering
  // HTTP. onMount fires whether or not the node is up: start the client first
  // and that one fetch fails, leaving info stale with no path back — node info
  // has no other refresh trigger, so refill it on every open, not just once.
  // Topology (streams/studio) is the studioStore's own concern: its
  // ensureStarted() subscribes to `onStatus` itself and refills on reconnect,
  // so this handler only needs to redo what is local to the chrome (info).
  // Closing deliberately keeps the last known info: a blanked chrome during a
  // node restart loses the user's place for no gain.
  // ---- Desktop connection -------------------------------------------------
  // Nothing in the app talks to the node until the shell has connected and
  // proven its credential, so a down node or a rejected credential is ONE
  // screen with a Retry — never a scatter of raw 401s from whichever panel
  // happened to fetch first. The chrome's own subscriptions start only
  // after that, which is why they are no longer created at component init.
  let conn = $state<"connecting" | "ready" | "failed">("connecting");
  let connFailure = $state<ConnectFailure | null>(null);
  let stopStatus: (() => void) | null = null;
  let stopEvents: (() => void) | null = null;
  let stopAppearanceNode: (() => void) | null = null;
  let stopCodeAppearance: (() => void) | null = null;

  function startChrome() {
    if (stopStatus) return; // idempotent: a Retry after success changes nothing
    stopStatus = onStatus(async (s) => {
      status = s;
      if (s === "open") await refreshInfo();
    });
    // Owns the one global topology subscription + initial /studio/state
    // fetch (07c-LIVE-STATE) — idempotent, other surfaces call this too.
    studioStore.ensureStarted();
    stopAppearanceNode = syncAppearanceWithNode();
    stopCodeAppearance = startCodeAppearance();
    void refreshInfo();
    stopEvents = onEvent(null, (ev) => {
      // Any surface can change write mode — keep chrome in sync. Topology
      // events are handled by studioStore's own subscription.
      if (ev.family === "policy.write_mode.set") void refreshInfo();
    });
  }

  let startPageShown = false;

  async function connect() {
    conn = "connecting";
    connFailure = null;
    const result = await connectToNode();
    if (result.ok) {
      conn = "ready";
      startChrome();
      // The Electron app opens on the new shell. The old screens stay one
      // ⌘⇧P away ("Open the old screens") until the shell covers them.
      if (!startPageShown && electronShell() && page.url.pathname === "/") void goto("/next", { replaceState: true });
      startPageShown = true;
    } else {
      connFailure = result.failure;
      conn = "failed";
    }
  }

  onDestroy(() => {
    stopStatus?.();
    stopEvents?.();
    stopAppearanceNode?.();
    stopCodeAppearance?.();
  });

  // ---- Stuck-app watchdog -----------------------------------------------------
  // Two failures leave no error behind: a navigation that never finishes, and
  // a page nothing can be clicked on (a closed menu or dialog that left
  // `pointer-events: none` on the body). Both are reported to the node log.
  let pendingNav: ReturnType<typeof setTimeout> | undefined;
  beforeNavigate(({ to }) => {
    clearTimeout(pendingNav);
    const target = to?.url.pathname ?? "(unknown)";
    pendingNav = setTimeout(() => reportClient("navigation", `navigation to ${target} not finished after 5s`), 5000);
  });
  afterNavigate(() => clearTimeout(pendingNav));
  onMount(() => {
    let stuckSince = 0;
    const id = setInterval(() => {
      const blocked =
        document.body.style.pointerEvents === "none" &&
        !document.querySelector('[role="dialog"], [role="menu"], [role="listbox"], [data-state="open"]');
      stuckSince = blocked ? stuckSince || Date.now() : 0;
      if (stuckSince && Date.now() - stuckSince > 3000) {
        reportClient("unclickable", "body pointer-events stayed none with nothing open");
      }
    }, 1000);
    return () => clearInterval(id);
  });

  // ---- Sidebar width ----------------------------------------------------------
  const SIDEBAR_KEY = "bakery.sidebar.w";
  const SIDEBAR_OPEN_KEY = "bakery.sidebar.open";
  const COMPACT_NAV_QUERY = "(max-width: 1050px)";
  const SIDEBAR_MIN = 190;
  const SIDEBAR_MAX = 460;
  const SIDEBAR_DEFAULT = 284;

  function loadSidebarW(): number {
    try {
      const v = Number(localStorage.getItem(SIDEBAR_KEY));
      if (!Number.isFinite(v) || v <= 0) return SIDEBAR_DEFAULT;
      return Math.max(SIDEBAR_MIN, Math.min(SIDEBAR_MAX, v));
    } catch {
      return SIDEBAR_DEFAULT;
    }
  }

  function saveSidebarW(w: number): void {
    try {
      localStorage.setItem(SIDEBAR_KEY, String(Math.round(w)));
    } catch {
      /* storage unavailable */
    }
  }

  let sidebarW = $state(loadSidebarW());

  function loadDesktopSidebarOpen(): boolean {
    try {
      return localStorage.getItem(SIDEBAR_OPEN_KEY) !== "0";
    } catch {
      return true;
    }
  }

  function saveDesktopSidebarOpen(open: boolean): void {
    try {
      localStorage.setItem(SIDEBAR_OPEN_KEY, open ? "1" : "0");
    } catch {
      /* storage unavailable */
    }
  }

  let compactNav = $state(false);
  let desktopSidebarOpen = $state(loadDesktopSidebarOpen());
  let compactDrawerOpen = $state(false);
  const sidebarVisible = $derived(compactNav ? compactDrawerOpen : desktopSidebarOpen);

  function restoreSidebarToggleFocus() {
    requestAnimationFrame(() => {
      const toggle = document.querySelector<HTMLElement>('[data-testid="sidebar-toggle"]');
      toggle?.focus();
    });
  }

  function sidebarFocusable(): HTMLElement[] {
    const drawer = document.getElementById("primary-sidebar");
    if (!drawer) return [];
    return Array.from(
      drawer.querySelectorAll<HTMLElement>(
        'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), [tabindex]:not([tabindex="-1"])',
      ),
    ).filter((el) => !el.hasAttribute("hidden") && el.getAttribute("aria-hidden") !== "true");
  }

  function focusCompactSidebar() {
    requestAnimationFrame(() => sidebarFocusable()[0]?.focus());
  }

  function closeCompactSidebar(restoreFocus = false) {
    if (!compactNav || !compactDrawerOpen) return;
    compactDrawerOpen = false;
    if (restoreFocus) restoreSidebarToggleFocus();
  }

  function toggleSidebar() {
    if (compactNav) {
      if (compactDrawerOpen) {
        closeCompactSidebar(true);
        return;
      }
      claimCompactOverlay("primary-navigation");
      compactDrawerOpen = true;
      focusCompactSidebar();
      return;
    }
    const focusWasInsideSidebar = desktopSidebarOpen &&
      document.getElementById("primary-sidebar")?.contains(document.activeElement);
    // The button moves between the sidebar and the title bar; a keyboard
    // press on it keeps focus on it.
    const focusWasOnToggle = !!document.activeElement?.closest('[data-testid="sidebar-toggle"]');
    desktopSidebarOpen = !desktopSidebarOpen;
    saveDesktopSidebarOpen(desktopSidebarOpen);
    if (focusWasOnToggle || (!desktopSidebarOpen && focusWasInsideSidebar)) restoreSidebarToggleFocus();
  }

  // ---- Command palette --------------------------------------------------------
  let paletteOpen = $state(false);
  let paletteCommandsMode = $state(false);

  /** Settings as a sheet over whatever is open (the new shell included). */
  function openSettings(section: string) {
    if (page.url.pathname.startsWith("/settings")) void goto(`/settings/${section}`);
    else pushState(`/settings/${section}`, { settings: section });
  }

  // The new React shell asks for what only this layout can do: Settings, and
  // (from ⌘K) a conversation opened in place rather than on its old screen.
  const SHELL_OPEN_SETTINGS = "bakery:open-settings";
  const SHELL_OPEN_CONVERSATION = "bakery:open-conversation";
  function jumpInShell(streamId: string, bakerThread: boolean) {
    window.dispatchEvent(new CustomEvent(SHELL_OPEN_CONVERSATION, { detail: { streamId, bakerThread } }));
  }
  onMount(() => {
    const onSettings = (e: Event) => openSettings(String((e as CustomEvent).detail ?? "agents"));
    const onPalette = () => openPalette();
    window.addEventListener(SHELL_OPEN_SETTINGS, onSettings);
    window.addEventListener("bakery:open-palette", onPalette);
    return () => {
      window.removeEventListener(SHELL_OPEN_SETTINGS, onSettings);
      window.removeEventListener("bakery:open-palette", onPalette);
    };
  });
  /** The new React shell (`/next`) draws its own sidebar and dock. */
  const bareShell = $derived(page.url.pathname === "/next");

  // ⌘⇧P. Kept small on purpose — add a command when you start wanting it.
  const paletteCommands: PaletteCommand[] = [
    { id: "new-chat", label: "New thread", run: () => void goto("/chat") },
    { id: "new-project", label: "New project", run: () => void goto("/?open=project") },
    { id: "baker", label: "Open the Baker", run: () => void goto("/baker") },
    { id: "floor", label: "Open the Floor", run: () => void goto("/") },
    { id: "new-layout", label: "Open the new shell", run: () => void goto("/next") },
    { id: "classic-layout", label: "Open the old screens", run: () => void goto("/") },
    {
      id: "settings",
      label: "Open Settings",
      run: () => openSettings("agents"),
    },
    { id: "sidebar", label: "Show or hide the sidebar", hint: "⌘\\", run: () => toggleSidebar() },
    { id: "jump", label: "Jump to a thread", hint: "⌘K", run: () => openPalette() },
  ];
  let paletteReturnFocus: HTMLElement | null = null;
  let paletteFallbackFocus: HTMLElement | null = null;

  // WebKit (the desktop app's engine) does not focus a button when it is
  // clicked, so a modal opened with the mouse finds `document.activeElement`
  // on <body> — which counts as connected and visible but cannot take focus,
  // so closing the modal left focus lost. Remember what was just pressed and
  // use that as the opener instead.
  let lastPressed: { el: HTMLElement; at: number } | null = null;
  function modalOpener(): HTMLElement | null {
    const active = document.activeElement;
    if (active instanceof HTMLElement && active !== document.body && active !== document.documentElement) {
      return active;
    }
    return lastPressed && Date.now() - lastPressed.at < 2000 ? lastPressed.el : null;
  }
  function rememberPress(e: PointerEvent) {
    const el = (e.target as Element | null)?.closest?.<HTMLElement>(
      'button, a[href], [role="button"], input, textarea, select, [tabindex]',
    );
    lastPressed = el ? { el, at: Date.now() } : null;
  }

  function fallbackForPaletteOpener(opener: HTMLElement | null): HTMLElement | null {
    if (!opener) return null;
    if (opener.closest('[data-testid="settings-nav-drawer"]')) {
      return document.querySelector<HTMLElement>('[data-testid="settings-nav-toggle"]');
    }
    if (opener.closest('[data-testid="primary-sidebar"]')) {
      return document.querySelector<HTMLElement>('[data-testid="sidebar-toggle"]');
    }
    return null;
  }

  function canRestorePaletteFocus(target: HTMLElement | null): target is HTMLElement {
    if (!target?.isConnected || target.closest('[aria-hidden="true"], [inert]')) return false;
    if (
      window.matchMedia("(max-width: 900px)").matches &&
      target.closest('[data-testid="settings-nav-drawer"]:not(.open)')
    ) return false;
    const style = getComputedStyle(target);
    return style.display !== "none" && style.visibility !== "hidden" && target.getClientRects().length > 0;
  }

  function openPalette(commands = false) {
    paletteCommandsMode = commands;
    // Keep the element that invoked the modal so closing it returns the
    // operator to the same place. A compact-sidebar trigger is about to be
    // unmounted, so close that drawer without running its own focus restore;
    // closePalette() falls back to the sidebar toggle (in the title bar
    // once the drawer is closed).
    if (paletteOpen) return;
    paletteReturnFocus = modalOpener();
    paletteFallbackFocus = fallbackForPaletteOpener(paletteReturnFocus);
    claimCompactOverlay("command-palette");
    closeCompactSidebar(false);
    paletteOpen = true;
  }

  function closePalette(options: { restoreFocus?: boolean } = {}) {
    if (!paletteOpen) return;
    paletteOpen = false;
    const returnFocus = paletteReturnFocus;
    const fallbackFocus = paletteFallbackFocus;
    paletteReturnFocus = null;
    paletteFallbackFocus = null;
    if (options.restoreFocus === false) return;
    requestAnimationFrame(() => {
      if (canRestorePaletteFocus(returnFocus)) {
        returnFocus.focus();
        return;
      }
      if (canRestorePaletteFocus(fallbackFocus)) {
        fallbackFocus.focus();
        return;
      }
      document.querySelector<HTMLElement>('[data-testid="sidebar-toggle"]')?.focus();
    });
  }

  function handleKeydown(e: KeyboardEvent) {
    // The new shell (/next) has its own search and command bar on these keys.
    if ((e.metaKey || e.ctrlKey) && e.shiftKey && e.key.toLowerCase() === "p" && !bareShell) {
      e.preventDefault();
      openPalette(true);
      return;
    }
    if ((e.metaKey || e.ctrlKey) && e.key === "k" && !bareShell) {
      e.preventDefault();
      openPalette();
      return;
    }
    if ((e.metaKey || e.ctrlKey) && e.key === "\\" && !bareShell) {
      e.preventDefault();
      toggleSidebar();
      return;
    }
    if (e.key === "Escape" && compactDrawerOpen) {
      e.preventDefault();
      closeCompactSidebar(true);
      return;
    }
    if (e.key === "Tab" && compactDrawerOpen) {
      const focusable = sidebarFocusable();
      if (focusable.length === 0) return;
      const first = focusable[0];
      const last = focusable[focusable.length - 1];
      if (e.shiftKey && (document.activeElement === first || !document.getElementById("primary-sidebar")?.contains(document.activeElement))) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && document.activeElement === last) {
        e.preventDefault();
        first.focus();
      }
    }
  }

  // A route selection made inside the compact drawer should reveal the new
  // surface, not leave navigation covering it. Desktop keeps its persistent
  // sidebar exactly as the operator left it.
  let previousRoute = `${page.url.pathname}${page.url.search}`;
  $effect(() => {
    const nextRoute = `${page.url.pathname}${page.url.search}`;
    if (compactNav && nextRoute !== previousRoute) compactDrawerOpen = false;
    previousRoute = nextRoute;
  });

  onMount(() => {
    const stopAppearanceSync = startAppearanceSync();
    // Connect first — every other subscription starts inside startChrome()
    // once the shell actually has a working credential.
    void connect();
    document.addEventListener("keydown", handleKeydown);
    document.addEventListener("pointerdown", rememberPress, true);
    const onNodeInfo = (e: Event) => {
      const d = (e as CustomEvent<NodeInfo>).detail;
      if (d && typeof d === "object") info = d;
    };
    window.addEventListener(NODE_INFO_EVENT, onNodeInfo);

    const compactQuery = window.matchMedia(COMPACT_NAV_QUERY);
    const syncCompactNav = () => {
      const next = compactQuery.matches;
      if (next !== compactNav) compactDrawerOpen = false;
      compactNav = next;
    };
    syncCompactNav();
    compactQuery.addEventListener("change", syncCompactNav);
    const stopOverlayClaims = onCompactOverlayClaim((owner) => {
      if (owner !== "primary-navigation") closeCompactSidebar(false);
      if (owner !== "command-palette") closePalette({ restoreFocus: false });
    });

    // Settings opens as a popup over the page you are on (a shallow route),
    // so that page stays mounted and coming back costs nothing. A reload or
    // a pasted /settings link still lands on the real route.
    const openSettingsInPlace = (event: MouseEvent) => {
      if (event.defaultPrevented || event.button !== 0) return;
      if (event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
      if (page.url.pathname.startsWith("/settings")) return;
      const link =
        event.target instanceof Element ? event.target.closest<HTMLAnchorElement>('a[href^="/settings/"]') : null;
      if (!link || link.target) return;
      const raw = new URL(link.href).pathname.slice("/settings/".length).split("/")[0];
      if (!raw) return;
      event.preventDefault();
      const settings = MOVED[raw] ?? raw;
      if (page.state.settings) replaceState(`/settings/${settings}`, { settings });
      else pushState(`/settings/${settings}`, { settings });
    };
    document.addEventListener("click", openSettingsInPlace, true);

    return () => {
      document.removeEventListener("click", openSettingsInPlace, true);
      stopAppearanceSync();
      document.removeEventListener("keydown", handleKeydown);
      document.removeEventListener("pointerdown", rememberPress, true);
      window.removeEventListener(NODE_INFO_EVENT, onNodeInfo);
      compactQuery.removeEventListener("change", syncCompactNav);
      stopOverlayClaims();
    };
  });
</script>

{#if conn !== "ready"}
  <ConnectionScreen
    phase={conn === "failed" ? "failed" : "connecting"}
    failure={connFailure}
    onRetry={() => { void connect(); }}
  />
{:else}
{#if bareShell}
  <!-- The new React shell draws the whole window itself. -->
  {@render children()}
{:else}
<div class="app-shell">
  {#if compactNav && compactDrawerOpen}
    <button
      class="sidebar-scrim"
      data-testid="sidebar-scrim"
      aria-label="Close navigation"
      onclick={() => closeCompactSidebar(true)}
    ></button>
  {/if}

  {#if sidebarVisible}
    <div
      class="sidebar-col"
      class:sidebar-overlay={compactNav}
      id="primary-sidebar"
      data-testid="primary-sidebar"
      style="width: {compactNav ? `min(${sidebarW}px, calc(100vw - 52px))` : `${sidebarW}px`}"
    >
      <Sidebar
        onAddProject={() => goto("/?open=project")}
        onClose={() => closeCompactSidebar(true)}
        onToggle={toggleSidebar}
        onOpenPalette={() => openPalette()}
        compact={compactNav}
        model={info?.model}
        {info}
      />
    </div>

    {#if !compactNav}
      <Resizer
        axis="x"
        unit="px"
        value={sidebarW}
        min={SIDEBAR_MIN}
        max={SIDEBAR_MAX}
        onchange={(v) => { sidebarW = v; }}
        oncommit={(v) => { sidebarW = v; saveSidebarW(v); }}
      />
    {/if}
  {/if}

  <div class="main-col" data-testid="studio-main">
    <TitleBar
      sidebarOpen={sidebarVisible}
      {status}
      onToggleSidebar={toggleSidebar}
    />
    <div class="view-body">
      {@render children()}
    </div>
  </div>
</div>
{/if}

{#if page.state.settings}
  <SettingsFrame section={page.state.settings}>
    <SettingsContent section={page.state.settings} />
  </SettingsFrame>
{/if}

<CommandPalette
  open={paletteOpen}
  onclose={closePalette}
  commands={paletteCommandsMode ? paletteCommands : undefined}
  onjump={bareShell ? jumpInShell : undefined}
/>
{/if}

<style>
  :global(html, body) {
    height: 100%;
  }

  .app-shell {
    height: 100dvh;
    display: flex;
    flex-direction: row;
    overflow: hidden;
    background: var(--bg-app);
  }

  .sidebar-col {
    flex: 0 0 auto;
    height: 100%;
    overflow: hidden;
    position: relative;
    z-index: 45;
  }

  .sidebar-col.sidebar-overlay {
    position: fixed;
    inset: 0 auto 0 0;
    max-width: calc(100vw - 52px);
    box-shadow: var(--shadow-modal);
    animation: sidebar-in 150ms ease-out;
  }

  .sidebar-scrim {
    position: fixed;
    inset: 0;
    z-index: 44;
    border: 0;
    background: var(--overlay-scrim);
    backdrop-filter: blur(2px);
    cursor: default;
  }

  .main-col {
    flex: 1 1 0;
    min-width: 0;
    height: 100%;
    display: flex;
    flex-direction: column;
    overflow: hidden;
    background: var(--bg-app);
  }

  .view-body {
    flex: 1 1 auto;
    min-height: 0;
    overflow: hidden;
  }

  @keyframes sidebar-in {
    from { transform: translateX(-12px); opacity: 0; }
    to { transform: translateX(0); opacity: 1; }
  }

  @media (prefers-reduced-motion: reduce) {
    .sidebar-col.sidebar-overlay { animation: none; }
  }
</style>
