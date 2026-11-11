/**
 * WP06 system tests: real app and runner binaries (engine `none`, mock agent).
 * Plan mode across the process boundary (the plan run changes nothing, the
 * approved plan reaches the implementing agent), a message delivered into
 * the running session (CLI resume with the session files the runner kept),
 * withdrawing, and "Weiter mit …" continuing the same session.
 */
import { expect, test } from "@playwright/test";
import { apiProject, apiUser, waitTask } from "../helpers";

const events = (d: { events: Record<string, any>[] }) => d.events.map((e) => `${e.kind}: ${e.content}`).join("\n");

test("plan mode: the plan run changes nothing; the approved plan is implemented", async ({ request }) => {
  await apiUser(request, "sysplan");
  const p = await apiProject(request, { name: "SysPlan" });
  await request.patch(`/api/projects/${p}`, { data: { review_enabled: false, plan_mode: true } });
  const files = async () => ((await (await request.get(`/api/projects/${p}/files`)).json()) as string[]).sort();
  const before = await files();
  const t = (await (await request.post("/api/tasks", { data: { project_id: p, prompt: "MOCK_PLAN_WRITE Galerie umbauen" } })).json()).id;
  const held = await waitTask(request, t, "held");
  expect(held.task.gate.stage).toBe("plan");
  expect(held.task.plan).toContain("MOCKPLAN-STEP-1");
  expect(held.task.plan_meta.attempted_changes).toContain("PLAN_SIDE_EFFECT.txt");
  expect(events(held)).toContain("alles verworfen");
  expect(await files()).toEqual(before);
  // Withdrawn before delivery: never reaches the agent; the other one does.
  const m1 = await (await request.post(`/api/tasks/${t}/messages`, { data: { body: "MOCKMSG-SYS-GONE" } })).json();
  await request.post(`/api/tasks/${t}/messages`, { data: { body: "MOCKMSG-SYS-KEPT" } });
  expect((await request.delete(`/api/tasks/${t}/messages/${m1.id}`)).status()).toBe(200);
  expect((await request.post(`/api/tasks/${t}/plan/approve`, { data: {} })).status()).toBe(200);
  const done = await waitTask(request, t, "done");
  expect(done.task.plan_state).toBe("approved");
  const seen = await (await request.get(`/api/projects/${p}/file?path=SEEN.md`)).json();
  expect(seen.content).toContain("MOCKPLAN-STEP-1");
  expect(seen.content).toContain("MOCKMSG-SYS-KEPT");
  expect(seen.content).not.toContain("MOCKMSG-SYS-GONE");
  expect(await files()).not.toContain("PLAN_SIDE_EFFECT.txt");
});

test("a message reaches the running session; Weiter mit continues it", async ({ request }) => {
  await apiUser(request, "syssteer");
  const p = await apiProject(request, { name: "SysSteer" });
  await request.patch(`/api/projects/${p}`, { data: { review_enabled: false } });
  const t = (await (await request.post("/api/tasks", { data: { project_id: p, prompt: "MOCK_SLOW lange Aufgabe" } })).json()).id;
  await expect
    .poll(async () => events(await (await request.get(`/api/tasks/${t}`)).json()), { timeout: 30_000 })
    .toContain("working slowly");
  const started = Date.now();
  const r = await request.post(`/api/tasks/${t}/messages`, { data: { body: "MOCKMSG-SYS-NOW bitte so", now: true } });
  expect(r.status(), await r.text()).toBe(200);
  const done = await waitTask(request, t, "done");
  expect(Date.now() - started).toBeLessThan(25_000);
  expect(done.task.steer_rounds).toBe(1);
  const msgs = await (await request.get(`/api/tasks/${t}/messages`)).json();
  expect(msgs.messages[0].delivery).toBe("resume");
  expect(events(done)).toContain("resumed session");
  expect(msgs.can_continue).toBe(true);
  // "Weiter mit …": the same session, continued by the next agent run.
  const c = await (await request.post(`/api/tasks/${t}/continue`, { data: { message: "MOCKMSG-SYS-NEXT weiter" } })).json();
  expect(c.kind).toBe("continue");
  expect(c.session_from).toBe(t);
  const next = await waitTask(request, c.id, "done");
  expect(events(next)).toMatch(/resumed session \S+ after \d+ earlier prompts/);
  const seen = await (await request.get(`/api/projects/${p}/file?path=SEEN.md`)).json();
  expect(seen.content).toContain("run resume MOCKMSG-SYS-NOW");
  expect(seen.content).toContain("run resume MOCKMSG-SYS-NEXT");
});
