/** WP11 desktop: admin console – lock a person (their browser is signed out at once), invitation link, audit log, runner overview, axe. */
import { expect, test } from "@playwright/test";
import { ADMIN_EMAIL, expectAccessible, navLink, PASSWORD, uiLogin, uiRegister, uniqueEmail } from "../helpers";

test("admin locks a person, invites someone, reads the audit log and the runners", async ({ page, browser }) => {
  // The person to lock works in another browser.
  const victimCtx = await browser.newContext();
  const victim = await victimCtx.newPage();
  const victimEmail = await uiRegister(victim, "locked");

  await uiLogin(page, ADMIN_EMAIL);
  await expect(navLink(page, "Aufträge")).toBeVisible();
  await page.getByTestId("admin-link").click();
  await expect(page.getByRole("heading", { level: 1, name: "Verwaltung" })).toBeVisible();
  for (const scheme of ["light", "dark"] as const) {
    await page.emulateMedia({ colorScheme: scheme });
    await expectAccessible(page, `admin people (${scheme})`);
  }

  // Lock: search, confirm with a reason.
  await page.getByLabel("Personen suchen").fill(victimEmail);
  await page.getByRole("button", { name: "Suchen" }).click();
  const row = page.locator(`[data-testid=admin-user][data-email="${victimEmail}"]`);
  await expect(row).toHaveCount(1);
  await row.getByTestId("lock-user").click();
  const dlg = page.getByRole("alertdialog");
  await expect(dlg).toContainText("sperren?");
  await dlg.getByLabel("Grund (optional, für das Protokoll)").fill("E2E-Test");
  await dlg.getByTestId("lock-confirm").click();
  await expect(row).toContainText("Gesperrt");
  await expect(row).toContainText("0 Geräte");
  // The locked person is out within seconds and cannot sign in again.
  await expect(victim.getByRole("button", { name: "Anmelden", exact: true })).toBeVisible({ timeout: 20_000 });
  await uiLogin(victim, victimEmail);
  await expect(victim.getByRole("alert")).toContainText("gesperrt");
  await row.getByRole("button", { name: "Entsperren" }).click();
  await expect(row).not.toContainText("Gesperrt");
  await uiLogin(victim, victimEmail);
  await expect(navLink(victim, "Aufträge")).toBeVisible();
  await victimCtx.close();

  // Invitation bound to an address; the link registers exactly once.
  await page.getByRole("tab", { name: "Einladungen" }).click();
  const invited = uniqueEmail("invited");
  await page.getByTestId("invite-email").fill(invited);
  await page.getByTestId("invite-create").click();
  const link = await page.getByTestId("invite-link").inputValue();
  expect(link).toContain("?invite=qinv_");
  await expectAccessible(page, "admin invitations");
  const guestCtx = await browser.newContext();
  const guest = await guestCtx.newPage();
  await guest.goto(`/${link.slice(link.indexOf("?"))}`);
  await expect(guest.getByText("Du wurdest eingeladen")).toBeVisible();
  await expect(guest.getByLabel("E-Mail")).toHaveValue(invited);
  await guest.getByLabel("Name").fill("Gast");
  await guest.getByLabel("Passwort").fill(PASSWORD);
  await guest.getByRole("button", { name: "Konto erstellen" }).click();
  await expect(navLink(guest, "Aufträge")).toBeVisible();
  await guestCtx.close();
  await page.getByRole("tab", { name: "Personen" }).click();
  await page.getByRole("tab", { name: "Einladungen" }).click();
  await expect(page.getByTestId("invites")).toContainText("Angenommen");

  // Rules (not changed here: other tests run in parallel on this instance).
  await page.getByRole("tab", { name: "Regeln" }).click();
  await expect(page.getByRole("radio", { name: /Offen/ })).toBeChecked();
  await expectAccessible(page, "admin rules");

  // Audit log: filter, integrity check.
  await page.getByRole("tab", { name: "Protokoll" }).click();
  await page.getByTestId("audit-group").selectOption({ label: "Verwaltung" });
  const entries = page.getByTestId("audit-entries");
  await expect(entries.locator('[data-action="admin.user_disabled"]').first()).toBeVisible();
  await expect(entries.locator('[data-action="admin.invite_created"]').first()).toBeVisible();
  await expect(entries).not.toContainText(PASSWORD);
  await page.getByTestId("audit-verify").click();
  await expect(page.getByTestId("audit-intact")).toContainText("keiner wurde verändert");
  for (const scheme of ["light", "dark"] as const) {
    await page.emulateMedia({ colorScheme: scheme });
    await expectAccessible(page, `admin audit (${scheme})`);
  }

  // Runners: the configured runner is ready; registering shows the token once.
  await page.getByRole("tab", { name: "Runner" }).click();
  const builtin = page.locator('[data-testid=runner][data-name="Standard-Runner"]');
  await expect(builtin.getByTestId("runner-state")).toHaveText("Bereit");
  await expect(builtin).toContainText("Sandbox: none");
  await page.getByTestId("runner-add").click();
  const add = page.getByRole("dialog");
  await add.getByTestId("runner-name").fill(`E2E-Runner-${Date.now()}`);
  await add.getByTestId("runner-url").fill("http://127.0.0.1:9");
  await add.getByTestId("runner-create").click();
  await expect(page.getByTestId("runner-token")).toHaveValue(/^qr_/);
  await expectAccessible(page, "admin runners");
  for (const scheme of ["light", "dark"] as const) {
    await page.emulateMedia({ colorScheme: scheme });
    await expectAccessible(page, `admin runners (${scheme})`);
  }
  // Revoke it again (nothing lives there).
  const fresh = page.locator("[data-testid=runner]").filter({ hasText: "E2E-Runner-" }).last();
  await expect(fresh.getByTestId("runner-state")).toHaveText("Nicht erreichbar");
  await fresh.getByRole("button", { name: "Widerrufen" }).click();
  await page.getByRole("alertdialog").getByRole("button", { name: "Widerrufen" }).click();
  await expect(fresh.getByTestId("runner-state")).toHaveText("Widerrufen");
});

test("non-admins see no console", async ({ page }) => {
  await uiRegister(page, "plain");
  await expect(page.getByTestId("admin-link")).toHaveCount(0);
  await page.goto("/admin");
  await expect(page.getByText("Nur für Admins")).toBeVisible();
  const r = await page.request.get("/api/admin/users");
  expect(r.status()).toBe(403);
});
