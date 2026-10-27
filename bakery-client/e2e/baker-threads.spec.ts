// Baker threads (docs/slices/BAKER-THREADS-03.md): one Baker, several
// parallel conversations. Real browser, real isolated bakery-node (same
// harness as every other spec — ephemeral ports, scratch HOME, never the
// operator's node). This daemon has no provider credentials, so these specs
// prove the conversation surface itself — list, create, rename, archive,
// restore, routing, live parallel status — and the op registry the Baker
// gets in every thread; the node's own tests prove dispatch and authority.
import { expect, type Page, type Route } from "@playwright/test";
import { test } from "./fixtures";
import {
  AUTH_HEADERS,
  NODE_BASE,
  bakerThreadCreate,
  makeScratchRepo,
  openStream,
  toHandle,
} from "./helpers";

const sidebar = (page: Page) => page.locator('[data-testid="primary-sidebar"]');
const rows = (page: Page) => page.locator('[data-testid="baker-thread-row"]');
const row = (page: Page, text: string) => rows(page).filter({ hasText: text });

/** Rewrites `/studio/state` for the given sockets — this daemon cannot run a
 *  real turn or produce a real proposal, so live parallel status is faked at
 *  the wire, the same technique as `fakeAgentProjection`. */
async function patchSockets(
  page: Page,
  patches: Record<string, { busy?: boolean; pending_gates?: number }>,
): Promise<void> {
  await page.route("**/studio/state", async (route: Route) => {
    const response = await route.fetch();
    const body = await response.json();
    for (const s of body.factory_sockets) {
      const p = patches[s.stream_id];
      if (!p) continue;
      if (p.pending_gates !== undefined) {
        s.pending_gates = p.pending_gates;
        s.status = "needs_input";
        s.status_note = `${p.pending_gates} changes to review`;
      }
      if (p.busy !== undefined) {
        for (const a of s.agents) a.busy = p.busy;
        if (p.busy) s.status = "working";
      }
    }
    await route.fulfill({ response, body: JSON.stringify(body) });
  });
}

test.beforeEach(async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 820 });
});

test.afterEach(async ({ page }) => {
  await page.unrouteAll({ behavior: "ignoreErrors" });
});

