<script lang="ts">
  import { mount, unmount, type Component } from "svelte";
  import {
    createDockview,
    themeAbyss,
    type DockviewApi,
    type IContentRenderer,
    type CreateComponentOptions,
    type AddPanelOptions,
    type SerializedDockview,
  } from "dockview";
  import "dockview/dist/styles/dockview.css";
  import { goto } from "$app/navigation";
  import { WorkbenchState } from "$lib/workbench.svelte";
  import { allConversations, conversationFor, reviewPath, statusLabel, type Conversation } from "$lib/conversations";
  import { studioStore } from "$lib/studioStore.svelte";
  import { taskStore } from "$lib/taskStore.svelte";
  import { loadDockLayout, saveDockLayout, resetDockLayout, type WidgetId } from "$lib/paneState";
  import { WIDGETS } from "$lib/widgets";
  import { node, type AgentMapNode, type TerminalInfo, type TerminalMode } from "$lib/node";
  import { canPickMode, defaultTerminalMode } from "$lib/terminalMode";
  import { deriveAvailableModels } from "$lib/studioTarget";
  import { createAgentPanelSource, refreshStudio } from "$lib/react/bridge.svelte";
  import { mountAgentMap, mountAgentPanel } from "$lib/react/mount";
  import { revealSubagent } from "$lib/react/subagentReveal";
  import { useTitleBarSlot } from "$lib/shell/titleBarSlots.svelte";
  import Memory from "$lib/widgets/Memory.svelte";
  import Progress from "$lib/widgets/Progress.svelte";
  import Review from "$lib/widgets/Review.svelte";
  import TerminalWidget from "$lib/widgets/Terminal.svelte";

  // Replaces the old fixed a/b/c slots (07c-STUDIO-SHELL §2): real dockable,
  // resizable, tabbed panels, serialized through Dockview's own layout
  // grammar and persisted per socket. Singleton widgets (review, code,
  // memory, events) get one fixed-id panel each, added/removed via the
  // panel menu below. Chat and terminals are the two genuinely
  // multi-instance widgets: one Chat panel per configured *agent*
  // (07d-A-AGENT-IDENTITY — panel id `chat:{role_id}`, params carry
  // `agentId`/`roleId`, content reads `workbench.conversationFor(agentId,
  // roleId)`, never the socket-wide `workbench.items`), one panel per
  // node-owned named terminal session (07c-STUDIO-SHELL §3). Code.svelte
  // folds the file tree and Monaco editor into one panel (Files no longer
  // exists as its own).
  //
  // Moving, hiding, or (with `renderer: 'always'`) even tab-switching away
  // from a chat/code/terminal panel never unmounts its Svelte component —
  // Dockview keeps the DOM alive off-screen. `AgentConversation`'s own
  // per-agent state, Editor.svelte's model registry, and the node's
  // terminal registry mean even a hard unmount (panel actually removed)
  // loses nothing but the *view*: reopening reattaches to the same
  // conversation, the same file model, or the same running shell.

  let {
    workbench,
    kind,
    openPanel = null,
    focusChange = null,
  }: {
    workbench: WorkbenchState;
    kind: "factory" | "project" | null;
    /** With `openPanel: "review"`, the change Review opens on (a proposal's
     *  event hash, from `&change=`). */
    focusChange?: string | null;
    /** One singleton widget id to open (or focus, if already open) right
     *  after this stream's layout loads — the seam behind the Factory
     *  Floor's task-row "review →" link (07d-B Phase 4 item 3: "a link to
     *  the task's Review", not just to the task's conversation). Applied
     *  once per `bindTo` (see the effect below), never re-applied on a
     *  later re-render of the same bound stream. */
    openPanel?: WidgetId | null;
  } = $props();

  let hostEl = $state<HTMLDivElement>();
  let dockApi: DockviewApi | undefined;
  /** Reactive echo of "`dockApi` is set" — `dockApi` itself stays a plain
   *  variable (Svelte 5's `$state` proxy breaks a class instance that uses
   *  private fields internally, which `DockviewApi` does), so an effect
   *  that only reads `dockApi` is not re-triggered when `buildDock` sets it.
   *  07d-A1 correction: this matters now that `.dock-host` (and so
   *  `hostEl`, and so `dockApi`) is no longer available on first render —
   *  the bootstrap effect below needs a real dependency to wake it up once
   *  `dockApi` actually exists, not a value it can only read once by luck. */
  let dockApiReady = $state(false);
  let boundStreamId: string | null = null;
  let saveTimer: ReturnType<typeof setTimeout> | undefined;
  let layoutDisposable: { dispose(): void } | undefined;
  /** Most-recent-first, de-duped ids of every panel that has been active in
   *  this `dockApi`'s lifetime — restoring focus on close picks from here
   *  (non-destructive-panels follow-up). Reset on every `clearDock()`, since
   *  singleton ids ("code", "memory", ...) are NOT unique across streams —
   *  every socket names its default panels identically — so a stale entry
   *  surviving a stream switch could otherwise false-match an unrelated
   *  same-named panel in a different stream. */
  let activePanelHistory: string[] = [];
  /** Snapshot of `dockApi.activePanel?.id` taken the instant *before* a
   *  `"remove"` mutation begins (via `onWillMutateLayout`), so
   *  `onDidRemovePanel` can tell whether the panel that just closed was
   *  actually the focused one — without depending on undocumented
   *  event-ordering between `onDidRemovePanel` and `onDidActivePanelChange`.
   *  Reset to `null` on every non-"remove" mutation (e.g. `clear()`'s own
   *  `"clear"` kind) so a stale value can never match a later, unrelated
   *  removal. */
  let preRemoveActiveId: string | null = null;
  let focusDisposables: { dispose(): void }[] = [];
  // Bumped on every stream switch — lets a `loadForStream`/`reconcileTerminals`
  // call in flight for the *previous* stream recognize it's been superseded.
  let loadGeneration = 0;
  /** Layout restore/clear emits the same Dockview events as an operator drag.
   *  Keep those bootstrap mutations out of persistence: otherwise switching
   *  streams can briefly serialize the previous pane tree under the new
   *  stream's key. The generation value prevents an older async load from
   *  re-enabling writes for the currently loading stream. */
  let persistSuppressedGeneration: number | null = null;

  let addMenuOpen = $state(false);
  // The panel controls live in the title bar, not in a toolbar row of their
  // own above the tab strip.
  useTitleBarSlot("tools", dockTools);
  // Controls must not accept clicks which the pending default-layout load
  // would immediately undo (addChatPanel closes the menu during bootstrap).
  let controlsStreamId = $state<string | null>(null);
  let openTerminals = $state<TerminalInfo[]>([]);
  /** "Add agent" assigns inline, in the menu itself, before opening
   *  anything (07d-A1 §4: "Assign first, then open that agent's
   *  conversation") — `addAgentExpandedId` names which unassigned agent's
   *  row currently shows the inline model picker, `null` when none does. */
  let addAgentExpandedId = $state<string | null>(null);
  let addAgentModel = $state("");
  let addAgentMsg = $state("");

  function toggleAddAgent(agentId: string) {
    addAgentExpandedId = addAgentExpandedId === agentId ? null : agentId;
    addAgentModel = "";
    addAgentMsg = "";
  }

  /** Assigns a model to `roleId` and, only once that succeeds, opens the
   *  real conversation pane for it — `agentId` is already the server-
   *  projected id from this role's own (pre-assignment) `AgentInfo` row,
   *  which does not change when the assignment lands, so no refetch is
   *  needed before opening it. */
  async function assignAndOpen(agentId: string, roleId: string, roleLabel: string) {
    if (!workbench.streamId || !addAgentModel.trim()) return;
    addAgentMsg = "";
    try {
      await node.seatAssign(workbench.streamId, roleId, addAgentModel.trim());
      studioStore.refresh();
      addAgentExpandedId = null;
      addChatPanel(agentId, roleId, roleLabel);
    } catch (e) {
      addAgentMsg = `${e}`;
    }
  }

  const SINGLETON_IDS: readonly WidgetId[] = ["review", "code", "memory", "events", "agents"];
  /** Every widget kind that can be placed as its own dockview group (i.e.
   *  every `SINGLETON_IDS` entry, now that `singletonPosition`'s fallback
   *  opens Review/Memory/Events beside the active panel instead of tabbing
   *  into it — non-destructive-panels follow-up — plus terminal/chat, which
   *  already always could) needs `renderer: "always"`, not just the
   *  tab-switch-preserves-state benefit the name suggests: dockview's
   *  default (lazy) renderer attaches a freshly-split group's content
   *  element to the wrong parent — the group's outer container instead of
   *  its `.dv-content-container` — which visually overlaps the tab strip.
   *  `"always"` attaches to the right container immediately. Reproduced
   *  directly (a fresh "beside the active panel" split with the default
   *  renderer places the content's bounding box flush with the whole
   *  group's box, including its tab strip, instead of the content area
   *  below it) before this was added. */
  //
  // Revised: that misplacement was our own content element (`position:
  // absolute; inset: 0`) resolving against the group box because
  // `.dv-content-container` was not a containing block — fixed in CSS below.
  // And `"always"` has its own failure: its content lives in Dockview's
  // overlay layer, and after a drag-move a hidden tab's overlay can stay
  // painted over the active panel ("Review stuck on top of everything").
  // So the singletons render in place again; only chat and terminal keep
  // `"always"`, where keeping a live React root / xterm attached matters.
  const ALWAYS_RENDER = new Set<WidgetId | "terminal" | "chat">(["terminal", "chat"]);

  /** Layouts saved before the revision above still say `"always"` for a
   *  singleton; bring every restored panel in line with `ALWAYS_RENDER`. */
  function normalizeRenderers() {
    if (!dockApi) return;
    for (const panel of dockApi.panels) {
      if ((SINGLETON_IDS as readonly string[]).includes(panel.id)) panel.api.setRenderer("onlyWhenVisible");
    }
  }

  function panelIdFor(widget: WidgetId): string {
    return widget;
  }
  function terminalPanelId(terminalId: string): string {
    return `terminal:${terminalId}`;
  }
  function chatPanelId(roleId: string): string {
    return `chat:${roleId}`;
  }
  /** Bootstrap-only label for the first-use default chat panel, created
   *  before `/studio/state` may have loaded this socket's `AgentInfo` rows
   *  yet — matches `studio.rs`'s `ROLES` labels by construction
   *  ("orchestrator" → "Orchestrator"). The on-demand menu uses the real
   *  `AgentInfo.role_label` instead once studio state is available. */
  function fallbackRoleLabel(roleId: string): string {
    return roleId.charAt(0).toUpperCase() + roleId.slice(1);
  }
  /** The real server-projected `agent_id` for `roleId` on this socket, or
   *  `null` when it isn't known yet — this client never reconstructs one
   *  (07d-A1 §4: "Every Chat binding consumes `AgentInfo.agent_id` from
   *  `/studio/state`; never reconstruct `${streamId}:${roleId}` in the
   *  client"). Every call site (the "+ Panel" menu, `applyDefaultLayout`,
   *  the unknown-component recovery path) only ever runs once
   *  `bootstrapStatus === "ready"` (see the gate on the effect below), so
   *  in practice this always finds a real row — the `null` return exists so
   *  a caller that somehow runs before then has something honest to check
   *  instead of a guessed id. */
  function agentIdFor(roleId: string): string | null {
    return workbench.socket?.agents.find((a) => a.role_id === roleId)?.agent_id ?? null;
  }

  /** Whether this socket's real kind/agent rows are known yet — "ready"
   *  once `/studio/state` has resolved this specific stream, "loading"
   *  while the fetch that would resolve it is still in flight, else
   *  "unavailable" (settled with no result — an error, or a stream that
   *  genuinely isn't in the response). 07d-A1 correction: the dock must
   *  never bootstrap a default pane (or recover a corrupted/unknown one)
   *  from a guess while this isn't "ready" — see the effect below. */
  const bootstrapStatus = $derived.by((): "ready" | "loading" | "unavailable" => {
    if (workbench.socket) return "ready";
    return studioStore.loading ? "loading" : "unavailable";
  });

  // ---- panel content: bridges Dockview's vanilla renderer contract to ----
  // ---- imperatively-mounted Svelte components, reading live props off ----
  // ---- `workbench` via getters so the mounted instance stays reactive. ---

  function createComponent(options: CreateComponentOptions): IContentRenderer {
    const el = document.createElement("div");
    el.style.position = "absolute";
    el.style.inset = "0";
    el.style.overflow = "hidden";
    let instance: Record<string, unknown> | undefined;
    /** Teardown for a React-rendered panel, when this one is a chat pane. */
    let unmountReact: (() => void) | undefined;
    /** A teammate's conversation (another socket in the same project, or
     *  another Baker thread) reads its own workbench, bound to its own
     *  stream and released with this panel — never the host's. */
    let ownBench: WorkbenchState | undefined;
    let stopOwnBench: (() => void) | undefined;
    let disposed = false;

    function mountInto<P extends Record<string, unknown>>(Comp: Component<P>, props: P) {
      if (disposed) return;
      instance = mount(Comp, { target: el, props }) as Record<string, unknown>;
    }

    function mountChat(agentId: string, roleId: string, teammateStream = "") {
      if (disposed) return;
      let bench = workbench;
      if (teammateStream && teammateStream !== workbench.streamId) {
        // Its own root, so nothing that happens to be running when Dockview
        // builds this panel can own (and later tear down) its state.
        stopOwnBench = $effect.root(() => {
          ownBench = new WorkbenchState();
        });
        ownBench!.bindTo(teammateStream);
        bench = ownBench!;
      }
      // The snapshot source reads the SAME `WorkbenchState` /
      // `AgentConversation` the Svelte pane read — no second transcript, no
      // second busy tracker, no second socket subscription. Only the
      // presentation moved across the boundary.
      const source = createAgentPanelSource(bench, agentId, roleId, {
        openReview: (hash) => {
          if (bench === workbench) return openReviewOn(hash);
          // A teammate's review lives in its own studio.
          const c = conversationFor(studioStore.data, teammateStream);
          if (c) void goto(reviewPath(c, hash));
        },
        onAssigned: refreshStudio,
      });
      unmountReact = mountAgentPanel(el, source);
    }

    return {
      element: el,
      init(initParams) {
        const params = (initParams.params ?? {}) as Record<string, unknown>;
        switch (options.name as WidgetId | "terminal" | "chat") {
          case "chat":
            mountChat(String(params.agentId ?? ""), String(params.roleId ?? ""), String(params.streamId ?? ""));
            break;
          case "events":
            mountInto(Progress, {
              get events() {
                return workbench.events;
              },
              get progress() {
                return workbench.progress;
              },
            } as never);
            break;
          case "review":
            mountInto(Review, {
              get items() {
                return workbench.items;
              },
              get busy() {
                return workbench.busy;
              },
              get studioSocket() {
                return workbench.socket;
              },
              get focusRequest() {
                return reviewFocus;
              },
              onDecide: (h: string, a: boolean, content?: string) => workbench.decide(h, a, content),
            } as never);
            break;
          case "code":
            import("$lib/widgets/Code.svelte").then((mod) => {
              mountInto(mod.default, {
                get streamId() {
                  return workbench.streamId ?? "";
                },
                get openPath() {
                  return workbench.selectedFile;
                },
                get items() {
                  return workbench.items;
                },
                onOpenFile: (p: string) => {
                  workbench.selectedFile = p;
                },
                onCloseFile: (p: string) => {
                  if (workbench.selectedFile === p) workbench.selectedFile = "";
                },
              } as never);
            });
            break;
          case "memory":
            mountInto(Memory, {
              get streamId() {
                return workbench.streamId ?? "";
              },
            } as never);
            break;
          case "agents":
            if (!disposed) unmountReact = mountAgentMap(el, { open: openFromMap });
            break;
          case "terminal": {
            const streamId = String(params.streamId ?? "");
            const terminalId = String(params.terminalId ?? "");
            const name = String(params.name ?? terminalId);
            mountInto(TerminalWidget, {
              streamId,
              terminalId,
              name,
              readOnly: params.readOnly === true,
              onClosed: () => {
                const panel = dockApi?.getPanel(terminalPanelId(terminalId));
                if (panel) dockApi?.removePanel(panel);
                void refreshOpenTerminals();
              },
            } as never);
            break;
          }
          default: {
            // 07d-A0: fail closed, never a blank panel. Reaching here means
            // a restored layout named a component this build doesn't know
            // (most plausibly a stale pre-07d-A-AGENT-IDENTITY
            // `"orchestrator"` panel that reached `fromJSON` despite the
            // layout-key version bump — a corrupted/hand-edited
            // localStorage value, or a future component this build
            // predates) — the socket's own default configured-agent
            // conversation is always a safe, meaningful thing to show
            // instead of an empty pane. `createComponent` only ever runs
            // from inside `loadForStream`, itself gated on
            // `bootstrapStatus === "ready"` (07d-A1 correction), so a real
            // `agent_id` is expected here; if it is somehow still missing
            // this leaves the panel unmounted rather than inventing one.
            const roleId = kind === "factory" ? "baker" : "orchestrator";
            const agentId = agentIdFor(roleId);
            if (agentId) mountChat(agentId, roleId);
          }
        }
      },
      dispose() {
        disposed = true;
        if (instance) unmount(instance);
        // Releases the React root and its snapshot subscription. It does not
        // touch the `AgentConversation`, so closing or detaching a pane
        // never stops the running turn — reopening finds the same history
        // and the same in-flight work.
        unmountReact?.();
        unmountReact = undefined;
        ownBench?.unbind();
        stopOwnBench?.();
        ownBench = undefined;
      },
    };
  }

  // ---- add / reopen panels -------------------------------------------------

  function addPanelOpts<P extends object>(component: WidgetId | "terminal" | "chat", opts: Omit<AddPanelOptions<P>, "component" | "renderer">): AddPanelOptions<P> {
    return { ...opts, component, renderer: ALWAYS_RENDER.has(component) ? "always" : undefined } as AddPanelOptions<P>;
  }

  /** 07c-UX-SHELL §2.3: Code opens to the right of the conversation. Every
   *  other on-demand singleton (Review, Memory, Events) opens beside
   *  whichever panel is currently active — never tabbed within it, which
   *  would hide the panel the user was just looking at instead of just
   *  backgrounding it (non-destructive-panels follow-up). "Beside the
   *  active panel" composes correctly for a 3rd/4th column too: each new
   *  singleton just splits whatever's active *right now*, so opening
   *  Memory while Code is active lands Memory to the right of Code
   *  specifically, not of Chat. */
  function singletonPosition(widget: WidgetId) {
    if (!dockApi) return undefined;
    if (widget === "code") {
      const chat = dockApi.panels.find((p) => p.id.startsWith("chat:"));
      if (chat) return { referencePanel: chat.id, direction: "right" as const };
    }
    const active = dockApi.activePanel;
    return active ? { referencePanel: active.id, direction: "right" as const } : undefined;
  }

  /** An on-demand second (or later) chat pane — not specified by the spec,
   *  so it keeps the same "tab into the active panel" default every other
   *  on-demand singleton uses. */
  function chatPosition() {
    if (!dockApi) return undefined;
    const active = dockApi.activePanel;
    return active ? { referencePanel: active.id, direction: "within" as const } : undefined;
  }

  /** 07c-UX-SHELL §2.3: a new or reopened terminal defaults along the
   *  bottom. The first terminal creates that bottom row (`direction:
   *  "below"`, relative to the whole grid); every later terminal joins the
   *  same row as a tab instead of splitting a fresh row each time. */
  function terminalPosition() {
    if (!dockApi) return { direction: "below" as const };
    const existing = dockApi.panels.find((p) => p.id.startsWith("terminal:"));
    if (existing) return { referencePanel: existing.id, direction: "within" as const };
    return { direction: "below" as const };
  }

  /** The open singleton panels — Dockview's own panel set, mirrored into
   *  state so the title bar's Review / Code buttons can show it. (`dockApi`
   *  itself is not reactive; see `dockApiReady`.) */
  let openSingletons = $state<readonly WidgetId[]>([]);
  let panelSetDisposables: { dispose(): void }[] = [];

  function syncOpenSingletons() {
    const api = dockApi;
    openSingletons = api ? SINGLETON_IDS.filter((w) => api.getPanel(panelIdFor(w))) : [];
  }

  /** A lone panel needs no tab strip; every strip shows again as soon as a
   *  second panel opens. A floating panel always keeps its strip — its tab
   *  is how it is moved and closed. Dockview relayouts a group when its
   *  strip hides, and saves the state in the layout. */
  function syncTabStrips() {
    const api = dockApi;
    if (!api) return;
    const lone = api.panels.length <= 1;
    for (const group of api.groups) {
      const hide = lone && group.api.location.type === "grid";
      if (group.header.hidden !== hide) group.header.hidden = hide;
    }
  }

  function syncPanelSet() {
    syncOpenSingletons();
    syncTabStrips();
  }

  /** Review / Code: open the panel; bring it forward if it is open behind
   *  another tab; close it if it is showing. Closing loses nothing — the
   *  panel is a view onto state that outlives it. */
  function toggleSingleton(widget: WidgetId) {
    const panel = dockApi?.getPanel(panelIdFor(widget));
    if (!panel) addSingleton(widget);
    else if (panel.api.isVisible) dockApi!.removePanel(panel);
    else panel.api.setActive();
  }

  function addSingleton(widget: WidgetId) {
    if (!dockApi) return;
    const id = panelIdFor(widget);
    const existing = dockApi.getPanel(id);
    if (existing) {
      existing.api.setActive();
      addMenuOpen = false;
      return;
    }
    dockApi.addPanel(
      addPanelOpts(widget, {
        id,
        title: WIDGETS.find((w) => w.id === widget)?.label ?? widget,
        position: singletonPosition(widget),
      }),
    );
    addMenuOpen = false;
  }

  /** The change Review should show next — "Open in Review" on a change in
   *  the chat, or a `&change=` link. `seq` makes a repeat click count after
   *  the operator has moved Review's focus elsewhere. */
  let reviewFocus = $state<{ hash: string; seq: number } | null>(null);
  let reviewFocusSeq = 0;
  function openReviewOn(hash?: string) {
    if (hash) reviewFocus = { hash, seq: ++reviewFocusSeq };
    addSingleton("review");
  }

  /** Open (or focus, if already open) one configured agent's chat pane.
   *  `agentId` is the server-projected `AgentInfo.agent_id` — callers never
   *  reconstruct it (07d-A1 §4). `label` is only used the first time this
   *  id is created — reopening an existing panel via `.setActive()` never
   *  touches its title. */
  function addChatPanel(agentId: string, roleId: string, label: string) {
    if (!dockApi) return;
    const id = chatPanelId(roleId);
    const existing = dockApi.getPanel(id);
    if (existing) {
      existing.api.setActive();
      addMenuOpen = false;
      return;
    }
    dockApi.addPanel(
      addPanelOpts("chat", {
        id,
        title: label,
        params: { agentId, roleId },
        position: chatPosition(),
      }),
    );
    addMenuOpen = false;
  }

  /** Opens a teammate's conversation beside this one — another socket or
   *  task in the same project, or another Baker thread — so many agents can
   *  work side by side in one studio. */
  function addTeammatePanel(c: Conversation, agentId: string) {
    if (!dockApi) return;
    const id = `chat:${c.roleId}@${c.streamId}`;
    const existing = dockApi.getPanel(id);
    if (existing) {
      existing.api.setActive();
      addMenuOpen = false;
      return;
    }
    const active = dockApi.activePanel;
    dockApi.addPanel(
      addPanelOpts("chat", {
        id,
        title: c.title,
        params: { agentId, roleId: c.roleId, streamId: c.streamId },
        position: active ? { referencePanel: active.id, direction: "right" as const } : undefined,
      }),
    );
    addMenuOpen = false;
  }

  /** The Agent map's click: that agent's conversation opens beside this one
   *  — for a subagent, its parent agent's, with the subagent's card open. */
  function openFromMap(n: AgentMapNode) {
    if (n.tool_call_id) revealSubagent(n.tool_call_id);
    if (n.stream_id === workbench.streamId) {
      const agentId = agentIdFor(n.seat);
      const label = workbench.socket?.agents.find((a) => a.role_id === n.seat)?.role_label ?? fallbackRoleLabel(n.seat);
      if (agentId) addChatPanel(agentId, n.seat, label);
      return;
    }
    const c = conversationFor(studioStore.data, n.stream_id);
    const agentId = studioStore.findSocket(n.stream_id)?.agents.find((a) => a.role_id === n.seat)?.agent_id;
    if (c && agentId) addTeammatePanel(c, agentId);
  }

  /** Lifts the active panel out of the grid into a floating window inside
   *  the studio; drag its tab back into the grid to dock it again. */
  function floatActivePanel() {
    const panel = dockApi?.activePanel;
    if (panel) dockApi!.addFloatingGroup(panel);
    addMenuOpen = false;
  }

  function addTerminalPanel(info: TerminalInfo) {
    if (!dockApi) return;
    const id = terminalPanelId(info.terminal_id);
    const existing = dockApi.getPanel(id);
    if (existing) {
      existing.api.setActive();
      addMenuOpen = false;
      return;
    }
    dockApi.addPanel(
      addPanelOpts("terminal", {
        id,
        title: info.name,
        params: {
          streamId: info.stream_id,
          terminalId: info.terminal_id,
          name: info.name,
          readOnly: info.mode === "agent",
        },
        position: terminalPosition(),
      }),
    );
    addMenuOpen = false;
  }

  async function createTerminal(mode: TerminalMode) {
    const sid = workbench.streamId;
    if (!sid) return;
    try {
      const info = await node.terminalCreate(sid, mode);
      addTerminalPanel(info);
    } catch (e) {
      workbench.notice = `${e}`;
    }
    void refreshOpenTerminals();
  }

  async function refreshOpenTerminals() {
    const sid = workbench.streamId;
    if (!sid) {
      openTerminals = [];
      return;
    }
    try {
      openTerminals = await node.terminalList(sid);
    } catch {
      /* node down — leave the last known list */
    }
  }

  // The Agent terminal has its own menu row, "Agent commands".
  const hiddenTerminals = $derived(
    openTerminals.filter((t) => t.live && t.mode !== "agent" && !dockApi?.getPanel(terminalPanelId(t.terminal_id))),
  );
  // `dockApi` itself is a plain variable, not `$state` — Svelte's fine-grained
  // reactivity has nothing to track on `dockApi.getPanel(...)` calls, so a
  // `$derived` that only reads those never recomputes just because a panel
  // was added/removed (dockview's own tab close button, "Close terminal",
  // `addChatPanel`, ...). Reading `addMenuOpen` here forces a fresh read of
  // the CURRENT panel set at the one moment it's actually displayed — the
  // instant this menu opens — the same "recompute exactly when the human is
  // about to look at this list" fix `refreshOpenTerminals`'s own
  // `addMenuOpen` effect already uses for `hiddenTerminals`, just without
  // needing a network round-trip since dockApi's panel set is already local.
  // Missing this is why a closed Orchestrator (or any singleton) could stay
  // invisible in "+ Panel" forever — 07d-A0 bug report.
  const missingSingletons = $derived.by(() => {
    addMenuOpen;
    // 07d-A3 §1: a chat-only socket (no root yet) has no worktree to browse
    // or edit — Code stays off the menu until a root attaches. The other
    // singletons (Review, Memory, Events) are Heart projections, not
    // filesystem-backed, so they stay available either way.
    const hasRoot = !!workbench.socket?.worktree_path;
    const offered = hasRoot ? SINGLETON_IDS : SINGLETON_IDS.filter((w) => w !== "code");
    return dockApi ? offered.filter((w) => !dockApi!.getPanel(panelIdFor(w))) : offered;
  });
  /** Every other agent this studio can open beside its own: the other
   *  sockets and tasks of the same project, or the other Baker threads. */
  const teammates = $derived.by(() => {
    addMenuOpen;
    const sid = workbench.streamId;
    const data = studioStore.data;
    if (!sid || !data) return [];
    const group = data.projects.find((p) => p.sockets.some((s) => s.stream_id === sid));
    const members = group ? new Set(group.sockets.map((s) => s.stream_id)) : null;
    const factory = data.factory_sockets.some((s) => s.stream_id === sid);
    return allConversations(data)
      .filter((c) => c.streamId !== sid && !c.archived && (members ? members.has(c.streamId) : factory && c.bakerThread))
      .map((c) => ({
        c,
        agentId: studioStore.findSocket(c.streamId)?.agents.find((a) => a.role_id === c.roleId)?.agent_id ?? "",
      }))
      .filter((t) => t.agentId && !dockApi?.getPanel(`chat:${t.c.roleId}@${t.c.streamId}`));
  });

  /** This socket's agent rows from `/studio/state` — already scoped to the
   *  roles valid for this socket's kind by the server (07d-A1 §4: a project
   *  socket never carries baker, the factory socket carries baker alone).
   *  Empty until studio state has loaded once. */
  const socketAgents = $derived.by(() => workbench.socket?.agents ?? []);
  /** Configured (assigned) agents not currently open as a pane — real
   *  conversations to (re)open (07d-A1 §4: "Conversation menus show only
   *  agents with an assignment"). */
  const missingConversations = $derived.by(() => {
    addMenuOpen;
    const assigned = socketAgents.filter((a) => a.assigned_model_id != null);
    return dockApi ? assigned.filter((a) => !dockApi!.getPanel(chatPanelId(a.role_id))) : assigned;
  });
  /** Valid roles on this socket with no assignment yet — a separate "Add
   *  agent" action, not a conversation to reopen (07d-A1 §4: "Unassigned
   *  valid project roles live under a separate Add agent action"). Opening
   *  one shows its configure card; it becomes a real conversation the
   *  instant a model is assigned. */
  const addableAgents = $derived.by(() => {
    addMenuOpen;
    return socketAgents.filter((a) => a.assigned_model_id == null);
  });
  const isBurn = $derived(workbench.socket?.lifetime === "burn");

  // ---- default layout: chat-alone first use (07c-UX-SHELL §2.1/§2.2) ----
  // The configured project-agent conversation is the whole dominant surface
  // on first paint. Code, Terminal, Review, Memory, and Events open on
  // demand via the "+ Panel" menu — never auto-added here. 07d-A: the
  // default agent's role is `baker` for a factory socket, else
  // `orchestrator` (mirrors the server's own `default_seat_for`).
  function applyDefaultLayout(k: "factory" | "project" | null) {
    if (!dockApi || !workbench.streamId) return;
    const roleId = k === "factory" ? "baker" : "orchestrator";
    // Only ever called once `bootstrapStatus === "ready"` (07d-A1
    // correction — see the effect below); a `null` here would mean this
    // socket's own default role has no `AgentInfo` row at all, which the
    // server should never produce.
    const agentId = agentIdFor(roleId);
    if (agentId) addChatPanel(agentId, roleId, fallbackRoleLabel(roleId));
  }

  /** Visible escape hatch (07d-A0) alongside the automatic layout-key
   *  version bump — clears just this socket's saved arrangement and
   *  re-applies the default layout immediately, so a human stuck with a
   *  broken or blank pane never has to know this is a localStorage
   *  problem to fix by hand. */
  function resetLayout() {
    if (!dockApi || !workbench.streamId) return;
    resetDockLayout(workbench.streamId);
    clearDock();
    applyDefaultLayout(kind);
    addMenuOpen = false;
  }

  /** After a layout restore, drop any terminal panel whose session no
   *  longer exists (killed, or the node restarted — 07c §3: node-restart
   *  survival is not required this slice). Never drops other panel types.
   *
   *  `generation` guards against a stale call clobbering a newer stream's
   *  panels: this function awaits a network round-trip, and if the user
   *  switches streams while it's in flight, `dockApi` by then holds the
   *  *new* stream's panels — re-reading `workbench.streamId` after the await
   *  would silently remove them (their ids are never in the old stream's
   *  live set) and stomp `openTerminals` with the old stream's list. */
  async function reconcileTerminals(sid: string, generation: number) {
    if (!dockApi) return;
    let live: TerminalInfo[];
    try {
      live = await node.terminalList(sid);
    } catch {
      return; // node unreachable — leave the layout as restored
    }
    if (!dockApi || generation !== loadGeneration) return; // superseded by a newer stream switch
    const liveIds = new Set(live.filter((t) => t.live).map((t) => t.terminal_id));
    for (const panel of [...dockApi.panels]) {
      if (!panel.id.startsWith("terminal:")) continue;
      const terminalId = panel.id.slice("terminal:".length);
      if (!liveIds.has(terminalId)) dockApi.removePanel(panel);
    }
    openTerminals = live;
  }

  function schedulePersist() {
    const sid = boundStreamId;
    const api = dockApi;
    const generation = loadGeneration;
    if (!sid || !api || persistSuppressedGeneration !== null) return;
    const snapshot = api.toJSON();
    clearTimeout(saveTimer);
    saveTimer = setTimeout(() => {
      if (dockApi === api && boundStreamId === sid && loadGeneration === generation) {
        saveDockLayout(sid, snapshot);
      }
    }, 300);
  }

  function persistBoundLayoutNow() {
    if (!boundStreamId || !dockApi || persistSuppressedGeneration !== null) return;
    clearTimeout(saveTimer);
    saveDockLayout(boundStreamId, dockApi.toJSON());
  }

  /** "Closing restores/focuses the previous pane" (non-destructive-panels
   *  follow-up) — and must never steal focus when closing a background
   *  panel instead. Wired once per `dockApi` instance from `buildDock`. */
  function wireFocusRestore(api: DockviewApi) {
    const onActive = api.onDidActivePanelChange((e) => {
      if (!e.panel) return;
      const id = e.panel.id;
      activePanelHistory = [id, ...activePanelHistory.filter((x) => x !== id)].slice(0, 20);
    });
    const onWillMutate = api.onWillMutateLayout((e) => {
      preRemoveActiveId = e.kind === "remove" ? (api.activePanel?.id ?? null) : null;
    });
    const onRemove = api.onDidRemovePanel((panel) => {
      activePanelHistory = activePanelHistory.filter((id) => id !== panel.id);
      const wasActive = preRemoveActiveId === panel.id;
      preRemoveActiveId = null;
      schedulePersist(); // safety net alongside onDidLayoutChange below
      if (!wasActive) return; // a background panel closed — leave focus alone
      for (const id of activePanelHistory) {
        const candidate = api.getPanel(id);
        if (candidate) {
          candidate.api.setActive();
          return;
        }
      }
      // No known previous panel still exists — Dockview's own post-removal
      // default stands.
    });
    focusDisposables = [onActive, onWillMutate, onRemove];
  }

  /** Every `dockApi.clear()` call site funnels through here — clearing the
   *  layout without also resetting the focus-history state would let a
   *  stale `activePanelHistory` entry survive into a DIFFERENT stream's
   *  same-named singleton panels (see the field doc above). */
  function clearDock() {
    dockApi?.clear();
    activePanelHistory = [];
    preRemoveActiveId = null;
    syncOpenSingletons();
  }

  function buildDock(el: HTMLDivElement) {
    dockApi = createDockview(el, {
      createComponent,
      theme: themeAbyss,
    });
    layoutDisposable = dockApi.onDidLayoutChange(schedulePersist);
    wireFocusRestore(dockApi);
    panelSetDisposables = [
      dockApi.onDidAddPanel(syncPanelSet),
      // Deferred: read the panel set once the removal has fully landed.
      dockApi.onDidRemovePanel(() => queueMicrotask(syncPanelSet)),
      dockApi.onDidLayoutFromJSON(syncPanelSet),
      // A panel floated, docked again, or dragged into a group of its own.
      dockApi.onDidMovePanel(syncTabStrips),
      dockApi.onDidAddGroup(syncTabStrips),
    ];
    dockApiReady = true;
  }

  function teardownDock() {
    controlsStreamId = null;
    clearTimeout(saveTimer);
    layoutDisposable?.dispose();
    layoutDisposable = undefined;
    focusDisposables.forEach((d) => d.dispose());
    focusDisposables = [];
    panelSetDisposables.forEach((d) => d.dispose());
    panelSetDisposables = [];
    clearDock();
    (dockApi as unknown as { dispose?: () => void } | undefined)?.dispose?.();
    dockApi = undefined;
    dockApiReady = false;
  }

  async function loadForStream(sid: string, generation: number) {
    if (!dockApi) return;
    try {
      const saved = loadDockLayout(sid);
      if (saved) {
        try {
          dockApi.fromJSON(saved as unknown as SerializedDockview);
          normalizeRenderers();
        } catch {
          clearDock();
          applyDefaultLayout(kind);
        }
      } else {
        applyDefaultLayout(kind);
      }
      // Deep-link seam (07d-B Phase 4 item 3): applied once per fresh bind,
      // after whatever layout (saved or default) is already in place — never
      // overrides a restored layout's own arrangement, just focuses/opens the
      // requested singleton on top of it. Re-checks `generation` the same way
      // `reconcileTerminals` below does: if the human already navigated to a
      // different stream while `loadDockLayout`/`applyDefaultLayout` ran
      // (synchronous today, but this guards future async layout sources too),
      // a stale `openPanel` must never reach for the CURRENT dockApi state.
      if (openPanel && generation === loadGeneration) addSingleton(openPanel);
      if (generation === loadGeneration) controlsStreamId = sid;
    } finally {
      if (persistSuppressedGeneration === generation) {
        persistSuppressedGeneration = null;
        schedulePersist();
      }
    }
    await reconcileTerminals(sid, generation);
  }

  $effect(() => {
    if (!hostEl) return;
    if (!dockApi) buildDock(hostEl);
  });

  $effect(() => {
    const sid = workbench.streamId;
    // `dockApiReady` (not `dockApi` itself — see its declaration) is what
    // actually wakes this effect once `buildDock` has run; `.dock-host`
    // (and so `dockApi`) only exists once `bootstrapStatus === "ready"`
    // below, which can land in a later reactive flush than this one.
    if (!sid || !dockApiReady || !dockApi || sid === boundStreamId) return;
    // Every path `loadForStream` can take — a fresh default pane, or
    // corrupted-layout / unknown-component recovery falling back to one —
    // needs this socket's real kind and agent rows to pick the right role
    // and its real agent_id (07d-A1 correction: never invent
    // `${streamId}:${roleId}`, and never guess a default pane on a failed
    // fetch either). Wait for `bootstrapStatus` to resolve; if it settles
    // "unavailable" this never binds at all — the template below shows a
    // recoverable surface instead, and a `studioStore.refresh()` retry
    // re-triggers this effect the moment status changes.
    if (bootstrapStatus !== "ready") return;
    persistBoundLayoutNow();
    const generation = ++loadGeneration;
    persistSuppressedGeneration = generation;
    boundStreamId = sid;
    clearDock();
    void loadForStream(sid, generation);
  });

  // Deep-link seam, continued (07d-B fix): the bind effect above — and so
  // `loadForStream`'s own `addSingleton(openPanel)` call — only runs on a
  // FRESH bind, deliberately skipping when `sid === boundStreamId` (a human
  // already parked on this stream's workbench). That means navigating from
  // `/w/X` to `/w/X?open=review` (the Factory Floor's own "review →" link,
  // clicked while already on that task's workbench — or a second tab
  // restoring that URL) changed nothing `loadForStream` reacts to: the URL
  // changed, the bound stream did not, so the requested panel never opened.
  // This effect is keyed on `openPanel` itself (plus the dock actually being
  // bound to the right stream) so a change in the REQUESTED panel opens it
  // independent of whether a bind just happened. `addSingleton` is
  // idempotent (focuses an already-open panel), so overlapping with the
  // fresh-bind path above is harmless. A `&change=` link focuses its change
  // once; a later re-run must not undo a focus the operator picked since.
  let linkedChange: string | null = null;
  $effect(() => {
    const panel = openPanel;
    const change = focusChange;
    const sid = workbench.streamId;
    if (!panel || !dockApiReady || !dockApi || sid !== boundStreamId) return;
    if (panel === "review" && change && change !== linkedChange) {
      linkedChange = change;
      openReviewOn(change);
      return;
    }
    addSingleton(panel);
  });

  $effect(() => {
    // Re-run whenever the add-panel menu opens, so the "reopen" list is fresh.
    if (addMenuOpen) void refreshOpenTerminals();
  });

  // A task that reaches sign-off opens Review once — signing off happens
  // there. A pending change does not: it is decided in the chat, and Review
  // opens only when asked ("Open in Review", the toolbar, a link). Never
  // replace an existing pane or steal focus.
  let attentionStream: string | null = null;
  let seenSignoffs = new Set<string>();
  $effect(() => {
    const sid = workbench.streamId;
    if (!sid || !dockApiReady || !dockApi || sid !== boundStreamId) return;
    if (attentionStream !== sid) {
      attentionStream = sid;
      seenSignoffs = new Set();
    }
    if (workbench.socket?.state === "task" && workbench.socket.project_id) {
      taskStore.ensureProject(workbench.socket.project_id);
    }
    const task = taskStore.forStream(sid);
    if (task?.status !== "awaiting_review" || seenSignoffs.has(task.task_id)) return;
    seenSignoffs.add(task.task_id);
    if (!dockApi.getPanel("review")) {
      const active = dockApi.activePanel;
      addSingleton("review");
      active?.api.setActive();
    }
  });

  $effect(() => {
    return () => teardownDock();
  });
