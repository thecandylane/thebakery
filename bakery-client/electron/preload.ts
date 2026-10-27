// The main window's bridge to the shell: main.ts answers it for that window
// only. Views get no preload at all. Types: src/lib/desktop.ts.
import { contextBridge, ipcRenderer } from "electron";

contextBridge.exposeInMainWorld("bakeryShell", {
  connect: () => ipcRenderer.invoke("bakery:connect"),
  pickFolder: (title: string) => ipcRenderer.invoke("bakery:pick-folder", title),
  confirm: (message: string) => ipcRenderer.invoke("bakery:confirm", message),
  openExternal: (url: string) => ipcRenderer.invoke("bakery:open-external", url),
  editors: () => ipcRenderer.invoke("bakery:editors"),
  openInEditor: (id: string, target: unknown) => ipcRenderer.invoke("bakery:editor-open", id, target),
  secretShown: (on: boolean) => ipcRenderer.invoke("bakery:secret-shown", on),
  notify: (spec: unknown) => ipcRenderer.invoke("bakery:notify", spec),
  closeNotification: (id: string) => ipcRenderer.invoke("bakery:notify-close", id),
});

/** A listener for one of main's view events; returns its unsubscribe. */
const on = (channel: string) => (listener: (...args: unknown[]) => void) => {
  const handler = (_e: unknown, ...args: unknown[]) => listener(...args);
  ipcRenderer.on(channel, handler);
  return () => {
    ipcRenderer.removeListener(channel, handler);
  };
};

contextBridge.exposeInMainWorld("bakeryViews", {
  open: (spec: unknown) => ipcRenderer.invoke("bakery:view-open", spec),
  setBounds: (id: string, bounds: unknown) => ipcRenderer.invoke("bakery:view-bounds", id, bounds),
  setVisible: (id: string, visible: boolean) => ipcRenderer.invoke("bakery:view-visible", id, visible),
  close: (id: string) => ipcRenderer.invoke("bakery:view-close", id),
  reload: (id: string) => ipcRenderer.invoke("bakery:view-reload", id),
  openDevTools: (id: string) => ipcRenderer.invoke("bakery:view-devtools", id),
  clearData: (id: string) => ipcRenderer.invoke("bakery:view-clear", id),
  onNavigate: on("bakery:view-navigate"),
  onTitle: on("bakery:view-title"),
  onLoadFailed: on("bakery:view-load-failed"),
  onFocus: on("bakery:view-focus"),
  onShortcut: on("bakery:view-shortcut"),
});
