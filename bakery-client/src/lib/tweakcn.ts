// Themes made in tweakcn (the shadcn/ui theme editor), imported as Bakery
// schemes: either its CSS export (Tailwind v4: a :root block and a .dark
// block) or its registry JSON (registry:style/theme, with cssVars). shadcn's
// variables map onto the Bakery tokens that every surface, AI Elements
// included, already reads; oklch and space-separated hsl are converted to
// plain hex/rgba so every renderer (Monaco, xterm, pre-paint) understands
// them. Only plain values are taken: url(), @import and expressions are
// refused outright, and any value that doesn't parse is left out.
//
// Pure and DOM-free: tweakcn.check.ts runs it on plain node.

import type { BuiltinTheme, ColorScheme, CustomTokens, SchemeExtras } from "./themeModel.ts";

type Vars = Record<string, string>;
export type TweakcnTheme = { name: string | null; light: Vars | null; dark: Vars | null; theme: Vars };
type RGBA = { r: number; g: number; b: number; a: number };

const clamp01 = (x: number) => Math.min(1, Math.max(0, x));
const num = (s: string) => (/^-?\d*\.?\d+(e-?\d+)?$/i.test(s) ? Number(s) : NaN);
const unit = (s: string) => (s.endsWith("%") ? num(s.slice(0, -1)) / 100 : num(s));

function hslToRgb(h: number, s: number, l: number): [number, number, number] {
  const k = (n: number) => (n + h / 30) % 12;
  const a = s * Math.min(l, 1 - l);
  const f = (n: number) => l - a * Math.max(-1, Math.min(k(n) - 3, Math.min(9 - k(n), 1)));
  return [f(0), f(8), f(4)];
}

function oklchToRgb(L: number, C: number, H: number): [number, number, number] {
  const a = C * Math.cos((H * Math.PI) / 180);
  const b = C * Math.sin((H * Math.PI) / 180);
  const l = (L + 0.3963377774 * a + 0.2158037573 * b) ** 3;
  const m = (L - 0.1055613458 * a - 0.0638541728 * b) ** 3;
  const s = (L - 0.0894841775 * a - 1.291485548 * b) ** 3;
  const lin = [
    4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
    -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
    -0.0041960863 * l - 0.7034186147 * m + 1.707614701 * s,
  ];
  const gamma = (x: number) => (x <= 0.0031308 ? 12.92 * x : 1.055 * x ** (1 / 2.4) - 0.055);
  return lin.map((x) => gamma(clamp01(x))) as [number, number, number];
}

/** hex, rgb()/rgba(), hsl()/hsla() (comma or space syntax, "/ alpha"),
 *  oklch(), or shadcn's bare "H S% L%". Null for anything else. */
export function parseColor(raw: string): RGBA | null {
  const v = raw.trim().toLowerCase();
  const hex = /^#([\da-f]{3,4}|[\da-f]{6}|[\da-f]{8})$/.exec(v);
  if (hex) {
    const h = hex[1].length <= 4 ? [...hex[1]].map((c) => c + c).join("") : hex[1];
    const n = (i: number) => parseInt(h.slice(i, i + 2), 16);
    return { r: n(0), g: n(2), b: n(4), a: h.length === 8 ? n(6) / 255 : 1 };
  }
  const fn = /^(rgba?|hsla?|oklch)\(([^()]*)\)$/.exec(v);
  const [kind, body] = fn ? [fn[1], fn[2]] : /^\d/.test(v) ? ["hsl", v] : [null, ""];
  if (!kind) return null;
  const [main, alphaPart] = body.includes("/") ? body.split("/") : [body, undefined];
  const parts = main.split(/[\s,]+/).filter(Boolean);
  let alpha = alphaPart === undefined ? 1 : unit(alphaPart.trim());
  if (alphaPart === undefined && parts.length === 4) alpha = unit(parts.pop()!);
  if (parts.length !== 3 || !Number.isFinite(alpha)) return null;
  let rgb: [number, number, number];
  if (kind.startsWith("rgb")) {
    const c = parts.map((p) => (p.endsWith("%") ? unit(p) : num(p) / 255));
    if (c.some((x) => !Number.isFinite(x))) return null;
    rgb = [c[0], c[1], c[2]];
  } else if (kind.startsWith("hsl")) {
    const h = num(parts[0].replace(/deg$/, ""));
    const s = unit(parts[1]);
    const l = unit(parts[2]);
    if (![h, s, l].every(Number.isFinite)) return null;
    rgb = hslToRgb(((h % 360) + 360) % 360, clamp01(s), clamp01(l));
  } else {
    const L = unit(parts[0]);
    const C = num(parts[1]);
    const H = parts[2] === "none" ? 0 : num(parts[2].replace(/deg$/, ""));
    if (![L, C, H].every(Number.isFinite)) return null;
    rgb = oklchToRgb(L, C, H);
  }
  return {
    r: Math.round(clamp01(rgb[0]) * 255),
    g: Math.round(clamp01(rgb[1]) * 255),
    b: Math.round(clamp01(rgb[2]) * 255),
    a: clamp01(alpha),
  };
}

