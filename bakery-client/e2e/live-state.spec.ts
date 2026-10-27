// Real-browser proof for 07c-LIVE-STATE's "Proof (E2E, no reload)" section
// (docs/slices/07c-LIVE-STATE.md, items 1-8). Same harness as the other e2e
// specs (see playwright.config.ts / helpers.ts): a real scratch git repo +
// the real bakery-node daemon, no mocked network layer except where the
// proof itself asks for one (stale-response and forced-failure injection).
//
// Topology surfaces under test, and where they actually live (07d-A2
// restructured the shell — verified against the current route files):
//   - Sidebar:            bakery-client/src/lib/shell/Sidebar.svelte
//                         (permanent now — rendered by +layout.svelte on
//                         every route, not just /w*/baker*). Its Projects
//                         tree covers project sockets only; Baker/factory
//                         sockets no longer render in the sidebar at all
//                         (07d-A2 decision — they read as Floor content).
//   - Work boards:        the AI Elements board (src/lib/react/WorkBoard.tsx)
//                         on the Floor (`/`) and on the Chat home (`/chat`) —
//                         every conversation by status, with the standard
//                         actions menu (Delete… is the discard). The Chat
//                         sidebar lists the same conversations. The Floor's
//                         "Baker jobs" card is what the slice doc calls
//                         "open jobs".
//   - Baker conversations: the sidebar's Baker mode (Sidebar.svelte, fed by
//                         `SocketInfo.baker_thread` on `/studio/state` —
//                         BAKER-THREADS-03). Legacy `/baker/assign` branch
//                         jobs are no longer conversations; they surface only
//                         on the Floor's "Baker jobs" card, which discards
//                         them directly.
//
// Most of these tests need exactly one navigation and no reload at all —
// stronger than "revisit" and a direct proof of the slice's "no reload"
// header.
import { expect, type Page } from "@playwright/test";
import { test } from "./fixtures";
import {
  makeScratchRepo,
  openStream,
  openStreamInProject,
  projectCreate,
  bakerAssign,
  bakerThreadArchive,
  bakerThreadCreate,
  cleanupBakerAssignment,
  discardStream,
  toHandle,
  type BakerAssignResult,
} from "./helpers";

async function gotoFloor(page: Page) {
  await page.goto("/");
}

/** Chat home: the Chat sidebar and the chats board, both on one page. */
async function gotoChat(page: Page) {
  await page.goto("/chat");
}

/** A conversation's row on a work board (the Floor or the Chat home). */
function boardItem(page: Page, title: string) {
  return page.locator('[data-testid="work-item"]', { hasText: title });
}

/** A conversation's row in the Chat sidebar. */
function chatRow(page: Page, title: string) {
  return page.locator('[data-testid="chat-row"]', { hasText: title });
}

/** Deletes a conversation through the board's standard actions menu. */
async function deleteFromBoard(page: Page, title: string) {
  page.once("dialog", (d) => void d.accept());
  await boardItem(page, title).getByRole("button", { name: `Conversation actions: ${title}` }).click();
  await page.getByRole("menuitem", { name: "Delete…" }).click();
}

