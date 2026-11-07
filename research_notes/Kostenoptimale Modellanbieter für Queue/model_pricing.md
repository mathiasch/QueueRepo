# LLM API pricing for an autonomous coding-agent queue (as of 7 Nov 2026)

**How these prices were checked (read first):**
- **[P-fetched]**: I fetched and read the provider's own page in full. Only Anthropic's docs (platform.claude.com) could be fetched.
- **[P-search]**: the provider's own domain was the only domain allowed in the web search, and the figure comes from the search tool's excerpt of that official page. The page itself could not be opened, because openai.com, ai.google.dev and mistral.ai do not resolve in this session's network sandbox. These figures are fairly reliable but should be checked on the live page.
- **[S]**: secondary source (aggregator, blog or tracker). Lower confidence.
- All prices are in USD per million tokens (MTok) unless stated otherwise.

**Claim check summary (the user's unverified claims):**

| Claim | Status | Correct value / note |
|---|---|---|
| Opus 5.5 $4/$20 | CONFIRMED [P-fetched] | Cache write 5m $5, cache write 1h $8, cache read $0.20 |
| Sonnet 5.5 $2/$10 | CONFIRMED [P-fetched] | Cache write 5m $2.50, cache write 1h $4, cache read $0.10 |
| Haiku 5.5 $0.10/$0.50 (prompts up to 100K) | CONFIRMED [P-fetched] | Prompts over 100K tokens cost $0.50/$2.50 |
| Anthropic Batch 50% off input and output | CONFIRMED [P-fetched] | Stacks with caching. 24h expiry |
| GPT-6 Astra $10/$50 | CONFIRMED [P-search] | Cached input $1. One aggregator (PricePerToken) lists $5/$25, which conflicts |
| GPT-6.1 Sol $2/$10 | CONFIRMED [P-search] | Cached input $0.10 |
| GPT-6 Luna $0.10/$0.50 | CONFIRMED [P-search] | Cached input $0.01 |
| GPT-6.1 Terra $1/$5 | CONFIRMED [P-search] | Cached input $0.05 |
| Gemini 4 Argon $2/$10 until 30.11.2026, then $4/$20 from 1.12.2026 | CONFIRMED [P-search on cloud.google.com and ai.google.dev, plus several S] | Cached input $0.10, rising to $0.20 |
| Gemini 3.x Flash $0.75/$3.75, doubling on 1.1.2027 | CONFIRMED for **Gemini 3.8 Flash** [P-search] | Goes to $1.50/$7.50 on 1.1.2027. Per [S], 3.6 and 3.7 Flash have the same price. **Gemini 3.5 Flash differs**: $1.50/$9 [S] |
| Mistral Large 4 $1.36/$4.18 | DIFFERENT (outdated) | Now **$0.70 input / $0.07 cached / $2.10 output** [P-search on docs.mistral.ai]. $1.36/$0.14/$4.18 was the list price before the open-weights release on 29.10.2026 [S] |
| Mistral Medium 3.5 $1.50/$7.50, open weights | CONFIRMED [S, plus Mistral model card via search] | Open weights under a modified MIT licence. The model card marks it **deprecated (31.10.2026)** and points to Large 4 for new work |
| Mistral Small 4 $0.15/$0.60 | CONFIRMED [P-search] | Cached input $0.015. Apache 2.0 licence |

## Q1: Exact list prices, including caching, batch and long-context rules, per provider

### Takeaway
Anthropic's prices are fully confirmed from the primary page. Opus 5.5 and Sonnet 5.5 have unusually cheap cache reads (0.05x base input). Haiku 5.5 at $0.10/$0.50 is the cheapest Anthropic tier, but prompts over 100K tokens cost 5x more, which matters for agent loops that build up context. OpenAI's GPT-6/6.1 prices are confirmed. Two Google prices go up soon: Gemini 4 Argon doubles on 1 Dec 2026 and the Flash models double on 1 Jan 2027. Mistral Large 4 is now much cheaper than the user's figure.

### Cited Findings

**Anthropic (Claude API, first-party) [P-fetched]**

| Model | Input | 5m cache write | 1h cache write | Cache read | Output | Batch in/out |
|---|---|---|---|---|---|---|
| Fable 5.1 / Mythos 5.1 (Mythos: limited access) | $10 | $12.50 | $20 | $0.25 | $50 | $5 / $25 |
| Opus 5.5 | $4 | $5 | $8 | $0.20 | $20 | $2 / $10 |
| Opus 5 / 4.8 / 4.7 / 4.6 / 4.5 | $5 | $6.25 | $10 | $0.50 | $25 | $2.50 / $12.50 |
| Sonnet 5.5 | $2 | $2.50 | $4 | $0.10 | $10 | $1 / $5 |
| Sonnet 5 | $2 | $2.50 | $4 | $0.20 | $10 | $1 / $5 |
| Sonnet 4.6 / 4.5 | $3 | $3.75 | $6 | $0.30 | $15 | $1.50 / $7.50 |
| Haiku 5.5, prompt up to 100K tokens | $0.10 | $0.125 | $0.20 | $0.01 | $0.50 | $0.05 / $0.25 |
| Haiku 5.5, prompt over 100K tokens | $0.50 | $0.625 | $1 | $0.05 | $2.50 | $0.25 / $1.25 |
| Haiku 4.5 | $1 | $1.25 | $2 | $0.10 | $5 | $0.50 / $2.50 |

Source for the table: [Anthropic pricing](https://platform.claude.com/docs/en/about-claude/pricing). Further details from the same page:
- Caching multipliers: a 5-minute cache write costs 1.25x base input and a 1-hour write costs 2x. A cache read costs 0.1x, except 0.05x on Opus 5.5 and Sonnet 5.5 and 0.025x on Fable 5.1 and Mythos 5.1. "These multipliers stack with other pricing modifiers, including the Batch API discount and data residency." — [Anthropic pricing](https://platform.claude.com/docs/en/about-claude/pricing)
- Long context: Claude 4.6 and later models (except Haiku 5.5) include the full 1M-token context at standard pricing, with no surcharge. Haiku 5.5 is priced per request by prompt length. The prompt length includes cache reads and writes, so a request over 100K tokens pays the higher rate even when part of it is a cache hit. — [Anthropic pricing](https://platform.claude.com/docs/en/about-claude/pricing)
- Fast mode: Opus 5.5 costs $8/$40, Opus 5 and 4.8 cost $10/$50. Fast mode cannot be used with Batch. — [Anthropic pricing](https://platform.claude.com/docs/en/about-claude/pricing)
- Sonnet 5's $2/$10 was introductory pricing that has now become permanent. The planned increase to $3/$15 on 1 Sep 2026 will not happen. — [Anthropic pricing](https://platform.claude.com/docs/en/about-claude/pricing)
- Tokenizer: Claude 4.7 and later models use a tokenizer that produces about 30% more tokens for the same text. Compare cost per task, not just price per token. — [Anthropic pricing](https://platform.claude.com/docs/en/about-claude/pricing)
- Managed Agents costs $0.08 per session-hour on top of tokens, and the Batch discount does not apply to it. — [Anthropic pricing](https://platform.claude.com/docs/en/about-claude/pricing)
- Web search costs $10 per 1,000 searches. Web fetch has no extra charge. — [Anthropic pricing](https://platform.claude.com/docs/en/about-claude/pricing)

**OpenAI (API) [P-search, from developers.openai.com/api/docs/pricing and the model pages]**

| Model | Input | Cached input | Output | Batch/Flex (input / cached / output) |
|---|---|---|---|---|
| GPT-6 Astra (flagship, released 3 Sep 2026 per [S]) | $10 | $1 | $50 | $5 / $0.50 / $25 |
| GPT-6.1 Sol | $2 | $0.10 | $10 | $1 / $0.05 / $5 |
| GPT-6.1 Terra | $1 | $0.05 | $5 | $0.50 / $0.025 / $2.50 |
| GPT-6 Luna | $0.10 | $0.01 | $0.50 | $0.05 / $0.005 / $0.25 |

Source for the table: [OpenAI Pricing](https://developers.openai.com/api/docs/pricing). Further details:
- OpenAI now charges for cache writes, with two retention options: 30 minutes and 12 hours. For Astra, a write costs $12.50 (30 min) or $25 (12 h), which is 1.25x and 2.5x input. — [OpenAI Pricing](https://developers.openai.com/api/docs/pricing), [GPT-6 Astra model page](https://developers.openai.com/api/docs/models/gpt-6-astra). For Terra, a write costs $1.25 (30 min) or $2.50 (12 h) per [S] [eesel.ai](https://www.eesel.ai/blog/gpt-6-1-terra-pricing).
- Batch and Flex both cost 50% of Standard. Fast mode costs 2x. — [OpenAI Pricing](https://developers.openai.com/api/docs/pricing)
- Long context: for prompts over 272K input tokens, input and cache rates double and output costs 1.5x, applied to the whole request. Astra has a 1.05M-token context window and 128K max output. — [OpenAI Pricing](https://developers.openai.com/api/docs/pricing), [GPT-6 Astra model page](https://developers.openai.com/api/docs/models/gpt-6-astra)
- Conflict: [PricePerToken](https://pricepertoken.com/pricing-page/model/openai-gpt-6-astra) [S] lists Astra at $5/$25. The official-domain excerpt and the ChatGPT Enterprise rate card ([OpenAI help](https://help.openai.com/en/articles/20001415-chatgpt-rate-card-enterprise-token-based-pricing)) both say $10/$1/$50.

**Google Gemini (Developer API and Vertex / "Gemini Enterprise Agent Platform")**
- Gemini 4 Argon costs $2 input, $0.10 cached input and $10 output through 30 Nov 2026. From 1 Dec 2026 it costs $4, $0.20 and $20. These rates are for the global endpoint on Vertex. — [P-search] [Agent Platform Pricing](https://docs.cloud.google.com/vertex-ai/generative-ai/pricing), [Gemini API pricing](https://ai.google.dev/gemini-api/docs/pricing). Secondary confirmation: [technology.org](https://www.technology.org/2026/10/14/gemini-4-argon-generally-available-pricing-december/), [TechRepublic](https://www.techrepublic.com/article/news-gemini-4-argon-access-pricing/).
- Gemini 4 Argon batch on the Developer API: input $1 through 30 Nov, then $2. Batch context caching $0.05, then $0.10. Argon supports **implicit caching only**, not explicit caching. — [P-search] [Gemini API pricing](https://ai.google.dev/gemini-api/docs/pricing)
- Gemini 3.8 Flash: introductory price of $0.75/$3.75 through 31 Dec 2026, then $1.50/$7.50 from 1 Jan 2027. On Vertex, from 1 Jan 2027 input costs $1.50 on the global endpoint and $1.65 on non-global endpoints, with cached input at $0.15 and $0.165. — [P-search] [Agent Platform Pricing](https://docs.cloud.google.com/vertex-ai/generative-ai/pricing), [What's new in Gemini 3.8 Flash](https://ai.google.dev/gemini-api/docs/latest-model?hl=en)
- Per [S], Gemini 3.6 and 3.7 Flash have the same introductory price and will also double. Since 8 Oct, requests for 3.7 Flash are routed to 3.8 Flash and requests for 3.5 Flash to 3.6 Flash. Gemini 3.5 Flash's standard price is $1.50/$9. — [codersera](https://codersera.com/blog/gemini-3-8-flash-complete-guide-2026/), [jahanzaib.ai](https://www.jahanzaib.ai/blog/gemini-3-7-flash-pricing-doubles-january-2027), [Simon Willison on 3.5 Flash](https://simonwillison.net/2026/May/19/gemini-35-flash/)
- Gemini 3.7 Flash will be retired on 28 Jan 2027. — [P-search] [3.7 Flash docs](https://docs.cloud.google.com/gemini-enterprise-agent-platform/models/gemini/3-7-flash)
- Batch costs 50% of the standard interactive price. — [P-search] [Gemini API pricing](https://ai.google.dev/gemini-api/docs/pricing)

**Mistral**
- Mistral Large 4 costs $0.70 input, $0.07 cached input and $2.10 output, with a 1M-token context window. — [P-search] [Mistral docs pricing](https://docs.mistral.ai/inference/pricing), [Large 4 model card](https://docs.mistral.ai/models/mistral-large-4). The model was released as open weights on 29 Oct 2026: [Mistral Large 4 is now open weight](https://mistral.ai/news/mistral-large-4-oss/). Before that, the list price was $1.36/$0.14/$4.18 per [S] [BenchLM](https://benchlm.ai/mistral/api-pricing) and [tech-insider](https://tech-insider.org/mistral-large-4-launch-1-05t-params-pricing-2026/).
- Mistral Medium 3.5 costs $1.50/$7.50. It is a 128B dense model with open weights under a modified MIT licence. The model card marks it deprecated (31 Oct 2026). — [Mistral Medium 3.5 model card](https://docs.mistral.ai/models/model-cards/mistral-medium-3-5-26-04) (via search excerpt), [Artificial Analysis](https://artificialanalysis.ai/models/mistral-medium-3-5) [S]
- Mistral Small 4 costs $0.15 input, $0.015 cached input and $0.60 output, under Apache 2.0. — [P-search] [Mistral docs pricing](https://docs.mistral.ai/inference/pricing)
- Devstral 2 costs $0.40/$2.00 and Devstral Small 2 costs $0.10/$0.30. Devstral 2 moved from free to paid API access, dated 26 Jan in the changelog. — [P-search] [Devstral 2 announcement](https://mistral.ai/news/devstral-2-vibe-cli/), [Mistral Vibe 2.0](https://mistral.ai/news/mistral-vibe-2-0/), [Changelog](https://docs.mistral.ai/resources/changelogs)
- Batch costs 50% less, and the 2024 announcement says it covers all models on La Plateforme. — [P-search] [Mistral pricing](https://mistral.ai/pricing/), [Mistral batch API](https://mistral.ai/news/batch-api/)

**Low-cost alternatives (all [S] unless noted; prices change often)**
- DeepSeek V4 Flash costs $0.14/$0.28 and V4 Pro costs $0.435/$0.87 on DeepSeek's first-party API (snapshot from July 2026). — [developersdigest](https://www.developersdigest.tech/blog/glm-5-2-vs-deepseek-v4-vs-qwen3-open-weights-coding-showdown). OpenRouter notes that DeepSeek's first-party API keeps data for training. — [OpenRouter blog June 2026](https://openrouter.ai/blog/insights/the-open-weight-models-that-matter-june-2026/)
- DeepSeek V4.1 Flash on OpenRouter: one listing shows $0.006 input / $0.50 output, which looks promotional. A batch variant costs $0.112/$0.336. — [OpenRouter DeepSeek](https://openrouter.ai/deepseek), [OpenRouter compare](https://openrouter.ai/compare/deepseek/deepseek-v4.1-flash/z-ai/glm-5.3-flash). Per [geotoolbox](https://geotoolbox.ai/blog/chinese-ai-models-compared), it costs about $0.15 input off-peak and roughly double at peak times.
- GLM: GLM-5.2 costs $1.40/$4.40 on Z.ai's list, and other providers charge about half that. GLM-5.3 input costs about $1.40, and its batch rate on OpenRouter is $0.45/$2. GLM-5.3 Flash costs about $0.03/$0.09–0.24. — [developersdigest](https://www.developersdigest.tech/blog/glm-5-2-vs-deepseek-v4-vs-qwen3-open-weights-coding-showdown), [geotoolbox](https://geotoolbox.ai/blog/chinese-ai-models-compared), [OpenRouter discounted](https://openrouter.ai/collections/discounted-models)
- Kimi: K3 costs $3/$15 and K3.1 Pro costs $2/$10. The open-weights K2.7-Code is only available on Moonshot's API to accounts registered before 31 Oct 2026. K2.6 on OpenRouter providers costs $0.39–0.50 input and $1.80–2.50 output. — [developersdigest](https://www.developersdigest.tech/blog/glm-5-2-vs-deepseek-v4-vs-qwen3-open-weights-coding-showdown), [geotoolbox](https://geotoolbox.ai/blog/chinese-ai-models-compared), [OpenRouter providers](https://openrouter.ai/provider/chutes)
- Qwen: 3.8-Max costs $2/$6 on Alibaba's API, and 3.7-Max cost $2.50/$7.50. — [yottalabs](https://www.yottalabs.ai/post/best-chinese-llm-models-2026-deepseek-qwen-glm-kimi-compared), [developersdigest](https://www.developersdigest.tech/blog/glm-5-2-vs-deepseek-v4-vs-qwen3-open-weights-coding-showdown)

### Inferences
- For a queue where tasks are not time-critical, the cheapest capable options at list price are: Haiku 5.5 batch at $0.05/$0.25 (only while prompts stay under 100K tokens), GPT-6 Luna batch at $0.05/$0.25, Devstral Small 2 at $0.10/$0.30 (about $0.05/$0.15 with batch, if the 50% discount applies), Mistral Small 4 at $0.15/$0.60, and DeepSeek V4 Flash at $0.14/$0.28.
- In the middle tier, Sonnet 5.5 and GPT-6.1 Sol cost the same ($2/$10), but Sonnet 5.5's cache read ($0.10) equals Sol's cached input ($0.10). Gemini 4 Argon matches them only until 30 Nov. After that it costs twice as much.
- Mistral Large 4 at $0.70/$2.10 is now the cheapest large Western model. Because it has open weights, it can also be self-hosted in Switzerland.
- Agent loops resend the conversation each turn, so input and cache-read tokens usually dominate cost. Cache-read price per MTok is therefore the key number: Haiku 5.5 $0.01, Luna $0.01, Terra $0.05, Sonnet 5.5 $0.10, Sol $0.10, Opus 5.5 $0.20, Astra $1.
- Haiku 5.5's 100K threshold is a trap for long agent sessions. A coding agent's context easily grows past 100K tokens, at which point Haiku costs $0.50/$2.50, more than Terra's input rate. The queue should compact or limit context to stay under 100K when using Haiku.

### Gaps
- I could not open openai.com, ai.google.dev or mistral.ai directly (DNS is blocked in the sandbox). Every [P-search] figure should be checked on the live page.
- Not found: Gemini 4 Argon's long-context (over 200K) tier and its output batch price on the official page. Gemini's explicit-cache storage price per hour. Gemini 3.8 Flash's non-global output price.
- Not found: an official Devstral 2 batch price, and a per-model cache-read price for Devstral.
- The Chinese models' prices come only from secondary sources, and model names differ between sources (for example V4 vs V4.1, K3 vs K3.1 Pro).

## Q2: Does batch combine with caching? Batch latency and SLA

### Takeaway
At Anthropic and OpenAI, batch and cache discounts stack. Google charges cache hits inside a batch at a separate cache rate. Anthropic's batch window is 24 hours, and most batches finish in under an hour. That latency is acceptable for a non-time-critical queue, but batch only suits single-turn calls. A multi-turn interactive agent loop cannot be submitted as one batch.

### Cited Findings
- Anthropic: "The pricing discounts from prompt caching and Message Batches can stack". Cache hits in batches are best-effort, with typical hit rates of 30–98%. Anthropic recommends the 1-hour cache for batches because batches can take longer than 5 minutes. — [Anthropic batch processing](https://platform.claude.com/docs/en/build-with-claude/batch-processing)
- Anthropic limits: up to 100,000 requests or 256 MB per batch. Most batches complete within 1 hour. A batch expires if it is not done within 24 hours, and expired requests are not billed. Results are kept for 29 days. Batch is not eligible for zero data retention. `max_tokens: 0` (cache pre-warming) is not allowed in a batch. — [Anthropic batch processing](https://platform.claude.com/docs/en/build-with-claude/batch-processing)
- Anthropic: `inference_geo` is supported on the Batch API, per request. — [Anthropic data residency](https://platform.claude.com/docs/en/manage-claude/data-residency)
- OpenAI: in Batch and Flex, cached input is also charged at 50% (for example $0.025 on Terra and $0.50 on Astra). Flex runs normal synchronous requests more slowly at the batch price, but can return 429 errors. — [P-search] [OpenAI Pricing](https://developers.openai.com/api/docs/pricing); Flex behaviour per [S] [dev.to](https://dev.to/alifar/openai-gpt-61-terra-cuts-batch-and-flex-api-costs-by-50-from-standard-pricing-1i2h)
- Google: context caching works inside batch requests. "If a request in your batch results in a cache hit, you pay the standard context caching rates." The Argon pricing table also lists a separate batch context-caching rate ($0.05, rising to $0.10), which is half the standard rate. The two statements partly conflict. — [P-search] [Gemini API pricing](https://ai.google.dev/gemini-api/docs/pricing), [Gemini Batch API](https://ai.google.dev/gemini-api/docs/batch-api)
- Mistral: batch costs 50% less. — [Mistral pricing](https://mistral.ai/pricing/)

### Inferences
- OpenAI Flex is probably the best fit for Queue's agent loops. It keeps the synchronous, multi-turn tool-use pattern at batch prices, with only slower responses and occasional 429s, and the tasks are not time-critical. Anthropic has no Flex equivalent in the pricing page I read. For Claude, batch savings therefore apply only to work that can be split into independent single-turn calls, such as planning, reviews or classification.
- From memory, not re-verified in this session: OpenAI and Gemini batch jobs also target a 24-hour completion window.

### Gaps
- I did not verify the official batch SLA wording for OpenAI, Google or Mistral in this session.
- I did not check whether Anthropic offers a "flex" or low-priority service tier (the service-tiers page was not fetched).

## Q3: EU/CH data residency and regional price premiums

### Takeaway
Anthropic's first-party API has **no EU option**: only "global" and "us" inference geos exist, and workspace data is stored only in the US. For Claude with EU residency you need Bedrock (EU geo profile, +10%) or Vertex (EU multi-region endpoint, +10%). Zurich in-region is unclear or not supported. Azure charges +20% for the EU Data Zone and +30% for Switzerland North. Gemini's non-global endpoints cost about +10%. Mistral has an EU endpoint at +10%.

### Cited Findings
- Anthropic first-party: `inference_geo` only accepts "global" (the default) and "us". "us" costs 1.1x. Workspace geo is "us" only. — [Anthropic data residency](https://platform.claude.com/docs/en/manage-claude/data-residency)
- Bedrock and Vertex: regional and multi-region endpoints carry a 10% premium over global endpoints. This applies to Claude Sonnet 4.5, Haiku 4.5, Opus 4.5 and all later models. — [Anthropic pricing](https://platform.claude.com/docs/en/about-claude/pricing)
- Bedrock and Zurich: the Opus 5.5 model card lists eu-central-2 (Zurich) as "not supported" for in-region use but supported for the Geo (EU) and Global profiles. The profile ID is `eu.anthropic.claude-opus-5-5`, and requests are routed to Frankfurt, Stockholm, Milan, Spain, Ireland or Paris. In-region use is only available in eu-west-2 (London). The regional availability page (possibly outdated) says Geo is not supported from eu-central-2, which conflicts. — [P-search] [Bedrock Opus 5.5 model card](https://docs.aws.amazon.com/bedrock/latest/userguide/model-card-anthropic-claude-opus-5-5.html), [Bedrock regional availability](https://docs.aws.amazon.com/bedrock/latest/userguide/models-region-compatibility.html). AWS also has a guide on EU cross-region inference for Switzerland: [AWS Alps blog](https://aws.amazon.com/blogs/alps/unlocking-ai-flexibility-in-switzerland-a-guide-to-cross-region-inference-for-eu-data-processing-and-model-access/)
- Vertex: an EU multi-region endpoint (`aiplatform.eu.rep.googleapis.com`, location `eu`) supports all Claude models from version 4.7 onward. Prompt caching is supported on it, provisioned throughput is not. Whether Opus 5.5 or Sonnet 5.5 is served in europe-west6 (Zurich) could not be confirmed. Some Google products exclude europe-west6 from the EU multi-region. Sources disagree on whether the endpoint is in preview or generally available (GA since 15 May 2026). — [P-search] [Google Cloud blog](https://cloud.google.com/blog/products/ai-machine-learning/multi-region-endpoints-for-claude-available-on-vertex-ai), [Vertex locations](https://cloud.google.com/vertex-ai/generative-ai/docs/learn/locations), [BigQuery locations](https://docs.cloud.google.com/bigquery/docs/locations)
- Gemini on Vertex: separate "Global" and "Non-global" price rows. For example, 3.8 Flash input from 1 Jan 2027 costs $1.50 global and $1.65 non-global, which is +10%. — [P-search] [Agent Platform Pricing](https://docs.cloud.google.com/vertex-ai/generative-ai/pricing)
- Azure / Microsoft Foundry: for models launched on or after 1 Sep 2026, the EU Data Zone costs +20% over Global and Switzerland North (a regional deployment) costs +30%. For example, GPT-6.1 Sol input costs $2.40 in the EU Data Zone versus $2.00 Global. The EU Data Zone follows the EU Data Boundary, which may include EFTA countries such as Switzerland and Norway. GPT-6 Astra, Sol and Luna and GPT-6.1 Terra are available in Foundry. — [P-search] [Foundry pricing update](https://techcommunity.microsoft.com/blog/azure-ai-foundry-blog/microsoft-foundry-model-deployment-pricing-update/4535385), [GPT-6.1 Sol in Foundry](https://techcommunity.microsoft.com/blog/azure-ai-foundry-blog/introducing-gpt-6-1-sol-in-microsoft-foundry-advanced-intelligence-optimized-for/4560811), [Foundry region availability](https://learn.microsoft.com/en-us/azure/foundry/foundry-models/concepts/models-sold-directly-by-azure-region-availability). The Azure pricing page showed placeholders ("$-") for the GPT-6 Data Zone prices.
- Claude in Microsoft Foundry: only a US Data Zone (1.1x) is documented. No EU data zone for Claude is mentioned. — [Anthropic pricing](https://platform.claude.com/docs/en/about-claude/pricing)
- Mistral: an EU regional endpoint (`api.eu.mistral.ai`) and a US endpoint for supported models. Each region is a separate deployment with its own data boundary, and regional routing costs **10% extra**. Requests without a regional endpoint go to the global endpoint. — [P-search] [Mistral Inference](https://mistral.ai/cloud/inference/)

### Inferences
- The cheapest EU-resident Claude option is Bedrock's EU geo profile or Vertex's EU multi-region at list price +10%. For Opus 5.5 that works out to about $4.40/$22, assuming Bedrock and Vertex use the same base prices as the first-party API, which I did not verify.
- For OpenAI models with EU residency, Azure's EU Data Zone (+20%) is cheaper than Switzerland North (+30%).
- For strict Swiss-only processing, the realistic options are Mistral Large 4 or Medium 3.5 open weights (or other open-weight models) self-hosted in Switzerland, or Azure Switzerland North at +30%.

### Gaps
- I did not see Bedrock's or Vertex's own price tables for Claude 5.5 (aws.amazon.com/bedrock/pricing was not reachable).
- Not verified: whether OpenAI's direct API offers EU data residency, and at what price.
- Not confirmed: Gemini 4 Argon availability and price in europe-west6.
- Not confirmed: which Mistral models are served from the EU endpoint.

## Q4: Subscription plans in 2026 and what they are worth compared with the API

### Takeaway
Claude's plans changed: Max 5x is now simply **Max ($100)**, and Max 20x is now **Ultra ($200)**, with 10x Pro's weekly usage and no 5-hour limit for verified accounts. ChatGPT Pro comes in three tiers ($100, $200, $500) and has no 5-hour Codex limit. Google AI Pro costs $19.99 and Ultra $99.99 or $199.99, with official Gemini CLI quotas of 1,500 or 2,000 requests per day. No provider publishes an API-dollar equivalent, and I found no credible third-party estimate in dollars.

### Cited Findings
- Claude Pro costs $20 per month, or $17 per month billed annually. — [S] [klymentiev](https://klymentiev.com/blog/claude-plans), consistent with [claude.com/pricing](https://claude.com/pricing) (not fetched)
- Claude Max costs $100 per month, billed monthly. It gives 5x Pro's usage per 5-hour session and per week. There is a weekly cap across all models plus a separate weekly cap for Fable. — [P-search] [What is the Max plan?](https://support.claude.com/en/articles/11049741-what-is-the-max-plan)
- Max 20x has been renamed Ultra, at $200 per month. Ultra gives 10x Pro's weekly usage and has no 5-hour limit for verified accounts. — [P-search] [What is the Ultra plan?](https://support.claude.com/en/articles/17728952-what-is-the-ultra-plan)
- Claude Code is included in all paid plans and uses the same usage pool as chat. — [P-search] [Use Claude Code with your Pro, Max, or Ultra plan](https://support.claude.com/en/articles/11145838-use-claude-code-with-your-pro-or-max-plan)
- Max includes $100 per month in API credits, and Ultra and Team also include monthly API credits. These credits cannot be used for interactive Claude Code sessions or for extra usage beyond plan limits. — [P-search] [Monthly API credits](https://support.claude.com/en/articles/17154008-monthly-api-credits-for-max-ultra-and-team-plans)
- About 24 days ago, Anthropic doubled Claude Code's 5-hour limits for Pro, Max, Team and seat-based Enterprise, and removed the peak-hours reduction for Pro and Max. — [P-search] [Anthropic: Higher usage limits and a SpaceX compute deal](https://www.anthropic.com/news/higher-limits-spacex)
- Usage credits let users keep working past a limit at pay-as-you-go rates. — [P-search] [Manage usage credits](https://support.claude.com/en/articles/12429409-manage-usage-credits-for-paid-claude-plans)
- A third-party measurement (26 Oct–2 Nov 2026) found that one 5-hour window on Max 20x/Ultra is about 22x a Pro window, but the whole week is only about 10x. — [S] [wokey.ai](https://wokey.ai/claude-usage-limits)
- ChatGPT plans: Free $0, Go $8, Plus $20, Pro $100 (5x Plus), $200 (20x) and $500. — [S] summary of OpenAI's page via [nxcode](https://www.nxcode.io/resources/news/claude-max-vs-chatgpt-pro-2026-premium-ai-comparison). Codex pricing page: Plus $20, Pro from $100. Local messages and cloud tasks share one allowance. On Plus, GPT-6.1 Sol allows 15–160 messages per 5 hours, and weekly limits may also apply. "Pro plans currently have no five-hour limit". Cloud tasks use more of the allowance than local messages. Extra credits can be bought. — [P-search] [Codex Pricing](https://developers.openai.com/codex/pricing), [chatgpt.com/codex/pricing](https://chatgpt.com/codex/pricing/)
- Google AI Pro costs $19.99 per month. AI Ultra costs $99.99 or $199.99, with 5x and 20x Pro's usage. At I/O 2026, Google added the $100 Ultra tier and cut the top tier from $250 to $200. — [P-search] [gemini.google/subscriptions](https://gemini.google/subscriptions/), [Google blog I/O 2026](https://blog.google/products-and-platforms/products/google-one/google-ai-subscriptions/)
- Official Gemini CLI / Code Assist quotas: AI Pro 1,500 requests per user per day, AI Ultra 2,000, Code Assist Standard 1,500, Code Assist Enterprise 2,000. The quotas are shared across all models. — [P-search] [Gemini Code Assist quotas](https://developers.google.com/gemini-code-assist/resources/quotas), [Gemini CLI quotas](https://geminicli.com/docs/resources/quota-and-pricing/)
- Conflicting reports: users report lower effective limits (for example, an Ultra account detected with a 200-request limit). A third-party tracker says the Code Assist individual, AI Pro and AI Ultra tiers stopped serving Gemini CLI and IDE requests from 18 Jun 2026. Not confirmed by Google. — [GitHub issue #27313](https://github.com/google-gemini/gemini-cli/issues/27313), [agentdeals issue #1900](https://github.com/robhunter/agentdeals/issues/1900)

### Inferences
- Rough break-even, my own estimate and not a published figure: $200 per month buys 10M Opus 5.5 output tokens plus about 20M cache reads at list price. At batch prices with Sonnet 5.5, it buys about 20M output tokens. Heavy users of Claude Code have often reported consuming several times their subscription price in API-equivalent value, but I found no current credible dollar figure in this session.
- The Codex Pro and Claude Ultra plans both now lack 5-hour caps (Ultra only for verified accounts) but have weekly caps. That suits a queue that runs continuously, but weekly caps make the throughput unpredictable. The queue should fall back to API keys (batch or Flex) when a weekly cap is reached.
- Check the provider terms of service for automated or shared use of consumer subscriptions in a self-hosted queue. I did not check them in this session.

### Gaps
- No official token or message counts for Claude plans. No credible published "API value per month" estimate found.
- ChatGPT Pro $500 tier: the exact Codex allowance was not found.
- Whether Gemini CLI with an AI Pro or Ultra login still works after June 2026 is unresolved.

## Q5: Coding benchmarks for judging price against performance

### Takeaway
Leaderboards for November 2026 come mostly from aggregators and use different harnesses. Opus 5.5 leads SWE-bench Pro and Terminal-Bench 4.0, with Sonnet 5.5 close behind on Terminal-Bench 4.0. For the cheap tiers I found only partial data: Gemini 3.8 Flash scores 80% on SWE-bench Verified and Devstral 2 scores 72.2. I found no scores for Haiku 5.5, Luna, Terra or Mistral Small 4.

### Cited Findings
- SWE-bench Pro: Opus 5.5 89.9%, Sonnet 5.5 81.3%, Fable 5.1 81.2% (BenchLM, updated 6 Nov 2026). — [S] [BenchLM SWE-bench Pro](https://benchlm.ai/benchmarks/swe-bench-pro), [llm-stats](https://llm-stats.com/benchmarks/swe-bench-pro)
- Terminal-Bench 4.0 (Vals AI, mini-SWE-agent harness, 5 Nov 2026): Opus 5.5 65.15%, Sonnet 5.5 64.14%, Kimi K3.1 Pro 63.64%. One blog reports Opus 5.5 at 64.8% instead. — [Vals AI Terminal-Bench 4](https://www.vals.ai/benchmarks/terminal-bench-4)
- Terminal-Bench 2.1 (BenchLM): Cognition SWE-2 92.8%, GPT-5.6 Sol 91.9%, DeepSeek V4.1 Flash 90.6%. — [S] [BenchLM TB 2.1](https://benchlm.ai/benchmarks/terminalbench21)
- SWE-bench Verified (Vals AI, mini-SWE-agent): Gemini 3.8 Flash 80.00% (rank 24, $2.19 per test), GPT-5.5 82.6%. — [Vals AI SWE-bench](https://www.vals.ai/benchmarks/swebench)
- SWE-bench Verified: Devstral 2 72.2, Devstral Small 2 68.0, Claude Haiku 4.5 73.3. — [S] [Frontier Benchmarks](https://frontierbenchmarks.com/benchmarks/SWE-bench-Verified/), [codesota](https://www.codesota.com/ocr/benchmark/swe-bench-verified)
- Mistral Medium 3.5 reportedly scores 77.6% on SWE-bench (variant not stated). — [S] [aimadetools](https://www.aimadetools.com/blog/mistral-medium-3-5-complete-guide/)

### Inferences
- Among cheap models with any published scores, Gemini 3.8 Flash ($0.75/$3.75 until year-end) and Devstral 2 ($0.40/$2) offer the best documented price against performance. Kimi K3.1 Pro ($2/$10) comes close to the Claude 5.5 models on Terminal-Bench 4.0.
- SWE-bench Verified looks saturated, and leaderboards are moving to SWE-bench Pro and Terminal-Bench 2.1/4.0. Scores depend heavily on the harness, so Queue should run its own small evaluation on its real tasks before choosing a cheap model.

### Gaps
- No scores found for Haiku 5.5, GPT-6 Luna, GPT-6.1 Terra, GPT-6 Astra, Gemini 4 Argon or Mistral Small 4 / Large 4 on SWE-bench Pro, Verified or Terminal-Bench. Official announcement pages were not reachable.
- The official leaderboards (scale.com SWE-bench Pro, tbench.ai) were not checked directly.
