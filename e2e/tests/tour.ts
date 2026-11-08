/**
 * Walks through every main view with realistic data: axe-core accessibility
 * checks (WCAG 2.0/2.1/2.2 A+AA) in light and dark mode plus screenshots for
 * the WP00 report.
 */
import { expect, type Page, type TestInfo } from "@playwright/test";
import { apiProject, expectAccessible, navLink, shot, uiRegister, waitTask } from "./helpers";

async function seed(page: Page) {
  await uiRegister(page, "ada", "Ada Lovelace");
  const pid = await apiProject(page.request, { name: "Vereins-Website", goal: "Schnelle, barrierefreie Website für den Sportverein" });
  await apiProject(page.request, { name: "Newsletter-Tool" });
  await page.request.patch(`/api/projects/${pid}`, { data: { description: "Website des TSV", verify_command: "test -f MOCK_LOG.md" } });
  const add = async (prompt: string, extra: Record<string, unknown> = {}) =>
    (await (await page.request.post("/api/tasks", { data: { project_id: pid, prompt, ...extra } })).json()).id as string;
  await waitTask(page.request, await add("Startseite mit Terminen und Kontaktformular bauen"), "done");
  await waitTask(page.request, await add("MOCK_ASK Welche Datenbank soll das Kontaktformular nutzen?"), "waiting_input");
  const slow = await add("MOCK_SLOW Bilder der Galerie für Mobilgeräte optimieren");
  await waitTask(page.request, slow, "running");
  await add("Impressum und Datenschutzerklärung ergänzen", { max_tokens: 200000, max_turns: 30 });
  await add("Barrierefreiheit der Navigation prüfen");
  return { pid, slow };
}

export async function tour(page: Page, info: TestInfo, device: string, full: boolean) {
  const out = (n: string) => info.outputPath(n);
  for (const scheme of ["light", "dark"] as const) {
    await page.emulateMedia({ colorScheme: scheme });
    await page.goto("/");
    await expect(page.getByRole("button", { name: "Anmelden" })).toBeVisible();
    await expectAccessible(page, `login (${scheme})`);
    if (full) await shot(page, out, `${device}-${scheme}-login`);
  }
  const { slow } = await seed(page);
  for (const scheme of ["light", "dark"] as const) {
    await page.emulateMedia({ colorScheme: scheme });
    await page.goto("/");
    await expect(page.locator('article[data-status="waiting_input"]')).toBeVisible();
    // Cost estimate of the composer, with its step table (WP13).
    await page.getByTestId("estimate").getByRole("button", { name: "Details" }).click();
    await expect(page.getByTestId("estimate").getByRole("table")).toBeVisible();
    await expectAccessible(page, `queue (${scheme})`);
    await shot(page, out, `${device}-${scheme}-queue`, device === "desktop");

    await page.locator('article[data-status="done"] button.open').first().click();
    const drawer = page.getByRole("dialog");
    await drawer.getByRole("button", { name: "Änderungen anzeigen" }).click();
    await expect(drawer.locator(".diff")).toBeVisible();
    await drawer.getByText("Protokoll anzeigen").click();
    await expectAccessible(page, `drawer (${scheme})`);
    await shot(page, out, `${device}-${scheme}-drawer`);
    await page.keyboard.press("Escape");
    await expect(drawer).toBeHidden();
    if (!full) continue;

    await navLink(page, "Postfach").click();
    await expect(page.getByRole("heading", { level: 1, name: "Postfach" })).toBeVisible();
    await expectAccessible(page, `inbox (${scheme})`);
    await shot(page, out, `${device}-${scheme}-inbox`);

    await navLink(page, "Projekte").click();
    await expect(page.locator(".project-card")).toHaveCount(2);
    await expectAccessible(page, `projects (${scheme})`);
    await shot(page, out, `${device}-${scheme}-projects`);
    await page.locator(".project-card").getByRole("button", { name: "Vereins-Website" }).click();
    await page.locator(".file", { hasText: "MOCK_LOG.md" }).click();
    await expect(page.locator(".file-view pre")).toBeVisible();
    await expectAccessible(page, `project files (${scheme})`);
    await page.getByRole("tab", { name: "Verlauf" }).click();
    await expect(page.locator(".commit-sha").first()).toBeVisible();
    await expectAccessible(page, `project git (${scheme})`);
    await page.getByRole("tab", { name: "Einstellungen" }).click();
    await expect(page.getByTestId("project-limits")).toBeVisible();
    await expect(page.getByTestId("project-routing").getByTestId("preset-balanced")).toBeVisible();
    await expectAccessible(page, `project settings (${scheme})`);
    await shot(page, out, `${device}-${scheme}-project-settings`, device === "desktop");
    // WP02: environment editor with the network presets expanded.
    await page.getByRole("tab", { name: "Umgebung" }).click();
    await expect(page.getByTestId("sandbox-status")).toContainText("nicht durchgesetzt");
    await page.getByTestId("net-agent").getByRole("radio", { name: "Erlaubte Adressen" }).check();
    await expect(page.getByTestId("net-agent").getByText("Python (PyPI)")).toBeVisible();
    await expectAccessible(page, `project environment (${scheme})`);
    await shot(page, out, `${device}-${scheme}-project-environment`, device === "desktop");
    await page.getByRole("tab", { name: "Mitglieder" }).click();
    await expect(page.locator(".member")).toHaveCount(1);
    await expectAccessible(page, `project members (${scheme})`);

    await navLink(page, "Einstellungen").click();
    await expect(page.getByTestId("budget-mock")).toBeVisible();
    await page.getByTestId("credential-claude").getByRole("button", { name: "Verbinden" }).click();
    // WP13: form for an additional access, price table, costs.
    await page.locator("#connectors").getByRole("button", { name: "Zugang hinzufügen" }).click();
    await expect(page.getByTestId("connector-form")).toBeVisible();
    await expect(page.getByTestId("price-table")).toBeVisible();
    await expectAccessible(page, `settings (${scheme})`);
    await shot(page, out, `${device}-${scheme}-settings`, device === "desktop");

    await page.keyboard.press("?");
    await expect(page.getByRole("dialog", { name: "Tastaturkürzel" })).toBeVisible();
    await expectAccessible(page, `shortcuts dialog (${scheme})`);
    await page.keyboard.press("Escape");
  }
  await page.request.post(`/api/tasks/${slow}/cancel`);
}
