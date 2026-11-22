# Auftrag 02: Agent orchestration best practices

- Gestartet: 2026-11-06 20:06 UTC
- Agent-Typ: `general-purpose`, Hintergrund: False

## Prompt (wörtlich)

````text
Research best practices for orchestrating autonomous coding agents for reliability, quality and token/cost efficiency, as of late 2026.

Context (the product being evaluated): A self-hosted task queue where each coding task is executed by a fresh CLI agent (Claude Code, Codex CLI, Gemini CLI) in a sandbox. Context for a task = task prompt + short "handoff" summary (≤4 KB) from the predecessor task + project goal; agent writes a JSON result contract (status, summary, handoff, questions, follow-up tasks). Definition of done = verify command (e.g. tests) + separate review agent on the diff. Failures spawn up to 2 "fix" tasks with root-cause prompts, then the task is blocked. Serial by default, parallelism only across projects (one task per project because of a shared working tree). Rolling 5h/7d token budgets per provider; subscription rate limits are detected from CLI output and pause the provider. When the queue is empty, maintenance runs propose new tasks (>2 need user approval).

Objective: Identify what a top-tier system does beyond this, with concrete techniques, numbers and sources, so the product owner can decide what to build next.

Key questions:
- Context engineering: project memory/rules files (AGENTS.md, CLAUDE.md), repo maps, retrieval, persistent knowledge bases, how to keep handoffs useful; prompt caching strategies and their measured savings.
- Task planning & decomposition: planning/spec modes, task DAGs/dependencies, splitting large tasks, estimating size/cost before running, model routing (cheap vs. strong models per step), "best-of-N" / parallel attempts and picking a winner.
- Verification: test generation, evals, LLM-as-judge reliability and pitfalls, guarding against agents gaming tests, flaky tests, CI integration, sandbox snapshots/caching of dependencies for speed.
- Parallelism in the same repo: git worktrees, branch-per-task, merge conflict handling, merge queues.
- Failure handling: loop detection, max turns/time/cost per task, stuck-agent detection, retries, checkpoints/resume.
- Cost and usage control: per-task token caps, predictive budgeting, how subscription limits of Claude (Pro/Max), ChatGPT (Plus/Pro) and Gemini plans work in 2026 (windows, weekly caps), and what tooling exists to track them (e.g. ccusage-style tools, rate-limit headers/events).
- Observability for agents: tracing (OpenTelemetry GenAI conventions), metrics (success rate, cost per merged task, time to merge), dashboards.
- Human-in-the-loop patterns: approval gates, plan approval, interrupting/steering a running agent.

Suggested sources:
- Anthropic engineering blog (e.g. "Building effective agents", context engineering, Claude Code best practices), OpenAI Codex docs/blog, Google Jules docs, Cognition/Devin blog, Factory, Sourcegraph Amp blog
- Research papers and benchmarks (SWE-bench, SWE-bench Verified/Pro, agent reliability studies), OpenTelemetry GenAI semantic conventions
- Practitioner write-ups (Simon Willison, Latent Space, HN) from 2025–2026

Constraints:
- Current state as of late 2026; mark older info as such. Give concrete numbers where available.

Save your output notes to /home/user/QueueRepo/research_notes/Funktionsumfang agentischer Coding Queues/orchestration_reliability_cost.md

**As a first step, you must read <deep-research-skill-dir>/references/researcher.md for instructions on how to conduct research.**
````
