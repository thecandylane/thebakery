// Bakery's desktop shell on the Mac (the phones keep Tauri: src-tauri). One
// window, the operator's. It shows the client build deploy installed
// (bundle.ts), or a dev server only when BAKERY_UI_DEV_URL says so, and it
// alone gets the operator credential (connect.ts), through its preload, once
// trust.ts agrees.
import { app, BrowserWindow, dialog, ipcMain, Menu, net, Notification, protocol, session, shell } from "electron";
import type { IpcMainInvokeEvent, WebFrameMain } from "electron";
import { readFileSync, watchFile } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { openBundle, resolveAsset } from "./bundle.ts";
import { connect, desktopOptions, threadDoor } from "./connect.ts";
import { isLocalAddr } from "./editor-rules.ts";
import { listEditors, openInEditor } from "./editors.ts";
import { linkTo, showsSecret } from "./link.ts";
import { Notices, noticeSpec } from "./notices.ts";
import { isTrustedSender, originOf } from "./trust.ts";
import { readZoom, shortcutOf, writeZoom, ZOOM_KEYS, zoomed } from "./view-rules.ts";
import {
  clearViewData,
  closeAllViews,
  closeView,
  onZoomKey,
  openView,
  placeView,
  reloadView,
  rescaleViews,
  showView,
  useDoors,
  viewDevTools,
  zoomCodeViews,
} from "./views.ts";

const APP = "bakery://app";
const devUrl = process.env.BAKERY_UI_DEV_URL?.trim() || null;
const appOrigin = (devUrl ? originOf(devUrl) : APP) ?? APP;
const deploy = join(process.env.HOME || homedir(), ".bakery", "deploy");
/** The deployed build's UI: deploy and `deploy --ui` write its hash here. */
const pointer = join(deploy, "current", "ui-hash");

protocol.registerSchemesAsPrivileged([
  { scheme: "bakery", privileges: { standard: true, secure: true, supportFetchAPI: true } },
]);
app.enableSandbox();

let win: BrowserWindow | null = null;
/** The checked bundle being served, or why there is none. */
let ui: { dir: string } | { error: string } = { error: "it has not been loaded yet" };

function loadUi(): typeof ui {
  try {
    return { dir: openBundle(join(deploy, "ui"), readFileSync(pointer, "utf8").trim()) };
  } catch (e) {
    return { error: (e as Error).message };
  }
}

