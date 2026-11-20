/** WP11 iPhone (portrait + landscape): security card, authenticator-app setup with QR code, admin console – safe areas, 44-pt targets, 16-px inputs, no horizontal scrolling, axe. */
import { expect, test, type Page } from "@playwright/test";
import { ADMIN_EMAIL, expectAccessible, navLink, PASSWORD, simulateInsets, uiLogin, uiRegister } from "../helpers";

async function fits(page: Page, what: string, side: number) {
  const vp = page.viewportSize()!;
  expect(await page.evaluate(() => document.documentElement.scrollWidth), `${what}: no horizontal scroll`).toBeLessThanOrEqual(vp.width);
  const boxes = await page.evaluate(() =>
    [...document.querySelectorAll<HTMLElement>("main .card, .dialog")].map((e) => {
      const r = e.getBoundingClientRect();
      return { x: r.x, right: r.right, w: r.width };
    }),
  );
  for (const b of boxes.filter((b) => b.w > 0)) {
    expect(b.x, `${what}: card inside the safe area`).toBeGreaterThanOrEqual(side - 1);
    expect(b.right, `${what}: card inside the safe area`).toBeLessThanOrEqual(vp.width - side + 1);
  }
}

async function inputsAre16px(page: Page, root: string, what: string) {
  const sizes = await page.locator(root).evaluate((r) =>
    [...r.querySelectorAll<HTMLElement>("input:not([type=checkbox]):not([type=radio]), select")].map((e) => parseFloat(getComputedStyle(e).fontSize)),
  );
  expect(sizes.length, what).toBeGreaterThan(0);
  expect(Math.min(...sizes), `${what}: iOS zooms into inputs < 16px`).toBeGreaterThanOrEqual(16);
}

async function buttons44(page: Page, root: string, what: string) {
  const hs = await page.locator(root).evaluate((r) =>
    [...r.querySelectorAll<HTMLElement>("button, a.btn, summary")].filter((b) => b.offsetParent !== null).map((b) => [b.textContent?.trim() ?? "", b.getBoundingClientRect().height] as const),
  );
  for (const [t, h] of hs) expect(h, `${what}: "${t}" is a 44-pt target`).toBeGreaterThanOrEqual(44);
}

test("security settings and authenticator setup on the iPhone", async ({ page }) => {
  const landscape = await simulateInsets(page);
  const side = landscape ? 59 : 0;
  await uiRegister(page, "secm");
  await navLink(page, "Einstellungen").tap();
  const card = page.locator("#security");
  await expect(card.getByTestId("security")).toBeVisible();
  await card.scrollIntoViewIfNeeded();
  await fits(page, "settings", side);
  await buttons44(page, "#security", "security card");
  await expectAccessible(page, `security card (${landscape ? "landscape" : "portrait"})`);
  await card.getByTestId("totp-start").tap();
  const dialog = page.getByRole("dialog");
  await inputsAre16px(page, ".dialog", "step-up");
  await dialog.getByLabel("Dein aktuelles Passwort").fill(PASSWORD);
  await dialog.getByRole("button", { name: "Weiter" }).tap();
  const qr = dialog.getByTestId("totp-qr");
  await expect(qr).toBeVisible();
  const q = (await qr.boundingBox())!;
  expect(q.width, "QR code large enough to scan").toBeGreaterThanOrEqual(160);
  await inputsAre16px(page, ".dialog", "code input");
  await buttons44(page, ".dialog", "setup dialog");
  await fits(page, "setup dialog", side);
  await expectAccessible(page, `totp setup (${landscape ? "landscape" : "portrait"})`);
  await page.screenshot({ path: test.info().outputPath(`${landscape ? "landscape" : "portrait"}-totp-setup.png`) });
});

test("admin console on the iPhone", async ({ page }) => {
  const landscape = await simulateInsets(page);
  const side = landscape ? 59 : 0;
  await uiLogin(page, ADMIN_EMAIL);
  await expect(navLink(page, "Aufträge")).toBeVisible();
  await page.goto("/admin");
  await expect(page.getByRole("heading", { level: 1, name: "Verwaltung" })).toBeVisible();
  for (const tab of ["Personen", "Einladungen", "Regeln", "Protokoll", "Runner"]) {
    const t = page.getByRole("tab", { name: tab });
    await t.tap();
    const b = (await t.boundingBox())!;
    expect(b.height, `${tab} tab`).toBeGreaterThanOrEqual(44);
    await expect(page.locator("#admin-panel .card")).toBeVisible();
    await fits(page, `admin ${tab}`, side);
    await buttons44(page, "#admin-panel", `admin ${tab}`);
    if (tab === "Personen" || tab === "Einladungen" || tab === "Protokoll") await inputsAre16px(page, "#admin-panel", `admin ${tab}`);
    await expectAccessible(page, `admin ${tab} (${landscape ? "landscape" : "portrait"})`);
  }
  await page.screenshot({ path: test.info().outputPath(`${landscape ? "landscape" : "portrait"}-admin-runners.png`), fullPage: true });
});
