// A project's rollup (GitHub and worktree counts), shared by everything that
// shows it — the sidebar and the thread headers ask once between them. The last
// one shows at once (kept across restarts); fetched when a project is shown
// and when the window regains focus, at most once a minute per project, as
// each call asks GitHub (1–2 s).

import { useEffect, useMemo, useState, useSyncExternalStore } from "react";
import { projectRollup, type ProjectRollup } from "$lib/api.generated";
import { keep, kept } from "./cache";
import type { Merge } from "./threadStates";
import { said } from "./ui";

export type Rollup = { data: ProjectRollup | null; error: string; busy: boolean; at: number | null };

const EMPTY: Rollup = { data: null, error: "", busy: false, at: null };
const got = new Map<string, Rollup>();
const asked = new Map<string, number>();
const listeners = new Set<() => void>();
/** Bumped on every answer, so a view of several rollups knows to look again. */
let version = 0;
const notify = () => {
  version++;
  for (const listener of listeners) listener();
};

/** A project's rollup as known: asked this run, else kept from the last. */
function rollupOf(projectId: string): Rollup {
  const have = got.get(projectId);
  if (have) return have;
  const last = kept(`rollup:${projectId}`);
  const seeded = last ? { data: last.data as ProjectRollup, error: "", busy: false, at: last.at } : EMPTY;
  got.set(projectId, seeded);
  return seeded;
}

function load(projectId: string) {
  if (Date.now() - (asked.get(projectId) ?? 0) < 60_000) return;
  asked.set(projectId, Date.now());
  got.set(projectId, { ...rollupOf(projectId), busy: true });
  notify();
  projectRollup({ project_id: projectId }).then(
    (data) => {
      const at = Date.now();
      keep(`rollup:${projectId}`, data, at);
      got.set(projectId, { data, error: "", busy: false, at });
      notify();
    },
    (e) => {
      got.set(projectId, { ...rollupOf(projectId), error: said(e), busy: false });
      notify();
    },
  );
}

/** Asks again now, after something changed what it counts (a fetch, an update). */
export function reloadRollup(projectId: string) {
  asked.delete(projectId);
  load(projectId);
}

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

export function useRollup(projectId: string | null): Rollup {
  useEffect(() => {
    if (!projectId) return;
    const again = () => load(projectId);
    again();
    window.addEventListener("focus", again);
    return () => window.removeEventListener("focus", again);
  }, [projectId]);
  return useSyncExternalStore(subscribe, () => (projectId ? rollupOf(projectId) : EMPTY));
}

/** Several projects' rollups, by project id, each asked for as `useRollup`
 *  asks for one. */
export function useRollups(projectIds: readonly string[]): ReadonlyMap<string, Rollup> {
  const key = projectIds.join("\n");
  useEffect(() => {
    const ids = key ? key.split("\n") : [];
    const again = () => {
      for (const id of ids) load(id);
    };
    again();
    window.addEventListener("focus", again);
    return () => window.removeEventListener("focus", again);
  }, [key]);
  const seen = useSyncExternalStore(subscribe, () => version);
  return useMemo(
    () => new Map((key ? key.split("\n") : []).map((id) => [id, rollupOf(id)])),
    // `seen` changes whenever any rollup does.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [key, seen],
  );
}

/** A project's rollup as last fetched, without asking again. */
export function peekRollup(projectId: string): ProjectRollup | null {
  return rollupOf(projectId).data;
}

/** A finished thread to check: its stream, the branch it merges into, and a
 *  stamp that changes when the thread moves on. */
export type Finished = { streamId: string; base: string; stamp: string };

const known = new Map<string, { stamp: string; merge: Merge | null }>();

/** Whether each finished thread's branch has something to merge into its
 *  base, and merges cleanly: the finish card's own rule, asked once per
 *  stamp through `preview` and remembered. */
export function useMergeStates(
  finished: readonly Finished[],
  preview: (streamId: string, base: string) => Promise<{ clean: boolean; files: number }>,
): ReadonlyMap<string, Merge | null> {
  const [, setVersion] = useState(0);
  const key = finished.map((f) => `${f.streamId}:${f.base}:${f.stamp}`).join(" ");
  useEffect(() => {
    let live = true;
    for (const f of finished) {
      if (known.get(f.streamId)?.stamp === f.stamp) continue;
      known.set(f.streamId, { stamp: f.stamp, merge: known.get(f.streamId)?.merge ?? null });
      preview(f.streamId, f.base).then(
        (p) => {
          known.set(f.streamId, { stamp: f.stamp, merge: p.files === 0 ? null : p.clean ? "ready" : "conflict" });
          if (live) setVersion((v) => v + 1);
        },
        () => {},
      );
    }
    return () => {
      live = false;
    };
    // `key` carries everything `finished` says.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key]);
  return new Map(finished.map((f) => [f.streamId, known.get(f.streamId)?.merge ?? null]));
}
