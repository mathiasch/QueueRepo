/** End-to-end UI tests (desktop): real browser → real app → real runner. */
import { expect, test, type Page } from "@playwright/test";
import { navLink, openLog, openTask, taskRow, uiRegister } from "../helpers";

async function createProject(page: Page, name: string) {
  await navLink(page, "Projekte").click();
  const newButton = page.getByRole("button", { name: "Neues Projekt" });
  if (await newButton.isVisible()) await newButton.click();
  await page.getByLabel("Projektname").fill(name);
  await page.getByRole("button", { name: "Anlegen" }).click();
  await expect(page.getByRole("heading", { level: 1, name })).toBeVisible();
  await page.getByRole("link", { name: "Alle Projekte" }).click();
  await navLink(page, "Aufträge").click();
}

async function enqueue(page: Page, prompt: string) {
  const box = page.getByPlaceholder(/Was soll erledigt werden/);
  await box.fill(prompt);
  await box.press("Control+Enter");
  await expect(box).toHaveValue("");
}

async function openProject(page: Page, name: string) {
  await navLink(page, "Projekte").click();
  await page.locator(".project-card", { hasText: name }).getByRole("button", { name }).click();
  await expect(page.getByRole("heading", { level: 1, name })).toBeVisible();
}

test("login screen, registration and empty state", async ({ page }) => {
  await page.goto("/");
  await expect(page.getByRole("heading", { name: /Queue/ })).toBeVisible();
  await page.getByLabel("E-Mail").fill("nobody@e2e.test");
  await page.getByLabel("Passwort").fill("wrongpass1");
  await page.getByRole("button", { name: "Anmelden" }).click();
  await expect(page.getByRole("alert").getByText("E-Mail oder Passwort ist falsch.")).toBeVisible();
  await uiRegister(page, "empty");
  await expect(page.getByText("Lege zuerst ein Projekt an")).toBeVisible();
  // Without a project there is no composer, only the onboarding.
  await expect(page.getByPlaceholder(/Was soll erledigt werden/)).toHaveCount(0);
});

test("onboarding: create the first project inline, then write the first task", async ({ page }) => {
  await uiRegister(page, "onboard");
  await page.reload();
  const ob = page.getByRole("region", { name: "Los geht's in drei Schritten" });
  // Mock is the default provider here, so step 2 already counts as done.
  await expect(ob).toContainText("1 von 3 erledigt");
  await ob.getByLabel("Projektname").fill("Erstes");
  await ob.getByRole("button", { name: "Projekt anlegen" }).click();
  await expect(page.locator(".toast", { hasText: "Projekt „Erstes“ angelegt" })).toBeVisible();
  await expect(ob).toContainText("2 von 3 erledigt");
  await ob.getByRole("button", { name: "Zum Eingabefeld" }).click();
  await expect(page.getByPlaceholder(/Was soll erledigt werden/)).toBeFocused();
  await enqueue(page, "first steps");
  await expect(taskRow(page, "first steps", "done")).toBeVisible({ timeout: 30_000 });
  await expect(ob).toHaveCount(0);
});

test("enqueue → live run → done → drawer with diff and git log", async ({ page }) => {
  await uiRegister(page, "flow");
  await createProject(page, "Flow");
  await enqueue(page, "Build the feature");
  await expect(taskRow(page, "Build the feature", "done")).toBeVisible({ timeout: 30_000 });
  const drawer = await openTask(page, "Build the feature", "done");
  await expect(drawer.getByText("mock done")).toBeVisible();
  await expect(drawer.locator(".block", { hasText: "Review durch zweiten Agenten" })).toContainText("Bestanden");
  await drawer.getByRole("button", { name: "Änderungen anzeigen" }).click();
  await expect(drawer.locator(".diff")).toContainText("MOCK_LOG.md");
  await expect(drawer.locator(".diff-files")).toContainText("MOCK_LOG.md");
  await openLog(page);
  await expect(drawer.locator(".ev-verify, .ev-review").first()).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(drawer).toBeHidden();

  await openProject(page, "Flow");
  await page.getByRole("tab", { name: "Verlauf" }).click();
  await expect(page.locator(".item", { hasText: "Build the feature" })).toBeVisible();
  await expect(page.locator(".item", { hasText: "Initial commit" })).toBeVisible();
  await page.getByRole("tab", { name: "Dateien" }).click();
  await page.locator(".file", { hasText: "MOCK_LOG.md" }).click();
  await expect(page.locator(".file-view pre")).toContainText("run ");
});

