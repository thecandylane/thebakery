// Tauri doesn't have a Node.js server to do proper SSR
// so we use adapter-static with a fallback to index.html to put the site in SPA mode
// See: https://svelte.dev/docs/kit/single-page-apps
// See: https://v2.tauri.app/start/frontend/sveltekit/ for more info
import adapter from "@sveltejs/adapter-static";
import { vitePreprocess } from "@sveltejs/vite-plugin-svelte";

/** @type {import('@sveltejs/kit').Config} */
const config = {
  preprocess: vitePreprocess(),
  kit: {
    adapter: adapter({
      fallback: "index.html",
    }),
    // The vendored upstream sources import `@/components/ui/*` and
    // `@/lib/utils` — shadcn's conventional root alias. Declaring it here
    // (rather than hand-editing tsconfig paths) means SvelteKit feeds it to
    // both Vite's resolver and the generated tsconfig, so runtime and
    // type-checking agree.
    alias: { "@": "src" },
  },
};

export default config;
