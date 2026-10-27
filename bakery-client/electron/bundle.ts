// The UI the desktop shell serves: bakery-client's build, installed by
// deploy (install-ui.ts) into a folder named by its own hash under
// ~/.bakery/deploy/ui, and served by main.ts at bakery://app/.
//
// The hash proves integrity, not origin. Whoever can write ~/.bakery can
// make a new folder whose hash matches and point the deploy at it. The trust
// root is write access to ~/.bakery, which agents never get: Seatbelt denies
// it at every reach, and containers never mount it. The check catches a
// half-copied or altered bundle; it is not a signature.
//
// Plain node, no electron import: deploy runs install-ui.ts on it, and
// shell.check.ts tests it.
import { createHash } from "node:crypto";
import { cpSync, existsSync, mkdirSync, readdirSync, readFileSync, renameSync, rmSync, statSync } from "node:fs";
import { isAbsolute, join, relative, resolve, sep } from "node:path";

/** Every file under `dir`, as sorted slash-separated relative paths. A
 *  symlink is refused: a bundle is plain files, and a link could point out. */
function files(dir: string): string[] {
  const out: string[] = [];
  const walk = (d: string) => {
    for (const entry of readdirSync(d, { withFileTypes: true })) {
      const path = join(d, entry.name);
      if (entry.isDirectory()) walk(path);
      else if (entry.isFile()) out.push(relative(dir, path).split(sep).join("/"));
      else throw new Error(`${path} is not a plain file`);
    }
  };
  walk(dir);
  return out.sort();
}

/** sha256 over every file's path, length and contents. */
export function hashBundle(dir: string): string {
  const hash = createHash("sha256");
  for (const file of files(dir)) {
    const body = readFileSync(join(dir, file));
    hash.update(`${file}\0${body.length}\0`);
    hash.update(body);
  }
  return hash.digest("hex");
}

/** Copies a build to `root/<its hash>` and returns the hash. An installed
 *  copy is left alone; a copy that doesn't match is never put in place. */
export function installBundle(build: string, root: string): string {
  const hash = hashBundle(build);
  const dest = join(root, hash);
  if (existsSync(dest)) return hash;
  const tmp = `${dest}.tmp-${process.pid}`;
  rmSync(tmp, { recursive: true, force: true });
  mkdirSync(root, { recursive: true });
  cpSync(build, tmp, { recursive: true });
  if (hashBundle(tmp) !== hash) {
    rmSync(tmp, { recursive: true, force: true });
    throw new Error(`the copy of ${build} does not match it`);
  }
  renameSync(tmp, dest);
  return hash;
}

/** The installed bundle `hash` names, once its contents still match. */
export function openBundle(root: string, hash: string): string {
  if (!/^[0-9a-f]{64}$/.test(hash)) throw new Error(`not a UI bundle hash: ${JSON.stringify(hash.slice(0, 80))}`);
  const dir = join(root, hash);
  if (!existsSync(dir)) throw new Error(`the UI bundle ${hash.slice(0, 12)} is not installed`);
  const actual = hashBundle(dir);
  if (actual !== hash) {
    throw new Error(`the UI bundle ${hash.slice(0, 12)} changed after it was installed (it hashes to ${actual.slice(0, 12)})`);
  }
  return dir;
}

/** The file a bakery://app path asks for, inside `dir` only: that file, or
 *  index.html for anything else (the app's own routes, like /next). */
export function resolveAsset(dir: string, pathname: string): string | null {
  let path: string;
  try {
    path = decodeURIComponent(pathname);
  } catch {
    return null;
  }
  if (path.includes("\0")) return null;
  const target = resolve(dir, `.${path}`);
  const inside = relative(dir, target);
  if (inside.startsWith("..") || isAbsolute(inside)) return null;
  return existsSync(target) && statSync(target).isFile() ? target : join(dir, "index.html");
}
