<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import { page } from "$app/state";
  import { goto } from "$app/navigation";
  import { ask } from "$lib/ask";
  import { askText } from "$lib/askText";
  import { node, type StreamRef, type ProjectGroup, type NodeInfo } from "$lib/node";
  import { studioStore } from "$lib/studioStore.svelte";
  import { warmHistory } from "$lib/workbench.svelte";
  import { taskStore } from "$lib/taskStore.svelte";
  import { fromHandle, toHandle } from "$lib/handle";
  import { reportClient } from "$lib/clientLog";
  import { bakerPath } from "$lib/bakerThreads";
  import {
    ago,
    allConversations,
    bakerChats,
    chatPath,
    chats,
    codePath,
    openPath,
    statusLabel,
    type Conversation,
  } from "$lib/conversations";
  import { navMode, modeForPath, type NavMode } from "$lib/navMode.svelte";
  import type { ConversationStatus } from "$lib/node";
  import { sidebarFilter, type StatusFilter } from "$lib/shell/sidebarFilter.svelte";
  import ConversationActions from "$lib/shell/ConversationActions.svelte";
  import { loadTreeExpansion, saveTreeExpansion } from "$lib/paneState";
  import { onStatus, type ConnStatus } from "$lib/events";
  import { isLegacyBakeryBranch, socketLabel, socketTag } from "$lib/sessionGroups";
  import SocketRow from "$lib/shell/SocketRow.svelte";
  import TaskRow from "$lib/shell/TaskRow.svelte";
  import { BUILTIN_THEMES, loadAppearance, themeLabel, applyTheme, THEME_CHANGED_EVENT, type Theme } from "$lib/theme";

  // 07d-A2: the one permanent navigator, and the app's action bar: the
  // Chat / Baker switch, search, new work, what needs you, every list, and
  // the button that hides it. The title bar carries the tools of what is
  // open. Retired lives on the Floor, not as an inline tree swap (see
  // routes/+page.svelte).

  let {
    onAddProject,
    onClose,
    onToggle,
    onOpenPalette,
    compact = false,
    model,
    info = null,
  }: {
    onAddProject?: () => void;
    onClose?: () => void;
    /** Hides the sidebar (closes the drawer when compact). */
    onToggle?: () => void;
    onOpenPalette?: () => void;
    compact?: boolean;
    /** Fallback model name for the orchestrator seat chip. */
    model?: string;
    info?: NodeInfo | null;
  } = $props();

  let expansion = $state(loadTreeExpansion());
  let lineageVisible = $state(false);
  let actionError = $state("");
  let appearance = $state(loadAppearance());

  onMount(() => {
    const sync = () => { appearance = loadAppearance(); };
    sync();
    window.addEventListener(THEME_CHANGED_EVENT, sync);
    return () => window.removeEventListener(THEME_CHANGED_EVENT, sync);
  });

  // Opening a conversation should never wait. Its history starts loading
  // the moment the pointer or keyboard focus reaches its row, and the most
  // recent conversations stay warm in the background.
  let asideEl = $state<HTMLElement>();
  let lastWarmed = "";
  function warmRow(event: Event) {
    const row = event.target instanceof Element ? event.target.closest("[data-stream-id]") : null;
    const id = row?.getAttribute("data-stream-id");
    if (!id || id === lastWarmed) return;
    lastWarmed = id;
    warmHistory(id);
  }
  onMount(() => {
    const el = asideEl;
    if (!el) return;
    el.addEventListener("pointerover", warmRow);
    el.addEventListener("focusin", warmRow);
    return () => {
      el.removeEventListener("pointerover", warmRow);
      el.removeEventListener("focusin", warmRow);
    };
  });
  $effect(() => {
    const recent = allConversations(studioStore.data)
      .filter((c) => !c.archived)
      .sort((a, b) => (a.lastTs < b.lastTs ? 1 : a.lastTs > b.lastTs ? -1 : 0))
      .slice(0, 8);
    for (const c of recent) warmHistory(c.streamId);
  });

  function cycleTheme() {
    const options: Theme[] = ["auto", ...BUILTIN_THEMES, ...appearance.schemes.map((s) => s.id)];
    const next = options[(options.indexOf(appearance.selected) + 1) % options.length];
    try { applyTheme(next); actionError = ""; }
    catch (error) { actionError = String(error); }
  }

  let status = $state<ConnStatus>("closed");
  const stopStatus = onStatus((s) => {
    status = s;
  });
  onDestroy(stopStatus);

  // 07d-B Phase 4 item 3: task rows join `SocketInfo` (topology, already
  // live via `studioStore`) with `TaskView` (contract/status, from
  // `taskStore`'s own `GET /task/list` cache) — never a second topology
  // fetch. `ensureStarted` is idempotent; every project group with a task
  // socket gets its list loaded/kept fresh below.
  taskStore.ensureStarted();

  const statusWord = $derived(
    status === "open" ? "LIVE" : status === "connecting" ? "CONN" : "DOWN",
  );

  // Canonical socket inventory (07c-LIVE-STATE) — the ONLY topology source
  // for this tree.
  const sockets = $derived(studioStore.sockets);

  let autoExpanded = false;
  $effect(() => {
    const groups = studioStore.data?.projects ?? [];
    if (autoExpanded || expansion.touched || groups.length === 0) return;
    autoExpanded = true;
    expansion = { projects: new Set(groups.map((g) => g.project_id)), touched: false };
  });

  // 07d-A3: Projects and Ungrouped straight off `/studio/state` — grouping
  // by real Project membership (`socket.project.set` via the server's own
  // `projects`/`ungrouped_sockets` buckets), never by repo_root or any
  // client-side name/branch heuristic. Factory-kind sockets never appear in
  // either bucket (the server excludes them) — they surface as operational
  // work on the Factory Floor instead (07d-A2 decision), not in this nav.
  // The live tree never shows retired sockets; a Project itself stays
  // listed even with zero active sockets — it is a durable identity, not a
  // derived grouping of currently-live work. Pinned projects sort first.
  const activeProjectGroups = $derived.by(() => {
    const groups = (studioStore.data?.projects ?? []).map((g) => ({
      ...g,
      sockets: g.sockets.filter((s) => !s.discarded && !s.archived),
    }));
    return [...groups].sort((a, b) => Number(b.pinned) - Number(a.pinned));
  });
  const activeUngrouped = $derived(
    (studioStore.data?.ungrouped_sockets ?? []).filter((s) => !s.discarded && !s.archived && s.repo_root !== ""),
  );

  // Keep every visible project's task list loaded — cheap no-op once fetched
  // (taskStore itself refreshes on `task.*` events; this only seeds the
  // first fetch as new task-bearing groups appear).
  $effect(() => {
    for (const g of activeProjectGroups) {
      if (g.sockets.some((s) => s.state === "task")) taskStore.ensureProject(g.project_id);
    }
  });
  const projectCount = $derived(activeProjectGroups.length);
  const ungroupedCount = $derived(activeUngrouped.length);
  const retiredCount = $derived(sockets.filter((s) => s.discarded).length);

  let menuOpenFor = $state<string | null>(null);

  async function renameProject(p: ProjectGroup) {
    menuOpenFor = null;
    const name = await askText("Rename project", p.name);
    if (name == null) return;
    const trimmed = name.trim();
    if (!trimmed || trimmed === p.name) return;
    actionError = "";
    try {
      await node.projectRename(p.project_id, trimmed);
      studioStore.refresh();
    } catch (e) {
      actionError = `${e}`;
    }
  }

  async function togglePin(p: ProjectGroup) {
    menuOpenFor = null;
    actionError = "";
    try {
      await node.projectPin(p.project_id, !p.pinned);
      studioStore.refresh();
    } catch (e) {
      actionError = `${e}`;
    }
  }

  async function removeProject(p: ProjectGroup) {
    menuOpenFor = null;
    const ok = await ask(
      `Remove "${p.name}" from Bakery?\n\nThis hides it here; its files, roots, and session history are kept.`,
    );
    if (!ok) return;
    actionError = "";
    try {
      await node.projectRetire(p.project_id);
      studioStore.refresh();
    } catch (e) {
      actionError = `${e}`;
    }
  }

  function onWindowPointerDownForMenu(e: PointerEvent) {
    const t = e.target as HTMLElement;
    if (!t.closest(".proj-menu")) menuOpenFor = null;
  }

  $effect(() => {
    if (menuOpenFor == null) return;
    window.addEventListener("pointerdown", onWindowPointerDownForMenu);
    return () => window.removeEventListener("pointerdown", onWindowPointerDownForMenu);
  });

  const currentHandle = $derived(
    page.url.pathname.startsWith("/w/") ? page.url.pathname.slice(3) : null,
  );
  // Two modes, derived from the route so the switch and the list always
  // match the actual location (Settings keeps Chat's):
  //   - Chat:  everything that is not the Baker — new chat, the Floor, the
  //            project tree (each socket opening its studio), and every chat;
  //   - Baker: the Baker's own conversations, never mixed into Chat.
  // Switching is navigation only, never permissions, identity, or runtime.
  // Each mode returns to where you left it; the mode you are in, to its home.
  const mode = $derived<NavMode>(modeForPath(page.url.pathname) ?? "chat");
  /** Why a remembered place cannot be returned to now, or "" when it can.
   *  Only places that can be checked against what exists are checked. */
  function placeProblem(path: string): string {
    const m = path.match(/^\/(chat|w)\/([^/?#]+)/);
    if (!m || !studioStore.data) return "";
    let sid: string;
    try {
      sid = fromHandle(m[2]);
    } catch {
      return "its link is not valid";
    }
    if (m[1] === "w") return studioStore.findSocket(sid) ? "" : "that studio no longer exists";
    const conv = allConversations(studioStore.data).find((c) => c.streamId === sid);
    if (!conv) return "that conversation no longer exists";
    return conv.baker ? "that is a Baker conversation" : "";
  }

  /** A tab always works. It returns to where you left that mode only when
   *  that place is still there and the tab really lands in the mode; any
   *  other outcome — a place that is gone, a hand-off to the other mode, an
   *  error page — forgets the place, goes to the mode's home, and says why in
   *  the node log, so a saved place can never trap the tab. */
  async function switchMode(target: NavMode) {
    const home = navMode.home(target);
    if (target === mode) return void goto(home);
    const path = navMode.pathFor(target);
    const bare = path.split("?")[0];
    const problem = placeProblem(path);
    if (problem) {
      navMode.forget(bare);
      reportClient("tab-fallback", `${target} tab: ${path} — ${problem}`);
      return void goto(home);
    }
    try {
      await goto(path);
    } catch (e) {
      reportClient("tab-fallback", `${target} tab: ${path} — navigation threw: ${e}`);
    }
    // A hand-off redirect lands a moment after the navigation resolves.
    await new Promise((resolve) => setTimeout(resolve, 400));
    if (modeForPath(page.url.pathname) !== target || page.error) {
      navMode.forget(bare);
      reportClient(
        "tab-fallback",
        `${target} tab: ${path} → ${page.url.pathname} ${page.error ? `(error page: ${page.error.message})` : "(other mode)"}`,
      );
      await goto(home);
    }
  }
  const TABS: { mode: NavMode; label: string; title: string; icon: string }[] = [
    {
      mode: "chat",
      label: "Chat",
      title: "Chats, project studios, and the Floor",
      icon: "M3 5.25A2.25 2.25 0 0 1 5.25 3h7.5A2.25 2.25 0 0 1 15 5.25v4.5A2.25 2.25 0 0 1 12.75 12H7l-3 3v-3H5.25A2.25 2.25 0 0 1 3 9.75Z",
    },
    {
      mode: "baker",
      label: "Baker",
      title: "The Bakery's own agent and its threads",
      icon: "m6.5 5.5-4 3.5 4 3.5M11.5 5.5l4 3.5-4 3.5",
    },
  ];

  // Chat mode's list: every conversation but tasks, each with the same
  // actions menu it has everywhere else.
  const chatList = $derived(chats(studioStore.data));
  const currentChat = $derived.by(() => {
    const p = page.url.pathname;
    if (!p.startsWith("/chat/")) return null;
    try {
      return fromHandle(p.slice("/chat/".length));
    } catch {
      return null;
    }
  });

  // Baker mode's list: the Baker's conversations, straight off the canonical
  // topology (`SocketInfo.baker_thread` on `/studio/state`) — never a second
  // cache, and never mixed into Chat. Statuses are live, so parallel threads
  // show when they are working or need you without opening each one.
  const bakerConvs = $derived(bakerChats(studioStore.data));

  /** A node older than Baker threads sends factory sockets with no
   *  `baker_thread` key at all (a current node sends `null`); the main
   *  conversation still gets a row then, plus an honest restart hint. */
  const nodeLacksThreads = $derived(
    (studioStore.data?.factory_sockets.length ?? 0) > 0 &&
      (studioStore.data?.factory_sockets ?? []).every((s) => !("baker_thread" in s)),
  );
  let creatingThread = $state(false);

  /** The conversation the Baker page is showing: `/baker` is the main one,
   *  `/baker/<handle>` a thread. */
  const currentBakerStream = $derived.by(() => {
    const p = page.url.pathname;
    if (!p.startsWith("/baker")) return null;
    const rest = p.slice("/baker".length).replace(/^\/+/, "");
    if (!rest) return bakerConvs.active.find((c) => c.main)?.streamId ?? null;
    try {
      return fromHandle(rest);
    } catch {
      return null;
    }
  });

  async function newThread() {
    if (creatingThread) return;
    creatingThread = true;
    actionError = "";
    try {
      const res = await node.bakerThreadCreate();
      studioStore.refresh();
      onClose?.();
      await goto(bakerPath({ streamId: res.thread.stream_id, main: false }));
    } catch (e) {
      actionError = `${e}`;
    } finally {
      creatingThread = false;
    }
  }

  // Status filter: what is working, what needs you, what is done — counted
  // across this mode's conversations (Chat: every one but the Baker's,
  // tasks included; Baker: its threads), and one click to list just those.
  const filter = $derived(sidebarFilter.value);
  const modeConvs = $derived(
    mode === "baker"
      ? bakerConvs.active
      : allConversations(studioStore.data).filter((c) => !c.baker && !c.archived),
  );
  const FILTER_LABEL: Record<ConversationStatus, string> = {
    needs_input: "Needs you",
    working: "Working",
    done: "Done",
    failed: "Failed",
    idle: "Idle",
  };
  const FILTER_EMPTY: Record<ConversationStatus, string> = {
    needs_input: "Nothing needs you.",
    working: "Nothing is working.",
    done: "Nothing done.",
    failed: "Nothing failed.",
    idle: "Nothing idle.",
  };
  const statusChips = $derived.by(() => {
    const count = (s: ConversationStatus) => modeConvs.filter((c) => c.status === s).length;
    // Failed only while there is something to see (or it is the filter).
    const shown: ConversationStatus[] = ["needs_input", "working", "done"];
    if (count("failed") > 0 || filter === "failed") shown.splice(1, 0, "failed");
    return shown.map((status) => ({ status, label: FILTER_LABEL[status], n: count(status) }));
  });
  const filtered = $derived(filter === "all" ? [] : modeConvs.filter((c) => c.status === filter));

  function filterChipName(label: string, n: number): string {
    return `${label}, ${n}`;
  }

  function isCurrent(c: Conversation): boolean {
    if (mode === "baker") return currentBakerStream === c.streamId;
    return currentChat === c.streamId || (currentHandle !== null && toHandle(c.streamId) === currentHandle);
  }

  /** What a row says after its title: its status, or how long it has been idle. */
  function rowMeta(c: Conversation): string {
    return c.status === "idle" ? ago(c.lastTs) : statusLabel(c.status);
  }

  /** In a filtered list every row has the same status: say where it is, or when. */
  function filteredMeta(c: Conversation): string {
    return c.place && c.place !== "Baker" ? c.place : ago(c.lastTs);
  }

  function pickFilter(next: StatusFilter) {
    sidebarFilter.toggle(next);
  }

  function rowTitle(c: Conversation): string {
    const where = c.place && !c.main ? ` — ${c.place}` : "";
    return `${c.title}${where} · ${statusLabel(c.status)}${c.note ? `: ${c.note}` : ""}`;
  }

  function sessionDot(s: StreamRef): "idle" | "error" {
    return s.exists_on_disk ? "idle" : "error";
  }

  function compactAgo(ts: string): string | null {
    const then = Date.parse(ts);
    if (Number.isNaN(then)) return null;
    const secs = Math.max(0, Math.round((Date.now() - then) / 1000));
    if (secs < 60) return `${secs}s`;
    if (secs < 3600) return `${Math.round(secs / 60)}m`;
    if (secs < 86400) return `${Math.round(secs / 3600)}h`;
    return `${Math.round(secs / 86400)}d`;
  }

  function sessionStatusStr(s: StreamRef): string {
    if (!s.exists_on_disk) return "gone";
    const age = compactAgo(s.last_ts);
    if (age) return age;
    if (s.events > 0) return `${s.events.toLocaleString("en-US")} ev`;
    return "—";
  }

  function toggleProject(projectId: string) {
    const next = new Set(expansion.projects);
    if (next.has(projectId)) next.delete(projectId);
    else next.add(projectId);
    expansion = { projects: next, touched: true };
    saveTreeExpansion(expansion);
  }

  function secondaryLine(s: StreamRef): string {
    const parts: string[] = [];
    if (socketLabel(s) !== s.branch && s.branch) parts.push(s.branch);
    if (socketTag(s) === "in place") parts.push("in place");
    if (isLegacyBakeryBranch(s.branch)) parts.push("legacy");
    parts.push(sessionStatusStr(s));
    return parts.join(" · ");
  }

  const TINTS = ["var(--accent)", "var(--ok)", "var(--warn)", "var(--violet)", "var(--bad)"];
  function projectTint(root: string): string {
    let h = 0;
    for (const c of root) h = (h * 31 + c.charCodeAt(0)) & 0xffff;
    return TINTS[h % TINTS.length];
  }

  function lineageFor(g: ProjectGroup): string {
    return g.sockets
      .map((s) => {
        let line = `${s.branch}\n  └─ ${s.name}`;
        if (!s.in_place) line += `\n       ${s.worktree_path}`;
        return line;
      })
      .join("\n");
  }

  const LIFETIME_CYCLE = ["slice", "living", "burn"] as const;

  async function cycleLifetime(s: StreamRef) {
    actionError = "";
    const next = LIFETIME_CYCLE[(LIFETIME_CYCLE.indexOf(s.lifetime) + 1) % LIFETIME_CYCLE.length];
    try {
      await node.setLifetime(s.stream_id, next);
    } catch (e) {
      actionError = `${e}`;
    }
  }

  async function discardSession(s: StreamRef) {
    actionError = "";
    if (s.lifetime === "living") {
      const ok = await ask(
        `"${s.name}" is pinned (living). Discard it anyway?\n\nThis removes its worktree and room; history is kept.`,
      );
      if (!ok) return;
    }
    try {
      await studioStore.discard(s.stream_id, s.lifetime === "living");
    } catch (e) {
      actionError = `${e}`;
    }
  }

  function addProject() {
    if (onAddProject) onAddProject();
    else goto("/");
    if (compact) onClose?.();
  }

  function openSocket(s: StreamRef) {
    onClose?.();
    void goto(`/w/${toHandle(s.stream_id)}`);
  }

  function openRetired() {
    goto("/?view=retired");
  }
</script>

{#snippet convRow(c: Conversation, href: string, sel: boolean, testid: string, meta: string)}
  <li class="conv-row" class:sel data-testid={testid} data-stream-id={c.streamId} data-status={c.status}>
    <a
      class="conv-link"
      {href}
      aria-current={sel ? "page" : undefined}
      aria-label={rowTitle(c)}
      title={rowTitle(c)}
      onclick={() => onClose?.()}
    >
      {#if c.main}
        <span class="conv-glyph" aria-hidden="true">B</span>
      {:else}
        <span class="conv-dot {c.status}" aria-hidden="true"></span>
      {/if}
      <span class="conv-name">{c.title}</span>
      {#if c.pendingGates > 0}<span class="gate-badge" aria-hidden="true">{c.pendingGates}</span>{/if}
      <span class="conv-meta {c.status}" aria-hidden="true">{meta}</span>
    </a>
    <ConversationActions streamId={c.streamId} onNavigate={() => onClose?.()} />
  </li>
{/snippet}

{#snippet icon(d: string)}
  <svg viewBox="0 0 16 16" aria-hidden="true"><path {d} /></svg>
{/snippet}

<aside class="sidebar" data-testid="sidebar" bind:this={asideEl}>
  <div class="sidebar-brand">
    <a class="sidebar-brand-link" href="/" aria-label="Bakery Factory Floor" onclick={() => onClose?.()}>
      <span class="sidebar-brand-glyph" aria-hidden="true">B</span>
      <span class="sidebar-brand-name">Bakery</span>
    </a>
    <span class="brand-spacer"></span>
    <button
      class="brand-action"
      type="button"
      data-testid="sidebar-toggle"
      aria-controls="primary-sidebar"
      aria-expanded="true"
      aria-label="Hide navigation"
      title="Hide navigation (⌘\)"
      onclick={() => onToggle?.()}
    >
      <svg viewBox="0 0 18 18" aria-hidden="true">
        <rect x="2.25" y="2.75" width="13.5" height="12.5" rx="2.25"></rect>
        <path d="M6.25 3v12"></path>
      </svg>
    </button>
  </div>

  <div class="nav-top">
    <div class="mode-toggle" role="tablist" aria-label="Chat or Baker" data-testid="mode-toggle">
      {#each TABS as tab (tab.mode)}
        <button
          type="button"
          role="tab"
          class="mode-tab"
          class:active={mode === tab.mode}
          aria-selected={mode === tab.mode}
          title={tab.title}
          data-testid="mode-{tab.mode}"
          onclick={() => switchMode(tab.mode)}
        >
          <svg viewBox="0 0 18 18" aria-hidden="true"><path d={tab.icon} /></svg>
          <span>{tab.label}</span>
        </button>
      {/each}
    </div>

    <button
      type="button"
      class="nav-search"
      aria-label="Search projects and work"
      aria-keyshortcuts="Meta+K Control+K"
      onclick={() => onOpenPalette?.()}
    >
      {@render icon("M7 11.5a4.5 4.5 0 1 0 0-9 4.5 4.5 0 0 0 0 9ZM10.3 10.3 13.5 13.5")}
      <span class="nav-search-label">Search</span>
      <kbd>⌘K</kbd>
    </button>

    {#if mode === "baker"}
      <button
        type="button"
        class="nav-item"
        onclick={newThread}
        disabled={creatingThread}
        title="Another Baker conversation, running in parallel"
        data-testid="baker-new-thread"
      >
        {@render icon("M8 3v10M3 8h10")}
        <span>{creatingThread ? "Starting…" : "New thread"}</span>
      </button>
    {:else}
      <a class="nav-item" href="/chat" class:sel={page.url.pathname === "/chat"} onclick={() => onClose?.()} data-testid="sidebar-new-chat">
        {@render icon("M8 3v10M3 8h10")}
        <span>New chat</span>
      </a>
      <a class="nav-item" class:sel={page.url.pathname === "/"} href="/" onclick={() => onClose?.()} data-testid="sidebar-floor">
        {@render icon("M2.5 2.5h4.5v4.5H2.5zM9 2.5h4.5v3H9zM9 7.5h4.5v6H9zM2.5 9h4.5v4.5H2.5z")}
        <span>Floor</span>
      </a>
    {/if}
  </div>

  <div class="status-filter" role="group" aria-label="Show conversations by status" data-testid="status-filter">
    <button
      type="button"
      class="filter-chip"
      aria-pressed={filter === "all"}
      aria-label={filterChipName("All", modeConvs.length)}
      onclick={() => pickFilter("all")}
    >All <span class="filter-n">{modeConvs.length}</span></button>
    {#each statusChips as chip (chip.status)}
      <button
        type="button"
        class="filter-chip"
        aria-pressed={filter === chip.status}
        aria-label={filterChipName(chip.label, chip.n)}
        data-status={chip.status}
        onclick={() => pickFilter(chip.status)}
      >
        <span class="conv-dot {chip.status}" aria-hidden="true"></span>{chip.label}
        <span class="filter-n">{chip.n}</span>
      </button>
    {/each}
  </div>

  {#if actionError}
    <p class="action-error">
      {actionError}
      <button type="button" class="dismiss-error" onclick={() => { actionError = ""; }} aria-label="Dismiss">×</button>
    </p>
  {/if}

  <div class="tree">
    {#if filter !== "all"}
      <ul class="conv-list flat" aria-label={FILTER_LABEL[filter]}>
        {#each filtered as c (c.streamId)}
          {@render convRow(
            c,
            mode === "baker" ? codePath(c) : openPath(c),
            isCurrent(c),
            mode === "baker" ? "baker-thread-row" : "chat-row",
            filteredMeta(c),
          )}
        {/each}
      </ul>
      {#if filtered.length === 0}
        <p class="tree-empty">{FILTER_EMPTY[filter]}</p>
      {/if}
    {:else if mode === "baker"}
      <!-- Baker mode: the one Baker and its conversations — the main one plus
           every parallel thread (BAKER-THREADS-03). Same identity, persona,
           memory, and toolbox in each; each runs on its own. No heading: the
           filter's "All" already counts them. -->
      {#if bakerConvs.active.length === 0}
        <!-- No conversation projected yet (the Baker is minted on first
             open, or the node predates threads): the main conversation is
             always reachable regardless. -->
        <ul class="conv-list flat" aria-label="Baker conversations">
          <li class="conv-row" class:sel={page.url.pathname === "/baker"} data-testid="baker-thread-row">
            <a
              class="conv-link"
              href="/baker"
              aria-current={page.url.pathname === "/baker" ? "page" : undefined}
              onclick={() => onClose?.()}
            >
              <span class="conv-glyph" aria-hidden="true">B</span>
              <span class="conv-name">Baker</span>
            </a>
          </li>
        </ul>
        {#if nodeLacksThreads}
          <p class="tree-empty">Restart bakery-node to enable parallel Baker threads.</p>
        {/if}
      {:else}
        <ul class="conv-list flat" aria-label="Baker conversations">
          {#each bakerConvs.active as c (c.streamId)}
            {@render convRow(c, codePath(c), currentBakerStream === c.streamId, "baker-thread-row", rowMeta(c))}
          {/each}
        </ul>
      {/if}

    {:else}
    <div class="section-hdr">
      <span class="section-name">Projects</span>
      <span class="count-chip">{projectCount}</span>
      <div class="hdr-spacer"></div>
      {#if retiredCount > 0}
        <button class="hdr-icon-btn retired-toggle" onclick={openRetired} title={`${retiredCount} retired — view on the Floor`}>
          {retiredCount}
        </button>
      {/if}
      <button
        class="hdr-icon-btn"
        class:active={lineageVisible}
        onclick={() => { lineageVisible = !lineageVisible; }}
        aria-pressed={lineageVisible}
        title="Toggle lineage"
      >
        <svg viewBox="0 0 14 14" width="12" height="12" fill="none"
             stroke="currentColor" stroke-width="1.5"
             stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
          <circle cx="3" cy="3" r="1.5"/>
          <circle cx="3" cy="11" r="1.5"/>
          <circle cx="11" cy="7" r="1.5"/>
          <line x1="3" y1="4.5" x2="3" y2="9.5"/>
          <path d="M3 7 Q7 7 9.5 7"/>
        </svg>
      </button>
      <button class="hdr-icon-btn" onclick={addProject} title="New project" aria-label="New project">
        <svg viewBox="0 0 14 14" width="12" height="12" fill="none"
             stroke="currentColor" stroke-width="1.5"
             stroke-linecap="round" aria-hidden="true">
          <line x1="7" y1="2" x2="7" y2="12"/>
          <line x1="2" y1="7" x2="12" y2="7"/>
        </svg>
      </button>
    </div>

    {#each activeProjectGroups as g (g.project_id)}
      {@const projExpanded = expansion.projects.has(g.project_id)}
      <div class="proj-row">
        <button
          class="chevron-btn"
          onclick={() => toggleProject(g.project_id)}
          aria-expanded={projExpanded}
          aria-label="{projExpanded ? 'Collapse' : 'Expand'} {g.name}"
        >
          <span class="chevron" class:open={projExpanded} aria-hidden="true">›</span>
        </button>
        <a class="proj-link" href="/?project={encodeURIComponent(g.project_id)}" title="Open {g.name} on the Floor" onclick={() => onClose?.()}>
          <span class="proj-initial" style="background:{projectTint(g.project_id)}" aria-hidden="true">
            {(g.name[0] ?? "?").toUpperCase()}
          </span>
          <span class="proj-name">{g.name}</span>
          {#if g.pinned}<span class="pin-dot" title="Pinned" aria-hidden="true">📌</span>{/if}
        </a>
        <span class="proj-count">{g.sockets.length}</span>
        <div class="proj-menu">
          <button
            class="hdr-icon-btn proj-menu-trigger"
            onclick={() => { menuOpenFor = menuOpenFor === g.project_id ? null : g.project_id; }}
            aria-expanded={menuOpenFor === g.project_id}
            aria-label="Project menu for {g.name}"
            title="Project menu"
          >⋯</button>
          {#if menuOpenFor === g.project_id}
            <div class="proj-menu-popover">
              <button type="button" onclick={() => renameProject(g)}>Rename…</button>
              <button type="button" onclick={() => togglePin(g)}>{g.pinned ? "Unpin" : "Pin"}</button>
              <button type="button" class="danger" onclick={() => removeProject(g)}>Remove from Bakery</button>
            </div>
          {/if}
        </div>
      </div>
      {#if projExpanded}
        <div class="session-track">
          {#if g.sockets.length === 0}
            <p class="tree-empty">No active sessions.</p>
          {/if}
          {#each g.sockets as s (s.stream_id)}
            {@const sel = currentHandle !== null && toHandle(s.stream_id) === currentHandle}
            {#if s.state === "task"}
              <TaskRow
                {s}
                task={taskStore.forStream(s.stream_id)}
                selected={sel}
                onOpen={() => openSocket(s)}
              >
                {#snippet actions()}<ConversationActions streamId={s.stream_id} onNavigate={() => onClose?.()} />{/snippet}
              </TaskRow>
            {:else}
              <SocketRow
                {s}
                selected={sel}
                dotClass={sessionDot(s)}
                displayName={socketLabel(s)}
                seatChipText="orc {model ?? '—'}"
                statusText={secondaryLine(s)}
                retiring={studioStore.isRetiring(s.stream_id)}
                onOpen={() => openSocket(s)}
                onCycleLifetime={(s) => void cycleLifetime(s)}
                onDiscard={(s) => void discardSession(s)}
              >
                {#snippet actions()}<ConversationActions streamId={s.stream_id} onNavigate={() => onClose?.()} />{/snippet}
              </SocketRow>
            {/if}
          {/each}
        </div>
        {#if lineageVisible}
          <pre class="lineage">{lineageFor(g)}</pre>
        {/if}
      {/if}
    {/each}

    {#if activeProjectGroups.length === 0}
      <p class="tree-empty">No projects yet.</p>
    {/if}

    <!-- 07d-A3: Ungrouped is real — sockets with no `socket.project.set`
         fact, or one that no longer resolves to an active Project. Loose
         chats (no folder of their own) are listed under Chats instead, and
         the Baker has its own mode. -->
    {#if ungroupedCount > 0}
    <div class="section-hdr ungrouped-hdr">
      <span class="section-name">Ungrouped</span>
      <span class="count-chip">{ungroupedCount}</span>
    </div>
      <div class="session-track">
        {#each activeUngrouped as s (s.stream_id)}
          {@const sel = currentHandle !== null && toHandle(s.stream_id) === currentHandle}
          <SocketRow
            {s}
            selected={sel}
            dotClass={sessionDot(s)}
            displayName={socketLabel(s)}
            seatChipText="orc {model ?? '—'}"
            statusText={secondaryLine(s)}
            retiring={studioStore.isRetiring(s.stream_id)}
            onOpen={() => openSocket(s)}
            onCycleLifetime={(s) => void cycleLifetime(s)}
            onDiscard={(s) => void discardSession(s)}
          >
            {#snippet actions()}<ConversationActions streamId={s.stream_id} onNavigate={() => onClose?.()} />{/snippet}
          </SocketRow>
        {/each}
      </div>
    {/if}
      <div class="section-hdr ungrouped-hdr">
        <span class="section-name">Chats</span>
        <span class="count-chip">{chatList.active.length}</span>
      </div>
      {#if chatList.active.length === 0}
        <p class="tree-empty">No conversations yet.</p>
      {/if}
      <ul class="conv-list flat" aria-label="Chats">
        {#each chatList.active as c (c.streamId)}
          {@render convRow(c, chatPath(c), currentChat === c.streamId, "chat-row", rowMeta(c))}
        {/each}
      </ul>
    {/if}
  </div>

  <!-- One-click controls, no popover: Settings itself (with the node's
       status), the color scheme, and what agents may write without asking. -->
  <div class="settings-bar" data-testid="settings-bar">
    <a
      class="bar-settings"
      href="/settings/agents"
      class:sel={page.url.pathname.startsWith("/settings") || !!page.state.settings}
      onclick={() => onClose?.()}
      title="Settings — node {statusWord.toLowerCase()}"
    >
      <span class="node-dot" class:glow={status === "open"} style="background: {status === 'open' ? 'var(--ok)' : status === 'connecting' ? 'var(--warn)' : 'var(--bad)'};" aria-hidden="true"></span>
      <svg viewBox="0 0 16 16" width="13" height="13" fill="none"
           stroke="currentColor" stroke-width="1.5"
           stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
        <circle cx="8" cy="8" r="2.5"/>
        <path d="M8 1.5v1M8 13.5v1M1.5 8h1M13.5 8h1M3.4 3.4l.7.7M11.9 11.9l.7.7M3.4 12.6l.7-.7M11.9 4.1l.7-.7"/>
      </svg>
      <span>Settings</span>
    </a>
    <span class="bar-spacer"></span>
    <button type="button" class="theme-chip" onclick={cycleTheme} title="Color scheme — click to switch; edit in Settings → Appearance">
      {themeLabel(appearance.selected, appearance)}
    </button>
  </div>
</aside>

<style>
  .sidebar {
    width: 100%;
    height: 100%;
    background: var(--bg-chrome);
    border-right: 1px solid var(--line-chrome);
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }

  .sidebar-brand {
    display: flex;
    align-items: center;
    gap: 5px;
    /* Same height and rule as the title bar, so the two read as one bar. */
    height: 40px;
    padding: 0 8px 0 10px;
    border-bottom: 1px solid var(--line-chrome);
    flex: 0 0 auto;
  }
  .sidebar-brand-link {
    display: flex;
    align-items: center;
    gap: 9px;
    min-width: 0;
    color: var(--fg);
    text-decoration: none;
  }
  .sidebar-brand-link:hover { text-decoration: none; }
  .sidebar-brand-glyph {
    display: grid;
    place-items: center;
    width: 24px;
    height: 24px;
    border-radius: 8px;
    background: var(--accent-solid);
    color: var(--fg-on-solid);
    box-shadow: var(--shadow-chip);
    font: 750 13px/1 var(--font-ui);
  }
  .sidebar-brand-name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font: 650 14px/1 var(--font-ui);
    letter-spacing: -0.01em;
  }
  .brand-spacer { flex: 1 1 auto; }
  .brand-action {
    display: grid;
    place-items: center;
    width: 28px;
    height: 28px;
    padding: 0;
    border: 1px solid transparent;
    border-radius: 8px;
    background: transparent;
    color: var(--fg-faint);
    cursor: pointer;
  }
  .brand-action:hover {
    border-color: var(--line-soft);
    background: var(--bg-chip);
    color: var(--fg);
  }
  .brand-action:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }
  .brand-action svg {
    width: 16px;
    height: 16px;
    fill: none;
    stroke: currentColor;
    stroke-width: 1.35;
    stroke-linecap: round;
    stroke-linejoin: round;
  }

  /* ---- Top: the switch, search, new work --------------------------------- */

  .nav-top {
    flex: 0 0 auto;
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: 8px 8px 4px;
  }
  .nav-top svg {
    width: 15px;
    height: 15px;
    flex: 0 0 auto;
    fill: none;
    stroke: currentColor;
    stroke-width: 1.45;
    stroke-linecap: round;
    stroke-linejoin: round;
  }

  /* Chat / Baker — colours only from Appearance tokens. */
  .mode-toggle {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 2px;
    margin-bottom: 6px;
    padding: 2px;
    border: 1px solid var(--line-soft);
    border-radius: var(--r-xl);
    background: var(--bg-app);
  }
  .mode-tab {
    min-width: 0;
    height: 26px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: 6px;
    padding: 0 8px;
    border: 0;
    border-radius: var(--r-lg);
    background: transparent;
    color: var(--fg-faint);
    font: 600 12px/1 var(--font-ui);
    cursor: pointer;
  }
  .mode-tab:hover { color: var(--fg-2); }
  .mode-tab.active {
    color: var(--fg);
    background: var(--bg-chip);
    box-shadow: var(--shadow-chip);
  }
  .mode-tab:focus-visible { outline: 2px solid var(--accent); outline-offset: -2px; }

  .nav-search,
  .nav-item {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    min-height: 30px;
    box-sizing: border-box;
    padding: 0 8px;
    border: 1px solid transparent;
    border-radius: var(--r-lg);
    background: transparent;
    font: 540 12.5px/1 var(--font-ui);
    text-align: left;
    text-decoration: none;
    cursor: pointer;
  }
  .nav-search {
    margin-bottom: 4px;
    border-color: var(--line-soft);
    background: var(--bg-app);
    color: var(--fg-faint);
  }
  .nav-search:hover { border-color: var(--line-strong); color: var(--fg-dim); }
  .nav-search-label { flex: 1 1 auto; font-weight: 450; }
  .nav-search kbd {
    border: 0;
    background: transparent;
    color: var(--fg-ghost);
    font: 500 10px/1 var(--font-mono);
  }
  .nav-item { color: var(--fg-2); }
  .nav-item:hover { background: var(--bg-chip); color: var(--fg); text-decoration: none; }
  .nav-item.sel { background: var(--accent-bg); color: var(--accent-fg); }
  .nav-item:disabled { opacity: 0.55; cursor: default; }
  .nav-search:focus-visible,
  .nav-item:focus-visible { outline: 2px solid var(--accent); outline-offset: -2px; }

  /* ---- Status filter ------------------------------------------------------- */

  .status-filter {
    flex: 0 0 auto;
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
    padding: 6px 8px 8px;
    border-bottom: 1px solid var(--line-chrome);
  }
  .filter-chip {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    height: 22px;
    padding: 0 8px;
    border: 1px solid var(--line-soft);
    border-radius: var(--r-pill);
    background: transparent;
    color: var(--fg-dim);
    font: 550 11px/1 var(--font-ui);
    white-space: nowrap;
    cursor: pointer;
  }
  .filter-chip:hover { border-color: var(--line-strong); color: var(--fg-2); }
  .filter-chip[aria-pressed="true"] {
    border-color: var(--accent-border);
    background: var(--accent-bg);
    color: var(--accent-fg);
  }
  .filter-chip:focus-visible { outline: 2px solid var(--accent); outline-offset: 1px; }
  .filter-chip .conv-dot { width: 6px; height: 6px; margin: 0; }
  .filter-n {
    color: var(--fg-faint);
    font: 500 10.5px/1 var(--font-mono);
    font-variant-numeric: tabular-nums;
  }
  .filter-chip[aria-pressed="true"] .filter-n { color: inherit; }

  /* ---- Conversation rows ----------------------------------------------------- */

  .conv-list {
    list-style: none;
    margin: 0;
  }
  .conv-list.flat { padding: 0 2px; }
  .tree > .conv-list:first-child { margin-top: 4px; }
  .conv-row .proj-initial { margin: 0 1px; }
  .conv-row {
    display: flex;
    align-items: center;
    gap: 2px;
    border-radius: 8px;
  }
  .conv-row:hover { background: var(--bg-chip); }
  .conv-row.sel { background: var(--accent-bg); }
  .conv-row :global(.ca) { opacity: 0; }
  .conv-row:hover :global(.ca),
  .conv-row:focus-within :global(.ca),
  .conv-row.sel :global(.ca) { opacity: 1; }
  @media (hover: none) {
    .conv-row :global(.ca) { opacity: 1; }
  }
  .conv-link {
    flex: 1 1 0;
    min-width: 0;
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 7px 8px;
    border-radius: 8px;
    color: var(--fg-2);
    text-decoration: none;
  }
  .conv-link:hover { text-decoration: none; color: var(--fg); }
  .conv-row.sel .conv-link { color: var(--accent-fg); }
  .conv-link:focus-visible { outline: 2px solid var(--accent); outline-offset: -2px; }
  .conv-glyph {
    flex: 0 0 auto;
    width: 18px;
    height: 18px;
    display: grid;
    place-items: center;
    border-radius: 6px;
    background: var(--accent-solid);
    color: var(--fg-on-solid);
    font: 700 10px/1 var(--font-ui);
  }
  .conv-dot {
    flex: 0 0 auto;
    width: 7px;
    height: 7px;
    margin: 0 5.5px;
    border-radius: 50%;
    background: var(--fg-ghost);
  }
  .conv-dot.needs_input { background: var(--warn); }
  .conv-dot.failed { background: var(--bad); }
  .conv-dot.done { background: var(--accent); }
  .conv-dot.working {
    background: var(--ok);
    box-shadow: var(--glow-live);
    animation: conv-pulse 1.4s ease-in-out infinite;
  }
  @keyframes conv-pulse {
    50% { opacity: 0.45; }
  }
  .conv-name {
    flex: 1 1 auto;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font: 540 12.5px/1.2 var(--font-ui);
  }
  .conv-meta {
    flex: 0 0 auto;
    color: var(--fg-faint);
    font: 400 10px/1 var(--font-mono);
  }
  .conv-meta.working { color: var(--ok); }
  .conv-meta.needs_input { color: var(--warn); font-weight: 600; }
  .conv-meta.failed { color: var(--bad); }
  .conv-meta.done { color: var(--accent-fg); }
  .gate-badge {
    flex: 0 0 auto;
    min-width: 16px;
    padding: 2px 5px;
    border-radius: var(--r-pill);
    background: var(--warn-bg);
    color: var(--warn);
    font: 650 10px/1 var(--font-mono);
    text-align: center;
  }
  @media (prefers-reduced-motion: reduce) {
    .conv-dot.working { animation: none; }
  }

  /* ---- Section header ------------------------------------------------------- */

  .section-hdr {
    display: flex;
    align-items: center;
    gap: 5px;
    padding: 11px 11px 6px;
    flex: 0 0 auto;
  }
  .ungrouped-hdr { margin-top: 6px; padding-top: 8px; border-top: 1px solid var(--line-chrome); }
  .section-name {
    font: 650 9.5px/1 var(--font-ui);
    letter-spacing: 0.08em;
    color: var(--fg-fainter);
    text-transform: uppercase;
  }
  .count-chip {
    padding: 2px 5px;
    border-radius: var(--r-sm);
    background: var(--bg-chip);
    color: var(--fg-dim);
    font: 400 10.5px/1 var(--font-mono);
  }
  .hdr-spacer { flex: 1 1 auto; }
  .hdr-icon-btn {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 20px;
    height: 20px;
    border: none;
    background: transparent;
    color: var(--fg-faint);
    cursor: pointer;
    border-radius: var(--r-sm);
    padding: 0;
  }
  .hdr-icon-btn:hover {
    background: var(--bg-chip);
    color: var(--fg);
  }
  .hdr-icon-btn.active {
    background: var(--accent-bg);
    color: var(--accent-fg);
  }
  .retired-toggle {
    width: auto;
    padding: 0 6px;
    font: 600 10px/1 var(--font-mono);
  }

  /* ---- Tree ----------------------------------------------------------------- */

  .tree {
    flex: 1 1 0;
    overflow-y: auto;
    overflow-x: hidden;
    padding: 2px 4px 8px;
    min-height: 0;
  }

  .proj-row {
    display: flex;
    align-items: center;
    gap: 3px;
    width: 100%;
    padding: 2px 5px 2px 1px;
    border-radius: 8px;
    color: var(--fg-2);
  }
  .proj-row:hover {
    background: var(--bg-chip);
  }
  .chevron-btn {
    flex: 0 0 auto;
    width: 20px;
    height: 24px;
    display: flex;
    align-items: center;
    justify-content: center;
    background: transparent;
    border: none;
    cursor: pointer;
    padding: 0;
  }
  .chevron {
    font-size: 11px;
    color: var(--fg-faint);
    display: inline-block;
    transform: rotate(0deg);
    transition: transform 120ms ease-out;
    line-height: 1;
  }
  .chevron.open {
    transform: rotate(90deg);
  }
  .proj-link {
    display: flex;
    align-items: center;
    gap: 7px;
    flex: 1 1 0;
    min-width: 0;
    padding: 5px 4px;
    border-radius: 7px;
    text-decoration: none;
    color: inherit;
  }
  .proj-link:hover {
    background: var(--accent-bg);
    text-decoration: none;
  }
  .proj-initial {
    width: 18px;
    height: 18px;
    border-radius: 6px;
    display: flex;
    align-items: center;
    justify-content: center;
    font: 650 9.5px/1 var(--font-ui);
    color: var(--fg-on-tint);
    flex: 0 0 auto;
  }
  .proj-name {
    font: 560 12px/1 var(--font-ui);
    flex: 1 1 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .proj-count {
    font: 400 10.5px/1 var(--font-mono);
    color: var(--fg-faint);
    flex: 0 0 auto;
    white-space: nowrap;
  }
  .pin-dot {
    flex: 0 0 auto;
    font-size: 9px;
    line-height: 1;
  }

  .proj-menu {
    position: relative;
    flex: 0 0 auto;
  }
  .proj-menu-trigger {
    font: 600 12px/1 var(--font-ui);
  }
  .proj-menu-popover {
    position: absolute;
    top: 100%;
    right: 0;
    margin-top: 2px;
    display: flex;
    flex-direction: column;
    min-width: 150px;
    padding: 4px;
    background: var(--bg-raised);
    border: 1px solid var(--line-strong);
    border-radius: var(--r-lg);
    box-shadow: var(--shadow-menu);
    z-index: 40;
  }
  .proj-menu-popover button {
    display: block;
    width: 100%;
    padding: 5px 8px;
    border: none;
    background: transparent;
    color: var(--fg-2);
    font: 400 11.5px/1.3 var(--font-ui);
    text-align: left;
    cursor: pointer;
    border-radius: var(--r-sm);
  }
  .proj-menu-popover button:hover {
    background: var(--bg-chip);
  }
  .proj-menu-popover button.danger {
    color: var(--bad);
  }
  .proj-menu-popover button.danger:hover {
    background: var(--bad-bg);
  }

  .session-track {
    margin-left: 12px;
    border-left: 1px solid var(--line);
    padding: 2px 0 3px 5px;
  }

  .action-error {
    display: flex;
    align-items: center;
    gap: 6px;
    margin: 0;
    padding: 6px 10px;
    font: 400 11px/1.4 var(--font-ui);
    color: var(--bad);
    background: var(--bad-bg);
    border-bottom: 1px solid var(--line-chrome);
  }
  .dismiss-error {
    margin-left: auto;
    flex: 0 0 auto;
    border: none;
    background: transparent;
    color: inherit;
    cursor: pointer;
    font: 600 13px/1 var(--font-ui);
    padding: 0 2px;
  }

  .lineage {
    margin: 0 0 0 4px;
    padding: 5px 8px 5px 10px;
    font: 400 10.5px/1.9 var(--font-mono);
    color: var(--fg-ghost);
    white-space: pre;
    overflow-x: auto;
    border-left: 1px dashed var(--line-strong);
  }

  .tree-empty {
    padding: 12px 10px;
    font: 400 11px/1.5 var(--font-ui);
    color: var(--fg-faint);
    margin: 0;
  }

  /* ---- Settings bar ------------------------------------------------------ */

  .settings-bar {
    flex: 0 0 auto;
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 8px 8px 9px;
    border-top: 1px solid var(--line-chrome);
  }
  .bar-settings {
    display: inline-flex;
    align-items: center;
    gap: 7px;
    min-width: 0;
    padding: 6px 8px;
    border-radius: 8px;
    color: var(--fg-dim);
    font: 500 12px/1 var(--font-ui);
    text-decoration: none;
  }
  .bar-settings:hover { background: var(--bg-chip); color: var(--fg); text-decoration: none; }
  .bar-settings.sel { background: var(--accent-bg); color: var(--accent-fg); }
  .bar-settings:focus-visible { outline: 2px solid var(--accent); outline-offset: 1px; }
  .bar-spacer { flex: 1 1 auto; }
  .node-dot {
    width: 7px;
    height: 7px;
    border-radius: var(--r-pill);
    flex: 0 0 auto;
  }
  .node-dot.glow { box-shadow: var(--glow-live); }
  .theme-chip {
    margin: 0;
    padding: 3px 8px;
    border-radius: var(--r-sm);
    background: var(--bg-chip);
    border: 1px solid var(--line);
    color: var(--fg-dim);
    font: 600 10.5px/1 var(--font-ui);
    flex-shrink: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    cursor: pointer;
  }
  .theme-chip:hover { color: var(--fg); border-color: var(--line-strong); }
</style>
