// The native views' rules (views.ts), in plain node so shell.check.ts tests
// them: what a view may load, where its storage lives, how its bounds scale,
// which keys belong to the shell rather than the page, and the UI's zoom.
import { readFileSync, renameSync, writeFileSync } from "node:fs";

export type ViewKind = "preview" | "code";
export type ViewSpec = { id: string; kind: ViewKind; url: string; streamId: string };
export type ViewBounds = { x: number; y: number; width: number; height: number };

/** A view request from the renderer, checked: an id, a known kind, an
 *  http(s) URL and the conversation it belongs to. */
export function checkSpec(raw: unknown): ViewSpec {
  const s = (raw ?? {}) as Partial<Record<keyof ViewSpec, unknown>>;
  const text = (v: unknown, what: string) => {
    if (typeof v !== "string" || !v.trim() || v.length > 4096) throw new Error(`a view needs ${what}`);
    return v.trim();
  };
  const id = text(s.id, "an id");
  const streamId = text(s.streamId, "the conversation it belongs to");
  const url = text(s.url, "a URL");
  if (s.kind !== "preview" && s.kind !== "code") throw new Error(`no view kind ${JSON.stringify(s.kind)}`);
  if (!/^https?:\/\//i.test(url)) throw new Error(`a view shows http(s) pages only, not ${url.slice(0, 80)}`);
  return { id, kind: s.kind, url, streamId };
}

/** Whether an agent's command naming `streamId` may touch this view: only a
 *  preview of that very conversation, never a code view. */
export function mayDrive(spec: Pick<ViewSpec, "kind" | "streamId">, streamId: string): boolean {
  return spec.kind === "preview" && spec.streamId === streamId;
}

/** One persistent partition per kind and conversation: two worktrees of the
 *  same app never share cookies or storage, and neither shares the app's. */
export function partitionFor(kind: ViewKind, streamId: string): string {
  return `persist:${kind}-${streamId.replace(/[^A-Za-z0-9_.-]/g, "_")}`;
}

/** This Mac's own addresses, where the app being built runs: a preview
 *  reaches them directly, never through its door. */
const LOOPBACK = "localhost,*.localhost,127.0.0.1/8,[::1]";
const isLoopback = (host: string) =>
  host === "localhost" || host.endsWith(".localhost") || host === "[::1]" || /^127\.\d+\.\d+\.\d+$/.test(host);

/** A preview session's proxy: its thread's door (`http://127.0.0.1:<port>`,
 *  the node's GET /thread/door), which holds it to the project's list, for
 *  everything but this Mac's own addresses. `<-loopback>` drops Chromium's
 *  implicit bypass, which also lets link-local addresses go direct. */
export function previewProxy(door: string) {
  const port = /^http:\/\/127\.0\.0\.1:(\d{1,5})\/?$/.exec(door)?.[1];
  if (!port || +port < 1 || +port > 65535) throw new Error(`not a door on this Mac: ${door.slice(0, 80)}`);
  return { mode: "fixed_servers" as const, proxyRules: `127.0.0.1:${port}`, proxyBypassRules: `<-loopback>,${LOOPBACK}` };
}

/** Whether a preview's request may go: to this Mac's own addresses always;
 *  anywhere else only once its door is set, which holds it to the project's
 *  list (https and wss tunnelled, http and ws carried). */
export function mayLeave(url: string, hasDoor: boolean): boolean {
  let u: URL;
  try {
    u = new URL(url);
  } catch {
    return false;
  }
  if (!["http:", "https:", "ws:", "wss:"].includes(u.protocol) || isLoopback(u.hostname)) return true;
  return hasDoor;
}

/** The renderer's CSS pixels as window pixels, at the app's zoom. */
export function scaleBounds(raw: unknown, zoom: number): ViewBounds {
  const b = (raw ?? {}) as Partial<Record<keyof ViewBounds, unknown>>;
  const n = (v: unknown) => (typeof v === "number" && Number.isFinite(v) ? v : 0);
  const z = Number.isFinite(zoom) && zoom > 0 ? zoom : 1;
  return {
    x: Math.round(n(b.x) * z),
    y: Math.round(n(b.y) * z),
    width: Math.max(0, Math.round(n(b.width) * z)),
    height: Math.max(0, Math.round(n(b.height) * z)),
  };
}

export type KeyInput = {
  type: string;
  key: string;
  /** The physical key, when known: ⌘⇧0 types ")" on a US keyboard. */
  code?: string;
  meta: boolean;
  shift: boolean;
  alt: boolean;
  control: boolean;
};

/** The shell's own shortcuts, which a focused view would otherwise swallow:
 *  ⌘K, ⌘⇧P, ⌘J (the Bakery's terminal), ⌘⇧F (its search, the same in Code
 *  as anywhere), ⌘\, ⌘0–9, ⌘⌥← and ⌘⌥→, and the zoom keys: ⌘= or ⌘+ in,
 *  ⌘- out, ⌘⇧0 back to 100% (⌘0 is the factory floor); and ⌃1–⌃9, the
 *  Bakery's workspaces. Anything else stays the page's. */
export function shortcutOf(input: KeyInput): string | null {
  if (input.type !== "keyDown") return null;
  if (input.control && !input.meta && !input.alt && !input.shift) {
    return /^[1-9]$/.test(input.key) ? `Ctrl+${input.key}` : null;
  }
  if (!input.meta || input.control) return null;
  const key = input.key.length === 1 ? input.key.toUpperCase() : input.key;
  if (input.alt) {
    if (input.shift) return null;
    if (key === "ArrowLeft") return "Cmd+Alt+Left";
    if (key === "ArrowRight") return "Cmd+Alt+Right";
    return null;
  }
  if (key === "=" || key === "+") return "Cmd+=";
  if (input.shift) {
    if (key === "P") return "Cmd+Shift+P";
    if (key === "F") return "Cmd+Shift+F";
    return input.code === "Digit0" ? "Cmd+Shift+0" : null;
  }
  if (key === "-") return "Cmd+-";
  return key === "K" || key === "J" || key === "\\" || /^[0-9]$/.test(key) ? `Cmd+${key}` : null;
}

/** The shortcuts that zoom the UI rather than reach the page. */
export const ZOOM_KEYS: readonly string[] = ["Cmd+=", "Cmd+-", "Cmd+Shift+0"];

/** The UI's zoom, stepped as VS Code steps it: Chromium zoom levels, each
 *  ×1.2 the last, from about a quarter of its size to four times it. */
export const ZOOM_RANGE = { min: -8, max: 8 } as const;

const clampZoom = (level: number) => Math.min(ZOOM_RANGE.max, Math.max(ZOOM_RANGE.min, Math.round(level)));

/** The level a zoom shortcut moves to from `level`; null for any other key. */
export function zoomed(level: number, shortcut: string): number | null {
  if (shortcut === "Cmd+=") return clampZoom(level + 1);
  if (shortcut === "Cmd+-") return clampZoom(level - 1);
  if (shortcut === "Cmd+Shift+0") return 0;
  return null;
}

/** The level kept in `file`, Electron's own userData; 100% when there is none. */
export function readZoom(file: string): number {
  try {
    const level: unknown = JSON.parse(readFileSync(file, "utf8")).level;
    return typeof level === "number" && Number.isFinite(level) ? clampZoom(level) : 0;
  } catch {
    return 0;
  }
}

/** Keeps `level` in `file`, written whole or not at all. */
export function writeZoom(file: string, level: number) {
  const tmp = `${file}.tmp`;
  writeFileSync(tmp, JSON.stringify({ level: clampZoom(level) }));
  renameSync(tmp, file);
}
