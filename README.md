# The Bakery

The Bakery is a desktop workspace for building software with AI agents. It is an IDE, a container manager and a git client in one app. Every model and agent runtime runs in the same harness: Claude, Grok, Gemini, OpenAI-compatible and local models, through their APIs or their own CLIs. The Baker, a permanent agent, can drive the whole thing for you.

It has two parts:

- **`bakery-node`**: a Rust daemon. It owns the Knowledge Heart (a SQLite event log), git, the filesystem, the agent loop and every sign-in. It listens on `127.0.0.1:7717`.
- **`bakery-client`**: the apps. Electron is the Mac app; a Tauri shell is for the iPhone and Android clients, which reach a Mac's node over Tailscale. Both run the same SvelteKit and React UI. It holds no Heart, no git and no credentials.

## Status

Unfinished. It is one person's daily desktop, shared so friends can pick it up and improve it. Expect rough edges.

It runs on **macOS only** today. Linux and Windows support is in progress.

## Get the code

```sh
git clone https://github.com/thecandylane/thebakery.git
cd thebakery
```

## Requirements

On a fresh Mac:

- Xcode Command Line Tools (`xcode-select --install`), for git and the linker.
- Rust 1.98.0. `mise.toml` pins it: with [mise](https://mise.jdx.dev), run `mise install` in the checkout. `rustup` works too.
- Node.js 24 and pnpm. The client's checks run `.ts` files with plain `node`.
- Optional: [Homebrew](https://brew.sh). The app uses it to install OrbStack when you ask.
- Optional: Docker through OrbStack or Colima, or Apple's `container`, for containers.
- Optional: `rg` (ripgrep), used by `scripts/gate.sh`.

Then install the client's dependencies once:

```sh
pnpm --dir bakery-client install
```

## Run it

Everything goes through `./scripts/bakery.sh`. Run `./scripts/bakery.sh help` for the full list.

```sh
./scripts/bakery.sh start
```

Until you deploy, this runs from source: `cargo run -p bakery-node`, Vite on port 1420, and the Electron app against it. If something is already running, it asks whether to reuse it or restart.

Other commands:

| Command | What it does |
|---------|--------------|
| `./scripts/bakery.sh dev` | Rebuild the debug node, swap it in only if the build passes, and run the app against Vite with hot reload. Run it again to pick up Rust edits. |
| `./scripts/bakery.sh deploy` | Build a release node and `Bakery.app` from source. Only if both builds pass, switch to them and restart. Add `--checked` to run the node's tests and the client's checks first. |
| `./scripts/bakery.sh status` | What is running, the deployed build, and source changes not deployed yet. |
| `./scripts/bakery.sh rollback` | Switch back to the previous deployed build. |
| `./scripts/bakery.sh kill` | Stop the node and the app. |
| `./scripts/bakery.sh service install` | Run the deployed node as a per-user LaunchAgent, so it starts at login. `service remove` undoes it. |
| `./scripts/bakery.sh uninstall` | Remove what the Bakery made. It asks whether to keep your work. |

Once you deploy, the app runs from the deployed build in `~/.bakery/deploy/current`, not from your checkout. Edits change the source only. The app changes when you deploy again.

`BAKERY_NODE_ADDR` changes the node's address. `BAKERY_DRY_RUN=1` prints what the script would do and does nothing.

## Where it keeps its state

Everything lives in `~/.bakery`:

- `heart.db`: the Heart, every conversation and event.
- `node.json`: this node's identity and the app's operator token.
- `secrets.json` (mode 0600): API keys and sign-ins.
- `worktrees/`, `projects/`, `chats/`: your work.
- `deploy/`: the deployed builds.
- `logs/node.log`: the node's log.

Dev logs from the script go to `.bakery-dev-logs/` in the checkout.

## Sign-ins and provider keys

No credential is in this repository. Every sign-in lives with the node, outside every worktree, so agents can't read it. Credentials never enter the Heart.

Add keys and sign-ins from the app's Settings, or set environment variables before starting the node:

| Provider | How |
|----------|-----|
| Anthropic API (Claude) | `ANTHROPIC_API_KEY`, or the key added in the app. This is how you use Claude. |
| xAI / Grok | `XAI_API_KEY`, or device sign-in from the app |
| Gemini | `GEMINI_API_KEY`, or the Gemini CLI's sign-in. The sign-in path also needs Google's desktop client secret in `BAKERY_GEMINI_CLIENT_SECRET` (or `gemini_client_secret` in `secrets.json`). It is not in the source. |
| OpenAI-compatible (Ollama, llama.cpp, vLLM and others) | A base URL, a model id and an optional key, set in the app |

Agents run in sandboxed rooms. A room never holds a real credential. It gets a stand-in, and the node swaps in the real one at the room's door, only for requests to that sign-in's own hosts.

See [SECURITY.md](SECURITY.md) for the rules.

## Tests

| Command | What it runs |
|---------|--------------|
| `scripts/test.sh -p bakery-node` | One crate's tests, each with a time limit |
| `scripts/test.sh --workspace` | Every crate's tests |
| `scripts/test-changed.sh` | Only the tests your uncommitted changes touch |
| `pnpm --dir bakery-client check:all` | The client's type-checks and checks |
| `./scripts/gate.sh` | The full gate: node, sandbox and client |

`scripts/test.sh` takes `cargo test`'s own arguments. `CARGO_BUILD_JOBS` limits build load; it defaults to 6.

## Repository layout

| Path | What it is |
|------|------------|
| `crates/bakery-core` | The event envelope, hashing and chain verification. No workspace dependencies. |
| `crates/bakery-heart` | The SQLite event store. |
| `crates/bakery-node` | The daemon: HTTP and WebSocket API, agent loop, git, containers, sign-ins. |
| `crates/bakery-agent` | The model providers: Claude, Grok, Gemini, OpenAI-compatible, and an offline stub. |
| `crates/bakery-exec` | Rooms: the sandboxed environments where agents and terminals run. |
| `bakery-client` | The apps: `electron/` is the Mac app, `src-tauri/` the Tauri shell for the phone clients, `src/` the UI. See [bakery-client/README.md](bakery-client/README.md). |
| `containers` | The workspace container image, and the `gh` and git push helper that container rooms use to reach the node. |
| `scripts` | `bakery.sh` and the test scripts. |
| `docs`, top-level `*.md` | Design notes and specs. Some are out of date. |

## Contributing

Contributions come as pull requests to [thecandylane/thebakery](https://github.com/thecandylane/thebakery). The maintainer reviews and merges them.

1. Fork the repository and branch from `main`.
2. Make your change. Run the tests it touches: `scripts/test-changed.sh`.
3. Before a pull request, run `./scripts/gate.sh`.
4. Never commit keys, tokens, `.env` files or anything from `~/.bakery`.
5. Open the pull request against `main` on thecandylane/thebakery.

Small, focused pull requests are easiest to review. If you run coding agents on the repo, they follow [AGENTS.md](AGENTS.md) (and [CLAUDE.md](CLAUDE.md)). For prior art before building something, see [REFERENCE_INDEX.md](REFERENCE_INDEX.md) and [ref.lookit.space](https://ref.lookit.space).

## License

MIT. See [LICENSE](LICENSE). Third-party code in the tree keeps its own license: see [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
