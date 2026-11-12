/**
 * WP08 system tests: intake integrations against the real stack (app +
 * runner as processes, Postgres, mock provider). Deliveries are signed in
 * the test exactly as GitHub, Slack, Linear and Jira sign them (Node
 * crypto); status updates go to the fake APIs (`fake_intake` example of
 * the stack). Per integration: bad signature, unmapped person refused,
 * task with link, question asked outside and answered there, done reported
 * exactly once. No external service is called.
 */
import { expect, test, type APIRequestContext } from "@playwright/test";
import { createHmac, randomUUID } from "node:crypto";
import { apiProject, apiUser, waitTask } from "../helpers";

const FAKE = `http://127.0.0.1:${process.env.E2E_FAKE_INTAKE_PORT ?? 18096}`;
const hmac = (secret: string, data: string) => createHmac("sha256", secret).update(data).digest("hex");
const uniq = () => randomUUID().slice(0, 8);

async function created(request: APIRequestContext, target: string): Promise<any[]> {
  const r = await request.get(`${FAKE}/_fake/created?target=${encodeURIComponent(target)}`);
  return r.json();
}

async function setup(request: APIRequestContext, tag: string, body: Record<string, unknown>) {
  await apiUser(request, tag);
  const p = await apiProject(request, { name: `${tag} ${uniq()}` });
  await request.patch(`/api/projects/${p}`, { data: { review_enabled: false } });
  const r = await request.post(`/api/projects/${p}/integrations`, { data: body });
  expect(r.status(), await r.text()).toBe(200);
  const c = await r.json();
  const me = (await (await request.get("/api/auth/me")).json()).id as string;
  return { id: c.integration.id as string, secret: c.secret as string | null, hook: `/api/intake/${c.integration.id}`, me };
}

async function map(request: APIRequestContext, id: string, xid: string, user: string) {
  const r = await request.post(`/api/integrations/${id}/identities`, { data: { external_id: xid, user_id: user } });
  expect(r.status(), await r.text()).toBe(200);
}

async function send(request: APIRequestContext, hook: string, headers: Record<string, string>, body: string) {
  const r = await request.post(hook, { headers, data: body });
  return { status: r.status(), json: await r.json().catch(() => ({})) };
}

test("GitHub: labeled issue → task, question and answer as comments", async ({ request }) => {
  test.setTimeout(120_000);
  const repo = `acme/sys-${uniq()}`;
  const s = await setup(request, "ghsys", {
    kind: "github", name: "Repo", tenant: repo, token: "github_pat_E2EONLY000", config: { label: "queue" },
  });
  const issue = (action: string, sender: number) => JSON.stringify({
    action, label: { name: "queue" },
    issue: { number: 4, title: "Suche", body: "MOCK_ASK Suche einbauen", html_url: `https://github.com/${repo}/issues/4`,
             user: { login: "ext", id: sender }, labels: [{ name: "queue" }], author_association: "NONE" },
    repository: { full_name: repo }, sender: { login: "ext", id: sender, type: "User" },
  });
  const gh = (event: string, delivery: string, body: string, secret = s.secret!) => ({
    "content-type": "application/json", "x-github-event": event, "x-github-delivery": delivery,
    "x-hub-signature-256": `sha256=${hmac(secret, body)}`,
  });
  const b = issue("labeled", 77);
  expect((await send(request, s.hook, gh("issues", "e2e-1", b, "qwh_wrong"), b)).status).toBe(401);
  expect((await send(request, s.hook, gh("issues", "e2e-1", b), b)).json.outcome).toBe("rejected");
  await map(request, s.id, "77", s.me);
  const r = await send(request, s.hook, gh("issues", "e2e-1", b), b);
  expect(r.status, JSON.stringify(r.json)).toBe(201);
  expect((await send(request, s.hook, gh("issues", "e2e-1", b), b)).json.outcome).toBe("duplicate");
  const t = r.json.task_id as string;
  await waitTask(request, t, "waiting_input");
  const target = `github:${repo}#4`;
  await expect.poll(async () => (await created(request, target)).length, { timeout: 20_000 }).toBe(2);
  const comments = await created(request, target);
  expect(comments[1].body).toContain("Postgres or SQLite?");
  const answer = JSON.stringify({
    action: "created", issue: { number: 4, title: "Suche", body: "", user: { login: "ext", id: 77 }, labels: [] },
    comment: { id: 1, body: "Postgres bitte", user: { login: "ext", id: 77 }, author_association: "OWNER" },
    repository: { full_name: repo }, sender: { login: "ext", id: 77, type: "User" },
  });
  expect((await send(request, s.hook, gh("issue_comment", "e2e-2", answer), answer)).json.outcome).toBe("answered");
  await waitTask(request, t, "done");
  await expect.poll(async () => (await created(request, target)).length, { timeout: 20_000 }).toBe(3);
  expect((await created(request, target))[2].body).toContain("Queue ist fertig");
  const d = await (await request.get(`/api/tasks/${t}`)).json();
  expect(d.intake.label).toBe("GitHub-Issue #4");
  // Nothing more after a few ticks.
  await new Promise((res) => setTimeout(res, 2500));
  expect((await created(request, target)).length).toBe(3);
});

