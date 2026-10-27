import {
  ARTIFACT_CSP,
  ARTIFACT_HEIGHT,
  artifactDoc,
  artifactTitle,
  drawingCache,
  fileName,
  labelEvery,
  MERMAID_FRAME_CSP,
  mermaidFrameDoc,
  mermaidKind,
  niceTicks,
  parseGenui,
  parseChart,
  pieAngles,
  sizedSvg,
  svgImageUrl,
  tickLabel,
  valueLabel,
  valueRange,
} from "./visual.ts";

let fails = 0;
function ok(name: string, cond: boolean, extra = "") {
  if (!cond) {
    fails++;
    console.log(`FAIL ${name} ${extra}`);
  } else console.log(`ok   ${name}`);
}

// ---- mermaid
ok("kind: first word", mermaidKind("flowchart LR\n  a --> b") === "flowchart");
ok("kind: past front matter and comments", mermaidKind("---\ntitle: Flow\n---\n%% note\nsequenceDiagram\n  a->>b: hi") === "sequenceDiagram");
ok("kind: none in an empty block", mermaidKind("  \n") === "");

const mermaidOut =
  '<svg id="m1" width="100%" xmlns="http://www.w3.org/2000/svg" style="max-width: 412.5px;" viewBox="-8 -8 412.5 230.25" role="graphics-document"><g/></svg>';
const sized = sizedSvg(mermaidOut);
ok("sized: from the viewBox, rounded up", sized?.width === 413 && sized?.height === 231, JSON.stringify(sized));
ok("sized: the stretch and max-width are gone", !!sized && !/width="100%"|style=/.test(sized.svg), sized?.svg);
ok("sized: everything else kept", !!sized?.svg.includes('role="graphics-document"') && !!sized?.svg.endsWith("<g/></svg>"));
ok("sized: xmlns added when missing", sizedSvg('<svg viewBox="0 0 10 10"></svg>')?.svg.startsWith('<svg width="10" height="10" xmlns="http://www.w3.org/2000/svg"') === true);
ok("sized: a `$` in the tag survives", sizedSvg('<svg aria-label="$& $1" viewBox="0 0 4 4"/>')?.svg.includes('aria-label="$& $1"') === true);
ok("sized: no viewBox is null", sizedSvg('<svg width="10" height="10"></svg>') === null);
ok("sized: an empty box is null", sizedSvg('<svg viewBox="0 0 0 10"></svg>') === null);
ok("image url: encoded whole", svgImageUrl('<svg a="#"/>') === "data:image/svg+xml;charset=utf-8,%3Csvg%20a%3D%22%23%22%2F%3E");
const frameDoc = mermaidFrameDoc("bakery://app/_app/immutable/assets/mermaid.min.js");
ok(
  "mermaid frame: the policy first, then mermaid",
  frameDoc.startsWith(`<!doctype html><meta http-equiv="Content-Security-Policy" content="${MERMAID_FRAME_CSP}">`) &&
    frameDoc.endsWith('<script src="bakery://app/_app/immutable/assets/mermaid.min.js"></script>'),
  frameDoc,
);
ok(
  "mermaid frame: mermaid's own script, inline styles and data: images, nothing from anywhere",
  MERMAID_FRAME_CSP === "default-src 'none'; script-src 'self'; style-src 'unsafe-inline'; img-src data:",
);
ok("mermaid frame: the script's URL stays in its attribute", mermaidFrameDoc('x" onload="y&').includes('src="x&#34; onload=&#34;y&#38;"'));

