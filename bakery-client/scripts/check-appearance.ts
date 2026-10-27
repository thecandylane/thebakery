import assert from "node:assert/strict";
import { runInNewContext } from "node:vm";
import { readFileSync } from "node:fs";
import { appearanceCodec as codec, appearanceBootstrapScript, appearanceFromNode, APPEARANCE_KEY, BUILTIN_THEMES, CUSTOM_TOKEN_KEYS } from "../src/lib/themeModel.ts";

let checks = 0;
function check(name: string, body: () => void) { body(); checks++; console.log(`✓ ${name}`); }
function storage(items: Record<string, string> = {}) { return { getItem: (key: string) => items[key] ?? null }; }
const scheme = { id: "custom:test", name: "My colors", base: "light", tokens: { "--accent": "#123abc", "--accent-bg": "rgba(18,58,188,.15)" } };
const state = { version: 1, selected: scheme.id, schemes: [scheme] };

check("default is original dark palette", () => assert.deepEqual(codec.read(storage()), codec.empty()));
for (const preset of BUILTIN_THEMES) check(`legacy preset ${preset} retained`, () => assert.equal(codec.read(storage({ "bakery.theme": preset })).selected, preset));
check("legacy custom tokens migrate with stable identity and filtering", () => {
  const legacy = codec.read(storage({ "bakery.theme": "custom", "bakery.theme.custom": JSON.stringify({ "--accent": "#abc", "--evil": "url(https://example.invalid)", "--fg": "var(--secret)" }) }));
  assert.equal(legacy.selected, "custom:legacy");
  assert.deepEqual(legacy.schemes[0].tokens, { "--accent": "#abc" });
});
check("unselected legacy custom colors are retained", () => assert.equal(codec.read(storage({ "bakery.theme": "dim", "bakery.theme.custom": '{"--fg":"#fff"}' })).schemes.length, 1));
check("v1 wins over legacy keys", () => assert.deepEqual(codec.read(storage({ [APPEARANCE_KEY]: JSON.stringify(state), "bakery.theme": "dark" })), state));
check("unknown selection falls back to Auto without deleting schemes", () => assert.deepEqual(codec.decode(JSON.stringify({ ...state, selected: "missing" })), { ...state, selected: "auto" }));
check("empty storage defaults to Auto", () => assert.equal(codec.read(storage()).selected, "auto"));
check("duplicate identities are rejected", () => assert.throws(() => codec.decode(JSON.stringify({ ...state, schemes: [scheme, scheme] })), /Duplicate/));
check("future library format is not overwritten or migrated", () => assert.throws(() => codec.read(storage({ [APPEARANCE_KEY]: '{"version":2,"schemes":[]}' })), /unsupported/));
check("malformed library is rejected", () => assert.throws(() => codec.read(storage({ [APPEARANCE_KEY]: "{" }))));
check("malformed legacy colors preserve selected preset", () => assert.equal(codec.read(storage({ "bakery.theme": "light", "bakery.theme.custom": "{" })).selected, "light"));
check("scheme limits enforced", () => {
  assert.throws(() => codec.scheme({ ...scheme, name: " " }));
  assert.throws(() => codec.scheme({ ...scheme, name: "a".repeat(81) }));
  assert.throws(() => codec.scheme({ ...scheme, base: "unsupported" }));
  assert.throws(() => codec.decode(JSON.stringify({ ...state, schemes: Array.from({ length: 101 }, (_, i) => ({ ...scheme, id: `custom:${i}` })) })));
});
check("CSS is restricted to literal colors", () => {
  for (const invalid of ["red", "url(https://example.invalid)", "var(--fg)", "#fff;display:none", "inherit", "rgb(999,0,0)", "rgb(100%,0,0)", "rgba(0,0,0,2)", "hsl(0,101%,0%)", "#12", "rgb(NaN,0,0)"]) assert.equal(codec.isColor(invalid), false, invalid);
  for (const valid of ["#fff", "#1234", "#aAbBcC", "#12345678", "rgba(10, 20,30,.25)", "rgb(10%,20%,30%)", "hsl(270, 60%, 50%)", "hsla(270,60%,50%,25%)"]) assert.equal(codec.isColor(valid), true, valid);
  assert.throws(() => codec.tokens({ "--bg-app": "#123", "--position": "#fff" }, true));
  assert.throws(() => codec.tokens([], true));
});
check("every exposed color has a valid preset default", () => {
  const css = readFileSync(new URL("../src/lib/styles/theme.css", import.meta.url), "utf8");
  const defaults = css.slice(css.indexOf(":root"), css.indexOf("/* ---- dim:"));
  for (const key of CUSTOM_TOKEN_KEYS) {
    const value = defaults.match(new RegExp(`${key}:\\s*([^;]+);`))?.[1];
    assert.ok(value && codec.isColor(value), `${key}: ${value}`);
  }
});
function prepaint(items: Record<string, string>, macLight = false) {
  const attrs: Record<string, string> = {};
  const styles: Record<string, string> = {};
  runInNewContext(appearanceBootstrapScript(), { localStorage: storage(items), matchMedia: () => ({ matches: macLight }), document: { documentElement: { setAttribute: (k: string, v: string) => { attrs[k] = v; }, style: { setProperty: (k: string, v: string) => { styles[k] = v; } } } } });
  return { attrs, styles };
}
for (const preset of BUILTIN_THEMES) check(`prepaint applies ${preset} before hydration`, () => {
  const { attrs } = prepaint({ "bakery.theme": preset });
  assert.equal(attrs["data-theme"], preset === "dark" ? undefined : preset);
});
const paired = {
  id: "custom:paired", name: "Paired", base: "light", tokens: { "--bg-app": "#ffffff" },
  dark: { base: "dark", tokens: { "--bg-app": "#101010" } },
  extras: { "--font-ui": "Montserrat, sans-serif", "--r-lg": "5.6px", "--shadow-chip": "0px 1px 2px 0px rgba(0, 0, 0, 0.1)" },
};
const pairedState = { version: 1, selected: paired.id, schemes: [paired] };
check("a two-variant scheme with fonts, radii and shadows round-trips", () => assert.deepEqual(codec.decode(JSON.stringify(pairedState)), pairedState));
check("extras refuse anything but plain values", () => {
  for (const extras of [{ "--font-ui": "x; background: url(a)" }, { "--r-lg": "calc(1px)" }, { "--shadow-chip": "0 0 var(--x)" }, { "--position": "1px" }]) {
    assert.throws(() => codec.scheme({ ...paired, extras }));
  }
});
check("prepaint: a two-variant scheme follows the Mac and applies its extras", () => {
  const dark = prepaint({ [APPEARANCE_KEY]: JSON.stringify(pairedState) });
  assert.equal(dark.attrs["data-theme"], undefined);
  assert.equal(dark.styles["--bg-app"], "#101010");
  assert.equal(dark.styles["--font-ui"], "Montserrat, sans-serif");
  const light = prepaint({ [APPEARANCE_KEY]: JSON.stringify(pairedState) }, true);
  assert.equal(light.attrs["data-theme"], "light");
  assert.equal(light.styles["--bg-app"], "#ffffff");
});
check("an appearance from the node passes the codec or is dropped whole", () => {
  assert.deepEqual(appearanceFromNode(pairedState), pairedState);
  assert.equal(appearanceFromNode(null), null);
  assert.equal(appearanceFromNode({ version: 2, selected: "dark", schemes: [] }), null);
  assert.equal(appearanceFromNode({ ...pairedState, schemes: [{ ...paired, name: "x url(https://evil.example)" }] }), null);
  assert.equal(appearanceFromNode({ ...pairedState, schemes: [{ ...paired, name: "javascript:alert(1)" }] }), null);
  assert.equal(appearanceFromNode({ ...pairedState, schemes: [{ ...paired, tokens: { "--bg-app": "expression(1)" } }] }), null);
  assert.equal(appearanceFromNode({ ...pairedState, schemes: [{ ...paired, extras: { "--font-ui": "a; } body { color: red" } }] }), null);
});
check("prepaint: Auto is Warm when the Mac is dark", () => {
  const { attrs } = prepaint({});
  assert.equal(attrs["data-theme"], "warm");
  assert.equal(attrs["data-color-mode"], "dark");
});
check("prepaint: Auto is Parchment, in light mode, when the Mac is light", () => {
  const { attrs } = prepaint({}, true);
  assert.equal(attrs["data-theme"], "parchment");
  assert.equal(attrs["data-color-mode"], "light");
});
check("prepaint restores custom base and every override", () => {
  const { attrs, styles } = prepaint({ [APPEARANCE_KEY]: JSON.stringify(state) });
  assert.equal(attrs["data-theme"], "light");
  assert.equal(attrs["data-color-scheme"], scheme.id);
  assert.deepEqual(styles, scheme.tokens);
});
check("prepaint rejects unsafe/corrupt data without executing it", () => {
  assert.deepEqual(prepaint({ [APPEARANCE_KEY]: "{" }), { attrs: {}, styles: {} });
  assert.deepEqual(prepaint({ [APPEARANCE_KEY]: JSON.stringify({ ...state, schemes: [{ ...scheme, tokens: { "--fg": "url(https://example.invalid)" } }] }) }), { attrs: {}, styles: {} });
});
console.log(`${checks} appearance schema/bootstrap checks passed`);
