// Pure queue-ordering / focus-selection logic for the Review pane (07d-A4:
// "one pending proposal at a time as the focus"). Extracted from Review.svelte
// so it is testable without a browser or Monaco — see reviewFocus.check.ts.

import type { TranscriptItem } from "./transcript.ts";

export type Proposal = Extract<TranscriptItem, { kind: "proposal" }>;

/** Live work first (pending, newest first), then history (newest first) —
 *  keeps the active queue at the top so the human isn't hunting for it. */
export function orderProposals(items: TranscriptItem[]): Proposal[] {
  const proposals = items.filter((i): i is Proposal => i.kind === "proposal");
  const pending = proposals.filter((p) => p.verdict === "pending").reverse();
  const decided = proposals.filter((p) => p.verdict !== "pending").reverse();
  return [...pending, ...decided];
}

/** Which hash is "the focus": stays on the current selection while it's
 *  still in the (ordered) list, otherwise defaults to the top of the queue.
 *  `undefined` only when the queue is empty. */
export function pickFocus(ordered: Proposal[], current: string | undefined): string | undefined {
  if (current !== undefined && ordered.some((p) => p.hash === current)) return current;
  return ordered[0]?.hash;
}
