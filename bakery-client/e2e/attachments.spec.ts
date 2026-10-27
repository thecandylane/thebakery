// Files, images and screenshots are always accepted — on every runtime, an
// HTTP (Grok) seat included — and an image is fitted to what every model
// reads before it is uploaded. Run in WebKit, the desktop app's engine, where
// the canvas re-encode actually happens.
import { expect, type Page } from "@playwright/test";
import { test } from "./fixtures";
import { fakeAgentProjection, makeScratchRepo, openStream, seatAssign, toHandle } from "./helpers";

test.use({ browserName: "webkit" });

/** A real `width`×`height` PNG, drawn by the page itself. */
async function pngOf(page: Page, width: number, height: number): Promise<Buffer> {
  const base64 = await page.evaluate(
    ([w, h]) => {
      const canvas = document.createElement("canvas");
      canvas.width = w;
      canvas.height = h;
      const context = canvas.getContext("2d")!;
      context.fillStyle = "#1f3a40";
      context.fillRect(0, 0, w, h);
      context.fillStyle = "#f8cd7a";
      context.font = "48px sans-serif";
      context.fillText("a 4K screenshot", 40, 80);
      return canvas.toDataURL("image/png").split(",")[1];
    },
    [width, height] as const,
  );
  return Buffer.from(base64, "base64");
}

/** Width and height from a PNG's IHDR chunk. */
function pngSize(bytes: Buffer): { width: number; height: number } {
  return { width: bytes.readUInt32BE(16), height: bytes.readUInt32BE(20) };
}

test("a screenshot on a Grok seat is accepted and uploaded as a PNG every model reads", async ({ page, request }) => {
  const streamId = await openStream(request, makeScratchRepo({ "a.txt": "x\n" }), `e2e-screenshot-${Date.now()}`);
  await seatAssign(request, streamId, "orchestrator", "grok");
  // The test node has no xAI key; the seat is marked ready so Send runs —
  // the upload, which is what is under test, happens before the turn.
  await fakeAgentProjection(page, streamId, "orchestrator", { assigned_model_id: "grok", socket_ready: true });
  const uploads: { filename: string; data_url: string }[] = [];
  await page.route("**/agent/attachments", async (route) => {
    uploads.push(route.request().postDataJSON());
    await route.continue();
  });
  await page.goto(`/w/${toHandle(streamId)}`);
  const composer = page.locator('[data-testid="composer"]');
  await expect(composer).toBeVisible({ timeout: 15_000 });

  await composer.locator('input[type="file"]').setInputFiles({
    name: "Screenshot 2026-10-02.png",
    mimeType: "image/png",
    buffer: await pngOf(page, 3000, 1500),
  });
  await expect(page.getByRole("button", { name: "Remove attachment" })).toHaveCount(1);
  await expect(page.locator('[data-testid="composer-notice"]')).toHaveCount(0);

  await composer.locator("textarea").fill("what is wrong in this screenshot?");
  await page.locator('[data-testid="send"]').click();
  await expect.poll(() => uploads.length, { timeout: 15_000 }).toBe(1);

  const [upload] = uploads;
  expect(upload.filename).toBe("Screenshot 2026-10-02.png");
  const [header, data] = upload.data_url.split(",");
  expect(header).toBe("data:image/png;base64");
  expect(pngSize(Buffer.from(data, "base64"))).toEqual({ width: 2000, height: 1000 });
});

test("the new-chat page accepts any file and says why one was not added", async ({ page }) => {
  await page.goto("/chat");
  const start = page.locator('[data-testid="chat-start"]');
  await expect(start).toBeVisible({ timeout: 15_000 });
  const fileInput = start.locator('input[type="file"]');

  await fileInput.setInputFiles([
    { name: "photo.png", mimeType: "image/png", buffer: Buffer.from("png") },
    { name: "spec.pdf", mimeType: "application/pdf", buffer: Buffer.from("%PDF-1.7") },
  ]);
  await expect(page.getByRole("button", { name: "Remove attachment" })).toHaveCount(2);

  await fileInput.setInputFiles({
    name: "huge.log",
    mimeType: "text/plain",
    buffer: Buffer.alloc(20 * 1024 * 1024 + 1, 97),
  });
  await expect(page.locator('[data-testid="chat-start-notice"]')).toContainText("maximum size");
  await expect(page.getByRole("button", { name: "Remove attachment" })).toHaveCount(2);
});
