// Process-local ownership for a queue POST that outlives its Dockview pane.
//
// The durable queue remains node/Heart-owned. This registry only covers the
// short browser interval before the node acknowledges a request: a reopened
// projection may keep drafting, but it must not submit the staged instruction
// a second time while the original POST can still be accepted.

export type PendingTurnLease = Readonly<{ key: string; token: symbol }>;

const pending = new Map<string, symbol>();
const listeners = new Map<string, Set<() => void>>();

export function pendingTurnKey(streamId: string | null, agentId: string): string {
  return `${streamId ?? "-"}\u0000${agentId}`;
}

function notify(key: string): void {
  for (const listener of listeners.get(key) ?? []) listener();
}

export function claimPendingTurn(key: string): PendingTurnLease | null {
  if (pending.has(key)) return null;
  const token = Symbol(key);
  pending.set(key, token);
  notify(key);
  return Object.freeze({ key, token });
}

export function releasePendingTurn(lease: PendingTurnLease): void {
  if (pending.get(lease.key) !== lease.token) return;
  pending.delete(lease.key);
  notify(lease.key);
}

export function hasPendingTurn(key: string): boolean {
  return pending.has(key);
}

export function subscribePendingTurn(key: string, listener: () => void): () => void {
  const current = listeners.get(key) ?? new Set<() => void>();
  current.add(listener);
  listeners.set(key, current);
  return () => {
    current.delete(listener);
    if (current.size === 0) listeners.delete(key);
  };
}
