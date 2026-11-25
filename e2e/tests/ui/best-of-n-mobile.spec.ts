/** WP12 on the iPhone (portrait and landscape): stepper in the composer,
 *  comparison cards, selection – safe areas, 44-pt targets, no sideways
 *  scrolling, axe. */
import { expect, test } from "@playwright/test";
import { apiProject, expectAccessible, openTask, simulateInsets, uiRegister, waitTask } from "../helpers";

test("comparison and selection fit on the iPhone", async ({ page }) => {
  const landscape = await simulateInsets(page);
  await uiRegister(page, "mbon");
  const s = await (await page.request.get("/api/settings")).json();
  await page.request.put("/api/settings", { data: { ...s, parallelism: 2 } });
  const pid = await apiProject(page.request, { name: "MBon" });
  await page.request.patch(`/api/projects/${pid}`, { data: { review_enabled: false } });
  const vp = page.viewportSize()!;
  const inset = landscape ? 59 : 0;

  await page.goto("/");
  await page.getByRole("button", { name: "Erweitert" }).click();
  const more = page.getByTestId("best-of-n").getByRole("button", { name: "Mehr Versuche" });
  await more.scrollIntoViewIfNeeded();
  const mb = (await more.boundingBox())!;
  expect(mb.height, "touch target").toBeGreaterThanOrEqual(44);
  expect(mb.width).toBeGreaterThanOrEqual(44);
  await more.click();
  const model = page.getByLabel("Versuch 2 – Modell");
  expect(parseFloat(await model.evaluate((e) => getComputedStyle(e).fontSize)), "no iOS zoom").toBeGreaterThanOrEqual(16);
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(vp.width);
  await expectAccessible(page, `composer attempts (${landscape ? "landscape" : "portrait"})`);

  const g = (await (await page.request.post("/api/tasks", { data: { project_id: pid, prompt: "Mobiler Vergleich MOCK_FILE:m.md", attempts: 2 } })).json()).id;
  await expect
    .poll(async () => ((await (await page.request.get(`/api/tasks/${g}/attempts`)).json()).attempts ?? []).filter((a: { status: string }) => a.status === "candidate").length, { timeout: 60_000 })
    .toBe(2);
  await page.reload();
  const drawer = await openTask(page, "Mobiler Vergleich", "comparing");
  const card = drawer.getByTestId("attempt-1");
  await card.scrollIntoViewIfNeeded();
  const cb = (await card.boundingBox())!;
  expect(cb.x).toBeGreaterThanOrEqual(inset);
  expect(cb.x + cb.width).toBeLessThanOrEqual(vp.width - inset + 1);
  const pick = card.getByRole("button", { name: "Diesen übernehmen" });
  expect((await pick.boundingBox())!.height).toBeGreaterThanOrEqual(44);
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(vp.width);
  await expectAccessible(page, `comparison (${landscape ? "landscape" : "portrait"})`);
  await page.screenshot({ path: test.info().outputPath(`${landscape ? "landscape" : "portrait"}-comparison.png`) });
  await pick.click();
  await page.getByRole("button", { name: "Übernehmen", exact: true }).click();
  await expect(drawer.getByTestId("comparison-status")).toContainText("Versuch 1 wurde von dir übernommen", { timeout: 30_000 });
  const won = (await (await page.request.get(`/api/tasks/${g}/attempts`)).json()).attempts.find((a: { attempt_no: number }) => a.attempt_no === 1).id;
  await waitTask(page.request, won, "done");
});