test.describe("07c-LIVE-STATE: one Studio projection", () => {
  // ---- 1. Same project, one truth, visible in both places at once --------

  test("a project stream is visible in both the Chat sidebar and the chats board", async ({
    page,
    request,
  }) => {
    const repo = makeScratchRepo({ "f.txt": "hi\n" });
    const name = `e2e-live-visible-${Date.now()}`;
    await openStream(request, repo, name);

    await gotoChat(page);

    await expect(chatRow(page, name)).toBeVisible({ timeout: 15_000 });
    await expect(boardItem(page, name)).toBeVisible({ timeout: 15_000 });
  });

  // ---- 1b (07d-A2 §2). Clicking a project name opens its Floor detail ----

  test("clicking a project's name in the sidebar opens its Floor project detail — the socket is right there, no reload", async ({
    page,
    request,
  }) => {
    // 07d-A3: a plain `/project/open` links to no Project (Ungrouped) — this
    // proof needs a real Project, explicitly linked, to have a `.proj-link`
    // row to click at all.
    const repo = makeScratchRepo({ "f.txt": "hi\n" });
    const name = `e2e-live-projdetail-${Date.now()}`;
    const projName = `e2e-live-projdetail-proj-${Date.now()}`;
    const proj = await projectCreate(request, projName);
    await openStreamInProject(request, repo, name, proj.projectId);

    await gotoFloor(page);
    const projLink = page.locator(".proj-link", { hasText: projName });
    await expect(projLink).toBeVisible({ timeout: 15_000 });

    await projLink.click();

    await expect(page).toHaveURL(new RegExp(`\\?project=${proj.projectId}`));
    await expect(page.locator(".page-hd h1", { hasText: projName })).toBeVisible();
    await expect(boardItem(page, name)).toBeVisible({ timeout: 15_000 });

    // Still the Floor, not a new app — the back link returns to it, and the
    // sidebar (never part of this view swap) still shows the same project.
    await expect(projLink).toBeVisible();
    await page.getByRole("link", { name: /back to the floor/i }).click();
    await expect(page).toHaveURL("/");
  });

  // ---- 2+3. Discard: gone from sidebar + floor recents, same tick --------

  test("deleting a project stream removes it from the Chat sidebar and the board together, no reload", async ({
    page,
    request,
  }) => {
    const repo = makeScratchRepo({ "f.txt": "hi\n" });
    const name = `e2e-live-discard-${Date.now()}`;
    await openStream(request, repo, name);

    await gotoChat(page);
    await expect(chatRow(page, name)).toBeVisible({ timeout: 15_000 });
    await expect(boardItem(page, name)).toBeVisible({ timeout: 15_000 });

    await deleteFromBoard(page, name);

    // Both surfaces read the one canonical store — no navigation, no reload.
    await expect(chatRow(page, name)).toHaveCount(0, { timeout: 15_000 });
    await expect(boardItem(page, name)).toHaveCount(0, { timeout: 15_000 });
  });

  // ---- 3 (Baker threads). The Baker's conversation list reacts live -------

  test("a Baker thread opened and archived behind the page's back shows and hides in the Baker sidebar, no reload", async ({
    page,
    request,
  }) => {
    await page.goto("/baker");
    const rows = page.locator('[data-testid="baker-thread-row"]');
    // The conversation already open — the main Baker — is itself listed
    // (the reported bug: an open conversation that never showed as a thread).
    await expect(rows.first()).toBeVisible({ timeout: 15_000 });

    // Opened through the node, exactly as the Baker's own `thread_create` op
    // would from another conversation: only the `baker.thread.opened`
    // topology event can surface it here — this page never refetches.
    const title = `e2e-live-thread-${Date.now()}`;
    const thread = await bakerThreadCreate(request, { title });
    const row = page.locator('[data-testid="baker-thread-row"]', { hasText: title });
    await expect(row).toBeVisible({ timeout: 15_000 });

    await bakerThreadArchive(request, thread.streamId, true);
    await expect(row).toHaveCount(0, { timeout: 15_000 });
    // Archived is kept, just out of the live list — on the Floor's archived
    // view, with every other archived chat and thread.
    await page.goto("/?view=archived");
    await expect(boardItem(page, title)).toBeVisible({ timeout: 15_000 });
  });

  // ---- 3b (07d-A2). The Floor's own "Baker jobs" card is a real Discard --

  test("the Floor's Baker jobs card discards a job stream directly, no reload", async ({ page, request }) => {
    const brief = `e2e-baker-floor-discard-${Date.now()}`;
    const assign = await bakerAssign(request, brief);
    try {
      await gotoFloor(page);

      const discardBtn = page.getByRole("button", { name: `Discard ${brief}`, exact: true });
      await expect(discardBtn).toBeVisible({ timeout: 15_000 });
      await discardBtn.click();
      await expect(discardBtn).toHaveCount(0, { timeout: 15_000 });
    } finally {
      cleanupBakerAssignment(assign);
    }
  });

  // ---- 4. Retired view still has the lineage ------------------------------

  test("the Floor's Retired view still surfaces a discarded socket — and it stays off the board", async ({
    page,
    request,
  }) => {
    const repo = makeScratchRepo({ "f.txt": "hi\n" });
    const name = `e2e-live-retired-${Date.now()}`;
    await openStream(request, repo, name);

    await gotoFloor(page);
    await expect(boardItem(page, name)).toBeVisible({ timeout: 15_000 });
    await deleteFromBoard(page, name);
    await expect(boardItem(page, name)).toHaveCount(0, { timeout: 15_000 });

    // 07d-A2 §2: Retired is a Floor view (`/?view=retired`), not a sidebar
    // section — the sidebar's Projects tree stays exactly as it was; only the
    // main content area swaps.
    await page.getByTitle(/retired — view on the floor/i).click();
    await expect(page).toHaveURL(/\/\?view=retired/);
    const retiredRow = page.locator(".retired-row", { hasText: name });
    await expect(retiredRow).toBeVisible({ timeout: 15_000 });

    // The sidebar's live Project tree is untouched by the Floor's own view
    // state — confirms Retired is a page-content swap, not a sidebar mode.
    await expect(page.locator(".section-name", { hasText: "Projects" })).toBeVisible();

    // The back link actually returns to the normal Floor — no dead click.
    await page.getByRole("link", { name: /back to the floor/i }).click();
    await expect(page).toHaveURL("/");
    await expect(page.locator(".retired-row")).toHaveCount(0);

    // Lineage is queryable, but never counts as active work.
    await expect(boardItem(page, name)).toHaveCount(0);
  });

  // ---- 5a. A stale, delayed /studio/state response cannot resurrect ------

  test("a stale, delayed /studio/state response cannot resurrect a socket a newer discard already retired", async ({
    page,
    request,
  }) => {
    const repo1 = makeScratchRepo({ "f.txt": "hi\n" });
    const name1 = `e2e-live-stale-target-${Date.now()}`;
    await openStream(request, repo1, name1);

    await gotoChat(page);
    const target = boardItem(page, name1);
    await expect(target).toBeVisible({ timeout: 15_000 });

    // Intercept the NEXT /studio/state round-trip: let the real request go
    // out and resolve now (capturing a genuinely pre-discard body), but hold
    // *delivery* to the page until the test explicitly releases it — that
    // models an in-flight topology refresh whose response lands late, after
    // a newer discard already committed locally. `pump()`'s in-flight
    // coalescing means only one real request is outstanding at a time, so
    // "the next call" is unambiguous here.
    let releaseStale: (() => void) | null = null;
    const staleHeld = new Promise<void>((resolve) => {
      releaseStale = resolve;
    });
    let resolveCaptured: (() => void) | null = null;
    const captured = new Promise<void>((resolve) => {
      resolveCaptured = resolve;
    });
    let firstSeen = false;
    await page.route("**/studio/state", async (route) => {
      if (firstSeen) {
        await route.continue();
        return;
      }
      firstSeen = true;
      const response = await route.fetch(); // real GET now — pre-discard truth
      resolveCaptured?.();
      await staleHeld; // held until the test releases it, below
      await route.fulfill({ response });
    });

    // Onboarding a second, unrelated stream fires `project.onboarded`, which
    // is a topology family the store subscribes to — this triggers the
    // refresh() that becomes the held request above.
    const repo2 = makeScratchRepo({ "g.txt": "yo\n" });
    const name2 = `e2e-live-stale-witness-${Date.now()}`;
    await openStream(request, repo2, name2);
    await captured; // the held response now reflects the world just before the discard below

    await deleteFromBoard(page, name1);
    // The local commit is instant and independent of any network response —
    // true immediately, before the stale response is ever released.
    await expect(target).toHaveCount(0);

    // Release the held response and let its own `route.fulfill()` continuation
    // run to completion BEFORE unrouting — calling `page.unroute()` right
    // after `releaseStale()` races that continuation (the resolved promise's
    // continuation is only scheduled, not yet run) and Playwright throws
    // "Route is already handled" when unroute beats it to the punch. The
    // `if (firstSeen) route.continue()` branch already makes every
    // subsequent request behave correctly without needing to unroute at all.
    releaseStale?.();

    // Give the generation-guarded follow-up refetch (fired by discard()'s own
    // generation bump) a moment to land, and confirm the row stays gone —
    // the late, older response must have been discarded on arrival, not
    // applied over the newer local truth.
    await expect(target).toHaveCount(0, { timeout: 15_000 });
    // Sanity: the rest of the projection is still healthy — the witness
    // stream from the held request's own snapshot is genuinely present.
    await expect(boardItem(page, name2)).toBeVisible({ timeout: 15_000 });
  });

  // ---- 5b. No parallel /streams inventory request -------------------------

  test("no active topology surface issues a parallel GET /streams request across a full load + discard", async ({
    page,
    request,
  }) => {
    const repo = makeScratchRepo({ "f.txt": "hi\n" });
    const name = `e2e-live-nostreams-${Date.now()}`;
    await openStream(request, repo, name);

    let streamsCalls = 0;
    await page.route("**/streams", (route) => {
      streamsCalls++;
      return route.continue();
    });

    await gotoFloor(page);
    await expect(boardItem(page, name)).toBeVisible({ timeout: 15_000 });

    await deleteFromBoard(page, name);
    await expect(boardItem(page, name)).toHaveCount(0, { timeout: 15_000 });

    // `GET /studio/state` is the client's only socket inventory now — a full
    // page load plus a discard must never fall back to a second, shadow
    // `/streams` fetch for topology.
    expect(streamsCalls).toBe(0);
  });

  // ---- 6. Forced discard failure never looks like success ----------------

  test("a forced discard failure clears retiring, keeps the row active, and shows an error", async ({
    page,
    request,
  }) => {
    const repo = makeScratchRepo({ "f.txt": "hi\n" });
    const name = `e2e-live-failed-discard-${Date.now()}`;
    await openStream(request, repo, name);

    await gotoFloor(page);
    await expect(boardItem(page, name)).toBeVisible({ timeout: 15_000 });

    await page.route("**/stream/discard", (route) =>
      route.fulfill({ status: 500, contentType: "text/plain", body: "forced e2e failure" }),
    );

    await deleteFromBoard(page, name);

    // Never looks like it succeeded: the row stays on the board with the
    // node's error on it, and Delete is usable again (the `retiring` overlay
    // must clear, not stick forever).
    await expect(boardItem(page, name).getByRole("status")).toContainText("forced e2e failure", {
      timeout: 15_000,
    });
    await expect(boardItem(page, name)).toBeVisible();
    await boardItem(page, name).getByRole("button", { name: `Conversation actions: ${name}` }).click();
    await expect(page.getByRole("menuitem", { name: "Delete…" })).toBeEnabled();
    await page.keyboard.press("Escape");

    await page.unroute("**/stream/discard");
  });

  // ---- 7. Onboarding a new socket appears live ----------------------------

  test("onboarding a new stream appears in the Chat sidebar and the board without a page reload", async ({
    page,
    request,
  }) => {
    const repo1 = makeScratchRepo({ "f.txt": "hi\n" });
    const name1 = `e2e-live-onboard-first-${Date.now()}`;
    await openStream(request, repo1, name1);

    await gotoChat(page);
    await expect(chatRow(page, name1)).toBeVisible({ timeout: 15_000 });

    // Onboarded through the API only — the page never navigates or reloads.
    const repo2 = makeScratchRepo({ "g.txt": "yo\n" });
    const name2 = `e2e-live-onboard-second-${Date.now()}`;
    await openStream(request, repo2, name2);

    await expect(chatRow(page, name2)).toBeVisible({ timeout: 15_000 });
    await expect(boardItem(page, name2)).toBeVisible({ timeout: 15_000 });
  });

  // ---- 8. Baker ship detail still works for the other active job ---------

  test("Baker ship detail still works for the other active job while one job's row disappears on discard", async ({
    page,
    request,
  }) => {
    const briefKeep = `e2e-baker-keep-${Date.now()}`;
    const briefGone = `e2e-baker-gone-${Date.now()}`;
    let keep: BakerAssignResult | null = null;
    let gone: BakerAssignResult | null = null;

    try {
      keep = await bakerAssign(request, briefKeep);
      gone = await bakerAssign(request, briefGone);

      // Legacy branch jobs are not Baker conversations: they surface only on
      // the Floor's "Baker jobs" card — a link to each job's studio, and its
      // Discard.
      await gotoFloor(page);
      const jobLink = (brief: string) => page.locator(".job-row a", { hasText: brief });
      await expect(jobLink(briefGone)).toBeVisible({ timeout: 15_000 });

      // Discarded behind the page's back — the same call the card's Discard
      // makes, same live-reactivity subject.
      await discardStream(request, gone.streamId);

      // The discarded job loses its link and its Discard (its branch may stay
      // listed, dimmed, as lineage), while the still-active job's row is
      // untouched throughout.
      await expect(jobLink(briefGone)).toHaveCount(0, { timeout: 10_000 });
      await expect(page.getByRole("button", { name: `Discard ${briefGone}`, exact: true })).toHaveCount(0);
      await expect(page.getByRole("button", { name: `Discard ${briefKeep}`, exact: true })).toBeVisible();

      // The OTHER, still-active job's detail still resolves: its row opens
      // its own studio.
      await jobLink(briefKeep).click();
      await expect(page).toHaveURL(new RegExp(`/w/${toHandle(keep.streamId)}$`));
      await expect(page.locator(".studio")).toHaveAttribute("aria-label", new RegExp(`${keep.branch}$`));
    } finally {
      if (keep) cleanupBakerAssignment(keep);
      if (gone) cleanupBakerAssignment(gone);
    }
  });
});