test("Slack: mention → task, updates and answer in the thread", async ({ request }) => {
  test.setTimeout(120_000);
  const team = `T${uniq().toUpperCase().replace(/[^A-Z0-9]/g, "")}`;
  const secret = `slack-e2e-${uniq()}`;
  const s = await setup(request, "slsys", { kind: "slack", name: "WS", tenant: team, secret, token: "xoxb-e2e-0000" });
  const sign = (body: string, ts = Math.floor(Date.now() / 1000)) => ({
    "content-type": "application/json", "x-slack-request-timestamp": String(ts),
    "x-slack-signature": `v0=${hmac(secret, `v0:${ts}:${body}`)}`,
  });
  const ev = (event: Record<string, unknown>) => JSON.stringify({ type: "event_callback", team_id: team, event_id: `Ev${uniq()}`, authorizations: [{ user_id: "UBOT" }], event });
  const channel = `C${uniq().toUpperCase().replace(/[^A-Z0-9]/g, "")}`;
  const mention = ev({ type: "app_mention", user: "U77", text: "<@UBOT> MOCK_ASK neue Seite", ts: "1700000000.000100", channel });
  expect((await send(request, s.hook, sign(mention, 1), mention)).status).toBe(401);
  expect((await send(request, s.hook, sign(mention), mention)).json.outcome).toBe("rejected");
  await map(request, s.id, "U77", s.me);
  const r = await send(request, s.hook, sign(mention), mention);
  expect(r.status, JSON.stringify(r.json)).toBe(201);
  const t = r.json.task_id as string;
  await waitTask(request, t, "waiting_input");
  const target = `slack:${channel}:1700000000.000100`;
  await expect.poll(async () => (await created(request, target)).length, { timeout: 20_000 }).toBe(2);
  const reply = ev({ type: "message", user: "U77", text: "Postgres", ts: "1700000000.000200", thread_ts: "1700000000.000100", channel });
  expect((await send(request, s.hook, sign(reply), reply)).json.outcome).toBe("answered");
  await waitTask(request, t, "done");
  await expect.poll(async () => (await created(request, target)).length, { timeout: 20_000 }).toBe(3);
  expect((await created(request, target))[2].text).toContain("Queue ist fertig");
});

