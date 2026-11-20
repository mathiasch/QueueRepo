/** WP10 desktop: Auswertung (KPIs, charts with text alternative and table, filters), session insights in the drawer, notification settings per event and channel. */
import { expect, test } from "@playwright/test";
import { apiProject, expectAccessible, navLink, openTask, uiRegister, waitTask } from "../helpers";

test("analytics dashboard explains the numbers and has tables for every chart", async ({ page }) => {
  await uiRegister(page, "ana");
  const pid = await apiProject(page.request, { name: "Website" });
  const other = await apiProject(page.request, { name: "Ohne Aktivität" });
  for (const prompt of ["Startseite bauen", "Kontaktformular bauen"]) {
    const t = await (await page.request.post("/api/tasks", { data: { project_id: pid, prompt } })).json();
    await waitTask(page.request, t.id, "done");
  }
  await page.goto("/");
  await navLink(page, "Auswertung").click();
  await expect(page.getByRole("heading", { level: 1, name: "Auswertung" })).toBeVisible();
  await expect(page.getByTestId("kpi-first-pass")).toContainText("100 %");
  await expect(page.getByTestId("kpi-first-pass")).toContainText("2 von 2 ohne Reparatur");
  await expect(page.getByTestId("kpi-cache")).toContainText("%");
  await expect(page.getByTestId("kpi-pauses")).toContainText("0 min");
  // Chart: summary for screen readers, table with all values.
  const chart = page.getByTestId("chart-tasks").getByRole("img");
  await expect(chart).toHaveAttribute("aria-label", /2 erledigt, 0 nicht erledigt/);
  await page.getByTestId("chart-tasks").getByText("Als Tabelle anzeigen").click();
  const table = page.getByTestId("chart-tasks").getByRole("table");
  await expect(table.getByRole("row")).toHaveCount(32); // header + 31 days
  await expect(page.getByTestId("by-project")).toContainText("Website");
  await expect(page.getByTestId("by-provider")).toContainText("Mock");
  await expectAccessible(page, "analytics");
  // The most expensive tasks open the drawer.
  await page.getByTestId("top-tasks").getByRole("button").first().click();
  await expect(page.getByRole("dialog")).toBeVisible();
  await page.keyboard.press("Escape");
  // Filters: 7 days, one project; a project without activity is empty.
  await page.getByRole("radio", { name: "7 Tage" }).check();
  await expect(page.getByTestId("chart-tasks").getByRole("img")).toHaveAttribute("aria-label", /über 8 Tage/);
  await page.getByLabel("Bereich").selectOption({ label: "Projekt: Ohne Aktivität" });
  await expect(page.getByText("Noch keine Daten")).toBeVisible();
  expect(other).toBeTruthy();
  await expectAccessible(page, "analytics empty");

  // Session insights in the drawer.
  await navLink(page, "Aufträge").click();
  const drawer = await openTask(page, "Startseite bauen");
  await drawer.getByTestId("task-insights").locator("summary").click();
  const ins = drawer.getByTestId("task-insights");
  await expect(ins).toContainText("Arbeitszeit der Agenten");
  await expect(ins).toContainText("Häufigste Werkzeuge: edit");
  await expect(ins.getByRole("table")).toContainText("Agent");
  await expectAccessible(page, "drawer insights");
});

test("notification settings per event and channel survive a reload", async ({ page }) => {
  await uiRegister(page, "notif");
  await page.goto("/settings");
  const box = page.getByTestId("notify-settings");
  await expect(box).toBeVisible();
  const done = box.locator('fieldset[data-event="task_done"]');
  await expect(done.getByRole("checkbox", { name: "Push" })).not.toBeChecked();
  await done.getByRole("checkbox", { name: "Push" }).check();
  await box.locator('fieldset[data-event="blocked"]').getByRole("checkbox", { name: "ntfy" }).uncheck();
  await box.getByTestId("quiet-enabled").check();
  await box.getByLabel("Von").fill("21:30");
  await box.getByLabel("Bis").fill("06:45");
  await box.getByLabel("Zeitzone").fill("Europe/Berlin");
  await box.getByTestId("digest-enabled").check();
  await box.getByLabel("Uhrzeit").fill("07:15");
  await box.getByTestId("notify-save").click();
  await expect(page.getByText("Benachrichtigungen gespeichert")).toBeVisible();
  await expectAccessible(page, "notification settings");
  await page.reload();
  const box2 = page.getByTestId("notify-settings");
  await expect(box2.locator('fieldset[data-event="task_done"]').getByRole("checkbox", { name: "Push" })).toBeChecked();
  await expect(box2.locator('fieldset[data-event="blocked"]').getByRole("checkbox", { name: "ntfy" })).not.toBeChecked();
  await expect(box2.getByTestId("quiet-enabled")).toBeChecked();
  await expect(box2.getByLabel("Von")).toHaveValue("21:30");
  await expect(box2.getByLabel("Zeitzone")).toHaveValue("Europe/Berlin");
  await expect(box2.getByLabel("Uhrzeit")).toHaveValue("07:15");
  // Equal start and end: understandable error, nothing saved.
  await box2.getByLabel("Bis").fill("21:30");
  await box2.getByTestId("notify-save").click();
  await expect(box2.getByRole("alert")).toContainText("Beginn und Ende");
});
