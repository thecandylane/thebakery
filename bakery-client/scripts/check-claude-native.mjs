#!/usr/bin/env node
/**
 * Handshake check for the installed Claude ACP adapter — the step before an
 * upgrade deploys (V1, `scripts/bakery.sh upgrade-adapter`). Its `initialize`
 * answer must name the version package.json pins and advertise Bakery's
 * `bakery.dev/review` extension, the proof the Bakery patch applied.
 *
 * Credential-free by construction: a scratch HOME and CLAUDE_CONFIG_DIR, a
 * minimal environment, and CLAUDE_CODE_EXECUTABLE pointed at /usr/bin/false,
 * so the adapter's background `claude auth status` probe never starts the
 * engine (no credential store or Keychain is ever asked).
 */
import { spawn } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { createInterface } from "node:readline";
import { fileURLToPath } from "node:url";
import { ADAPTER, readAdapterPin } from "./adapter-pin.mjs";

const clientRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const { version } = readAdapterPin(clientRoot);
const entry = join(clientRoot, "node_modules", ...ADAPTER.split("/"), "dist", "index.js");
const scratch = mkdtempSync(join(tmpdir(), "bakery-adapter-check-"));
const child = spawn(process.execPath, [entry], {
  stdio: ["pipe", "pipe", "pipe"],
  env: { PATH: process.env.PATH, HOME: scratch, CLAUDE_CONFIG_DIR: scratch, CLAUDE_CODE_EXECUTABLE: "/usr/bin/false" },
});
let stderr = "";
child.stderr.on("data", (chunk) => (stderr += chunk));

function finish(problem) {
  clearTimeout(timer);
  child.kill();
  rmSync(scratch, { recursive: true, force: true });
  if (problem) {
    console.error(`adapter handshake failed: ${problem}${stderr.trim() ? `\n${stderr.trim()}` : ""}`);
    process.exit(1);
  }
  console.log(`adapter handshake ok: ${ADAPTER}@${version} advertises bakery.dev/review`);
  process.exit(0);
}

const timer = setTimeout(() => finish("no initialize answer within 20 s"), 20_000);
child.on("error", (error) => finish(error.message));
child.on("exit", (code) => finish(`the adapter exited (${code}) before answering`));
child.stdin.on("error", () => {}); // a dead adapter is reported by "exit"
createInterface({ input: child.stdout }).on("line", (line) => {
  let message;
  try {
    message = JSON.parse(line);
  } catch {
    return;
  }
  if (message.id !== 1) return;
  const reported = message.result?.agentInfo?.version;
  const review = message.result?._meta?.["bakery.dev/review"];
  const tools = Array.isArray(review?.tools) ? review.tools : [];
  if (reported !== version) finish(`initialize reported ${reported}, but package.json pins ${version}`);
  else if (review?.version !== 1 || !tools.includes("Edit") || !tools.includes("Write"))
    finish("initialize does not advertise bakery.dev/review: the Bakery patch is not applied");
  else finish(null);
});
child.stdin.write(
  `${JSON.stringify({
    jsonrpc: "2.0",
    id: 1,
    method: "initialize",
    params: { protocolVersion: 1, clientCapabilities: {}, clientInfo: { name: "bakery-upgrade-check", version: "1" } },
  })}\n`,
);
