<script lang="ts">
  import { untrack } from "svelte";
  import { ask } from "$lib/ask";
  import { monaco } from "$lib/monacoSetup";
  import { languageFor } from "$lib/editorModels.svelte";
  import { collapse, countChanges, diffLines } from "$lib/diff";
  import { orderProposals, pickFocus } from "$lib/reviewFocus";
  import { node, type SocketInfo, type TaskReviewResp } from "$lib/node";
  import { taskStore } from "$lib/taskStore.svelte";
  import { onEvent } from "$lib/events";
  import { reviewFilesChanged } from "$lib/taskReviewCompare";
  import { actorLabel, type TranscriptItem } from "$lib/transcript";

  // The Review queue (07d-A4): a GitHub-style split diff, one pending
  // proposal at a time as the focus — a list of files/paths on the left is
  // still OK to pick among, but only one Monaco DiffEditor is ever mounted.
  // Pending and decided proposals both appear in the list (the decided ones
  // are the audit trail); only a pending one gets Accept/Reject.

  type Proposal = Extract<TranscriptItem, { kind: "proposal" }>;

  let {
    items,
    busy = false,
    studioSocket = null,
    focusRequest = null,
    onDecide,
  }: {
    items: TranscriptItem[];
    busy?: boolean;
    /** Present for a task socket (`state: "task"`) — drives the Task
     *  baseline header above the proposal list (07d-B Phase 4 item 2).
     *  `null`/non-task sockets render exactly as before. */
    studioSocket?: SocketInfo | null;
    /** A change to focus — "Open in Review" on it in the chat. A new `seq`
     *  is a new request, even for the same change. */
    focusRequest?: { hash: string; seq: number } | null;
    /** May be async (route decide); Accept all awaits each so busy stays true end-to-end. */
    onDecide: (hash: string, accept: boolean, newContent?: string) => void | Promise<void>;
  } = $props();

  // ---- Task baseline header (07d-B Phase 4 item 2) --------------------
  //
  // The real surface: the COMPLETE baseline diff — worktree vs
  // `task.workspace.materialized`'s base commit, not just this pane's own
  // recorded-proposal list below — plus a "Sign off" that pins a commit.
  // `GET /task/review`/`POST /task/signoff` are real server routes
  // (`task_review.rs`); this header renders the former's file list (a
  // separate baseline-files panel, distinct from the proposal list/diff
  // below) and wires the latter to the button.
  const isTaskSocket = $derived(studioSocket?.state === "task");
  $effect(() => {
    if (studioSocket?.state === "task" && studioSocket.project_id) {
      taskStore.ensureProject(studioSocket.project_id);
    }
  });
  const task = $derived(studioSocket ? taskStore.forStream(studioSocket.stream_id) : null);

  let baselineRefusal = $state("");
  let review = $state<TaskReviewResp | null>(null);

  async function loadReview(projectId: string, taskId: string): Promise<TaskReviewResp | null> {
    try {
      const resp = await node.taskReview(projectId, taskId);
      baselineRefusal = "";
      review = resp;
      return resp;
    } catch (e) {
      baselineRefusal = `${e}`;
      review = null;
      return null;
    }
  }

  $effect(() => {
    const projectId = studioSocket?.project_id;
    const taskId = task?.task_id;
    if (!isTaskSocket || !projectId || !taskId) {
      baselineRefusal = "";
      review = null;
      return;
    }
    void loadReview(projectId, taskId);
  });

  // The identity effect above only re-runs on `studioSocket.project_id` /
  // `task.*` changes — neither fires for an edit-gate decision made in this
  // very pane (`agent.edit.accepted`/`rejected`) or a new proposal
  // (`agent.file.changed`), so without this the baseline panel goes stale
  // exactly when a human is most likely to act on it (see `signOff`'s own
  // mandatory re-check for the failure mode this leaves if left
  // unguarded). Scoped to this task's own stream — a debounce match to
  // studioStore's gate-refresh timer, since "accept all" can fire a burst.
  let reviewRefreshTimer: ReturnType<typeof setTimeout> | null = null;
  $effect(() => {
    const projectId = studioSocket?.project_id;
    const taskId = task?.task_id;
    const streamId = studioSocket?.stream_id;
    if (!isTaskSocket || !projectId || !taskId || !streamId) return;
    const stop = onEvent(streamId, (ev) => {
      if (ev.family !== "agent.file.changed" && !ev.family.startsWith("agent.edit.")) return;
      if (reviewRefreshTimer) clearTimeout(reviewRefreshTimer);
      reviewRefreshTimer = setTimeout(() => {
        reviewRefreshTimer = null;
        void loadReview(projectId, taskId);
      }, 400);
    });
    return () => {
      stop();
      if (reviewRefreshTimer) {
        clearTimeout(reviewRefreshTimer);
        reviewRefreshTimer = null;
      }
    };
  });

  // One baseline file selected at a time, mirroring the proposal list's own
  // single-focus pattern — defaults to the first file whenever the list
  // changes and the current selection no longer exists in it.
  let baselineFocusPath = $state<string | undefined>(undefined);
  $effect(() => {
    const files = review?.files ?? [];
    if (files.length === 0) {
      if (baselineFocusPath !== undefined) baselineFocusPath = undefined;
      return;
    }
    if (!files.some((f) => f.path === baselineFocusPath)) {
      baselineFocusPath = files[0].path;
    }
  });
  const baselineFocused = $derived(review?.files.find((f) => f.path === baselineFocusPath));

  let signingOff = $state(false);
  let signoffError = $state("");

  // Task shipping (confirmed gap 5) — the client surface over the
  // task-scoped ship routes. Nothing automatic: integrate confirms and
  // pins the exact target branch shown; the server re-verifies both the
  // signed-off commit and that branch at merge time.
  let shipping = $state<"" | "integrate" | "push" | "pr">("");
  let shipMsg = $state("");
  let shipError = $state("");
  async function integrateTask() {
    const projectId = studioSocket?.project_id;
    const taskId = task?.task_id;
    const target = review?.repo_current_branch;
    if (!projectId || !taskId || shipping) return;
    if (!target) {
      shipError = "The project repo's checked-out branch is unknown — refresh Review first.";
      return;
    }
    const ok = await ask(
      `Merge this task's signed-off branch into ${target} (local --no-ff merge)? Nothing else is touched.`,
    );
    if (!ok) return;
    shipping = "integrate";
    shipMsg = "";
    shipError = "";
    try {
      const resp = await node.taskIntegrate(projectId, taskId, target);
      shipMsg = resp.detail;
    } catch (e) {
      shipError = `${e}`;
    } finally {
      shipping = "";
    }
  }
  async function shipTask(kind: "push" | "pr") {
    const projectId = studioSocket?.project_id;
    const taskId = task?.task_id;
    if (!projectId || !taskId || shipping) return;
    shipping = kind;
    shipMsg = "";
    shipError = "";
    try {
      const resp = kind === "push" ? await node.taskPush(projectId, taskId) : await node.taskPr(projectId, taskId);
      shipMsg = resp.detail;
    } catch (e) {
      shipError = `${e}`;
    } finally {
      shipping = "";
    }
  }
  const canSignOff = $derived(isTaskSocket && task?.status === "awaiting_review" && !signingOff);

  async function signOff() {
    const projectId = studioSocket?.project_id;
    const taskId = task?.task_id;
    if (!canSignOff || !projectId || !taskId) return;
    signingOff = true;
    signoffError = "";
    try {
      // Mandatory fresh comparison (Phase 4 item 3: sign-off is one of the
      // three surfaces that must never act on a stale snapshot). Nothing
      // refetches `review` when a proposal is accepted/rejected in this very
      // pane, or when a terminal/editor writes into the task worktree — the
      // panel the human is looking at can already differ from what a
      // `git add -A` + commit would pin. Re-fetch and compare BEFORE
      // confirming; a mismatch replaces the panel with the fresh diff and
      // refuses to sign off until the human looks at it and clicks again.
      const fresh = await node.taskReview(projectId, taskId);
      if (reviewFilesChanged(review, fresh)) {
        review = fresh;
        baselineRefusal = "";
        signoffError = "The reviewed files changed since this panel loaded — showing the current diff. Review it and sign off again.";
        return;
      }
      // 07d-B Phase 4 item 2 explicitly wants this spelled out at the point
      // of action, not just implied by the button's presence: nothing
      // merges without the human, and sign-off is the one-way door into
      // that path.
      const ok = await ask(
        "Sign off this task? Nothing merges without you — this pins the reviewed state as a commit and marks the task done.",
      );
      if (!ok) return;
      // The server re-validates this snapshot id against a fresh recompute
      // under the task's execution locks — the client-side compare above is
      // UX; this is the authority (a mismatch refuses instead of silently
      // committing whatever is on disk by then).
      await node.taskSignoff(projectId, taskId, fresh.snapshot);
      // Re-fetch rather than trust the response alone: sign-off recomputes
      // fresh server-side, and this keeps `review.signed_off`/`files` in
      // sync with exactly what got committed.
      review = await node.taskReview(projectId, taskId);
    } catch (e) {
      signoffError = `${e}`;
    } finally {
      signingOff = false;
    }
  }

  function signoffTitle(): string {
    if (signingOff) return "Signing off…";
    if (!task) return "No task loaded yet";
    if (task.status !== "awaiting_review") {
      return `Sign-off requires the task to be awaiting review (currently ${task.status.replace("_", " ")})`;
    }
    return "Commit exactly what's shown below and mark this task done";
  }

  const proposalsLive = $derived(orderProposals(items));
  const pending = $derived(proposalsLive.filter((p) => p.verdict === "pending"));
  const queueKey = $derived(proposalsLive.map((p) => `${p.hash}:${p.verdict}`).join("|"));

  let focusedHash = $state<string | undefined>(undefined);
  const focused = $derived(proposalsLive.find((p) => p.hash === focusedHash));

  // Keep the current selection while it's still in the queue; otherwise
  // default to the top (pending first). Never sticks to a hash the queue no
  // longer has.
  $effect(() => {
    const next = pickFocus(proposalsLive, focusedHash);
    if (next !== focusedHash) focusedHash = next;
  });

  // After the default above, so a requested change wins. A request for a
  // change not in the list yet waits for it rather than being dropped.
  let appliedFocusSeq = 0;
  $effect(() => {
    const req = focusRequest;
    if (!req || req.seq === appliedFocusSeq) return;
    if (!proposalsLive.some((p) => p.hash === req.hash)) return;
    appliedFocusSeq = req.seq;
    focusedHash = req.hash;
  });

  let acceptingAll = $state(false);
  let deciding = $state(false);
  let decisionError = $state("");
  let editingHash = $state<string | null>(null);
  const drafts = new Map<string, string>();
  let listEl = $state<HTMLDivElement | undefined>();

  // Stick the list to the top when the queue's identity changes (a new
  // proposal arrives, or a verdict lands) — the live queue stays in view
  // without the human hunting for it.
  $effect(() => {
    void queueKey;
    const el = listEl;
    if (!el) return;
    requestAnimationFrame(() => {
      el.scrollTop = 0;
    });
  });

  // Hashes the focus pane has actually shown — "read first" for Accept all,
  // now that only one diff is ever visible at a time. Imperative, not
  // reactive state: consulted only inside acceptAll(), never bound in markup.
  const viewed = new Set<string>();

  let diffEl = $state<HTMLDivElement | undefined>();
  let diffEditor: monaco.editor.IStandaloneDiffEditor | undefined;
  let diffOriginal: monaco.editor.ITextModel | undefined;
  let diffModified: monaco.editor.ITextModel | undefined;

  function teardownDiff() {
    diffEditor?.dispose();
    diffEditor = undefined;
    diffOriginal?.dispose();
    diffOriginal = undefined;
    diffModified?.dispose();
    diffModified = undefined;
  }

  $effect(() => {
    const p = focused;
    const host = diffEl;
    if (!p || !host) {
      untrack(teardownDiff);
      return;
    }
    untrack(() => {
      teardownDiff();
      const lang = languageFor(p.filePath);
      diffOriginal = monaco.editor.createModel(p.oldContent, lang);
      diffModified = monaco.editor.createModel(p.verdict === "pending" ? drafts.get(p.hash) ?? p.newContent : p.newContent, lang);
      const modified = diffModified;
      modified.onDidChangeContent(() => drafts.set(p.hash, modified.getValue()));
      diffEditor = monaco.editor.createDiffEditor(host, {
        theme: "bakery-dark",
        readOnly: true,
        renderSideBySide: true,
        useInlineViewWhenSpaceIsLimited: false,
        automaticLayout: true,
      });
      diffEditor.setModel({ original: diffOriginal, modified: diffModified });
      viewed.add(p.hash);
    });
  });

  $effect(() => {
    // A proposal's original side is always read-only. Editing changes only
    // a local draft until the human explicitly submits the acceptance.
    const editable = focused?.verdict === "pending" && editingHash === focused.hash && !busy && !deciding;
    diffEditor?.updateOptions({ readOnly: !editable, originalEditable: false });
  });

  async function decideOne(hash: string, accept: boolean) {
    if (busy || deciding || acceptingAll) return;
    decisionError = "";
    deciding = true;
    try {
      await onDecide(hash, accept, accept ? drafts.get(hash) : undefined);
      drafts.delete(hash);
      if (editingHash === hash) editingHash = null;
    } catch (e) {
      decisionError = String(e); // keep draft and pending proposal for retry
    } finally {
      deciding = false;
    }
  }

  $effect(() => {
    return () => untrack(teardownDiff);
  });

  async function acceptAll() {
    if (busy || deciding || acceptingAll || pending.length === 0) return;
    // Gate says "read first": bulk-accept only after confirm if any pending
    // proposal has never been opened in the focus pane.
    const unread = pending.some((p) => !viewed.has(p.hash));
    if (unread) {
      const ok = await ask(
        `Accept all ${pending.length} proposals? Some have not been opened for review yet.`,
      );
      if (!ok) return;
    }
    acceptingAll = true;
    try {
      // Sequential: node serializes Heart anyway; client must keep busy honest
      // and not let a second click re-fire mid-flight.
      for (const p of pending) {
        await onDecide(p.hash, true, drafts.get(p.hash));
        drafts.delete(p.hash);
      }
    } catch (e) {
      decisionError = String(e);
    } finally {
      acceptingAll = false;
    }
  }

  function hashShort(hash: string): string {
    return hash.slice(0, 8);
  }

  function rowStats(p: Proposal) {
    return countChanges(diffLines(p.oldContent, p.newContent));
  }
