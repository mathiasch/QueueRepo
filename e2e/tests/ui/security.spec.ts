/** WP11 desktop: two-factor sign-in (authenticator app with QR code, recovery code, passkey) and signed-in devices. */
import { expect, test, type Page } from "@playwright/test";
import { expectAccessible, navLink, PASSWORD, totp, uiLogin, uiRegister } from "../helpers";

async function openSecurity(page: Page) {
  await navLink(page, "Einstellungen").click();
  const card = page.locator("#security");
  await expect(card.getByTestId("security")).toBeVisible();
  return card;
}

test("authenticator app: QR setup, sign-in with code and recovery code, other devices signed out", async ({ page, browser }) => {
  const email = await uiRegister(page, "mfa");
  let card = await openSecurity(page);
  await expect(card).toContainText("Zwei-Faktor-Anmeldung ist aus");
  await card.getByTestId("totp-start").click();
  const dialog = page.getByRole("dialog");
  await dialog.getByLabel("Dein aktuelles Passwort").fill(PASSWORD);
  await dialog.getByRole("button", { name: "Weiter" }).click();
  await expect(dialog.getByTestId("totp-qr")).toBeVisible();
  await expectAccessible(page, "totp setup dialog");
  await dialog.getByText("Schlüssel von Hand eingeben").click();
  const secret = (await dialog.getByTestId("totp-secret").innerText()).replace(/\s/g, "");
  expect(secret).toMatch(/^[A-Z2-7]{32}$/);
  await dialog.getByTestId("totp-confirm-code").fill("000000");
  await dialog.getByRole("button", { name: "Einrichten" }).click();
  await expect(dialog.getByRole("alert")).toContainText("Der Code stimmt nicht");
  await dialog.getByTestId("totp-confirm-code").fill(totp(secret));
  await dialog.getByRole("button", { name: "Einrichten" }).click();
  const codes = card.getByTestId("recovery-codes");
  await expect(codes.locator("li")).toHaveCount(10);
  const recovery = (await codes.locator("li").first().innerText()).trim();
  expect(recovery).toMatch(/^[2-9A-Z]{5}-[2-9A-Z]{5}$/);
  await expect(card).toContainText("Zwei-Faktor-Anmeldung ist aktiv");
  await expectAccessible(page, "security card with recovery codes");
  await card.getByRole("button", { name: "Ich habe die Codes gespeichert" }).click();

  // Second device: password + recovery code.
  const other = await browser.newContext();
  const p2 = await other.newPage();
  await uiLogin(p2, email);
  await expect(p2.getByTestId("mfa-step")).toBeVisible();
  await expectAccessible(p2, "second sign-in step");
  await p2.getByRole("button", { name: "Kein Zugriff? Wiederherstellungscode verwenden" }).click();
  await p2.getByLabel("Wiederherstellungscode").fill(recovery);
  await p2.getByRole("button", { name: "Bestätigen", exact: true }).click();
  await expect(navLink(p2, "Aufträge")).toBeVisible();

  // This device: sign out and in again with a code from the app.
  await page.getByRole("button", { name: "Abmelden" }).first().click();
  await expect(page.getByRole("button", { name: "Anmelden", exact: true })).toBeVisible();
  await uiLogin(page, email);
  await expect(page.getByTestId("mfa-step")).toBeVisible();
  await page.getByTestId("mfa-code").fill("123456");
  await page.getByRole("button", { name: "Bestätigen", exact: true }).click();
  await expect(page.getByTestId("mfa-step").getByRole("alert")).toContainText("Der Code stimmt nicht");
  // The setup consumed the current step: the next one is accepted (±30 s).
  await page.getByTestId("mfa-code").fill(totp(secret, 1));
  await page.getByRole("button", { name: "Bestätigen", exact: true }).click();
  await expect(navLink(page, "Aufträge")).toBeVisible();

  // Devices: both listed, the other one signed out from here.
  card = await openSecurity(page);
  await expect(card.getByTestId("sessions").locator("li")).toHaveCount(2);
  await expect(card.getByTestId("sessions")).toContainText("Dieses Gerät");
  await expect(card.getByTestId("sessions")).toContainText("Wiederherstellungscode");
  await expect(card.getByTestId("recovery-left")).toContainText("Noch 9 von 10");
  await card.getByTestId("sessions-revoke-others").click();
  await page.getByRole("alertdialog").getByRole("button", { name: "Abmelden" }).click();
  await expect(card.getByTestId("sessions").locator("li")).toHaveCount(1);
  // The other browser notices within seconds (live stream ends).
  await expect(p2.getByRole("button", { name: "Anmelden", exact: true })).toBeVisible({ timeout: 20_000 });
  await expect(p2.getByRole("alert")).toContainText("abgemeldet");
  await other.close();
  for (const scheme of ["light", "dark"] as const) {
    await page.emulateMedia({ colorScheme: scheme });
    await expectAccessible(page, `security card (${scheme})`);
  }
});

test("passkey: added with the browser's authenticator and used to sign in", async ({ browser }) => {
  // WebAuthn needs a domain (no IP address): the stack's PUBLIC_URL is localhost.
  const ctx = await browser.newContext({ baseURL: `http://localhost:${process.env.E2E_APP_PORT ?? 18080}` });
  const page = await ctx.newPage();
  const cdp = await ctx.newCDPSession(page);
  await cdp.send("WebAuthn.enable");
  await cdp.send("WebAuthn.addVirtualAuthenticator", {
    options: { protocol: "ctap2", transport: "internal", hasResidentKey: true, hasUserVerification: true, isUserVerified: true, automaticPresenceSimulation: true },
  });
  const email = await uiRegister(page, "passkey");
  const card = await openSecurity(page);
  await card.getByTestId("passkey-add").click();
  const dialog = page.getByRole("dialog");
  await dialog.getByTestId("passkey-name").fill("Laptop");
  await dialog.getByLabel("Dein aktuelles Passwort").fill(PASSWORD);
  await dialog.getByTestId("passkey-create").click();
  await expect(card.getByTestId("passkeys")).toContainText("Laptop");
  await expect(card.getByTestId("recovery-codes").locator("li")).toHaveCount(10);
  await card.getByRole("button", { name: "Ich habe die Codes gespeichert" }).click();

  await page.getByRole("button", { name: "Abmelden" }).first().click();
  await expect(page.getByRole("button", { name: "Anmelden", exact: true })).toBeVisible();
  await uiLogin(page, email);
  const step = page.getByTestId("mfa-step");
  await expect(step).toBeVisible();
  await step.getByRole("button", { name: "Mit Passkey bestätigen" }).click();
  await expect(navLink(page, "Aufträge")).toBeVisible();
  await ctx.close();
});
