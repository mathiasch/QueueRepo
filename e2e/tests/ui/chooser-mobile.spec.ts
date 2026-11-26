/** WP15 on the iPhone (portrait and landscape): the explanation of the
 *  model choice and its settings – safe areas, 44-pt targets, 16-px inputs,
 *  no sideways scrolling, axe. */
import { expect, test } from "@playwright/test";
import { apiProject, expectAccessible, openTask, simulateInsets, uiRegister, waitTask } from "../helpers";

test("model choice explanation and settings fit on the iPhone", async ({ page }) => {
  const landscape = await simulateInsets(page);
  const mode = landscape ? "landscape" : "portrait";
  await uiRegister(page, "mchoose");
  const pid = await apiProject(page.request, { name: "MModellwahl" });
  const vp = page.viewportSize()!;
  const inset = landscape ? 59 : 0;
  const noSideways = async () => expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(vp.width);

  await page.goto(`/projects/${pid}?tab=settings`);
  const card = page.getByTestId("chooser-card");
  await card.scrollIntoViewIfNeeded();
  const slider = card.getByTestId("chooser-quality");
  expect((await slider.boundingBox())!.height, "touch target").toBeGreaterThanOrEqual(44);
  const budget = card.getByLabel("Budget zum Ausprobieren ($ / 30 Tage)");
  expect(parseFloat(await budget.evaluate((e) => getComputedStyle(e).fontSize)), "no iOS zoom").toBeGreaterThanOrEqual(16);
  await card.getByTestId("chooser-enabled").check();
  await budget.fill("0");
  const save = card.getByTestId("chooser-save");
  expect((await save.boundingBox())!.height).toBeGreaterThanOrEqual(44);
  await save.click();
  await expect(page.getByText("Automatische Modellwahl gespeichert")).toBeVisible();
  const cb = (await card.boundingBox())!;
  expect(cb.x).toBeGreaterThanOrEqual(inset);
  expect(cb.x + cb.width).toBeLessThanOrEqual(vp.width - inset + 1);
  await noSideways();
  await expectAccessible(page, `chooser settings (${mode})`);

  const blocker = (await (await page.request.post("/api/tasks", { data: { project_id: pid, prompt: "Mobil blockiert MOCK_LONG" } })).json()).id;
  await waitTask(page.request, blocker, "running");
  const id = (await (await page.request.post("/api/tasks", { data: { project_id: pid, prompt: "Dokumentiere die Option --mobil in README.md" } })).json()).id;
  await page.goto("/");
  const drawer = await openTask(page, "Dokumentiere die Option --mobil", "queued");
  const report = drawer.getByTestId("chooser-report");
  await report.scrollIntoViewIfNeeded();
  await expect(report).toContainText("Gewählt:");
  const rb = (await report.boundingBox())!;
  expect(rb.x).toBeGreaterThanOrEqual(inset);
  expect(rb.x + rb.width).toBeLessThanOrEqual(vp.width - inset + 1);
  await report.getByTestId("chooser-alternatives").locator("summary").click();
  const use = report.getByRole("button", { name: /^Stattdessen .* verwenden$/ }).first();
  await use.scrollIntoViewIfNeeded();
  expect((await use.boundingBox())!.height).toBeGreaterThanOrEqual(44);
  await noSideways();
  await expectAccessible(page, `explanation (${mode})`);
  await page.screenshot({ path: test.info().outputPath(`${mode}-chooser.png`) });

  await page.request.post(`/api/tasks/${blocker}/cancel`);
  await waitTask(page.request, id, "done");
  // The open drawer survives a reload (address): close it first.
  await page.keyboard.press("Escape");
  await expect(page.getByRole("dialog")).toBeHidden();
  await page.goto("/");
  const done = await openTask(page, "Dokumentiere die Option --mobil", "done");
  const good = done.getByTestId("chooser-feedback").getByRole("button", { name: "Gut" });
  await good.scrollIntoViewIfNeeded();
  expect((await good.boundingBox())!.height).toBeGreaterThanOrEqual(44);
  await good.click();
  await expect(good).toHaveAttribute("aria-pressed", "true");
  await noSideways();
  await expectAccessible(page, `feedback (${mode})`);
});
