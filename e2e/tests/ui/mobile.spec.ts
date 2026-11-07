/**
 * iPhone layout: Dynamic Island / notch / home indicator.
 * Chromium has no real safe-area insets, so we inject the values an
 * iPhone 15 Pro reports (portrait: top 59 / bottom 34, landscape: left+right 59,
 * bottom 21) through the CSS custom properties the app reads them from.
 */
import { expect, test, type Page } from "@playwright/test";
import { apiProject, navLink, openTask, simulateInsets, uiRegister } from "../helpers";

const box = async (page: Page, sel: string) => (await page.locator(sel).first().boundingBox())!;

/** Every visible button/link/input that can be tapped is at least 44×44 (inline text links excepted). */
async function expectTouchTargets(page: Page, scope: string) {
  const small = await page.locator(scope).evaluate((root) => {
    const out: string[] = [];
    for (const el of root.querySelectorAll<HTMLElement>("button, a.btn, a.tab, a.chip, input:not([type=checkbox]):not([type=radio]), select, textarea, summary")) {
      // Row titles stretch their hit area over the whole row/card (::after overlay).
      const target = el.classList.contains("open") ? (el.closest("article") ?? el) : el;
      const r = target.getBoundingClientRect();
      const st = getComputedStyle(el);
      if (r.width === 0 || r.height === 0 || st.visibility === "hidden" || el.closest(".visually-hidden")) continue;
      // Toggletips are 32px with an invisible 44px hit area (::before inset -6px).
      const help = el.parentElement?.classList.contains("help");
      const min = help ? 32 : 44;
      const chip = el.classList.contains("chip");
      if ((!chip && r.width < min) || r.height < (chip ? 24 : min)) out.push(`${el.tagName}.${el.className} "${(el.textContent ?? el.getAttribute("aria-label") ?? "").trim().slice(0, 30)}" ${Math.round(r.width)}×${Math.round(r.height)}`);
    }
    return out;
  });
  expect(small, `touch targets < 44px in ${scope}`).toEqual([]);
}

test("content never sits under the Dynamic Island, notch or home indicator", async ({ page }, info) => {
  const landscape = await simulateInsets(page);
  await page.goto("/");
  // Login card respects insets.
  const login = await box(page, ".login");
  if (landscape) expect(login.x).toBeGreaterThanOrEqual(59);
  else expect(login.y).toBeGreaterThanOrEqual(59);
  await expectTouchTargets(page, ".login");

  await uiRegister(page, "mobile");
  await apiProject(page.request, { name: "Mobile" });
  await page.reload();

  const viewport = page.viewportSize()!;
  // The skip link stays invisible until focused (it must not peek out below the island).
  const skip = (await page.getByRole("link", { name: "Zum Inhalt springen" }).boundingBox())!;
  expect(skip.width * skip.height).toBeLessThanOrEqual(1);
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
    expect(b.height).toBeGreaterThanOrEqual(44);
    if (landscape) expect(b.y).toBeGreaterThanOrEqual(0);
    else {
      expect(b.y, "tab bar sits at the bottom").toBeGreaterThan(viewport.height / 2);
      expect(b.y + b.height).toBeLessThanOrEqual(viewport.height - 34);
    }
  }
  // The most important action is reachable without scrolling.
  const newTask = page.getByRole("button", { name: "Neuer Auftrag" }).filter({ visible: true }).first();
  await expect(newTask).toBeInViewport();
  const composer = await box(page, ".composer");
  expect(composer.x).toBeGreaterThanOrEqual(landscape ? 59 : 0);
  expect(composer.x + composer.width).toBeLessThanOrEqual(viewport.width - (landscape ? 59 : 0));

  // No horizontal scrolling.
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(viewport.width);

  // iOS zooms into inputs < 16px.
  const fontSize = await page.locator(".composer textarea").evaluate((el) => parseFloat(getComputedStyle(el).fontSize));
  expect(fontSize).toBeGreaterThanOrEqual(16);
  const selectSize = await page.locator(".composer select").evaluate((el) => parseFloat(getComputedStyle(el).fontSize));
  expect(selectSize).toBeGreaterThanOrEqual(16);

  // Drawer header and close button clear the island; body clears the home indicator.
  const box2 = page.getByPlaceholder(/Was soll erledigt werden/);
  await box2.fill("mobile task");
  await page.getByRole("button", { name: "Einreihen" }).click();
  await expectTouchTargets(page, "main");
  await expectTouchTargets(page, ".appbar");
  const drawer = await openTask(page, "mobile task");
  // Measure after the slide-in animation has settled.
  await page.locator(".drawer").evaluate((el) => Promise.all(el.getAnimations().map((a) => a.finished)));
  const close = (await drawer.getByRole("button", { name: "Schließen" }).boundingBox())!;
  if (landscape) expect(close.x + close.width).toBeLessThanOrEqual(viewport.width - 59);
  else expect(close.y).toBeGreaterThanOrEqual(59);
  const pad = await page.locator(".drawer-body").evaluate((el) => parseFloat(getComputedStyle(el).paddingBottom));
  expect(pad).toBeGreaterThanOrEqual(landscape ? 21 : 34);
  if (landscape) {
    // Content box of the drawer body clears both side insets.
    const [left, right] = await page.locator(".drawer-body").evaluate((el) => {
      const r = el.getBoundingClientRect();
      const st = getComputedStyle(el);
      return [r.left + parseFloat(st.paddingLeft), r.right - parseFloat(st.paddingRight)];
    });
    expect(left).toBeGreaterThanOrEqual(59);
    expect(right).toBeLessThanOrEqual(viewport.width - 59);
  }
  await expectTouchTargets(page, ".drawer");
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(viewport.width);

  await page.screenshot({ path: info.outputPath(`${landscape ? "landscape" : "portrait"}-drawer.png`) });
  await drawer.getByRole("button", { name: "Schließen" }).click();
  await expect(drawer).toBeHidden();
  await page.screenshot({ path: info.outputPath(`${landscape ? "landscape" : "portrait"}-queue.png`) });
});

