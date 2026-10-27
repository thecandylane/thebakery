// Responsive shell proof. This uses the standard Playwright fixture, so every
// page talks to the invocation's isolated bakery-node, scratch HOME, Heart,
// and projects — never the operator's running node or live store.
import { expect, type APIRequestContext, type Page } from "@playwright/test";
import { test } from "./fixtures";
import { fakeAgentProjection, makeScratchRepo, openMemoryPanel, openStream, seatAssign, toHandle } from "./helpers";

const sidebar = (page: Page) => page.locator('[data-testid="primary-sidebar"]');
const sidebarToggle = (page: Page) => page.locator('[data-testid="sidebar-toggle"]');

async function readyOrchestrator(page: Page, request: APIRequestContext, label: string): Promise<string> {
  const repo = makeScratchRepo({ "responsive.txt": "responsive shell\n" });
  const streamId = await openStream(request, repo, `${label}-${Date.now()}`);
  await seatAssign(request, streamId, "orchestrator", "grok");
  await fakeAgentProjection(page, streamId, "orchestrator", {
    assigned_model_id: "grok",
    socket_ready: true,
  });
  return streamId;
}

async function waitForComposer(page: Page): Promise<void> {
  // Attached, not visible: a lone panel's tab strip is hidden.
  await expect(page.locator(".dv-default-tab", { hasText: "Orchestrator" })).toBeAttached({ timeout: 15_000 });
  await expect(page.locator('[data-testid="composer"]')).toBeVisible({ timeout: 15_000 });
}

async function expectNoPageOverflow(page: Page): Promise<void> {
  await expect
    .poll(() =>
      page.evaluate(
        () => document.documentElement.scrollWidth <= document.documentElement.clientWidth,
      ),
    )
    .toBe(true);
}

test.afterEach(async ({ page }) => {
  await page.unrouteAll({ behavior: "ignoreErrors" });
});

