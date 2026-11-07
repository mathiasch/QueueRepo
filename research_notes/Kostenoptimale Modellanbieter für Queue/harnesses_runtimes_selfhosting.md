# Agent harnesses, gateways, managed runtimes and self-hosting for Queue (as of 2026-11-07)

Method note: Research was done on 2026-11-07. The session network blocked direct fetches of aws.amazon.com and docs.cloud.google.com, and a full fetch of cloud.google.com/vertex-ai/pricing failed (page too large). For those, figures come from web-search summaries of secondary sources and are marked [secondary]. The Anthropic pricing page (platform.claude.com) was fetched and read directly [primary]. Items marked [prior knowledge, unverified] come from the researcher's background knowledge (cutoff mid-2026) and were not re-checked against a source in this session.

---

## 0. Verification of the user-supplied runtime prices

| Claim | Verdict | Evidence |
|---|---|---|
| Claude Managed Agents: $0.08 per session-hour, billed only while running | **CONFIRMED** [primary] | "Session runtime: $0.08 USD per session-hour, metered on `running` status duration. Measured to the millisecond; `idle`, `rescheduling`, `terminated` do not count." — [Anthropic pricing](https://platform.claude.com/docs/en/about-claude/pricing) |
| Claude Managed Agents: web search $10 / 1,000 searches | **CONFIRMED** [primary] | "Web search triggered inside a session incurs the standard $10 USD per 1,000 searches." — [Anthropic pricing](https://platform.claude.com/docs/en/about-claude/pricing) |
| AWS Bedrock AgentCore Runtime/Browser/Code Interpreter ~$0.0895/vCPU-h and $0.00945/GB-h, tokens separate | **PARTLY OUTDATED / DIFFERENT** [secondary] | Those are the original **Runtime v1** rates (us-east-1). Several secondary sources say **Runtime v2** (microVM, GA 2026) is priced at **$0.1276/vCPU-h and $0.0169/GB-h**, plus a reported committed baseline of $0.0997/vCPU-h and $0.0132/GB-h (single source, unconfirmed). The official AWS page could not be fetched. — [cloudburn.io](https://cloudburn.io/blog/amazon-bedrock-agentcore-pricing); [factualminds v2 GA](https://www.factualminds.com/blog/amazon-bedrock-agentcore-runtime-v2-ga-2026/); [betterclaw Oct 2026](https://www.betterclaw.io/blog/aws-bedrock-agentcore-pricing-alternatives) |
| Vertex AI Agent Engine: $0.0864/vCPU-h, $0.0090/GB-h, free 50 vCPU-h/month | **CONFIRMED** [secondary, citing Google release notes] | Price cut from $0.0994 to $0.0864/vCPU-h and from $0.0105 to $0.0090/GiB-h effective 2025-12-16 (SKU 8A55-0B95-B7DC). Free tier: first 180,000 vCPU-seconds (50 h) and 360,000 GiB-seconds (100 GiB-h) per month. — [agentdeals.dev](https://agentdeals.dev/vendor/google-vertex-ai-agent-engine); [Google Agent Builder release notes (not fetchable here)](https://docs.cloud.google.com/agent-builder/release-notes) |

---

## 1. Harnesses that run headless with structured output and non-native models

### Takeaway
Claude Code (via `ANTHROPIC_BASE_URL` / Bedrock / Vertex), Codex CLI (custom `model_providers`, `--oss`), opencode, goose, Qwen Code and OpenHands all have a headless mode with JSON or JSONL output, so Queue can drive them the same way it drives Claude Code today. The two most useful *provider-agnostic* additions are **opencode** (MIT, 75+ providers, `run`/`serve`) and **goose** (Apache-2.0, `goose run --output-format json`). Two big changes in 2026 need checking: **Gemini CLI has been retired in favour of the closed-source Antigravity CLI (`agy`)**, and the **OpenHands CLI is marked as no longer actively maintained**.

### Cited Findings

**Claude Code / Claude Agent SDK**
- Headless: `claude -p` / `--print` with `--output-format json`; there is also a `--bare` mode for scripting/CI [secondary]. — [pinggy.io](https://pinggy.io/blog/best_open_source_cli_coding_agents/)
- Can run against a LiteLLM proxy by setting `ANTHROPIC_BASE_URL` to the proxy; LiteLLM serves the Anthropic `/v1/messages` format, so Claude Code can use non-Anthropic models behind it. — [LiteLLM docs: Claude Code](https://docs.litellm.ai/docs/proxy/client_setup/claude_code); [LiteLLM: Claude Code with non-Anthropic models](https://docs.litellm.ai/docs/tutorials/claude_non_anthropic_models)
- Pass the key with `ANTHROPIC_AUTH_TOKEN` (master/virtual key), or with `ANTHROPIC_CUSTOM_HEADERS` and `x-litellm-api-key`. Unset a stray `ANTHROPIC_API_KEY` first. — [morphllm Claude Code + LiteLLM](https://www.morphllm.com/claude-code-litellm); [LiteLLM BYOK tutorial](https://docs.litellm.ai/docs/tutorials/claude_code_byok)
- Claude Code turns tool search off when `ANTHROPIC_BASE_URL` is not a first-party Anthropic host, so MCP tool schemas get inlined. That costs more tokens unless tool search is re-enabled [secondary]. — [morphllm](https://www.morphllm.com/claude-code-litellm)
- Bedrock and Vertex are first-class backends for Claude models. Starting with Claude 4.5, Bedrock regional endpoints and Google multi-region/regional endpoints carry a **10% premium** over global endpoints. — [Anthropic pricing](https://platform.claude.com/docs/en/about-claude/pricing)

**Codex CLI**
- Custom providers are defined in `~/.codex/config.toml` as `[model_providers.<id>]` (base URL, wire API, auth, headers) and selected with `model_provider`. The reserved built-in IDs are `openai`, `ollama`, `lmstudio`, `amazon-bedrock`, `amazon-bedrock-runtime` and `gcp`. — [OpenAI Codex advanced config](https://developers.openai.com/codex/config-advanced); [codex.danielvaughan.com](https://codex.danielvaughan.com/2026/03/31/codex-cli-custom-model-providers/)
- `wire_api = "chat"` was removed in February 2026, so only `wire_api = "responses"` works. A provider or gateway must therefore expose an OpenAI **Responses API**-compatible endpoint. — [morphllm Codex provider config](https://www.morphllm.com/codex-provider-configuration)
- `--oss` uses the configured `oss_provider` (Ollama / LM Studio) for local models. — [knightli OSS mode guide](https://knightli.com/en/2026/06/18/codex-cli-oss-mode-local-model-guide/)
- A project-local `.codex/config.toml` **cannot** set `model_provider` or `model_providers`. These must go in the user config, which matters for how Queue injects config into containers. — [morphllm](https://www.morphllm.com/codex-provider-configuration)

**Gemini CLI → Antigravity CLI**
- Google decommissioned Gemini CLI and replaced it with **Antigravity CLI (`agy`)**, a closed-source Go binary. Free-tier access to Gemini CLI reportedly ended on **2026-06-18** [secondary]. — [The New Stack](https://thenewstack.io/gemini-cli-antigravity-replacement/); [Let's Data Science](https://letsdatascience.com/news/google-replaces-gemini-cli-with-antigravity-cli-574b2056)
- Antigravity supports a scripted mode and `ANTIGRAVITY_API_KEY` for scripts. Reports on first-run login conflict: one says it needs a browser OAuth flow, another describes a device-code flow. An importer migrates Gemini CLI config. — [DEV Community](https://dev.to/arindam_1729/antigravity-cli-a-hands-on-guide-to-googles-terminal-coding-agent-5bc7); [The New Stack](https://thenewstack.io/gemini-cli-antigravity-replacement/)
- Opinion: the change mainly hurts open-source users. — [FOSS Force](https://fossforce.com/?p=2487461)

**opencode**
- `opencode run` runs a prompt non-interactively, and `opencode serve` exposes a headless HTTP server. MIT licensed [secondary]. — [pinggy.io](https://pinggy.io/blog/best_open_source_cli_coding_agents/); [builderai.tools](https://builderai.tools/blog/terminal-coding-agents-opencode-crush-goose)
- opencode publishes per-model usage and cost data, for example for Mistral Medium 3.5. — [opencode.ai data](https://opencode.ai/data/mistral/mistral-medium-3-5.md)

**goose**
- Headless with `goose run --recipe <file>`. A recipe used headlessly must include a `prompt` field. `--output-format json` gives the complete JSON output after execution, and `--no-session` skips the session file. Apache-2.0. Governance reportedly moved to the Linux Foundation's AAIF [secondary]. — [goose headless tutorial](https://goose-docs.ai/docs/tutorials/headless-goose/); [goose running tasks](https://goose-docs.ai/docs/guides/running-tasks/); [pinggy.io](https://pinggy.io/blog/best_open_source_cli_coding_agents/)

**Aider**
- `aider --message "..."` applies the edits and exits, so it can be scripted. Aider supports 100+ providers through LiteLLM and can point at a LiteLLM proxy with `--openai-api-base`. — [Aider scripting](https://aider.chat/docs/scripting.html); [Aider options](https://aider.chat/docs/config/options.html); [LiteLLM proxy docs](https://docs.litellm.ai/docs/proxy_server)

**OpenHands**
- Headless mode always auto-approves actions. `--json` streams JSONL events, one object per line. — [OpenHands headless docs](https://docs.openhands.dev/openhands/usage/cli/headless)
- The command reference says the OpenHands CLI is **no longer actively maintained** and points to "Agent Canvas". Automation reportedly needs `--override-with-envs` for the CLI to pick up LLM environment variables [secondary]. — [OpenHands command reference](https://docs.openhands.dev/openhands/usage/cli/command-reference); [glukhov.org](https://www.glukhov.org/ai-devtools/openhands/)

**Qwen Code**
- Headless with `qwen -p` and `--output-format text|json|stream-json`. Apache-2.0 [secondary]. — [pinggy.io](https://pinggy.io/blog/best_open_source_cli_coding_agents/)

**Mistral Vibe**
- Apache-2.0. Headless mode is `-p`. No source describes its JSON output format [secondary]. — [pinggy.io](https://pinggy.io/blog/best_open_source_cli_coding_agents/)
- Mistral Medium 3.5 replaced Devstral 2 as the model behind Vibe. — [letsdatascience](https://letsdatascience.com/blog/mistral-medium-3-5-128b-open-weight-merged-model)

**Crush (Charm)**
- Licensed under FSL (each release converts to MIT after two years). No source confirms a JSON/headless output mode [secondary]. — [pinggy.io](https://pinggy.io/blog/best_open_source_cli_coding_agents/)

### Inferences
- Summary matrix. Cells are from the findings above unless marked (P) = prior knowledge, unverified.

| Harness | Providers | Headless | Structured events | Sandbox | Cost tracking | License |
|---|---|---|---|---|---|---|
| Claude Code / Agent SDK | Anthropic API, Bedrock, Vertex, any Anthropic-compatible gateway (LiteLLM) | `-p` | `json`, `stream-json` (P) | Own permission system; Queue's Podman container | `total_cost_usd` in result event (P); wrong for non-Anthropic models behind a gateway | Proprietary |
| Codex CLI | OpenAI, Bedrock, GCP, Ollama/LM Studio, any Responses-API endpoint | `codex exec` (P) | `--json` JSONL (P) | Built-in Landlock/seatbelt sandbox (P) | Token counts; no $ (P) | Apache-2.0 (P) |
| Antigravity CLI (ex-Gemini) | Google models only (P) | scripted mode | unknown | unknown | unknown | Closed source |
| opencode | 75+ providers via AI SDK/models.dev (P), local | `run`, `serve` | `--format json` (P), HTTP/SSE API (P) | None of its own; rely on Podman | Per-session cost (P) | MIT |
| goose | Many providers incl. Ollama, OpenRouter, LiteLLM (P) | `goose run` | `--output-format json` (stream-json also, P) | None of its own | Limited | Apache-2.0 |
| Aider | 100+ via LiteLLM | `--message` | No JSON event stream (P) | None | Reports $ per message (P) | Apache-2.0 (P) |
| OpenHands | LiteLLM (any) | headless | `--json` JSONL | Docker runtime (P) | Has LLM metrics (P) | MIT (P); CLI unmaintained |
| Qwen Code | OpenAI-compatible endpoints (P) | `-p` | `json`, `stream-json` | Gemini-CLI-derived (P) | Token stats (P) | Apache-2.0 |
| Mistral Vibe | Mistral API, local (P) | `-p` | not confirmed | — | — | Apache-2.0 |
| Crush | Many providers (P) | `crush run` (P) | not confirmed | — | — | FSL→MIT |

- Recommendation for Queue: build an **opencode** connector first (widest provider coverage, MIT, HTTP server mode fits a runner). Second, either **goose** or **Qwen Code**. Qwen Code's stream-json is Gemini-CLI-like, so the existing Gemini parser may be reusable. Queue already has Codex and Claude Code, so "arbitrary models" for those mostly needs config injection (Codex user-level `config.toml`, Claude Code `ANTHROPIC_BASE_URL`) plus a gateway that speaks Responses API and Anthropic Messages.
- Queue's budget enforcement should not trust harness-reported $ figures when the model is routed through a gateway. It should compute cost from token counts and its own price table, or read the gateway's spend logs.
- The existing Gemini CLI connector is at risk: the free/OAuth path reportedly ended in June 2026, and the successor is closed source. Re-check API-key mode and how long the open-source repo will be supported.

### Gaps
- Primary docs for opencode, Crush, Qwen Code, Vibe and Codex `exec --json` were not fetched, because of the network restrictions and the call budget. Their flags and event schemas need confirming against the repos.
- No confirmation of Crush or Mistral Vibe JSON event output.
- Whether Antigravity CLI accepts non-Google models or custom base URLs is unknown.

---

## 2. Gateways: LiteLLM proxy, OpenRouter, Portkey, Cloudflare AI Gateway

### Takeaway
For a self-hosted, data-protection-focused Queue, a **self-hosted LiteLLM proxy** is the best fit. It is free open source, has per-key/team/user budgets, and speaks both the Anthropic and OpenAI formats, so Claude Code and Codex can both use it. **OpenRouter** gives the widest model catalogue with one key. It charges a 5.5% credit fee (8% on Business), and has EU in-region routing (eu.openrouter.ai), but account and billing records may sit in the US. **Cloudflare AI Gateway** is free apart from a 5% fee on Unified Billing credits. **Portkey** is SaaS, from $49/month.

### Cited Findings
- **LiteLLM:** Free to self-host. Supports per-key, per-team and per-user budgets plus provider, model and tag budgets. Budgets **require a database**, and multi-instance setups use Redis for shared state. — [LiteLLM budgets docs](https://docs.litellm.ai/docs/proxy/users); [LiteLLM provider budget routing](https://docs.litellm.ai/docs/proxy/provider_budget_routing)
- LiteLLM enterprise pricing is not public. A $250/month figure seen online is an unconfirmed third-party estimate. — [truefoundry](https://www.truefoundry.com/blog/litellm-pricing-guide)
- LiteLLM serves `/v1/messages` (Anthropic format) for Claude Code. — [LiteLLM docs](https://docs.litellm.ai/docs/proxy/client_setup/claude_code)
- **OpenRouter:** BYOK usage above an included allowance costs 5% of the normal OpenRouter price for that model/provider. The FAQ states the allowance as $25,000/month of list-price inference; a third-party source says ~1M requests, so the sources conflict. — [OpenRouter FAQ](https://openrouter.ai/docs/faq); [maximem.ai](https://www.maximem.ai/blog/openrouter-pricing-2026)
- OpenRouter EU in-region routing via `eu.openrouter.ai` is self-serve on the Business plan (no contract, no minimum). Business has an 8% credit fee versus 5.5% on Standard. Account, billing and operational records may be stored or processed in the US. Some sources still describe EU routing as Enterprise-only. — [OpenRouter Sovereign AI guide](https://openrouter.ai/docs/guides/features/sovereign-ai); [multimodelchat](https://www.multimodelchat.com/blog/openrouter-in-region-routing-2026)
- **Portkey:** Free Developer tier (10k logs/month). Production tier is $49/month with 100k logs, then $9 per extra 100k. Enterprise adds SSO, VPC hosting and SOC2/HIPAA [secondary]. — [truefoundry Portkey guide](https://www.truefoundry.com/blog/portkey-pricing-guide)
- **Cloudflare AI Gateway:** Core features (caching, rate limiting, analytics) are free. The only direct fee is 5% on credits bought through Unified Billing. Guardrails are billed as Workers AI inference. — [Cloudflare AI Gateway pricing](https://developers.cloudflare.com/ai-gateway/reference/pricing/)
- Cloudflare spend limits are available to all users; per-user identity-driven budgets are in closed beta. — [Cloudflare blog](https://blog.cloudflare.com/ai-gateway-spend-limits)

### Inferences
- LiteLLM self-hosted in CH/EU keeps prompts on Queue's own infrastructure until they reach the chosen upstream. That makes it the cleanest option for FADP/GDPR: the only subprocessors are the model providers.
- OpenRouter is useful as one upstream behind LiteLLM, for long-tail models such as GLM, Kimi or DeepSeek through EU-hosted providers. It should not be the only path for regulated data.
- Prompt-caching pass-through matters for agent costs (cache reads are 0.05–0.1x input on Claude). Before relying on a gateway, check that it preserves `cache_control` (Anthropic) and the cache-related usage fields (P: LiteLLM does).

### Gaps
- No primary confirmation of Portkey's current pricing, or of its EU hosting region.
- Whether OpenRouter's fee figures (5.5% / 8%) apply as of November 2026 was not checked on its pricing page.
- Cloudflare AI Gateway's EU data localisation for logs (Data Localization Suite) was not researched.

---

## 3. Managed runtimes vs Queue's own sandbox

### Takeaway
Managed runtimes charge **$0.08–$0.36 per active hour** on top of tokens. For non-time-critical coding tasks this is small next to token costs (for example $0.08 vs ~$0.50 of tokens in Anthropic's own worked example). It is still more than a Hetzner VPS once several tasks run concurrently. Their main value is offloading isolation and ops. Queue already has rootless Podman, so the managed runtimes are mostly redundant except as an optional "no runner" mode. **Claude Managed Agents has no EU data residency option**: only `global`, or `us` at 1.1x. It also cannot be used through Bedrock or Vertex.

### Cited Findings
- **Claude Managed Agents:** billed on tokens plus session runtime ($0.08/session-hour, running state only, per millisecond). Session runtime replaces container-hour billing. Web search costs $10/1,000. Batch discount and partner-cloud pricing **do not apply** (it is not available on partner-operated clouds). The `inference_geo: "us"` 1.1x multiplier does apply. — [Anthropic pricing](https://platform.claude.com/docs/en/about-claude/pricing)
  - Correction to secondary blogs: some say "data residency pricing doesn't apply" to Managed Agents. The official page says the 1.1x US multiplier *does* apply. — [Anthropic pricing](https://platform.claude.com/docs/en/about-claude/pricing) vs [truefoundry](https://www.truefoundry.com/blog/claude-managed-agents-pricing)
  - Anthropic's worked example: a 1-hour Opus 5.5 session with 50k input and 15k output tokens costs $0.20 + $0.30 tokens + $0.08 runtime = **$0.58**. With 40k cached input it costs $0.428. — [Anthropic pricing](https://platform.claude.com/docs/en/about-claude/pricing)
- First-party Claude API data residency: `inference_geo` supports `global` (default) and `us` (1.1x) only. No EU option is listed. — [Anthropic pricing](https://platform.claude.com/docs/en/about-claude/pricing)
- The Claude API code execution tool (outside Managed Agents) includes 1,550 free hours/org/month, then $0.05/hour per container with a 5-minute minimum. It is free when used with web_search/web_fetch 20260209+. — [Anthropic pricing](https://platform.claude.com/docs/en/about-claude/pricing)
- **AWS Bedrock AgentCore:** v1 Runtime is $0.0895/vCPU-h and $0.00945/GB-h (us-east-1), billed on active use; I/O wait during LLM and tool calls is not billed as CPU. v2 is reported at $0.1276/vCPU-h and $0.0169/GB-h with scale-to-zero [secondary]. — [cloudburn.io](https://cloudburn.io/blog/amazon-bedrock-agentcore-pricing); [factualminds](https://www.factualminds.com/blog/amazon-bedrock-agentcore-runtime-v2-ga-2026/); [AWS Builder Center](https://builder.aws.com/content/3KC60kJzFVDfHVxJ5CNecLZVPmA/amazon-bedrock-agentcore-pricing-hidden-costs-that-bill-while-your-agent-idles)
  - One Japanese field report shows **$5.63 billed for 29 minutes** on an AgentCore harness, a warning that the many meters can add up. — [classmethod](https://dev.classmethod.jp/articles/bedrock-agentcore-harness-cost-analysis/)
  - AgentCore has about 12 separately billed components (Runtime, Gateway, Memory, Identity, Observability, Browser, Code Interpreter, Policy, …) [secondary]. — [cloudburn.io](https://cloudburn.io/blog/amazon-bedrock-agentcore-pricing)
- **Vertex AI Agent Engine:** $0.0864/vCPU-h and $0.0090/GiB-h; free 50 vCPU-h and 100 GiB-h per month. Code Execution moved from free to the same rates (change date 2026-01-28). Sessions and Memory Bank cost $0.25 per 1,000 events; sources disagree on whether billing started 2026-01-28 or 2026-02-11 [secondary]. — [agentdeals.dev](https://agentdeals.dev/vendor/google-vertex-ai-agent-engine)
- **OpenAI equivalent (Agents API + hosted sandboxes, public beta, reportedly September 2026):** containers cost **$0.03 (1 GB), $0.12 (4 GB), $0.48 (16 GB), $1.92 (64 GB) per 20-minute session**, metered per minute with a 5-minute minimum. That works out to about $0.09/h (1 GB) and $0.36/h (4 GB). There is no platform fee for the Agents API itself; vCPU per tier is unpublished. — [OpenAI pricing](https://developers.openai.com/api/docs/pricing); [hermes-agent review](https://hermes-agent.ai/blog/openai-agents-api-review); [tokencost.app](https://tokencost.app/blog/openai-agents-api-hosted-sandbox-pricing); [OpenAI community announcement](https://community.openai.com/t/introducing-the-agents-api-and-hosted-sandboxes/1396481)

### Inferences
- Cost per active task-hour for a 2 vCPU / 4 GB sandbox (researcher's arithmetic):
  - Claude Managed Agents: $0.08 flat.
  - Vertex Agent Engine: 2×0.0864 + 4×0.0090 = **$0.209/h**. The free tier covers about 25 such hours per month.
  - AgentCore v1: 2×0.0895 + 4×0.00945 = **$0.217/h** at 100% CPU. It is much lower in practice, since I/O wait (time spent waiting on the LLM) isn't billed for CPU, although memory is billed throughout.
  - AgentCore v2 (reported): 2×0.1276 + 4×0.0169 = **$0.323/h** at full CPU.
  - OpenAI hosted sandbox, 4 GB: **~$0.36/h**.
- Example: Queue runs 300 tasks/month at about 1 active hour each. The runtime fee is about $24 (Claude MA), about $63 (Vertex, minus the free tier) or about $108 (OpenAI 4 GB). A Hetzner cloud VPS with 4–8 vCPU / 8–16 GB costs on the order of €10–€30/month (P: Hetzner CX/CPX/CCX list prices not verified this session) and can run several Podman tasks in parallel. It wins clearly from roughly 100+ task-hours/month, and keeps the code in CH/EU (Hetzner Germany/Finland; no Hetzner CH region, P).
- Managed runtimes win only for very low volume (Vertex free tier), for bursty parallelism beyond one VPS, or when a provider-native feature is needed: Claude MA's built-in harness, memory and web search.
- Data protection: Claude Managed Agents runs only on Anthropic's first-party platform with global or US routing, so it fits CH/EU data-residency requirements poorly. AgentCore and Agent Engine can run in EU regions, but EU-specific availability (Frankfurt, Zurich) was not confirmed in this session.

### Gaps
- The official AWS AgentCore pricing page and regional availability (eu-central-1, eu-central-2) could not be fetched. The v2 rates are secondary-only.
- The official Vertex Agent Engine pricing page was not readable, and regional availability in europe-west6 (Zurich) was not confirmed.
- No information on whether Codex Cloud tasks have separate pricing.
- Current Hetzner cloud VPS prices were not fetched.

---

## 4. Self-hosting open-weight coding models

### Takeaway
Self-hosting a frontier-class open model (Mistral Medium 3.5 128B dense, GLM-5.x, DeepSeek V4) needs at least one 96 GB card, quantised, or 2–4 80 GB GPUs. In the EU that costs about **€889/month** (Hetzner GEX131, RTX PRO 6000 96 GB) or **€2.7–3.2/h per H100** at Scaleway. Hosted open-weight APIs are extremely cheap: DeepSeek V4 Flash costs $0.14/$0.28 per MTok, Qwen3-Coder-Next $0.11/$0.80, Mistral Medium 3.5 $1.50/$7.50. So self-hosting only pays off at sustained high utilisation, or when data must never leave Queue's own hardware.

### Cited Findings
- **Mistral Medium 3.5:** 128B dense, open weights on Hugging Face under a *modified MIT* licence (companies above $20M monthly revenue must use Mistral's paid channel). 77.6% on SWE-bench Verified (self-reported), 256K context. Released around 2026-04-29/30. It supersedes Devstral; Devstral 2 is reportedly 72.2% on SWE-bench Verified. — [Hugging Face model card](https://huggingface.co/mistralai/Mistral-Medium-3.5-128B); [letsdatascience](https://letsdatascience.com/blog/mistral-medium-3-5-128b-open-weight-merged-model); [aimadetools](https://www.aimadetools.com/blog/mistral-medium-3-5-complete-guide/)
- Mistral Medium 3.5 memory: Mistral says it runs on "as few as four GPUs". Community estimates are ~70–74 GB VRAM at Q4, ~128 GB at FP8, and quants from Q2_K 46.6 GB to Q8_0 133 GB; the Ollama build is ~80 GB. There is no official memory table. — [letsdatascience](https://letsdatascience.com/blog/mistral-medium-3-5-128b-open-weight-merged-model); [Ollama library](https://ollama.com/library/mistral-medium-3.5); [nerdleveltech](https://nerdleveltech.com/mistral-medium-3-5-open-weight-128b-frontier-coder)
- Mistral Medium 3.5 API price: $1.50/MTok input, $7.50/MTok output, $0.15 cached input, batch $0.75/$3.75 [secondary trackers]. — [conductatlas (from model card)](https://conductatlas.com/platform/mistral-ai/mistral-medium-35-model-card/api-token-based-pricing/); [pricepertoken](https://pricepertoken.com/pricing-page/model/mistral-ai-mistral-medium-3.5)
- **GLM-5.2:** 62.1% on SWE-bench Pro and 81.0% on Terminal-Bench 2.1, MIT licence, described as the strongest open-weight model for long-running agents [secondary]. — [kingy.ai](https://kingy.ai/news/best-open-weight-ai-models-in-2026-glm-5-2-vs-deepseek-v4-vs-kimi-k2-6-vs-qwen-vs-mistral/)
- **DeepSeek V4-Pro:** 80.6% on SWE-bench Verified, about 55% on SWE-bench Pro. NIST CAISI puts it about 8 months behind the proprietary frontier [secondary]. V4 Flash official price (as of 2026-06-21): $0.14/MTok input (cache miss), $0.0028 (cache hit), $0.28 output. V4 Pro price is reported inconsistently ($0.435/$0.87 or ~$0.50/$2.00). — [morphllm](https://www.morphllm.com/best-open-source-coding-model-2026); [faros.ai](https://www.faros.ai/blog/open-weight-models)
- **Qwen3-Coder:** ~67–70.6% on SWE-bench Verified depending on variant and scaffold. Qwen3-Coder-Next (80B total, 3B active) runs on consumer hardware; $0.11/$0.80 per MTok on Alibaba Cloud [secondary]. — [morphllm](https://www.morphllm.com/best-open-source-coding-model-2026)
- **gpt-oss-120B:** weaker at coding, Apache-2.0, 131K context, $0.15/$0.60 per MTok on Fireworks [secondary]. — [fireworks.ai](https://fireworks.ai/blog/best-llms-for-coding)
- Reference proprietary price points: Claude Sonnet 5.5 $2/$10 per MTok (cache hit $0.10), Opus 5.5 $4/$20, Haiku 5.5 $0.10/$0.50 (≤100k prompt), with a 50% batch discount. — [Anthropic pricing](https://platform.claude.com/docs/en/about-claude/pricing)
- **GPU prices (EU):**
  - Hetzner GEX131 (RTX PRO 6000 Blackwell Max-Q, 96 GB): about **€889/month** ($989), with no interruptible tier. An older December 2025 listing was €1,057.91/month [secondary]. — [wz-it.com Oct 2026](https://wz-it.com/en/blog/hetzner-gpu-server-comparison/); [bex.co](https://bex.co/blog/2026/07/27/hetzner-gex131-gpu-blackwell-vs-aws-g5xlarge); [Hetzner GPU matrix](https://www.hetzner.com/dedicated-rootserver/matrix-gpu/)
  - Scaleway (Paris/Warsaw): H100 PCIe 80 GB at €2.73–€2.87/h, H100 SXM about €3.17/GPU-h, L40S from €1.47/h, L4 €0.79/h. Hourly billing, 1-hour minimum. — [Scaleway L40S page](https://www.scaleway.com/en/l40s-gpu-instance/); [Qovery](https://www.qovery.com/blog/gpu-pricing-comparison-method-aws-gcp-azure-scaleway-2026)
  - RunPod H100 from $2.89/h (unverified secondary). — [Qovery](https://qovery.com/blog/best-european-gpu-clouds-self-hosting-llms-2026)
  - Exoscale (CH) GPU about CHF 2.15/h, billed per second; 4 GPUs 24/7 is about CHF 6,200/month [secondary]. — [devs-group.ch](https://devs-group.ch/en/blog/sovereign-ai-inference-switzerland/)

### Inferences
- **Break-even sketch (researcher's estimate, not sourced):**
  - Assume a typical agentic task at Mistral Medium 3.5 API prices: 2M input tokens (80% cache hits), 30k output → 0.4M×$1.50 + 1.6M×$0.15 + 0.03M×$7.50 ≈ **$1.07/task**.
  - A GEX131 at ~$990/month equals about **900 tasks/month (~30/day)** before self-hosting breaks even, with the GPU fully utilised and no ops cost.
  - A 128B dense model at Q4 on one RTX PRO 6000 decodes at roughly tens of tokens/s per stream. Thirty long tasks a day is roughly at that card's practical limit, so break-even is marginal.
  - Against DeepSeek V4 Flash or Qwen3-Coder-Next APIs (~10x cheaper), self-hosting **never** breaks even on cost alone. Against Claude Sonnet 5.5 it breaks even sooner, but at lower quality.
- Self-hosting is justified mainly for strict data protection (code never leaves own hardware) or for a fixed-cost, unlimited "night queue". Queue's tasks are not time-critical, which suits one GPU working through jobs serially.
- Serving stack (P): vLLM or SGLang for OpenAI-compatible endpoints with tool calling and prefix caching; Ollama for simple single-user setups. All harnesses above can point at an OpenAI-compatible base URL. Codex needs Responses-API compatibility; vLLM exposes `/v1/responses` (P, verify).

### Gaps
- vLLM/SGLang documentation was not fetched. Responses-API support and tool-call parsers for each model are unverified.
- Measured throughput (tokens/s) for Mistral Medium 3.5, GLM or DeepSeek on RTX PRO 6000 or H100 was not found.
- Lambda, OVH and Hetzner GEX44/GEX63 prices were not verified.
- Aider polyglot and Terminal-Bench leaderboard pages were not fetched directly. Benchmark numbers are mostly vendor-reported or from aggregators.
- GPU sizes for GLM-5.x, DeepSeek V4 and Qwen3-Coder were not found.

---

## 5. EU/CH data-residency options

### Takeaway
For Claude, EU residency means **Bedrock EU regions** (Frankfurt, Ireland, Paris, Stockholm) or **Vertex EU multi-region**, at a ~10% premium; Zurich-specific availability was not confirmed. The first-party Claude API offers only global or US routing. **Swiss-hosted token APIs exist**: Infomaniak sells open models (Qwen3.5-397B, Apertus, Gemma 4) priced in CHF per MTok. Swisscom (subscription) and Exoscale (GPU-hours) are the other Swiss options. Mistral La Plateforme is EU-based and its flagship has open weights.

### Cited Findings
- Bedrock and Google regional/multi-region endpoints for Claude 4.5+ cost **10% more** than global endpoints. Google offers global and multi-region endpoints for new models, and single-region endpoints only for Sonnet 4.6 and earlier. — [Anthropic pricing](https://platform.claude.com/docs/en/about-claude/pricing)
- Claude on Bedrock is reportedly available in eu-central-1 (Frankfurt), eu-west-1, eu-west-3 and eu-north-1, and on Vertex in the EU multi-region. No source mentions eu-central-2 (Zurich) [secondary]. — [requesty.ai](https://www.requesty.ai/blog/eu-data-residency-smartest-models-rankings-premium)
- EU-regional endpoints typically cost about 10% more for Claude on Bedrock/Vertex, Gemini non-global endpoints and Azure EU Data Zone; one source reports a 20% premium for one OpenAI model on Azure EU Data Zone. Microsoft reportedly announced an EU Data Zone price increase from 2026-09-01 (amount unknown) [secondary]. — [aliteq](https://aliteq.com/eu-data-residency-ai-api-cost); [digitalapplied](https://www.digitalapplied.com/blog/ai-data-residency-cost-premiums-reference)
- Claude in Microsoft Foundry offers a US Data Zone (1.1x); no EU Data Zone for Claude is listed. — [Anthropic pricing](https://platform.claude.com/docs/en/about-claude/pricing)
- **Infomaniak (CH):** official per-MTok prices in CHF. Examples: gemma-4-31B-it CHF 0.20 in / 0.40 out; Apertus-v1.5-70B CHF 0.70 / 2.50; Qwen3.5-397B up to CHF 3.60 output. — [Infomaniak AI services prices](https://www.infomaniak.com/en/hosting/ai-services/prices)
- **Swisscom:** reportedly subscription-based (Llama 8B from CHF 1,000/month, 70B from CHF 2,000/month incl. 50M tokens). Another source says from CHF 0.38/MTok input; the two figures conflict [secondary]. — [mydata.ch](https://mydata.ch/llm-vergleich); [openstream.ch](https://www.openstream.ch/vergleich-ki-anbieter-schweiz/)
- **Exoscale (CH):** dedicated inference billed by GPU runtime, about CHF 2.15/GPU-h per second [secondary]. — [devs-group.ch](https://devs-group.ch/en/blog/sovereign-ai-inference-switzerland/)
- **OpenRouter EU** (eu.openrouter.ai) routes inference in-region, but account and billing data may be processed in the US. — [OpenRouter Sovereign AI](https://openrouter.ai/docs/guides/features/sovereign-ai)
- **Mistral:** Medium 3.5 API at $1.50/$7.50; weights downloadable for self-hosting. — [Hugging Face](https://huggingface.co/mistralai/Mistral-Medium-3.5-128B); [conductatlas](https://conductatlas.com/platform/mistral-ai/mistral-medium-35-model-card/api-token-based-pricing/)

### Inferences
- Suggested CH/EU tiering for Queue users:
  1. **Strict (code stays in CH):** Infomaniak API (Qwen3.5-397B or similar) via opencode or goose, or self-hosted models on Exoscale/own hardware.
  2. **EU:** Mistral La Plateforme (Medium 3.5); Claude via Bedrock Frankfurt or Vertex EU (+10%) using Claude Code's Bedrock/Vertex mode; Gemini via Vertex EU.
  3. **Global/cheapest:** first-party APIs, DeepSeek or OpenRouter. Note that the DeepSeek first-party API is China-hosted (P), which matters for FADP/GDPR transfer assessments.
- Infomaniak Qwen3.5-397B at CHF 3.60/MTok output is in the same range as Mistral Medium 3.5, and cheaper than Claude Sonnet 5.5 ($10 output). Its coding-agent quality relative to those models was not verified.

### Gaps
- Whether Claude, Gemini or AgentCore are available in AWS eu-central-2 (Zurich) or GCP europe-west6 (Zurich) could not be confirmed. The official regional tables were not reachable.
- Whether Infomaniak's models support tool calling or an OpenAI-compatible API well enough for agent harnesses is unverified.
- Mistral La Plateforme's data-processing location and its zero-retention options were not verified from primary sources.
- Azure OpenAI EU Data Zone pricing was found only in secondary sources.
