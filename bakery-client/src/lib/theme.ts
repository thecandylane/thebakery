import {
  APPEARANCE_KEY, appearanceCodec, AUTO_LABEL, BUILTIN_THEMES, CUSTOM_TOKEN_KEYS, EXTRA_KEYS, THEME_LABELS,
  type AppearanceState, type BuiltinTheme, type ColorScheme, type CustomTokenKey,
  type CustomTokens, type SchemeExtras, type Theme,
} from "./themeModel";
import { colorToHex, isLightColor } from "./themeColors";
import { codeAppearance, uiAppearance, uiAppearanceSet } from "./api.generated";
import { onEnvelope } from "./events";
import { appearanceFromNode } from "./themeModel";

export * from "./themeModel";
export { colorToHex } from "./themeColors";
export const THEMES = BUILTIN_THEMES;
/** Every applied change, including same-scheme edits and temporary previews.
 * Monaco, xterm and the shell subscribe; no duplicate React theme cache. */
export const THEME_CHANGED_EVENT = "bakery:theme-changed";

export function loadAppearance(): AppearanceState {
  try { return appearanceCodec.read(localStorage); }
  catch { return appearanceCodec.empty(); }
}

export function appearanceStorageIssue(): string | null {
  try { appearanceCodec.read(localStorage); return null; }
  catch { return "Your saved color library could not be read. It has not been overwritten. Download a backup before resetting it."; }
}

export function exportAppearanceBackup(): string {
  try { return localStorage.getItem(APPEARANCE_KEY) ?? JSON.stringify(appearanceCodec.read(localStorage), null, 2); }
  catch { throw new Error("Browser storage is unavailable. A backup could not be read."); }
}

/** Explicit settings-only recovery, after the operator confirms the loss of
 * saved custom schemes. Never called during startup or normal read failures. */
export function resetAppearanceLibrary(): void { persist(appearanceCodec.empty()); }

export function loadTheme(): Theme { return loadAppearance().selected; }
export function themeLabel(id: Theme, state = loadAppearance()): string {
  if (id === "auto") return AUTO_LABEL;
  return appearanceCodec.builtin(id) ? THEME_LABELS[id] : state.schemes.find((s) => s.id === id)?.name ?? AUTO_LABEL;
}

function render(base: BuiltinTheme, tokens: CustomTokens, id: string, preview = false, extras: SchemeExtras = {}) {
  const root = document.documentElement;
  if (base === "dark") root.removeAttribute("data-theme");
  else root.setAttribute("data-theme", base);
  root.setAttribute("data-color-scheme", id);
  root.toggleAttribute("data-theme-preview", preview);
  for (const key of CUSTOM_TOKEN_KEYS) {
    const value = tokens[key];
    if (value) root.style.setProperty(key, value);
    else root.style.removeProperty(key);
  }
  for (const key of EXTRA_KEYS) {
    const value = extras[key];
    if (value) root.style.setProperty(key, value);
    else root.style.removeProperty(key);
  }
  // Native form controls follow the actual surface, including a custom
  // light scheme built on a dark preset.
  const mode = isLightColor(getComputedStyle(root).getPropertyValue("--bg-app").trim()) ? "light" : "dark";
  root.style.colorScheme = mode;
  root.setAttribute("data-color-mode", mode);
  window.dispatchEvent(new CustomEvent(THEME_CHANGED_EVENT, { detail: { preview } }));
}

const lightQuery = () => matchMedia("(prefers-color-scheme: light)");

/** Auto's palette right now: Warm in dark mode, Parchment in light. */
export function autoBase(): BuiltinTheme {
  return lightQuery().matches ? "parchment" : "warm";
}

function renderState(state: AppearanceState) {
  const scheme = state.schemes.find((s) => s.id === state.selected);
  // A two-variant theme follows the Mac, as Auto does.
  const variant = scheme?.dark && !lightQuery().matches ? scheme.dark : scheme;
  const base = variant?.base ?? (state.selected === "auto" ? autoBase() : (state.selected as BuiltinTheme));
  render(base, variant?.tokens ?? {}, state.selected, false, scheme?.extras);
}

