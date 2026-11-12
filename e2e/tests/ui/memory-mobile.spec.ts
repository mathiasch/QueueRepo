/** WP09 iPhone: photo through the camera input (tap), EXIF/GPS removed on the server, touch targets and safe areas; rules and state tabs fit; axe. */
import { expect, test } from "@playwright/test";
import { join } from "node:path";
import { apiProject, expectAccessible, simulateInsets, uiRegister } from "../helpers";

const fixture = (n: string) => join(__dirname, "..", "..", "fixtures", n);

test("photo from the camera input on the iPhone; EXIF removed; rules and state tabs fit", async ({ page }) => {
  const landscape = await simulateInsets(page);
  const side = landscape ? 59 : 0;
  await uiRegister(page, "mcam");
  const pid = await apiProject(page.request, { name: "Kamera" });
  await page.request.patch(`/api/projects/${pid}`, { data: { review_enabled: false } });
  await page.goto("/");
  const vp = page.viewportSize()!;
  const input = page.getByTestId("attach-camera-input");
  await expect(input).toHaveAttribute("capture", "environment");
  await expect(input).toHaveAttribute("accept", "image/*");
  const cam = page.getByRole("button", { name: "Foto aufnehmen" });
  await cam.scrollIntoViewIfNeeded();
  for (const b of [cam, page.getByRole("button", { name: "Datei anhängen" })]) {
    const box = (await b.boundingBox())!;
    expect(box.height, "touch target").toBeGreaterThanOrEqual(44);
    expect(box.x).toBeGreaterThanOrEqual(side);
    expect(box.x + box.width).toBeLessThanOrEqual(vp.width - side);
  }
  const chooser = page.waitForEvent("filechooser");
  await cam.tap();
  const fc = await chooser;
  expect(fc.isMultiple()).toBe(false);
  await fc.setFiles(fixture("iphone-gps.jpg"));
  const chip = page.locator("#composer").getByTestId("attach-item");
  await expect(chip).toHaveCount(1);
  await expect(chip).toContainText("48×64", { timeout: 15_000 });
  await expect(chip.locator("img")).toBeVisible();
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(vp.width);
  const remove = chip.getByRole("button", { name: /entfernen/ });
  const rb = (await remove.boundingBox())!;
  expect(rb.height).toBeGreaterThanOrEqual(44);
  await expectAccessible(page, `composer with photo (${landscape ? "landscape" : "portrait"})`);
  await page.screenshot({ path: test.info().outputPath(`${landscape ? "landscape" : "portrait"}-composer-photo.png`) });

  await page.getByLabel("Auftrag beschreiben").fill("Der Knopf auf dem Foto ist kaputt");
  await page.getByRole("button", { name: "Einreihen" }).tap();
  await expect(chip).toHaveCount(0);
  let att: any;
  await expect.poll(async () => {
    const ts: any[] = await (await page.request.get("/api/tasks")).json();
    const t = ts.find((x) => x.title.startsWith("Der Knopf auf dem Foto"));
    if (!t) return 0;
    const list: any[] = await (await page.request.get(`/api/tasks/${t.id}/attachments`)).json();
    att = list[0];
    return list.length;
  }).toBe(1);
  const body = Buffer.from(await (await page.request.get(`/api/attachments/${att.id}`)).body());
  expect(body.includes("GPS-SECRET-E2E"), "GPS/EXIF removed").toBe(false);
  expect(body.includes("Exif")).toBe(false);

  for (const tab of ["knowledge", "state"]) {
    await page.goto(`/projects/${pid}?tab=${tab}`);
    await expect(page.getByTestId(tab === "knowledge" ? "rules-form" : "project-state")).toBeVisible();
    expect(await page.evaluate(() => document.documentElement.scrollWidth), `${tab}: no horizontal scroll`).toBeLessThanOrEqual(vp.width);
    const sizes = await page.locator("#project-panel").evaluate((root) =>
      [...root.querySelectorAll<HTMLElement>("textarea, input:not([type=checkbox]), select")].map((e) => parseFloat(getComputedStyle(e).fontSize)),
    );
    expect(Math.min(...sizes), "iOS zooms into inputs < 16px").toBeGreaterThanOrEqual(16);
    await expectAccessible(page, `${tab} (${landscape ? "landscape" : "portrait"})`);
  }
  const save = page.getByTestId("state-save");
  await save.scrollIntoViewIfNeeded();
  const s = (await save.boundingBox())!;
  expect(s.height).toBeGreaterThanOrEqual(44);
  expect(s.y + s.height).toBeLessThanOrEqual(vp.height - (landscape ? 21 : 34));
});
