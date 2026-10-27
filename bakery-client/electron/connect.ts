// The desktop bootstrap, as src-tauri/src/connect.rs does it: reuse the node
// that answers (never start a second one, never stop one), else start one,
// through its LaunchAgent when there is one, so launchd owns it;
// read the operator credential from ~/.bakery/node.json (0600, on the
// sandbox deny list); prove it against the node. The credential goes from
// here to the trusted window only (main.ts, trust.ts): never a URL, a log
// line or an environment.
//
// Plain node, no electron import, so shell.check.ts tests it.
import { spawn, spawnSync } from "node:child_process";
import { existsSync, mkdirSync, openSync, readFileSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join } from "node:path";

export const DEFAULT_ADDR = "127.0.0.1:7717";

export type Connected = { ok: true; addr: string; token: string; startedNode: boolean };
export type Failure = {
  ok: false;
  kind: "unreachable" | "no_credential" | "unauthorized" | "misconfigured";
  message: string;
};
/** The node's LaunchAgent (`scripts/bakery.sh service install`). */
export type Service = { plist: string; target: string };
export const SERVICE_LABEL = "dev.bakery.node";

export type Options = {
  /** ~/.bakery */
  base: string;
  /** This HOME's node service, when installed: the node starts through it,
   *  never as a process of the app's own. */
  service: Service | null;
  addr: string;
  /** null: start nothing (tests that must prove no process starts). */
  nodeBin: string | null;
  probeMs: number;
  startWaitMs: number;
};

const fail = (kind: Failure["kind"], message: string): Failure => ({ ok: false, kind, message });

/** Where the deployed node is: BAKERY_NODE_BIN, beside Bakery.app (deploy
 *  puts both in one build folder), or the current deployed build. */
export function findNodeBinary(env: NodeJS.ProcessEnv, exe: string): string | null {
  const home = env.HOME || homedir();
  const candidates = [
    env.BAKERY_NODE_BIN?.trim(),
    // <build>/Bakery.app/Contents/MacOS/Bakery → <build>/bakery-node
    join(dirname(exe), "..", "..", "..", "bakery-node"),
    join(home, ".bakery", "deploy", "current", "bakery-node"),
  ];
  return candidates.find((p): p is string => !!p && existsSync(p)) ?? null;
}

/** This HOME's node service, when it has one. A node the app started
 *  itself would hold the port the service needs, and launchd would start
 *  the service again and again, failing each time. */
export function nodeService(home: string, uid: number = process.getuid?.() ?? -1): Service | null {
  const plist = join(home, "Library", "LaunchAgents", `${SERVICE_LABEL}.plist`);
  return uid >= 0 && existsSync(plist) ? { plist, target: `gui/${uid}/${SERVICE_LABEL}` } : null;
}

export function desktopOptions(env: NodeJS.ProcessEnv = process.env, exe: string = process.execPath): Options {
  const home = env.HOME || homedir();
  return {
    base: join(home, ".bakery"),
    service: nodeService(home),
    addr: env.BAKERY_NODE_ADDR?.trim() || DEFAULT_ADDR,
    nodeBin: findNodeBinary(env, exe),
    probeMs: 2000,
    // A fresh build's first connection can wait on a network filter such as
    // LuLu for half a minute and more on a busy Mac.
    startWaitMs: 120_000,
  };
}

/** The status of a loopback GET, or the error that stopped it. */
async function status(addr: string, path: string, ms: number, token?: string): Promise<number | Error> {
  try {
    const res = await fetch(`http://${addr}${path}`, {
      headers: token ? { authorization: `Bearer ${token}` } : {},
      signal: AbortSignal.timeout(ms),
    });
    await res.body?.cancel();
    return res.status;
  } catch (e) {
    return e instanceof Error ? e : new Error(String(e));
  }
}

const healthy = async (o: Options) => (await status(o.addr, "/health", o.probeMs)) === 200;

/** The credential in node.json, or why there is none. */
export function readOperatorToken(base: string): string | Failure {
  const path = join(base, "node.json");
  let text: string;
  try {
    text = readFileSync(path, "utf8");
  } catch (e) {
    return fail(
      "no_credential",
      `Could not read the node's configuration at ${path} (${(e as Error).message}). bakery-node writes that file the first time it starts — start the node once, then retry.`,
    );
  }
  let parsed: { operator_token?: unknown };
  try {
    parsed = JSON.parse(text);
  } catch (e) {
    return fail("misconfigured", `The node's configuration at ${path} is not valid JSON (${(e as Error).message}).`);
  }
  const token = typeof parsed?.operator_token === "string" ? parsed.operator_token.trim() : "";
  return token
    ? token
    : fail(
        "no_credential",
        `The node's configuration at ${path} has no operator credential yet. Start bakery-node once (it mints one on load), then retry.`,
      );
}

