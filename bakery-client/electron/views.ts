// The native views the shell places over the main window (window.bakeryViews,
// agreed with bakery-ui): a worktree's app preview, or its code-server. Each
// is a WebContentsView in a persistent partition per kind and conversation,
// with no preload and no IPC, so nothing in it reaches the shell or the
// operator credential. The renderer asks through main.ts's bridge; this
// module owns the views.
import { session, shell, WebContentsView } from "electron";
import type { BrowserWindow } from "electron";
import { originOf } from "./trust.ts";
import { checkSpec, mayDrive, mayLeave, partitionFor, previewProxy, scaleBounds, shortcutOf, ZOOM_KEYS } from "./view-rules.ts";
import type { ViewSpec } from "./view-rules.ts";

/** Sends an event to the main window's page. */
export type Tell = (channel: string, ...args: unknown[]) => void;

/** `bounds`: where the page last placed it, in its CSS pixels. */
type Open = { spec: ViewSpec; view: WebContentsView; tell: Tell; bounds?: unknown };
const views = new Map<string, Open>();
const guarded = new Set<string>();
/** Each preview partition's door while the node has given one: until then
 *  its requests reach this Mac only (`mayLeave`). */
const doors = new Map<string, string>();
let doorOf: (streamId: string) => Promise<string> = () => Promise.reject(new Error("the Bakery isn't connected yet"));
let zoomKey: (shortcut: string) => void = () => {};
let codeZoom = 0;

/** Called with a zoom shortcut pressed in a view: it zooms the UI, never
 *  just the page in the view. */
export function onZoomKey(handler: (shortcut: string) => void) {
  zoomKey = handler;
}

/** The Code views' text follows the UI's zoom level. A preview stays at
 *  100%: it is the site being built, and what agents read of it must not
 *  change with the operator's zoom. */
export function zoomCodeViews(level: number) {
  codeZoom = level;
  for (const { spec, view } of views.values()) {
    if (spec.kind === "code") view.webContents.setZoomLevel(level);
  }
}

/** Every placed view again, at the window's zoom now. */
export function rescaleViews(win: BrowserWindow) {
  const zoom = win.webContents.getZoomFactor();
  for (const { view, bounds } of views.values()) {
    if (bounds !== undefined) view.setBounds(scaleBounds(bounds, zoom));
  }
}

function find(id: unknown): Open {
  const open = typeof id === "string" ? views.get(id) : undefined;
  if (!open) throw new Error(`no view ${JSON.stringify(id)}`);
  return open;
}

