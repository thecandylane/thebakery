// Scratch only: does a Claude Code session, its transcript copied into
// another config folder as the move-in copies it, resume from its last
// summary? Talks to the stand-in (STAND_IN, run with STAND_IN_BODIES and
// STAND_IN_MARK), never a real account. Prints OBS=<json>: counts and
// yes/no answers only, never a transcript's words.
//
// Made (no TRANSCRIPT): one engine remembers PINEAPPLE, compacts, then
// remembers MANGO; a second engine, its config folder holding only the
// copied transcript, resumes the session and is asked a question. Its
// request must carry the summary and MANGO, and not PINEAPPLE.
//
// Copied (TRANSCRIPT=<a copy of a real transcript>): the second engine
// resumes that one, and its request must carry the text of the transcript's
// last summary, and none of a message from before its last compaction.
//
// Env: ADAPTER, ACP_SDK, SCRATCH, STAND_IN, BODIES (the stand-in's
// STAND_IN_BODIES), TRANSCRIPT (optional).
import fs from "node:fs";
import { spawn } from "node:child_process";
import { Readable, Writable } from "node:stream";

const acp = await import(process.env.ACP_SDK);
const scratch = process.env.SCRATCH;
const bodies = process.env.BODIES;
const cwd = fs.mkdtempSync(`${scratch}/project-`);
const slug = cwd.replace(/[^a-zA-Z0-9]/g, "-");
const obs = {};

async function engine(home, work) {
  const child = spawn(process.execPath, [process.env.ADAPTER], {
    stdio: ["pipe", "pipe", "pipe"],
    env: {
      PATH: "/bin:/usr/bin",
      HOME: home,
      CLAUDE_CONFIG_DIR: `${home}/.claude`,
      TMPDIR: process.env.TMPDIR || "/tmp",
      ANTHROPIC_BASE_URL: process.env.STAND_IN,
      ANTHROPIC_AUTH_TOKEN: "synthetic-stand-in",
      ANTHROPIC_MODEL: "claude-sonnet-5-5",
      ANTHROPIC_SMALL_FAST_MODEL: "claude-haiku-5-5",
      CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC: "1",
    },
  });
  child.stderr.on("data", () => {});
  const stream = acp.ndJsonStream(Writable.toWeb(child.stdin), Readable.toWeb(child.stdout));
  try {
    return await acp
      .client({ name: "bakery-compact-resume-probe" })
      .onRequest(acp.methods.client.session.requestPermission, () => ({ outcome: { outcome: "cancelled" } }))
      .onNotification(acp.methods.client.session.update, () => {})
      .connectWith(stream, async (ctx) => {
        await ctx.request(acp.methods.agent.initialize, { protocolVersion: acp.PROTOCOL_VERSION, clientCapabilities: {} });
        return work(ctx);
      });
  } finally {
    // Claude Code writes its transcript just after a turn ends.
    await new Promise((done) => setTimeout(done, 2000));
    child.kill("SIGTERM");
  }
}

const say = (ctx, sessionId, text) =>
  ctx.request(acp.methods.agent.session.prompt, { sessionId, prompt: [{ type: "text", text }] });

/** A transcript's entries, one per line; a torn line is skipped. */
function entries(path) {
  return fs
    .readFileSync(path, "utf8")
    .split("\n")
    .filter(Boolean)
    .flatMap((line) => {
      try {
        return [JSON.parse(line)];
      } catch {
        return [];
      }
    });
}

/** A message's text, whether a string or blocks of text. */
function text(entry) {
  const content = entry?.message?.content;
  if (typeof content === "string") return content;
  if (Array.isArray(content)) return content.map((b) => (typeof b?.text === "string" ? b.text : "")).join("\n");
  return "";
}

