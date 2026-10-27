<script lang="ts">
  import { goto } from "$app/navigation";
  import { page } from "$app/state";
  import SettingsFrame from "$lib/controlplane/SettingsFrame.svelte";
  import SettingsContent, { MOVED } from "$lib/controlplane/SettingsContent.svelte";

  // A reload or a pasted /settings link lands here. In-app links open the
  // same frame as a popup over the page you are on instead (+layout.svelte).
  const section = $derived(page.params.section ?? "");

  $effect(() => {
    const to = MOVED[section];
    if (to) void goto(`/settings/${to}`, { replaceState: true });
  });
</script>

<SettingsFrame>
  {#if !MOVED[section]}
    <SettingsContent {section} />
  {/if}
</SettingsFrame>
