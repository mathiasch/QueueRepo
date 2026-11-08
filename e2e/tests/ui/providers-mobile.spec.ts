/** WP13 on the iPhone: access form and model choice keep 16px inputs, 44pt targets, no sideways scrolling, safe areas. */
import { expect, test, type Page } from "@playwright/test";
import { apiProject, navLink, simulateInsets, uiRegister } from "../helpers";

async function noSideways(page: Page) {
  const vp = page.viewportSize()!;
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(vp.width);
}

async function inputs16(page: Page, scope: string) {
  const sizes = await page.locator(scope).evaluate((root) =>
    [...root.querySelectorAll<HTMLElement>("input:not([type=checkbox]):not([type=radio]), select, textarea")].map((e) => parseFloat(getComputedStyle(e).fontSize)),
  );
  expect(sizes.length).toBeGreaterThan(0);
  for (const s of sizes) expect(s, "iOS zooms into inputs < 16px").toBeGreaterThanOrEqual(16);
}

async function targets44(page: Page, scope: string) {
  const small = await page.locator(scope).evaluate((root) => {
    const out: string[] = [];
    for (const el of root.querySelectorAll<HTMLElement>("button, a.btn, input:not([type=checkbox]):not([type=radio]), select, textarea, summary, .seg-radio")) {
      const r = el.getBoundingClientRect();
      if (r.width === 0 || r.height === 0 || el.closest(".visually-hidden")) continue;
      const min = el.parentElement?.classList.contains("help") ? 32 : 44;
      if (r.width < min || r.height < min) out.push(`${el.tagName}.${el.className} "${(el.textContent ?? "").trim().slice(0, 30)}" ${Math.round(r.width)}×${Math.round(r.height)}`);
    }
    return out;
  });
  expect(small, `touch targets < 44px in ${scope}`).toEqual([]);
}

test("access form and model choice on the iPhone", async ({ page }) => {
  const landscape = await simulateInsets(page);
  await uiRegister(page, "mob-conn");
  const pid = await apiProject(page.request, { name: "Mobil" });
  await page.request.post("/api/connectors", { data: { kind: "mistral", label: "Mistral EU", secret: "k", config: { endpoint: "eu" } } });

  await navLink(page, "Einstellungen").click();
  await page.locator("#connectors").getByRole("button", { name: "Zugang hinzufügen" }).click();
  const form = page.getByTestId("connector-form");
  await expect(form).toBeVisible();
  await inputs16(page, '[data-testid="connector-form"]');
  await targets44(page, '[data-testid="connector-form"]');
  await targets44(page, "#connectors");
  await targets44(page, "#prices");
  await noSideways(page);

  await page.goto(`/projects/${pid}?tab=settings`);
  const routing = page.getByTestId("project-routing");
  await expect(routing.getByTestId("preset-eu")).toBeVisible();
  await noSideways(page);
  await targets44(page, '[data-testid="project-routing"]');
  const b = (await routing.getByTestId("preset-eu").getByRole("button").boundingBox())!;
  const vp = page.viewportSize()!;
  expect(b.x).toBeGreaterThanOrEqual(landscape ? 59 : 0);
  expect(b.x + b.width).toBeLessThanOrEqual(vp.width - (landscape ? 59 : 0));

  // Composer estimate fits on the phone.
  await navLink(page, "Aufträge").click();
  await expect(page.getByTestId("estimate")).toContainText("Geschätzte Kosten");
  await page.getByTestId("estimate").getByRole("button", { name: "Details" }).click();
  await noSideways(page);
});
