// Which state a thread's glyph shows (Round 5, M0): its status from the node,
// the live phase when the shell knows it, and whether a finished branch has
// something to merge. Pure and DOM-free: threadStates.check.ts runs it on node.

export type Phase = "thinking" | "writing" | "working";
export type Merge = "ready" | "conflict";
export type Glyph = "needs" | "failed" | "working" | "writing" | "thinking" | "conflict" | "ready" | "done" | "stopped" | "idle";

/** The glyph for a thread, the highest state winning: needs you, failed,
 *  then a running turn's phase (one at a time), then a finished turn
 *  (won't merge cleanly, ready to merge, done), then stopped and idle. */
export function glyphOf(status: string, phase: Phase | null = null, merge: Merge | null = null): Glyph {
  if (status === "needs_input") return "needs";
  if (status === "failed") return "failed";
  if (status === "working") return phase ?? "working";
  if (status === "done") return merge ?? "done";
  if (status === "stopped") return "stopped";
  return "idle";
}

/** An activity phase in the glyph's words; null when it shows no turn running. */
export function phaseOf(activity: string | null | undefined): Phase | null {
  if (activity === "contacting_model") return "thinking";
  if (activity === "streaming") return "writing";
  if (activity === "running_tool") return "working";
  return null;
}
