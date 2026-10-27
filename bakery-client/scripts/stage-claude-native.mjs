#!/usr/bin/env node
/**
 * Build the production Node-module closure for Bakery's pinned Claude ACP
 * adapter. This is intentionally an offline staging step: every source is a
 * package already installed by pnpm, and no credential/configuration path is
 * copied into the desktop bundle.
 *
 * pnpm stores dependencies behind symlinks under `.pnpm`. Tauri resources
 * must be self-contained instead, so recursively copy only runtime
 * dependencies at the same resolution locations Node would use. Keeping each
 * dependency nested below its consumer avoids flattening version conflicts.
 */
import { cpSync, existsSync, lstatSync, mkdirSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { basename, dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { ADAPTER as PACKAGE, readAdapterPin } from "./adapter-pin.mjs";

const clientRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
// From package.json and pnpm-lock.yaml, like the node's own pin (V1).
const { version: VERSION, integrity: INTEGRITY } = readAdapterPin(clientRoot);
const stageRoot = join(clientRoot, "src-tauri", ".runtime", "claude-native");
const stageModules = join(stageRoot, "node_modules");
const copied = new Set();

function manifestAt(packageRoot) {
  const path = join(packageRoot, "package.json");
  return JSON.parse(readFileSync(path, "utf8"));
}

function copyPackageTree(source, destination) {
  cpSync(source, destination, {
    recursive: true,
    dereference: true,
    // Reconstruct runtime dependency locations ourselves. Copying pnpm's
    // nested symlink forest would either escape the stage or drag in the
    // entire frontend dependency tree.
    filter(path) {
      return path === source || basename(path) !== "node_modules";
    },
  });
}

function packageRootForResolvedFile(resolvedFile, expectedName) {
  let current = dirname(resolvedFile);
  while (current !== dirname(current)) {
    const manifest = join(current, "package.json");
    if (existsSync(manifest) && manifestAt(current).name === expectedName) return current;
    current = dirname(current);
  }
  throw new Error(`could not locate package root for ${expectedName}`);
}

function dependencyRoot(packageRoot, dependency) {
  // pnpm's own dependency links sit at the nearest ancestor
  // `node_modules`. This path also works for packages that intentionally do
  // not expose a `main`/`exports` entry (their manifest may still describe a
  // transitive runtime dependency).
  let current = packageRoot;
  while (current !== dirname(current)) {
    const candidate = join(current, "node_modules", ...dependency.split("/"));
    if (existsSync(join(candidate, "package.json"))) return realpathSync(candidate);
    current = dirname(current);
  }
  const requireFromPackage = createRequire(join(packageRoot, "package.json"));
  const resolved = requireFromPackage.resolve(dependency);
  return realpathSync(packageRootForResolvedFile(resolved, dependency));
}

function destinationForDependency(parentDestination, dependency) {
  return join(parentDestination, "node_modules", ...dependency.split("/"));
}

function stagePackage(source, destination) {
  const sourceReal = realpathSync(source);
  const key = `${sourceReal}\0${destination}`;
  if (copied.has(key)) return;
  copied.add(key);
  copyPackageTree(sourceReal, destination);
  const manifest = manifestAt(sourceReal);
  const requiredPeers = Object.fromEntries(
    Object.entries(manifest.peerDependencies ?? {}).filter(
      ([name]) => manifest.peerDependenciesMeta?.[name]?.optional !== true,
    ),
  );
  const dependencies = {
    ...(manifest.dependencies ?? {}),
    ...(manifest.optionalDependencies ?? {}),
    ...requiredPeers,
  };
  for (const dependency of Object.keys(dependencies).sort()) {
    try {
      stagePackage(dependencyRoot(sourceReal, dependency), destinationForDependency(destination, dependency));
    } catch (error) {
      // Optional dependencies genuinely absent on this platform must remain
      // optional. Every non-optional dependency is a packaging failure.
      if (Object.hasOwn(manifest.optionalDependencies ?? {}, dependency)) continue;
      throw new Error(`${manifest.name}@${manifest.version} dependency ${dependency}: ${error.message}`);
    }
  }
}

const sourceRoot = realpathSync(join(clientRoot, "node_modules", ...PACKAGE.split("/")));
const sourceManifest = manifestAt(sourceRoot);
if (sourceManifest.name !== PACKAGE || sourceManifest.version !== VERSION) {
  throw new Error(`expected ${PACKAGE}@${VERSION}, found ${sourceManifest.name}@${sourceManifest.version}`);
}

if (existsSync(stageRoot)) rmSync(stageRoot, { recursive: true, force: true });
mkdirSync(stageModules, { recursive: true });
stagePackage(sourceRoot, join(stageModules, ...PACKAGE.split("/")));
writeFileSync(
  join(stageRoot, ".bakery-native-manifest.json"),
  `${JSON.stringify({ package: PACKAGE, version: VERSION, integrity: INTEGRITY, source: relative(clientRoot, sourceRoot) }, null, 2)}\n`,
);

const stagedEntry = join(stageModules, ...PACKAGE.split("/"), "dist", "index.js");
if (!existsSync(stagedEntry) || !lstatSync(stagedEntry).isFile()) {
  throw new Error("staged Claude ACP adapter is missing dist/index.js");
}
process.stdout.write(`staged ${PACKAGE}@${VERSION} at ${stageRoot}\n`);
