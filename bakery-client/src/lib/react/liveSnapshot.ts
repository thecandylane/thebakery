// A store snapshot that is rebuilt only while some view shows it.
//
// A thread stays warm after its pane hides (shellSource's WARM_THREADS): its
// history and live updates keep landing, so showing it again loads nothing.
// Rebuilding its snapshot on each of those updates is work nobody sees, so
// while no view subscribes, nothing rebuilds; the next view reads it fresh.
// A rebuild that comes out the same as the last keeps the last, so React
// skips the render.

export type LiveSnapshot<T> = {
  subscribe(onChange: () => void): () => void;
  getSnapshot(): T;
  /** The reactive effect's body: rebuilds while watched, and reads nothing
   *  at all while not, so no change re-runs it. */
  track(): void;
  clear(): void;
};

/** `onWatched` is told as the first view subscribes and the last leaves:
 *  the effect reads it, so it starts tracking again on a show. */
export function liveSnapshot<T extends object>(
  build: () => T,
  watched: () => boolean,
  onWatched: (watched: boolean) => void,
): LiveSnapshot<T> {
  const listeners = new Set<() => void>();
  // Computed synchronously so React's very first `getSnapshot()` — which
  // happens during render, before any effect has flushed — sees real state
  // rather than a placeholder.
  let snapshot = build();

  /** Rebuilds; true when the snapshot changed. */
  function rebuild(): boolean {
    const next = build();
    if (sameSnapshot(snapshot, next)) return false;
    snapshot = next;
    return true;
  }

  return {
    subscribe(onChange) {
      listeners.add(onChange);
      // Nothing tracked a change while unwatched: read current at once.
      if (listeners.size === 1) {
        rebuild();
        onWatched(true);
      }
      return () => {
        if (!listeners.delete(onChange)) return;
        if (listeners.size === 0) onWatched(false);
      };
    },
    getSnapshot() {
      // Unwatched, nothing tracks a change: built on read instead.
      if (listeners.size === 0) rebuild();
      return snapshot;
    },
    track() {
      if (!watched()) return;
      if (rebuild()) for (const notify of listeners) notify();
    },
    clear() {
      if (listeners.size > 0) onWatched(false);
      listeners.clear();
    },
  };
}

type Store<T> = { subscribe(onChange: () => void): () => void; getSnapshot(): T };

/** A store a view can hold still while it is out of sight (a column
 *  scrolled away): held, it reads the snapshot it had and listens to
 *  nothing, so the source is unwatched and rebuilds nothing. Let go, it
 *  listens again and reads current at once. */
export function holdable<S extends Store<unknown>>(store: S): S & { hold(on: boolean): void } {
  type T = ReturnType<S["getSnapshot"]>;
  const listeners = new Set<() => void>();
  let held = false;
  let kept: { snapshot: T } | null = null;
  let stop: (() => void) | null = null;
  const tell = () => {
    for (const notify of listeners) notify();
  };
  const listen = () => {
    stop ??= store.subscribe(tell);
  };
  const deafen = () => {
    stop?.();
    stop = null;
  };
  const wrapped = {
    ...store,
    subscribe(onChange: () => void) {
      listeners.add(onChange);
      if (!held) listen();
      return () => {
        listeners.delete(onChange);
        if (listeners.size === 0) deafen();
      };
    },
    getSnapshot: (): T => (kept ? kept.snapshot : (store.getSnapshot() as T)),
    hold(on: boolean) {
      if (on === held) return;
      held = on;
      if (on) {
        kept = { snapshot: store.getSnapshot() as T };
        deafen();
        return;
      }
      kept = null;
      if (listeners.size > 0) listen();
      tell();
    },
  };
  return wrapped as S & { hold(on: boolean): void };
}

function isPlain(v: unknown): v is Record<string, unknown> {
  if (v === null || typeof v !== "object") return false;
  const proto = Object.getPrototypeOf(v);
  return proto === Object.prototype || proto === null;
}

function shallowSame(a: unknown, b: unknown): boolean {
  if (Object.is(a, b)) return true;
  if (!isPlain(a) || !isPlain(b)) return false;
  const keys = Object.keys(a);
  if (keys.length !== Object.keys(b).length) return false;
  return keys.every((k) => Object.prototype.hasOwnProperty.call(b, k) && Object.is(a[k], b[k]));
}

/** Two snapshots with the same facts: each field the same value, or a fresh
 *  array or object holding the same values one level down (what `build`
 *  copies out of a topology answer that did not move). */
export function sameSnapshot<T extends object>(a: T, b: T): boolean {
  if (Object.is(a, b)) return true;
  const x = a as Record<string, unknown>;
  const y = b as Record<string, unknown>;
  const keys = Object.keys(x);
  if (keys.length !== Object.keys(y).length) return false;
  return keys.every((k) => {
    const u = x[k];
    const v = y[k];
    if (Array.isArray(u) && Array.isArray(v)) return u.length === v.length && u.every((item, i) => shallowSame(item, v[i]));
    return shallowSame(u, v);
  });
}
