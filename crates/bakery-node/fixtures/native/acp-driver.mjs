// Fixture ACP client driver (BAKER-NATIVE-CHAT-CODE-02). Spawns the REAL
// PATCHED adapter (Bakery's bakery.dev/review patch applied), speaks ACP via the
// locked @agentclientprotocol/sdk, drives one prompt with permission prompts ON
// (no bypass), and records operation-correlated evidence:
//   - the Edit reaches requestPermission before any write (Review decision point);
//   - the file is UNCHANGED while the approval is held;
//   - on approve, exactly ONE native write happens IN the engine process
//     (writeTextFile is never called on the client => no delegated/duplicate write);
//   - edited-accept: when DRIVER_EDIT=edited and the request carries the
//     bakery.dev/review _meta, the client returns updatedInput and the native
//     tool performs the single write with the edited bytes.
// Synthetic data; provider traffic is loopback-redirected (NOT enforced network
// isolation). Env: ACP_SDK, ADAPTER, PROJECT, DRIVER_FS(0/1), DRIVER_PERM(approve|reject),
// DRIVER_HOLD_MS, DRIVER_EDIT(plain|edited).
import { spawn } from "node:child_process";
import { Writable, Readable } from "node:stream";
import fs from "node:fs";
const acp = await import(process.env.ACP_SDK);
const ADAPTER = process.env.ADAPTER, PROJECT = process.env.PROJECT;
const FS_DELEGATE = process.env.DRIVER_FS === "1", PERM = process.env.DRIVER_PERM || "approve";
const HOLD_MS = parseInt(process.env.DRIVER_HOLD_MS || "0", 10);
const EDIT_MODE = process.env.DRIVER_EDIT || "plain";
const TARGET = PROJECT + "/target.txt";
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const readOr = (p) => { try { return fs.readFileSync(p, "utf8"); } catch { return "(missing)"; } };
const obs = {
  permission_requests: [], write_calls: [], read_calls: [], updates: [],
  content_while_held: null, held_ms: 0, edited_accept: false,
  stop_reason: null, error: null,
};

// Allowlisted child environment — never the whole parent env. Only what the
// engine needs to find its config dir, reach the loopback mock, and run.
const childEnv = {
  PATH: "/bin:/usr/bin",
  HOME: process.env.HOME,
  CLAUDE_CONFIG_DIR: process.env.CLAUDE_CONFIG_DIR,
  ANTHROPIC_BASE_URL: process.env.ANTHROPIC_BASE_URL,
  CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC: "1",
  TMPDIR: process.env.TMPDIR || "/tmp",
};

const child = spawn(process.execPath, [ADAPTER], { stdio: ["pipe", "pipe", "inherit"], env: childEnv });
const stream = acp.ndJsonStream(Writable.toWeb(child.stdin), Readable.toWeb(child.stdout));
const clientCaps = FS_DELEGATE ? { fs: { readTextFile: true, writeTextFile: true } } : {};

try {
  const result = await acp.client({ name: "bakery-fixture-driver" })
    .onRequest(acp.methods.client.session.requestPermission, async (ctx) => {
      const p = ctx.params, tc = p.toolCall || {};
      const isEdit = tc.title && tc.title.startsWith("Edit");
      const reviewMeta = p._meta && p._meta["bakery.dev/review"];
      obs.permission_requests.push({
        title: tc.title || tc.kind || "?",
        rawInputKeys: tc.rawInput ? Object.keys(tc.rawInput) : null,
        review_meta: reviewMeta ? { version: reviewMeta.version, toolName: reviewMeta.toolName } : null,
        options: (p.options || []).map((o) => ({ id: o.optionId, kind: o.kind })),
      });
      // Operation-correlated: capture the file state at the moment the edit is
      // proposed, then HOLD, then confirm it did not change while held.
      if (isEdit) {
        obs.content_while_held = readOr(TARGET).trim();
        if (HOLD_MS > 0) { await sleep(HOLD_MS); obs.held_ms = HOLD_MS; obs.content_after_hold = readOr(TARGET).trim(); }
      }
      const allow = (p.options || []).find((o) => o.kind === "allow_once") || (p.options || []).find((o) => o.kind === "allow_always");
      const reject = (p.options || []).find((o) => o.kind === "reject_once") || (p.options || []).find((o) => o.kind === "reject_always");
      const chosen = (PERM === "reject" ? reject : allow) || (p.options || [])[0];
      const outcome = { outcome: { outcome: "selected", optionId: chosen.optionId } };
      // Edited-accept: only valid on allow-once with the review extension present.
      if (isEdit && EDIT_MODE === "edited" && reviewMeta && chosen.optionId === "allow-once") {
        const ri = tc.rawInput || {};
        outcome._meta = { "bakery.dev/review": { version: 1, toolCallId: reviewMeta.toolCallId || tc.toolCallId, updatedInput: { file_path: ri.file_path, old_string: ri.old_string, new_string: "EDITED_BY_HUMAN_REVIEW" } } };
        obs.edited_accept = true;
      }
      return outcome;
    })
    .onRequest(acp.methods.client.fs.writeTextFile, (ctx) => { obs.write_calls.push({ path: ctx.params.path }); fs.writeFileSync(ctx.params.path, ctx.params.content); return {}; })
    .onRequest(acp.methods.client.fs.readTextFile, (ctx) => { obs.read_calls.push({ path: ctx.params.path }); return { content: readOr(ctx.params.path) }; })
    .connectWith(stream, async (ctx) => {
      const init = await ctx.request(acp.methods.agent.initialize, { protocolVersion: acp.PROTOCOL_VERSION, clientCapabilities: clientCaps });
      obs.protocol = init.protocolVersion;
      return ctx.buildSession(PROJECT).withSession(async (session) => {
        obs.session_id = session.sessionId;
        session.prompt("Please edit the target file and run the build.");
        for (;;) { const m = await session.nextUpdate(); if (m.kind === "stop") { obs.stop_reason = m.response?.stopReason; return m.response; } const u = m.notification?.update; if (u) obs.updates.push(u.sessionUpdate); }
      });
    });
  obs.stop_reason = obs.stop_reason || result?.stopReason;
} catch (e) { obs.error = String((e && e.message) || e); }
finally {
  child.kill("SIGTERM");
  obs.final_content = readOr(TARGET).trim();
  obs.file_changed_by = obs.write_calls.length ? "client(delegated)" : (obs.final_content.startsWith("EDITED") ? "engine(in-process)" : "none");
  obs.build_artifact = readOr(TARGET + ".built").trim();
  process.stdout.write("OBS=" + JSON.stringify(obs) + "\n");
  process.exit(0);
}
