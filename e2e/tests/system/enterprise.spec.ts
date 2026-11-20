/**
 * WP11 system tests (separate processes, real binaries):
 * - MFA over the API of the running stack (TOTP replay refused, token cannot reach account/admin routes),
 * - an admin lock revokes a token at once,
 * - `queued backup` / `queued restore` as commands against their own databases and two
 *   runner processes: encrypted, private files, refusal of a non-empty target, identical refs.
 */
import { expect, request as pwRequest, test } from "@playwright/test";
import { execFileSync, spawn, type ChildProcess } from "node:child_process";
import { mkdtempSync, readdirSync, readFileSync, statSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { ADMIN_EMAIL, apiUser, PASSWORD, totp } from "../helpers";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "../../..");
const BIN = join(ROOT, "backend/target/debug");
const ADMIN_DB = process.env.E2E_DATABASE_ADMIN_URL ?? "postgres://queued:queued@localhost:5432/postgres";

test("two-factor sign-in over the API: replay refused, tokens never reach account or admin routes", async ({ request }) => {
  const email = await apiUser(request, "sysmfa");
  const setup = await (await request.post("/api/account/mfa/totp/setup", { data: { password: PASSWORD } })).json();
  const first = totp(setup.secret);
  expect((await request.post("/api/account/mfa/totp/confirm", { data: { code: first } })).status()).toBe(200);
  const tok = await (await request.post("/api/tokens", { data: { name: "sys", scopes: ["tasks:read"], expires_in_days: 7 } })).json();
  const t = await pwRequest.newContext({ baseURL: test.info().project.use.baseURL, extraHTTPHeaders: { Authorization: `Bearer ${tok.token}` } });
  expect((await t.get("/api/tasks")).status()).toBe(200);
  for (const p of ["/api/account/security", "/api/account/sessions", "/api/admin/users", "/api/admin/audit"]) {
    expect((await t.get(p)).status(), p).toBe(403);
  }
  const c = await pwRequest.newContext({ baseURL: test.info().project.use.baseURL });
  const login = await (await c.post("/api/auth/login", { data: { email, password: PASSWORD } })).json();
  expect(login).toMatchObject({ ok: false, mfa_required: true, methods: ["totp", "recovery"] });
  expect((await c.get("/api/tasks")).status(), "no session before the second factor").toBe(401);
  expect((await c.post("/api/auth/mfa/totp", { data: { code: first } })).status(), "replay").toBe(401);
  expect((await c.post("/api/auth/mfa/totp", { data: { code: totp(setup.secret, 1) } })).status()).toBe(200);
  expect((await c.get("/api/tasks")).status()).toBe(200);

  // Admin lock: the token is dead at once.
  const admin = await pwRequest.newContext({ baseURL: test.info().project.use.baseURL });
  expect((await admin.post("/api/auth/login", { data: { email: ADMIN_EMAIL, password: PASSWORD } })).status()).toBe(200);
  const users = await (await admin.get(`/api/admin/users?q=${encodeURIComponent(email)}`)).json();
  expect(users).toHaveLength(1);
  expect((await admin.patch(`/api/admin/users/${users[0].id}`, { data: { disabled: true } })).status()).toBe(200);
  expect((await t.get("/api/tasks")).status()).toBe(401);
  expect((await c.get("/api/tasks")).status()).toBe(401);
  for (const x of [t, c, admin]) await x.dispose();
});

function startRunner(dir: string, token: string, port: number): ChildProcess {
  return spawn(join(BIN, "queued-runner"), [], {
    env: { ...process.env, RUNNER_ALLOW_ROOT: "1", RUNNER_ENGINE: "none", RUNNER_TOKEN: token, RUNNER_DATA_DIR: dir, RUNNER_BIND: `127.0.0.1:${port}`, RUST_LOG: "warn" },
    stdio: "ignore",
  });
}

async function waitUp(url: string) {
  for (let i = 0; i < 100; i++) {
    try {
      if ((await fetch(url)).ok) return;
    } catch {
      /* not yet */
    }
    await new Promise((r) => setTimeout(r, 100));
  }
  throw new Error(`${url} did not start`);
}

function git(dir: string, ...args: string[]) {
  return execFileSync("git", args, { cwd: dir }).toString();
}

function walk(d: string): string[] {
  return readdirSync(d).flatMap((n) => {
    const p = join(d, n);
    return statSync(p).isDirectory() ? [p, ...walk(p)] : [p];
  });
}