// ---- charts
const chart = (spec: unknown) => parseChart(JSON.stringify(spec));
const rows = chart({
  type: "line",
  unit: "ms",
  data: [
    { day: "Mon", api: 182, web: 240 },
    { day: "Tue", api: "171", web: null },
  ],
});
ok("rows: x guessed from its name, y from the numbers", rows.ok && rows.value.x === "day" && rows.value.series.map((s) => s.name).join() === "api,web", JSON.stringify(rows));
ok("rows: categories in order", rows.ok && rows.value.categories.join() === "Mon,Tue");
ok("rows: numeric strings read as numbers, null is a gap", rows.ok && rows.value.series[0].values.join() === "182,171" && rows.value.series[1].values[1] === null);
const picked = chart({ type: "bar", x: "team", y: "open", data: [{ team: "a", open: 3, closed: 9 }] });
ok("rows: x and y as given", picked.ok && picked.value.series.length === 1 && picked.value.series[0].name === "open" && picked.value.categories[0] === "a");
const years = chart({ type: "bar", data: [{ year: 2024, sales: 5 }, { year: 2025, sales: 8 }] });
ok("rows: a numeric year is the category, not a series", years.ok && years.value.categories.join() === "2024,2025" && years.value.series.length === 1);
const bare = chart({ type: "bar", title: "Builds", data: [3, 5, 2] });
ok("bare numbers: one series named by the title", bare.ok && bare.value.series[0].name === "Builds" && bare.value.categories.join() === "1,2,3");
const chartjs = chart({ type: "bar", labels: ["Q1", "Q2"], datasets: [{ label: "rev", data: [1, 2] }, { data: [4] }] });
ok("chart.js shape", chartjs.ok && chartjs.value.series[1].name === "series 2" && chartjs.value.series[1].values[1] === null, JSON.stringify(chartjs));
ok("type aliases", chart({ type: "Doughnut", data: [1, 2] }).ok && chart({ type: "column", data: [1] }).ok);
const err = (spec: unknown) => {
  const out = typeof spec === "string" ? parseChart(spec) : chart(spec);
  return out.ok ? "" : out.error;
};
ok("not JSON is an error that says so", err("{type: bar}").startsWith("not JSON"));
ok("a type every object inherits is no type", err({ type: "constructor", data: [1] }).includes("bar, line") && err({ type: "__proto__", data: [1] }).includes("bar, line"));
ok(
  "a toString in a spec is only a key",
  err('{"type": {"toString": 1}, "data": [1]}').includes("bar, line") &&
    chart({ type: "bar", data: [{ label: { toString: 1 }, v: 1 }] }).ok &&
    chart({ type: "bar", labels: [{ toString: 1 }], datasets: [{ data: [1] }] }).ok,
);
ok("an unknown type names the choices", err({ type: "radar", data: [1] }).includes("bar, line, area, pie, donut"));
ok("no data", err({ type: "bar", data: [] }) === "there is no data to plot");
ok("nothing numeric", err({ type: "bar", data: [{ a: "x" }] }) === "no numbers to plot");
ok("a missing y field is named", err({ type: "bar", y: ["nope"], data: [{ a: 1 }] }).includes('"nope"'));
ok("a missing x field is named", err({ type: "bar", x: "when", data: [{ a: 1 }] }).includes('"when"'));
ok("mixed rows are refused", err({ type: "bar", data: [{ a: 1 }, 2, "x"] }).includes("every row"));
ok("nine series are too many", err({ type: "line", labels: ["a"], datasets: Array.from({ length: 9 }, () => ({ data: [1] })) }).includes("at most 8"));
ok("a pie takes one series", err({ type: "pie", labels: ["a"], datasets: [{ data: [1] }, { data: [2] }] }).includes("one series"));
ok("a pie has no negatives", err({ type: "pie", data: [1, -2] }).includes("negative"));
const folded = chart({ type: "pie", data: Array.from({ length: 10 }, (_, i) => ({ label: `s${i}`, value: i + 1 })) });
ok(
  "a pie past eight slices folds the tail into Other",
  folded.ok && folded.value.categories.length === 8 && folded.value.categories[7] === "Other" && folded.value.series[0].values[7] === 8 + 9 + 10,
  JSON.stringify(folded),
);

const range = (spec: unknown) => {
  const out = chart(spec);
  return out.ok ? valueRange(out.value).join() : "";
};
ok("range: bars grow from zero", range({ type: "bar", data: [5, 9] }) === "0,9");
ok("range: a line keeps its own extent", range({ type: "line", data: [5, 9] }) === "5,9");
ok("range: negative bars reach zero from below", range({ type: "bar", data: [-4, -2] }) === "-4,0");
ok("range: a flat line still has height", range({ type: "line", data: [3, 3] }) === "0,3");
ok("range: all zero", range({ type: "bar", data: [0, 0] }) === "0,1");

