/** End-to-end UI tests (desktop): real browser → real app → real runner. */
import { expect, test } from "@playwright/test";
import { uiRegister } from "../helpers";

async function createProject(page: import("@playwright/test").Page, name: string) {
  await page.getByRole("button", { name: "Projekte" }).click();
  await page.getByPlaceholder("Name").fill(name);
  await page.getByRole("button", { name: "Anlegen" }).click();
  await expect(page.getByRole("heading", { name })).toBeVisible();
  await page.getByRole("button", { name: "← Projekte" }).click();
  await page.getByRole("button", { name: /^Queue/ }).click();
}

async function enqueue(page: import("@playwright/test").Page, prompt: string) {
  const box = page.getByPlaceholder(/Was soll erledigt werden/);
  await box.fill(prompt);
  await box.press("Control+Enter");
  await expect(box).toHaveValue("");
}

test("login screen, registration and empty state", async ({ page }) => {
  await page.goto("/");
  await expect(page.getByRole("heading", { name: /Queue/ })).toBeVisible();
  await page.getByPlaceholder("E-Mail").fill("nobody@e2e.test");
  await page.getByPlaceholder("Passwort").fill("wrongpass1");
  await page.getByRole("button", { name: "Anmelden" }).click();
  await expect(page.getByText("invalid credentials")).toBeVisible();
  await uiRegister(page, "empty");
  await expect(page.getByText("Lege zuerst ein Projekt an")).toBeVisible();
});

test("enqueue → live run → done → drawer with diff and git log", async ({ page }) => {
  await uiRegister(page, "flow");
  await createProject(page, "Flow");
  await enqueue(page, "Build the feature");
  const row = page.locator(".item", { hasText: "Build the feature" });
  await expect(row).toHaveClass(/done/, { timeout: 30_000 });
  await row.click();
  const drawer = page.locator(".drawer");
  await expect(drawer.getByText("mock done")).toBeVisible();
  await expect(drawer.getByText("Review ✓")).toBeVisible();
  await drawer.getByRole("button", { name: "Änderungen anzeigen" }).click();
  await expect(drawer.locator(".diff")).toContainText("MOCK_LOG.md");
  await expect(drawer.locator(".ev-verify, .ev-review").first()).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(drawer).toBeHidden();

  await page.getByRole("button", { name: "Projekte" }).click();
  await page.locator(".project-card", { hasText: "Flow" }).click();
  await page.getByRole("button", { name: "Git" }).click();
  await expect(page.locator(".item", { hasText: "Build the feature" })).toBeVisible();
  await expect(page.locator(".item", { hasText: "Initial commit" })).toBeVisible();
  await page.getByRole("button", { name: "Dateien" }).click();
  await page.locator(".file", { hasText: "MOCK_LOG.md" }).click();
  await expect(page.locator(".file-view pre")).toContainText("run ");
});

test("agent question appears in 'Braucht dich' and can be answered inline", async ({ page }) => {
  await uiRegister(page, "ask");
  await createProject(page, "Ask");
  await enqueue(page, "MOCK_ASK choose");
  const q = page.locator(".item.attention", { hasText: "Postgres or SQLite?" });
  await expect(q).toBeVisible({ timeout: 30_000 });
  await expect(page.locator(".badge.warn")).toBeVisible();
  await q.getByPlaceholder("Antwort…").fill("Postgres");
  await q.getByRole("button", { name: "Antworten" }).click();
  await expect(page.locator(".item.done", { hasText: "MOCK_ASK choose" })).toBeVisible({ timeout: 30_000 });
});

test("more than two generated tasks require approval, approve all", async ({ page }) => {
  await uiRegister(page, "approve");
  await createProject(page, "Approve");
  await enqueue(page, "MOCK_FOLLOWUPS plan");
  await expect(page.getByRole("button", { name: "Alle 3 freigeben" })).toBeVisible({ timeout: 30_000 });
  await expect(page.locator(".toast", { hasText: "Freigabe nötig" })).toBeVisible();
  await page.getByRole("button", { name: "Alle 3 freigeben" }).click();
  await expect(page.locator(".item.done .tag.agent")).toHaveCount(3, { timeout: 30_000 });
});

test("failed task gets fix tasks and ends blocked with a notification", async ({ page }) => {
  await uiRegister(page, "fix");
  await createProject(page, "Fix");
  await enqueue(page, "MOCK_FAIL broken");
  await expect(page.locator(".item.blocked", { hasText: "Fix #2" })).toBeVisible({ timeout: 45_000 });
  await page.getByRole("button", { name: /^Inbox/ }).click();
  await expect(page.locator(".item.notif", { hasText: "Blockiert" }).first()).toBeVisible();
});

test("token budget can be set and pauses the provider", async ({ page }) => {
  await uiRegister(page, "budgetui");
  await createProject(page, "Budget");
  await enqueue(page, "first");
  await expect(page.locator(".item.done", { hasText: "first" })).toBeVisible({ timeout: 30_000 });
  await page.getByRole("button", { name: "Einstellungen" }).click();
  const row = page.getByTestId("budget-mock");
  await expect(row).toContainText("5 Std.");
  await row.getByLabel("5h-Budget (Tokens)").fill("1000");
  await row.getByRole("button", { name: "Speichern" }).click();
  await expect(row.locator(".tag.err", { hasText: "Budget erschöpft" })).toBeVisible();
  await expect(page.locator(".chip.err", { hasText: "Budget" })).toBeVisible();
  await expect(row.locator(".bar-fill.err").first()).toBeVisible();
});

test("drag-free reordering with buttons and cancel", async ({ page }) => {
  await uiRegister(page, "order");
  await createProject(page, "Order");
  await enqueue(page, "MOCK_SLOW blocker");
  await expect(page.locator(".item.running", { hasText: "blocker" })).toBeVisible({ timeout: 30_000 });
  await enqueue(page, "alpha");
  await enqueue(page, "beta");
  const queued = page.locator(".section", { hasText: "Warteschlange" }).locator(".item");
  await expect(queued.nth(0)).toContainText("alpha");
  await queued.nth(1).getByTitle("Ganz nach oben").click();
  await expect(queued.nth(0)).toContainText("beta");
  await page.locator(".item.running", { hasText: "blocker" }).getByTitle("Abbrechen").click();
  await expect(page.locator(".item.done", { hasText: "beta" })).toBeVisible({ timeout: 30_000 });
});