function writableState(): AppearanceState {
  // Fail closed rather than overwriting a corrupt/newer library with fallback.
  try { return appearanceCodec.read(localStorage); }
  catch { throw new Error("Cannot read the saved color library. No saved schemes were changed."); }
}

function persist(state: AppearanceState) {
  const checked = appearanceCodec.decode(JSON.stringify(state));
  try { localStorage.setItem(APPEARANCE_KEY, JSON.stringify(checked)); }
  catch { throw new Error("Appearance could not be saved. Storage may be full or disabled; your previous selection is unchanged."); }
  renderState(checked);
  // The node keeps it too, for other windows, a new app origin and the Baker.
  void uiAppearanceSet({ appearance: checked }).catch(() => {});
}

/** Take a checked appearance from the node: cache it for the pre-paint
 *  script and show it. Never sends it back. */
function adopt(state: AppearanceState) {
  const raw = JSON.stringify(state);
  try {
    if (localStorage.getItem(APPEARANCE_KEY) === raw) return;
    localStorage.setItem(APPEARANCE_KEY, raw);
  } catch { /* shown, just not cached */ }
  renderState(state);
}

/** Once connected: the node's appearance wins over this window's cache
 *  (seeded from it when the node has none), and the Baker's changes arrive
 *  live. Everything from the node is checked first (`appearanceFromNode`). */
export function syncAppearanceWithNode(): () => void {
  let alive = true;
  uiAppearance().then(
    (blob) => {
      if (!alive) return;
      const state = appearanceFromNode(blob);
      if (state) adopt(state);
      else if (blob === null) void uiAppearanceSet({ appearance: loadAppearance() }).catch(() => {});
    },
    () => {},
  );
  const off = onEnvelope("ui-appearance", (envelope) => {
    const state = appearanceFromNode(envelope.appearance);
    if (state) adopt(state);
  });
  return () => {
    alive = false;
    off();
  };
}

export function applyTheme(id: Theme): void {
  const state = writableState();
  if (id !== "auto" && !appearanceCodec.builtin(id) && !state.schemes.some((s) => s.id === id)) {
    throw new Error("That color scheme no longer exists.");
  }
  persist({ ...state, selected: id });
}

export function saveScheme(input: Omit<ColorScheme, "id"> & { id?: ColorScheme["id"] }): ColorScheme {
  const state = writableState();
  // The editor only knows colors: an edited import keeps its dark variant,
  // fonts, radii and shadows.
  const prev = input.id ? state.schemes.find((s) => s.id === input.id) : undefined;
  const scheme = appearanceCodec.scheme({
    ...(prev?.dark ? { dark: prev.dark } : {}),
    ...(prev?.extras ? { extras: prev.extras } : {}),
    ...input,
    id: input.id ?? `custom:${crypto.randomUUID()}`,
  });
  const exists = state.schemes.some((s) => s.id === scheme.id);
  if (input.id && !exists) throw new Error("That scheme was deleted in another window. Create a new scheme to keep these colors.");
  const schemes = exists ? state.schemes.map((s) => s.id === scheme.id ? scheme : s) : [...state.schemes, scheme];
  persist({ ...state, schemes, selected: scheme.id });
  return scheme;
}

export function deleteScheme(id: ColorScheme["id"]): void {
  const state = writableState();
  const scheme = state.schemes.find((s) => s.id === id);
  if (!scheme) return;
  persist({ ...state, selected: state.selected === id ? scheme.base : state.selected, schemes: state.schemes.filter((s) => s.id !== id) });
}

export function previewTheme(base: BuiltinTheme, tokens: CustomTokens): void {
  if (!appearanceCodec.builtin(base)) throw new Error("Unknown base palette.");
  render(base, appearanceCodec.tokens(tokens, true), "preview", true);
}
export function restoreTheme(): void { renderState(loadAppearance()); }

/** App shell owns cross-window synchronization, not the settings pane. */
export function startAppearanceSync(): () => void {
  restoreTheme();
  const onStorage = (event: StorageEvent) => {
    if (event.key === APPEARANCE_KEY || event.key === null || event.key === "bakery.theme" || event.key === "bakery.theme.custom") restoreTheme();
  };
  window.addEventListener("storage", onStorage);
  // Auto follows the Mac's appearance live; for any other choice this
  // re-renders the same palette.
  const mac = lightQuery();
  mac.addEventListener("change", restoreTheme);
  return () => {
    window.removeEventListener("storage", onStorage);
    mac.removeEventListener("change", restoreTheme);
  };
}