test("agent question appears in 'Wartet auf dich' and can be answered inline", async ({ page }) => {
  await uiRegister(page, "ask");
  await createProject(page, "Ask");
  await enqueue(page, "MOCK_ASK choose");
  const q = taskRow(page, "Postgres or SQLite?", "waiting_input");
  await expect(q).toBeVisible({ timeout: 30_000 });
  await expect(navLink(page, "Postfach").locator(".badge.attention")).toBeVisible();
  await q.getByRole("textbox", { name: /Antwort auf/ }).fill("Postgres");
  await q.getByRole("button", { name: "Antworten" }).click();
  await expect(taskRow(page, "MOCK_ASK choose", "done")).toBeVisible({ timeout: 30_000 });
});

test("more than two generated tasks require approval, approve all", async ({ page }) => {
  await uiRegister(page, "approve");
  await createProject(page, "Approve");
  await enqueue(page, "MOCK_FOLLOWUPS plan");
  await expect(page.getByRole("button", { name: "Alle 3 freigeben" })).toBeVisible({ timeout: 30_000 });
  await expect(page.locator(".toast", { hasText: "Freigabe nötig" })).toBeVisible();
  await page.getByRole("button", { name: "Alle 3 freigeben" }).click();
  await expect(page.locator('article[data-status="done"] .agent-tag')).toHaveCount(3, { timeout: 30_000 });
});

test("failed task gets fix tasks and ends blocked with a notification", async ({ page }) => {
  await uiRegister(page, "fix");
  await createProject(page, "Fix");
  await enqueue(page, "MOCK_FAIL broken");
  await expect(taskRow(page, "Fix #2", "blocked")).toBeVisible({ timeout: 45_000 });
  await navLink(page, "Postfach").click();
  await expect(page.locator(".notification", { hasText: "Blockiert" }).first()).toBeVisible();
});

test("token budget can be set and pauses the provider", async ({ page }) => {
  await uiRegister(page, "budgetui");
  await createProject(page, "Budget");
  await enqueue(page, "first");
  await expect(taskRow(page, "first", "done")).toBeVisible({ timeout: 30_000 });
  await navLink(page, "Einstellungen").click();
  const row = page.getByTestId("budget-mock");
  await expect(row).toContainText("5 Std.");
  // Unsaved input in another row survives the refresh that a save triggers.
  await page.getByTestId("budget-claude").getByLabel("5h-Budget (Tokens)").fill("777");
  await row.getByLabel("5h-Budget (Tokens)").fill("1000");
  await row.getByRole("button", { name: "Speichern" }).click();
  await expect(row.locator(".tag.tone-danger", { hasText: "Budget erschöpft" })).toBeVisible();
  await expect(page.locator(".chip.tone-danger", { hasText: "Budget" })).toBeVisible();
  await expect(row.locator(".bar-fill.tone-danger").first()).toBeVisible();
  await expect(page.getByTestId("budget-claude").getByLabel("5h-Budget (Tokens)")).toHaveValue("777");
  // Invalid input is rejected in German before the request.
  await row.getByLabel("5h-Budget (Tokens)").fill("1,5");
  await row.getByRole("button", { name: "Speichern" }).click();
  await expect(row.getByText("5h-Budget: bitte eine ganze Zahl ab 1 eingeben")).toBeVisible();
});

test("drag-free reordering with buttons, discard with restore, cancel with confirmation", async ({ page }) => {
  await uiRegister(page, "order");
  await createProject(page, "Order");
  await enqueue(page, "MOCK_SLOW blocker");
  await expect(taskRow(page, "blocker", "running")).toBeVisible({ timeout: 30_000 });
  await enqueue(page, "alpha");
  await enqueue(page, "beta");
  await enqueue(page, "gamma");
  const queued = page.getByTestId("queue-list").locator("article");
  await expect(queued).toHaveCount(3);
  await expect(queued.nth(0)).toContainText("alpha");
  await queued.nth(1).getByRole("button", { name: "Ganz nach oben" }).click();
  await expect(queued.nth(0)).toContainText("beta");
  // Discard without a dialog, but with "Wiederherstellen".
  await taskRow(page, "gamma", "queued").getByRole("button", { name: "Verwerfen" }).click();
  await expect(queued).toHaveCount(2);
  await page.locator(".toast", { hasText: "verworfen" }).getByRole("button", { name: "Wiederherstellen" }).click();
  await expect(queued).toHaveCount(3);
  await expect(queued.nth(2)).toContainText("gamma");
  // Cancelling a running task asks first; "Abbrechen" in the dialog keeps it running.
  await taskRow(page, "blocker", "running").getByRole("button", { name: "Abbrechen" }).click();
  let dialog = page.getByRole("alertdialog", { name: "Laufenden Auftrag abbrechen?" });
  await expect(dialog).toBeVisible();
  await expect(dialog.getByRole("button", { name: "Abbrechen", exact: true })).toBeFocused();
  await dialog.getByRole("button", { name: "Abbrechen", exact: true }).click();
  await expect(dialog).toBeHidden();
  await expect(taskRow(page, "blocker", "running")).toBeVisible();
  await taskRow(page, "blocker", "running").getByRole("button", { name: "Abbrechen" }).click();
  dialog = page.getByRole("alertdialog", { name: "Laufenden Auftrag abbrechen?" });
  await dialog.getByRole("button", { name: "Auftrag abbrechen" }).click();
  await expect(taskRow(page, "beta", "done")).toBeVisible({ timeout: 30_000 });
});

