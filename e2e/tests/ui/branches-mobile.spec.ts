/** WP04 on the iPhone: parallelism setting and the branch block – safe areas, touch targets, axe. */
import { expect, test } from "@playwright/test";
import { apiProject, expectAccessible, openTask, simulateInsets, uiRegister, waitTask } from "../helpers";

test("parallelism and branch info fit on the iPhone", async ({ page }) => {
  const landscape = await simulateInsets(page);
  await uiRegister(page, "mbranch");
  const pid = await apiProject(page.request, { name: "MBranch" });
  await page.request.patch(`/api/projects/${pid}`, { data: { review_enabled: false } });
  const tid = (await (await page.request.post("/api/tasks", { data: { project_id: pid, prompt: "Mobile Branch-Ansicht" } })).json()).id;
  await waitTask(page.request, tid, "done");
  const vp = page.viewportSize()!;

  await page.goto(`/projects/${pid}?tab=settings`);
  const select = page.getByLabel("Gleichzeitige Aufträge");
  await select.scrollIntoViewIfNeeded();
  const box = (await select.boundingBox())!;
  expect(box.height, "touch target").toBeGreaterThanOrEqual(44);
  expect(box.x).toBeGreaterThanOrEqual(landscape ? 59 : 0);
  expect(box.x + box.width).toBeLessThanOrEqual(vp.width - (landscape ? 59 : 0));
  expect(parseFloat(await select.evaluate((e) => getComputedStyle(e).fontSize)), "iOS zooms into inputs < 16px").toBeGreaterThanOrEqual(16);
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(vp.width);
  await expectAccessible(page, `settings (${landscape ? "landscape" : "portrait"})`);

  await page.goto(`/projects/${pid}?tab=git`);
  await expect(page.getByTestId("merge-queue")).toBeVisible();
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(vp.width);
  await expectAccessible(page, `merge queue (${landscape ? "landscape" : "portrait"})`);

  await page.goto("/");
  const drawer = await openTask(page, "Mobile Branch-Ansicht", "done");
  const block = drawer.getByTestId("branch-block");
  await block.scrollIntoViewIfNeeded();
  await expect(block.getByTestId("merge-state")).toHaveText("Übernommen");
  const b = (await block.boundingBox())!;
  expect(b.x).toBeGreaterThanOrEqual(landscape ? 59 : 0);
  expect(b.x + b.width).toBeLessThanOrEqual(vp.width - (landscape ? 59 : 0));
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(vp.width);
  await expectAccessible(page, `branch drawer (${landscape ? "landscape" : "portrait"})`);
  await page.screenshot({ path: test.info().outputPath(`${landscape ? "landscape" : "portrait"}-branch.png`) });
});
