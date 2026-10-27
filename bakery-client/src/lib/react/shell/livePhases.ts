// The live phase of every conversation, for its glyph (`livePhase.ts`): read
// from the activity frames the node pushes on the events socket the shell
// already holds, once for the whole window. Nothing is attached per thread,
// so a conversation that was never opened shows its phase too.

import { useCallback, useEffect, useRef, useState, useSyncExternalStore } from "react";
import { onActivity, onEnvelope, onStatus } from "$lib/events";
import { node } from "$lib/node";
import { createLivePhases } from "./livePhase";
import type { Phase } from "./threadStates";

const phases = createLivePhases();
let started = false;
function start() {
  if (started) return;
  started = true;
  onActivity(null, phases.push);
}

/** A conversation's live phase, for its glyph; re-renders only when its own
 *  changes. */
export function useLivePhase(streamId: string | undefined): Phase | null {
  const watch = useCallback(
    (onChange: () => void) => {
      start();
      return streamId ? phases.watch(streamId, onChange) : () => {};
    },
    [streamId],
  );
  return useSyncExternalStore(
    watch,
    () => (streamId ? phases.get(streamId) : null),
    () => null,
  );
}

/** For a window that opened, or reconnected, in the middle of a turn, which
 *  no push has told yet: asks the node where each working conversation's turn
 *  stands (the route the panes use) — for all of them whenever pushes may
 *  have been missed (a connection opening, the socket falling behind), else
 *  for those still unknown. A push that lands meanwhile wins. */
export function useSeedLivePhases(working: readonly { streamId: string; roleId: string }[]) {
  const key = working.map((w) => `${w.streamId}\t${w.roleId}`).join("\n");
  const [missable, setMissable] = useState(0);
  const asked = useRef(0);
  useEffect(() => {
    const offStatus = onStatus((s) => {
      if (s === "open") setMissable((n) => n + 1);
    });
    const offLag = onEnvelope("events-lagged", () => setMissable((n) => n + 1));
    return () => {
      offStatus();
      offLag();
    };
  }, []);
  useEffect(() => {
    if (missable === 0) return;
    start();
    const all = asked.current !== missable;
    asked.current = missable;
    for (const w of working) {
      if (all || phases.get(w.streamId) === null) void phases.seed(w.streamId, () => node.activity(w.streamId, w.roleId));
    }
    // `key` carries what `working` says.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [missable, key]);
}
