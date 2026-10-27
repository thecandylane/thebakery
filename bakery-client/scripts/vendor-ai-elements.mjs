// Re-vendor the upstream AI Elements + shadcn/ui sources this app owns.
//
// AI Elements is a source-owned library: `npx ai-elements` copies component
// source into your project rather than adding a runtime dependency. That
// makes provenance a real obligation — this script is how the copy is
// reproduced and audited, instead of "someone pasted some files once".
//
//   node scripts/vendor-ai-elements.mjs          # write files + provenance
//   node scripts/vendor-ai-elements.mjs --check  # verify nothing drifted
//
// The one mechanical patch applied to shadcn sources is recorded in the
// generated provenance file: the registry ships `import { cn } from "cn"`,
// a placeholder the shadcn CLI rewrites to the consuming project's utils
// path. We rewrite it to `@/lib/utils` exactly as the CLI would.
import { createHash } from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const AI_ELEMENTS_REGISTRY = "https://registry.ai-sdk.dev/all.json";
const SHADCN_STYLE = "new-york-v4";
const SHADCN_REGISTRY = (name) => `https://ui.shadcn.com/r/styles/${SHADCN_STYLE}/${name}.json`;

/** Only what this app actually renders. Adding one here is a deliberate
 *  act: every component brings dependencies and surface area. */
const AI_ELEMENTS = [
  "conversation",
  "message",
  "prompt-input",
  "tool",
  "code-block",
  "loader",
  "context",
  "confirmation",
  "model-selector",
  "queue",
  "suggestion",
  "task",
];

/** shadcn primitives the components above import, plus their transitive
 *  registry dependencies (button-group→separator, command→dialog,
 *  input-group→input/textarea). */
const SHADCN = [
  "button",
  "button-group",
  "tooltip",
  "badge",
  "collapsible",
  "command",
  "dropdown-menu",
  "hover-card",
  "input-group",
  "select",
  "progress",
  "alert",
  "separator",
  "dialog",
  "input",
  "textarea",
  "scroll-area",
];

const AI_ELEMENTS_DIR = path.join(ROOT, "src/lib/ai-elements");
const UI_DIR = path.join(ROOT, "src/components/ui");
const PROVENANCE = path.join(ROOT, "src/lib/ai-elements/PROVENANCE.md");

const sha = (s) => createHash("sha256").update(s).digest("hex").slice(0, 16);

async function getJson(url) {
  const res = await fetch(url);
  if (!res.ok) throw new Error(`${res.status} ${res.statusText} for ${url}`);
  return res.json();
}

/** Local patches applied to vendored source, in order.
 *
 *  Two kinds live here, and the distinction matters when reading the
 *  provenance table:
 *
 *  - Import rewrites the shadcn CLI performs itself. The registry ships
 *    authoring-time placeholders (`"cn"`, `@/registry/<style>/ui/...`) that
 *    do not resolve in a consuming project; `npx shadcn add` rewrites them
 *    to the project's own paths. We are the CLI here, so we do the same.
 *    Without these the app does not build at all.
 *
 *  - Adaptations to the `ai` version this repo pins. The registry declares
 *    `ai` with no version, and the components were written against an older
 *    one: they carry `@ts-expect-error` for tool-approval states that DO
 *    exist in ai@7 (an unused directive is itself an error), and read two
 *    usage fields that ai@7 moved into detail objects. Both renames below
 *    are exact semantic equivalents, not casts.
 *
 *  Every rule must match at least once across the whole vendored set. A rule
 *  that matches nothing means upstream changed underneath us, and silently
 *  skipping it would be how a stale patch list rots.
 */
