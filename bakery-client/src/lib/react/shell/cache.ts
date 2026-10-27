// A panel's data, shown at once and refreshed in the background on open, on
// the panel's own events and timers, and on Reload; what's kept, and how, is
// `$lib/kept`.

import { useCallback, useEffect, useRef, useState, useSyncExternalStore } from "react";
// Relative, with the extension: the plain-`node` check loads this file.
import { kept, listenKept, refresh } from "../../kept.ts";

export { keep, kept, patchKept, refresh } from "../../kept.ts";

/** A panel's data by `key` (null: nothing to load yet): the kept reply at
 *  once, then `load` in the background on open (`maxAge`: only once what's
 *  kept is older) and on `reload()` (or
 *  `reload(other)`, a different read of the same data, such as asking the
 *  source again). Call `reload` from the panel's events and timers too.
 *  `at` is when it was last fresh. */
export function useCached<T>(key: string | null, load: () => Promise<T>, opts: { maxAge?: number } = {}) {
  const loadRef = useRef(load);
  loadRef.current = load;
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const subscribe = useCallback((listener: () => void) => (key ? listenKept(key, listener) : () => {}), [key]);
  const entry = useSyncExternalStore(subscribe, () => (key ? kept(key) : null));
  const reload = useCallback((using?: () => Promise<T>) => {
    if (!key) return Promise.resolve();
    setBusy(true);
    return refresh(key, () => (using ?? loadRef.current)()).then(
      () => {
        setError("");
        setBusy(false);
      },
      (e) => {
        setError(e instanceof Error ? e.message : String(e));
        setBusy(false);
      },
    );
  }, [key]);
  // On open: read again, unless what's kept is younger than `maxAge` (a
  // read that costs the node real work, like asking inside a room).
  const { maxAge } = opts;
  useEffect(() => {
    const last = key ? kept(key) : null;
    if (maxAge !== undefined && last && Date.now() - last.at < maxAge) return;
    void reload();
  }, [reload, key, maxAge]);
  return { data: (entry?.data as T | undefined) ?? null, at: entry?.at ?? null, busy, error, reload };
}

// ---- "updated N min ago" ----------------------------------------------------

const minuteListeners = new Set<() => void>();
let minute = Math.floor(Date.now() / 60_000);
let ticking: ReturnType<typeof setInterval> | null = null;

/** The current minute, re-rendering its readers as it turns. */
export function useMinute(): number {
  return useSyncExternalStore(
    (listener) => {
      minuteListeners.add(listener);
      ticking ??= setInterval(() => {
        const now = Math.floor(Date.now() / 60_000);
        if (now === minute) return;
        minute = now;
        for (const l of minuteListeners) l();
      }, 10_000);
      return () => {
        minuteListeners.delete(listener);
        if (minuteListeners.size === 0 && ticking) {
          clearInterval(ticking);
          ticking = null;
        }
      };
    },
    () => minute,
  );
}

/** "updated now", "updated 3 min ago", "updated 2 h ago". */
export function updatedAgo(at: number, now = Date.now()): string {
  const mins = Math.max(0, Math.floor((now - at) / 60_000));
  if (mins < 1) return "updated now";
  if (mins < 60) return `updated ${mins} min ago`;
  const hours = Math.floor(mins / 60);
  if (hours < 24) return `updated ${hours} h ago`;
  return `updated ${Math.floor(hours / 24)} d ago`;
}
