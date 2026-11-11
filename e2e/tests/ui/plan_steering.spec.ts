/** WP06 (desktop): plan view, messages to the agent, "Weiter mit …", inline comments in the diff. Mock provider, runner engine `none`. */
import { expect, test, type Page } from "@playwright/test";
import { apiProject, expectAccessible, openTask, uiRegister, waitTask } from "../helpers";

async function newTask(page: Page, pid: string, prompt: string, extra: Record<string, unknown> = {}) {
  const r = await page.request.post("/api/tasks", { data: { project_id: pid, prompt, ...extra } });
  expect(r.status(), await r.text()).toBe(200);
  return (await r.json()).id as string;
}

async function seen(page: Page, pid: string) {
  const files = (await (await page.request.get(`/api/projects/${pid}/files`)).json()) as string[];
  let all = "";
  for (const f of files.filter((f) => f.startsWith("SEEN"))) {
    all += (await (await page.request.get(`/api/projects/${pid}/file?path=${encodeURIComponent(f)}`)).json()).content;
  }
  return all;
}

test("plan view: the plan waits; edit and approve, ask for a new one, discard", async ({ page }) => {
  await uiRegister(page, "plan");
  const pid = await apiProject(page.request, { name: "PlanProj" });
  await page.request.patch(`/api/projects/${pid}`, { data: { review_enabled: false } });
  // The owner switches plan mode on in the project settings.
  await page.goto(`/projects/${pid}?tab=settings`);
  const settings = page.getByTestId("plan-settings");
  await settings.getByRole("switch", { name: /Plan-Modus für neue Aufträge/ }).check();
  await expect(settings.getByLabel("Automatisch genehmigen", { exact: true })).toHaveValue("");
  await page.getByRole("button", { name: "Speichern" }).first().click();
  await expect(page.getByText("Projekt-Einstellungen gespeichert")).toBeVisible();
  expect((await (await page.request.get(`/api/projects/${pid}`)).json()).plan_mode).toBe(true);

  // Composer: the option follows the project default.
  await page.goto("/");
  await page.getByLabel("Auftrag beschreiben").fill("Kontaktformular mit Spam-Schutz");
  await page.getByRole("button", { name: "Erweitert" }).click();
  await expect(page.getByTestId("plan-first")).toBeChecked();
  await page.getByRole("button", { name: "Einreihen" }).click();
  const tasks = (await (await page.request.get("/api/tasks")).json()) as Record<string, any>[];
  const tid = tasks.find((t) => String(t.title).includes("Spam-Schutz"))!.id as string;
  expect(tasks.find((t) => t.id === tid)!.plan_mode).toBe(true);
  await waitTask(page.request, tid, "held");
  await page.reload();
  const drawer = await openTask(page, "Spam-Schutz", "held");
  const panel = drawer.getByTestId("plan-panel");
  await expect(panel).toContainText("Der Plan wartet auf deine Freigabe");
  await expect(panel.getByTestId("plan-text")).toContainText("MOCKPLAN-STEP-1");
  await expectAccessible(page, "plan panel");

  // Ask for a new plan first.
  await panel.getByRole("button", { name: "Ändern" }).click();
  await panel.getByRole("button", { name: "Neuen Plan anfordern" }).first().click();
  await panel.getByLabel("Was soll anders werden?").fill("Bitte zuerst Tests schreiben");
  await panel.getByRole("button", { name: "Neuen Plan anfordern" }).last().click();
  await expect.poll(async () => (await (await page.request.get(`/api/tasks/${tid}`)).json()).task.plan_revision).toBe(1);
  await waitTask(page.request, tid, "held");
  await expect(drawer.getByTestId("plan-text")).toContainText("überarbeitet");
  await expect(drawer.getByTestId("plan-panel")).toContainText("Überarbeitung 1");

  // Edit it and approve the edited version.
  await drawer.getByTestId("plan-panel").getByRole("button", { name: "Ändern" }).click();
  const edit = drawer.getByTestId("plan-panel").getByLabel("Plan", { exact: true });
  await edit.fill("1. MOCKPLAN-UI-EDITED nur das Formular");
  await expectAccessible(page, "plan editor");
  await drawer.getByRole("button", { name: "Geänderten Plan genehmigen" }).click();
  await waitTask(page.request, tid, "done");
  await expect(drawer.getByText("Genehmigter Plan")).toBeVisible();
  expect(await seen(page, pid)).toContain("MOCKPLAN-UI-EDITED");
  await page.keyboard.press("Escape");

  // Discard: confirmation, nothing changes.
  const t2 = await newTask(page, pid, "MOCK_FILE:never.md Unnötiges Feature");
  await waitTask(page.request, t2, "held");
  const d2 = await openTask(page, "Unnötiges Feature", "held");
  await d2.getByTestId("plan-panel").getByRole("button", { name: "Verwerfen" }).click();
  await expect(page.getByRole("alertdialog")).toContainText("Plan verwerfen?");
  await page.getByRole("alertdialog").getByRole("button", { name: "Plan verwerfen" }).click();
  const c = await waitTask(page.request, t2, "cancelled");
  expect(c.task.plan_state).toBe("rejected");
  expect((await (await page.request.get(`/api/projects/${pid}/files`)).json())).not.toContain("never.md");
});