test("backup and restore commands: encrypted, private, only into an empty database, refs identical", async () => {
  test.setTimeout(120_000);
  const work = mkdtempSync(join(tmpdir(), "wp11-backup-"));
  const base = ADMIN_DB.slice(0, ADMIN_DB.lastIndexOf("/"));
  const suffix = `${Date.now()}_${process.pid}`;
  const [dbA, dbB] = [`wp11_sys_a_${suffix}`, `wp11_sys_b_${suffix}`];
  for (const db of [dbA, dbB]) execFileSync("psql", [ADMIN_DB, "-qc", `CREATE DATABASE ${db}`]);
  const token = "system-test-runner-token-0123456789";
  const [portA, portB] = [18190 + (process.pid % 50), 18250 + (process.pid % 50)];
  const runners = [startRunner(join(work, "rA"), token, portA), startRunner(join(work, "rB"), token, portB)];
  try {
    await waitUp(`http://127.0.0.1:${portA}/healthz`);
    await waitUp(`http://127.0.0.1:${portB}/healthz`);
    // A repository on runner A (as a project workspace would be).
    const ws = "11111111-2222-4333-8444-555555555555";
    const init = await fetch(`http://127.0.0.1:${portA}/v1/workspaces/${ws}`, {
      method: "POST",
      headers: { Authorization: `Bearer ${token}`, "Content-Type": "application/json" },
      body: JSON.stringify({}),
    });
    expect(init.status).toBe(200);
    const env = (db: string, port: number, data: string) => ({
      ...process.env,
      DATABASE_URL: `${base}/${db}`,
      RUNNER_URL: `http://127.0.0.1:${port}`,
      RUNNER_TOKEN: token,
      DATA_DIR: data,
      MASTER_KEY: Buffer.alloc(32, 9).toString("base64"),
      RUST_LOG: "warn",
    });
    const keyFile = join(work, "backup.key");
    execFileSync(join(BIN, "queued"), ["backup-key", keyFile], { env: env(dbA, portA, join(work, "app")) });
    expect(statSync(keyFile).mode & 0o777).toBe(0o600);
    // Without a key: refused (no plaintext backups by accident).
    expect(() => execFileSync(join(BIN, "queued"), ["backup", "--out", join(work, "b")], { env: env(dbA, portA, join(work, "app")), stdio: "pipe" })).toThrow();
    const out = execFileSync(join(BIN, "queued"), ["backup", "--out", join(work, "b")], {
      env: { ...env(dbA, portA, join(work, "app")), BACKUP_KEY_FILE: keyFile },
    }).toString();
    const dir = out.trim().split("backup complete: ")[1];
    expect(statSync(dir).mode & 0o777).toBe(0o700);
    for (const f of walk(dir)) expect(statSync(f).mode & 0o077, f).toBe(0);
    const manifest = JSON.parse(readFileSync(join(dir, "manifest.json"), "utf8"));
    expect(manifest.encrypted).toBe(true);
    expect(manifest.workspaces.map((w: { id: string }) => w.id)).toContain(ws);
    expect(readFileSync(join(dir, "db.dump.qbk")).subarray(0, 4).toString()).toBe("QBK1");

    // Restore into the empty database B and the empty runner B.
    const r = execFileSync(join(BIN, "queued"), ["restore", "--from", dir], {
      env: { ...env(dbB, portB, join(work, "app2")), BACKUP_KEY_FILE: keyFile },
    }).toString();
    expect(r).toContain("restore complete");
    expect(git(join(work, "rB/workspaces", ws), "for-each-ref")).toBe(git(join(work, "rA/workspaces", ws), "for-each-ref"));
    const count = (db: string) => execFileSync("psql", [`${base}/${db}`, "-tAc", "SELECT count(*) FROM _sqlx_migrations"]).toString().trim();
    expect(count(dbB)).toBe(count(dbA));
    // A second restore into the now filled database is refused.
    expect(() =>
      execFileSync(join(BIN, "queued"), ["restore", "--from", dir], { env: { ...env(dbB, portB, join(work, "app2")), BACKUP_KEY_FILE: keyFile }, stdio: "pipe" }),
    ).toThrow(/not empty/);
  } finally {
    for (const r of runners) r.kill();
    for (const db of [dbA, dbB]) execFileSync("psql", [ADMIN_DB, "-qc", `DROP DATABASE IF EXISTS ${db} WITH (FORCE)`]);
  }
});
