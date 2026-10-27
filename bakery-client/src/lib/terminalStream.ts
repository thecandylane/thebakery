// What a terminal's socket says beside its bytes, and how a view keeps its
// place in them across reconnects. Only reads text, so `node` checks it
// (terminalStream.check.ts); terminalSession.ts uses it.

/** The node's word in a text frame (with `offsets=true`): where the next
 *  byte sits in the terminal's whole output, and how much just before it is
 *  gone. It comes before every replay, and again wherever output the view
 *  was due is no longer kept. */
export type StreamWord = { at: number; dropped: number };

/** The node's word, or null for a text frame that isn't one. */
export function streamWord(text: string): StreamWord | null {
  let v: unknown;
  try {
    v = JSON.parse(text);
  } catch {
    return null;
  }
  if (!v || typeof v !== "object") return null;
  const { at, dropped } = v as Record<string, unknown>;
  return typeof at === "number" && typeof dropped === "number" ? { at, dropped } : null;
}

/** A byte count as a person reads it. */
export function sizeText(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

/** The dim line a terminal shows where output is gone: never a silent gap. */
export function droppedLine(bytes: number): string {
  return `\r\n\x1b[2m[bakery: ${sizeText(bytes)} of output dropped here]\x1b[22m\r\n`;
}

/** How long to wait before the `n`th reconnect in a row (0 the first):
 *  quick at first, then every 8 seconds for as long as it takes. */
export function retryDelay(n: number): number {
  return Math.min(8000, 250 * 2 ** Math.max(0, n));
}

/** The exit code a socket's close reason names (`exited:<code>`, `null` when
 *  the code is unknown), or undefined when the process did not exit: the
 *  connection dropped, and the view reconnects. */
export function exitOf(reason: string): number | null | undefined {
  const m = /^exited:(\d+|\?)$/.exec(reason);
  if (!m) return undefined;
  return m[1] === "?" ? null : Number(m[1]);
}

/** Whether a failed ticket says the terminal is gone from the node (killed,
 *  ended, or the node restarted), not that the node is out of reach. */
export const isGone = (message: string) => /^no terminal /.test(message);

/** A command row's block in an Agent terminal: its call (the id the node
 *  marks the block with), and its command, for a block marked otherwise. */
export type CommandAt = { call?: string; command: string };

/** The first line of a command, as an Agent terminal block's header shows it. */
export const headerOf = (command: string) => `$ ${command.split("\n")[0].trimEnd()}`;