test("messages: withdraw before delivery, deliver at once, then Weiter mit", async ({ page }) => {
  await uiRegister(page, "steer");
  const pid = await apiProject(page.request, { name: "SteerProj" });
  await page.request.patch(`/api/projects/${pid}`, { data: { review_enabled: false } });
  const tid = await newTask(page, pid, "MOCK_SLOW Galerie optimieren");
  await expect
    .poll(async () => JSON.stringify((await (await page.request.get(`/api/tasks/${tid}`)).json()).events), { timeout: 30_000 })
    .toContain("working slowly");
  await page.reload();
  const drawer = await openTask(page, "Galerie optimieren", "running");
  const box = drawer.getByTestId("messages");
  await box.getByLabel("Deine Nachricht").fill("Bitte nur WebP verwenden");
  await box.getByRole("button", { name: "Senden" }).click();
  const first = box.locator(".message").first();
  await expect(first).toContainText("Wartet auf Zustellung");
  await expectAccessible(page, "messages");
  await first.getByRole("button", { name: "Zurückziehen" }).click();
  await expect(first).toContainText("Zurückgezogen");
  await box.getByLabel("Deine Nachricht").fill("MOCKMSG-UI-NOW Bitte AVIF statt WebP");
  await box.getByRole("button", { name: "Sofort zustellen" }).click();
  await waitTask(page.request, tid, "done");
  await expect(drawer.getByTestId("messages").locator(".message").nth(1)).toContainText("Zugestellt in die laufende Sitzung");
  expect(await seen(page, pid)).toContain("run resume MOCKMSG-UI-NOW");

  // Finished: continue with the same session.
  const cont = drawer.getByTestId("messages");
  await expect(cont).toContainText("Weiter mit diesem Auftrag");
  await cont.getByLabel("Wie soll es weitergehen?").fill("MOCKMSG-UI-NEXT Jetzt noch Lazy Loading");
  await cont.getByRole("button", { name: "Weiter mit diesem Auftrag" }).click();
  await expect(page.getByRole("dialog").locator(".drawer-head")).toContainText("Weiter: MOCK_SLOW Galerie optimieren");
  await expect.poll(async () => (await seen(page, pid)).includes("run resume MOCKMSG-UI-NEXT"), { timeout: 30_000 }).toBe(true);
});

test("inline comments: select lines, collect, send once; stale after the diff changed", async ({ page }) => {
  await uiRegister(page, "comments");
  const pid = await apiProject(page.request, { name: "CommentProj" });
  await page.request.patch(`/api/projects/${pid}`, { data: { review_enabled: false } });
  const tid = await newTask(page, pid, "MOCK_ASK MOCK_REWRITE MOCK_FILE:notes.md Notizen anlegen");
  await waitTask(page.request, tid, "waiting_input");
  await page.reload();
  const drawer = await openTask(page, "Notizen anlegen", "waiting_input");
  await drawer.getByRole("button", { name: "Änderungen anzeigen" }).click();
  const diff = drawer.getByTestId("diff");
  await diff.getByTestId("comment-mode").click();
  await expect(diff.getByTestId("comment-mode")).toHaveAttribute("aria-pressed", "true");
  await diff.getByRole("button", { name: "Neue Zeile 1 von notes.md" }).click();
  const bar = diff.getByTestId("comment-bar");
  await expect(bar).toContainText("Zeile 1 in notes.md");
  await bar.getByRole("button", { name: "Kommentar schreiben" }).click();
  const form = diff.getByTestId("comment-form");
  await expect(form.getByLabel("Dein Kommentar")).toBeFocused();
  await form.getByLabel("Dein Kommentar").fill("MOCKCOMMENT-UI Überschrift fehlt");
  await expectAccessible(page, "comment form");
  await form.getByRole("button", { name: "Kommentar speichern" }).click();
  const card = diff.getByTestId("comment").first();
  await expect(card).toContainText("Entwurf");
  await expect(card).toContainText("aktuell");
  await expect(diff.getByTestId("comment-send")).toContainText("1 Kommentar gesammelt");
  // Sent as one message: here it answers the open question, the task goes on.
  await diff.getByRole("button", { name: "1 Kommentar senden" }).click();
  await waitTask(page.request, tid, "done");
  expect(await seen(page, pid)).toContain("MOCKCOMMENT-UI");
  // The agent rewrote the commented line: the comment is stale now.
  await page.keyboard.press("Escape");
  const d = await openTask(page, "Notizen anlegen", "done");
  await d.getByRole("button", { name: "Änderungen anzeigen" }).click();
  const stale = d.getByTestId("stale-comments");
  await expect(stale).toContainText("veraltet");
  await expect(stale).toContainText("gesendet");
  await expectAccessible(page, "stale comments");
});