async function openProjectSettings(page: Page, name: string) {
  await openProject(page, name);
  await page.getByRole("tab", { name: "Einstellungen" }).click();
  return page.getByTestId("project-limits");
}

test("project limits are saved, survive a reload and the token limit stops a run", async ({ page }) => {
  await uiRegister(page, "limits");
  await createProject(page, "Limits");
  let limits = await openProjectSettings(page, "Limits");
  await limits.getByLabel("Max. Tokens", { exact: true }).fill("2000");
  await limits.getByLabel("Max. Laufzeit (Min.)", { exact: true }).fill("10");
  await page.getByRole("button", { name: "Speichern" }).click();
  await expect(page.locator(".toast", { hasText: "Gespeichert" })).toBeVisible();
  await page.reload();
  limits = page.getByTestId("project-limits");
  await expect(page.getByRole("tab", { name: "Einstellungen" })).toHaveAttribute("aria-selected", "true");
  await expect(limits.getByLabel("Max. Tokens", { exact: true })).toHaveValue("2000");
  await expect(limits.getByLabel("Max. Laufzeit (Min.)", { exact: true })).toHaveValue("10");
  await expect(limits.getByLabel("Max. Turns", { exact: true })).toHaveValue("");
  // Invalid values get a German message at the field, nothing is saved.
  await limits.getByLabel("Max. Turns", { exact: true }).fill("2,5");
  await page.getByRole("button", { name: "Speichern" }).click();
  await expect(limits.getByText("Bitte eine ganze Zahl eingeben")).toBeVisible();
  await expect(limits.getByLabel("Max. Turns", { exact: true })).toHaveAttribute("aria-invalid", "true");
  await limits.getByLabel("Max. Turns", { exact: true }).fill("");

  await page.getByRole("link", { name: "Alle Projekte" }).click();
  await navLink(page, "Aufträge").click();
  await enqueue(page, "MOCK_TOKENS burn");
  const row = taskRow(page, "MOCK_TOKENS burn", "blocked");
  await expect(row).toBeVisible({ timeout: 30_000 });
  await expect(row.getByTestId("failure-tag")).toHaveText("Limit: Tokens");
  await expect(taskRow(page, "Fix #1")).toHaveCount(0);
  const drawer = await openTask(page, "MOCK_TOKENS burn", "blocked");
  await expect(drawer.getByTestId("abort-reason")).toContainText("Limit: Tokens");
  await expect(drawer.getByTestId("abort-reason")).toContainText("Kein Fix-Versuch verbraucht");
  await expect(drawer.getByText("Limit überschritten: Tokens 2000").first()).toBeVisible();
  await openLog(page);
  await expect(drawer.locator(".ev-guard").first()).toBeVisible();
});

test("composer: advanced options set task limits, invalid limits are rejected before sending", async ({ page }) => {
  await uiRegister(page, "composer");
  await createProject(page, "Composer");
  await page.getByRole("button", { name: "Erweitert" }).click();
  const composer = page.locator(".composer");
  await composer.getByLabel("Max. Tokens", { exact: true }).fill("1,5");
  await page.getByPlaceholder(/Was soll erledigt werden/).fill("limited task");
  await page.getByRole("button", { name: "Einreihen" }).click();
  await expect(composer.getByText("Bitte eine ganze Zahl eingeben")).toBeVisible();
  await expect(page.getByPlaceholder(/Was soll erledigt werden/)).toHaveValue("limited task");
  expect((await (await page.request.get("/api/tasks")).json()).length).toBe(0);
  await composer.getByLabel("Max. Tokens", { exact: true }).fill("50000");
  await composer.getByLabel("Max. Turns", { exact: true }).fill("7");
  await composer.getByLabel("Zusätzlicher Kontext", { exact: true }).fill("Bitte auf Deutsch");
  await page.getByRole("button", { name: "Einreihen" }).click();
  await expect(page.getByPlaceholder(/Was soll erledigt werden/)).toHaveValue("");
  const tasks = await (await page.request.get("/api/tasks")).json();
  expect(tasks).toHaveLength(1);
  expect(tasks[0]).toMatchObject({ max_tokens: 50000, max_turns: 7, max_runtime_secs: null, context: "Bitte auf Deutsch" });
  const drawer = await openTask(page, "limited task");
  await expect(drawer.locator(".block", { hasText: "Limits dieses Auftrags" })).toContainText("Tokens 50k · Turns 7");
});

