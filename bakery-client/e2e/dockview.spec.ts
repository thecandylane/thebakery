// Real-browser proof for 07c-STUDIO-SHELL §2 + 07c-UX-SHELL §2: Dockview's
// own serialized layout, persisted per socket, survives a reload — and a
// different socket keeps its own independent layout. Same harness as
// editor.spec.ts (see playwright.config.ts / helpers.ts).
import { expect, type Page } from "@playwright/test";
import { test } from "./fixtures";
import { makeScratchRepo, openStream, toHandle, openCodePanel, openMemoryPanel, seatAssign, fakeAgentProjection } from "./helpers";

async function gotoStudio(page: Page, streamId: string) {
  await page.goto(`/w/${toHandle(streamId)}`);
}

test.describe("Studio dock layout (Dockview persistence)", () => {
  test("a fresh socket's first-use layout is the conversation alone (07c-UX-SHELL §2.1/§2.2)", async ({
    page,
    request,
  }) => {
    const repo = makeScratchRepo({ "f.txt": "hi\n" });
    const streamId = await openStream(request, repo, `e2e-dock-default-${Date.now()}`);

    await gotoStudio(page, streamId);

    // Alone, the conversation shows no tab strip: its tab is in the DOM, hidden.
    await expect(page.locator(".dv-default-tab", { hasText: "Orchestrator" })).toBeAttached({ timeout: 15_000 });
    // Nothing else is present on first paint — Code/Review/Memory/Events all
    // open on demand now, never auto-added to a fresh socket's layout.
    await expect(page.locator(".dv-default-tab", { hasText: "Code" })).toHaveCount(0);
    await expect(page.locator(".dv-default-tab", { hasText: "Review queue" })).toHaveCount(0);
    await expect(page.locator(".dv-default-tab", { hasText: "Memory" })).toHaveCount(0);
    await expect(page.locator(".dv-default-tab", { hasText: "Event queue" })).toHaveCount(0);
  });

  test("a lone panel shows no tab strip; a second panel brings the strips back, and closing it hides them again", async ({
    page,
    request,
  }) => {
    const repo = makeScratchRepo({ "f.txt": "hi\n" });
    const streamId = await openStream(request, repo, `e2e-dock-strip-${Date.now()}`);

    await gotoStudio(page, streamId);
    const orchTab = page.locator(".dv-default-tab", { hasText: "Orchestrator" });
    await expect(orchTab).toBeAttached({ timeout: 15_000 });
    await expect(orchTab).toBeHidden();

    await openMemoryPanel(page);
    const memTab = page.locator(".dv-default-tab", { hasText: "Memory & context" });
    await expect(memTab).toBeVisible();
    await expect(orchTab).toBeVisible();

    await memTab.hover();
    await memTab.locator(".dv-default-tab-action").click();
    await expect(memTab).toHaveCount(0);
    await expect(orchTab).toBeAttached();
    await expect(orchTab).toBeHidden();
  });

  // 07d-A0 bug report: `missingSingletons`/`missingChatRoles` only read
  // `dockApi.getPanel(...)` and `studioStore.data` — `dockApi` itself is a
  // plain variable, not `$state`, so closing a panel alone (no other
  // studioStore-refreshing action in between) must not leave the "+ Panel"
  // menu showing stale, already-open state forever.
  test("closing the only open chat pane makes it reappear in '+ Panel' immediately, with no other action in between", async ({
    page,
    request,
  }) => {
    const repo = makeScratchRepo({ "f.txt": "hi\n" });
    const streamId = await openStream(request, repo, `e2e-dock-reopen-menu-${Date.now()}`);

    await gotoStudio(page, streamId);
    const orchTab = page.locator(".dv-default-tab", { hasText: "Orchestrator" });
    await expect(orchTab).toBeAttached({ timeout: 15_000 });

    // A lone panel has no tab strip, so no close button: open Memory beside
    // it first. Orchestrator is still the only chat pane.
    await openMemoryPanel(page);
    await expect(orchTab).toBeVisible();

    await orchTab.locator(".dv-default-tab-action").click();
    await expect(page.locator('[data-testid="agent-panel"]')).toHaveCount(0);

    await page.getByRole("button", { name: "+ Panel" }).click();
    await expect(page.getByRole("button", { name: "Orchestrator", exact: true })).toBeVisible();
  });

  test("opening Code places it to the right of the conversation; a new Terminal defaults along the bottom (07c-UX-SHELL §2.3)", async ({
    page,
    request,
  }) => {
    const repo = makeScratchRepo({ "f.txt": "hi\n" });
    const streamId = await openStream(request, repo, `e2e-dock-placement-${Date.now()}`);

    await gotoStudio(page, streamId);
    const orchTab = page.locator(".dv-default-tab", { hasText: "Orchestrator" });
    await expect(orchTab).toBeAttached({ timeout: 15_000 });

    await openCodePanel(page);
    const codeTab = page.locator(".dv-default-tab", { hasText: "Code" });
    await expect(codeTab).toBeVisible();
    const codeBox = (await codeTab.boundingBox())!;
    // Alone, the conversation had no tab strip to measure; with Code open
    // its tab shows again.
    await expect(orchTab).toBeVisible();
    const orchBox = (await orchTab.boundingBox())!;

    // "Right of the conversation": Code's tab starts to the right of the
    // whole Orchestrator tab, on roughly the same row.
    expect(codeBox.x).toBeGreaterThan(orchBox.x + orchBox.width);
    expect(Math.abs(codeBox.y - orchBox.y)).toBeLessThan(10);

    await page.getByRole("button", { name: "+ Panel" }).click();
    await page.getByRole("button", { name: "New room terminal", exact: true }).click();
    const termTab = page.locator(".dv-default-tab", { hasText: "room" });
    await expect(termTab).toBeVisible({ timeout: 15_000 });
    const termBox = (await termTab.boundingBox())!;

    // "Along the bottom": the terminal's row sits below both the
    // conversation and Code, not tabbed into either.
    expect(termBox.y).toBeGreaterThan(orchBox.y + orchBox.height);
    expect(termBox.y).toBeGreaterThan(codeBox.y + codeBox.height);
  });

  test("switching sockets in-app restores each one's own layout with no stale panels", async ({
    page,
    request,
  }) => {
    const repoA = makeScratchRepo({ "f.txt": "hi\n" });
    const streamA = await openStream(request, repoA, `e2e-dock-switch-a-${Date.now()}`);
    const repoB = makeScratchRepo({ "f.txt": "hi\n" });
    const streamB = await openStream(request, repoB, `e2e-dock-switch-b-${Date.now()}`);

    await gotoStudio(page, streamA);
    await expect(page.locator(".dv-default-tab", { hasText: "Orchestrator" })).toBeAttached({ timeout: 15_000 });
    await openCodePanel(page);
    await expect(page.locator(".dv-default-tab", { hasText: "Code" })).toBeVisible();
    await page.waitForTimeout(500); // debounced persist

    // Live in-app switch — not a reload — to the socket that never opened Code.
    await gotoStudio(page, streamB);
    await expect(page.locator(".dv-default-tab", { hasText: "Orchestrator" })).toBeAttached({ timeout: 15_000 }); // alone: no tab strip
    await expect(page.locator(".dv-default-tab", { hasText: "Code" })).toHaveCount(0);

    // And back — A's own layout (with Code) is still there, not reset by
    // chat-alone's first-use default re-applying over a saved arrangement.
    await gotoStudio(page, streamA);
    await expect(page.locator(".dv-default-tab", { hasText: "Orchestrator" })).toBeVisible({ timeout: 15_000 });
    await expect(page.locator(".dv-default-tab", { hasText: "Code" })).toBeVisible();
  });

  test("dragging the sash between Orchestrator and Code resizes them, and a reload keeps the resized widths", async ({
    page,
    request,
  }) => {
    const repo = makeScratchRepo({ "f.txt": "hi\n" });
    const streamId = await openStream(request, repo, `e2e-dock-resize-${Date.now()}`);

    await gotoStudio(page, streamId);
    await expect(page.locator(".dv-default-tab", { hasText: "Orchestrator" })).toBeAttached({ timeout: 15_000 });
    await openCodePanel(page);
    const codeGroup = page.locator(".dv-groupview", { has: page.locator(".dv-default-tab", { hasText: "Code" }) });
    await expect(codeGroup).toBeVisible();

    const beforeBox = (await codeGroup.boundingBox())!;
    const sash = page.locator(".dv-sash-container .dv-sash").first();
    const sashBox = (await sash.boundingBox())!;
    const dragBy = 120;

    // A real drag: mouse down on the sash, move it right in small steps
    // (dockview's sash needs intermediate move events, not a single jump),
    // then release — growing Orchestrator and shrinking Code.
    await page.mouse.move(sashBox.x + sashBox.width / 2, sashBox.y + sashBox.height / 2);
    await page.mouse.down();
    await page.mouse.move(sashBox.x + sashBox.width / 2 + dragBy, sashBox.y + sashBox.height / 2, { steps: 10 });
    await page.mouse.up();

    const afterDragBox = (await codeGroup.boundingBox())!;
    const shrunkBy = beforeBox.width - afterDragBox.width;
    expect(shrunkBy).toBeGreaterThan(dragBy - 40); // real resize happened, not a no-op

    // Give the debounced persist (300ms) time to land before reloading.
    await page.waitForTimeout(500);
    await page.reload();

    await expect(page.locator(".dv-default-tab", { hasText: "Orchestrator" })).toBeVisible({ timeout: 15_000 });
    const codeGroupAfterReload = page.locator(".dv-groupview", { has: page.locator(".dv-default-tab", { hasText: "Code" }) });
    await expect(codeGroupAfterReload).toBeVisible();
    const afterReloadBox = (await codeGroupAfterReload.boundingBox())!;

    // The resized width survives the reload — not reset back to the
    // pre-drag default, and not some unrelated arbitrary width.
    expect(Math.abs(afterReloadBox.width - afterDragBox.width)).toBeLessThan(40);
  });

  test("adding a panel and reloading restores it; a different socket keeps its own layout", async ({ page, request }) => {
    const repoA = makeScratchRepo({ "f.txt": "hi\n" });
    const streamA = await openStream(request, repoA, `e2e-dock-persist-a-${Date.now()}`);
    const repoB = makeScratchRepo({ "f.txt": "hi\n" });
    const streamB = await openStream(request, repoB, `e2e-dock-persist-b-${Date.now()}`);

    await gotoStudio(page, streamA);
    await expect(page.locator(".dv-default-tab", { hasText: "Orchestrator" })).toBeAttached({ timeout: 15_000 });

    // Add a panel that isn't in the default set.
    await page.getByRole("button", { name: "+ Panel" }).click();
    await page.getByRole("button", { name: "Memory & context", exact: true }).click();
    await expect(page.locator(".dv-default-tab", { hasText: "Memory & context" })).toBeVisible();

    // Give the debounced persist (300ms) time to land before reloading.
    await page.waitForTimeout(500);
    await page.reload();

    await expect(page.locator(".dv-default-tab", { hasText: "Orchestrator" })).toBeVisible({ timeout: 15_000 });
    await expect(page.locator(".dv-default-tab", { hasText: "Memory & context" })).toBeVisible();

    // A different socket never saw this change — its own default layout only.
    await gotoStudio(page, streamB);
    await expect(page.locator(".dv-default-tab", { hasText: "Orchestrator" })).toBeAttached({ timeout: 15_000 }); // alone: no tab strip
    await expect(page.locator(".dv-default-tab", { hasText: "Memory & context" })).toHaveCount(0);
  });

  // Non-destructive-panels follow-up: Memory/Review used to open tabbed
  // INTO whatever panel was active, hiding it — not just backgrounding it,
  // with no way to get back to it except reopening it from "+ Panel" and no
  // focus-restore on close. This proves the fix end to end: beside (not
  // over), a working close affordance, closing restores focus to Chat, and
  // the closed state survives reload.
  test("opening Memory sits beside Chat without hiding it; closing it (as a background panel) leaves Chat focused, and the closed state survives reload", async ({
    page,
    request,
  }) => {
    const repo = makeScratchRepo({ "f.txt": "hi\n" });
    const streamId = await openStream(request, repo, `e2e-dock-close-memory-${Date.now()}`);
    await seatAssign(request, streamId, "orchestrator", "grok");
    await fakeAgentProjection(page, streamId, "orchestrator", { assigned_model_id: "grok", socket_ready: true });

    await gotoStudio(page, streamId);
    const orchTab = page.locator(".dv-default-tab", { hasText: "Orchestrator" });
    await expect(orchTab).toBeAttached({ timeout: 15_000 }); // alone: no tab strip yet

    await openMemoryPanel(page);
    const memTab = page.locator(".dv-default-tab", { hasText: "Memory & context" });
    await expect(memTab).toBeVisible();

    // Beside, not over: both tabs, and both underlying panel bodies, are
    // visible at once — proves non-destructive placement.
    await expect(orchTab).toBeVisible();
    await expect(page.locator('[data-testid="agent-panel"]')).toBeVisible();
    await expect(page.locator(".mem-w")).toBeVisible();
    const orchBox = (await orchTab.boundingBox())!;
    const memBox = (await memTab.boundingBox())!;
    expect(memBox.x).toBeGreaterThan(orchBox.x + orchBox.width);

    // Return focus to Chat.
    await orchTab.click();
    await expect(page.locator(".mem-w")).toBeVisible(); // still there, just inactive

    // Close Memory via its own tab's built-in close button — it's inactive,
    // so dockview's own CSS hides its close action until hovered.
    await memTab.hover();
    await memTab.locator(".dv-default-tab-action").click();
    await expect(page.locator(".mem-w")).toHaveCount(0);

    // Closing a background panel must not steal focus from Chat.
    const textarea = page.locator('[data-testid="composer"] textarea');
    await expect(textarea).toBeVisible();
    await expect(textarea).toBeEditable();

    await page.waitForTimeout(500); // debounced persist
    await page.reload();

    // Chat is alone again, so its tab strip stays hidden.
    await expect(orchTab).toBeAttached({ timeout: 15_000 });
    await expect(page.locator('[data-testid="composer"] textarea')).toBeEditable();
    await expect(page.locator(".dv-default-tab", { hasText: "Memory & context" })).toHaveCount(0);
  });

  test("closing the currently active panel restores focus to whatever was active before it", async ({
    page,
    request,
  }) => {
    const repo = makeScratchRepo({ "f.txt": "hi\n" });
    const streamId = await openStream(request, repo, `e2e-dock-restore-focus-${Date.now()}`);
    await seatAssign(request, streamId, "orchestrator", "grok");
    await fakeAgentProjection(page, streamId, "orchestrator", { assigned_model_id: "grok", socket_ready: true });

    await gotoStudio(page, streamId);
    await expect(page.locator(".dv-default-tab", { hasText: "Orchestrator" })).toBeAttached({ timeout: 15_000 });

    await openMemoryPanel(page);
    const memTab = page.locator(".dv-default-tab", { hasText: "Memory & context" });
    await expect(memTab).toBeVisible(); // freshly opened => already active, close action visible

    await memTab.locator(".dv-default-tab-action").click();
    await expect(memTab).toHaveCount(0);

    // Focus must land back on Chat automatically, not on nothing.
    await expect(page.locator('[data-testid="composer"] textarea')).toBeEditable();
  });

  test("the title bar's Review and Code buttons are toggles: pressed while open, and a click closes them", async ({
    page,
    request,
  }) => {
    const repo = makeScratchRepo({ "f.txt": "hi\n" });
    const streamId = await openStream(request, repo, `e2e-dock-toggles-${Date.now()}`);

    await gotoStudio(page, streamId);
    await expect(page.locator(".dv-default-tab", { hasText: "Orchestrator" })).toBeAttached({ timeout: 15_000 });

    for (const [name, tab] of [["Review panel", "Review queue"], ["Code panel", "Code"]] as const) {
      const toggle = page.getByRole("button", { name, exact: true });
      await expect(toggle).toHaveAttribute("aria-pressed", "false");
      await toggle.click();
      await expect(page.locator(".dv-default-tab", { hasText: tab })).toBeVisible();
      await expect(toggle).toHaveAttribute("aria-pressed", "true");
      await toggle.click();
      await expect(page.locator(".dv-default-tab", { hasText: tab })).toHaveCount(0);
      await expect(toggle).toHaveAttribute("aria-pressed", "false");
    }
  });
});
