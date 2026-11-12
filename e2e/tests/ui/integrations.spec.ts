/** WP08 in the browser: connect an integration (address + secret once, steps), map people (two-sided consent), history, the link on the task. */
import { expect, test } from "@playwright/test";
import { createHmac, randomUUID } from "node:crypto";
import { apiProject, expectAccessible, uiRegister, waitTask } from "../helpers";

test("connect GitHub, map an account and see the link on the task", async ({ page }) => {
  test.setTimeout(90_000);
  await uiRegister(page, "uiint");
  const pid = await apiProject(page.request, { name: "Anbindung" });
  await page.request.patch(`/api/projects/${pid}`, { data: { review_enabled: false } });
  await page.goto(`/projects/${pid}?tab=integrations`);
  const panel = page.getByTestId("project-integrations");
  await expect(panel).toContainText("Noch keine Anbindung");
  await expectAccessible(page, "integrations empty");

  await panel.getByTestId("connect-integration").click();
  const form = panel.getByTestId("connect-form");
  await form.getByTestId("kind-slack").click();
  await expect(form.getByLabel("Signing Secret der Slack-App")).toBeVisible();
  await form.getByTestId("kind-github").click();
  await expect(form.getByLabel("Signing Secret der Slack-App")).toHaveCount(0);
  const repo = `acme/ui-${randomUUID().slice(0, 8)}`;
  await form.getByLabel("Repository", { exact: true }).fill(repo);
  await expect(form.getByLabel("Label", { exact: true })).toHaveValue("queue");
  await form.getByText("Erweitert").click();
  await expect(form).toContainText("auf deine Kosten");
  await expectAccessible(page, "connect form");
  await form.getByTestId("connect-submit").click();

  const dialog = page.getByRole("dialog");
  await expect(dialog).toContainText("einrichten");
  const secret = await dialog.getByTestId("intake-secret").inputValue();
  expect(secret).toMatch(/^qwh_/);
  const url = await dialog.getByTestId("intake-url").inputValue();
  expect(url).toMatch(/\/api\/intake\/[0-9a-f-]{36}$/);
  await expect(dialog.getByTestId("intake-steps")).toContainText("Issues: Read and write");
  await expectAccessible(page, "integration created");
  await dialog.getByRole("button", { name: "Fertig" }).click();
  await expect(page.locator("body")).not.toContainText(secret);

  const row = panel.getByTestId("integration-row");
  await expect(row.getByTestId("integration-trigger")).toContainText("Label „queue“");
  await expect(row.getByTestId("integration-who")).toContainText("Noch niemand zugeordnet");
  await expect(row).toContainText("kein Token");

  // Map my own GitHub account (owner ⇒ active at once).
  await row.getByTestId("integration-people").locator("summary").click();
  const people = row.getByTestId("identity-form");
  await people.getByLabel("GitHub-Nutzer-ID (Zahl)").fill("4242");
  await people.getByLabel("Name dort (optional)").fill("octo");
  await people.getByRole("button", { name: "Zuordnen" }).click();
  await expect(row.getByTestId("identity-row")).toHaveAttribute("data-state", "active");
  await expectAccessible(page, "people mapped");

  // A labeled issue arrives (signed like GitHub).
  const body = JSON.stringify({
    action: "labeled", label: { name: "queue" },
    issue: { number: 9, title: "Dunkelmodus", body: "Bitte einen Dunkelmodus", html_url: `https://github.com/${repo}/issues/9`,
             user: { login: "octo", id: 4242 }, labels: [{ name: "queue" }], author_association: "OWNER" },
    repository: { full_name: repo }, sender: { login: "octo", id: 4242, type: "User" },
  });
  const path = new URL(url).pathname;
  const r = await page.request.post(path, {
    headers: { "content-type": "application/json", "x-github-event": "issues", "x-github-delivery": randomUUID(),
               "x-hub-signature-256": `sha256=${createHmac("sha256", secret).update(body).digest("hex")}` },
    data: body,
  });
  expect(r.status(), await r.text()).toBe(201);
  const task = (await r.json()).task_id as string;
  await waitTask(page.request, task, "done");

  await page.reload();
  const row2 = page.getByTestId("integration-row");
  await row2.getByTestId("integration-history").locator("summary").click();
  const ev = row2.getByTestId("intake-event").first();
  await expect(ev).toHaveAttribute("data-outcome", "created");
  await expect(ev).toContainText("issues.labeled");
  await expectAccessible(page, "history");
  await ev.getByRole("button", { name: "Auftrag öffnen" }).click();
  const drawer = page.getByRole("dialog");
  const link = drawer.getByTestId("intake-link");
  await expect(link).toContainText("Aus GitHub-Issue #9");
  await expect(link).toContainText("Dunkelmodus");
  const a = link.getByRole("link", { name: "In GitHub öffnen" });
  await expect(a).toHaveAttribute("href", `https://github.com/${repo}/issues/9`);
  await expect(a).toHaveAttribute("rel", "noopener noreferrer");
  await expectAccessible(page, "task with link");
});

test("a member maps their own account; the owner confirms", async ({ page, browser }) => {
  test.setTimeout(90_000);
  const ownerEmail = await uiRegister(page, "uiintown");
  const pid = await apiProject(page.request, { name: "Team" });
  const team = `T${randomUUID().replace(/-/g, "").slice(0, 10).toUpperCase()}`;
  const c = await page.request.post(`/api/projects/${pid}/integrations`, { data: { kind: "slack", name: "Team-Slack", tenant: team, secret: "slack-ui-secret-0001" } });
  expect(c.status()).toBe(200);
  const ctx = await browser.newContext({ colorScheme: "light" });
  const member = await ctx.newPage();
  const memberEmail = await uiRegister(member, "uiintmem");
  const add = await page.request.post(`/api/projects/${pid}/members`, { data: { email: memberEmail, role: "editor" } });
  expect(add.status(), await add.text()).toBe(200);
  await member.goto(`/projects/${pid}?tab=integrations`);
  const panel = member.getByTestId("project-integrations");
  await expect(panel).toContainText("Verbinden kann nur die Projekt-Eigentümerin");
  await expect(panel.getByTestId("connect-integration")).toHaveCount(0);
  const row = panel.getByTestId("integration-row");
  await row.getByTestId("integration-people").locator("summary").click();
  await row.getByLabel("Slack-Mitglieds-ID (beginnt mit U oder W)").fill("U0MEMBER1");
  await row.getByRole("button", { name: "Zuordnen" }).click();
  await expect(row.getByTestId("identity-row")).toHaveAttribute("data-state", "waiting_owner");
  await expect(row.getByTestId("identity-row")).toContainText("Wartet auf Freigabe");
  await expectAccessible(member, "member waits for the owner");
  await ctx.close();

  await page.goto(`/projects/${pid}?tab=integrations`);
  const orow = page.getByTestId("integration-row");
  await expect(orow.getByTestId("integration-who")).toContainText("1 warten");
  await orow.getByTestId("integration-people").locator("summary").click();
  await orow.getByTestId("identity-row").getByRole("button", { name: "Bestätigen" }).click();
  await expect(orow.getByTestId("identity-row")).toHaveAttribute("data-state", "active");
  expect(ownerEmail).toBeTruthy();
});
