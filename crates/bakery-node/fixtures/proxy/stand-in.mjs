// A stand-in for Anthropic's Messages API, for scratch tests of the model
// proxy: loopback only, synthetic replies, never a real account. Each
// streamed reply sends STAND_IN_CHUNKS pieces STAND_IN_DELAY_MS apart, so a
// test can stop the proxy partway through one. Every request is appended to
// STAND_IN_LOG as a JSON line: the path, the headers (secret ones shown as
// "(set)"), the model, the start of the system prompt, the thinking and
// effort settings, and the tools' names.
// With STAND_IN_BODIES (a folder), each Messages request's whole body is
// kept there as <n>.json; with STAND_IN_MARK, each reply starts with its own
// id ("reply msg_stand_in_<n>:"), so a later request shows which replies it
// carries.
// Env: STAND_IN_PORT (0 picks a free one), STAND_IN_CHUNKS, STAND_IN_DELAY_MS,
// STAND_IN_LOG, STAND_IN_BODIES, STAND_IN_MARK. Prints PORT=<port> once
// listening.
import fs from "node:fs";
import http from "node:http";

const chunks = Number(process.env.STAND_IN_CHUNKS || 3);
const delay = Number(process.env.STAND_IN_DELAY_MS || 50);
const log = process.env.STAND_IN_LOG;
const bodies = process.env.STAND_IN_BODIES;
const mark = Boolean(process.env.STAND_IN_MARK);
const secret = /^(authorization|x-api-key|cookie|proxy-authorization)$/i;
const sleep = (ms) => new Promise((done) => setTimeout(done, ms));
let served = 0;

function parse(body) {
  try {
    return JSON.parse(body || "{}");
  } catch {
    return {};
  }
}

function record(req, json) {
  if (!log) return;
  const system = Array.isArray(json.system) ? json.system.map((b) => b.text ?? "").join("\n") : String(json.system ?? "");
  const headers = Object.fromEntries(Object.entries(req.headers).map(([k, v]) => [k, secret.test(k) ? "(set)" : v]));
  const line = {
    at: new Date().toISOString(),
    method: req.method,
    path: req.url,
    headers,
    model: json.model,
    stream: json.stream,
    system_start: system.slice(0, 300),
    thinking: json.thinking,
    output_config: json.output_config,
    tools: (json.tools || []).map((t) => t.name),
    messages: (json.messages || []).length,
  };
  fs.appendFileSync(log, JSON.stringify(line) + "\n");
}

async function stream(res, id) {
  res.writeHead(200, { "content-type": "text/event-stream" });
  const send = (event, data) => res.write(`event: ${event}\ndata: ${JSON.stringify(data)}\n\n`);
  const usage = { input_tokens: 5, output_tokens: 1 };
  send("message_start", { type: "message_start", message: { id, type: "message", role: "assistant", model: "claude-stand-in", content: [], stop_reason: null, usage } });
  send("content_block_start", { type: "content_block_start", index: 0, content_block: { type: "text", text: "" } });
  if (mark) send("content_block_delta", { type: "content_block_delta", index: 0, delta: { type: "text_delta", text: `reply ${id}: ` } });
  for (let i = 1; i <= chunks; i++) {
    if (res.destroyed) return;
    send("content_block_delta", { type: "content_block_delta", index: 0, delta: { type: "text_delta", text: `part ${i} of ${chunks}. ` } });
    await sleep(delay);
  }
  send("content_block_stop", { type: "content_block_stop", index: 0 });
  send("message_delta", { type: "message_delta", delta: { stop_reason: "end_turn", stop_sequence: null }, usage: { output_tokens: chunks } });
  send("message_stop", { type: "message_stop" });
  res.end();
}

const server = http.createServer((req, res) => {
  let body = "";
  req.on("data", (chunk) => (body += chunk));
  req.on("end", () => {
    const json = parse(body);
    record(req, json);
    const path = (req.url || "").split("?")[0];
    const reply = (status, value) => {
      res.writeHead(status, { "content-type": "application/json" });
      res.end(JSON.stringify(value));
    };
    if (req.method === "GET" && path === "/v1/models") {
      return reply(200, { data: [{ id: "claude-stand-in", type: "model", display_name: "Stand-in" }], has_more: false });
    }
    if (req.method === "POST" && path === "/v1/messages/count_tokens") return reply(200, { input_tokens: 5 });
    if (req.method === "POST" && path === "/v1/messages") {
      const id = `msg_stand_in_${++served}`;
      if (bodies) fs.writeFileSync(`${bodies}/${served}.json`, body);
      if (json.stream) return void stream(res, id);
      const content = [{ type: "text", text: mark ? `reply ${id}: stand-in reply` : "stand-in reply" }];
      return reply(200, { id, type: "message", role: "assistant", model: "claude-stand-in", content, stop_reason: "end_turn", usage: { input_tokens: 5, output_tokens: 2 } });
    }
    reply(404, { type: "error", error: { type: "not_found_error", message: path } });
  });
});
server.listen(Number(process.env.STAND_IN_PORT || 0), "127.0.0.1", () => console.log(`PORT=${server.address().port}`));
