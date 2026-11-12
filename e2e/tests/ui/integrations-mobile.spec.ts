/** WP08 on the iPhone: the "Anbindungen" tab, the connect form and the shown-once dialog – safe areas, touch targets, 16-px inputs, no sideways scrolling, axe. */
import { expect, test, type Locator, type Page } from "@playwright/test";
import { randomUUID } from "node:crypto";
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

test("integrations fit on the iPhone", async ({ page }) => {
  const landscape = await simulateInsets(page);
  const mode = landscape ? "landscape" : "portrait";
  await uiRegister(page, "mint");
  const pid = await apiProject(page.request, { name: "Mobil-Anbindung" });
  await page.request.post(`/api/projects/${pid}/integrations`, {
    data: { kind: "linear", name: "Linear", tenant: randomUUID(), secret: "lin-mobile-secret-01" },
  });
  await page.goto(`/projects/${pid}?tab=integrations`);
  const panel = page.getByTestId("project-integrations");
  const row = panel.getByTestId("integration-row");
  await expect(row).toContainText("delegiert");
  await fits(page, row, landscape);
  await touch(row.getByRole("button", { name: "Bearbeiten" }));
  await touch(row.getByTestId("integration-people").locator("summary"));
  await row.getByTestId("integration-people").locator("summary").click();
  const xid = row.getByLabel("Linear-Nutzer-ID");
  await noZoom(xid);
  await touch(xid);
  await fits(page, xid, landscape);
  await touch(row.getByRole("button", { name: "Zuordnen" }));

  await touch(panel.getByTestId("connect-integration"));
  await panel.getByTestId("connect-integration").click();
  const form = panel.getByTestId("connect-form");
  await touch(form.getByTestId("kind-jira"));
  await form.getByTestId("kind-jira").click();
  const tenant = form.getByTestId("tenant");
  await noZoom(tenant);
  await touch(tenant);
  await fits(page, tenant, landscape);
  await tenant.fill(`https://m${randomUUID().slice(0, 6)}.atlassian.net`);
  await expectAccessible(page, `connect form (${mode})`);
  await touch(form.getByTestId("connect-submit"));
  await form.getByTestId("connect-submit").click();
  const dialog = page.getByRole("dialog");
  const secret = dialog.getByTestId("intake-secret");
  await expect(secret).toHaveValue(/^qwh_/);
  await noZoom(secret);
  await fits(page, secret, landscape);
  await expectAccessible(page, `created dialog (${mode})`);
  await page.screenshot({ path: test.info().outputPath(`${mode}-integration.png`) });
  await dialog.getByRole("button", { name: "Fertig" }).click();
  await expect(panel.getByTestId("integration-row")).toHaveCount(2);
  await page.screenshot({ path: test.info().outputPath(`${mode}-integrations.png`), fullPage: true });
});
