// Pure terminal-mode selection rules for the studio Terminal dock (07b-B).
// The thin room has no prerequisites, so Room is always offered for every
// active socket — there is no `checkRoom()` probe anymore. Burn never gets
// Host (STEERING: "hostile/untrusted: do not offer host").

export type PtyMode = "host" | "room";

/** Trusted (non-Burn) sockets default to Host; Burn always defaults to (and
 *  is stuck on) Room. */
export function defaultTerminalMode(isBurn: boolean): PtyMode {
  return isBurn ? "room" : "host";
}

/** Whether picking `to` from the mode toggle is allowed right now. */
export function canPickMode(to: PtyMode, isBurn: boolean): boolean {
  return !(to === "host" && isBurn);
}
