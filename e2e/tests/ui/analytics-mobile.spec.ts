/** WP10 iPhone (portrait + landscape): Auswertung and notification settings fit, 44-pt targets, 16-px inputs, safe areas, axe. */
import { expect, test } from "@playwright/test";
import { apiProject, expectAccessible, navLink, simulateInsets, uiRegister, waitTask } from "../helpers";

test("analytics and notification settings on the iPhone", async ({ page }) => {
  const landscape = await simulateInsets(page);
  const side = landscape ? 59 : 0;
  await uiRegister(page, "anam");
  const pid = await apiProject(page.request, { name: "Handy" });
  const t = await (await page.request.post("/api/tasks", { data: { project_id: pid, prompt: "Knopf reparieren" } })).json();
  await waitTask(page.request, t.id, "done");
  await page.goto("/");
  const vp = page.viewportSize()!;
  const tab = navLink(page, "Auswertung");
  const tb = (await tab.boundingBox())!;
  expect(tb.height).toBeGreaterThanOrEqual(44);
  expect(tb.x).toBeGreaterThanOrEqual(side);
  expect(tb.x + tb.width).toBeLessThanOrEqual(vp.width - side);
  await tab.tap();
  await expect(page.getByTestId("kpi-first-pass")).toBeVisible();
  await page.getByRole("radio", { name: "90 Tage" }).check();
  await expect(page.getByTestId("chart-tasks").getByRole("img")).toHaveAttribute("aria-label", /über 91 Tage/);
  expect(await page.evaluate(() => document.documentElement.scrollWidth), "no horizontal scroll").toBeLessThanOrEqual(vp.width);
  for (const k of ["kpi-first-pass", "kpi-costs", "chart-tasks", "chart-cost"]) {
    const b = (await page.getByTestId(k).boundingBox())!;
    expect(b.x, k).toBeGreaterThanOrEqual(side);
    expect(b.x + b.width, k).toBeLessThanOrEqual(vp.width - side + 1);
  }
  const sel = await page.getByLabel("Bereich").evaluate((e) => parseFloat(getComputedStyle(e).fontSize));
  expect(sel, "iOS zooms into inputs < 16px").toBeGreaterThanOrEqual(16);
  await expectAccessible(page, `analytics (${landscape ? "landscape" : "portrait"})`);
  await page.screenshot({ path: test.info().outputPath(`${landscape ? "landscape" : "portrait"}-analytics.png`), fullPage: true });

  await page.goto("/settings#notify-rules");
  const box = page.getByTestId("notify-settings");
  await expect(box).toBeVisible();
  const sizes = await box.evaluate((root) =>
    [...root.querySelectorAll<HTMLElement>("input:not([type=checkbox]), select")].map((e) => parseFloat(getComputedStyle(e).fontSize)),
  );
  expect(Math.min(...sizes)).toBeGreaterThanOrEqual(16);
  const push = box.locator('fieldset[data-event="needs_input"]').getByRole("checkbox", { name: "Push" });
  await push.scrollIntoViewIfNeeded();
  const label = (await push.locator("xpath=..").boundingBox())!;
  expect(label.height, "checkbox row is a 44-pt target").toBeGreaterThanOrEqual(44);
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(vp.width);
  const save = page.getByTestId("notify-save");
  await save.scrollIntoViewIfNeeded();
  const s = (await save.boundingBox())!;
  expect(s.height).toBeGreaterThanOrEqual(44);
  expect(s.y + s.height).toBeLessThanOrEqual(vp.height - (landscape ? 21 : 34));
  await expectAccessible(page, `notification settings (${landscape ? "landscape" : "portrait"})`);
});
