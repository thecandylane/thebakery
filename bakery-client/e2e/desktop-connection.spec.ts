// The connection screen, proven in a real browser.
//
// This file deliberately imports the RAW Playwright `test`, not the
// harness fixture in `./fixtures` — its pages therefore have no injected
// credential and no Tauri IPC, which is precisely the "cannot connect"
// state the screen exists for. Every other spec uses the fixture.
//
// The native bootstrap itself (reuse, start, restart, invalidated
// credential, trusted-window scoping) is proven in Rust —
// `bakery-client/src-tauri/src/connect.rs`'s tests — because a browser
// with an injected token exercises none of that path.
import { expect, test } from "@playwright/test";

test.describe("desktop connection screen", () => {
  test("without a native connection the app shows one connection screen, not a broken studio", async ({ page }) => {
    const failures: string[] = [];
    page.on("response", (r) => {
      if (r.status() === 401 || r.status() === 403) failures.push(`${r.status()} ${r.url()}`);
    });

    await page.goto("/");

    // One screen, with the reason and a Retry — not a half-rendered shell.
    await expect(page.getByTestId("connection-screen")).toBeVisible();
    await expect(page.getByTestId("connection-failed")).toBeVisible();
    await expect(page.getByTestId("connection-message")).toContainText("desktop application");
    await expect(page.getByTestId("connection-retry")).toBeEnabled();

    // The studio chrome must not be behind it.
    await expect(page.locator(".app-shell")).toHaveCount(0);

    // And nothing went looking for authenticated routes: the shell never
    // starts its subscriptions before it has a credential, so there are no
    // scattered authorization errors to confuse the human.
    expect(failures, `unexpected authorization noise: ${failures.join(", ")}`).toEqual([]);
  });

  test("Retry re-attempts the connection and keeps explaining itself", async ({ page }) => {
    await page.goto("/");
    await expect(page.getByTestId("connection-failed")).toBeVisible();

    await page.getByTestId("connection-retry").click();

    // Still no way to connect from a browser tab, so the same honest
    // screen — and never a stuck spinner or a blank page.
    await expect(page.getByTestId("connection-screen")).toBeVisible();
    await expect(page.getByTestId("connection-retry")).toBeEnabled();
  });
});
