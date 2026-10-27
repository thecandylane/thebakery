<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import { ask } from "$lib/ask";
  import { onEvent, onStatus } from "$lib/events";
  import { toHandle } from "$lib/handle";
  import { node, type HeartStats, type OperatorSnapshot, type SocketInfo, type TaskView } from "$lib/node";
  import { studioStore } from "$lib/studioStore.svelte";
  import { taskStore } from "$lib/taskStore.svelte";
  import { taskAgentChipText } from "$lib/taskAgentLabel";

  let actionError = $state("");

  type TaskRow = { task: TaskView; socket: SocketInfo };

  // 07d-B Phase 4 item 3: Tasks section — active tasks across every
  // Project, joined the same way the sidebar's task rows are (SocketInfo
  // topology + taskStore's TaskView cache), never a shadow fetch of its own.
  taskStore.ensureStarted();
  $effect(() => {
    for (const g of studioStore.data?.projects ?? []) {
      if (g.sockets.some((s) => s.state === "task")) taskStore.ensureProject(g.project_id);
    }
  });
  const activeTasks = $derived.by((): TaskRow[] => {
    const rows: TaskRow[] = [];
    for (const t of taskStore.allActive()) {
      if (t.status === "done" || t.status === "cancelled") continue;
      const socket = studioStore.findSocket(t.stream_id);
      if (socket) rows.push({ task: t, socket });
    }
    return rows;
  });

  /** Baker job rows come from operatorSnapshot (git-branch truth, includes
   *  orphan branches with no live stream) — cross-reference the canonical
   *  socket store for lifetime/discard, the same way the sidebar's Project
   *  rows do. 07d-A2: Baker/factory sockets no longer render in the
   *  sidebar tree at all, so this card is now their only discard control. */
  function bakerJobSocket(streamId: string) {
    return studioStore.sockets.find((s) => s.stream_id === streamId);
  }

  async function discardBakerJob(streamId: string) {
    actionError = "";
    const sock = bakerJobSocket(streamId);
    if (sock?.lifetime === "living") {
      const ok = await ask(
        `This job is pinned (living). Discard it anyway?\n\nThis removes its worktree and room; history is kept.`,
      );
      if (!ok) return;
    }
    try {
      await studioStore.discard(streamId, sock?.lifetime === "living");
    } catch (e) {
      actionError = `${e}`;
    }
  }

  // Factory floor live-status block — embedded directly on / (routes/+page.svelte
  // owns the page header; this renders no header of its own so the two don't
  // duplicate). Data: GET /operator/snapshot + /heart + /studio/state (measured
  // only). Conversations and their statuses are the Floor's work board, off
  // the canonical studioStore (07c-LIVE-STATE) — this component keeps no
  // socket inventory of its own; pending writes live in each socket's Review.

  let snap = $state<OperatorSnapshot | null>(null);
  let heart = $state<HeartStats | null>(null);
  let loading = $state(true);
  let loadError = $state("");

  let timer: ReturnType<typeof setTimeout> | null = null;
  let stop: (() => void) | null = null;
  let stopStatus: (() => void) | null = null;

  async function load(opts: { quiet?: boolean } = {}) {
    if (!opts.quiet) {
      loading = true;
      loadError = "";
    }
    try {
      const [s, h] = await Promise.all([
        node.operatorSnapshot(),
        node.heart().catch(() => null),
      ]);
      snap = s;
      heart = h;
      if (opts.quiet) loadError = "";
    } catch (e) {
      loadError = `${e}`;
    } finally {
      loading = false;
    }
  }

  function schedule() {
    if (timer) clearTimeout(timer);
    timer = setTimeout(() => {
      timer = null;
      void load({ quiet: true });
    }, 500);
  }

  onMount(() => {
    // Defensive/idempotent: +layout.svelte's onMount also calls this, but this
    // panel must not assume that ordering — ensureStarted() is a cheap no-op
    // on the second call.
    studioStore.ensureStarted();
    void load();
    // Mounting with the node down leaves loadError pinned forever: nothing else
    // retries, because every other trigger is a node event. Reload when the
    // socket opens so a node started after the client heals this panel too.
    // onStatus replays the current status synchronously on subscribe; that tick
    // is already covered by the load() above, so only act on later transitions.
    let replayed = false;
    stopStatus = onStatus((s) => {
      if (!replayed) {
        replayed = true;
        return;
      }
      if (s === "open") void load({ quiet: true });
    });
    // This filter is for `snap`/`heart` only — socket topology refreshes
    // itself on topology events via studioStore's own subscription; do not
    // add those here, that was 07c-LIVE-STATE's exact bug. project.onboarded
    // and baker.assignment.accepted stay in this list because they change
    // operatorSnapshot's own fields (stream_count, baker.job_count/jobs), not
    // because they used to drive the removed /streams fetch. `stream.discarded`
    // is here for a different reason (07d-A2): `snap.baker.jobs` is the ONLY
    // place that shows an orphan baker/* branch with no live stream, so it
    // has no reactive-store equivalent to fall back on — its own Discard
    // button (added alongside this trigger) would otherwise leave a stale,
    // still-clickable row after a successful discard.
    stop = onEvent(null, (ev) => {
      if (
        ev.family === "agent.file.changed" ||
        ev.family.startsWith("agent.edit.") ||
        ev.family === "stream.discarded" ||
        ev.family === "project.onboarded" ||
        ev.family === "baker.assignment.accepted" ||
        ev.family === "policy.write_mode.set" ||
        ev.family === "memory.written"
      ) {
        schedule();
      }
    });
  });

  onDestroy(() => {
    stop?.();
    stopStatus?.();
    if (timer) clearTimeout(timer);
  });