test.describe("responsive Studio shell", () => {
  test("the desktop sidebar is visible, keyboard-resizable, and remembers its width", async ({ page }) => {
    await page.setViewportSize({ width: 1280, height: 800 });
    await page.goto("/w");

    await expect(sidebar(page)).toBeVisible({ timeout: 15_000 });
    await expect(sidebarToggle(page)).toHaveAttribute("aria-expanded", "true");

    const separator = page.getByRole("separator", { name: "Resize sidebar" });
    await expect(separator).toBeVisible();
    const startingValue = Number(await separator.getAttribute("aria-valuenow"));
    const startingBox = await sidebar(page).boundingBox();
    expect(startingBox).not.toBeNull();

    await separator.focus();
    await page.keyboard.press("ArrowRight");
    await expect(separator).toHaveAttribute("aria-valuenow", String(startingValue + 8));
    await expect
      .poll(async () => (await sidebar(page).boundingBox())?.width ?? 0)
      .toBeGreaterThan((startingBox?.width ?? 0) + 7);

    await page.reload();
    await expect(sidebar(page)).toBeVisible({ timeout: 15_000 });
    await expect(page.getByRole("separator", { name: "Resize sidebar" })).toHaveAttribute(
      "aria-valuenow",
      String(startingValue + 8),
    );

    // A global keyboard close must not strand focus in the sidebar subtree
    // it removes: it lands on the toggle, now back in the title bar.
    const sidebarHome = sidebar(page).getByRole("link", { name: "Bakery Factory Floor" });
    await sidebarHome.focus();
    await expect(sidebarHome).toBeFocused();
    await page.keyboard.down("Control");
    await page.keyboard.press("\\");
    await page.keyboard.up("Control");
    await expect(sidebar(page)).toHaveCount(0);
    await expect(sidebarToggle(page)).toBeFocused();
  });

  test("the compact sidebar overlays the workspace and closes with Escape or its scrim", async ({ page }) => {
    await page.setViewportSize({ width: 1000, height: 800 });
    await page.goto("/w");

    const toggle = sidebarToggle(page);
    const main = page.locator('[data-testid="studio-main"]');
    await expect(toggle).toBeVisible({ timeout: 15_000 });
    await expect(toggle).toHaveAttribute("aria-expanded", "false");
    await expect(sidebar(page)).toHaveCount(0);

    const before = await main.boundingBox();
    expect(before).not.toBeNull();
    await toggle.click();
    await expect(sidebar(page)).toBeVisible();
    await expect(page.locator('[data-testid="sidebar-scrim"]')).toBeVisible();
    await expect(toggle).toHaveAttribute("aria-expanded", "true");

    // The drawer is fixed over the workspace: opening it cannot reflow or
    // narrow the active work surface.
    const open = await main.boundingBox();
    expect(open?.x).toBeCloseTo(before?.x ?? 0, 0);
    expect(open?.width).toBeCloseTo(before?.width ?? 0, 0);

    await page.keyboard.press("Escape");
    await expect(sidebar(page)).toHaveCount(0);
    await expect(toggle).toBeFocused();

    await toggle.click();
    await expect(sidebar(page)).toBeVisible();
    await page.keyboard.down("Control");
    await page.keyboard.press("\\");
    await page.keyboard.up("Control");
    await expect(sidebar(page)).toHaveCount(0);
    await expect(toggle).toBeFocused();

    await toggle.click();
    const scrim = page.locator('[data-testid="sidebar-scrim"]');
    await expect(scrim).toBeVisible();
    await scrim.click({ position: { x: 700, y: 400 } });
    await expect(sidebar(page)).toHaveCount(0);
    await expect(toggle).toBeFocused();
  });

  test("the compact sidebar's settings bar is one Tab stop per control, in order", async ({ page }) => {
    await page.setViewportSize({ width: 1000, height: 800 });
    await page.goto("/w");

    await sidebarToggle(page).click();
    const compactSidebar = sidebar(page);
    await expect(compactSidebar).toBeVisible();
    const bar = compactSidebar.locator('[data-testid="settings-bar"]');
    const settingsLink = bar.getByRole("link", { name: "Settings" });
    const appearance = bar.getByRole("button", { name: /^(Dark|Dim|Light)$/ });
    await expect(bar).toBeVisible();

    // One click to Settings — no menu in the way. The bar is Settings, then
    // the scheme chip (write mode lives in Settings → Approvals); the chip is
    // the drawer's last stop, so Tab wraps to its first.
    await settingsLink.focus();
    await page.keyboard.press("Tab");
    await expect(appearance).toBeFocused();
    await page.keyboard.press("Tab");
    await expect(compactSidebar.getByRole("link", { name: "Bakery Factory Floor" })).toBeFocused();
    await settingsLink.click();
    await expect(page).toHaveURL(/\/settings\/agents$/);
  });

  test("the command palette exposes combobox selection semantics", async ({ page, request }) => {
    await page.setViewportSize({ width: 1000, height: 800 });
    const repo = makeScratchRepo({ "palette.txt": "palette semantics\n" });
    const paletteRun = `palette-${Date.now()}`;
    await openStream(request, repo, `${paletteRun}-first`);
    await openStream(request, repo, `${paletteRun}-second`);
    await page.goto("/w");

    await expect(sidebarToggle(page)).toBeVisible({ timeout: 15_000 });
    await page.keyboard.press("Control+k");
    const palette = page.getByRole("dialog", { name: "Command palette" });
    const combobox = palette.getByRole("combobox", { name: "Search chats and threads" });
    const listbox = palette.getByRole("listbox", { name: "Chats and threads" });
    const options = listbox.getByRole("option");
    await expect(combobox).toBeFocused();
    await expect(combobox).toHaveAttribute("aria-controls", "command-palette-results");
    await combobox.fill(paletteRun);
    await expect(options).toHaveCount(2);

    const firstId = await options.nth(0).getAttribute("id");
    const secondId = await options.nth(1).getAttribute("id");
    expect(firstId).toBeTruthy();
    expect(secondId).toBeTruthy();
    await expect(combobox).toHaveAttribute("aria-activedescendant", firstId!);
    await expect(options.nth(0)).toHaveAttribute("aria-selected", "true");
    await expect(options.nth(1)).toHaveAttribute("aria-selected", "false");

    await page.keyboard.press("ArrowDown");
    await expect(combobox).toHaveAttribute("aria-activedescendant", secondId!);
    await expect(options.nth(0)).toHaveAttribute("aria-selected", "false");
    await expect(options.nth(1)).toHaveAttribute("aria-selected", "true");
  });

  test("the compact sidebar yields modal focus to the command palette and restores the title bar", async ({ page }) => {
    await page.setViewportSize({ width: 1000, height: 800 });
    await page.goto("/w");

    const toggle = sidebarToggle(page);
    await expect(toggle).toBeVisible({ timeout: 15_000 });
    await toggle.click();
    await expect(sidebar(page)).toBeVisible();

    await page.getByRole("button", { name: /Search projects and work/ }).click();
    const palette = page.getByRole("dialog", { name: "Command palette" });
    const paletteInput = palette.getByRole("combobox", { name: "Search chats and threads" });
    await expect(palette).toBeVisible();
    await expect(sidebar(page)).toHaveCount(0);
    await expect(page.locator('[data-testid="sidebar-scrim"]')).toHaveCount(0);
    await expect(paletteInput).toBeFocused();

    // With no sessions the input is the sole focus target, so both traversal
    // directions must remain contained by the modal.
    await page.keyboard.press("Shift+Tab");
    await expect(paletteInput).toBeFocused();
    await page.keyboard.press("Tab");
    await expect(paletteInput).toBeFocused();

    await page.keyboard.press("Escape");
    await expect(palette).toHaveCount(0);
    await expect(toggle).toBeFocused();
  });

  test("a 720px workspace has no page overflow and preserves a draft across pane close/reopen", async ({
    page,
    request,
  }) => {
    await page.setViewportSize({ width: 720, height: 800 });
    const streamId = await readyOrchestrator(page, request, "responsive-draft");
    await page.goto(`/w/${toHandle(streamId)}`);
    await waitForComposer(page);

    const textarea = page.locator('[data-testid="composer"] textarea');
    const draft = "keep this narrow-screen draft intact";
    await textarea.fill(draft);
    await page.waitForTimeout(400); // allow the composer's debounced durable write
    await expectNoPageOverflow(page);

    // A lone panel has no tab strip, so its tab (and close button) shows
    // only once a second panel is open.
    await openMemoryPanel(page);
    const tab = page.locator(".dv-default-tab", { hasText: "Orchestrator" });
    await expect(tab).toBeVisible();
    await tab.hover();
    await tab.locator(".dv-default-tab-action").click();
    await expect(page.locator('[data-testid="agent-panel"]')).toHaveCount(0);

    await page.getByRole("button", { name: "+ Panel" }).click();
    await page.locator(".menu").getByRole("button", { name: "Orchestrator", exact: true }).click();
    await waitForComposer(page);
    await expect(textarea).toHaveValue(draft);
    await expectNoPageOverflow(page);
  });

  test("the Settings rail is a mutually safe compact drawer, and Baker's conversations live in the primary one", async ({ page }) => {
    await page.setViewportSize({ width: 800, height: 800 });
    await page.goto("/settings/providers");

    const settingsToggle = page.locator('[data-testid="settings-nav-toggle"]');
    const settingsDrawer = page.locator('[data-testid="settings-nav-drawer"]');
    await expect(settingsToggle).toBeVisible({ timeout: 15_000 });
    await expect(settingsDrawer).toBeHidden();
    await settingsToggle.click();
    await expect(settingsDrawer).toBeVisible();
    await page.keyboard.press("Escape");
    await expect(settingsDrawer).toBeHidden();
    await expect(settingsToggle).toBeFocused();

    await settingsToggle.click();
    await page.locator('[data-testid="settings-nav-scrim"]').click({ position: { x: 600, y: 300 } });
    await expect(settingsDrawer).toBeHidden();
    await expect(settingsToggle).toBeFocused();

    // Claiming the global primary drawer closes the route-owned drawer before
    // either window-level focus trap can compete. Settings is a window over
    // the whole shell (title bar included), so the drawer is claimed with its
    // shortcut and opens over the window.
    await settingsToggle.click();
    await expect(settingsDrawer).toBeVisible();
    await page.keyboard.down("Control");
    await page.keyboard.press("\\");
    await page.keyboard.up("Control");
    await expect(settingsDrawer).toBeHidden();
    await expect(sidebar(page)).toBeVisible();
    await page.keyboard.down("Control");
    await page.keyboard.press("\\");
    await page.keyboard.up("Control");
    await expect(sidebar(page)).toHaveCount(0);

    // The palette claims the same lane and restores to the route toggle when
    // its original active link disappears with the settings drawer.
    await settingsToggle.click();
    await expect(settingsDrawer).toBeVisible();
    await page.keyboard.press("Control+k");
    const palette = page.getByRole("dialog", { name: "Command palette" });
    await expect(settingsDrawer).toBeHidden();
    await expect(palette.getByRole("combobox", { name: "Search chats and threads" })).toBeFocused();
    await page.keyboard.press("Escape");
    await expect(settingsToggle).toBeFocused();

    await page.goto("/baker");
    // Baker threads (BAKER-THREADS-03): the Baker page no longer owns a
    // drawer of its own. Its conversations live in the ONE primary drawer
    // (Baker mode), and "New thread" stays reachable in the page header at
    // compact width — so there is no second focus trap left to race.
    const newThread = page.locator('[data-testid="baker-page-new-thread"]');
    await expect(newThread).toBeVisible({ timeout: 15_000 });
    await expect(newThread).toHaveAccessibleName("New thread");
    await expectNoPageOverflow(page);

    await sidebarToggle(page).click();
    await expect(sidebar(page)).toBeVisible();
    const mainRow = sidebar(page).locator('[data-testid="baker-thread-row"]').first();
    await expect(mainRow).toBeVisible({ timeout: 15_000 });
    await expect(mainRow.getByRole("link")).toHaveAttribute("aria-current", "page");
    await page.keyboard.press("Escape");
    await expect(sidebar(page)).toHaveCount(0);
    await expect(sidebarToggle(page)).toBeFocused();

    // The palette still claims the lane from the Baker page and returns focus.
    await sidebarToggle(page).click();
    await expect(sidebar(page)).toBeVisible();
    await page.keyboard.press("Control+k");
    await expect(sidebar(page)).toHaveCount(0);
    await expect(palette.getByRole("combobox", { name: "Search chats and threads" })).toBeFocused();
    await page.keyboard.press("Escape");
    await expect(palette).toHaveCount(0);
    await expectNoPageOverflow(page);
  });
});