test.describe("Baker threads", () => {
  test("the open main conversation is listed, selected, and has no archive/rename controls", async ({ page }) => {
    await page.goto("/baker");
    // Attached, not visible: a lone panel's tab strip is hidden.
    await expect(page.locator(".dv-default-tab", { hasText: "Baker" })).toBeAttached({ timeout: 20_000 });

    const main = rows(page).first();
    await expect(main).toBeVisible({ timeout: 15_000 });
    await expect(main.getByRole("link")).toHaveAttribute("aria-current", "page");
    await expect(main.getByRole("link")).toHaveAttribute("href", "/baker");
    // The main conversation can be neither archived, renamed, nor deleted.
    await main.getByRole("button", { name: /Conversation actions/ }).click();
    for (const item of ["Archive", "Rename…", "Delete…"]) {
      await expect(page.getByRole("menuitem", { name: item })).toHaveCount(0);
    }
    await expect(page.getByRole("menuitem", { name: "Restart runtime" })).toBeVisible();
  });

  test("New thread opens another conversation, routes to it, and binds the studio to it", async ({ page }) => {
    await page.goto("/baker");
    await expect(rows(page).first()).toBeVisible({ timeout: 15_000 });
    const before = await rows(page).count();

    await sidebar(page).locator('[data-testid="baker-new-thread"]').click();
    await expect(page).toHaveURL(/\/baker\/[A-Za-z0-9_-]+$/, { timeout: 15_000 });
    await expect(page.locator('[data-testid="baker-thread-title"]')).toHaveText("New thread");
    await expect(rows(page)).toHaveCount(before + 1);

    // The new row is the selected one, and the studio rebound to it.
    const selected = rows(page).filter({ has: page.locator('[aria-current="page"]') });
    await expect(selected).toHaveCount(1);
    await expect(selected).toContainText("New thread");
    await expect(page.locator(".dv-default-tab", { hasText: "Baker" })).toBeAttached({ timeout: 20_000 });

    // The main conversation is still one click away and still first.
    await rows(page).first().getByRole("link").click();
    await expect(page).toHaveURL(/\/baker$/);
    await expect(page.locator('[data-testid="baker-thread-title"]')).toHaveCount(0);
  });

  test("a thread renames, archives out of the list, and restores", async ({ page, request }) => {
    const title = `e2e-thread-${Date.now()}`;
    const thread = await bakerThreadCreate(request, { title });
    await page.goto(`/baker/${toHandle(thread.streamId)}`);
    await expect(page.locator('[data-testid="baker-thread-title"]')).toHaveText(title, { timeout: 15_000 });

    const renamed = `${title}-renamed`;
    const r = row(page, title);
    await r.hover();
    page.once("dialog", (d) => void d.accept(renamed));
    await r.getByRole("button", { name: `Conversation actions: ${title}` }).click();
    await page.getByRole("menuitem", { name: "Rename…" }).click();
    await expect(row(page, renamed)).toBeVisible({ timeout: 15_000 });
    await expect(page.locator('[data-testid="baker-thread-title"]')).toHaveText(renamed);

    const r2 = row(page, renamed);
    await r2.hover();
    await r2.getByRole("button", { name: `Conversation actions: ${renamed}` }).click();
    await page.getByRole("menuitem", { name: "Archive" }).click();
    // It leaves the list; the page it was open on says so and restores it.
    await expect(row(page, renamed)).toHaveCount(0, { timeout: 15_000 });
    const banner = page.locator('[data-testid="baker-archived-banner"]');
    await expect(banner).toBeVisible();

    // No Archived list in the sidebar: archived chats and threads are the
    // Floor's archived view, restored from the same ⋯ actions menu.
    await page.goto("/?view=archived");
    const archivedRow = page.locator('[data-testid="work-item"]', { hasText: renamed });
    await expect(archivedRow).toBeVisible({ timeout: 15_000 });
    await archivedRow.getByRole("button", { name: `Conversation actions: ${renamed}` }).click();
    await page.getByRole("menuitem", { name: "Restore" }).click();
    await expect(archivedRow).toHaveCount(0, { timeout: 15_000 });
    // The Floor is Chat's; the restored thread is back in the Baker's list.
    await page.goto("/baker");
    await expect(row(page, renamed)).toBeVisible({ timeout: 15_000 });
  });

  test("an archived thread opened by URL says so and restores in place", async ({ page, request }) => {
    const title = `e2e-archived-${Date.now()}`;
    const thread = await bakerThreadCreate(request, { title });
    const resp = await request.post(`${NODE_BASE}/baker/threads/archive`, {
      headers: AUTH_HEADERS,
      data: { stream_id: thread.streamId, archived: true },
    });
    expect(resp.ok()).toBe(true);

    await page.goto(`/baker/${toHandle(thread.streamId)}`);
    const banner = page.locator('[data-testid="baker-archived-banner"]');
    await expect(banner).toBeVisible({ timeout: 15_000 });
    await banner.getByRole("button", { name: "Restore" }).click();
    await expect(banner).toHaveCount(0, { timeout: 15_000 });
    await expect(row(page, title)).toBeVisible();
  });

  test("parallel threads show working and needs-you status without being opened", async ({ page, request }) => {
    const stamp = Date.now();
    const running = await bakerThreadCreate(request, { title: `e2e-running-${stamp}` });
    const waiting = await bakerThreadCreate(request, { title: `e2e-waiting-${stamp}` });
    await patchSockets(page, {
      [running.streamId]: { busy: true },
      [waiting.streamId]: { pending_gates: 2 },
    });

    await page.goto("/baker");
    await expect(row(page, running.title)).toContainText("working", { timeout: 15_000 });
    await expect(row(page, waiting.title).locator(".gate-badge")).toHaveText("2");
    await expect(row(page, waiting.title).getByRole("link")).toHaveAccessibleName(
      `${waiting.title} — Baker · needs you: 2 changes to review`,
    );

    // The sidebar counts every conversation by status, and its filter lists
    // just the ones waiting — one click from there to the conversation.
    const filters = page.locator('[data-testid="status-filter"]');
    await expect(filters.getByRole("button", { name: /^Working, [1-9]/ })).toBeVisible();
    await filters.getByRole("button", { name: /^Needs you, / }).click();
    await expect(row(page, running.title)).toHaveCount(0);
    await row(page, waiting.title).getByRole("link").click();
    await expect(page).toHaveURL(new RegExp(`/baker/${toHandle(waiting.streamId)}$`));
    // Picking the filter that is on turns it off: every thread again.
    await filters.getByRole("button", { name: /^Needs you, / }).click();
    await expect(row(page, running.title)).toBeVisible();

    // A running thread cannot be archived out from under its turn.
    const r = row(page, running.title);
    await r.hover();
    await r.getByRole("button", { name: `Conversation actions: ${running.title}` }).click();
    await expect(page.getByRole("menuitem", { name: "Archive" })).toBeDisabled();
  });

  test("a handle that is not a Baker conversation is refused, never bound", async ({ page, request }) => {
    const repo = makeScratchRepo({ "a.txt": "x\n" });
    const projectSocket = await openStream(request, repo, `e2e-not-a-thread-${Date.now()}`);
    await page.goto(`/baker/${toHandle(projectSocket)}`);
    await expect(page.getByText(/doesn’t exist on this node/)).toBeVisible({ timeout: 15_000 });
    await expect(page.locator(".dv-default-tab")).toHaveCount(0);
  });

  test("every Baker conversation — main and thread — carries the thread/project toolbox", async ({ request }) => {
    const home = await (await request.get(`${NODE_BASE}/baker`, { headers: AUTH_HEADERS })).json();
    const thread = await bakerThreadCreate(request, { title: `e2e-caps-${Date.now()}` });
    for (const streamId of [home.stream_id as string, thread.streamId]) {
      const caps = await (
        await request.get(`${NODE_BASE}/baker/capabilities`, {
          headers: AUTH_HEADERS,
          params: { stream_id: streamId },
        })
      ).json();
      const names = (caps.baker_ops as { name: string }[]).map((o) => o.name);
      for (const op of ["thread_create", "thread_message", "thread_archive", "project_create", "project_retire"]) {
        expect(names, `${op} on ${streamId}`).toContain(op);
      }
    }
    // An ordinary project socket gets none of them.
    const repo = makeScratchRepo({ "a.txt": "x\n" });
    const plain = await openStream(request, repo, `e2e-caps-plain-${Date.now()}`);
    const plainCaps = await (
      await request.get(`${NODE_BASE}/baker/capabilities`, { headers: AUTH_HEADERS, params: { stream_id: plain } })
    ).json();
    expect(plainCaps.baker_ops).toEqual([]);
  });
});

test.describe("the Baker is its own mode", () => {
  test("Baker conversations never appear in Chat, and a Baker link in Chat hands off to the Baker", async ({
    page,
    request,
  }) => {
    const title = `e2e-not-in-chat-${Date.now()}`;
    const thread = await bakerThreadCreate(request, { title });

    await page.goto("/chat");
    await expect(page.locator('[data-testid="chat-home"]')).toBeVisible({ timeout: 15_000 });
    await expect(page.locator('[data-testid="chat-row"]', { hasText: title })).toHaveCount(0);
    await expect(page.locator('[data-testid="work-item"]', { hasText: title })).toHaveCount(0);
    await expect(page.getByRole("option", { name: /Baker/ })).toHaveCount(0);

    await page.goto(`/chat/${toHandle(thread.streamId)}`);
    await expect(page).toHaveURL(new RegExp(`/baker/${toHandle(thread.streamId)}$`), { timeout: 15_000 });
    await expect(page.locator('[data-testid="mode-baker"]')).toHaveAttribute("aria-selected", "true");
  });
});
