<script lang="ts">
  import { untrack } from "svelte";
  import { mountMachines } from "$lib/react/mount";

  // Settings → Machines: this node, the nodes and phones paired with it,
  // reaching it from them over the tailnet, and pairing a phone. The same
  // React view as the shell's node box (SidebarParts.tsx).

  let host: HTMLDivElement | undefined = $state();

  $effect(() => {
    const el = host;
    if (!el) return;
    return untrack(() => mountMachines(el));
  });
</script>

<section class="set-section">
  <h2 class="set-section-h">Machines and phones</h2>
  <div bind:this={host}></div>
</section>