test("drawer: edit a waiting task including its limits", async ({ page }) => {
  await uiRegister(page, "edit");
  await createProject(page, "Edit");
  await enqueue(page, "MOCK_SLOW blocker");
  await expect(taskRow(page, "blocker", "running")).toBeVisible({ timeout: 30_000 });
  await enqueue(page, "draft title");
  const drawer = await openTask(page, "draft title", "queued");
  await drawer.getByRole("button", { name: "Bearbeiten" }).click();
  await expect(drawer.getByLabel("Titel", { exact: true })).toBeFocused();
  await drawer.getByLabel("Titel", { exact: true }).fill("final title");
  await drawer.getByLabel("Max. Turns", { exact: true }).fill("4");
  await drawer.getByLabel("Titel", { exact: true }).press("Control+Enter");
  await expect(page.locator(".toast", { hasText: "Auftrag gespeichert" })).toBeVisible();
  await expect(drawer.getByRole("heading", { name: "final title" })).toBeVisible();
  const t = (await (await page.request.get("/api/tasks")).json()).find((x: any) => x.title === "final title");
  expect(t.max_turns).toBe(4);
  await page.keyboard.press("Escape");
  await taskRow(page, "blocker", "running").getByRole("button", { name: "Abbrechen" }).click();
  await page.getByRole("alertdialog").getByRole("button", { name: "Auftrag abbrechen" }).click();
});

test("revert asks for confirmation and undoes the task's commit", async ({ page }) => {
  await uiRegister(page, "revert");
  await createProject(page, "Revert");
  // The mock reviewer appends to the same file, which would make the revert conflict (409).
  const pid = (await (await page.request.get("/api/projects")).json())[0].id;
  await page.request.patch(`/api/projects/${pid}`, { data: { review_enabled: false } });
  await enqueue(page, "change something");
  await expect(taskRow(page, "change something", "done")).toBeVisible({ timeout: 30_000 });
  const drawer = await openTask(page, "change something", "done");
  await drawer.getByRole("button", { name: "Rückgängig machen" }).click();
  const dialog = page.getByRole("alertdialog", { name: "Änderungen rückgängig machen?" });
  await dialog.getByRole("button", { name: "Rückgängig machen" }).click();
  await expect(page.locator(".toast", { hasText: "Änderungen zurückgesetzt" })).toBeVisible();
  // Focus returns into the drawer after the dialog closes.
  await expect(drawer.getByRole("button", { name: "Rückgängig machen" })).toBeFocused();
  const commits = await (await page.request.get(`/api/projects/${pid}/commits`)).json();
  expect(commits[0].subject).toMatch(/^Revert/);
});

test("transient provider error is retried without a fix attempt", async ({ page }) => {
  await uiRegister(page, "transientui");
  await createProject(page, "Transient");
  await enqueue(page, "MOCK_TRANSIENT flaky");
  const row = taskRow(page, "MOCK_TRANSIENT flaky", "done");
  await expect(row).toBeVisible({ timeout: 30_000 });
  await expect(taskRow(page, "Fix #1")).toHaveCount(0);
  const drawer = await openTask(page, "MOCK_TRANSIENT flaky", "done");
  await openLog(page);
  await expect(drawer.locator(".ev-retry").first()).toContainText("overloaded");
  await expect(drawer.getByText(/Erneut eingereiht \(Provider gestört\)/)).toBeVisible();
});

