/** WP07 on the iPhone: token dialog and the automation tab – safe areas, touch targets, 16-px inputs, no sideways scrolling, axe. */
import { expect, test, type Locator, type Page } from "@playwright/test";
import { apiProject, expectAccessible, simulateInsets, uiRegister } from "../helpers";

async function fits(page: Page, el: Locator, landscape: boolean) {
  const vp = page.viewportSize()!;
  await el.scrollIntoViewIfNeeded();
  const box = (await el.boundingBox())!;
  expect(box.x).toBeGreaterThanOrEqual(landscape ? 59 : 0);
  expect(box.x + box.width).toBeLessThanOrEqual(vp.width - (landscape ? 59 : 0) + 0.5);
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(vp.width);
}

async function touch(el: Locator) {
  await el.scrollIntoViewIfNeeded();
  expect((await el.boundingBox())!.height, "touch target").toBeGreaterThanOrEqual(44);
}

async function noZoom(el: Locator) {
  expect(parseFloat(await el.evaluate((e) => getComputedStyle(e).fontSize)), "iOS zooms into inputs < 16px").toBeGreaterThanOrEqual(16);
}

test("tokens and automations fit on the iPhone", async ({ page }) => {
  const landscape = await simulateInsets(page);
  const mode = landscape ? "landscape" : "portrait";
  await uiRegister(page, "mauto");
  const pid = await apiProject(page.request, { name: "Mobil-Automatik" });
  const tpl = await (await page.request.post(`/api/projects/${pid}/templates`, { data: { name: "Nacht", title: "Nachtlauf", prompt: "Prüfe alles" } })).json();
  await page.request.post(`/api/projects/${pid}/triggers`, { data: { template_id: tpl.id, kind: "schedule", name: "Morgens", cron: "15 7 * * 1-5", timezone: "Europe/Zurich" } });

  // Token: form and dialog shown once.
  await page.goto("/settings#api-tokens");
  const card = page.getByTestId("api-tokens");
  await touch(card.getByTestId("new-token"));
  await card.getByTestId("new-token").click();
  const name = card.getByLabel("Name", { exact: true });
  await touch(name);
  await noZoom(name);
  await fits(page, name, landscape);
  await name.fill("Telefon");
  await touch(card.getByTestId("scope-tasks:read").locator(".."));
  await expectAccessible(page, `token form (${mode})`);
  await card.getByRole("button", { name: "Token erstellen" }).click();
  const dialog = page.getByRole("dialog");
  const value = dialog.getByTestId("token-value");
  await expect(value).toHaveValue(/^qk_/);
  await noZoom(value);
  await fits(page, value, landscape);
  await touch(dialog.getByRole("button", { name: "API-Token kopieren" }));
  await expectAccessible(page, `token dialog (${mode})`);
  await page.screenshot({ path: test.info().outputPath(`${mode}-token.png`) });
  await dialog.getByRole("button", { name: "Ich habe es kopiert" }).click();

  // Automation tab.
  await page.goto(`/projects/${pid}?tab=automation`);
  const panel = page.getByTestId("project-automation");
  const row = panel.getByTestId("trigger-row").first();
  await expect(row.getByTestId("trigger-summary")).toContainText("Mo–Fr um 07:15");
  await fits(page, row, landscape);
  await touch(row.getByTestId("trigger-fire"));
  await touch(row.getByTestId("trigger-enabled").locator(".."));
  await panel.getByTestId("new-trigger").click();
  const form = panel.getByTestId("trigger-form");
  await touch(form.getByTestId("kind-github"));
  await form.getByTestId("kind-github").click();
  const branches = form.getByLabel("Branches", { exact: true });
  await noZoom(branches);
  await touch(branches);
  await fits(page, branches, landscape);
  await touch(form.getByTestId("event-issues").locator(".."));
  await form.getByTestId("kind-schedule").click();
  const cron = form.getByTestId("cron");
  await noZoom(cron);
  await fits(page, cron, landscape);
  await expectAccessible(page, `automation form (${mode})`);
  await page.screenshot({ path: test.info().outputPath(`${mode}-automation.png`), fullPage: true });
});
