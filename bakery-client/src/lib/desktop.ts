// The desktop shell under the client: on the Mac, Electron's bridge
// (window.bakeryShell, electron/preload.ts); on the phones and the old
// desktop build, Tauri's IPC. A browser tab has neither.

export type ShellConnected = { ok: true; addr: string; token: string; startedNode: boolean };
export type ShellFailure = { ok: false; kind: string; message: string };

/** What electron/preload.ts exposes to the main window, and only to it. */
export type BakeryShell = {
  connect(): Promise<ShellConnected | ShellFailure>;
  /** A folder the operator picks, or null when they cancel. */
  pickFolder(title: string): Promise<string | null>;
  confirm(message: string): Promise<boolean>;
  /** http(s) only; opens in the system browser. */
  openExternal(url: string): Promise<void>;
  /** The external editors installed on this Mac, the last one used first;
   *  none when the node isn't on this Mac. */
  editors(): Promise<{ id: string; name: string; icon: string }[]>;
  /** Opens a thread's worktree in that editor with the file at the line. */
  openInEditor(id: string, target: { folder: string; file: string; line: number }): Promise<void>;
  /** Whether the page shows a secret now (a node reply's `secret: true`):
   *  no desktop screenshot while it does. A new page starts with none. */
  secretShown(on: boolean): Promise<void>;
  /** A Mac notification with a button per action (one to four): the
   *  pressed button's text; "open" when the operator clicks its body, which
   *  brings the app to the front; null once it's dismissed, replaced by
   *  another with the same id, taken down, or when it can't be shown
   *  (notifications turned off for Bakery). A page reload takes every one
   *  down. */
  notify(spec: { id: string; title: string; body: string; actions: string[] }): Promise<string | null>;
  /** Takes a notification down; its notify answers null. */
  closeNotification(id: string): Promise<void>;
};

export function electronShell(): BakeryShell | null {
  if (typeof window === "undefined") return null;
  return (window as unknown as { bakeryShell?: BakeryShell }).bakeryShell ?? null;
}

export type ViewKind = "preview" | "code";
/** CSS pixels of the placeholder the view covers; the shell scales them by
 *  the app's zoom. */
export type ViewBounds = { x: number; y: number; width: number; height: number };
type Unsubscribe = () => void;

/** Native Chromium views over the window (electron/views.ts), each in its own
 *  storage per kind and conversation, with no access to the shell. They paint
 *  above the page: hide one while a menu, dialog or palette covers it. */
export type BakeryViews = {
  /** Opens a view, hidden until setBounds and setVisible place it. Opening
   *  an open id with the same kind and conversation loads the new URL. */
  open(spec: { id: string; kind: ViewKind; url: string; streamId: string }): Promise<void>;
  setBounds(id: string, bounds: ViewBounds): Promise<void>;
  setVisible(id: string, visible: boolean): Promise<void>;
  close(id: string): Promise<void>;
  reload(id: string): Promise<void>;
  openDevTools(id: string): Promise<void>;
  /** Forgets everything the view's pages stored (cookies, storage, cache),
   *  then reloads it. */
  clearData(id: string): Promise<void>;
  onNavigate(listener: (id: string, url: string) => void): Unsubscribe;
  onTitle(listener: (id: string, title: string) => void): Unsubscribe;
  /** A failed load, or the page's crash. */
  onLoadFailed(listener: (id: string, error: string) => void): Unsubscribe;
  onFocus(listener: (id: string) => void): Unsubscribe;
  /** The shell's own keys, pressed in a focused view: "Cmd+K",
   *  "Cmd+Shift+P", "Cmd+J", "Cmd+\\", "Cmd+0"…"Cmd+9", "Cmd+Alt+Left",
   *  "Cmd+Alt+Right". */
  onShortcut(listener: (key: string) => void): Unsubscribe;
};

/** The views, in the Electron shell only (null under Tauri and in a tab).
 *  Every view closes when the page reloads. */
export function electronViews(): BakeryViews | null {
  if (typeof window === "undefined") return null;
  return (window as unknown as { bakeryViews?: BakeryViews }).bakeryViews ?? null;
}

export function isTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

/** A folder the operator picks, or null when they cancel; throws outside
 *  a desktop shell. */
export async function pickFolder(title: string): Promise<string | null> {
  const shell = electronShell();
  if (shell) return shell.pickFolder(title);
  const { open } = await import("@tauri-apps/plugin-dialog");
  const picked = await open({ directory: true, multiple: false, title });
  return typeof picked === "string" ? picked : null;
}
