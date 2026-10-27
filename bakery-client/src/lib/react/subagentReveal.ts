// A subagent the Agent map asked to show. The map opens the parent agent's
// chat; the subagent's card, once rendered there, takes the request, opens,
// and scrolls into view. A card already on screen hears it at once.

let pending: string | null = null;
const listeners = new Set<() => void>();

export function revealSubagent(toolCallId: string): void {
  pending = toolCallId;
  for (const listener of listeners) listener();
}

/** True once, for the card whose call was asked for. */
export function takeReveal(toolCallId: string): boolean {
  if (pending !== toolCallId) return false;
  pending = null;
  return true;
}

export function onReveal(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}
