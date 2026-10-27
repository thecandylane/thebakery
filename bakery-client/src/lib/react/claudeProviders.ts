// Where a native Claude seat gets its model: the Claude subscription, an
// Anthropic Console API key, or OpenRouter. The seat's provider is a setting
// of the seat (POST /seat/assign); a provider is offered only when its key is
// saved, and the one the seat already has always stays listed.

export const CLAUDE_PROVIDERS = [
  { id: "subscription", label: "Subscription" },
  { id: "console", label: "Console key" },
  { id: "openrouter", label: "OpenRouter" },
] as const;

/** The keys the node says are saved (`GET /auth/status`). */
export type SavedKeys = { anthropic: { api_key_set: boolean }; openrouter: { api_key_set: boolean } };

/** The providers to offer a seat now on `current`: the subscription, the
 *  console and OpenRouter when their keys are saved, and `current` itself. */
export function providersOffered(keys: SavedKeys | null, current: string | null): { id: string; label: string }[] {
  const saved: Record<string, boolean> = {
    subscription: true,
    console: keys?.anthropic.api_key_set === true,
    openrouter: keys?.openrouter.api_key_set === true,
  };
  return CLAUDE_PROVIDERS.filter((p) => saved[p.id] || p.id === current);
}

/** A switch of a seat's model or provider asked for while its turn runs:
 *  the node lands it when that turn ends, and the turn runs on meanwhile
 *  (the agent's `switching`). */
export type WaitingSwitch = { modelId: string; provider: string | null };

/** The provider a waiting switch moves a seat to while it stays on its
 *  model; none when it goes to another model, or when nothing waits. */
export function waitingProvider(waiting: WaitingSwitch | null, assignedModelId: string | null): string | null {
  return waiting && waiting.modelId === assignedModelId ? waiting.provider : null;
}

/** What the seat's picker says of a waiting switch: where it goes (the
 *  provider it moves to, or for another model that model, which `label`
 *  names, with its provider) and when ("when this turn ends"; idle, after
 *  the node restarted under the turn, as the next turn starts). */
export function waitingSwitchNote(
  waiting: WaitingSwitch | null,
  assignedModelId: string | null,
  busy: boolean,
  label: (modelId: string) => string,
): { to: string; when: string } | null {
  if (!waiting) return null;
  const provider = CLAUDE_PROVIDERS.find((p) => p.id === waiting.provider)?.label ?? waiting.provider;
  const to = waiting.modelId === assignedModelId && provider ? provider : [label(waiting.modelId), provider].filter(Boolean).join(" · ");
  return { to, when: busy ? "when this turn ends" : "as its next turn starts" };
}
