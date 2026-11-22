import { request, type FullConfig } from "@playwright/test";
import { ADMIN_EMAIL, PASSWORD } from "./tests/helpers";

/**
 * WP11: the first account of a fresh instance becomes admin. The stack
 * starts with an empty database, so this account is registered before any
 * test runs; admin tests sign in with it, every other test gets ordinary
 * accounts.
 */
export default async function globalSetup(config: FullConfig) {
  const baseURL = config.projects[0].use.baseURL!;
  const ctx = await request.newContext({ baseURL });
  const r = await ctx.post("/api/auth/register", { data: { email: ADMIN_EMAIL, password: PASSWORD, name: "Admin" } });
  if (r.status() !== 200) throw new Error(`admin registration failed: ${r.status()} ${await r.text()}`);
  const me = await (await ctx.get("/api/auth/me")).json();
  if (!me.is_admin) throw new Error("the e2e admin is not the first account of this instance");
  await ctx.dispose();
}