/** The scheme on screen as the Code editors take it: every token as a plain
 *  colour (a probe's computed colour, so var() and color-mix() arrive as
 *  rgb() or color(srgb …)), whether its surfaces are dark, and its code font. */
function codeLook() {
  const probe = document.createElement("div");
  probe.hidden = true;
  document.body.append(probe);
  try {
    const colors: Record<string, string> = {};
    for (const key of CUSTOM_TOKEN_KEYS) {
      probe.style.color = `var(${key})`;
      colors[key] = getComputedStyle(probe).color;
    }
    const root = document.documentElement;
    const mono = getComputedStyle(root).getPropertyValue("--font-mono").trim();
    return { dark: root.getAttribute("data-color-mode") !== "light", colors, mono_font: mono || null };
  } finally { probe.remove(); }
}

/** Once connected: the Code editors take the app's look now, and again on
 *  every change of scheme or of the Mac's light or dark mode (a preview in
 *  Appearance is left out until it is kept). */
export function startCodeAppearance(): () => void {
  let last = "";
  const send = () => {
    const look = codeLook();
    // A re-render of the same scheme changes nothing for the editors.
    const key = JSON.stringify(look);
    if (key === last) return;
    last = key;
    void codeAppearance(look).catch(() => {
      last = "";
    });
  };
  const onChange = (e: Event) => {
    if (!(e as CustomEvent<{ preview?: boolean }>).detail?.preview) send();
  };
  send();
  window.addEventListener(THEME_CHANGED_EVENT, onChange);
  return () => window.removeEventListener(THEME_CHANGED_EVENT, onChange);
}

/** Resolve a preset even while the root has custom overrides. */
export function readPaletteTokens(base: BuiltinTheme): Record<CustomTokenKey, string> {
  const probe = document.createElement("div");
  probe.dataset.palette = base;
  probe.hidden = true;
  document.body.append(probe);
  try {
    const css = getComputedStyle(probe);
    return Object.fromEntries(CUSTOM_TOKEN_KEYS.map((key) => [key, css.getPropertyValue(key).trim()])) as Record<CustomTokenKey, string>;
  } finally { probe.remove(); }
}

export function accentTokens(value: string): CustomTokens {
  if (!appearanceCodec.isColor(value)) throw new Error("Choose a valid accent color.");
  const hex = colorToHex(value);
  const rgb = [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16));
  return {
    "--accent": hex, "--accent-fg": hex, "--accent-solid": hex, "--accent-solid-h": hex,
    "--accent-border": `rgba(${rgb.join(",")},.55)`, "--accent-bg": `rgba(${rgb.join(",")},.14)`,
    "--accent-bg-2": `rgba(${rgb.join(",")},.24)`, "--accent-sel": `rgba(${rgb.join(",")},.10)`,
  };
}

export function exportScheme(scheme: ColorScheme): string {
  const checked = appearanceCodec.scheme(scheme);
  const { name, base, tokens, dark, extras } = checked;
  return JSON.stringify({ format: "bakery-color-scheme", version: 1, name, base, tokens, ...(dark ? { dark } : {}), ...(extras ? { extras } : {}) }, null, 2);
}

export function importScheme(json: string): ColorScheme {
  if (json.length > 64_000) throw new Error("Choose a color scheme JSON file smaller than 64 KB.");
  let value;
  try { value = JSON.parse(json); } catch { throw new Error("This file is not valid JSON."); }
  if (!value || value.format !== "bakery-color-scheme" || value.version !== 1) throw new Error("Choose a Bakery color scheme export (version 1).");
  // Never reuse a supplied identity: importing is additive, never destructive.
  return saveScheme({
    name: value.name,
    base: value.base,
    tokens: value.tokens,
    ...(value.dark !== undefined ? { dark: value.dark } : {}),
    ...(value.extras !== undefined ? { extras: value.extras } : {}),
  });
}
