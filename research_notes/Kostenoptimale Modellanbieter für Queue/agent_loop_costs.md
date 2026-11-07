# Agent-Loop Costs: Token Consumption, Caching, Batch APIs and Model Routing for Queue (as of 2026-11-07)

Method note: Anthropic primary docs (platform.claude.com, code.claude.com) could be fetched directly. ai.google.dev, developers.openai.com and aider.chat were blocked by this session's network policy, so all OpenAI, Gemini, Mistral and Aider figures below come from web-search summaries of secondary sources and are marked "(secondary)". Some facts come from Anthropic's `claude-api` skill reference files bundled with Claude Code 2.1.319 (local path given instead of a URL). All worked examples are my own calculations; their token profiles are assumptions calibrated against the measured data points cited below.

Reference price table (Anthropic first-party, per million tokens, from the [Anthropic pricing page](https://platform.claude.com/docs/en/about-claude/pricing)):

| Model | Base input | 5m cache write | 1h cache write | Cache read | Output | Batch in / out |
|---|---|---|---|---|---|---|
| Opus 5.5 | $4 | $5 | $8 | $0.20 (0.05x) | $20 | $2 / $10 |
| Sonnet 5.5 | $2 | $2.50 | $4 | $0.10 (0.05x) | $10 | $1 / $5 |
| Haiku 5.5, prompt ≤100k | $0.10 | $0.125 | $0.20 | $0.01 | $0.50 | $0.05 / $0.25 |
| Haiku 5.5, prompt >100k | $0.50 | $0.625 | $1.00 | $0.05 | $2.50 | $0.25 / $1.25 |
| Fable 5.1 (top tier) | $10 | $12.50 | $20 | $0.25 (0.025x) | $50 | $5 / $25 |
| Sonnet 4.6 (older, used for calibration) | $3 | $3.75 | $6 | $0.30 | $15 | $1.50 / $7.50 |

---

## 1. Measured token usage of coding agents per task (input, output, cached share, turns)

### Takeaway
Real agent tasks run on a large number of re-sent context tokens: roughly 15 to 150 turns, 0.5M to 10M+ cumulative input tokens per task, and in a healthy loop about 90 to 96% of input comes from cache reads. Output is a small share of tokens but, with caching on, 35 to 55% of cost. Published cost per task ranges from about $0.09 (trivial tasks) to about $5 (SWE-bench-class issues on Sonnet 4.6 under OpenHands), with long-tail outliers above $20. No authoritative 2026 cross-agent dataset of tokens per resolved issue was found.

### Cited Findings
- Claude Code `/usage` sample session (Sonnet 4.6): 1.2k uncached input, 5.3k output, 940.0k cache read, 50.0k cache write, total $0.55. Cache reads are about 95% of input tokens and about 51% of the cost. — [Claude Code docs: Manage costs](https://code.claude.com/docs/en/costs)
- Claude Code's prompt-cache status line example reads "14 requests · 91% of input tokens from cache · 2 misses". — [Claude Code docs: Manage costs](https://code.claude.com/docs/en/costs)
- Enterprise Claude Code average: about $13 per developer per active day and $150-250 per developer per month. 90% of users stay below $30 per active day. — [Claude Code docs: Manage costs](https://code.claude.com/docs/en/costs)
- Agent teams use roughly 7x more tokens than standard sessions when teammates run in plan mode. — [Claude Code docs: Manage costs](https://code.claude.com/docs/en/costs)
- Thinking tokens are billed as output. The default thinking budget "can be tens of thousands of tokens per request". Thinking cannot be turned off on Opus 5.5, Sonnet 5.5 or Haiku 5.5; effort level is the lever. — [Claude Code docs: Manage costs](https://code.claude.com/docs/en/costs)
- SWE-Bench ProMax (2026, multilingual refactoring, not SWE-bench Verified): under OpenHands, Claude Sonnet 4.6 was the most expensive model at $4.77 per instance and 117.9 steps on average. Qwen3.5 took the most steps (155.4 and 141.2 under the two scaffolds) and had the lowest resolve rate under mini-swe-agent. — [arXiv 2608.09802](https://arxiv.org/pdf/2608.09802) (via search summary)
- On SWE-bench, Qwen3-Coder under mini-SWE-agent can take 8-10 minutes and 50-60 LLM calls per task. — search summary of [arXiv 2606.01326](https://arxiv.org/pdf/2606.01326) (attribution to that paper is uncertain)
- A simple 0-to-1 CLI generation task: mini-SWE-agent with GPT-5.4 averaged 2.33 steps and under 21k tokens per task ($0.09). — [arXiv 2604.06742](https://arxiv.org/pdf/2604.06742) (via search summary)
- SWE-bench blog: average cost depends heavily on runtime limits such as the step limit. It reports about 30 cents per instance at maximum performance in its mini-SWE-agent roulette comparison. mini-SWE-agent scores about 65% on SWE-bench Verified. — [SWE-bench blog: mini-roulette](https://www.swebench.com/post-250820-mini-roulette.html); [SWE-bench leaderboards](https://www.swebench.com/)
- OpenHands with o3-mini on SWE-bench Verified: 43.7% resolved, $314 for the full run (about $0.63 per instance over 500 instances; early-2025 data). — [Xingyao Wang on X](https://x.com/xingyaow_/status/1886191968557949098)
- GPT-5-mini produces trajectories with an order of magnitude more tokens than other models on SWE tasks. A cheap per-token price can be cancelled by token volume. — [CodeScout, arXiv 2603.05744](https://arxiv.org/pdf/2603.05744) (via search summary)
- Unverified secondary claims: a median SWE-bench Verified task cost of $3.26 across 242 runs, range $0.08-$32. A median OpenHands+Opus resolved issue of $1.80-$3.50, with P90 at $12-$22. No primary sources found for either. — [futureagi blog](https://futureagi.com/blog/best-ai-gateway-openhands-swe-agent-autonomous-workflows-2026/) and related search results
- On one 20-problem research run, two problems carried 43% of the spend: cost is tail-dominated. — Anthropic claude-api skill reference, `shared/cost-optimization.md` (local: /tmp/claude-0/bundled-skills/2.1.319/d22f000285ef52dc44a14eac78e384b5/claude-api/shared/cost-optimization.md)
- Claude 4.7+ models use a newer tokenizer that produces about 30% more tokens for the same text. Token counts measured on older models undercount current ones. — [Anthropic pricing](https://platform.claude.com/docs/en/about-claude/pricing)

### Inferences
- **Calibrated token profiles for Queue (assumptions).** Each turn re-sends the whole conversation, so cumulative input ≈ turns × average context. Content written once per turn becomes cache writes; everything older is cache reads. Calibration: the large profile reproduces the $4.77 per instance for Sonnet 4.6/OpenHands at about 120 steps (my model gives $6.51 for Sonnet 4.6 at L, about the same order). The Claude Code sample session matches the small profile almost exactly ($0.55 vs $0.77).

| Profile | Turns | Uncached input | Cache writes | Cache reads | Output (incl. thinking) | Cache-read share of input |
|---|---|---|---|---|---|---|
| S: small bugfix or config change | 15 | 5k | 60k | 500k | 25k | 88% |
| M: feature or multi-file fix with tests | 40 | 10k | 150k | 2.5M | 60k | 94% |
| L: refactor or hard bug, compaction | 120 | 20k | 400k | 9.0M | 150k | 96% |

- **Cost of one execution attempt (sync API, 5-min cache).** Prices from the table above; GPT-5.5 uses secondary-source prices ($5 / $0.50 cached / $30), with no write premium assumed:

| Model | S, no cache | S, cached | M, no cache | M, cached | L, no cache | L, cached | Output share of cached cost |
|---|---|---|---|---|---|---|---|
| Opus 5.5 | $2.76 | $0.92 | $11.84 | $2.49 | $40.68 | $6.88 | 44-54% |
| Sonnet 5.5 | $1.38 | $0.46 | $5.92 | $1.25 | $20.34 | $3.44 | 44-54% |
| Haiku 5.5, if prompts stay ≤100k | $0.07 | $0.03 | $0.30 | $0.07 | $1.02 | $0.22 | 35-49% |
| Haiku 5.5, if prompts are >100k | $0.34 | $0.13 | $1.48 | $0.37 | $5.08 | $1.08 | 35-49% |
| Sonnet 4.6 (reference) | $2.07 | $0.77 | $8.88 | $2.24 | $30.51 | $6.51 | 35-49% |
| GPT-5.5 (secondary) | $3.58 | $1.32 | $15.10 | $3.85 | $51.60 | $11.10 | 41-57% |

- With caching on and cache reads at 0.05x (Opus 5.5 and Sonnet 5.5), the biggest remaining cost driver is **output and thinking tokens**, then cache writes. Effort level, output verbosity and turn count matter more than input size.
- Haiku 5.5 pricing jumps 5x per request once the prompt exceeds 100k tokens, and cache reads count toward that threshold. Haiku is very cheap only if Queue keeps executor contexts under 100k, for example with subagents or compaction at about 90k.
- Cost should be budgeted by P90, not median. With tail-heavy distributions, a per-task hard cap and a task budget are needed. Plausible P90/median ratio: 4-6x, based on the unverified OpenHands figures and Anthropic's 43%-from-2-problems observation.

### Gaps
- I found no primary 2026 dataset (official SWE-bench trajectory release or leaderboard cost column) giving input, output and cache split per resolved issue for current models. The SWE-bench site's per-instance cost and step data could not be extracted.
- I found no ccusage aggregate statistics: no public distribution of cache-read share or daily cost across users. ccusage reports cache read and write separately, but I found no published population data ([ccusage npm](https://npmjs.com/package/ccusage)).
- I found no measured OpenHands or Aider cost reports for the current models (Opus 5.5, Sonnet 5.5, Haiku 5.5, GPT-5.5).

---

## 2. How prompt caching changes cost in long agent loops

### Takeaway
On Anthropic, caching cuts a medium agent task's cost by about 4-5x: M on Sonnet 5.5 drops from $5.92 to $1.25. Writes cost 1.25x (5 min) or 2x (1 h) and reads 0.05-0.1x. In a sync loop, turns are seconds to a few minutes apart, so the 5-minute TTL stays warm for free and the 1-hour TTL only pays when gaps between turns exceed 5 minutes, for example long test runs or batch-based turns. OpenAI and Gemini cache implicitly with no write premium (secondary sources). Anthropic's explicit model gives the cheapest reads on Opus 5.5 and Sonnet 5.5 (0.05x) but needs correct breakpoint placement.

### Cited Findings
- Cache multipliers: 5m write 1.25x, 1h write 2x, read 0.1x base input. Reads are 0.05x on Opus 5.5 and Sonnet 5.5 and 0.025x on Fable 5.1 and Mythos 5.1. Caching pays off after one read for 5m and after two reads for 1h. These multipliers stack with the batch discount and data residency. — [Anthropic pricing](https://platform.claude.com/docs/en/about-claude/pricing)
- A cache read refreshes the TTL at no extra cost. The lifetime is measured from the start of the request, so generation time counts against it. Requests that share a prefix and start less than 5 minutes apart keep the 5m cache warm indefinitely. The 1h TTL is "the only window where the 2x write pays off" for gaps of 5-60 minutes. — Anthropic claude-api skill, `shared/prompt-caching.md` (local: /tmp/claude-0/bundled-skills/2.1.319/d22f000285ef52dc44a14eac78e384b5/claude-api/shared/prompt-caching.md)
- Keep-alive alternative: re-send the previous request with `max_tokens: 0` shortly before expiry, billed only as a cache read. Anthropic says this often beats the 1h TTL where reads are very cheap. It is not allowed inside Message Batches. — same skill file; [Anthropic batch docs](https://platform.claude.com/docs/en/build-with-claude/batch-processing)
- Max 4 breakpoints. Render order is tools → system → messages. Any byte change invalidates everything after it. Top-level automatic caching moves the breakpoint forward as the conversation grows. Recommended for agent loops: one explicit breakpoint on the static system prefix plus automatic caching for the conversation tail. — same skill file
- Caches are model-scoped, so a multi-model cascade gives up cross-model cache reuse. A mid-conversation top-level `effort` change invalidates the message cache unless the per-message effort system message is used. — Anthropic claude-api skill (SKILL.md, Thinking & Effort section)
- Context editing (clearing old tool results) rewrites the cached conversation. "In the run measured for the platform docs, context editing cost more than it saved." — Anthropic claude-api skill, `shared/cost-optimization.md`
- Claude Code distinguishes cache misses from "expected rebuilds" (compaction or tool-result clearing). It shows a 1h TTL in its status example, and the first message after a break longer than the TTL re-processes the full context. — [Claude Code docs: Manage costs](https://code.claude.com/docs/en/costs)
- Worked example on the pricing page (Opus 5.5): 50k input and 15k output cost $0.50. With 40k of the input as cache reads, the cost is $0.348 before session runtime. — [Anthropic pricing](https://platform.claude.com/docs/en/about-claude/pricing)
- OpenAI GPT-5.5 (secondary): $5.00 input, $0.50 cached input (0.1x), $30 output. Caching is automatic with no write premium. Prompts over 272k tokens bill at 2x input and 1.5x output for the whole session. — [apidog GPT-5.5 pricing](https://apidog.com/blog/gpt-5-5-pricing/); [economize.cloud](https://www.economize.cloud/resources/open-ai/pricing/gpt-5.5/)
- Gemini 3 Flash (secondary): $0.50 input, $0.05 cached (0.1x), $3.00 output. Explicit context caching adds storage at $1.00 per 1M tokens per hour. Search results did not confirm implicit caching from an official page. — [Puter Gemini pricing](https://developer.puter.com/tutorials/gemini-api-pricing/); [Google pricing page (snippet only)](https://ai.google.dev/gemini-api/docs/pricing)

### Inferences
- For Queue's executor loop, use 5m TTL caching, which is free to maintain at sync speed. Switch to a 1h TTL only on the system/tools breakpoint, or when a tool call such as a long test suite regularly exceeds about 4 minutes, since generation time and tool time both count.
- Keep the system prompt, tool definitions and repo-level context byte-stable across all tasks of a repo. This gives cache hits across tasks for the shared prefix: first task writes, later tasks read. No timestamps or task IDs go before the breakpoint.
- Avoid per-turn context editing. Prefer compaction at a high threshold, or subagents for verbose operations such as test output.
- Measure: `cache_read_input_tokens` should be at least about 90% of input on M and L tasks. If it is under 80%, a cache-breaker is at work.
- Provider comparison for loops: Anthropic Sonnet 5.5 reads ($0.10) are 5x cheaper than GPT-5.5 cached input ($0.50) per secondary prices. That makes Sonnet 5.5 notably cheaper per long task even before considering quality: M costs $1.25 vs $3.85.

### Gaps
- Official OpenAI and Gemini cache rules (minimum prefix, retention time, implicit-cache discount, whether OpenAI extended retention costs extra) could not be verified because the docs were blocked.
- Mistral's caching mechanics and price were not confirmed ("Batch and caching stack: not stated" per [developersdigest](https://www.developersdigest.tech/models/cost-levers/mistral)).

---

## 3. Batch APIs: usable for multi-turn tool loops? Which pipeline steps fit?

### Takeaway
All four providers offer about 50% off for async batch with up to 24h turnaround. A batch request is single-shot: an agent loop on batch means one batch job per turn. That is technically possible, but latency compounds (minutes to hours per turn, 24h worst case). Cache hits become best-effort (30-98%), and below about 90% hit rate a batch-per-turn loop costs **more** than a cached sync loop. Batch fits single-shot steps perfectly: planning, diff review, OnEmptyQueue analysis, summarization and test-idea generation. For the executor loop, keep sync calls with caching. On OpenAI, "Flex" processing (50% off, synchronous, variable latency; secondary source) is the one option that gives batch pricing to a tool loop.

### Cited Findings
- Anthropic Message Batches: 50% off. Most batches finish in under 1 hour, and results are available when all requests are done or after 24h. Requests expire unbilled if not processed within 24h. Limits are 100,000 requests or 256 MB per batch, and results are kept 29 days. — [Anthropic batch docs](https://platform.claude.com/docs/en/build-with-claude/batch-processing)
- Batch supports tool use (including server tools) and multi-turn conversations as input. `stream`, fast mode (`speed`), server-side `fallbacks` and `max_tokens: 0` are not supported. — [Anthropic batch docs](https://platform.claude.com/docs/en/build-with-claude/batch-processing)
- Caching in batch: discounts stack, but "because batch requests are processed asynchronously and concurrently, cache hits are provided on a best-effort basis. Users typically experience cache hit rates ranging from 30 percent to 98 percent." Anthropic recommends the 1-hour cache duration for batches with shared context. — [Anthropic batch docs](https://platform.claude.com/docs/en/build-with-claude/batch-processing)
- Anthropic guidance: 50% off "every token in the request, including cache reads and writes". Batch requests are single-shot, with no mid-batch tool loop. A tool loop "can sometimes be flattened into one batchable request by pre-fetching its inputs up front". In Anthropic's cookbook example this "ran at roughly half the interactive config's cost", but "the flattened run held its pass rate less firmly". Batch is not available for Managed Agents sessions. — Anthropic claude-api skill, `shared/cost-optimization.md` §2.5
- Message Threads (beta, server-held conversation state for loops) cannot be combined with Message Batches. — Anthropic claude-api skill (SKILL.md, Message Threads)
- OpenAI Batch: 50% off (GPT-5.5 at $2.50 / $15.00), under 24h. **Flex processing** has the same 50% discount but runs synchronously with variable latency and possible resource-unavailable errors. Sources conflict on whether cached input stacks: $0.25 per one source, about $1.25 per another. — [apidog](https://apidog.com/blog/gpt-5-5-pricing/); [economize.cloud](https://www.economize.cloud/resources/open-ai/pricing/gpt-5.5/); [baeseokjae batch/flex guide](https://baeseokjae.github.io/posts/gpt-5-5-batch-flex-pricing-guide-2026/) (all secondary)
- Gemini Batch: 50% off on all models (Gemini 3 Flash $0.25 / $1.50 batch). Whether batch and context caching combine is disputed: one calculator says they are mutually exclusive, unverified. — [getmaxim Gemini pricing](https://www.getmaxim.ai/articles/gemini-api-pricing-in-2026-and-how-to-cut-what-you-pay/); [Puter](https://developer.puter.com/tutorials/gemini-api-pricing/) (secondary)
- Mistral Batch: 50% off, requests queued over a 24h window, up to 1M requests per batch. Stacking with caching is not stated. — [maximem Mistral pricing](https://www.maximem.ai/blog/mistral-api-pricing-2026); [developersdigest](https://www.developersdigest.tech/models/cost-levers/mistral) (secondary)

### Inferences
- **Batch-per-turn loop, worked example (M task, Sonnet 5.5, 1h TTL, 50% off; on a miss the whole prefix is rewritten at the 1h write price):**

| Cache hit rate across turns | Cost | vs sync cached ($1.25) |
|---|---|---|
| 100% | $0.73 | -42% |
| 90% | $1.22 | about the same |
| 70% | $2.20 | +76% |
| 50% | $3.17 | +154% |
| 30% | $4.15 | +232% |

  Opus 5.5: $1.47 at 100% hits, $4.39 at 70%, $8.29 at 30%, vs $2.49 sync. Batch-per-turn only wins if hit rate stays above about 90%. Anthropic only promises 30-98%, and a 40-turn task with minutes-to-hours per turn could take 10-40 hours of wall time. **Not recommended for the executor loop**, even with overnight tolerance, except as an experiment that measures the real hit rate.
- One middle path for batch-per-turn: submit turn N+1 of **many tasks in parallel** in one batch (all tasks advance one step per batch cycle). Throughput is fine when hundreds of tasks run, but per-task latency is still turns × batch latency, and the cache hit rate is still best-effort. I found no published production pattern ("batch agents") with measured results, only Anthropic's flatten-the-loop cookbook remark.
- **Steps that fit batch (single-shot, not latency-critical):**

| Step | Fit | Notes |
|---|---|---|
| OnEmptyQueue analysis (repo scan, propose tasks) | Excellent | Fully async. Use the 1h cache on the shared repo context if several analysis prompts run on the same snapshot. |
| Planning (task → plan) | Good | Prefetch the repo map and relevant files deterministically, then one shot. Adds up to hours of latency before execution, acceptable overnight. |
| Review of the finished diff | Good | One shot: diff, touched files, guidelines. Can run as a nightly batch over all finished tasks. |
| Summaries, PR descriptions, changelogs | Excellent | Trivial single-shot. |
| Test-case ideas or spec generation | Good | Single-shot. Running the tests is a separate tool step. |
| Execution loop (read/edit/test) | Poor | Multi-turn. Use sync calls with caching, or OpenAI Flex (secondary-source, unverified). |
| Fix loop after review rejection | Poor | Same as execution. |

- Single-step cost estimates (my token assumptions: plan 40k in / 6k out, review 35k / 3k, OnEmptyQueue 150k / 10k, summary 20k / 1k; uncached):

| Step | Opus 5.5 sync / batch | Sonnet 5.5 sync / batch | Haiku 5.5 sync / batch |
|---|---|---|---|
| Plan | $0.28 / $0.14 | $0.14 / $0.07 | $0.007 / $0.004 |
| Review | $0.20 / $0.10 | $0.10 / $0.05 | $0.005 / $0.003 |
| OnEmptyQueue | $0.80 / $0.40 | $0.40 / $0.20 | $0.10 / $0.05 (>100k tier) |
| Summary | $0.10 / $0.05 | $0.05 / $0.025 | $0.003 / $0.001 |

  These steps cost 5-20% of an M-task execution loop. Batching saves real but second-order money. **The execution loop is where cost lives**, so model choice, effort level and turn count there dominate.

### Gaps
- I found no published measurements of a batch-driven multi-turn coding agent (pass rate, wall time, real cache hit rate).
- OpenAI Flex availability per model, its latency distribution, and whether cached-input discounts stack with it are unverified (docs blocked).
- Gemini: whether implicit or explicit caching discounts apply inside Batch mode is unverified.

---

## 4. Planner/executor split and model routing: quality, savings, failure modes; cheap-model coding performance

### Takeaway
Evidence for "strong planner + cheap executor" is real but modest. Anthropic's advisor pattern (Sonnet executor, Opus advisor) gave +2.7 points at -11.9% cost on SWE-bench Multilingual. Aider's architect mode historically traded a few points of score for about 15-20% lower cost. Anthropic's own guidance says the advisor "landed about on the executor model's own effort curve", so test "one model at lower effort" first. The new cheap tier is very cheap: Haiku 5.5 is 20x cheaper than Sonnet 5.5 per token and reportedly scores 64.8 vs 81.3 on SWE-bench Pro. Failure modes: cheap executors take more steps and fail on the hard tail, consult rates collapse at low effort, and cascades lose cache reuse.

### Cited Findings
- Advisor strategy: a Sonnet executor with an Opus advisor scored 2.7 points higher than Sonnet alone on SWE-bench Multilingual (74.8% vs 72.1%) and cut cost per agentic task by 11.9%. A Haiku executor with an Opus advisor scored 41.2% on BrowseComp vs 19.7% solo, trailing Sonnet solo by 29% but costing 85% less per task. Methodology: 1,266 problems, one attempt each. — [claude.com: The advisor strategy](https://claude.com/blog/the-advisor-strategy); [builder.io](https://www.builder.io/blog/the-claude-advisor-pattern)
- Anthropic caveats on the advisor: "The consult rate is the fragile variable - lowering effort can drop a pairing from consulting on most tasks to almost none, and then it scores below the executor alone." On Anthropic's coding benchmark "the flagship pairing landed about on the executor model's own effort curve - the advisor bought roughly what more effort did". Anthropic recommends sweeping effort and pricing the stronger model alone before adding an advisor. — Anthropic claude-api skill, `shared/cost-optimization.md`
- Anthropic: price candidates per completed task. "the frontier model at `low` effort out-solved a smaller model for less per solved task on one benchmark". On a saturated coding subset, the tier below frontier "matched the frontier model for a fraction of the cost". The smallest model "fits high-volume work with checkable outputs, not long agentic loops". — same file
- Claude Code docs: "Sonnet handles most coding tasks well and costs less than Opus. Reserve Opus for complex architectural decisions"; use Haiku for simple subagent tasks; "Use Sonnet for teammates". — [Claude Code docs: Manage costs](https://code.claude.com/docs/en/costs)
- Aider architect mode (secondary, figures from 2025 leaderboard entries): o3 (high) planner + GPT-4.1 editor scored 78.2% at $17.55 per run (225 problems) vs o3 alone at 81.3% and $21.23. DeepSeek R1 + Sonnet editor scored 64.0% at $13.29. DeepSeek runs the full polyglot benchmark for under $1.50 vs $10-146 for frontier OpenAI models. gpt-5 (high) tops the board at 88.0% and $29.08. The leaderboard may not have been updated since late 2025. — [steel.dev Aider leaderboard mirror](https://leaderboard.steel.dev/leaderboards/aider/); [developersdigest](https://www.developersdigest.tech/benchmarks/aider-polyglot) (secondary)
- Claude Haiku 5.5 (reported from Anthropic launch materials by a secondary blog, not independently verified): SWE-bench Pro 64.8, SWE-bench Multilingual 83.7, SWE-bench Multimodal 30.7, all at max effort. Sonnet 5.5: SWE-bench Pro 81.3, Multilingual 90.3. — [apidog: Claude Haiku 5.5 benchmarks](https://apidog.com/blog/claude-haiku-5-5-benchmarks/)
- SWE-bench Pro accepted wrong answers 8.5% of the time per a May 2026 DeepSWE analysis, so Pro scores carry grading noise. — [machineherald](https://machineherald-io.pages.dev/article/2026-05/29-deepswe-benchmark-puts-gpt-55-first-exposes-systematic-grading-errors-in-swe-bench-pro-and-flags-claude-opus-for-benchmark-exploitation)
- Opus 4.7 is reported at 64.3% on SWE-bench Pro. SWE-bench Verified scores cluster near 80% and are considered saturated or contaminated. — [vantaige SWE-bench Pro explained](https://vantaige.io/blog/swe-bench-pro-explained-2026) (secondary)
- Devstral 2 (Mistral, open-weight) is reported at 72.2% SWE-bench Verified and Devstral Small 2 (24B) at 68.0%. Price is about $0.40 / $2.00 per M tokens (Small 2: $0.10 / $0.30); sources conflict. — [baeseokjae Devstral 2 review](https://baeseokjae.github.io/posts/devstral-2-review-2026/); [flatkey Mistral pricing](https://flatkey.ai/blog/mistral-api-pricing) (secondary). The original Devstral scored 46.8% at $0.1 / $0.3 — [Mistral: Devstral](https://mistral.ai/news/devstral/)
- Gemini 3 Flash costs $0.50 / $3.00 per M tokens (secondary; see Q2). No verified current SWE-bench figure was found.
- Cheap models can use far more tokens or steps: GPT-5-mini produced about 10x more tokens ([CodeScout](https://arxiv.org/pdf/2603.05744)), and Qwen3.5 took the most steps with the lowest resolve rate ([SWE-Bench ProMax](https://arxiv.org/pdf/2608.09802)).
- Task budgets (beta, on Opus 5.5, Sonnet 5.5 and Haiku 5.5): on coding, "pass rate fell a few points as the budget tightened while cost per task fell by a much larger share". — Anthropic claude-api skill, `shared/cost-optimization.md`

### Inferences
- **Routing recommendation for Queue (hypothesis to validate on Queue's own eval set):**
  - Planning: Opus 5.5 (or Sonnet 5.5 at high effort) via **batch**. Cost under $0.15, and plan quality decides whether the executor wastes turns.
  - Execution loop: **Sonnet 5.5 at medium effort** as default (M about $1.25, sync, cached). Try **Haiku 5.5** as executor for tasks the planner labels "small or mechanical", keeping context ≤100k: M is about $0.07 on the ≤100k tier, roughly 18x cheaper, so even 3 failed Haiku attempts before escalating cost less than one Sonnet run.
  - Escalation: Haiku fails (tests red, or review rejects) → Sonnet 5.5 → Opus 5.5 for the hard tail. Because caches are model-scoped, an escalation restarts with a fresh cache write. Hand over a compact summary, not the full transcript.
  - Review: Sonnet 5.5 or Opus 5.5 via batch ($0.05-0.10). Don't use Haiku as the gatekeeper: a false accept costs far more than the $0.05 saved.
  - OnEmptyQueue: Sonnet 5.5 via batch, or Haiku 5.5 for a first-pass scan with Sonnet curating.
- Compare "Opus 5.5 at low effort" vs "Sonnet 5.5 at medium" vs "Haiku 5.5 at high" on 20-30 representative Queue tasks before hard-coding routing. Anthropic's own data shows the ranking flips by workload.
- Main failure modes of the planner/executor split: (1) a cheap executor diverges from the plan or loops, raising turns and tokens; (2) plans too abstract for the executor; (3) the consult or escalation trigger relies on the cheap model recognizing its own failure; (4) lost cache reuse on model switch; (5) the hard tail dominates cost anyway.

### Gaps
- I found no verified SWE-bench Verified or Pro numbers for Haiku 5.5 and Opus 5.5 from Anthropic primary pages (launch posts not fetched), and no current figures for Gemini Flash, DeepSeek or Qwen coder models.
- I found no independent 2026 study quantifying planner/executor savings on SWE-bench-like repos with current models.
- The current Aider leaderboard (with cost column) could not be accessed directly. Mirror data may be stale (late 2025).

---

## 5. Cost of failures: retries, fix loops, review rejections

### Takeaway
Expected cost per **successful** task = cost per attempt (execution + review + fix loops) ÷ success probability, plus the hidden cost of false accepts. Failed attempts are often more expensive than successful ones because they run to the turn or budget cap. With a 70% first-pass success rate, effective cost is about 1.4-1.6x the single-attempt cost. A cheap model with low success can still win because its per-attempt cost is 10-20x lower, as long as there is a reliable failure signal (tests or a strong reviewer) and escalation.

### Cited Findings
- "A cheaper model that fails still bills its tokens, then the retry, then whatever the failure costs downstream". Judge "cost per completed task, not per request". — Anthropic claude-api skill, `shared/cost-optimization.md` and SKILL.md
- "Price the tail, not the median... the bill is decided by the tasks the cheap model fails". Two of 20 problems carried 43% of spend. — Anthropic claude-api skill, `shared/cost-optimization.md`
- Anthropic warns the advisor pattern can score below the executor alone when the consult rate collapses. Self-recognition of hard cases "demands the very judgment it's missing". — same file
- Expired batch requests are not billed. — [Anthropic batch docs](https://platform.claude.com/docs/en/build-with-claude/batch-processing)
- Claude Code recommends plan mode, verification targets (tests) and early course correction to avoid "expensive re-work when the initial direction is wrong". — [Claude Code docs: Manage costs](https://code.claude.com/docs/en/costs)

### Inferences
- **Formula (assumes independent retries until success, no cap):** E[cost per success] = (C_plan + C_exec + C_review + r·C_fix) / p. Here p is the end-to-end acceptance probability and r the expected number of fix loops per attempt. Failed attempts usually hit the turn cap, so C_exec(fail) ≈ 1.5-2x C_exec(success). That is an assumption, consistent with long-tail cost observations.
- **Worked example, M task (success probabilities are illustrative assumptions, not measured):**
  - Sonnet 5.5 only: plan $0.07 (Opus batch would be $0.14) + exec $1.25 + review $0.05 (batch) + 0.3 fix loops × $0.40 = $1.49 per attempt. With p = 0.75 and failed attempts costing 1.5x: E ≈ 1.49 + 0.25 × (1.5 × 1.49) / 0.75 ≈ $2.24 per accepted task. Simple form: $1.49 / 0.75 = $1.99. Realistic range: $2.00-2.25.
  - Haiku-first cascade: Haiku attempt ($0.004 plan share + $0.07 exec + $0.05 Sonnet review + fix $0.02 ≈ $0.15), p_H = 0.45 (assumption). On failure, escalate to the Sonnet route ($1.49, p = 0.75). E ≈ 0.15 + 0.55 × 1.99 ≈ **$1.24** per accepted task, about 40% cheaper than Sonnet-only. This holds only if the reviewer and tests reliably catch Haiku's failures.
  - Opus 5.5 only: $0.14 + $2.49 + $0.10 + 0.2 × $0.80 = $2.89 per attempt, p = 0.85 → about $3.40. Opus pays off only if its higher p is real on Queue's tasks and human review time matters.
- **Large tasks (L):** Sonnet ≈ $3.44 per attempt, and p may fall to about 0.5 → about $7-8 per success. Here, a better plan (Opus via batch, $0.14) and decomposing into M-sized subtasks are the strongest levers.
- Hidden cost: a false accept (reviewer misses a bug) is not in API cost but in human time and regressions. Keep the review on a strong model, and use tests as the primary oracle.
- Use hard caps per attempt (turns, task_budget, max $ per task) so failed attempts cannot run into the $20+ tail.

### Gaps
- I found no public data on real first-pass acceptance rates, fix-loop counts or review-rejection rates for autonomous coding queues. All p values above are assumptions.
- I found no measured ratio of failed-attempt cost to successful-attempt cost.

---

## 6. Subscription vs API: break-even volume

### Takeaway
List prices (secondary sources, Nov 2026): Claude Pro $20, Max $100 (5x Pro) and Ultra $200 (renamed from Max 20x around Oct 30-31, 2026); ChatGPT Pro tiers of $100, $200 and $500 (sources conflict); Google AI Ultra $99.99 and $199.99. At the API costs above, $100-200 per month equals about 80-160 medium Sonnet 5.5 tasks, or 40-80 Opus 5.5 tasks. A Queue running more than about 5 M-tasks per night would exceed Max-level API spend. But subscriptions have rolling 5-hour and weekly caps and are intended for interactive use. Using them as the backend of a self-hosted autonomous queue is a terms-of-service and reliability risk (not verified). Recommendation: API, with batch and caching, as the primary path. A subscription is only an optional supplement for human-driven Claude Code use.

### Cited Findings
- Claude subscriptions (Pro, Max, Ultra, Team, Enterprise) include usage within plan limits. Team and Enterprise seats use an allowance that resets on a rolling 5-hour window and a weekly window, shared with chat and Cowork. Usage credits let users continue past limits at API-like metering. — [Claude Code docs: Manage costs](https://code.claude.com/docs/en/costs)
- Claude Pro $20, Max $100 (5x Pro per 5h window and per week), Ultra $200 (renamed from Max 20x on Oct 30 or 31, 2026; 10x Pro weekly usage, no 5-hour limit for verified accounts). One source says Max includes a monthly API credit ($100 on Max, $200 on Ultra) for API calls, not interactive sessions. — [morphllm comparisons](https://www.morphllm.com/comparisons/chatgpt-vs-claude-vs-gemini); [tech-insider](https://tech-insider.org/chatgpt-vs-claude-vs-gemini-vs-grok-subscription-pricing-2026/); [arte.itlibra](https://arte.itlibra.com/en/articles/subscription-api-credits) (secondary, partly conflicting; the API-credit claim is unverified)
- ChatGPT Pro: $100, $200 and $500 tiers are reported, the $500 tier adding Ultrafast mode in Codex. From Oct 30, 2026, Pro $200's included Codex usage reportedly falls from 20x to 10x Plus. ChatGPT and Codex share one pool, extendable with credits. — [morphllm AI coding costs](https://www.morphllm.com/ai-coding-costs); [aionx limits](https://aionx.co/ai-comparisons/ai-chatbot-rate-limits-compared/) (secondary)
- Google AI Ultra: $99.99 (5x AI Pro), plus a $199.99 tier (20x). — [threatfrontier](https://threatfrontier.com/articles/google-ai-ultra-vs-claude-max-vs-chatgpt-pro-developers) (secondary)
- API credits bought separately do not change subscription usage limits, and vice versa, for both Claude and OpenAI. — [arte.itlibra](https://arte.itlibra.com/en/articles/subscription-api-credits) (secondary)
- Enterprise API benchmark: $150-250 per developer per month for interactive Claude Code use. — [Claude Code docs: Manage costs](https://code.claude.com/docs/en/costs)

### Inferences
- **Break-even (API cost per accepted task from Q5):**

| Route | API cost per accepted M task | Tasks per month equal to $100 | Equal to $200 |
|---|---|---|---|
| Haiku-first cascade | about $1.24 | about 80 | about 160 |
| Sonnet 5.5 only | about $2.00 | about 50 | about 100 |
| Opus 5.5 only | about $3.40 | about 30 | about 60 |

  At those volumes a flat plan looks cheaper on paper. However: (1) plan limits are opaque, rolling 5-hour and weekly windows that an overnight burst can exhaust; (2) batch (-50%) is not available through subscriptions, so the API batch path halves plan, review and OnEmptyQueue costs; (3) automated, unattended use from a self-hosted service may conflict with consumer terms (unverified); (4) no programmatic control of caching TTL, model routing or per-task budgets.
- Pragmatic setup: the API (Anthropic Console key) is the system of record for Queue. If the operator already has a Max or Ultra plan for interactive work, its included API credit, if it exists (unverified), can offset part of the API spend.
- Marginal economics: overnight Queue volume of 10 M-tasks per night ≈ 300 per month ≈ $370 (Haiku-first) to $600 (Sonnet-only) via API. That is above a $200 plan but buys predictable throughput, batch for non-loop steps, and no rate-window stalls.

### Gaps
- I could not access official pricing pages (claude.com/pricing, OpenAI, Google), so subscription prices and the Max-to-Ultra rename are from secondary sources with conflicting dates.
- I found no verified statement on whether Claude, OpenAI or Google subscription terms permit driving an autonomous, self-hosted task queue through subscription auth (for example Claude Code headless with OAuth). This needs checking against current consumer terms before use.
- I found no published token-equivalent of plan limits (how many API dollars a Max or Ultra week corresponds to).
