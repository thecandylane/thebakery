# syntax=docker/dockerfile:1
# The Baker workspace: one Linux image (arm64 on the Mac, amd64 on a
# Linux server) with the repo's toolchains and the agents' own tools. An agent
# runs in it with only its worktree mounted, no Docker socket and no keys.
#
# The node builds it (Settings → Agents, or on its own when it's needed),
# writing this file beside the Claude connector's patch and passing the
# connector's release:
#
#   container build --build-arg CLAUDE_ACP_VERSION=<bakery-client's pin> -t bakery/workspace <that folder>

ARG RUST_VERSION=1.98
ARG NODE_VERSION=24

FROM rust:${RUST_VERSION}-bookworm AS tools
# Agent tools built from their release tags.
RUN cargo install --locked --root /opt/tools \
        --git https://github.com/1jehuang/agentgrep --tag v0.1.7 agentgrep \
 && cargo install --locked --root /opt/tools \
        --git https://github.com/rtk-ai/rtk --tag v0.51.0 rtk

FROM node:${NODE_VERSION}-bookworm-slim AS node

FROM rust:${RUST_VERSION}-bookworm
ARG TARGETARCH
ARG CBM_VERSION=v0.11.0
ARG PNPM_VERSION=11.23.0
# The Claude connector the host runs: the release bakery-client pins, which
# the node passes, and the Bakery's patch to it, which the node writes
# beside this file (empty when there is none). Either one changing changes
# the node's recipe, so the image is built again.
ARG CLAUDE_ACP_VERSION
# The operator's editor in a worktree's container (MIT); its sums are the
# release's own digests for this version.
ARG OPENVSCODE_VERSION=1.109.5

RUN apt-get update \
 && apt-get install -y --no-install-recommends ripgrep fd-find jq \
 && ln -s /usr/bin/fdfind /usr/local/bin/fd \
 && rm -rf /var/lib/apt/lists/*

COPY --from=node /usr/local/bin/node /usr/local/bin/node
COPY --from=node /usr/local/lib/node_modules /usr/local/lib/node_modules
COPY claude-agent-acp.patch /opt/bakery/claude-agent-acp.patch
RUN test -n "${CLAUDE_ACP_VERSION}" \
 && ln -s ../lib/node_modules/npm/bin/npm-cli.js /usr/local/bin/npm \
 && ln -s ../lib/node_modules/npm/bin/npx-cli.js /usr/local/bin/npx \
 && npm install -g --no-fund --no-audit \
        pnpm@${PNPM_VERSION} @agentclientprotocol/claude-agent-acp@${CLAUDE_ACP_VERSION} \
 && npm cache clean --force \
 # The Bakery's patch, applied as pnpm applies it on the host.
 && if [ -s /opt/bakery/claude-agent-acp.patch ]; then \
      patch --batch --forward -p1 -d /usr/local/lib/node_modules/@agentclientprotocol/claude-agent-acp \
        < /opt/bakery/claude-agent-acp.patch; \
    fi \
 # `claude`, for signing in: the CLI the adapter already carries for this arch.
 && ln -s "$(ls -d /usr/local/lib/node_modules/@agentclientprotocol/claude-agent-acp/node_modules/@anthropic-ai/claude-agent-sdk-linux-*/claude | head -n 1)" /usr/local/bin/claude

COPY --from=tools /opt/tools/bin/ /usr/local/bin/

# codebase-memory-mcp's release binary, checked against the release's sums.
RUN set -eu; \
    file="codebase-memory-mcp-linux-${TARGETARCH}-portable.tar.gz"; \
    base="https://github.com/DeusData/codebase-memory-mcp/releases/download/${CBM_VERSION}"; \
    cd /tmp; \
    curl -fsSLO "$base/$file"; \
    curl -fsSLO "$base/checksums.txt"; \
    awk -v f="$file" '$2 == f || $2 == "*" f { print $1 "  " f }' checksums.txt | sha256sum -c -; \
    tar --no-same-owner -xzf "$file" -C /usr/local/bin codebase-memory-mcp; \
    rm -f "$file" checksums.txt

