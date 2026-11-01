import { expect, type APIRequestContext, type Page } from "@playwright/test";

export const uniqueEmail = (tag: string) => `${tag}-${Date.now()}-${Math.random().toString(36).slice(2, 8)}@e2e.test`;
export const PASSWORD = "password123";

/** Registers via API (fast) and configures the mock provider. Cookies land in the request context. */
export async function apiUser(request: APIRequestContext, tag: string, settings: Record<string, unknown> = {}) {
  const email = uniqueEmail(tag);
  const r = await request.post("/api/auth/register", { data: { email, password: PASSWORD, name: tag } });
  expect(r.status()).toBe(200);
  const s = await (await request.get("/api/settings")).json();
  const put = await request.put("/api/settings", { data: { ...s, default_provider: "mock", onempty_enabled: false, ...settings } });
  expect(put.status()).toBe(200);
  return email;
}

export async function apiProject(request: APIRequestContext, body: Record<string, unknown>) {
  const r = await request.post("/api/projects", { data: body });
  expect(r.status(), await r.text()).toBe(200);
  return (await r.json()).id as string;
}

export async function waitTask(request: APIRequestContext, id: string, status: string) {
  await expect
    .poll(async () => (await (await request.get(`/api/tasks/${id}`)).json()).task.status, { timeout: 30_000 })
    .toBe(status);
  return (await (await request.get(`/api/tasks/${id}`)).json()) as { task: Record<string, any>; events: Record<string, any>[] };
}

/** Logs a page in by registering through the UI. */
export async function uiRegister(page: Page, tag: string) {
  const email = uniqueEmail(tag);
  await page.goto("/");
  await page.getByRole("button", { name: "Neues Konto erstellen" }).click();
  await page.getByPlaceholder("Name").fill(tag);
  await page.getByPlaceholder("E-Mail").fill(email);
  await page.getByPlaceholder("Passwort").fill(PASSWORD);
  await page.getByRole("button", { name: "Konto erstellen" }).click();
  await expect(page.getByRole("button", { name: /Queue/ }).first()).toBeVisible();
  // Use the deterministic mock provider.
  const s = await (await page.request.get("/api/settings")).json();
  await page.request.put("/api/settings", { data: { ...s, default_provider: "mock", onempty_enabled: false } });
  return email;
}
