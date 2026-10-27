// Visual blocks in replies, drawn from fenced code (Streamdown's custom
// renderers): ```mermaid diagrams, ```chart data, ```html and ```svg
// artifacts, and ```genui cards. Each sits in one frame: the fence's own
// name as the agent wrote it, its views (the visual, or the exact source),
// and its actions. The text-only half lives in ./visual.

import {
  Component,
  type ComponentType,
  memo,
  useCallback,
  useEffect,
  useId,
  useMemo,
  useRef,
  useState,
  useSyncExternalStore,
  type ReactNode,
} from "react";
import {
  AppWindowIcon,
  ChartAreaIcon,
  ChartColumnIcon,
  ChartLineIcon,
  ChartPieIcon,
  CheckIcon,
  CopyIcon,
  DownloadIcon,
  LayoutDashboardIcon,
  Maximize2Icon,
  ShapesIcon,
  TriangleAlertIcon,
  WorkflowIcon,
  type LucideIcon,
} from "lucide-react";
import mermaidScript from "mermaid/dist/mermaid.min.js?url";
import type { CustomRenderer, CustomRendererProps } from "streamdown";
import { Dialog, DialogContent, DialogTitle } from "@/components/ui/dialog";
import { reportError } from "$lib/clientLog";
import { CodeView } from "./ToolRows";
import {
  ARTIFACT_HEIGHT,
  type ArtifactKind,
  artifactDoc,
  artifactTitle,
  type Chart,
  drawingCache,
  fileName,
  type GenuiNode,
  labelEvery,
  mermaidFrameDoc,
  mermaidKind,
  niceTicks,
  parseChart,
  parseGenui,
  pieAngles,
  type SizedSvg,
  sizedSvg,
  svgImageUrl,
  tickLabel,
  valueLabel,
  valueRange,
} from "./visual";

// ---- shared -----------------------------------------------------------------

/** Hands the browser a file to save (Electron asks where). Bare bytes, as a
 *  sent file saves: where a webview ignores `download` and opens the blob
 *  instead (the phone), an HTML or SVG file must never run as a page in the
 *  app's own origin. */
function saveFile(name: string, text: string) {
  const url = URL.createObjectURL(new Blob([text], { type: "application/octet-stream" }));
  const link = document.createElement("a");
  link.href = url;
  link.download = name;
  link.click();
  setTimeout(() => URL.revokeObjectURL(url), 60_000);
}

/** The scheme in use, as a key that changes when the operator switches it
 *  (or edits a custom one), for visuals whose colours are baked in. */
function schemeKey(): string {
  const root = document.documentElement;
  return ["data-theme", "data-color-scheme", "data-color-mode", "style"].map((a) => root.getAttribute(a)).join("|");
}
function onSchemeChange(change: () => void) {
  const observer = new MutationObserver(change);
  observer.observe(document.documentElement, {
    attributes: true,
    attributeFilter: ["data-theme", "data-color-scheme", "data-color-mode", "style"],
  });
  return () => observer.disconnect();
}
const useSchemeKey = () => useSyncExternalStore(onSchemeChange, schemeKey, () => "");

/** An error's message, from whichever realm threw it (mermaid's are its
 *  frame's, where `instanceof Error` is false). */
function errorText(e: unknown): string {
  const message = (e as { message?: unknown } | null)?.message;
  return typeof message === "string" ? message : String(e);
}

function FrameAction({ label, onClick, children }: { label: string; onClick: () => void; children: ReactNode }) {
  return (
    <button aria-label={label} className="bakery-visual-action" onClick={onClick} title={label} type="button">
      {children}
    </button>
  );
}

function CopyAction({ text, label }: { text: string; label: string }) {
  const [copied, setCopied] = useState(false);
  const copy = useCallback(() => {
    void navigator.clipboard?.writeText(text).then(
      () => {
        setCopied(true);
        setTimeout(() => setCopied(false), 1200);
      },
      () => {},
    );
  }, [text]);
  return (
    <FrameAction label={copied ? "Copied" : label} onClick={copy}>
      {copied ? <CheckIcon className="size-3.5" /> : <CopyIcon className="size-3.5" />}
    </FrameAction>
  );
}

type FrameView = { id: string; label: string };

/** One visual's frame: what it is, its views, and its actions. `open`
 *  renders it again, large, in a dialog over the app. */
