// The studio's terminal widget (xterm on a node-owned terminal: ticket,
// socket, theme, resize, "Close terminal") mounted from React, so both
// shells run one implementation.

import { mount, unmount } from "svelte";
import TerminalWidget from "$lib/widgets/Terminal.svelte";

export type TerminalProps = {
  streamId: string;
  terminalId: string;
  name: string;
  readOnly: boolean;
  /** The terminal was killed (its own "Close terminal"). */
  onClosed: () => void;
  /** Only the terminal: the panel's list shows its state and Close. */
  bare?: boolean;
  /** The session outlives the view (terminalSession.ts): shown again, it is
   *  the same terminal, nothing rebuilt or replayed. */
  keepAlive?: boolean;
  /** The connection's state as it changes. */
  onStatus?: (status: { live: boolean; text: string }) => void;
};

export function mountTerminal(target: HTMLElement, props: TerminalProps): () => void {
  const widget = mount(TerminalWidget, { target, props });
  return () => void unmount(widget);
}
