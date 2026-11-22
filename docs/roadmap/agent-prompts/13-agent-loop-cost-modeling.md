# Auftrag 13: Agent loop cost modeling

- Gestartet: 2026-11-07 19:45 UTC
- Agent-Typ: `general-purpose`, Hintergrund: False

## Prompt (wörtlich)

````text
Research the real token consumption and cost structure of autonomous coding-agent tasks, and how batch APIs, caching and model routing affect cost, as of November 2026.

Context: Project "Queue" is a self-hosted agentic coding task queue. Each task is a multi-turn agent loop (read files, run tools/tests, edit) plus a separate review step on the diff, a planning step, and idle-time "OnEmptyQueue" analysis runs that propose new tasks. Tasks are NOT time-critical (overnight is fine). Goal: find the cheapest setup without losing quality.

Objective: Quantitative basis for cost calculations with worked examples (e.g. small/medium/large task) and clear guidance which steps can use batch APIs or cheap models.

Key questions:
- Measured token usage of coding agents per task (input, output, cached input share, number of turns) from public data: e.g. SWE-bench agent trajectories, Claude Code /cost or ccusage reports, OpenHands/Aider cost reports, published "cost per resolved issue" numbers. What fraction of input is cache reads in practice?
- How prompt caching changes cost in long agent loops (cache write premium vs read discount, TTL 5 min vs 1 h, provider differences: Anthropic explicit, OpenAI/Gemini implicit).
- Batch APIs (Anthropic Message Batches, OpenAI Batch, Gemini Batch, Mistral Batch): can they be used for multi-turn tool-use loops at all (each turn = separate batch job, latency per turn minutes to 24h)? Which pipeline steps fit batch well (planning, review, OnEmptyQueue analysis, test generation, summarization)? Any published patterns of "batch agents" or "async agent loops via batch"? Does caching still apply in batch?
- Planner/executor split and model routing: evidence that strong planner + cheap executor (e.g. Opus+Haiku, architect/editor mode in Aider) keeps quality; reported cost savings and failure modes; how cheap models (Haiku, GPT Luna/Terra, Gemini Flash, Mistral Small/Devstral, DeepSeek/Qwen coder) perform on SWE-bench-like tasks.
- Cost of failures: retries, fix loops, review rejections – how they change expected cost per successful task.
- Subscription vs API: at which monthly volume is a flat subscription (Claude Max, ChatGPT Pro, Gemini Ultra) cheaper than API pay-per-token for agent loops?

Suggested sources:
- Anthropic/OpenAI/Google docs on caching and batch, engineering blogs, Aider leaderboards (cost column), SWE-bench/OpenHands cost reports, ccusage, Artificial Analysis, practitioner write-ups (Simon Willison, HN) 2025–2026

Constraints:
- State as of November 2026; give concrete numbers and mark estimates/assumptions explicitly.

Save your output notes to /home/user/QueueRepo/research_notes/Kostenoptimale Modellanbieter für Queue/agent_loop_costs.md

**As a first step, you must read <deep-research-skill-dir>/references/researcher.md for instructions on how to conduct research.**
````
