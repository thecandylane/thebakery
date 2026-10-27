// Screenshot capture for STUDIO-AI-ELEMENTS-01's delivery, not a proof.
//
// Asserts nothing beyond "the panel rendered", and is skipped unless
// BAKERY_E2E_SHOTS=1 so it never slows the real suite. Kept in the tree so
// the images are reproducible rather than hand-taken:
//
//   BAKERY_E2E_SHOTS=1 BAKERY_E2E_SHOT_DIR=/some/dir \
//     pnpm exec playwright test e2e/screenshots.spec.ts
//
// Every state below is driven by the same synthetic fixtures the proofs
// use — no production state, no real account.

import fs from "node:fs";
import path from "node:path";
import type { APIRequestContext, Page } from "@playwright/test";
import { expect, test } from "./fixtures";
import { fakeAgentProjection, fakeEvent, makeScratchRepo, openStream, seatAssign, toHandle } from "./helpers";

const SHOT_DIR = process.env.BAKERY_E2E_SHOT_DIR ?? "screenshots";

test.skip(process.env.BAKERY_E2E_SHOTS !== "1", "set BAKERY_E2E_SHOTS=1 to capture");

test.beforeAll(() => {
  fs.mkdirSync(SHOT_DIR, { recursive: true });
});

const shot = (page: Page, name: string) => page.screenshot({ path: path.join(SHOT_DIR, `${name}.png`) });

/** A socket whose history covers every row kind the panel renders. */
async function seededSocket(page: Page, request: APIRequestContext, label: string) {
  const repo = makeScratchRepo({ "f.txt": "hi\n" });
  const streamId = await openStream(request, repo, `${label}-${Date.now()}`);
  await seatAssign(request, streamId, "orchestrator", "grok");
  await fakeAgentProjection(page, streamId, "orchestrator", { assigned_model_id: "grok", socket_ready: true });
  const agent = `${streamId}:orchestrator`;

  await page.route(`**/stream/${encodeURIComponent(streamId)}`, async (route) => {
    const response = await route.fetch();
    const body = await response.json();
    body.push(
      fakeEvent(
        streamId,
        "agent.turn.started",
        { agent_id: agent, instruction: "Tighten the greeting copy and show me the diff.", file_path: "f.txt" },
        { actor: "human" },
      ),
      fakeEvent(streamId, "agent.tool.called", {
        agent_id: agent,
        tool: "read_file",
        args: { path: "f.txt" },
        ok: true,
        summary: "read 1 line",
      }),
      fakeEvent(streamId, "agent.file.changed", {
        agent_id: agent,
        file_path: "f.txt",
        rationale: "greeting reads warmer as a full word",
        old_content: "hi\n",
        new_content: "hello\n",
      }),
      fakeEvent(streamId, "agent.turn.done", {
        agent_id: agent,
        turn_id: "shot-1",
        summary:
          "Proposed one change to `f.txt`.\n\n" +
          "The file held a bare `hi`, which reads clipped in the onboarding view:\n\n" +
          "```diff\n-hi\n+hello\n```\n\n" +
          "- the diff is one line\n" +
          "- nothing else in the worktree was touched\n\n" +
          "Open Review to accept, edit, or reject it.",
      }),
      fakeEvent(streamId, "agent.turn.usage", {
        agent_id: agent,
        turn_id: "shot-1",
        provider: "grok",
        model_id: "grok-4",
        effort: "high",
        tokens_in: 1840,
        tokens_out: 320,
        context_used: 24_500,
        context_window: 256_000,
      }),
    );
    await route.fulfill({ response, body: JSON.stringify(body) });
  });

  return streamId;
}

