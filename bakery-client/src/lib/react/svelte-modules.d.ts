// tsc (check:react) cannot read a .svelte file; svelte-check types them
// properly. This lets the island import one to mount it with Svelte's own
// `mount` (shell/svelteTerminal.ts).
declare module "*.svelte" {
  import type { Component } from "svelte";
  const component: Component<Record<string, unknown>>;
  export default component;
}
