// Real-browser proof for docs/slices/07d-A3-PROJECTS-NOT-REPOS.md's Proof
// checklist: opening or creating a Project must never require Git. Server-
// side substance (root uniqueness, fail-closed catalog reads, atomicity) is
// already covered directly by Rust-level tests in
// crates/bakery-node/src/{catalog,routes/project_catalog,routes/studio}.rs —
// these specs prove what only a real browser can: the one-step "New project"
// form (and its "More options" for a brand-new folder) actually lands you on
// a working socket with no git chrome, Files
// works against a plain directory, and a chat-only project (no root at all)
// still boots with Code disabled until a root attaches. Same harness as
// ux-shell.spec.ts / identity-finish.spec.ts (see playwright.config.ts /
// helpers.ts).
import { expect } from "@playwright/test";
import { test } from "./fixtures";
import { makeScratchDir, freshParentAndChildName } from "./helpers";
import fs from "node:fs";
import path from "node:path";

test.describe("07d-A3 Projects not repos", () => {
  test("creating a project on an existing non-git directory has no git chrome and Files works", async ({ page }) => {
    const dir = makeScratchDir({ "notes.txt": "hello from a plain directory\n" });
    const name = `e2e-nogit-existing-${Date.now()}`;

    await page.goto("/?open=project");
    await page.getByText(/More options/).click();
    const advanced = page.locator(".create-project");
    await advanced.getByPlaceholder("project name", { exact: true }).fill(name);
    await advanced.getByLabel("use an existing folder").check();
    await advanced.getByPlaceholder("folder path").fill(dir);
    await advanced.getByRole("button", { name: "Create project" }).click();

    // Lands on the new starter socket's workbench. Attached, not visible: a
    // lone panel's tab strip is hidden.
    await expect(page).toHaveURL(/\/w\//, { timeout: 15_000 });
    await expect(page.locator(".dv-default-tab", { hasText: "Orchestrator" })).toBeAttached({ timeout: 15_000 });

    // No git chrome: a non-git root has no branch to show.
    await expect(page.locator(".branch-chip")).toHaveCount(0);

    // Files works against a plain directory — no admission requirement
    // beyond "the path resolves to a directory" (07d-A3 §1).
    await page.getByRole("button", { name: "+ Panel" }).click();
    await page.getByRole("button", { name: "Code", exact: true }).click();
    await expect(page.locator(".dv-default-tab", { hasText: "Code" })).toBeVisible({ timeout: 15_000 });
    await expect(page.locator(".row.file", { hasText: "notes.txt" })).toBeVisible();
  });

  test("creating a project with a not-yet-existing folder actually creates it on disk", async ({ page }) => {
    const { parent, name: childName } = freshParentAndChildName("brand-new-project-root");
    const newDir = path.join(parent, childName);
    expect(fs.existsSync(newDir)).toBe(false);
    const name = `e2e-nogit-create-${Date.now()}`;

    await page.goto("/?open=project");
    await page.getByText(/More options/).click();
    const advanced = page.locator(".create-project");
    await advanced.getByPlaceholder("project name", { exact: true }).fill(name);
    await advanced.getByLabel("a new folder somewhere else").check();
    await advanced.getByPlaceholder("parent folder").fill(parent);
    await advanced.getByPlaceholder("new folder name").fill(childName);
    await advanced.getByRole("button", { name: "Create project" }).click();

    await expect(page).toHaveURL(/\/w\//, { timeout: 15_000 });
    await expect(page.locator(".dv-default-tab", { hasText: "Orchestrator" })).toBeAttached({ timeout: 15_000 });
    expect(fs.existsSync(newDir)).toBe(true);
  });

  test("creating a new project folder never exposes a sibling file in its parent directory", async ({ page }) => {
    const { parent, name: childName } = freshParentAndChildName(`sibling-child-${Date.now()}`);
    fs.writeFileSync(path.join(parent, "secret.txt"), "top secret\n");
    const name = `e2e-containment-${Date.now()}`;

    await page.goto("/?open=project");
    await page.getByText(/More options/).click();
    const advanced = page.locator(".create-project");
    await advanced.getByPlaceholder("project name", { exact: true }).fill(name);
    await advanced.getByLabel("a new folder somewhere else").check();
    await advanced.getByPlaceholder("parent folder").fill(parent);
    await advanced.getByPlaceholder("new folder name").fill(childName);
    await advanced.getByRole("button", { name: "Create project" }).click();

    await expect(page).toHaveURL(/\/w\//, { timeout: 15_000 });
    expect(fs.existsSync(path.join(parent, childName))).toBe(true);

    await page.getByRole("button", { name: "+ Panel" }).click();
    await page.getByRole("button", { name: "Code", exact: true }).click();
    await expect(page.locator(".dv-default-tab", { hasText: "Code" })).toBeVisible({ timeout: 15_000 });
    await expect(page.locator(".row.file", { hasText: "secret.txt" })).toHaveCount(0);
  });

  test("a name alone makes a Git project inside Bakery and opens its studio with its files", async ({ page }) => {
    const name = `e2e-in-bakery-${Date.now()}`;

    await page.goto("/?open=project");
    await page.getByLabel("Project name").fill(name);
    await page.getByTestId("new-project-create").click();

    await expect(page).toHaveURL(/\/w\//, { timeout: 15_000 });
    await expect(page.locator(".dv-default-tab", { hasText: "Orchestrator" })).toBeAttached({ timeout: 15_000 });
    // Scaffolded as a Git repo on `main`, with a README to start from.
    await expect(page.locator(".branch-chip")).toHaveText("main");
    await page.getByRole("button", { name: "+ Panel" }).click();
    await page.getByRole("button", { name: "Code", exact: true }).click();
    await expect(page.locator(".row.file", { hasText: "README.md" })).toBeVisible({ timeout: 15_000 });
  });
});