function VisualFrame({
  kind,
  icon: Icon,
  detail,
  note,
  bad,
  views,
  view,
  onView,
  copy,
  save,
  open,
  children,
}: {
  /** The fence's language, as written. */
  kind: string;
  icon: LucideIcon;
  /** What the visual says it is (a diagram's type, a chart's title…). */
  detail?: string;
  /** Its state: "drawing…", an error. */
  note?: string;
  bad?: boolean;
  views: FrameView[];
  view: string;
  onView: (id: string) => void;
  copy: { text: string; label: string };
  save?: { name: string; text: string };
  open?: () => ReactNode;
  children: ReactNode;
}) {
  const [opened, setOpened] = useState(false);
  return (
    <figure className="bakery-visual" data-kind={kind} data-testid="visual">
      <figcaption className="bakery-visual-head">
        <Icon aria-hidden className="bakery-visual-icon size-3.5 shrink-0" />
        <span className="bakery-visual-kind">{kind}</span>
        {detail && <span className="bakery-visual-detail">{detail}</span>}
        {note && (
          <span className="bakery-visual-note" data-bad={bad ? "" : undefined}>
            {note}
          </span>
        )}
        <span className="flex-1" />
        {views.length > 1 && (
          <span aria-label="View" className="bakery-visual-views" role="group">
            {views.map((v) => (
              <button aria-pressed={v.id === view} key={v.id} onClick={() => onView(v.id)} type="button">
                {v.label}
              </button>
            ))}
          </span>
        )}
        <CopyAction label={copy.label} text={copy.text} />
        {save && (
          <FrameAction label={`Save ${save.name}`} onClick={() => saveFile(save.name, save.text)}>
            <DownloadIcon className="size-3.5" />
          </FrameAction>
        )}
        {open && (
          <FrameAction label="Open large" onClick={() => setOpened(true)}>
            <Maximize2Icon className="size-3.5" />
          </FrameAction>
        )}
      </figcaption>
      <div className="bakery-visual-body">{children}</div>
      {open && (
        <Dialog onOpenChange={setOpened} open={opened}>
          <DialogContent aria-describedby={undefined} className="bakery-visual-dialog">
            <DialogTitle className="bakery-visual-head">
              <Icon aria-hidden className="bakery-visual-icon size-3.5 shrink-0" />
              <span className="bakery-visual-kind">{kind}</span>
              {detail && <span className="bakery-visual-detail">{detail}</span>}
            </DialogTitle>
            <div className="bakery-visual-dialog-body">{opened && open()}</div>
          </DialogContent>
        </Dialog>
      )}
    </figure>
  );
}

/** The exact text of the fence. */
function SourceView({ code, lang }: { code: string; lang: string }) {
  return <CodeView code={code} lang={lang} />;
}

// ---- mermaid ----------------------------------------------------------------

type Mermaid = (typeof import("mermaid"))["default"];
let mermaidLoad: Promise<Mermaid> | null = null;
let drawn = 0;

/** A scheme token's literal colour: mermaid computes its shades from real
 *  colours, so `var()` and `color-mix()` are no use to it. */
