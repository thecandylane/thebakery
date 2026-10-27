// Real-browser proof for 07c-STUDIO-SHELL §4 + 07c-LIVE-STATE: a discarded
// socket disappears from the active sidebar and its open workbench navigates
// away rather than sitting on a dead /w/[sid] (07c-LIVE-STATE: "do not leave
// a dead /w/[sid]" — this supersedes 07c-STUDIO-SHELL's original "stay and
// show a retired banner" design), while its retired lineage stays queryable
// on the Factory Floor's separate Retired view (07d-A2 §2 moved this off the
// sidebar and onto `/?view=retired`; never erased, see StreamRef.discarded /
// stream.discarded in bakery-node). Same harness as editor.spec.ts (see
// playwright.config.ts / helpers.ts). Baker's open-jobs exclusion is proven
// server-side by baker_work_list_excludes_a_discarded_stream in baker.rs,
// and client-side (instant, same-tick) by live-state.spec.ts.
import { expect, type Page } from "@playwright/test";
import { test } from "./fixtures";
import { makeScratchRepo, openStream, toHandle } from "./helpers";

async function gotoStudio(page: Page, streamId: string) {
  await page.goto(`/w/${toHandle(streamId)}`);
}

test.describe("Studio discard consistency", () => {
  test("discarding removes the active sidebar row and retires the open workbench, without erasing lineage", async ({
    page,
    request,
  }) => {
    const repo = makeScratchRepo({ "f.txt": "hi\n" });
    const name = `e2e-discard-${Date.now()}`;
    const streamId = await openStream(request, repo, name);

    await gotoStudio(page, streamId);

    // The sidebar's socket row discards through its standard ⋯ actions menu
    // (Delete…, confirmed), which replaced the row's lone discard ×.
    const sidebarRows = page.locator(`[data-testid="primary-sidebar"] [data-stream-id="${streamId}"]`);
    const actions = sidebarRows
      .and(page.locator(".session-row"))
      .getByRole("button", { name: `Conversation actions: ${name}` });
    await expect(actions).toBeVisible({ timeout: 15_000 });

    const deadHandle = toHandle(streamId);
    expect(page.url()).toContain(`/w/${deadHandle}`);

    page.once("dialog", (d) => void d.accept());
    await actions.click();
    await page.getByRole("menuitem", { name: "Delete…" }).click();

    // The still-open workbench must stop presenting this as a live session —
    // no manual reload needed to learn its own stream was just discarded.
    // 07c-LIVE-STATE: rather than staying and showing a "retired" banner,
    // it navigates away entirely (to another active socket, or the floor
    // if none exist) — "do not leave a dead /w/[sid]". Which one it lands
    // on isn't deterministic here (other E2E specs sharing this daemon may
    // leave other active sockets around), so the only reliable assertion is
    // that the URL no longer names *this* now-discarded stream.
    await expect(async () => {
      expect(page.url()).not.toContain(`/w/${deadHandle}`);
    }).toPass({ timeout: 15_000 });

    // Gone from the active sidebar — no leftover "still running" row, in
    // Ungrouped or in Chats.
    await expect(sidebarRows).toHaveCount(0);

    // Lineage is not erased: the Factory Floor's Retired view (07d-A2 §2 —
    // a separate page view, not an inline reveal in the sidebar's live tree)
    // still surfaces it — a plain, non-discardable row, never silently
    // dropped.
    await page.getByTitle(/retired — view on the floor/i).click();
    await expect(page).toHaveURL(/\/\?view=retired/);
    const retiredRow = page.locator(".retired-row", { hasText: name });
    await expect(retiredRow).toBeVisible();
  });
});
