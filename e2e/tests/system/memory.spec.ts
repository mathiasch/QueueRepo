/**
 * WP09 system tests over the real process boundary (app + runner binaries,
 * mock agent): attachments (MIME forgery, EXIF removed, protective headers,
 * provided to the agent, never in a commit), project rules (agent proposal
 * only after approval in the prompt), invalid project state rejected and
 * OnEmpty dedupe preventing a duplicate.
 */
import { expect, test } from "@playwright/test";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { apiProject, apiUser, waitTask } from "../helpers";

const fixture = (n: string) => readFileSync(join(__dirname, "..", "..", "fixtures", n));
const b64 = (s: string) => Buffer.from(s).toString("base64");

test("attachments: forgery refused, EXIF/GPS removed, provided to the agent, never committed", async ({ request }) => {
  await apiUser(request, "sys-att");
  const pid = await apiProject(request, { name: "SysAnhänge" });
  await request.patch(`/api/projects/${pid}`, { data: { review_enabled: false } });
  const up = (name: string, data: Buffer, type = "application/octet-stream") =>
    request.post(`/api/projects/${pid}/attachments?name=${encodeURIComponent(name)}`, { data, headers: { "content-type": type } });

  // SVG/HTML under an image name: refused whatever the Content-Type says.
  for (const [n, d] of [["bild.png", "<svg onload=alert(1)>"], ["notiz.txt", "<!doctype html><script>alert(1)</script>"]] as const) {
    const r = await up(n, Buffer.from(d), "image/png");
    expect(r.status(), n).toBe(422);
  }
  const src = fixture("iphone-gps.jpg");
  expect(src.includes("GPS-SECRET-E2E")).toBe(true);
  const r = await up("IMG_4711.JPG", src, "text/html");
  expect(r.status(), await r.text()).toBe(200);
  const photo = await r.json();
  expect(photo.mime).toBe("image/jpeg");
  expect([photo.width, photo.height]).toEqual([48, 64]);
  const dl = await request.get(`/api/attachments/${photo.id}`);
  expect(dl.status()).toBe(200);
  const h = dl.headers();
  expect(h["content-disposition"]).toMatch(/^attachment; filename="IMG_4711.JPG"/);
  expect(h["x-content-type-options"]).toBe("nosniff");
  expect(h["content-security-policy"]).toBe("default-src 'none'; sandbox");
  const body = Buffer.from(await dl.body());
  expect(body.includes("GPS-SECRET-E2E")).toBe(false);
  expect(body.includes("Exif")).toBe(false);
  expect(body.includes("iPhone 15 Pro")).toBe(false);

  const t = await (await request.post("/api/tasks", {
    data: { project_id: pid, title: "Fehler aus dem Foto beheben", prompt: "MOCK_SEE_ATTACHMENTS MOCK_COMMIT_ATTACHMENT Fehler aus dem Foto", attachment_ids: [photo.id] },
  })).json();
  const done = await waitTask(request, t.id, "done");
  expect(done.events.some((e) => e.content.includes("ohne diese Dateien zusammengefasst"))).toBe(true);
  const files: { path: string }[] = await (await request.get(`/api/projects/${pid}/files`)).json();
  const paths = files.map((f: any) => (typeof f === "string" ? f : f.path));
  expect(paths).toContain("SEEN-attachments.txt");
  expect(paths.filter((p) => p.startsWith(".queue"))).toEqual([]);
  const seen = await (await request.get(`/api/projects/${pid}/file?path=SEEN-attachments.txt`)).json();
  expect(seen.content).toContain(photo.name);
});

