/** WP07 in the browser: API tokens (shown once, revoke), templates, schedule/webhook/GitHub triggers, secret shown once, history. */
import { expect, test } from "@playwright/test";
import { apiProject, expectAccessible, uiRegister, waitTask } from "../helpers";

test("an API token is shown exactly once and can be revoked", async ({ page }) => {
  await uiRegister(page, "uitok");
  await page.goto("/settings#api-tokens");
  const card = page.getByTestId("api-tokens");
  await expect(card).toContainText("Noch keine Tokens");
  await card.getByTestId("new-token").click();
  const form = card.getByTestId("token-form");
  await form.getByLabel("Name", { exact: true }).fill("Laptop");
  // Preset: queuectl scopes are checked by default; add one more.
  await expect(form.getByTestId("scope-tasks:write")).toBeChecked();
  await form.getByTestId("scope-triggers:fire").check();
  await form.getByLabel("Gültig für").selectOption("30");
  await expectAccessible(page, "token form");
  await form.getByRole("button", { name: "Token erstellen" }).click();

  const dialog = page.getByRole("dialog");
  await expect(dialog).toContainText("Token „Laptop“ erstellt");
  const value = await dialog.getByTestId("token-value").inputValue();
  expect(value).toMatch(/^qk_[0-9A-Za-z]{49}$/);
  await dialog.getByText("So richtest du queuectl ein").click();
  await expect(dialog).toContainText("chmod 600");
  await expectAccessible(page, "token shown once");
  await dialog.getByRole("button", { name: "Ich habe es kopiert" }).click();
  await expect(dialog).toHaveCount(0);

  const row = card.getByTestId("token-row");
  await expect(row).toContainText("Laptop");
  await expect(row).toContainText(value.slice(0, 9));
  await expect(row).toContainText("Auslöser starten");
  await expect(page.locator("body")).not.toContainText(value);
  await page.reload();
  await expect(page.locator("body")).not.toContainText(value);
  // The token works – until it is revoked.
  const call = () => page.request.get("/api/whoami", { headers: { Authorization: `Bearer ${value}` } });
  expect((await call()).status()).toBe(200);
  await page.getByTestId("token-row").getByRole("button", { name: "Token Laptop widerrufen" }).click();
  await page.getByRole("alertdialog").getByRole("button", { name: "Widerrufen" }).click();
  // The row's own button is labelled "Widerrufen" too: wait until it is gone
  // (the revocation finished), not only for the text (race, seen under load).
  await expect(page.getByTestId("token-row").getByRole("button", { name: "Token Laptop widerrufen" })).toHaveCount(0);
  await expect(page.getByTestId("token-row")).toContainText("Widerrufen");
  expect((await call()).status()).toBe(401);
  await expectAccessible(page, "revoked token");
});