/** A distinctive stretch of `words`: the middle of its longest line. */
function snippet(words) {
  const line = words.split("\n").reduce((a, b) => (b.length > a.length ? b : a), "");
  const start = Math.max(0, Math.floor(line.length / 2) - 30);
  return line.slice(start, start + 60);
}

let transcript = process.env.TRANSCRIPT;
let sessionId;
let expect;
if (!transcript) {
  const first = fs.mkdtempSync(`${scratch}/seat-a-`);
  sessionId = await engine(first, async (ctx) => {
    const created = await ctx.request(acp.methods.agent.session.new, { cwd, mcpServers: [] });
    // Enough of a conversation for Claude Code to compact.
    for (const word of ["PINEAPPLE", "KIWI", "PLUM", "FIG"]) {
      await say(ctx, created.sessionId, `Remember the word ${word}.`);
    }
    await say(ctx, created.sessionId, "/compact");
    await say(ctx, created.sessionId, "Now remember the word MANGO.");
    return created.sessionId;
  });
  transcript = `${first}/.claude/projects/${slug}/${sessionId}.jsonl`;
  expect = { after: "MANGO", before: "PINEAPPLE" };
} else {
  sessionId = transcript.split("/").pop().replace(/\.jsonl$/, "");
}
obs.transcript_found = fs.existsSync(transcript);
const all = entries(transcript);
const boundaries = all.flatMap((e, at) => (e.subtype === "compact_boundary" ? [at] : []));
obs.compactions = boundaries.length;
const last = boundaries.at(-1) ?? -1;
const summary = all.slice(last + 1).find((e) => e.isCompactSummary);
obs.summary_found = Boolean(summary);
if (!expect) {
  const earlier = all
    .slice(0, Math.max(0, last))
    .filter((e) => e.type === "user" && !e.isCompactSummary && !e.isMeta && text(e).trim().length >= 80)
    .at(-1);
  expect = { before: earlier ? snippet(text(earlier)) : null };
}
expect.summary = summary ? snippet(text(summary)) : null;

// The move-in's copy: the transcript, and the folder beside it, into
// another config folder, under the new working folder's own name.
const second = fs.mkdtempSync(`${scratch}/seat-b-`);
const into = `${second}/.claude/projects/${slug}`;
fs.mkdirSync(into, { recursive: true });
fs.copyFileSync(transcript, `${into}/${sessionId}.jsonl`);
const beside = transcript.replace(/\.jsonl$/, "");
if (fs.existsSync(beside)) fs.cpSync(beside, `${into}/${sessionId}`, { recursive: true });

const seen = new Set(fs.readdirSync(bodies));
obs.resumed = await engine(second, async (ctx) => {
  await ctx.request(acp.methods.agent.session.resume, { sessionId, cwd, mcpServers: [] });
  const done = await say(ctx, sessionId, "Which words did I ask you to remember?");
  return done.stopReason;
}).catch((e) => ({ error: String(e?.message || e).slice(0, 200) }));

// The resumed question's own request: of those after the resume, the one
// with the most messages that asks it (not a side request, a title, say).
const question = "Which words did I ask you to remember?";
const fresh = fs.readdirSync(bodies).filter((f) => !seen.has(f));
const asking = fresh
  .map((f) => fs.readFileSync(`${bodies}/${f}`, "utf8"))
  .filter((b) => b.includes(question))
  .map((b) => ({ b, n: (JSON.parse(b).messages || []).length }))
  .sort((x, y) => y.n - x.n);
const body = asking[0]?.b ?? "";
// As the request writes it: JSON-escaped, on one line of the original.
const carries = (words) => (words ? body.includes(JSON.stringify(words).slice(1, -1)) : null);
obs.requests_after_resume = fresh.length;
obs.carries_summary = carries(expect.summary);
obs.carries_after = expect.after ? body.includes(expect.after) : null;
obs.carries_before = expect.before ? carries(expect.before) : null;
process.stdout.write("OBS=" + JSON.stringify(obs) + "\n");
process.exit(0);
