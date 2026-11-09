/**
 * WP03 system tests: real app and runner binaries (engine `none`). Gates
 * before commit and push, approvals, the hardened git of the runner. The git
 * remote is a local bare repository (file://), no external calls.
 */
import { execFileSync } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test } from "@playwright/test";
import { apiProject, apiUser, waitTask } from "../helpers";

const git = (args: string[], cwd?: string) => execFileSync("git", args, { cwd, encoding: "utf8" });

test("auto-push never goes to the default branch without approval", async ({ request }) => {
  const dir = mkdtempSync(join(tmpdir(), "wp03-remote-"));
  try {
    const bare = join(dir, "origin.git");
    git(["init", "-q", "--bare", "-b", "main", bare]);
    await apiUser(request, "sysgate");
    const p = await apiProject(request, { name: "SysGate", git_remote_url: `file://${bare}`, git_branch: "main" });
    await request.patch(`/api/projects/${p}`, { data: { auto_push: true, review_enabled: false } });
    const t = (await (await request.post("/api/tasks", { data: { project_id: p, prompt: "harmless change" } })).json()).id;
    const held = await waitTask(request, t, "held");
    expect(held.task.gate.stage).toBe("push");
    expect(held.task.gate.findings[0].kind).toBe("push_protected");
    expect(() => git(["--git-dir", bare, "rev-parse", "--verify", "-q", "refs/heads/main"])).toThrow();
    expect((await request.post(`/api/tasks/${t}/gate/approve`, { data: { action: "approve" } })).status()).toBe(200);
    const done = await waitTask(request, t, "done");
    expect(done.events.map((e) => e.content)).toContain("git push (freigegeben): ok");
    expect(git(["--git-dir", bare, "log", "--format=%s", "main"])).toContain("harmless change");
    // "Nicht hochladen" finishes without a push.
    const t2 = (await (await request.post("/api/tasks", { data: { project_id: p, prompt: "second change" } })).json()).id;
    await waitTask(request, t2, "held");
    expect((await request.post(`/api/tasks/${t2}/gate/reject`, { data: {} })).status()).toBe(200);
    await waitTask(request, t2, "done");
    expect(git(["--git-dir", bare, "log", "--format=%s", "main"])).not.toContain("second change");
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("secrets are held and redacted end to end; a rejected hold leaves no trace", async ({ request }) => {
  await apiUser(request, "sysgate2");
  const p = await apiProject(request, { name: "SysGate2" });
  await request.patch(`/api/projects/${p}`, { data: { review_enabled: false } });
  const t = (await (await request.post("/api/tasks", { data: { project_id: p, prompt: "MOCK_SECRET_FILE MOCK_SECRET_LOG leak" } })).json()).id;
  const held = await waitTask(request, t, "held");
  const kinds = held.task.gate.findings.map((f: { kind: string }) => f.kind).sort();
  expect(kinds).toEqual(["secret", "secret_in_log"]);
  const everything = JSON.stringify(held) + JSON.stringify(await (await request.get(`/api/tasks/${t}/gate/diff`)).json()) + JSON.stringify(await (await request.get("/api/notifications")).json());
  expect(everything).not.toMatch(/gh[p]_[A-Za-z0-9]{36}/);
  expect(everything).toContain("[geschwärzt: GitHub-Token]");
  expect((await request.post(`/api/tasks/${t}/gate/reject`, { data: {} })).status()).toBe(200);
  const b = await waitTask(request, t, "blocked");
  expect(b.task.failure_class).toBe("gate");
  const files = (await (await request.get(`/api/projects/${p}/files`)).json()) as string[];
  expect(files).not.toContain("config/credentials.py");
});
