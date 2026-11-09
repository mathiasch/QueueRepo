/** WP03: decisions on held tasks and the protection tab (desktop). Mock provider, runner engine `none`. */
import { expect, test, type Page } from "@playwright/test";
import { apiProject, navLink, openTask, taskRow, uiRegister, waitTask } from "../helpers";

const TOKEN = /gh[p]_[A-Za-z0-9]{36}/;

async function newTask(page: Page, pid: string, prompt: string, extra: Record<string, unknown> = {}) {
  const r = await page.request.post("/api/tasks", { data: { project_id: pid, prompt, ...extra } });
  expect(r.status(), await r.text()).toBe(200);
  return (await r.json()).id as string;
}

test("a possible secret holds the task; the value never shows; approving needs a confirmation", async ({ page }) => {
  await uiRegister(page, "gate");
  const pid = await apiProject(page.request, { name: "GateProj" });
  await page.request.patch(`/api/projects/${pid}`, { data: { review_enabled: false } });
  const tid = await newTask(page, pid, "MOCK_SECRET Kalender-Zugang einbauen");
  await waitTask(page.request, tid, "held");
  await page.reload();
  // Shows up where people look for things that need them.
  await expect(page.getByRole("heading", { name: /Wartet auf dich/ })).toBeVisible();
  const drawer = await openTask(page, "Kalender-Zugang", "held");
  const panel = drawer.getByTestId("gate-panel");
  await expect(drawer.locator(".drawer-head .pill").first()).toContainText("Freigabe nötig");
  await expect(panel).toContainText("Änderungen brauchen deine Freigabe");
  await expect(panel).toContainText("config/credentials.py, Zeile 1");
  await expect(panel).toContainText("GitHub-Token ghp_… (40 Zeichen)");
  await expect(panel).toContainText("Der Wert wird hier nie angezeigt");
  await panel.getByRole("button", { name: "Zurückgehaltene Änderungen ansehen" }).click();
  await expect(panel.locator(".diff")).toContainText('GITHUB_TOKEN = "[geschwärzt: GitHub-Token]"');
  expect(await page.content()).not.toMatch(TOKEN);
  // Approving a secret asks first; cancelling keeps the hold.
  await panel.getByRole("button", { name: "Freigeben" }).click();
  const confirm = page.getByRole("alertdialog");
  await expect(confirm).toContainText("Mögliches Geheimnis freigeben?");
  await confirm.getByRole("button", { name: "Abbrechen" }).click();
  expect((await (await page.request.get(`/api/tasks/${tid}`)).json()).task.status).toBe("held");
  await panel.getByRole("button", { name: "Freigeben" }).click();
  await page.getByRole("alertdialog").getByRole("button", { name: "Trotzdem freigeben" }).click();
  await waitTask(page.request, tid, "done");
  await expect(drawer.getByTestId("gate-panel")).toHaveCount(0);
  expect(await page.content()).not.toMatch(TOKEN);
});

