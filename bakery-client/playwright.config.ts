import { defineConfig } from "@playwright/test";
import { fileURLToPath } from "node:url";
import path from "node:path";
import fs from "node:fs";
import os from "node:os";
import net from "node:net";

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const bakeryNodeBin = path.join(repoRoot, "target", "debug", "bakery-node");
if (!fs.existsSync(bakeryNodeBin)) {
  throw new Error(`bakery-node binary not found at ${bakeryNodeBin} — run \`cargo build -p bakery-node\` first`);
}

/** Binds to port 0 (the OS picks a free ephemeral port), reads it back, and
 *  releases it immediately — the caller then passes that number to the real
 *  server with `--strictPort`/an explicit bind address, so a lost race against
 *  another concurrent allocator fails loudly (a bind error) rather than
 *  silently drifting to a different port. Never kills or inspects anything
 *  already listening anywhere — this only ever asks the OS for a number
 *  nothing is using *at this instant*. */
async function getFreePort(exclude: readonly number[] = []): Promise<number> {
  for (;;) {
    const port = await new Promise<number>((resolve, reject) => {
      const srv = net.createServer();
      srv.unref();
      srv.on("error", reject);
      srv.listen(0, "127.0.0.1", () => {
        const address = srv.address();
        if (address && typeof address === "object") {
          const p = address.port;
          srv.close(() => resolve(p));
        } else {
          srv.close();
          reject(new Error("failed to allocate a free port"));
        }
      });
    });
    if (!exclude.includes(port)) return port;
  }
}

// Playwright reloads this file in every worker process it spawns, not just
// once in its own orchestrator process — so anything here with a real side
// effect (picking a random free port, minting a scratch HOME) must run
// exactly once, in whichever process reaches it first, and every later
// reload — crucially including the worker process(es) that actually run the
// tests — must inherit that same choice rather than silently recomputing its
// own. A worker that picked its own different port here would still build
// `baseURL`/`NODE_BASE` from ITS OWN wrong port, never the one the real
// webServer processes actually bound to. `__BAKERY_E2E_NODE_PORT` already
// being set is exactly how a later reload recognizes "the first evaluation
// already decided this" — workers are children of the process that set it,
// so plain env inheritance carries it forward; nothing here needs IPC.
let nodePort: number;
let vitePort: number;
let scratchHome: string;

if (process.env.__BAKERY_E2E_NODE_PORT && process.env.__BAKERY_E2E_VITE_PORT && process.env.__BAKERY_E2E_HOME) {
  nodePort = Number(process.env.__BAKERY_E2E_NODE_PORT);
  vitePort = Number(process.env.__BAKERY_E2E_VITE_PORT);
  scratchHome = process.env.__BAKERY_E2E_HOME;
} else {
  nodePort = await getFreePort();
  vitePort = await getFreePort([nodePort]);

  // Isolated ~/.bakery for the E2E daemon — never the real one. bakery-node
  // only reads HOME (crates/bakery-node/src/main.rs) for its base dir, and
  // Heart/cache/worktrees all live under it (`state::cache_root`, `main.rs`'s
  // `worktree_base`), so overriding HOME alone already isolates every one of
  // those per invocation — no separate env var needed for any of them.
  scratchHome = fs.mkdtempSync(path.join(os.tmpdir(), "bakery-e2e-home-"));
  fs.mkdirSync(path.join(scratchHome, ".bakery"), { recursive: true });

  process.env.__BAKERY_E2E_NODE_PORT = String(nodePort);
  process.env.__BAKERY_E2E_VITE_PORT = String(vitePort);
  process.env.__BAKERY_E2E_HOME = scratchHome;

  // Only ever removes the one temp dir *this* invocation created above —
  // never anything a concurrent invocation or a real running node owns. Both
  // webServer processes below are spawned (and torn down) by Playwright
  // itself (`reuseExistingServer: false`), so process cleanup needs no code
  // here; this only has to clean up the directory. Registered only in the
  // one process that actually created it (guarded by the `if` above) — a
  // worker that merely inherited this same path must never independently
  // delete it out from under whichever other process (the orchestrator, or
  // bakery-node itself) might still be using it.
  const homeToClean = scratchHome;
  process.on("exit", () => {
    try {
      fs.rmSync(homeToClean, { recursive: true, force: true });
    } catch {
      /* best-effort — a leaked temp dir is not worth failing the run over */
    }
  });
}

