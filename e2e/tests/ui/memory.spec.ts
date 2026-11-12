/** WP09 desktop: attachments in the composer and drawer, project rules with an agent proposal approved in the UI, project state, dedupe reason. */
import { expect, test } from "@playwright/test";
import { join } from "node:path";
import { apiProject, expectAccessible, openTask, uiRegister, waitTask } from "../helpers";

const fixture = (n: string) => join(__dirname, "..", "..", "fixtures", n);
const b64 = (s: string) => Buffer.from(s).toString("base64");

test("composer attachment reaches the task; drawer lists it", async ({ page }) => {
  await uiRegister(page, "att");
  const pid = await apiProject(page.request, { name: "Anhänge" });
  await page.request.patch(`/api/projects/${pid}`, { data: { review_enabled: false } });
  await page.goto("/");
  const chooser = page.waitForEvent("filechooser");
  await page.getByRole("button", { name: "Datei anhängen" }).click();
  await (await chooser).setFiles(fixture("screenshot.png"));
  const chip = page.locator("#composer").getByTestId("attach-item");
  await expect(chip).toHaveCount(1);
  await expect(chip).toContainText("screenshot.png");
  await expect(chip).toContainText("Bild");
  await expect(chip.locator("img")).toBeVisible();
  // A forged file is refused with an understandable message.
  const bad = page.waitForEvent("filechooser");
  await page.getByRole("button", { name: "Datei anhängen" }).click();
  await (await bad).setFiles({ name: "bild.png", mimeType: "image/png", buffer: Buffer.from("<svg onload=alert(1)>") });
  await expect(page.getByText("SVG-, HTML- und XML-Dateien werden aus Sicherheitsgründen nicht angenommen.")).toBeVisible();
  await expect(chip).toHaveCount(1);
  await expectAccessible(page, "composer with attachment");
  await page.getByLabel("Auftrag beschreiben").fill("MOCK_SLOW Fehler auf dem Bildschirmfoto beheben");
  await page.getByRole("button", { name: "Einreihen" }).click();
  await expect(chip).toHaveCount(0);
  const drawer = await openTask(page, "Fehler auf dem Bildschirmfoto");
  const block = drawer.getByTestId("task-attachments");
  await expect(block.getByTestId("attach-item")).toHaveCount(1);
  await expect(block).toContainText(".queue/attachments/");
  await expectAccessible(page, "drawer with attachments");
  await page.keyboard.press("Escape");
});

test("rules: owner saves, an agent proposes, the owner approves in the UI; project state is edited", async ({ page }) => {
  await uiRegister(page, "rules");
  const pid = await apiProject(page.request, { name: "Regeln" });
  await page.request.patch(`/api/projects/${pid}`, { data: { review_enabled: false } });
  await page.goto(`/projects/${pid}?tab=knowledge`);
  await expect(page.getByTestId("rules-status")).toContainText("Noch keine Regeln");
  await page.getByTestId("rules-text").fill("- Vor dem Abschluss immer die Tests laufen lassen");
  await page.getByRole("button", { name: "Speichern und freigeben" }).click();
  await expect(page.getByTestId("rules-status")).toContainText("Version 1");
  // An agent proposes a change (only stored as a proposal).
  const t = await (await page.request.post("/api/tasks", {
    data: { project_id: pid, title: "Regeln lernen", prompt: `MOCK_KNOWLEDGE_B64:${b64("- Vor dem Abschluss immer die Tests laufen lassen\n- Kleine Commits mit klarer Nachricht\n")}` },
  })).json();
  await waitTask(page.request, t.id, "done");
  await page.reload();
  const prop = page.getByTestId("rules-proposal");
  await expect(prop).toHaveCount(1);
  await expect(prop).toContainText("Vorschlag eines Agenten");
  await expect(prop.locator(".d-add")).toContainText("Kleine Commits mit klarer Nachricht");
  await expect(prop).toContainText("1 Zeile(n) neu, 0 Zeile(n) entfernt");
  await page.getByText("Was liest welcher Agent?").click();
  await expect(page.getByTestId("cli-instructions")).toContainText("project_doc_max_bytes=0");
  await expectAccessible(page, "rules with proposal");
  await prop.getByRole("button", { name: "Freigeben" }).click();
  await expect(page.getByTestId("rules-status")).toContainText("Version 2");
  await expect(page.getByTestId("rules-proposal")).toHaveCount(0);
  await expect(page.getByTestId("rules-text")).toHaveValue(/Kleine Commits/);

  await page.getByRole("tab", { name: "Zustand" }).click();
  await expect(page.getByTestId("state-summary")).toContainText("Noch keine Einträge");
  await page.getByTestId("state-new").fill("Login per Passkey");
  await page.getByRole("button", { name: "Hinzufügen" }).click();
  await page.getByLabel("Status von „Login per Passkey“").selectOption("in_progress");
  await page.getByTestId("state-save").click();
  await expect(page.getByText("Projektzustand gespeichert")).toBeVisible();
  await page.reload();
  await expect(page.getByTestId("state-summary")).toContainText("1 In Arbeit");
  await expect(page.getByTestId("state-history")).toContainText("Stand 1");
  await expectAccessible(page, "project state");
});

test("OnEmpty: the dedupe reason is visible on the proposal and the report", async ({ page }) => {
  await uiRegister(page, "dedupe");
  const pid = await apiProject(page.request, { name: "Leerlauf" });
  await page.request.patch(`/api/projects/${pid}`, { data: { review_enabled: false } });
  const first = await (await page.request.post("/api/tasks", {
    data: { project_id: pid, title: "Refactor the config loader", prompt: "Split config loading into smaller functions and add error context." },
  })).json();
  await waitTask(page.request, first.id, "done");
  const result = { status: "done", summary: "Vorschläge", handoff: "", questions: [], follow_up_tasks: [
    { title: "Improve config loader errors", prompt: "Add error context to the config loader and document the config fields." },
  ] };
  await page.request.patch(`/api/projects/${pid}`, { data: { goal: `Ship MOCK_ONEMPTY_B64:${b64(JSON.stringify(result))}` } });
  const s = await (await page.request.get("/api/settings")).json();
  await page.request.put("/api/settings", { data: { ...s, onempty_enabled: true, onempty_actions: ["improve"], onempty_cooldown_minutes: 300 } });
  await expect.poll(async () => {
    const ts: any[] = await (await page.request.get("/api/tasks")).json();
    return ts.find((t) => t.title === "Improve config loader errors")?.status;
  }, { timeout: 30_000 }).toBe("pending_approval");
  await page.request.put("/api/settings", { data: { ...s, onempty_enabled: false } });
  await page.goto("/");
  const drawer = await openTask(page, "Improve config loader errors", "pending_approval");
  const why = drawer.getByTestId("dedupe-reason");
  await expect(why).toContainText("Freigabe nötig");
  await expect(why).toContainText("Refactor the config loader");
  await expect(why).toContainText("bitte prüfen");
  await expectAccessible(page, "drawer with dedupe reason");
  await why.getByRole("button", { name: "Refactor the config loader" }).click();
  await expect(page.getByRole("dialog")).toContainText("Refactor the config loader");
});
