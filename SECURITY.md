# Security

This repository is maintained as if it were **public**, whether or not GitHub visibility is private.

## Deny lists (keep in sync)

| File | Purpose |
|------|---------|
| [`.gitignore`](.gitignore) | Never commit secrets, Heart DBs, env files, private providers |
| [`.claudeignore`](.claudeignore) | Anthropic agents: do not load credential-adjacent paths into context |
| [`.cursorignore`](.cursorignore) | Same for Cursor / IDE agents |
| [`CLAUDE.md`](CLAUDE.md) / [`AGENTS.md`](AGENTS.md) | Policy for paths **outside** the repo (`$HOME/.bakery`, Keychain) |

`.claudeignore` only applies under the workspace. Agents must still refuse `$HOME/.bakery/**` and OS keychains per `CLAUDE.md` / `AGENTS.md`.

## Credentials

| Location | Allowed? |
|----------|----------|
| Source tree, docs, tests as real secrets | **No** |
| `~/.bakery/secrets.json` (mode 0600) | **Yes** — API keys, OAuth tokens, optional Gemini client secret |
| Environment variables | **Yes** — e.g. `XAI_API_KEY`, `GEMINI_API_KEY`, `ANTHROPIC_API_KEY`, `BAKERY_GEMINI_CLIENT_SECRET` |
| Heart event payloads / activity queue | **No** |
| HTTP `/auth/status` and OAuth poll responses | **Booleans, model ids, optional account email only** — never tokens or keys |
| Agent worktree (tool jail) | Must not include `~/.bakery` |

OAuth **client ids** in source (xAI device flow, Gemini desktop id) are public identifiers, not user credentials. Matching **client secrets** are never hardcoded.

Optional operator-local HTTP clients live **outside** the repo (`$HOME/.bakery/providers/`) and are not selected by model `auto`. Do not describe their protocols in git.

## Runtime data

Do not commit or ship:

- `secrets.json`, `heart.db*`, `node.json`, `.bakery/`
- User worktrees under `~/.bakery/worktrees/`
- `.env` files

`heart.db` can store full old/new file contents from edit events — treat it like source code with secrets possibly inside user projects.

## Reporting

If you find a credential in git history or a release artifact, rotate the credential and open a private report to the maintainer before filing a public issue.
