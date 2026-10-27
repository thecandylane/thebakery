// External editors on this Mac: which of editor-rules.ts's list are
// installed, each with its app's icon, and starting one at a thread's file
// and line. main.ts offers them only when the node runs on this Mac.
import { app } from "electron";
import { spawn } from "node:child_process";
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import { argsFor, checkTarget, EDITORS, ordered } from "./editor-rules.ts";

const lastFile = () => join(app.getPath("userData"), "editor.json");

/** Those installed in the Mac's Applications folder or the operator's. */
function installed() {
  const dirs = ["/Applications", join(homedir(), "Applications")];
  return EDITORS.flatMap((editor) => {
    const dir = dirs.find((d) => existsSync(join(d, editor.app, editor.bin)));
    return dir ? [{ ...editor, path: join(dir, editor.app) }] : [];
  });
}

function lastUsed(): string | null {
  try {
    const id: unknown = JSON.parse(readFileSync(lastFile(), "utf8")).id;
    return typeof id === "string" ? id : null;
  } catch {
    return null;
  }
}

/** The installed editors, the last one used first, each with its icon. */
export async function listEditors(): Promise<{ id: string; name: string; icon: string }[]> {
  const found = await Promise.all(
    installed().map(async (e) => ({
      id: e.id,
      name: e.name,
      icon: (await app.getFileIcon(e.path, { size: "normal" })).toDataURL(),
    })),
  );
  return ordered(found, lastUsed());
}

/** Opens the file at its line in an installed editor, which runs on its own:
 *  this returns once the editor's command has started. */
export async function openInEditor(id: unknown, raw: unknown): Promise<void> {
  const editor = installed().find((e) => e.id === id);
  if (!editor) throw new Error(`no editor ${JSON.stringify(id)} is installed`);
  const target = checkTarget(raw);
  if (!existsSync(target.folder)) throw new Error(`there is no folder ${target.folder} on this Mac`);
  // The shell's own Electron settings would confuse an editor built on Electron.
  const env = Object.fromEntries(Object.entries(process.env).filter(([k]) => !k.startsWith("ELECTRON_")));
  const child = spawn(join(editor.path, editor.bin), argsFor(editor.how, target), { detached: true, stdio: "ignore", env });
  await new Promise<void>((started, failed) => {
    child.once("spawn", started);
    child.once("error", failed);
  });
  child.unref();
  try {
    writeFileSync(lastFile(), JSON.stringify({ id: editor.id }));
  } catch (e) {
    console.error(`bakery: the last editor used was not kept: ${(e as Error).message}`);
  }
}