const same = (a: number[], b: number[]) => a.join() === b.join();
const ticks = (lo: number, hi: number) => niceTicks(lo, hi).ticks;
ok("ticks: 0 to 9", same(ticks(0, 9), [0, 2, 4, 6, 8, 10]), ticks(0, 9).join());
ok("ticks: 0 to 1", same(ticks(0, 1), [0, 0.2, 0.4, 0.6, 0.8, 1]), ticks(0, 1).join());
ok("ticks: no float dust", same(ticks(0.3, 0.9), [0.2, 0.4, 0.6, 0.8, 1]), ticks(0.3, 0.9).join());
ok("ticks: across zero, zero among them", same(ticks(-4, 7), [-5, 0, 5, 10]), ticks(-4, 7).join());
ok("ticks: thousands", same(ticks(0, 4300), [0, 1000, 2000, 3000, 4000, 5000]), ticks(0, 4300).join());
const tiny = ticks(0, 2e-110);
ok("ticks: a range below 1e-100 keeps them apart", tiny.length === 5 && tiny.every((t, i) => i === 0 || t > tiny[i - 1]), tiny.join());
ok("ticks: steps past 2⁵³ still end", ticks(1e300, 1.0000000000000002e300).length <= 8);
ok("ticks: a step that underflows still ends", ticks(-5e-324, 5e-324).length <= 8);
const axis = (lo: number, hi: number) => {
  const { ticks, step } = niceTicks(lo, hi);
  return ticks.map((t) => tickLabel(t, step)).join(" ");
};
ok("tick labels: plain below a step of 1,000", axis(0, 9) === "0 2 4 6 8 10" && axis(0, 2400) === "0 500 1,000 1,500 2,000 2,500", axis(0, 2400));
ok("tick labels: compact from a step of 1,000", axis(0, 4300) === "0 1K 2K 3K 4K 5K" && axis(-2_400_000, 0) === "-2.5M -2M -1.5M -1M -500K 0", axis(-2_400_000, 0));
ok(
  "tick labels: no two gridlines alike",
  axis(1280, 1310) === "1,280 1,290 1,300 1,310" && axis(1_000_000, 1_010_000) === "1M 1.002M 1.004M 1.006M 1.008M 1.01M",
  axis(1_000_000, 1_010_000),
);
ok("tick labels: to the step's last digit", axis(0.004, 0.0045) === "0.004 0.0041 0.0042 0.0043 0.0044 0.0045" && axis(0, 4.3e-7) === "0 1E-7 2E-7 3E-7 4E-7 5E-7", axis(0, 4.3e-7));
ok("value labels in full, with the unit", valueLabel(1284.5, "ms") === "1,284.5 ms" && valueLabel(12, "%") === "12%" && valueLabel(null, "") === "—");
ok(
  "value labels: six decimals or six digits, whichever keeps more",
  valueLabel(0.0042, "") === "0.0042" && valueLabel(0.000001234, "s") === "0.000001234 s" && valueLabel(1_234_567, "") === "1,234,567" && valueLabel(33.333333333333336, "%") === "33.333333%",
);
ok("labels: every one when they fit", labelEvery(10, 80, 6) === 1);
ok("labels: thinned when crowded", labelEvery(60, 12, 5) === 4, String(labelEvery(60, 12, 5)));
ok("labels: still apart at under a pixel a point", labelEvery(2000, 0.325, 3) === 90, String(labelEvery(2000, 0.325, 3)));
const angles = pieAngles([1, null, 3]);
ok("pie: full circle, gaps for nothing", Math.abs(angles[2][1] - Math.PI * 2) < 1e-9 && angles[1][0] === angles[1][1] && Math.abs(angles[0][1] - Math.PI / 2) < 1e-9);

