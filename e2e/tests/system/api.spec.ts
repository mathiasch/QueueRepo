/**
 * System tests: real binaries (app + runner as separate processes), real
 * Postgres, HTTP only. Verifies the process boundary and full task flow.
 */
import { expect, request as pwRequest, test } from "@playwright/test";
import { apiProject, apiUser, waitTask } from "../helpers";

const runnerPort = Number(process.env.E2E_RUNNER_PORT ?? 18090);

test("runner API is not usable without its token", async () => {
  const r = await pwRequest.newContext({ baseURL: `http://127.0.0.1:${runnerPort}` });
  expect((await r.get("/healthz")).status()).toBe(200);
  expect((await r.get("/v1/workspaces/x/files")).status()).toBe(401);
  expect((await r.post("/v1/exec", { data: {} })).status()).toBe(401);
  expect((await r.get("/v1/workspaces/x/files", { headers: { Authorization: "Bearer wrong" } })).status()).toBe(401);
});

test("app API requires a session", async ({ request }) => {
  for (const p of ["/api/tasks", "/api/projects", "/api/settings", "/api/limits", "/api/events"]) {
    expect((await request.get(p)).status(), p).toBe(401);
  }
  const meta = await (await request.get("/api/meta")).json();
  expect(meta.providers).toEqual(expect.arrayContaining(["claude", "codex", "gemini", "mock"]));
});

test("full task flow across processes: git commit, verify, review, usage", async ({ request }) => {
  await apiUser(request, "sys");
  const p = await apiProject(request, { name: "System", goal: "test" });
  await request.patch(`/api/projects/${p}`, { data: { verify_command: "test -f MOCK_LOG.md && git log --oneline | head -1" } });
  const t = (await (await request.post("/api/tasks", { data: { project_id: p, prompt: "system task" } })).json()).id;
  const d = await waitTask(request, t, "done");
  expect(d.task.review.approved).toBe(true);
  expect(d.events.map((e) => e.kind)).toEqual(expect.arrayContaining(["verify", "review"]));
  const commits = await (await request.get(`/api/projects/${p}/commits`)).json();
  expect(commits[0].subject).toBe("system task");
  const diff = await (await request.get(`/api/tasks/${t}/diff`)).json();
  expect(diff.diff).toContain("MOCK_LOG.md");
  const limits = await (await request.get("/api/limits")).json();
  expect(limits.find((l: any) => l.provider === "mock").used_5h).toBeGreaterThan(0);
});

test("budget pauses the queue across processes", async ({ request }) => {
  await apiUser(request, "budget");
  const p = await apiProject(request, { name: "Budget" });
  await request.patch(`/api/projects/${p}`, { data: { review_enabled: false } });
  expect((await request.put("/api/budgets/mock", { data: { limit_5h_tokens: 500 } })).status()).toBe(200);
  const t1 = (await (await request.post("/api/tasks", { data: { project_id: p, prompt: "one" } })).json()).id;
  await waitTask(request, t1, "done");
  const t2 = (await (await request.post("/api/tasks", { data: { project_id: p, prompt: "two" } })).json()).id;
  await new Promise((r) => setTimeout(r, 2500));
  expect((await (await request.get(`/api/tasks/${t2}`)).json()).task.status).toBe("queued");
  const notes = await (await request.get("/api/notifications")).json();
  expect(notes.some((n: any) => n.kind === "budget")).toBe(true);
  await request.put("/api/budgets/mock", { data: { limit_5h_tokens: null } });
  await waitTask(request, t2, "done");
});

test("transient provider error is retried across processes without a fix task", async ({ request }) => {
  await apiUser(request, "transient");
  const p = await apiProject(request, { name: "Transient" });
  const t = (await (await request.post("/api/tasks", { data: { project_id: p, prompt: "MOCK_TRANSIENT flaky" } })).json()).id;
  const d = await waitTask(request, t, "done");
  expect(d.task.attempt).toBe(0);
  expect(d.task.transient_retries).toBe(1);
  expect(d.task.failure_class).toBeNull();
  expect(d.events.some((e) => e.kind === "retry")).toBe(true);
  const tasks = await (await request.get(`/api/projects/${p}/tasks`)).json();
  expect(tasks.filter((x: any) => x.kind === "fix")).toHaveLength(0);
});