const PATCHES = [
  {
    id: "cn-import",
    scope: "shadcn",
    match: /from "cn"/g,
    replace: 'from "@/lib/utils"',
    why: '`from "cn"` → `from "@/lib/utils"` (the rewrite `npx shadcn add` performs)',
  },
  {
    id: "registry-import",
    scope: "shadcn",
    match: /@\/registry\/[^/]+\/ui\//g,
    replace: "@/components/ui/",
    why: "`@/registry/<style>/ui/*` → `@/components/ui/*` (same CLI rewrite; unresolvable otherwise)",
  },
  {
    id: "stale-ts-expect-error",
    scope: "ai-elements",
    match: /^[ \t]*\/\/ @ts-expect-error state only available in AI SDK v6\r?\n/gm,
    replace: "",
    why: "removed `@ts-expect-error` for tool-approval states that exist in the pinned `ai` (an unused directive is an error)",
  },
  {
    id: "usage-reasoning-tokens",
    scope: "ai-elements",
    match: /usage\?\.reasoningTokens/g,
    replace: "usage?.outputTokenDetails?.reasoningTokens",
    why: "`usage.reasoningTokens` → `usage.outputTokenDetails.reasoningTokens` (ai@7 moved it; same value)",
  },
  {
    id: "usage-cached-input-tokens",
    scope: "ai-elements",
    match: /usage\?\.cachedInputTokens/g,
    replace: "usage?.inputTokenDetails?.cacheReadTokens",
    why: "`usage.cachedInputTokens` → `usage.inputTokenDetails.cacheReadTokens` (ai@7 moved it; same value)",
  },
  {
    id: "model-selector-remote-logo",
    scope: "ai-elements",
    match: /export type ModelSelectorLogoProps[\s\S]*?(?=export type ModelSelectorNameProps)/g,
    replace: "",
    why: "omitted `ModelSelectorLogo`/`ModelSelectorLogoGroup` because they fetch provider artwork from `models.dev`; Bakery renders the provider/model name locally instead",
  },
  {
    id: "prompt-input-provider-drafts",
    scope: "prompt-input-overlay",
    match: /this scope is applied by patchPromptInput/g,
    replace: "",
    why: "wired provider-mode attachment validation and stream+agent-scoped in-process draft-file recovery through Bakery's local integration hook; preserved attachment ids across async submit conversion",
  },
];

function replaceExact(source, before, after, label) {
  if (!source.includes(before)) {
    throw new Error(`prompt-input overlay no longer matches ${label} — review upstream before re-vendoring`);
  }
  return source.replace(before, after);
}

/** PromptInputProvider bypasses upstream's local attachment validator. Bakery
 * uses that provider for controlled draft text, so this small, reproducible
 * overlay delegates provider attachment state to our integration hook. The
 * hook owns policy and remount recovery; the vendored component remains the
 * UI/form implementation. */
