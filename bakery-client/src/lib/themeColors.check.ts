import { colorToHex, cssColorToHex, isLightColor } from "./themeColors.ts";

let failures = 0;
function check(label: string, actual: unknown, expected: unknown) {
  if (actual !== expected) {
    failures++;
    console.error(`FAIL ${label}: ${String(actual)} !== ${String(expected)}`);
  } else console.log(`ok   ${label}`);
}

for (const [input, expected] of [
  ["#f03", "#ff0033"],
  ["#f038", "#ff003388"],
  ["#A1B2C3", "#a1b2c3"],
  ["#A1B2C344", "#a1b2c344"],
  ["rgb(255, 0, 51)", "#ff0033"],
  ["rgba(255, 0, 51, .5)", "#ff003380"],
  ["rgba(100%, 0%, 20%, 25%)", "#ff003340"],
  ["rgba(91,140,255,.14)", "#5b8cff24"],
  ["hsl(0, 100%, 50%)", "#ff0000"],
  ["hsl(120, 100%, 50%)", "#00ff00"],
  ["hsl(240, 100%, 50%)", "#0000ff"],
  ["hsl(-60, 100%, 50%)", "#ff00ff"],
  ["hsl(420deg, 100%, 50%)", "#ffff00"],
  ["hsla(180, 100%, 50%, 40%)", "#00ffff66"],
  ["hsl(0, 0%, 25%)", "#404040"],
  ["hsl(0, 100%, 100%)", "#ffffff"],
  ["hsl(0, 100%, 0%)", "#000000"],
]) {
  check(`renderer color ${input}`, cssColorToHex(input), expected);
}
check("native picker discards alpha only", colorToHex("hsla(240, 100%, 50%, .2)"), "#0000ff");
check("syntax rule expands short alpha hex", colorToHex("#abc8"), "#aabbcc");
check("default dark renderer", isLightColor("#0d0e11"), false);
check("light renderer", isLightColor("#fafafa"), true);
check("custom RGB light renderer", isLightColor("rgb(240, 240, 240)"), true);
check("custom HSL dark renderer", isLightColor("hsl(210, 20%, 15%)"), false);
check("luminance accounts for color channels", isLightColor("#00ff00"), true);
check("reject invalid color syntax", cssColorToHex("url(https://example.com)"), "#000000");
check("reject arbitrary css", cssColorToHex("var(--something)"), "#000000");
check("reject missing RGB channel", cssColorToHex("rgb(255, 0)"), "#000000");
check("reject invalid HSL channel", cssColorToHex("hsl(0, xx%, 50%)"), "#000000");
check("reject overflow hue", cssColorToHex(`hsl(${'9'.repeat(400)}, 100%, 50%)`), "#000000");

if (failures) throw new Error(`${failures} theme color checks failed`);
console.log("Theme color checks passed");
