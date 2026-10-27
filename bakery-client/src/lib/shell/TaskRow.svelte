<script lang="ts">
  import type { Snippet } from "svelte";
  import type { SocketInfo, TaskView } from "$lib/node";
  import { taskAgentChipText } from "$lib/taskAgentLabel";

  // A task socket's sidebar affordance (07d-B Phase 4 item 3) — distinct
  // from an ordinary SocketRow: a status chip off `task.status.changed`
  // (joined by `stream_id` via `taskStore`), a busy/phase dot off the
  // EXISTING `AgentInfo.busy` `studioStore` already patches live (never a
  // second activity subscription), a pending-approvals count and
  // baseline-diff +N/−N straight off `SocketInfo.pending_gates`/
  // `changed_added`/`changed_removed` (server-computed, bounded-staleness
  // cached — never re-derived client-side), and no lifetime/discard
  // controls — a task's lifecycle is `task.cancel`/`task.retire`, not the
  // ordinary stream lifetime dial.

  let {
    s,
    task,
    selected,
    onOpen,
    actions,
  }: {
    s: SocketInfo;
    /** `null` while the parent project's task list hasn't loaded yet. */
    task: TaskView | null;
    selected: boolean;
    onOpen: () => void;
    /** The standard conversation actions menu. */
    actions?: Snippet;
  } = $props();

  const busy = $derived(s.agents.some((a) => a.busy));
  // Phase 4 item 3: every live task row shows "agent + model" — see
  // taskAgentLabel.ts for which seat drives a task and why.
  const agentChipText = $derived(taskAgentChipText(s));

  function dotClass(status: TaskView["status"] | null, isBusy: boolean): string {
    if (isBusy) return "busy";
    switch (status) {
      case "blocked":
        return "blocked";
      case "awaiting_review":
        return "awaiting";
      case "done":
        return "done";
      case "cancelled":
        return "cancelled";
      case "queued":
      case "running":
      default:
        return "queued";
    }
  }

  function statusLabel(status: TaskView["status"] | null): string {
    if (!status) return "…";
    return status.replace("_", " ");
  }
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div
  class="task-row"
  class:selected
  data-stream-id={s.stream_id}
  role="button"
  tabindex="0"
  onclick={onOpen}
  onkeydown={(e) => {
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      onOpen();
    }
  }}
>
  <div class="task-line1">
    <span class="task-dot {dotClass(task?.status ?? null, busy)}" class:pulse={busy} aria-hidden="true"></span>
    <span class="task-name">{task?.title ?? s.name}</span>
    {#if s.changed_added > 0 || s.changed_removed > 0}
      <span class="changed-ct" title="{s.changed_added} added, {s.changed_removed} removed vs. baseline">
        <span class="add">+{s.changed_added}</span><span class="del">−{s.changed_removed}</span>
      </span>
    {/if}
    {#if s.pending_gates > 0}
      <span class="pending-chip" title="{s.pending_gates} pending approval{s.pending_gates === 1 ? '' : 's'}">{s.pending_gates}</span>
    {/if}
    <span class="status-chip status-{dotClass(task?.status ?? null, busy)}">{statusLabel(task?.status ?? null)}</span>
    {#if actions}
      <!-- svelte-ignore a11y_click_events_have_key_events -->
      <!-- svelte-ignore a11y_no_static_element_interactions -->
      <span class="row-actions" onclick={(e) => e.stopPropagation()} onkeydown={(e) => e.stopPropagation()}>
        {@render actions()}
      </span>
    {/if}
  </div>
  <div class="task-line2">
    <span class="seat-chip">{agentChipText}</span>
    {#if task?.status_reason && (task.status === "blocked" || task.status === "queued")}
      <span class="reason">{task.status_reason}</span>
    {/if}
  </div>
</div>

<style>
  .task-row {
    display: flex;
    flex-direction: column;
    gap: 4px;
    width: calc(100% - 4px);
    margin: 1px 2px;
    padding: 7px 7px 7px 10px;
    background: transparent;
    border: none;
    border-radius: 8px;
    cursor: pointer;
    text-align: left;
    font: inherit;
  }
  .task-row:hover {
    background: var(--bg-chip);
  }
  .row-actions { display: inline-flex; flex: 0 0 auto; }
  .task-row.selected {
    background: var(--accent-sel);
    box-shadow: inset 2px 0 0 var(--violet);
  }
  .task-row:focus-visible {
    outline: 2px solid var(--violet);
    outline-offset: -2px;
  }

  .task-line1 {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .task-line2 {
    display: flex;
    align-items: center;
    gap: 5px;
    padding-left: 12px;
  }

  .seat-chip {
    flex: 0 0 auto;
    padding: 2px 5px;
    border-radius: var(--r-xs);
    background: var(--bg-chip);
    color: var(--fg-dim);
    font: 500 9px/1 var(--font-mono);
    letter-spacing: 0.04em;
  }

  .task-dot {
    width: 6px;
    height: 6px;
    border-radius: var(--r-pill);
    flex: 0 0 auto;
    background: var(--fg-fainter);
  }
  .task-dot.busy { background: var(--ok); box-shadow: var(--glow-live); }
  .task-dot.busy.pulse { animation: task-pulse 1.4s ease-in-out infinite; }
  .task-dot.queued { background: var(--fg-fainter); }
  .task-dot.blocked { background: var(--warn); }
  .task-dot.awaiting { background: var(--violet); }
  .task-dot.done { background: var(--ok); }
  .task-dot.cancelled { background: var(--bad); opacity: 0.6; }

  @keyframes task-pulse {
    0%, 100% { opacity: 1; }
    50% { opacity: 0.4; }
  }

  .task-name {
    font: 520 12px/1.1 var(--font-ui);
    color: var(--fg-2);
    flex: 1 1 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .status-chip {
    flex: 0 0 auto;
    padding: 2px 5px;
    border-radius: var(--r-xs);
    font: 500 8.5px/1 var(--font-mono);
    letter-spacing: 0.03em;
    text-transform: uppercase;
    background: var(--bg-chip);
    color: var(--fg-dim);
  }
  .status-chip.status-blocked { background: var(--warn-bg); color: var(--warn); }
  .status-chip.status-awaiting { background: var(--violet-bg); color: var(--violet); }
  .status-chip.status-done { background: var(--ok-bg); color: var(--ok); }
  .status-chip.status-cancelled { background: var(--bad-bg); color: var(--bad); }
  .status-chip.status-busy { background: var(--ok-bg); color: var(--ok); }

  .reason {
    font: 400 10.5px/1.3 var(--font-ui);
    color: var(--fg-faint);
  }

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
</style>
