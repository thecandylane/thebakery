import { test, expect } from "./fixtures";
import type { Page } from "@playwright/test";
import { readFile } from "node:fs/promises";

const savedState = (page: Page) => page.evaluate(() => JSON.parse(localStorage.getItem("bakery.appearance.v1") ?? "null"));
const rootColor = (page: Page, key: string) => page.evaluate((token) => getComputedStyle(document.documentElement).getPropertyValue(token).trim(), key);
async function create(page: Page, name: string, accent = "#d34477") {
  await page.getByRole("button", { name: "Create scheme", exact: true }).click();
  await page.getByLabel("Scheme name", { exact: true }).fill(name);
  await page.getByLabel("Accent color", { exact: true }).fill(accent);
  await page.getByRole("button", { name: "Apply to accent family" }).click();
  await page.getByRole("button", { name: "Save scheme", exact: true }).click();
  await expect(page.getByRole("button", { name: `Edit ${name}`, exact: true })).toBeVisible();
}

test("original palette is restored and all seven preset cards have real colors", async ({ page }) => {
  await page.goto("/settings/appearance");
  await expect(page.getByRole("heading", { name: "Appearance", exact: true })).toBeVisible();
  expect(await rootColor(page, "--bg-app")).toBe("#0d0e11");
  expect(await rootColor(page, "--accent")).toBe("#5b8cff");
  await page.getByRole("radio", { name: "Dark", exact: true }).focus();
  await page.keyboard.press("ArrowRight");
  await expect(page.getByRole("radio", { name: "Dim", exact: true })).toBeChecked();
  expect(await rootColor(page, "--bg-app")).not.toBe("#0d0e11");
  const canvases: string[] = [];
  for (const name of ["Dark", "Dim", "Light", "Monokai", "Solarized Dark", "Solarized Light", "High Contrast"]) {
    await page.getByRole("radio", { name, exact: true }).check();
    canvases.push(await rootColor(page, "--bg-app"));
    await expect(page.getByRole("radio", { name, exact: true })).toBeChecked();
    const sample = page.locator(".scheme-card").filter({ has: page.getByRole("radio", { name, exact: true }) }).locator(".scheme-preview");
    const previewCanvas = await sample.evaluate((element) => getComputedStyle(element).getPropertyValue("--bg-app").trim());
    expect(previewCanvas).toBe(canvases.at(-1));
  }
  expect(new Set(canvases).size).toBe(7);
  await page.reload();
  await expect(page.getByRole("radio", { name: "High Contrast", exact: true })).toBeChecked();
});

test("custom color editing previews, validates, saves, survives reload, and cancels without changing saved colors", async ({ page }) => {
  await page.goto("/settings/appearance");
  await page.getByRole("button", { name: "Create scheme", exact: true }).click();
  await page.getByLabel("Scheme name", { exact: true }).fill("Orchid workspace");
  await page.getByLabel("Accent color", { exact: true }).fill("#b455da");
  await page.getByRole("button", { name: "Apply to accent family" }).click();
  expect(await rootColor(page, "--accent-solid")).toBe("#b455da");
  expect(await savedState(page)).toBeNull();
  await expect(page.locator("html")).toHaveAttribute("data-theme-preview", "");
  await page.getByLabel("Accent tint", { exact: true }).fill("rgba(180,85,218,.21)");
  await page.getByLabel("Workspace", { exact: true }).fill("rgb(100%,0,0)");
  await expect(page.getByLabel("Workspace", { exact: true })).toHaveAttribute("aria-invalid", "true");
  await expect(page.getByRole("button", { name: "Save scheme", exact: true })).toBeDisabled();
  expect(await rootColor(page, "--bg-app")).toBe("#0d0e11");
  await page.getByRole("button", { name: "Reset Workspace", exact: true }).click();
  await page.getByLabel("Base palette").selectOption("light");
  expect(await rootColor(page, "--accent")).toBe("#b455da");
  await page.getByRole("button", { name: "Save scheme", exact: true }).click();
  const state = await savedState(page);
  expect(state.schemes).toHaveLength(1);
  expect(state.schemes[0]).toMatchObject({ name: "Orchid workspace", base: "light", tokens: { "--accent": "#b455da", "--accent-bg": "rgba(180,85,218,.21)" } });
  await page.reload();
  await expect(page.getByRole("radio", { name: "Orchid workspace", exact: true })).toBeChecked();
  expect(await rootColor(page, "--accent")).toBe("#b455da");
  await page.getByRole("button", { name: "Edit Orchid workspace", exact: true }).click();
  await page.getByLabel("Accent color", { exact: true }).fill("#12ab34");
  expect(await rootColor(page, "--accent")).toBe("#12ab34");
  await page.getByRole("checkbox", { name: "Preview in Studio" }).uncheck();
  expect(await rootColor(page, "--accent")).toBe("#b455da");
  await page.getByRole("button", { name: "Cancel editing" }).click();
  expect(await savedState(page)).toEqual(state);
  await expect(page.locator("html")).not.toHaveAttribute("data-theme-preview");
});

