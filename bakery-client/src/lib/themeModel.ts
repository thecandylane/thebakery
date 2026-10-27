// Shared by settings, storage migration, and the pre-paint script.
// theme.css remains the only source of built-in palette values.
export const BUILTIN_THEMES = [
  "dark", "dim", "light", "monokai", "solarized-dark", "solarized-light", "high-contrast", "warm", "parchment",
] as const;
export type BuiltinTheme = (typeof BUILTIN_THEMES)[number];
/** "auto" follows the Mac: Warm in dark mode, Parchment in light. The default. */
export type Theme = BuiltinTheme | "auto" | `custom:${string}`;
export const AUTO_LABEL = "Auto";
/** Built-in palettes whose surfaces are light (native controls, dark: variants). */
export const LIGHT_THEMES: readonly BuiltinTheme[] = ["light", "solarized-light", "parchment"];
export const THEME_LABELS: Record<BuiltinTheme, string> = {
  dark: "Dark", dim: "Dim", light: "Light", monokai: "Monokai",
  "solarized-dark": "Solarized Dark", "solarized-light": "Solarized Light", "high-contrast": "High Contrast",
  warm: "Warm", parchment: "Parchment",
};
export const CUSTOM_TOKEN_KEYS = [
  "--bg-rail", "--bg-app", "--bg-chrome", "--bg-panel", "--bg-raised", "--bg-chip",
  "--line-chrome", "--line-row", "--line-soft", "--line", "--line-strong", "--scrollbar-thumb-hover",
  "--fg", "--fg-body", "--fg-2", "--fg-dim", "--fg-faint", "--fg-fainter", "--fg-ghost",
  "--fg-linenum", "--fg-rule", "--fg-on-solid", "--fg-on-tint",
  "--accent", "--accent-fg", "--accent-bg", "--accent-bg-2", "--accent-sel",
  "--accent-border", "--accent-solid", "--accent-solid-h",
  "--ok", "--ok-bg", "--ok-border", "--warn", "--warn-bg", "--warn-border",
  "--bad", "--bad-bg", "--bad-border", "--gone-border", "--violet", "--violet-bg", "--idle",
  "--baker-solid", "--baker-solid-h", "--baker-border",
  "--diff-add-bg", "--diff-del-bg", "--diff-add-fg", "--diff-del-fg", "--diff-add-bg-editor", "--diff-del-bg-editor",
  "--syntax-keyword", "--syntax-string", "--syntax-comment", "--syntax-number", "--syntax-type", "--overlay-scrim",
] as const;
export type CustomTokenKey = (typeof CUSTOM_TOKEN_KEYS)[number];
export type CustomTokens = Partial<Record<CustomTokenKey, string>>;
/** Type and shape an imported theme may set besides colors: fonts, the
 *  corner radii (px), and the three shadow levels. Values are validated as
 *  plain fonts, lengths and shadows, never anything executable. */
export const EXTRA_KEYS = [
  "--font-ui", "--font-mono",
  "--r-xs", "--r-sm", "--r-md", "--r-lg", "--r-xl", "--r-2xl", "--r-3xl", "--r-4xl",
  "--shadow-chip", "--shadow-menu", "--shadow-modal",
] as const;
export type ExtraKey = (typeof EXTRA_KEYS)[number];
export type SchemeExtras = Partial<Record<ExtraKey, string>>;
export type SchemeVariant = { base: BuiltinTheme; tokens: CustomTokens };
export interface ColorScheme {
  id: `custom:${string}`;
  name: string;
  base: BuiltinTheme;
  tokens: CustomTokens;
  /** A theme with two variants (an imported tweakcn one): base/tokens while
   *  the Mac is light, these while it is dark. */
  dark?: SchemeVariant;
  extras?: SchemeExtras;
}
export interface AppearanceState { version: 1; selected: Theme; schemes: ColorScheme[] }
export const APPEARANCE_KEY = "bakery.appearance.v1";
export const APPEARANCE_CONFIG = {
  builtins: BUILTIN_THEMES, keys: CUSTOM_TOKEN_KEYS, extraKeys: EXTRA_KEYS, storageKey: APPEARANCE_KEY,
};

/** Self-contained: the SAME decoder runs before first paint. No DOM, imports,
 * arbitrary properties, or executable imports. Embedded by hooks.server.ts. */
