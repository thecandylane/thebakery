// Saved replies (Round 10): the operator's own, kept by the node outside
// every thread on operator-only routes, so no agent ever sees one and
// nothing from them reaches an agent's context. One list for the window,
// kept current by the node's saved.updated (the Mac and the phone in step).
// A node from before Saved has no routes for it: `available` stays false
// there and nothing about Saved shows.

import { useEffect, useSyncExternalStore } from "react";
import { saved, savedRemove, type SavedReply, savedSet, savedTick } from "$lib/api.generated";
import { onEnvelope } from "$lib/events";
import { said } from "./ui";

export type { SavedReply };
type Saved = { available: boolean; items: readonly SavedReply[]; error: string };

let state: Saved = { available: false, items: [], error: "" };
const listeners = new Set<() => void>();
const set = (next: Partial<Saved>) => {
  state = { ...state, ...next };
  for (const listener of listeners) listener();
};
const newest = (a: SavedReply, b: SavedReply) => (a.saved_at < b.saved_at ? 1 : a.saved_at > b.saved_at ? -1 : 0);
const upsert = (r: SavedReply) => {
  set({ items: [r, ...state.items.filter((i) => i.id !== r.id)].sort(newest) });
  return r;
};

let loading = false;
function load() {
  if (loading) return;
  loading = true;
  saved({})
    .then(
      (r) => set({ available: true, items: [...r.items].sort(newest), error: "" }),
      // A node from before Saved has no such route: Saved stays out of sight.
      (e) => set(said(e).includes("has no route for") ? { available: false } : { error: said(e) }),
    )
    .finally(() => {
      loading = false;
    });
}

let started = false;
function start() {
  if (started) return;
  started = true;
  load();
  window.addEventListener("focus", load);
  onEnvelope("saved.updated", (e) => {
    const item = (e.item as SavedReply | null | undefined) ?? null;
    if (item) upsert(item);
    else set({ items: state.items.filter((i) => i.id !== e.id) });
  });
}

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

export function useSaved(): Saved {
  useEffect(start, []);
  return useSyncExternalStore(subscribe, () => state);
}

export const saveReply = (streamId: string, eventHash: string) => savedSet({ stream_id: streamId, event_hash: eventHash }).then(upsert);
export const unsaveReply = (id: string) => savedRemove({ id }).then(() => set({ items: state.items.filter((i) => i.id !== id) }));
export const tickItem = (id: string, item: number, done: boolean) => savedTick({ id, item, done }).then(upsert);
