// Dev-only React Fast Refresh preamble.
//
// `@vitejs/plugin-react` normally injects this through Vite's
// `transformIndexHtml`, but SvelteKit renders its page shell through its own
// pipeline and never runs that hook — so under `vite dev` the preamble is
// missing and every transformed `.tsx` module throws
// "@vitejs/plugin-react can't detect preamble" the moment a component
// evaluates. The agent panel then never mounts at all.
//
// The alternatives are worse: `server.hmr: false` would disable HMR for the
// whole app including Svelte, and putting this in `app.html` unconditionally
// would 404 on `/@react-refresh` in a production build.
//
// Appearance bootstrap is also embedded here for both dev and static builds.
// It uses the same versioned decoder as settings, before the first paint.

import { dev } from "$app/environment";
import type { Handle } from "@sveltejs/kit";
import { appearanceBootstrapScript } from "$lib/themeModel";

const REACT_REFRESH_PREAMBLE = `<script type="module">
import RefreshRuntime from "/@react-refresh";
RefreshRuntime.injectIntoGlobalHook(window);
window.$RefreshReg$ = () => {};
window.$RefreshSig$ = () => (type) => type;
window.__vite_plugin_react_preamble_installed__ = true;
</script>`;

export const handle: Handle = ({ event, resolve }) =>
  resolve(event, {
    // Injected immediately after `<head>` so it evaluates before any module
    // SvelteKit adds below it.
    transformPageChunk: ({ html }) => {
      const themed = html.replace("/* bakery-appearance-bootstrap */", () => appearanceBootstrapScript());
      return dev ? themed.replace("<head>", `<head>${REACT_REFRESH_PREAMBLE}`) : themed;
    },
  });
