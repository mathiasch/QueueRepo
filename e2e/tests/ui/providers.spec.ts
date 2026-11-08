/**
 * WP13 in the UI: additional accesses, model choice per project (presets,
 * data residency), cost estimate before submitting, costs per task, price
 * table and CLI status. Mock provider only – no real provider API calls.
 */
import { expect, test } from "@playwright/test";
import { apiProject, navLink, openTask, taskRow, uiRegister, waitTask } from "../helpers";

test("an access is added in plain language and its key is never shown again", async ({ page }) => {
  await uiRegister(page, "conn");
  await navLink(page, "Einstellungen").click();
  const card = page.locator("#connectors");
  await card.getByRole("button", { name: "Zugang hinzufügen" }).click();
  const form = page.getByTestId("connector-form");
  await form.getByLabel("Art des Zugangs").selectOption("mistral");
  await expect(form.getByText("Nicht verifiziert")).toBeVisible();
  await form.getByLabel("Bezeichnung", { exact: true }).fill("Mistral EU");
  await form.getByLabel("API-Schlüssel", { exact: true }).fill("sk-e2e-SECRET-1");
  await form.getByRole("button", { name: "Zugang speichern" }).click();
  const row = page.getByTestId("connector-Mistral EU");
  await expect(row).toBeVisible();
  await expect(row).toContainText("Daten: EU");
  await expect(row).toContainText("Batch −50 %");
  await expect(page.locator("body")).not.toContainText("SECRET-1");
  expect(JSON.stringify(await (await page.request.get("/api/connectors")).json())).not.toContain("SECRET-1");

  // Server validation is shown in German at the form.
  await card.getByRole("button", { name: "Zugang hinzufügen" }).click();
  await form.getByLabel("Art des Zugangs").selectOption("infomaniak");
  await form.getByLabel("Bezeichnung", { exact: true }).fill("Schweiz");
  await form.getByLabel("API-Token", { exact: true }).fill("tok");
  await form.getByLabel("OpenAI-kompatible Basis-URL").fill("http://ai.example.ch/v1");
  await form.getByRole("button", { name: "Zugang speichern" }).click();
  await expect(form.getByRole("alert")).toContainText("https://");
  await form.getByRole("button", { name: "Abbrechen" }).click();

  // Removing asks first.
  await row.getByRole("button", { name: "Zugang Mistral EU entfernen" }).click();
  await page.getByRole("alertdialog", { name: "Zugang „Mistral EU“ entfernen?" }).getByRole("button", { name: "Entfernen" }).click();
  await expect(row).toBeHidden();
});

test("EU preset sets the residency rule, the composer estimates costs, the server enforces it", async ({ page }) => {
  await uiRegister(page, "route");
  const pid = await apiProject(page.request, { name: "EU-Projekt" });
  const c = await page.request.post("/api/connectors", { data: { kind: "mistral", label: "Mistral EU", secret: "k", config: { endpoint: "eu" } } });
  expect(c.status()).toBe(200);
  await page.goto(`/projects/${pid}?tab=settings`);
  const routing = page.getByTestId("project-routing");
  await expect(routing.getByTestId("routing-current")).toContainText("Standard:");
  const eu = routing.getByTestId("preset-eu");
  await expect(eu).toContainText("pro Versuch");
  await eu.getByRole("button", { name: "Übernehmen" }).click();
  await expect(eu.getByRole("button", { name: "Aktiv" })).toBeDisabled();
  await expect(routing.getByTestId("routing-current")).toContainText("Mistral EU · mistral-large-4");
  await expect(routing.getByRole("radio", { name: "Nur EU/Schweiz" })).toBeChecked();
  // Presets without the needed access explain why.
  await expect(routing.getByTestId("preset-cheapest")).toContainText("OpenAI API");

  // Composer: estimate before submitting, with the data location.
  await navLink(page, "Aufträge").click();
  const estimate = page.getByTestId("estimate");
  await expect(estimate).toContainText("Geschätzte Kosten");
  await expect(estimate).toContainText("pro Versuch");
  await expect(estimate).toContainText("Daten: EU");
  await estimate.getByRole("button", { name: "Details" }).click();
  await expect(estimate.getByRole("table")).toContainText("Umsetzung");
  await expect(estimate.getByRole("table")).toContainText("mistral-large-4");

  // Server-side rule: a US access is refused for this project.
  const r = await page.request.post("/api/tasks", { data: { project_id: pid, prompt: "x", provider: "claude" } });
  expect(r.status()).toBe(400);
  expect(await r.text()).toContain("Nur EU/Schweiz");

  // Back to the standard behaviour.
  await page.goto(`/projects/${pid}?tab=settings`);
  await routing.getByRole("button", { name: "Zurück zum Standard" }).click();
  await expect(routing.getByTestId("routing-current")).toContainText("Standard:");
});

