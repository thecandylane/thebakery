// The client's checks (bakery-client's `check:*` scripts) that changed files
// reach, for scripts/test-changed.sh: a check whose files load one of them,
// by import or by `new URL(…, import.meta.url)`, directly or through other
// modules; the type-checks over the folders they're in; and every check when
// the client's dependencies change. Prints one script name per line.
//
//   node scripts/client-checks.mjs <path from the repository's root>...

import { existsSync, readFileSync, statSync } from "node:fs";
import { dirname, join, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const client = join(root, "bakery-client");
const scripts = JSON.parse(readFileSync(join(client, "package.json"), "utf8")).scripts;
const changed = process.argv.slice(2).map((p) => resolve(root, p));
const under = (dir) => (file) => file.startsWith(join(client, dir) + sep);

const LOADS = [
  /\b(?:import|export)\b[^'"`;]*?\bfrom\s*["']([^"']+)["']/g,
  /\bimport\s*\(\s*["']([^"']+)["']\s*\)/g,
  /\bimport\s+["']([^"']+)["']/g,
  /\bnew URL\(\s*["']([^"']+)["']\s*,\s*import\.meta\.url\s*\)/g,
];

/** The file an import names, if it's one of the client's own. */
function resolveLoad(from, spec) {
  let base;
  if (spec.startsWith(".")) base = resolve(dirname(from), spec);
  else if (spec.startsWith("$lib/")) base = join(client, "src/lib", spec.slice("$lib/".length));
  else return null;
  for (const candidate of [base, `${base}.ts`, `${base}.tsx`, `${base}.js`, `${base}.svelte`, `${base}.svelte.ts`, join(base, "index.ts")]) {
    if (existsSync(candidate) && statSync(candidate).isFile()) return candidate;
  }
  return null;
}

/** Every file `file` loads, itself included. */
function reach(file, out = new Set()) {
  if (out.has(file)) return out;
  out.add(file);
  let text;
  try {
    text = readFileSync(file, "utf8");
  } catch {
    return out;
  }
  for (const pattern of LOADS) {
    for (const match of text.matchAll(pattern)) {
      const target = resolveLoad(file, match[1]);
      if (target) reach(target, out);
    }
  }
  return out;
}

const DEPENDENCIES = ["package.json", "pnpm-lock.yaml", "pnpm-workspace.yaml"].map((f) => join(client, f));
const everything = changed.some((f) => DEPENDENCIES.includes(f) || under("patches")(f));
const code = (f) => /\.(ts|tsx|js|mjs|svelte)$/.test(f);

const picked = [];
for (const [name, line] of Object.entries(scripts)) {
  if (!name.startsWith("check:") || name === "check:all" || name === "check:watch") continue;
  const files = [...line.matchAll(/\bnode\s+(\S+\.(?:ts|mjs|js))/g)].map((m) => join(client, m[1]));
  const reached = new Set(files.flatMap((f) => [...reach(f)]));
  if (everything || changed.some((f) => reached.has(f))) picked.push(name);
  else if (name === "check:react" && changed.some((f) => under("src")(f) && code(f))) picked.push(name);
  else if (name === "check:electron" && changed.some((f) => under("electron")(f) && code(f))) picked.push(name);
}
// svelte-check, over everything in src.
if (everything || changed.some((f) => under("src")(f))) picked.unshift("check");

console.log(picked.join("\n"));
