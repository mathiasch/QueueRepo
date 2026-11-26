/** WP15 on the desktop: automatic model choice in the project, the
 *  explanation at the task ("Warum dieses Modell?" with alternatives as a
 *  table), override, feedback, and the admin view with the replay – axe
 *  light/dark. */
import { expect, test, type APIRequestContext } from "@playwright/test";
import { ADMIN_EMAIL, apiProject, expectAccessible, navLink, openTask, uiLogin, uiRegister, waitTask } from "../helpers";

async function taskId(request: APIRequestContext, prefix: string) {
  const ts = await (await request.get("/api/tasks")).json();
  return ts.find((t: { title: string }) => t.title.startsWith(prefix)).id as string;
}

test("chooser: settings, explanation, override and feedback", async ({ page }) => {
  await uiRegister(page, "choose");
  const pid = await apiProject(page.request, { name: "Modellwahl" });

  await page.goto(`/projects/${pid}?tab=settings`);
  const card = page.getByTestId("chooser-card");
  await expect(card.getByRole("heading", { name: "Automatische Modellwahl" })).toBeVisible();
  await card.getByTestId("chooser-enabled").check();
  await card.getByTestId("chooser-quality").fill("40");
  await expect(card).toContainText("40 von 100 – eher günstig");
  await card.getByLabel("Budget zum Ausprobieren ($ / 30 Tage)").fill("0");
  await card.getByTestId("chooser-save").click();
  await expect(page.getByText("Automatische Modellwahl gespeichert")).toBeVisible();
  await expectAccessible(page, "chooser settings");

  // A long task keeps the project busy, so the next one waits (override).
  const blocker = (await (await page.request.post("/api/tasks", { data: { project_id: pid, prompt: "Blockiert MOCK_LONG" } })).json()).id;
  await waitTask(page.request, blocker, "running");
  await page.goto("/");
  await page.getByRole("textbox", { name: "Auftrag beschreiben" }).fill("Dokumentiere die Option --count in README.md");
  await page.getByRole("button", { name: "Einreihen" }).click();
  const drawer = await openTask(page, "Dokumentiere die Option", "queued");
  const why = drawer.getByTestId("routing-decision");
  await expect(why).toContainText("Automatische Modellwahl");
  const report = why.getByTestId("chooser-report");
  await expect(report).toContainText("Gewählt: claude-haiku-5-5");
  await expect(report).toContainText("Dokumentation");
  await expect(report.getByTestId("chooser-cpa")).toContainText("$");
  await report.getByTestId("chooser-alternatives").locator("summary").click();
  const table = report.getByRole("table", { name: "Alternativen der Modellwahl" });
  await expect(table).toBeVisible();
  await expect(table.getByRole("row", { name: /claude-sonnet-5-5/ }).first()).toBeVisible();
  await expectAccessible(page, "explanation (dark)");
  await page.emulateMedia({ colorScheme: "light" });
  await expectAccessible(page, "explanation (light)");

  await table.getByRole("button", { name: "Stattdessen claude-sonnet-5-5 verwenden" }).first().click();
  await expect(page.getByText("Modell geändert: claude-sonnet-5-5")).toBeVisible();
  const id = await taskId(page.request, "Dokumentiere die Option");
  await expect.poll(async () => (await (await page.request.get(`/api/tasks/${id}`)).json()).task.routing_decision.source).toBe("override");
  await page.request.post(`/api/tasks/${blocker}/cancel`);
  await waitTask(page.request, id, "done");
  // The open drawer survives a reload (address): close it first.
  await page.keyboard.press("Escape");
  await expect(page.getByRole("dialog")).toBeHidden();
  await page.goto("/");
  const done = await openTask(page, "Dokumentiere die Option", "done");
  await expect(done.getByTestId("chooser-overridden")).toContainText("claude-haiku-5-5");
  const fb = done.getByTestId("chooser-feedback");
  await fb.getByRole("button", { name: "Schlecht" }).click();
  await expect(fb.getByRole("button", { name: "Schlecht" })).toHaveAttribute("aria-pressed", "true");
  await expect((await (await page.request.get(`/api/tasks/${id}/feedback`)).json()).good).toBe(false);
  await expectAccessible(page, "feedback");
  await page.screenshot({ path: test.info().outputPath("chooser-explanation.png") });
});

test("chooser: admin statistics and replay against the presets", async ({ page }) => {
  await uiLogin(page, ADMIN_EMAIL);
  await expect(navLink(page, "Aufträge")).toBeVisible();
  await page.getByTestId("admin-link").click();
  await page.getByRole("tab", { name: "Modellwahl" }).click();
  const card = page.getByTestId("admin-chooser");
  await expect(card.getByRole("table", { name: /Gelernte Erfolgsquoten/ })).toBeVisible();
  await card.getByTestId("replay-synthetic").click();
  const t = card.getByRole("table", { name: "Ergebnis des Replays" });
  await expect(t).toBeVisible({ timeout: 60_000 });
  await expect(t).toContainText("Vorlage „Ausgewogen“");
  await expect(t).toContainText("Automatische Modellwahl");
  await expect(card.getByTestId("replay-verdict")).toContainText("pro erledigtem Auftrag");
  await expect(card).toContainText("Synthetische Historie");
  for (const scheme of ["light", "dark"] as const) {
    await page.emulateMedia({ colorScheme: scheme });
    await expectAccessible(page, `admin chooser (${scheme})`);
  }
});
