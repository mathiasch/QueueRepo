/** WP02: environment editor in the project (desktop). Mock provider, runner engine `none`. */
import { expect, test } from "@playwright/test";
import { apiProject, navLink, openLog, openTask, taskRow, uiRegister, waitTask } from "../helpers";

const SECRET = "e2e-secret-value-4711";

async function openEnvironment(page: import("@playwright/test").Page, name: string) {
  await navLink(page, "Projekte").click();
  await page.locator(".project-card", { hasText: name }).getByRole("button", { name }).click();
  await page.getByRole("tab", { name: "Umgebung" }).click();
  await expect(page.getByTestId("env-form")).toBeVisible();
}

test("owner configures setup, variables, network and a secret; the secret value never shows", async ({ page }) => {
  await uiRegister(page, "envowner");
  await apiProject(page.request, { name: "EnvProj" });
  await page.reload();
  await openEnvironment(page, "EnvProj");

  // The dev engine is clearly marked as not protected.
  await expect(page.getByTestId("sandbox-status")).toContainText("Entwicklungsmodus: Netzwerkregeln werden nicht durchgesetzt");
  // Defaults: agent only reaches its model API, setup the package registries.
  const agent = page.getByTestId("net-agent");
  await expect(agent.getByRole("radio", { name: "Nur Modell-API" })).toBeChecked();
  await expect(page.getByTestId("net-setup").getByRole("radio", { name: "Erlaubte Adressen" })).toBeChecked();

  await page.getByLabel("Setup-Skript").fill("echo installing\necho done > .installed");
  await page.getByRole("button", { name: "Variable hinzufügen" }).click();
  await page.getByLabel("Name", { exact: true }).first().fill("NODE_ENV");
  await page.getByLabel("Wert von NODE_ENV").fill("ci");
  await agent.getByRole("radio", { name: "Erlaubte Adressen" }).check();
  await agent.getByLabel(/Python \(PyPI\)/).check();
  await agent.getByLabel("Weitere Adressen").fill("Docs.RS\nhttps://bad.example/path");
  await page.getByRole("button", { name: "Umgebung speichern" }).click();
  // Invalid entry: explained, nothing saved.
  await expect(page.getByTestId("env-errors")).toContainText("ohne https:// und Pfad");
  await agent.getByLabel("Weitere Adressen").fill("Docs.RS");
  await page.getByRole("button", { name: "Umgebung speichern" }).click();
  await expect(page.getByText("Umgebung gespeichert")).toBeVisible();

  // Secret: saved, listed by name only.
  const secrets = page.getByTestId("env-secrets");
  await secrets.getByLabel("Name").fill("NPM_TOKEN");
  await secrets.getByLabel("Wert").fill("short");
  await secrets.getByRole("button", { name: "Geheimnis speichern" }).click();
  await expect(secrets).toContainText("mindestens 8 Zeichen");
  await secrets.getByLabel("Wert").fill(SECRET);
  await secrets.getByRole("button", { name: "Geheimnis speichern" }).click();
  await expect(page.getByTestId("secret-list")).toContainText("NPM_TOKEN");
  await expect(secrets.getByLabel("Wert")).toHaveValue("");

  // After a reload everything is still there – and the value is nowhere.
  await page.reload();
  await page.getByRole("tab", { name: "Umgebung" }).click();
  await expect(page.getByLabel("Setup-Skript")).toHaveValue("echo installing\necho done > .installed");
  await expect(page.getByLabel("Wert von NODE_ENV")).toHaveValue("ci");
  await expect(agent.getByRole("radio", { name: "Erlaubte Adressen" })).toBeChecked();
  await expect(agent.getByLabel("Weitere Adressen")).toHaveValue("docs.rs");
  await expect(agent.getByLabel(/Python \(PyPI\)/)).toBeChecked();
  expect(await page.content()).not.toContain(SECRET);

  // A task runs the setup first; the drawer shows setup and network steps.
  const pid = new URL(page.url()).pathname.split("/").pop()!;
  const created = await page.request.post("/api/tasks", { data: { project_id: pid, prompt: "MOCK_NET:https://exfil.invalid/x env task" } });
  const tid = (await created.json()).id;
  const d = await waitTask(page.request, tid, "done");
  expect(JSON.stringify(d)).not.toContain(SECRET);
  await navLink(page, "Aufträge").click();
  await expect(taskRow(page, "env task", "done")).toBeVisible();
  const drawer = await openTask(page, "env task", "done");
  await openLog(page);
  await expect(drawer.getByText("Setup startet", { exact: false })).toBeVisible();
  await expect(drawer.getByText("Blockiert (Agent): CONNECT exfil.invalid:443", { exact: false })).toBeVisible();
});

test("members who are not owner see the environment read-only", async ({ page, browser }) => {
  await uiRegister(page, "envown2");
  const pid = await apiProject(page.request, { name: "SharedEnv" });
  const ctx = await browser.newContext();
  const other = await ctx.newPage();
  const email = await uiRegister(other, "enved");
  expect((await page.request.post(`/api/projects/${pid}/members`, { data: { email, role: "editor" } })).status()).toBe(200);
  await other.reload();
  await openEnvironment(other, "SharedEnv");
  await expect(other.getByText("Nur die Eigentümerin bzw. der Eigentümer")).toBeVisible();
  await expect(other.getByLabel("Setup-Skript")).toBeDisabled();
  await expect(other.getByRole("button", { name: "Umgebung speichern" })).toHaveCount(0);
  await expect(other.getByRole("button", { name: "Geheimnis speichern" })).toHaveCount(0);
  // Clearing the cache widens nothing: editors may.
  await other.getByTestId("env-clear-cache").click();
  await expect(other.getByText("Cache geleert")).toBeVisible();
  await ctx.close();
});
