// Fixtures trimmed from tweakcn's real "catppuccin" registry theme
// (https://tweakcn.com/r/themes/catppuccin.json, 2026-10-08) and its CSS export.
import { appearanceCodec } from "./themeModel.ts";
import { cssColor, parseColor, parseTweakcn, tweakcnScheme } from "./tweakcn.ts";
import { BAKERY_THEMES } from "./bakeryThemes.ts";

let fails = 0;
function ok(name: string, cond: boolean, extra = "") {
  if (!cond) {
    fails++;
    console.log(`FAIL ${name} ${extra}`);
  } else console.log(`ok   ${name}`);
}
const near = (raw: string, hex: string) => {
  const c = parseColor(raw);
  const want = parseColor(hex)!;
  return !!c && Math.abs(c.r - want.r) <= 2 && Math.abs(c.g - want.g) <= 2 && Math.abs(c.b - want.b) <= 2;
};

// Catppuccin Latte's published colors, from tweakcn's oklch.
ok("oklch base → #eff1f5", near("oklch(0.9578 0.0058 264.5321)", "#eff1f5"), JSON.stringify(parseColor("oklch(0.9578 0.0058 264.5321)")));
ok("oklch text → #4c4f69", near("oklch(0.4355 0.0430 279.3250)", "#4c4f69"));
ok("oklch mauve → #8839ef", near("oklch(0.5547 0.2503 297.0156)", "#8839ef"), JSON.stringify(parseColor("oklch(0.5547 0.2503 297.0156)")));
ok("oklch white", cssColor(parseColor("oklch(1.0000 0 0)")!) === "#ffffff");
ok("space hsl with alpha", cssColor(parseColor("hsl(240 30% 25% / 0.12)")!) === "rgba(45, 45, 83, 0.12)", cssColor(parseColor("hsl(240 30% 25% / 0.12)")!));
ok("shadcn bare hsl", cssColor(parseColor("240 30% 25%")!) === "#2d2d53");
ok("comma rgb and hex", cssColor(parseColor("rgb(10, 20, 30)")!) === "#0a141e" && cssColor(parseColor("#abc")!) === "#aabbcc");
ok("not a color", parseColor("var(--x)") === null && parseColor("calc(1px)") === null && parseColor("red") === null);

