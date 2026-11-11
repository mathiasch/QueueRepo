/** WP06 on the iPhone: plan view and commenting in the diff by touch – safe areas, 44pt targets, 16px inputs, no sideways scrolling, axe. */
import { expect, test } from "@playwright/test";
import { apiProject, expectAccessible, openTask, simulateInsets, uiRegister, waitTask } from "../helpers";

test("plan view and inline comments work by touch on the iPhone", async ({ page }) => {
  const landscape = await simulateInsets(page);
  const tag = landscape ? "landscape" : "portrait";
  await uiRegister(page, "mplan");
  const pid = await apiProject(page.request, { name: "MPlan" });
  await page.request.patch(`/api/projects/${pid}`, { data: { review_enabled: false } });
  const add = async (prompt: string, extra: Record<string, unknown> = {}) =>
    (await (await page.request.post("/api/tasks", { data: { project_id: pid, prompt, ...extra } })).json()).id as string;
  const vp = page.viewportSize()!;
  const inside = async (name: string, b: { x: number; y: number; width: number; height: number } | null) => {
    expect(b, name).not.toBeNull();
    expect(b!.height, `${name} touch target`).toBeGreaterThanOrEqual(44);
    expect(b!.x).toBeGreaterThanOrEqual(landscape ? 59 : 0);
    expect(b!.x + b!.width).toBeLessThanOrEqual(vp.width - (landscape ? 59 : 0));
  };

  // Plan view.
  const plan = await add("Kalender-Export als ICS", { plan_mode: true });
  await waitTask(page.request, plan, "held");
  await page.reload();
  const drawer = await openTask(page, "Kalender-Export", "held");
  const panel = drawer.getByTestId("plan-panel");
  await expect(panel.getByTestId("plan-text")).toContainText("MOCKPLAN-STEP-1");
  for (const name of ["Plan genehmigen", "Ändern", "Verwerfen"]) {
    const b = panel.getByRole("button", { name, exact: true });
    await b.scrollIntoViewIfNeeded();
    await inside(name, await b.boundingBox());
  }
  await panel.getByRole("button", { name: "Ändern" }).tap();
  const sizes = await panel.evaluate((root) => [...root.querySelectorAll<HTMLElement>("textarea")].map((e) => parseFloat(getComputedStyle(e).fontSize)));
  expect(Math.min(...sizes), "iOS zooms into inputs < 16px").toBeGreaterThanOrEqual(16);
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(vp.width);
  await expectAccessible(page, `plan panel (${tag})`);
  await page.screenshot({ path: test.info().outputPath(`${tag}-plan.png`) });
  await panel.getByRole("button", { name: "Plan genehmigen", exact: true }).tap();
  await waitTask(page.request, plan, "done");
  await page.keyboard.press("Escape");

  // Inline comment by touch: tap a line, tap "Kommentar schreiben", save.
  const t = await add("MOCK_FILE:m.md Notizen für unterwegs");
  await waitTask(page.request, t, "done");
  const d = await openTask(page, "Notizen für unterwegs", "done");
  await d.getByRole("button", { name: "Änderungen anzeigen" }).tap();
  const diff = d.getByTestId("diff");
  await diff.getByTestId("comment-mode").tap();
  const line = diff.getByRole("button", { name: "Neue Zeile 1 von m.md" });
  await line.scrollIntoViewIfNeeded();
  expect((await line.boundingBox())!.height, "diff line touch target").toBeGreaterThanOrEqual(44);
  await line.tap();
  await expect(line).toHaveAttribute("aria-pressed", "true");
  const bar = diff.getByTestId("comment-bar");
  // The selection bar stays clear of the home indicator and the sides.
  const bb = (await bar.boundingBox())!;
  expect(bb.y + bb.height).toBeLessThanOrEqual(vp.height - (landscape ? 21 : 34) + 1);
  expect(bb.x).toBeGreaterThanOrEqual(landscape ? 59 : 0);
  await inside("Kommentar schreiben", await bar.getByRole("button", { name: "Kommentar schreiben" }).boundingBox());
  await expectAccessible(page, `comment bar (${tag})`);
  await page.screenshot({ path: test.info().outputPath(`${tag}-selection.png`) });
  await bar.getByRole("button", { name: "Kommentar schreiben" }).tap();
  const form = diff.getByTestId("comment-form");
  const input = form.getByLabel("Dein Kommentar");
  await expect(input).toBeFocused();
  expect(parseFloat(await input.evaluate((e) => getComputedStyle(e).fontSize))).toBeGreaterThanOrEqual(16);
  await input.fill("Bitte mit Datum");
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(vp.width);
  await expectAccessible(page, `comment form (${tag})`);
  await page.screenshot({ path: test.info().outputPath(`${tag}-comment.png`) });
  await form.getByRole("button", { name: "Kommentar speichern" }).tap();
  await expect(diff.getByTestId("comment")).toContainText("Bitte mit Datum");
  const send = diff.getByRole("button", { name: "1 Kommentar senden" });
  await send.scrollIntoViewIfNeeded();
  await inside("senden", await send.boundingBox());
  await send.tap();
  // Finished task: the comments start a "Weiter mit …" task.
  await expect(page.getByRole("dialog").locator(".drawer-head")).toContainText("Weiter: MOCK_FILE:m.md Notizen für unterwegs");
});
