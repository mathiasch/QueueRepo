/** WP04 UI: parallelism per project, the merge queue, branch per task and conflicts. */
import { expect, test } from "@playwright/test";
import { apiProject, expectAccessible, openTask, uiRegister, waitTask } from "../helpers";

test("owner sets the parallelism; editors see it read-only", async ({ page, browser }) => {
  const owner = await uiRegister(page, "paro");
  const pid = await apiProject(page.request, { name: "Parallel" });
  await page.goto(`/projects/${pid}?tab=settings`);
  const select = page.getByLabel("Gleichzeitige Aufträge");
  await expect(select).toHaveValue("1");
  await expect(select.locator("option")).toHaveCount(8);
  await select.selectOption("3");
  await page.getByRole("button", { name: "Speichern" }).click();
  await expect(page.getByText("Projekt-Einstellungen gespeichert")).toBeVisible();
  await page.reload();
  await expect(page.getByLabel("Gleichzeitige Aufträge")).toHaveValue("3");
  await page.getByRole("button", { name: "Hilfe: Gleichzeitig arbeiten" }).click();
  await expect(page.getByText("eigenen Branch", { exact: false }).first()).toBeVisible();
  await expectAccessible(page, "project settings with parallelism");

  // An editor can change the rest, not the parallelism.
  const ctx = await browser.newContext();
  const ed = await ctx.newPage();
  const editor = await uiRegister(ed, "pare");
  expect((await page.request.post(`/api/projects/${pid}/members`, { data: { email: editor, role: "editor" } })).status()).toBe(200);
  await ed.goto(`/projects/${pid}?tab=settings`);
  await expect(ed.getByLabel("Gleichzeitige Aufträge")).toBeDisabled();
  await expect(ed.getByText("Nur die Eigentümerin oder der Eigentümer")).toBeVisible();
  await ed.getByRole("textbox", { name: "Projektziel" }).fill("Neues Ziel");
  await ed.getByRole("button", { name: "Speichern" }).click();
  await expect(ed.getByText("Projekt-Einstellungen gespeichert")).toBeVisible();
  expect((await (await page.request.get(`/api/projects/${pid}`)).json()).parallelism).toBe(3);
  await ctx.close();
  expect(owner).toContain("@");
});

test("merge queue, branch block and a conflict are explained in the UI", async ({ page }) => {
  await uiRegister(page, "mq");
  const s = await (await page.request.get("/api/settings")).json();
  await page.request.put("/api/settings", { data: { ...s, parallelism: 4 } });
  const pid = await apiProject(page.request, { name: "Merge-Demo" });
  // Own verify takes a moment; after the first merge the second verifies again.
  await page.request.patch(`/api/projects/${pid}`, { data: { parallelism: 2, review_enabled: false, verify_command: "sleep 6" } });
  const add = async (prompt: string) => (await (await page.request.post("/api/tasks", { data: { project_id: pid, prompt } })).json()).id as string;
  const a = await add("MOCK_FILE:links.md Linke Spalte");
  const b = await add("MOCK_FILE:rechts.md Rechte Spalte");
  await page.goto("/");
  // One of them waits in (or goes through) the merge queue with a re-check.
  const section = page.getByRole("region", { name: /Wird übernommen/ }).or(page.locator("section[aria-labelledby=sec-merging]"));
  await expect(section).toBeVisible({ timeout: 40_000 });
  await expect(page.getByTestId("merging-list").locator("article")).not.toHaveCount(0);
  await expectAccessible(page, "queue with merge section");
  const merging = page.getByTestId("merging-list").locator("article").first();
  await merging.locator("button.open").click();
  const drawer = page.getByRole("dialog");
  const block = drawer.getByTestId("branch-block");
  await expect(block).toContainText("Branch: queue/");
  await expect(block.getByTestId("merge-state")).toHaveText(/Wartet auf Übernahme|Wird übernommen/);
  await expect(drawer.getByRole("button", { name: "Rückgängig machen" })).toHaveCount(0);
  await expectAccessible(page, "drawer of a task in the merge queue");
  await page.keyboard.press("Escape");
  for (const t of [a, b]) await waitTask(page.request, t, "done");

  // Done: merged, revertable.
  await page.goto("/");
  const d = await openTask(page, "Linke Spalte", "done");
  await expect(d.getByTestId("merge-state")).toHaveText("Übernommen");
  await expect(d.getByTestId("branch-block")).toContainText("Übernommen auf Stand");
  await expect(d.getByRole("button", { name: "Rückgängig machen" })).toBeVisible();
  await page.keyboard.press("Escape");

  // Conflict: two tasks change the same file at once.
  await page.request.patch(`/api/projects/${pid}`, { data: { verify_command: "" } });
  const c1 = await add("MOCK_QUIET MOCK_FILE:gemeinsam.md Erste Fassung");
  const c2 = await add("MOCK_QUIET MOCK_FILE:gemeinsam.md Zweite Fassung");
  await expect
    .poll(async () => {
      const st = await Promise.all([c1, c2].map(async (t) => (await (await page.request.get(`/api/tasks/${t}`)).json()).task.status));
      return st.sort().join(",");
    }, { timeout: 40_000 })
    .toMatch(/^done,failed$/);
  const loser = (await (await page.request.get(`/api/tasks/${c1}`)).json()).task.status === "failed" ? c1 : c2;
  await page.goto("/");
  await page.locator(`article[data-task="${loser}"] button.open`).first().click();
  const f = page.getByRole("dialog");
  await expect(f).toBeVisible();
  await expect(f.getByTestId("merge-state")).toHaveText("Konflikt");
  await expect(f.getByTestId("branch-block")).toContainText("Korrektur-Auftrag");
  await expect(f.getByRole("button", { name: "Rückgängig machen" })).toHaveCount(0);
  await expectAccessible(page, "drawer of a conflicting task");
  await page.keyboard.press("Escape");

  // Project → Verlauf: the merge queue card.
  await page.goto(`/projects/${pid}?tab=git`);
  await expect(page.getByTestId("merge-queue")).toBeVisible();
  await expect(page.getByTestId("merge-queue").getByRole("heading", { name: /Übernahme-Warteschlange/ })).toBeVisible();
  await expectAccessible(page, "project git with merge queue");
  for (const t of (await (await page.request.get("/api/tasks")).json()) as { id: string; status: string }[]) {
    if (["queued", "running", "merging"].includes(t.status)) await page.request.post(`/api/tasks/${t.id}/cancel`);
  }
});
