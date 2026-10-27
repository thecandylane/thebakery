<script lang="ts">
  import { untrack } from "svelte";
  import { goto } from "$app/navigation";
  import { mountShell } from "$lib/react/mount";
  import { createShellSource } from "$lib/react/shell/shellSource.svelte";
  import { studioStore } from "$lib/studioStore.svelte";

  // The new React shell (bakery-ui slice 1) — the whole window, beside the
  // classic app until the operator switches. The root layout draws no chrome
  // of its own on this route.

  let host: HTMLDivElement | undefined = $state();

  $effect(() => {
    const el = host;
    if (!el) return;
    studioStore.ensureStarted();
    return untrack(() => mountShell(el, createShellSource({ go: (path) => void goto(path) })));
  });
</script>

<div class="next-shell" bind:this={host}></div>

<style>
  .next-shell {
    height: 100dvh;
  }
</style>