</script>

{#snippet dockTools()}
  <div class="quick-panels" aria-label="Workspace tools">
    <button
      type="button"
      class="quick-panel-btn"
      disabled={bootstrapStatus !== "ready" || controlsStreamId !== workbench.streamId}
      aria-label="Review panel"
      aria-pressed={openSingletons.includes("review")}
      title="Review"
      onclick={() => toggleSingleton("review")}
    >
      <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M3 2.5h10v11H3zM5.5 5h5M5.5 8h5M5.5 11h3" /></svg>
    </button>
    {#if workbench.socket?.worktree_path}
      <button
        type="button"
        class="quick-panel-btn"
        disabled={bootstrapStatus !== "ready" || controlsStreamId !== workbench.streamId}
        aria-label="Code panel"
        aria-pressed={openSingletons.includes("code")}
        title="Code"
        onclick={() => toggleSingleton("code")}
      >
        <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M2.5 4.5h4l1-1.5h6v10h-11z" /></svg>
      </button>
    {/if}
    <button
      type="button"
      class="quick-panel-btn"
      disabled={bootstrapStatus !== "ready" || controlsStreamId !== workbench.streamId}
      aria-label="Open new terminal"
      title="Terminal"
      onclick={() => void createTerminal(defaultTerminalMode(isBurn))}
    >
      <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M2.5 3.5h11v9h-11zM4.5 6l2 2-2 2M8 10h3" /></svg>
    </button>
  </div>
  <div class="add-menu">
      <button type="button" class="add-btn" disabled={bootstrapStatus !== "ready" || controlsStreamId !== workbench.streamId} onclick={() => { addMenuOpen = !addMenuOpen; }} aria-expanded={addMenuOpen}>
        + Panel
      </button>
      {#if addMenuOpen}
        <!-- svelte-ignore a11y_click_events_have_key_events -->
        <!-- svelte-ignore a11y_no_static_element_interactions -->
        <div class="menu-backdrop" onclick={() => { addMenuOpen = false; }}></div>
        <div class="menu">
          {#if missingConversations.length > 0}
            <div class="menu-section">Conversations</div>
            {#each missingConversations as a (a.agent_id)}
              <button
                type="button"
                class="menu-row"
                onclick={() => addChatPanel(a.agent_id, a.role_id, a.role_label)}
              >
                {a.role_label}
              </button>
            {/each}
          {/if}
          {#if addableAgents.length > 0}
            <div class="menu-section">Add agent</div>
            {#each addableAgents as a (a.agent_id)}
              <button
                type="button"
                class="menu-row"
                aria-expanded={addAgentExpandedId === a.agent_id}
                onclick={() => toggleAddAgent(a.agent_id)}
              >
                {a.role_label}
              </button>
              {#if addAgentExpandedId === a.agent_id}
                <div class="add-agent-inline">
                  <select class="model-select" bind:value={addAgentModel}>
                    <option value="">— pick model —</option>
                    {#each deriveAvailableModels(workbench.socket, workbench.modelOptions) as opt}
                      <option value={opt.id}>{opt.label}</option>
                    {/each}
                  </select>
                  <button
                    type="button"
                    class="assign-btn"
                    disabled={!addAgentModel.trim()}
                    onclick={() => assignAndOpen(a.agent_id, a.role_id, a.role_label)}
                  >assign</button>
                </div>
                {#if addAgentMsg}
                  <div class="add-agent-msg">{addAgentMsg}</div>
                {/if}
              {/if}
            {/each}
          {/if}
          {#if teammates.length > 0}
            <div class="menu-section">{workbench.groupKind === "factory" ? "Other Baker threads" : "Other agents in this project"}</div>
            {#each teammates as t (t.c.streamId)}
              <button
                type="button"
                class="menu-row"
                title={t.c.note || statusLabel(t.c.status)}
                onclick={() => addTeammatePanel(t.c, t.agentId)}
              >
                {t.c.title} · {statusLabel(t.c.status)}
              </button>
            {/each}
          {/if}
          {#if missingSingletons.length > 0}
            <div class="menu-section">Panels</div>
            {#each missingSingletons as w (w)}
              <button type="button" class="menu-row" onclick={() => addSingleton(w)}>
                {WIDGETS.find((x) => x.id === w)?.label ?? w}
              </button>
            {/each}
          {/if}
          <div class="menu-section">Terminals</div>
          <button type="button" class="menu-row" onclick={() => createTerminal("host")} disabled={!workbench.streamId || !canPickMode("host", isBurn)}>
            New host terminal
          </button>
          <button type="button" class="menu-row" onclick={() => createTerminal("room")} disabled={!workbench.streamId}>
            New room terminal
          </button>
          <button
            type="button"
            class="menu-row"
            title="Every command an agent runs here, with its output and exit code (read-only)"
            onclick={() => createTerminal("agent")}
            disabled={!workbench.streamId}
          >
            Agent commands
          </button>
          {#if hiddenTerminals.length > 0}
            <div class="menu-section">Reopen</div>
            {#each hiddenTerminals as t (t.terminal_id)}
              <button type="button" class="menu-row" onclick={() => addTerminalPanel(t)}>{t.name}</button>
            {/each}
          {/if}
          <div class="menu-section">Window</div>
          <button type="button" class="menu-row" onclick={floatActivePanel}>
            Float the active panel
          </button>
          <button
            type="button"
            class="menu-row"
            title="Discard this socket's saved arrangement and restore the default layout"
            onclick={resetLayout}
          >Reset layout</button>
        </div>
      {/if}
  </div>
{/snippet}

<div class="dock-wrap">
  {#if bootstrapStatus === "ready"}
    <div class="dock-host" bind:this={hostEl}></div>
  {:else}
    <!-- 07d-A1 correction: never bootstrap a guessed default pane while
         this socket's real kind/agent rows are unknown — show a
         recoverable surface instead of an invented Orchestrator. -->
    <div class="dock-status">
      {#if bootstrapStatus === "loading"}
        <p>Loading session…</p>
      {:else}
        <p>Could not load this session's agents.</p>
        <button type="button" onclick={() => studioStore.refresh()}>Retry</button>
      {/if}
    </div>
  {/if}
</div>

<style>
  .dock-wrap {
    height: 100%;
    display: flex;
    flex-direction: column;
    overflow: hidden;
    background: var(--bg-app);
  }

  /* The controls below render in the title bar (`dockTools`). */
  .add-menu {
    position: relative;
  }

  .quick-panels {
    display: flex;
    align-items: center;
    gap: 2px;
    min-width: 0;
  }

  .quick-panel-btn {
    width: 28px;
    height: 28px;
    display: grid;
    place-items: center;
    padding: 0;
    border: 1px solid transparent;
    border-radius: 7px;
    background: transparent;
    color: var(--fg-faint);
    cursor: pointer;
  }
  .quick-panel-btn:hover:not(:disabled) {
    color: var(--fg-2);
    background: var(--bg-chip);
    border-color: var(--line-soft);
  }
  /* Review / Code while open: pressed, and a click closes them. */
  .quick-panel-btn[aria-pressed="true"] {
    color: var(--accent-fg);
    background: var(--accent-bg);
    border-color: var(--accent-border);
  }
  .quick-panel-btn:disabled { opacity: 0.4; cursor: default; }
  .quick-panel-btn:focus-visible {
    outline: 2px solid var(--accent-border);
    outline-offset: 1px;
  }
  .quick-panel-btn svg {
    width: 14px;
    height: 14px;
    fill: none;
    stroke: currentColor;
    stroke-width: 1.3;
    stroke-linecap: round;
    stroke-linejoin: round;
  }

  .add-btn {
    height: 26px;
    font: 600 11px/1 var(--font-ui);
    color: var(--fg-2);
    background: var(--bg-panel);
    border: 1px solid var(--line-soft);
    border-radius: 8px;
    padding: 0 9px;
    white-space: nowrap;
    cursor: pointer;
  }
  .add-btn:hover:not(:disabled) { background: var(--bg-chip); border-color: var(--line-strong); }
  .add-btn:disabled { opacity: 0.4; cursor: default; }
  .add-btn:focus-visible {
    outline: 2px solid var(--accent-border);
    outline-offset: 1px;
  }

  .menu-backdrop {
    position: fixed;
    inset: 0;
    z-index: 40;
  }

  /* The button sits at the left of the title bar: open rightward. */
  .menu {
    position: absolute;
    top: 100%;
    left: 0;
    margin-top: 6px;
    min-width: 208px;
    max-height: calc(100dvh - 60px);
    overflow-y: auto;
    background: var(--bg-raised);
    border: 1px solid var(--line-strong);
    border-radius: 12px;
    box-shadow: var(--shadow-menu);
    z-index: 41;
    padding: 6px;
    display: flex;
    flex-direction: column;
  }

  .menu-section {
    font: 600 9.5px/1.6 var(--font-ui);
    color: var(--fg-fainter);
    text-transform: uppercase;
    letter-spacing: 0.03em;
    padding: 4px 8px 2px;
  }

  .menu-row {
    text-align: left;
    background: none;
    border: none;
    color: var(--fg-2);
    font: 400 11.5px/1.4 var(--font-ui);
    padding: 7px 9px;
    border-radius: 8px;
    cursor: pointer;
  }
  .menu-row:hover:not(:disabled) { background: var(--bg-chip); }
  .menu-row:disabled { opacity: 0.5; cursor: default; }

  .add-agent-inline {
    display: flex;
    align-items: center;
    gap: 5px;
    padding: 4px 8px 6px;
  }
  .add-agent-inline .model-select {
    flex: 1 1 auto;
    min-width: 0;
    background: var(--bg-panel);
    border: 1px solid var(--line);
    border-radius: var(--r-sm);
    padding: 3px 6px;
    font: 400 11px/1 var(--font-mono);
    color: var(--fg-2);
    outline: none;
  }
  .add-agent-inline .assign-btn {
    flex-shrink: 0;
    padding: 3px 9px;
    border-radius: var(--r-sm);
    background: var(--bg-chip);
    border: 1px solid var(--line);
    color: var(--fg-dim);
    font: 400 10.5px/1 var(--font-mono);
    cursor: pointer;
  }
  .add-agent-inline .assign-btn:hover:not(:disabled) { background: var(--accent-bg); color: var(--accent-fg); border-color: var(--accent-border); }
  .add-agent-inline .assign-btn:disabled { opacity: 0.4; cursor: default; }

  .add-agent-msg {
    font: 400 10.5px/1.3 var(--font-mono);
    color: var(--fg-faint);
    padding: 0 8px 6px;
  }

  .dock-host {
    flex: 1 1 auto;
    min-height: 0;
    position: relative;
  }

  .dock-status {
    flex: 1 1 auto;
    min-height: 0;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 8px;
    color: var(--fg-faint);
    font: 400 12px/1.4 var(--font-ui);
  }
  .dock-status button {
    padding: 4px 10px;
    border-radius: var(--r-xl);
    background: var(--bg-chip);
    border: 1px solid var(--line);
    color: var(--fg-dim);
    font: 400 11px/1 var(--font-ui);
    cursor: pointer;
  }
  .dock-status button:hover { background: var(--accent-bg); color: var(--accent-fg); border-color: var(--accent-border); }

  /* Bakery palette over the Abyss base theme. Dockview 8 names the theme
     class `dockview-theme-abyss`; the old `.dv-theme-abyss` selector matched
     nothing, which is why the tab strip showed Abyss's own navy. Every color
     here is an Appearance token, so custom schemes flow through too. */
  .dock-host :global(.dockview-theme-abyss) {
    --dv-color-abyss-dark: var(--bg-app);
    --dv-color-abyss: var(--bg-panel);
    --dv-color-abyss-light: var(--bg-app);
    --dv-color-abyss-lighter: var(--line-chrome);
    --dv-color-abyss-accent: var(--accent);
    --dv-color-abyss-primary-text: var(--fg);
    --dv-color-abyss-secondary-text: var(--fg-dim);
    --dv-tabs-and-actions-container-height: 30px;
    --dv-tabs-and-actions-container-font-size: 11.5px;
    --dv-group-view-background-color: var(--bg-app);
    --dv-tabs-and-actions-container-background-color: var(--bg-app);
    --dv-activegroup-visiblepanel-tab-background-color: var(--bg-panel);
    --dv-activegroup-hiddenpanel-tab-background-color: transparent;
    --dv-inactivegroup-visiblepanel-tab-background-color: var(--bg-chip);
    --dv-inactivegroup-hiddenpanel-tab-background-color: transparent;
    --dv-tab-divider-color: transparent;
    --dv-activegroup-visiblepanel-tab-color: var(--fg);
    --dv-activegroup-hiddenpanel-tab-color: var(--fg-faint);
    --dv-inactivegroup-visiblepanel-tab-color: var(--fg-2);
    --dv-inactivegroup-hiddenpanel-tab-color: var(--fg-faint);
    --dv-separator-border: var(--line-chrome);
    --dv-paneview-header-border-color: var(--line-chrome);
    --dv-paneview-active-outline-color: var(--accent);
    --dv-drag-over-background-color: var(--accent-bg);
    --dv-drag-over-border-color: var(--accent);
    --dv-edge-dock-indicator-color: var(--accent);
    --dv-tabs-container-scrollbar-color: var(--line-strong);
    --dv-icon-hover-background-color: var(--bg-chip);
    --dv-sash-color: transparent;
    --dv-active-sash-color: var(--accent);
    --dv-tab-border-radius: 7px 7px 0 0;
  }

  .dock-host :global(.dv-tabs-and-actions-container) {
    border-bottom: 1px solid var(--line-chrome);
    padding-inline: 4px;
  }
  /* The active tab of the focused group carries the accent, like the
     sidebar's selected row. */
  .dock-host :global(.dv-groupview.dv-active-group .dv-tab.dv-active-tab) {
    box-shadow: inset 0 -2px 0 var(--accent);
  }
  /* In-place (non-overlay) panels fill exactly the content area below the
     tab strip — our content element is `position: absolute; inset: 0`. */
  .dock-host :global(.dv-content-container) {
    position: relative;
  }
  .dock-host :global(.dv-default-tab) {
    padding-inline: 10px;
    font-family: var(--font-ui);
    font-weight: 520;
  }
  .dock-host :global(.dv-sash) {
    transition: background-color 120ms ease-out;
  }

  @media (max-width: 720px) {
    .menu { max-width: calc(100vw - 24px); }
  }
</style>
