// Bakery-inspired themes in tweakcn's own format, to try in Appearance with
// one click (they go through the same import as a pasted tweakcn theme).
// Each has its light colors in :root and its dark ones in .dark; the two
// dark-only ones are dark either way. All keep look C's mono type.

export type BakeryTheme = { name: string; blurb: string; css: string };

const theme = (light: string | null, dark: string) => `${light ? `:root {\n${light}\n}\n` : ""}.dark {\n${dark}\n}\n`;
const MONO = "'JetBrains Mono', ui-monospace, Menlo, monospace";
const vars = (v: Record<string, string>) =>
  Object.entries({ ...v, "font-sans": MONO, "font-mono": MONO })
    .map(([k, value]) => `  --${k}: ${value};`)
    .join("\n");

export const BAKERY_THEMES: readonly BakeryTheme[] = [
  {
    name: "Sourdough",
    blurb: "Crust browns and flour cream: look C, a degree warmer.",
    css: theme(
      vars({ background: "#faf5ec", foreground: "#34261b", card: "#fffdf8", "card-foreground": "#34261b", popover: "#ffffff", muted: "#f0e6d6", "muted-foreground": "#8c7864", primary: "#b8772f", "primary-foreground": "#fffaf2", destructive: "#c4513f", border: "#e2d4bf", input: "#d9c7ae", sidebar: "#f3eadc", radius: "0.5rem" }),
      vars({ background: "#1d1915", foreground: "#f5ece0", card: "#25201b", "card-foreground": "#ebe1d4", popover: "#2c2621", muted: "#332b24", "muted-foreground": "#a99a88", primary: "#e0a464", "primary-foreground": "#1d1610", destructive: "#e5735f", border: "#4a3f35", input: "#5a4c40", sidebar: "#211c17", radius: "0.5rem" }),
    ),
  },
  {
    name: "Pain au chocolat",
    blurb: "Dark chocolate with a butter-yellow accent; butter cream by day.",
    css: theme(
      vars({ background: "#fbf3e2", foreground: "#3a2418", card: "#fffaf0", "card-foreground": "#3a2418", popover: "#ffffff", muted: "#f2e4c8", "muted-foreground": "#8a6a52", primary: "#7a4a2a", "primary-foreground": "#fff6e8", destructive: "#b9483a", border: "#e3cfa8", input: "#d8bf92", sidebar: "#f5e9d0", radius: "0.5rem" }),
      vars({ background: "#1a1310", foreground: "#f3e7d8", card: "#221915", "card-foreground": "#e9dccb", popover: "#291e19", muted: "#31241d", "muted-foreground": "#a8907c", primary: "#e8c46a", "primary-foreground": "#1a1310", destructive: "#e36f5c", border: "#4a372c", input: "#5a4436", sidebar: "#1d1512", radius: "0.5rem" }),
    ),
  },
  {
    name: "Matcha latte",
    blurb: "Green tea on deep green-black; rice paper by day.",
    css: theme(
      vars({ background: "#f6f5ec", foreground: "#26301f", card: "#fffef8", "card-foreground": "#26301f", popover: "#ffffff", muted: "#e9ead9", "muted-foreground": "#77806a", primary: "#5f8a3e", "primary-foreground": "#f8fbf2", destructive: "#b9483a", border: "#d8dcc4", input: "#c9cfb2", sidebar: "#eef0e2", radius: "0.5rem" }),
      vars({ background: "#141a15", foreground: "#ecf2e6", card: "#1a221c", "card-foreground": "#e1e9da", popover: "#202a22", muted: "#263228", "muted-foreground": "#95a690", primary: "#9bc27a", "primary-foreground": "#122012", destructive: "#e07565", border: "#354536", input: "#435644", sidebar: "#161d17", radius: "0.5rem" }),
    ),
  },
  {
    name: "Strawberry tart",
    blurb: "Berry red on dark crust; pastry pink by day.",
    css: theme(
      vars({ background: "#fff7f5", foreground: "#3a1f25", card: "#ffffff", "card-foreground": "#3a1f25", popover: "#ffffff", muted: "#fbe7e6", "muted-foreground": "#926d74", primary: "#c9455a", "primary-foreground": "#fff7f8", destructive: "#c2410c", border: "#f0d3d2", input: "#e6bfbe", sidebar: "#fdeeed", radius: "0.625rem" }),
      vars({ background: "#1c1416", foreground: "#f7e9ec", card: "#241a1d", "card-foreground": "#ecdde1", popover: "#2b1f23", muted: "#33242a", "muted-foreground": "#ad9198", primary: "#e2687a", "primary-foreground": "#1f0f13", destructive: "#f08a4b", border: "#4a343b", input: "#5a414a", sidebar: "#1f1619", radius: "0.625rem" }),
    ),
  },
  {
    name: "Blueberry muffin",
    blurb: "Blueberry violet on a dusk-blue dark; muffin crumb by day.",
    css: theme(
      vars({ background: "#f8f6f1", foreground: "#24223a", card: "#fffdf9", "card-foreground": "#24223a", popover: "#ffffff", muted: "#eeeaf3", "muted-foreground": "#77738f", primary: "#4f54c9", "primary-foreground": "#f7f7ff", destructive: "#c2413a", border: "#ddd8e6", input: "#cdc7da", sidebar: "#f1eef6", radius: "0.5rem" }),
      vars({ background: "#15161f", foreground: "#eceaf6", card: "#1b1d28", "card-foreground": "#e0def0", popover: "#212331", muted: "#282a3a", "muted-foreground": "#9a98b3", primary: "#8b8ff0", "primary-foreground": "#13142a", destructive: "#ef7a6c", border: "#383b52", input: "#464a66", sidebar: "#171823", radius: "0.5rem" }),
    ),
  },
  {
    name: "Oven glow",
    blurb: "Brushed steel with oven-amber for what's hot. Dark only.",
    css: theme(
      null,
      vars({ background: "#16181a", foreground: "#eef1f3", card: "#1e2124", "card-foreground": "#dfe4e8", popover: "#24282c", muted: "#2b3034", "muted-foreground": "#8b949c", primary: "#ff9d3c", "primary-foreground": "#1a1206", destructive: "#ff6b5b", border: "#3a4045", input: "#4a5157", sidebar: "#1a1d1f", radius: "0.25rem" }),
    ),
  },
  {
    name: "Chalkboard menu",
    blurb: "Slate green with chalk-white type and a chalk-yellow accent. Dark only.",
    css: theme(
      null,
      vars({ background: "#1d2220", foreground: "#f3f1e7", card: "#232a27", "card-foreground": "#e6e3d8", popover: "#28302c", muted: "#2f3934", "muted-foreground": "#99a29b", primary: "#f2d06b", "primary-foreground": "#1d2220", destructive: "#ef8a7d", border: "#3d4743", input: "#4b5751", sidebar: "#1a1e1c", radius: "0.375rem" }),
    ),
  },
];