test("discarding held changes; review outage and test tasks", async ({ page }) => {
  await uiRegister(page, "gate2");
  const pid = await apiProject(page.request, { name: "GateTwo" });
  await page.request.patch(`/api/projects/${pid}`, { data: { review_enabled: false } });
  const tid = await newTask(page, pid, "MOCK_SECRET verworfen werden");
  await waitTask(page.request, tid, "held");
  await page.reload();
  const drawer = await openTask(page, "verworfen werden", "held");
  await drawer.getByTestId("gate-panel").getByRole("button", { name: "Änderungen verwerfen" }).click();
  await page.getByRole("alertdialog").getByRole("button", { name: "Verwerfen" }).click();
  const d = await waitTask(page.request, tid, "blocked");
  expect(d.task.failure_class).toBe("gate");
  await expect(drawer.getByTestId("abort-reason")).toContainText("Freigabe abgelehnt");
  await page.keyboard.press("Escape");

  // The review agent fails: retried, then a decision instead of "done".
  await page.request.patch(`/api/projects/${pid}`, { data: { review_enabled: true } });
  const rid = await newTask(page, pid, "MOCK_REVIEW_DOWN Prüfung fällt aus");
  await waitTask(page.request, rid, "held");
  const rd = await openTask(page, "Prüfung fällt aus", "held");
  const panel = rd.getByTestId("gate-panel");
  await expect(panel).toContainText("Prüfung durch den Review-Agenten war nicht möglich");
  await expect(panel.getByRole("button", { name: "Review wiederholen" })).toBeVisible();
  await panel.getByRole("button", { name: "Ohne Prüfung übernehmen" }).click();
  await waitTask(page.request, rid, "done");
  await page.keyboard.press("Escape");

  // Composer: a test task may change tests without approval.
  await page.getByLabel("Auftrag beschreiben").fill("MOCK_SKIP_TEST Test vorübergehend überspringen");
  await page.getByRole("button", { name: "Erweitert" }).click();
  await page.getByTestId("test-task").check();
  await page.getByRole("button", { name: "Einreihen" }).click();
  await expect(taskRow(page, "vorübergehend überspringen").first()).toBeVisible();
  const tasks = (await (await page.request.get("/api/tasks")).json()) as Record<string, any>[];
  const tt = tasks.find((t) => String(t.title).includes("vorübergehend überspringen"))!;
  expect(tt.allow_test_changes).toBe(true);
  await waitTask(page.request, tt.id, "done");
});

test("owner edits protection; others see it read-only", async ({ page, browser }) => {
  await uiRegister(page, "prot");
  const pid = await apiProject(page.request, { name: "ProtProj" });
  await page.reload();
  await navLink(page, "Projekte").click();
  await page.locator(".project-card", { hasText: "ProtProj" }).getByRole("button", { name: "ProtProj" }).click();
  await page.getByRole("tab", { name: "Schutz" }).click();
  const form = page.getByTestId("protection-form");
  await expect(form).toBeVisible();
  await expect(page.getByTestId("trust-repo")).not.toBeChecked();
  await expect(page.getByTestId("test-guard")).toBeChecked();
  await expect(page.getByLabel("Geschützte Dateien und Ordner")).toHaveValue(/tests\//);
  await page.getByLabel("Geschützte Dateien und Ordner").fill("tests/\n../geheim");
  await form.getByRole("button", { name: "Schutz speichern" }).click();
  await expect(page.getByTestId("protection-errors")).toContainText("`..` ist nicht erlaubt");
  await page.getByLabel("Geschützte Dateien und Ordner").fill("tests/\nspec/");
  await page.getByTestId("trust-repo").check();
  await form.getByRole("button", { name: "Schutz speichern" }).click();
  // Trusting repository configuration is explained and confirmed.
  await page.getByRole("alertdialog").getByRole("button", { name: "Erlauben und speichern" }).click();
  await expect(page.getByText("Schutz gespeichert")).toBeVisible();
  const v = await (await page.request.get(`/api/projects/${pid}/protection`)).json();
  expect(v).toMatchObject({ trust_repo_config: true, protected_paths: ["tests/", "spec/"], protected_default: false });
  await page.reload();
  await expect(page.getByTestId("trust-repo")).toBeChecked();
  await expect(page.getByLabel("Geschützte Dateien und Ordner")).toHaveValue("tests/\nspec/");

  // An editor sees everything, but cannot change it.
  const ctx = await browser.newContext();
  const other = await ctx.newPage();
  const email = await uiRegister(other, "protedit");
  expect((await page.request.post(`/api/projects/${pid}/members`, { data: { email, role: "editor" } })).status()).toBe(200);
  await other.goto(`/projects/${pid}?tab=protection`);
  await expect(other.getByText("Nur die Eigentümerin bzw. der Eigentümer des Projekts kann den Schutz ändern.")).toBeVisible();
  await expect(other.getByTestId("trust-repo")).toBeDisabled();
  await expect(other.getByRole("button", { name: "Schutz speichern" })).toHaveCount(0);
  await ctx.close();
});