// ---- artifacts
const page = '<!DOCTYPE html>\n<html lang="en"><head><title> Sprint   board </title><script>alert(1)</script></head><body>hi</body></html>';
const doc = artifactDoc("html", page);
ok("doc: one doctype, ours, first", doc.startsWith("<!doctype html>") && (doc.match(/<!doctype/gi) ?? []).length === 1, doc.slice(0, 40));
ok("doc: the policy comes before anything of the artifact's", doc.indexOf("Content-Security-Policy") < doc.indexOf("alert(1)") && doc.indexOf("Content-Security-Policy") < doc.indexOf("<html"));
ok("doc: the artifact itself is kept whole", doc.endsWith(page.replace("<!DOCTYPE html>", "")));
ok("doc: DNS prefetch off before anything of the artifact's", doc.indexOf('<meta http-equiv="x-dns-prefetch-control" content="off">') > 0 && doc.indexOf("x-dns-prefetch-control") < doc.indexOf("alert(1)"));
ok("doc: the height reporter is there", doc.includes(`postMessage({${ARTIFACT_HEIGHT}:`));
ok("doc: it reports the document's height, which can be less than the frame's", doc.includes("documentElement.getBoundingClientRect().height") && !doc.includes("scrollHeight"));
const snippet = artifactDoc("html", "<div>card</div>");
ok("doc: a snippet gets the same head", snippet.startsWith("<!doctype html><meta charset") && snippet.endsWith("<div>card</div>"));
const svg = artifactDoc("svg", '<svg viewBox="0 0 10 10"><circle r="4"/></svg>');
ok("doc: an svg sits in a page body of its own", svg.includes('<body><svg viewBox="0 0 10 10"><circle r="4"/></svg></body>') && svg.indexOf("Content-Security-Policy") < svg.indexOf("<svg"));
ok("csp: off the network", ARTIFACT_CSP.includes("connect-src 'none'") && ARTIFACT_CSP.includes("form-action 'none'") && ARTIFACT_CSP.startsWith("default-src 'none'"));
ok("csp: no double quote to break out of the attribute", !ARTIFACT_CSP.includes('"'));
ok(
  "csp: images from the page itself, the rest from the three CDNs only",
  ARTIFACT_CSP.includes("img-src data: blob:;") &&
    !/(?:^|\s)(?:https?:|\*)(?=;|\s|$)/.test(ARTIFACT_CSP) &&
    (ARTIFACT_CSP.match(/https:\/\/[^\s;]+/g) ?? []).every((host) => ["https://cdnjs.cloudflare.com", "https://cdn.jsdelivr.net", "https://unpkg.com"].includes(host)),
);
ok("title: trimmed, spaces folded", artifactTitle(page) === "Sprint board");
ok("title: none", artifactTitle("<div/>") === "");
ok("title: the first, in any case", artifactTitle("<TITLE lang=en>A</Title><title>B</title>") === "A");
ok("title: unclosed ones read in one pass", artifactTitle("<title>".repeat(50_000)) === "");
ok("file name: from the title", fileName("Sprint board", "html") === "sprint-board.html");
ok("file name: nothing usable", fileName(" ¿? ", "svg") === "artifact.svg");
ok("file name: long titles cut clean", fileName("a ".repeat(50), "html").length <= 65 && !fileName("a ".repeat(50), "html").includes("-.html"));

