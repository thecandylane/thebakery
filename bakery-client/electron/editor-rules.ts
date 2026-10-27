// The external editors "Open in ▾" offers after Code: what each is, where
// its command line sits in its app, and how that command opens a thread's
// worktree with one file at a line. Pure, so shell.check.ts tests it; the
// Electron side (finding apps, their icons, starting one) is editors.ts.
import { isAbsolute, relative, resolve } from "node:path";

/** How an editor's command takes a folder plus a file at a line. */
type How = "code" | "zed" | "subl" | "xed" | "jetbrains";

export type Editor = { id: string; name: string; app: string; bin: string; how: How };

/** The operator's list (X-1): VS Code, Cursor, Windsurf, Zed, Sublime Text,
 *  Xcode and the JetBrains IDEs, each found by its app's own name. */
export const EDITORS: readonly Editor[] = [
  { id: "vscode", name: "VS Code", app: "Visual Studio Code.app", bin: "Contents/Resources/app/bin/code", how: "code" },
  { id: "cursor", name: "Cursor", app: "Cursor.app", bin: "Contents/Resources/app/bin/cursor", how: "code" },
  { id: "windsurf", name: "Windsurf", app: "Windsurf.app", bin: "Contents/Resources/app/bin/windsurf", how: "code" },
  { id: "zed", name: "Zed", app: "Zed.app", bin: "Contents/MacOS/cli", how: "zed" },
  { id: "sublime", name: "Sublime Text", app: "Sublime Text.app", bin: "Contents/SharedSupport/bin/subl", how: "subl" },
  { id: "xcode", name: "Xcode", app: "Xcode.app", bin: "Contents/Developer/usr/bin/xed", how: "xed" },
  { id: "idea", name: "IntelliJ IDEA", app: "IntelliJ IDEA.app", bin: "Contents/MacOS/idea", how: "jetbrains" },
  { id: "idea-ce", name: "IntelliJ IDEA CE", app: "IntelliJ IDEA CE.app", bin: "Contents/MacOS/idea", how: "jetbrains" },
  { id: "webstorm", name: "WebStorm", app: "WebStorm.app", bin: "Contents/MacOS/webstorm", how: "jetbrains" },
  { id: "pycharm", name: "PyCharm", app: "PyCharm.app", bin: "Contents/MacOS/pycharm", how: "jetbrains" },
  { id: "pycharm-ce", name: "PyCharm CE", app: "PyCharm CE.app", bin: "Contents/MacOS/pycharm", how: "jetbrains" },
  { id: "goland", name: "GoLand", app: "GoLand.app", bin: "Contents/MacOS/goland", how: "jetbrains" },
  { id: "rustrover", name: "RustRover", app: "RustRover.app", bin: "Contents/MacOS/rustrover", how: "jetbrains" },
  { id: "clion", name: "CLion", app: "CLion.app", bin: "Contents/MacOS/clion", how: "jetbrains" },
  { id: "phpstorm", name: "PhpStorm", app: "PhpStorm.app", bin: "Contents/MacOS/phpstorm", how: "jetbrains" },
  { id: "rubymine", name: "RubyMine", app: "RubyMine.app", bin: "Contents/MacOS/rubymine", how: "jetbrains" },
  { id: "rider", name: "Rider", app: "Rider.app", bin: "Contents/MacOS/rider", how: "jetbrains" },
];

/** A file to open: the thread's worktree, a file in it, and the line. */
export type Target = { folder: string; file: string; line: number };

/** Refuses anything but an absolute folder, a file inside it (absolute or
 *  relative to it) and a line from 1; the file comes back absolute. */
export function checkTarget(raw: unknown): Target {
  const t = (raw ?? {}) as Record<string, unknown>;
  const folder = typeof t.folder === "string" ? t.folder : "";
  if (!isAbsolute(folder)) throw new Error("open in an editor: the worktree must be an absolute folder");
  const file = resolve(folder, typeof t.file === "string" ? t.file : "");
  const inside = relative(folder, file);
  if (!inside || inside.startsWith("..") || isAbsolute(inside)) {
    throw new Error("open in an editor: the file must be inside its worktree");
  }
  const line = Number(t.line ?? 1);
  if (!Number.isInteger(line) || line < 1) throw new Error("open in an editor: the line must be a whole number from 1");
  return { folder, file, line };
}

/** The command's arguments (X-2): the worktree as the editor's folder with
 *  the file open at the line; Xcode, which can't take both, the file alone. */
export function argsFor(how: How, { folder, file, line }: Target): string[] {
  switch (how) {
    case "code":
      return [folder, "--goto", `${file}:${line}`];
    case "zed":
    case "subl":
      return [folder, `${file}:${line}`];
    case "xed":
      return ["--line", String(line), file];
    case "jetbrains":
      return [folder, "--line", String(line), file];
  }
}

/** The last one used first (X-3), then the rest by name. */
export function ordered<T extends { id: string; name: string }>(found: T[], last: string | null): T[] {
  return [...found].sort((a, b) => Number(b.id === last) - Number(a.id === last) || a.name.localeCompare(b.name));
}

/** Whether the node the app reached runs on this Mac (X-5): only then are its
 *  worktrees folders an editor here can open. */
export function isLocalAddr(addr: string): boolean {
  const host = addr.startsWith("[") ? addr.slice(1, addr.indexOf("]")) : addr.split(":")[0];
  return host === "127.0.0.1" || host === "localhost" || host === "::1";
}
