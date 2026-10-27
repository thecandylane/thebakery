<script module lang="ts">
  /** Addresses of merged pages land on the page that now holds them. */
  export const MOVED: Record<string, string> = {
    seats: "agents",
    notifications: "agents",
    "write-mode": "approvals",
    tools: "machine-access",
    "agent-storage": "memory",
  };
</script>

<script lang="ts">
  import AgentsPanel from "./AgentsPanel.svelte";
  import ApprovalsPanel from "./ApprovalsPanel.svelte";
  import MachineAccessPanel from "./MachineAccessPanel.svelte";
  import MemoryPanel from "./MemoryPanel.svelte";
  import AgentStoragePanel from "./AgentStoragePanel.svelte";
  import ProvidersPanel from "./ProvidersPanel.svelte";
  import VersionsPanel from "./VersionsPanel.svelte";
  import AppearancePanel from "./AppearancePanel.svelte";
  import MachinesPanel from "./MachinesPanel.svelte";
  import OutsideWindowPanel from "./OutsideWindowPanel.svelte";
  import NodesStreams from "./NodesStreams.svelte";
  import KnowledgeHeart from "./KnowledgeHeart.svelte";
  import StubPanel from "./StubPanel.svelte";

  // One page per concern — no two pages edit the same thing. Shared by the
  // /settings route and the Settings popup (+layout.svelte).
  let { section }: { section: string } = $props();
  const key = $derived(MOVED[section] ?? section);
</script>

{#if key === "agents"}
  <AgentsPanel />
{:else if key === "approvals"}
  <ApprovalsPanel />
{:else if key === "machine-access"}
  <MachineAccessPanel />
{:else if key === "memory"}
  <MemoryPanel />
  <AgentStoragePanel />
{:else if key === "providers"}
  <ProvidersPanel />
{:else if key === "versions"}
  <VersionsPanel />
{:else if key === "appearance"}
  <AppearancePanel />
{:else if key === "machines"}
  <MachinesPanel />
{:else if key === "developer"}
  <OutsideWindowPanel />
  <NodesStreams />
  <KnowledgeHeart />
{:else}
  <StubPanel heading="Not found" subhead="This settings section does not exist." note="Pick a section on the left." />
{/if}