const hex2 = (n: number) => n.toString(16).padStart(2, "0");
/** As the scheme codec accepts it: #rrggbb, or rgba() when see-through. */
export function cssColor(c: RGBA): string {
  return c.a >= 0.999 ? `#${hex2(c.r)}${hex2(c.g)}${hex2(c.b)}` : `rgba(${c.r}, ${c.g}, ${c.b}, ${Number(c.a.toFixed(3))})`;
}
const mix = (a: RGBA, b: RGBA, t: number): RGBA => ({
  r: Math.round(a.r + (b.r - a.r) * t),
  g: Math.round(a.g + (b.g - a.g) * t),
  b: Math.round(a.b + (b.b - a.b) * t),
  a: 1,
});
const alpha = (c: RGBA, a: number): RGBA => ({ ...c, a });
const luminance = (c: RGBA) => (0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b) / 255;

function declarations(block: string): Vars {
  const vars: Vars = {};
  for (const m of block.matchAll(/--([\w-]+)\s*:\s*([^;}]+)/g)) vars[m[1]] = m[2].trim();
  return vars;
}

/** The theme's variables from a tweakcn CSS export or registry JSON. */
export function parseTweakcn(input: string): TweakcnTheme {
  if (input.length > 100_000) throw new Error("That theme is larger than 100 KB.");
  if (/@import|url\s*\(|expression\s*\(|javascript:/i.test(input)) {
    throw new Error("Only plain theme variables can be imported: no url(), @import or expressions.");
  }
  const text = input.trim();
  if (text.startsWith("{")) {
    let json: { name?: unknown; cssVars?: { theme?: unknown; light?: unknown; dark?: unknown } };
    try {
      json = JSON.parse(text);
    } catch {
      throw new Error("This isn't valid JSON.");
    }
    const vars = (v: unknown): Vars | null =>
      v && typeof v === "object" && !Array.isArray(v)
        ? (Object.fromEntries(Object.entries(v).filter(([, x]) => typeof x === "string")) as Vars)
        : null;
    if (!json.cssVars) throw new Error("This JSON has no cssVars: it isn't a tweakcn theme.");
    return {
      name: typeof json.name === "string" ? json.name : null,
      theme: vars(json.cssVars.theme) ?? {},
      light: vars(json.cssVars.light),
      dark: vars(json.cssVars.dark),
    };
  }
  const root = /(?:^|[\s}]):root\s*\{([^}]*)\}/.exec(text);
  const dark = /\.dark\s*\{([^}]*)\}/.exec(text);
  if (!root && !dark) throw new Error("Paste tweakcn's CSS (with its :root and .dark blocks) or its registry JSON.");
  return { name: null, theme: {}, light: root ? declarations(root[1]) : null, dark: dark ? declarations(dark[1]) : null };
}

/** One shadcn color set as Bakery tokens, on the built-in palette whose
 *  lightness matches (that palette fills what shadcn has no name for). */
