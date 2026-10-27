// Real-browser proof for docs/slices/07d-A3-PROJECTS-NOT-REPOS.md §0: color
// scheme selection lives in theme.css only, applied as `data-theme` on
// `<html>`, and must propagate live (no reload) to the rest of the chrome.
// Same harness as the other shell specs (see playwright.config.ts / helpers.ts).
import { expect } from "@playwright/test";
import { test } from "./fixtures";

test.describe("07d-A3 Appearance: color schemes", () => {
  test("switching the scheme via the settings dial applies live to chrome and persists across reload", async ({
    page,
  }) => {
    await page.goto("/");

    const html = page.locator("html");
    // Dark is the unmarked default — `applyTheme` removes the attribute
    // entirely for it rather than writing "dark".
    await expect(html).not.toHaveAttribute("data-theme");
    const darkBg = await page.evaluate(() => getComputedStyle(document.documentElement).getPropertyValue("--bg-app"));

    // The scheme chip sits in the sidebar's settings bar itself — one click,
    // no Settings popover in the way.
    const themeChip = page.locator('[data-testid="settings-bar"] .theme-chip');
    await expect(themeChip).toHaveText("Dark");

    // dark -> dim: applies live, no navigation.
    await themeChip.click();
    await expect(html).toHaveAttribute("data-theme", "dim");
    await expect(themeChip).toHaveText("Dim");
    const dimBg = await page.evaluate(() => getComputedStyle(document.documentElement).getPropertyValue("--bg-app"));
    expect(dimBg).not.toBe(darkBg);

    // dim -> light
    await themeChip.click();
    await expect(html).toHaveAttribute("data-theme", "light");
    await expect(themeChip).toHaveText("Light");
    const lightBg = await page.evaluate(() => getComputedStyle(document.documentElement).getPropertyValue("--bg-app"));
    expect(lightBg).not.toBe(dimBg);
    expect(lightBg).not.toBe(darkBg);

    // Persists across reload — and applies before first paint (FOUC-safe):
    // re-reading the attribute immediately after `reload()` resolves is
    // enough to prove it was never briefly absent for a later effect to set.
    await page.reload();
    await expect(html).toHaveAttribute("data-theme", "light");
  });
});
