/** WP02: environment editor on the iPhone – safe areas, 16px inputs, 44pt targets, axe. */
import { expect, test } from "@playwright/test";
import { apiProject, expectAccessible, navLink, simulateInsets, uiRegister } from "../helpers";

test("environment editor fits the iPhone and stays clear of the safe areas", async ({ page }) => {
  const landscape = await simulateInsets(page);
  await uiRegister(page, "menv");
  await apiProject(page.request, { name: "MEnv" });
  await page.reload();
  await navLink(page, "Projekte").click();
  await page.locator(".project-card", { hasText: "MEnv" }).getByRole("button", { name: "MEnv" }).click();
  await page.getByRole("tab", { name: "Umgebung" }).click();
  await expect(page.getByTestId("env-form")).toBeVisible();
  await page.getByRole("button", { name: "Variable hinzufügen" }).click();
  await page.getByTestId("net-agent").getByRole("radio", { name: "Erlaubte Adressen" }).check();
  const vp = page.viewportSize()!;
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(vp.width);
  const sizes = await page.locator("#project-panel").evaluate((root) =>
    [...root.querySelectorAll<HTMLElement>("input:not([type=checkbox]):not([type=radio]), select, textarea")].map((e) => parseFloat(getComputedStyle(e).fontSize)),
  );
  expect(Math.min(...sizes), "iOS zooms into inputs < 16px").toBeGreaterThanOrEqual(16);
  // Every control inside the safe area.
  const boxes = await page.locator("#project-panel").evaluate((root) =>
    [...root.querySelectorAll<HTMLElement>("input, textarea, button")]
      .map((e) => e.getBoundingClientRect())
      .filter((r) => r.width > 0)
      .map((r) => ({ l: r.left, r: r.right, h: r.height })),
  );
  for (const b of boxes) {
    expect(b.l).toBeGreaterThanOrEqual(landscape ? 59 : 0);
    expect(b.r).toBeLessThanOrEqual(vp.width - (landscape ? 59 : 0));
  }
  const save = page.getByRole("button", { name: "Umgebung speichern" });
  await save.scrollIntoViewIfNeeded();
  const s = (await save.boundingBox())!;
  expect(s.height).toBeGreaterThanOrEqual(44);
  expect(s.y + s.height).toBeLessThanOrEqual(vp.height - (landscape ? 21 : 34));
  if (!landscape) expect(s.y + s.height).toBeLessThanOrEqual((await page.locator(".tabs").boundingBox())!.y);
  await expectAccessible(page, `environment (${landscape ? "landscape" : "portrait"})`);
  await page.screenshot({ path: test.info().outputPath(`${landscape ? "landscape" : "portrait"}-environment.png`), fullPage: true });
});