function patchPromptInput(source, hits) {
  const id = "prompt-input-provider-drafts";
  let out = source;
  out = replaceExact(
    out,
    `import { cn } from "@/lib/utils";\nimport type { ChatStatus, FileUIPart } from "ai";`,
    `import { cn } from "@/lib/utils";\nimport {\n  type PromptAttachmentConstraints,\n  type PromptAttachmentError,\n  usePromptAttachmentDrafts,\n} from "@/lib/react/promptAttachmentDrafts";\nimport type { ChatStatus, FileUIPart } from "ai";`,
    "integration import",
  );
  out = replaceExact(
    out,
    `};\n\nexport type TextInputContext = {`,
    `};\n\nexport type PromptInputError = PromptAttachmentError;\n\nexport type TextInputContext = {`,
    "public error alias",
  );
  out = replaceExact(
    out,
    `  __registerFileInput: (\n    ref: RefObject<HTMLInputElement | null>,\n    open: () => void\n  ) => void;`,
    `  __registerFileInput: (\n    ref: RefObject<HTMLInputElement | null>,\n    open: () => void,\n    constraints: PromptAttachmentConstraints\n  ) => void;`,
    "provider registration contract",
  );
  out = replaceExact(
    out,
    `export type PromptInputProviderProps = PropsWithChildren<{\n  initialInput?: string;\n}>;`,
    `export type PromptInputProviderProps = PropsWithChildren<{\n  initialInput?: string;\n  /** Keeps unsent browser \`File\`s alive while a pane is closed and reopened\n   * in this app process. The node still owns every uploaded attachment; this\n   * cache contains only pre-submit draft files and never credentials. */\n  attachmentDraftKey?: string;\n}>;`,
    "draft-key prop",
  );
  out = replaceExact(
    out,
    `export function PromptInputProvider({\n  initialInput: initialTextInput = "",\n  children,`,
    `export function PromptInputProvider({\n  initialInput: initialTextInput = "",\n  attachmentDraftKey,\n  children,`,
    "draft-key destructuring",
  );
  out = replaceExact(
    out,
    `  // ----- attachments state (global when wrapped)\n  const [attachmentFiles, setAttachmentFiles] = useState<\n    (FileUIPart & { id: string })[]\n  >([]);\n  const fileInputRef = useRef<HTMLInputElement | null>(null);\n  const openRef = useRef<() => void>(() => {});\n\n  const add = useCallback((files: File[] | FileList) => {\n    const incoming = Array.from(files);\n    if (incoming.length === 0) {\n      return;\n    }\n\n    setAttachmentFiles((prev) =>\n      prev.concat(\n        incoming.map((file) => ({\n          id: nanoid(),\n          type: "file" as const,\n          url: URL.createObjectURL(file),\n          mediaType: file.type,\n          filename: file.name,\n        }))\n      )\n    );\n  }, []);\n\n  const remove = useCallback((id: string) => {\n    setAttachmentFiles((prev) => {\n      const found = prev.find((f) => f.id === id);\n      if (found?.url) {\n        URL.revokeObjectURL(found.url);\n      }\n      return prev.filter((f) => f.id !== id);\n    });\n  }, []);\n\n  const clear = useCallback(() => {\n    setAttachmentFiles((prev) => {\n      for (const f of prev) {\n        if (f.url) {\n          URL.revokeObjectURL(f.url);\n        }\n      }\n      return [];\n    });\n  }, []);\n\n  // Keep a ref to attachments for cleanup on unmount (avoids stale closure)\n  const attachmentsRef = useRef(attachmentFiles);\n  attachmentsRef.current = attachmentFiles;\n\n  // Cleanup blob URLs on unmount to prevent memory leaks\n  useEffect(() => {\n    return () => {\n      for (const f of attachmentsRef.current) {\n        if (f.url) {\n          URL.revokeObjectURL(f.url);\n        }\n      }\n    };\n  }, []);`,
    `  // ----- attachments state (global when wrapped)\n  const {\n    files: attachmentFiles,\n    add,\n    remove,\n    clear,\n    configure,\n  } = usePromptAttachmentDrafts(attachmentDraftKey);\n  const fileInputRef = useRef<HTMLInputElement | null>(null);\n  const openRef = useRef<() => void>(() => {});`,
    "provider attachment state",
  );
  out = replaceExact(
    out,
    `  const __registerFileInput = useCallback(\n    (ref: RefObject<HTMLInputElement | null>, open: () => void) => {\n      fileInputRef.current = ref.current;\n      openRef.current = open;\n    },\n    []\n  );`,
    `  const __registerFileInput = useCallback(\n    (\n      ref: RefObject<HTMLInputElement | null>,\n      open: () => void,\n      constraints: PromptAttachmentConstraints\n    ) => {\n      fileInputRef.current = ref.current;\n      openRef.current = open;\n      configure(constraints);\n    },\n    [configure]\n  );`,
    "constraint registration",
  );
  out = replaceExact(
    out,
    `  onError?: (err: {\n    code: "max_files" | "max_file_size" | "accept";\n    message: string;\n  }) => void;`,
    `  onError?: (err: PromptInputError) => void;`,
    "shared error type",
  );
  out = replaceExact(
    out,
    `export type PromptInputMessage = {\n  text: string;\n  files: FileUIPart[];\n};`,
    `export type PromptInputMessage = {\n  text: string;\n  files: (FileUIPart & { id: string })[];\n};`,
    "submit attachment identity contract",
  );
  out = replaceExact(
    out,
    `      files.map(async ({ id, ...item }) => {`,
    `      files.map(async (item) => {`,
    "submit attachment id preservation",
  );
  out = replaceExact(
    out,
    `      .then((convertedFiles: FileUIPart[]) => {`,
    `      .then((convertedFiles: (FileUIPart & { id: string })[]) => {`,
    "converted attachment identity type",
  );
  out = replaceExact(
    out,
    `  useEffect(() => {\n    if (!usingProvider) return;\n    controller.__registerFileInput(inputRef, () => inputRef.current?.click());\n  }, [usingProvider, controller]);`,
    `  useEffect(() => {\n    if (!usingProvider) return;\n    controller.__registerFileInput(\n      inputRef,\n      () => inputRef.current?.click(),\n      { accept, multiple, maxFiles, maxFileSize, onError }\n    );\n  }, [usingProvider, controller, accept, multiple, maxFiles, maxFileSize, onError]);`,
    "constraint handoff",
  );
  hits.set(id, (hits.get(id) ?? 0) + 1);
  return { source: out, note: PATCHES.find((rule) => rule.id === id).why };
}

