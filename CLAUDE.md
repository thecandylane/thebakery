# Rules for coding agents on this repo

These apply to Claude Code and any other agent working in this repository. [AGENTS.md](AGENTS.md) has the same rules plus notes on providers and references.

## Never read secrets

Do not read, open, search, quote, diff or shell-cat:

### Outside the workspace

- `$HOME/.bakery/**`: the node's state, sign-ins, API keys and Heart
- Credential files of other tools, such as `$HOME/.claude/**`, `$HOME/.gemini/oauth_creds.json`, `$HOME/.codex/auth.json`, `$HOME/.cursor/**`
- `$HOME/.ssh/**` and any private key
- The OS keychain (`security find-generic-password` and the like)
- Any path matching `*claude_code_oauth*`, `*credentials*`, `oauth_creds*`, `secrets.json`

### Inside the workspace (also in `.claudeignore` and `.cursorignore`)

- `crates/bakery-agent/src/private/**` except `README.md`
- Any `.env`, `*.pem`, `auth.json`, `*-auth.json`, `heart.db*`
- `target/**`

## Never print secrets

- Never print, log, paste or commit API keys, refresh tokens, access tokens, client secrets or private keys, in source, tests, docs or commit messages.
- Editing the code that handles secrets (`secrets.rs`, auth routes that return booleans only) is fine. Real values never go into the repo.
- If a secret appears in a tool result by accident: **stop**, do not repeat it, and tell the human to rotate it.

## Providers

| Path | Auth |
|------|------|
| `crates/bakery-agent/src/claude.rs` | `ANTHROPIC_API_KEY` |
| `crates/bakery-agent/src/grok.rs` | `XAI_API_KEY`, or the node's stored sign-in |
| `crates/bakery-agent/src/gemini.rs` | API key, or a client secret supplied through the environment. **Never hardcode it.** |
| Stub in `lib.rs` | none |

Every sign-in lives with the node, never in a room or the workspace. The node reads its own sign-ins at run time and swaps the real credential in at a room's door; agents never open those files.
