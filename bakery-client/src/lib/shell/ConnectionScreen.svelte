<script lang="ts">
  // The one place a connection problem is ever shown. Before this, a node
  // that was down or a credential the node rejected surfaced as whatever
  // raw 401/"failed to fetch" happened to reach a panel first — different
  // wording per surface, none of it actionable. Everything that can stop
  // the desktop shell from reaching its node now arrives here as one
  // structured reason with one Retry.
  import type { ConnectFailure } from "$lib/node";

  let {
    phase,
    failure = null,
    onRetry,
  }: {
    phase: "connecting" | "failed";
    failure?: ConnectFailure | null;
    onRetry: () => void;
  } = $props();

  /** What the human should DO — one line, specific to why it failed. */
  const hint = $derived.by(() => {
    switch (failure?.kind) {
      case "no_credential":
        return "The node mints its operator credential the first time it runs, so this usually means it has never started on this machine.";
      case "unauthorized":
        return "The node and this app disagree about the credential — restarting the node without BAKERY_OPERATOR_TOKEN makes them agree again.";
      case "unreachable":
        return "Bakery reuses a running node and starts one only when none is listening; it never stops anything already running.";
      case "misconfigured":
        return "This is a setup problem on this machine, not a Bakery state problem — your projects and history are untouched.";
      case "untrusted":
        return "Only Bakery's own main window may request operator access.";
      case "not_desktop":
        return "The desktop shell holds the credential natively; there is deliberately no browser sign-in.";
      default:
        return "";
    }
  });
</script>

<div class="connection-screen" data-testid="connection-screen">
  <div class="card">
    <div class="brand">Bakery</div>

    {#if phase === "connecting"}
      <div class="status" data-testid="connection-connecting">Connecting to your local node…</div>
      <p class="detail">Reusing a running node, or starting one if none is listening.</p>
    {:else}
      <div class="status failed" data-testid="connection-failed">Can't reach your Bakery node</div>
      <p class="detail" data-testid="connection-message">{failure?.message ?? "The connection attempt failed."}</p>
      {#if hint}
        <p class="hint">{hint}</p>
      {/if}
      <button type="button" class="retry" data-testid="connection-retry" onclick={onRetry}>Retry</button>
    {/if}
  </div>
</div>

<style>
  .connection-screen {
    height: 100dvh;
    display: flex;
    align-items: center;
    justify-content: center;
    background: var(--bg);
    color: var(--fg);
    padding: 24px;
  }

  .card {
    width: min(560px, 100%);
    border: 1px solid var(--border);
    border-radius: 10px;
    padding: 28px 30px;
    background: var(--bg-panel, transparent);
  }

  .brand {
    font-size: 13px;
    letter-spacing: 0.14em;
    text-transform: uppercase;
    color: var(--fg-ghost);
    margin-bottom: 14px;
  }

  .status {
    font-size: 17px;
    font-weight: 600;
    margin-bottom: 10px;
  }

  .status.failed {
    color: var(--warn);
  }

  .detail {
    margin: 0 0 10px;
    line-height: 1.5;
    color: var(--fg-muted);
    white-space: pre-wrap;
  }

  .hint {
    margin: 0 0 18px;
    line-height: 1.5;
    color: var(--fg-ghost);
    font-size: 13px;
  }

  .retry {
    font: inherit;
    font-size: 13px;
    padding: 6px 20px;
    border: 1px solid var(--border);
    border-radius: 5px;
    background: transparent;
    color: inherit;
    cursor: pointer;
  }

  .retry:hover {
    border-color: var(--fg-muted);
  }
</style>
