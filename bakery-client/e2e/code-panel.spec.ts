// Real-browser proof that Files + Editor are unified into one Code panel
// (07c-STUDIO-SHELL follow-up): a collapsible file tree on the left,
// Monaco + tabs on the right, sharing the route's `selectedFile` as the one
// source of truth for "which file is open" — no second tree/editor state.
// Same harness as editor.spec.ts (see playwright.config.ts / helpers.ts).
import { expect, type Page } from "@playwright/test";
import { test } from "./fixtures";
import { makeScratchRepo, openStream, toHandle, filesRead, openCodePanel } from "./helpers";

async function gotoStudio(page: Page, streamId: string) {
  await page.goto(`/w/${toHandle(streamId)}`);
}

test.describe("Studio Code panel (file tree + editor unified)", () => {
  test("expand a directory, click a file, edit/save it, switch tabs, and the tree stays usable", async ({
    page,
    request,
  }) => {
    const repo = makeScratchRepo({
      "top.txt": "top level\n",
      "sub/deep.txt": "deep file\n",
    });
    const streamId = await openStream(request, repo, `e2e-code-panel-${Date.now()}`);

    await gotoStudio(page, streamId);
    await expect(page.locator(".dv-default-tab", { hasText: "Orchestrator" })).toBeAttached({ timeout: 15_000 }); // alone: no tab strip

    // 07c-UX-SHELL: Code opens on demand now, not as part of first paint.
    // No separate "Files" tab to click once it's open — the tree sits
    // alongside the (empty) editor in the one Code panel.
    await openCodePanel(page);
    await expect(page.locator(".dv-default-tab", { hasText: "Code" })).toBeVisible({ timeout: 15_000 });
    await expect(page.locator(".row.file", { hasText: "top.txt" })).toBeVisible();

    // The subdirectory starts collapsed — its file is not in the DOM yet.
    const subRow = page.locator(".row.dir", { hasText: "sub" });
    await expect(subRow).toBeVisible();
    await expect(page.locator(".row.file", { hasText: "deep.txt" })).toHaveCount(0);

    // Expand it.
    await subRow.click();
    const deepRow = page.locator(".row.file", { hasText: "deep.txt" });
    await expect(deepRow).toBeVisible();

    // Click the file — it opens/selects in the same panel, right next to the tree.
    await deepRow.click();
    await expect(page.locator(".tab.active .tab-label-btn", { hasText: "deep.txt" })).toBeVisible();
    await expect(page.getByRole("textbox", { name: "Editor content" })).toBeAttached();
    await expect(deepRow).toHaveClass(/selected/);

    // Edit and save it.
    await page.locator(".monaco-host .monaco-editor").click();
    await page.keyboard.press("Control+End");
    await page.keyboard.type(" — edited");
    await page.locator(".save-btn.dirty:not([disabled])").click();
    await expect(page.locator(".save-btn")).not.toHaveClass(/dirty/);
    const onDisk = await filesRead(request, streamId, "sub/deep.txt");
    expect(onDisk.text).toBe("deep file\n — edited");

    // Open a second file — a new tab, not a replacement.
    await page.locator(".row.file", { hasText: "top.txt" }).click();
    await expect(page.locator(".tab.active .tab-label-btn", { hasText: "top.txt" })).toBeVisible();

    // Switch back to the first tab — its edit is still there (same model,
    // never reloaded from disk just because the tab lost focus).
    await page.locator(".tab .tab-label-btn", { hasText: "deep.txt" }).click();
    await expect(page.locator(".tab.active .tab-label-btn", { hasText: "deep.txt" })).toBeVisible();
    await expect(page.getByText("deep file\n — edited", { exact: false })).toBeVisible();

    // The tree remains usable after all that tab switching: collapse...
    await subRow.click();
    await expect(page.locator(".row.file", { hasText: "deep.txt" })).toHaveCount(0);
    // ...re-expand...
    await subRow.click();
    const deepRowAgain = page.locator(".row.file", { hasText: "deep.txt" });
    await expect(deepRowAgain).toBeVisible();
    // ...and clicking it still correctly re-selects that open tab.
    await deepRowAgain.click();
    await expect(page.locator(".tab.active .tab-label-btn", { hasText: "deep.txt" })).toBeVisible();
    await expect(deepRowAgain).toHaveClass(/selected/);
  });
});
