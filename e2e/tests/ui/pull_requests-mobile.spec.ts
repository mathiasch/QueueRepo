/** WP05 on the iPhone: delivery settings and the pull request block – safe areas, touch targets, axe. */
import { expect, test } from "@playwright/test";
import { expectAccessible, openTask, prProject, simulateInsets, taskPull, uiRegister, waitTask } from "../helpers";

test("delivery settings and the pull request fit on the iPhone", async ({ page }) => {
  const landscape = await simulateInsets(page);
  await uiRegister(page, "mpr");
  const p = await prProject(page.request, "MPR");
  const tid = (await (await page.request.post("/api/tasks", { data: { project_id: p.pid, prompt: "Mobiler Pull Request MOCK_FILE:m.txt" } })).json()).id;
  await waitTask(page.request, tid, "done");
  await expect.poll(async () => (await taskPull(page.request, tid))?.state, { timeout: 45_000 }).toBe("open");
  const vp = page.viewportSize()!;
  const inside = async (box: { x: number; width: number } | null) => {
    expect(box!.x).toBeGreaterThanOrEqual(landscape ? 59 : 0);
    expect(box!.x + box!.width).toBeLessThanOrEqual(vp.width - (landscape ? 59 : 0));
  };

  await page.goto(`/projects/${p.pid}?tab=delivery`);
  const host = page.getByLabel("Git-Server", { exact: true });
  await host.scrollIntoViewIfNeeded();
  expect((await host.boundingBox())!.height, "touch target").toBeGreaterThanOrEqual(44);
  expect(parseFloat(await host.evaluate((e) => getComputedStyle(e).fontSize)), "iOS zooms into inputs < 16px").toBeGreaterThanOrEqual(16);
  await inside(await host.boundingBox());
  const radio = page.getByTestId("mode-pr");
  await radio.scrollIntoViewIfNeeded();
  expect((await radio.boundingBox())!.height).toBeGreaterThanOrEqual(44);
  await inside(await radio.boundingBox());
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(vp.width);
  await expectAccessible(page, `delivery (${landscape ? "landscape" : "portrait"})`);

  await page.goto("/");
  const drawer = await openTask(page, "Mobiler Pull Request", "done");
  const block = drawer.getByTestId("pr-block");
  await block.scrollIntoViewIfNeeded();
  await expect(block.getByTestId("pr-link")).toBeVisible();
  await inside(await block.boundingBox());
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(vp.width);
  await expectAccessible(page, `pull request drawer (${landscape ? "landscape" : "portrait"})`);
  await page.screenshot({ path: test.info().outputPath(`${landscape ? "landscape" : "portrait"}-pr.png`) });
});
