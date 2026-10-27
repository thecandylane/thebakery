// "i cant select models in chat": after a deploy restarts the node, a native
// account knows no models until a conversation has a session on it, and the
// new-chat composer used to hide its model picker without a word. Run in
// WebKit, the desktop app's engine.
import { expect, type Page, type Route } from "@playwright/test";
import { test } from "./fixtures";

test.use({ browserName: "webkit" });

const MODELS = {
  source: "native_session",
  catalog: {
    status: "available",
    models: [
      { id: "opus", label: "Opus", context_window: null, effort_levels: [{ id: "high", label: "High" }, { id: "max", label: "Max" }] },
      { id: "sonnet", label: "Sonnet", context_window: null, effort_levels: [] },
      { id: "haiku", label: "Haiku", context_window: null, effort_levels: [] },
    ],
  },
  text_attachments: true,
  image_attachments: true,
  audio_attachments: false,
  plan_enforceable: true,
  metadata_provenance: "probe",
  catalog_expected: true,
};

const unreadable = (reason: string) => ({
  source: "unavailable",
  catalog: {
    status: "unavailable",
    default: { id: "claude-native", label: "claude-native", context_window: null, effort_levels: [] },
    reason,
  },
  text_attachments: false,
  image_attachments: false,
  audio_attachments: false,
  plan_enforceable: false,
  availability_reason: reason,
  metadata_provenance: "none",
  catalog_expected: true,
});

/** Makes Claude native a runnable account on the (credential-less) test node. */
async function withRunnableAccount(page: Page) {
  await page.route("**/studio/state", async (route) => {
    const response = await route.fetch();
    const body = await response.json();
    for (const option of body.model_options ?? []) {
      if (option.id === "claude-native") {
        option.node_available = true;
        option.reason = null;
      }
    }
    await route.fulfill({ response, body: JSON.stringify(body) });
  });
  await page.addInitScript(() => localStorage.setItem("bakery.chat.model", "claude-native"));
}

const capabilityRoute = (page: Page, answer: (route: Route, refresh: boolean) => Promise<void> | void) =>
  page.route("**/accounts/capabilities*", (route) => answer(route, new URL(route.request().url()).searchParams.get("refresh") === "true"));

test("the model picker opens, picks by click and by keyboard, and leaves the page usable", async ({ page }) => {
  await withRunnableAccount(page);
  await capabilityRoute(page, (route) => route.fulfill({ json: MODELS }));
  await page.goto("/chat");

  const picker = page.getByTestId("chat-start-model");
  await expect(picker).toHaveText(/Default model/);
  await picker.click();
  await expect(page.locator("[cmdk-item]")).toHaveCount(4);
  await page.locator("[cmdk-item]", { hasText: "Sonnet" }).click();
  await expect(picker).toHaveText(/Sonnet/);

  await picker.click();
  await page.keyboard.type("haiku");
  await page.keyboard.press("Enter");
  await expect(picker).toHaveText(/Haiku/);

  // The modal dialog let go of the page: nothing is left unclickable.
  expect(await page.evaluate(() => document.body.style.pointerEvents)).not.toBe("none");
  await page.getByTestId("chat-start-account").click();
  await expect(page.getByRole("listbox")).toBeVisible();
});

test("a model list still being read says so; one that cannot be read says why, and Retry asks the adapter again", async ({
  page,
}) => {
  await withRunnableAccount(page);
  const asked: boolean[] = [];
  await capabilityRoute(page, async (route, refresh) => {
    asked.push(refresh);
    if (asked.length === 1) {
      await new Promise((resolve) => setTimeout(resolve, 700));
      return route.fulfill({ json: unreadable("Setup needed: sign in with Claude Code") });
    }
    return route.fulfill({ json: MODELS });
  });
  await page.goto("/chat");

  await expect(page.getByTestId("chat-start-models-loading")).toBeVisible();
  const reason = page.getByTestId("chat-start-models-unavailable");
  await expect(reason).toContainText("Models unavailable — Setup needed: sign in with Claude Code");
  await expect(page.getByTestId("chat-start-model")).toHaveCount(0);

  await page.getByTestId("chat-start-models-retry").click();
  await expect(page.getByTestId("chat-start-model")).toBeVisible();
  await expect(reason).toHaveCount(0);
  expect(asked).toEqual([false, true]);
});

test("an account with no model list at all shows neither a picker nor an error", async ({ page }) => {
  await withRunnableAccount(page);
  await capabilityRoute(page, (route) =>
    route.fulfill({ json: { ...unreadable("this model does not report a live catalog"), catalog_expected: false } }),
  );
  await page.goto("/chat");
  await expect(page.getByTestId("chat-start")).toBeVisible();
  await expect(page.getByTestId("chat-start-account")).toBeVisible();
  await page.waitForTimeout(500);
  await expect(page.getByTestId("chat-start-model")).toHaveCount(0);
  await expect(page.getByTestId("chat-start-models-unavailable")).toHaveCount(0);
  await expect(page.getByTestId("chat-start-models-loading")).toHaveCount(0);
});
