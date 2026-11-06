# Competitive Landscape: Agentic / Asynchronous "Background" Coding Agents and Task-Queue Tools (as of Nov 2026)

Method note: research was done on 2026-11-06 with web search. Direct page fetches were blocked by the session's network policy for cursor.com, jules.google, developers.openai.com and docs.github.com, so for those vendors the claims rest on search-result summaries of official pages (changelog/blog/docs URLs) plus secondary sources. The only primary doc read in full was Anthropic's Claude Code cloud sessions page (code.claude.com). Secondary sources (SEO comparison blogs, review sites) often disagree on prices, plan names and model versions. Those claims are marked **[secondary]**. Conflicts are noted inline.

## Q1: Which products exist in this space and what are their core feature sets?

### Takeaway
The market has split into three tiers:
1. **Vendor-hosted cloud agents bundled with a model subscription:** OpenAI Codex cloud, Claude Code cloud sessions, Google Jules, GitHub Copilot cloud agent, Cursor Cloud Agents.
2. **Autonomous-engineer / enterprise platforms:** Devin, Factory, Warp Oz, OpenHands Cloud, Tembo.
3. **Local/open-source multi-agent orchestrators on worktrees or containers:** Claude Squad, Conductor, Vibe Kanban, Sculptor.

Third-party "Claude Code as a background agent" wrappers had a hard 2026. Terragon shut down in Feb 2026 and pointed users to Claude Code Web and Codex Web. Roo Code was archived in May 2026. Vibe Kanban is reported as unmaintained. The vendors absorbed the category: cloud sandboxes, PR auto-fix, schedules/automations and mobile are now first-party features.

### Cited Findings