test("toasts stay above the home indicator and the tab bar", async ({ page }) => {
  const landscape = await simulateInsets(page);
  await uiRegister(page, "toast");
  await apiProject(page.request, { name: "T" });
  await page.reload();
  await page.getByPlaceholder(/Was soll erledigt werden/).fill("toast me");
  await page.getByRole("button", { name: "Einreihen" }).click();
  const toast = page.locator(".toast").first();
  await expect(toast).toBeVisible({ timeout: 30_000 });
  // Measure after the slide-in animation has settled.
  await toast.evaluate((el) => Promise.all(el.getAnimations().map((a) => a.finished)));
  const t = (await toast.boundingBox())!;
  const vp = page.viewportSize()!;
  expect(t.y + t.height).toBeLessThanOrEqual(vp.height - (landscape ? 21 : 34));
  expect(t.x).toBeGreaterThanOrEqual(landscape ? 59 : 0);
  expect(t.x + t.width).toBeLessThanOrEqual(vp.width - (landscape ? 59 : 0));
  if (!landscape) {
    const bar = (await page.locator(".tabs").boundingBox())!;
    expect(t.y + t.height, "toast does not cover the tab bar").toBeLessThanOrEqual(bar.y);
  }
  await expectTouchTargets(page, ".toasts");
});

test("project settings with limits: 16px inputs, no horizontal scrolling, clear of safe areas", async ({ page }) => {
  const landscape = await simulateInsets(page);
  await uiRegister(page, "mlimits");
  await apiProject(page.request, { name: "MLimits" });
  await page.reload();
  await navLink(page, "Projekte").click();
  await page.locator(".project-card", { hasText: "MLimits" }).getByRole("button", { name: "MLimits" }).click();
  await page.getByRole("tab", { name: "Einstellungen" }).click();
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
    expect(b.height).toBeGreaterThanOrEqual(44);
  }
  await inputs.first().fill("5");
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(vp.width);
  // Every input of the settings form avoids the iOS zoom.
  const sizes = await page.locator("#project-panel").evaluate((root) =>
    [...root.querySelectorAll<HTMLElement>("input:not([type=checkbox]), select, textarea")].map((e) => parseFloat(getComputedStyle(e).fontSize)),
  );
  expect(Math.min(...sizes)).toBeGreaterThanOrEqual(16);
  await expectTouchTargets(page, "main");
  // The save button is reachable above the tab bar / home indicator.
  const save = page.getByRole("button", { name: "Speichern" });
  await save.scrollIntoViewIfNeeded();
  const s = (await save.boundingBox())!;
  expect(s.y + s.height).toBeLessThanOrEqual(vp.height - (landscape ? 21 : 34));
  if (!landscape) expect(s.y + s.height).toBeLessThanOrEqual((await page.locator(".tabs").boundingBox())!.y);
  await page.screenshot({ path: test.info().outputPath(`${landscape ? "landscape" : "portrait"}-limits.png`), fullPage: true });
});

test("settings page: no horizontal scrolling and 44pt targets", async ({ page }) => {
  await simulateInsets(page);
  await uiRegister(page, "msettings");
  await navLink(page, "Einstellungen").click();
  await expect(page.getByTestId("budget-mock")).toBeVisible();
  const vp = page.viewportSize()!;
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(vp.width);
  await expectTouchTargets(page, "main");
});
