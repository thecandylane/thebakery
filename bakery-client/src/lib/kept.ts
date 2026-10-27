// What the app keeps of the node's replies, so a panel or the shell shows
// its last data at once (the operator: "they should just save on the first
// time they load and allow me to reload for stuff thats added"): in memory
// across tab switches and in this window's own storage across restarts, per
// node. A reply marked `secret` is never kept, and forgets what was kept
// before it. The React panels read it through `react/shell/cache.ts`.

// Relative, with the extension: the plain-`node` checks load this file.
import { NODE } from "./node.ts";

export type Kept = { data: unknown; at: number };

const PREFIX = "bakery.cache.";
const memory = new Map<string, Kept | null>();
const listeners = new Map<string, Set<() => void>>();
const inflight = new Map<string, Promise<void>>();
const stored = (key: string) => `${PREFIX}${NODE}:${key}`;

const isSecret = (data: unknown) => !!data && typeof data === "object" && (data as { secret?: unknown }).secret === true;

function tell(key: string) {
  for (const listener of listeners.get(key) ?? []) listener();
}

/** Calls `listener` whenever what's kept for `key` changes. */
export function listenKept(key: string, listener: () => void): () => void {
  const set = listeners.get(key) ?? new Set();
  set.add(listener);
  listeners.set(key, set);
  return () => set.delete(listener);
}

/** What was kept for `key`: from memory, else this window's storage. */
export function kept(key: string): Kept | null {
  if (memory.has(key)) return memory.get(key) ?? null;
  let hit: Kept | null = null;
  try {
    const raw = localStorage.getItem(stored(key));
    const v = raw ? (JSON.parse(raw) as Kept) : null;
    if (v && typeof v.at === "number" && "data" in v) hit = v;
  } catch {
    // Storage blocked or unreadable: nothing kept.
  }
  memory.set(key, hit);
  return hit;
}

/** Keeps a fresh reply for `key`, or forgets it when the reply is a secret. */
export function keep(key: string, data: unknown, at = Date.now()) {
  const next = isSecret(data) ? null : { data, at };
  memory.set(key, next);
  try {
    if (next) localStorage.setItem(stored(key), JSON.stringify(next));
    else localStorage.removeItem(stored(key));
  } catch {
    // Full or blocked: memory only.
  }
  tell(key);
}

/** Changes what's kept for `key` in place (a live update), keeping when it
 *  was fresh; nothing kept, nothing to change. */
export function patchKept<T>(key: string, change: (data: T) => T) {
  const k = kept(key);
  if (k) keep(key, change(k.data as T), k.at);
}

/** Runs `load` once at a time per key, keeping what it returns. */
export function refresh(key: string, load: () => Promise<unknown>): Promise<void> {
  const running = inflight.get(key);
  if (running) return running;
  const run = load()
    .then((data) => keep(key, data))
    .finally(() => inflight.delete(key));
  inflight.set(key, run);
  return run;
}
