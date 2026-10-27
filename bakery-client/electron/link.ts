// Agents drive a conversation's own previews through the node. Main keeps one
// WebSocket to its /desktop/ws, behind a one-time ticket the operator
// credential mints, tells it which previews are open for which conversation,
// and runs each command it relays on that one view. views.ts finds only the
// previews it made, so the app window is never driven; it is only ever
// captured, whole, for a screenshot. The node's side:
// crates/bakery-node/src/desktop.rs.
import type { BrowserWindow } from "electron";
import { doorsLost, onPreviewsChanged, previewFor, previews, redoorPreviews } from "./views.ts";

type Command = { id: string; op: string; view_id: string; stream_id: string; args?: Record<string, unknown> };

const ANSWER_MS = 20_000;
const MAX_TEXT = 20_000;
/** A screenshot's longest edge: what models read well; more costs tokens and shows nothing more. */
const SHOT_EDGE = 1568;
let node: { addr: string; token: string } | null = null;
let socket: WebSocket | null = null;
let appWindow: () => BrowserWindow | null = () => null;
/** The page shows a secret right now (a node reply said `secret: true`): no
 *  screenshot until it says it no longer does, or a new page loads. */
let secretShown = false;

export function showsSecret(on: boolean) {
  secretShown = on;
}

/** Starts the link on the first connect; later connects retarget it. */
export function linkTo(addr: string, token: string, window: () => BrowserWindow | null) {
  const first = node === null;
  node = { addr, token };
  appWindow = window;
  if (first) void keepLinked();
}

async function keepLinked() {
  onPreviewsChanged(tellPreviews);
  for (;;) {
    try {
      await linkOnce();
    } catch (e) {
      console.error(`bakery: desktop link: ${(e as Error).message}`);
    }
    await new Promise((r) => setTimeout(r, 3000));
  }
}

async function linkOnce() {
  const { addr, token } = node!;
  const res = await fetch(`http://${addr}/events/ticket`, {
    method: "POST",
    headers: { authorization: `Bearer ${token}`, "content-type": "application/json" },
    body: "{}",
  });
  if (!res.ok) throw new Error(`no ticket (${res.status})`);
  const { ticket } = (await res.json()) as { ticket: string };
  await new Promise<void>((done) => {
    const ws = new WebSocket(`ws://${addr}/desktop/ws?ticket=${encodeURIComponent(ticket)}`);
    socket = ws;
    // A node back from a restart may have handed the previews' doors to
    // other threads: they go out again only once each has its own.
    ws.addEventListener("open", () => {
      tellPreviews();
      redoorPreviews();
    });
    ws.addEventListener("message", (e) => void answer(ws, String(e.data)));
    ws.addEventListener("close", () => {
      if (socket === ws) socket = null;
      doorsLost();
      done();
    });
  });
}

function tellPreviews() {
  if (socket?.readyState === WebSocket.OPEN) socket.send(JSON.stringify({ type: "previews", previews: previews() }));
}

async function answer(ws: WebSocket, text: string) {
  let command: Command;
  try {
    command = JSON.parse(text) as Command;
  } catch {
    return;
  }
  let reply: Record<string, unknown>;
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    const late = new Promise<never>((_, reject) => {
      timer = setTimeout(() => reject(new Error(`the page did not answer within ${ANSWER_MS / 1000}s`)), ANSWER_MS);
    });
    reply = { type: "result", id: command.id, ok: true, value: await Promise.race([run(command), late]) };
  } catch (e) {
    reply = { type: "result", id: command.id, ok: false, error: (e as Error).message };
  } finally {
    clearTimeout(timer);
  }
  if (ws.readyState === WebSocket.OPEN) ws.send(JSON.stringify(reply));
}

/** The app window's own page, as the operator sees it. A Code or Preview view
 *  is a separate view on top of it, so it is never in the capture. */
async function screenshot() {
  if (secretShown) throw new Error("the operator has a secret on screen");
  const win = appWindow();
  if (!win || win.isDestroyed()) throw new Error("Bakery's window is not open");
  let image = await win.webContents.capturePage();
  if (image.isEmpty()) throw new Error("Bakery's window showed nothing to capture (is it minimized?)");
  const { width, height } = image.getSize();
  const scale = SHOT_EDGE / Math.max(width, height);
  if (scale < 1) image = image.resize({ width: Math.round(width * scale), height: Math.round(height * scale), quality: "good" });
  const size = image.getSize();
  return { width: size.width, height: size.height, png: image.toPNG().toString("base64") };
}

/** One command: a screenshot of the app window, or one on the one preview it
 *  names, if that preview belongs to the conversation it names. */
async function run({ op, view_id, stream_id, args = {} }: Command): Promise<unknown> {
  if (op === "screenshot") return screenshot();
  const page = previewFor(view_id, stream_id).webContents;
  const where = () => ({ url: page.getURL(), title: page.getTitle() });
  switch (op) {
    case "snapshot":
      return page.executeJavaScript(
        `({ url: location.href, title: document.title, text: (document.body ? document.body.innerText : "").slice(0, ${MAX_TEXT}) })`,
      );
    case "navigate": {
      const url = String(args.url ?? "");
      if (!/^https?:\/\//i.test(url)) throw new Error(`a preview loads http(s) pages only, not ${url.slice(0, 80)}`);
      await page.loadURL(url);
      return where();
    }
    case "click": {
      const selector = String(args.selector ?? "");
      const clicked = await page.executeJavaScript(
        `(() => { const el = document.querySelector(${JSON.stringify(selector)}); if (!el) return false; el.scrollIntoView({ block: "center" }); el.click(); return true; })()`,
        true,
      );
      if (!clicked) throw new Error(`no element matches ${selector}`);
      return where();
    }
    case "eval": {
      // Inlined, not eval()'d, so a page's CSP doesn't refuse it.
      const value = await page.executeJavaScript(
        `(async () => { const v = await (\n${String(args.expression ?? "undefined")}\n); try { return JSON.stringify(v) ?? "undefined"; } catch { return String(v); } })()`,
        true,
      );
      return { value: String(value).slice(0, MAX_TEXT) };
    }
    default:
      throw new Error(`no preview command ${JSON.stringify(op)}`);
  }
}
