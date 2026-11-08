/** WP13 system tests across process boundaries (real app + runner binaries): connectors, prices, residency, estimate, costs. */
import { expect, test } from "@playwright/test";
import { apiProject, apiUser, waitTask } from "../helpers";

test("meta lists connector types, CLI notes and the price catalog version", async ({ request }) => {
  const meta = await (await request.get("/api/meta")).json();
  const kinds = meta.connector_types.map((t: any) => t.kind);
  for (const k of ["anthropic_api", "bedrock", "vertex", "anthropic_gateway", "openai_api", "azure_openai", "mistral", "infomaniak", "openai_compatible", "openrouter", "litellm", "gemini_api", "claude_subscription", "codex_subscription", "gemini_subscription"]) {
    expect(kinds).toContain(k);
  }
  expect(meta.cli_notes.gemini.state).toBe("frozen");
  expect(meta.cli_versions.opencode).toBe("1.19.5");
  expect(meta.price_catalog_version).toBe("2026-11-07");
});

test("connector secrets never come back; residency and costs are enforced server-side", async ({ request }) => {
  await apiUser(request, "sys-wp13");
  const pid = await apiProject(request, { name: "Sys" });
  const c = await request.post("/api/connectors", { data: { kind: "anthropic_api", label: "API", secret: "sk-ant-SYSTEM-SECRET" } });
  expect(c.status()).toBe(200);
  expect(await c.text()).not.toContain("SYSTEM-SECRET");
  const prices = await (await request.get("/api/prices")).json();
  expect(prices.entries.find((e: any) => e.id === "anthropic/claude-opus-5-5").input).toBe(4);

  const r = await request.put(`/api/projects/${pid}/routing`, { data: { routing: null, data_residency: "eu" } });
  expect(r.status()).toBe(200);
  const bad = await request.post("/api/tasks", { data: { project_id: pid, prompt: "x", connector_id: (await c.json()).id } });
  expect(bad.status()).toBe(400);

  const est = await (await request.post("/api/estimate", { data: { project_id: pid, model: "claude-sonnet-5-5" } })).json();
  expect(est.steps.find((s: any) => s.step === "execute").mid).toBeCloseTo(1.245, 6);
  expect(est.location).toBe("Lokal");

  const t = await request.post("/api/tasks", { data: { project_id: pid, prompt: "cost", model: "claude-sonnet-5-5" } });
  const done = await waitTask(request, (await t.json()).id, "done");
  expect(done.task.cost.totals[0].amount).toBeCloseTo(2 * (1200 * 2 + 150 * 10 + 800 * 0.1) / 1e6, 9);
  const costs = await (await request.get(`/api/costs?project_id=${pid}`)).json();
  expect(costs.totals[0].amount).toBeCloseTo(done.task.cost.totals[0].amount, 9);
});
