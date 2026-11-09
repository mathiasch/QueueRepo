/**
 * WP04 system tests: real app and runner binaries (engine `none`). Several
 * tasks of one project at the same time, each on its own branch; the merge
 * queue (rebase → verify again → fast-forward); a conflict becomes a fix
 * task that carries the changes; diff and revert against the merge base.
 * Agents are the mock provider, no external calls.
 */
import { expect, test, type APIRequestContext } from "@playwright/test";
import { apiProject, apiUser, waitTask } from "../helpers";

const add = async (request: APIRequestContext, project: string, prompt: string) =>
  (await (await request.post("/api/tasks", { data: { project_id: project, prompt } })).json()).id as string;

const status = async (request: APIRequestContext, id: string) => (await (await request.get(`/api/tasks/${id}`)).json()).task.status as string;

test("tasks of one project run in parallel on their own branches and merge one after another", async ({ request }) => {
  await apiUser(request, "sysbranch", { parallelism: 4 });
  const p = await apiProject(request, { name: "SysBranch" });
  const patch = await request.patch(`/api/projects/${p}`, { data: { parallelism: 2, review_enabled: false } });
  expect(patch.status()).toBe(200);
  expect((await patch.json()).parallelism).toBe(2);
  const a = await add(request, p, "MOCK_QUIET MOCK_FILE:alpha.md Alpha");
  const b = await add(request, p, "MOCK_QUIET MOCK_FILE:beta.md Beta");
  const c = await add(request, p, "MOCK_FILE:gamma.md Gamma");
  await expect.poll(async () => [await status(request, a), await status(request, b)], { timeout: 20_000 }).toEqual(["running", "running"]);
  expect(await status(request, c)).toBe("queued");
  for (const t of [a, b, c]) {
    const d = await waitTask(request, t, "done");
    expect(d.task.branch).toBe(`queue/${t.replace(/-/g, "").slice(0, 12)}`);
    expect(d.task.merge_state).toBe("merged");
    expect(d.events.some((e) => e.kind === "merge" && String(e.content).startsWith("In den Hauptstand übernommen"))).toBe(true);
  }
  const files = (await (await request.get(`/api/projects/${p}/files`)).json()) as string[];
  expect(files).toEqual(expect.arrayContaining(["alpha.md", "beta.md", "gamma.md"]));
  const commits = (await (await request.get(`/api/projects/${p}/commits`)).json()) as { subject: string }[];
  for (const title of ["Alpha", "Beta", "Gamma"]) {
    expect(commits.filter((x) => x.subject.endsWith(title))).toHaveLength(1);
  }
  expect(await (await request.get(`/api/projects/${p}/merge-queue`)).json()).toEqual([]);
});

test("a merge conflict becomes a fix task that carries the changes and resolves them", async ({ request }) => {
  await apiUser(request, "sysconflict", { parallelism: 4 });
  const p = await apiProject(request, { name: "SysConflict" });
  await request.patch(`/api/projects/${p}`, { data: { parallelism: 2, review_enabled: false } });
  const a = await add(request, p, "MOCK_QUIET MOCK_RESOLVE MOCK_FILE:shared.md left");
  const b = await add(request, p, "MOCK_QUIET MOCK_RESOLVE MOCK_FILE:shared.md right");
  await expect
    .poll(async () => [await status(request, a), await status(request, b)].sort().join(","), { timeout: 40_000 })
    .toBe("done,failed");
  const loser = (await status(request, a)) === "failed" ? a : b;
  const l = (await (await request.get(`/api/tasks/${loser}`)).json()).task;
  expect(l.merge_state).toBe("conflict");
  expect(l.error).toContain("shared.md");
  let fix = "";
  await expect
    .poll(async () => {
      const ts = (await (await request.get("/api/tasks")).json()) as { id: string; parent_id: string; kind: string }[];
      fix = ts.find((t) => t.parent_id === loser && t.kind === "fix")?.id ?? "";
      return fix;
    })
    .not.toBe("");
  const f = await waitTask(request, fix, "done");
  expect(f.task.carry_from).toBe(loser);
  expect(f.events.some((e) => String(e.content).includes("Konflikte in: shared.md"))).toBe(true);
  const shared = (await (await request.get(`/api/projects/${p}/file?path=shared.md`)).json()).content as string;
  expect(shared).not.toMatch(/^(<<<<<<<|>>>>>>>) /m);
  expect(shared.split("\n").filter((x) => x.startsWith("run ")).length).toBeGreaterThanOrEqual(2);
  // Unmerged work cannot be reverted; the fix (merged) can.
  expect((await request.post(`/api/tasks/${loser}/revert`)).status()).toBe(409);
  expect((await request.post(`/api/tasks/${fix}/revert`)).status()).toBe(200);
});

test("verify runs again when the target moved; diff and revert use the merge base", async ({ request }) => {
  await apiUser(request, "sysreverify", { parallelism: 4 });
  const p = await apiProject(request, { name: "SysReverify" });
  // Each change alone passes, both together fail.
  await request.patch(`/api/projects/${p}`, {
    data: { parallelism: 2, review_enabled: false, verify_command: "test ! -f one.md || test ! -f two.md" },
  });
  const a = await add(request, p, "MOCK_QUIET MOCK_FILE:one.md one");
  const b = await add(request, p, "MOCK_QUIET MOCK_FILE:two.md two");
  await expect
    .poll(async () => [await status(request, a), await status(request, b)].sort().join(","), { timeout: 40_000 })
    .toMatch(/^done,(failed|blocked)$/);
  const loser = ["failed", "blocked"].includes(await status(request, a)) ? a : b;
  const l = await (await request.get(`/api/tasks/${loser}`)).json();
  expect(l.task.merge_state).toBe("failed");
  expect(l.events.some((e: { kind: string; content: string }) => e.kind === "verify" && e.content.includes("nach Aktualisierung"))).toBe(true);
  // The unmerged task's diff shows only its own change (against its merge base).
  const diff = (await (await request.get(`/api/tasks/${loser}/diff`)).json()).diff as string;
  const own = loser === a ? "one.md" : "two.md";
  const other = loser === a ? "two.md" : "one.md";
  expect(diff).toContain(own);
  expect(diff).not.toContain(other);
  // Stop the fix chain (it cannot pass this verify).
  for (const t of (await (await request.get("/api/tasks")).json()) as { id: string; kind: string; status: string }[]) {
    if (t.kind === "fix" && !["done", "failed", "blocked", "cancelled"].includes(t.status)) await request.post(`/api/tasks/${t.id}/cancel`);
  }
  // Revert of the merged task against its merge base.
  const winner = loser === a ? b : a;
  expect((await request.post(`/api/tasks/${winner}/revert`)).status()).toBe(200);
  const files = (await (await request.get(`/api/projects/${p}/files`)).json()) as string[];
  expect(files).not.toContain(winner === a ? "one.md" : "two.md");
});
