# Auftrag 01: Competitor feature landscape

- Gestartet: 2026-11-06 20:06 UTC
- Agent-Typ: `general-purpose`, Hintergrund: False

## Prompt (wörtlich)

````text
Research the competitive landscape of agentic / asynchronous "background" coding agent products and task-queue style tools, as of late 2026.

Context (the product being evaluated): A self-hosted web app/PWA ("Queue") where users enqueue coding tasks; each task is executed by a fresh CLI agent (Claude Code, OpenAI Codex CLI, Gemini CLI, using the user's own subscription) in a rootless sandbox; results are verified (verify command + review agent), committed to the project's git repo, then the next task runs. When the queue is empty, "OnEmptyQueue" actions (code review, web research, improvement proposals) can refill it. Multi-user/multi-tenant, per-user queue, token budgets per provider (5h/7d windows), push notifications, iPhone PWA.

Objective: Build a dense feature matrix of the leading products so we can identify which features are table stakes for a top-tier product and which differentiate.

Key questions:
- Which products exist in this space and what are their core feature sets? Cover at least: OpenAI Codex (cloud), GitHub Copilot coding agent, Claude Code on the web / Claude Code GitHub Actions / Claude Agent SDK-based offerings, Google Jules, Cursor background agents, Devin (Cognition), Factory (Droids), Amp (Sourcegraph), OpenHands (All Hands), Cline/Roo Code, Augment Code remote agents, Warp, Sculptor (Imbue), Conductor, Vibe Kanban, Claude Squad, Terragon / similar orchestrators, Sweep, Tembo, and any newer 2025–2026 entrants.
- For each: task intake (issue, chat, Slack, Linear/Jira, API), parallelism (worktrees, multiple agents per repo), environment setup (setup scripts, caching, snapshots, devcontainers), output (PR, branch, diff), review loop (PR comments → agent iterates), planning/spec mode, memory/rules files (AGENTS.md etc.), scheduled/recurring tasks, multi-model support, pricing/usage limits handling, mobile apps, notifications, team/enterprise features.
- Which features do almost all of them have (table stakes) vs. only a few (differentiators)?
- What do users complain about or praise most (reviews, HN/Reddit discussions, blog posts)?

Suggested sources:
- Official product docs and changelogs (openai.com, docs.github.com, docs.anthropic.com / code.claude.com, jules.google, cursor.com/docs, docs.devin.ai, factory.ai, ampcode.com, docs.all-hands.dev, etc.)
- Comparison blog posts and reviews from 2025–2026, Hacker News and Reddit threads
- GitHub repos of open-source orchestrators (Vibe Kanban, Claude Squad, OpenHands, Conductor)

Constraints:
- Focus on the current state as of late 2026; note when information may be outdated.
- Prefer primary sources; mark claims from secondary sources.

Save your output notes to /home/user/QueueRepo/research_notes/Funktionsumfang agentischer Coding Queues/competitor_landscape.md

**As a first step, you must read <deep-research-skill-dir>/references/researcher.md for instructions on how to conduct research.**
````
