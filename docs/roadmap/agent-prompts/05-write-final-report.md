# Auftrag 05: Write final report

- Gestartet: 2026-11-06 20:11 UTC
- Agent-Typ: `general-purpose`, Hintergrund: False

## Prompt (wörtlich)

````text
Read the notes in /home/user/QueueRepo/research_notes/Funktionsumfang agentischer Coding Queues/ and synthesize into a research report that answers: Welche Funktionen gehören zu einem Spitzenprodukt im Bereich agentischer Coding-Task-Queues (wie dem Produkt "Queue" des Nutzers)? Woran hat der Nutzer nicht gedacht? Was muss noch umgesetzt werden – priorisiert?

Write the report in German. The reader is an IT architect who wants concise, technically precise content without basics explained.

Current state of the user's product "Queue" (use this for the gap analysis; it is the baseline, not research findings):
- Self-hosted web app/PWA (React/Vite frontend, Rust/axum backend, Postgres). Per-user queue, multi-tenant; projects shareable with roles owner/editor/viewer; multiple projects per user.
- Auth: local accounts (Argon2) + OIDC (Google, Apple incl. client-secret JWT, GitHub, generic OIDC). No MFA/passkeys, no SAML/SCIM, no audit log, no admin console.
- Providers: Claude Code, Codex CLI, Gemini CLI via official CLIs with the user's own subscription credentials (claude setup-token, codex auth.json, gemini oauth_creds.json) or API keys, stored in the app DB encrypted with AES-256-GCM using a single MASTER_KEY; refreshed OAuth files are written back. Mock provider for tests.
- Execution: separate unprivileged service "queued-runner" (owns git workspaces, starts agents in rootless Podman or Docker-as-runner-uid with --cap-drop ALL, no-new-privileges, memory/cpu/pids limits). App has no host privileges and talks to the runner via HTTP + bearer token. Agents have unrestricted internet access. Git token passed via http.extraHeader for clone/pull/push by the runner.
- Task flow: fresh agent per task; context = task prompt + handoff (≤4 KB, free text) from predecessor + optional user context + project goal; JSON result contract in .queue/result.json (status, summary, handoff, questions, follow_up_tasks). Definition of done: verify command + separate review agent on the diff (review provider/model configurable). Failures spawn up to 2 fix tasks with root-cause prompt, then blocked + push notification. Rate limits detected by parsing CLI output → provider paused, task re-queued. Cancel of running tasks. Queue reorder, edit, retry, approve.
- Every project is a real git repo (init or clone, branch); one commit per task; revert via git revert (atomic on conflict); optional auto-push to a branch; git log in UI. No PR workflow, no branch-per-task, no worktrees.
- Parallelism: serial by default; configurable parallelism only across projects (one task per project).
- OnEmptyQueue: review/research/improve maintenance run when idle, cooldown (default once per 5h window), budget-aware; >2 generated tasks or depth >3 require approval.
- Token budgets per user/provider: rolling 5h and 7d token windows + 30d USD; scheduler pauses exhausted providers; one notification per exhaustion; "observed limit" recorded when a real subscription limit hits.
- Notifications: in-app SSE, Web Push (VAPID), ntfy. Inbox. Questions from agent answered inline.
- UI: composer, queue sections, task drawer with live event log, summary, review, diff, revert; projects with file browser, ZIP import/export, members, git settings; settings for parallelism, OnEmptyQueue, budgets, credentials; iPhone PWA with safe-area support (Dynamic Island) and bottom tab bar.
- Ops: systemd units (app hardened), install script, rootless Postgres via podman-compose, Cloudflare Tunnel option. No backups, no metrics/OTel, no health/readiness beyond runner /healthz, single runner host.
- Tests: Rust unit + integration (sqlx::test with in-process runner), vitest, Playwright system + E2E incl. iPhone; CI workflow. Real agent CLIs not yet tested live.
- Not present: task dependencies/DAG, scheduling/cron, templates, steering a running agent with follow-up messages, plan mode/plan approval, attachments/images, Slack/Linear/Jira/GitHub-issue intake, PR creation/PR-comment loop, MCP config, per-task egress allowlist, secret scanning, structured CLI flags (e.g. --json-schema, --max-budget-usd), OTel, analytics dashboard, dedupe of generated tasks, environment setup scripts/caching.

The report should: (1) summarize the market and which features are table stakes vs differentiators, (2) do an explicit gap analysis against the baseline above, (3) highlight what the user likely has not thought of (especially ToS/legal risk of storing subscription tokens, lethal trifecta/egress, reward hacking of tests, review overload, structured CLI signals), (4) give a prioritized roadmap (e.g. P0 must-fix before real use / P1 for parity / P2 differentiators) with concrete implementation hints tied to the existing architecture, and (5) state where evidence is weak (several vendor docs could not be fetched).

Earlier research in this conversation to build on: none

Save your final report to this exact path: /home/user/QueueRepo/reports/Funktionsumfang agentischer Coding Queues.md

**As a first step, you must read <deep-research-skill-dir>/references/report-writer.md for instructions on how to write your research report.**
````