const nodeAddr = `127.0.0.1:${nodePort}`;
const viteOrigin = `http://127.0.0.1:${vitePort}`;

// `e2e/helpers.ts`'s `makeScratchRepo`/`makeScratchDir`/`freshNonexistentPath`
// mint their own scratch directories under this root instead of straight
// under the system temp dir — nesting it *inside* `scratchHome` means the one
// exit-time `rm -rf` above already removes every one of them, with nothing
// extra to track. Pure `path.join` of an already-resolved `scratchHome` —
// safe to recompute on every reload (main or worker), always the same value,
// and `{ recursive: true }` makes a worker's redundant re-creation a no-op.
const scratchRoot = path.join(scratchHome, "scratch");
fs.mkdirSync(scratchRoot, { recursive: true });
process.env.BAKERY_E2E_SCRATCH_ROOT = scratchRoot;

// Read by `e2e/helpers.ts` (`process.env.VITE_BAKERY_NODE_ADDR`) and, via the
// Vite webServer's own `env` below, by the browser bundle itself
// (`import.meta.env.VITE_BAKERY_NODE_ADDR`, `src/lib/node.ts`) — one value,
// two consumers, always in agreement.
process.env.VITE_BAKERY_NODE_ADDR = nodeAddr;
// Read by `e2e/helpers.ts` — the operator bearer every direct node request
// must carry now that the loopback surface is authenticated. Matches both
// webServer env blocks below.
// Runner-side only (no VITE_ prefix, so Vite never compiles it into the
// frontend bundle): `e2e/fixtures.ts` injects it into each page at runtime
// and `e2e/helpers.ts` uses it for direct node requests.
process.env.BAKERY_E2E_OPERATOR_TOKEN = "e2e-operator-token";

export default defineConfig({
  testDir: "./e2e",
  timeout: 30_000,
  expect: { timeout: 5_000 },
  // Serial: every spec shares one bakery-node daemon (real PTYs, real Heart)
  // rather than a mock — parallel specs would race the same worktree base.
  // (Two separate `playwright test` *invocations* — e.g. two terminals, or
  // CI running this suite twice at once — are a different story: each gets
  // its own fresh scratch HOME and its own fresh port pair from this same
  // config file, so they don't race each other either.)
  fullyParallel: false,
  workers: 1,
  retries: 0,
  reporter: [["list"]],
  use: {
    baseURL: viteOrigin,
    trace: "retain-on-failure",
  },
  projects: [{ name: "chromium", use: { browserName: "chromium" } }],
  webServer: [
    {
      // --host 127.0.0.1: plain `vite dev` binds "localhost", which on some
      // Node/OS combinations resolves IPv6-only (::1) — Playwright's own
      // readiness probe hits 127.0.0.1 explicitly and never sees it up.
      // --strictPort: fail loudly on a lost `getFreePort` race rather than
      // silently drifting to a different port than the rest of this config
      // agreed on.
      command: `pnpm exec vite dev --port ${vitePort} --strictPort --host 127.0.0.1`,
      url: viteOrigin,
      cwd: path.dirname(fileURLToPath(import.meta.url)),
      // Only the node's address — no credential. The bundle must never
      // carry one; pages get their token injected at runtime by
      // `e2e/fixtures.ts`.
      env: { VITE_BAKERY_NODE_ADDR: nodeAddr },
      reuseExistingServer: false,
      timeout: 30_000,
      stdout: "pipe",
      stderr: "pipe",
    },
    {
      command: bakeryNodeBin,
      url: `http://${nodeAddr}/health`,
      cwd: repoRoot,
      env: {
        HOME: scratchHome,
        BAKERY_NODE_ADDR: nodeAddr,
        // Narrowly validated server-side (state::extra_allowed_origin_from_env
        // — loopback http:// only, never a wildcard): lets this run's own
        // dynamically-ported Vite instance through the CORS/WS-origin
        // allowlist without ever widening it beyond that one exact origin.
        BAKERY_ALLOWED_ORIGIN: viteOrigin,
        // A knowable operator token for this run — overrides the scratch
        // node.json's minted one; helpers.ts and the Vite env agree on it.
        BAKERY_OPERATOR_TOKEN: "e2e-operator-token",
      },
      reuseExistingServer: false,
      timeout: 30_000,
      stdout: "pipe",
      stderr: "pipe",
    },
  ],
});
