// Real-browser proof for the 07d-A0 Studio-recovery checkpoint: a socket
// carrying a real pre-07d-A-AGENT-IDENTITY saved layout (the old fixed
// "orchestrator" singleton panel, under the v2 layout key) must still open
// with a working Orchestrator conversation — never a blank pane — and the
// full pane lifecycle (open, send, close, reopen, drag, reload) must work
// on it exactly as it would on a brand-new socket. Same harness as
// editor.spec.ts (see playwright.config.ts / helpers.ts).
import { expect, type Page } from "@playwright/test";
import { test } from "./fixtures";
import { makeScratchRepo, openStream, toHandle, seatAssign, fakeAgentProjection, openMemoryPanel } from "./helpers";

async function gotoStudio(page: Page, streamId: string) {
  await page.goto(`/w/${toHandle(streamId)}`);
}

/** A plausible serialization of the pre-07d-A default layout: one fixed
 *  singleton panel, id and component both the literal string
 *  "orchestrator" (paneState.ts's old `WIDGET_IDS`/`panelIdFor` before this
 *  checkpoint). `DockviewHost.svelte`'s `createComponent` no longer has a
 *  case for that component at all — restoring this for real (if the v3 key
 *  bump did not exist) would render a blank panel. */
function legacyV2Layout() {
  return {
    grid: {
      root: {
        type: "leaf",
        data: { views: ["orchestrator"], activeView: "orchestrator", id: "group-1" },
      },
      width: 100,
      height: 100,
      orientation: "HORIZONTAL",
    },
    panels: {
      orchestrator: { id: "orchestrator", contentComponent: "orchestrator", title: "Orchestrator" },
    },
    activeGroup: "group-1",
  };
}