test("schemes duplicate, rename, reset, export/import, and delete independently", async ({ page }) => {
  await page.goto("/settings/appearance");
  await create(page, "Rose");
  await page.getByRole("button", { name: "Duplicate Rose", exact: true }).click();
  await page.getByLabel("Scheme name", { exact: true }).fill("Ocean");
  await page.getByLabel("Accent color", { exact: true }).fill("#148ac4");
  await page.getByRole("button", { name: "Save scheme", exact: true }).click();
  let state = await savedState(page);
  expect(state.schemes).toHaveLength(2);
  expect(state.schemes[0].tokens["--accent"]).toBe("#d34477");
  expect(state.schemes[1].id).not.toBe(state.schemes[0].id);

  await page.getByRole("button", { name: "Edit Ocean", exact: true }).click();
  await page.getByLabel("Scheme name", { exact: true }).fill("Blue hour");
  await page.getByRole("button", { name: "Reset all colors", exact: true }).click();
  await page.getByRole("button", { name: "Save scheme", exact: true }).click();
  state = await savedState(page);
  expect(state.schemes[1].tokens).toEqual({});
  const downloadEvent = page.waitForEvent("download");
  await page.getByRole("button", { name: "Export Rose", exact: true }).click();
  const download = await downloadEvent;
  const json = await readFile((await download.path())!, "utf8");
  expect(JSON.parse(json)).toMatchObject({ format: "bakery-color-scheme", name: "Rose" });
  await page.getByLabel("Import color scheme file").setInputFiles({ name: "rose.json", mimeType: "application/json", buffer: Buffer.from(json) });
  await expect.poll(async () => (await savedState(page)).schemes.length).toBe(3);
  state = await savedState(page);
  expect(state.schemes[2].id).not.toBe(state.schemes[0].id);
  expect(state.schemes[2].tokens).toEqual(state.schemes[0].tokens);
  await page.getByRole("button", { name: "Delete Blue hour", exact: true }).click();
  await page.getByRole("button", { name: "Keep scheme", exact: true }).click();
  expect((await savedState(page)).schemes).toHaveLength(3);
  await page.getByRole("radio", { name: "Blue hour", exact: true }).check();
  await page.getByRole("button", { name: "Delete Blue hour", exact: true }).click();
  await page.getByRole("button", { name: "Delete scheme", exact: true }).click();
  expect((await savedState(page)).schemes).toHaveLength(2);
  expect((await savedState(page)).selected).toBe("dark");
  expect(await rootColor(page, "--accent")).toBe("#5b8cff");
});

test("invalid imports and failed storage writes preserve the saved library", async ({ page }) => {
  await page.goto("/settings/appearance");
  await create(page, "Keep me");
  const before = await savedState(page);
  const invalid = { format: "bakery-color-scheme", version: 1, name: "Invalid", base: "dark", tokens: { "--accent": "url(https://example.invalid)" } };
  await page.getByLabel("Import color scheme file").setInputFiles({ name: "invalid.json", mimeType: "application/json", buffer: Buffer.from(JSON.stringify(invalid)) });
  await expect(page.getByRole("alert")).toContainText("Invalid color");
  expect(await savedState(page)).toEqual(before);
  await page.evaluate(() => {
    const original = Storage.prototype.setItem;
    Storage.prototype.setItem = function(key, value) {
      if (key === "bakery.appearance.v1") throw new DOMException("Test storage full", "QuotaExceededError");
      return original.call(this, key, value);
    };
  });
  await page.getByRole("radio", { name: "Light", exact: true }).click();
  await expect(page.getByRole("alert")).toContainText("could not be saved");
  expect(await savedState(page)).toEqual(before);
  expect(await rootColor(page, "--accent")).toBe("#d34477");
  await expect(page.getByRole("radio", { name: "Keep me", exact: true })).toBeChecked();
});