</script>

<div class="floor">
  {#if loading && !snap}
    <p class="muted">Loading…</p>
  {:else if loadError && !snap}
    <p class="bad">{loadError}</p>
  {:else if snap}
    {#if loadError}
      <p class="bad soft">May be stale — {loadError}</p>
    {/if}

    <div class="statline">
      <a class="stat" href="#nodes-streams"><b>{snap.stream_count}</b> streams</a>
      <a class="stat" href="#knowledge-heart"><b>{heart?.events ?? snap.events}</b> Heart events</a>
      <span class="stat">default model <b>{snap.model || "—"}</b></span>
      {#if snap.baker.known && snap.baker.job_count > 0}
        <a class="stat" href="/baker"><b>{snap.baker.job_count}</b> Baker jobs</a>
      {/if}
    </div>

    {#if activeTasks.length > 0}
      <section class="panel tasks-panel">
        <div class="panel-hd">
          <h2>Tasks</h2>
          <span class="count-chip">{activeTasks.length}</span>
        </div>
        <div class="panel-body">
          <ul class="rows">
            {#each activeTasks as row (row.task.task_id)}
              {@const busy = row.socket.agents.some((a) => a.busy)}
              <li class="task-row">
                <div class="task-row-top">
                  <a href={`/w/${toHandle(row.task.stream_id)}`} class="task-link">
                    <span class="task-dot" class:busy class:blocked={row.task.status === "blocked"} class:awaiting={row.task.status === "awaiting_review"} aria-hidden="true"></span>
                    <span class="name">{row.task.title}</span>
                    {#if row.socket.changed_added > 0 || row.socket.changed_removed > 0}
                      <span class="changed-ct" title="{row.socket.changed_added} added, {row.socket.changed_removed} removed vs. baseline">
                        <span class="add">+{row.socket.changed_added}</span><span class="del">−{row.socket.changed_removed}</span>
                      </span>
                    {/if}
                    {#if row.socket.pending_gates > 0}
                      <span class="pending-chip" title="{row.socket.pending_gates} pending approval{row.socket.pending_gates === 1 ? '' : 's'}">{row.socket.pending_gates}</span>
                    {/if}
                    <span class="status-chip status-{row.task.status}">{row.task.status.replace("_", " ")}</span>
                  </a>
                  <a
                    class="review-link"
                    href={`/w/${toHandle(row.task.stream_id)}?open=review`}
                    title="Open this task's Review"
                  >review →</a>
                </div>
                <div class="task-row-bottom">
                  <span class="seat-chip">{taskAgentChipText(row.socket)}</span>
                  {#if row.task.status_reason && (row.task.status === "blocked" || row.task.status === "queued")}
                    <span class="task-reason">{row.task.status_reason}</span>
                  {/if}
                </div>
              </li>
            {/each}
          </ul>
        </div>
      </section>
    {/if}

    <!-- Legacy baker/* branches: this card is their only discard control, so
         it shows whenever there are any, and not otherwise. -->
    {#if snap.baker.known && snap.baker.jobs.length > 0}
    <div class="cols">
      <section class="panel">
        <div class="panel-hd">
          <h2>Baker jobs</h2>
          <span class="count-chip">{snap.baker.jobs.length}</span>
        </div>
        <div class="panel-body">
          {#if actionError}<p class="bad">{actionError}</p>{/if}
            <ul class="rows">
              {#each snap.baker.jobs as j (j.branch)}
                <li class="job-row">
                  {#if j.stream_id}
                    <a href={`/w/${toHandle(j.stream_id)}`}>
                      <span class="name">{j.brief || j.branch.replace(/^baker\//, "")}</span>
                      <span class="meta">{j.branch}</span>
                    </a>
                    <button
                      type="button"
                      class="job-discard"
                      disabled={studioStore.isRetiring(j.stream_id)}
                      title="Discard — removes the worktree and room, keeps history"
                      aria-label={`Discard ${j.brief || j.branch}`}
                      onclick={() => j.stream_id && discardBakerJob(j.stream_id)}
                    >{studioStore.isRetiring(j.stream_id) ? "…" : "×"}</button>
                  {:else}
                    <span class="name dim">{j.brief || j.branch}</span>
                  {/if}
                </li>
              {/each}
            </ul>
        </div>
      </section>
    </div>
    {/if}
  {/if}
</div>

<style>
  .floor {
    width: 100%;
    min-width: 0;
  }

  /* One line of node facts, not a row of tiles. */
  .statline {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 4px 16px;
    margin-bottom: 12px;
    font: 400 11.5px/1.4 var(--font-ui);
    color: var(--fg-faint);
  }
  .stat {
    color: inherit;
    text-decoration: none;
    white-space: nowrap;
  }
  .stat b {
    font: 600 11.5px/1 var(--font-mono);
    color: var(--fg-2);
  }
  a.stat:hover,
  a.stat:hover b { color: var(--accent-fg); text-decoration: none; }
  a.stat:focus-visible,
  .task-link:focus-visible,
  .review-link:focus-visible,
  .rows li a:focus-visible,
  .job-discard:focus-visible {
    outline: 2px solid var(--accent-border);
    outline-offset: -2px;
  }

  .tasks-panel { margin-bottom: 12px; }
  .count-chip {
    padding: 2px 6px;
    border-radius: var(--r-sm);
    background: var(--bg-chip);
    color: var(--fg-dim);
    font: 400 10.5px/1 var(--font-mono);
  }
  .task-row {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
    border-bottom: 1px solid var(--line-row);
  }
  .task-row:last-child { border-bottom: 0; }
  .task-row-top {
    display: flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
  }
  .task-row-bottom {
    display: flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
    padding: 0 8px 6px 23px;
  }
  .seat-chip {
    flex: 0 0 auto;
    padding: 2px 5px;
    border-radius: var(--r-xs);
    background: var(--bg-chip);
    color: var(--fg-dim);
    font: 500 8.5px/1 var(--font-mono);
    letter-spacing: 0.04em;
  }
  .task-reason {
    font: 400 10px/1.3 var(--font-ui);
    color: var(--fg-faint);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .task-link {
    display: flex;
    align-items: center;
    gap: 7px;
    flex: 1 1 auto;
    min-width: 0;
    padding: 7px 8px;
    border-radius: var(--r-xl);
    text-decoration: none;
    color: inherit;
  }
  .task-link:hover { background: var(--bg-chip); }
  .task-dot {
    width: 6px;
    height: 6px;
    border-radius: var(--r-pill);
    flex: 0 0 auto;
    background: var(--fg-fainter);
  }
  .task-dot.busy { background: var(--ok); box-shadow: var(--glow-live); }
  .task-dot.blocked { background: var(--warn); }
  .task-dot.awaiting { background: var(--violet); }
  .status-chip {
    flex: 0 0 auto;
    padding: 2px 6px;
    border-radius: var(--r-xs);
    font: 500 9px/1 var(--font-mono);
    text-transform: uppercase;
    letter-spacing: 0.03em;
    background: var(--bg-chip);
    color: var(--fg-dim);
    margin-left: auto;
  }
  .status-chip.status-blocked { background: var(--warn-bg); color: var(--warn); }
  .status-chip.status-awaiting_review { background: var(--violet-bg); color: var(--violet); }
  .changed-ct {
    flex: 0 0 auto;
    display: flex;
    gap: 4px;
    font: 500 9.5px/1 var(--font-mono);
    font-variant-numeric: tabular-nums;
  }
  .changed-ct .add { color: var(--ok); }
  .changed-ct .del { color: var(--bad); }
  .pending-chip {
    flex: 0 0 auto;
    min-width: 13px;
    padding: 1px 4px;
    border-radius: var(--r-pill);
    text-align: center;
    font: 600 9px/1.3 var(--font-mono);
    background: var(--warn-bg);
    color: var(--warn);
  }
  .review-link {
    flex: 0 0 auto;
    padding: 7px 9px;
    border-radius: var(--r-lg);
    font: 500 10.5px/1 var(--font-ui);
    color: var(--accent-fg);
    text-decoration: none;
    white-space: nowrap;
  }
  .review-link:hover { background: var(--accent-bg); }

  .cols {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: 16px;
  }
  .panel {
    min-width: 0;
    border: 1px solid var(--line-soft);
    border-radius: var(--r-2xl);
    background: var(--bg-panel);
    overflow: hidden;
  }
  .panel-hd {
    display: flex;
    align-items: center;
    gap: 8px;
    box-sizing: border-box;
    padding: 8px 12px;
    background: var(--bg-raised);
    border-bottom: 1px solid var(--line-soft);
  }
  .panel-hd h2 {
    margin: 0;
    font: 600 12.5px/1.2 var(--font-ui);
    color: var(--fg-2);
  }
  .panel-body { min-width: 0; padding: 4px 6px 6px; }
  .rows {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  .rows > li > a, .rows > li > .name {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
    padding: 9px 8px;
    border-radius: var(--r-xl);
    text-decoration: none;
    color: inherit;
  }
  .rows li a:hover { background: var(--bg-chip); }
  .job-row {
    display: flex;
    align-items: center;
    gap: 4px;
  }
  .job-row a { flex: 1 1 auto; min-width: 0; }
  .job-discard {
    flex: 0 0 auto;
    display: flex;
    align-items: center;
    justify-content: center;
    width: 28px;
    height: 28px;
    border: none;
    background: transparent;
    color: var(--fg-fainter);
    cursor: pointer;
    padding: 0;
    font: 400 13px/1 var(--font-ui);
    border-radius: var(--r-lg);
  }
  .job-discard:hover:not(:disabled) { color: var(--bad); background: var(--bad-bg); }
  .job-discard:disabled { cursor: default; opacity: 0.6; }
  .name {
    min-width: 0;
    font: 500 12px/1.35 var(--font-ui);
    color: var(--fg);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .name.dim { color: var(--fg-faint); padding: 9px 8px; }
  .meta {
    font: 400 10.5px/1.3 var(--font-mono);
    color: var(--fg-fainter);
  }
  .muted {
    margin: 0;
    padding: 12px 8px;
    font: 400 12px/1.5 var(--font-ui);
    color: var(--fg-fainter);
  }
  .bad {
    margin: 0 0 10px;
    color: var(--bad);
    font: 400 12px/1.4 var(--font-ui);
  }
  .bad.soft { color: var(--fg-dim); }

  @media (max-width: 520px) {
    .task-row-top { align-items: stretch; flex-direction: column; gap: 0; }
    .review-link { align-self: flex-end; margin: -3px 3px 5px 0; }
    .status-chip { max-width: 40%; overflow: hidden; text-overflow: ellipsis; }
  }
</style>