test.describe("STUDIO-AI-ELEMENTS-01 screenshots", () => {
  test("panel states", async ({ page, request }) => {
    await page.setViewportSize({ width: 1440, height: 900 });
    const streamId = await seededSocket(page, request, "shot");
    await page.goto(`/w/${toHandle(streamId)}`);
    await expect(page.locator('[data-testid="composer"]')).toBeVisible({ timeout: 20_000 });
    await expect(page.locator('[data-testid="message-body"]').first()).toBeVisible();
    await page.waitForTimeout(900); // let Shiki finish highlighting the fence
    await shot(page, "01-conversation-1440x900");

    // Composer with a slash menu open.
    await page.locator('[data-testid="composer"] textarea').fill("/");
    await expect(page.locator('[data-testid="slash-menu"]')).toBeVisible();
    await shot(page, "02-slash-menu");

    await page.locator('[data-testid="composer"] textarea').fill("");

    // Review opened beside chat from the change's own row.
    await page.getByRole("button", { name: "Open in Review" }).first().click();
    await expect(page.locator(".dv-default-tab", { hasText: "Review queue" })).toBeVisible();
    await page.waitForTimeout(900); // Monaco's diff editor
    await shot(page, "04-review-beside-chat");

    // A narrow pane — the toolbar must wrap, not overflow.
    await page.setViewportSize({ width: 900, height: 820 });
    await page.waitForTimeout(500);
    await shot(page, "05-narrow-pane");

    // Light theme, proving the token bridge holds in both directions.
    await page.setViewportSize({ width: 1440, height: 900 });
    await page.evaluate(() => localStorage.setItem("bakery.theme", "light"));
    // Reload rather than flipping the attribute in place: `app.html` applies
    // the stored theme before first paint, and the Monaco/Dockview bridges
    // pick it up on boot. Setting the attribute alone would leave those
    // engines dark and the screenshot would misrepresent a real switch.
    await page.reload();
    await expect(page.locator('[data-testid="composer"]')).toBeVisible({ timeout: 20_000 });
    await page.waitForTimeout(900);
    await shot(page, "06-light-theme");
  });

  test("live activity and the interrupted banner", async ({ page, request }) => {
    await page.setViewportSize({ width: 1100, height: 800 });
    const repo = makeScratchRepo({ "f.txt": "hi\n" });
    const streamId = await openStream(request, repo, `shot-live-${Date.now()}`);
    await seatAssign(request, streamId, "orchestrator", "grok");
    await fakeAgentProjection(page, streamId, "orchestrator", { assigned_model_id: "grok", socket_ready: true });
    const agent = `${streamId}:orchestrator`;

    await page.route("**/agent/activity*", async (route) => {
      await route.fulfill({
        json: {
          agent_id: agent,
          stream_id: streamId,
          turn_id: "shot-live",
          phase: {
            phase: "running_tool",
            tool: "grep",
            call_id: "c1",
            started_at: new Date().toISOString(),
            kind: "tool",
            preview: null,
          },
          turn_started_at: new Date().toISOString(),
          recent_completed_tools: [],
          usage: { tokens_in: 900, tokens_out: 120, context_used: 9_800, context_window: 256_000 },
          cancel_requested: false,
        },
      });
    });

    await page.goto(`/w/${toHandle(streamId)}`);
    await expect(page.locator('[data-testid="activity"]')).toBeVisible({ timeout: 20_000 });
    await shot(page, "07-running-tool-with-stop");

    // An instruction with no terminal row and nothing live: the recovery
    // banner the node-restart path produces.
    await page.unroute("**/agent/activity*");
    await page.route("**/agent/activity*", (route) => route.fulfill({ status: 404, body: "" }));
    await page.route(`**/stream/${encodeURIComponent(streamId)}`, async (route) => {
      const response = await route.fetch();
      const body = await response.json();
      body.push(
        fakeEvent(
          streamId,
          "agent.turn.started",
          { agent_id: agent, instruction: "Run the full test suite and summarise failures.", file_path: null },
          { actor: "human" },
        ),
      );
      await route.fulfill({ response, body: JSON.stringify(body) });
    });
    await page.reload();
    await expect(page.locator('[data-testid="resume-banner"]')).toBeVisible({ timeout: 20_000 });
    await shot(page, "08-interrupted-resume");
  });

  test("responsive Studio shell and secondary surfaces", async ({ page }) => {
    await page.setViewportSize({ width: 1440, height: 900 });
    await page.goto("/");
    await expect(page.getByRole("heading", { name: "Factory floor" })).toBeVisible({ timeout: 20_000 });
    await shot(page, "09-factory-floor-1440x900");

    await page.setViewportSize({ width: 800, height: 820 });
    await expect(page.locator('[data-testid="sidebar-toggle"]')).toHaveAttribute("aria-expanded", "false");
    await shot(page, "10-factory-floor-800x820");
    await page.locator('[data-testid="sidebar-toggle"]').click();
    await expect(page.locator('[data-testid="primary-sidebar"]')).toBeVisible();
    await page.waitForTimeout(250);
    await shot(page, "11-primary-drawer-800x820");

    await page.keyboard.press("Escape");
    await page.goto("/settings/providers");
    await expect(page.locator('[data-testid="settings-nav-toggle"]')).toBeVisible();
    await page.locator('[data-testid="settings-nav-toggle"]').click();
    await page.waitForTimeout(250);
    await shot(page, "12-settings-drawer-800x820");

    await page.keyboard.press("Escape");
    await page.goto("/baker");
    // Baker threads (BAKER-THREADS-03): the conversation list is the primary
    // drawer's Code mode, not a route-owned rail.
    await expect(page.locator('[data-testid="baker-page-new-thread"]')).toBeVisible({ timeout: 20_000 });
    await page.locator('[data-testid="sidebar-toggle"]').click();
    await expect(page.locator('[data-testid="baker-thread-row"]').first()).toBeVisible({ timeout: 15_000 });
    await page.waitForTimeout(250);
    await shot(page, "13-baker-conversations-800x820");
  });
});
