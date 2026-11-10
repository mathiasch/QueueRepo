import AxeBuilder from "@axe-core/playwright";
import { expect, type APIRequestContext, type Page } from "@playwright/test";
import { execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

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

/** Main navigation link (sidebar on desktop, tab bar on phones). */
export const navLink = (page: Page, name: "Aufträge" | "Postfach" | "Projekte" | "Einstellungen") =>
  page.getByRole("navigation", { name: "Hauptnavigation" }).getByRole("link", { name: new RegExp(`^${name}`) });

/** Logs a page in by registering through the UI (labels, not placeholders). */
export async function uiRegister(page: Page, tag: string, name = tag) {
  const email = uniqueEmail(tag);
  await page.goto("/");
  await page.getByRole("button", { name: "Neues Konto erstellen" }).click();
  await page.getByLabel("Name").fill(name);
  await page.getByLabel("E-Mail").fill(email);
  await page.getByLabel("Passwort").fill(PASSWORD);
  await page.getByRole("button", { name: "Konto erstellen" }).click();
  await expect(navLink(page, "Aufträge")).toBeVisible();
  // Use the deterministic mock provider.
  const s = await (await page.request.get("/api/settings")).json();
  await page.request.put("/api/settings", { data: { ...s, default_provider: "mock", onempty_enabled: false } });
  return email;
}

/** Task row/card by its title, optionally in a given status. */
export const taskRow = (page: Page, text: string, status?: string) =>
  page.locator(status ? `article[data-status="${status}"]` : "article[data-task]", { hasText: text });

/** Opens the task drawer from a row. */
export async function openTask(page: Page, text: string, status?: string) {
  await taskRow(page, text, status).first().locator("button.open").click();
  const drawer = page.getByRole("dialog");
  await expect(drawer).toBeVisible();
  return drawer;
}

/** Opens the (collapsed by default) log in the drawer. */
export async function openLog(page: Page) {
  await page.getByRole("dialog").getByText("Protokoll anzeigen").click();
}

/** WCAG 2.0/2.1/2.2 A + AA checks with axe-core. Fails on any violation. */
export async function expectAccessible(page: Page, what: string) {
  // Let entry animations finish so colours are measured at their final value.
  await page.evaluate(() => Promise.all(document.getAnimations().filter((a) => a.effect?.getTiming().iterations !== Infinity).map((a) => a.finished.catch(() => {}))));
  const r = await new AxeBuilder({ page }).withTags(["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"]).analyze();
  const summary = r.violations.map((v) => `${v.id} (${v.impact}): ${v.help}\n  ${v.nodes.map((n) => n.target.join(" ")).slice(0, 5).join("\n  ")}`);
  expect(summary, `axe violations on ${what}`).toEqual([]);
}

/** Saves a screenshot into the test output and, if WP00_SCREENSHOTS_DIR is set, into the docs. */
export async function shot(page: Page, outputPath: (name: string) => string, name: string, fullPage = false) {
  // Full-page captures start at the top, otherwise sticky bars end up mid-page.
  if (fullPage) await page.evaluate(() => window.scrollTo(0, 0));
  await page.screenshot({ path: outputPath(`${name}.png`), fullPage });
  const dir = process.env.WP00_SCREENSHOTS_DIR;
  if (dir) {
    mkdirSync(dir, { recursive: true });
    await page.screenshot({ path: join(dir, `${name}.png`), fullPage });
  }
}

/**
 * iPhone 15 Pro safe-area insets (portrait: top 59 / bottom 34, landscape:
 * left+right 59, bottom 21), injected through the CSS custom properties the
 * app reads them from – Chromium has no real insets.
 */
export async function simulateInsets(page: Page) {
  const landscape = (page.viewportSize()?.width ?? 0) > (page.viewportSize()?.height ?? 0);
  const css = landscape
    ? ":root{--sat:0px !important;--sar:59px !important;--sab:21px !important;--sal:59px !important}"
    : ":root{--sat:59px !important;--sar:0px !important;--sab:34px !important;--sal:0px !important}";
  await page.addInitScript((c) => {
    const add = () => {
      const s = document.createElement("style");
      s.textContent = c;
      document.head.appendChild(s);
    };
    if (document.head) add();
    else document.addEventListener("DOMContentLoaded", add);
  }, css);
  return landscape;
}

/** WP05: fake GitHub/GitLab/Gitea of the test stack (`scripts/stack.sh`). */
export const FAKE_HOST = `http://127.0.0.1:${process.env.E2E_FAKE_HOST_PORT ?? 18095}`;
export const PR_TOKEN = "tok-e2e-secret-wp05";

/** Bare git remote with a first commit on `main` (a local stand-in for the host's repository). */
export function bareRemote(): string {
  const dir = mkdtempSync(join(tmpdir(), "wp05-remote-"));
  const bare = join(dir, "origin.git");
  const seed = join(dir, "seed");
  const git = (cwd: string, ...args: string[]) =>
    execFileSync("git", ["-c", "user.name=Seed", "-c", "user.email=s@x", ...args], { cwd, stdio: ["ignore", "pipe", "pipe"], env: { ...process.env, GIT_CONFIG_NOSYSTEM: "1" } });
  git(dir, "init", "-q", "--bare", "-b", "main", bare);
  git(dir, "clone", "-q", bare, seed);
  writeFileSync(join(seed, "README.md"), "# app\n");
  git(seed, "add", "-A");
  git(seed, "commit", "-q", "-m", "init");
  git(seed, "push", "-q", "origin", "HEAD:main");
  return bare;
}

/** Project in output mode `pr` on the fake host; returns ids and the fake's repo name. */
export async function prProject(
  request: APIRequestContext,
  name: string,
  opts: { kind?: "github" | "gitlab" | "gitea"; permissions?: Record<string, unknown>; delivery?: Record<string, unknown> } = {},
) {
  const kind = opts.kind ?? "github";
  const bare = bareRemote();
  const repo = `acme/${name.toLowerCase().replace(/[^a-z0-9]/g, "")}${Date.now().toString(36)}${Math.random().toString(36).slice(2, 6)}`;
  const pid = await apiProject(request, { name, git_remote_url: `file://${bare}`, git_branch: "main", git_token: PR_TOKEN });
  expect((await request.patch(`/api/projects/${pid}`, { data: { review_enabled: false } })).status()).toBe(200);
  const api = { github: "github/api/v3", gitlab: "gitlab/api/v4", gitea: "gitea/api/v1" }[kind];
  const d = await request.put(`/api/projects/${pid}/delivery`, {
    data: { output_mode: "pr", git_host: kind, git_api_url: `${FAKE_HOST}/${api}`, git_repo_path: repo, ...(opts.delivery ?? {}) },
  });
  expect(d.status(), await d.text()).toBe(200);
  const s = await request.post(`${FAKE_HOST}/_fake/setup`, { data: { kind, repo, token: PR_TOKEN, bare, permissions: opts.permissions ?? {} } });
  expect(s.status()).toBe(200);
  const control = async (path: string, body: Record<string, unknown>) => {
    const r = await request.post(`${FAKE_HOST}/_fake/${path}`, { data: { kind, repo, ...body } });
    expect(r.status(), await r.text()).toBe(200);
    return r.json();
  };
  const state = async () => (await (await request.get(`${FAKE_HOST}/_fake/state?kind=${kind}&repo=${encodeURIComponent(repo)}`)).json()) as {
    prs: { number: number; head: string; base: string; draft: boolean; open: boolean; labels: string[] }[];
    requests: string[];
  };
  return { pid, repo, kind, bare, control, state };
}

/** The pull request of a task as the app reports it (null while there is none). */
export async function taskPull(request: APIRequestContext, task: string) {
  return ((await (await request.get(`/api/tasks/${task}`)).json()).pull_request ?? null) as Record<string, any> | null;
}