function literal(style: CSSStyleDeclaration, name: string): string | undefined {
  const value = style.getPropertyValue(name).trim();
  return /^(#[0-9a-f]{3,8}|rgba?\(|hsla?\()/i.test(value) ? value : undefined;
}

/** Mermaid's colours, from the scheme in use. */
function mermaidTheme() {
  const root = document.documentElement;
  const style = getComputedStyle(root);
  const t = (name: string) => literal(style, name);
  const vars: Record<string, string | boolean | undefined> = {
    darkMode: root.getAttribute("data-color-mode") !== "light",
    fontFamily: style.getPropertyValue("--font-ui").trim() || undefined,
    fontSize: "13px",
    background: t("--bg-app"),
    primaryColor: t("--bg-raised"),
    primaryTextColor: t("--fg"),
    primaryBorderColor: t("--accent"),
    secondaryColor: t("--bg-chip"),
    secondaryTextColor: t("--fg-body"),
    secondaryBorderColor: t("--line-strong"),
    tertiaryColor: t("--bg-panel"),
    tertiaryTextColor: t("--fg-body"),
    tertiaryBorderColor: t("--line"),
    mainBkg: t("--bg-raised"),
    nodeBorder: t("--accent"),
    lineColor: t("--fg-dim"),
    textColor: t("--fg-body"),
    titleColor: t("--fg"),
    clusterBkg: t("--bg-panel"),
    clusterBorder: t("--line-strong"),
    edgeLabelBackground: t("--bg-app"),
    noteBkgColor: t("--bg-chip"),
    noteTextColor: t("--fg-body"),
    noteBorderColor: t("--line-strong"),
    actorBkg: t("--bg-raised"),
    actorBorder: t("--accent"),
    actorTextColor: t("--fg"),
    actorLineColor: t("--line-strong"),
    signalColor: t("--fg-dim"),
    signalTextColor: t("--fg-body"),
    labelBoxBkgColor: t("--bg-chip"),
    labelBoxBorderColor: t("--line-strong"),
    labelTextColor: t("--fg"),
    loopTextColor: t("--fg-body"),
    activationBkgColor: t("--bg-chip"),
    activationBorderColor: t("--accent"),
    errorBkgColor: t("--bad"),
    errorTextColor: t("--fg"),
  };
  for (let i = 1; i <= 8; i++) vars[`pie${i}`] = t(`--viz-${i}`);
  return Object.fromEntries(Object.entries(vars).filter(([, v]) => v !== undefined));
}

/** Mermaid, in a frame of its own whose policy (MERMAID_FRAME_CSP) lets
 *  nothing but mermaid load: it draws there, live, never in the app's
 *  document. The first diagram makes it, never the app. It is the app's
 *  own origin, so the app calls it directly; laid out but never shown,
 *  since mermaid measures its text as it draws. */
function loadMermaid(): Promise<Mermaid> {
  return (mermaidLoad ??= new Promise<Mermaid>((resolve, reject) => {
    const frame = document.createElement("iframe");
    frame.style.cssText = "position:fixed;inset:0;width:100%;height:100%;border:0;visibility:hidden;pointer-events:none";
    frame.srcdoc = mermaidFrameDoc(new URL(mermaidScript, document.baseURI).href);
    frame.addEventListener(
      "load",
      () => {
        const mermaid = (frame.contentWindow as (Window & { mermaid?: Mermaid }) | null)?.mermaid;
        if (mermaid) resolve(mermaid);
        else reject(new Error("mermaid did not load"));
      },
      { once: true },
    );
    document.body.append(frame);
  }));
}

/** The diagram as SVG text. Strict: labels are text, links and clicks do
 *  nothing. */
async function renderMermaid(code: string): Promise<string> {
  const mermaid = await loadMermaid();
  const themeVariables = mermaidTheme();
  mermaid.initialize({
    startOnLoad: false,
    securityLevel: "strict",
    // Mermaid's own list, and two more a diagram's config can't set.
    secure: ["secure", "securityLevel", "startOnLoad", "maxTextSize", "suppressErrorRendering", "maxEdges", "htmlLabels", "themeCSS"],
    suppressErrorRendering: true,
    theme: "base",
    // SVG text, not HTML in a foreignObject: the picture draws the same as
    // an image everywhere.
    htmlLabels: false,
    fontFamily: themeVariables.fontFamily as string | undefined,
    themeVariables,
  });
  const { svg } = await mermaid.render(`bakery-mermaid-${++drawn}`, code);
  return svg;
}

type Drawing = { key: string; image: SizedSvg | null; error: string };

/** Each diagram drawn once per scheme: a workspace shown again (its effects
 *  run again) or a thread reopened shows its picture at once. */
const drawings = drawingCache<SizedSvg | null>(64);
const drawingOf = (key: string, image: SizedSvg | null): Drawing => ({ key, image, error: image ? "" : "mermaid drew no viewBox" });

const MermaidBlock = memo(function MermaidBlock({ code, isIncomplete }: CustomRendererProps) {
  const scheme = useSchemeKey();
  const key = `${scheme}\u0000${code}`;
  const [drawing, setDrawing] = useState<Drawing | null>(() => {
    const image = drawings.peek(key);
    return image === undefined ? null : drawingOf(key, image);
  });
  const [view, setView] = useState("diagram");
  useEffect(() => {
    if (isIncomplete) return;
    let live = true;
    drawings.draw(key, () => renderMermaid(code).then(sizedSvg)).then(
      (image) => live && setDrawing((was) => (was?.key === key && was.image === image ? was : drawingOf(key, image))),
      (e: unknown) => live && setDrawing({ key, image: null, error: errorText(e) }),
    );
    return () => {
      live = false;
    };
  }, [code, isIncomplete, key]);
  // A redraw for a new scheme keeps the last picture up until it lands.
  const image = drawing?.image ?? null;
  const error = drawing?.key === key ? drawing.error : "";
  const src = useMemo(() => (image ? svgImageUrl(image.svg) : ""), [image]);
  const picture = (large: boolean) =>
    image && (
      <div className="bakery-visual-canvas" data-large={large ? "" : undefined}>
        <img alt={`${mermaidKind(code) || "mermaid"} diagram`} height={image.height} src={src} width={image.width} />
      </div>
    );
  return (
    <VisualFrame
      bad={error !== ""}
      copy={{ text: code, label: "Copy mermaid source" }}
      detail={mermaidKind(code)}
      icon={WorkflowIcon}
      kind="mermaid"
      note={isIncomplete ? "writing…" : error ? "error" : drawing?.key !== key ? "drawing…" : undefined}
      onView={setView}
      open={image ? () => picture(true) : undefined}
      save={image ? { name: "diagram.svg", text: image.svg } : undefined}
      view={view}
      views={
        image
          ? [
              { id: "diagram", label: "Diagram" },
              { id: "source", label: "Source" },
            ]
          : []
      }
    >
      {/* Until the first picture lands, the source as it is written. */}
      {error && <pre className="bakery-visual-error">{error}</pre>}
      {view === "source" || !image ? <SourceView code={code} lang="mermaid" /> : picture(false)}
    </VisualFrame>
  );
});

// ---- charts -----------------------------------------------------------------
//
// Drawn as SVG at the exact width the frame has, so text never stretches.
// Marks follow the dataviz rules: thin bars (≤ 24px) rounded at the data end
// and square at the baseline, 2px lines, a 2px surface gap and ring, hairline
// grid, colour from theme.css `--viz-*` in fixed order and never on text, a
// legend from two series, and every value also in the Table view.

const seriesColor = (i: number) => `var(--viz-${i + 1})`;

/** A callback ref that keeps the element's width. */
function useWidth(): [(el: HTMLElement | null) => (() => void) | undefined, number] {
  const [width, setWidth] = useState(0);
  const ref = useCallback((el: HTMLElement | null) => {
    if (!el) return;
    const measure = () => setWidth(Math.floor(el.getBoundingClientRect().width));
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(el);
    return () => observer.disconnect();
  }, []);
  return [ref, width];
}

const shortLabel = (text: string) => (text.length > 16 ? `${text.slice(0, 15)}…` : text);

/** A bar from `base` to `end`, its data end rounded. */
function barPath(x: number, w: number, base: number, end: number): string {
  const r = Math.min(4, w / 2, Math.abs(end - base));
  const s = end <= base ? 1 : -1;
  return `M${x},${base}V${end + s * r}Q${x},${end} ${x + r},${end}H${x + w - r}Q${x + w},${end} ${x + w},${end + s * r}V${base}Z`;
}

/** Runs of present values: a null breaks the line. */
function runs(values: readonly (number | null)[]): [number, number][][] {
  const out: [number, number][][] = [];
  let run: [number, number][] = [];
  values.forEach((v, i) => {
    if (v === null) {
      if (run.length) out.push(run);
      run = [];
    } else run.push([i, v]);
  });
  if (run.length) out.push(run);
  return out;
}

function lastPresent(values: readonly (number | null)[]): number {
  for (let i = values.length - 1; i >= 0; i--) if (values[i] !== null) return i;
  return -1;
}

function Legend({ chart, line }: { chart: Chart; line: boolean }) {
  return (
    <div className="bakery-chart-legend">
      {chart.series.map((s, i) => (
        <span className="bakery-chart-legend-item" key={i}>
          <span className="bakery-chart-swatch" data-line={line ? "" : undefined} style={{ background: seriesColor(i) }} />
          {s.name}
        </span>
      ))}
    </div>
  );
}

function XYPlot({ chart, width, height }: { chart: Chart; width: number; height: number }) {
  // A gradient id that is safe to name in url(#…).
  const id = `viz${useId().replace(/[^a-zA-Z0-9]/g, "")}`;
  const [hover, setHover] = useState<number | null>(null);
  const [lo, hi] = valueRange(chart);
  const { ticks, step: tickStep } = niceTicks(lo, hi, Math.max(2, Math.round(height / 52)));
  const yMin = ticks[0];
  const yMax = ticks[ticks.length - 1];
  const tickText = ticks.map((t) => tickLabel(t, tickStep));
  const n = chart.categories.length;
  const bars = chart.kind === "bar";
  // One line: its last value at its end. More: the legend names them.
  const endAt = !bars && chart.series.length === 1 ? lastPresent(chart.series[0].values) : -1;
  const endText = endAt >= 0 ? valueLabel(chart.series[0].values[endAt], chart.unit) : "";
  const x0 = Math.max(...tickText.map((t) => t.length)) * 6.4 + 16;
  const x1 = width - (endText ? endText.length * 6.6 + 14 : 14);
  const y0 = 10;
  const y1 = height - 26;
  // Under a pixel a point when there are more points than pixels: every
  // one still lands inside the plot.
  const band = Math.max(0, x1 - x0) / n;
  const y = (v: number) => y1 - ((v - yMin) / (yMax - yMin)) * (y1 - y0);
  const cx = (i: number) => x0 + band * (i + 0.5);
  const base = y(Math.min(Math.max(0, yMin), yMax));
  const every = labelEvery(n, band, Math.max(...chart.categories.map((c) => shortLabel(c).length)));
  const k = chart.series.length;
  const barW = Math.max(1, Math.min(24, (band * 0.72 - 2 * (k - 1)) / k));
  const groupW = barW * k + 2 * (k - 1);

  const pick = (clientX: number, svg: SVGSVGElement) => {
    const i = Math.floor((clientX - svg.getBoundingClientRect().left - x0) / band);
    setHover(i >= 0 && i < n ? i : null);
  };
  const step = (by: number) => setHover((h) => Math.min(n - 1, Math.max(0, (h ?? (by > 0 ? -1 : n)) + by)));
  const tipLeft = hover !== null && cx(hover) < width * 0.6;

  return (
    <div className="bakery-chart-plot">
      {/* Arrow keys walk the categories, as the pointer does. */}
      <svg
        aria-label={`${chart.title || chart.kind} chart: ${k} series over ${n} categories. The Table view lists every value.`}
        className="bakery-chart-svg"
        height={height}
        onBlur={() => setHover(null)}
        onKeyDown={(e) => {
          if (e.key === "ArrowRight" || e.key === "ArrowLeft") {
            e.preventDefault();
            step(e.key === "ArrowRight" ? 1 : -1);
          }
        }}
        onPointerLeave={() => setHover(null)}
        onPointerMove={(e) => pick(e.clientX, e.currentTarget)}
        role="img"
        tabIndex={0}
        width={width}
      >
        <defs>
          {!bars &&
            chart.series.map((_, si) => (
              <linearGradient id={`${id}-wash-${si}`} key={si} x1="0" x2="0" y1="0" y2="1">
                <stop offset="0" style={{ stopColor: seriesColor(si), stopOpacity: chart.kind === "area" ? 0.26 : 0.1 }} />
                <stop offset="1" style={{ stopColor: seriesColor(si), stopOpacity: 0 }} />
              </linearGradient>
            ))}
        </defs>
        {ticks.map((t, i) => (
          <g key={t}>
            <line className={t === 0 ? "bakery-chart-zero" : "bakery-chart-grid"} x1={x0} x2={x1} y1={y(t)} y2={y(t)} />
            <text className="bakery-chart-tick" dy="0.32em" textAnchor="end" x={x0 - 8} y={y(t)}>
              {tickText[i]}
            </text>
          </g>
        ))}
        {chart.categories.map((c, i) =>
          i % every === 0 ? (
            <text className="bakery-chart-tick" key={i} textAnchor="middle" x={cx(i)} y={y1 + 17}>
              {shortLabel(c)}
            </text>
          ) : null,
        )}
        {hover !== null && bars && <rect className="bakery-chart-band" height={y1 - y0} width={band} x={x0 + band * hover} y={y0} />}
        {bars
          ? chart.series.map((s, si) =>
              s.values.map((v, i) =>
                v === null ? null : (
                  <path
                    d={barPath(cx(i) - groupW / 2 + si * (barW + 2), barW, base, y(v))}
                    data-dim={hover !== null && hover !== i ? "" : undefined}
                    key={`${si}:${i}`}
                    style={{ fill: seriesColor(si) }}
                  />
                ),
              ),
            )
          : chart.series.map((s, si) => (
              <g key={si}>
                {runs(s.values).map((run, ri) => (
                  <g key={ri}>
                    <path
                      d={`M${cx(run[0][0])},${base}${run.map(([i, v]) => `L${cx(i)},${y(v)}`).join("")}L${cx(run[run.length - 1][0])},${base}Z`}
                      style={{ fill: `url(#${id}-wash-${si})` }}
                    />
                    <path
                      className="bakery-chart-line"
                      d={run.map(([i, v], j) => `${j ? "L" : "M"}${cx(i)},${y(v)}`).join("")}
                      style={{ stroke: seriesColor(si) }}
                    />
                  </g>
                ))}
              </g>
            ))}
        {!bars && hover !== null && <line className="bakery-chart-cross" x1={cx(hover)} x2={cx(hover)} y1={y0} y2={y1} />}
        {!bars &&
          chart.series.map((s, si) => {
            const at = hover ?? lastPresent(s.values);
            const v = s.values[at];
            return v === null || at < 0 ? null : (
              <circle className="bakery-chart-dot" cx={cx(at)} cy={y(v)} key={si} r={4} style={{ fill: seriesColor(si) }} />
            );
          })}
        {endText && (
          <text className="bakery-chart-end" dy="0.32em" x={cx(endAt) + 9} y={y(chart.series[0].values[endAt] ?? 0)}>
            {endText}
          </text>
        )}
      </svg>
      {hover !== null && (
        <div
          className="bakery-chart-tip"
          style={tipLeft ? { left: cx(hover) + 14 } : { right: width - cx(hover) + 14 }}
        >
          <div className="bakery-chart-tip-head">{chart.categories[hover]}</div>
          {chart.series.map((s, si) => (
            <div className="bakery-chart-tip-row" key={si}>
              <span className="bakery-chart-swatch" data-line="" style={{ background: seriesColor(si) }} />
              <strong>{valueLabel(s.values[hover], chart.unit)}</strong>
              {k > 1 && <span>{s.name}</span>}
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

/** A slice from angle a0 to a1 (clockwise from twelve), as a ring when r0 > 0. */
function slicePath(c: number, r: number, r0: number, a0: number, a1: number): string {
  const at = (rad: number, a: number) => `${c + rad * Math.sin(a)},${c - rad * Math.cos(a)}`;
  if (a1 - a0 >= Math.PI * 2 - 1e-6) {
    // A whole circle is two half arcs.
    const ring = (rad: number, sweep: number) =>
      `M${at(rad, 0)}A${rad},${rad} 0 1 ${sweep} ${at(rad, Math.PI)}A${rad},${rad} 0 1 ${sweep} ${at(rad, 0)}Z`;
    return r0 > 0 ? `${ring(r, 1)}${ring(r0, 0)}` : ring(r, 1);
  }
  const large = a1 - a0 > Math.PI ? 1 : 0;
  const outer = `M${at(r, a0)}A${r},${r} 0 ${large} 1 ${at(r, a1)}`;
  return r0 > 0 ? `${outer}L${at(r0, a1)}A${r0},${r0} 0 ${large} 0 ${at(r0, a0)}Z` : `${outer}L${c},${c}Z`;
}

function PiePlot({ chart, width, height }: { chart: Chart; width: number; height: number }) {
  const [hover, setHover] = useState<number | null>(null);
  const values = chart.series[0].values;
  const total = values.reduce<number>((a, v) => a + (v ?? 0), 0);
  const angles = pieAngles(values);
  const size = Math.max(120, Math.min(height - 20, width - 24, 360));
  const c = size / 2;
  const r = c - 4;
  const r0 = chart.kind === "donut" ? r * 0.62 : 0;
  const share = (v: number | null) => `${((100 * (v ?? 0)) / total).toFixed(1)}%`;
  return (
    <div className="bakery-chart-pie">
      <svg
        aria-label={`${chart.title || chart.kind}: ${chart.categories.length} slices. The legend and the Table view list every value.`}
        className="bakery-chart-svg"
        height={size}
        onPointerLeave={() => setHover(null)}
        role="img"
        width={size}
      >
        {angles.map(([a0, a1], i) =>
          a1 > a0 ? (
            <path
              className="bakery-chart-slice"
              d={slicePath(c, r, r0, a0, a1)}
              data-dim={hover !== null && hover !== i ? "" : undefined}
              fillRule="evenodd"
              key={i}
              onPointerEnter={() => setHover(i)}
              style={{ fill: seriesColor(i) }}
            />
          ) : null,
        )}
        {r0 > 0 && (
          <text className="bakery-chart-center" textAnchor="middle" x={c} y={c}>
            <tspan className="bakery-chart-center-value" dy="-0.1em" x={c}>
              {valueLabel(hover === null ? total : values[hover], chart.unit)}
            </tspan>
            <tspan dy="1.5em" x={c}>
              {hover === null ? "total" : shortLabel(chart.categories[hover])}
            </tspan>
          </text>
        )}
      </svg>
      <ul className="bakery-chart-slices">
        {chart.categories.map((name, i) => (
          <li
            data-active={hover === i ? "" : undefined}
            key={i}
            onPointerEnter={() => setHover(i)}
            onPointerLeave={() => setHover(null)}
          >
            <span className="bakery-chart-swatch" style={{ background: seriesColor(i) }} />
            <span className="bakery-chart-slice-name">{name}</span>
            <strong>{valueLabel(values[i], chart.unit)}</strong>
            <span className="bakery-chart-share">{share(values[i])}</span>
          </li>
        ))}
      </ul>
    </div>
  );
}

function ChartView({ chart, height }: { chart: Chart; height: number }) {
  const [ref, width] = useWidth();
  const pie = chart.kind === "pie" || chart.kind === "donut";
  // The width is the plot's own box, inside the frame's padding.
  return (
    <div className="bakery-chart">
      {!pie && chart.series.length > 1 && <Legend chart={chart} line={chart.kind === "line"} />}
      <div ref={ref}>
        {width > 0 &&
          (pie ? <PiePlot chart={chart} height={height} width={width} /> : <XYPlot chart={chart} height={height} width={width} />)}
      </div>
    </div>
  );
}

/** Every value, as a table: the chart's readable twin. */
function ChartTable({ chart }: { chart: Chart }) {
  return (
    <div className="bakery-visual-scroll">
      <table className="bakery-visual-table">
        <thead>
          <tr>
            <th>{chart.x || "#"}</th>
            {chart.series.map((s, si) => (
              <th data-num="" key={si}>
                {s.name}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {chart.categories.map((c, i) => (
            <tr key={i}>
              <td>{c}</td>
              {chart.series.map((s, si) => (
                <td data-num="" key={si}>
                  {valueLabel(s.values[i], chart.unit)}
                </td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

const CHART_ICON: Record<Chart["kind"], LucideIcon> = {
  bar: ChartColumnIcon,
  line: ChartLineIcon,
  area: ChartAreaIcon,
  pie: ChartPieIcon,
  donut: ChartPieIcon,
};

const ChartBlock = memo(function ChartBlock({ code, isIncomplete }: CustomRendererProps) {
  const parsed = useMemo(() => (isIncomplete ? null : parseChart(code)), [code, isIncomplete]);
  const [view, setView] = useState("chart");
  const chart = parsed?.ok ? parsed.value : null;
  const error = parsed && !parsed.ok ? parsed.error : "";
  const points = chart?.categories.length ?? 0;
  return (
    <VisualFrame
      bad={error !== ""}
      copy={{ text: code, label: "Copy chart spec" }}
      detail={chart?.title}
      icon={chart ? CHART_ICON[chart.kind] : ChartColumnIcon}
      kind="chart"
      note={
        isIncomplete
          ? "writing…"
          : error
            ? "error"
            : chart
              ? `${chart.kind} · ${chart.series.length} series · ${points} ${points === 1 ? "point" : "points"}`
              : undefined
      }
      onView={setView}
      open={chart ? () => <ChartView chart={chart} height={560} /> : undefined}
      view={view}
      views={
        chart
          ? [
              { id: "chart", label: "Chart" },
              { id: "table", label: "Table" },
              { id: "source", label: "Source" },
            ]
          : []
      }
    >
      {error && <pre className="bakery-visual-error">{error}</pre>}
      {!chart || view === "source" ? (
        <SourceView code={code} lang="json" />
      ) : view === "table" ? (
        <ChartTable chart={chart} />
      ) : (
        <ChartView chart={chart} height={260} />
      )}
    </VisualFrame>
  );
});

// ---- artifacts ----------------------------------------------------------------

/** The artifact in its sandbox: `allow-scripts` and nothing else, so its
 *  origin is its own and the app is out of its reach. Inline, it takes the
 *  height its document reports, within bounds; large, the dialog's. */
function ArtifactFrame({ doc, large }: { doc: string; large?: boolean }) {
  const frame = useRef<HTMLIFrameElement>(null);
  const [height, setHeight] = useState(280);
  useEffect(() => {
    if (large) return;
    const onMessage = (e: MessageEvent) => {
      if (e.source === null || e.source !== frame.current?.contentWindow) return;
      const reported = (e.data as Record<string, unknown> | null)?.[ARTIFACT_HEIGHT];
      if (typeof reported === "number" && Number.isFinite(reported)) setHeight(Math.min(720, Math.max(60, Math.ceil(reported))));
    };
    window.addEventListener("message", onMessage);
    return () => window.removeEventListener("message", onMessage);
  }, [large]);
  return (
    <iframe
      className="bakery-artifact-frame"
      data-large={large ? "" : undefined}
      ref={frame}
      referrerPolicy="no-referrer"
      sandbox="allow-scripts"
      srcDoc={doc}
      style={large ? undefined : { height }}
      title="Artifact preview"
    />
  );
}

const ArtifactBlock = memo(function ArtifactBlock({ code, isIncomplete, language }: CustomRendererProps) {
  const kind: ArtifactKind = language === "svg" ? "svg" : "html";
  // It runs only once the operator opens its Preview: a reply, moved-in
  // history or another agent's message never runs one by being shown.
  const [picked, setPicked] = useState("source");
  const { doc, title } = useMemo(
    () => ({ doc: isIncomplete ? "" : artifactDoc(kind, code), title: artifactTitle(code) }),
    [code, isIncomplete, kind],
  );
  const view = isIncomplete ? "source" : picked;
  return (
    <VisualFrame
      copy={{ text: code, label: `Copy ${kind} source` }}
      detail={title}
      icon={kind === "svg" ? ShapesIcon : AppWindowIcon}
      kind={language}
      note={isIncomplete ? "writing…" : undefined}
      onView={setPicked}
      open={isIncomplete ? undefined : () => <ArtifactFrame doc={doc} large />}
      save={isIncomplete ? undefined : { name: fileName(title, kind), text: code }}
      view={view}
      views={
        isIncomplete
          ? []
          : [
              { id: "preview", label: "Preview" },
              { id: "source", label: "Source" },
            ]
      }
    >
      {view === "preview" ? <ArtifactFrame doc={doc} /> : <SourceView code={code} lang={kind === "svg" ? "xml" : "html"} />}
    </VisualFrame>
  );
});

// ---- GenUI --------------------------------------------------------------------
//
// Every value lands as a text node: React escapes it, and no part takes
// markup, a link, a style or a handler from the spec.

const NUMERIC = /^[-+−]?[\d,.]+\s?%?$/;

/** Metrics in a row sit side by side; the rest stack. */
function GenuiParts({ nodes }: { nodes: GenuiNode[] }) {
  const out: ReactNode[] = [];
  for (let i = 0; i < nodes.length; ) {
    if (nodes[i].type === "metric") {
      const start = i;
      while (i < nodes.length && nodes[i].type === "metric") i++;
      out.push(
        <div className="bakery-genui-metrics" key={start}>
          {nodes.slice(start, i).map((node, j) => (
            <GenuiPart key={j} node={node} />
          ))}
        </div>,
      );
    } else {
      out.push(<GenuiPart key={i} node={nodes[i]} />);
      i++;
    }
  }
  return <>{out}</>;
}

function GenuiPart({ node }: { node: GenuiNode }) {
  switch (node.type) {
    case "card":
      return (
        <section className="bakery-genui-card" data-tone={node.tone ?? undefined}>
          {(node.title || node.subtitle) && (
            <header className="bakery-genui-card-head">
              {node.title && <span className="bakery-genui-title">{node.title}</span>}
              {node.subtitle && <span className="bakery-genui-sub">{node.subtitle}</span>}
            </header>
          )}
          {node.children.length > 0 && (
            <div className="bakery-genui-stack">
              <GenuiParts nodes={node.children} />
            </div>
          )}
        </section>
      );
    case "metric":
      return (
        <div className="bakery-genui-metric" data-tone={node.tone ?? undefined}>
          <span className="bakery-genui-label">{node.label}</span>
          <span className="bakery-genui-value">{node.value || "—"}</span>
          {(node.delta || node.hint) && (
            <span className="bakery-genui-foot">
              {node.delta && <span className="bakery-genui-delta">{node.delta}</span>}
              {node.hint && <span>{node.hint}</span>}
            </span>
          )}
        </div>
      );
    case "progress": {
      const share = Math.min(100, Math.max(0, (node.value / node.max) * 100));
      return (
        <div className="bakery-genui-progress" data-tone={node.tone ?? undefined}>
          <span className="bakery-genui-progress-head">
            <span className="bakery-genui-label">{node.label}</span>
            <span className="bakery-genui-progress-value">
              {node.max === 100 ? `${valueLabel(node.value, "")}%` : `${valueLabel(node.value, "")} / ${valueLabel(node.max, "")}`}
            </span>
          </span>
          <span
            aria-label={node.label || "progress"}
            aria-valuemax={node.max}
            aria-valuemin={0}
            aria-valuenow={node.value}
            className="bakery-genui-track"
            role="progressbar"
          >
            <span className="bakery-genui-fill" style={{ width: `${share}%` }} />
          </span>
          {node.note && <span className="bakery-genui-note">{node.note}</span>}
        </div>
      );
    }
    case "list": {
      const List = node.ordered ? "ol" : "ul";
      return (
        <List className="bakery-genui-list" data-tone={node.tone ?? undefined}>
          {node.items.map((item, i) => (
            <li data-tone={item.tone ?? undefined} key={i}>
              <span aria-hidden className="bakery-genui-mark">
                {node.ordered ? i + 1 : null}
              </span>
              <span className="bakery-genui-item">
                <span>{item.text}</span>
                {item.detail && <span className="bakery-genui-detail">{item.detail}</span>}
              </span>
            </li>
          ))}
        </List>
      );
    }
    case "table": {
      // A column of numbers aligns right, its heading too.
      const width = Math.max(node.columns.length, ...node.rows.map((row) => row.length));
      const numeric = Array.from(
        { length: width },
        (_, j) => node.rows.some((row) => (row[j] ?? "") !== "") && node.rows.every((row) => (row[j] ?? "") === "" || NUMERIC.test(row[j])),
      );
      return (
        <div className="bakery-genui-table bakery-visual-scroll" data-tone={node.tone ?? undefined}>
          <table className="bakery-visual-table">
            {node.columns.length > 0 && (
              <thead>
                <tr>
                  {node.columns.map((c, j) => (
                    <th data-num={numeric[j] ? "" : undefined} key={j}>
                      {c}
                    </th>
                  ))}
                </tr>
              </thead>
            )}
            <tbody>
              {node.rows.map((row, i) => (
                <tr key={i}>
                  {row.map((c, j) => (
                    <td data-num={numeric[j] ? "" : undefined} key={j}>
                      {c}
                    </td>
                  ))}
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      );
    }
    case "kv":
      return (
        <dl className="bakery-genui-kv" data-tone={node.tone ?? undefined}>
          {node.items.map((item, i) => (
            <div key={i}>
              <dt>{item.key}</dt>
              <dd>{item.value}</dd>
            </div>
          ))}
        </dl>
      );
    case "unknown":
      return <span className="bakery-genui-unknown">unknown genui type “{node.name}”</span>;
  }
}

const GenuiBlock = memo(function GenuiBlock({ code, isIncomplete }: CustomRendererProps) {
  const parsed = useMemo(() => (isIncomplete ? null : parseGenui(code)), [code, isIncomplete]);
  const [view, setView] = useState("view");
  const ui = parsed?.ok ? parsed.value : null;
  const error = parsed && !parsed.ok ? parsed.error : "";
  const first = ui?.nodes[0];
  return (
    <VisualFrame
      bad={error !== ""}
      copy={{ text: code, label: "Copy GenUI spec" }}
      detail={first?.type === "card" ? first.title : undefined}
      icon={LayoutDashboardIcon}
      kind="genui"
      note={isIncomplete ? "writing…" : error ? "error" : ui ? `${ui.count} ${ui.count === 1 ? "part" : "parts"}` : undefined}
      onView={setView}
      view={view}
      views={
        ui
          ? [
              { id: "view", label: "View" },
              { id: "source", label: "Source" },
            ]
          : []
      }
    >
      {error && <pre className="bakery-visual-error">{error}</pre>}
      {!ui || view === "source" ? (
        <SourceView code={code} lang="json" />
      ) : (
        <div className="bakery-genui">
          <GenuiParts nodes={ui.nodes} />
        </div>
      )}
    </VisualFrame>
  );
});

// ---- guard --------------------------------------------------------------------

const SOURCE_LANG: Record<string, string> = { chart: "json", genui: "json", svg: "xml" };

/** A visual that throws while it draws shows why over its exact source,
 *  and the reply and the panel around it stay up. The error still goes to
 *  the node log, as an uncaught one would (mount.tsx). */
class VisualBoundary extends Component<{ code: string; language: string; children: ReactNode }, { error: string | null }> {
  state: { error: string | null } = { error: null };
  static getDerivedStateFromError(e: unknown) {
    return { error: errorText(e) };
  }
  componentDidCatch(e: unknown) {
    reportError("react-visual", e);
  }
  render() {
    const { code, language, children } = this.props;
    if (this.state.error === null) return children;
    return (
      <VisualFrame
        bad
        copy={{ text: code, label: `Copy ${language} source` }}
        icon={TriangleAlertIcon}
        kind={language}
        note="error"
        onView={() => {}}
        view="source"
        views={[]}
      >
        <pre className="bakery-visual-error">{this.state.error}</pre>
        <SourceView code={code} lang={SOURCE_LANG[language] ?? language} />
      </VisualFrame>
    );
  }
}

function guarded(Draw: ComponentType<CustomRendererProps>): ComponentType<CustomRendererProps> {
  return function Guarded(props: CustomRendererProps) {
    return (
      <VisualBoundary code={props.code} language={props.language}>
        <Draw {...props} />
      </VisualBoundary>
    );
  };
}

/** Streamdown's custom renderers: each fence language a visual takes. */
export const VISUAL_RENDERERS: CustomRenderer[] = [
  { language: "mermaid", component: guarded(MermaidBlock) },
  { language: "chart", component: guarded(ChartBlock) },
  { language: ["html", "svg"], component: guarded(ArtifactBlock) },
  { language: "genui", component: guarded(GenuiBlock) },
];
