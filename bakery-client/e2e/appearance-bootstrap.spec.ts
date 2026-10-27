import { test, expect } from "./fixtures";

// Block the hydrated client: this is the actual HTML bootstrap, not a test of
// onMount eventually fixing the palette after a flash of the wrong colors.
test("every preset and custom overrides restore before application JavaScript", async ({ page }) => {
  await page.route("**/src/**", (route) => route.abort());
  await page.route("**/@vite/**", (route) => route.abort());
  await page.goto("/", { waitUntil: "domcontentloaded" });
  for (const preset of ["dark", "dim", "light", "monokai", "solarized-dark", "solarized-light", "high-contrast"]) {
    await page.evaluate((name) => localStorage.setItem("bakery.appearance.v1", JSON.stringify({ version: 1, selected: name, schemes: [] })), preset);
    await page.reload({ waitUntil: "domcontentloaded" });
    await expect(page.locator("html")).toHaveAttribute("data-color-scheme", preset);
    if (preset === "dark") await expect(page.locator("html")).not.toHaveAttribute("data-theme");
    else await expect(page.locator("html")).toHaveAttribute("data-theme", preset);
  }
  await page.evaluate(() => localStorage.setItem("bakery.appearance.v1", JSON.stringify({
    version: 1, selected: "custom:first-paint", schemes: [{
      id: "custom:first-paint", name: "First paint", base: "dark",
      tokens: { "--bg-app": "hsl(0,0%,95%)", "--accent": "#bc1234", "--accent-bg": "rgba(188,18,52,.14)" },
    }],
  })));
  await page.reload({ waitUntil: "domcontentloaded" });
  await expect(page.locator("html")).toHaveAttribute("data-color-scheme", "custom:first-paint");
  await expect(page.locator("html")).toHaveAttribute("data-color-mode", "light");
  expect(await page.evaluate(() => document.documentElement.style.colorScheme)).toBe("light");
  expect(await page.evaluate(() => document.documentElement.style.getPropertyValue("--accent"))).toBe("#bc1234");
});

test("legacy custom slot migrates without losing colors or accepting injected CSS", async ({ page }) => {
  await page.addInitScript(() => {
    localStorage.setItem("bakery.theme", "custom");
    localStorage.setItem("bakery.theme.custom", JSON.stringify({ "--accent": "#ab1234", "--bg-app": "#eee", "--fg": "url(https://example.invalid)", "--arbitrary": "#fff" }));
  });
  await page.goto("/settings/appearance");
  await expect(page.locator("html")).toHaveAttribute("data-color-scheme", "custom:legacy");
  expect(await page.evaluate(() => document.documentElement.style.getPropertyValue("--accent"))).toBe("#ab1234");
  expect(await page.evaluate(() => document.documentElement.style.getPropertyValue("--fg"))).toBe("");
  expect(await page.evaluate(() => document.documentElement.style.getPropertyValue("--arbitrary"))).toBe("");
  await expect(page.getByRole("button", { name: "Edit My custom scheme", exact: true })).toBeVisible();
});
