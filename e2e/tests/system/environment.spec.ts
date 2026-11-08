/**
 * WP02 system tests: real app and runner binaries (engine `none`, egress proxy
 * on loopback), setup phase with a secret, egress decisions as task events.
 * The only allowed target is a local fake server (127.0.0.1 is listed in
 * RUNNER_EGRESS_PRIVATE_HOSTS of the test stack); refused targets never
 * leave the host (refused before DNS).
 */
import { createServer } from "node:http";
import { AddressInfo } from "node:net";
import { expect, request as pwRequest, test } from "@playwright/test";
import { apiProject, apiUser, waitTask } from "../helpers";

const runnerPort = Number(process.env.E2E_RUNNER_PORT ?? 18090);
const SECRET = "system-secret-value-99";

test("runner reports that engine none enforces nothing; only members learn it", async ({ request }) => {
  const r = await pwRequest.newContext({ baseURL: `http://127.0.0.1:${runnerPort}` });
  expect((await r.get("/v1/info")).status()).toBe(401);
  expect((await request.get("/api/meta")).ok()).toBe(true);
  expect((await (await request.get("/api/meta")).json()).sandbox).toBeUndefined();
  await apiUser(request, "sysenvmeta");
  const p = await apiProject(request, { name: "SysEnvMeta" });
  const env = await (await request.get(`/api/projects/${p}/environment`)).json();
  expect(env.sandbox).toEqual({ engine: "none", egress_enforced: false });
});

test("setup with secret, then agent without it; every egress decision is logged", async ({ request }) => {
  let hits = 0;
  const upstream = createServer((_, res) => {
    hits++;
    res.end("ok");
  });
  await new Promise<void>((done) => upstream.listen(0, "127.0.0.1", done));
  const port = (upstream.address() as AddressInfo).port;
  try {
    await apiUser(request, "sysenv");
    const p = await apiProject(request, { name: "SysEnv" });
    await request.patch(`/api/projects/${p}`, { data: { review_enabled: false } });
    const env = await request.put(`/api/projects/${p}/environment`, {
      data: {
        setup_script: `echo "token=$REG_TOKEN"\ncurl -s --noproxy '' -o /dev/null http://127.0.0.1:${port}/pkg\ncurl -s -o /dev/null https://not-listed.invalid/ || true`,
        setup_timeout_secs: 60,
        env_vars: [{ name: "APP_MODE", value: "test" }],
        setup_network: { mode: "allowlist", presets: [], domains: [`127.0.0.1:${port}`], read_only: false },
        agent_network: { mode: "off", presets: [], domains: [], read_only: false },
        cache_enabled: true,
        cache_files: [],
      },
    });
    expect(env.status(), await env.text()).toBe(200);
    expect((await request.put(`/api/projects/${p}/secrets/REG_TOKEN`, { data: { value: SECRET } })).status()).toBe(200);
    const t = (await (await request.post("/api/tasks", { data: { project_id: p, prompt: "MOCK_ENV MOCK_NET:https://exfil.invalid/leak" } })).json()).id;
    const d = await waitTask(request, t, "done");
    const ev = d.events.map((e) => `${e.kind}|${e.content}`);
    expect(ev).toEqual(expect.arrayContaining([
      "setup|token=***",
      "egress|Blockiert (Setup): CONNECT not-listed.invalid:443 – nicht in der Allowlist",
      "egress|Blockiert (Agent): CONNECT exfil.invalid:443 – Netz in dieser Phase aus",
    ]));
    expect(ev.some((e) => e.startsWith(`egress|Netzwerk (Setup): 127.0.0.1:${port} ×1 (Allowlist)`))).toBe(true);
    expect(JSON.stringify(d)).not.toContain(SECRET);
    expect(hits).toBe(1);
    // The agent's environment (names only) has the variable, not the secret.
    const f = await (await request.get(`/api/projects/${p}/file?path=MOCK_ENV.txt`)).json();
    expect(f.content).toContain("APP_MODE");
    expect(f.content).not.toContain("REG_TOKEN");
  } finally {
    upstream.close();
  }
});