test("an agent's rule proposal reaches the prompt only after the owner approved it", async ({ request }) => {
  await apiUser(request, "sys-rules");
  const pid = await apiProject(request, { name: "SysRegeln" });
  await request.patch(`/api/projects/${pid}`, { data: { review_enabled: false } });
  const t = await (await request.post("/api/tasks", {
    data: { project_id: pid, title: "Lernen", prompt: `learn MOCK_KNOWLEDGE_B64:${b64("- MOCKRULE-SYS immer klein committen\n")}` },
  })).json();
  await waitTask(request, t.id, "done");
  const k = await (await request.get(`/api/projects/${pid}/knowledge`)).json();
  expect(k.current).toBeNull();
  expect(k.proposals).toHaveLength(1);
  const seenBefore = await (await request.get(`/api/projects/${pid}/file?path=SEEN.md`)).json().catch(() => ({ content: "" }));
  expect(seenBefore.content ?? "").not.toContain("MOCKRULE-SYS");
  const t2 = await (await request.post("/api/tasks", { data: { project_id: pid, prompt: "vorher" } })).json();
  await waitTask(request, t2.id, "done");
  expect(((await (await request.get(`/api/projects/${pid}/file?path=SEEN.md`)).json()).content ?? "")).not.toContain("MOCKRULE-SYS");
  const ap = await request.post(`/api/projects/${pid}/knowledge/${k.proposals[0].id}/approve`, { data: {} });
  expect(ap.status()).toBe(200);
  const t3 = await (await request.post("/api/tasks", { data: { project_id: pid, prompt: "nachher" } })).json();
  await waitTask(request, t3.id, "done");
  expect((await (await request.get(`/api/projects/${pid}/file?path=SEEN.md`)).json()).content).toContain("MOCKRULE-SYS");
});

test("invalid project state from the agent is rejected, the old state stays", async ({ request }) => {
  await apiUser(request, "sys-state");
  const pid = await apiProject(request, { name: "SysZustand" });
  await request.patch(`/api/projects/${pid}`, { data: { review_enabled: false } });
  const put = await request.put(`/api/projects/${pid}/state`, {
    data: { base_rev: 0, state: { version: 1, features: [{ id: "login", title: "Login", status: "todo" }], progress: [] } },
  });
  expect(put.status()).toBe(200);
  const bad = JSON.stringify({ version: 2, features: [], progress: [] });
  const t = await (await request.post("/api/tasks", { data: { project_id: pid, title: "Zustand", prompt: `MOCK_STATE_B64:${b64(bad)}` } })).json();
  const d = await waitTask(request, t.id, "done");
  expect(d.events.some((e) => e.content.includes("Projektzustand des Agenten abgelehnt") && e.content.includes("Schema-Version 2"))).toBe(true);
  const s = await (await request.get(`/api/projects/${pid}/state`)).json();
  expect(s.rev).toBe(1);
  expect(s.state.features[0].status).toBe("todo");
});

test("OnEmpty dedupe drops the duplicate and explains why", async ({ request }) => {
  await apiUser(request, "sys-dedupe");
  const pid = await apiProject(request, { name: "SysLeerlauf" });
  await request.patch(`/api/projects/${pid}`, { data: { review_enabled: false } });
  const first = await (await request.post("/api/tasks", {
    data: { project_id: pid, title: "Add unit tests for the CSV parser", prompt: "Write unit tests covering the CSV parser edge cases (quotes, empty lines)." },
  })).json();
  await waitTask(request, first.id, "done");
  const result = { status: "done", summary: "Vorschläge", handoff: "", questions: [], follow_up_tasks: [
    { title: "Add unit tests for CSV parser", prompt: "Write unit tests covering the CSV parser edge cases like quotes and empty lines." },
    { title: "Document the HTTP API", prompt: "Write a short reference of all HTTP endpoints with request and response examples." },
  ] };
  await request.patch(`/api/projects/${pid}`, { data: { goal: `Ship it MOCK_ONEMPTY_B64:${b64(JSON.stringify(result))}` } });
  const s = await (await request.get("/api/settings")).json();
  await request.put("/api/settings", { data: { ...s, onempty_enabled: true, onempty_actions: ["improve"], onempty_cooldown_minutes: 300, max_auto_tasks: 5 } });
  let report: any[] = [];
  await expect.poll(async () => {
    const ts: any[] = await (await request.get("/api/tasks")).json();
    const oe = ts.find((t) => t.kind === "onempty" && t.project_id === pid && t.status === "done" && Array.isArray(t.proposals));
    report = oe?.proposals ?? [];
    return report.map((r) => r.verdict);
  }, { timeout: 30_000 }).toEqual(["dropped", "accepted"]);
  expect(report[0].reasons[0]).toMatch(/^Duplikat: \d+ % ähnlich zu „Add unit tests for the CSV parser“/);
  const ts: any[] = await (await request.get("/api/tasks")).json();
  expect(ts.filter((t) => t.project_id === pid && t.title === "Add unit tests for CSV parser")).toHaveLength(0);
  expect(ts.find((t) => t.project_id === pid && t.title === "Document the HTTP API")?.dedupe?.verdict).toBe("accepted");
  await request.put("/api/settings", { data: { ...s, onempty_enabled: false } });
});
