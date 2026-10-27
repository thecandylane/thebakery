# Local-only providers (directory reserved)

Sources that are not in this repository may exist on the developer machine at:

```text
$HOME/.bakery/providers/
```

`build.rs` copies a file from that directory into Cargo `OUT_DIR` only when it exists.
`auto` never selects those ids. If the files are missing, the workspace still builds.

Do not read that directory. Public providers: `claude.rs` (API key), `grok.rs`, `gemini.rs`, openai-compat, stub. Claude Code login is an operator-local slot: Settings → Sign in with Claude Code. `auto` never selects it.
