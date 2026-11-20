/**
 * WP10 system test (separate processes: app, runner, Postgres): health,
 * readiness, Prometheus metrics of app and runner with their own tokens,
 * analytics separated per user, notification settings over HTTP.
 */
import { expect, request as pwRequest, test } from "@playwright/test";
import { apiProject, apiUser, waitTask } from "../helpers";

const APP_METRICS = process.env.E2E_METRICS_TOKEN ?? "e2e-app-metrics-token-0123456789";
const RUNNER_METRICS = process.env.E2E_RUNNER_METRICS_TOKEN ?? "e2e-runner-metrics-token-0123456789";
const RUNNER = `http://127.0.0.1:${process.env.E2E_RUNNER_PORT ?? 18090}`;

/** Every sample line of a Prometheus text exposition belongs to a TYPE. */
function checkExposition(text: string) {
  const types = new Set<string>();
  for (const line of text.split("\n")) {
    if (!line) continue;
    const m = line.match(/^# TYPE ([a-zA-Z_:][a-zA-Z0-9_:]*) (counter|gauge|histogram)$/);
    if (m) {
      types.add(m[1]);
      continue;
    }
    if (line.startsWith("# HELP ")) continue;
    const s = line.match(/^([a-zA-Z_:][a-zA-Z0-9_:]*)(\{.*\})? (-?[0-9.e+]+|\+Inf|NaN)$/);
    expect(s, `malformed sample: ${line}`).not.toBeNull();
    const name = s![1].replace(/_(bucket|sum|count)$/, "");
    expect(types.has(s![1]) || types.has(name), `sample without TYPE: ${line}`).toBe(true);
  }
}

test("health, readiness and protected metrics of app and runner", async ({ request }) => {
  const h = await request.get("/healthz");
  expect(h.status()).toBe(200);
  const r = await request.get("/readyz");
  expect(r.status()).toBe(200);
  expect(await r.json()).toEqual({ status: "ready", checks: { database: "ok", runner: "ok" } });

  // App metrics: only with the metrics token.
  expect((await request.get("/metrics")).status()).toBe(401);
  expect((await request.get("/metrics", { headers: { authorization: "Bearer wrong" } })).status()).toBe(401);
  await apiUser(request, "sysmet");
  const pid = await apiProject(request, { name: "Metrics" });
  const t = await (await request.post("/api/tasks", { data: { project_id: pid, prompt: "METRICS-TITLE-SECRET" } })).json();
  await waitTask(request, t.id, "done");
  const anon = await pwRequest.newContext({ baseURL: test.info().project.use.baseURL });
  const m = await anon.get("/metrics", { headers: { authorization: `Bearer ${APP_METRICS}` } });
  expect(m.status()).toBe(200);
  expect(m.headers()["content-type"]).toContain("text/plain; version=0.0.4");
  const text = await m.text();
  checkExposition(text);
  expect(text).toContain('queued_agent_runs_total{provider="mock",kind="agent",outcome="success"}');
  expect(text).not.toContain(t.id);
  expect(text).not.toContain("METRICS-TITLE-SECRET");

  // Runner: its own token; the runner token of the app is not known here,
  // the metrics token opens nothing else on the runner.
  const rc = await pwRequest.newContext();
  expect((await rc.get(`${RUNNER}/healthz`)).status()).toBe(200);
  expect((await rc.get(`${RUNNER}/metrics`)).status()).toBe(401);
  const rm = await rc.get(`${RUNNER}/metrics`, { headers: { authorization: `Bearer ${RUNNER_METRICS}` } });
  expect(rm.status()).toBe(200);
  const rtext = await rm.text();
  checkExposition(rtext);
  expect(rtext).toMatch(/queued_runner_exec_total\{kind="agent",outcome="success"\} [1-9]/);
  expect(rtext).not.toContain(t.id.replace(/-/g, "").slice(0, 12));
  expect((await rc.get(`${RUNNER}/v1/info`, { headers: { authorization: `Bearer ${RUNNER_METRICS}` } })).status()).toBe(401);
  await anon.dispose();
  await rc.dispose();
});

test("analytics are separated per user across processes", async ({ playwright }) => {
  const base = test.info().project.use.baseURL;
  const a = await playwright.request.newContext({ baseURL: base });
  const b = await playwright.request.newContext({ baseURL: base });
  await apiUser(a, "sysana");
  await apiUser(b, "sysanb");
  const pa = await apiProject(a, { name: "Analytics A" });
  const ta = await (await a.post("/api/tasks", { data: { project_id: pa, prompt: "Analytics task of A" } })).json();
  await waitTask(a, ta.id, "done");
  const ma = await (await a.get("/api/analytics?days=7")).json();
  expect(ma.outcomes.done).toBe(1);
  expect(ma.by_project.map((p: any) => p.name)).toEqual(["Analytics A"]);
  const mb = await (await b.get("/api/analytics?days=7")).json();
  expect(mb.outcomes.finished).toBe(0);
  expect(mb.by_project).toEqual([]);
  expect((await b.get(`/api/analytics?days=7&scope=project&project_id=${pa}`)).status()).toBe(404);
  expect((await b.get(`/api/tasks/${ta.id}/insights`)).status()).toBe(404);
  const ins = await (await a.get(`/api/tasks/${ta.id}/insights`)).json();
  expect(ins.runs.length).toBeGreaterThan(0);
  // Notification settings round trip.
  const s = await (await a.get("/api/notifications/settings")).json();
  s.quiet_enabled = true;
  s.quiet_start = 22 * 60;
  s.quiet_end = 7 * 60;
  s.timezone = "Europe/Zurich";
  s.matrix.task_done.push = true;
  const put = await a.put("/api/notifications/settings", { data: s });
  expect(put.status()).toBe(200);
  const back = await (await a.get("/api/notifications/settings")).json();
  expect(back.matrix.task_done.push).toBe(true);
  expect(back.timezone).toBe("Europe/Zurich");
  await a.dispose();
  await b.dispose();
});