/** Applies every rule for `scope`, returning the patched source and a note
 *  describing what actually changed in THIS file. */
function patch(source, scope, hits) {
  let out = source;
  const applied = [];
  for (const rule of PATCHES) {
    if (rule.scope !== scope) continue;
    const found = out.match(rule.match);
    if (!found) continue;
    hits.set(rule.id, (hits.get(rule.id) ?? 0) + found.length);
    applied.push(rule.why);
    out = out.replace(rule.match, rule.replace);
  }
  return { source: out, note: applied.length === 0 ? "no (verbatim)" : applied.join("; ") };
}

async function main() {
  const check = process.argv.includes("--check");
  const rows = [];
  const writes = [];
  const hits = new Map();

  const aiRegistry = await getJson(AI_ELEMENTS_REGISTRY);
  const byName = new Map();
  for (const file of aiRegistry.files ?? []) {
    byName.set(path.basename(file.path, ".tsx"), file);
  }
  for (const name of AI_ELEMENTS) {
    const file = byName.get(name);
    if (!file) throw new Error(`AI Elements registry has no component "${name}"`);
    const base = patch(file.content, "ai-elements", hits);
    const { source, note } =
      name === "prompt-input"
        ? (() => {
            const overlay = patchPromptInput(base.source, hits);
            return {
              source: overlay.source,
              note: base.note === "no (verbatim)" ? overlay.note : `${base.note}; ${overlay.note}`,
            };
          })()
        : base;
    const target = path.join(AI_ELEMENTS_DIR, `${name}.tsx`);
    writes.push([target, source]);
    rows.push({
      file: `src/lib/ai-elements/${name}.tsx`,
      source: `${AI_ELEMENTS_REGISTRY} → ${file.path}`,
      sha: sha(source),
      patched: note,
    });
  }

  for (const name of SHADCN) {
    const entry = await getJson(SHADCN_REGISTRY(name));
    for (const file of entry.files ?? []) {
      const base = path.basename(file.path);
      const { source, note } = patch(file.content, "shadcn", hits);
      const target = path.join(UI_DIR, base);
      writes.push([target, source]);
      rows.push({
        file: `src/components/ui/${base}`,
        source: `${SHADCN_REGISTRY(name)} → ${file.path}`,
        sha: sha(source),
        patched: note,
      });
    }
  }

  // A patch rule that no longer matches anything means upstream moved. Fail
  // loudly: quietly dropping it is how a vendored tree rots into a fork.
  const dead = PATCHES.filter((r) => !hits.has(r.id));
  if (dead.length > 0) {
    throw new Error(
      `these patch rules matched nothing — upstream changed, review them: ${dead.map((r) => r.id).join(", ")}`,
    );
  }

  if (check) {
    let drift = 0;
    for (const [target, content] of writes) {
      const actual = fs.existsSync(target) ? fs.readFileSync(target, "utf8") : null;
      if (actual !== content) {
        console.error(`DRIFT: ${path.relative(ROOT, target)}`);
        drift++;
      }
    }
    console.log(drift === 0 ? `ok — ${writes.length} vendored files match upstream` : `${drift} file(s) drifted`);
    process.exit(drift === 0 ? 0 : 1);
  }

  fs.mkdirSync(AI_ELEMENTS_DIR, { recursive: true });
  fs.mkdirSync(UI_DIR, { recursive: true });
  for (const [target, content] of writes) fs.writeFileSync(target, content);

  const deps = (aiRegistry.dependencies ?? []).join(", ");
  const doc = `# Vendored upstream sources — provenance

Generated by \`node scripts/vendor-ai-elements.mjs\`. Re-verify with
\`node scripts/vendor-ai-elements.mjs --check\` (fails on any local drift).

Fetched: ${new Date().toISOString().slice(0, 10)}

## What this is

[AI Elements](https://elements.ai-sdk.dev/docs) is **source-owned**: the
upstream project distributes component source to copy into your app, not an
npm runtime dependency. These files are therefore ours to maintain, and this
document is the record of where each one came from.

- AI Elements registry: \`${AI_ELEMENTS_REGISTRY}\` (the exact endpoint
  \`npx ai-elements@latest\` reads). The registry is unversioned; the
  content hashes below pin what we actually took.
- shadcn/ui primitives: style \`${SHADCN_STYLE}\`, from
  \`https://ui.shadcn.com/r/styles/${SHADCN_STYLE}/<name>.json\`.
- Licenses: AI Elements is Apache-2.0 (Copyright 2023 Vercel, Inc.; full
  text in \`LICENSE\` beside this file). shadcn/ui is MIT (Copyright (c) 2023
  shadcn; \`src/components/ui/LICENSE.md\`). The files here are modified
  copies: "Local patches" below lists every change.

## Primitive variant: Radix (\`${SHADCN_STYLE}\`), not Base UI

The slice prefers Base UI "if supported by the selected upstream
components". It is not, and this was checked rather than assumed:

- \`https://ui.shadcn.com/r/styles/base-ui/<name>.json\` → 404,
  \`https://ui.shadcn.com/r/base-ui/<name>.json\` → 404, and
  \`base-ui.shadcn.com\` does not resolve. The only \`base-ui\` string in
  shadcn's registry index belongs to an unrelated third-party registry.
- Upstream AI Elements itself depends on Radix directly
  (\`@radix-ui/react-use-controllable-state\` in \`reasoning.tsx\`).

So: one variant, \`${SHADCN_STYLE}\`, which uses the unified \`radix-ui\`
package. No mixing.

## Runtime dependencies these components pull in

Declared by the AI Elements registry: ${deps}.

Installed for the components we actually vendor: \`lucide-react\`,
\`radix-ui\`, \`cmdk\`, \`class-variance-authority\`, \`clsx\`,
\`tailwind-merge\`, \`streamdown\`, \`use-stick-to-bottom\`, \`shiki\`,
\`tokenlens\`, \`nanoid\`.

**\`ai\` (the AI SDK) is a devDependency only.** Every import of it in these
components is \`import type\` (\`UIMessage\`, \`ToolUIPart\`, \`ChatStatus\`,
\`FileUIPart\`, \`LanguageModelUsage\`). No AI SDK runtime, transport,
\`useChat\`, or provider client is used — Bakery's node remains the only
harness. \`@xyflow/react\` and \`motion\` are NOT installed: they belong to
canvas/flow and shimmer components we deliberately do not vendor.

## Local patches

Applied by \`scripts/vendor-ai-elements.mjs\`, which fails if any rule stops
matching — a stale rule is how a vendored tree silently becomes a fork.

${PATCHES.map((r) => `- **${r.id}** (${r.scope}, ${hits.get(r.id) ?? 0} site(s)): ${r.why}`).join("\n")}

The \`ai\` adaptations exist because the registry declares \`ai\` with no
version and these components were written against an older one. Both field
renames are exact equivalents in the pinned version, not casts: reasoning
tokens moved to \`outputTokenDetails\`, cached input tokens to
\`inputTokenDetails.cacheReadTokens\`.

## Files

| File | Upstream source | sha256 (16) | Local patch |
|---|---|---|---|
${rows.map((r) => `| \`${r.file}\` | ${r.source} | \`${r.sha}\` | ${r.patched} |`).join("\n")}

## Not vendored, and why

\`artifact\`, \`canvas\`, \`chain-of-thought\`, \`checkpoint\`, \`connection\`,
\`controls\`, \`edge\`, \`image\`, \`inline-citation\`,
\`node\`, \`open-in-chat\`, \`panel\`, \`plan\`, \`reasoning\`,
\`shimmer\`, \`sources\`, \`toolbar\`,
\`web-preview\` — none has a real Bakery fact behind it at this checkpoint.
Shipping them would mean inert controls, which this slice forbids.
\`reasoning\` in particular is withheld deliberately: Bakery records no
model reasoning stream, and inventing one would be a lie about the agent.

\`model-selector\`, \`queue\`, \`suggestion\`, and \`task\` are vendored
because the React conversation island binds them to real Bakery facts:
session-advertised models, Heart's durable per-agent queue, real
\`ask_user\` choices, and observable activity/tool history respectively.
\`ModelSelectorLogo\` and its image group are intentionally omitted from the
vendored module: upstream fetches their artwork from a remote host, while
Bakery's desktop surface must remain self-contained.
`;
  fs.writeFileSync(PROVENANCE, doc);
  console.log(`vendored ${writes.length} files; provenance → ${path.relative(ROOT, PROVENANCE)}`);
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