test("saved selections synchronize between windows and the settings dial", async ({ page, context }) => {
  await page.goto("/settings/appearance");
  const other = await context.newPage();
  await other.goto("/");
  await create(page, "Across Studio", "#23ab89");
  await expect(other.locator(".theme-chip")).toHaveText("Across Studio");
  expect(await rootColor(other, "--accent")).toBe("#23ab89");
  await other.locator(".theme-chip").click();
  await expect(page.getByRole("radio", { name: "Dark", exact: true })).toBeChecked();
  expect((await savedState(page)).schemes).toHaveLength(1);
  await page.getByRole("button", { name: "Edit Across Studio", exact: true }).click();
  await page.getByLabel("Accent color", { exact: true }).fill("#e34578");
  await expect(page.getByRole("checkbox", { name: "Preview in Studio" })).toBeChecked();
  await other.locator(".theme-chip").click(); // dark -> dim, wins over temporary preview
  await expect(page.getByRole("checkbox", { name: "Preview in Studio" })).not.toBeChecked();
  await expect(page.getByLabel("Accent color", { exact: true })).toHaveValue("#e34578");
  await expect(page.locator("html")).toHaveAttribute("data-color-scheme", "dim");
  await page.getByRole("button", { name: "Cancel editing" }).click();
  await expect(page.locator("html")).toHaveAttribute("data-color-scheme", "dim");
});

test("Appearance stays usable on narrow windows with no horizontal overflow", async ({ page }, testInfo) => {
  await page.setViewportSize({ width: 760, height: 920 });
  await page.goto("/settings/appearance");
  await expect(page.getByRole("heading", { name: "Appearance", exact: true })).toBeVisible();
  await page.screenshot({ path: testInfo.outputPath("appearance-narrow.png"), fullPage: true });
  await page.getByRole("button", { name: "Create scheme", exact: true }).click();
  await page.getByLabel("Scheme name", { exact: true }).fill("Narrow test");
  await page.getByLabel("Workspace", { exact: true }).fill("#18202a");
  await page.getByLabel("Accent color", { exact: true }).fill("#55aacc");
  // What would scroll sideways: the Settings scroll area and the page. (The
  // colour-group headings' hover highlight deliberately bleeds 8px into the
  // panel's own padding, so the panel's inner box is not the measure.)
  const overflow = await page.evaluate(() =>
    [document.querySelector(".settings-main")!, document.documentElement].map(
      (element) => element.scrollWidth - element.clientWidth,
    ),
  );
  expect(overflow).toEqual([0, 0]);
  await page.getByLabel("Scheme name", { exact: true }).scrollIntoViewIfNeeded();
  await page.screenshot({ path: testInfo.outputPath("appearance-editor-narrow.png"), fullPage: true });
  await page.getByRole("button", { name: "Save scheme", exact: true }).click();
  await expect(page.getByRole("radio", { name: "Narrow test", exact: true })).toBeChecked();
});

test("Escape and navigation restore saved colors without saving a draft", async ({ page }) => {
  await page.goto("/settings/appearance");
  await page.getByRole("button", { name: "Create scheme", exact: true }).click();
  await page.getByLabel("Accent color", { exact: true }).fill("#123abc");
  await page.keyboard.press("Escape");
  await expect(page.getByRole("checkbox", { name: "Preview in Studio" })).not.toBeChecked();
  expect(await rootColor(page, "--accent")).toBe("#5b8cff");
  await expect(page.getByLabel("Accent color", { exact: true })).toHaveValue("#123abc");
  await page.getByRole("checkbox", { name: "Preview in Studio" }).check();
  page.once("dialog", (dialog) => dialog.accept());
  await page.getByRole("link", { name: "Approvals", exact: true }).click();
  await expect(page).toHaveURL(/\/settings\/approvals$/);
  expect(await rootColor(page, "--accent")).toBe("#5b8cff");
  expect(await savedState(page)).toBeNull();
});