# openvscode-server, checked against its release's digests.
RUN set -eu; \
    case "${TARGETARCH}" in \
      arm64) arch=arm64; sum=36d9c14036489b63de84ebace837fcacf7e60e669a0dc715802c5443684ea4dc ;; \
      amd64) arch=x64; sum=b433bf4f0227321a7014d8460d10a8f958adc0f45aa79bd889e84e65e8f88363 ;; \
    esac; \
    file="openvscode-server-v${OPENVSCODE_VERSION}-linux-${arch}.tar.gz"; \
    cd /tmp; \
    curl -fsSLO "https://github.com/gitpod-io/openvscode-server/releases/download/openvscode-server-v${OPENVSCODE_VERSION}/$file"; \
    echo "$sum  $file" | sha256sum -c -; \
    mkdir -p /opt/openvscode-server; \
    tar --no-same-owner -xzf "$file" -C /opt/openvscode-server --strip-components=1; \
    ln -s /opt/openvscode-server/bin/openvscode-server /usr/local/bin/openvscode-server; \
    rm -f "$file"

# The Bakery's look for Code, beside its shared settings: one menu button
# (bottom left) for everything the screen hides, each open starting on the
# file with the side and bottom panels and any welcome page closed (⌘B shows
# the tree), and the editor's own terminal, search and git shortcuts opening
# nothing of its own (the app takes ⌘J and ⌘⇧F for the Bakery's).
COPY <<"EOF" /opt/openvscode-server/extensions/bakery-code/package.json
{"name": "bakery-code", "displayName": "Bakery", "publisher": "bakery", "version": "1.0.0",
 "engines": {"vscode": "^1.90.0"}, "activationEvents": ["onStartupFinished"], "main": "./extension.js",
 "capabilities": {"untrustedWorkspaces": {"supported": true}, "virtualWorkspaces": true},
 "contributes": {
  "commands": [{"command": "bakery.menu", "title": "Bakery: Menu"},
   {"command": "bakery.taken", "title": "Opens in the Bakery"}],
  "menus": {"commandPalette": [{"command": "bakery.taken", "when": "false"}]},
  "keybindings": [
   {"key": "cmd+j", "command": "bakery.taken"}, {"key": "ctrl+`", "command": "bakery.taken"},
   {"key": "ctrl+shift+`", "command": "bakery.taken"}, {"key": "cmd+shift+f", "command": "bakery.taken"},
   {"key": "cmd+shift+h", "command": "bakery.taken"}, {"key": "ctrl+shift+g", "command": "bakery.taken"}]}}
EOF
COPY <<"EOF" /opt/openvscode-server/extensions/bakery-code/extension.js
exports.activate = async (context) => {
  const vscode = require("vscode");
  const run = (command) => vscode.commands.executeCommand(command).then(undefined, () => {});
  const hidden = [
    ["Explorer", "workbench.view.explorer"], ["Source Control", "workbench.view.scm"],
    ["Run and Debug", "workbench.view.debug"], ["Extensions", "workbench.view.extensions"],
    ["Problems", "workbench.actions.view.problems"], ["Output", "workbench.action.output.toggleOutput"],
    ["Settings", "workbench.action.openSettings"], ["Keyboard Shortcuts", "workbench.action.openGlobalKeybindings"],
    ["All commands", "workbench.action.showCommands"],
  ];
  context.subscriptions.push(
    vscode.commands.registerCommand("bakery.taken", () => {}),
    vscode.commands.registerCommand("bakery.menu", async () => {
      const pick = await vscode.window.showQuickPick(hidden.map(([label, command]) => ({ label, command })));
      if (pick) await run(pick.command);
    }),
  );
  const menu = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left, 10000);
  menu.text = "$(menu)";
  menu.tooltip = "Menu";
  menu.command = "bakery.menu";
  menu.show();
  context.subscriptions.push(menu);
  for (const command of ["workbench.action.closeSidebar", "workbench.action.closeAuxiliaryBar", "workbench.action.closePanel"]) {
    await run(command);
  }
  const welcome = vscode.window.tabGroups.all.flatMap((group) => group.tabs)
    .filter((tab) => tab.input === undefined && /^(Welcome|Walkthrough)/.test(tab.label));
  if (welcome.length) await vscode.window.tabGroups.close(welcome).then(undefined, () => {});
};
EOF

# Agents run unprivileged. Cargo's registry and git caches live in the
# agent's own home; the toolchain under /usr/local stays read-only.
RUN useradd --create-home --uid 1000 --shell /bin/bash agent
USER agent
ENV CARGO_HOME=/home/agent/.cargo \
    PATH=/home/agent/.cargo/bin:/usr/local/cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
WORKDIR /home/agent