/** A conversation's door on the node (GET /thread/door): its Preview's
 *  proxy, held to its project's list. */
export async function threadDoor(addr: string, token: string, streamId: string): Promise<string> {
  const res = await fetch(`http://${addr}/thread/door?stream_id=${encodeURIComponent(streamId)}`, {
    headers: { authorization: `Bearer ${token}` },
    signal: AbortSignal.timeout(30_000),
  });
  const text = await res.text();
  if (!res.ok) throw new Error(`the node gave no door (${res.status}${text ? `: ${text.slice(0, 200)}` : ""})`);
  return String((JSON.parse(text) as { url?: unknown }).url ?? "");
}

/** Starts the node through its service: running, nothing changes; loaded
 *  but stopped, it starts; not loaded, it loads, and starts at once. */
function startService(service: Service) {
  const run = (args: string[]) => spawnSync("/bin/launchctl", args, { stdio: "ignore", timeout: 10_000 }).status === 0;
  if (!run(["kickstart", service.target])) run(["bootstrap", service.target.slice(0, service.target.lastIndexOf("/")), service.plist]);
}

/** Whether /health answers by `until`. Each try may wait that long, since a
 *  network filter such as LuLu holds a fresh process's first connection and
 *  short tries never see it through; a refused one tries again. */
async function healthyBy(o: Options, until: number): Promise<boolean> {
  do {
    if ((await status(o.addr, "/health", Math.max(until - Date.now(), o.probeMs))) === 200) return true;
    await new Promise((r) => setTimeout(r, 100));
  } while (Date.now() < until);
  return false;
}

/** Starts a node, logging to ~/.bakery/node.log. Only ever adds a process. */
function startNode(bin: string, base: string) {
  mkdirSync(base, { recursive: true });
  const log = openSync(join(base, "node.log"), "a", 0o600);
  const child = spawn(bin, [], { detached: true, stdio: ["ignore", log, log] });
  child.on("error", () => {});
  child.unref();
}

function logTail(base: string, lines: number): string | null {
  try {
    const tail = readFileSync(join(base, "node.log"), "utf8").trimEnd().split("\n").slice(-lines).join("\n");
    return tail.trim() ? tail : null;
  } catch {
    return null;
  }
}

/** Reuse, or start, the node; read the credential; prove it. */
export async function connect(o: Options): Promise<Connected | Failure> {
  let startedNode = false;
  if (!(await healthy(o))) {
    if (o.service || o.nodeBin) {
      try {
        if (o.service) startService(o.service);
        else if (o.nodeBin) startNode(o.nodeBin, o.base);
      } catch (e) {
        return fail("misconfigured", `Could not start the node (${(e as Error).message}).`);
      }
      startedNode = await healthyBy(o, Date.now() + o.startWaitMs);
    }
    if (!startedNode && !(await healthy(o))) {
      let message = o.nodeBin
        ? `The Bakery node is not answering on ${o.addr}. If something else is using that port, free it or set BAKERY_NODE_ADDR for both the node and this app.`
        : `No node is listening on ${o.addr} and no bakery-node binary was found to start. Deploy one (./scripts/bakery.sh deploy) or set BAKERY_NODE_BIN to its path, then retry.`;
      const tail = o.nodeBin ? logTail(o.base, 8) : null;
      if (tail) message += `\n\nThe node's last output was:\n${tail}`;
      return fail("unreachable", message);
    }
  }
  const token = readOperatorToken(o.base);
  if (typeof token !== "string") return token;
  const checked = await status(o.addr, "/node", o.probeMs, token);
  if (checked === 200) return { ok: true, addr: o.addr, token, startedNode };
  if (checked === 401 || checked === 403) {
    return fail(
      "unauthorized",
      "The node is running but rejected this machine's stored operator credential. That happens when the node was started with BAKERY_OPERATOR_TOKEN set (its token then differs from ~/.bakery/node.json), or when node.json was replaced while the node kept running. Restart bakery-node without BAKERY_OPERATOR_TOKEN, then retry.",
    );
  }
  return fail(
    "unreachable",
    checked instanceof Error
      ? `Lost the connection while checking the operator credential (${checked.message}). Retry.`
      : `The node answered the credential check with an unexpected status ${checked}. It may be mid-restart — retry.`,
  );
}
