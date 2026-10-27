<script lang="ts">
  import { untrack } from "svelte";
  import { page } from "$app/state";
  import { mountWorkBoard } from "$lib/react/mount";
  import { createWorkBoardSource, type WorkScope } from "$lib/react/workBoardSource.svelte";
  import { runConversationAction } from "$lib/conversationActions";

  // Hosts the AI Elements work board (a React island) in a Svelte page. The
  // board mounts once per host; key the host on anything that changes its
  // scope.

  let { scope, emptyText = "Nothing here yet." }: { scope: WorkScope; emptyText?: string } = $props();
  let host: HTMLDivElement | undefined = $state();

  $effect(() => {
    const el = host;
    if (!el) return;
    return untrack(() =>
      mountWorkBoard(
        el,
        createWorkBoardSource(scope, {
          here: () => page.url.pathname,
          run: (id, conv) => runConversationAction(id, conv, page.url.pathname),
        }),
        emptyText,
      ),
    );
  });
</script>

<div class="work-board" bind:this={host}></div>