export function createAppearanceCodec(config: {
  builtins: readonly string[]; keys: readonly string[]; extraKeys: readonly string[]; storageKey: string;
}) {
  const empty = (): AppearanceState => ({ version: 1, selected: "auto", schemes: [] });
  const builtin = (v: unknown): v is BuiltinTheme => typeof v === "string" && config.builtins.includes(v);
  const customId = (v: unknown): v is ColorScheme["id"] => typeof v === "string" && /^custom:[a-zA-Z0-9_-]{1,80}$/.test(v);
  const object = (v: unknown): v is Record<string, unknown> => !!v && typeof v === "object" && !Array.isArray(v);

  function isColor(v: unknown): v is string {
    if (typeof v !== "string" || v.length > 100) return false;
    const s = v.trim();
    if (/^#(?:[\da-f]{3}|[\da-f]{4}|[\da-f]{6}|[\da-f]{8})$/i.test(s)) return true;
    const m = /^(rgb|rgba|hsl|hsla)\(([^()]+)\)$/i.exec(s);
    if (!m) return false;
    const parts = m[2].split(",").map((x) => x.trim());
    const alpha = m[1].toLowerCase().endsWith("a");
    if (parts.length !== (alpha ? 4 : 3)) return false;
    const number = (x: string) => /^\d*\.?\d+%?$/.test(x) && Number.isFinite(parseFloat(x));
    if (!parts.every(number)) return false;
    if (alpha && parseFloat(parts[3]) > (parts[3].endsWith("%") ? 100 : 1)) return false;
    if (m[1].toLowerCase().startsWith("hsl")) {
      return !parts[0].endsWith("%") && parseFloat(parts[0]) <= 360 &&
        parts.slice(1, 3).every((x) => x.endsWith("%") && parseFloat(x) <= 100);
    }
    // CSS comma syntax cannot mix percentage and number channels.
    const percentChannels = parts[0].endsWith("%");
    return parts.slice(0, 3).every((x) => x.endsWith("%") === percentChannels && parseFloat(x) <= (percentChannels ? 100 : 255));
  }

  function tokens(value: unknown, strict = false): CustomTokens {
    if (!object(value)) {
      if (strict) throw new Error("Colors must be an object of named color values.");
      return {};
    }
    const result: CustomTokens = {};
    for (const [key, val] of Object.entries(value)) {
      if (!config.keys.includes(key) || !isColor(val)) {
        if (strict) throw new Error(`Invalid color ${key}. Use hex, rgb/rgba, or hsl/hsla values.`);
        continue;
      }
      result[key as CustomTokenKey] = val.trim();
    }
    return result;
  }

  /** Fonts are family lists; radii plain px; shadows "x y [blur [spread]]
   *  color" parts. No parentheses but a color's, so no url() or calc(). */
  function isExtra(key: string, v: unknown): boolean {
    if (typeof v !== "string" || v.length > 300) return false;
    const s = v.trim();
    if (key.startsWith("--r-")) return /^\d{1,3}(\.\d{1,2})?px$/.test(s);
    if (key.startsWith("--font-")) return /^[A-Za-z0-9 ,"'_-]{1,200}$/.test(s);
    return s.split(/,(?![^(]*\))/).every((part) => {
      const m = /^((?:-?\d+(?:\.\d+)?(?:px)?\s+){2,4})(.+)$/.exec(part.trim());
      return !!m && isColor(m[2]);
    });
  }

  function extras(value: unknown): SchemeExtras {
    if (!object(value)) throw new Error("Theme extras must be an object.");
    const result: SchemeExtras = {};
    for (const [key, val] of Object.entries(value)) {
      if (!config.extraKeys.includes(key) || !isExtra(key, val)) throw new Error(`Invalid theme value ${key}.`);
      result[key as ExtraKey] = (val as string).trim();
    }
    return result;
  }

  function scheme(value: unknown): ColorScheme {
    if (!object(value) || !customId(value.id) || !builtin(value.base) ||
      typeof value.name !== "string" || !value.name.trim() || value.name.trim().length > 80) {
      throw new Error("A scheme needs a name (1–80 characters) and a supported base palette.");
    }
    const result: ColorScheme = { id: value.id, name: value.name.trim(), base: value.base, tokens: tokens(value.tokens, true) };
    if (value.dark !== undefined) {
      const dark = value.dark;
      if (!object(dark) || !builtin(dark.base)) throw new Error("A theme's dark variant needs a supported base palette.");
      result.dark = { base: dark.base, tokens: tokens(dark.tokens, true) };
    }
    if (value.extras !== undefined) result.extras = extras(value.extras);
    return result;
  }

  function decode(raw: string): AppearanceState {
    if (raw.length > 1_000_000) throw new Error("The color library is too large.");
    const value: unknown = JSON.parse(raw);
    if (!object(value) || value.version !== 1 || !Array.isArray(value.schemes) || value.schemes.length > 100) {
      throw new Error("This color library has an unsupported format.");
    }
    const schemes = value.schemes.map(scheme);
    if (new Set(schemes.map((s) => s.id)).size !== schemes.length) throw new Error("Duplicate color scheme identities.");
    const selected = value.selected === "auto" || builtin(value.selected) ||
      (customId(value.selected) && schemes.some((s) => s.id === value.selected))
      ? value.selected : "auto";
    return { version: 1, selected, schemes };
  }

  function read(storage: Pick<Storage, "getItem">): AppearanceState {
    const raw = storage.getItem(config.storageKey);
    if (raw !== null) return decode(raw);
    // Non-destructive migration: legacy keys remain, first successful action
    // writes the selected id and the entire library together atomically.
    const legacy = storage.getItem("bakery.theme");
    let legacyTokens: CustomTokens = {};
    const oldTokens = storage.getItem("bakery.theme.custom");
    if (oldTokens) {
      try { legacyTokens = tokens(JSON.parse(oldTokens)); } catch { /* malformed legacy slot */ }
    }
    const schemes: ColorScheme[] = legacy === "custom" || Object.keys(legacyTokens).length
      ? [{ id: "custom:legacy", name: "My custom scheme", base: "dark", tokens: legacyTokens }] : [];
    return { version: 1, selected: legacy === "custom" ? "custom:legacy" : builtin(legacy) ? legacy : "auto", schemes };
  }
  return { empty, builtin, isColor, tokens, scheme, decode, read };
}
export const appearanceCodec = createAppearanceCodec(APPEARANCE_CONFIG);

/** An appearance from the node, which anyone with write access (the Baker
 *  included) may have set: the same codec as a local load, and refused whole
 *  if it so much as mentions url(), @import, expression() or javascript:.
 *  Null means "nothing usable". Only this result is ever cached or applied. */
export function appearanceFromNode(blob: unknown): AppearanceState | null {
  if (blob === null || blob === undefined) return null;
  const raw = JSON.stringify(blob);
  if (raw.length > 64_000 || /url\s*\(|@import|expression\s*\(|javascript:/i.test(raw)) return null;
  try {
    return appearanceCodec.decode(raw);
  } catch {
    return null;
  }
}
export const isColor = appearanceCodec.isColor;

/** Generated from the decoder above, never a hand-maintained second loader. */
export function appearanceBootstrapScript(): string {
  return `try {
    var codec = (${createAppearanceCodec.toString()})(${JSON.stringify(APPEARANCE_CONFIG)});
    var state = codec.read(localStorage);
    var scheme = state.schemes.find(function(s) { return s.id === state.selected; });
    var macLight = function() { return matchMedia("(prefers-color-scheme: light)").matches; };
    var variant = scheme && scheme.dark && !macLight() ? scheme.dark : scheme;
    var base = variant ? variant.base : state.selected;
    if (base === "auto") base = macLight() ? "parchment" : "warm";
    var root = document.documentElement;
    var mode = ${JSON.stringify(LIGHT_THEMES)}.indexOf(base) >= 0 ? "light" : "dark";
    if (base !== "dark") root.setAttribute("data-theme", base);
    root.setAttribute("data-color-scheme", state.selected);
    var set = function(entry) { root.style.setProperty(entry[0], entry[1]); };
    if (variant) Object.entries(variant.tokens).forEach(set);
    if (scheme && scheme.extras) Object.entries(scheme.extras).forEach(set);
    if (variant && variant.tokens["--bg-app"]) {
      // CSS has not loaded yet. The browser normalizes our validated literal
      // color without attaching a canvas or waiting for hydration.
      var ctx = document.createElement("canvas").getContext("2d");
      if (ctx) {
        ctx.fillStyle = variant.tokens["--bg-app"];
        var color = ctx.fillStyle;
        var rgb = color[0] === "#" ? [1, 3, 5].map(function(i) { return parseInt(color.slice(i, i + 2), 16); }) : color.match(/[\\d.]+/g).slice(0, 3).map(Number);
        mode = (rgb[0] * .2126 + rgb[1] * .7152 + rgb[2] * .0722) / 255 > .5 ? "light" : "dark";
      }
    }
    root.style.colorScheme = mode;
    root.setAttribute("data-color-mode", mode);
  } catch (_) { /* unavailable storage: readable default; no imported CSS execution */ }`;
}
