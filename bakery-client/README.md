# bakery-client

The Bakery's UI and its shells. The UI (`src/`) is SvelteKit with React islands, built as a static SPA. It holds no Heart, no git and no credentials: every call goes to a `bakery-node`.

## Shells

| Shell | Where | What it is |
|-------|-------|------------|
| Electron | `electron/` | The Mac app. It connects to the node on the same Mac (`electron/connect.ts`). |
| Tauri | `src-tauri/` | The shell for the phone clients (iPhone and Android), which reach a Mac's node over Tailscale. |

## Build

Install once: `pnpm --dir bakery-client install`.

**Mac app (Electron).** `../scripts/bakery.sh` drives it:

- `./scripts/bakery.sh dev` runs `pnpm electron:dev` against Vite on port 1420, with hot reload.
- `./scripts/bakery.sh deploy` runs `pnpm build`, then `pnpm electron:pack`, which leaves `Bakery.app` under `electron/out` (`electron/builder.json`).

**Phone clients (Tauri).** `pnpm tauri` runs the Tauri CLI against `src-tauri/`. Its `beforeBundleCommand` stages the Claude ACP adapter with `scripts/stage-claude-native.mjs`. No iOS or Android project is checked in (`src-tauri/gen/` is ignored), and no script in this repo builds the phone apps yet.

On the node side, phones pair with a Mac's node in Settings → Machines: the node shows a one-time code, the phone trades it at `POST /pair` for a device token of its own, and the node serves its API to that phone over HTTPS on its tailnet address (`crates/bakery-node/src/devices.rs`).

## Checks

- `pnpm check:all`: type-checks and the client's checks.
- `pnpm test:e2e`: the Playwright suite.
- `cd src-tauri && cargo test --lib`: the Tauri shell's connect tests.
