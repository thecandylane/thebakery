# Patches to third-party packages

pnpm applies these at install (`patchedDependencies` in `pnpm-workspace.yaml`).

## `@agentclientprotocol__claude-agent-acp@0.90.0.patch`

Modifies [`@agentclientprotocol/claude-agent-acp`](https://github.com/agentclientprotocol/claude-agent-acp) 0.90.0 by Zed Industries, licensed under the Apache License, Version 2.0. The full text is in [`LICENSE-Apache-2.0.txt`](LICENSE-Apache-2.0.txt).

The Bakery changed two of its files: `dist/acp-agent.js` and `dist/permissions/response.js`. The patch itself shows every change.
