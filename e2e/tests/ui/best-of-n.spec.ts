/** WP12 on the desktop: Best-of-N from the composer, comparison view and
 *  selection; routing explanation; preview in a sandboxed frame on an own
 *  origin and screenshots after UI changes – axe light/dark. */
import { expect, test, type APIRequestContext } from "@playwright/test";
import { apiProject, expectAccessible, openTask, uiRegister, waitTask } from "../helpers";

async function candidates(request: APIRequestContext, group: string, n: number) {
  await expect
    .poll(async () => {
      const c = await (await request.get(`/api/tasks/${group}/attempts`)).json();
      return (c.attempts ?? []).filter((a: { status: string }) => a.status === "candidate").length;
    }, { timeout: 60_000 })
    .toBe(n);
}

test("best-of-n: composer, comparison and selection", async ({ page }) => {
  await uiRegister(page, "bon");
  const s = await (await page.request.get("/api/settings")).json();
  await page.request.put("/api/settings", { data: { ...s, parallelism: 2 } });
  const pid = await apiProject(page.request, { name: "BonDesk" });
  await page.request.patch(`/api/projects/${pid}`, { data: { review_enabled: false } });
  await page.goto("/");
  await page.getByRole("textbox", { name: "Auftrag beschreiben" }).fill("Vergleich im Browser MOCK_FILE:cmp.md");
  await page.getByRole("button", { name: "Erweitert" }).click();
  const bon = page.getByTestId("best-of-n");
  await expect(bon.getByTestId("bon-count")).toHaveText("1 Versuch");
  await bon.getByRole("button", { name: "Mehr Versuche" }).click();
  await expect(bon.getByTestId("bon-count")).toHaveText("2 Versuche");
  await expect(bon.getByLabel("Versuch 2 – Zugang")).toBeVisible();
  await expect(page.getByTestId("estimate")).toContainText("2 Versuche");
  await expectAccessible(page, "composer with attempts");
  await page.getByRole("button", { name: "Einreihen" }).click();
  await expect(page.getByTestId("comparing-list")).toContainText("Vergleich im Browser");
  await expect(page.getByTestId("bon-tag")).toHaveText("2 Versuche");
  const group = (await (await page.request.get("/api/tasks")).json()).find((t: { title: string; attempts: number }) => t.title.startsWith("Vergleich im Browser") && t.attempts === 2).id;
  await candidates(page.request, group, 2);

  const drawer = await openTask(page, "Vergleich im Browser", "comparing");
  const cmp = drawer.getByTestId("comparison");
  await expect(cmp.getByTestId("comparison-status")).toContainText("Bitte einen übernehmen");
  await expect(cmp.getByTestId("attempt-1")).toContainText("Erfolgreich – wählbar");
  await expect(cmp.getByTestId("attempt-2")).toContainText("1 Datei");
  await expect(drawer.getByTestId("routing-decision")).toContainText("Standard");
  await cmp.getByText("Als Tabelle").click();
  await expect(cmp.getByRole("table", { name: "Vergleich der Versuche" })).toBeVisible();
  await expectAccessible(page, "comparison (dark)");
  await page.emulateMedia({ colorScheme: "light" });
  await expectAccessible(page, "comparison (light)");
  await cmp.getByTestId("attempt-2").getByRole("button", { name: "Diesen übernehmen" }).click();
  const dlg = page.getByRole("alertdialog").or(page.getByRole("dialog", { name: /Versuch 2 übernehmen/ }));
  await dlg.getByRole("button", { name: "Übernehmen", exact: true }).click();
  await expect(cmp.getByTestId("comparison-status")).toContainText("Versuch 2 wurde von dir übernommen", { timeout: 30_000 });
  await expect(cmp.getByTestId("attempt-1")).toContainText("Verworfen");
  const won = (await (await page.request.get(`/api/tasks/${group}/attempts`)).json()).attempts.find((a: { attempt_no: number }) => a.attempt_no === 2).id;
  await waitTask(page.request, won, "done");
  await page.screenshot({ path: test.info().outputPath("comparison.png") });
});

test("preview in a sandboxed frame and screenshots", async ({ browser }) => {
  // The app's own origin (PUBLIC_URL) may frame previews (frame-ancestors).
  const ctx = await browser.newContext({ baseURL: `http://localhost:${process.env.E2E_APP_PORT ?? 18080}`, colorScheme: "dark" });
  const page = await ctx.newPage();
  await uiRegister(page, "pv");
  const s = await (await page.request.get("/api/settings")).json();
  await page.request.put("/api/settings", { data: { ...s, parallelism: 2 } });
  const pid = await apiProject(page.request, { name: "Preview" });
  await page.request.patch(`/api/projects/${pid}`, { data: { review_enabled: false } });

  await page.goto(`/projects/${pid}?tab=settings`);
  const card = page.getByTestId("project-preview");
  await card.getByTestId("pv-enabled").check();
  await card.getByLabel("Startkommando").fill('exec python3 -m http.server "$PORT" --bind 127.0.0.1');
  await card.getByLabel("Port", { exact: true }).fill("4555");
  await card.getByLabel("Oberflächen-Dateien (Muster, Komma)").fill("**/*.html");
  await card.getByTestId("pv-save").click();
  await expect(page.getByText("Vorschau-Einstellungen gespeichert")).toBeVisible();
  await expectAccessible(page, "preview settings");

  const g = (await (await page.request.post("/api/tasks", { data: { project_id: pid, prompt: "Seite MOCK_FILE:index.html", attempts: 2 } })).json()).id;
  await candidates(page.request, g, 2);
  const att = (await (await page.request.get(`/api/tasks/${g}/attempts`)).json()).attempts[0];
  expect(att.screenshots.length, "desktop + mobile after a UI change").toBe(2);

  await page.goto("/");
  const drawer = await openTask(page, "Seite MOCK_FILE:index.html", "comparing");
  await expect(drawer.getByTestId("attempt-1").getByTestId("screenshots").locator("img")).toHaveCount(2);
  await drawer.getByTestId("attempt-1").getByRole("button", { name: "Details & Änderungen" }).click();
  const shots = page.getByRole("dialog").getByTestId("screenshots").first();
  await expect(shots.locator("img")).toHaveCount(2);
  await expect.poll(() => shots.locator("img").first().evaluate((i: HTMLImageElement) => i.naturalWidth)).toBeGreaterThan(0);

  const pv = page.getByRole("dialog").getByTestId("preview");
  await pv.getByRole("button", { name: "Vorschau starten" }).click();
  const frame = pv.locator("iframe.preview-frame");
  await expect(frame).toBeVisible({ timeout: 30_000 });
  expect(await frame.getAttribute("sandbox")).toBe("allow-scripts allow-forms");
  const src = (await frame.getAttribute("src"))!;
  expect(new URL(src).hostname).toMatch(/^[a-z2-7]{26}\.localhost$/);
  const inner = page.frameLocator("iframe.preview-frame");
  await expect(inner.locator("body")).toContainText(/run \d{4}-/);
  // Opaque origin: no cookies, no access to the app.
  const f = page.frames().find((fr) => fr.url().startsWith(src))!;
  expect(await f.evaluate(() => self.origin)).toBe("null");
  expect(await f.evaluate(() => { try { return document.cookie; } catch { return "blocked"; } })).toBe("blocked");
  await expectAccessible(page, "preview in the drawer");
  await pv.getByRole("button", { name: "Vorschau beenden" }).click();
  await expect(frame).toHaveCount(0);
  // The link is dead at once (the browser resolves *.localhost itself).
  expect((await page.goto(src))!.status()).toBe(404);
  await ctx.close();
});
