// The model proxy going away partway through a native Claude turn: what
// Claude Code does on its own, how long it rides it out, and what the turn
// ends with. A relay stands in front of the scratch proxy (scratch.sh);
// OUTAGE_AT_S seconds into the turn it drops every connection and refuses
// new ones for OUTAGE_S seconds, as a proxy that crashed and came back
// would. Bring the scratch up with a slow stand-in (STAND_IN_CHUNKS=60
// STAND_IN_DELAY_MS=500) so the outage lands mid-reply.
// Env: ADAPTER and ACP_SDK (an assembled native runtime), SCRATCH (the
// folder scratch.sh used), OUTAGE_AT_S, OUTAGE_S, RELAY_PORT (47916),
// GIVE_UP_S (900). Prints OBS=<json>: a timeline in ms from the prompt.
import fs from "node:fs";
import net from "node:net";
import { spawn } from "node:child_process";
import { Readable, Writable } from "node:stream";

const acp = await import(process.env.ACP_SDK);
const scratch = process.env.SCRATCH;
const relayPort = Number(process.env.RELAY_PORT || 47916);
const outageAt = Number(process.env.OUTAGE_AT_S || 8) * 1000;
const outageFor = Number(process.env.OUTAGE_S || 5) * 1000;
const giveUp = Number(process.env.GIVE_UP_S || 900) * 1000;
const env = Object.fromEntries(
  fs.readFileSync(`${scratch}/env`, "utf8").trim().split("\n").map((line) => line.split(/=(.*)/s).slice(0, 2)),
);
const proxyPort = Number(new URL(env.ANTHROPIC_BASE_URL).port);

let began = Date.now();
const timeline = [];
const at = (what, extra) => timeline.push({ ms: Date.now() - began, what, ...extra });

// The relay: every connection piped to the proxy until the outage.
const open = new Set();
let connections = 0;
const relay = net.createServer((client) => {
  connections++;
  at("connection");
  const upstream = net.connect(proxyPort, "127.0.0.1");
  for (const s of [client, upstream]) {
    open.add(s);
    s.on("close", () => open.delete(s));
    s.on("error", () => {});
  }
  client.pipe(upstream).pipe(client);
});
const listen = () => new Promise((done) => relay.listen(relayPort, "127.0.0.1", done));
await listen();
setTimeout(() => {
  at("outage begins", { seconds: outageFor / 1000 });
  relay.close();
  for (const s of open) s.destroy();
  setTimeout(() => listen().then(() => at("outage ends")), outageFor);
}, outageAt);

const home = fs.mkdtempSync(`${scratch}/engine-`);
const child = spawn(process.execPath, [process.env.ADAPTER], {
  stdio: ["pipe", "pipe", "pipe"],
  env: {
    PATH: "/bin:/usr/bin",
    HOME: home,
    CLAUDE_CONFIG_DIR: `${home}/.claude`,
    TMPDIR: process.env.TMPDIR || "/tmp",
    ANTHROPIC_BASE_URL: `http://127.0.0.1:${relayPort}`,
    ANTHROPIC_AUTH_TOKEN: env.ANTHROPIC_AUTH_TOKEN,
    ANTHROPIC_MODEL: "claude-sonnet-5-5",
    ANTHROPIC_SMALL_FAST_MODEL: "claude-haiku-5-5",
    CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC: "1",
  },
});
let stderr = "";
child.stderr.on("data", (d) => (stderr = (stderr + d).slice(-4000)));
const stream = acp.ndJsonStream(Writable.toWeb(child.stdin), Readable.toWeb(child.stdout));
const obs = { outage_at_s: outageAt / 1000, outage_s: outageFor / 1000, text: "", stop_reason: null, error: null };
const stop = setTimeout(() => {
  obs.error = `no end within ${giveUp / 1000} s`;
  finish();
}, giveUp);

function finish() {
  clearTimeout(stop);
  child.kill("SIGTERM");
  relay.close();
  obs.connections = connections;
  obs.timeline = timeline;
  obs.engine_stderr_tail = stderr.slice(-1500);
  process.stdout.write("OBS=" + JSON.stringify(obs) + "\n");
  process.exit(0);
}

try {
  const result = await acp
    .client({ name: "bakery-outage-probe" })
    .onRequest(acp.methods.client.session.requestPermission, (ctx) => {
      const options = ctx.params.options || [];
      const allow = options.find((o) => o.kind === "allow_once") || options[0];
      return { outcome: { outcome: "selected", optionId: allow.optionId } };
    })
    .connectWith(stream, async (ctx) => {
      await ctx.request(acp.methods.agent.initialize, { protocolVersion: acp.PROTOCOL_VERSION, clientCapabilities: {} });
      return ctx.buildSession(home).withSession(async (session) => {
        began = Date.now();
        at("prompt");
        session.prompt("Say hello.");
        for (;;) {
          const m = await session.nextUpdate();
          if (m.kind === "stop") return m.response;
          const u = m.notification?.update;
          if (u?.sessionUpdate === "agent_message_chunk" && u.content?.type === "text") {
            if (!obs.text) at("first text");
            obs.text += u.content.text;
            at("text", { chars: obs.text.length });
          } else if (u) {
            at(u.sessionUpdate);
          }
        }
      });
    });
  obs.stop_reason = result?.stopReason ?? null;
  at("stop", { reason: obs.stop_reason });
} catch (e) {
  obs.error = String((e && e.message) || e);
  at("error", { message: obs.error.slice(0, 300) });
}
finish();
