# Auftrag 12: Verify model API pricing

- Gestartet: 2026-11-07 19:45 UTC
- Agent-Typ: `general-purpose`, Hintergrund: False

## Prompt (wörtlich)

````text
Research and verify current LLM API pricing (as of November 2026) relevant for an autonomous coding-agent task queue, including caching, batch and EU-region pricing, and compare against subscription plans.

Context: Project "Queue" is a self-hosted agentic coding task queue (Rust backend). Agents currently run as CLIs (Claude Code, Codex CLI, Gemini CLI) with users' subscriptions. Tasks are NOT time-critical. Users are in Switzerland/EU. The user supplied the following UNVERIFIED claims that must be checked one by one against primary sources (mark each as confirmed / different (with the correct value) / not found):
- Anthropic: Opus 5.5 $4 input / $20 output per MTok, Sonnet 5.5 $2/$10, Haiku 5.5 $0.10/$0.50 (prompts up to 100K tokens), Batch API 50% off input and output.
- OpenAI: GPT-6 Astra $10/$50, GPT-6.1 Sol $2/$10, GPT-6 Luna $0.10/$0.50, GPT-6.1 Terra $1/$5.
- Google: Gemini 4 Argon $2/$10 until 30.11.2026 then $4/$20 from 1.12.2026; Gemini 3.x Flash $0.75/$3.75, doubling on 1.1.2027.
- Mistral: Large 4 $1.36/$4.18, Medium 3.5 $1.50/$7.50 (open weights), Small 4 $0.15/$0.60.

Objective: A verified price table (input, output, cache write, cache read, batch, long-context surcharges, tiered pricing) per model, plus subscription plan comparison.

Key questions:
- Exact current list prices incl. prompt-caching prices (write/read, TTL options) and batch discounts for Anthropic, OpenAI, Google (Gemini API and Vertex), Mistral; also notable low-cost coding-capable alternatives (e.g. DeepSeek, Qwen, Kimi, GLM via their APIs or OpenRouter) with prices.
- Does batch pricing combine with caching discounts? Batch latency/SLA (e.g. 24h) per provider.
- EU/CH data residency: are models available in EU regions on AWS Bedrock (e.g. eu-central-1, eu-central-2 Zurich), Google Vertex (europe-west6 Zurich), Azure OpenAI/Foundry EU Data Zone, and at what price premium (e.g. regional endpoints surcharge)? Mistral's EU hosting.
- Subscription plans in 2026: Claude Pro/Max (5x/20x) prices and usage limits, ChatGPT Plus/Pro (Codex included usage), Google AI Pro/Ultra (Gemini CLI quotas) – and published or credible estimates of equivalent API value per month.
- Coding benchmark positioning of each model (SWE-bench Verified/Pro, Terminal-Bench) to judge price/performance, especially cheap tiers (Haiku, Luna, Terra, Flash, Small, Devstral).

Suggested sources:
- Official pricing pages: anthropic.com/pricing, docs.claude.com / platform.claude.com pricing docs, openai.com/api/pricing, ai.google.dev/pricing, cloud.google.com/vertex-ai/pricing, mistral.ai/pricing, aws.amazon.com/bedrock/pricing, azure.microsoft.com pricing; OpenRouter model pages
- Official model announcement posts/benchmarks; independent leaderboards (SWE-bench, Artificial Analysis)

Constraints:
- State as of November 2026; prices in USD per million tokens; clearly mark anything from secondary sources or not verifiable.

Save your output notes to /home/user/QueueRepo/research_notes/Kostenoptimale Modellanbieter für Queue/model_pricing.md

**As a first step, you must read <deep-research-skill-dir>/references/researcher.md for instructions on how to conduct research.**
````