const registry = JSON.stringify({
  $schema: "https://ui.shadcn.com/schema/registry-item.json",
  name: "catppuccin",
  type: "registry:style",
  cssVars: {
    theme: { "font-sans": "Montserrat, sans-serif", "font-mono": "Fira Code, monospace", radius: "0.35rem", "tracking-tight": "calc(var(--tracking-normal) - 0.025em)" },
    light: {
      background: "oklch(0.9578 0.0058 264.5321)",
      foreground: "oklch(0.4355 0.0430 279.3250)",
      card: "oklch(1.0000 0 0)",
      primary: "oklch(0.5547 0.2503 297.0156)",
      "primary-foreground": "oklch(1.0000 0 0)",
      "muted-foreground": "oklch(0.5471 0.0343 279.0837)",
      destructive: "oklch(0.5505 0.2155 19.8095)",
      border: "oklch(0.8083 0.0174 271.1982)",
      sidebar: "oklch(0.9335 0.0087 264.5206)",
      "shadow-sm": "0px 4px 6px 0px hsl(240 30% 25% / 0.12), 0px 1px 2px -1px hsl(240 30% 25% / 0.12)",
      "shadow-2xl": "0px 4px 6px 0px hsl(240 30% 25% / 0.30)",
    },
    dark: {
      background: "oklch(0.2155 0.0254 284.0647)",
      foreground: "oklch(0.8787 0.0426 272.2767)",
      primary: "oklch(0.7871 0.1187 304.7693)",
    },
  },
});
const s = tweakcnScheme(parseTweakcn(registry));
ok("registry: named after the theme", s.name === "Catppuccin");
ok("registry: light first, on the light palette", s.base === "light" && s.tokens["--bg-app"] === cssColor(parseColor("oklch(0.9578 0.0058 264.5321)")!));
ok("registry: shadcn primary is the Bakery accent", s.tokens["--accent"] === s.tokens["--accent-solid"] && s.tokens["--fg-on-solid"] === "#ffffff");
ok("registry: accent tints are see-through", /^rgba\(/.test(s.tokens["--accent-bg"] ?? ""));
ok("registry: dark variant on the dark palette", s.dark?.base === "dark" && !!s.dark?.tokens["--bg-app"]);
ok("registry: fonts", s.extras?.["--font-ui"] === "Montserrat, sans-serif" && s.extras?.["--font-mono"] === "Fira Code, monospace");
ok("registry: radii scale to 0.35rem (5.6px lg)", s.extras?.["--r-lg"] === "5.6px" && s.extras?.["--r-xs"] === "2.8px", s.extras?.["--r-lg"]);
ok(
  "registry: shadows made plain",
  s.extras?.["--shadow-chip"] === "0px 4px 6px 0px rgba(45, 45, 83, 0.12), 0px 1px 2px -1px rgba(45, 45, 83, 0.12)",
  s.extras?.["--shadow-chip"],
);
ok("registry: calc() tracking is not taken", !JSON.stringify(s).includes("calc"));
let valid = true;
try {
  appearanceCodec.scheme({ ...s, id: "custom:catppuccin" });
} catch (e) {
  valid = false;
  console.log(String(e));
}
ok("registry: the result passes the scheme codec, as saved", valid);

const css = `:root {
  --background: oklch(0.9578 0.0058 264.5321);
  --foreground: oklch(0.4355 0.0430 279.3250);
  --primary: oklch(0.5547 0.2503 297.0156);
  --radius: 6px;
}

.dark {
  --background: oklch(0.2155 0.0254 284.0647);
  --foreground: oklch(0.8787 0.0426 272.2767);
}

@theme inline {
  --color-background: var(--background);
}`;
const fromCss = tweakcnScheme(parseTweakcn(css), "My CSS theme");
ok("css: both blocks, named by the operator", fromCss.name === "My CSS theme" && !!fromCss.dark && fromCss.base === "light");
ok("css: px radius", fromCss.extras?.["--r-lg"] === "6px");
ok("css: light-only themes have no dark variant", !tweakcnScheme(parseTweakcn(css.replace(/\.dark[\s\S]*?\}/, ""))).dark);

const refuses = (input: string) => {
  try {
    parseTweakcn(input);
    return false;
  } catch {
    return true;
  }
};
ok("refuses @import", refuses(`@import url("https://evil.example/x.css");\n${css}`));
ok("refuses url()", refuses(":root { --background: url(https://evil.example/x.png); --foreground: #000; }"));
ok("refuses expression()", refuses(":root { --background: expression(alert(1)); }"));
ok("refuses non-theme text", refuses("hello"));
const injected = tweakcnScheme(parseTweakcn(":root { --background: #fff; --foreground: #000; --font-sans: Inter; } body { color: red }"));
ok("declarations stop at the block", injected.extras?.["--font-ui"] === "Inter");
ok("an unparseable primary falls back, never through", tweakcnScheme(parseTweakcn(":root { --background: #fff; --foreground: #000; --primary: var(--x); }")).tokens["--accent"] === "#000000");

// The bakery themes in Appearance convert and save like any tweakcn import.
for (const t of BAKERY_THEMES) {
  let scheme: ReturnType<typeof tweakcnScheme> | null = null;
  try {
    scheme = tweakcnScheme(parseTweakcn(t.css), t.name);
    appearanceCodec.scheme({ ...scheme, id: "custom:check" });
  } catch (e) {
    ok(`bakery theme ${t.name} converts`, false, String(e));
    continue;
  }
  ok(`bakery theme ${t.name} converts, light and dark when it has both`, scheme.name === t.name && !!scheme.dark === t.css.includes(":root"));
}

console.log(fails === 0 ? "\nALL PASS" : `\n${fails} FAILURES`);
if (fails > 0) throw new Error(`${fails} tweakcn check failure(s)`);