test("templates and triggers in plain words: schedule, webhook with its secret once, history", async ({ page }) => {
  await uiRegister(page, "uiauto");
  const pid = await apiProject(page.request, { name: "Automatik" });
  await page.request.patch(`/api/projects/${pid}`, { data: { review_enabled: false } });
  await page.goto(`/projects/${pid}?tab=automation`);
  const panel = page.getByTestId("project-automation");
  await expect(panel.getByTestId("new-trigger")).toBeDisabled();
  await expect(panel).toContainText("Lege zuerst eine Vorlage an");

  await panel.getByTestId("new-template").click();
  const tf = panel.getByTestId("template-form");
  await tf.getByLabel("Name der Vorlage").fill("Nachtlauf");
  await tf.getByLabel("Titel des Auftrags").fill("Abhängigkeiten prüfen");
  await tf.getByLabel("Was soll der Agent tun?").fill("Prüfe die Abhängigkeiten und aktualisiere sie.");
  await tf.getByRole("button", { name: "Vorlage speichern" }).click();
  await expect(panel.getByTestId("template-row")).toContainText("Nachtlauf");

  // Schedule with live description; more than hourly is explained.
  await panel.getByTestId("new-trigger").click();
  let form = panel.getByTestId("trigger-form");
  await expect(form.getByTestId("kind-schedule").getByRole("radio")).toBeChecked();
  await form.getByLabel("Name des Auslösers").fill("Werktags früh");
  await form.getByTestId("cron").fill("*/5 * * * *");
  await expect(form.getByTestId("cron-text")).toContainText("höchstens einmal pro Stunde");
  await form.getByRole("button", { name: "Werktags um 07:15" }).click();
  await expect(form.getByTestId("cron-text")).toContainText("Mo–Fr um 07:15");
  await form.getByLabel("Zeitzone").fill("Europe/Zurich");
  await expectAccessible(page, "schedule form");
  await form.getByRole("button", { name: "Auslöser speichern" }).click();
  const schedule = panel.getByTestId("trigger-row").filter({ hasText: "Werktags früh" });
  await expect(schedule.getByTestId("trigger-summary")).toContainText("Mo–Fr um 07:15 (Europe/Zurich) · nächster Lauf (deine Uhrzeit)");

  // Webhook: secret and address exactly once.
  await panel.getByTestId("new-trigger").click();
  form = panel.getByTestId("trigger-form");
  await form.getByTestId("kind-webhook").click();
  await form.getByLabel("Name des Auslösers").fill("Deploy");
  await form.getByRole("button", { name: "Auslöser speichern" }).click();
  const dialog = page.getByRole("dialog");
  await expect(dialog).toContainText("Secret für „Deploy“");
  const secret = await dialog.getByTestId("hook-secret").inputValue();
  expect(secret).toMatch(/^qwh_/);
  await expect(dialog.getByTestId("hook-url")).toHaveValue(/\/api\/hooks\/[0-9a-f-]{36}$/);
  await dialog.getByText("So signiert ein Absender").click();
  await expect(dialog).toContainText("X-Queue-Signature");
  await expectAccessible(page, "webhook secret once");
  await dialog.getByRole("button", { name: "Fertig" }).click();
  await expect(page.locator("body")).not.toContainText(secret);

  // GitHub: steps in the dialog.
  await panel.getByTestId("new-trigger").click();
  form = panel.getByTestId("trigger-form");
  await form.getByTestId("kind-github").click();
  await form.getByLabel("Name des Auslösers").fill("CI rot");
  await form.getByTestId("event-push").uncheck();
  await form.getByTestId("event-check_failure").check();
  await form.getByLabel("Branches", { exact: true }).fill("main");
  await form.getByLabel("Repository", { exact: true }).fill("acme/app");
  await expectAccessible(page, "github form");
  await form.getByRole("button", { name: "Auslöser speichern" }).click();
  await expect(page.getByRole("dialog")).toContainText("Payload URL");
  await expect(page.getByRole("dialog")).toContainText("check_run");
  await page.getByRole("dialog").getByRole("button", { name: "Fertig" }).click();
  await expect(panel.getByTestId("trigger-row").filter({ hasText: "CI rot" })).toContainText("Fehlgeschlagene Checks · Branches main · acme/app");

  // Fire the webhook trigger by hand, see the task in the history.
  const deploy = panel.getByTestId("trigger-row").filter({ hasText: "Deploy" });
  await deploy.getByTestId("trigger-fire").click();
  await expect(page.getByText("Auftrag erstellt").first()).toBeVisible();
  await deploy.getByTestId("trigger-history").locator("summary").click();
  const run = deploy.getByTestId("run-row").first();
  await expect(run).toHaveAttribute("data-outcome", "created");
  await expect(run).toContainText("Von Hand");
  await run.getByRole("button", { name: /Öffnen: Abhängigkeiten prüfen \(von Hand\)/ }).click();
  await expect(page.getByRole("dialog")).toContainText("Abhängigkeiten prüfen (von Hand)");
  await page.keyboard.press("Escape");

  // Switch off ⇒ "Jetzt auslösen" disabled.
  await deploy.getByTestId("trigger-enabled").uncheck();
  await expect(page.getByText("Auslöser ausgeschaltet")).toBeVisible();
  await expect(panel.getByTestId("trigger-row").filter({ hasText: "Deploy" }).getByTestId("trigger-fire")).toBeDisabled();
  await expectAccessible(page, "automation tab with triggers");

  // The composer offers the template.
  await page.goto("/");
  const tasks = await (await page.request.get("/api/tasks")).json();
  for (const t of tasks) await waitTask(page.request, t.id, "done");
  await page.getByRole("button", { name: /Erweitert/ }).click();
  await page.getByTestId("composer-template").selectOption({ label: "Nachtlauf" });
  await expect(page.getByLabel("Auftrag beschreiben")).toHaveValue("Prüfe die Abhängigkeiten und aktualisiere sie.");
  await expect(page.getByTestId("composer-template-title")).toContainText("Abhängigkeiten prüfen");
});
