import { expect } from "@playwright/test";
import { test } from "./fixtures";
import { NODE_BASE } from "./helpers";

test.describe("Provider accounts", () => {
  test("Claude Code is an account flow and raw adapter fields stay advanced", async ({ page }) => {
    let linked = false;

    await page.addInitScript(() => {
      window.open = () => null;
    });
    await page.route(`${NODE_BASE}/providers`, async (route) => {
      await route.fulfill({
        contentType: "application/json",
        body: JSON.stringify({ acp_plugs: [], openai_compat_plugs: [] }),
      });
    });
    await page.route(`${NODE_BASE}/studio/state`, async (route) => {
      await route.fulfill({
        contentType: "application/json",
        body: JSON.stringify({
          model_options: [],
          role_options: [],
          projects: [],
          ungrouped_sockets: [],
          factory_sockets: [],
        }),
      });
    });
    await page.route(`${NODE_BASE}/info`, async (route) => {
      await route.fulfill({
        contentType: "application/json",
        body: JSON.stringify({ write_mode: "manual" }),
      });
    });
    await page.route(`${NODE_BASE}/auth/status`, async (route) => {
      await route.fulfill({
        contentType: "application/json",
        body: JSON.stringify({
          active_model: linked ? "claude-code" : "auto",
          resolved_hint: linked ? "claude-code" : "auto→stub",
          xai: { api_key_set: false, oauth_set: false },
          gemini: { api_key_set: false, oauth_set: false },
          anthropic: { api_key_set: false, oauth_set: linked },
        }),
      });
    });
    await page.route(`${NODE_BASE}/auth/claude-code/login/start`, async (route) => {
      await route.fulfill({
        contentType: "application/json",
        body: JSON.stringify({
          verification_uri: "https://claude.ai/test-only-login",
          built: true,
        }),
      });
    });
    await page.route(`${NODE_BASE}/auth/claude-code/login/complete`, async (route) => {
      expect(route.request().postDataJSON()).toEqual({ callback: "test-only-callback" });
      linked = true;
      await route.fulfill({
        contentType: "application/json",
        body: JSON.stringify({ status: "complete" }),
      });
    });

    await page.goto("/settings/providers");

    const claude = page.locator("li.row").filter({ hasText: "Claude Code / Anthropic" });
    await expect(claude.getByRole("button", { name: "Sign in with Claude Code" })).toBeVisible();

    const advanced = page.locator("details.advanced");
    await expect(advanced).not.toHaveAttribute("open");
    await expect(page.getByLabel("argv")).toBeHidden();

    await claude.getByRole("button", { name: "Sign in with Claude Code" }).click();
    await claude.getByPlaceholder("paste callback URL or code").fill("test-only-callback");
    await claude.getByRole("button", { name: "Finish sign-in" }).click();
    await expect(claude).toContainText("Linked");
    await expect(page.getByText("Claude Code login complete — model set to claude-code")).toBeVisible();

    await advanced.getByText("Advanced adapters", { exact: true }).click();
    await advanced.getByRole("button", { name: "+ Add ACP plug" }).click();
    await expect(page.getByLabel("argv")).toBeVisible();
    await expect(page.getByLabel("config source")).toBeVisible();
    await expect(page.getByLabel("config dest")).toBeVisible();
  });

  // 07d-A3D1: Codex/Cursor as first-class accounts, not raw ACP config.
  // This node has no provider credentials and no real codex/cursor-agent
  // login flow reachable from a browser test, so /auth/cli-status,
  // /terminal/create|ticket and the WS attach are all mocked at the
  // network boundary — same pattern the Claude Code case above already
  // uses for /auth/claude-code/*. The point of this spec is the CLIENT
  // wiring (the right request — no client-chosen name, per review finding 1
  // — the embedded terminal, the row updating once the login pane closes
  // itself) — real accept/reject-shaped backend correctness (the idempotent
  // reuse-by-command, the narrowed login environment, the CLI status probe,
  // the WS close-reason fix) is covered by `cargo test -p bakery-agent -p
  // bakery-node`.
  test("Codex shows real status and Sign in opens a login terminal that closes itself on success", async ({ page }) => {
    let forced = false;

    await page.addInitScript(() => {
      window.open = () => null;
    });
    await page.route(`${NODE_BASE}/providers`, async (route) => {
      await route.fulfill({
        contentType: "application/json",
        body: JSON.stringify({ acp_plugs: [], openai_compat_plugs: [] }),
      });
    });
    await page.route(`${NODE_BASE}/studio/state`, async (route) => {
      await route.fulfill({
        contentType: "application/json",
        body: JSON.stringify({
          model_options: [],
          role_options: [],
          projects: [],
          ungrouped_sockets: [],
          factory_sockets: [],
        }),
      });
    });
    await page.route(`${NODE_BASE}/info`, async (route) => {
      await route.fulfill({ contentType: "application/json", body: JSON.stringify({ write_mode: "manual" }) });
    });
    await page.route(`${NODE_BASE}/auth/status`, async (route) => {
      await route.fulfill({
        contentType: "application/json",
        body: JSON.stringify({
          active_model: "auto",
          resolved_hint: "auto→stub",
          xai: { api_key_set: false, oauth_set: false },
          gemini: { api_key_set: false, oauth_set: false },
          anthropic: { api_key_set: false, oauth_set: false },
        }),
      });
    });
    await page.route(/\/auth\/cli-status/, async (route) => {
      if (new URL(route.request().url()).searchParams.get("force") === "1") forced = true;
      await route.fulfill({
        contentType: "application/json",
        body: JSON.stringify({
          codex: forced ? { installed: true, linked: true } : { installed: true, linked: false },
          cursor: { installed: false, linked: false },
        }),
      });
    });
    let createBody: unknown = null;
    await page.route(`${NODE_BASE}/terminal/create`, async (route) => {
      createBody = route.request().postDataJSON();
      await route.fulfill({
        contentType: "application/json",
        body: JSON.stringify({
          terminal_id: "t-codex-login",
          stream_id: "sys:accounts",
          name: "codex-login",
          mode: "host",
          cwd: "/home/test",
          live: true,
          created_ts: new Date().toISOString(),
        }),
      });
    });
    await page.route(`${NODE_BASE}/terminal/ticket`, async (route) => {
      await route.fulfill({
        contentType: "application/json",
        body: JSON.stringify({ ticket: "test-ticket", expires_in_secs: 30 }),
      });
    });

    let wsClosedWithExit = false;
    await page.routeWebSocket(/\/terminal\/ws/, (ws) => {
      ws.onMessage(() => {});
      setTimeout(() => {
        wsClosedWithExit = true;
        void ws.close({ code: 1000, reason: "exited:0" });
      }, 300);
    });

    await page.goto("/settings/providers");

    const codex = page.locator("li.row").filter({ hasText: "Codex" });
    await expect(codex.locator(".chip")).toHaveText("Not linked");

    await codex.getByRole("button", { name: "Sign in with Codex" }).click();

    // The client no longer names the terminal or pre-checks for an existing
    // one by name (07d-A3D1 review finding 1) — it just asks for the login
    // command and trusts the node's own idempotent reuse-or-create.
    await expect
      .poll(() => createBody)
      .toEqual({ stream_id: "sys:accounts", mode: "host", name: null, command: "codex-login" });

    await expect(page.locator(".cli-login-term .terminal-widget")).toBeVisible();

    // The login pane closes itself once the mocked WS reports the process
    // exited — the embedded terminal disappears and the row's status is
    // re-fetched with force=1 rather than waiting out the 10s cache.
    await expect.poll(() => wsClosedWithExit, { timeout: 5_000 }).toBe(true);
    await expect(page.locator(".cli-login-term")).toHaveCount(0);
    await expect(codex.locator(".chip")).toHaveText("Linked");
  });
});
