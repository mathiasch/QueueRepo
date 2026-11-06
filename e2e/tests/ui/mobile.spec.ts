/**
 * iPhone layout: Dynamic Island / notch / home indicator.
 * Chromium has no real safe-area insets, so we inject the values an
 * iPhone 15 Pro reports (portrait: top 59 / bottom 34, landscape: left+right 59,
 * bottom 21) through the CSS custom properties the app reads them from.
 */
import { expect, test, type Page } from "@playwright/test";
import { apiProject, uiRegister } from "../helpers";

async function simulateInsets(page: Page) {
  const landscape = (page.viewportSize()?.width ?? 0) > (page.viewportSize()?.height ?? 0);
  const css = landscape
    ? ":root{--sat:0px !important;--sar:59px !important;--sab:21px !important;--sal:59px !important}"
    : ":root{--sat:59px !important;--sar:0px !important;--sab:34px !important;--sal:0px !important}";
  await page.addInitScript((c) => {
    const add = () => {
      const s = document.createElement("style");
      s.textContent = c;
      document.head.appendChild(s);
    };
    if (document.head) add();
    else document.addEventListener("DOMContentLoaded", add);
  }, css);
  return landscape;
}

const box = async (page: Page, sel: string) => (await page.locator(sel).first().boundingBox())!;

test("content never sits under the Dynamic Island, notch or home indicator", async ({ page }, info) => {
  const landscape = await simulateInsets(page);
  await page.goto("/");
  // Login card respects insets.
  const login = await box(page, ".login");
  if (landscape) expect(login.x).toBeGreaterThanOrEqual(59);
  else expect(login.y).toBeGreaterThanOrEqual(59);

  await uiRegister(page, "mobile");
  await apiProject(page.request, { name: "Mobile" });
  await page.reload();

  const viewport = page.viewportSize()!;
  const brand = await box(page, ".brand");
  if (landscape) expect(brand.x).toBeGreaterThanOrEqual(59);
  else expect(brand.y).toBeGreaterThanOrEqual(59);
  // All tabs fully visible and tappable (≥ 44pt), clear of island / home indicator.
  const tabs = page.locator(".tabs .tab");
  await expect(tabs).toHaveCount(4);
  for (let i = 0; i < 4; i++) {
    const b = (await tabs.nth(i).boundingBox())!;
    expect(b.x).toBeGreaterThanOrEqual(landscape ? 59 : 0);
    expect(b.x + b.width).toBeLessThanOrEqual(viewport.width - (landscape ? 59 : 0));
    if (landscape) expect(b.y).toBeGreaterThanOrEqual(0);
    else {
      expect(b.y, "tab bar sits at the bottom").toBeGreaterThan(viewport.height / 2);
      expect(b.height).toBeGreaterThanOrEqual(44);
      expect(b.y + b.height).toBeLessThanOrEqual(viewport.height - 34);
    }
  }
  const composer = await box(page, ".composer");
  expect(composer.x + composer.width).toBeLessThanOrEqual(viewport.width - (landscape ? 59 : 0));

  // No horizontal scrolling.
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(viewport.width);

  // iOS zooms into inputs < 16px.
  const fontSize = await page.locator(".composer textarea").evaluate((el) => parseFloat(getComputedStyle(el).fontSize));
  expect(fontSize).toBeGreaterThanOrEqual(16);

  // Drawer header and close button clear the island; body clears the home indicator.
  const box2 = page.getByPlaceholder(/Was soll erledigt werden/);
  await box2.fill("mobile task");
  await page.getByRole("button", { name: "Einreihen" }).click();
  await page.locator(".item", { hasText: "mobile task" }).click();
  // Measure after the slide-in animation has settled.
  await page.locator(".drawer").evaluate((el) => Promise.all(el.getAnimations().map((a) => a.finished)));
  const close = await box(page, ".drawer-head button.icon");
  if (landscape) expect(close.x + close.width).toBeLessThanOrEqual(viewport.width - 59);
  else expect(close.y).toBeGreaterThanOrEqual(59);
  const pad = await page.locator(".drawer-body").evaluate((el) => parseFloat(getComputedStyle(el).paddingBottom));
  expect(pad).toBeGreaterThanOrEqual(landscape ? 21 : 34);

  await page.screenshot({ path: info.outputPath(`${landscape ? "landscape" : "portrait"}-drawer.png`) });
  await page.keyboard.press("Escape");
  await page.screenshot({ path: info.outputPath(`${landscape ? "landscape" : "portrait"}-queue.png`) });
});

test("toasts stay above the home indicator", async ({ page }) => {
  const landscape = await simulateInsets(page);
  await uiRegister(page, "toast");
  await apiProject(page.request, { name: "T" });
  await page.reload();
  await page.getByPlaceholder(/Was soll erledigt werden/).fill("toast me");
  await page.getByRole("button", { name: "Einreihen" }).click();
  const toast = page.locator(".toast").first();
  await expect(toast).toBeVisible({ timeout: 30_000 });
  const t = (await toast.boundingBox())!;
  const vp = page.viewportSize()!;
  expect(t.y + t.height).toBeLessThanOrEqual(vp.height - (landscape ? 21 : 34));
});

test("project settings with limits: 16px inputs, no horizontal scrolling, clear of safe areas", async ({ page }) => {
  const landscape = await simulateInsets(page);
  await uiRegister(page, "mlimits");
  await apiProject(page.request, { name: "MLimits" });
  await page.reload();
  await page.getByRole("button", { name: "Projekte" }).click();
  await page.locator(".project-card", { hasText: "MLimits" }).click();
  await page.locator(".seg").getByRole("button", { name: "Einstellungen" }).click();
  const limits = page.getByTestId("project-limits");
  await limits.scrollIntoViewIfNeeded();
  const inputs = limits.locator("input");
  await expect(inputs).toHaveCount(5);
  const vp = page.viewportSize()!;
  for (let i = 0; i < 5; i++) {
    const el = inputs.nth(i);
    await el.scrollIntoViewIfNeeded();
    expect(await el.evaluate((e) => parseFloat(getComputedStyle(e).fontSize)), "iOS zooms into inputs < 16px").toBeGreaterThanOrEqual(16);
    const b = (await el.boundingBox())!;
    expect(b.x).toBeGreaterThanOrEqual(landscape ? 59 : 0);
    expect(b.x + b.width).toBeLessThanOrEqual(vp.width - (landscape ? 59 : 0));
  }
  await inputs.first().fill("5");
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(vp.width);
  await page.screenshot({ path: test.info().outputPath(`${landscape ? "landscape" : "portrait"}-limits.png`), fullPage: true });
});
