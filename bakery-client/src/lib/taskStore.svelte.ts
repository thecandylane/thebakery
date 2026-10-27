// Shared reactive task-list cache (07d-B Phase 4 item 3/4) — the one place
// that fetches `GET /task/list` per project, so the sidebar's task rows, the
// Factory Floor's Tasks section, and (once it exists) Review's task-baseline
// header all read the same live data instead of each keeping its own fetch
// loop. Mirrors `studioStore`'s shape (a plain reactive class singleton,
// `SvelteMap` for reactivity) but stays a distinct module: task rows are a
// join of `SocketInfo` (topology, from `studioStore`) with `TaskView`
// (contract/status, from `GET /task/list`), never a duplicate topology
// fetch of its own (studio.rs's own "no shadow fetch" rule extends here).
import { SvelteMap } from "svelte/reactivity";
import { node, type TaskView } from "./node";
import { onEvent } from "./events";
import { studioStore } from "./studioStore.svelte";
import { KeyedRefresher } from "./keyedRefresh";

/** Every Heart family that changes a project's task list — the driver
 *  (07d-B Phase 4) is not built yet, so today only create/start/cancel/
 *  retire ever fire, but the full status vocabulary is listed for when it
 *  is. */
const TASK_FAMILIES = new Set([
  "task.created",
  "task.started",
  "task.status.changed",
  "task.cancelled",
  "task.retired",
]);

class TaskStore {
  /** project_id -> that project's task list, latest fetch wins. */
  private byProject = new SvelteMap<string, TaskView[]>();
  /** Per-project fetch coalescer — see `keyedRefresh.ts` for why this is a
   *  separate, plain-`node`-testable module rather than inline generation
   *  tracking: two `task.*` events fired back to back by the same server
   *  handler (e.g. `tasks::task_cancel_as` appending `task.cancelled` then
   *  `task.status.changed`) can land less than one HTTP round trip apart,
   *  and a naive "drop if already in flight" guard silently lost the
   *  second — fatal when it carries a terminal status with no later event
   *  to retry it. */
  private refresher = new KeyedRefresher<TaskView[]>(
    (projectId) => node.taskList(projectId),
    (projectId, list) => this.byProject.set(projectId, list),
  );
  private started = false;
  private stopEvents: (() => void) | null = null;

  /** Idempotent, like `studioStore.ensureStarted()` — safe to call from
   *  every consumer's own mount. Only reacts to `task.*` events; topology
   *  (which projects exist, which sockets are task-kind) still comes from
   *  `studioStore`. */
  ensureStarted(): void {
    if (this.started) return;
    this.started = true;
    this.stopEvents = onEvent(null, (ev) => {
      if (!TASK_FAMILIES.has(ev.family)) return;
      for (const projectId of this.projectsWithTasks()) this.refreshProject(projectId);
    });
  }

  dispose(): void {
    this.stopEvents?.();
    this.stopEvents = null;
    this.started = false;
  }

  /** Every Project id that currently has at least one `state: "task"`
   *  socket, per the canonical topology store. */
  private projectsWithTasks(): string[] {
    return (studioStore.data?.projects ?? [])
      .filter((g) => g.sockets.some((s) => s.state === "task"))
      .map((g) => g.project_id);
  }

  private refreshProject(projectId: string): void {
    this.refresher.request(projectId);
  }

  /** Ensures `projectId`'s task list has been fetched at least once (and
   *  kicks a fresh fetch if none is in flight) — call from a component's own
   *  `$effect` for every project group it renders that has task sockets. */
  ensureProject(projectId: string): void {
    if (!this.byProject.has(projectId)) this.refreshProject(projectId);
  }

  /** One task by its own socket's `stream_id`, or `null` while its
   *  project's list hasn't loaded yet (call `ensureProject` first). */
  forStream(streamId: string): TaskView | null {
    for (const list of this.byProject.values()) {
      const t = list.find((x) => x.stream_id === streamId);
      if (t) return t;
    }
    return null;
  }

  /** Every non-retired task across every project this store has fetched —
   *  the Factory Floor's cross-project Tasks section. Callers must
   *  `ensureProject` every relevant project first (typically every project
   *  with a `state: "task"` socket, from `studioStore`). */
  allActive(): TaskView[] {
    return [...this.byProject.values()].flat().filter((t) => !t.retired);
  }
}

export const taskStore = new TaskStore();