test("costs are shown per step in the details and in the settings", async ({ page }) => {
  await uiRegister(page, "costs");
  await apiProject(page.request, { name: "Kosten" });
  await page.reload();
  await page.getByRole("button", { name: "Erweitert" }).click();
  const composer = page.locator("#composer");
  await composer.getByLabel("Modell", { exact: true }).fill("claude-sonnet-5-5");
  await composer.getByLabel("Modell", { exact: true }).press("Tab");
  // Report profile M on Sonnet 5.5 (agent loop $1.245) + review (35k/3k tokens, $0.10).
  await expect(page.getByTestId("estimate")).toContainText("1,35 $");
  await composer.getByLabel("Auftrag beschreiben").fill("Preis zeigen");
  await composer.getByRole("button", { name: "Einreihen" }).click();
  await expect(taskRow(page, "Preis zeigen", "done")).toBeVisible({ timeout: 30_000 });
  const drawer = await openTask(page, "Preis zeigen", "done");
  // Mock: 1200 in / 150 out / 800 cache read per run (agent + review) at Sonnet 5.5 prices.
  await expect(drawer.getByTestId("task-cost")).toContainText("0,0080 $");
  const table = drawer.getByRole("table");
  await expect(table).toContainText("Umsetzung");
  await expect(table).toContainText("Prüfung");
  await expect(table).toContainText("aus der Preistabelle");
  await page.keyboard.press("Escape");

  await navLink(page, "Einstellungen").click();
  await expect(page.getByTestId("costs-total")).toContainText("0,0080 $");
  await expect(page.locator("#costs")).toContainText("claude-sonnet-5-5");
});

test("price table with sources and the sandbox CLI check", async ({ page }) => {
  await uiRegister(page, "prices");
  await navLink(page, "Einstellungen").click();
  const prices = page.locator("#prices");
  await prices.getByLabel("Modell suchen").fill("Sonnet 5.5");
  const row = prices.getByTestId("price-table").getByRole("row", { name: /Claude Sonnet 5\.5/ });
  await expect(row).toContainText("2,00 $");
  await expect(row).toContainText("10,00 $");
  await expect(row.getByRole("link", { name: "Anbieterseite gelesen" })).toHaveAttribute("href", /platform\.claude\.com/);
  await prices.getByLabel("Modell suchen").fill("Argon");
  await expect(prices.getByTestId("price-table")).toContainText("ab 01.12.2026");
  // The Gemini CLI note is always visible; the live check lists every CLI.
  const cli = page.locator("#cli-status");
  await expect(cli).toContainText("eingefroren");
  await cli.getByRole("button", { name: "Jetzt in der Sandbox prüfen" }).click();
  await expect(cli.getByTestId("cli-status").locator("li")).toHaveCount(5);
  await expect(cli.getByTestId("cli-status")).toContainText("agy");
});

test("a model test runs the eval set with the mock and shows the success rate", async ({ page }) => {
  test.setTimeout(120_000);
  await uiRegister(page, "eval");
  const s = await (await page.request.get("/api/settings")).json();
  await page.request.put("/api/settings", { data: { ...s, parallelism: 4 } });
  await navLink(page, "Einstellungen").click();
  const card = page.locator("#evals");
  await card.getByLabel("Zugang").selectOption({ label: "Mock" });
  await card.getByLabel("Modell zum Testen").fill("mock-weak");
  await card.getByRole("button", { name: "Test starten" }).click();
  await expect(card.getByTestId("eval-runs")).toContainText("3 von 8 gelöst", { timeout: 90_000 });
  // Eval projects stay out of the project list.
  await navLink(page, "Projekte").click();
  await expect(page.locator(".project-card")).toHaveCount(0);
});
