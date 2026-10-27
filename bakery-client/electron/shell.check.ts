// The desktop shell's plain-node parts, without Electron: the UI bundle's
// hash and path guard, the bridge's trust rule, and the bootstrap against a
// stand-in node on a random port (never the operator's).
// Run: node electron/shell.check.ts
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { hashBundle, installBundle, openBundle, resolveAsset } from "./bundle.ts";
import { argsFor, checkTarget, EDITORS, isLocalAddr, ordered } from "./editor-rules.ts";
import { connect, threadDoor } from "./connect.ts";
import { Notices, noticeSpec } from "./notices.ts";
import type { Native } from "./notices.ts";
import { isTrustedSender, originOf } from "./trust.ts";
import {
  checkSpec,
  mayDrive,
  mayLeave,
  partitionFor,
  previewProxy,
  readZoom,
  scaleBounds,
  shortcutOf,
  writeZoom,
  ZOOM_KEYS,
  ZOOM_RANGE,
  zoomed,
} from "./view-rules.ts";

let failures = 0;
function assert(label: string, cond: boolean) {
  if (!cond) {
    console.error(`FAIL: ${label}`);
    failures++;
  } else {
    console.log(`pass: ${label}`);
  }
}

const scratch = mkdtempSync(join(tmpdir(), "bakery-shell-"));
try {
  // The bundle.
  const build = join(scratch, "build");
  mkdirSync(join(build, "assets"), { recursive: true });
  writeFileSync(join(build, "index.html"), "<!doctype html>app");
  writeFileSync(join(build, "assets", "app.js"), "console.log(1)");
  writeFileSync(join(scratch, "secret.txt"), "never served");
  const root = join(scratch, "ui");
  const hash = installBundle(build, root);
  assert("a build installs under its own hash", hash === hashBundle(build) && /^[0-9a-f]{64}$/.test(hash));
  const dir = openBundle(root, hash);
  assert("an intact bundle opens", dir === join(root, hash));
  assert("installing it again changes nothing", installBundle(build, root) === hash);
  assert("a file is served", resolveAsset(dir, "/assets/app.js") === join(dir, "assets", "app.js"));
  assert("the app's own routes get index.html", resolveAsset(dir, "/next") === join(dir, "index.html"));
  assert("the root gets index.html", resolveAsset(dir, "/") === join(dir, "index.html"));
  for (const sneaky of ["/../secret.txt", "/%2e%2e/secret.txt", "/assets/../../../secret.txt", "/%00", "/%E0%A4%A"]) {
    assert(`${sneaky} never leaves the bundle`, resolveAsset(dir, sneaky) === null);
  }
  writeFileSync(join(dir, "assets", "app.js"), "console.log('changed')");
  const refusal = (f: () => unknown) => {
    try {
      f();
      return "";
    } catch (e) {
      return (e as Error).message;
    }
  };
  assert("a changed bundle is refused", refusal(() => openBundle(root, hash)).includes("changed after it was installed"));
  assert("a pointer that isn't a hash is refused", refusal(() => openBundle(root, "../ui")).includes("not a UI bundle hash"));
  // The node hashes a build the same way before installing it from the app
  // (bakery-node's routes/ui_bundle.rs pins the same files to the same hash).
  const pinned = join(scratch, "pinned");
  mkdirSync(join(pinned, "a"), { recursive: true });
  mkdirSync(join(pinned, "_app", "immutable"), { recursive: true });
  writeFileSync(join(pinned, "index.html"), "<!doctype html><p>hi</p>\n");
  writeFileSync(join(pinned, "a.js"), "1");
  writeFileSync(join(pinned, "a", "b.js"), "export {}\n");
  writeFileSync(join(pinned, "_app", "immutable", "x.css"), "");
  assert("the node's hash of the same files", hashBundle(pinned) === "b3d74736eef58ad680734a1afcfdff658066efef93dbe254efa5dcd3abe9ea01");

  // The trust rule.
  const main = { contents: 1, frame: "7:1", origin: "bakery://app" };
  assert("bakery://app/next has the origin bakery://app", originOf("bakery://app/next") === "bakery://app");
  assert("the app's own top frame is trusted", isTrustedSender({ contents: 1, frame: "7:1", url: "bakery://app/next" }, main));
  assert("another webContents is not", !isTrustedSender({ contents: 2, frame: "7:1", url: "bakery://app/" }, main));
  assert("a subframe is not", !isTrustedSender({ contents: 1, frame: "7:2", url: "bakery://app/" }, main));
  assert("a frame that is gone is not", !isTrustedSender({ contents: 1, frame: null, url: "bakery://app/" }, main));
  assert("a page it was navigated to is not", !isTrustedSender({ contents: 1, frame: "7:1", url: "https://example.com/" }, main));
  assert("a look-alike host is not", !isTrustedSender({ contents: 1, frame: "7:1", url: "bakery://app.example/" }, main));

  // The views' rules.
  const spec = { id: "p1", kind: "preview", url: "http://127.0.0.1:5173/", streamId: "wt:abc" };
  assert("a preview of an http page opens", checkSpec(spec).url === "http://127.0.0.1:5173/");
  for (const [what, bad] of [
    ["an unknown kind", { ...spec, kind: "app" }],
    ["a file:// page", { ...spec, url: "file:///etc/passwd" }],
    ["a bakery:// page", { ...spec, url: "bakery://app/" }],
    ["no conversation", { ...spec, streamId: " " }],
    ["no id", { ...spec, id: "" }],
  ] as const) {
    assert(`${what} is refused`, refusal(() => checkSpec(bad)) !== "");
  }
  assert("an agent drives its own conversation's preview", mayDrive({ kind: "preview", streamId: "wt:abc" }, "wt:abc"));
  assert("never another conversation's", !mayDrive({ kind: "preview", streamId: "wt:abc" }, "wt:xyz"));
  assert("never a code view", !mayDrive({ kind: "code", streamId: "wt:abc" }, "wt:abc"));
  assert("storage is per kind and conversation", partitionFor("preview", "wt:abc/1") === "persist:preview-wt_abc_1");
  assert("code and preview never share it", partitionFor("code", "wt:abc") !== partitionFor("preview", "wt:abc"));
  // A preview's way out: its thread's door; this Mac's own addresses direct.
  const proxy = previewProxy("http://127.0.0.1:47803");
  assert("a preview goes through its door", proxy.mode === "fixed_servers" && proxy.proxyRules === "127.0.0.1:47803");
  assert("only this Mac's own addresses skip it", proxy.proxyBypassRules === "<-loopback>,localhost,*.localhost,127.0.0.1/8,[::1]");
  const notDoor = (url: string) => {
    try {
      previewProxy(url);
      return false;
    } catch {
      return true;
    }
  };
  assert(
    "a door is http on this Mac, with a port, nothing else",
    ["http://10.0.0.2:47803", "https://127.0.0.1:47803", "http://127.0.0.1", "http://127.0.0.1:0", "http://127.0.0.1:70000", "http://127.0.0.1:47803/x", "http://u@127.0.0.1:47803"].every(notDoor),
  );
  assert(
    "this Mac's own addresses, door or not",
    ["http://127.0.0.1:5173/", "http://localhost:3000", "ws://[::1]:24678/", "http://app.localhost:5173", "http://127.0.0.2/"].every((u) => mayLeave(u, false)),
  );
  assert("the web only once its door is set", !mayLeave("https://example.com/", false) && mayLeave("https://example.com/", true) && mayLeave("wss://example.com/s", true));
  assert("plain http and ws go through the door too", mayLeave("http://example.com/", true) && mayLeave("ws://example.com/", true));
  assert("but never without it", !mayLeave("http://example.com/", false) && !mayLeave("ws://example.com/", false));
  assert("a name that only looks local is the web", !mayLeave("http://localhost.example.com/", false) && !mayLeave("http://127.0.0.1.nip.io/", false));
  assert("a page's own data stays in", ["data:text/plain,hi", "blob:http://127.0.0.1:5173/x", "about:blank"].every((u) => mayLeave(u, false)));
  assert("what can't be read is held", !mayLeave("http://[bad", true));
  const scaled = scaleBounds({ x: 10, y: 20.4, width: 300, height: -5 }, 1.25);
  assert("bounds scale by the zoom", scaled.x === 13 && scaled.y === 26 && scaled.width === 375 && scaled.height === 0);
  assert("junk bounds are empty", JSON.stringify(scaleBounds("x", 1)) === '{"x":0,"y":0,"width":0,"height":0}');
  const key = (k: string, mods: Partial<{ code: string; meta: boolean; shift: boolean; alt: boolean; control: boolean }> = {}) =>
    shortcutOf({ type: "keyDown", key: k, meta: true, shift: false, alt: false, control: false, ...mods });
  assert("⌘K is the shell's", key("k") === "Cmd+K");
  assert("⌘⇧P is the shell's", key("P", { shift: true }) === "Cmd+Shift+P");
  assert("⌘J, ⌘\\, ⌘0 and ⌘3 are the shell's", key("j") === "Cmd+J" && key("\\") === "Cmd+\\" && key("0") === "Cmd+0" && key("3") === "Cmd+3");
  assert("⌘⌥← and ⌘⌥→ are the shell's", key("ArrowLeft", { alt: true }) === "Cmd+Alt+Left" && key("ArrowRight", { alt: true }) === "Cmd+Alt+Right");
  assert("⌘C and ⌘⇧K stay the page's", key("c") === null && key("K", { shift: true }) === null);
  assert("⌘⇧F is the Bakery's search, ⌘F stays the page's", key("F", { shift: true }) === "Cmd+Shift+F" && key("f") === null);

  // External editors: the worktree as the folder, the file at the line.
  const target = checkTarget({ folder: "/w/t", file: "src/a b.ts", line: 12 });
  assert("a relative file lands in its worktree", target.file === "/w/t/src/a b.ts" && target.line === 12);
  assert("VS Code-like editors take the folder and --goto", argsFor("code", target).join("|") === "/w/t|--goto|/w/t/src/a b.ts:12");
  assert("JetBrains IDEs take the folder and --line", argsFor("jetbrains", target).join("|") === "/w/t|--line|12|/w/t/src/a b.ts");
  assert("Xcode takes the file alone", argsFor("xed", target).join("|") === "--line|12|/w/t/src/a b.ts");
  const refused = (raw: unknown) => {
    try {
      checkTarget(raw);
      return false;
    } catch {
      return true;
    }
  };
  assert("a file outside its worktree is refused", refused({ folder: "/w/t", file: "../other/x.ts", line: 1 }));
  assert("a relative worktree is refused", refused({ folder: "w/t", file: "x.ts", line: 1 }));
  assert("a line from 1 only", refused({ folder: "/w/t", file: "x.ts", line: 0 }) && refused({ folder: "/w/t", file: "x.ts", line: 1.5 }));
  const found = [{ id: "zed", name: "Zed" }, { id: "cursor", name: "Cursor" }, { id: "vscode", name: "VS Code" }];
  assert("the last one used first, then by name", ordered(found, "zed").map((e) => e.id).join() === "zed,cursor,vscode");
  assert("by name with none used", ordered(found, null).map((e) => e.id).join() === "cursor,vscode,zed");
  assert("only a node on this Mac", isLocalAddr("127.0.0.1:7717") && isLocalAddr("localhost:1") && isLocalAddr("[::1]:7717") && !isLocalAddr("100.64.0.3:7717"));
  assert("every editor has a command", EDITORS.every((e) => e.app.endsWith(".app") && e.bin.length > 0));
  // The zoom keys are the shell's too, so a focused Code or Preview view
  // can't keep them: ⌘= or ⌘+ (⌘⇧=, or the keypad's +) in, ⌘- out, ⌘⇧0 back.
  assert("⌘= and ⌘+ zoom in", key("=") === "Cmd+=" && key("+", { shift: true }) === "Cmd+=" && key("+") === "Cmd+=");
  assert("⌘- zooms out", key("-") === "Cmd+-");
  assert("⌘⇧0 resets, by the key it is, whatever it types", key(")", { shift: true, code: "Digit0" }) === "Cmd+Shift+0");
  assert("⌘0 stays the factory floor", key("0", { code: "Digit0" }) === "Cmd+0");
  assert("⌘⌥= and ⌘⇧- stay the page's", key("=", { alt: true }) === null && key("_", { shift: true, code: "Minus" }) === null);
  assert("the zoom keys are exactly those", ZOOM_KEYS.join(" ") === "Cmd+= Cmd+- Cmd+Shift+0");
  assert("zoom steps one level each way", zoomed(0, "Cmd+=") === 1 && zoomed(0, "Cmd+-") === -1 && zoomed(3, "Cmd+Shift+0") === 0);
  assert("zoom stays within its range", zoomed(ZOOM_RANGE.max, "Cmd+=") === ZOOM_RANGE.max && zoomed(ZOOM_RANGE.min, "Cmd+-") === ZOOM_RANGE.min);
  assert("other keys don't zoom", zoomed(2, "Cmd+K") === null);
  // The level outlives a restart: kept in a file, read back on the next start.
  const zoomFile = join(scratch, "userData", "zoom.json");
  mkdirSync(join(scratch, "userData"), { recursive: true });
  assert("no zoom kept is 100%", readZoom(zoomFile) === 0);
  writeZoom(zoomFile, 3);
  assert("a kept zoom comes back after a restart", readZoom(zoomFile) === 3);
  writeZoom(zoomFile, -2);
  assert("the latest one wins", readZoom(zoomFile) === -2);
  writeFileSync(zoomFile, '{"level": 99}');
  assert("a level out of range comes back within it", readZoom(zoomFile) === ZOOM_RANGE.max);
  writeFileSync(zoomFile, "not json");
  assert("a broken file is 100%, not a crash", readZoom(zoomFile) === 0);
  assert("K alone stays the page's", key("k", { meta: false }) === null);
  const ctrl = (k: string, mods: Parameters<typeof key>[1] = {}) => key(k, { meta: false, control: true, ...mods });
  assert("⌃1–⌃9 switch the Bakery's workspaces", ctrl("1") === "Ctrl+1" && ctrl("9") === "Ctrl+9");
  assert("⌃0, ⌃⇧1 and ⌃C stay the page's", ctrl("0") === null && ctrl("1", { shift: true }) === null && ctrl("c") === null);
  assert("⌘⌃1 is nobody's", ctrl("1", { meta: true }) === null);
  assert("a key going up is nobody's", shortcutOf({ type: "keyUp", key: "k", meta: true, shift: false, alt: false, control: false }) === null);

  // The bootstrap, against a stand-in node.
  const token = `tok-${Math.random().toString(36).slice(2)}`;
  const base = join(scratch, "home", ".bakery");
  mkdirSync(base, { recursive: true });
  const node = createServer((req, res) => {
    if (req.url === "/health") return void res.writeHead(200).end("ok");
    if (req.url === "/node") return void res.writeHead(req.headers.authorization === `Bearer ${token}` ? 200 : 401).end();
    if (req.url?.startsWith("/thread/door?")) {
      const ours = req.headers.authorization === `Bearer ${token}` && new URL(req.url, "http://n").searchParams.get("stream_id") === "wt:abc/1";
      return void res.writeHead(ours ? 200 : 404).end(ours ? JSON.stringify({ url: "http://127.0.0.1:47803" }) : "no conversation");
    }
    res.writeHead(404).end();
  });
  await new Promise<void>((r) => node.listen(0, "127.0.0.1", r));
  const addr = `127.0.0.1:${(node.address() as { port: number }).port}`;
  // A fresh process's first loopback connection can take seconds on a busy
  // machine, or behind a network filter such as LuLu. None of these checks
  // needs a short timeout: "no node" is a refused connection, which answers
  // at once.
  const options = { base, service: null, addr, nodeBin: null, probeMs: 30_000, startWaitMs: 0 };
  // This process's first loopback connection is the one a filter holds, for
  // longer than any one try under load: it is made here, tried until the
  // stand-in answers, so no check below meets it.
  for (const until = Date.now() + 120_000; ; ) {
    const up = await fetch(`http://${addr}/health`, { signal: AbortSignal.timeout(30_000) }).then((res) => res.ok, () => false);
    if (up || Date.now() > until) break;
    await new Promise((r) => setTimeout(r, 200));
  }
  const said = (r: Awaited<ReturnType<typeof connect>>) => (r.ok ? "connected" : `${r.kind}: ${r.message}`);
  let r = await connect(options);
  assert(`no node.json: no credential (${said(r)})`, !r.ok && r.kind === "no_credential");
  writeFileSync(join(base, "node.json"), JSON.stringify({ operator_token: token }));
  r = await connect(options);
  assert("a running node and its credential connect", r.ok && r.token === token && r.addr === addr && !r.startedNode);
  writeFileSync(join(base, "node.json"), JSON.stringify({ operator_token: "stale-credential" }));
  r = await connect(options);
  assert("a credential the node refuses is named, not shown", !r.ok && r.kind === "unauthorized" && !r.message.includes("stale-credential"));
  assert("a conversation's door, asked with the operator credential", (await threadDoor(addr, token, "wt:abc/1")) === "http://127.0.0.1:47803");
  const noDoor = await threadDoor(addr, token, "wt:other").then(String, (e: Error) => e.message);
  assert("no door: the node's own reason", noDoor.includes("404") && noDoor.includes("no conversation"));
  node.closeAllConnections();
  await new Promise((r) => node.close(r));
  r = await connect(options);
  assert("no node and nothing to start: unreachable", !r.ok && r.kind === "unreachable" && !r.message.includes(token));

  // Notifications with buttons, against stand-ins for Electron's.
  type Fake = Native & { fire(event: string, ...args: unknown[]): void; shown: boolean; closed: boolean };
  const made: Fake[] = [];
  let fronted = 0;
  const notices = new Notices(
    () => {
      const listeners = new Map<string, (...args: unknown[]) => void>();
      const fake: Fake = {
        on: (event, listener) => listeners.set(event, listener as (...args: unknown[]) => void),
        show: () => void (fake.shown = true),
        close: () => void (fake.closed = true),
        fire: (event, ...args) => listeners.get(event)?.(...args),
        shown: false,
        closed: false,
      };
      made.push(fake);
      return fake;
    },
    () => fronted++,
  );
  const ask = { id: "host:1", title: "Allow example.com?", body: "build", actions: ["Allow for this project", "Deny"] };
  let answer = notices.notify(ask);
  made[0].fire("action", { actionIndex: 1 }, 1);
  assert("a pressed button answers with its text", made[0].shown && (await answer) === "Deny");
  answer = notices.notify(ask);
  made[1].fire("click");
  assert("a click on the body answers open and brings the app forward", (await answer) === "open" && fronted === 1);
  answer = notices.notify(ask);
  const replaced = answer;
  answer = notices.notify(ask);
  assert("a second notify with the same id takes the first down", (await replaced) === null && made[2].closed && !made[3].closed);
  notices.close("host:1");
  assert("taking one down answers null", (await answer) === null && made[3].closed);
  answer = notices.notify(ask);
  made[4].fire("failed", {}, "not allowed");
  assert("one that can't be shown answers null", (await answer) === null);
  answer = notices.notify(ask);
  made[5].fire("close");
  made[5].fire("action", { actionIndex: 0 });
  assert("a dismissal answers null, and only the first answer counts", (await answer) === null);
  const other = notices.notify({ ...ask, id: "host:2" });
  answer = notices.notify(ask);
  notices.closeAll();
  assert("a new page takes them all down", (await other) === null && (await answer) === null);
  const badSpec = (raw: unknown) => {
    try {
      noticeSpec(raw);
      return false;
    } catch {
      return true;
    }
  };
  assert(
    "a spec needs an id, a title and one to four buttons",
    badSpec({ title: "t", actions: ["a"] }) &&
      badSpec({ id: "x", actions: ["a"] }) &&
      badSpec({ id: "x", title: "t", actions: [] }) &&
      badSpec({ id: "x", title: "t", actions: ["a", "b", "c", "d", "e"] }) &&
      noticeSpec({ id: "x", title: "t", body: 7, actions: ["a", 5, ""] }).actions.length === 1,
  );
} finally {
  rmSync(scratch, { recursive: true, force: true });
}

if (failures > 0) throw new Error(`${failures} desktop shell check failure(s)`);
