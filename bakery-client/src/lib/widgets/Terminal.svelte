<script lang="ts">
  import { node, SYSTEM_ACCOUNTS_STREAM_ID } from "$lib/node";
  import { dropSession, session, type TerminalSession } from "$lib/terminalSession";
  import "@xterm/xterm/css/xterm.css";

  // A real interactive shell in a dock panel — type, enter, see a shell. One
  // panel per node-owned named terminal (07c-STUDIO-SHELL §3): this widget
  // only shows the `terminalId` it's given, it never creates or picks a mode
  // — that happens once, when the panel itself is created (see
  // DockviewHost's "new terminal" action). The terminal itself is the
  // window's session (terminalSession.ts): its xterm, its socket and its
  // reconnects. Closing this panel only ever takes the view away; the
  // process keeps running on the node. "Close terminal" below is the one
  // action that actually kills it — a deliberately separate, explicit
  // control, not a side effect of hiding or removing the panel.

  let {
    streamId,
    terminalId,
    name,
    readOnly = false,
    bare = false,
    keepAlive = false,
    onClosed,
    onExited,
    onStatus,
    closeLabel,
  }: {
    streamId: string;
    terminalId: string;
    name: string;
    /** The stream's Agent terminal: the node writes it, nobody types into
     *  it, and there is no shell to open elsewhere. */
    readOnly?: boolean;
    /** Called once the terminal is confirmed killed, so the host can also
     *  remove this now-pointless panel from the layout. */
    onClosed?: () => void;
    /** Called once, only when the socket closes because the underlying
     *  process exited *on its own* (07d-A3D1's close-reason fix), parsed
     *  from `exited:<code>` / `exited:?` — distinct from `onClosed`, which
     *  fires only after an explicit "Close terminal" click. `code` is
     *  `null` when the reason carried no parseable number. */
    onExited?: (code: number | null) => void;
    /** Only the terminal: the host shows its name, state and Close itself. */
    bare?: boolean;
    /** The session outlives this view: shown again, it is the same terminal,
     *  nothing rebuilt or replayed. Otherwise it goes with the view. */
    keepAlive?: boolean;
    /** The connection's state as it changes, for a host that shows it. */
    onStatus?: (status: { live: boolean; text: string }) => void;
    /** What its kill button says ("Cancel build" for a build's pane). */
    closeLabel?: string;
  } = $props();

  let containerEl = $state<HTMLDivElement | undefined>();
  let connected = $state(false);
  let statusText = $state("connecting…");
  let closing = $state(false);
  /** Its process is over: what's left is dismissing its pane. */
  const ended = $derived(!connected && /^exited:/.test(statusText));
  $effect(() => {
    onStatus?.({ live: connected, text: statusText });
  });
  let ghosttyAvailable = $state(false);
  let ghosttyBusy = $state(false);
  let shown: TerminalSession | null = null;

  // Shows the session for whichever terminal the props name; a view taken
  // away (a hidden tab, a socket switch, a reload) only detaches it.
  $effect(() => {
    const el = containerEl;
    if (!el) return;
    const s = session(streamId, terminalId, readOnly);
    const offStatus = s.onStatus((st) => {
      connected = st.live;
      statusText = st.text;
    });
    const offExit = s.onExit((code) => {
      if (!closing) onExited?.(code);
    });
    s.attach(el);
    shown = s;
    return () => {
      offStatus();
      offExit();
      shown = null;
      if (keepAlive) s.detach(el);
      else dropSession(s.streamId, s.terminalId);
    };
  });

  /** The one action that actually ends the session — kills the process tree
   *  on the node, then tells the host to drop this panel. */
  async function closeTerminal() {
    if (closing) return;
    const over = ended;
    closing = true;
    shown?.setClosing(true);
    try {
      await node.terminalClose(streamId, terminalId);
    } catch (e) {
      // An ended one may be gone from the node already: dismissing it is all that's left.
      if (!over) {
        statusText = `${e}`;
        closing = false;
        shown?.setClosing(false);
        return;
      }
    }
    // Done either way: the button is free again, and the host takes the pane away.
    closing = false;
    statusText = "closed";
    dropSession(streamId, terminalId);
    onClosed?.();
  }

  async function openGhostty() {
    if (!streamId || ghosttyBusy) return;
    ghosttyBusy = true;
    try {
      await node.ghosttyOpen(streamId);
    } catch {
      /* quiet — optional convenience only, never a slice-blocking error */
    } finally {
      ghosttyBusy = false;
    }
  }

  void node
    .ghosttyAvailable()
    .then((r) => {
      ghosttyAvailable = r.available;
    })
    .catch(() => {
      ghosttyAvailable = false;
    });
</script>

<div class="terminal-widget">
  {#if !bare}
  <div class="control-row">
    <span class="term-name">{name}</span>
    <span class="status" class:live={connected}>{statusText}</span>

    {#if ghosttyAvailable && streamId !== SYSTEM_ACCOUNTS_STREAM_ID && !readOnly}
      <button type="button" class="ghostty-btn" disabled={!streamId || ghosttyBusy} onclick={openGhostty}>
        Open in Ghostty
      </button>
    {/if}
    <button type="button" class="close-btn" disabled={closing} onclick={closeTerminal} title="Kill this terminal">
      {closing ? "Closing…" : ended ? "Dismiss" : (closeLabel ?? "Close terminal")}
    </button>
  </div>
  {/if}

  <div class="term-shell" bind:this={containerEl}></div>
</div>

<style>
  .terminal-widget {
    display: flex;
    flex-direction: column;
    height: 100%;
    overflow: hidden;
    background: var(--bg-rail);
  }

  .control-row {
    flex: 0 0 auto;
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 5px 8px;
    background: var(--bg-panel);
    border-bottom: 1px solid var(--line-chrome);
  }

  .term-name {
    font: 600 10.5px/1 var(--font-ui);
    color: var(--fg-2);
    flex-shrink: 0;
  }

  .status {
    flex: 1 1 auto;
    min-width: 0;
    text-align: right;
    font: 400 10.5px/1 var(--font-mono);
    color: var(--fg-faint);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .status.live { color: var(--ok); }

  .ghostty-btn,
  .close-btn {
    flex-shrink: 0;
    font: 600 10.5px/1 var(--font-ui);
    color: var(--fg-2);
    background: var(--bg-raised);
    border: 1px solid var(--line);
    border-radius: var(--r-md);
    padding: 4px 8px;
    cursor: pointer;
  }
  .ghostty-btn:hover:not(:disabled),
  .close-btn:hover:not(:disabled) { background: var(--bg-chip); }
  .ghostty-btn:disabled,
  .close-btn:disabled { opacity: 0.6; cursor: default; }
  .close-btn { color: var(--bad); border-color: var(--bad-border); }

  .term-shell {
    flex: 1 1 auto;
    min-height: 0;
    padding: 4px 6px;
    overflow: hidden;
  }

  .term-shell :global(.xterm) {
    height: 100%;
  }
</style>