</script>

<div class="review">
  {#if isTaskSocket}
    <div class="task-baseline">
      <div class="task-baseline-hdr">
        <span class="task-baseline-title">Task baseline</span>
        {#if task}
          <span class="status-chip status-{task.status}">{task.status.replace("_", " ")}</span>
        {/if}
        {#if review?.signed_off && review.signoff_commit}
          <code class="signoff-commit" title="Signed-off commit — proof and integration pin to this exact hash">{review.signoff_commit.slice(0, 12)}</code>
        {/if}
        <span class="spacer"></span>
        <button
          type="button"
          class="signoff-btn"
          disabled={!canSignOff}
          title={signoffTitle()}
          onclick={() => { void signOff(); }}
        >{signingOff ? "Signing off…" : "Sign off"}</button>
      </div>
      {#if task?.contract.scope}
        <p class="task-scope">{task.contract.scope}</p>
      {/if}
      {#if baselineRefusal}
        <p class="task-refusal">{baselineRefusal}</p>
      {/if}
      {#if signoffError}
        <p class="task-refusal">{signoffError}</p>
      {/if}

      {#if review?.signed_off && review.signoff_commit}
        <!-- Task shipping (confirmed gap 5): the confirmed, task-scoped
             action surface over /task/integrate|push|pr — the same server
             implementations, now operable without hand-crafted curl. All
             human-confirmed; the server refuses on any drift from the
             signed-off commit. -->
        <div class="ship-row" data-testid="task-ship-row">
          <button type="button" class="ship-btn" data-testid="task-integrate" disabled={shipping !== ""} onclick={() => { void integrateTask(); }}>
            {shipping === "integrate" ? "Integrating…" : `Integrate into ${review.repo_current_branch ?? "the checked-out branch"}`}
          </button>
          <button type="button" class="ship-btn" data-testid="task-push" disabled={shipping !== ""} onclick={() => { void shipTask("push"); }}>
            {shipping === "push" ? "Pushing…" : "Push branch"}
          </button>
          <button type="button" class="ship-btn" data-testid="task-pr" disabled={shipping !== ""} onclick={() => { void shipTask("pr"); }}>
            {shipping === "pr" ? "Opening PR…" : "Open PR"}
          </button>
        </div>
        {#if shipMsg}
          <p class="task-scope" data-testid="task-ship-result">{shipMsg}</p>
        {/if}
        {#if shipError}
          <p class="task-refusal" data-testid="task-ship-error">{shipError}</p>
        {/if}
      {/if}

      {#if review}
        {#if review.files.length === 0}
          <p class="task-scope">No changes yet in this task's worktree.</p>
        {:else}
          <div class="baseline-files">
            {#each review.files as f (f.path)}
              <button
                type="button"
                class="baseline-row"
                class:active={f.path === baselineFocusPath}
                onclick={() => { baselineFocusPath = f.path; }}
              >
                <code class="path">{f.path}</code>
                <span class="chip chip-status-{f.status}">{f.status}</span>
                {#if f.note}
                  <!-- Explicit handling of content the diff cannot fully
                       display: binary / truncated / refused / oversize is
                       SAID, never a blank pane that reads as "no change". -->
                  <span class="chip chip-muted" title={f.note}>{f.note}</span>
                {/if}
                <span class="add-ct">+{f.added}</span>
                <span class="del-ct">−{f.removed}</span>
              </button>
            {/each}
          </div>
          {#if baselineFocused}
            <div class="baseline-diff">
              {#each collapse(diffLines(baselineFocused.old_content, baselineFocused.new_content)) as line, i (i)}
                <div class="dl dl-{line.tag === ' ' ? 'ctx' : line.tag === '+' ? 'add' : line.tag === '-' ? 'del' : 'skip'}">{line.text}</div>
              {/each}
            </div>
          {/if}
        {/if}
      {/if}
    </div>
  {/if}

  <div class="review-hdr">
    {#if pending.length > 0}
      <span class="waiting">{pending.length} waiting on you</span>
    {:else if proposalsLive.length > 0}
      <span class="quiet">all reviewed</span>
    {:else}
      <span class="quiet">no proposals yet</span>
    {/if}
    <span class="spacer"></span>
    {#if pending.length > 1}
      <!-- warn tokens: bulk write is irreversible + multi-file. Gate is title/status chip, not here. -->
      <button
        type="button"
        class="hdr-btn warn-btn"
        disabled={busy || acceptingAll}
        onclick={() => { void acceptAll(); }}
      >Accept all</button>
    {/if}
  </div>

  <div class="body">
    <div class="list" bind:this={listEl}>
      {#if proposalsLive.length === 0}
        <p class="empty">No file proposals yet. The agent will ask for approval before writing.</p>
      {/if}
      {#each proposalsLive as p (p.hash)}
        {@const stats = rowStats(p)}
        <button
          type="button"
          class="row {p.verdict}"
          class:active={p.hash === focusedHash}
          class:write-failed={!!p.lastFailure}
          onclick={() => { focusedHash = p.hash; }}
        >
          <code class="path">{p.filePath}</code>
          <span class="add-ct">+{stats.add}</span>
          <span class="del-ct">−{stats.del}</span>
          {#if p.verdict === "accepted"}
            <span class="chip chip-ok">accepted</span>
          {:else if p.verdict === "rejected"}
            <span class="chip chip-muted">{p.cancelled ? "stopped" : "rejected"}</span>
          {:else if p.lastFailure}
            <span class="chip chip-bad">write failed</span>
          {/if}
        </button>
      {/each}
    </div>

    <div class="focus">
      {#if focused}
        {@const stats = rowStats(focused)}
        <div class="focus-hdr">
          <code class="path">{focused.filePath}</code>
          <span class="add-ct">+{stats.add}</span>
          <span class="del-ct">−{stats.del}</span>
          {#if focused.verdict === "accepted"}
            <span class="chip chip-ok">accepted</span>
          {:else if focused.verdict === "rejected"}
            <span class="chip chip-muted">{focused.cancelled ? "stopped" : "rejected"}</span>
          {:else if focused.lastFailure}
            <span class="chip chip-bad">write failed</span>
          {/if}
          {#if focused.verdict !== "pending" && focused.decidedBy}
            <span class="decided-by">by {actorLabel(focused.decidedBy)}</span>
          {/if}
          <span class="hash-ref">{hashShort(focused.hash)}</span>
        </div>

        {#if focused.agentId || focused.modelId}
          <!-- Named identity, not "the model authored it" — role/agent_id first. -->
          <p class="author">
            {focused.agentId ?? "agent"}{#if focused.modelId}<span class="author-model"> · {focused.modelId}</span>{/if}
          </p>
        {/if}

        {#if focused.rationale}
          <p class="rationale">{focused.rationale}</p>
        {/if}

        {#if focused.lastFailure}
          <p class="fail-reason">Last attempt failed: {focused.lastFailure} — still pending, retry below.</p>
        {/if}
      {:else}
        <p class="empty focus-empty">Select a proposal from the list.</p>
      {/if}

      <div class="diff-host" class:hidden={!focused} bind:this={diffEl}></div>

      {#if decisionError}<p class="fail-reason" role="alert">{decisionError}</p>{/if}
      {#if focused && focused.verdict === "pending"}
        <div class="actions">
          <button
            type="button"
            class="act accept"
            disabled={busy || deciding || acceptingAll}
            onclick={() => { void decideOne(focused.hash, true); }}
          >{editingHash === focused.hash ? "Accept edited content · write to disk" : "Accept · write to disk"}</button>
          <button
            type="button"
            class="act reject"
            disabled={busy || deciding || acceptingAll}
            onclick={() => { void decideOne(focused.hash, false); }}
          >Reject</button>
          <button type="button" class="act ghost" disabled={busy || deciding || acceptingAll}
            onclick={() => { editingHash = focused.hash; diffEditor?.getModifiedEditor().focus(); }}
          >Edit then accept</button>
        </div>
      {/if}
    </div>
  </div>
</div>

<style>
  .review {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
    overflow: hidden;
  }

  /* ---- task baseline header (07d-B Phase 4 item 2) ---- */

  .task-baseline {
    flex: 0 0 auto;
    padding: 8px 10px;
    background: var(--violet-bg);
    border-bottom: 1px solid var(--baker-border, var(--line-chrome));
  }
  .task-baseline-hdr {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .task-baseline-title {
    font: 600 10.5px/1 var(--font-ui);
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--violet);
  }
  .status-chip {
    padding: 2px 6px;
    border-radius: var(--r-xs);
    font: 500 9px/1 var(--font-mono);
    text-transform: uppercase;
    letter-spacing: 0.03em;
    background: var(--bg-chip);
    color: var(--fg-dim);
  }
  .status-chip.status-blocked { background: var(--warn-bg); color: var(--warn); }
  .status-chip.status-awaiting_review { background: var(--violet-bg); color: var(--violet); }
  .status-chip.status-done { background: var(--ok-bg); color: var(--ok); }
  .status-chip.status-cancelled { background: var(--bad-bg); color: var(--bad); }
  .signoff-commit {
    font: 500 10px/1 var(--font-mono);
    color: var(--fg-dim);
    background: var(--bg-chip);
    padding: 2px 6px;
    border-radius: var(--r-xs);
  }
  .ship-row {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin: 6px 0;
  }
  .ship-btn {
    font: inherit;
    font-size: 12px;
    padding: 3px 12px;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: transparent;
    color: inherit;
    cursor: pointer;
  }
  .ship-btn:hover:not(:disabled) { border-color: var(--fg-muted); }
  .ship-btn:disabled { opacity: 0.5; cursor: default; }

  .signoff-btn {
    background: var(--ok-bg);
    border: 1px solid var(--ok-border);
    color: var(--ok);
    cursor: pointer;
    font: 500 10.5px/1 var(--font-ui);
    padding: 4px 10px;
    border-radius: var(--r-sm);
  }
  .signoff-btn:hover:not(:disabled) { opacity: 0.85; }
  .signoff-btn:disabled {
    background: none;
    border-color: var(--line);
    color: var(--fg-faint);
    cursor: not-allowed;
    opacity: 0.55;
  }
  .task-scope {
    margin: 6px 0 0;
    font: 400 11.5px/1.4 var(--font-ui);
    color: var(--fg-dim);
  }
  .task-refusal {
    margin: 4px 0 0;
    font: 400 10.5px/1.4 var(--font-ui);
    color: var(--fg-faint);
  }

  .baseline-files {
    margin-top: 8px;
    max-height: 110px;
    overflow-y: auto;
    border: 1px solid var(--line-soft);
    border-radius: var(--r-sm);
  }

  .baseline-row {
    display: flex;
    align-items: center;
    gap: 6px;
    width: 100%;
    padding: 4px 7px;
    border: none;
    border-bottom: 1px solid var(--line-row);
    background: none;
    cursor: pointer;
    text-align: left;
  }
  .baseline-row:last-child { border-bottom: none; }
  .baseline-row:hover { background: var(--bg-chip); }
  .baseline-row.active { background: var(--bg-raised); box-shadow: inset 2px 0 0 var(--violet); }

  .chip-status-added { background: var(--ok-bg); color: var(--ok); }
  .chip-status-modified { background: var(--violet-bg); color: var(--violet); }
  .chip-status-deleted { background: var(--bad-bg); color: var(--bad); }
  .chip-status-unreviewable { background: var(--warn-bg); color: var(--warn); }

  .baseline-diff {
    margin-top: 6px;
    max-height: 220px;
    overflow: auto;
    border: 1px solid var(--line-soft);
    border-radius: var(--r-sm);
    padding: 4px 0;
    background: var(--bg-panel);
  }

  .dl {
    padding: 0 8px;
    font: 400 11px/1.5 var(--font-mono);
    white-space: pre-wrap;
    word-break: break-all;
  }
  .dl-ctx { color: var(--fg-faint); }
  .dl-add { color: var(--ok); background: var(--ok-bg); }
  .dl-del { color: var(--bad); background: var(--bad-bg); }
  .dl-skip {
    color: var(--fg-fainter);
    font-style: italic;
    text-align: center;
  }

  /* ---- widget header ---- */

  .review-hdr {
    flex: 0 0 30px;
    display: flex;
    align-items: center;
    gap: 5px;
    padding: 0 10px;
    background: var(--bg-panel);
    border-bottom: 1px solid var(--line-chrome);
  }

  .waiting {
    font: 600 11px/1 var(--font-ui);
    color: var(--warn);
    flex-shrink: 0;
  }

  .quiet {
    font: 400 11px/1 var(--font-ui);
    color: var(--fg-faint);
    flex-shrink: 0;
  }

  .spacer { flex: 1 1 auto; }

  .hdr-btn {
    background: none;
    border: 1px solid transparent;
    cursor: pointer;
    font: 400 10.5px/1 var(--font-ui);
    padding: 3px 8px;
    border-radius: var(--r-sm);
    flex-shrink: 0;
  }

  .hdr-btn.warn-btn {
    background: var(--warn-bg);
    color: var(--warn);
    border-color: var(--warn-border);
  }
  .hdr-btn.warn-btn:disabled {
    opacity: 0.45;
    cursor: default;
  }

  /* ---- body: file list + focus pane ---- */

  .body {
    flex: 1 1 auto;
    min-height: 0;
    display: flex;
    overflow: hidden;
  }

  /* ---- proposal list (left) ---- */

  .list {
    flex: 0 0 200px;
    min-width: 0;
    overflow-y: auto;
    overscroll-behavior: contain;
    border-right: 1px solid var(--line-chrome);
    display: flex;
    flex-direction: column;
  }

  .row {
    display: flex;
    align-items: center;
    gap: 5px;
    padding: 6px 8px;
    border: none;
    border-bottom: 1px solid var(--line-row);
    background: none;
    cursor: pointer;
    text-align: left;
    flex-wrap: wrap;
  }

  .row:hover { background: var(--bg-chip); }
  .row.active { background: var(--bg-raised); box-shadow: inset 2px 0 0 var(--accent); }

  .row.accepted, .row.rejected { opacity: 0.65; }
  .row.write-failed { box-shadow: inset 2px 0 0 var(--bad); }
  .row.active.write-failed { box-shadow: inset 2px 0 0 var(--bad); }

  .row .path {
    font: 400 11px/1.3 var(--font-mono);
    color: var(--fg-2);
    flex: 1 1 100%;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  /* ---- focus pane (right) ---- */

  .focus {
    flex: 1 1 auto;
    min-width: 0;
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }

  .empty {
    font: 400 11.5px/1.5 var(--font-ui);
    color: var(--fg-faint);
    font-style: italic;
    text-align: center;
    margin: 24px auto;
  }

  .focus-empty { margin: auto; }

  .focus-hdr {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 6px 10px;
    background: var(--bg-raised);
    border-bottom: 1px solid var(--line-chrome);
    flex-wrap: wrap;
    flex-shrink: 0;
  }

  .path {
    font: 400 11.5px/1 var(--font-mono);
    color: var(--fg-2);
    flex: 1 1 auto;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .add-ct {
    font: 600 11px/1 var(--font-mono);
    color: var(--ok);
    font-variant-numeric: tabular-nums;
    flex-shrink: 0;
  }

  .del-ct {
    font: 600 11px/1 var(--font-mono);
    color: var(--bad);
    font-variant-numeric: tabular-nums;
    flex-shrink: 0;
  }

  .chip {
    font: 500 9px/1 var(--font-ui);
    text-transform: uppercase;
    letter-spacing: 0.04em;
    padding: 2px 5px;
    border-radius: var(--r-xs);
    flex-shrink: 0;
  }

  .chip-ok   { background: var(--ok-bg);   color: var(--ok); }
  .chip-muted { background: var(--bg-chip); color: var(--fg-faint); }
  .chip-bad  { background: var(--bad-bg);  color: var(--bad); }

  .decided-by {
    font: 400 10px/1 var(--font-mono);
    color: var(--fg-faint);
    flex-shrink: 0;
  }

  /* ---- author / rationale / fail reason ---- */

  .author {
    font: 400 11px/1.5 var(--font-ui);
    color: var(--fg-faint);
    padding: 6px 10px 0;
    margin: 0;
    flex-shrink: 0;
  }

  .author-model { color: var(--fg-fainter); }

  .rationale {
    font: 400 12px/1.55 var(--font-ui);
    color: var(--fg-dim);
    padding: 4px 10px 0;
    margin: 0;
    flex-shrink: 0;
  }

  .fail-reason {
    font: 400 11.5px/1.45 var(--font-ui);
    color: var(--bad);
    padding: 4px 10px;
    margin: 0;
    flex-shrink: 0;
  }

  /* ---- split diff ---- */

  .diff-host {
    flex: 1 1 auto;
    min-height: 0;
    margin: 8px 10px;
    border: 1px solid var(--line-soft);
    border-radius: var(--r-lg);
    overflow: hidden;
  }

  .diff-host.hidden { visibility: hidden; }

  /* ---- actions row ---- */

  .actions {
    display: flex;
    align-items: center;
    gap: 5px;
    padding: 6px 10px 8px;
    flex-wrap: wrap;
    flex-shrink: 0;
  }

  .act {
    background: none;
    border: 1px solid var(--line);
    cursor: pointer;
    font: 400 11px/1 var(--font-ui);
    padding: 4px 10px;
    border-radius: var(--r-sm);
  }

  .act.ghost { color: var(--fg-faint); }
  .act.ghost:hover:not(:disabled) { background: var(--bg-chip); color: var(--fg-2); }

  .act.accept {
    background: var(--ok-bg);
    color: var(--ok);
    border-color: var(--ok-border);
  }
  .act.accept:hover:not(:disabled) { opacity: 0.85; }

  .act.reject {
    color: var(--bad);
    border-color: var(--bad-border);
  }
  .act.reject:hover:not(:disabled) { background: var(--bad-bg); }

  .act:disabled { opacity: 0.4; cursor: default; }

  .hash-ref {
    margin-left: auto;
    font: 400 10px/1 var(--font-mono);
    color: var(--fg-linenum);
    letter-spacing: 0.03em;
    flex-shrink: 0;
  }
</style>