function variant(v: Vars): { base: BuiltinTheme; tokens: CustomTokens } {
  const c = (k: string) => (v[k] ? parseColor(v[k]) : null);
  const bg = c("background");
  const fg = c("foreground");
  if (!bg || !fg) throw new Error("The theme needs at least a background and a foreground color.");
  const card = c("card") ?? bg;
  const side = c("sidebar") ?? bg;
  const border = c("border") ?? mix(fg, bg, 0.85);
  const mutedFg = c("muted-foreground") ?? mix(fg, bg, 0.4);
  const primary = c("primary") ?? fg;
  const destructive = c("destructive");
  const light = luminance(bg) > 0.5;
  const tokens: CustomTokens = {
    "--bg-rail": cssColor(side),
    "--bg-app": cssColor(bg),
    "--bg-chrome": cssColor(side),
    "--bg-panel": cssColor(card),
    "--bg-raised": cssColor(c("popover") ?? card),
    "--bg-chip": cssColor(c("muted") ?? c("secondary") ?? card),
    "--line-chrome": cssColor(border),
    "--line-row": cssColor(mix(border, card, 0.5)),
    "--line-soft": cssColor(mix(border, card, 0.3)),
    "--line": cssColor(border),
    "--line-strong": cssColor(c("input") ?? border),
    "--scrollbar-thumb-hover": cssColor(mutedFg),
    "--fg": cssColor(fg),
    "--fg-body": cssColor(c("card-foreground") ?? fg),
    "--fg-2": cssColor(mix(fg, mutedFg, 0.3)),
    "--fg-dim": cssColor(mutedFg),
    "--fg-faint": cssColor(mix(mutedFg, card, 0.25)),
    "--fg-fainter": cssColor(mix(mutedFg, card, 0.4)),
    "--fg-ghost": cssColor(mix(mutedFg, card, 0.55)),
    "--fg-linenum": cssColor(mix(mutedFg, card, 0.6)),
    "--fg-rule": cssColor(mix(mutedFg, card, 0.7)),
    "--fg-on-solid": cssColor(c("primary-foreground") ?? bg),
    "--accent": cssColor(primary),
    "--accent-fg": cssColor(mix(primary, fg, light ? 0.2 : 0.25)),
    "--accent-bg": cssColor(alpha(primary, 0.14)),
    "--accent-bg-2": cssColor(alpha(primary, 0.24)),
    "--accent-sel": cssColor(alpha(primary, 0.1)),
    "--accent-border": cssColor(mix(primary, card, 0.5)),
    "--accent-solid": cssColor(primary),
    "--accent-solid-h": cssColor(mix(primary, fg, 0.12)),
  };
  if (destructive) {
    tokens["--bad"] = cssColor(destructive);
    tokens["--bad-bg"] = cssColor(alpha(destructive, 0.14));
    tokens["--bad-border"] = cssColor(mix(destructive, card, 0.55));
  }
  return { base: light ? "light" : "dark", tokens };
}

const FONT = /^[A-Za-z0-9 ,"'_-]{1,200}$/;
/** Bakery's radius steps at its own 8px "lg", scaled to the theme's radius. */
const RADII: [keyof SchemeExtras, number][] = [
  ["--r-xs", 4], ["--r-sm", 6], ["--r-md", 7], ["--r-lg", 8], ["--r-xl", 10], ["--r-2xl", 12], ["--r-3xl", 14], ["--r-4xl", 16],
];

/** "0px 4px 6px 0px hsl(240 30% 25% / 0.12), …" with each color made plain. */
function shadow(v: string | undefined): string | null {
  if (!v) return null;
  const parts = v.split(/,(?![^(]*\))/).map((part) => {
    const m = /^((?:-?\d+(?:\.\d+)?(?:px)?\s+){2,4})(.+)$/.exec(part.trim());
    const color = m && parseColor(m[2]);
    return m && color ? `${m[1]}${cssColor(color)}` : null;
  });
  return parts.every(Boolean) ? parts.join(", ") : null;
}

function extrasOf(v: Vars): SchemeExtras {
  const extras: SchemeExtras = {};
  if (v["font-sans"] && FONT.test(v["font-sans"])) extras["--font-ui"] = v["font-sans"];
  if (v["font-mono"] && FONT.test(v["font-mono"])) extras["--font-mono"] = v["font-mono"];
  const r = /^(\d*\.?\d+)(rem|px)$/.exec(v.radius ?? "");
  if (r) {
    const px = Number(r[1]) * (r[2] === "rem" ? 16 : 1);
    for (const [key, step] of RADII) extras[key] = `${Number(Math.min(999, (px / 8) * step).toFixed(2))}px`;
  }
  const shadows: [keyof SchemeExtras, string][] = [
    ["--shadow-chip", "shadow-sm"],
    ["--shadow-menu", "shadow-lg"],
    ["--shadow-modal", "shadow-2xl"],
  ];
  for (const [key, from] of shadows) {
    const s = shadow(v[from]);
    if (s) extras[key] = s;
  }
  return extras;
}

const title = (name: string) => name.replace(/[-_]+/g, " ").replace(/\b\w/g, (ch) => ch.toUpperCase()).trim();

/** The Bakery scheme for an imported theme: its light colors as the scheme,
 *  its dark ones as the dark variant (when it has both), plus fonts, radii
 *  and shadows. */
export function tweakcnScheme(t: TweakcnTheme, name?: string): Omit<ColorScheme, "id"> {
  const light = t.light ? { ...t.theme, ...t.light } : null;
  const dark = t.dark ? { ...t.theme, ...t.dark } : null;
  const first = light ?? dark;
  if (!first) throw new Error("The theme has no colors.");
  const main = variant(first);
  const extras = extrasOf(first);
  return {
    name: (name?.trim() || (t.name ? title(t.name) : "") || "Imported theme").slice(0, 80),
    base: main.base,
    tokens: main.tokens,
    ...(light && dark ? { dark: variant(dark) } : {}),
    ...(Object.keys(extras).length ? { extras } : {}),
  };
}
