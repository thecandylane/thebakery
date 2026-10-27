// Real-browser proof for 07c-STUDIO-SHELL §3 through the actual Dockview UI
// (§2): one panel per node-owned named terminal. Closing that panel's dock
// tab only ever detaches the view (the process keeps running); "Close
// terminal" inside the panel is the one action that actually kills it.
// Same harness as editor.spec.ts (see playwright.config.ts / helpers.ts).
import { expect, type Page, type Locator } from "@playwright/test";
import { test } from "./fixtures";
import { makeScratchRepo, AUTH_HEADERS, NODE_BASE, openStream, toHandle } from "./helpers";

async function gotoStudio(page: Page, streamId: string) {
  await page.goto(`/w/${toHandle(streamId)}`);
}

async function openAddMenu(page: Page) {
  await page.getByRole("button", { name: "+ Panel" }).click();
}

async function createTerminal(page: Page, mode: "New host terminal" | "New room terminal") {
  await openAddMenu(page);
  await page.getByRole("button", { name: mode, exact: true }).click();
}

function terminalPanel(page: Page, name: string) {
  return page.locator(".terminal-widget", { has: page.locator(".term-name", { hasText: name }) });
}

/** xterm fragments a line across many small spans (color/style runs), and in
 *  a narrow docked panel a marker can itself soft-wrap onto a second xterm
 *  row — `.innerText()` inserts a newline between row divs, tearing the
 *  substring in two. `getByText(x).toHaveCount(n)` is unreliable for the
 *  same reason. Strip whitespace before counting so a wrapped marker
 *  rejoins into one contiguous match. */
async function countOccurrences(locator: Locator, needle: string): Promise<number> {
  const text = (await locator.innerText()).replace(/\s+/g, "");
  return text.split(needle).length - 1;
}

async function expectOccurrences(locator: Locator, needle: string, count: number, timeoutMs = 15_000) {
  await expect(async () => {
    expect(await countOccurrences(locator, needle)).toBe(count);
  }).toPass({ timeout: timeoutMs });
}

