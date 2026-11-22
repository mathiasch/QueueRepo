# Auftrag 14: Harnesses runtimes self-hosting

- Gestartet: 2026-11-07 19:45 UTC
- Agent-Typ: `general-purpose`, Hintergrund: False

## Prompt (wörtlich)

````text
Research which agent harnesses, gateways, managed runtimes and self-hosting options can drive API models of arbitrary providers for an autonomous coding task queue, with costs, as of November 2026.

Context: Project "Queue" is a self-hosted agentic coding task queue (Rust backend; a separate unprivileged runner starts agent CLIs in rootless Podman containers, streams JSON events, enforces budgets). Today it supports Claude Code, Codex CLI and Gemini CLI with subscription logins or API keys. The user wants additional connectors so each user can choose provider/model, optimizing cost; tasks are not time-critical; users in CH/EU care about data protection (Swiss FADP/GDPR).

The user supplied these UNVERIFIED runtime prices – verify each against primary sources (confirmed / different / not found):
- Claude Managed Agents: $0.08 per session-hour (only while running), web search $10 per 1,000 searches.
- AWS Bedrock AgentCore: Runtime/Browser/Code Interpreter ~$0.0895 per vCPU-hour and $0.00945 per GB-hour, tokens billed separately.
- Google Vertex AI Agent Engine: $0.0864 per vCPU-hour, $0.0090 per GB-hour, free tier 50 vCPU-hours/month.

Objective: A comparison of integration options with capabilities, headless/streaming support, provider flexibility, cost and data-protection fit, so we can choose which connectors to build.

Key questions:
- Harnesses that run headless with structured (JSON/stream) output and can use non-native models: Claude Code / Claude Agent SDK (API key, Bedrock, Vertex, custom base URL/gateway), Codex CLI (API key, custom model providers, OSS mode), Gemini CLI (API key, Vertex), opencode, Aider, goose, OpenHands (headless/SDK), Mistral's coding agent (Vibe/Devstral), Crush, Qwen Code. For each: provider support, headless mode, structured events, tool sandboxing, cost tracking, licensing.
- Gateways: LiteLLM proxy, OpenRouter, Portkey, Cloudflare AI Gateway – features (unified API, cost tracking, budgets, fallbacks, caching pass-through), pricing/markup, EU data handling.
- Managed runtimes: Claude Managed Agents, Bedrock AgentCore, Vertex Agent Engine, OpenAI equivalent – what they provide vs. Queue's own sandbox, pricing, EU regions; when they are cheaper than a self-hosted VPS (e.g. Hetzner CPU prices).
- Self-hosting open-weight coding models (Mistral Medium 3.5 open weights, Devstral, Qwen3-Coder, DeepSeek, GLM, gpt-oss): quality on coding benchmarks, required GPU memory, serving with vLLM/SGLang/Ollama, hardware or GPU-rental cost (e.g. Hetzner GEX, Lambda, RunPod, Scaleway/OVH in EU) and break-even vs API prices.
- EU/CH data residency options: Bedrock eu-central-2 (Zurich), Vertex europe-west6, Azure EU Data Zone, Mistral La Plateforme (EU), Swiss providers (e.g. Infomaniak, Swisscom, Exoscale) offering LLM APIs.

Suggested sources:
- Official docs/pricing: docs.claude.com (Agent SDK, Managed Agents), aws.amazon.com/bedrock/agentcore/pricing, cloud.google.com/vertex-ai/pricing, github.com/openai/codex docs, opencode.ai, aider.chat, block.github.io/goose, docs.all-hands.dev, mistral.ai, litellm docs, openrouter.ai docs, Hetzner/Scaleway/OVH GPU pricing, vLLM docs, Hugging Face model cards
- Benchmarks: SWE-bench, Aider polyglot leaderboard, Terminal-Bench

Constraints:
- State as of November 2026; mark secondary/unverifiable info; CH/EU focus for hosting.

Save your output notes to /home/user/QueueRepo/research_notes/Kostenoptimale Modellanbieter für Queue/harnesses_runtimes_selfhosting.md

**As a first step, you must read <deep-research-skill-dir>/references/researcher.md for instructions on how to conduct research.**
````
