// One React root per conversation panel, and the explicit teardown for it.
//
// This is the whole surface where React touches the app. Dockview owns the
// element; this owns what React does inside it. Deliberately tiny: the
// mixed-framework boundary is temporary migration infrastructure, and the
// smaller it is, the cheaper the eventual extraction.

import { createRoot } from "react-dom/client";
import { reportError } from "$lib/clientLog";
import type { AgentPanelSource } from "./bridge.svelte";
import { AgentMap, type AgentMapProps } from "./AgentMap";
import { AgentPanel } from "./AgentPanel";
import { ChatStart } from "./ChatStart";
import type { ChatStartStore } from "./chatStartSource.svelte";
import { Shell } from "./shell/Shell";
import type { ShellStore } from "./shell/shellSource.svelte";
import { MachinesSettings } from "./shell/SidebarParts";
import { WorkBoard } from "./WorkBoard";
import type { WorkBoardStore } from "./workBoardSource.svelte";

/** React errors would otherwise reach only a console nobody sees in the
 *  deployed app; they go to the node log as well (clientLog.ts). */
const ROOT_OPTIONS = {
  onUncaughtError: (error: unknown) => {
    console.error(error);
    reportError("react", error);
  },
  onRecoverableError: (error: unknown) => reportError("react-recoverable", error),
};

/** Renders the panel into `target` and returns its teardown.
 *
 *  The returned function releases the React root and the snapshot
 *  subscription. It does NOT touch the `AgentConversation` — closing a pane
 *  must never stop a running turn, and the conversation outlives every panel
 *  bound to it (see `WorkbenchState.conversations`). */
export function mountAgentPanel(target: HTMLElement, source: AgentPanelSource): () => void {
  const root = createRoot(target, ROOT_OPTIONS);
  root.render(<AgentPanel store={source} />);

  let torn = false;
  return () => {
    if (torn) return;
    torn = true;
    // Dockview disposes from its own event handling, never from inside a
    // React render or commit, so unmounting synchronously here is safe.
    root.unmount();
    source.dispose();
  };
}

/** Renders the work board into `target` and returns its teardown, which also
 *  releases the board's snapshot subscription. */
export function mountWorkBoard(target: HTMLElement, store: WorkBoardStore, emptyText: string): () => void {
  const root = createRoot(target, ROOT_OPTIONS);
  root.render(<WorkBoard store={store} emptyText={emptyText} />);
  let torn = false;
  return () => {
    if (torn) return;
    torn = true;
    root.unmount();
    store.dispose();
  };
}

/** Renders the Agent map into `target` and returns its teardown. */
export function mountAgentMap(target: HTMLElement, props: AgentMapProps): () => void {
  const root = createRoot(target, ROOT_OPTIONS);
  root.render(<AgentMap {...props} />);
  let torn = false;
  return () => {
    if (torn) return;
    torn = true;
    root.unmount();
  };
}

/** Renders the Chat home's composer into `target` and returns its teardown,
 *  which also releases the composer's snapshot subscription. */
export function mountChatStart(target: HTMLElement, store: ChatStartStore): () => void {
  const root = createRoot(target, ROOT_OPTIONS);
  root.render(<ChatStart store={store} />);
  let torn = false;
  return () => {
    if (torn) return;
    torn = true;
    root.unmount();
    store.dispose();
  };
}

/** Renders the new shell (the whole window) into `target` and returns its
 *  teardown, which also releases the shell's snapshot subscription. */
/** Settings › Machines: the same view as the shell's node box. */
export function mountMachines(target: HTMLElement): () => void {
  const root = createRoot(target, ROOT_OPTIONS);
  root.render(<MachinesSettings />);
  return () => root.unmount();
}

export function mountShell(target: HTMLElement, store: ShellStore): () => void {
  const root = createRoot(target, ROOT_OPTIONS);
  root.render(<Shell store={store} />);
  let torn = false;
  return () => {
    if (torn) return;
    torn = true;
    root.unmount();
    store.dispose();
  };
}
