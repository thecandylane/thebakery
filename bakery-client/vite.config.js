import { defineConfig } from "vite";
import { sveltekit } from "@sveltejs/kit/vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

const host = process.env.TAURI_DEV_HOST;

// https://vite.dev/config/
export default defineConfig(async () => ({
  plugins: [
    // `include` is deliberately narrowed to .tsx: this app is Svelte, and
    // the React plugin must never claim the ordinary .ts modules (stores,
    // node client, check scripts) that Svelte and plain `node` compile.
    // The React island is the agent panel and nothing else.
    react({ include: ["**/*.tsx"] }),
    tailwindcss(),
    sveltekit(),
  ],

  // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
  //
  // 1. prevent Vite from obscuring rust errors
  clearScreen: false,
  // 2. tauri expects a fixed port, fail if that port is not available
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1421,
        }
      : undefined,
    watch: {
      // 3. tell Vite to ignore watching `src-tauri`
      ignored: ["**/src-tauri/**"],
    },
  },
}));
