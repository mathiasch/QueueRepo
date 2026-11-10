/** WP05 in the browser: output mode and host access, the pull request in the drawer and in the history. */
import { expect, test } from "@playwright/test";
import { apiProject, bareRemote, expectAccessible, FAKE_HOST, openTask, prProject, PR_TOKEN, taskPull, uiRegister, waitTask } from "../helpers";

test("the owner sets up pull requests in plain words and checks the connection", async ({ page }) => {
  await uiRegister(page, "uiprsetup");
  const bare = bareRemote();
  const pid = await apiProject(page.request, { name: "PR-Einrichtung", git_remote_url: `file://${bare}`, git_branch: "main", git_token: PR_TOKEN });
  const repo = `acme/setup${Date.now().toString(36)}`;
  await page.request.post(`${FAKE_HOST}/_fake/setup`, { data: { kind: "gitea", repo, token: PR_TOKEN, bare } });
  await page.goto(`/projects/${pid}?tab=delivery`);
  const form = page.getByTestId("delivery-form");
  await expect(form.getByRole("radio", { name: /Commit im Hauptstand/ })).toBeChecked();
  await form.getByRole("radio", { name: /Pull Request/ }).check();
  await expect(page.getByTestId("who-pushes")).toContainText("nie selbst");
  await form.getByLabel("Git-Server", { exact: true }).selectOption("gitea");
  await expect(page.getByTestId("token-rights")).toContainText("write:repository");
  await form.getByLabel("API-Adresse", { exact: true }).fill(`${FAKE_HOST}/gitea/api/v1`);
  await form.getByLabel("Repository", { exact: true }).fill(repo);
  await form.getByLabel("Labels", { exact: true }).fill("queue, ki");
  await form.getByLabel("Folge-Runden pro Pull Request", { exact: true }).selectOption("2");
  await form.getByRole("button", { name: "Speichern" }).click();
  await expect(page.getByText("Ergebnis-Einstellungen gespeichert")).toBeVisible();
  await page.reload();
  await expect(page.getByTestId("delivery-form").getByRole("radio", { name: /Pull Request/ })).toBeChecked();
  await expect(page.getByTestId("delivery-form").getByLabel("Labels", { exact: true })).toHaveValue("queue, ki");
  await expect(page.getByTestId("delivery-problems")).toHaveCount(0);
  await page.getByTestId("host-check").click();
  await expect(page.getByTestId("host-check-result")).toContainText("queue-bot");
  await expect(page.getByTestId("host-check-result")).toContainText("darf hochladen");
  await expect(page.locator("body")).not.toContainText(PR_TOKEN);
  await expectAccessible(page, "delivery tab");
});

test("the drawer shows the pull request with checks and lets you allow one more round", async ({ page }) => {
  await uiRegister(page, "uipr");
  const p = await prProject(page.request, "PR-Anzeige", { delivery: { pr_max_followups: 0 } });
  const tid = (await (await page.request.post("/api/tasks", { data: { project_id: p.pid, prompt: "Pull Request zeigen MOCK_FILE:show.txt" } })).json()).id;
  const done = await waitTask(page.request, tid, "done");
  await expect.poll(async () => (await taskPull(page.request, tid))?.state, { timeout: 45_000 }).toBe("open");
  await p.control("checks", { branch: done.task.branch, checks: [{ name: "unit", state: "failure", summary: "x" }, { name: "lint", state: "success" }] });
  await expect.poll(async () => (await taskPull(page.request, tid))?.ci_state, { timeout: 45_000 }).toBe("failure");

  await page.goto("/");
  const drawer = await openTask(page, "Pull Request zeigen", "done");
  const block = drawer.getByTestId("pr-block");
  await block.scrollIntoViewIfNeeded();
  await expect(block.getByTestId("pr-link")).toHaveText("Pull Request #1 öffnen");
  await expect(block.getByTestId("pr-link")).toHaveAttribute("href", /^https:\/\//);
  await expect(block.getByTestId("pr-state")).toHaveText("Entwurf");
  await expect(block.getByTestId("pr-ci")).toHaveText("Checks rot");
  await expect(block.getByTestId("pr-checks")).toContainText("unit");
  await expect(block.getByTestId("pr-rounds")).toContainText("aus");
  await expectAccessible(page, "pull request in the drawer");
  await block.getByTestId("pr-extra-round").click();
  await expect(page.getByText("Eine weitere Runde erlaubt")).toBeVisible();
  await expect.poll(async () => ((await (await page.request.get("/api/tasks")).json()) as any[]).some((t) => t.kind === "pr_fix"), { timeout: 45_000 }).toBe(true);
  await page.keyboard.press("Escape");

  await page.goto(`/projects/${p.pid}?tab=git`);
  const card = page.getByTestId("pull-requests");
  await expect(card).toContainText("Pull Request #1");
  await expect(card).toContainText("Pull Request zeigen");
  await expectAccessible(page, "pull requests in the history");
});

test("editors see the delivery settings read-only", async ({ page, browser }) => {
  await uiRegister(page, "uiprowner");
  const p = await prProject(page.request, "PR-Rollen");
  const ctx = await browser.newContext();
  const editor = await ctx.newPage();
  const email = await uiRegister(editor, "uipreditor");
  expect((await page.request.post(`/api/projects/${p.pid}/members`, { data: { email, role: "editor" } })).status()).toBe(200);
  await editor.goto(`/projects/${p.pid}?tab=delivery`);
  await expect(editor.getByText("Nur die Eigentümerin bzw. der Eigentümer des Projekts kann das ändern.")).toBeVisible();
  await expect(editor.getByTestId("delivery-form").getByRole("radio", { name: /Pull Request/ })).toBeDisabled();
  await expect(editor.getByTestId("host-check")).toHaveCount(0);
  await editor.goto(`/projects/${p.pid}?tab=settings`);
  await expect(editor.getByLabel("Repository-Adresse", { exact: true })).toBeDisabled();
  await expectAccessible(editor, "delivery read-only");
  await ctx.close();
});