function openOutside(url: string) {
  if (/^https?:\/\//i.test(url)) void shell.openExternal(url);
}

function noUiPage(reason: string): Response {
  const text = reason.replace(/[&<>]/g, (c) => `&#${c.charCodeAt(0)};`);
  return new Response(
    `<!doctype html><meta charset="utf-8"><title>Bakery</title>` +
      `<body style="font:14px system-ui;padding:2em"><h2>Bakery has no UI to show</h2><p>${text}</p>` +
      `<p>Deploy one with <code>./scripts/bakery.sh deploy</code>, or set BAKERY_UI_DEV_URL to a dev server.</p>`,
    { headers: { "content-type": "text/html; charset=utf-8" } },
  );
}

const frameKey = (frame: WebFrameMain | null) => (frame ? `${frame.processId}:${frame.routingId}` : null);

/** Only the main window's top frame, showing the app, gets an answer. */
function bridge(channel: string, answer: (...args: unknown[]) => unknown) {
  ipcMain.handle(channel, (e: IpcMainInvokeEvent, ...args: unknown[]) => {
    const trusted =
      win !== null &&
      isTrustedSender(
        { contents: e.sender.id, frame: frameKey(e.senderFrame), url: e.senderFrame?.url ?? "" },
        { contents: win.webContents.id, frame: frameKey(win.webContents.mainFrame), origin: appOrigin },
      );
    if (!trusted) throw new Error("only Bakery's own window may use the shell");
    return answer(...args);
  });
}

/** The UI's zoom level, as VS Code keeps its own: in Electron's userData
 *  across restarts, never in the node's layout. */
let zoomLevel = 0;
const zoomFile = () => join(app.getPath("userData"), "zoom.json");

function zoomTo(level: number) {
  zoomLevel = level;
  try {
    writeZoom(zoomFile(), level);
  } catch (e) {
    console.error(`bakery: the zoom level was not kept: ${(e as Error).message}`);
  }
  if (win) {
    win.webContents.setZoomLevel(level);
    rescaleViews(win);
  }
  zoomCodeViews(level);
}

/** A zoom shortcut or menu item, from the main window or any view. */
function zoomBy(shortcut: string) {
  const next = zoomed(zoomLevel, shortcut);
  if (next !== null && next !== zoomLevel) zoomTo(next);
}

function createWindow() {
  const window = new BrowserWindow({
    width: 1440,
    height: 900,
    title: "Bakery",
    webPreferences: {
      preload: join(__dirname, "preload.cjs"),
      contextIsolation: true,
      sandbox: true,
      nodeIntegration: false,
      webSecurity: true,
    },
  });
  win = window;
  const contents = window.webContents;
  // WebRTC's own UDP (an artifact's STUN host) answers to no page policy:
  // the window sends none, as a preview sends none past its door (views.ts).
  contents.setWebRTCIPHandlingPolicy("disable_non_proxied_udp");
  // The shell owns ⌘0 and the pinch: no pinch zoom. The UI zooms by the
  // View menu's levels and their keys only (zoomBy).
  void contents.setVisualZoomLevelLimits(1, 1);
  contents.on("before-input-event", (e, input) => {
    const key = shortcutOf(input);
    if (key && ZOOM_KEYS.includes(key)) {
      e.preventDefault();
      zoomBy(key);
    }
  });
  contents.setWindowOpenHandler(({ url }) => {
    openOutside(url);
    return { action: "deny" };
  });
  const stayOnTheApp = (e: Electron.Event, url: string) => {
    if (originOf(url) !== appOrigin) {
      e.preventDefault();
      openOutside(url);
    }
  };
  contents.on("will-navigate", stayOnTheApp);
  contents.on("will-redirect", stayOnTheApp);
  // The page's only frames are srcdoc documents, the artifacts in replies
  // and the one mermaid draws in: one loads its own document and goes
  // nowhere else, so no URL (a link, a location change, a meta refresh)
  // carries anything out of it.
  contents.on("will-frame-navigate", (e) => {
    if (!e.isMainFrame && e.url !== "about:srcdoc" && e.url !== "about:blank") e.preventDefault();
  });
  // A new page (a reload, a UI deploy) places its own views again, at the
  // UI's zoom.
  contents.on("did-navigate", () => {
    closeAllViews(window);
    contents.setZoomLevel(zoomLevel);
    showsSecret(false);
    notices.closeAll();
  });
  if (devUrl) {
    // The dev server may still be starting.
    contents.on("did-fail-load", (_e, _code, _description, _url, isMainFrame) => {
      if (isMainFrame) setTimeout(() => void window.loadURL(devUrl), 500);
    });
  }
  void window.loadURL(devUrl ?? `${APP}/`);
  window.on("closed", () => {
    closeAllViews(window);
    if (win === window) win = null;
  });
}

/** The node this app reached: external editors open its worktrees only when
 *  it runs on this Mac (X-5). */
let nodeAddr: string | null = null;
const nodeIsLocal = () => nodeAddr !== null && isLocalAddr(nodeAddr);

bridge("bakery:connect", async () => {
  const connected = await connect(desktopOptions());
  // The node's link to the previews and the window's screenshot, for
  // agents' commands (link.ts).
  if (connected.ok) {
    nodeAddr = connected.addr;
    linkTo(connected.addr, connected.token, () => win);
    // Previews go out through their thread's door, which is on the node's
    // own Mac: from anywhere else, they reach this Mac only.
    const { addr, token } = connected;
    useDoors((streamId) =>
      nodeIsLocal() ? threadDoor(addr, token, streamId) : Promise.reject(new Error("the Bakery's node runs on another machine")),
    );
  }
  return connected;
});
bridge("bakery:secret-shown", (on) => showsSecret(on === true));
bridge("bakery:editors", () => (nodeIsLocal() ? listEditors() : []));
bridge("bakery:editor-open", (id, target) => {
  if (!nodeIsLocal()) throw new Error("external editors open a worktree only on the node's own Mac");
  return openInEditor(id, target);
});
bridge("bakery:pick-folder", async (title) => {
  const picked = await dialog.showOpenDialog(win!, {
    title: String(title ?? ""),
    properties: ["openDirectory", "createDirectory"],
  });
  return picked.canceled ? null : (picked.filePaths[0] ?? null);
});
bridge("bakery:confirm", async (message) => {
  const { response } = await dialog.showMessageBox(win!, {
    type: "question",
    message: String(message),
    buttons: ["OK", "Cancel"],
    defaultId: 0,
    cancelId: 1,
  });
  return response === 0;
});
bridge("bakery:open-external", (url) => openOutside(String(url)));

// Mac notifications with buttons (notices.ts). Each shown one gets an id of
// its own, so taking one down never reaches its replacement.
let shownNotices = 0;
const notices = new Notices(
  (spec) =>
    new Notification({
      id: `${spec.id}#${++shownNotices}`,
      groupId: spec.id,
      title: spec.title,
      body: spec.body,
      actions: spec.actions.map((text) => ({ type: "button" as const, text })),
    }),
  () => {
    app.focus({ steal: true });
    if (win?.isMinimized()) win.restore();
    win?.show();
    win?.focus();
  },
);
bridge("bakery:notify", (spec) => (Notification.isSupported() ? notices.notify(noticeSpec(spec)) : null));
bridge("bakery:notify-close", (id) => notices.close(String(id)));

// The views (views.ts). Their events go to the main window's page only.
const tell = (channel: string, ...args: unknown[]) => win?.webContents.send(channel, ...args);
bridge("bakery:view-open", (spec) => openView(win!, spec, tell));
bridge("bakery:view-bounds", (id, bounds) => placeView(win!, id, bounds));
bridge("bakery:view-visible", (id, visible) => showView(id, visible));
bridge("bakery:view-close", (id) => closeView(win!, id));
bridge("bakery:view-reload", (id) => reloadView(id));
bridge("bakery:view-devtools", (id) => viewDevTools(id));
bridge("bakery:view-clear", (id) => clearViewData(id));

// A page out of sight (a hidden Code or Preview view, the window minimised)
// stops drawing, but its timers keep full speed: an editor's or a dev
// server's connection holds however long it is hidden, and nothing
// reconnects or reloads as it shows again. Set before the app is ready.
app.commandLine.appendSwitch("disable-background-timer-throttling");
app.commandLine.appendSwitch("disable-features", "IntensiveWakeUpThrottling");

if (!app.requestSingleInstanceLock()) {
  app.quit();
} else {
  app.on("second-instance", () => {
    if (win?.isMinimized()) win.restore();
    win?.focus();
  });
  app.on("web-contents-created", (_e, contents) => {
    contents.on("will-attach-webview", (e) => e.preventDefault());
  });
  app.on("window-all-closed", () => app.quit());
  void app.whenReady().then(() => {
    // The default menu, with the shell's own zoom: the whole UI and the Code
    // views, never a preview. Reset is ⌘⇧0, since ⌘0 is the shell's factory
    // floor; ⌘+ is ⌘= with shift, a hidden twin of Zoom In.
    zoomLevel = readZoom(zoomFile());
    onZoomKey(zoomBy);
    zoomCodeViews(zoomLevel);
    Menu.setApplicationMenu(
      Menu.buildFromTemplate([
        { role: "appMenu" },
        { role: "editMenu" },
        {
          label: "View",
          submenu: [
            { role: "reload" },
            { role: "forceReload" },
            { role: "toggleDevTools" },
            { type: "separator" },
            { label: "Reset Zoom", accelerator: "CommandOrControl+Shift+0", click: () => zoomBy("Cmd+Shift+0") },
            { label: "Zoom In", accelerator: "CommandOrControl+=", click: () => zoomBy("Cmd+=") },
            { label: "Zoom In", accelerator: "CommandOrControl+Plus", visible: false, click: () => zoomBy("Cmd+=") },
            { label: "Zoom Out", accelerator: "CommandOrControl+-", click: () => zoomBy("Cmd+-") },
            { type: "separator" },
            { role: "togglefullscreen" },
          ],
        },
        { role: "windowMenu" },
      ]),
    );
    // Copying to the clipboard, and nothing else, without asking.
    const allowed = (permission: string) => permission === "clipboard-sanitized-write";
    session.defaultSession.setPermissionRequestHandler((_wc, permission, done) => done(allowed(permission)));
    session.defaultSession.setPermissionCheckHandler((_wc, permission) => allowed(permission));
    protocol.handle("bakery", (req) => {
      const { host, pathname } = new URL(req.url);
      if (host !== "app") return new Response("not found", { status: 404 });
      if ("error" in ui) return noUiPage(ui.error);
      const file = resolveAsset(ui.dir, pathname);
      return file ? net.fetch(pathToFileURL(file).toString()) : new Response("not found", { status: 404 });
    });
    if (!devUrl) {
      ui = loadUi();
      // A UI deploy writes a new pointer: the new bundle is checked, then
      // the window reloads on it. One that fails the check is not served.
      watchFile(pointer, { interval: 1000 }, () => {
        const next = loadUi();
        if ("error" in next && "dir" in ui) {
          console.error(`bakery: kept the current UI: ${next.error}`);
          return;
        }
        if ("dir" in next && "dir" in ui && next.dir === ui.dir) return;
        ui = next;
        win?.webContents.reloadIgnoringCache();
      });
    }
    createWindow();
    app.on("activate", () => {
      if (!win) createWindow();
    });
  });
}
