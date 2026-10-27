// Real-browser proof for 07c-STUDIO-SHELL §1: Monaco replaces the read-only
// viewer, save is conflict-safe, and Monaco's own worker actually loads (no
// "Could not create web worker" fallback). Runs against a real bakery-node
// daemon + a real scratch git repo (see playwright.config.ts / helpers.ts) —
// not a mocked network layer, so this doubles as the harness Dockview
// persistence and terminal reattachment specs reuse later in 07c.
import { expect, type Page, type ConsoleMessage } from "@playwright/test";
import { test } from "./fixtures";
import { makeScratchRepo, openStream, toHandle, filesRead, filesWrite, openCodePanel } from "./helpers";

const WORKER_FALLBACK_MARKERS = ["could not create web worker", "worker-fallback", "loading web worker code in main thread"];

function watchForWorkerFallback(page: Page): ConsoleMessage[] {
  const hits: ConsoleMessage[] = [];
  page.on("console", (msg) => {
    const text = msg.text().toLowerCase();
    if (WORKER_FALLBACK_MARKERS.some((m) => text.includes(m))) hits.push(msg);
  });
  return hits;
}

async function gotoStudio(page: Page, streamId: string) {
  await page.goto(`/w/${toHandle(streamId)}`);
}

// Files + Editor are unified into one Code panel — the tree sits alongside
// the editor in the same panel. 07c-UX-SHELL: Code is on-demand (opened via
// "+ Panel"), no longer part of the first-use default layout, so this opens
// it explicitly before reaching for a file row.
async function openFileInEditor(page: Page, fileName: string) {
  await openCodePanel(page);
  await page.locator(".row.file", { hasText: fileName }).click();
  await expect(page.locator(".tab.active .tab-label-btn", { hasText: fileName })).toBeVisible();
  // Monaco's hidden input carries this accessible name — a more reliable
  // handle than guessing its internal CSS classes across versions.
  await expect(page.getByRole("textbox", { name: "Editor content" })).toBeAttached();
}

test.describe("Studio editor (Monaco)", () => {
  test("types, undoes/redoes, and finds inside a real file", async ({ page, request }) => {
    const repo = makeScratchRepo({ "notes.txt": "line one\nline two\n" });
    const streamId = await openStream(request, repo, `e2e-editor-${Date.now()}`);
    const workerHits = watchForWorkerFallback(page);

    await gotoStudio(page, streamId);
    await openFileInEditor(page, "notes.txt");

    // Click into the body (not just the tab) to place the caret before typing.
    await page.locator(".monaco-host .monaco-editor").click();
    await page.keyboard.press("Control+End");
    await page.keyboard.type("typed by e2e");

    await expect(page.getByText("typed by e2e")).toBeVisible();
    await expect(page.locator(".save-btn.dirty:not([disabled])")).toBeVisible();
    await expect(page.locator(".tab.active .dirty-dot")).toBeVisible();

    // Undo/redo — Monaco's own commands, bound to the platform's usual chord.
    await page.keyboard.press("Meta+z");
    await expect(page.getByText("typed by e2e")).toHaveCount(0);
    await page.keyboard.press("Meta+Shift+z");
    await expect(page.getByText("typed by e2e")).toBeVisible();

    // Search — Monaco's built-in find widget, not a Bakery-authored one.
    await page.keyboard.press("Meta+f");
    await expect(page.locator(".find-widget")).toBeVisible();
    await page.keyboard.press("Escape");

    expect(workerHits.map((m) => m.text())).toEqual([]);
  });

  test("save round-trips real bytes to disk", async ({ page, request }) => {
    const repo = makeScratchRepo({ "save-me.txt": "before\n" });
    const streamId = await openStream(request, repo, `e2e-save-${Date.now()}`);

    await gotoStudio(page, streamId);
    await openFileInEditor(page, "save-me.txt");

    await page.locator(".monaco-host .monaco-editor").click();
    await page.keyboard.press("Control+End");
    await page.keyboard.type("\nafter");
    await page.locator(".save-btn.dirty:not([disabled])").click();

    await expect(page.locator(".save-btn")).not.toHaveClass(/dirty/);
    const onDisk = await filesRead(request, streamId, "save-me.txt");
    expect(onDisk.text).toBe("before\n\nafter");
  });

  test("a stale save is refused and offers reload/diff, never a silent overwrite", async ({ page, request }) => {
    const repo = makeScratchRepo({ "conflict.txt": "original\n" });
    const streamId = await openStream(request, repo, `e2e-conflict-${Date.now()}`);

    await gotoStudio(page, streamId);
    await openFileInEditor(page, "conflict.txt");

    // The editor now holds digest D0 for "original\n". Dirty it locally...
    await page.locator(".monaco-host .monaco-editor").click();
    await page.keyboard.press("Control+End");
    await page.keyboard.type(" — the operator's edit");

    // ...then someone else (an agent accept, another tab) saves first, still
    // against D0 (the file hasn't changed since the editor read it).
    const baseline = await filesRead(request, streamId, "conflict.txt");
    const external = await filesWrite(request, streamId, "conflict.txt", "changed externally\n", baseline.digest);
    expect(external.status).toBe("ok");

    // The editor's save now conflicts — it must refuse, not clobber.
    await page.locator(".save-btn.dirty:not([disabled])").click();
    await expect(page.locator(".conflict-overlay")).toBeVisible();
    await expect(page.locator(".diff-host")).toBeVisible();

    const onDiskDuringConflict = await filesRead(request, streamId, "conflict.txt");
    expect(onDiskDuringConflict.text).toBe("changed externally\n");

    await page.locator(".conflict-btn", { hasText: "Discard mine" }).click();
    await expect(page.locator(".conflict-overlay")).toHaveCount(0);
    await expect(page.getByText("changed externally")).toBeVisible();
    await expect(page.locator(".save-btn")).not.toHaveClass(/dirty/);
  });
});
