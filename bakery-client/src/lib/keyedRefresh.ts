// Generic per-key async-refresh coalescer (07d-B Phase 4 fix). A plain,
// framework-free module — deliberately NOT a `.svelte.ts` file — so it can
// be unit-tested directly under `node` (svelte runes like `$state` only run
// through the Svelte compiler; see node.ts's own comment on the identical
// constraint for why files meant to run under plain `node` avoid it).
//
// The bug this replaces: `taskStore.refreshProject` used to read
// `if (this.inFlight.has(projectId)) return;` with no pending flag at all —
// a refresh requested while one was already in flight for the same key was
// dropped on the floor, permanently. That is fatal for a *terminal* event:
// `tasks::task_cancel_as` appends `task.cancelled` and then
// `task.status.changed{cancelled}` as two consecutive Heart `append` calls,
// each broadcast immediately; on a fast localhost link the second event can
// land while the fetch triggered by the first is still in flight (typical
// round trip 1-3ms). Because `cancelled` is terminal, no later `task.*`
// event ever arrives to retry, so the row stayed wrong until a page reload.
//
// Fix: track a generation counter per key, bumped on every `request()`. A
// request that finds a fetch for its key already running does not start a
// second one — the *running* fetch's completion notices the generation
// moved while it was in flight, discards its own (now possibly stale)
// result instead of applying it, and re-requests once more to converge.
// Same shape as `studioStore.pump`'s single-key generation guard,
// generalized to many independent keys.
export class KeyedRefresher<T> {
  private generation = new Map<string, number>();
  private inFlight = new Set<string>();
  private fetch: (key: string) => Promise<T>;
  private apply: (key: string, value: T) => void;

  constructor(fetch: (key: string) => Promise<T>, apply: (key: string, value: T) => void) {
    this.fetch = fetch;
    this.apply = apply;
  }

  /** Request a refresh for `key`. Fire-and-forget: coalesces with any fetch
   *  already running for this key rather than starting a redundant one, and
   *  never drops the request outright. */
  request(key: string): void {
    const gen = (this.generation.get(key) ?? 0) + 1;
    this.generation.set(key, gen);
    if (this.inFlight.has(key)) return; // the in-flight run's `finally` will notice and re-run
    void this.run(key, gen);
  }

  private async run(key: string, gen: number): Promise<void> {
    this.inFlight.add(key);
    try {
      const value = await this.fetch(key);
      // Only apply if nothing superseded this request while it was in the
      // air — a stale response must never overwrite a newer one's result.
      if (this.generation.get(key) === gen) this.apply(key, value);
    } catch {
      // Best-effort cache — a failed refresh leaves the prior value in
      // place rather than blanking something that was showing real data.
    } finally {
      this.inFlight.delete(key);
      if (this.generation.get(key) !== gen) this.request(key); // requested again mid-flight — converge
    }
  }

  /** True while a fetch for `key` is running — test/inspection hook. */
  isInFlight(key: string): boolean {
    return this.inFlight.has(key);
  }
}
