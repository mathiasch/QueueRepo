/** WP03: decision on a held task and the protection tab on the iPhone – safe areas, touch targets, axe. */
import { expect, test } from "@playwright/test";
import { apiProject, expectAccessible, openTask, simulateInsets, uiRegister, waitTask } from "../helpers";

test("held task can be decided on the iPhone; protection tab fits", async ({ page }) => {
  const landscape = await simulateInsets(page);
  await uiRegister(page, "mgate");
  const pid = await apiProject(page.request, { name: "MGate" });
  await page.request.patch(`/api/projects/${pid}`, { data: { review_enabled: false } });
  const tid = (await (await page.request.post("/api/tasks", { data: { project_id: pid, prompt: "MOCK_SECRET mobile Freigabe" } })).json()).id;
  await waitTask(page.request, tid, "held");
  await page.reload();
  const drawer = await openTask(page, "mobile Freigabe", "held");
  const panel = drawer.getByTestId("gate-panel");
  await expect(panel).toBeVisible();
  const vp = page.viewportSize()!;
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(vp.width);
  for (const name of ["Freigeben", "Änderungen verwerfen", "Zurückgehaltene Änderungen ansehen"]) {
    const b = panel.getByRole("button", { name });
    await b.scrollIntoViewIfNeeded();
    const box = (await b.boundingBox())!;
    expect(box.height, `${name} touch target`).toBeGreaterThanOrEqual(44);
    expect(box.x).toBeGreaterThanOrEqual(landscape ? 59 : 0);
    expect(box.x + box.width).toBeLessThanOrEqual(vp.width - (landscape ? 59 : 0));
  }
  await expectAccessible(page, `gate drawer (${landscape ? "landscape" : "portrait"})`);
  await page.screenshot({ path: test.info().outputPath(`${landscape ? "landscape" : "portrait"}-gate.png`) });
  await page.keyboard.press("Escape");

  await page.goto(`/projects/${pid}?tab=protection`);
  await expect(page.getByTestId("protection-form")).toBeVisible();
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(vp.width);
  const sizes = await page.locator("#project-panel").evaluate((root) =>
    [...root.querySelectorAll<HTMLElement>("textarea, input:not([type=checkbox])")].map((e) => parseFloat(getComputedStyle(e).fontSize)),
  );
  expect(Math.min(...sizes), "iOS zooms into inputs < 16px").toBeGreaterThanOrEqual(16);
  const save = page.getByRole("button", { name: "Schutz speichern" });
  await save.scrollIntoViewIfNeeded();
  const s = (await save.boundingBox())!;
  expect(s.height).toBeGreaterThanOrEqual(44);
  expect(s.y + s.height).toBeLessThanOrEqual(vp.height - (landscape ? 21 : 34));
  if (!landscape) expect(s.y + s.height).toBeLessThanOrEqual((await page.locator(".tabs").boundingBox())!.y);
  await expectAccessible(page, `protection (${landscape ? "landscape" : "portrait"})`);
});