test("SIGINT cancel flushes usage across processes", async ({ request }) => {
  await apiUser(request, "sigint");
  const p = await apiProject(request, { name: "Sigint" });
  const t = (await (await request.post("/api/tasks", { data: { project_id: p, prompt: "MOCK_TRAP long" } })).json()).id;
  await expect
    .poll(async () => (await (await request.get(`/api/tasks/${t}`)).json()).events.some((e: any) => e.content === "waiting for SIGINT"), { timeout: 30_000 })
    .toBe(true);
  const started = Date.now();
  await request.post(`/api/tasks/${t}/cancel`);
  const c = await waitTask(request, t, "cancelled");
  expect(Date.now() - started).toBeLessThan(10_000);
  expect(c.task.usage.input_tokens).toBe(2400);
  expect(c.events.some((e) => e.content === "interrupted cleanly")).toBe(true);
});

test("cancel crosses the process boundary", async ({ request }) => {
  await apiUser(request, "cancel");
  const p = await apiProject(request, { name: "Cancel" });
  const t = (await (await request.post("/api/tasks", { data: { project_id: p, prompt: "MOCK_SLOW" } })).json()).id;
  await waitTask(request, t, "running");
  await new Promise((r) => setTimeout(r, 500));
  const started = Date.now();
  await request.post(`/api/tasks/${t}/cancel`);
  await waitTask(request, t, "cancelled");
  expect(Date.now() - started).toBeLessThan(10_000);
});

test("cancel during verify across processes ends cancelled without a fix task", async ({ request }) => {
  await apiUser(request, "verifycancel");
  const p = await apiProject(request, { name: "VerifyCancel" });
  await request.patch(`/api/projects/${p}`, { data: { review_enabled: false, verify_command: "echo verifying; sleep 30" } });
  const t = (await (await request.post("/api/tasks", { data: { project_id: p, prompt: "slow verify" } })).json()).id;
  await expect
    .poll(async () => (await (await request.get(`/api/tasks/${t}`)).json()).events.some((e: any) => e.kind === "verify" && e.content.startsWith("$")), { timeout: 30_000 })
    .toBe(true);
  const started = Date.now();
  await request.post(`/api/tasks/${t}/cancel`);
  const c = await waitTask(request, t, "cancelled");
  expect(Date.now() - started).toBeLessThan(10_000);
  expect(c.task.failure_class).toBe("cancelled");
  const tasks = await (await request.get(`/api/projects/${p}/tasks`)).json();
  expect(tasks.filter((x: any) => x.kind === "fix")).toHaveLength(0);
});

test("resume only lifts the caller's own provider pause", async ({ playwright, baseURL }) => {
  const a = await playwright.request.newContext({ baseURL });
  const b = await playwright.request.newContext({ baseURL });
  await apiUser(a, "pauseowner");
  await apiUser(b, "pauseother");
  const p = await apiProject(a, { name: "PauseOwner" });
  const t = (await (await a.post("/api/tasks", { data: { project_id: p, prompt: "MOCK_AUTH login" } })).json()).id;
  await expect
    .poll(async () => (await (await a.get("/api/limits")).json()).find((l: any) => l.provider === "mock").paused_reason, { timeout: 30_000 })
    .toBe("Invalid API key");
  expect((await b.post("/api/limits/mock/resume")).status()).toBe(200);
  const mine = (await (await a.get("/api/limits")).json()).find((l: any) => l.provider === "mock");
  expect(mine.paused_reason).toBe("Invalid API key");
  expect(mine.blocked).toBe(true);
  // The pause is set just before the task is re-queued.
  await expect.poll(async () => (await (await a.get(`/api/tasks/${t}`)).json()).task.status).toBe("queued");
  expect([403, 404]).toContain((await b.get(`/api/tasks/${t}`)).status());
  await a.post(`/api/tasks/${t}/cancel`);
  await a.dispose();
  await b.dispose();
});