// ---- GenUI
const ui = (spec: unknown) => parseGenui(JSON.stringify(spec));
const card = ui({
  type: "card",
  title: "Release 0.9",
  tone: "ok",
  children: [
    { type: "stat", label: "Tests", value: 1837, delta: "+12", tone: "ok" },
    { type: "progress", label: "Coverage", value: "72" },
    { type: "kv", items: { Branch: "main", Clean: true } },
    { type: "list", items: ["Notes", { text: "Deploy", tone: "warn", detail: "waits on CI" }, 3] },
    { type: "table", rows: [{ crate: "node", tests: 412 }, { crate: "exec", skipped: 4 }] },
    { type: "key-value", items: [["a", 1], { key: "b", value: "two" }] },
  ],
});
const kids = card.ok && card.value.nodes[0].type === "card" ? card.value.nodes[0].children : [];
ok("genui: a card and its six children, counted", card.ok && card.value.count === 7 && kids.length === 6, JSON.stringify(card));
ok("genui: stat is a metric, its number formatted", kids[0]?.type === "metric" && kids[0].value === "1,837" && kids[0].tone === "ok");
ok("genui: a small number keeps its digits", (() => { const m = ui({ type: "metric", value: 0.0042 }); return m.ok && m.value.nodes[0].type === "metric" && m.value.nodes[0].value === "0.0042"; })());
ok("genui: progress reads a numeric string, out of 100", kids[1]?.type === "progress" && kids[1].value === 72 && kids[1].max === 100);
ok("genui: kv from an object, booleans in words", kids[2]?.type === "kv" && kids[2].items.map((i) => `${i.key}=${i.value}`).join() === "Branch=main,Clean=yes");
ok(
  "genui: list items from strings, objects and numbers",
  kids[3]?.type === "list" && kids[3].items.map((i) => i.text).join() === "Notes,Deploy,3" && kids[3].items[1].tone === "warn" && kids[3].items[1].detail === "waits on CI",
);
ok(
  "genui: table rows from objects take every key, blanks where missing",
  kids[4]?.type === "table" && kids[4].columns.join() === "crate,tests,skipped" && kids[4].rows[1].join("|") === "exec||4",
  JSON.stringify(kids[4]),
);
ok("genui: kv from pairs and objects", kids[5]?.type === "kv" && kids[5].items.map((i) => `${i.key}=${i.value}`).join() === "a=1,b=two");
const list = ui([{ type: "metric", label: "a", value: "1" }, { type: "chart" }, { type: "table", columns: ["x"], rows: [["1", { no: 1 }]] }]);
ok("genui: a list at the root", list.ok && list.value.nodes.length === 3);
ok("genui: an unknown type stays, named", list.ok && list.value.nodes[1].type === "unknown" && list.value.nodes[1].name === "chart");
ok("genui: an object in a cell is no text", list.ok && list.value.nodes[2].type === "table" && list.value.nodes[2].rows[0].join("|") === "1|");
ok("genui: a type every object inherits is unknown", (() => { const c = ui({ type: "constructor" }); return c.ok && c.value.nodes[0].type === "unknown" && c.value.nodes[0].name === "constructor"; })());
ok("genui: a toString type is unknown, not a throw", (() => { const t = parseGenui('{"type": {"toString": 1}}'); return t.ok && t.value.nodes[0].type === "unknown"; })());
ok(
  "genui: a list, a table and key-values take a tone",
  (() => {
    const t = ui([{ type: "list", tone: "warn", items: ["a"] }, { type: "table", tone: "bad", rows: [["1"]] }, { type: "kv", tone: "info", items: { a: 1 } }]);
    return t.ok && t.value.nodes.map((n) => ("tone" in n ? n.tone : "")).join() === "warn,bad,info";
  })(),
);
ok("genui: an unknown tone is none", (() => { const m = ui({ type: "metric", tone: "red" }); return m.ok && m.value.nodes[0].type === "metric" && m.value.nodes[0].tone === null; })());
const uiErr = (spec: unknown) => {
  const out = typeof spec === "string" ? parseGenui(spec) : ui(spec);
  return out.ok ? "" : out.error;
};
ok("genui: not JSON", uiErr("{card}").startsWith("not JSON"));
ok("genui: a part that is not an object, by its path", uiErr({ type: "card", children: [1] }) === '$.children[0]: not an object with a "type"');
ok("genui: progress without a number", uiErr({ type: "progress", value: "lots" }).includes('"value" is not a number'));
ok("genui: progress with a bad max", uiErr({ type: "progress", value: 1, max: 0 }).includes('"max"'));
ok("genui: list items must be a list", uiErr({ type: "list", items: "a" }) === "$.items: not a list");
const deep = (n: number): unknown => (n === 0 ? { type: "metric" } : { type: "card", children: [deep(n - 1)] });
ok("genui: six deep is fine", uiErr(deep(5)) === "");
ok("genui: seven deep is refused", uiErr(deep(6)).includes("nested more than 6"));
ok("genui: at most 400 parts", uiErr(Array.from({ length: 401 }, () => ({ type: "metric" }))).includes("400"));
ok("genui: long text is cut", (() => { const m = ui({ type: "metric", label: "x".repeat(5000) }); return m.ok && m.value.nodes[0].type === "metric" && m.value.nodes[0].label.length === 4000; })());

{
  // A diagram is drawn once; shown again, the same picture, nothing drawn.
  const cache = drawingCache<string>(2);
  let draws = 0;
  const make = (v: string) => () => {
    draws++;
    return Promise.resolve(v);
  };
  const first = cache.draw("a", make("A"));
  const again = cache.draw("a", make("A"));
  ok("drawing: drawn once for the same key", first === again && draws === 1);
  ok("drawing: nothing to peek before it lands", cache.peek("a") === undefined);
  await first;
  ok("drawing: drawn, it is there at once", cache.peek("a") === "A");
  await cache.draw("b", make("B"));
  await cache.draw("a", make("A"));
  await cache.draw("c", make("C"));
  ok("drawing: only the newest are kept", cache.peek("b") === undefined && cache.peek("a") === "A" && cache.peek("c") === "C");
  ok("drawing: one a view used is kept as newest", draws === 3);
  let tries = 0;
  const fail = () => {
    tries++;
    return Promise.reject(new Error("no"));
  };
  await cache.draw("bad", fail).catch(() => {});
  await cache.draw("bad", fail).catch(() => {});
  ok("drawing: a failed draw is tried again", tries === 2 && cache.peek("bad") === undefined);
}

console.log(fails === 0 ? "\nALL PASS" : `\n${fails} FAILURES`);
if (fails > 0) throw new Error(`${fails} visual check failure(s)`);
