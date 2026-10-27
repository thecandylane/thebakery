// The visual blocks a reply can carry, as fenced code: ```mermaid diagrams,
// ```chart data, ```html and ```svg artifacts, and ```genui cards. This is
// the part that only reads text, so `node` checks it (visual.check.ts);
// VisualBlocks.tsx draws the result.

// ---- mermaid ----------------------------------------------------------------

/** The diagram's own name for itself ("flowchart", "sequenceDiagram"…): the
 *  first word after any front matter and comments. */
export function mermaidKind(code: string): string {
  const body = code.replace(/^\s*---\n[\s\S]*?\n---\s*\n/, "");
  for (const line of body.split("\n")) {
    const t = line.trim();
    if (t !== "" && !t.startsWith("%%")) return t.split(/[\s;]/)[0];
  }
  return "";
}

export type SizedSvg = { svg: string; width: number; height: number };

/** Mermaid's SVG sized by its viewBox: it ships `width="100%"` and a
 *  max-width style, which leave an `<img>` with no size of its own. Null when
 *  there is no root `<svg>` with a viewBox. */
export function sizedSvg(svg: string): SizedSvg | null {
  const open = svg.match(/<svg\b[^>]*>/);
  const box = open?.[0].match(/viewBox="\s*(-?[\d.]+)[\s,]+(-?[\d.]+)[\s,]+([\d.]+)[\s,]+([\d.]+)\s*"/);
  if (!open || !box) return null;
  const width = Math.ceil(Number(box[3]));
  const height = Math.ceil(Number(box[4]));
  if (!(width > 0 && height > 0)) return null;
  let tag = open[0].replace(/\s(?:width|height|style)="[^"]*"/g, "");
  if (!/\sxmlns="/.test(tag)) tag = tag.replace(/^<svg/, '<svg xmlns="http://www.w3.org/2000/svg"');
  tag = tag.replace(/^<svg/, `<svg width="${width}" height="${height}"`);
  return { svg: svg.replace(open[0], () => tag), width, height };
}

/** An SVG as an image URL: drawn as a picture, it can run no script and
 *  load nothing, whatever the text inside it says. */
export function svgImageUrl(svg: string): string {
  return `data:image/svg+xml;charset=utf-8,${encodeURIComponent(svg)}`;
}

export type DrawingCache<T> = {
  /** The picture already drawn for `key`, if any. */
  peek(key: string): T | undefined;
  /** The picture for `key`: drawn once, then the same one each time. */
  draw(key: string, make: () => Promise<T>): Promise<T>;
};

/** Drawn pictures by key, the `keep` newest kept, so a view drawn again (a
 *  workspace shown again runs its effects again, a thread reopened) draws
 *  nothing. A draw that failed is not kept: the next view tries again. */
export function drawingCache<T>(keep: number): DrawingCache<T> {
  type Entry = { promise: Promise<T>; done?: { value: T } };
  const held = new Map<string, Entry>();
  return {
    peek: (key) => held.get(key)?.done?.value,
    draw(key, make) {
      let entry = held.get(key);
      if (entry) held.delete(key);
      else {
        const made: Entry = {
          promise: make().then(
            (value) => {
              made.done = { value };
              return value;
            },
            (e: unknown) => {
              if (held.get(key) === made) held.delete(key);
              throw e;
            },
          ),
        };
        entry = made;
      }
      held.set(key, entry);
      for (const old of held.keys()) {
        if (held.size <= keep) break;
        held.delete(old);
      }
      return entry.promise;
    },
  };
}

/** What the frame mermaid draws in may load: mermaid itself, from the app,
 *  and nothing else. Mermaid draws live, to measure its text, and a diagram
 *  can name a URL it fetches while it draws (a node's `img`, an actor's
 *  icon, a url() in a classDef or in its config): each is refused there. */
export const MERMAID_FRAME_CSP = "default-src 'none'; script-src 'self'; style-src 'unsafe-inline'; img-src data:";

/** The frame's document: the policy first, then mermaid's script. */
export function mermaidFrameDoc(script: string): string {
  const src = script.replace(/[&"<>]/g, (c) => `&#${c.charCodeAt(0)};`);
  return `<!doctype html><meta http-equiv="Content-Security-Policy" content="${MERMAID_FRAME_CSP}"><script src="${src}"></script>`;
}

// ---- shared -----------------------------------------------------------------

export type Parsed<T> = { ok: true; value: T } | { ok: false; error: string };
const fail = (error: string): { ok: false; error: string } => ({ ok: false, error });
type Obj = Record<string, unknown>;
const isObj = (v: unknown): v is Obj => typeof v === "object" && v !== null && !Array.isArray(v);

/** A JSON value as text: strings, numbers and booleans as written, anything
 *  else none. `String()` would call an object's own `toString`, and in a
 *  spec that is whatever the agent wrote under that key. */
const text = (v: unknown): string =>
  typeof v === "string" ? v : typeof v === "number" || typeof v === "boolean" ? String(v) : "";

/** A table's own entry: never one an object inherits ("constructor",
 *  "__proto__"). */
const own = <T>(table: Record<string, T>, key: string): T | undefined => (Object.hasOwn(table, key) ? table[key] : undefined);

function parseJson(source: string): Parsed<unknown> {
  try {
    return { ok: true, value: JSON.parse(source) };
  } catch (e) {
    return fail(`not JSON: ${(e as Error).message}`);
  }
}

// ---- charts -----------------------------------------------------------------
//
// ```chart
// {"type": "line", "title": "p95 latency", "unit": "ms", "x": "day", "y": ["api", "web"],
//  "data": [{"day": "Mon", "api": 182, "web": 240}, {"day": "Tue", "api": 171, "web": 233}]}
// ```
//
// `type` is bar, line, area, pie or donut. `data` is rows (objects, or bare
// numbers); `x` names the category field and `y` the series, both guessed
// when left out. Chart.js's shape works too: `labels` with `datasets` (or
// `series`) of `{label, data}`.

export type ChartKind = "bar" | "line" | "area" | "pie" | "donut";
export type ChartSeries = { name: string; values: (number | null)[] };
export type Chart = { kind: ChartKind; title: string; unit: string; x: string; categories: string[]; series: ChartSeries[] };

const CHART_KINDS: Record<string, ChartKind> = {
  bar: "bar",
  column: "bar",
  line: "line",
  area: "area",
  pie: "pie",
  donut: "donut",
  doughnut: "donut",
};
/** One colour each (theme.css `--viz-1`…`--viz-8`); a pie folds the rest. */
export const MAX_SERIES = 8;
const MAX_POINTS = 2000;
const X_NAMES = ["label", "name", "x", "category", "date", "time", "day", "week", "month", "quarter", "year", "period", "step", "version", "key"];

function num(v: unknown): number | null {
  if (typeof v === "number") return Number.isFinite(v) ? v : null;
  if (typeof v === "string" && v.trim() !== "") {
    const n = Number(v);
    return Number.isFinite(n) ? n : null;
  }
  return null;
}

type Table = { x: string; categories: string[]; series: ChartSeries[] };

function fromRows(spec: Obj, title: string): Parsed<Table> {
  const data = spec.data;
  if (!Array.isArray(data)) return fail('"data" is the rows to plot: [{"label": "a", "value": 1}, …]');
  if (data.every((v) => num(v) !== null)) {
    return { ok: true, value: { x: "", categories: data.map((_, i) => String(i + 1)), series: [{ name: title || "value", values: data.map(num) }] } };
  }
  const rows = data.filter(isObj);
  if (rows.length !== data.length) return fail('every row of "data" is an object, or every one a number');
  const keys = [...new Set(rows.flatMap((row) => Object.keys(row)))];
  if (typeof spec.x === "string" && !keys.includes(spec.x)) return fail(`no row has the field ${JSON.stringify(spec.x)}`);
  const x =
    typeof spec.x === "string"
      ? spec.x
      : (X_NAMES.find((k) => keys.includes(k)) ?? keys.find((k) => rows.some((row) => typeof row[k] === "string" && num(row[k]) === null)) ?? "");
  const wanted = typeof spec.y === "string" ? [spec.y] : Array.isArray(spec.y) ? spec.y.filter((k): k is string => typeof k === "string") : null;
  const ys = wanted ?? keys.filter((k) => k !== x && rows.some((row) => num(row[k]) !== null));
  const missing = ys.find((k) => !keys.includes(k));
  if (missing !== undefined) return fail(`no row has the field ${JSON.stringify(missing)}`);
  return {
    ok: true,
    value: {
      x,
      categories: rows.map((row, i) => (x !== "" && row[x] != null ? text(row[x]) : String(i + 1))),
      series: ys.map((k) => ({ name: k, values: rows.map((row) => num(row[k])) })),
    },
  };
}

function fromLabels(spec: Obj): Parsed<Table> {
  const labels = (spec.labels as unknown[]).map(text);
  const list = Array.isArray(spec.datasets) ? spec.datasets : Array.isArray(spec.series) ? spec.series : null;
  if (!list) return fail('"labels" go with "datasets" (or "series"): [{"label": "a", "data": [1, 2]}]');
  const series: ChartSeries[] = [];
  for (const [i, s] of list.entries()) {
    if (!isObj(s)) return fail(`series ${i + 1} is not an object`);
    const values = Array.isArray(s.data) ? s.data : Array.isArray(s.values) ? s.values : null;
    if (!values) return fail(`series ${i + 1} has no "data" array`);
    const name = typeof s.label === "string" ? s.label : typeof s.name === "string" ? s.name : `series ${i + 1}`;
    series.push({ name, values: labels.map((_, j) => num(values[j])) });
  }
  return { ok: true, value: { x: "", categories: labels, series } };
}

export function parseChart(source: string): Parsed<Chart> {
  const json = parseJson(source);
  if (!json.ok) return json;
  const spec = json.value;
  if (!isObj(spec)) return fail('a chart is one JSON object: {"type": "bar", "data": […]}');
  const kind = own(CHART_KINDS, text(spec.type).toLowerCase());
  if (!kind) return fail(`"type" ${JSON.stringify(spec.type ?? null)} is not one of bar, line, area, pie, donut`);
  const title = typeof spec.title === "string" ? spec.title : "";
  const unit = typeof spec.unit === "string" ? spec.unit : "";
  const table = Array.isArray(spec.labels) ? fromLabels(spec) : fromRows(spec, title);
  if (!table.ok) return table;
  let { categories, series } = table.value;
  if (categories.length === 0) return fail("there is no data to plot");
  if (categories.length > MAX_POINTS) return fail(`${categories.length} points: at most ${MAX_POINTS}`);
  if (series.length === 0 || series.every((s) => s.values.every((v) => v === null))) return fail("no numbers to plot");
  if (kind === "pie" || kind === "donut") {
    if (series.length > 1) return fail(`a ${kind} shows one series, and this has ${series.length}: use a bar chart`);
    const values = series[0].values.map((v) => v ?? 0);
    if (values.some((v) => v < 0)) return fail(`a ${kind} cannot show a negative value`);
    if (!values.some((v) => v > 0)) return fail(`a ${kind} needs a value above zero`);
    // Past the palette, the tail folds into one slice.
    if (categories.length > MAX_SERIES) {
      const rest = values.slice(MAX_SERIES - 1).reduce((a, b) => a + b, 0);
      categories = [...categories.slice(0, MAX_SERIES - 1), "Other"];
      series = [{ name: series[0].name, values: [...values.slice(0, MAX_SERIES - 1), rest] }];
    }
  } else if (series.length > MAX_SERIES) {
    return fail(`${series.length} series: at most ${MAX_SERIES}, one colour each`);
  }
  return { ok: true, value: { kind, title, unit, x: table.value.x, categories, series } };
}

/** The value range the y axis covers: bars and areas grow from zero. */
export function valueRange(chart: Chart): [number, number] {
  const values = chart.series.flatMap((s) => s.values).filter((v): v is number => v !== null);
  let lo = Math.min(...values);
  let hi = Math.max(...values);
  if (chart.kind !== "line" || lo === hi) {
    lo = Math.min(lo, 0);
    hi = Math.max(hi, 0);
  }
  return lo === hi ? [lo, lo + 1] : [lo, hi];
}

/** Round ticks covering [lo, hi], about `count` of them, on steps of 1, 2 or
 *  5 × 10ⁿ. The first and last are the axis ends; `step` is for their
 *  labels (tickLabel). */
export function niceTicks(lo: number, hi: number, count = 5): { ticks: number[]; step: number } {
  const raw = (hi - lo) / count;
  const magnitude = 10 ** Math.floor(Math.log10(raw));
  const r = raw / magnitude;
  const step = (r <= 1 ? 1 : r <= 2 ? 2 : r <= 5 ? 5 : 10) * magnitude;
  // Each tick is rounded at the step's last digit, so float error never
  // shows (3 × 0.1 is 0.30000000000000004), however small the step:
  // toFixed would take no more than 100 digits.
  const scale = 10 ** Math.max(0, -Math.floor(Math.log10(step)));
  // The nudge keeps float error (0.3 / 0.1 = 2.9999…) from adding a step.
  const first = Math.floor(lo / step + 1e-9);
  // Counted, never more than the range holds: past 2⁵³ `first + 1` is
  // `first`, and a step that underflowed to zero spans infinitely many.
  const last = Math.min(Math.ceil(hi / step - 1e-9) - first, count + 2);
  const ticks: number[] = [];
  for (let k = 0; k <= last; k++) ticks.push(Math.round((first + k) * step * scale) / scale);
  return { ticks, step };
}

/** An axis tick, to the last digit its step has, so no two gridlines read
 *  the same: 0.004, 250, 1,285. From a step of a thousand up it is compact,
 *  and below a millionth scientific, either with every digit down to the
 *  step's: 5K, 1.5M, 1.002M, 2.5E-9. */
export function tickLabel(n: number, step: number): string {
  const place = Math.floor(Math.log10(step));
  if (step < 1000 && place >= -6) return new Intl.NumberFormat("en", { maximumFractionDigits: Math.max(0, -place) }).format(n);
  if (n === 0) return "0";
  const digits = Math.floor(Math.log10(Math.abs(n))) - place + 1;
  return new Intl.NumberFormat("en", {
    notation: step < 1000 ? "scientific" : "compact",
    maximumSignificantDigits: Math.min(21, Math.max(1, digits)),
  }).format(n);
}

/** Six decimals or six significant digits, whichever keeps more: 1,284.5,
 *  0.0042, 0.000001234, 1,234,567. */
const readout = new Intl.NumberFormat("en", { maximumFractionDigits: 6, maximumSignificantDigits: 6, roundingPriority: "morePrecision" });

/** A value read out in full, with its unit: "1,284.5 ms", "12%". */
export function valueLabel(n: number | null, unit: string): string {
  if (n === null) return "—";
  const shown = readout.format(n);
  return unit === "" ? shown : unit === "%" ? `${shown}%` : `${shown} ${unit}`;
}

/** Every how-many-th category gets its label, so labels never overlap. */
export function labelEvery(count: number, band: number, longest: number): number {
  const needed = longest * 6.4 + 10;
  return Math.max(1, Math.ceil(needed / band));
}

/** A pie's slices as [start, end] angles, clockwise from twelve o'clock. */
export function pieAngles(values: readonly (number | null)[]): [number, number][] {
  const total = values.reduce<number>((a, v) => a + (v ?? 0), 0);
  let at = 0;
  return values.map((v) => {
    const start = at;
    at += total > 0 ? ((v ?? 0) / total) * Math.PI * 2 : 0;
    return [start, at];
  });
}

// ---- artifacts --------------------------------------------------------------
//
// An ```html or ```svg fence runs in a sandboxed frame: no allow-same-origin,
// so it gets an origin of its own and can never script, read or store
// anything of the app's; its scripts run in there only. Its document opens
// with a Content-Security-Policy that lets it load code, styles and fonts
// from three public CDNs and nothing else, and the desktop shell keeps the
// frame from navigating anywhere (electron/main.ts).

export type ArtifactKind = "html" | "svg";

/** Scripts, styles and fonts inline or from cdnjs, jsdelivr and unpkg;
 *  images and media from the page itself only, since an image's URL can
 *  carry whatever the page knows to any host; and no fetch, socket, form or
 *  <base>. An artifact's own policy can only narrow this: every policy on a
 *  page must pass. */
export const ARTIFACT_CSP = [
  "default-src 'none'",
  "script-src 'unsafe-inline' 'unsafe-eval' https://cdnjs.cloudflare.com https://cdn.jsdelivr.net https://unpkg.com",
  "style-src 'unsafe-inline' https://cdnjs.cloudflare.com https://cdn.jsdelivr.net https://unpkg.com",
  "font-src data: https://cdnjs.cloudflare.com https://cdn.jsdelivr.net https://unpkg.com",
  "img-src data: blob:",
  "media-src data: blob:",
  "connect-src 'none'",
  "form-action 'none'",
  "base-uri 'none'",
].join("; ");

/** The field an artifact's document posts its height in. */
export const ARTIFACT_HEIGHT = "bakeryArtifactHeight";

/** The document's own height: the root's scrollHeight is never below the
 *  frame's, so a frame sized by it could only grow. */
const SIZE_REPORT =
  `<script>(()=>{const s=()=>parent.postMessage({${ARTIFACT_HEIGHT}:Math.ceil(document.documentElement.getBoundingClientRect().height)},"*");` +
  `addEventListener("load",s);new ResizeObserver(s).observe(document.documentElement)})()</script>`;

/** The document the frame loads: the policy first, before anything of the
 *  artifact's can run, with DNS prefetch off (no policy governs a hint's
 *  lookup), then a script that reports the height, then the artifact (an
 *  SVG centred on a page of its own). */
export function artifactDoc(kind: ArtifactKind, source: string): string {
  const head =
    `<meta charset="utf-8"><meta http-equiv="x-dns-prefetch-control" content="off">` +
    `<meta http-equiv="Content-Security-Policy" content="${ARTIFACT_CSP}">${SIZE_REPORT}`;
  if (kind === "svg") {
    return (
      `<!doctype html><html><head>${head}<style>html,body{margin:0}` +
      `body{display:flex;justify-content:center;padding:12px}svg{max-width:100%;height:auto}</style></head>` +
      `<body>${source}</body></html>`
    );
  }
  // Its own doctype goes: whatever comes first in the file comes before the policy.
  return `<!doctype html>${head}${source.replace(/^\s*<!doctype[^>]*>/i, "")}`;
}

/** The artifact's <title>, if it has one. Searched once, front to back: a
 *  regex would rescan the rest from every `<title` that never closes. */
export function artifactTitle(source: string): string {
  const open = source.search(/<title/i);
  const start = open < 0 ? -1 : source.indexOf(">", open);
  const end = start < 0 ? -1 : source.slice(start).search(/<\/title>/i);
  return end < 0 ? "" : source.slice(start + 1, start + end).replace(/\s+/g, " ").trim().slice(0, 80);
}

/** A file name from a title: "Sprint board" → "sprint-board.html". */
export function fileName(title: string, ext: string): string {
  const slug = title
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 60)
    .replace(/-+$/, "");
  return `${slug || "artifact"}.${ext}`;
}

// ---- GenUI --------------------------------------------------------------------
//
// ```genui
// {"type": "card", "title": "Release 0.9", "tone": "ok", "children": [
//   {"type": "metric", "label": "Tests", "value": 837, "delta": "+12", "tone": "ok"},
//   {"type": "progress", "label": "Coverage", "value": 72},
//   {"type": "kv", "items": {"Branch": "main", "Commit": "abbc17a"}},
//   {"type": "list", "items": ["Notes written", {"text": "Deploy", "tone": "warn", "detail": "waits on CI"}]},
//   {"type": "table", "columns": ["crate", "tests"], "rows": [["bakery-node", 412]]}]}
// ```
//
// Six shapes (card, metric, progress, list, table, kv) holding text and
// numbers only: nothing in a spec becomes markup, a link or a script. Each
// takes an optional tone (ok, warn, bad, info, accent, muted), and so does a
// list's item. The root is one of them or a list of them. An unknown type
// stays in place, named.

export type Tone = "ok" | "warn" | "bad" | "info" | "accent" | "muted";
export type GenuiNode =
  | { type: "card"; title: string; subtitle: string; tone: Tone | null; children: GenuiNode[] }
  | { type: "metric"; label: string; value: string; delta: string; hint: string; tone: Tone | null }
  | { type: "progress"; label: string; value: number; max: number; note: string; tone: Tone | null }
  | { type: "list"; ordered: boolean; tone: Tone | null; items: { text: string; detail: string; tone: Tone | null }[] }
  | { type: "table"; columns: string[]; rows: string[][]; tone: Tone | null }
  | { type: "kv"; items: { key: string; value: string }[]; tone: Tone | null }
  | { type: "unknown"; name: string };
export type Genui = { nodes: GenuiNode[]; count: number };

const GENUI_TYPES: Record<string, Exclude<GenuiNode["type"], "unknown">> = {
  card: "card",
  metric: "metric",
  stat: "metric",
  progress: "progress",
  list: "list",
  table: "table",
  kv: "kv",
  "key-value": "kv",
  keyvalue: "kv",
};
const TONES = new Set<string>(["ok", "warn", "bad", "info", "accent", "muted"]);
const GENUI_LIMITS = { nodes: 400, depth: 6, text: 4000, rows: 500, columns: 30 };

class SpecError extends Error {}

/** Text and numbers as shown; anything else is no text at all. */
function cell(v: unknown): string {
  if (typeof v === "string") return v.slice(0, GENUI_LIMITS.text);
  if (typeof v === "number" && Number.isFinite(v)) return readout.format(v);
  if (typeof v === "boolean") return v ? "yes" : "no";
  return "";
}

const tone = (v: unknown): Tone | null => (typeof v === "string" && TONES.has(v) ? (v as Tone) : null);

export function parseGenui(source: string): Parsed<Genui> {
  const json = parseJson(source);
  if (!json.ok) return json;
  let count = 0;
  const array = (v: unknown, at: string, max: number): unknown[] => {
    if (!Array.isArray(v)) throw new SpecError(`${at}: not a list`);
    if (v.length > max) throw new SpecError(`${at}: ${v.length} entries, at most ${max}`);
    return v;
  };
  const node = (v: unknown, at: string, depth: number): GenuiNode => {
    if (++count > GENUI_LIMITS.nodes) throw new SpecError(`more than ${GENUI_LIMITS.nodes} parts`);
    if (depth > GENUI_LIMITS.depth) throw new SpecError(`${at}: nested more than ${GENUI_LIMITS.depth} deep`);
    if (!isObj(v)) throw new SpecError(`${at}: not an object with a "type"`);
    const type = own(GENUI_TYPES, text(v.type).toLowerCase());
    switch (type) {
      case "card":
        return {
          type,
          title: cell(v.title),
          subtitle: cell(v.subtitle),
          tone: tone(v.tone),
          children:
            v.children === undefined
              ? []
              : array(v.children, `${at}.children`, GENUI_LIMITS.nodes).map((c, i) => node(c, `${at}.children[${i}]`, depth + 1)),
        };
      case "metric":
        return { type, label: cell(v.label), value: cell(v.value), delta: cell(v.delta), hint: cell(v.hint), tone: tone(v.tone) };
      case "progress": {
        const value = num(v.value);
        const max = v.max === undefined ? 100 : num(v.max);
        if (value === null) throw new SpecError(`${at}: "value" is not a number`);
        if (max === null || max <= 0) throw new SpecError(`${at}: "max" is not a number above zero`);
        return { type, label: cell(v.label), value, max, note: cell(v.note), tone: tone(v.tone) };
      }
      case "list":
        return {
          type,
          ordered: v.ordered === true,
          tone: tone(v.tone),
          items: array(v.items, `${at}.items`, GENUI_LIMITS.rows).map((item) =>
            isObj(item) ? { text: cell(item.text), detail: cell(item.detail), tone: tone(item.tone) } : { text: cell(item), detail: "", tone: null },
          ),
        };
      case "table": {
        const rows = array(v.rows, `${at}.rows`, GENUI_LIMITS.rows);
        const given = v.columns === undefined ? null : array(v.columns, `${at}.columns`, GENUI_LIMITS.columns).map(cell);
        const columns = given ?? [...new Set(rows.flatMap((row) => (isObj(row) ? Object.keys(row) : [])))].slice(0, GENUI_LIMITS.columns);
        return {
          type,
          columns,
          rows: rows.map((row, i) =>
            isObj(row)
              ? columns.map((c) => cell(row[c]))
              : array(row, `${at}.rows[${i}]`, GENUI_LIMITS.columns).map(cell),
          ),
          tone: tone(v.tone),
        };
      }
      case "kv": {
        const items = isObj(v.items)
          ? Object.entries(v.items).map(([key, value]) => ({ key, value: cell(value) }))
          : array(v.items, `${at}.items`, GENUI_LIMITS.rows).map((item) =>
              Array.isArray(item) ? { key: cell(item[0]), value: cell(item[1]) } : isObj(item) ? { key: cell(item.key), value: cell(item.value) } : { key: cell(item), value: "" },
            );
        return { type, items: items.slice(0, GENUI_LIMITS.rows), tone: tone(v.tone) };
      }
      default:
        return { type: "unknown", name: text(v.type) };
    }
  };
  const root = json.value;
  try {
    const nodes = Array.isArray(root) ? array(root, "$", GENUI_LIMITS.nodes).map((v, i) => node(v, `$[${i}]`, 1)) : [node(root, "$", 1)];
    return { ok: true, value: { nodes, count } };
  } catch (e) {
    if (e instanceof SpecError) return fail(e.message);
    throw e;
  }
}