function openOutside(url: string) {
  if (/^https?:\/\//i.test(url)) void shell.openExternal(url);
}

/** Where previews' doors come from: the node, once main.ts has connected. */
export function useDoors(source: (streamId: string) => Promise<string>) {
  doorOf = source;
}

/** Routes a preview's session through its thread's door, which holds it to
 *  the project's list (an off-list host asks in the thread), then lets its
 *  requests leave this Mac. */
async function admit({ spec, view }: Open) {
  const partition = partitionFor(spec.kind, spec.streamId);
  const s = view.webContents.session;
  doors.delete(partition);
  const proxy = previewProxy(await doorOf(spec.streamId));
  await s.setProxy(proxy);
  // The network service takes the setting in its own time: held until it
  // routes through the door.
  for (let tries = 0; (await s.resolveProxy("https://example.com/")) !== `PROXY ${proxy.proxyRules}`; tries++) {
    if (tries === 100) throw new Error("its proxy didn't take");
    await new Promise((r) => setTimeout(r, 20));
  }
  // Pooled connections from before (another door, or none) close.
  await s.closeAllConnections();
  doors.set(partition, proxy.proxyRules);
}

/** A preview's door, or why it has none: then it reaches this Mac only.
 *  A reopen, a reload or the node's link coming back tries again. */
const reach = (open: Open) =>
  admit(open).catch((e: Error) =>
    open.tell("bakery:view-load-failed", open.spec.id, `no door to the web for this conversation: ${e.message}`),
  );

/** Loads a view's URL: a preview's once it has its door, so its first
 *  requests already go through it. */
function load(open: Open) {
  if (open.spec.kind !== "preview") return void open.view.webContents.loadURL(open.spec.url);
  void reach(open).then(() => {
    if (views.get(open.spec.id) === open) void open.view.webContents.loadURL(open.spec.url);
  });
}

/** The node's link is back, maybe after a restart that handed doors out
 *  anew: each preview gets its thread's door again. */
export function redoorPreviews() {
  for (const open of views.values()) if (open.spec.kind === "preview") void reach(open);
}

/** The node's link went down: previews reach this Mac only until it's back. */
export function doorsLost() {
  doors.clear();
}

let changed = () => {};

/** Called whenever a preview opens or closes (link.ts tells the node). */
export function onPreviewsChanged(listener: () => void) {
  changed = listener;
}

/** The open previews, for the node: which view shows which conversation. */
export function previews(): { view_id: string; stream_id: string; url: string }[] {
  return [...views.values()]
    .filter((o) => o.spec.kind === "preview")
    .map((o) => ({ view_id: o.spec.id, stream_id: o.spec.streamId, url: o.spec.url }));
}

/** The one view an agent's command may touch: a preview of the very
 *  conversation it names. Only views this module made are found, so never
 *  the app window, and never a code view. */
export function previewFor(viewId: string, streamId: string): WebContentsView {
  const open = views.get(viewId);
  if (!open || !mayDrive(open.spec, streamId)) {
    throw new Error(`no preview ${viewId} is open for ${streamId}`);
  }
  return open.view;
}

/** Opens a view, hidden until the renderer places it; an open one with the
 *  same id, kind and conversation just loads the new URL. */
export function openView(win: BrowserWindow, raw: unknown, tell: Tell) {
  const spec = checkSpec(raw);
  const existing = views.get(spec.id);
  if (existing && existing.spec.kind === spec.kind && existing.spec.streamId === spec.streamId) {
    existing.spec = spec;
    existing.tell = tell;
    load(existing);
    return;
  }
  if (existing) closeView(win, spec.id);
  const partition = partitionFor(spec.kind, spec.streamId);
  if (!guarded.has(partition)) {
    // Clipboard writes only; a page that needs the camera, location or
    // notifications opens in the system browser instead.
    const s = session.fromPartition(partition);
    const allowed = (permission: string) => permission === "clipboard-sanitized-write";
    s.setPermissionRequestHandler((_wc, permission, done) => done(allowed(permission)));
    s.setPermissionCheckHandler((_wc, permission) => allowed(permission));
    if (spec.kind === "preview") {
      s.webRequest.onBeforeRequest((details, done) => done({ cancel: !mayLeave(details.url, doors.has(partition)) }));
    }
    guarded.add(partition);
  }
  // A hidden view stays alive: the page hides it rather than closing it.
  // Hidden, it stops drawing (a view that never throttles keeps the whole
  // window drawing frames, minimised too), while its timers keep full speed
  // (main.ts's switches), so an editor's or a dev server's connection holds
  // however long it is out of sight. The operator would rather spend memory
  // than lose a view's state.
  const view = new WebContentsView({
    webPreferences: {
      partition,
      sandbox: true,
      contextIsolation: true,
      nodeIntegration: false,
      webSecurity: true,
    },
  });
  const open: Open = { spec, view, tell };
  views.set(spec.id, open);
  const { id } = spec;
  const contents = view.webContents;
  // WebRTC's own UDP would go around the door.
  if (spec.kind === "preview") contents.setWebRTCIPHandlingPolicy("disable_non_proxied_udp");
  // A preview's pop-up (a page's, or one an agent's command opens) loads in
  // the preview itself, through its door like the rest of it; the operator's
  // own "open in my browser" is the app's, never the page's.
  contents.setWindowOpenHandler(({ url }) => {
    if (spec.kind !== "preview") openOutside(url);
    else if (/^https?:\/\//i.test(url)) void contents.loadURL(url);
    return { action: "deny" };
  });
  // A code view stays on its server's origin (its iframes load freely); a
  // preview goes wherever its page goes, like any browser tab.
  const stay = (e: Electron.Event, url: string) => {
    if (open.spec.kind === "code" && originOf(url) !== originOf(open.spec.url)) {
      e.preventDefault();
      openOutside(url);
    }
  };
  contents.on("will-navigate", stay);
  contents.on("will-redirect", stay);
  contents.on("did-navigate", (_e, url) => {
    if (open.spec.kind === "code") contents.setZoomLevel(codeZoom);
    tell("bakery:view-navigate", id, url);
  });
  contents.on("did-navigate-in-page", (_e, url, isMainFrame) => {
    if (isMainFrame) tell("bakery:view-navigate", id, url);
  });
  contents.on("page-title-updated", (_e, title) => tell("bakery:view-title", id, title));
  contents.on("did-fail-load", (_e, code, description, _url, isMainFrame) => {
    // -3 is an aborted load: a newer navigation replaced it. -20 is the
    // preview's own hold (mayLeave), not an extension's.
    const why = code === -20 ? "a preview reaches the web only through its conversation's door" : `${description} (${code})`;
    if (isMainFrame && code !== -3) tell("bakery:view-load-failed", id, why);
  });
  contents.on("render-process-gone", (_e, details) => {
    tell("bakery:view-load-failed", id, `the page crashed (${details.reason})`);
  });
  contents.on("focus", () => tell("bakery:view-focus", id));
  contents.on("before-input-event", (e, input) => {
    const key = shortcutOf(input);
    if (!key) return;
    e.preventDefault();
    if (ZOOM_KEYS.includes(key)) zoomKey(key);
    else tell("bakery:view-shortcut", key);
  });
  view.setVisible(false);
  win.contentView.addChildView(view);
  load(open);
  changed();
}

export function placeView(win: BrowserWindow, id: unknown, bounds: unknown) {
  const open = find(id);
  open.bounds = bounds;
  open.view.setBounds(scaleBounds(bounds, win.webContents.getZoomFactor()));
}

export function showView(id: unknown, visible: unknown) {
  find(id).view.setVisible(visible === true);
}

function drop(win: BrowserWindow, { spec, view }: Open) {
  views.delete(spec.id);
  if (!win.isDestroyed()) win.contentView.removeChildView(view);
  view.webContents.close();
  changed();
}

export function closeView(win: BrowserWindow, id: unknown) {
  drop(win, find(id));
}

/** Every view goes with the page that placed them (a reload, a UI deploy). */
export function closeAllViews(win: BrowserWindow) {
  for (const open of [...views.values()]) drop(win, open);
}

export function reloadView(id: unknown) {
  const open = find(id);
  if (open.spec.kind !== "preview") return open.view.webContents.reload();
  void reach(open).then(() => {
    if (views.get(open.spec.id) === open) open.view.webContents.reload();
  });
}

export function viewDevTools(id: unknown) {
  find(id).view.webContents.openDevTools({ mode: "detach" });
}

/** Forgets what the view's pages stored (cookies, storage, cache): the
 *  operator's way out of a logged-in session an agent could act in. */
export async function clearViewData(id: unknown) {
  const { view } = find(id);
  await view.webContents.session.clearStorageData();
  await view.webContents.session.clearCache();
  reloadView(id);
}