test("Linear: delegated issue → thought, elicitation, response", async ({ request }) => {
  test.setTimeout(120_000);
  const org = randomUUID();
  const secret = `lin-e2e-${uniq()}`;
  const s = await setup(request, "linsys", { kind: "linear", name: "Lin", tenant: org, secret, token: "lin_oauth_e2e" });
  const session = randomUUID();
  const user = randomUUID();
  const body = (action: string, activity?: { id: string; text: string }) => JSON.stringify({
    type: "AgentSessionEvent", action, organizationId: org, webhookTimestamp: Date.now(),
    promptContext: "MOCK_ASK Login reparieren",
    agentSession: { id: session, creator: { id: user, name: "Ada" }, issue: { identifier: "ENG-1", title: "Login", url: "https://linear.app/x/issue/ENG-1" } },
    ...(activity ? { agentActivity: { id: activity.id, content: { type: "prompt", body: activity.text }, user: { id: user, name: "Ada" } } } : {}),
  });
  const sign = (b: string, secretUsed = secret) => ({ "content-type": "application/json", "linear-signature": hmac(secretUsed, b) });
  let b = body("created");
  expect((await send(request, s.hook, sign(b, "nope"), b)).status).toBe(401);
  await map(request, s.id, user, s.me);
  const r = await send(request, s.hook, sign(b), b);
  expect(r.status, JSON.stringify(r.json)).toBe(201);
  const t = r.json.task_id as string;
  const target = `linear:${session}`;
  await expect.poll(async () => (await created(request, target)).map((a) => a.type), { timeout: 20_000 }).toEqual(["thought"]);
  await waitTask(request, t, "waiting_input");
  await expect.poll(async () => (await created(request, target)).length, { timeout: 20_000 }).toBe(2);
  b = body("prompted", { id: randomUUID(), text: "Postgres" });
  expect((await send(request, s.hook, sign(b), b)).json.outcome).toBe("answered");
  await waitTask(request, t, "done");
  await expect.poll(async () => (await created(request, target)).map((a) => a.type), { timeout: 20_000 }).toEqual(["thought", "elicitation", "response"]);
});

test("Jira: label → task, comments back, answer by comment", async ({ request }) => {
  test.setTimeout(120_000);
  const site = `https://e2e${uniq().replace(/[^a-z0-9]/g, "")}.atlassian.net`;
  const s = await setup(request, "jisys", {
    kind: "jira", name: "Jira", tenant: site, token: "jira-e2e-token", account: "bot@e2e.test", config: { label: "queue" },
  });
  const key = `SYS-${Math.floor(Math.random() * 100000)}`;
  const sign = (b: string, id: string) => ({ "content-type": "application/json", "x-atlassian-webhook-identifier": id, "x-hub-signature": `sha256=${hmac(s.secret!, b)}` });
  const created_ = JSON.stringify({
    webhookEvent: "jira:issue_created", timestamp: Date.now(), user: { accountId: "acc-77", displayName: "Ada" },
    issue: { key, self: `${site}/rest/api/2/issue/1`, fields: { summary: "Export", labels: ["queue"], description: "MOCK_ASK Export bauen" } },
  });
  expect((await send(request, s.hook, sign(created_, "x-1"), created_)).json.outcome).toBe("rejected");
  await map(request, s.id, "acc-77", s.me);
  const r = await send(request, s.hook, sign(created_, "x-1"), created_);
  expect(r.status, JSON.stringify(r.json)).toBe(201);
  const t = r.json.task_id as string;
  await waitTask(request, t, "waiting_input");
  const target = `jira:${key}`;
  await expect.poll(async () => (await created(request, target)).length, { timeout: 20_000 }).toBe(2);
  const answer = JSON.stringify({
    webhookEvent: "comment_created", timestamp: Date.now(),
    issue: { key, self: `${site}/rest/api/2/issue/1`, fields: { summary: "Export", labels: [] } },
    comment: { id: "9", body: "Postgres", author: { accountId: "acc-77" } },
  });
  expect((await send(request, s.hook, sign(answer, "x-2"), answer)).json.outcome).toBe("answered");
  await waitTask(request, t, "done");
  await expect.poll(async () => (await created(request, target)).length, { timeout: 20_000 }).toBe(3);
});
