# Auftrag 04: UX integrations and operations

- Gestartet: 2026-11-06 20:07 UTC
- Agent-Typ: `general-purpose`, Hintergrund: False

## Prompt (wörtlich)

````text
Research user experience, integrations and operational features expected from a top-tier agentic coding task-queue product, as of late 2026.

Context (the product being evaluated): Self-hosted web app/PWA ("Queue", German UI) where users type coding tasks into a composer; tasks queue per user and are executed one after another by CLI agents (Claude Code, Codex CLI, Gemini CLI) in sandboxes. UI today: queue list (running / needs-you / queued with up/down/top reorder / history), task drawer with live event log, summary, handoff, review result, diff, revert; inbox with notifications; projects with file browser, git log, ZIP import/export, members; settings for parallelism, OnEmptyQueue (review/research/improve), budgets and provider credentials. Notifications via in-app SSE, Web Push and ntfy. Git per project with optional auto-push to a branch, but no pull-request workflow. No integrations with GitHub issues, Linear, Jira, Slack. Mobile: iPhone PWA with safe-area support and bottom tab bar. Deployment: systemd + rootless Podman on a single VPS; Postgres; no backups, metrics or admin console yet.

Objective: Identify UX, integration and operations features a best-in-class product offers, with examples and sources, so we can find what the product owner has not thought of.

Key questions:
- Task intake & steering: templates/recipes, voice input, attaching images/screenshots/files, @-mentions of files, task dependencies, scheduling (cron/recurring tasks, run at night when quota resets), priorities/labels, bulk actions, interrupting and steering a running agent with follow-up messages, plan review before execution.
- Review UX: PR creation and PR-comment-driven iterations, inline diff comments that become follow-up tasks, side-by-side comparison of parallel attempts, preview deployments/live app preview, screenshots of UI changes, test reports.
- Integrations: GitHub/GitLab/Bitbucket apps (issues → tasks, PRs, checks), Linear/Jira, Slack/Teams/Discord bots, MCP servers configuration per project, webhooks/API/CLI for the queue, IDE extensions.
- Mobile & notifications: what mobile apps (Codex iOS, Claude iOS, Cursor, Devin, Jules) offer for background agents; notification granularity, digests, quiet hours, Live Activities on iOS.
- Dashboards & analytics: usage/cost per project/user, success rates, time saved, quota forecasts.
- Onboarding & DX: first-run wizard, repo auto-setup detection (detect build/test commands, generate AGENTS.md), environment setup scripts and dependency caching, docs, keyboard shortcuts, accessibility (WCAG), i18n, dark mode.
- Operations for self-hosting: backups/restore (Postgres + workspaces), upgrades/migrations, health checks, metrics (Prometheus/OpenTelemetry), log management, horizontal scaling of runners (multiple runner hosts), admin console (user management, quotas, invitations), license/open-source considerations.

Suggested sources:
- Product docs and changelogs: OpenAI Codex (incl. iOS app), GitHub Copilot coding agent, Claude Code web/mobile, Google Jules, Cursor background agents & Slack integration, Devin (Slack, Linear), Factory, Linear agent integrations, Vibe Kanban, Conductor
- UX writeups and reviews 2025–2026, HN/Reddit discussions about what users want from background agents
- Self-hosting best practices (e.g. Plausible, Gitea, n8n self-hosting docs) for backups/upgrades/monitoring

Constraints:
- Current state as of late 2026; mark older info as such.

Save your output notes to /home/user/QueueRepo/research_notes/Funktionsumfang agentischer Coding Queues/ux_integrations_ops.md

**As a first step, you must read <deep-research-skill-dir>/references/researcher.md for instructions on how to conduct research.**
````
