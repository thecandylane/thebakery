/**
 * The Claude native adapter's pin, read from the files pnpm owns — the exact
 * version in package.json and the integrity pnpm locked for it — so nothing
 * keeps a second copy by hand (V1). The node compiles in the same two values
 * from the same files (crates/bakery-node/build.rs, src/adapter_pin.rs);
 * this is that parser's twin, with the same fixtures in adapter-pin.check.mjs.
 */
import { readFileSync } from "node:fs";
import { join } from "node:path";

export const ADAPTER = "@agentclientprotocol/claude-agent-acp";

/** A release, not a range: MAJOR.MINOR.PATCH with an optional prerelease or
 *  build suffix. A range would let any install pick a release no one reviewed. */
export function exactVersion(spec) {
  const parts = spec.split(/[-+]/)[0].split(".");
  return parts.length === 3 && parts.every((part) => /^[0-9]+$/.test(part)) && /^[0-9A-Za-z.+-]+$/.test(spec);
}

export function pinnedVersion(packageJson, pkg) {
  let spec;
  try {
    spec = JSON.parse(packageJson).dependencies?.[pkg];
  } catch {
    return null;
  }
  return typeof spec === "string" && exactVersion(spec) ? spec : null;
}

/** The `resolution: {integrity: …}` under the lockfile's `packages:` entry for
 *  `pkg@version`. The same key also appears with a value on its line
 *  (`patchedDependencies:`) or with a suffix (`snapshots:`); neither matches. */
export function lockedIntegrity(lock, pkg, version) {
  const keys = [`'${pkg}@${version}':`, `${pkg}@${version}:`];
  const lines = lock.split("\n");
  const at = lines.findIndex((line) => keys.includes(line.trim()));
  if (at < 0) return null;
  const indentOf = (line) => line.length - line.trimStart().length;
  for (const line of lines.slice(at + 1)) {
    if (line.trim() && indentOf(line) <= indentOf(lines[at])) break;
    const field = line.trim();
    if (field.startsWith("resolution: {integrity: ")) {
      return field.slice("resolution: {integrity: ".length).split(/[,}]/)[0].trim() || null;
    }
  }
  return null;
}

/** The pin as bakery-client's own files state it. */
export function readAdapterPin(clientRoot) {
  const version = pinnedVersion(readFileSync(join(clientRoot, "package.json"), "utf8"), ADAPTER);
  if (!version) throw new Error(`package.json: ${ADAPTER} must be pinned to an exact version`);
  const integrity = lockedIntegrity(readFileSync(join(clientRoot, "pnpm-lock.yaml"), "utf8"), ADAPTER, version);
  if (!integrity) throw new Error(`pnpm-lock.yaml: no locked integrity for ${ADAPTER}@${version} — run pnpm install`);
  return { version, integrity };
}