test("provider pause after an auth failure is shown and can be lifted", async ({ page }) => {
  await uiRegister(page, "pause");
  await createProject(page, "Pause");
  await enqueue(page, "MOCK_AUTH login");
  await navLink(page, "Einstellungen").click();
  const row = page.getByTestId("budget-mock");
  await expect(row.locator(".tag.tone-danger", { hasText: "pausiert: Invalid API key" })).toBeVisible({ timeout: 30_000 });
  await expect(page.locator(".chip.tone-danger", { hasText: "pausiert" })).toBeVisible();
  // The paused task stays queued; drop it so it does not trip the pause again.
  const tasks = await (await page.request.get("/api/tasks")).json();
  const authTask = tasks.find((t: any) => t.title === "MOCK_AUTH login");
  expect(authTask.status).toBe("queued");
  expect(authTask.failure_class).toBe("provider_auth");
  expect(authTask.attempt).toBe(0);
  await page.request.post(`/api/tasks/${authTask.id}/cancel`);
  await row.getByRole("button", { name: "Pause aufheben" }).click();
  await expect(page.locator(".toast", { hasText: "läuft wieder" })).toBeVisible();
  await expect(row.locator(".tag.tone-danger", { hasText: "pausiert" })).toHaveCount(0);
  await navLink(page, "Aufträge").click();
  await enqueue(page, "after pause");
  await expect(taskRow(page, "after pause", "done")).toBeVisible({ timeout: 30_000 });
});

test("keyboard shortcuts, live region and theme choice", async ({ page }) => {
  await uiRegister(page, "keys");
  await createProject(page, "Keys");
  // Skip link: first Tab stop, visible when focused, jumps to the main content.
  await page.reload();
  await expect(navLink(page, "Aufträge")).toBeVisible();
  await page.keyboard.press("Tab");
  const skip = page.getByRole("link", { name: "Zum Inhalt springen" });
  await expect(skip).toBeFocused();
  expect((await skip.boundingBox())!.width).toBeGreaterThan(100);
  await page.keyboard.press("Enter");
  await expect(page.locator("main#main")).toBeFocused();
  await page.locator("body").click({ position: { x: 600, y: 10 } });
  await page.keyboard.press("g");
  await page.keyboard.press("o");
  await expect(page).toHaveURL(/\/projects$/);
  await page.keyboard.press("g");
  await page.keyboard.press("e");
  await expect(page).toHaveURL(/\/settings$/);
  await page.keyboard.press("?");
  const help = page.getByRole("dialog", { name: "Tastaturkürzel" });
  await expect(help).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(help).toBeHidden();
  await page.keyboard.press("n");
  await expect(page).toHaveURL(/\/$/);
  const box = page.getByPlaceholder(/Was soll erledigt werden/);
  await expect(box).toBeFocused();
  // Typing "g" in a field must not navigate.
  await box.pressSequentially("go");
  await expect(page).toHaveURL(/\/$/);
  await box.press("Control+Enter");
  // Status changes are announced for screen readers.
  await expect(page.locator('[role="status"][aria-live="polite"][aria-atomic="true"]')).toContainText("go: Fertig", { timeout: 30_000 });

  await navLink(page, "Einstellungen").click();
  await page.getByRole("button", { name: "Dunkel" }).click();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  await page.reload();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  await page.getByRole("button", { name: "Wie Gerät" }).click();
  await expect(page.locator("html")).not.toHaveAttribute("data-theme", /.*/);
});

test("deleting a project needs confirmation; members can be added and removed", async ({ page, browser }) => {
  // Second user must exist before being added.
  const other = await browser.newContext({ baseURL: test.info().project.use.baseURL });
  const otherEmail = `member-${Date.now()}@e2e.test`;
  expect((await other.request.post("/api/auth/register", { data: { email: otherEmail, password: "password123", name: "Mia Member" } })).status()).toBe(200);
  await other.close();

  await uiRegister(page, "owner");
  await createProject(page, "Shared");
  await openProject(page, "Shared");
  await page.getByRole("tab", { name: "Mitglieder" }).click();
  await page.getByLabel("E-Mail-Adresse").fill(otherEmail);
  await page.getByLabel("Rolle").selectOption("viewer");
  await page.getByRole("button", { name: "Hinzufügen" }).click();
  const member = page.locator(".member", { hasText: "Mia Member" });
  await expect(member).toContainText("Nur ansehen");
  await member.getByRole("button", { name: "Mia Member entfernen" }).click();
  await page.getByRole("alertdialog").getByRole("button", { name: "Entfernen" }).click();
  await expect(member).toHaveCount(0);

  await page.getByRole("tab", { name: "Einstellungen" }).click();
  await page.getByRole("button", { name: "Projekt löschen" }).click();
  const dialog = page.getByRole("alertdialog", { name: "Projekt „Shared“ löschen?" });
  await page.keyboard.press("Escape");
  await expect(dialog).toBeHidden();
  await page.getByRole("button", { name: "Projekt löschen" }).click();
  await dialog.getByRole("button", { name: "Endgültig löschen" }).click();
  await expect(page).toHaveURL(/\/projects$/);
  await expect(page.getByText("Noch keine Projekte")).toBeVisible();
});