**OpenAI Codex (cloud)**
- At DevDay (Sept 29, 2026) OpenAI added reusable cloud environments. Each new task starts from a saved setup with dependencies already installed. Environments can be shared in a ChatGPT workspace, but colleagues can't see each other's tasks or edit the setup. Credentials go in env vars or "network secrets". Available on Plus, Pro, Business, Enterprise, Edu and Healthcare. — [TechCrunch](https://techcrunch.com/2026/09/29/openai-gives-codex-reusable-cloud-environments-that-work-across-devices/); [SiliconANGLE](https://siliconangle.com/2026/09/29/openais-codex-gets-reusable-cloud-environments-that-follow-developers-across-devices/)
- Reported limitations at that launch: no GitLab or self-hosted GitHub Enterprise Server, and no computer/browser use in cloud environments. This conflicts with an Aug 19, 2026 changelog entry for GitLab support in beta (start tasks from issues/MRs with @codex, MR reviews). Status unclear. — [TechCrunch](https://techcrunch.com/2026/09/29/openai-gives-codex-reusable-cloud-environments-that-work-across-devices/); [Codex changelog (via search summary)](https://developers.openai.com/codex/changelog)
- Changelog items via search summary:
  - Aug 25, 2026: ChatGPT scheduled tasks can fire on events in Gmail, Slack or GitHub.
  - July 2026: interactive forms in Codex tasks.
  - May 18, 2026: open completed Codex tasks from iOS notifications.
  - Source: [Codex changelog](https://developers.openai.com/codex/changelog)
- Usage limits are shared between local CLI/IDE use and cloud tasks. There is a rolling 5-hour window plus a weekly cap. OpenAI's pricing docs once gave "5–40 cloud tasks every 5 hours" for average users, with higher figures on higher tiers. Cloud tasks didn't count against limits until Oct 20, 2025. Auto-reviewing all PRs needs a Business plan with flexible pricing. — [knightli.com, summarizing OpenAI pricing docs [secondary]](https://knightli.com/en/2026/04/15/codex-usage-limits-five-hour-weekly-credits/); [inventivehq [secondary]](https://inventivehq.com/blog/codex-cli-usage-rate-limits)
- Users ask for a usage bar in the Codex app (GitHub issue). One user said a single PR plus a GitHub-connected review used about 20% of their weekly quota (anecdotal). — [openai/codex#24181](https://github.com/openai/codex/issues/24181); [OpenAI community thread](https://community.openai.com/t/pro-plan-hit-5-hour-limit-twice-in-2h-and-1-5h-and-nearly-exhausted-weekly-cap-in-1-day-after-today-s-update/1364782)

**Claude Code cloud sessions (formerly "Claude Code on the web"), plus Routines and GitHub App** (primary doc read in full)
- **Plans and cost:** Pro, Max, Ultra and Team, plus Enterprise premium or Chat+Code seats. Usage counts toward the plan's usage limits. "No separate charge for the cloud VM." — [Claude Code docs](https://code.claude.com/docs/en/claude-code-on-the-web)
- **Entry points:** browser (claude.ai/code), the Code tab in the mobile app, the Desktop app, terminal (`claude --cloud "task"`), and cloud scheduled tasks/routines. `claude -p "msg" --cloud <session-id>` queues a follow-up message into a running session, including from CI. `--teleport` pulls a cloud session into the local terminal. — [Claude Code docs](https://code.claude.com/docs/en/claude-code-on-the-web)
- **Environments:** cloud environments control network access level, env vars and setup scripts. A setup script is cached only if it finishes in about 5 minutes. Network secrets and git credentials stay outside the sandbox and are attached by a proxy. Self-hosted environments exist for orgs. — [Claude Code docs](https://code.claude.com/docs/en/claude-code-on-the-web)
- **Parallelism:** each `--cloud` call is its own independent session. "Projects" let one conversation coordinate many parallel cloud sessions across repos. Subagents and (experimental) agent teams work inside a session. — [Claude Code docs](https://code.claude.com/docs/en/claude-code-on-the-web)
- **Output and review:**
  - Changes are pushed to a branch, and a diff view supports inline line comments that are sent back to Claude.
  - A PR can be created from the UI.
  - **Auto-fix** subscribes to PR events (CI failures, review comments) and pushes fixes when they are clear. It asks the user when a comment is ambiguous, and it replies in review threads labelled as Claude.
  - It can't react to merge conflicts, because GitHub sends no webhook for them.
  - Source: [Claude Code docs](https://code.claude.com/docs/en/claude-code-on-the-web)
- **Planning:** the docs recommend planning locally in plan mode, committing the plan and running it in the cloud. Permission mode can be picked per cloud session. — [Claude Code docs](https://code.claude.com/docs/en/claude-code-on-the-web)
- **Sharing and team:** Team and Enterprise sessions can be set to Team visibility, with repo-access verification. Org settings include Quick setup, custom roles and the `allow_remote_sessions` policy. — [Claude Code docs](https://code.claude.com/docs/en/claude-code-on-the-web)
- **Limitations:** GitHub is required for cloning and creating PRs (GHES for Team/Enterprise). Other hosts work only as an upload bundle with no push-back. Sessions expire when inactive; the VM is reclaimed and background shell work and in-session scheduled tasks are lost. ZDR, HIPAA and CMEK orgs can't use cloud sessions. — [Claude Code docs](https://code.claude.com/docs/en/claude-code-on-the-web)
- **Routines:** saved prompt + repos + connectors that run on Anthropic cloud. They can be triggered by a cron schedule, an HTTP API call or GitHub events (pull_request, push, issues, check_run, workflow_run, release, merge_queue…). Webhook events have hourly caps in the research preview. — [DataCamp [secondary]](https://datacamp.com/tutorial/claude-code-routines); [Builder.io [secondary]](https://www.builder.io/blog/claude-code-routines); [ITdaily](https://itdaily.com/news/cloud/claude-code-gets-routines/)
- Claude Tag is an organization-managed @Claude in Slack that runs on the same cloud infrastructure. Slack sessions are shared with Team visibility automatically. — [Claude Code docs](https://code.claude.com/docs/en/claude-code-on-the-web)

**GitHub Copilot cloud agent (formerly "coding agent")**
- **Changelog items:**
  - Apr 1, 2026: implementation plans for approval before code, and deep research sessions; all paid plans.
  - Mar 19, 2026: starts work 50% faster.
  - Feb 18, 2026: code-referencing flags in session logs.
  - Feb 2026: network/firewall changes for self-hosted and larger runners.
  - Sources: [GitHub Changelog Apr 1](https://github.blog/changelog/2026-04-01-research-plan-and-code-with-copilot-cloud-agent/); [Mar 19](https://github.blog/changelog/2026-03-19-copilot-coding-agent-now-starts-work-50-faster/); [Feb 18](https://github.blog/changelog/2026-02-18-copilot-coding-agent-supports-code-referencing/); [Feb 13](https://github.blog/changelog/2026-02-13-network-configuration-changes-for-copilot-coding-agent/)
- **Intake:** GitHub Issues, Azure Boards, Raycast, Linear, Slack and Teams. Context from the conversation is carried over. You can choose Copilot or custom agents. — [GitHub Copilot agents page](https://github.com/features/copilot/agents)
- One field guide says external integrations (Azure Boards, Jira, Linear, Slack, Teams) only support direct PR creation, and that planning and deep research are only on GitHub.com. — [Agent Builder field guide [secondary, May 2026]](https://rkalani1.github.io/agent-builder-field-guide/platforms/copilot/)
- A blog claims the Linear integration went GA on Jul 23, 2026 with model selection, custom agents, branch control and mid-task steering (unverified). — [Vibecoding Academy [secondary]](https://www.vibecodingacademy.ai/blog/linear-ai-agent-github-copilot-cloud-agent-ga)
- **Billing:** tasks use both GitHub Actions minutes and premium requests. The agent runs in an ephemeral Actions-based environment. — [GitHub Copilot agents page](https://github.com/features/copilot/agents); [amux guide [secondary]](https://amux.io/guides/background-agents-compared/)
- **VS Code (May–Sept 2026):** an Agents window, session sync, side-by-side sessions and gutter feedback. A September release reportedly added "Automations" (scheduled or on-demand tasks) and "agent merge", which handles reviews, conflicts and checks. — [GitHub Changelog Jul 8](https://github.blog/changelog/2026-07-08-github-copilot-in-visual-studio-code-june-2026-releases/); [GHchangelog on X [secondary]](https://x.com/GHchangelog/status/2105745849556849018)

**Google Jules**
- **Scheduled Tasks:** daily, weekly or monthly recurring runs (dependency checks, lint fixes) that deliver PRs. They live in a Scheduled tab per repo, can be edited, paused and resumed, and are available to everyone. — [Google blog: Jules proactive updates](https://blog.google/innovation-and-ai/technology/developers-tools/jules-proactive-updates/); [Jules changelog (via search)](https://jules.google/docs/changelog/)
- **Proactive features:**
  - **Suggested Tasks** scans the codebase for improvements (initially Pro/Ultra only).
  - Jules auto-fixes failing GitHub Actions on its own PRs.
  - A **Planning Critic** agent reviews plans that run without human approval.
  - Source: [Google blog](https://blog.google/innovation-and-ai/technology/developers-tools/jules-proactive-updates/); [HackerNoon [secondary]](https://hackernoon.com/googles-jules-starts-surfacing-work-on-its-own-signaling-a-shift-in-ai-coding-assistants)
- **Limits [secondary, figures conflict]:** Free is 15 tasks per rolling 24h with 3 concurrent. Pro is 100/day with 15 concurrent. Ultra is 300/day with 60 concurrent; Ultra's price is given as both $99.99 and $124.99. — [morphllm](https://www.morphllm.com/comparisons/jules-google-coding-agent); [danilchenko.dev](https://www.danilchenko.dev/posts/google-jules-review)
- "Project Jitro" was previewed at I/O 2026 as Google's next-generation coding agent. — [danilchenko.dev [secondary]](https://www.danilchenko.dev/posts/google-jules-review)

**Cursor Cloud Agents (formerly Background Agents)**
- They were renamed from Background Agents and run in isolated VMs. They can be started with @cursor in Slack, or with @cursor or by assigning a Linear issue. Linear shows live status, and a PR is created automatically when the agent finishes. — [Cursor docs (via search)](https://cursor.com/docs/cloud-agent); [Cursor help](https://cursor.com/help/ai-features/background-agents); [Cursor blog](https://cursor.com/blog/cloud-agents)
- **Automations** (Mar 2026) start cloud agents on a schedule or on events from source control, Slack, webhooks, Linear, Sentry or PagerDuty. An Aug 19, 2026 update lets agents subscribe to PRs, Slack threads and cron schedules, and spawn subagents on isolated VMs. — [spotdev [secondary]](https://www.spotdev.co.uk/blog/cursor-cloud-agents-and-automations); [explainx [secondary]](https://www.explainx.ai/blog/cursor-event-driven-cloud-agents-isolated-vms-august-2026)
- Other capabilities: multi-repo environments, video artifacts that prove changes work, a Cloud Agents API (public beta) and an SDK (public beta, Apr 29, 2026). Cursor's own warning is that agents have internet access by default and auto-run commands, which creates prompt-injection and exfiltration risk. — [buildfastwithai [secondary]](https://blog.buildfastwithai.com/cursor-cloud-agents-development-environments-2026); [Cursor docs (via search)](https://cursor.com/docs/cloud-agent)
- Reported parallelism is up to 8 parallel agents, one VM per task. — [ssojet / sinatra comparisons [secondary]](https://www.sinatra.dev/blog/best-background-coding-agents)

**Devin (Cognition)**
- **Pricing [secondary, conflicting]:** older figures are Core $20 plus $2.25 per ACU, and Team $500 for 250 ACUs. Newer figures (from about June 2026) are Free, Pro $20, Max $200 and Teams $80 + $40/seat, with usage quotas; ACUs are reportedly enterprise-only now. — [vp0](https://vp0.com/blogs/devin-ai-pricing-plans-2026); [Pensero](https://pensero.ai/blog/devin-pricing); [usagepricing](https://www.usagepricing.com/blueprint/cognition)
- **Features [secondary]:** Interactive Planning (approve the approach before coding), Devin Search, Devin Wiki, and Enterprise VPC deployment with SSO. — [Pensero](https://pensero.ai/blog/devin-pricing)
- **Linear (official docs):** assigning a ticket starts a session with the default playbook. Playbooks can be synced as Linear labels. Linear users can be mapped to Devin users for attribution. — [docs.devin.ai Linear](https://docs.devin.ai/integrations/linear.md)
- **Other mechanisms [secondary]:** @Devin in Jira or Linear comments starts a run. Persistent "Knowledge" entries are recalled when relevant. Playbooks are step-by-step specs. Docker snapshots give a ready environment. — [fast.io](https://www.fast.io/resources/devin-ai-integration-guide.md); [tools-34m guide](https://tools-34m.pages.dev/en/devin-ai-software-engineer-setup-guide-team-workspace-github-slack-en/)

**Factory (Droids)**
- **Pricing [secondary]:** Pro $20, Plus $100, Max $200, and Teams $60 + $40/seat. Usage is limited by rolling **5-hour, 7-day and 30-day windows**, and the model you choose changes how fast the allowance burns. "Droid Core" is a free pool of open-weight models used after standard usage runs out. — [kunavo](https://kunavo.com/guides/factory-droid-pricing); [saastruecost](https://saastruecost.com/guides/factory-droid-pricing-decoded/)
- **Features [secondary]:** desktop app (macOS/Windows, Apr 2026), CLI, SDK and CI; Factory Router picks the model per task; BYOK; "Missions" for multi-step work; managed "Droid Computers" on Plus and above. — [developersdigest](https://www.developersdigest.tech/blog/factory-droid-review-setup-2026); [aiagentslist](https://www.aiagentslist.io/agents/factory-droid)
- Funding: a reported $150M Series C at a $1.5B valuation led by Khosla. — [tech-insider [secondary]](https://tech-insider.org/factory-ai-150-million-series-c-khosla-coding-droids-2026/)

**Amp (Sourcegraph / Amp, Inc.)**
- **[secondary, conflicting]:** reportedly spun out as Amp, Inc. in Dec 2025, and reportedly CLI-only since the VS Code extension was killed in Mar 2026 (another source still lists VS Code, JetBrains and Zed). There is no user model picker; Amp picks the model per mode (smart/deep/rush) and BYOK is enterprise-only. There was an ad-supported free tier (ads possibly ended in Mar 2026) and a $20/month "Smart Mode". Team features include shared threads and leaderboards. Subagents are spawned automatically. — [rywalker.com](https://rywalker.com/research/amp-sourcegraph); [baeseokjae review](https://baeseokjae.github.io/posts/amp-code-review-2026/); [makerstack](https://makerstack.co/reviews/amp-review/)

**OpenHands (All Hands AI)**
- **Cloud:** Free tier ($0) includes Jira and Slack integrations. Enterprise is $40/user/month with Git, Slack and ticketing integrations. Self-hosted in a private VPC with BYO LLM key is "contact us". — [OpenHands pricing](https://hub.openhands.dev/pricing)
- Cloud works with Jira Cloud, Jira Data Center and Linear. You invoke it with @openhands in a ticket comment or with an `openhands` label. Enterprises can self-host OpenHands Cloud on Kubernetes. — [OpenHands docs: project management](https://docs.all-hands.dev/usage/cloud/project-management/overview); [OpenHands docs intro](https://docs.openhands.dev/overview/introduction)
- The core is MIT-licensed open source. "Agent Canvas" runs any ACP agent locally, in Docker, on a VM or in the cloud. — [Medium harness comparison [secondary]](https://roman-kryvolapov.medium.com/ai-agent-harnesses-compared-from-claude-code-to-openclaw-c95d5f4f6298)

**Warp Oz**
- Oz launched on Feb 10, 2026 as an orchestration platform for cloud coding agents: sandboxed environments, hundreds of agents in parallel, scheduled recurring workflows, and interactive or programmatic launch. — [Warp newsroom](https://www.warp.dev/newsroom/2026/2/10/warp-launches-oz-the-orchestration-platform-for-cloud-coding-agents); [Warp blog](https://www.warp.dev/blog/oz-orchestration-platform-cloud-agents)
- Since May 2026 it is **multi-harness**: it runs Claude Code, Codex and Warp Agent, coordinates subagents automatically, and has cross-harness "Agent Memory" owned by the org. — [Warp blog: multi-harness](https://www.warp.dev/blog/multi-harness-cloud-agent-orchestration); [It's FOSS](https://itsfoss.com/news/warp-oz-multi-harness-update/); [Warp docs: harnesses](https://docs.warp.dev/agent-platform/cloud-agents/harnesses/)
- Triggers are Slack, GitHub PRs, Linear issues and schedules. In Apr 2026 Warp said it would open-source the terminal and Oz (MIT/AGPL). — [rywalker [secondary]](https://rywalker.com/research/warp-oz)

**Augment Code**
- Remote Agents run autonomously from VS Code in configurable containers, handle parallel tasks and open PRs. GA was in VS Code, with JetBrains/vim lagging (date unclear, likely 2025). — [HackerNoon [secondary]](https://sia.hackernoon.com/agents-everywhereaugment-brings-async-coding-power-to-your-ide)
- The Auggie CLI has a headless `--print` mode for CI and GitHub Actions for PR review. One source says Harness acquired Augment's cloud agents and CLI in October 2026 (unverified, wording unclear). — [Medium harness comparison [secondary]](https://roman-kryvolapov.medium.com/ai-agent-harnesses-compared-from-claude-code-to-openclaw-c95d5f4f6298)

**Cline / Roo Code**
- The Roo Code extension repo was archived on May 15, 2026 after a pivot to cloud agents, and maintainers recommended going back to Cline. — [nexasphere [secondary]](https://nexasphere.io/blog/cline-vs-roo-code-in-2026-which-open-source-ai-coding-agent-should)
- Cline has no dedicated cloud-agent product. Background use means running the CLI headless with auto-approval or running a hub daemon. CLI 2.0, an SDK and JetBrains early access shipped in 2026. — [fast.io [secondary]](https://fast.io/resources/cline-background-agent/); [rywalker](https://rywalker.com/research/cline)

**Local / open-source parallel orchestrators**
- **Claude Squad:** terminal tool on tmux plus git worktrees, one isolated workspace per task. It wraps any CLI agent (Claude Code, Codex, Aider…). AGPL-3.0. — [Augment: 9 open-source orchestrators](https://www.augmentcode.com/tools/open-source-agent-orchestrators); [dev.to](https://dev.to/stravukarl/best-tools-for-managing-parallel-ai-coding-agents-in-2026-14l8)
- **Conductor (Melty Labs):** closed-source native macOS app. One git worktree per workspace, parallel agents and a diff review panel. — [crystl.dev](https://crystl.dev/blog/best-ai-agent-orchestration-tools/); [abralo](https://abralo.com/alternatives)
- **Vibe Kanban (Bloop):** kanban board (CLI + web UI) for managing coding agents. Apache-2.0. Reported as shut down by Bloop / community-run or unmaintained (sources conflict). — [Augment list](https://www.augmentcode.com/tools/open-source-agent-orchestrators); [nimbalyst](https://nimbalyst.com/blog/best-agent-management-tools-2026/)
- **Sculptor (Imbue):** desktop app using **containers rather than worktrees**. Changes merge into the local repo only on approval. MIT. Apple Silicon and Linux builds. — [Augment list](https://www.augmentcode.com/tools/open-source-agent-orchestrators); [agentsroom](https://agentsroom.dev/blog/best-multi-agent-coding-tools)
- **Terragon** ("use Claude Code as a background agent") **shut down on Feb 9, 2026**. All sandboxes were terminated, an unmaintained OSS snapshot (terragon-oss) was published, and users were pointed to Claude Code Web and Codex Web. — [terragonlabs.com](https://www.terragonlabs.com/); [Sawyer Hood on X](https://x.com/sawyerhood/status/1938263088836579536)
- **Tembo:** shut its earlier Tembo Cloud service and rebuilt as a control plane for third-party agents focused on autonomous software maintenance. Founded 2022 as managed Postgres, pivoted to agent orchestration in May 2025. — [rywalker: Tembo](https://rywalker.com/research/tembo); [Tembo docs: Agents](https://docs.tembo.io/features/agents)
- **Sweep:** the original open-source issue-to-PR bot, since pivoted to a JetBrains plugin. The bot is unmaintained. — [software-factories list](https://github.com/pranshuchittora/agent-qa-software-factories-20261031)
- Curated lists of orchestrators and "software factories" (autonomous, parallel, looping agent systems) show a long tail of new 2026 entrants (e.g. amux, Abralo, Crystal, Nimbalyst, crystl). — [awesome-agent-orchestrators](https://github.com/andyrewlee/awesome-agent-orchestrators); [awesome-cli-coding-agents](https://github.com/bradAGI/awesome-cli-coding-agents); [software-factories list](https://github.com/pranshuchittora/agent-qa-software-factories-20261031)

### Inferences
- Vendor consolidation has largely ended the "hosted wrapper around the user's Claude Code subscription" business (Terragon). An independent product like Queue can't win on "runs Claude Code in the background" alone. Its defensible ground is what vendors structurally won't do:
  - Self-hosting with data on the user's own infrastructure.
  - One queue across Claude Code, Codex CLI and Gemini CLI using the user's own subscriptions.
  - A strictly serialized, verified, commit-to-main pipeline.
  - Budget-aware scheduling across providers.
- Warp Oz (multi-harness) and Tembo (control plane for third-party agents) are the closest conceptual competitors to Queue's multi-CLI approach. Both are cloud/enterprise-focused rather than self-hosted per-user queues.

### Gaps
- Could not fetch Cursor, Jules, OpenAI Codex or GitHub docs directly (network policy blocked those hosts). Their details come from search summaries and secondary sources.
- No reliable 2026 info found on Codex's Slack/Linear integrations, best-of-N attempts, or the official Codex iOS app beyond notifications.
- No current primary info on Augment Remote Agents' 2026 status, or on Sculptor/Conductor feature changes in 2026.
- No 2026 data on newer entrants such as Kiro (AWS), Windsurf/Cognition merged agents or Gemini CLI's own background mode. Searches did not surface them.

## Q2: Feature-by-feature matrix (intake, parallelism, environment, output, review loop, planning, memory, scheduling, multi-model, limits, mobile, notifications, team)

### Takeaway
Across the hosted leaders, the common core is the same: start from a tracker or chat, isolated cloud VM/container, configurable setup, branch/PR output, and iteration from PR feedback. The differences lie in triggers and automation (schedules and event subscriptions), proactive work generation, multi-harness support, and how usage limits are exposed.

### Cited Findings
Matrix legend: Y = documented, P = partial/preview/secondary-only, ? = not found. Sources for each cell are in Q1. This table condenses them.

| Product | Intake channels | Parallelism / isolation | Env setup | Output | PR review loop | Plan/spec mode | Scheduled / event triggers | Multi-model | Limits model | Mobile / notif. | Team / enterprise |
|---|---|---|---|---|---|---|---|---|---|---|---|
| OpenAI Codex cloud | Web, CLI/IDE, GitHub @codex, GitLab @codex (beta, Aug 2026) | Y, parallel cloud tasks | Y, reusable saved environments (Sept 2026), env vars + network secrets | Branch/PR | P, PR review (auto-review all PRs needs Business) | ? | P, ChatGPT scheduled tasks on Gmail/Slack/GitHub events (Aug 2026) | OpenAI only | 5h + weekly windows shared with local use; credits | Y, iOS notifications open tasks (May 2026) | Y, workspace-shared environments |
| Claude Code cloud | Web, mobile app, desktop, CLI `--cloud`, follow-ups via `-p --cloud`, Slack (Claude Tag), routines API | Y, one VM per session; Projects coordinate many sessions | Y, environments: network levels, env vars, cached setup script (<~5 min), network secrets, self-hosted runners | Branch, PR from UI, diff with inline comments | Y, Auto-fix on CI failures and review comments | Y, plan mode (locally, then cloud) | Y, Routines: cron, HTTP API, GitHub events | Anthropic only | Shares plan usage limits (5h/weekly) | Y, mobile app Code tab | Y, Team visibility, roles, org policies, GHES |
| GitHub Copilot cloud agent | Issues, Azure Boards, Linear, Raycast, Slack, Teams, VS Code Agents window | Y, ephemeral Actions-based env | Y, Actions-based setup, firewall | Draft PR | Y, iterate via PR comments (each steer costs a premium request [secondary]) | Y, plan for approval + deep research (Apr 2026) | P, VS Code "Automations" + "agent merge" (Sept 2026, secondary) | Y, model selection, custom agents | Premium requests + Actions minutes | P, GitHub mobile (not verified) | Y, org instructions, Business/Enterprise policies |
| Google Jules | Web, GitHub | Y, 3–60 concurrent by tier | P, environment setup (details not verified) | PR | Y, auto-fixes failing CI on its own PRs | Y, plan approval + Planning Critic | Y, Scheduled Tasks (daily/weekly/monthly) + Suggested Tasks (proactive) | Gemini only | Tasks per 24h and concurrent caps per tier | ? | ? |
| Cursor Cloud Agents | IDE, web, Slack @cursor, Linear (assign/@cursor), GitHub, API/SDK | Y, isolated VMs, multi-repo, subagents | Y, environments (details unverified) | PR (auto on Linear), video artifacts | Y, can subscribe to PRs/Slack threads | P | Y, Automations: cron, Slack, webhook, Linear, Sentry, PagerDuty | Y, multi-vendor + Composer | Usage-based pricing (details not verified) | P | Y |
| Devin | Web, Slack, Linear (assign/label playbooks), Jira (@Devin), API | Y | Y, snapshots / machine setup [secondary] | PR | Y | Y, Interactive Planning | ? | P | Plan quota; ACUs for Enterprise | ? | Y, VPC, SSO |
| Factory Droids | Desktop, CLI, SDK, CI | Y, Missions | Y, managed Droid Computers | PR | ? | Y, Missions/spec | ? | Y, Router + BYOK | **5h / 7d / 30d windows**, model-dependent burn | ? | Y, Teams/Enterprise |
| Warp Oz | Warp app, API, Slack, GitHub, Linear, schedules | Y, hundreds in parallel; subagents | Y, sandboxed environments | PR | ? | ? | Y, schedules | **Y, multi-harness: Claude Code, Codex, Warp Agent** | ? | ? | Y, org-owned cross-harness memory |
| OpenHands Cloud | Web, CLI, Slack, Jira, Linear (@openhands / label), GitHub | Y, Docker/VM/cloud | Y | PR | Y (resolver, not verified 2026) | ? | ? | Y, BYO LLM | Free tier; $40/user Enterprise | ? | Y, self-host on K8s/VPC |
| Tembo | Control plane for third-party agents | Y | ? | PR | ? | ? | Y, maintenance focus | Y, third-party agents | ? | ? | Y |
| Claude Squad | Local TUI | Y, tmux + worktrees | Local | Local branch | N | N | N | Y, any CLI agent | User's own subscriptions | N | N |
| Conductor | macOS app | Y, worktree per workspace | Local | Branch, diff panel | P, local review | ? | N | Y, Claude Code/Codex | User's own subscriptions | N | N |
| Vibe Kanban | Kanban web UI + CLI | Y, worktrees | Local | Branch/PR | P | ? | N | Y, multiple CLI agents | User's own subscriptions | N | N (unmaintained) |
| Sculptor | Desktop app | Y, **containers** | Container | Merge on approval | P | ? | N | Claude Code focus | User's own subscription | N | N |

- Memory/rules files: Codex reads AGENTS.md (well established in 2025; not re-verified for 2026 here). Claude Code loads repo `.claude/agents/`, `.claude/settings.json` and CLAUDE.md in cloud sessions. Devin uses Knowledge plus Playbooks. Warp Oz offers org-owned cross-harness memory. — [Claude Code docs](https://code.claude.com/docs/en/claude-code-on-the-web); [fast.io on Devin](https://www.fast.io/resources/devin-ai-integration-guide.md); [Warp blog](https://www.warp.dev/blog/multi-harness-cloud-agent-orchestration)
- Benchmarks used in marketing (late 2026, secondary):
  - Artificial Analysis Coding Agent Index: Claude Opus 4.7 in Claude Code 66, GPT-5.5 in Codex 65, Cursor Composer 2.5 62.
  - Codex is reported leading Terminal-Bench 2.1 at 88.8%.
  - Source: [techsy.io / comparison summary [secondary]](https://techsy.io/en/blog/background-coding-agents-compared); [encore.dev](https://encore.dev/articles/best-ai-coding-agents)

### Inferences
- Queue's "token budgets per provider (5h/7d windows)" mirrors how OpenAI, Anthropic and Factory meter usage (5h + weekly, and Factory also 30d). A visible usage/budget meter is an explicitly requested missing feature in Codex (GitHub issue #24181). Making budgets first-class and visible in the UI is a real differentiator.
- Queue's sequential "verify, review agent, commit, next task" model differs from every hosted product. They are all PR-centric and parallel, and leave review to humans. Queue's built-in verification gate targets the main complaint in Q4 (the review bottleneck).
- "OnEmptyQueue" refill (code review, research, improvement proposals) has close analogs only in Jules Suggested Tasks plus Scheduled Tasks, Cursor Automations, Claude Code Routines and Tembo's maintenance focus. It is an emerging differentiator, not yet table stakes.

### Gaps
- Exact environment-caching/snapshot mechanics for Cursor, Jules and Devin could not be verified from primary docs.
- No confirmed data on mobile apps or push notifications for Jules, Devin, Factory or Cursor. Only Codex (iOS notifications) and Claude (mobile app) are confirmed.

## Q3: Which features are table stakes vs. differentiators?

### Takeaway
Table stakes in late 2026:
- Isolated cloud sandbox per task.
- Configurable environment (setup script, env vars, secrets, network policy).
- Several tasks in parallel.
- Branch/PR output with a diff view.
- Iterating on PR comments and CI failures.
- A plan-before-code option.
- Repo rules/memory files.
- GitHub integration.
- At least one chat/tracker intake (Slack and/or Linear).

Differentiators:
- Event- and schedule-driven automations, and proactive self-generated tasks.
- Multi-harness / multi-model orchestration.
- Visible usage-limit management.
- Self-hosting / BYO compute.
- Built-in automated verification and review gates before human review.
- Mobile-first steering.
- Non-GitHub git hosts.

### Cited Findings
- Isolated VM/sandbox per task is universal among hosted products: Codex cloud, Claude Code ("each session runs in an isolated, Anthropic-managed VM"), Cursor (isolated VMs), Copilot (ephemeral Actions env), Oz (sandboxed environments). — [Claude Code docs](https://code.claude.com/docs/en/claude-code-on-the-web); [amux](https://amux.io/guides/background-agents-compared/); [Warp newsroom](https://www.warp.dev/newsroom/2026/2/10/warp-launches-oz-the-orchestration-platform-for-cloud-coding-agents)
- Reusable, pre-built environments became standard in 2026. Codex added saved environments in Sept 2026. Claude caches setup scripts. Devin uses snapshots [secondary]. — [TechCrunch](https://techcrunch.com/2026/09/29/openai-gives-codex-reusable-cloud-environments-that-work-across-devices/); [Claude Code docs](https://code.claude.com/docs/en/claude-code-on-the-web)
- The PR feedback loop is common: Claude Auto-fix (CI and review comments), Jules CI fixer, Copilot PR-comment iteration, and Cursor PR subscriptions. — [Claude Code docs](https://code.claude.com/docs/en/claude-code-on-the-web); [Google blog](https://blog.google/innovation-and-ai/technology/developers-tools/jules-proactive-updates/); [explainx](https://www.explainx.ai/blog/cursor-event-driven-cloud-agents-isolated-vms-august-2026)
- Plan-approval modes are common: Copilot (Apr 2026), Jules (plan approval + critic), Devin Interactive Planning and Claude plan mode. — [GitHub changelog](https://github.blog/changelog/2026-04-01-research-plan-and-code-with-copilot-cloud-agent/); [Pensero](https://pensero.ai/blog/devin-pricing)
- Slack/Linear intake is near-universal among the commercial leaders: Cursor, Copilot, Devin, OpenHands, Oz and Claude (Slack via Claude Tag). — Q1 sources
- Scheduling and event automation went mainstream in 2026, but implementations differ:
  - Jules Scheduled Tasks
  - Cursor Automations (Mar/Aug 2026)
  - Claude Routines
  - Warp Oz schedules
  - ChatGPT event-triggered scheduled tasks (Aug 2026)
  - Copilot VS Code Automations (Sept 2026, secondary)
  - Sources: [Google blog](https://blog.google/innovation-and-ai/technology/developers-tools/jules-proactive-updates/); [spotdev](https://www.spotdev.co.uk/blog/cursor-cloud-agents-and-automations); [ITdaily](https://itdaily.com/news/cloud/claude-code-gets-routines/); [Codex changelog](https://developers.openai.com/codex/changelog)
- Proactive, self-generated work is rare: Jules Suggested Tasks was explicitly framed as a shift toward agents "surfacing work on its own". — [HackerNoon](https://hackernoon.com/googles-jules-starts-surfacing-work-on-its-own-signaling-a-shift-in-ai-coding-assistants)
- Multi-harness support is rare: Warp Oz (Claude Code + Codex + Warp Agent), Tembo, and the local orchestrators (Claude Squad wraps any CLI). Vendor clouds are locked to their own model family (Codex: OpenAI; Claude: Anthropic; Jules: Gemini). — [It's FOSS](https://itsfoss.com/news/warp-oz-multi-harness-update/); [Augment list](https://www.augmentcode.com/tools/open-source-agent-orchestrators)
- Non-GitHub hosts are a common weakness:
  - Claude cloud needs GitHub for cloning and PRs.
  - Codex lacked GitLab/GHES at the Sept 2026 launch, or had only beta GitLab support (conflicting).
  - Sources: [Claude Code docs](https://code.claude.com/docs/en/claude-code-on-the-web); [TechCrunch](https://techcrunch.com/2026/09/29/openai-gives-codex-reusable-cloud-environments-that-work-across-devices/)
- Self-hosting is limited to enterprise tiers: OpenHands (VPC/K8s, contact sales), Claude self-hosted environments (Team/Enterprise beta), Devin VPC (Enterprise), and Warp open-sourcing Oz. — [OpenHands pricing](https://hub.openhands.dev/pricing); [Claude Code docs](https://code.claude.com/docs/en/claude-code-on-the-web)

### Inferences
- **Table stakes for Queue to feel top-tier:**
  - A per-task sandbox with network policy and secret handling.
  - Setup scripts with caching/snapshots, so tasks don't re-install everything.
  - A live log/transcript with mid-task steering (Claude's queued follow-ups, Copilot's steering).
  - A diff view with inline comments fed back to the agent.
  - Plan/approve mode.
  - AGENTS.md/CLAUDE.md support.
  - GitHub (and ideally GitLab) integration.
  - Retries on CI failure.
  - Notifications on completion or when input is needed.
  - At least one external intake (API/webhook; Slack or Linear as a stretch).
- **Queue's differentiators relative to the field:**
  - Self-hosted for individuals, not just enterprises.
  - Provider-agnostic across three vendor CLIs on users' own subscriptions.
  - Explicit 5h/7d budget scheduling.
  - Serialized commit-to-repo with an automatic verify + review-agent gate.
  - OnEmptyQueue self-refill.
  - iPhone PWA with push.
- **Risks:**
  - Running vendor CLIs on consumer subscriptions in a multi-user/multi-tenant setting may conflict with provider terms. Terragon's fate shows how fragile that model is. (Inference; ToS not researched here.)
  - Serial execution will feel slow next to products that market up to 8, 60 or "hundreds" of parallel agents.

### Gaps
- No systematic data (e.g. a survey) on which features users rank as essential. The classification here comes from how common each feature is across vendor docs.
- Provider ToS on automated/multi-user use of consumer subscriptions was not researched.

## Q4: What do users complain about or praise most?

### Takeaway
The dominant 2026 complaint isn't agent capability. Agents produce more code and more PRs than humans can review, which leads to review bottlenecks, duplicate PRs, verbose code and quality issues that get past reviewers. The second big complaint is opaque, quickly exhausted usage limits. Praise centres on async "close the laptop" workflows, generous free tiers (Jules) and tight tool integration (Copilot in GitHub, Cursor in Linear/Slack).

### Cited Findings
- **Review is the bottleneck.** "The review queue grows faster than the team can process it". Reviewer headcount is flat while PR volume grows, and AI code "looks clean and passes basic tests", so important problems slip through. — [DevOps.com](https://devops.com/coding-agents-moved-the-bottleneck-from-writing-to-reviewing-prs)
- A Harvard working paper (700+ firms, through Mar 2026) is reported as finding about +30% lines of code with no corresponding increase in software output. Merge time rose 49% and comments per PR rose 35% (figures via secondary summary). — [explainx summary [secondary]](https://explainx.ai/blog/ai-coding-agents-more-code-not-more-software-harvard-jellyfish-review-bottleneck-2026); [HN thread](https://news.ycombinator.com/item?id=50059523); [redreamality](https://redreamality.com/blog/coding-agent-more-code-not-more-software-2026/)
- In an MSR 2026 study (cited secondhand), 23% of rejected agent PRs were duplicates and 61.38% of agent-authored PRs had no recorded review activity. The post argues "more review is the wrong instinct" and that verification is the fix. — [DEV: Four Horsemen of Agent PRs](https://dev.to/max_quimby/four-horsemen-of-agent-prs-and-how-to-stop-them-bgk); [AgentConn](https://agentconn.com/blog/four-horsemen-agentic-coding-agent-prs-2026/)
- HN commenters:
  - Senior engineers find it hard to give up line-by-line review.
  - Skipping review led to "janky crap or weird decisions".
  - Agents are "still overly verbose".
  - A minority argues the shift is to trust agent code and not review it.
  - Sources: [HN item 50060068](https://news.ycombinator.com/item?id=50060068); [HN item 50059523](https://news.ycombinator.com/item?id=50059523); [HN "what should we do when code is cheap"](https://news.ycombinator.com/item?id=48019025)
- **Usage limits.** A Pro user hit the 5-hour limit twice within hours and nearly exhausted the weekly cap in a day. Another reported a single review using about 20% of weekly quota. Users request an in-app usage bar. — [OpenAI community](https://community.openai.com/t/pro-plan-hit-5-hour-limit-twice-in-2h-and-1-5h-and-nearly-exhausted-weekly-cap-in-1-day-after-today-s-update/1364782); [openai/codex#24181](https://github.com/openai/codex/issues/24181)
- **Security concerns.** Cursor itself warns that cloud agents' default internet access plus auto-run commands creates prompt-injection and exfiltration risk. Claude warns that Auto-fix replies can trigger comment-driven automation (Atlantis, `issue_comment` Actions). — [Cursor docs (via search)](https://cursor.com/docs/cloud-agent); [Claude Code docs](https://code.claude.com/docs/en/claude-code-on-the-web)
- **Pricing confusion.** Comparisons report wildly inconsistent prices for Devin, Copilot's agent and Jules Ultra, which suggests frequent plan changes and opaque metering (ACUs, premium-request multipliers). — [Pensero](https://pensero.ai/blog/devin-pricing); [usagebar](https://usagebar.com/blog/devin-pricing-and-rate-limits); [ssojet](https://ssojet.com/blog/ai-coding-agents-compared)
- **Vendor/tool churn.** Terragon shut down, Roo Code was archived, Vibe Kanban is reportedly unmaintained, Tembo Cloud closed, and Sweep pivoted. Users of third-party orchestrators risk abandonment. — [terragonlabs.com](https://www.terragonlabs.com/); [nexasphere](https://nexasphere.io/blog/cline-vs-roo-code-in-2026-which-open-source-ai-coding-agent-should); [rywalker: Tembo](https://rywalker.com/research/tembo)
- **Praise:**
  - Comparisons consistently describe Devin and Jules as built for genuinely async work.
  - Jules has "the most generous free tier".
  - Copilot is the GitHub-native issue-to-PR choice.
  - Codex fits parallel cloud tasks.
  - Sources: [devtoollab](https://devtoollab.com/blog/best-background-coding-agents); [sinatra.dev](https://www.sinatra.dev/blog/best-background-coding-agents); [amux](https://amux.io/guides/background-agents-compared/)

### Inferences
- Queue's verify-command + review-agent gate before commit, plus serialized execution, addresses the top complaint (review overload, duplicate/conflicting PRs) more directly than PR-centric parallel products. This should be the headline positioning.
- Duplicate PRs (23% of rejections) argue for de-duplication when OnEmptyQueue generates improvement tasks. Without it, self-refill could worsen the "more code, not more software" problem.
- A visible budget meter across providers (5h/7d) directly answers a voiced pain point in Codex.

### Gaps
- No Reddit threads surfaced in searches, so community sentiment comes mostly from HN, DEV and vendor forums.
- The original Harvard and MSR 2026 papers weren't read. The figures are from secondary summaries.
- No product-specific user reviews (e.g. G2 ratings) were collected for most products.
