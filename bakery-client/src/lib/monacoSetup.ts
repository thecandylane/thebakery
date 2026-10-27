// Minimal Monaco import — deliberately NOT the `monaco-editor` barrel.
//
// The barrel (`import * as monaco from "monaco-editor"`) eagerly registers
// the TypeScript/JSON/CSS/HTML *language features* (validation services),
// each of which asks `MonacoEnvironment.getWorker` for its OWN dedicated
// worker script (`ts.worker`, `json.worker`, ...). This slice only wires the
// core `editor.worker` (diff computation, bracket/indent services) —
// language-service workers are 07e scope. Importing the barrel anyway would
// mean those features silently get the wrong worker and error at runtime
// the moment a .ts/.json/.css/.html file opens.
//
// `editor/editor.api` is the base editor with none of that; pairing it with
// `basic-languages/monaco.contribution` (Monarch tokenizers only — no
// worker dependency) still gets every language in editorModels.svelte.ts's
// LANGUAGE_BY_EXT its syntax highlighting, lazily loaded per-language on
// first use, same as the full barrel would, just without the mismatched
// worker registrations. `./monacoContrib` adds back the editor-UI
// contributions (find, undo grouping helpers, bracket matching, multicursor,
// ...) the barrel also carries — everything except the four language
// "features" this pairing deliberately excludes.
//
import * as monaco from "monaco-editor/editor/editor.api.js";
import "monaco-editor/basic-languages/monaco.contribution.js";
import "./monacoContrib";
import EditorWorker from "monaco-editor/editor/editor.worker.js?worker";
import { THEME_CHANGED_EVENT } from "./theme";
import { colorToHex, cssColorToHex, isLightColor } from "./themeColors";

self.MonacoEnvironment = {
  getWorker() {
    return new EditorWorker();
  },
};

// theme.css is the only color origin (07d-A2 §1) — this bridge resolves its
// custom properties at call time instead of duplicating literal hex here, so
// editing one token and reloading changes Monaco too. Monaco's defineTheme
// needs literal color strings (no var()), which is why this reads
// getComputedStyle rather than just pointing colors at var(--x).
function cssVar(name: string): string {
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
}

// Monaco requires literal hex, while Appearance accepts CSS color forms.
// Normalize every token, not only the diff tints; syntax rules reject alpha.
function editorColor(name: string): string {
  return cssColorToHex(cssVar(name));
}
function syntaxColor(name: string): string {
  return colorToHex(cssVar(name)).slice(1);
}

// Retain the stable theme ID used by existing editor consumers. Its inherited
// defaults follow the actual canvas, including a light custom scheme.
function defineBakeryTheme() {
  monaco.editor.defineTheme("bakery-dark", {
    base: isLightColor(cssVar("--bg-app")) ? "vs" : "vs-dark",
    inherit: true,
    rules: [
      { token: "comment", foreground: syntaxColor("--syntax-comment") },
      { token: "keyword", foreground: syntaxColor("--syntax-keyword") },
      { token: "string", foreground: syntaxColor("--syntax-string") },
      { token: "number", foreground: syntaxColor("--syntax-number") },
      { token: "type", foreground: syntaxColor("--syntax-type") },
    ],
    colors: {
      "editor.background": editorColor("--bg-app"),
      "editor.foreground": editorColor("--fg-body"),
      "editorLineNumber.foreground": editorColor("--fg-linenum"),
      "editorLineNumber.activeForeground": editorColor("--fg-dim"),
      "editorCursor.foreground": editorColor("--accent"),
      "editor.selectionBackground": editorColor("--accent-border"),
      "editor.lineHighlightBackground": editorColor("--bg-panel"),
      "editorGutter.background": editorColor("--bg-app"),
      "editorWidget.background": editorColor("--bg-raised"),
      "editorWidget.border": editorColor("--line"),
      "editor.findMatchBackground": editorColor("--accent-bg-2"),
      "editor.findMatchHighlightBackground": editorColor("--accent-bg"),
      "diffEditor.insertedLineBackground": editorColor("--diff-add-bg-editor"),
      "diffEditor.removedLineBackground": editorColor("--diff-del-bg-editor"),
      "diffEditor.insertedTextBackground": editorColor("--diff-add-bg"),
      "diffEditor.removedTextBackground": editorColor("--diff-del-bg"),
    },
  });
}

defineBakeryTheme();

// 07d-A3 §0: a scheme switch re-reads the (now-changed) computed styles and
// re-applies — `defineTheme` alone only updates the registered definition;
// `setTheme` is what actually redraws every already-open editor with it.
window.addEventListener(THEME_CHANGED_EVENT, () => {
  defineBakeryTheme();
  monaco.editor.setTheme("bakery-dark");
});

export { monaco };