test.describe("Studio terminal panels (node-owned sessions)", () => {
  test("panel controls wait for socket bootstrap, then the first menu click stays open", async ({ page, request }) => {
    const repo = makeScratchRepo({ "f.txt": "hi\n" });
    const streamId = await openStream(request, repo, `e2e-terminal-bootstrap-${Date.now()}`);
    let release!: () => void;
    const gate = new Promise<void>((resolve) => { release = resolve; });
    await page.route("**/studio/state", async (route) => {
      await gate;
      await route.continue();
    });
    await page.goto(`/w/${toHandle(streamId)}`, { waitUntil: "domcontentloaded" });
    const add = page.getByRole("button", { name: "+ Panel" });
    try {
      await expect(add).toBeDisabled();
    } finally {
      release();
    }
    await expect(add).toBeEnabled();
    await add.click();
    await expect(page.getByRole("button", { name: "New room terminal", exact: true })).toBeVisible();
    await expect(add).toHaveAttribute("aria-expanded", "true");
  });

  test("closing the dock tab detaches, not kills; reopening replays buffered output", async ({ page, request }) => {
    const repo = makeScratchRepo({ "f.txt": "hi\n" });
    const streamId = await openStream(request, repo, `e2e-terminal-${Date.now()}`);

    await gotoStudio(page, streamId);
    await createTerminal(page, "New room terminal");

    const panel = terminalPanel(page, "room");
    await expect(panel.getByText("live")).toBeVisible({ timeout: 15_000 });

    const marker = `PRINTED_BEFORE_DETACH_${Date.now()}`;
    await panel.locator(".term-shell").click();
    await page.keyboard.type(`echo ${marker}`);
    await page.keyboard.press("Enter");
    // Once for the pty's own echo of the typed command, once for its output.
    await expectOccurrences(panel, marker, 2);

    // Close the dock tab (Dockview's own "×") — detach, not kill.
    const tab = page.locator(".dv-default-tab", { hasText: "room" });
    await tab.locator(".dv-default-tab-action").click();
    await expect(terminalPanel(page, "room")).toHaveCount(0);

    // The session must still be alive on the node — a detach, not a kill.
    const stillLive = await request.get(`${NODE_BASE}/terminal/list`, { headers: AUTH_HEADERS, params: { stream_id: streamId } });
    const list = (await stillLive.json()) as { name: string; live: boolean }[];
    expect(list.find((t) => t.name === "room")?.live).toBe(true);

    // Reopen through the same "+ Panel" menu's "Reopen" section — the real
    // UI path a human would use, not a raw API replay.
    await openAddMenu(page);
    await page.getByRole("button", { name: "room", exact: true }).click();

    const reopened = terminalPanel(page, "room");
    await expect(reopened.getByText("live")).toBeVisible({ timeout: 15_000 });
    await expectOccurrences(reopened, marker, 2);
  });

  test("reloading the browser reattaches to the same live terminal, replaying buffered output before new live output", async ({
    page,
    request,
  }) => {
    const repo = makeScratchRepo({ "f.txt": "hi\n" });
    const streamId = await openStream(request, repo, `e2e-terminal-reload-${Date.now()}`);

    await gotoStudio(page, streamId);
    await createTerminal(page, "New room terminal");

    const panel = terminalPanel(page, "room");
    await expect(panel.getByText("live")).toBeVisible({ timeout: 15_000 });

    const before = `PRINTED_BEFORE_RELOAD_${Date.now()}`;
    await panel.locator(".term-shell").click();
    await page.keyboard.type(`echo ${before}`);
    await page.keyboard.press("Enter");
    await expectOccurrences(panel, before, 2);

    const listBefore = await request.get(`${NODE_BASE}/terminal/list`, { headers: AUTH_HEADERS, params: { stream_id: streamId } });
    const idBefore = ((await listBefore.json()) as { name: string; terminal_id: string }[]).find((t) => t.name === "room")
      ?.terminal_id;

    // Give the debounced persist (300ms) time to land before reloading —
    // same reasoning as dockview.spec.ts's own persistence test.
    await page.waitForTimeout(500);

    // A hard reload — not a panel close/reopen. The whole SPA, its Dockview
    // instance, and every websocket are torn down and rebuilt from scratch,
    // reading the persisted layout back from localStorage.
    await page.reload();

    const reattached = terminalPanel(page, "room");
    await expect(reattached.getByText("live")).toBeVisible({ timeout: 15_000 });
    // Buffered output survives the reload without a manual "Reopen" click —
    // the terminal panel is part of the persisted layout, not a fresh add.
    await expectOccurrences(reattached, before, 2);

    const after = `PRINTED_AFTER_RELOAD_${Date.now()}`;
    await reattached.locator(".term-shell").click();
    await page.keyboard.type(`echo ${after}`);
    await page.keyboard.press("Enter");
    await expectOccurrences(reattached, after, 2);

    const listAfter = await request.get(`${NODE_BASE}/terminal/list`, { headers: AUTH_HEADERS, params: { stream_id: streamId } });
    const idAfter = ((await listAfter.json()) as { name: string; terminal_id: string }[]).find((t) => t.name === "room")
      ?.terminal_id;
    expect(idAfter, "reload must reattach to the same node-owned session, not spawn a fresh one").toBe(idBefore);
  });

  test("explicit \"Close terminal\" kills the process; the panel drops from the layout", async ({ page, request }) => {
    const repo = makeScratchRepo({ "f.txt": "hi\n" });
    const streamId = await openStream(request, repo, `e2e-terminal-close-${Date.now()}`);

    await gotoStudio(page, streamId);
    await createTerminal(page, "New host terminal");

    const panel = terminalPanel(page, "host");
    await expect(panel.getByText("live")).toBeVisible({ timeout: 15_000 });

    await panel.getByRole("button", { name: "Close terminal" }).click();
    await expect(terminalPanel(page, "host")).toHaveCount(0, { timeout: 15_000 });

    const list = await request.get(`${NODE_BASE}/terminal/list`, { headers: AUTH_HEADERS, params: { stream_id: streamId } });
    const terminals = (await list.json()) as { name: string }[];
    expect(terminals.find((t) => t.name === "host")).toBeUndefined();
  });

  test("three named terminals can be open in the dock at once", async ({ page, request }) => {
    const repo = makeScratchRepo({ "f.txt": "hi\n" });
    const streamId = await openStream(request, repo, `e2e-terminal-three-${Date.now()}`);

    await gotoStudio(page, streamId);
    await createTerminal(page, "New room terminal");
    await expect(terminalPanel(page, "room").getByText("live")).toBeVisible({ timeout: 15_000 });
    await createTerminal(page, "New room terminal");
    await expect(terminalPanel(page, "room-2").getByText("live")).toBeVisible({ timeout: 15_000 });
    await createTerminal(page, "New host terminal");
    await expect(terminalPanel(page, "host").getByText("live")).toBeVisible({ timeout: 15_000 });

    await expect(page.locator(".terminal-widget")).toHaveCount(3);

    // Closing one terminal does not kill the other two (07c Proof). All three
    // landed as tabs in one group ("host" created last is the active tab), so
    // bring room-2's tab to front before its "Close terminal" is clickable.
    await page.locator(".dv-default-tab", { hasText: "room-2" }).click();
    await terminalPanel(page, "room-2").getByRole("button", { name: "Close terminal" }).click();
    await expect(terminalPanel(page, "room-2")).toHaveCount(0, { timeout: 15_000 });
    // Only one tab in the group is visible at a time — presence (not
    // visibility) is what proves closing room-2 didn't also remove these.
    await expect(terminalPanel(page, "room")).toHaveCount(1);
    await expect(terminalPanel(page, "host")).toHaveCount(1);

    const list = await request.get(`${NODE_BASE}/terminal/list`, { headers: AUTH_HEADERS, params: { stream_id: streamId } });
    const terminals = (await list.json()) as { name: string; live: boolean }[];
    expect(terminals.find((t) => t.name === "room")?.live).toBe(true);
    expect(terminals.find((t) => t.name === "host")?.live).toBe(true);
    expect(terminals.find((t) => t.name === "room-2")).toBeUndefined();
  });
});
