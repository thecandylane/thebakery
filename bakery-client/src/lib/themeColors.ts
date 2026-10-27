/** Color adapters for renderers that cannot consume CSS custom properties.
 * Keep this pure: the Appearance picker, Monaco and terminal share the same
 * conversion, and importing it must not require a browser or an editor worker.
 * Supported inputs match the Appearance schema: hex, comma rgb[a], hsl[a].
 */
type Rgba = { rgb: [number, number, number]; alpha?: number };

const NUMBER = /^[+-]?(?:\d+(?:\.\d*)?|\.\d+)$/;
const clamp = (value: number, max = 1) => Math.max(0, Math.min(max, value));
const byteHex = (value: number) => Math.round(clamp(value, 255)).toString(16).padStart(2, "0");

function numeric(value: string): number | undefined {
  const parsed = NUMBER.test(value) ? Number(value) : NaN;
  return Number.isFinite(parsed) ? parsed : undefined;
}

function proportion(value: string): number | undefined {
  const percent = value.endsWith("%");
  const parsed = numeric(percent ? value.slice(0, -1) : value);
  return parsed === undefined ? undefined : clamp(parsed / (percent ? 100 : 1));
}

function parseColor(value: string): Rgba | undefined {
  const color = value.trim().toLowerCase();
  if (/^#(?:[\da-f]{3}|[\da-f]{4}|[\da-f]{6}|[\da-f]{8})$/.test(color)) {
    const hex = color.length <= 5 ? [...color.slice(1)].map((c) => c + c).join("") : color.slice(1);
    return {
      rgb: [0, 2, 4].map((offset) => parseInt(hex.slice(offset, offset + 2), 16)) as Rgba["rgb"],
      ...(hex.length === 8 ? { alpha: parseInt(hex.slice(6, 8), 16) / 255 } : {}),
    };
  }
  const match = /^(rgb|rgba|hsl|hsla)\(([^)]+)\)$/.exec(color);
  if (!match) return undefined;
  const [, fn, body] = match;
  const parts = body.split(",").map((part) => part.trim());
  if (parts.length !== (fn.endsWith("a") ? 4 : 3)) return undefined;
  const alpha = parts.length === 4 ? proportion(parts[3]) : undefined;
  if (parts.length === 4 && alpha === undefined) return undefined;
  if (fn.startsWith("rgb")) {
    const rgb = parts.slice(0, 3).map((part) => {
      const percent = part.endsWith("%");
      const parsed = numeric(percent ? part.slice(0, -1) : part);
      return parsed === undefined ? undefined : clamp(parsed / (percent ? 100 : 255)) * 255;
    });
    if (rgb.some((part) => part === undefined)) return undefined;
    return { rgb: rgb as Rgba["rgb"], alpha };
  }
  const hue = numeric(parts[0].replace(/deg$/, ""));
  if (hue === undefined || !parts[1].endsWith("%") || !parts[2].endsWith("%")) return undefined;
  const saturation = proportion(parts[1]);
  const lightness = proportion(parts[2]);
  if (saturation === undefined || lightness === undefined) return undefined;
  const h = ((hue % 360) + 360) % 360 / 60;
  const chroma = (1 - Math.abs(2 * lightness - 1)) * saturation;
  const x = chroma * (1 - Math.abs(h % 2 - 1));
  const offset = lightness - chroma / 2;
  const sectors: Rgba["rgb"][] = [[chroma, x, 0], [x, chroma, 0], [0, chroma, x], [0, x, chroma], [x, 0, chroma], [chroma, 0, x]];
  return { rgb: sectors[Math.floor(h)].map((channel) => (channel + offset) * 255) as Rgba["rgb"], alpha };
}

/** Full hex for Monaco/xterm; keep authored alpha instead of making tints opaque. */
export function cssColorToHex(value: string): string {
  const parsed = parseColor(value);
  if (!parsed) return "#000000"; // Defensive only; the shared theme schema rejects invalid colors.
  return `#${parsed.rgb.map(byteHex).join("")}${parsed.alpha === undefined ? "" : byteHex(parsed.alpha * 255)}`;
}

/** Native color inputs and Monaco syntax rules require exactly six hex digits. */
export function colorToHex(value: string): string {
  return cssColorToHex(value).slice(0, 7);
}

/** Use the effective canvas, not the preset name, to choose renderer defaults. */
export function isLightColor(value: string): boolean {
  const parsed = parseColor(value);
  if (!parsed) return false;
  const [r, g, b] = parsed.rgb;
  return (0.2126 * r + 0.7152 * g + 0.0722 * b) / 255 > 0.5;
}