test("corrupt libraries are preserved for backup and reset only after confirmation", async ({ page }) => {
  await page.goto("/");
  await page.evaluate(() => localStorage.setItem("bakery.appearance.v1", '{"version":999,"retain":"my data"}'));
  await page.goto("/settings/appearance");
  await expect(page.getByRole("alert")).toContainText("could not be read");
  const downloadEvent = page.waitForEvent("download");
  await page.getByRole("button", { name: "Download saved library backup" }).click();
  const file = await downloadEvent;
  expect(await readFile((await file.path())!, "utf8")).toBe('{"version":999,"retain":"my data"}');
  await page.getByRole("button", { name: "Reset color library", exact: true }).click();
  await page.getByRole("button", { name: "Keep library", exact: true }).click();
  expect(await savedState(page)).toEqual({ version: 999, retain: "my data" });
  await page.getByRole("button", { name: "Reset color library", exact: true }).click();
  await page.getByRole("button", { name: "Reset library", exact: true }).click();
  expect(await savedState(page)).toEqual({ version: 1, selected: "dark", schemes: [] });
  await expect(page.getByRole("alert")).toHaveCount(0);
  await create(page, "Recovered");
});

test("a scheme deleted in another window cannot be resurrected, but its draft can be saved as new", async ({ page, context }) => {
  await page.goto("/settings/appearance");
  await create(page, "Shared scheme");
  const originalId = (await savedState(page)).selected;
  const other = await context.newPage();
  await other.goto("/settings/appearance");
  await page.getByRole("button", { name: "Edit Shared scheme", exact: true }).click();
  await page.getByLabel("Accent color", { exact: true }).fill("#7722aa");
  await other.getByRole("button", { name: "Delete Shared scheme", exact: true }).click();
  await other.getByRole("button", { name: "Delete scheme", exact: true }).click();
  await expect(page.getByRole("checkbox", { name: "Preview in Studio" })).not.toBeChecked();
  await page.getByRole("button", { name: "Save scheme", exact: true }).click();
  await expect(page.getByRole("alert")).toContainText("deleted in another window");
  expect((await savedState(page)).schemes).toHaveLength(0);
  await page.getByRole("button", { name: "Save as new scheme", exact: true }).click();
  expect((await savedState(page)).selected).not.toBe(originalId);
  expect((await savedState(page)).schemes[0].tokens["--accent"]).toBe("#7722aa");
});

test("a late file import cannot override a newer selection from another window", async ({ page, context }) => {
  await page.goto("/settings/appearance");
  const other = await context.newPage();
  await other.goto("/settings/appearance");
  await page.evaluate(() => {
    const original = File.prototype.text;
    File.prototype.text = async function() {
      await new Promise<void>((resolve) => { (window as unknown as { releaseThemeImport: () => void }).releaseThemeImport = resolve; });
      return original.call(this);
    };
  });
  const json = JSON.stringify({ format: "bakery-color-scheme", version: 1, name: "Late import", base: "dark", tokens: {} });
  await page.getByLabel("Import color scheme file").setInputFiles({ name: "late.json", mimeType: "application/json", buffer: Buffer.from(json) });
  await expect(page.getByRole("button", { name: "Import scheme", exact: true })).toBeDisabled();
  await other.getByRole("radio", { name: "Light", exact: true }).check();
  await expect(page.getByRole("radio", { name: "Light", exact: true })).toBeChecked();
  await page.evaluate(() => (window as unknown as { releaseThemeImport: () => void }).releaseThemeImport());
  await expect(page.getByRole("button", { name: "Import scheme", exact: true })).toBeEnabled();
  expect((await savedState(page)).selected).toBe("light");
  expect((await savedState(page)).schemes).toHaveLength(0);
});