test.describe("07d-A0: Studio recovery from a pre-07d-A layout", () => {
  test("open, send, close, reopen, drag, and reload all work on a socket carrying a legacy v2 layout", async ({
    page,
    request,
  }) => {
    const repo = makeScratchRepo({ "f.txt": "hi\n" });
    const streamId = await openStream(request, repo, `e2e-recovery-${Date.now()}`);
    await seatAssign(request, streamId, "orchestrator", "grok");

    // Seed a REAL pre-07d-A v2-keyed localStorage entry before the page's
    // own scripts ever run — exactly what a returning user's browser would
    // already hold. `layoutKey()` is `bakery.layout.v2.${stream_id}` at
    // that version; hardcoded here (not imported) because the whole point
    // is to model what actually got written under the OLD scheme, which
    // must not silently track future renames of the current version.
    await page.addInitScript(
      ([key, value]) => {
        window.localStorage.setItem(key, value);
      },
      [`bakery.layout.v2.${streamId}`, JSON.stringify(legacyV2Layout())],
    );

    // Fake readiness — this e2e daemon has no real Grok credentials.
    await fakeAgentProjection(page, streamId, "orchestrator", { assigned_model_id: "grok", socket_ready: true });

    // ---- open: the legacy v2 entry is invisible under v3 — a fresh,
    // correct default layout applies instead of restoring the dead
    // "orchestrator" component shape.
    await gotoStudio(page, streamId);
    const orchTab = page.locator(".dv-default-tab", { hasText: "Orchestrator" });
    await expect(orchTab).toBeAttached({ timeout: 15_000 }); // alone: no tab strip
    await expect(page.locator(".dv-default-tab")).toHaveCount(1);
    await expect(page.locator('[data-testid="composer"]')).toBeVisible();
    await expect(page.locator('[data-testid="configure-card"]')).toHaveCount(0);

    // ---- send: a real POST /agent/turn round-trip: the human's own line
    // lands in the log for real (the turn itself will go on to fail server
    // side — no real Grok credentials here either — but that is a
    // completion detail already covered by the Rust-level tests; this
    // proves the send PATH itself works end to end through the real API).
    const marker = `recovery check ${Date.now()}`;
    await page.locator('[data-testid="composer"] textarea').fill(marker);
    await page.locator('[data-testid="send"]').click();
    const sentLine = page.locator('[data-testid="message-body"]', { hasText: marker });
    await expect(sentLine).toBeVisible({ timeout: 15_000 });

    // ---- close: a view close, not a cancel — the pane disappears. A lone
    // pane has no tab strip (so no close button): open Memory beside it
    // first. Memory stays open for the rest of the test.
    await openMemoryPanel(page);
    await expect(orchTab).toBeVisible();
    await orchTab.locator(".dv-default-tab-action").click();
    await expect(page.locator('[data-testid="agent-panel"]')).toHaveCount(0);

    // ---- reopen: through the real "+ Panel" menu, history intact. It tabs
    // in front of Memory (the active panel), so its tab strip shows.
    await page.getByRole("button", { name: "+ Panel" }).click();
    await page.getByRole("button", { name: "Orchestrator", exact: true }).click();
    await expect(page.locator(".dv-default-tab", { hasText: "Orchestrator" })).toBeVisible({ timeout: 15_000 });
    await expect(sentLine).toBeVisible();

    // ---- drag: open Code alongside it and resize the real sash between
    // them (same technique as dockview.spec.ts's own resize proof).
    await page.getByRole("button", { name: "+ Panel" }).click();
    await page.getByRole("button", { name: "Code", exact: true }).click();
    const codeGroup = page.locator(".dv-groupview", { has: page.locator(".dv-default-tab", { hasText: "Code" }) });
    await expect(codeGroup).toBeVisible();
    const beforeBox = (await codeGroup.boundingBox())!;
    const sash = page.locator(".dv-sash-container .dv-sash").first();
    const sashBox = (await sash.boundingBox())!;
    await page.mouse.move(sashBox.x + sashBox.width / 2, sashBox.y + sashBox.height / 2);
    await page.mouse.down();
    await page.mouse.move(sashBox.x + sashBox.width / 2 + 100, sashBox.y + sashBox.height / 2, { steps: 10 });
    await page.mouse.up();
    const afterDragBox = (await codeGroup.boundingBox())!;
    expect(beforeBox.width - afterDragBox.width).toBeGreaterThan(60); // a real resize happened

    // ---- reload: the layout just saved under v3 (this session's own,
    // correct shape) restores cleanly — Orchestrator, Code, and history
    // all still there. This is the "never repeat this recovery" half: once
    // through the one-time reset, ordinary persistence takes back over.
    await page.waitForTimeout(500);
    await page.reload();
    await expect(page.locator(".dv-default-tab", { hasText: "Orchestrator" })).toBeVisible({ timeout: 15_000 });
    await expect(page.locator(".dv-default-tab", { hasText: "Code" })).toBeVisible();
    await expect(page.locator('[data-testid="message-body"]', { hasText: marker })).toBeVisible();
  });

  test("an unrecognized restored panel component falls back to the default Chat, never a blank pane", async ({
    page,
    request,
  }) => {
    const repo = makeScratchRepo({ "f.txt": "hi\n" });
    const streamId = await openStream(request, repo, `e2e-unknown-component-${Date.now()}`);

    // Let the app create its own real, valid v3 layout first...
    await gotoStudio(page, streamId);
    await expect(page.locator(".dv-default-tab", { hasText: "Orchestrator" })).toBeAttached({ timeout: 15_000 });
    await page.waitForTimeout(500); // debounced persist

    // ...then corrupt just the one field that names the component, leaving
    // every other real dockview-produced field untouched — isolates
    // exactly the "unknown component" case DockviewHost.svelte's
    // `createComponent` must fail closed on, without hand-authoring a
    // low-level SerializedDockview fixture that might not match dockview's
    // real internal shape.
    const key = `bakery.layout.v3.${streamId}`;
    await page.evaluate((k) => {
      const raw = window.localStorage.getItem(k);
      if (!raw) throw new Error("expected a saved v3 layout before corrupting it");
      const parsed = JSON.parse(raw);
      for (const panel of Object.values(parsed.panels as Record<string, { contentComponent?: string }>)) {
        if (panel.contentComponent === "chat") panel.contentComponent = "mystery-widget-from-the-future";
      }
      window.localStorage.setItem(k, JSON.stringify(parsed));
    }, key);

    await page.reload();

    // Not blank: the default configured-agent Chat renders in its place.
    await expect(page.locator(".dv-default-tab")).toHaveCount(1, { timeout: 15_000 });
    await expect(page.locator('[data-testid="agent-panel"]')).toBeVisible();
    await expect(page.locator('[data-testid="configure-card"], [data-testid="composer"]')).toHaveCount(1);
  });
});
